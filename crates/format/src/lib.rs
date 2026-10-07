//! A SQL formatter that decides the whitespace between tokens and the case of keywords, and
//! nothing else: the tokens, strings, quoted names and comments stay as they are, so the meaning
//! of a script never changes. A statement that does not parse is left as it is, and so is the
//! whole text when the result would not have the very same tokens.
//!
//! The layout is a conventional one: every statement and every clause of a query on a line of its
//! own, the items of a list a line each when there are several, joins and the conditions of
//! `WHERE` one level in, and subqueries, common table expressions and the blocks of a routine one
//! level in.

mod layout;
mod options;
mod spacing;

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxToken, TextRange, parse};

use crate::layout::Layout;
pub use crate::options::{FormatOptions, Indent, KeywordCase};

/// A replacement of a byte range of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Why a text is left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The layout would have changed a token, which a formatter must never do.
    ChangedTokens,
}

/// The whole text formatted, or why it is left as it is.
pub fn try_format(text: &str, dialect: Dialect, options: &FormatOptions) -> Result<String, Refusal> {
    let run = Run::new(text, dialect, options)?;
    Ok(apply(text, &run.edits(0, text.len())))
}

/// The layout without the check that the tokens stayed, for finding why a text is refused.
#[doc(hidden)]
pub fn format_unchecked(text: &str, dialect: Dialect, options: &FormatOptions) -> String {
    let run = Run::build(text, dialect, options);
    apply(text, &run.edits(0, text.len()))
}

/// The whole text formatted, or `None` for a text that is left as it is.
pub fn format(text: &str, dialect: Dialect, options: &FormatOptions) -> Option<String> {
    try_format(text, dialect, options).ok()
}

/// What formatting the whole text changes.
pub fn edits(text: &str, dialect: Dialect, options: &FormatOptions) -> Option<Vec<Edit>> {
    let run = Run::new(text, dialect, options).ok()?;
    Some(run.edits(0, text.len()))
}

/// What formatting changes within a byte range, widened to whole lines. The layout follows the
/// whole text, so a line comes out as it would in a full format.
pub fn range_edits(
    text: &str,
    dialect: Dialect,
    start: usize,
    end: usize,
    options: &FormatOptions,
) -> Option<Vec<Edit>> {
    if !(text.is_char_boundary(start) && text.is_char_boundary(end)) || start > end {
        return None;
    }
    let run = Run::new(text, dialect, options).ok()?;
    Some(run.edits(start, end))
}

/// What typing `;` at `offset` (just after it) changes: the statement it ends is laid out.
pub fn on_type_edits(
    text: &str,
    dialect: Dialect,
    offset: usize,
    typed: char,
    options: &FormatOptions,
) -> Option<Vec<Edit>> {
    if typed != ';' || !text.is_char_boundary(offset) || offset == 0 {
        return None;
    }
    let root = parse(text, dialect).syntax();
    let at = sql_syntax::TextSize::from(offset as u32);
    let statement = root
        .children()
        .find(|statement| statement.text_range().end() == at && statement.text_range().start() < at)?;
    let start = usize::from(statement.text_range().start());
    range_edits(text, dialect, start, offset, options)
}

/// The text with the edits applied, which must not overlap and must be in order.
pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut at = 0;
    for edit in edits {
        out.push_str(&text[at..edit.start]);
        out.push_str(&edit.text);
        at = edit.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Whether two texts have the same tokens, keywords compared without case.
pub fn same_tokens(left: &str, right: &str, dialect: Dialect) -> bool {
    let tokens = |text: &str| -> Vec<SyntaxToken> {
        parse(text, dialect)
            .syntax()
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() != WHITESPACE)
            .collect()
    };
    let (left, right) = (tokens(left), tokens(right));
    left.len() == right.len()
        && left.iter().zip(&right).all(|(a, b)| {
            a.kind() == b.kind()
                && if a.kind().is_keyword() {
                    a.text().eq_ignore_ascii_case(b.text())
                } else {
                    a.text() == b.text()
                }
        })
}

/// One formatting of a text: the new whitespace before each token and after the last, and the
/// new spelling of each keyword.
struct Run<'a> {
    text: &'a str,
    ranges: Vec<TextRange>,
    /// The whitespace before each token, then after the last.
    gaps: Vec<String>,
    words: Vec<Option<String>>,
}

impl<'a> Run<'a> {
    fn new(text: &'a str, dialect: Dialect, options: &FormatOptions) -> Result<Run<'a>, Refusal> {
        let run = Run::build(text, dialect, options);
        let formatted = apply(text, &run.edits(0, text.len()));
        if !same_tokens(text, &formatted, dialect) {
            return Err(Refusal::ChangedTokens);
        }
        Ok(run)
    }

    fn build(text: &'a str, dialect: Dialect, options: &FormatOptions) -> Run<'a> {
        let parsed = parse(text, dialect);
        let root = parsed.syntax();
        let errors: Vec<TextRange> = parsed.errors().iter().map(|error| error.range).collect();
        let mut layout = Layout::new(&root, &errors, options.leading_commas);
        layout.walk_file(&root);
        let ranges: Vec<TextRange> = layout.leaves.iter().map(|leaf| leaf.text_range()).collect();
        let original = |index: usize| -> &str {
            let start = if index == 0 {
                0
            } else {
                usize::from(ranges[index - 1].end())
            };
            let end = ranges.get(index).map_or(text.len(), |range| usize::from(range.start()));
            &text[start..end]
        };
        place_comments(&mut layout, &original);
        let unit = options.indent.unit();
        let count = layout.leaves.len();
        let mut gaps = Vec::with_capacity(count + 1);
        for index in 0..count {
            let before = original(index);
            let gap = if index == 0 {
                String::new()
            } else if layout.frozen[index] {
                before.to_string()
            } else if let Some(level) = layout.breaks[index] {
                let blank = before.matches('\n').count() >= 2;
                let mut gap = String::from(if blank { "\n\n" } else { "\n" });
                gap.push_str(&unit.repeat(level));
                gap
            } else {
                let previous = &layout.leaves[index - 1];
                let next = &layout.leaves[index];
                if previous.kind().is_trivia() || next.kind().is_trivia() {
                    " ".to_string()
                } else {
                    spacing::space(previous, next, before).to_string()
                }
            };
            gaps.push(gap);
        }
        let tail = original(count);
        gaps.push(if count > 0 && tail.contains('\n') {
            "\n".to_string()
        } else {
            tail.to_string()
        });
        let words = layout
            .leaves
            .iter()
            .enumerate()
            .map(|(index, leaf)| {
                if layout.verbatim[index] || !leaf.kind().is_keyword() {
                    return None;
                }
                let spelled = match options.keyword_case {
                    KeywordCase::Upper => leaf.text().to_ascii_uppercase(),
                    KeywordCase::Lower => leaf.text().to_ascii_lowercase(),
                    KeywordCase::Preserve => return None,
                };
                (spelled != leaf.text()).then_some(spelled)
            })
            .collect();
        Run {
            text,
            ranges,
            gaps,
            words,
        }
    }

    /// The edits of the gaps and words on the lines a byte range is on.
    fn edits(&self, from: usize, to: usize) -> Vec<Edit> {
        let text = self.text;
        let from = text[..from.min(text.len())].rfind('\n').map_or(0, |at| at + 1);
        let to = text[to.min(text.len())..].find('\n').map_or(text.len(), |at| to + at);
        let mut out = Vec::new();
        let count = self.ranges.len();
        for index in 0..=count {
            let start = if index == 0 {
                0
            } else {
                usize::from(self.ranges[index - 1].end())
            };
            let end = self
                .ranges
                .get(index)
                .map_or(text.len(), |range| usize::from(range.start()));
            if (from..=to).contains(&end) && text[start..end] != self.gaps[index] {
                out.push(Edit {
                    start,
                    end,
                    text: self.gaps[index].clone(),
                });
            }
            if let Some(word) = self.words.get(index).and_then(Option::as_ref) {
                let range = self.ranges[index];
                if (from..=to).contains(&usize::from(range.start())) {
                    out.push(Edit {
                        start: usize::from(range.start()),
                        end: usize::from(range.end()),
                        text: word.clone(),
                    });
                }
            }
        }
        out
    }
}

/// Comments keep their place: one on a line of its own stays on a line of its own, at the level
/// of what follows it; one after code stays after it. What follows a line comment starts a line.
fn place_comments<'t>(layout: &mut Layout, original: &impl Fn(usize) -> &'t str) {
    let count = layout.leaves.len();
    for index in 0..count {
        let leaf = layout.leaves[index].clone();
        if !leaf.kind().is_trivia() || layout.verbatim[index] {
            continue;
        }
        let own_line = index == 0 || original(index).contains('\n');
        if own_line {
            if index > 0 {
                let next = (index + 1..count).find(|other| !layout.leaves[*other].kind().is_trivia());
                let level = match next.and_then(|next| layout.breaks[next]) {
                    Some(level) => level,
                    None if leaf.parent().is_some_and(|parent| parent.kind() == SOURCE_FILE) => 0,
                    None => layout.line_level(index - 1) + 1,
                };
                layout.breaks[index] = Some(level);
            }
        } else {
            layout.breaks[index] = None;
        }
        let ends_line = leaf.kind() == LINE_COMMENT || original(index + 1).contains('\n');
        // A comment on its own line before what it was written beside takes that line's break.
        if own_line && !ends_line && index > 0 && index + 1 < count && !layout.frozen[index + 1] {
            if let Some(level) = layout.breaks[index + 1].take() {
                layout.breaks[index] = Some(level);
            }
        }
        if ends_line && index + 1 < count && layout.breaks[index + 1].is_none() && !layout.frozen[index + 1] {
            let level = if own_line {
                layout.line_level(index)
            } else {
                layout.line_level(index) + 1
            };
            layout.breaks[index + 1] = Some(level);
        }
    }
}

#[cfg(test)]
mod tests;
