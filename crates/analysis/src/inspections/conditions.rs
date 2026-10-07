//! Conditions that do not do what they say: a `DELETE` or `UPDATE` without any, a comparison with
//! `NULL` that is never true, a condition whose outcome is fixed, `LIKE` without a wildcard, tables
//! joined by a comma that nothing links, and `NOT IN` over a subquery that may give `NULL`, which
//! then matches nothing.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxNode, SyntaxToken, TextRange};

use super::tree::{is_null, on_nullable_side, own_descendants, string_literal, strip_parens, table_column};
use super::{
    CONSTANT_CONDITION, Cx, IMPLICIT_CROSS_JOIN, LIKE_WITHOUT_WILDCARD, MISSING_WHERE, NOT_IN_NULLABLE,
    NULL_COMPARISON, Stmt,
};
use crate::ast::{child, children, inner_query, tokens};
use crate::rename::TextEdit;
use crate::resolve::{Referent, Resolution};

pub(super) fn run(cx: &Cx, stmt: &Stmt) {
    if !stmt.clean {
        return;
    }
    for node in stmt.node.descendants() {
        if !cx.wants(node.text_range()) {
            continue;
        }
        match node.kind() {
            DELETE_STMT | UPDATE_STMT if cx.on(MISSING_WHERE) => missing_where(cx, &node),
            BINARY_EXPR => {
                if cx.on(NULL_COMPARISON) {
                    null_comparison(cx, &node);
                }
                if cx.on(CONSTANT_CONDITION) {
                    constant_condition(cx, &node);
                }
            }
            CASE_EXPR if cx.on(NULL_COMPARISON) => case_when_null(cx, &node),
            LIKE_EXPR if cx.on(LIKE_WITHOUT_WILDCARD) => like_without_wildcard(cx, &node),
            SELECT if cx.on(IMPLICIT_CROSS_JOIN) => implicit_cross_join(cx, stmt, &node),
            IN_EXPR if cx.on(NOT_IN_NULLABLE) => not_in_nullable(cx, stmt, &node),
            _ => {}
        }
    }
}

fn missing_where(cx: &Cx, statement: &SyntaxNode) {
    if child(statement, WHERE_CLAUSE).is_some() || child(statement, LIMIT_CLAUSE).is_some() {
        return;
    }
    let joined = own_descendants(statement).any(|node| {
        node.kind() == ON_CLAUSE
            || (node.kind() == USING_CLAUSE && node.parent().is_some_and(|parent| parent.kind() == JOIN_EXPR))
    });
    if joined {
        return;
    }
    let Some(keyword) = statement.first_token() else {
        return;
    };
    let target = statement
        .descendants()
        .find(|node| node.kind() == QUALIFIED_NAME)
        .filter(|name| !super::tree::nested_in_query(name, statement));
    let Some(target) = target else {
        return;
    };
    let range = TextRange::new(keyword.text_range().start(), target.text_range().end());
    let message = if statement.kind() == DELETE_STMT {
        format!("DELETE without WHERE removes every row of '{}'", target.text())
    } else {
        format!("UPDATE without WHERE changes every row of '{}'", target.text())
    };
    cx.report(MISSING_WHERE, range, message).emit();
}

fn operator(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| !token.kind().is_trivia())
}

fn null_comparison(cx: &Cx, node: &SyntaxNode) {
    let Some(operator) = operator(node) else {
        return;
    };
    if !matches!(operator.kind(), EQ | NEQ | BANG_EQ) {
        return;
    }
    let operands: Vec<SyntaxNode> = node.children().collect();
    let [left, right] = operands.as_slice() else {
        return;
    };
    let other = match (is_null(left), is_null(right)) {
        (false, true) => left,
        (true, false) => right,
        _ => return,
    };
    let negated = operator.kind() != EQ;
    let test = if negated { "IS NOT NULL" } else { "IS NULL" };
    cx.report(
        NULL_COMPARISON,
        node.text_range(),
        format!(
            "A comparison with NULL through {} is never true; use {test}",
            operator.text()
        ),
    )
    .fix(format!("Replace with {test}"), || {
        Some(vec![TextEdit {
            range: node.text_range(),
            text: format!("{} {test}", other.text()),
        }])
    })
    .emit();
}

/// Whether a node stands where its value decides which rows a statement takes or which branch a
/// `CASE` follows, as opposed to a value of the select list.
fn in_condition(node: &SyntaxNode) -> bool {
    for ancestor in node.ancestors().skip(1) {
        match ancestor.kind() {
            WHERE_CLAUSE | HAVING_CLAUSE | ON_CLAUSE | WHEN_CLAUSE => return true,
            BINARY_EXPR | PREFIX_EXPR | PAREN_EXPR => {}
            _ => return false,
        }
    }
    false
}

/// A number literal's value.
fn number(node: &SyntaxNode) -> Option<f64> {
    let node = strip_parens(node);
    let token = node.first_token().filter(|_| node.kind() == LITERAL)?;
    if !matches!(token.kind(), INT_NUMBER | FLOAT_NUMBER) {
        return None;
    }
    token.text().replace('_', "").parse().ok()
}

/// `1 = 1`, `0 = 1` and the like: what query builders write for a condition with nothing in it
/// (no filters yet, or an empty list for `IN`), so the comparison is meant.
fn builder_idiom(operator: &SyntaxToken, left: &SyntaxNode, right: &SyntaxNode) -> bool {
    let bit = |node: &SyntaxNode| {
        let node = strip_parens(node);
        node.kind() == LITERAL
            && node
                .first_token()
                .is_some_and(|token| token.kind() == INT_NUMBER && matches!(token.text(), "0" | "1"))
    };
    matches!(operator.kind(), EQ | EQ_EQ | NEQ | BANG_EQ) && bit(left) && bit(right)
}

/// A comparison of two numbers, or of a column with itself, in a condition: `1 = 1` always holds,
/// `1 = 0` never does, and `a = a` holds wherever `a` is not NULL.
fn constant_condition(cx: &Cx, node: &SyntaxNode) {
    let Some(operator) = operator(node) else {
        return;
    };
    let operands: Vec<SyntaxNode> = node.children().collect();
    let [left, right] = operands.as_slice() else {
        return;
    };
    if !in_condition(node) || builder_idiom(&operator, left, right) {
        return;
    }
    let outcome = match (number(left), number(right)) {
        (Some(left), Some(right)) => match operator.kind() {
            EQ | EQ_EQ => left == right,
            NEQ | BANG_EQ => left != right,
            LT => left < right,
            GT => left > right,
            LTE => left <= right,
            GTE => left >= right,
            _ => return,
        },
        _ => {
            let (left, right) = (strip_parens(left), strip_parens(right));
            let same = left.kind() == COLUMN_REF
                && right.kind() == COLUMN_REF
                && crate::ast::compact(&left).eq_ignore_ascii_case(&crate::ast::compact(&right));
            if !same || !matches!(operator.kind(), EQ | EQ_EQ | LTE | GTE | NEQ | BANG_EQ | LT | GT) {
                return;
            }
            let holds = matches!(operator.kind(), EQ | EQ_EQ | LTE | GTE);
            let message = if holds {
                format!("'{}' always holds where '{}' is not NULL", node.text(), left.text())
            } else {
                format!("'{}' never holds", node.text())
            };
            cx.report(CONSTANT_CONDITION, node.text_range(), message).emit();
            return;
        }
    };
    let message = if outcome {
        format!("'{}' always holds", node.text())
    } else {
        format!("'{}' never holds", node.text())
    };
    cx.report(CONSTANT_CONDITION, node.text_range(), message).emit();
}

/// `CASE x WHEN NULL` compares `x = NULL`, which never matches.
fn case_when_null(cx: &Cx, node: &SyntaxNode) {
    let Some(subject) = node.children().next().filter(|first| first.kind() != WHEN_CLAUSE) else {
        return;
    };
    for when in children(node, WHEN_CLAUSE) {
        let Some(value) = when.children().next() else {
            continue;
        };
        if is_null(&value) {
            cx.report(
                NULL_COMPARISON,
                value.text_range(),
                format!(
                    "CASE {} WHEN NULL never matches; use CASE WHEN {} IS NULL",
                    subject.text(),
                    subject.text()
                ),
            )
            .emit();
        }
    }
}

fn like_without_wildcard(cx: &Cx, node: &SyntaxNode) {
    // SQLite's LIKE ignores the case of ASCII letters where `=` does not.
    if cx.dialect() == Dialect::Sqlite {
        return;
    }
    let words: Vec<SyntaxToken> = tokens(node).collect();
    if !words.iter().any(|token| token.kind() == LIKE_KW)
        || words
            .iter()
            .any(|token| matches!(token.kind(), ESCAPE_KW | ILIKE_KW | SOUNDS_KW))
    {
        return;
    }
    let operands: Vec<SyntaxNode> = node.children().collect();
    let [_, pattern] = operands.as_slice() else {
        return;
    };
    let Some(literal) = string_literal(pattern) else {
        return;
    };
    if literal.text().contains(['%', '_']) {
        return;
    }
    let negated = words.iter().any(|token| token.kind() == NOT_KW);
    let first = words
        .first()
        .filter(|token| token.kind() == NOT_KW)
        .unwrap_or(&words[0]);
    let Some(like) = words.iter().find(|token| token.kind() == LIKE_KW) else {
        return;
    };
    let replacement = if negated { "<>" } else { "=" };
    cx.report(
        LIKE_WITHOUT_WILDCARD,
        TextRange::new(first.text_range().start(), literal.text_range().end()),
        format!(
            "The pattern {} has no wildcard, so {} compares like {replacement}",
            literal.text(),
            if negated { "NOT LIKE" } else { "LIKE" }
        ),
    )
    .fix(format!("Replace with {replacement}"), || {
        Some(vec![TextEdit {
            range: TextRange::new(first.text_range().start(), like.text_range().end()),
            text: replacement.to_string(),
        }])
    })
    .emit();
}

/// The conjuncts of a condition: the operands of its `AND`s.
fn conjuncts(node: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
    let node = strip_parens(node);
    if node.kind() == BINARY_EXPR && operator(&node).is_some_and(|token| token.kind() == AND_KW) {
        for operand in node.children() {
            conjuncts(&operand, out);
        }
    } else {
        out.push(node);
    }
}

fn find(parent: &mut [usize], item: usize) -> usize {
    let mut root = item;
    while parent[root] != root {
        root = parent[root];
    }
    parent[item] = root;
    root
}

/// `FROM a, b` where no condition of `WHERE` names both: every row of one is paired with every row
/// of the other. Only plain tables are judged, and only when every column of the condition is
/// known, so a link the inspection cannot see is never taken for none.
fn implicit_cross_join(cx: &Cx, stmt: &Stmt, select: &SyntaxNode) {
    let Some(from) = child(select, FROM_CLAUSE) else {
        return;
    };
    let items: Vec<SyntaxNode> = from.children().collect();
    if items.len() < 2 || items.iter().any(|item| item.kind() != TABLE_REF) {
        return;
    }
    let group_of = |node: &SyntaxNode| {
        items
            .iter()
            .position(|item| node.ancestors().any(|ancestor| ancestor == *item))
    };
    let mut parent: Vec<usize> = (0..items.len()).collect();
    if let Some(condition) = child(select, WHERE_CLAUSE).and_then(|clause| clause.children().next()) {
        let mut parts = Vec::new();
        conjuncts(&condition, &mut parts);
        for part in parts {
            let mut groups = Vec::new();
            for reference in part.descendants().filter(|node| node.kind() == COLUMN_REF) {
                let Some(name) = children(&reference, NAME).last() else {
                    return;
                };
                match stmt.resolver.resolve_name(&name) {
                    Some(Resolution::Found(Referent::Column { source, .. })) => {
                        if let Some(group) = group_of(&source.node) {
                            groups.push(group);
                        }
                    }
                    Some(Resolution::Found(_)) => {}
                    _ => return,
                }
            }
            for pair in groups.windows(2) {
                let (first, second) = (find(&mut parent, pair[0]), find(&mut parent, pair[1]));
                parent[second] = first;
            }
        }
    }
    let first = find(&mut parent, 0);
    for (position, item) in items.iter().enumerate().skip(1) {
        if find(&mut parent, position) == first {
            continue;
        }
        let Some(comma) = item
            .first_token()
            .and_then(|token| token.prev_token())
            .into_iter()
            .flat_map(|token| std::iter::successors(Some(token), |token| token.prev_token()))
            .find(|token| !token.kind().is_trivia())
            .filter(|token| token.kind() == COMMA)
        else {
            continue;
        };
        let name = child(item, QUALIFIED_NAME)
            .map(|name| name.text().to_string())
            .unwrap_or_default();
        cx.report(
            IMPLICIT_CROSS_JOIN,
            item.text_range(),
            format!("'{name}' is joined by a comma without a condition that links it to the tables before it, which pairs every row with every row"),
        )
        .fix("Write it as CROSS JOIN", || {
            Some(vec![TextEdit {
                range: comma.text_range(),
                text: " CROSS JOIN".to_string(),
            }])
        })
        .emit();
    }
}

/// `x NOT IN (SELECT c ...)` where `c` may be NULL: one NULL makes the condition unknown for every
/// row, so nothing matches.
fn not_in_nullable(cx: &Cx, stmt: &Stmt, node: &SyntaxNode) {
    if !stmt.known || !tokens(node).any(|token| token.kind() == NOT_KW) {
        return;
    }
    let Some(subquery) = child(node, PAREN_QUERY) else {
        return;
    };
    let Some(select) = inner_query(&subquery).filter(|query| query.kind() == SELECT) else {
        return;
    };
    let items: Vec<SyntaxNode> = child(&select, SELECT_LIST)
        .map(|list| children(&list, SELECT_ITEM).collect())
        .unwrap_or_default();
    let [item] = items.as_slice() else {
        return;
    };
    let Some(column) = item.children().next().filter(|node| node.kind() == COLUMN_REF) else {
        return;
    };
    let Some(found) = table_column(stmt, &column) else {
        return;
    };
    let nullable = found.column.nullable == Some(true) || found.source.as_ref().is_some_and(on_nullable_side);
    if !nullable || excludes_null(&select, &column) {
        return;
    }
    let written = column.text().to_string();
    cx.report(
        NOT_IN_NULLABLE,
        subquery.text_range(),
        format!("'{written}' may be NULL, and NOT IN matches no row once the subquery gives a NULL"),
    )
    .fix(format!("Leave out the NULLs of '{written}'"), || {
        let condition = format!("{written} IS NOT NULL");
        match child(&select, WHERE_CLAUSE).and_then(|clause| clause.children().next()) {
            Some(existing) => {
                let has_or = existing
                    .descendants_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .any(|token| token.kind() == OR_KW);
                let mut edits = Vec::new();
                if has_or {
                    edits.push(TextEdit {
                        range: TextRange::empty(existing.text_range().start()),
                        text: "(".to_string(),
                    });
                }
                edits.push(TextEdit {
                    range: TextRange::empty(existing.text_range().end()),
                    text: format!("{} AND {condition}", if has_or { ")" } else { "" }),
                });
                Some(edits)
            }
            None => {
                let from = child(&select, FROM_CLAUSE)?;
                Some(vec![TextEdit {
                    range: TextRange::empty(from.text_range().end()),
                    text: format!(" WHERE {condition}"),
                }])
            }
        }
    })
    .emit();
}

/// Whether the subquery's `WHERE` already says the column is not NULL.
fn excludes_null(select: &SyntaxNode, column: &SyntaxNode) -> bool {
    let Some(condition) = child(select, WHERE_CLAUSE) else {
        return false;
    };
    let wanted = column.text().to_string().to_ascii_lowercase();
    condition
        .descendants()
        .filter(|node| node.kind() == IS_EXPR)
        .any(|test| {
            let negated = tokens(&test).any(|token| token.kind() == NOT_KW);
            let null = tokens(&test).any(|token| token.kind() == NULL_KW);
            negated
                && null
                && test
                    .children()
                    .next()
                    .is_some_and(|subject| subject.text().to_string().to_ascii_lowercase() == wanted)
        })
}
