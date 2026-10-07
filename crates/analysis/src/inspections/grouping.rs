//! Grouping: a column a grouped query neither groups nor aggregates, which PostgreSQL always
//! rejects and MySQL rejects under `ONLY_FULL_GROUP_BY` (on by default since 5.7), while MariaDB
//! (off by default) and SQLite pick a value from the group; `DISTINCT` that `GROUP BY` already
//! makes unnecessary; and `COUNT(column)` of a column that cannot be NULL.
//!
//! A column is grouped when `GROUP BY` names it, names the whole expression it stands in, or names
//! the select item by position or alias. PostgreSQL also takes a column of a table whose primary
//! key is grouped; MySQL takes one of a table whose primary key or a unique key of NOT NULL
//! columns is grouped, and one an equality of `WHERE` or `ON` fixes, so a column compared there is
//! left alone.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, TextRange};

use super::tree::{CallKind, base_table, call_kind, on_nullable_side, table_column};
use super::{COUNT_NOT_NULL_COLUMN, Cx, DISTINCT_WITH_GROUP_BY, NONAGGREGATED_COLUMN, Stmt};
use crate::ast::{child, children, compact, has_token, is_query, tokens};
use crate::rename::TextEdit;
use crate::resolve::{Referent, Resolution, Source, SourceKind};

pub(super) fn run(cx: &Cx, stmt: &Stmt) {
    if !stmt.clean {
        return;
    }
    let strict = match cx.dialect() {
        Dialect::Postgres => true,
        Dialect::Mysql | Dialect::Mariadb => stmt.mode.has("ONLY_FULL_GROUP_BY") == Some(true),
        _ => false,
    };
    for select in stmt.node.descendants().filter(|node| node.kind() == SELECT) {
        if !cx.wants(select.text_range()) {
            continue;
        }
        let grouping = Grouping::of(stmt, &select);
        if cx.on(NONAGGREGATED_COLUMN) {
            if let Some(grouping) = &grouping {
                if strict {
                    nonaggregated(cx, stmt, &select, grouping);
                }
                if matches!(cx.dialect(), Dialect::Mysql | Dialect::Mariadb) {
                    having_unselected(cx, stmt, &select, grouping);
                }
            }
        }
        if cx.on(DISTINCT_WITH_GROUP_BY) {
            if let Some(grouping) = &grouping {
                redundant_distinct(cx, &select, grouping);
            }
        }
    }
    if cx.on(COUNT_NOT_NULL_COLUMN) && stmt.known {
        for call in stmt.node.descendants().filter(|node| node.kind() == FUNCTION_CALL) {
            count_not_null(cx, stmt, &call);
        }
    }
}

/// A column by the source it is read through and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ColumnKey {
    source: SyntaxNode,
    name: String,
}

/// What a grouped query groups by.
struct Grouping {
    clause: Option<SyntaxNode>,
    /// The expressions, without whitespace and case.
    expressions: Vec<String>,
    columns: Vec<ColumnKey>,
    /// `ROLLUP`, `CUBE`, `GROUPING SETS` or `WITH ROLLUP`.
    sets: bool,
    /// Items that are neither a column, a position nor an alias.
    plain: bool,
}

fn key(node: &SyntaxNode) -> String {
    compact(node).to_ascii_lowercase()
}

fn column_key(stmt: &Stmt, reference: &SyntaxNode) -> Option<(ColumnKey, Source)> {
    let name = children(reference, NAME).last()?;
    let Some(Resolution::Found(Referent::Column { source, column })) = stmt.resolver.resolve_name(&name) else {
        return None;
    };
    Some((
        ColumnKey {
            source: source.node.clone(),
            name: column.name.to_ascii_lowercase(),
        },
        source,
    ))
}

impl Grouping {
    /// The grouping of a query that groups: by `GROUP BY`, or by an aggregate without it.
    fn of(stmt: &Stmt, select: &SyntaxNode) -> Option<Grouping> {
        let clause = child(select, GROUP_BY_CLAUSE);
        if clause.is_none() {
            let aggregated = [SELECT_LIST, HAVING_CLAUSE]
                .iter()
                .filter_map(|kind| child(select, *kind))
                .any(|part| has_aggregate(stmt, &part));
            if !aggregated {
                return None;
            }
        }
        let mut grouping = Grouping {
            clause: clause.clone(),
            expressions: Vec::new(),
            columns: Vec::new(),
            sets: false,
            plain: true,
        };
        let Some(clause) = clause else {
            return Some(grouping);
        };
        let items = child(select, SELECT_LIST)
            .map(|list| children(&list, SELECT_ITEM).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut pending: Vec<SyntaxNode> = clause.children().collect();
        grouping.sets = has_token(&clause, ROLLUP_KW) || has_token(&clause, WITH_KW);
        while let Some(item) = pending.pop() {
            if matches!(item.kind(), GROUPING_SET | ROW_EXPR | PAREN_EXPR) {
                grouping.sets |= item.kind() == GROUPING_SET;
                if item.kind() == PAREN_EXPR {
                    grouping.expressions.push(key(&item));
                }
                pending.extend(item.children());
                continue;
            }
            grouping.expressions.push(key(&item));
            if item.kind() == LITERAL {
                let position = item.text().to_string().trim().parse::<usize>().ok();
                if let Some(selected) = position.and_then(|position| items.get(position.wrapping_sub(1))) {
                    grouping.add_item(stmt, selected);
                }
                continue;
            }
            if item.kind() == COLUMN_REF {
                let name = children(&item, NAME).last();
                match name.and_then(|name| stmt.resolver.resolve_name(&name)) {
                    Some(Resolution::Found(Referent::SelectAlias(selected))) => grouping.add_item(stmt, &selected),
                    Some(Resolution::Found(Referent::Column { source, column })) => grouping.columns.push(ColumnKey {
                        source: source.node.clone(),
                        name: column.name.to_ascii_lowercase(),
                    }),
                    _ => {}
                }
                continue;
            }
            grouping.plain = false;
        }
        Some(grouping)
    }

    fn add_item(&mut self, stmt: &Stmt, item: &SyntaxNode) {
        let Some(expression) = item.children().find(|node| node.kind() != ALIAS) else {
            return;
        };
        self.expressions.push(key(&expression));
        if expression.kind() == COLUMN_REF {
            if let Some((column, _)) = column_key(stmt, &expression) {
                self.columns.push(column);
            }
        }
    }
}

pub(super) fn has_aggregate(stmt: &Stmt, node: &SyntaxNode) -> bool {
    let mut stack = vec![node.clone()];
    while let Some(current) = stack.pop() {
        if current.kind() == FUNCTION_CALL && call_kind(stmt, &current) == CallKind::Aggregate {
            return true;
        }
        stack.extend(current.children().filter(|inner| !is_query(inner.kind())));
    }
    false
}

/// The column references of an expression a grouped query must group, leaving out what an
/// aggregate, a window function, a function nobody knows or a subquery holds.
fn loose_columns(stmt: &Stmt, node: &SyntaxNode, grouping: &Grouping, out: &mut Vec<SyntaxNode>) {
    if grouping.expressions.contains(&key(node)) {
        return;
    }
    match node.kind() {
        COLUMN_REF => {
            out.push(node.clone());
            return;
        }
        FUNCTION_CALL if call_kind(stmt, node) != CallKind::Scalar => return,
        WILDCARD | ALIAS => return,
        kind if is_query(kind) || kind == EXISTS_EXPR => return,
        _ => {}
    }
    for inner in node.children() {
        loose_columns(stmt, &inner, grouping, out);
    }
}

/// Whether the columns of a source that a grouping fixes include a key of its table, which makes
/// every other column of the table depend on the group.
fn depends_on_key(stmt: &Stmt, source: &Source, grouping: &Grouping, dialect: Dialect) -> bool {
    let Some(table) = base_table(stmt, source) else {
        return false;
    };
    let grouped = |name: &String| {
        grouping
            .columns
            .iter()
            .any(|column| column.source == source.node && column.name.eq_ignore_ascii_case(name))
    };
    if table
        .primary_key
        .as_ref()
        .is_some_and(|key| key.columns.iter().all(grouped))
    {
        return true;
    }
    dialect != Dialect::Postgres
        && table.unique_keys.iter().any(|key| {
            key.columns.iter().all(grouped)
                && key.columns.iter().all(|name| {
                    table
                        .columns
                        .iter()
                        .any(|column| column.name.eq_ignore_ascii_case(name) && column.nullable == Some(false))
                })
        })
}

/// Whether an equality of `WHERE` or `ON` names a column, which MySQL reads as fixing its value.
fn compared_for_equality(select: &SyntaxNode, stmt: &Stmt, column: &ColumnKey) -> bool {
    let mut places: Vec<SyntaxNode> = child(select, WHERE_CLAUSE).into_iter().collect();
    if let Some(from) = child(select, FROM_CLAUSE) {
        places.extend(from.descendants().filter(|node| node.kind() == ON_CLAUSE));
    }
    places.iter().any(|place| {
        place
            .descendants()
            .filter(|node| node.kind() == BINARY_EXPR && has_token(node, EQ))
            .flat_map(|equality| equality.children().collect::<Vec<_>>())
            .filter(|operand| operand.kind() == COLUMN_REF)
            .any(|operand| column_key(stmt, &operand).is_some_and(|(found, _)| found == *column))
    })
}

fn nonaggregated(cx: &Cx, stmt: &Stmt, select: &SyntaxNode, grouping: &Grouping) {
    let dialect = cx.dialect();
    let Some(from) = child(select, FROM_CLAUSE) else {
        return;
    };
    let mut places: Vec<SyntaxNode> = Vec::new();
    if let Some(list) = child(select, SELECT_LIST) {
        places
            .extend(children(&list, SELECT_ITEM).filter_map(|item| item.children().find(|node| node.kind() != ALIAS)));
    }
    if dialect == Dialect::Postgres {
        places.extend(
            child(select, HAVING_CLAUSE)
                .into_iter()
                .flat_map(|having| having.children().collect::<Vec<_>>()),
        );
    }
    if let Some(order) = child(select, ORDER_BY_CLAUSE) {
        places.extend(children(&order, ORDER_ITEM).filter_map(|item| item.children().next()));
    }
    let mut loose = Vec::new();
    for place in &places {
        loose_columns(stmt, place, grouping, &mut loose);
    }
    let mut reported: Vec<ColumnKey> = Vec::new();
    for reference in loose {
        let Some((column, source)) = column_key(stmt, &reference) else {
            continue;
        };
        let from_here = source.node.ancestors().any(|ancestor| ancestor == from);
        if !from_here || grouping.columns.contains(&column) || reported.contains(&column) {
            continue;
        }
        if dialect != Dialect::Postgres
            && (base_table(stmt, &source).is_none() || compared_for_equality(select, stmt, &column))
        {
            continue;
        }
        if grouping.clause.is_some() && depends_on_key(stmt, &source, grouping, dialect) {
            continue;
        }
        if !matches!(
            source.kind,
            SourceKind::Table(_) | SourceKind::Cte(_) | SourceKind::Derived(_)
        ) {
            continue;
        }
        reported.push(column);
        let written = reference.text().to_string();
        let message = match &grouping.clause {
            Some(_) => format!("'{written}' is neither in GROUP BY nor in an aggregate function"),
            None => {
                format!("'{written}' is not in an aggregate function, though the query aggregates without GROUP BY")
            }
        };
        let mut pending = cx.report(NONAGGREGATED_COLUMN, reference.text_range(), message);
        if let Some(clause) = grouping.clause.as_ref().filter(|_| !grouping.sets) {
            pending = pending.fix(format!("Add '{written}' to GROUP BY"), || {
                let last = clause.children().last()?;
                Some(vec![TextEdit {
                    range: TextRange::empty(last.text_range().end()),
                    text: format!(", {written}"),
                }])
            });
        }
        pending.emit();
    }
}

/// MySQL and MariaDB let `HAVING` read the select list besides what `GROUP BY` names, in any
/// mode, and refuse any other column outside an aggregate as unknown.
fn having_unselected(cx: &Cx, stmt: &Stmt, select: &SyntaxNode, grouping: &Grouping) {
    let (Some(_), Some(having), Some(from)) = (
        &grouping.clause,
        child(select, HAVING_CLAUSE),
        child(select, FROM_CLAUSE),
    ) else {
        return;
    };
    let items: Vec<SyntaxNode> = child(select, SELECT_LIST)
        .map(|list| {
            children(&list, SELECT_ITEM)
                .filter_map(|item| item.children().find(|node| node.kind() != ALIAS))
                .collect()
        })
        .unwrap_or_default();
    if items.iter().any(|item| item.kind() == WILDCARD) {
        return;
    }
    let selected_columns: Vec<ColumnKey> = items
        .iter()
        .filter(|item| item.kind() == COLUMN_REF)
        .filter_map(|item| column_key(stmt, item).map(|(column, _)| column))
        .collect();
    let selected: Vec<String> = items.iter().map(key).collect();
    let mut loose = Vec::new();
    for condition in having.children() {
        loose_columns(stmt, &condition, grouping, &mut loose);
    }
    for reference in loose {
        if selected.contains(&key(&reference)) {
            continue;
        }
        let Some((column, source)) = column_key(stmt, &reference) else {
            continue;
        };
        let from_here = source.node.ancestors().any(|ancestor| ancestor == from);
        if !from_here || grouping.columns.contains(&column) || selected_columns.contains(&column) {
            continue;
        }
        let written = reference.text().to_string();
        cx.report(
            NONAGGREGATED_COLUMN,
            reference.text_range(),
            format!("'{written}' in HAVING is neither in GROUP BY, in the select list nor in an aggregate function"),
        )
        .emit();
    }
}

/// `SELECT DISTINCT a, b ... GROUP BY a, b`: each group is one row, and the select list holds
/// everything the groups differ by, so the rows are distinct already.
fn redundant_distinct(cx: &Cx, select: &SyntaxNode, grouping: &Grouping) {
    if grouping.clause.is_none() || grouping.sets || !grouping.plain {
        return;
    }
    let Some(distinct) = tokens(select).find(|token| token.kind() == DISTINCT_KW) else {
        return;
    };
    if tokens(select).any(|token| token.kind() == ON_KW) {
        return;
    }
    let Some(list) = child(select, SELECT_LIST) else {
        return;
    };
    let selected: Vec<String> = children(&list, SELECT_ITEM)
        .filter_map(|item| item.children().find(|node| node.kind() != ALIAS))
        .map(|expression| key(&expression))
        .collect();
    let Some(clause) = &grouping.clause else {
        return;
    };
    let aliases: Vec<String> = children(&list, SELECT_ITEM)
        .filter_map(|item| crate::ast::alias_of(&item, cx.dialect()))
        .map(|(alias, _)| alias.ident.text.to_ascii_lowercase())
        .collect();
    let every_group_key_selected = clause.children().all(|item| {
        let written = key(&item);
        let position = (item.kind() == LITERAL)
            .then(|| written.parse::<usize>().ok())
            .flatten()
            .is_some_and(|position| position >= 1 && position <= selected.len());
        selected.contains(&written) || position || (item.kind() == COLUMN_REF && aliases.contains(&written))
    });
    if !every_group_key_selected {
        return;
    }
    let removal = TextRange::new(
        distinct.text_range().start(),
        distinct
            .next_token()
            .filter(|next| next.kind() == WHITESPACE)
            .map_or(distinct.text_range().end(), |next| next.text_range().end()),
    );
    cx.report(
        DISTINCT_WITH_GROUP_BY,
        distinct.text_range(),
        "DISTINCT changes nothing: GROUP BY already gives one row per group, and the select list holds every grouped expression",
    )
    .fix("Remove DISTINCT", || Some(vec![TextEdit { range: removal, text: String::new() }]))
    .emit();
}

/// `COUNT(c)` of a column that cannot be NULL counts what `COUNT(*)` counts.
fn count_not_null(cx: &Cx, stmt: &Stmt, call: &SyntaxNode) {
    let named_count = child(call, QUALIFIED_NAME).is_some_and(|name| compact(&name).eq_ignore_ascii_case("count"));
    if !named_count || !cx.wants(call.text_range()) {
        return;
    }
    let Some(arguments) = child(call, ARG_LIST) else {
        return;
    };
    if tokens(&arguments).any(|token| token.kind() == DISTINCT_KW) {
        return;
    }
    let values: Vec<SyntaxNode> = arguments.children().collect();
    let [argument] = values.as_slice() else {
        return;
    };
    if argument.kind() != COLUMN_REF {
        return;
    }
    let Some(found) = table_column(stmt, argument) else {
        return;
    };
    if found.column.nullable != Some(false) || found.source.as_ref().is_some_and(on_nullable_side) {
        return;
    }
    cx.report(
        COUNT_NOT_NULL_COLUMN,
        argument.text_range(),
        format!(
            "'{}' is NOT NULL, so COUNT({}) counts every row, as COUNT(*) does",
            found.column.name,
            argument.text()
        ),
    )
    .fix("Replace with COUNT(*)", || {
        Some(vec![TextEdit {
            range: argument.text_range(),
            text: "*".to_string(),
        }])
    })
    .emit();
}
