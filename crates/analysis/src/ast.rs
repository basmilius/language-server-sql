//! Small questions about nodes of the tree that every analysis asks.

use sql_syntax::SyntaxKind::{self, *};
use sql_syntax::{Dialect, SyntaxElement, SyntaxNode, SyntaxToken, TextRange};

use crate::ident::Ident;

pub fn child(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|child| child.kind() == kind)
}

pub fn children(node: &SyntaxNode, kind: SyntaxKind) -> impl Iterator<Item = SyntaxNode> {
    node.children().filter(move |child| child.kind() == kind)
}

/// The tokens directly in a node, without trivia.
pub fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| !token.kind().is_trivia())
}

pub fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    tokens(node).any(|token| token.kind() == kind)
}

/// The text of a node with its whitespace and comments folded to single spaces.
pub fn compact(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for token in node.descendants_with_tokens().filter_map(SyntaxElement::into_token) {
        if token.kind().is_trivia() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        out.push_str(token.text());
    }
    out.trim().to_string()
}

/// One part of a dotted name: what it says and where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub ident: Ident,
    pub node: SyntaxNode,
}

impl Part {
    pub fn range(&self) -> TextRange {
        self.node.text_range()
    }
}

/// The `NAME`s of a `QUALIFIED_NAME`, a `COLUMN_REF` or a `WILDCARD`, in order.
pub fn parts(node: &SyntaxNode, dialect: Dialect) -> Vec<Part> {
    children(node, NAME)
        .filter_map(|name| {
            Some(Part {
                ident: Ident::of_name(&name, dialect)?,
                node: name,
            })
        })
        .collect()
}

/// The schema and the name of an object a `QUALIFIED_NAME` names; with three parts the first is a
/// database or catalog, which is left out.
pub fn object_name(node: &SyntaxNode, dialect: Dialect) -> Option<(Option<Part>, Part)> {
    let mut parts = parts(node, dialect);
    let name = parts.pop()?;
    Some((parts.pop(), name))
}

/// The alias of a table or select item, and the column names it gives.
pub fn alias_of(node: &SyntaxNode, dialect: Dialect) -> Option<(Part, Vec<Part>)> {
    let alias = child(node, ALIAS)?;
    let name = child(&alias, NAME)?;
    let part = Part {
        ident: Ident::of_name(&name, dialect)?,
        node: name,
    };
    let mut columns = Vec::new();
    if let Some(list) = child(&alias, NAME_LIST) {
        columns = parts(&list, dialect);
    } else if let Some(list) = child(&alias, TABLE_ELEMENT_LIST) {
        for column in children(&list, COLUMN_DEF) {
            if let Some(name) = child(&column, NAME) {
                if let Some(ident) = Ident::of_name(&name, dialect) {
                    columns.push(Part { ident, node: name });
                }
            }
        }
    }
    Some((part, columns))
}

/// Whether a node is the kind of a query: `SELECT`, a set operation, `VALUES`, `TABLE` or a
/// parenthesized query.
pub fn is_query(kind: SyntaxKind) -> bool {
    matches!(kind, SELECT | COMPOUND_SELECT | VALUES | TABLE_QUERY | PAREN_QUERY)
}

/// The query a `PAREN_QUERY` or a statement holds.
pub fn inner_query(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children().find(|child| is_query(child.kind()))
}

/// The units MySQL's and MariaDB's `TIMESTAMPDIFF()` and `TIMESTAMPADD()` take as their first
/// argument, a word and no column.
const TIME_UNITS: [&str; 9] = [
    "microsecond",
    "second",
    "minute",
    "hour",
    "day",
    "week",
    "month",
    "quarter",
    "year",
];

/// Whether a column reference is the unit of `TIMESTAMPDIFF(day, a, b)` or `TIMESTAMPADD(minute, 5, c)`,
/// which the grammar reads as a column since the unit is a plain word.
pub fn is_time_unit(column_ref: &SyntaxNode) -> bool {
    if column_ref.kind() != COLUMN_REF {
        return false;
    }
    let mut names = children(column_ref, NAME);
    let (Some(name), None) = (names.next(), names.next()) else {
        return false;
    };
    let word = name.text().to_string().to_ascii_lowercase();
    let word = word.strip_prefix("sql_tsi_").unwrap_or(&word);
    if !TIME_UNITS.contains(&word) {
        return false;
    }
    let Some(arguments) = column_ref.parent().filter(|parent| parent.kind() == ARG_LIST) else {
        return false;
    };
    if arguments.children().next().as_ref() != Some(column_ref) {
        return false;
    }
    arguments
        .parent()
        .filter(|call| call.kind() == FUNCTION_CALL)
        .and_then(|call| child(&call, QUALIFIED_NAME))
        .and_then(|function| children(&function, NAME).last())
        .is_some_and(|function| {
            let function = function.text().to_string();
            function.eq_ignore_ascii_case("timestampdiff") || function.eq_ignore_ascii_case("timestampadd")
        })
}
