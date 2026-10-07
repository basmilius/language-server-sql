use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::LineIndex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldKind {
    Comment,
    Region,
}

/// A stretch of lines that can fold: from the line that stays visible to the last line that hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: Option<FoldKind>,
}

/// What folds in a script: every statement of more than one line, parenthesized lists and
/// subqueries, `BEGIN ... END` and the bodies of `IF`, `CASE` and the loops, `CASE` expressions,
/// block comments and runs of line comments, and `-- region` to `-- endregion`.
///
/// A fold ends on the line before a closing parenthesis or `END` that starts its line, so that line
/// stays visible when the fold is closed.
pub fn folding_ranges(root: &SyntaxNode, text: &str, index: &LineIndex) -> Vec<Fold> {
    let mut walker = Walker {
        text,
        index,
        folds: Vec::new(),
    };
    walker.walk(root);
    let mut folds = walker.folds;
    folds.retain(|fold| fold.end_line > fold.start_line);
    folds.sort_by_key(|fold| (fold.start_line, std::cmp::Reverse(fold.end_line)));
    folds.dedup_by_key(|fold| (fold.start_line, fold.end_line));
    folds
}

struct Walker<'a> {
    text: &'a str,
    index: &'a LineIndex,
    folds: Vec<Fold>,
}

fn last_significant_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let mut token = node.last_token();
    while let Some(current) = token {
        if !current.kind().is_trivia() && !matches!(current.kind(), SEMICOLON | CUSTOM_DELIMITER) {
            return Some(current);
        }
        if current.text_range().start() <= node.text_range().start() {
            return None;
        }
        token = current.prev_token();
    }
    None
}

impl Walker<'_> {
    fn line(&self, offset: u32) -> u32 {
        self.index.line_of(offset)
    }

    fn push(&mut self, start_line: u32, end_line: u32, kind: Option<FoldKind>) {
        self.folds.push(Fold {
            start_line,
            end_line,
            kind,
        });
    }

    /// Whether only whitespace precedes the offset on its line.
    fn starts_its_line(&self, offset: u32) -> bool {
        let line_start = self.index.line_start(self.line(offset)) as usize;
        self.text[line_start..offset as usize].chars().all(char::is_whitespace)
    }

    /// From the line of `start` to the line of `closer`, or the line before it when the closer
    /// starts its line.
    fn bracketed(&mut self, start: u32, closer: &SyntaxToken) {
        let closer_start: u32 = closer.text_range().start().into();
        let mut end = self.line(closer_start);
        if self.starts_its_line(closer_start) {
            end = end.saturating_sub(1);
        }
        self.push(self.line(start), end, None);
    }

    fn walk(&mut self, root: &SyntaxNode) {
        let mut comments: Vec<SyntaxToken> = Vec::new();
        let mut regions: Vec<u32> = Vec::new();
        for element in root.descendants_with_tokens() {
            match element {
                SyntaxElement::Token(token) => self.token(&token, &mut comments, &mut regions),
                SyntaxElement::Node(node) => self.node(&node),
            }
        }
        self.flush_comments(&mut comments);
    }

    fn node(&mut self, node: &SyntaxNode) {
        let kind = node.kind();
        let start: u32 = node.text_range().start().into();
        if is_statement(kind) && node.parent().is_some_and(|parent| parent.kind() == SOURCE_FILE) {
            if let Some(last) = last_significant_token(node) {
                self.push(self.line(start), self.line(last.text_range().start().into()), None);
            }
            return;
        }
        match kind {
            TABLE_ELEMENT_LIST | ARG_LIST | PAREN_QUERY | IN_LIST | NAME_LIST | ROW_EXPR | PAREN_EXPR | WINDOW_SPEC
            | INDEX_COLUMN_LIST | PARAM_LIST | ENUM_VALUE_LIST | JSON_TABLE_COLUMNS | PAREN_JOIN => {
                let closer = node
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .filter(|token| token.kind() == RPAREN)
                    .last();
                let opener = node
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .find(|token| token.kind() == LPAREN);
                if let (Some(opener), Some(closer)) = (opener, closer) {
                    self.bracketed(opener.text_range().start().into(), &closer);
                }
            }
            CTE => {
                let closer = node
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .filter(|token| token.kind() == RPAREN)
                    .last();
                if let Some(closer) = closer {
                    self.bracketed(start, &closer);
                }
            }
            BLOCK | CASE_EXPR | CASE_STMT | IF_STMT | LOOP_STMT | WHILE_STMT | REPEAT_STMT => {
                let end = node
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .filter(|token| token.kind() == END_KW)
                    .last();
                if let Some(end) = end {
                    self.bracketed(start, &end);
                }
            }
            _ => {}
        }
    }

    fn token(&mut self, token: &SyntaxToken, comments: &mut Vec<SyntaxToken>, regions: &mut Vec<u32>) {
        let kind = token.kind();
        let start: u32 = token.text_range().start().into();
        if kind == WHITESPACE {
            if token.text().matches('\n').count() > 1 {
                self.flush_comments(comments);
            }
            return;
        }
        if kind == BLOCK_COMMENT {
            self.flush_comments(comments);
            let end: u32 = token.text_range().end().into();
            self.push(
                self.line(start),
                self.line(end.saturating_sub(1)),
                Some(FoldKind::Comment),
            );
            return;
        }
        if kind != LINE_COMMENT {
            self.flush_comments(comments);
            return;
        }
        let body = token.text().trim_start_matches(['-', '#']).trim();
        let lower = body.to_ascii_lowercase();
        if lower.starts_with("endregion") || lower.starts_with("#endregion") {
            self.flush_comments(comments);
            if let Some(open) = regions.pop() {
                self.push(open, self.line(start), Some(FoldKind::Region));
            }
            return;
        }
        if lower.starts_with("region") || lower.starts_with("#region") {
            self.flush_comments(comments);
            regions.push(self.line(start));
            return;
        }
        if let Some(previous) = comments.last() {
            let previous_line = self.line(previous.text_range().start().into());
            if self.line(start) != previous_line + 1 {
                self.flush_comments(comments);
            }
        }
        comments.push(token.clone());
    }

    fn flush_comments(&mut self, comments: &mut Vec<SyntaxToken>) {
        if comments.len() > 1 {
            let first = self.line(comments[0].text_range().start().into());
            let last = self.line(comments[comments.len() - 1].text_range().start().into());
            self.push(first, last, Some(FoldKind::Comment));
        }
        comments.clear();
    }
}

fn is_statement(kind: SyntaxKind) -> bool {
    kind.name().ends_with("_STMT") || kind == BLOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_syntax::{Dialect, parse};

    fn folds(text: &str) -> Vec<(u32, u32, Option<FoldKind>)> {
        let parsed = parse(text, Dialect::Generic);
        let index = LineIndex::new(text);
        folding_ranges(&parsed.syntax(), text, &index)
            .into_iter()
            .map(|fold| (fold.start_line, fold.end_line, fold.kind))
            .collect()
    }

    #[test]
    fn statements_lists_and_comments_fold() {
        let text = "-- one\n-- two\nCREATE TABLE t (\n    a int,\n    b int\n);\n/* a\n   b */\nSELECT a,\n       b\nFROM t\nWHERE a IN (\n  1,\n  2\n);\nSELECT 1;\n";
        assert_eq!(
            folds(text),
            [
                (0, 1, Some(FoldKind::Comment)),
                (2, 5, None),
                (2, 4, None),
                (6, 7, Some(FoldKind::Comment)),
                (8, 14, None),
                (11, 13, None),
            ]
        );
    }

    #[test]
    fn regions_and_blocks_fold() {
        let text = "-- region setup\nSELECT 1;\nSELECT 2;\n-- endregion\nCREATE FUNCTION f() RETURNS int LANGUAGE sql\nBEGIN ATOMIC\n  SELECT 1;\nEND;\n";
        assert_eq!(
            folds(text),
            [(0, 3, Some(FoldKind::Region)), (4, 7, None), (5, 6, None)]
        );
    }
}
