//! What MySQL and MariaDB read differently from the standard: `||` is a logical `OR` unless
//! `PIPES_AS_CONCAT` is on, a double-quoted text is a string unless `ANSI_QUOTES` is on, a
//! subquery of `IN`, `ANY`, `SOME` or `ALL` may not have `LIMIT`, and the `ORDER BY` of a subquery
//! without `LIMIT` does not order anything.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxNode, SyntaxToken};

use super::tree::{string_literal, string_value, table_column, with_space_before};
use super::{Cx, DOUBLE_QUOTED_STRING, LIMIT_IN_SUBQUERY, ORDER_BY_IN_SUBQUERY, PIPES_AS_OR, QuickFix, Stmt};
use crate::ast::{child, compact, inner_query, is_query, tokens};
use crate::diagnostics::DiagnosticSeverity;
use crate::ident::{Ident, quote_name};
use crate::rename::TextEdit;
use crate::resolve::{Referent, Resolution};

pub(super) fn run(cx: &Cx, stmt: &Stmt) {
    if !stmt.clean || !matches!(cx.dialect(), Dialect::Mysql | Dialect::Mariadb) {
        return;
    }
    let pipes = cx.on(PIPES_AS_OR) && stmt.mode.has("PIPES_AS_CONCAT") == Some(false);
    let quotes = cx.on(DOUBLE_QUOTED_STRING) && stmt.mode.has("ANSI_QUOTES") == Some(false);
    for node in stmt.node.descendants() {
        if !cx.wants(node.text_range()) {
            continue;
        }
        match node.kind() {
            BINARY_EXPR if pipes => pipes_as_or(cx, stmt, &node),
            LITERAL if quotes => double_quoted(cx, stmt, &node),
            IN_EXPR | QUANTIFIED_EXPR if cx.on(LIMIT_IN_SUBQUERY) => limit_in_subquery(cx, &node),
            PAREN_QUERY if cx.on(ORDER_BY_IN_SUBQUERY) => order_by_in_subquery(cx, stmt, &node),
            _ => {}
        }
    }
}

fn is_pipes(node: &SyntaxNode) -> bool {
    node.kind() == BINARY_EXPR && tokens(node).any(|token| token.kind() == PIPE_PIPE)
}

/// The operands of a chain `a || b || c` and its operators.
fn chain(node: &SyntaxNode, operands: &mut Vec<SyntaxNode>, operators: &mut Vec<SyntaxToken>) {
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Node(inner) if is_pipes(&inner) => chain(&inner, operands, operators),
            SyntaxElement::Node(inner) => operands.push(inner),
            SyntaxElement::Token(token) if token.kind() == PIPE_PIPE => operators.push(token),
            SyntaxElement::Token(_) => {}
        }
    }
}

const STRING_FUNCTIONS: &[&str] = &[
    "concat",
    "concat_ws",
    "lower",
    "upper",
    "lcase",
    "ucase",
    "trim",
    "ltrim",
    "rtrim",
    "substring",
    "substr",
    "left",
    "right",
    "lpad",
    "rpad",
    "replace",
    "repeat",
    "reverse",
    "format",
    "date_format",
    "group_concat",
    "char",
    "hex",
    "md5",
    "sha1",
    "sha2",
    "uuid",
];

/// Whether an operand is text, which shows `||` was meant to join strings.
fn is_text(stmt: &Stmt, operand: &SyntaxNode) -> bool {
    if string_literal(operand).is_some() {
        return true;
    }
    match operand.kind() {
        FUNCTION_CALL => child(operand, QUALIFIED_NAME)
            .is_some_and(|name| STRING_FUNCTIONS.contains(&compact(&name).to_ascii_lowercase().as_str())),
        COLUMN_REF => table_column(stmt, operand).is_some_and(|found| {
            let data_type = found.column.data_type.as_deref().unwrap_or("").to_ascii_lowercase();
            ["char", "text", "enum", "set("]
                .iter()
                .any(|word| data_type.contains(word))
        }),
        _ => false,
    }
}

fn pipes_as_or(cx: &Cx, stmt: &Stmt, node: &SyntaxNode) {
    if !is_pipes(node) || node.parent().is_some_and(|parent| is_pipes(&parent)) {
        return;
    }
    let mut operands = Vec::new();
    let mut operators = Vec::new();
    chain(node, &mut operands, &mut operators);
    if !operands.iter().any(|operand| is_text(stmt, operand)) {
        return;
    }
    let joined: Vec<String> = operands.iter().map(|operand| operand.text().to_string()).collect();
    let concat = format!("CONCAT({})", joined.join(", "));
    for operator in operators {
        cx.report(
            PIPES_AS_OR,
            operator.text_range(),
            format!(
                "{} reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on",
                cx.dialect()
            ),
        )
        .fix("Join the strings with CONCAT()", || {
            Some(vec![TextEdit {
                range: node.text_range(),
                text: concat.clone(),
            }])
        })
        .emit();
    }
}

/// A double-quoted string in single quotes, which mean the same whatever the modes.
fn single_quoted(text: &str) -> String {
    let body = if text.len() >= 2 { &text[1..text.len() - 1] } else { "" };
    let mut out = String::from("'");
    let mut chars = body.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' => {
                out.push('\\');
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '"' if chars.peek() == Some(&'"') => {
                chars.next();
                out.push('"');
            }
            '\'' => out.push_str("''"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn double_quoted(cx: &Cx, stmt: &Stmt, literal: &SyntaxNode) {
    let Some(token) = literal
        .first_token()
        .filter(|token| token.kind() == STRING && token.text().starts_with('"'))
    else {
        return;
    };
    let value = string_value(&token, cx.dialect());
    let column = stmt.known
        && !value.is_empty()
        && matches!(
            stmt.resolver
                .resolve_unqualified(&stmt.resolver.scope(literal), &Ident::new(value.clone())),
            Resolution::Found(Referent::Column { .. })
        );
    let single = single_quoted(token.text());
    let quote_fix = QuickFix {
        title: "Use single quotes".to_string(),
        edits: vec![TextEdit {
            range: token.text_range(),
            text: single.clone(),
        }],
    };
    if column {
        let name = quote_name(&value, cx.target);
        let name = if name == value { format!("`{value}`") } else { name };
        cx.report(
            DOUBLE_QUOTED_STRING,
            token.text_range(),
            format!(
                "{} is a string in {}, not the column '{value}'; write a column name bare or in backticks",
                token.text(),
                cx.dialect()
            ),
        )
        .severity(DiagnosticSeverity::Warning)
        .fixes(|| {
            vec![
                QuickFix {
                    title: format!("Read the column '{value}'"),
                    edits: vec![TextEdit {
                        range: token.text_range(),
                        text: name,
                    }],
                },
                quote_fix,
            ]
        })
        .emit();
        return;
    }
    cx.report(
        DOUBLE_QUOTED_STRING,
        token.text_range(),
        "A double-quoted string is a name where ANSI_QUOTES is on, as in standard SQL; single quotes are a string everywhere",
    )
    .fixes(|| vec![quote_fix])
    .emit();
}

/// The query of a `PAREN_QUERY` and the clause a subquery may not have.
fn limit_in_subquery(cx: &Cx, node: &SyntaxNode) {
    let Some(subquery) = child(node, PAREN_QUERY) else {
        return;
    };
    let Some(query) = inner_query(&subquery) else {
        return;
    };
    let Some(limit) = child(&query, LIMIT_CLAUSE) else {
        return;
    };
    let what = if node.kind() == IN_EXPR {
        "IN"
    } else {
        "ANY, SOME and ALL"
    };
    cx.report(
        LIMIT_IN_SUBQUERY,
        limit.text_range(),
        format!("{} does not take LIMIT in a subquery of {what}", cx.dialect()),
    )
    .fix("Wrap the subquery in a derived table", || {
        Some(vec![TextEdit {
            range: query.text_range(),
            text: format!("SELECT * FROM ({}) AS limited", query.text()),
        }])
    })
    .emit();
}

fn order_by_in_subquery(cx: &Cx, stmt: &Stmt, subquery: &SyntaxNode) {
    let Some(parent) = subquery.parent() else {
        return;
    };
    let nested = matches!(parent.kind(), DERIVED_TABLE | COMPOUND_SELECT)
        || !(parent.kind().name().ends_with("_STMT") || is_query(parent.kind()) || parent.kind() == CTE);
    if !nested {
        return;
    }
    let Some(query) = inner_query(subquery) else {
        return;
    };
    let Some(order) = child(&query, ORDER_BY_CLAUSE) else {
        return;
    };
    if [LIMIT_CLAUSE, FETCH_CLAUSE, OFFSET_CLAUSE]
        .iter()
        .any(|kind| child(&query, *kind).is_some())
    {
        return;
    }
    if cx.dialect() == Dialect::Mysql && parent.kind() == DERIVED_TABLE && order_propagates(stmt, &parent) {
        return;
    }
    cx.report(
        ORDER_BY_IN_SUBQUERY,
        order.text_range(),
        "ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it",
    )
    .fix("Remove ORDER BY", || {
        Some(vec![TextEdit {
            range: with_space_before(&order),
            text: String::new(),
        }])
    })
    .emit();
}

/// MySQL hands the order of a derived table on to a query that has it as its only table and
/// neither groups, aggregates, removes duplicates nor orders by itself.
fn order_propagates(stmt: &Stmt, derived: &SyntaxNode) -> bool {
    let Some(from) = derived.parent().filter(|parent| parent.kind() == FROM_CLAUSE) else {
        return false;
    };
    let Some(select) = from.parent().filter(|parent| parent.kind() == SELECT) else {
        return false;
    };
    let only = from.children().count() == 1;
    let plain = [GROUP_BY_CLAUSE, HAVING_CLAUSE, ORDER_BY_CLAUSE]
        .iter()
        .all(|kind| child(&select, *kind).is_none())
        && !tokens(&select).any(|token| token.kind() == DISTINCT_KW)
        && child(&select, SELECT_LIST).is_none_or(|list| !super::grouping::has_aggregate(stmt, &list));
    only && plain
}
