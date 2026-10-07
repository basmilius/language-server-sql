//! How much a string looks like SQL: one signal for a host that decides which of its strings to
//! read as SQL. Interface text such as "Select a file" or "Update failed" starts with the same
//! words, so the first word alone is not enough.

use sql_syntax::{Dialect, parse};

use crate::fragment::{Fragment, Piece};

/// The words a statement starts with.
const STATEMENTS: &[&str] = &[
    "ALTER",
    "ANALYZE",
    "ATTACH",
    "BEGIN",
    "CALL",
    "COMMENT",
    "COMMIT",
    "COPY",
    "CREATE",
    "DEALLOCATE",
    "DELETE",
    "DESC",
    "DESCRIBE",
    "DETACH",
    "DO",
    "DROP",
    "EXECUTE",
    "EXPLAIN",
    "GRANT",
    "HANDLER",
    "INSERT",
    "LOAD",
    "LOCK",
    "MERGE",
    "OPTIMIZE",
    "PRAGMA",
    "PREPARE",
    "REFRESH",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "REVOKE",
    "ROLLBACK",
    "SAVEPOINT",
    "SELECT",
    "SET",
    "SHOW",
    "START",
    "TABLE",
    "TRUNCATE",
    "UNLOCK",
    "UPDATE",
    "USE",
    "VACUUM",
    "VALUES",
    "WITH",
];

/// Words that only SQL puts after the first.
const CLAUSES: &[&str] = &[
    "FROM",
    "WHERE",
    "INTO",
    "VALUES",
    "SET",
    "JOIN",
    "TABLE",
    "BY",
    "ON",
    "LIMIT",
    "RETURNING",
    "INDEX",
    "VIEW",
    "DATABASE",
    "SCHEMA",
    "PROCEDURE",
    "FUNCTION",
    "TRIGGER",
    "HAVING",
    "UNION",
    "DISTINCT",
    "AS",
];

/// The first word after whitespace, comments and opening parentheses.
fn first_word(text: &str) -> &str {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start_matches(|character: char| character.is_whitespace() || character == '(');
        if let Some(comment) = trimmed.strip_prefix("--") {
            rest = comment.split_once('\n').map_or("", |(_, after)| after);
        } else if let Some(comment) = trimmed.strip_prefix("/*") {
            rest = comment.split_once("*/").map_or("", |(_, after)| after);
        } else {
            rest = trimmed;
            break;
        }
    }
    let end = rest
        .find(|character: char| !character.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    &rest[..end]
}

/// How sure it is that a string is SQL, from 0 to 1: it must start with a statement's first
/// word; words of other clauses, the case of SQL rather than of a sentence, its punctuation and
/// parsing without errors in `dialect` make it surer. A host might take 0.6 and up as SQL.
pub fn confidence(text: &str, dialect: Dialect) -> f32 {
    let word = first_word(text);
    if word.is_empty() || !STATEMENTS.contains(&word.to_ascii_uppercase().as_str()) {
        return 0.0;
    }
    let mut score: f32 = 0.35;
    if word.chars().all(|character| character.is_ascii_uppercase()) {
        score += 0.2;
    } else if word.chars().all(|character| character.is_ascii_lowercase()) {
        score += 0.1;
    } else {
        score -= 0.1;
    }
    let clauses = text
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .skip_while(|part| part.is_empty())
        .skip(1)
        .filter(|part| CLAUSES.contains(&part.to_ascii_uppercase().as_str()))
        .count();
    score += match clauses {
        0 => 0.0,
        1 => 0.2,
        _ => 0.25,
    };
    if text.contains(['*', '=', ',', '(', '?', ';']) {
        score += 0.1;
    }
    score += match parse(text, dialect).errors().len() {
        0 => 0.2,
        1 => -0.1,
        _ => -0.3,
    };
    score.clamp(0.0, 1.0)
}

impl Fragment {
    /// [`confidence`] of the fragment, with each hole read as a value.
    pub fn confidence(&self, dialect: Dialect) -> f32 {
        let text: String = self
            .pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, .. } | Piece::Escape { text, .. } => text.as_str(),
                Piece::Hole { .. } => "?",
            })
            .collect();
        confidence(&text, dialect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_queries_from_sentences() {
        let sure = |text: &str| confidence(text, Dialect::Mysql);
        assert!(sure("SELECT * FROM users WHERE id = ?") > 0.9);
        assert!(sure("select id, name from users") > 0.7);
        assert!(sure("  -- the active ones\n  UPDATE users SET active = 1 WHERE id = :id") > 0.9);
        assert!(sure("INSERT INTO t (a) VALUES (?)") > 0.9);
        assert!(sure("Select a file") < 0.6);
        assert!(sure("Update failed") < 0.6);
        assert!(sure("Delete this item?") < 0.6);
        assert!(sure("Show more") < 0.6);
        assert_eq!(sure("Hello world"), 0.0);
        assert_eq!(sure(""), 0.0);
    }
}
