//! A readable text form of a tree, for snapshot tests and for looking at what the parser made.

use std::fmt::Write;

use crate::{SyntaxElement, SyntaxKind, SyntaxNode};

/// What a dump leaves in.
#[derive(Clone, Copy, Debug, Default)]
pub struct DumpOptions {
    /// Whitespace and comments.
    pub trivia: bool,
    /// The byte range of every element.
    pub ranges: bool,
}

/// Prints a node and everything under it, one element per line, indented by depth.
pub fn dump(node: &SyntaxNode, options: DumpOptions) -> String {
    let mut out = String::new();
    write_element(&mut out, &SyntaxElement::Node(node.clone()), 0, options);
    out
}

fn write_element(out: &mut String, element: &SyntaxElement, depth: usize, options: DumpOptions) {
    let kind = element.kind();
    if kind.is_trivia() && !options.trivia {
        return;
    }
    let indent = "  ".repeat(depth);
    let range = if options.ranges {
        let range = element.text_range();
        format!("@{}..{}", u32::from(range.start()), u32::from(range.end()))
    } else {
        String::new()
    };
    match element {
        SyntaxElement::Node(node) => {
            let _ = writeln!(out, "{indent}{}{range}", kind.name());
            for child in node.children_with_tokens() {
                write_element(out, &child, depth + 1, options);
            }
        }
        SyntaxElement::Token(token) => {
            let _ = writeln!(out, "{indent}{}{range} {:?}", kind.name(), token.text());
        }
    }
}

/// A compact form for snapshots: nodes as `(KIND child ...)`, tokens as their quoted text and no
/// trivia. Lists of statements put each child on its own line.
pub fn dump_compact(node: &SyntaxNode) -> String {
    let mut out = String::new();
    write_compact(&mut out, node, 0);
    out.push('\n');
    out
}

fn is_container(kind: SyntaxKind) -> bool {
    matches!(kind, SyntaxKind::SOURCE_FILE | SyntaxKind::STATEMENT_LIST)
}

fn write_compact(out: &mut String, node: &SyntaxNode, depth: usize) {
    let multiline = is_container(node.kind());
    let _ = write!(out, "({}", node.kind().name());
    for child in node.children_with_tokens() {
        if child.kind().is_trivia() {
            continue;
        }
        if multiline {
            let _ = write!(out, "\n{}", "  ".repeat(depth + 1));
        } else {
            out.push(' ');
        }
        match child {
            SyntaxElement::Node(child) => write_compact(out, &child, depth + 1),
            SyntaxElement::Token(token) => {
                let _ = write!(out, "{:?}", token.text());
            }
        }
    }
    out.push(')');
}
