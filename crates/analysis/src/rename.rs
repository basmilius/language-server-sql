//! Rename: which names can change, the edits that follow from the places a symbol is named, and
//! the checks that keep a rename from changing what a statement means.
//!
//! A local symbol (an alias, a common table expression, a column alias, a window, a variable) is
//! renamed in its statement, and the statement is read again with the new name to make sure every
//! name still stands for what it stood for. An object of the schema is renamed in the document and
//! in the workspace's files, but only when DDL in a file defines it: a table only the snapshot has
//! lives in the database, which a rename of text cannot reach. A name that also stands inside a
//! string of SQL the server does not read (a routine body in a string, `PREPARE`, `EXECUTE`) is not
//! renamed at all, since the rename could not be complete.

use std::path::PathBuf;

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxNode, SyntaxToken, Target, TextRange, TextSize, parse};

use crate::ast::{child, children, inner_query, parts};
use crate::catalog::{Catalog, Layer};
use crate::context::{DocumentSchema, Schemas};
use crate::ident::{Case, Ident, needs_quotes};
use crate::references::{Current, OtherFile, may_mention};
use crate::refs::{
    Cases, Hit, LocalKind, Namer, ObjectKind, Symbol, bare_text, dynamic_sql_mentions, find_hits, name_at,
    statement_of, symbol_at_offset, user_variable_name, variable_at,
};
use crate::resolve::{Resolution, Resolver};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// The name as the editor should offer it for editing.
    pub range: TextRange,
    pub placeholder: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdits {
    /// The file; `None` is the document asked about.
    pub path: Option<PathBuf>,
    pub edits: Vec<TextEdit>,
}

/// The name under a position, when it can be renamed.
pub fn prepare_rename(current: &Current, offset: u32) -> Result<Prepared, String> {
    let (range, symbol, _) = symbol_at_offset(current.root, offset, current.target, current.schemas)
        .ok_or_else(|| "There is no name to rename here".to_string())?;
    check_renamable(current, &symbol)?;
    let placeholder = match variable_at(current.root, offset, current.target.dialect) {
        Some(token) => user_variable_name(token.text()).to_string(),
        None => match name_at(current.root, offset) {
            Some(name) if name.text_range() == range => bare_text(&name),
            _ => {
                let text = current.root.text().to_string();
                text.get(usize::from(range.start())..usize::from(range.end()))
                    .map_or_else(
                        || symbol.name().to_string(),
                        |found| found.trim_matches('"').to_string(),
                    )
            }
        },
    };
    let range = match variable_at(current.root, offset, current.target.dialect) {
        Some(token) => token.text_range(),
        None => range,
    };
    Ok(Prepared { range, placeholder })
}

/// Whether DDL in the document or the workspace defines a symbol, which is what a rename of text
/// can change.
fn check_renamable(current: &Current, symbol: &Symbol) -> Result<(), String> {
    let name = symbol.name();
    match symbol {
        Symbol::Local { .. } => Ok(()),
        Symbol::Builtin(_) => Err(format!("'{name}' is a built-in function and cannot be renamed")),
        _ => {
            let document = full_document(current);
            let catalog = document.catalog();
            let cases = Cases::of(&catalog);
            let declared = find_hits(
                current.root,
                current.target,
                current.schemas,
                std::slice::from_ref(symbol),
            )
            .iter()
            .any(|(_, hit)| hit.access == crate::refs::Access::Declaration)
                || in_layer(current.schemas.workspace, symbol, cases);
            if declared {
                return Ok(());
            }
            if in_layer(current.schemas.snapshot, symbol, cases) {
                return Err(format!(
                    "The {} '{name}' is defined only in the schema snapshot, not in a file, so renaming it here would not rename it in the database",
                    symbol.label()
                ));
            }
            if is_system(&catalog, symbol) {
                return Err(format!(
                    "The {} '{name}' belongs to the database itself and cannot be renamed",
                    symbol.label()
                ));
            }
            Err(format!(
                "No DDL in the workspace defines the {} '{name}', so a rename cannot be complete",
                symbol.label()
            ))
        }
    }
}

/// The document with all its DDL applied, for the names a new name must not clash with.
fn full_document<'a>(current: &Current<'a>) -> DocumentSchema<'a> {
    let mut document = DocumentSchema::new(current.target, current.schemas);
    for statement in current.root.children() {
        document.apply(&statement);
    }
    document
}

fn schema_matches(layer_schema: &str, wanted: &Option<String>, case: Case) -> bool {
    match wanted {
        None => true,
        Some(wanted) => layer_schema.is_empty() || case.eq(layer_schema, wanted),
    }
}

/// Whether a layer has the object a symbol names.
fn in_layer(layer: Option<&Layer>, symbol: &Symbol, cases: Cases) -> bool {
    let Some(layer) = layer else {
        return false;
    };
    layer.snapshot.schemas.iter().any(|schema| match symbol {
        Symbol::Object {
            kind,
            schema: wanted,
            name,
        } => {
            schema_matches(&schema.name, wanted, cases.table)
                && match kind {
                    ObjectKind::Table | ObjectKind::View => {
                        schema.tables.iter().any(|table| cases.table.eq(&table.name, name))
                    }
                    ObjectKind::Routine => schema.routines.iter().any(|routine| cases.name.eq(&routine.name, name)),
                    ObjectKind::Type => schema.types.iter().any(|known| cases.name.eq(&known.name, name)),
                    ObjectKind::Sequence => schema.sequences.iter().any(|known| cases.name.eq(&known.name, name)),
                }
        }
        Symbol::Column {
            schema: wanted,
            table,
            name,
        } => {
            schema_matches(&schema.name, wanted, cases.table)
                && schema.tables.iter().any(|known| {
                    cases.table.eq(&known.name, table)
                        && known.columns.iter().any(|column| cases.name.eq(&column.name, name))
                })
        }
        Symbol::Schema(name) => {
            !schema.name.is_empty() && cases.table.eq(&schema.name, name) && schema.location.is_some()
        }
        _ => false,
    })
}

fn is_system(catalog: &Catalog, symbol: &Symbol) -> bool {
    let builtins = catalog.builtins;
    match symbol {
        Symbol::Object { schema, name, .. }
        | Symbol::Column {
            schema, table: name, ..
        } => {
            let table = match symbol {
                Symbol::Column { table, .. } => table,
                _ => name,
            };
            builtins.schemas.iter().any(|known| {
                schema
                    .as_ref()
                    .is_none_or(|schema| known.name.eq_ignore_ascii_case(schema))
                    && known.tables.iter().any(|entry| entry.name.eq_ignore_ascii_case(table))
            })
        }
        Symbol::Schema(name) => builtins.system_schema(name).is_some(),
        _ => false,
    }
}

/// The name a person typed, without the quotes they may have put around it.
fn new_name_of(typed: &str) -> Result<String, String> {
    let typed = typed.trim();
    let unquoted = [('"', '"'), ('`', '`'), ('[', ']')]
        .iter()
        .find(|(open, close)| typed.len() >= 2 && typed.starts_with(*open) && typed.ends_with(*close))
        .map(|(open, close)| {
            let inner = &typed[1..typed.len() - 1];
            if open == close {
                inner.replace(&format!("{close}{close}"), &close.to_string())
            } else {
                inner.to_string()
            }
        });
    let name = unquoted.unwrap_or_else(|| typed.to_string());
    if name.is_empty() {
        return Err("The new name is empty".to_string());
    }
    if name
        .chars()
        .any(|character| character == '\0' || character == '\n' || character == '\r')
    {
        return Err(format!("'{name}' cannot be a name"));
    }
    Ok(name)
}

/// The edits that rename what is under a position.
pub fn rename(current: &Current, offset: u32, typed: &str, others: &[OtherFile]) -> Result<Vec<FileEdits>, String> {
    let (_, symbol, _) = symbol_at_offset(current.root, offset, current.target, current.schemas)
        .ok_or_else(|| "There is no name to rename here".to_string())?;
    check_renamable(current, &symbol)?;
    let new_name = new_name_of(typed)?;
    if let Symbol::Local {
        kind: LocalKind::UserVariable,
        ..
    } = symbol
    {
        let valid = new_name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$' | '.'));
        if !valid {
            return Err(format!("'{new_name}' cannot be the name of a user variable"));
        }
    }
    let document = full_document(current);
    let catalog = document.catalog();
    let cases = Cases::of(&catalog);
    let old_name = symbol.name().to_string();
    let same_name = match &symbol {
        Symbol::Object {
            kind: ObjectKind::Table | ObjectKind::View,
            ..
        }
        | Symbol::Schema(_) => cases.table.eq(&old_name, &new_name),
        _ => cases.name.eq(&old_name, &new_name),
    };
    if same_name && old_name == new_name {
        return Ok(Vec::new());
    }
    if !same_name {
        check_free(&catalog, &symbol, &new_name)?;
        check_free_in(others, &symbol, &new_name)?;
    }
    if symbol.is_local() {
        return rename_local(current, &symbol, &new_name);
    }
    let files: Vec<(Option<&OtherFile>, SyntaxNode)> = std::iter::once((None, current.root.clone()))
        .chain(
            others
                .iter()
                .filter(|other| may_mention(other.text, &symbol))
                .map(|other| (Some(other), parse(other.text, other.target.dialect).syntax())),
        )
        .collect();
    for (other, root) in &files {
        if let Some(range) = dynamic_sql_mentions(root, &old_name).first() {
            let place = match other {
                Some(other) => format!("{} line {}", other.path.display(), line_of(other.text, range.start())),
                None => format!("line {}", line_of(&current.root.text().to_string(), range.start())),
            };
            return Err(format!(
                "'{old_name}' is also named inside a string of SQL ({place}), which a rename cannot follow"
            ));
        }
    }
    let mut symbols = vec![symbol.clone()];
    let mut found: Vec<Vec<Hit>> = Vec::new();
    for _ in 0..8 {
        found = files
            .iter()
            .map(|(other, root)| {
                let (target, schemas) = match other {
                    Some(other) => (other.target, other.schemas),
                    None => (current.target, current.schemas),
                };
                find_hits(root, target, schemas, &symbols)
                    .into_iter()
                    .map(|(_, hit)| hit)
                    .collect()
            })
            .collect();
        let mut derived = Vec::new();
        for ((other, root), hits) in files.iter().zip(&found) {
            let (target, schemas) = match other {
                Some(other) => (other.target, other.schemas),
                None => (current.target, current.schemas),
            };
            for hit in hits {
                if let Some(column) = derived_column(root, hit, target, schemas) {
                    if !symbols
                        .iter()
                        .chain(&derived)
                        .any(|known: &Symbol| known.same(&column, cases))
                    {
                        derived.push(column);
                    }
                }
            }
        }
        if derived.is_empty() {
            break;
        }
        symbols.extend(derived);
    }
    for ((other, root), hits) in files.iter().zip(&found) {
        let (target, schemas) = match other {
            Some(other) => (other.target, other.schemas),
            None => (current.target, current.schemas),
        };
        check_captures(root, target, schemas, hits, &symbol, &new_name)?;
    }
    let mut result = Vec::new();
    for ((other, root), hits) in files.iter().zip(&found) {
        if hits.is_empty() {
            continue;
        }
        let target = other.map_or(current.target, |other| other.target);
        let edits = hits.iter().map(|hit| edit_for(root, hit, &new_name, target)).collect();
        result.push(FileEdits {
            path: other.map(|other| other.path.to_path_buf()),
            edits,
        });
    }
    Ok(result)
}

fn line_of(text: &str, offset: TextSize) -> usize {
    text[..usize::from(offset).min(text.len())].matches('\n').count() + 1
}

/// The error when the new name is taken: a table, column or routine of that name already there.
fn check_free(catalog: &Catalog, symbol: &Symbol, new_name: &str) -> Result<(), String> {
    let taken = |what: &str| Err(format!("There is already a {what} named '{new_name}'"));
    match symbol {
        Symbol::Object { kind, schema, .. } => {
            let schema = schema.as_ref().map(Ident::new);
            let ident = Ident::new(new_name);
            match kind {
                ObjectKind::Table | ObjectKind::View => {
                    if let Some(id) = catalog.find_table(schema.as_ref(), &ident) {
                        let other = catalog.table(id);
                        return taken(other.kind.label());
                    }
                }
                ObjectKind::Routine => {
                    if !catalog.routines(schema.as_ref(), new_name).is_empty() {
                        return taken("routine");
                    }
                }
                ObjectKind::Type => {
                    if catalog.user_type(schema.as_ref(), new_name).is_some() {
                        return taken("type");
                    }
                }
                ObjectKind::Sequence => {
                    if catalog.sequence(schema.as_ref(), new_name).is_some() {
                        return taken("sequence");
                    }
                }
            }
            Ok(())
        }
        Symbol::Column { schema, table, .. } => {
            let schema = schema.as_ref().map(Ident::new);
            if let Some(id) = catalog.find_table(schema.as_ref(), &Ident::new(table.clone())) {
                let has = catalog
                    .table(id)
                    .columns
                    .iter()
                    .any(|column| catalog.name_case.eq(&column.name, new_name));
                if has {
                    return Err(format!("The table '{table}' already has a column named '{new_name}'"));
                }
            }
            Ok(())
        }
        Symbol::Schema(_) => {
            if catalog.is_schema(&Ident::new(new_name)) {
                return taken("schema");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The error when another file defines an object of the new name.
fn check_free_in(others: &[OtherFile], symbol: &Symbol, new_name: &str) -> Result<(), String> {
    if symbol.is_local() {
        return Ok(());
    }
    let renamed = symbol.renamed(new_name);
    for other in others.iter().filter(|other| may_mention(other.text, &renamed)) {
        let root = parse(other.text, other.target.dialect).syntax();
        let defined = find_hits(&root, other.target, other.schemas, std::slice::from_ref(&renamed))
            .iter()
            .any(|(_, hit)| hit.access == crate::refs::Access::Declaration);
        if defined {
            let what = match symbol {
                Symbol::Column { table, .. } => format!("The table '{table}' already has a column"),
                _ => format!("There is already a {}", symbol.label()),
            };
            return Err(format!("{what} named '{new_name}' ({})", other.path.display()));
        }
    }
    Ok(())
}

/// A local symbol is renamed in its statement; the statement is read again with the new name, and
/// every name must stand for what it stood for, no more and no less.
fn rename_local(current: &Current, symbol: &Symbol, new_name: &str) -> Result<Vec<FileEdits>, String> {
    let hits: Vec<Hit> = find_hits(
        current.root,
        current.target,
        current.schemas,
        std::slice::from_ref(symbol),
    )
    .into_iter()
    .map(|(_, hit)| hit)
    .collect();
    let edits: Vec<TextEdit> = hits
        .iter()
        .map(|hit| edit_for(current.root, hit, new_name, current.target))
        .collect();
    let text = current.root.text().to_string();
    let renamed_text = apply(&text, &edits);
    let renamed_root = parse(&renamed_text, current.target.dialect).syntax();
    let Symbol::Local { kind, declaration, .. } = symbol else {
        return Ok(Vec::new());
    };
    let expected: Vec<TextRange> = edits.iter().map(|edit| shifted(&edits, edit.range)).collect();
    let renamed = Symbol::Local {
        kind: *kind,
        name: new_name.to_string(),
        declaration: shifted(&edits, *declaration),
    };
    let mut wanted = vec![renamed];
    if let Symbol::Local {
        kind: LocalKind::UserVariable,
        ..
    } = symbol
    {
        if let Some(first) = renamed_root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| token.kind() == VARIABLE && user_variable_name(token.text()).eq_ignore_ascii_case(new_name))
        {
            wanted = vec![Symbol::Local {
                kind: LocalKind::UserVariable,
                name: new_name.to_string(),
                declaration: first.text_range(),
            }];
        }
    }
    let after: Vec<TextRange> = find_hits(&renamed_root, current.target, current.schemas, &wanted)
        .into_iter()
        .map(|(_, hit)| hit.range)
        .collect();
    if after != expected {
        return Err(format!(
            "'{new_name}' is already a name in this scope: renaming the {} would change what other names stand for",
            symbol.label()
        ));
    }
    Ok(vec![FileEdits { path: None, edits }])
}

/// Where a range of the text lands after edits made before and in it.
fn shifted(edits: &[TextEdit], range: TextRange) -> TextRange {
    let mut delta: i64 = 0;
    let mut length = i64::from(u32::from(range.len()));
    for edit in edits {
        if edit.range.end() <= range.start() {
            delta += edit.text.len() as i64 - i64::from(u32::from(edit.range.len()));
        } else if edit.range == range {
            length = edit.text.len() as i64;
        }
    }
    let start = (i64::from(u32::from(range.start())) + delta).max(0) as u32;
    TextRange::at(TextSize::from(start), TextSize::from(length.max(0) as u32))
}

/// The text with the edits applied; they must not overlap.
pub fn apply(text: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| edit.range.start());
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for edit in sorted {
        let (start, end) = (usize::from(edit.range.start()), usize::from(edit.range.end()));
        out.push_str(&text[at..start]);
        out.push_str(&edit.text);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// The new name as one place has to write it: in the quotes it had, or bare where the dialect
/// lets it stand bare.
fn edit_for(root: &SyntaxNode, hit: &Hit, new_name: &str, target: Target) -> TextEdit {
    let token = root.covering_element(hit.range).into_token().or_else(|| {
        root.covering_element(hit.range)
            .into_node()
            .and_then(|node| node.first_token())
    });
    let text = match token {
        _ if hit.in_string => {
            if needs_quotes(new_name, target) {
                format!("\"{}\"", new_name.replace('"', "\"\"")).replace('\'', "''")
            } else {
                new_name.to_string()
            }
        }
        Some(token) => spell(&token, new_name, target),
        None => crate::ident::quote_name(new_name, target),
    };
    TextEdit { range: hit.range, text }
}

/// A name written the way the token it replaces was: the same quotes, or none when none are
/// needed.
pub fn spell(token: &SyntaxToken, new_name: &str, target: Target) -> String {
    let wrap = |open: char, close: char| {
        let escaped = if open == close {
            new_name.replace(close, &format!("{close}{close}"))
        } else {
            new_name.to_string()
        };
        format!("{open}{escaped}{close}")
    };
    match token.kind() {
        QUOTED_IDENT => wrap('"', '"'),
        BACKTICK_IDENT => wrap('`', '`'),
        BRACKET_IDENT if !new_name.contains(']') => wrap('[', ']'),
        BRACKET_IDENT => wrap('"', '"'),
        STRING => wrap('\'', '\''),
        VARIABLE => {
            let bare = new_name
                .chars()
                .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$' | '.'));
            if bare {
                format!("@{new_name}")
            } else {
                format!("@`{}`", new_name.replace('`', "``"))
            }
        }
        _ => crate::ident::quote_name(new_name, target),
    }
}

/// The column of a view or a `CREATE TABLE ... AS` that a select item passes a column on to under
/// its own name, which a rename of the column renames too.
fn derived_column(root: &SyntaxNode, hit: &Hit, target: Target, schemas: Schemas) -> Option<Symbol> {
    if hit.in_string {
        return None;
    }
    let name = root.covering_element(hit.range).into_token()?.parent()?;
    let reference = name.parent().filter(|parent| parent.kind() == COLUMN_REF)?;
    if children(&reference, NAME).last().as_ref() != Some(&name) {
        return None;
    }
    let item = reference.parent().filter(|parent| parent.kind() == SELECT_ITEM)?;
    if child(&item, ALIAS).is_some() {
        return None;
    }
    let select = item.parent()?.parent()?;
    let statement = statement_of(&select)?;
    if !matches!(statement.kind(), CREATE_VIEW_STMT | CREATE_TABLE_STMT) || child(&statement, NAME_LIST).is_some() {
        return None;
    }
    let mut first = inner_query(&statement)?;
    while first.kind() != SELECT {
        first = first.children().find(|inner| crate::ast::is_query(inner.kind()))?;
    }
    if first != select {
        return None;
    }
    let qualified = child(&statement, QUALIFIED_NAME)?;
    let document = DocumentSchema::before(root, u32::from(statement.text_range().start()), target, schemas);
    let catalog = document.catalog();
    let namer = Namer::new(&catalog, &document.state);
    let (table, _) = namer.symbol_at(&children(&qualified, NAME).last()?)?;
    let Symbol::Object {
        schema, name: table, ..
    } = table
    else {
        return None;
    };
    Some(Symbol::Column {
        schema,
        table,
        name: Ident::of_name(&name, target.dialect)?.text,
    })
}

/// The error when a name of the new name already stands where the renamed one is named, so the
/// rename would make a name ambiguous or have it stand for something else.
fn check_captures(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    hits: &[Hit],
    symbol: &Symbol,
    new_name: &str,
) -> Result<(), String> {
    let new_ident = Ident::new(new_name);
    let mut document = DocumentSchema::new(target, schemas);
    for statement in root.children() {
        let range = statement.text_range();
        let inside: Vec<&Hit> = hits
            .iter()
            .filter(|hit| !hit.in_string && range.contains_range(hit.range))
            .collect();
        if !inside.is_empty() {
            let catalog = document.catalog();
            let resolver = Resolver::new(&catalog);
            for hit in inside {
                let Some(name) = root
                    .covering_element(hit.range)
                    .into_token()
                    .and_then(|token| token.parent())
                else {
                    continue;
                };
                if let Some(reason) = capture_at(&resolver, &name, symbol, &new_ident, target.dialect) {
                    return Err(reason);
                }
            }
        }
        document.apply(&statement);
    }
    Ok(())
}

fn capture_at(
    resolver: &Resolver,
    name: &SyntaxNode,
    symbol: &Symbol,
    new_ident: &Ident,
    dialect: Dialect,
) -> Option<String> {
    let parent = name.parent()?;
    let new_name = &new_ident.text;
    match symbol {
        Symbol::Object {
            kind: ObjectKind::Table | ObjectKind::View,
            ..
        } => {
            let ctes = resolver.ctes(name);
            if ctes
                .iter()
                .any(|cte| resolver.catalog.table_case.eq(&cte.name.text, new_name))
            {
                return Some(format!(
                    "A common table expression named '{new_name}' is in scope where the table is named"
                ));
            }
            if matches!(parent.kind(), COLUMN_REF | WILDCARD) {
                let levels = resolver.scope(&parent);
                if resolver.find_source(&levels, new_ident).is_some() {
                    return Some(format!(
                        "A table or alias named '{new_name}' is in scope where the table is named"
                    ));
                }
            }
            None
        }
        Symbol::Column { .. } => {
            if parent.kind() != COLUMN_REF || parts(&parent, dialect).len() != 1 {
                return None;
            }
            let levels = resolver.scope(&parent);
            match resolver.resolve_unqualified(&levels, new_ident) {
                Resolution::Unknown { .. } => None,
                _ => Some(format!(
                    "'{new_name}' already names something where the column is named, so the column would no longer be the one meant"
                )),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_quotes_off_a_typed_name() {
        assert_eq!(new_name_of("\"My Table\"").as_deref(), Ok("My Table"));
        assert_eq!(new_name_of("`a``b`").as_deref(), Ok("a`b"));
        assert_eq!(new_name_of("[x]").as_deref(), Ok("x"));
        assert_eq!(new_name_of("  users ").as_deref(), Ok("users"));
        assert!(new_name_of("").is_err());
        assert!(new_name_of("\"\"").is_err());
    }

    #[test]
    fn a_range_moves_with_the_edits_before_it() {
        let edits = [
            TextEdit {
                range: TextRange::new(0.into(), 1.into()),
                text: "abc".to_string(),
            },
            TextEdit {
                range: TextRange::new(5.into(), 6.into()),
                text: "xy".to_string(),
            },
        ];
        assert_eq!(
            shifted(&edits, TextRange::new(5.into(), 6.into())),
            TextRange::new(7.into(), 9.into())
        );
        assert_eq!(apply("a---b-", &edits), "abc---bxy");
    }
}
