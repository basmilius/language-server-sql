//! Semantic tokens: what each token of a script is, so an editor can color a name by what it
//! stands for and not only by its shape. Names are read the way references read them: a table, a
//! view, a column, an alias, a common table expression, a routine, a type, a schema. A name nothing
//! resolves is colored by where it stands, so a script without a schema still reads well.
//!
//! The legend uses the standard token types of LSP only; [`TOKEN_TYPES`] says which SQL thing each
//! one carries, and `docs/clients.md` repeats it for clients.

use sql_syntax::SyntaxKind::{self, *};
use sql_syntax::{SyntaxElement, SyntaxNode, SyntaxToken, Target, TextRange, check_features};

use crate::ast::children;
use crate::catalog::Catalog;
use crate::context::{DocumentSchema, Schemas};
use crate::ident::Ident;
use crate::refs::{Access, LocalKind, Namer, ObjectKind, Symbol};

/// The token types of the legend, in the order a token's `ty` indexes them.
pub const TOKEN_TYPES: &[&str] = &[
    // Keywords, also `NULL`, `TRUE` and `FALSE`.
    "keyword",
    "comment",
    // Strings of every form, a routine body kept as a string included.
    "string",
    "number",
    "operator",
    // Built-in functions (with `defaultLibrary`) and the routines of the schema.
    "function",
    // Data types, built in (with `defaultLibrary`) or of the schema.
    "type",
    // Schemas and databases.
    "namespace",
    // Tables.
    "class",
    // Views and materialized views.
    "interface",
    // Common table expressions.
    "struct",
    // Columns and the names of select items.
    "property",
    // Aliases of tables, windows, sequences, the variables of a routine and user variables.
    "variable",
    // Parameters of a routine and placeholders (`?`, `$1`, `:name`).
    "parameter",
];

/// The modifiers of the legend: bit `n` of a token's `modifiers` is entry `n` here.
pub const TOKEN_MODIFIERS: &[&str] = &["declaration", "readonly", "deprecated", "defaultLibrary"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum Kind {
    Keyword = 0,
    Comment,
    String,
    Number,
    Operator,
    Function,
    Type,
    Namespace,
    Class,
    Interface,
    Struct,
    Property,
    Variable,
    Parameter,
}

const DECLARATION: u32 = 1;
const READONLY: u32 = 1 << 1;
const DEPRECATED: u32 = 1 << 2;
const DEFAULT_LIBRARY: u32 = 1 << 3;

/// A token of a script: a range on one line, an index into [`TOKEN_TYPES`] and a set of bits of
/// [`TOKEN_MODIFIERS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    pub start: u32,
    pub end: u32,
    pub ty: u32,
    pub modifiers: u32,
}

/// The tokens of a script, or of the part of it a range covers, in order. A token over several
/// lines, as a block comment, is given once per line.
pub fn semantic_tokens(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    range: Option<TextRange>,
) -> Vec<SemanticToken> {
    let deprecated: Vec<TextRange> = check_features(root, target)
        .into_iter()
        .filter(|finding| finding.deprecated)
        .map(|finding| finding.range)
        .collect();
    let wanted =
        |piece: TextRange| range.is_none_or(|range| range.intersect(piece).is_some_and(|both| !both.is_empty()));
    let mut document = DocumentSchema::new(target, schemas);
    let mut out = Vec::new();
    let text = root.text().to_string();
    for element in root.children_with_tokens() {
        let statement = match element {
            SyntaxElement::Token(token) => {
                if wanted(token.text_range()) {
                    if let Some((kind, modifiers)) = plain(&token) {
                        push(&mut out, &text, token.text_range(), kind, modifiers);
                    }
                }
                continue;
            }
            SyntaxElement::Node(statement) => statement,
        };
        if wanted(statement.text_range()) {
            let catalog = document.catalog();
            let namer = Namer::new(&catalog, &document.state);
            for token in statement
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|token| wanted(token.text_range()))
            {
                let Some((kind, mut modifiers)) = classify(&namer, &catalog, &token) else {
                    continue;
                };
                if deprecated.iter().any(|range| range.contains_range(token.text_range())) {
                    modifiers |= DEPRECATED;
                }
                push(&mut out, &text, token.text_range(), kind, modifiers);
            }
        }
        document.apply(&statement);
    }
    out
}

/// A token given once for each line it is on.
fn push(out: &mut Vec<SemanticToken>, text: &str, range: TextRange, kind: Kind, modifiers: u32) {
    let start = usize::from(range.start());
    let piece = &text[start..usize::from(range.end())];
    let mut at = start;
    for line in piece.split('\n') {
        let trimmed = line.strip_suffix('\r').unwrap_or(line);
        if !trimmed.is_empty() {
            out.push(SemanticToken {
                start: at as u32,
                end: (at + trimmed.len()) as u32,
                ty: kind as u32,
                modifiers,
            });
        }
        at += line.len() + 1;
    }
}

/// What a token is by its kind alone.
fn plain(token: &SyntaxToken) -> Option<(Kind, u32)> {
    let kind = token.kind();
    Some(match kind {
        LINE_COMMENT | BLOCK_COMMENT => (Kind::Comment, 0),
        INT_NUMBER | FLOAT_NUMBER => (Kind::Number, 0),
        PARAM => (Kind::Parameter, 0),
        VARIABLE => (Kind::Variable, 0),
        SYSTEM_VARIABLE => (Kind::Variable, DEFAULT_LIBRARY),
        _ if kind.is_string() => (Kind::String, 0),
        _ if kind.is_keyword() => (Kind::Keyword, 0),
        _ if is_operator(kind) => (Kind::Operator, 0),
        _ => return None,
    })
}

fn is_operator(kind: SyntaxKind) -> bool {
    (kind >= STAR && kind <= CUSTOM_OP) || matches!(kind, DOUBLE_COLON | COLON_EQ)
}

fn classify(namer: &Namer, catalog: &Catalog, token: &SyntaxToken) -> Option<(Kind, u32)> {
    let parent = token.parent()?;
    if parent.kind() == NAME {
        return name_kind(namer, catalog, &parent);
    }
    let kind = token.kind();
    if kind.is_keyword() {
        if parent.kind() == TYPE {
            return Some((Kind::Type, DEFAULT_LIBRARY));
        }
        if parent.kind() == VALUE_FUNCTION {
            return Some((Kind::Function, DEFAULT_LIBRARY));
        }
    }
    if kind == STAR && parent.kind() == WILDCARD {
        return None;
    }
    plain(token)
}

fn name_kind(namer: &Namer, catalog: &Catalog, name: &SyntaxNode) -> Option<(Kind, u32)> {
    let Some((symbol, access)) = namer.symbol_at(name) else {
        return by_place(catalog, name);
    };
    let declaration = if access == Access::Declaration { DECLARATION } else { 0 };
    let (kind, modifiers) = match &symbol {
        Symbol::Local { kind, .. } => (
            match kind {
                LocalKind::CommonTableExpression => Kind::Struct,
                LocalKind::ColumnAlias => Kind::Property,
                LocalKind::Parameter => Kind::Parameter,
                LocalKind::Alias | LocalKind::Window | LocalKind::Variable | LocalKind::UserVariable => Kind::Variable,
            },
            0,
        ),
        Symbol::Object { kind, schema, .. } => {
            let system = schema
                .as_deref()
                .is_some_and(|schema| catalog.builtins.system_schema(schema).is_some());
            let library = if system { DEFAULT_LIBRARY } else { 0 };
            (
                match kind {
                    ObjectKind::Table => Kind::Class,
                    ObjectKind::View => Kind::Interface,
                    ObjectKind::Routine => Kind::Function,
                    ObjectKind::Type => Kind::Type,
                    ObjectKind::Sequence => Kind::Variable,
                },
                library,
            )
        }
        Symbol::Column {
            schema,
            table,
            name: column,
        } => {
            let system = schema
                .as_deref()
                .is_some_and(|schema| catalog.builtins.system_schema(schema).is_some());
            let mut modifiers = if system { DEFAULT_LIBRARY } else { 0 };
            if generated(catalog, name, schema.as_deref(), table, column) {
                modifiers |= READONLY;
            }
            (Kind::Property, modifiers)
        }
        Symbol::Schema(schema) => {
            let system = catalog.builtins.system_schema(schema).is_some();
            (Kind::Namespace, if system { DEFAULT_LIBRARY } else { 0 })
        }
        Symbol::Builtin(_) => (Kind::Function, DEFAULT_LIBRARY),
    };
    Some((kind, modifiers | declaration))
}

/// Whether a column is generated, which nothing writes to: from the catalog, or from the column
/// definition the name is in.
fn generated(catalog: &Catalog, name: &SyntaxNode, schema: Option<&str>, table: &str, column: &str) -> bool {
    if let Some(definition) = name.parent().filter(|parent| parent.kind() == COLUMN_DEF) {
        return children(&definition, COLUMN_CONSTRAINT).any(|constraint| {
            constraint
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|token| !token.kind().is_trivia() && token.kind() != CONSTRAINT_KW)
                .is_some_and(|token| matches!(token.kind(), GENERATED_KW | AS_KW))
        });
    }
    let schema = schema.map(Ident::new);
    let Some(id) = catalog.find_table(schema.as_ref(), &Ident::new(table)) else {
        return false;
    };
    catalog
        .table(id)
        .columns
        .iter()
        .find(|known| catalog.name_case.eq(&known.name, column))
        .is_some_and(|known| known.generated.is_some())
}

/// What a name nothing resolves is by where it stands.
fn by_place(catalog: &Catalog, name: &SyntaxNode) -> Option<(Kind, u32)> {
    let parent = name.parent()?;
    let last = children(&parent, NAME).last().as_ref() == Some(name);
    let text = || crate::refs::bare_text(name);
    match parent.kind() {
        QUALIFIED_NAME => {
            let owner = parent.parent()?;
            if !last {
                return Some((Kind::Namespace, 0));
            }
            match owner.kind() {
                // A type no layer defines is the dialect's own, whether the catalog lists the spelling
                // (`integer`) or only the grammar reads it (`int`).
                TYPE => Some((Kind::Type, DEFAULT_LIBRARY)),
                FUNCTION_CALL | CALL_STMT => {
                    let library = if catalog.builtins.function(&text()).is_some() {
                        DEFAULT_LIBRARY
                    } else {
                        0
                    };
                    Some((Kind::Function, library))
                }
                TABLE_REF | INSERT_STMT | UPDATE_STMT | DELETE_STMT | MERGE_STMT | TRUNCATE_STMT
                | REFERENCES_CLAUSE | TABLE_QUERY | ALTER_TABLE_STMT => Some((Kind::Class, 0)),
                _ => None,
            }
        }
        COLUMN_REF => Some(if last { (Kind::Property, 0) } else { (Kind::Variable, 0) }),
        NAME_LIST if parent.parent().is_some_and(|owner| owner.kind() == INSERT_STMT) => Some((Kind::Property, 0)),
        _ => None,
    }
}

/// The tokens as text, a line each: the token, its type and its modifiers.
#[cfg(test)]
pub fn describe(text: &str, tokens: &[SemanticToken]) -> String {
    let mut out = String::new();
    for token in tokens {
        let piece = &text[token.start as usize..token.end as usize];
        let modifiers: Vec<&str> = TOKEN_MODIFIERS
            .iter()
            .enumerate()
            .filter(|(index, _)| token.modifiers & (1 << index) != 0)
            .map(|(_, name)| *name)
            .collect();
        out.push_str(&format!("{piece} {}", TOKEN_TYPES[token.ty as usize]));
        if !modifiers.is_empty() {
            out.push_str(&format!(" [{}]", modifiers.join(", ")));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use sql_syntax::{Dialect, Target, TextRange, Version, parse};

    use super::*;
    use crate::testing::{shop, target, with_snapshot};

    fn check(dialect: Dialect, schemas: Schemas, text: &str, expect: Expect) {
        let root = parse(text, dialect).syntax();
        expect.assert_eq(&describe(text, &semantic_tokens(&root, target(dialect), schemas, None)));
    }

    #[test]
    fn names_are_colored_by_what_they_stand_for() {
        let layer = shop(Dialect::Postgres);
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "WITH recent AS (SELECT * FROM orders) -- recent\nSELECT u.email, lower(u.name) AS n, order_total(r.id), $1\nFROM public.users u JOIN recent r ON r.user_id = u.id, active_users, pg_class;",
            expect![[r#"
                WITH keyword
                recent struct [declaration]
                AS keyword
                SELECT keyword
                FROM keyword
                orders class
                -- recent comment
                SELECT keyword
                u variable
                email property
                lower function [defaultLibrary]
                u variable
                name property
                AS keyword
                n property [declaration]
                order_total function
                r variable
                id property
                $1 parameter
                FROM keyword
                public namespace
                users class
                u variable [declaration]
                JOIN keyword
                recent struct
                r variable [declaration]
                ON keyword
                r variable
                user_id property
                = operator
                u variable
                id property
                active_users interface
                pg_class class [defaultLibrary]
            "#]],
        );
    }

    #[test]
    fn definitions_types_and_generated_columns() {
        check(
            Dialect::Postgres,
            Schemas::NONE,
            "CREATE TABLE t (a int, b int GENERATED ALWAYS AS (a * 2) STORED, c timestamp with time zone);\nSELECT b FROM t;",
            expect![[r#"
                CREATE keyword
                TABLE keyword
                t class [declaration]
                a property [declaration]
                int type [defaultLibrary]
                b property [declaration, readonly]
                int type [defaultLibrary]
                GENERATED keyword
                ALWAYS keyword
                AS keyword
                a property
                * operator
                2 number
                STORED keyword
                c property [declaration]
                timestamp type [defaultLibrary]
                with type [defaultLibrary]
                time type [defaultLibrary]
                zone type [defaultLibrary]
                SELECT keyword
                b property [readonly]
                FROM keyword
                t class
            "#]],
        );
    }

    #[test]
    fn variables_deprecations_and_lines() {
        let text = "SELECT SQL_CALC_FOUND_ROWS @a, @@sql_mode /* two\nlines */ FROM t;";
        let root = parse(text, Dialect::Mysql).syntax();
        let tokens = semantic_tokens(
            &root,
            Target::new(Dialect::Mysql, Version::parse("8.0.30")),
            Schemas::NONE,
            None,
        );
        expect![[r#"
            SELECT keyword
            SQL_CALC_FOUND_ROWS keyword [deprecated]
            @a variable
            @@sql_mode variable [defaultLibrary]
            /* two comment
            lines */ comment
            FROM keyword
            t class
        "#]]
        .assert_eq(&describe(text, &tokens));
        let range = TextRange::new(27.into(), 30.into());
        let only = semantic_tokens(&root, target(Dialect::Mysql), Schemas::NONE, Some(range));
        assert_eq!(describe(text, &only), "@a variable\n");
    }
}
