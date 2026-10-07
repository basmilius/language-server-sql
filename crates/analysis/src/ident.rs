//! Names as each dialect reads them: what a quoted name says, how an unquoted one folds, how two
//! names compare, and how a name has to be written to stand in a statement.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, SyntaxToken, Target, is_reserved_word};

/// A name as written, read the way its dialect compares it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ident {
    /// The name without quotes; unquoted in PostgreSQL, in lower case.
    pub text: String,
    pub quoted: bool,
}

impl Ident {
    pub fn new(text: impl Into<String>) -> Ident {
        Ident {
            text: text.into(),
            quoted: false,
        }
    }

    /// The name a token spells in a dialect.
    pub fn of_token(token: &SyntaxToken, dialect: Dialect) -> Ident {
        let (text, quoted) = unquote(token.kind(), token.text());
        Ident {
            text: fold(dialect, &text, quoted),
            quoted,
        }
    }

    /// The name a `NAME` node holds.
    pub fn of_name(name: &SyntaxNode, dialect: Dialect) -> Option<Ident> {
        let token = name
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| !token.kind().is_trivia())?;
        Some(Ident::of_token(&token, dialect))
    }
}

/// How two names compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Exact,
    Insensitive,
}

impl Case {
    pub fn eq(self, a: &str, b: &str) -> bool {
        match self {
            Case::Exact => a == b,
            Case::Insensitive => a.eq_ignore_ascii_case(b) || (!a.is_ascii() && a.to_lowercase() == b.to_lowercase()),
        }
    }
}

/// How names other than tables and schemas compare in a dialect: PostgreSQL has folded the name
/// already and compares exactly; the others ignore case.
pub fn name_case(dialect: Dialect) -> Case {
    match dialect {
        Dialect::Postgres => Case::Exact,
        _ => Case::Insensitive,
    }
}

/// The text of a name token without its quotes, and whether it had them.
pub fn unquote(kind: sql_syntax::SyntaxKind, text: &str) -> (String, bool) {
    let inner = |open: char, close: char| {
        let body = text.strip_prefix(open).unwrap_or(text);
        let body = body.strip_suffix(close).unwrap_or(body);
        if open == close {
            let doubled: String = [close, close].iter().collect();
            body.replace(&doubled, &close.to_string())
        } else {
            body.to_string()
        }
    };
    match kind {
        QUOTED_IDENT => (inner('"', '"'), true),
        BACKTICK_IDENT => (inner('`', '`'), true),
        BRACKET_IDENT => (inner('[', ']'), true),
        STRING => (inner('\'', '\''), true),
        _ => (text.to_string(), false),
    }
}

/// How a dialect stores a name it reads: PostgreSQL folds an unquoted name to lower case.
pub fn fold(dialect: Dialect, text: &str, quoted: bool) -> String {
    if dialect == Dialect::Postgres && !quoted {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// A name as it has to be written in a statement: quoted when it is reserved, holds characters an
/// unquoted name cannot, or, in PostgreSQL, has capitals that folding would lose.
pub fn quote_name(name: &str, target: Target) -> String {
    if !needs_quotes(name, target) {
        return name.to_string();
    }
    let quote = match target.dialect {
        Dialect::Mysql | Dialect::Mariadb => '`',
        _ => '"',
    };
    let escaped = name.replace(quote, &format!("{quote}{quote}"));
    format!("{quote}{escaped}{quote}")
}

pub fn needs_quotes(name: &str, target: Target) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return true;
    };
    let mysql = matches!(target.dialect, Dialect::Mysql | Dialect::Mariadb);
    let start_ok = first.is_ascii_alphabetic() || first == '_' || (mysql && (first.is_ascii_digit() || first == '$'));
    let rest_ok = name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '$');
    if !start_ok || !rest_ok {
        return true;
    }
    if mysql && name.chars().all(|character| character.is_ascii_digit()) {
        return true;
    }
    if target.dialect == Dialect::Postgres && name.chars().any(|character| character.is_ascii_uppercase()) {
        return true;
    }
    is_reserved_word(&name.to_ascii_uppercase(), target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_syntax::parse;

    fn first_name(text: &str, dialect: Dialect) -> Ident {
        let root = parse(text, dialect).syntax();
        let name = root.descendants().find(|node| node.kind() == NAME).expect("a name");
        Ident::of_name(&name, dialect).expect("a token")
    }

    #[test]
    fn reads_names_the_way_each_dialect_does() {
        assert_eq!(first_name("SELECT Users", Dialect::Postgres).text, "users");
        assert_eq!(first_name("SELECT \"Users\"", Dialect::Postgres).text, "Users");
        assert_eq!(first_name("SELECT \"a\"\"b\"", Dialect::Postgres).text, "a\"b");
        assert_eq!(first_name("SELECT Users", Dialect::Mysql).text, "Users");
        assert_eq!(first_name("SELECT `my table`", Dialect::Mysql).text, "my table");
        assert_eq!(first_name("SELECT [x y]", Dialect::Sqlite).text, "x y");
        assert!(first_name("SELECT [x y]", Dialect::Sqlite).quoted);
    }

    #[test]
    fn quotes_what_cannot_stand_bare() {
        let postgres = Target::new(Dialect::Postgres, None);
        assert_eq!(quote_name("users", postgres), "users");
        assert_eq!(quote_name("Users", postgres), "\"Users\"");
        assert_eq!(quote_name("order", postgres), "\"order\"");
        assert_eq!(quote_name("my table", postgres), "\"my table\"");
        let mysql = Target::new(Dialect::Mysql, None);
        assert_eq!(quote_name("Users", mysql), "Users");
        assert_eq!(quote_name("order", mysql), "`order`");
        assert_eq!(quote_name("1st", mysql), "1st");
        assert_eq!(quote_name("a`b", mysql), "`a``b`");
        assert_eq!(quote_name("1st", Target::new(Dialect::Sqlite, None)), "\"1st\"");
    }

    #[test]
    fn compares_names() {
        assert!(Case::Insensitive.eq("Users", "USERS"));
        assert!(!Case::Exact.eq("Users", "users"));
        assert!(Case::Insensitive.eq("Ärger", "ärger"));
    }
}
