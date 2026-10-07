//! Strings a column's type cannot read: `'abc'` for a number, `'2024-13-01'` for a date, a value
//! an enum does not have. Only what is certain: a string any reading of the type would refuse.
//! PostgreSQL rejects such a value wherever it meets the column; MySQL and MariaDB reject it when a
//! strict mode is on and the value is written, and only warn when it is compared; SQLite stores
//! anything, so there it is a warning.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, SyntaxToken};

use super::tree::{TableColumn, string_literal, string_value, strip_parens, table_column};
use super::{Cx, INVALID_LITERAL, Stmt, UNKNOWN_ENUM_VALUE};
use crate::diagnostics::DiagnosticSeverity;
use crate::rename::TextEdit;

/// What a type reads a string as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TypeClass {
    Integer,
    Decimal,
    Date,
    Timestamp,
    Time,
    Enum(Vec<String>),
    Other,
}

/// Where a value meets a column, which decides whether the server refuses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Use {
    Compare,
    Write,
}

/// The values of MySQL's `enum('a','b')`.
fn enum_values(text: &str) -> Option<Vec<String>> {
    let inner = text.trim();
    let open = inner.find('(')?;
    if !inner[..open].trim().eq_ignore_ascii_case("enum") || !inner.ends_with(')') {
        return None;
    }
    let body = &inner[open + 1..inner.len() - 1];
    let mut values = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\'' {
            continue;
        }
        let mut value = String::new();
        loop {
            match chars.next() {
                Some('\'') if chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                Some('\'') | None => break,
                Some('\\') => {
                    if let Some(next) = chars.next() {
                        value.push(next);
                    }
                }
                Some(other) => value.push(other),
            }
        }
        values.push(value);
    }
    Some(values)
}

pub(super) fn classify(stmt: &Stmt, data_type: &str) -> TypeClass {
    if let Some(values) = enum_values(data_type) {
        return TypeClass::Enum(values);
    }
    let lower = data_type.trim().to_ascii_lowercase();
    if lower.ends_with("[]") || lower.starts_with('_') {
        return TypeClass::Other;
    }
    let base = lower.split('(').next().unwrap_or("").trim();
    let words: Vec<&str> = base
        .split_whitespace()
        .filter(|word| !matches!(*word, "unsigned" | "signed" | "zerofill"))
        .collect();
    let name = words.join(" ");
    match name.as_str() {
        "int" | "integer" | "int2" | "int4" | "int8" | "smallint" | "bigint" | "tinyint" | "mediumint" | "serial"
        | "bigserial" | "smallserial" | "serial4" | "serial8" => TypeClass::Integer,
        "numeric" | "decimal" | "dec" | "real" | "float" | "float4" | "float8" | "double" | "double precision" => {
            TypeClass::Decimal
        }
        "date" => TypeClass::Date,
        "datetime" | "timestamp" | "timestamptz" | "timestamp with time zone" | "timestamp without time zone" => {
            TypeClass::Timestamp
        }
        "time" | "timetz" | "time with time zone" | "time without time zone" => TypeClass::Time,
        _ => {
            if stmt.catalog.dialect() != Dialect::Postgres {
                return TypeClass::Other;
            }
            let simple = name.rsplit('.').next().unwrap_or(&name).trim_matches('"');
            let Some((_, user_type)) = stmt.catalog.user_type(None, simple) else {
                return TypeClass::Other;
            };
            match user_type.kind {
                sql_catalog::model::TypeKind::Enum => TypeClass::Enum(user_type.values.clone()),
                sql_catalog::model::TypeKind::Domain => match &user_type.base_type {
                    Some(base) if base.to_ascii_lowercase() != name => match classify(stmt, base) {
                        TypeClass::Enum(_) => TypeClass::Other,
                        other => other,
                    },
                    _ => TypeClass::Other,
                },
                _ => TypeClass::Other,
            }
        }
    }
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Digits with single underscores between them, as PostgreSQL 16 and later read a number.
fn grouped_digits(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('_')
        && !text.ends_with('_')
        && !text.contains("__")
        && text.bytes().all(|byte| byte.is_ascii_digit() || byte == b'_')
}

fn is_integer(text: &str, dialect: Dialect) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    if dialect == Dialect::Postgres {
        let lower = body.to_ascii_lowercase();
        for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
            if let Some(rest) = lower.strip_prefix(prefix) {
                return !rest.is_empty()
                    && rest
                        .split('_')
                        .all(|group| !group.is_empty() && group.chars().all(|digit| digit.is_digit(radix)));
            }
        }
        return grouped_digits(body);
    }
    digits(body)
}

fn is_decimal(text: &str, dialect: Dialect) -> bool {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let lower = body.to_ascii_lowercase();
    if dialect == Dialect::Postgres && matches!(lower.as_str(), "nan" | "infinity" | "inf") {
        return true;
    }
    let (mantissa, exponent) = match lower.find('e') {
        Some(at) => (&lower[..at], Some(&lower[at + 1..])),
        None => (lower.as_str(), None),
    };
    let numeric = |part: &str| {
        if dialect == Dialect::Postgres {
            grouped_digits(part)
        } else {
            digits(part)
        }
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (whole.is_empty() || numeric(whole))
                && (fraction.is_empty() || numeric(fraction))
                && !(whole.is_empty() && fraction.is_empty())
        }
        None => numeric(mantissa),
    };
    let exponent_ok = exponent.is_none_or(|exponent| digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent)));
    mantissa_ok && exponent_ok || (dialect == Dialect::Postgres && is_integer(text, dialect))
}

fn days_in(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// A date written `YYYY-MM-DD` at the start of the text whose month or day cannot be; a zero part
/// is left to the modes of MySQL.
fn impossible_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let number = |range: std::ops::Range<usize>| text.get(range).filter(|part| digits(part))?.parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (number(0..4), number(5..7), number(8..10)) else {
        return false;
    };
    if bytes.get(10).is_some_and(|next| next.is_ascii_digit()) {
        return false;
    }
    if month == 0 || day == 0 {
        return false;
    }
    month > 12 || day > days_in(year, month)
}

/// Why a type cannot read a string, or `None` when it may.
fn refusal(class: &TypeClass, value: &str, dialect: Dialect) -> Option<&'static str> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    let has_digit = trimmed.bytes().any(|byte| byte.is_ascii_digit());
    match class {
        TypeClass::Integer => {
            let readable =
                is_integer(trimmed, dialect) || (dialect != Dialect::Postgres && is_decimal(trimmed, dialect));
            (!readable).then_some("a number")
        }
        TypeClass::Decimal => (!is_decimal(trimmed, dialect)).then_some("a number"),
        TypeClass::Date | TypeClass::Timestamp => {
            let special = dialect == Dialect::Postgres
                && matches!(
                    lower.as_str(),
                    "epoch" | "infinity" | "-infinity" | "now" | "today" | "tomorrow" | "yesterday"
                );
            if special {
                return None;
            }
            if !has_digit || impossible_date(trimmed) {
                return Some(if *class == TypeClass::Date {
                    "a date"
                } else {
                    "a date and time"
                });
            }
            None
        }
        TypeClass::Time => {
            let special = dialect == Dialect::Postgres && matches!(lower.as_str(), "now" | "allballs");
            (!has_digit && !special).then_some("a time")
        }
        TypeClass::Enum(_) | TypeClass::Other => None,
    }
}

/// The severity of a refused value: an error where the server refuses it.
fn severity(stmt: &Stmt, used: Use, class: &TypeClass) -> DiagnosticSeverity {
    let temporal = matches!(class, TypeClass::Date | TypeClass::Timestamp | TypeClass::Time);
    match stmt.catalog.dialect() {
        Dialect::Postgres => DiagnosticSeverity::Error,
        // MySQL 8 refuses a date it cannot read even in a comparison; MariaDB compares it as a string.
        Dialect::Mysql if temporal => DiagnosticSeverity::Error,
        Dialect::Mysql | Dialect::Mariadb if used == Use::Write && stmt.mode.strict() == Some(true) => {
            DiagnosticSeverity::Error
        }
        _ => DiagnosticSeverity::Warning,
    }
}

/// Checks a string written to or compared with a column.
pub(super) fn check_value(cx: &Cx, stmt: &Stmt, column: &sql_catalog::model::Column, value: &SyntaxNode, used: Use) {
    let Some(token) = string_literal(value) else {
        return;
    };
    let Some(data_type) = column.data_type.as_deref() else {
        return;
    };
    let dialect = cx.dialect();
    let class = classify(stmt, data_type);
    let text = string_value(&token, dialect);
    if let TypeClass::Enum(values) = &class {
        if !cx.on(UNKNOWN_ENUM_VALUE) || values.is_empty() {
            return;
        }
        let found = |value: &str| {
            if dialect == Dialect::Postgres {
                values.iter().any(|known| known == value)
            } else {
                values
                    .iter()
                    .any(|known| known.trim_end().eq_ignore_ascii_case(value.trim_end()))
            }
        };
        if found(&text) || (dialect != Dialect::Postgres && text.is_empty()) {
            return;
        }
        let list: Vec<String> = values.iter().map(|value| format!("'{value}'")).collect();
        cx.report(
            UNKNOWN_ENUM_VALUE,
            token.text_range(),
            format!(
                "'{text}' is not a value of '{}', which takes {}",
                column.name,
                list.join(", ")
            ),
        )
        .severity(severity(stmt, used, &class))
        .fixes(|| enum_fixes(&token, &text, values))
        .emit();
        return;
    }
    if !cx.on(INVALID_LITERAL) {
        return;
    }
    let Some(expected) = refusal(&class, &text, dialect) else {
        return;
    };
    cx.report(
        INVALID_LITERAL,
        token.text_range(),
        format!(
            "'{text}' is not {expected}, which the column '{}' ({data_type}) holds",
            column.name
        ),
    )
    .severity(severity(stmt, used, &class))
    .emit();
}

fn enum_fixes(token: &SyntaxToken, text: &str, values: &[String]) -> Vec<super::QuickFix> {
    super::names::near_misses(text, values.iter().cloned())
        .into_iter()
        .map(|value| super::QuickFix {
            title: format!("Change to '{value}'"),
            edits: vec![TextEdit {
                range: token.text_range(),
                text: format!("'{}'", value.replace('\'', "''")),
            }],
        })
        .collect()
}

/// Comparisons of a column with a string: `=`, `<>`, `<` and the others, and `IN (...)`.
pub(super) fn run_comparisons(cx: &Cx, stmt: &Stmt) {
    if !stmt.known || !stmt.clean || (!cx.on(INVALID_LITERAL) && !cx.on(UNKNOWN_ENUM_VALUE)) {
        return;
    }
    for node in stmt.node.descendants() {
        match node.kind() {
            BINARY_EXPR => {
                let comparison = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .any(|token| matches!(token.kind(), EQ | NEQ | BANG_EQ | LT | GT | LTE | GTE));
                let operands: Vec<SyntaxNode> = node.children().collect();
                let [left, right] = operands.as_slice() else {
                    continue;
                };
                if !comparison {
                    continue;
                }
                for (column, value) in [(left, right), (right, left)] {
                    compare(cx, stmt, column, value);
                }
            }
            IN_EXPR => {
                let Some(subject) = node.children().next() else {
                    continue;
                };
                let Some(list) = node.children().find(|child| child.kind() == IN_LIST) else {
                    continue;
                };
                for value in list.children() {
                    compare(cx, stmt, &subject, &value);
                }
            }
            _ => {}
        }
    }
}

fn compare(cx: &Cx, stmt: &Stmt, column: &SyntaxNode, value: &SyntaxNode) {
    let column = strip_parens(column);
    if column.kind() != COLUMN_REF || string_literal(value).is_none() {
        return;
    }
    if let Some(TableColumn { column, .. }) = table_column(stmt, &column) {
        check_value(cx, stmt, column, value, Use::Compare);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_dates_and_enums_as_the_servers_do() {
        assert!(is_integer("-12", Dialect::Mysql));
        assert!(is_integer("1_000", Dialect::Postgres));
        assert!(!is_integer("1_000", Dialect::Mysql));
        assert!(is_integer("0x1F", Dialect::Postgres));
        assert!(is_decimal("1.5e3", Dialect::Mysql));
        assert!(is_decimal(".5", Dialect::Mysql));
        assert!(!is_decimal(".", Dialect::Mysql));
        assert!(is_decimal("NaN", Dialect::Postgres));
        assert_eq!(refusal(&TypeClass::Integer, "abc", Dialect::Mysql), Some("a number"));
        assert_eq!(refusal(&TypeClass::Integer, "1.5", Dialect::Mysql), None);
        assert_eq!(refusal(&TypeClass::Integer, "1.5", Dialect::Postgres), Some("a number"));
        assert_eq!(refusal(&TypeClass::Integer, " 7 ", Dialect::Postgres), None);
        assert_eq!(refusal(&TypeClass::Date, "2024-02-30", Dialect::Mysql), Some("a date"));
        assert_eq!(refusal(&TypeClass::Date, "2024-02-29", Dialect::Mysql), None);
        assert_eq!(refusal(&TypeClass::Date, "0000-00-00", Dialect::Mysql), None);
        assert_eq!(refusal(&TypeClass::Date, "today", Dialect::Postgres), None);
        assert_eq!(refusal(&TypeClass::Date, "today", Dialect::Mysql), Some("a date"));
        assert_eq!(
            enum_values("enum('a','it''s','b\\'c')"),
            Some(vec!["a".to_string(), "it's".to_string(), "b'c".to_string()])
        );
    }
}
