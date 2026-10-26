use crate::reserved::reserved_ranges;
use crate::reserved_words::{MARIADB, MYSQL, MYSQL_8_0_ONLY, MYSQL_8_4, POSTGRES, SQLITE};
use crate::{Dialect, Target, Version, is_reserved_word};

fn words(text: &str, dialect: Dialect) -> Vec<String> {
    reserved_ranges(text, Target::new(dialect, None))
        .into_iter()
        .map(|(_, word)| word)
        .collect()
}

#[test]
fn the_lists_are_sorted_capitals() {
    for list in [SQLITE, MYSQL, MYSQL_8_0_ONLY, MYSQL_8_4, MARIADB, POSTGRES] {
        let mut sorted = list.to_vec();
        sorted.sort();
        assert_eq!(list, sorted.as_slice());
        assert!(list.iter().all(|word| {
            word.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        }));
    }
}

#[test]
fn a_reserved_word_is_reported_where_it_names_something() {
    let text = "CREATE TABLE kv (key TEXT, value TEXT, rank INT);";
    assert_eq!(words(text, Dialect::Mysql), ["key", "rank"]);
    assert_eq!(words(text, Dialect::Mariadb), ["key"]);
    assert!(words(text, Dialect::Sqlite).is_empty());
    assert!(words(text, Dialect::Postgres).is_empty());
    assert!(words(text, Dialect::Generic).is_empty());
    assert_eq!(words("CREATE TABLE t (order INT);", Dialect::Generic), ["order"]);
}

#[test]
fn quoted_qualified_and_function_names_are_left_alone() {
    let text = "SELECT `key`, t.key, left(a, 1), CAST(a AS INT) FROM t;";
    assert!(words(text, Dialect::Mysql).is_empty());
    assert_eq!(words("SELECT t.order FROM t;", Dialect::Sqlite), ["order"]);
    assert!(words("SELECT 1 AS user;", Dialect::Postgres).is_empty());
    assert_eq!(words("SELECT 1 user;", Dialect::Postgres), ["user"]);
    assert_eq!(words("SELECT 1 AS key;", Dialect::Mysql), ["key"]);
}

#[test]
fn what_mysql_reserves_follows_its_version() {
    let qualify = |version: &str| is_reserved_word("QUALIFY", Target::new(Dialect::Mysql, Version::parse(version)));
    assert!(!qualify("8.0.40"));
    assert!(qualify("8.4"));
    let bind = |version: &str| is_reserved_word("MASTER_BIND", Target::new(Dialect::Mysql, Version::parse(version)));
    assert!(bind("8.0.40"));
    assert!(!bind("8.4"));
}
