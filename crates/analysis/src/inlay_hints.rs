//! Inlay hints, kept to the places where a value's meaning is not on the screen: the column each
//! value of an `INSERT ... VALUES` row goes to when the statement lists no columns or many, the
//! column each item of an `INSERT ... SELECT` fills, and the names of the parameters a call's
//! arguments go to. A hint that would repeat what is written (a value that is a column of that
//! name) is left out.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxElement, SyntaxNode, Target, TextRange};

use crate::ast::{child, children, is_query, object_name, parts};
use crate::catalog::Catalog;
use crate::context::{DocumentSchema, Schemas};
use crate::ident::{Ident, name_case};
use crate::refs::bare_text;
use crate::resolve::{Referent, Resolution, Resolver};

/// A column list this long or longer gets hints even when it is written out.
pub const LONG_COLUMN_LIST: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HintOptions {
    /// The column a value of `INSERT ... VALUES` goes to.
    pub insert_columns: bool,
    /// The column an item of `INSERT ... SELECT` fills.
    pub select_columns: bool,
    /// The parameter an argument of a call goes to.
    pub parameter_names: bool,
}

impl Default for HintOptions {
    fn default() -> HintOptions {
        HintOptions {
            insert_columns: true,
            select_columns: true,
            parameter_names: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintKind {
    Column,
    Parameter,
}

/// A label shown before the expression at `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    pub offset: u32,
    pub label: String,
    pub kind: HintKind,
}

/// The hints of a script, or of the statements a range touches.
pub fn inlay_hints(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    range: Option<TextRange>,
    options: HintOptions,
) -> Vec<InlayHint> {
    let mut document = DocumentSchema::new(target, schemas);
    let mut out = Vec::new();
    for statement in root.children() {
        let wanted = range.is_none_or(|range| range.intersect(statement.text_range()).is_some());
        if wanted {
            let catalog = document.catalog();
            let resolver = Resolver::new(&catalog);
            let mut found = Vec::new();
            for node in statement.descendants() {
                match node.kind() {
                    INSERT_STMT => insert_hints(&catalog, &node, options, &mut found),
                    FUNCTION_CALL | CALL_STMT if options.parameter_names => {
                        call_hints(&resolver, &catalog, target, &node, &mut found)
                    }
                    _ => {}
                }
            }
            found.retain(|hint: &InlayHint| {
                range.is_none_or(|range| range.contains(hint.offset.into()) || range.end() == hint.offset.into())
            });
            out.extend(found);
        }
        document.apply(&statement);
    }
    out.sort_by_key(|hint| hint.offset);
    out
}

/// The columns an `INSERT` fills, in order, and whether it lists them.
fn insert_columns(catalog: &Catalog, statement: &SyntaxNode) -> Option<(Vec<String>, bool)> {
    if let Some(list) = child(statement, NAME_LIST) {
        let names = children(&list, NAME).map(|name| bare_text(&name)).collect();
        return Some((names, true));
    }
    let (schema, table) = object_name(&child(statement, QUALIFIED_NAME)?, catalog.dialect())?;
    let id = catalog.find_table(schema.as_ref().map(|part| &part.ident), &table.ident)?;
    let table = catalog.table(id);
    if table.open {
        return None;
    }
    let visible = table.columns.iter().filter(|column| !column.invisible);
    Some((visible.map(|column| column.name.clone()).collect(), false))
}

/// Whether an expression is a column of the same name, which a hint would only repeat.
fn says_itself(expression: &SyntaxNode, column: &str, catalog: &Catalog) -> bool {
    let named = match expression.kind() {
        COLUMN_REF => parts(expression, catalog.dialect()).pop().map(|part| part.ident.text),
        _ => None,
    };
    named.is_some_and(|named| name_case(catalog.dialect()).eq(&named, column) || named.eq_ignore_ascii_case(column))
}

fn insert_hints(catalog: &Catalog, statement: &SyntaxNode, options: HintOptions, out: &mut Vec<InlayHint>) {
    let Some(source) = statement
        .children()
        .find(|inner| inner.kind() == VALUES || is_query(inner.kind()))
    else {
        return;
    };
    let Some((columns, listed)) = insert_columns(catalog, statement) else {
        return;
    };
    if source.kind() == VALUES {
        if !options.insert_columns || (listed && columns.len() < LONG_COLUMN_LIST) {
            return;
        }
        for row in children(&source, ROW_EXPR) {
            for (value, column) in row.children().zip(&columns) {
                if !says_itself(&value, column, catalog) {
                    out.push(column_hint(&value, column));
                }
            }
        }
        return;
    }
    if !options.select_columns {
        return;
    }
    let mut select = source;
    while select.kind() != SELECT {
        let Some(inner) = select.children().find(|inner| is_query(inner.kind())) else {
            return;
        };
        select = inner;
    }
    let Some(list) = child(&select, SELECT_LIST) else {
        return;
    };
    for (item, column) in children(&list, SELECT_ITEM).zip(&columns) {
        let Some(expression) = item.children().next() else {
            continue;
        };
        if expression.kind() == WILDCARD {
            return;
        }
        let output = crate::resolve::output_name(&item, catalog.dialect());
        let same = output.is_some_and(|output| name_case(catalog.dialect()).eq(&output, column));
        if !same {
            out.push(column_hint(&expression, column));
        }
    }
}

fn column_hint(value: &SyntaxNode, column: &str) -> InlayHint {
    InlayHint {
        offset: u32::from(value.text_range().start()),
        label: format!("{column}:"),
        kind: HintKind::Column,
    }
}

/// The arguments of a call by position, or nothing for a call that names its arguments or uses a
/// form of the grammar (`EXTRACT(... FROM ...)`, `count(DISTINCT ...)`).
fn positional_arguments(list: &SyntaxNode) -> Option<Vec<SyntaxNode>> {
    let words = list
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .any(|token| token.kind().is_keyword());
    if words
        || list
            .children()
            .any(|inner| matches!(inner.kind(), NAMED_ARG | ORDER_BY_CLAUSE))
    {
        return None;
    }
    Some(list.children().collect())
}

/// The names of the parameters the arguments go to: of the routines of the schema, or of the
/// built-in function at the version, the ways of calling it that take this many arguments. A
/// position where those disagree gets no name.
fn call_hints(resolver: &Resolver, catalog: &Catalog, target: Target, call: &SyntaxNode, out: &mut Vec<InlayHint>) {
    let Some(list) = child(call, ARG_LIST) else {
        return;
    };
    let Some(arguments) = positional_arguments(&list) else {
        return;
    };
    if arguments.is_empty() {
        return;
    }
    let Some(name) = child(call, QUALIFIED_NAME).and_then(|name| children(&name, NAME).last()) else {
        return;
    };
    let count = arguments.len();
    let mut candidates: Vec<Vec<String>> = Vec::new();
    let mut builtin = false;
    match resolver.resolve_name(&name) {
        Some(Resolution::Found(Referent::Routines(ids))) => {
            for id in ids {
                let routine = catalog.routine_at(id);
                let inputs: Vec<&sql_catalog::model::Parameter> = routine
                    .parameters
                    .iter()
                    .filter(|parameter| {
                        !matches!(parameter.mode, Some(sql_catalog::model::ParameterMode::Out))
                            || call.kind() == CALL_STMT
                    })
                    .collect();
                let required = inputs.iter().filter(|parameter| parameter.default.is_none()).count();
                let variadic = inputs
                    .iter()
                    .any(|parameter| matches!(parameter.mode, Some(sql_catalog::model::ParameterMode::Variadic)));
                if count < required || (count > inputs.len() && !variadic) {
                    continue;
                }
                candidates.push(
                    (0..count)
                        .map(|position| {
                            inputs
                                .get(position.min(inputs.len().saturating_sub(1)))
                                .and_then(|parameter| parameter.name.clone())
                                .unwrap_or_default()
                        })
                        .collect(),
                );
            }
        }
        Some(Resolution::Found(Referent::Function(function))) => {
            builtin = true;
            let Some(function) = catalog.builtins.function(&function) else {
                return;
            };
            for overload in function.overloads_at(target) {
                let (least, most) = overload.arity();
                if count < least || most.is_some_and(|most| count > most) {
                    continue;
                }
                candidates.push(
                    (0..count)
                        .map(|position| {
                            overload
                                .params
                                .get(position.min(overload.params.len().saturating_sub(1)))
                                .map(|param| param.name.clone())
                                .unwrap_or_default()
                        })
                        .collect(),
                );
            }
        }
        _ => return,
    }
    // One argument of a built-in function says what it is by the function's name alone.
    if builtin && count < 2 {
        return;
    }
    let Some(first) = candidates.first() else {
        return;
    };
    for (position, argument) in arguments.iter().enumerate() {
        let parameter = &first[position];
        if parameter.is_empty() || candidates.iter().any(|other| &other[position] != parameter) {
            continue;
        }
        let parameter = Ident::new(parameter.clone());
        if says_itself(argument, &parameter.text, catalog) {
            continue;
        }
        out.push(InlayHint {
            offset: u32::from(argument.text_range().start()),
            label: format!("{}:", parameter.text),
            kind: HintKind::Parameter,
        });
    }
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use sql_syntax::{Dialect, parse};

    use super::*;
    use crate::testing::{shop, target, with_snapshot};

    fn render(text: &str, hints: &[InlayHint]) -> String {
        let mut out = String::new();
        let mut at = 0usize;
        for hint in hints {
            let offset = hint.offset as usize;
            out.push_str(&text[at..offset]);
            out.push_str(&format!("<{}>", hint.label));
            at = offset;
        }
        out.push_str(&text[at..]);
        out
    }

    fn check(dialect: Dialect, schemas: Schemas, text: &str, expect: Expect) {
        let root = parse(text, dialect).syntax();
        let hints = inlay_hints(&root, target(dialect), schemas, None, HintOptions::default());
        expect.assert_eq(&render(text, &hints));
    }

    #[test]
    fn values_without_a_column_list_say_their_columns() {
        let layer = shop(Dialect::Postgres);
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "INSERT INTO orgs VALUES (1, 'Acme'), (2, name);\nINSERT INTO orgs (id, name) VALUES (3, 'Short');\nINSERT INTO users (id, org_id, email, status) VALUES (1, 2, 'a@b', DEFAULT);",
            expect![[r#"
                INSERT INTO orgs VALUES (<id:>1, <name:>'Acme'), (<id:>2, name);
                INSERT INTO orgs (id, name) VALUES (3, 'Short');
                INSERT INTO users (id, org_id, email, status) VALUES (<id:>1, <org_id:>2, <email:>'a@b', <status:>DEFAULT);"#]],
        );
    }

    #[test]
    fn insert_select_says_the_column_each_item_fills() {
        check(
            Dialect::Postgres,
            Schemas::NONE,
            "CREATE TABLE t (a int, b int, c int);\nINSERT INTO t SELECT a, x + 1, y AS c FROM u;\nINSERT INTO t (b) SELECT * FROM u;",
            expect![[r#"
                CREATE TABLE t (a int, b int, c int);
                INSERT INTO t SELECT a, <b:>x + 1, y AS c FROM u;
                INSERT INTO t (b) SELECT * FROM u;"#]],
        );
    }

    #[test]
    fn calls_name_their_parameters() {
        let layer = shop(Dialect::Postgres);
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT order_total(42), make_date(2026, 10, 7), left('abc', 2), lower('A'), substring('abc' FROM 2), order_total(order_id);\nCALL archive(now());",
            expect![[r#"
                SELECT order_total(<order_id:>42), make_date(<year:>2026, <month:>10, <day:>7), left('abc', 2), lower('A'), substring('abc' FROM 2), order_total(order_id);
                CALL archive(<before:>now());"#]],
        );
    }

    #[test]
    fn a_range_keeps_the_hints_inside_it() {
        let layer = shop(Dialect::Postgres);
        let text = "INSERT INTO orgs VALUES (1, 'a');\nINSERT INTO orgs VALUES (2, 'b');";
        let root = parse(text, Dialect::Postgres).syntax();
        let range = TextRange::new(34.into(), u32::try_from(text.len()).expect("short").into());
        let hints = inlay_hints(
            &root,
            target(Dialect::Postgres),
            with_snapshot(&layer),
            Some(range),
            HintOptions::default(),
        );
        assert_eq!(hints.len(), 2);
        let none = inlay_hints(
            &root,
            target(Dialect::Postgres),
            with_snapshot(&layer),
            None,
            HintOptions {
                insert_columns: false,
                ..HintOptions::default()
            },
        );
        assert!(none.is_empty());
    }
}
