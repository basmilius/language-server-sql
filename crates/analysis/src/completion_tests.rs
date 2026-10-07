use sql_syntax::{Dialect, Target, Version};

use crate::completion::{CompletionItem, CompletionList, CompletionOptions, ItemKind, complete};
use crate::context::Schemas;
use crate::testing::{shop, target, with_snapshot};

fn run_in(target: Target, schemas: Schemas, code: &str) -> CompletionList {
    let (offset, text) = lsc_text::testing::cursor(code);
    complete(&text, offset, target, schemas, CompletionOptions::default())
}

fn run(dialect: Dialect, code: &str) -> CompletionList {
    let layer = shop(dialect);
    run_in(target(dialect), with_snapshot(&layer), code)
}

fn labels(list: &CompletionList) -> Vec<&str> {
    list.items.iter().map(|item| item.label.as_str()).collect()
}

fn first(list: &CompletionList, count: usize) -> Vec<&str> {
    labels(list).into_iter().take(count).collect()
}

fn item<'a>(list: &'a CompletionList, label: &str) -> &'a CompletionItem {
    list.items
        .iter()
        .find(|item| item.label == label)
        .unwrap_or_else(|| panic!("no item {label} in {:?}", labels(list)))
}

fn has(list: &CompletionList, label: &str) -> bool {
    list.items.iter().any(|item| item.label == label)
}

fn applied(code: &str, item: &CompletionItem) -> String {
    let mut text = code.replacen(lsc_text::testing::CURSOR, "", 1);
    text.replace_range(item.edit.start as usize..item.edit.end as usize, &item.edit.new_text);
    text
}

#[test]
fn tables_after_from_with_ctes_first() {
    let list = run(Dialect::Postgres, "WITH recent AS (SELECT 1) SELECT * FROM $0");
    assert_eq!(first(&list, 5), ["recent", "orgs", "users", "orders", "active_users"]);
    let users = item(&list, "users");
    assert_eq!(users.kind, ItemKind::Table);
    assert_eq!(users.detail.as_deref(), Some("table"));
    assert_eq!(users.description.as_deref(), Some("public"));
    assert!(
        users
            .documentation
            .as_deref()
            .is_some_and(|text| text.starts_with("People who log in"))
    );
    assert_eq!(item(&list, "active_users").kind, ItemKind::View);
    assert!(has(&list, "audit"), "schemas come after the tables");
    assert!(has(&list, "pg_class"), "system tables last");
    let list = run(Dialect::Postgres, "SELECT * FROM us$0");
    assert_eq!(first(&list, 1), ["users"]);
    assert_eq!(applied("SELECT * FROM us$0", &list.items[0]), "SELECT * FROM users");
}

#[test]
fn tables_of_a_schema_after_its_name() {
    let list = run(Dialect::Postgres, "SELECT * FROM audit.$0");
    assert_eq!(labels(&list), ["events"]);
    let list = run(Dialect::Postgres, "SELECT * FROM information_schema.$0");
    assert!(has(&list, "columns") && has(&list, "tables"));
}

#[test]
fn columns_of_the_tables_in_scope_first() {
    let list = run(
        Dialect::Postgres,
        "SELECT $0 FROM users u JOIN orgs o ON o.id = u.org_id",
    );
    assert_eq!(
        first(&list, 7),
        ["id", "org_id", "email", "status", "name", "id", "name"]
    );
    let email = item(&list, "email");
    assert_eq!(email.detail.as_deref(), Some("varchar(255)"));
    assert_eq!(email.description.as_deref(), Some("u"));
    assert_eq!(email.documentation.as_deref(), Some("Where mail goes\n\nNOT NULL"));
    assert!(has(&list, "u") && has(&list, "o"), "aliases as qualifiers");
    assert!(has(&list, "count"), "functions");
    assert!(has(&list, "CASE"), "expression keywords");
    let after_alias = run(Dialect::Postgres, "SELECT u.$0 FROM users u");
    assert_eq!(labels(&after_alias), ["id", "org_id", "email", "status", "name"]);
}

#[test]
fn a_star_expands_to_the_columns() {
    let list = run(Dialect::Postgres, "SELECT $0 FROM orgs");
    let all = item(&list, "id, name");
    assert_eq!(all.kind, ItemKind::Snippet);
    assert_eq!(all.edit.new_text, "id, name");
    let list = run(Dialect::Postgres, "SELECT $0 FROM orgs o, users u");
    assert!(
        list.items
            .iter()
            .any(|item| item.edit.new_text.starts_with("o.id, o.name, u.id"))
    );
}

#[test]
fn join_conditions_come_from_foreign_keys() {
    let list = run(Dialect::Postgres, "SELECT * FROM users u JOIN $0");
    assert_eq!(
        first(&list, 2),
        ["orgs ON u.org_id = orgs.id", "orders ON u.id = orders.user_id"]
    );
    let list = run(Dialect::Postgres, "SELECT * FROM users u JOIN orgs o ON $0");
    assert_eq!(first(&list, 1), ["o.id = u.org_id"]);
}

#[test]
fn insert_column_lists_and_values_templates() {
    let list = run(Dialect::Postgres, "INSERT INTO users ($0");
    assert_eq!(first(&list, 2), ["id, org_id, email, status, name", "id"]);
    let list = run(Dialect::Postgres, "INSERT INTO users (email, $0");
    assert!(!has(&list, "email"), "a listed column is not offered again");
    let list = run(Dialect::Postgres, "INSERT INTO users $0");
    let template = item(&list, "(org_id, email, status, name) VALUES (...)");
    assert!(template.snippet);
    assert_eq!(
        template.edit.new_text,
        "(org_id, email, status, name) VALUES (${1:org_id}, ${2:email}, ${3:status}, ${4:name})"
    );
    let list = run(Dialect::Postgres, "INSERT INTO users (email, name) $0");
    assert_eq!(
        item(&list, "VALUES (email, name)").edit.new_text,
        "VALUES (${1:email}, ${2:name})"
    );
}

#[test]
fn functions_with_snippets_and_without() {
    let list = run(Dialect::Postgres, "SELECT coun$0");
    let count = item(&list, "count");
    assert!(count.snippet);
    assert_eq!(count.edit.new_text, "count($1)$0");
    assert_eq!(count.description.as_deref(), Some("aggregate function"));
    let now = item(&run(Dialect::Postgres, "SELECT no$0"), "now")
        .edit
        .new_text
        .clone();
    assert_eq!(now, "now()$0");
    let layer = shop(Dialect::Mysql);
    let (offset, text) = lsc_text::testing::cursor("SELECT conc$0");
    let plain = complete(
        &text,
        offset,
        target(Dialect::Mysql),
        with_snapshot(&layer),
        CompletionOptions {
            snippets: false,
            ..CompletionOptions::default()
        },
    );
    assert_eq!(item(&plain, "concat").edit.new_text, "concat()");
    let routine = item(&run(Dialect::Postgres, "SELECT order_$0"), "order_total").clone();
    assert_eq!(
        routine.detail.as_deref(),
        Some("order_total(order_id integer): numeric")
    );
    let procedures = run(Dialect::Mysql, "CALL $0");
    assert_eq!(labels(&procedures), ["archive"]);
}

#[test]
fn functions_and_keywords_follow_the_version() {
    let at = |version: &str| Target::new(Dialect::Mariadb, Version::parse(version));
    let layer = shop(Dialect::Mariadb);
    let old = run_in(at("11.4"), with_snapshot(&layer), "SELECT UUID_V$0");
    assert!(!has(&old, "UUID_V7"));
    let new = run_in(at("11.8"), with_snapshot(&layer), "SELECT UUID_V$0");
    assert!(has(&new, "UUID_V7"));
    let mysql = run(Dialect::Mysql, "SELECT * FROM users u $0");
    assert!(has(&mysql, "LEFT JOIN") && !has(&mysql, "FULL JOIN"));
    let postgres = run(Dialect::Postgres, "SELECT * FROM users u $0");
    assert!(has(&postgres, "FULL JOIN") && has(&postgres, "WHERE") && has(&postgres, "ORDER BY"));
}

#[test]
fn keywords_where_a_statement_or_a_clause_begins() {
    let list = run(Dialect::Postgres, "$0");
    assert!(has(&list, "SELECT") && has(&list, "MERGE INTO") && !has(&list, "PRAGMA"));
    let list = run(Dialect::Sqlite, "SELECT 1;\npr$0");
    assert_eq!(labels(&list), ["pragma"]);
    let list = run(Dialect::Postgres, "SELECT * FROM users WHERE id = 1 $0");
    assert!(has(&list, "AND") && has(&list, "ORDER BY") && !has(&list, "WHERE"));
    let list = run(Dialect::Postgres, "SELECT id $0");
    assert!(has(&list, "FROM") && has(&list, "AS"));
    let list = run(Dialect::Postgres, "UPDATE users u $0");
    assert!(has(&list, "SET"));
    let list = run(Dialect::Mysql, "INSERT INTO users (email) VALUES ('a') $0");
    assert!(has(&list, "ON DUPLICATE KEY UPDATE") && !has(&list, "ON CONFLICT"));
}

#[test]
fn types_in_definitions() {
    let list = run(Dialect::Postgres, "CREATE TABLE t (a $0");
    assert!(has(&list, "integer") && has(&list, "text") && has(&list, "mood"));
    assert!(!has(&list, "anyelement"), "pseudo types do not name a column's type");
    let list = run(Dialect::Mysql, "CREATE TABLE t (a VARCHAR(10) $0");
    assert!(
        has(&list, "NOT NULL") && has(&list, "AUTO_INCREMENT"),
        "{:?}",
        labels(&list)
    );
}

#[test]
fn enum_values_where_an_enum_column_is_compared() {
    let list = run(Dialect::Mysql, "SELECT * FROM users WHERE status = $0");
    assert_eq!(first(&list, 2), ["'active'", "'blocked'"]);
    let code = "SELECT * FROM users WHERE status = 'bl$0'";
    let list = run(Dialect::Mysql, code);
    assert_eq!(labels(&list), ["blocked"]);
    assert_eq!(
        applied(code, &list.items[0]),
        "SELECT * FROM users WHERE status = 'blocked'"
    );
    let list = run(Dialect::Postgres, "SELECT * FROM orders WHERE mood IN ('$0");
    assert_eq!(labels(&list), ["happy", "sad"]);
    let list = run(Dialect::Postgres, "SELECT nextval('$0')");
    assert_eq!(labels(&list), ["order_numbers"]);
}

#[test]
fn names_are_quoted_where_the_dialect_needs_it() {
    let snapshot = sql_catalog::read_snapshot(
        r#"{ "formatVersion": 1, "schemas": [ { "name": "public", "tables": [ { "name": "Order Lines", "columns": [ { "name": "select" }, { "name": "Qty" } ] } ] } ] }"#,
    )
    .expect("a snapshot");
    let layer = crate::catalog::Layer::new(crate::catalog::Origin::Snapshot, snapshot);
    let list = run_in(
        target(Dialect::Postgres),
        with_snapshot(&layer),
        "SELECT $0 FROM \"Order Lines\"",
    );
    assert_eq!(item(&list, "select").edit.new_text, "\"select\"");
    assert_eq!(item(&list, "Qty").edit.new_text, "\"Qty\"");
    let list = run_in(target(Dialect::Mysql), with_snapshot(&layer), "SELECT * FROM Ord$0");
    assert_eq!(item(&list, "Order Lines").edit.new_text, "`Order Lines`");
    let code = "SELECT * FROM \"Ord$0";
    let list = run_in(target(Dialect::Postgres), with_snapshot(&layer), code);
    assert_eq!(
        applied(code, item(&list, "Order Lines")),
        "SELECT * FROM \"Order Lines\""
    );
}

#[test]
fn a_document_without_a_snapshot_completes_from_its_own_ddl() {
    let list = run_in(
        target(Dialect::Sqlite),
        Schemas::NONE,
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);\nSELECT $0 FROM notes;",
    );
    assert_eq!(first(&list, 2), ["id", "body"]);
    assert_eq!(item(&list, "body").detail.as_deref(), Some("TEXT"));
}

#[test]
fn group_by_and_order_by_offer_select_aliases() {
    let list = run(
        Dialect::Postgres,
        "SELECT org_id AS o, count(*) AS n FROM users GROUP BY o ORDER BY $0",
    );
    assert!(has(&list, "n") && has(&list, "o"));
    assert_eq!(item(&list, "n").kind, ItemKind::Alias);
}

#[test]
fn mysql_variables_after_two_ats() {
    let list = run(Dialect::Mysql, "SELECT @@sql_mo$0");
    assert!(has(&list, "sql_mode"), "{:?}", labels(&list));
}

#[test]
fn invisible_columns_stay_out_of_the_lists_of_every_column() {
    let schema = "CREATE TABLE h (id INT, secret INT INVISIBLE, label VARCHAR(10));\n";
    let list = run_in(
        target(Dialect::Mysql),
        Schemas::NONE,
        &format!("{schema}SELECT $0 FROM h"),
    );
    assert!(has(&list, "secret"), "an invisible column may still be named");
    assert!(has(&list, "id, label"), "{:?}", labels(&list));
    let list = run_in(
        target(Dialect::Mysql),
        Schemas::NONE,
        &format!("{schema}INSERT INTO h $0"),
    );
    assert!(
        labels(&list)
            .iter()
            .any(|label| label.starts_with("(id, label) VALUES")),
        "{:?}",
        labels(&list)
    );
}
