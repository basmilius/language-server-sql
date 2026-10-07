//! Reports words a dialect reserves where they name a column, a table or an alias without quotes.
//! The parser reads any keyword as a name where a name must stand, since what is reserved differs
//! per dialect; this pass is where the dialect decides. The lists come from the servers themselves,
//! in `reserved_words.rs`.

use crate::SyntaxKind::{self, *};
use crate::features::{FeatureDiagnostic, FeatureSeverity};
use crate::reserved_words::{MARIADB, MYSQL, MYSQL_8_0_ONLY, MYSQL_8_4, POSTGRES, SQLITE};
use crate::{Dialect, SyntaxNode, Target, Version};

/// The id of the findings, which a client switches the check off by.
pub const RESERVED_WORD: &str = "reserved-word";

fn listed(list: &[&str], word: &str) -> bool {
    list.binary_search(&word).is_ok()
}

/// Whether `word`, in capitals, is reserved by the target. Without a dialect only a word every
/// dialect reserves is.
pub fn is_reserved_word(word: &str, target: Target) -> bool {
    match target.dialect {
        Dialect::Generic => Dialect::DATABASES
            .iter()
            .all(|dialect| is_reserved_word(word, Target::new(*dialect, None))),
        Dialect::Sqlite => listed(SQLITE, word),
        Dialect::Mysql => {
            let since_8_4 = target.at_least(Version::new(8, 4, 0));
            listed(MYSQL, word)
                || (since_8_4 && listed(MYSQL_8_4, word))
                || (!since_8_4 && listed(MYSQL_8_0_ONLY, word))
        }
        Dialect::Mariadb => listed(MARIADB, word),
        Dialect::Postgres => listed(POSTGRES, word),
    }
}

/// Where a name is checked: what names a column, a table, an alias, a constraint or a parameter.
/// Function names, type names and the words of options stand in other places.
fn checked(name: &SyntaxNode) -> bool {
    let Some(parent) = name.parent() else {
        return false;
    };
    match parent.kind() {
        COLUMN_DEF | COLUMN_REF | ALIAS | NAME_LIST | CTE | WINDOW_DEF | DROP_COLUMN_ACTION | RENAME_COLUMN_ACTION
        | ALTER_COLUMN_ACTION | MODIFY_COLUMN_ACTION | TABLE_CONSTRAINT | COLUMN_CONSTRAINT | PARAM_DEF
        | DECLARE_STMT | CREATE_SCHEMA_STMT => true,
        QUALIFIED_NAME => parent.parent().is_some_and(|owner| {
            matches!(
                owner.kind(),
                TABLE_REF
                    | CREATE_TABLE_STMT
                    | CREATE_VIEW_STMT
                    | CREATE_INDEX_STMT
                    | INSERT_STMT
                    | DROP_STMT
                    | ALTER_TABLE_STMT
                    | TRUNCATE_STMT
                    | REFERENCES_CLAUSE
                    | CREATE_SEQUENCE_STMT
                    | CREATE_TYPE_STMT
                    | CREATE_DOMAIN_STMT
                    | CREATE_FUNCTION_STMT
                    | CREATE_TRIGGER_STMT
                    | RENAME_TABLE_STMT
                    | MERGE_STMT
                    | COPY_STMT
                    | LIKE_CLAUSE
            )
        }),
        _ => false,
    }
}

fn previous_token_kind(name: &SyntaxNode) -> Option<SyntaxKind> {
    let mut token = name.first_token()?.prev_token();
    while let Some(previous) = token {
        if !previous.kind().is_trivia() {
            return Some(previous.kind());
        }
        token = previous.prev_token();
    }
    None
}

/// Reserved words used as names without quotes.
pub fn check_reserved_words(root: &SyntaxNode, target: Target) -> Vec<FeatureDiagnostic> {
    let mut found = Vec::new();
    for name in root.descendants().filter(|node| node.kind() == NAME) {
        let Some(token) = name.first_token() else {
            continue;
        };
        let unquoted = token.kind() == IDENT || token.kind().is_keyword();
        if !unquoted || !checked(&name) {
            continue;
        }
        let previous = previous_token_kind(&name);
        // MySQL and PostgreSQL take any word after a dot, and PostgreSQL any word after AS.
        if previous == Some(DOT) && target.dialect != Dialect::Sqlite {
            continue;
        }
        let explicit_alias = previous == Some(AS_KW) && name.parent().is_some_and(|parent| parent.kind() == ALIAS);
        if explicit_alias && target.dialect == Dialect::Postgres {
            continue;
        }
        let word = token.text().to_ascii_uppercase();
        if !is_reserved_word(&word, target) {
            continue;
        }
        let dialect = match target.dialect {
            Dialect::Generic => "every dialect".to_string(),
            dialect => dialect.name().to_string(),
        };
        found.push(FeatureDiagnostic {
            range: token.text_range(),
            message: format!(
                "'{}' is a reserved word in {dialect}; quote it to use it as a name",
                token.text()
            ),
            severity: FeatureSeverity::Error,
            deprecated: false,
            feature: RESERVED_WORD,
        });
    }
    found
}

/// The range of every word in `text` that `target` reserves, for tests.
#[cfg(test)]
pub(crate) fn reserved_ranges(text: &str, target: Target) -> Vec<(rowan::TextRange, String)> {
    let parsed = crate::parse(text, target.dialect);
    check_reserved_words(&parsed.syntax(), target)
        .into_iter()
        .map(|finding| (finding.range, text[finding.range].to_string()))
        .collect()
}
