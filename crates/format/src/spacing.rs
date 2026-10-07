//! The space between two tokens on one line: none around a dot, inside parentheses and before a
//! comma, none between a function's name and its arguments, one around an operator and between
//! words.

use sql_syntax::SyntaxKind::{self, *};
use sql_syntax::SyntaxToken;

fn is_word(kind: SyntaxKind) -> bool {
    kind.is_identifier() || kind.is_keyword()
}

fn parent_kind(token: &SyntaxToken) -> Option<SyntaxKind> {
    token.parent().map(|parent| parent.kind())
}

/// Parentheses that hold a list after a word, with a space before them: `IN (`, `VALUES (`,
/// `users (id)`, `OVER (`.
fn spaced_parenthesis(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        PAREN_QUERY
            | PAREN_EXPR
            | ROW_EXPR
            | IN_LIST
            | NAME_LIST
            | TABLE_ELEMENT_LIST
            | INDEX_COLUMN_LIST
            | WINDOW_SPEC
            | FILTER_CLAUSE
            | WITHIN_GROUP_CLAUSE
            | CTE
            | COLUMN_CONSTRAINT
            | TABLE_CONSTRAINT
            | CONFLICT_TARGET
            | DERIVED_TABLE
            | EXISTS_EXPR
            | ENUM_VALUE_LIST
            | JSON_TABLE_COLUMNS
            | PARTITION_SELECTION
            | INDEX_HINT
            | MATCH_AGAINST_EXPR
            | ALIAS
    )
}

/// Parentheses right after a name: the arguments of a call, of a type, the parameters of a routine.
fn attached_parenthesis(kind: SyntaxKind) -> bool {
    matches!(kind, ARG_LIST | TYPE_ARGS | PARAM_LIST | CAST_EXPR)
}

/// The space between two tokens that stay on one line. `original` is the whitespace between them
/// as written, which decides only where nothing else does.
pub(crate) fn space(previous: &SyntaxToken, next: &SyntaxToken, original: &str) -> &'static str {
    let (before, after) = (previous.kind(), next.kind());
    let kept = || if original.is_empty() { "" } else { " " };
    if matches!(
        parent_kind(next),
        Some(LITERAL | TYPED_LITERAL | ACCOUNT_NAME | OPERATOR_NAME | PARAMETER | VARIABLE_REF)
    ) && parent_kind(previous) == parent_kind(next)
    {
        return kept();
    }
    if after == CUSTOM_DELIMITER {
        return " ";
    }
    if matches!(after, COMMA | SEMICOLON | RPAREN | RBRACKET | DOT) {
        return "";
    }
    if matches!(before, LPAREN | LBRACKET | DOT) {
        return "";
    }
    if before == DOUBLE_COLON || after == DOUBLE_COLON {
        return "";
    }
    if after == COLON && parent_kind(next) != Some(PARAMETER) {
        return "";
    }
    if before == COLON {
        return if parent_kind(previous) == Some(INDEX_EXPR) {
            ""
        } else {
            " "
        };
    }
    if before == AT || after == AT {
        return kept();
    }
    if after == LPAREN {
        let holder = parent_kind(next);
        if is_word(before) && holder.is_some_and(attached_parenthesis) {
            return "";
        }
        if matches!(before, ROW_KW | ARRAY_KW) && holder == Some(ROW_EXPR) {
            return "";
        }
        let alias_columns = holder == Some(NAME_LIST)
            && next
                .parent()
                .and_then(|list| list.parent())
                .is_some_and(|owner| owner.kind() == ALIAS);
        if is_word(before) && (alias_columns || !holder.is_some_and(spaced_parenthesis)) {
            return kept();
        }
        return " ";
    }
    if after == LBRACKET {
        return if is_word(before) || matches!(before, RPAREN | RBRACKET) {
            ""
        } else {
            " "
        };
    }
    let glues = next
        .text()
        .starts_with(|character: char| "+-*/<>=~!@#%^&|`?".contains(character));
    if parent_kind(previous) == Some(PREFIX_EXPR)
        && !is_word(before)
        && !glues
        && previous.next_sibling_or_token().is_some()
        && previous
            .parent()
            .and_then(|parent| parent.first_token())
            .is_some_and(|first| first == *previous)
    {
        return "";
    }
    if parent_kind(previous) == Some(TABLE_OPTION) && (before == EQ || after == EQ) {
        return kept();
    }
    " "
}
