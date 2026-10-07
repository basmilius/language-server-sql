use sql_syntax::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize, TokenAtOffset};

/// The ranges around an offset, from the smallest to the whole script, each one inside the next.
pub fn selection_ranges(root: &SyntaxNode, offset: TextSize) -> Vec<TextRange> {
    let offset = offset.min(root.text_range().end());
    let mut chain = Vec::new();
    let Some(start) = pick_token(root, offset) else {
        return vec![root.text_range()];
    };
    let mut push = |range: TextRange| {
        if chain.last() != Some(&range) && !range.is_empty() {
            chain.push(range);
        }
    };
    if !start.kind().is_trivia() {
        push(start.text_range());
    }
    for ancestor in start.parent_ancestors() {
        push(ancestor.text_range());
    }
    if chain.is_empty() {
        chain.push(root.text_range());
    }
    chain
}

/// The token a caret at `offset` belongs to: the one it is inside of, and between two tokens the
/// one that is not trivia, preferring the one after.
fn pick_token(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    match root.token_at_offset(offset) {
        TokenAtOffset::None => None,
        TokenAtOffset::Single(token) => Some(token),
        TokenAtOffset::Between(left, right) => {
            let is_word = |kind: SyntaxKind| !kind.is_trivia();
            if is_word(right.kind()) || !is_word(left.kind()) {
                Some(right)
            } else {
                Some(left)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_syntax::{Dialect, parse};

    fn chain(text: &str, marker: &str) -> Vec<String> {
        let offset = text.find(marker).expect("the marker") as u32;
        let parsed = parse(text, Dialect::Generic);
        selection_ranges(&parsed.syntax(), TextSize::from(offset))
            .into_iter()
            .map(|range| text[range].to_string())
            .collect()
    }

    #[test]
    fn grows_from_the_token_to_the_script() {
        let found = chain("SELECT a + 1 FROM t WHERE b = 2;\nSELECT 3;", "a + 1");
        assert_eq!(found[0], "a");
        assert!(found.contains(&"a + 1".to_string()));
        assert!(found.contains(&"SELECT a + 1 FROM t WHERE b = 2".to_string()));
        assert!(found.contains(&"SELECT a + 1 FROM t WHERE b = 2;".to_string()));
        assert!(found.last().expect("a range").ends_with("SELECT 3;"));
        for pair in found.windows(2) {
            assert!(pair[1].contains(&pair[0]) && pair[1].len() > pair[0].len());
        }
    }

    #[test]
    fn a_caret_in_whitespace_starts_at_the_enclosing_node() {
        let found = chain("SELECT f(   ) FROM t;", "  )");
        assert_eq!(found[0], "(   )");
    }
}
