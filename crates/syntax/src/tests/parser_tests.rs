use crate::{Dialect, SyntaxError, SyntaxKind, SyntaxNode, dump_compact, parse};
use expect_test::Expect;

fn render(node: &SyntaxNode, errors: &[SyntaxError]) -> String {
    let mut out = dump_compact(node);
    for SyntaxError { range, message } in errors {
        out.push_str(&format!(
            "error {}..{}: {message}\n",
            u32::from(range.start()),
            u32::from(range.end())
        ));
    }
    out
}

/// The compact tree of a script with its errors after it. Also checks the tree keeps every byte.
fn tree_of(text: &str, dialect: Dialect) -> String {
    let parsed = parse(text, dialect);
    assert_eq!(
        parsed.syntax().text().to_string(),
        text,
        "the tree must hold every byte"
    );
    render(&parsed.syntax(), parsed.errors())
}

pub(super) fn check(text: &str, expect: Expect) {
    expect.assert_eq(&tree_of(text, Dialect::Generic));
}

pub(super) fn check_in(dialect: Dialect, text: &str, expect: Expect) {
    expect.assert_eq(&tree_of(text, dialect));
}

/// The first expression of a `SELECT`, which is what precedence tests are about.
pub(super) fn expr(text: &str, expect: Expect) {
    let source = format!("SELECT {text}");
    let parsed = parse(&source, Dialect::Generic);
    assert_eq!(parsed.syntax().text().to_string(), source);
    let item = parsed
        .syntax()
        .descendants()
        .find(|node| node.kind() == SyntaxKind::SELECT_ITEM)
        .expect("a select item");
    let expression = item.children().next().expect("an expression");
    let mut out = dump_compact(&expression);
    for error in parsed.errors() {
        out.push_str(&format!("error: {}\n", error.message));
    }
    expect.assert_eq(&out);
}

mod expressions;
mod recovery;
mod robustness;
mod statements;

#[test]
fn a_keyword_that_names_something_is_an_identifier_in_the_tree() {
    let parsed = parse(
        "SELECT date, t.year FROM t AS user WHERE EXTRACT(day FROM date) > 1;",
        Dialect::Generic,
    );
    let names: Vec<(SyntaxKind, String)> = parsed
        .syntax()
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::NAME)
        .filter_map(|node| node.first_token())
        .map(|token| (token.kind(), token.text().to_string()))
        .collect();
    assert!(names.iter().all(|(kind, _)| *kind == SyntaxKind::IDENT), "{names:?}");
    let words: Vec<&str> = names.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(words, ["date", "t", "year", "t", "user", "EXTRACT", "day", "date"]);
}
