//! What several inspections ask of the tree and the schema.

use sql_catalog::FunctionKind;
use sql_catalog::model::{Column, RoutineKind, Table};
use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxNode, SyntaxToken, TextRange};

use super::Stmt;
use crate::ast::{child, children, inner_query, is_query};
use crate::catalog::TableId;
use crate::resolve::{ColumnOrigin, Referent, Resolution, Source, SourceKind};

/// An expression without the parentheses around it.
pub(super) fn strip_parens(node: &SyntaxNode) -> SyntaxNode {
    let mut node = node.clone();
    while node.kind() == PAREN_EXPR {
        match node.children().next() {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The token of a plain string literal: `'...'`, and `"..."` where the dialect reads it as one.
pub(super) fn string_literal(node: &SyntaxNode) -> Option<SyntaxToken> {
    let node = strip_parens(node);
    if node.kind() != LITERAL {
        return None;
    }
    let token = node.first_token()?;
    (token.kind() == STRING && node.text_range() == token.text_range()).then_some(token)
}

/// The text of a string literal without its quotes, with doubled quotes and, in MySQL and MariaDB,
/// backslash escapes read.
pub(super) fn string_value(token: &SyntaxToken, dialect: Dialect) -> String {
    let text = token.text();
    let quote = text.chars().next().unwrap_or('\'');
    let body = if text.len() >= 2 { &text[1..text.len() - 1] } else { "" };
    let backslashes = matches!(dialect, Dialect::Mysql | Dialect::Mariadb | Dialect::Generic);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' && backslashes {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else if character == quote && chars.peek() == Some(&quote) {
            chars.next();
            out.push(quote);
        } else {
            out.push(character);
        }
    }
    out
}

pub(super) fn is_null(node: &SyntaxNode) -> bool {
    let node = strip_parens(node);
    node.kind() == LITERAL && node.first_token().is_some_and(|token| token.kind() == NULL_KW)
}

pub(super) fn is_default(node: &SyntaxNode) -> bool {
    let node = strip_parens(node);
    node.kind() == DEFAULT_EXPR
        || (node.kind() == LITERAL && node.first_token().is_some_and(|token| token.kind() == DEFAULT_KW))
}

/// A column of a table of the schema that a `COLUMN_REF` or a `NAME` stands for.
pub(super) struct TableColumn<'c> {
    pub table: &'c Table,
    pub column: &'c Column,
    /// The source the column is read through, when the name is read in a query.
    pub source: Option<Source>,
}

/// The column of the schema a name reads, when the schema is known.
pub(super) fn table_column<'c>(stmt: &Stmt<'_, 'c, '_>, name: &SyntaxNode) -> Option<TableColumn<'c>> {
    if !stmt.known {
        return None;
    }
    let name = if name.kind() == COLUMN_REF {
        children(name, NAME).last()?
    } else {
        name.clone()
    };
    let Some(Resolution::Found(Referent::Column { source, column })) = stmt.resolver.resolve_name(&name) else {
        return None;
    };
    let ColumnOrigin::Table(id, position) = column.origin else {
        return None;
    };
    let table = stmt.catalog.table(id);
    Some(TableColumn {
        table,
        column: table.columns.get(position)?,
        source: Some(source),
    })
}

/// The table an `INSERT` writes to, when the schema has it as a table and knows all its columns.
pub(super) fn insert_table<'c>(stmt: &Stmt<'_, 'c, '_>, insert: &SyntaxNode) -> Option<(TableId, &'c Table)> {
    if !stmt.known {
        return None;
    }
    let target = child(insert, QUALIFIED_NAME)?;
    let name = children(&target, NAME).last()?;
    let Some(Resolution::Found(Referent::Table(id))) = stmt.resolver.resolve_name(&name) else {
        return None;
    };
    let table = stmt.catalog.table(id);
    (!table.kind.is_view() && !table.open && !table.columns.is_empty()).then_some((id, table))
}

/// The columns of a table, by name in the dialect's case.
pub(super) fn find_column<'t>(stmt: &Stmt, table: &'t Table, name: &str) -> Option<(usize, &'t Column)> {
    let case = crate::ident::name_case(stmt.catalog.dialect());
    table
        .columns
        .iter()
        .enumerate()
        .find(|(_, column)| case.eq(&column.name, name))
}

/// Whether a trigger of the table runs before a row is written, which may fill in what a
/// statement leaves out.
pub(super) fn has_before_trigger(stmt: &Stmt, table: &Table) -> bool {
    stmt.catalog
        .all(|schema| &schema.triggers, |trigger| &trigger.name)
        .iter()
        .any(|trigger| {
            trigger.table.eq_ignore_ascii_case(&table.name)
                && trigger.timing.as_deref().is_none_or(|timing| {
                    let timing = timing.to_ascii_uppercase();
                    timing.contains("BEFORE") || timing.contains("INSTEAD")
                })
        })
}

/// The select items of the first query a query starts with, or `None` when a wildcard or
/// something unknown makes the count unsure.
pub(super) fn query_width(query: &SyntaxNode) -> Option<(usize, SyntaxNode)> {
    match query.kind() {
        SELECT => {
            let list = child(query, SELECT_LIST)?;
            let items: Vec<SyntaxNode> = children(&list, SELECT_ITEM).collect();
            let wildcard = items
                .iter()
                .any(|item| item.children().next().is_some_and(|first| first.kind() == WILDCARD));
            (!wildcard && !items.is_empty()).then_some((items.len(), list))
        }
        VALUES => {
            let row = child(query, ROW_EXPR)?;
            Some((row.children().count(), row))
        }
        PAREN_QUERY | COMPOUND_SELECT => query_width(&inner_query(query)?),
        _ => None,
    }
}

/// Whether a node is inside a query nested in `outer` rather than in `outer` itself.
pub(super) fn nested_in_query(node: &SyntaxNode, outer: &SyntaxNode) -> bool {
    node.ancestors()
        .take_while(|ancestor| ancestor != outer)
        .any(|ancestor| is_query(ancestor.kind()) || ancestor.kind() == CTE)
}

/// The descendants of a node that belong to it and not to a query nested in it.
pub(super) fn own_descendants(node: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
    node.descendants()
        .filter(move |inner| inner == node || !nested_in_query(inner, node))
}

/// What a function call is, as far as grouping cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CallKind {
    Aggregate,
    /// A window function, or an aggregate with `OVER`.
    Window,
    Scalar,
    /// Neither built in nor in the schema, so it may be anything.
    Unknown,
}

const AGGREGATES: &[&str] = &[
    "any_value",
    "array_agg",
    "avg",
    "bit_and",
    "bit_or",
    "bit_xor",
    "bool_and",
    "bool_or",
    "corr",
    "count",
    "covar_pop",
    "covar_samp",
    "every",
    "group_concat",
    "grouping",
    "json_agg",
    "json_arrayagg",
    "json_object_agg",
    "json_objectagg",
    "jsonb_agg",
    "jsonb_object_agg",
    "listagg",
    "max",
    "min",
    "mode",
    "percentile_cont",
    "percentile_disc",
    "std",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "string_agg",
    "sum",
    "total",
    "var_pop",
    "var_samp",
    "variance",
    "xmlagg",
];

pub(super) fn call_kind(stmt: &Stmt, call: &SyntaxNode) -> CallKind {
    if child(call, OVER_CLAUSE).is_some() {
        return CallKind::Window;
    }
    let Some(name) = child(call, QUALIFIED_NAME).and_then(|name| children(&name, NAME).last()) else {
        return CallKind::Unknown;
    };
    let text = crate::refs::bare_text(&name).to_ascii_lowercase();
    if AGGREGATES.contains(&text.as_str()) || text.starts_with("regr_") {
        return CallKind::Aggregate;
    }
    match stmt.resolver.resolve_name(&name) {
        Some(Resolution::Found(Referent::Function(function))) => {
            match stmt
                .catalog
                .builtins
                .function(&function)
                .map(|function| function.kind())
            {
                Some(FunctionKind::Aggregate) => CallKind::Aggregate,
                Some(FunctionKind::Window) => CallKind::Window,
                Some(_) => CallKind::Scalar,
                None => CallKind::Unknown,
            }
        }
        Some(Resolution::Found(Referent::Routines(ids))) => {
            let kinds: Vec<RoutineKind> = ids.iter().map(|id| stmt.catalog.routine_at(*id).kind).collect();
            if kinds
                .iter()
                .any(|kind| matches!(kind, RoutineKind::Aggregate | RoutineKind::Window))
            {
                CallKind::Aggregate
            } else {
                CallKind::Scalar
            }
        }
        _ => {
            if stmt.catalog.builtins.function(&text).is_some() {
                CallKind::Scalar
            } else {
                CallKind::Unknown
            }
        }
    }
}

/// Whether a source sits on the side of an outer join that may come out as NULL.
pub(super) fn on_nullable_side(source: &Source) -> bool {
    let mut inner = source.node.clone();
    while let Some(parent) = inner.parent() {
        if parent.kind() == FROM_CLAUSE || is_query(parent.kind()) {
            break;
        }
        if parent.kind() == JOIN_EXPR {
            let words: Vec<_> = parent
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .map(|token| token.kind())
                .collect();
            let operands: Vec<SyntaxNode> = parent
                .children()
                .filter(|node| !matches!(node.kind(), ON_CLAUSE | USING_CLAUSE))
                .collect();
            let left = operands.first().is_some_and(|left| *left == inner);
            if words.contains(&FULL_KW) || (words.contains(&LEFT_KW) && !left) || (words.contains(&RIGHT_KW) && left) {
                return true;
            }
        }
        inner = parent;
    }
    false
}

/// Whether a source is a table of the schema, not a view, a subquery or a function.
pub(super) fn base_table<'c>(stmt: &Stmt<'_, 'c, '_>, source: &Source) -> Option<&'c Table> {
    let SourceKind::Table(id) = source.kind else {
        return None;
    };
    let table = stmt.catalog.table(id);
    (!table.kind.is_view()).then_some(table)
}

/// A node with the whitespace before it, for an edit that removes it and leaves one space.
pub(super) fn with_space_before(node: &SyntaxNode) -> TextRange {
    let start = node
        .first_token()
        .and_then(|token| token.prev_token())
        .filter(|previous| previous.kind() == WHITESPACE)
        .map_or(node.text_range().start(), |previous| previous.text_range().start());
    TextRange::new(start, node.text_range().end())
}
