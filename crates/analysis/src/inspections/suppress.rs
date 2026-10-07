//! Suppression comments. `-- sql-suppress <id> ...` silences the ids for the statement it stands
//! in, before, or after on the statement's last line; `-- sql-suppress-file <id> ...` anywhere
//! silences them for the whole script. An id is an inspection's or a row of the feature table's,
//! and `all` stands for every one. The list ends at the first word that is none of those, so a
//! reason may follow it.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{FEATURES, SyntaxElement, SyntaxNode, SyntaxToken, TextRange, TextSize};

use super::{Finding, QuickFix, inspection_info};
use crate::diagnostics::Diagnostic;
use crate::rename::TextEdit;

pub const STATEMENT_DIRECTIVE: &str = "sql-suppress";
pub const FILE_DIRECTIVE: &str = "sql-suppress-file";

/// A directive comment: whether it is for the file, its ids, and where the last id ends.
struct Directive {
    file: bool,
    ids: Vec<String>,
    end: TextSize,
}

fn is_id(word: &str) -> bool {
    word == "all" || inspection_info(word).is_some() || FEATURES.iter().any(|feature| feature.id == word)
}

fn directive(token: &SyntaxToken) -> Option<Directive> {
    let text = token.text();
    let (body, skipped) = if let Some(rest) = text.strip_prefix("--") {
        (rest, 2)
    } else if let Some(rest) = text.strip_prefix("/*") {
        (rest.strip_suffix("*/").unwrap_or(rest), 2)
    } else {
        (text.strip_prefix('#')?, 1)
    };
    let trimmed = body.trim_start();
    let mut at = skipped + body.len() - trimmed.len();
    let (file, rest) = if let Some(rest) = trimmed.strip_prefix(FILE_DIRECTIVE) {
        at += FILE_DIRECTIVE.len();
        (true, rest)
    } else {
        let rest = trimmed.strip_prefix(STATEMENT_DIRECTIVE)?;
        at += STATEMENT_DIRECTIVE.len();
        (false, rest)
    };
    if !rest.is_empty() && !rest.starts_with([' ', '\t', ',', ':']) {
        return None;
    }
    let mut ids = Vec::new();
    let mut end = at;
    let mut offset = at;
    for piece in rest.split_inclusive([' ', '\t', ',', ':', '\n']) {
        let word = piece.trim_end_matches([' ', '\t', ',', ':', '\n']);
        if !word.is_empty() {
            if !is_id(word) {
                break;
            }
            ids.push(word.to_string());
            end = offset + word.len();
        }
        offset += piece.len();
    }
    Some(Directive {
        file,
        ids,
        end: token.text_range().start() + TextSize::from(end as u32),
    })
}

fn comments(root: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    root.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| matches!(token.kind(), LINE_COMMENT | BLOCK_COMMENT))
}

fn has_newline_between(root: &SyntaxNode, start: TextSize, end: TextSize) -> bool {
    start < end && root.text().slice(TextRange::new(start, end)).contains_char('\n')
}

/// The statement of the script a comment speaks for: the one it stands in, the one whose last line
/// it ends, or the one after it.
fn statement_of_comment(root: &SyntaxNode, comment: &SyntaxToken) -> Option<SyntaxNode> {
    let parent = comment.parent()?;
    if parent.kind() != SOURCE_FILE {
        return parent
            .ancestors()
            .find(|node| node.parent().is_some_and(|parent| parent.kind() == SOURCE_FILE));
    }
    let start = comment.text_range().start();
    let before = root
        .children()
        .take_while(|statement| statement.text_range().end() <= start)
        .last();
    if let Some(before) = before {
        if !has_newline_between(root, before.text_range().end(), start) {
            return Some(before);
        }
    }
    root.children()
        .find(|statement| statement.text_range().start() >= start)
}

fn matches(ids: &[String], diagnostic: &Diagnostic) -> bool {
    ids.iter()
        .any(|id| id == "all" || id == diagnostic.code || diagnostic.feature.is_some_and(|feature| feature == id))
}

/// Drops the findings a suppression comment silences.
pub(super) fn apply(root: &SyntaxNode, found: &mut Vec<Finding>) {
    if found.is_empty() {
        return;
    }
    let mut regions: Vec<(TextRange, Vec<String>)> = Vec::new();
    for comment in comments(root) {
        let Some(directive) = directive(&comment) else {
            continue;
        };
        if directive.file {
            regions.push((root.text_range(), directive.ids));
        } else if let Some(statement) = statement_of_comment(root, &comment) {
            regions.push((statement.text_range(), directive.ids));
        }
    }
    if regions.is_empty() {
        return;
    }
    found.retain(|finding| {
        let diagnostic = &finding.diagnostic;
        !regions
            .iter()
            .any(|(region, ids)| region.contains_range(diagnostic.range) && matches(ids, diagnostic))
    });
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |newline| newline + 1)
}

/// The fixes that silence a diagnostic of an inspection: for its statement and for the script. A
/// finding of the feature table is silenced by its row, the narrower id.
pub fn suppress_fixes(root: &SyntaxNode, diagnostic: &Diagnostic) -> Vec<QuickFix> {
    let Some(_) = inspection_info(diagnostic.code) else {
        return Vec::new();
    };
    let id = diagnostic.feature.unwrap_or(diagnostic.code);
    let text = root.text().to_string();
    let directives: Vec<(SyntaxToken, Directive)> = comments(root)
        .filter_map(|comment| directive(&comment).map(|directive| (comment, directive)))
        .collect();
    let mut out = Vec::new();
    let statement = root
        .children()
        .find(|statement| statement.text_range().contains_range(diagnostic.range));
    if let Some(statement) = statement {
        let start = usize::from(statement.text_range().start());
        let existing = directives.iter().find(|(comment, directive)| {
            !directive.file
                && comment.kind() == LINE_COMMENT
                && comment.text().starts_with("--")
                && comment.text_range().end() <= statement.text_range().start()
                && statement_of_comment(root, comment).as_ref() == Some(&statement)
        });
        let edit = match existing {
            Some((_, directive)) => TextEdit {
                range: TextRange::empty(directive.end),
                text: format!(" {id}"),
            },
            None => {
                let line = line_start(&text, start);
                let indent: String = text[line..start]
                    .chars()
                    .take_while(|character| *character == ' ' || *character == '\t')
                    .collect();
                let alone = text[line..start].trim().is_empty();
                let prefix = if alone { String::new() } else { "\n".to_string() };
                TextEdit {
                    range: TextRange::empty(statement.text_range().start()),
                    text: format!("{prefix}-- {STATEMENT_DIRECTIVE} {id}\n{indent}"),
                }
            }
        };
        out.push(QuickFix {
            title: format!("Suppress '{id}' for this statement"),
            edits: vec![edit],
        });
    }
    let existing = directives.iter().find(|(_, directive)| directive.file);
    let edit = match existing {
        Some((_, directive)) => TextEdit {
            range: TextRange::empty(directive.end),
            text: format!(" {id}"),
        },
        None => TextEdit {
            range: TextRange::empty(TextSize::from(0)),
            text: format!("-- {FILE_DIRECTIVE} {id}\n"),
        },
    };
    out.push(QuickFix {
        title: format!("Suppress '{id}' for the file"),
        edits: vec![edit],
    });
    out
}
