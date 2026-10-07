//! What `INSERT` and `UPDATE` write, held to the table: as many values as columns, values the
//! columns' types read, no `NULL` where the column is `NOT NULL`, nothing written to a generated
//! column, and no column left out that needs a value. And set operations whose queries give as
//! many columns each.

use sql_catalog::model::{Column, Generated, Table};
use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, TextRange};

use super::literals::{Use, check_value, run_comparisons};
use super::tree::{find_column, has_before_trigger, insert_table, is_default, is_null, query_width, table_column};
use super::{
    Cx, GENERATED_COLUMN_WRITE, INSERT_COLUMN_COUNT, INVALID_LITERAL, MISSING_REQUIRED_COLUMN, NOT_NULL_VIOLATION,
    SET_OPERATION_COLUMN_COUNT, Stmt, UNKNOWN_ENUM_VALUE,
};
use crate::ast::{child, children, is_query, tokens};
use crate::diagnostics::DiagnosticSeverity;
use crate::ident::Ident;
use crate::rename::TextEdit;

pub(super) fn run(cx: &Cx, stmt: &Stmt) {
    if !stmt.clean {
        return;
    }
    run_comparisons(cx, stmt);
    for node in stmt.node.descendants() {
        match node.kind() {
            INSERT_STMT => insert(cx, stmt, &node),
            UPDATE_STMT => update(cx, stmt, &node),
            COMPOUND_SELECT => set_operation(cx, &node),
            _ => {}
        }
    }
}

/// A column an `INSERT` names, with the name as written.
struct Target<'c> {
    name: SyntaxNode,
    column: Option<(usize, &'c Column)>,
}

/// Whether a modifier turns the errors of an `INSERT` into warnings or skipped rows: MySQL's
/// `IGNORE` and SQLite's `OR IGNORE` and `OR REPLACE`.
fn forgiving(insert: &SyntaxNode) -> bool {
    tokens(insert).any(|token| matches!(token.kind(), IGNORE_KW | OR_KW))
}

/// The severity of a refused write in MySQL and MariaDB, which refuse it only in a strict mode.
fn strict_severity(stmt: &Stmt) -> DiagnosticSeverity {
    match stmt.catalog.dialect() {
        Dialect::Mysql | Dialect::Mariadb if stmt.mode.strict() != Some(true) => DiagnosticSeverity::Warning,
        Dialect::Generic => DiagnosticSeverity::Warning,
        _ => DiagnosticSeverity::Error,
    }
}

fn insert(cx: &Cx, stmt: &Stmt, insert: &SyntaxNode) {
    let list = child(insert, NAME_LIST);
    let rows: Vec<SyntaxNode> = child(insert, VALUES)
        .map(|values| children(&values, ROW_EXPR).collect())
        .unwrap_or_default();
    let query = insert
        .children()
        .find(|node| is_query(node.kind()) && node.kind() != VALUES);
    let names: Vec<SyntaxNode> = list
        .as_ref()
        .map(|list| children(list, NAME).collect())
        .unwrap_or_default();
    if let Some(list) = &list {
        if cx.on(INSERT_COLUMN_COUNT) {
            for row in &rows {
                let count = row.children().count();
                if count != names.len() {
                    cx.report(
                        INSERT_COLUMN_COUNT,
                        row.text_range(),
                        format!("{} for {}", values(count), columns(names.len())),
                    )
                    .related(list.text_range(), "The columns the values go to")
                    .emit();
                }
            }
            if let Some((count, at)) = query.as_ref().and_then(query_width) {
                if count != names.len() {
                    cx.report(
                        INSERT_COLUMN_COUNT,
                        at.text_range(),
                        format!("The query gives {} for {}", columns(count), columns(names.len())),
                    )
                    .related(list.text_range(), "The columns the values go to")
                    .emit();
                }
            }
        }
    }
    let Some((_, table)) = insert_table(stmt, insert) else {
        return;
    };
    if list.is_none() && !rows.is_empty() && cx.on(INSERT_COLUMN_COUNT) {
        implicit_columns(cx, stmt, table, &rows);
    }
    let targets: Vec<Target> = if list.is_some() {
        names
            .iter()
            .map(|name| Target {
                name: name.clone(),
                column: Ident::of_name(name, cx.dialect()).and_then(|ident| find_column(stmt, table, &ident.text)),
            })
            .collect()
    } else {
        Vec::new()
    };
    let positional: Vec<(usize, &Column)> = if list.is_some() {
        targets
            .iter()
            .map(|target| target.column)
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    } else {
        table.columns.iter().enumerate().collect()
    };
    let rowcount = rows.len();
    for row in &rows {
        let values: Vec<SyntaxNode> = row.children().collect();
        if values.len() != positional.len() {
            continue;
        }
        for ((_, column), value) in positional.iter().zip(&values) {
            write(cx, stmt, table, column, value, insert, rowcount);
        }
    }
    if let Some(set) = child(insert, SET_CLAUSE) {
        for assignment in children(&set, ASSIGNMENT) {
            assigned(cx, stmt, table, &assignment, insert);
        }
    }
    if cx.on(GENERATED_COLUMN_WRITE) {
        for (position, target) in targets.iter().enumerate() {
            let Some((_, column)) = target.column else {
                continue;
            };
            if !refuses_values(column, insert) {
                continue;
            }
            let only_defaults = !rows.is_empty()
                && rows
                    .iter()
                    .all(|row| row.children().nth(position).is_some_and(|value| is_default(&value)));
            if only_defaults {
                continue;
            }
            cx.report(
                GENERATED_COLUMN_WRITE,
                target.name.text_range(),
                format!(
                    "'{}' is {}, so no value can be written to it",
                    column.name,
                    column.generated.map_or("generated", Generated::label)
                ),
            )
            .fix(format!("Leave out '{}'", column.name), || {
                leave_out(&names, &rows, query.is_some(), position)
            })
            .emit();
        }
    }
    if cx.on(MISSING_REQUIRED_COLUMN) && !forgiving(insert) {
        missing_columns(cx, stmt, table, insert, &targets, list.is_some());
    }
}

/// `INSERT INTO t VALUES (...)` without a column list fills the table's columns in order: MySQL,
/// MariaDB and SQLite want a value for each, PostgreSQL takes fewer and fills the rest with their
/// defaults.
fn implicit_columns(cx: &Cx, stmt: &Stmt, table: &Table, rows: &[SyntaxNode]) {
    let dialect = stmt.catalog.dialect();
    let generated = table.columns.iter().any(|column| column.generated.is_some());
    if dialect == Dialect::Generic || (dialect == Dialect::Sqlite && generated) {
        return;
    }
    let expected = table.columns.len();
    for row in rows {
        let count = row.children().count();
        let wrong = if dialect == Dialect::Postgres {
            count > expected
        } else {
            count != expected
        };
        if wrong {
            cx.report(
                INSERT_COLUMN_COUNT,
                row.text_range(),
                format!("{} for the {} of '{}'", values(count), columns(expected), table.name),
            )
            .emit();
        }
    }
}

/// Whether a column takes no value from an `INSERT`: a generated column, or an identity column
/// `GENERATED ALWAYS` without `OVERRIDING SYSTEM VALUE`.
fn refuses_values(column: &Column, statement: &SyntaxNode) -> bool {
    match column.generated {
        Some(Generated::Stored | Generated::Virtual) => true,
        Some(Generated::IdentityAlways) => !tokens(statement).any(|token| token.kind() == OVERRIDING_KW),
        _ => false,
    }
}

/// The edits that take a column out of an `INSERT`: its name from the list and its value from
/// every row.
fn leave_out(names: &[SyntaxNode], rows: &[SyntaxNode], query: bool, position: usize) -> Option<Vec<TextEdit>> {
    if query || names.len() < 2 {
        return None;
    }
    let mut edits = vec![list_item_removal(names, position)?];
    for row in rows {
        let values: Vec<SyntaxNode> = row.children().collect();
        if values.len() != names.len() {
            return None;
        }
        edits.push(list_item_removal(&values, position)?);
    }
    Some(edits)
}

/// The removal of one item of a comma-separated list with the comma that goes with it.
pub(super) fn list_item_removal(items: &[SyntaxNode], position: usize) -> Option<TextEdit> {
    let item = items.get(position)?;
    let range = if position + 1 < items.len() {
        TextRange::new(item.text_range().start(), items[position + 1].text_range().start())
    } else {
        TextRange::new(
            items.get(position.checked_sub(1)?)?.text_range().end(),
            item.text_range().end(),
        )
    };
    Some(TextEdit {
        range,
        text: String::new(),
    })
}

/// A value an `INSERT` or `UPDATE` writes to a column.
fn write(
    cx: &Cx,
    stmt: &Stmt,
    table: &Table,
    column: &Column,
    value: &SyntaxNode,
    statement: &SyntaxNode,
    rows: usize,
) {
    if is_null(value) {
        if cx.on(NOT_NULL_VIOLATION) && refuses_null(stmt, table, column) && !forgiving(statement) {
            let single = statement.kind() == INSERT_STMT && rows == 1;
            let severity = match stmt.catalog.dialect() {
                Dialect::Mysql | Dialect::Mariadb if single => DiagnosticSeverity::Error,
                _ => strict_severity(stmt),
            };
            cx.report(
                NOT_NULL_VIOLATION,
                value.text_range(),
                format!("'{}' is NOT NULL, so it cannot be set to NULL", column.name),
            )
            .severity(severity)
            .emit();
        }
        return;
    }
    if cx.on(INVALID_LITERAL) || cx.on(UNKNOWN_ENUM_VALUE) {
        check_value(cx, stmt, column, value, Use::Write);
    }
}

/// Whether the server refuses `NULL` for a column: it is `NOT NULL`, it does not number itself
/// (an auto-increment column or SQLite's rowid takes `NULL` as "the next one"), and no trigger may
/// fill it in first.
fn refuses_null(stmt: &Stmt, table: &Table, column: &Column) -> bool {
    if column.nullable != Some(false) || column.auto_increment || column.generated.is_some() {
        return false;
    }
    if stmt.catalog.dialect() == Dialect::Sqlite && in_primary_key(table, column) {
        return false;
    }
    !has_before_trigger(stmt, table)
}

fn in_primary_key(table: &Table, column: &Column) -> bool {
    table
        .primary_key
        .as_ref()
        .is_some_and(|key| key.columns.iter().any(|name| name.eq_ignore_ascii_case(&column.name)))
}

fn assigned(cx: &Cx, stmt: &Stmt, table: &Table, assignment: &SyntaxNode, statement: &SyntaxNode) {
    let mut operands = assignment.children();
    let (Some(target), Some(value)) = (operands.next(), operands.next()) else {
        return;
    };
    if target.kind() != COLUMN_REF {
        return;
    }
    let Some(name) = children(&target, NAME)
        .last()
        .and_then(|name| Ident::of_name(&name, cx.dialect()))
    else {
        return;
    };
    let Some((_, column)) = find_column(stmt, table, &name.text) else {
        return;
    };
    write(cx, stmt, table, column, &value, statement, 1);
    if cx.on(GENERATED_COLUMN_WRITE) && refuses_values(column, statement) && !is_default(&value) {
        cx.report(
            GENERATED_COLUMN_WRITE,
            target.text_range(),
            format!(
                "'{}' is {}, so no value can be written to it",
                column.name,
                column.generated.map_or("generated", Generated::label)
            ),
        )
        .emit();
    }
}

fn update(cx: &Cx, stmt: &Stmt, update: &SyntaxNode) {
    let Some(set) = child(update, SET_CLAUSE) else {
        return;
    };
    for assignment in children(&set, ASSIGNMENT) {
        let Some(target) = assignment.children().next().filter(|node| node.kind() == COLUMN_REF) else {
            continue;
        };
        let Some(found) = table_column(stmt, &target) else {
            continue;
        };
        if found.table.kind.is_view() {
            continue;
        }
        assigned(cx, stmt, found.table, &assignment, update);
    }
}

/// The columns an `INSERT` with a column list leaves out that the server cannot fill in.
fn missing_columns(cx: &Cx, stmt: &Stmt, table: &Table, insert: &SyntaxNode, targets: &[Target], listed: bool) {
    let dialect = stmt.catalog.dialect();
    let default_values = tokens(insert).any(|token| token.kind() == DEFAULT_KW);
    let set: Vec<String> = child(insert, SET_CLAUSE)
        .map(|set| {
            children(&set, ASSIGNMENT)
                .filter_map(|assignment| assignment.children().next())
                .filter_map(|target| children(&target, NAME).last())
                .filter_map(|name| Ident::of_name(&name, dialect).map(|ident| ident.text))
                .collect()
        })
        .unwrap_or_default();
    if !listed && !default_values && set.is_empty() {
        return;
    }
    if targets.iter().any(|target| target.column.is_none())
        || dialect == Dialect::Generic
        || has_before_trigger(stmt, table)
    {
        return;
    }
    let case = crate::ident::name_case(dialect);
    let missing: Vec<&Column> = table
        .columns
        .iter()
        .filter(|column| {
            column.nullable == Some(false)
                && column.default.is_none()
                && column.generated.is_none()
                && !column.auto_increment
                && !(dialect == Dialect::Sqlite && in_primary_key(table, column))
        })
        .filter(|column| {
            !targets.iter().any(|target| {
                target
                    .column
                    .is_some_and(|(_, known)| case.eq(&known.name, &column.name))
            }) && !set.iter().any(|name| case.eq(name, &column.name))
        })
        .collect();
    if missing.is_empty() {
        return;
    }
    let names: Vec<String> = missing.iter().map(|column| format!("'{}'", column.name)).collect();
    let default_words = || {
        let words: Vec<_> = tokens(insert)
            .filter(|token| matches!(token.kind(), DEFAULT_KW | VALUES_KW))
            .collect();
        Some(TextRange::new(
            words.first()?.text_range().start(),
            words.last()?.text_range().end(),
        ))
    };
    let range = child(insert, NAME_LIST)
        .or_else(|| child(insert, SET_CLAUSE))
        .map(|node| node.text_range())
        .or_else(default_words)
        .unwrap_or_else(|| insert.text_range());
    let message = if missing.len() == 1 {
        format!(
            "{} is NOT NULL and has no default, so the INSERT must give it a value",
            names[0]
        )
    } else {
        format!(
            "{} are NOT NULL and have no default, so the INSERT must give them values",
            names.join(", ")
        )
    };
    cx.report(MISSING_REQUIRED_COLUMN, range, message)
        .severity(strict_severity(stmt))
        .emit();
}

fn values(count: usize) -> String {
    if count == 1 {
        "1 value".to_string()
    } else {
        format!("{count} values")
    }
}

fn columns(count: usize) -> String {
    if count == 1 {
        "1 column".to_string()
    } else {
        format!("{count} columns")
    }
}

/// The queries of a set operation, left to right.
fn operands(compound: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
    for node in compound.children() {
        match node.kind() {
            COMPOUND_SELECT => operands(&node, out),
            kind if is_query(kind) => out.push(node),
            _ => {}
        }
    }
}

fn set_operation(cx: &Cx, compound: &SyntaxNode) {
    if !cx.on(SET_OPERATION_COLUMN_COUNT) || compound.parent().is_some_and(|parent| parent.kind() == COMPOUND_SELECT) {
        return;
    }
    let mut queries = Vec::new();
    operands(compound, &mut queries);
    let widths: Vec<Option<(usize, SyntaxNode)>> = queries.iter().map(query_width).collect();
    let Some(Some((first, first_list))) = widths.first() else {
        return;
    };
    for width in widths.iter().skip(1).flatten() {
        let (count, list) = width;
        if count != first {
            cx.report(
                SET_OPERATION_COLUMN_COUNT,
                list.text_range(),
                format!(
                    "This query gives {} where the first gives {}",
                    columns(*count),
                    columns(*first)
                ),
            )
            .related(first_list.text_range(), "The columns of the first query")
            .emit();
        }
    }
}
