//! What the feature table reports as inspections: syntax a dialect or version does not have
//! (`unsupported-syntax`), syntax it deprecates (`deprecated-syntax`) and reserved words used as
//! names (`reserved-word`), each with the rewrite the server documents where there is one.

use sql_syntax::SyntaxKind::{self, *};
use sql_syntax::{Dialect, FeatureSeverity, SyntaxElement, SyntaxNode, SyntaxToken, TextRange, check_features};

use super::{Cx, DEPRECATED_SYNTAX, RESERVED_WORD, UNSUPPORTED_SYNTAX};
use crate::ast::{child, children, compact, tokens};
use crate::ident::{Ident, unquote};
use crate::rename::TextEdit;

pub(super) fn run(cx: &Cx) {
    if !cx.on(UNSUPPORTED_SYNTAX) && !cx.on(DEPRECATED_SYNTAX) && !cx.on(RESERVED_WORD) {
        return;
    }
    for finding in check_features(&cx.root, cx.target) {
        if !cx.wants(finding.range) {
            continue;
        }
        if finding.feature == sql_syntax::RESERVED_WORD {
            cx.report(RESERVED_WORD, finding.range, finding.message)
                .fix("Quote the name", || {
                    quote_reserved(cx, finding.range).map(|edit| vec![edit])
                })
                .emit();
            continue;
        }
        let id = match finding.severity {
            FeatureSeverity::Warning => DEPRECATED_SYNTAX,
            FeatureSeverity::Error => UNSUPPORTED_SYNTAX,
        };
        let feature = finding.feature;
        let mut pending = cx.report(id, finding.range, finding.message).feature(feature);
        if cx.fixes() {
            if let Some((title, edits)) = rewrite(cx, feature, finding.range) {
                pending = pending.fix(title, || Some(edits));
            }
        }
        pending.emit();
    }
}

fn element_at(cx: &Cx, range: TextRange) -> SyntaxElement {
    cx.root.covering_element(range)
}

fn token_at(cx: &Cx, range: TextRange) -> Option<SyntaxToken> {
    element_at(cx, range)
        .into_token()
        .filter(|token| token.text_range() == range)
}

fn node_at(cx: &Cx, range: TextRange, kind: SyntaxKind) -> Option<SyntaxNode> {
    let element = element_at(cx, range);
    let start = match element {
        SyntaxElement::Node(node) => node,
        SyntaxElement::Token(token) => token.parent()?,
    };
    start.ancestors().find(|node| node.kind() == kind)
}

fn replace(range: TextRange, text: impl Into<String>) -> Vec<TextEdit> {
    vec![TextEdit {
        range,
        text: text.into(),
    }]
}

/// A token and the whitespace before it, so removing it leaves one space where there were two.
fn removal(token: &SyntaxToken) -> TextRange {
    let start = token
        .prev_token()
        .filter(|previous| previous.kind() == WHITESPACE)
        .map_or(token.text_range().start(), |previous| previous.text_range().start());
    TextRange::new(start, token.text_range().end())
}

/// A reserved word as a quoted name; PostgreSQL folds the unquoted word to lower case first, so
/// the quoted name means what the bare one would have.
fn quote_reserved(cx: &Cx, range: TextRange) -> Option<TextEdit> {
    let token = token_at(cx, range)?;
    let name = Ident::of_token(&token, cx.dialect()).text;
    let quoted = match cx.dialect() {
        Dialect::Mysql | Dialect::Mariadb => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    };
    Some(TextEdit { range, text: quoted })
}

/// The rewrite of a row's syntax into what the target takes, with its title.
fn rewrite(cx: &Cx, feature: &str, range: TextRange) -> Option<(String, Vec<TextEdit>)> {
    let simple = |from: &str, to: &str| Some((format!("Replace {from} with {to}"), replace(range, to.to_string())));
    match feature {
        "double-ampersand" => simple("&&", "AND"),
        "double-pipe" if matches!(cx.dialect(), Dialect::Mysql) => simple("||", "OR"),
        "double-equals" => simple("==", "="),
        "null-safe-equal" => simple("<=>", "IS NOT DISTINCT FROM"),
        "autoincrement" => simple("AUTOINCREMENT", "AUTO_INCREMENT"),
        "insert-value-keyword" => simple("VALUE", "VALUES"),
        "start-transaction" => simple("START", "BEGIN"),
        "end-transaction" => simple("END", "COMMIT"),
        "backtick-identifiers" => {
            let token = token_at(cx, range)?;
            let (text, _) = unquote(BACKTICK_IDENT, token.text());
            Some((
                "Quote with double quotes".to_string(),
                replace(range, format!("\"{}\"", text.replace('"', "\"\""))),
            ))
        }
        "string-aliases" => {
            let token = token_at(cx, range)?;
            let (text, _) = unquote(STRING, token.text());
            let quoted = match cx.dialect() {
                Dialect::Mysql | Dialect::Mariadb => format!("`{}`", text.replace('`', "``")),
                _ => format!("\"{}\"", text.replace('"', "\"\"")),
            };
            Some(("Quote the alias as a name".to_string(), replace(range, quoted)))
        }
        "is-distinct-from" => {
            let node = node_at(cx, range, IS_EXPR)?;
            let operands: Vec<SyntaxNode> = node.children().collect();
            let [left, right] = operands.as_slice() else {
                return None;
            };
            let negated = tokens(&node).any(|token| token.kind() == NOT_KW);
            let text = if negated {
                format!("{} <=> {}", left.text(), right.text())
            } else {
                format!("NOT ({} <=> {})", left.text(), right.text())
            };
            Some(("Rewrite with <=>".to_string(), replace(node.text_range(), text)))
        }
        "cast-operator" => {
            let node = node_at(cx, range, TYPECAST_EXPR)?;
            let operand = node.children().next()?;
            let target = child(&node, TYPE)?;
            Some((
                "Rewrite as CAST".to_string(),
                replace(
                    node.text_range(),
                    format!("CAST({} AS {})", operand.text(), target.text()),
                ),
            ))
        }
        "limit-with-comma" => {
            let node = node_at(cx, range, LIMIT_CLAUSE)?;
            let values: Vec<SyntaxNode> = node.children().collect();
            let [offset, count] = values.as_slice() else {
                return None;
            };
            let keyword = tokens(&node).find(|token| token.kind() == LIMIT_KW)?;
            Some((
                "Rewrite as LIMIT ... OFFSET".to_string(),
                replace(
                    node.text_range(),
                    format!("{} {} OFFSET {}", keyword.text(), count.text(), offset.text()),
                ),
            ))
        }
        "cast-to-integer" | "cast-to-text" if matches!(cx.dialect(), Dialect::Mysql | Dialect::Mariadb) => {
            let node = node_at(cx, range, TYPE)?;
            let name = crate::ast::parts(&child(&node, QUALIFIED_NAME)?, cx.dialect())
                .pop()?
                .ident
                .text
                .to_ascii_lowercase();
            let text = match name.as_str() {
                "varchar" => format!(
                    "CHAR{}",
                    child(&node, TYPE_ARGS)
                        .map(|args| args.text().to_string())
                        .unwrap_or_default()
                ),
                "text" => "CHAR".to_string(),
                "boolean" | "bool" => return None,
                _ if compact(&node).to_ascii_lowercase().contains("unsigned") => "UNSIGNED".to_string(),
                _ => "SIGNED".to_string(),
            };
            Some((format!("Cast to {text}"), replace(node.text_range(), text)))
        }
        "binary-operator" => {
            let node = node_at(cx, range, PREFIX_EXPR)?;
            let operand = node.children().next()?;
            Some((
                "Rewrite as CAST(... AS BINARY)".to_string(),
                replace(node.text_range(), format!("CAST({} AS BINARY)", operand.text())),
            ))
        }
        "zerofill" => {
            let token = token_at(cx, range)?;
            Some(("Remove ZEROFILL".to_string(), replace(removal(&token), "")))
        }
        "integer-display-width" => Some(("Remove the display width".to_string(), replace(range, ""))),
        "sql-calc-found-rows" => found_rows(cx, range),
        "values-function" => row_alias(cx, range),
        _ => None,
    }
}

/// `SELECT SQL_CALC_FOUND_ROWS ... LIMIT n; SELECT FOUND_ROWS();` becomes the query without the
/// modifier and a `COUNT(*)` of the same rows, as MySQL documents in place of both.
fn found_rows(cx: &Cx, range: TextRange) -> Option<(String, Vec<TextEdit>)> {
    let token = token_at(cx, range)?;
    let select = token.parent().filter(|parent| parent.kind() == SELECT)?;
    let mut edits = replace(removal(&token), "");
    let statement = select
        .ancestors()
        .find(|node| node.parent().is_some_and(|parent| parent.kind() == SOURCE_FILE))?;
    let next = statement.next_sibling();
    let counts_rows = next.as_ref().is_some_and(|next| {
        compact(next)
            .trim_end_matches(';')
            .trim()
            .eq_ignore_ascii_case("SELECT FOUND_ROWS()")
    });
    if let (true, Some(next)) = (counts_rows, next) {
        let from = child(&select, FROM_CLAUSE)?;
        let last = [WHERE_CLAUSE, GROUP_BY_CLAUSE, HAVING_CLAUSE]
            .iter()
            .rev()
            .find_map(|kind| child(&select, *kind))
            .unwrap_or_else(|| from.clone());
        let text = cx.root.text().to_string();
        let rows = &text[usize::from(from.text_range().start())..usize::from(last.text_range().end())];
        let count =
            if child(&select, GROUP_BY_CLAUSE).is_some() || tokens(&select).any(|token| token.kind() == DISTINCT_KW) {
                let list = child(&select, SELECT_LIST)?;
                let modifiers: String = tokens(&select)
                    .filter(|token| token.kind() == DISTINCT_KW)
                    .map(|token| format!("{} ", token.text()))
                    .collect();
                format!(
                    "SELECT COUNT(*) FROM (SELECT {modifiers}{} {rows}) AS counted",
                    list.text()
                )
            } else {
                format!("SELECT COUNT(*) {rows}")
            };
        let semicolon = tokens(&next).find(|token| matches!(token.kind(), SEMICOLON | CUSTOM_DELIMITER));
        let end = semicolon.map_or(next.text_range().end(), |token| token.text_range().start());
        edits.push(TextEdit {
            range: TextRange::new(next.text_range().start(), end),
            text: count,
        });
        return Some((
            "Count the rows with COUNT(*) in place of FOUND_ROWS()".to_string(),
            edits,
        ));
    }
    Some(("Remove SQL_CALC_FOUND_ROWS".to_string(), edits))
}

/// `VALUES(c)` in `ON DUPLICATE KEY UPDATE` becomes `new.c` of a row alias the `VALUES` gets,
/// every one of the statement at once.
fn row_alias(cx: &Cx, range: TextRange) -> Option<(String, Vec<TextEdit>)> {
    let call = node_at(cx, range, FUNCTION_CALL)?;
    let insert = call.ancestors().find(|node| node.kind() == INSERT_STMT)?;
    let values = child(&insert, VALUES)?;
    let clause = child(&insert, ON_DUPLICATE_KEY_CLAUSE)?;
    let existing = child(&insert, ALIAS)
        .or_else(|| child(&values, ALIAS))
        .and_then(|alias| child(&alias, NAME))
        .map(|name| name.text().to_string());
    let mut edits = Vec::new();
    let alias = match existing {
        Some(alias) => alias,
        None => {
            edits.push(TextEdit {
                range: TextRange::empty(values.text_range().end()),
                text: " AS new".to_string(),
            });
            "new".to_string()
        }
    };
    for call in clause.descendants().filter(|node| node.kind() == FUNCTION_CALL) {
        let named_values =
            child(&call, QUALIFIED_NAME).is_some_and(|name| compact(&name).eq_ignore_ascii_case("values"));
        let arguments: Vec<SyntaxNode> = child(&call, ARG_LIST)
            .map(|list| list.children().collect())
            .unwrap_or_default();
        let [column] = arguments.as_slice() else {
            continue;
        };
        if !named_values || column.kind() != COLUMN_REF || children(column, NAME).count() != 1 {
            continue;
        }
        edits.push(TextEdit {
            range: call.text_range(),
            text: format!("{alias}.{}", column.text()),
        });
    }
    Some(("Use a row alias in place of VALUES()".to_string(), edits))
}
