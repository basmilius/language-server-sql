use sql_syntax::{Dialect, Target, Version};

use crate::completion::{CompletionItem, CompletionList, CompletionOptions, ItemKind, QuoteIdentifiers, complete};
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

fn run_with(dialect: Dialect, options: CompletionOptions, code: &str) -> CompletionList {
    let layer = shop(dialect);
    let (offset, text) = lsc_text::testing::cursor(code);
    complete(&text, offset, target(dialect), with_snapshot(&layer), options)
}

/// The text an item leaves, what a client filters it by, and the range it replaces, which a client
/// filters on: from the start of the edit to the cursor.
fn quoted_item(dialect: Dialect, code: &str, label: &str) -> (String, String, String) {
    let list = run(dialect, code);
    let found = item(&list, label);
    let (offset, text) = lsc_text::testing::cursor(code);
    let typed = text[found.edit.start as usize..offset as usize].to_string();
    let filter = found.filter_text.clone().unwrap_or_else(|| found.label.clone());
    assert!(
        filter.to_lowercase().starts_with(&typed.to_lowercase()),
        "a client filtering on {typed:?} keeps {filter:?}"
    );
    (applied(code, found), filter, typed)
}

#[test]
fn a_backtick_left_open_completes_and_closes_the_name() {
    let (text, filter, typed) = quoted_item(Dialect::Mariadb, "SELECT * FROM `us$0", "users");
    assert_eq!(text, "SELECT * FROM `users`");
    assert_eq!((filter.as_str(), typed.as_str()), ("`users", "`us"));
    let (text, ..) = quoted_item(Dialect::Mariadb, "SELECT * FROM `$0", "orgs");
    assert_eq!(text, "SELECT * FROM `orgs`");
    let (text, ..) = quoted_item(Dialect::Mariadb, "SELECT * FROM `us$0\nWHERE `id` = 1;", "users");
    assert_eq!(text, "SELECT * FROM `users`\nWHERE `id` = 1;");
    let (text, ..) = quoted_item(Dialect::Mariadb, "SELECT * FROM `us$0 WHERE `id` = 1", "users");
    assert_eq!(text, "SELECT * FROM `users` WHERE `id` = 1");
    let (text, ..) = quoted_item(
        Dialect::Mariadb,
        "SELECT `email` FROM `us$0\nSELECT `id` FROM `orgs`;",
        "users",
    );
    assert_eq!(text, "SELECT `email` FROM `users`\nSELECT `id` FROM `orgs`;");
}

#[test]
fn a_backtick_the_editor_closed_is_replaced_with_the_name() {
    let (text, filter, _) = quoted_item(Dialect::Mariadb, "SELECT * FROM `$0`", "orgs");
    assert_eq!(text, "SELECT * FROM `orgs`");
    assert_eq!(filter, "`orgs");
    let (text, ..) = quoted_item(Dialect::Mariadb, "SELECT * FROM `us$0`", "users");
    assert_eq!(text, "SELECT * FROM `users`");
    let (text, ..) = quoted_item(Dialect::Mariadb, "SELECT * FROM `us$0ers` WHERE `id` = 1", "users");
    assert_eq!(text, "SELECT * FROM `users` WHERE `id` = 1");
    let (text, ..) = quoted_item(Dialect::Mariadb, "SELECT * FROM `$0` AS `u`", "users");
    assert_eq!(text, "SELECT * FROM `users` AS `u`");
}

#[test]
fn a_quoted_name_after_a_qualifier() {
    for (code, label, expected) in [
        ("SELECT * FROM `shop`.`$0", "users", "SELECT * FROM `shop`.`users`"),
        ("SELECT * FROM `shop`.`$0`", "users", "SELECT * FROM `shop`.`users`"),
        ("SELECT * FROM `shop`.`us$0`", "users", "SELECT * FROM `shop`.`users`"),
        ("SELECT * FROM `shop`.$0", "users", "SELECT * FROM `shop`.`users`"),
        ("SELECT * FROM shop.`$0", "users", "SELECT * FROM shop.`users`"),
        ("SELECT * FROM shop.`$0`", "users", "SELECT * FROM shop.`users`"),
        (
            "SELECT `u`.`em$0 FROM `users` AS `u`",
            "email",
            "SELECT `u`.`email` FROM `users` AS `u`",
        ),
        (
            "SELECT `u`.`em$0` FROM `users` AS `u`",
            "email",
            "SELECT `u`.`email` FROM `users` AS `u`",
        ),
        (
            "SELECT `u`.$0 FROM `users` AS `u`",
            "email",
            "SELECT `u`.`email` FROM `users` AS `u`",
        ),
        (
            "SELECT u.`$0` FROM users AS u",
            "email",
            "SELECT u.`email` FROM users AS u",
        ),
    ] {
        let (text, ..) = quoted_item(Dialect::Mariadb, code, label);
        assert_eq!(text, expected, "{code}");
    }
    let list = run(Dialect::Mariadb, "SELECT `u`.`$0 FROM `users` AS `u`");
    assert_eq!(labels(&list), ["id", "org_id", "email", "status", "name"]);
}

#[test]
fn a_quoted_word_offers_names_only() {
    let list = run(Dialect::Mariadb, "SELECT * FROM `users` WHERE `$0");
    assert_eq!(first(&list, 5), ["id", "org_id", "email", "status", "name"]);
    assert!(!has(&list, "CASE") && !has(&list, "COUNT"), "{:?}", labels(&list));
    let list = run(Dialect::Mariadb, "SELECT * FROM `users` WHERE `status` = `$0");
    assert!(!has(&list, "'active'"), "an enum value is a string, not a name");
    let list = run(Dialect::Mariadb, "SELECT `order_t$0");
    assert_eq!(item(&list, "order_total").edit.new_text, "`order_total`($1)$0");
    let list = run(Dialect::Mariadb, "SELECT * FROM `users` `$0");
    assert!(list.items.is_empty(), "an alias is being named: {:?}", labels(&list));
    let list = run(Dialect::Mariadb, "`$0");
    assert!(list.items.is_empty());
}

#[test]
fn quoted_names_in_every_context() {
    for (code, label, expected) in [
        ("SELECT `$0 FROM `users`", "email", "SELECT `email` FROM `users`"),
        (
            "SELECT `id`, `$0` FROM `users`",
            "email",
            "SELECT `id`, `email` FROM `users`",
        ),
        (
            "SELECT * FROM `users` WHERE `st$0",
            "status",
            "SELECT * FROM `users` WHERE `status`",
        ),
        (
            "SELECT * FROM `users` JOIN `$0",
            "orgs",
            "SELECT * FROM `users` JOIN `orgs`",
        ),
        ("INSERT INTO `users` (`$0", "email", "INSERT INTO `users` (`email`"),
        (
            "INSERT INTO `users` (`email`, `$0`)",
            "name",
            "INSERT INTO `users` (`email`, `name`)",
        ),
        ("UPDATE `users` SET `em$0", "email", "UPDATE `users` SET `email`"),
        (
            "UPDATE `users` SET `email` = 'a' WHERE `$0`",
            "id",
            "UPDATE `users` SET `email` = 'a' WHERE `id`",
        ),
        ("DELETE FROM `us$0", "users", "DELETE FROM `users`"),
        ("USE `sh$0", "shop", "USE `shop`"),
        ("USE `$0`", "shop", "USE `shop`"),
        ("ALTER TABLE `us$0", "users", "ALTER TABLE `users`"),
        ("DROP TABLE `us$0`", "users", "DROP TABLE `users`"),
        (
            "CREATE TABLE `x` (`a` INT REFERENCES `orgs` (`$0",
            "id",
            "CREATE TABLE `x` (`a` INT REFERENCES `orgs` (`id`",
        ),
        (
            "CREATE INDEX `i` ON `users` (`$0`)",
            "email",
            "CREATE INDEX `i` ON `users` (`email`)",
        ),
        (
            "CREATE TABLE `t` (`id` INT, PRIMARY KEY (`$0",
            "id",
            "CREATE TABLE `t` (`id` INT, PRIMARY KEY (`id`",
        ),
    ] {
        let (text, ..) = quoted_item(Dialect::Mariadb, code, label);
        assert_eq!(text, expected, "{code}");
    }
}

#[test]
fn templates_quote_their_names_the_way_the_statement_does() {
    let list = run(Dialect::Mariadb, "SELECT * FROM `users` AS `u` JOIN $0");
    assert_eq!(first(&list, 1), ["`orgs` ON `u`.`org_id` = `orgs`.`id`"]);
    let list = run(Dialect::Mariadb, "SELECT * FROM `users` AS `u` JOIN `$0");
    let join = &list.items[0];
    assert_eq!(join.edit.new_text, "`orgs` ON `u`.`org_id` = `orgs`.`id`");
    assert_eq!(join.filter_text.as_deref(), Some("`orgs"));
    let list = run(
        Dialect::Mariadb,
        "SELECT * FROM `users` AS `u` JOIN `orgs` AS `o` ON $0",
    );
    assert_eq!(first(&list, 1), ["`o`.`id` = `u`.`org_id`"]);
    let code = "SELECT * FROM `users` AS `u` JOIN `orgs` AS `o` ON `$0`";
    let list = run(Dialect::Mariadb, code);
    assert_eq!(
        applied(code, &list.items[0]),
        "SELECT * FROM `users` AS `u` JOIN `orgs` AS `o` ON `o`.`id` = `u`.`org_id`"
    );
    let list = run(Dialect::Mariadb, "INSERT INTO `users` $0");
    assert_eq!(
        list.items[0].edit.new_text,
        "(`org_id`, `email`, `status`, `name`) VALUES (${1:`org_id`}, ${2:`email`}, ${3:`status`}, ${4:`name`})"
    );
    let list = run(Dialect::Mariadb, "INSERT INTO `users` (`email`, `name`) $0");
    assert_eq!(list.items[0].label, "VALUES (`email`, `name`)");
    let list = run(Dialect::Mariadb, "INSERT INTO `users` (`$0");
    assert_eq!(list.items[0].edit.new_text, "`id`, `org_id`, `email`, `status`, `name`");
    let list = run(Dialect::Mariadb, "SELECT $0 FROM `orgs` AS `o`, `users` AS `u`");
    assert!(
        list.items
            .iter()
            .any(|item| item.edit.new_text.starts_with("`o`.`id`, `o`.`name`, `u`.`id`")),
        "{:?}",
        labels(&list)
    );
    let list = run(Dialect::Mariadb, "SELECT * FROM users u JOIN $0");
    assert_eq!(first(&list, 1), ["orgs ON u.org_id = orgs.id"], "bare names stay bare");
    let list = run(
        Dialect::Mariadb,
        "SELECT `id` FROM `orgs`;\nSELECT `name` FROM `users`;\nINSERT INTO users $0",
    );
    assert!(
        list.items[0].edit.new_text.starts_with("(org_id, email"),
        "the statement's own habit comes first: {}",
        list.items[0].edit.new_text
    );
    let list = run(
        Dialect::Mariadb,
        "SELECT `id` FROM `orgs`;\nSELECT `name` FROM `users`;\nSELECT * FROM $0",
    );
    assert!(has(&list, "orgs"));
}

#[test]
fn a_document_that_quotes_its_names_decides_where_the_statement_does_not() {
    let quoted = "SELECT `id` FROM `orgs`;\nSELECT `name` FROM `users`;\n";
    let list = run(Dialect::Mariadb, &format!("{quoted}SELECT * FROM `users` u JOIN $0"));
    assert_eq!(first(&list, 1), ["`orgs` ON `u`.`org_id` = `orgs`.`id`"]);
    let list = run(
        Dialect::Mariadb,
        "SELECT id FROM orgs;\nSELECT * FROM `users` u JOIN $0",
    );
    assert_eq!(first(&list, 1), ["orgs ON u.org_id = orgs.id"]);
    let list = run(Dialect::Mariadb, &format!("{quoted}INSERT INTO users (email, $0"));
    assert_eq!(
        item(&list, "name").edit.new_text,
        "name",
        "a name typed bare stays bare"
    );
}

#[test]
fn the_setting_forces_how_names_are_quoted() {
    let always = CompletionOptions {
        quote_identifiers: QuoteIdentifiers::Always,
        ..CompletionOptions::default()
    };
    let never = CompletionOptions {
        quote_identifiers: QuoteIdentifiers::Never,
        ..CompletionOptions::default()
    };
    let list = run_with(Dialect::Mariadb, always, "SELECT * FROM us$0");
    assert_eq!(item(&list, "users").edit.new_text, "`users`");
    let list = run_with(Dialect::Mariadb, always, "SELECT * FROM users u JOIN $0");
    assert_eq!(first(&list, 1), ["`orgs` ON `u`.`org_id` = `orgs`.`id`"]);
    let list = run_with(Dialect::Postgres, always, "SELECT * FROM users u JOIN $0");
    assert_eq!(first(&list, 1), ["\"orgs\" ON \"u\".\"org_id\" = \"orgs\".\"id\""]);
    let list = run_with(Dialect::Mariadb, never, "SELECT * FROM `users` AS `u` JOIN $0");
    assert_eq!(first(&list, 1), ["orgs ON u.org_id = orgs.id"]);
    let list = run_with(Dialect::Mariadb, never, "SELECT * FROM `users` AS `u` JOIN `$0");
    assert_eq!(
        list.items[0].edit.new_text, "`orgs` ON `u`.`org_id` = `orgs`.`id`",
        "a quote the person typed wins"
    );
    assert_eq!(QuoteIdentifiers::parse("Always"), Some(QuoteIdentifiers::Always));
    assert_eq!(QuoteIdentifiers::parse("sometimes"), None);
}

#[test]
fn double_quotes_and_brackets_where_the_dialect_reads_them_as_names() {
    for (dialect, code, label, expected) in [
        (
            Dialect::Postgres,
            "SELECT * FROM \"us$0",
            "users",
            "SELECT * FROM \"users\"",
        ),
        (
            Dialect::Postgres,
            "SELECT * FROM \"$0\"",
            "orgs",
            "SELECT * FROM \"orgs\"",
        ),
        (
            Dialect::Postgres,
            "SELECT * FROM \"public\".\"$0\"",
            "users",
            "SELECT * FROM \"public\".\"users\"",
        ),
        (
            Dialect::Postgres,
            "SELECT * FROM \"public\".$0",
            "users",
            "SELECT * FROM \"public\".\"users\"",
        ),
        (
            Dialect::Postgres,
            "SELECT \"u\".\"em$0\" FROM \"users\" AS \"u\"",
            "email",
            "SELECT \"u\".\"email\" FROM \"users\" AS \"u\"",
        ),
        (
            Dialect::Sqlite,
            "SELECT * FROM \"us$0",
            "users",
            "SELECT * FROM \"users\"",
        ),
        (
            Dialect::Sqlite,
            "SELECT * FROM `us$0`",
            "users",
            "SELECT * FROM `users`",
        ),
        (Dialect::Sqlite, "SELECT * FROM [us$0", "users", "SELECT * FROM [users]"),
        (Dialect::Sqlite, "SELECT * FROM [$0]", "orgs", "SELECT * FROM [orgs]"),
        (
            Dialect::Sqlite,
            "SELECT [u].[em$0] FROM [users] AS [u]",
            "email",
            "SELECT [u].[email] FROM [users] AS [u]",
        ),
    ] {
        let (text, ..) = quoted_item(dialect, code, label);
        assert_eq!(text, expected, "{dialect:?} {code}");
    }
    let list = run(Dialect::Postgres, "SELECT * FROM \"users\" AS \"u\" JOIN $0");
    assert_eq!(first(&list, 1), ["\"orgs\" ON \"u\".\"org_id\" = \"orgs\".\"id\""]);
    let list = run(Dialect::Sqlite, "SELECT * FROM [users] AS [u] JOIN $0");
    assert_eq!(first(&list, 1), ["[orgs] ON [u].[org_id] = [orgs].[id]"]);
    let list = run(Dialect::Postgres, "SELECT * FROM `us$0");
    assert!(list.items.is_empty(), "PostgreSQL has no backticks");
    let list = run(Dialect::Mysql, "SELECT * FROM \"us$0");
    assert!(list.items.is_empty(), "a string in MySQL");
    let list = run(Dialect::Mysql, "SELECT * FROM [us$0");
    assert!(!has(&list, "users") || item(&list, "users").edit.new_text == "users");
}

#[test]
fn double_quotes_are_names_in_mysql_under_ansi_quotes() {
    let code = "SET sql_mode = 'ANSI_QUOTES';\nSELECT * FROM \"us$0";
    let (text, ..) = quoted_item(Dialect::Mysql, code, "users");
    assert_eq!(text, "SET sql_mode = 'ANSI_QUOTES';\nSELECT * FROM \"users\"");
    let code = "SET sql_mode = 'ANSI';\nSELECT * FROM \"$0\"";
    let (text, ..) = quoted_item(Dialect::Mariadb, code, "orgs");
    assert_eq!(text, "SET sql_mode = 'ANSI';\nSELECT * FROM \"orgs\"");
}

#[test]
fn a_typed_quote_asks_only_where_it_opens_a_name() {
    let on = |trigger| CompletionOptions {
        trigger: Some(trigger),
        ..CompletionOptions::default()
    };
    assert!(has(&run_with(Dialect::Mariadb, on('`'), "SELECT * FROM `$0"), "users"));
    assert!(has(&run_with(Dialect::Mariadb, on('`'), "SELECT * FROM `$0`"), "users"));
    assert!(run_with(Dialect::Mariadb, on('"'), "SELECT \"$0").items.is_empty());
    assert!(has(
        &run_with(Dialect::Postgres, on('"'), "SELECT * FROM \"$0\""),
        "users"
    ));
    assert!(
        run_with(Dialect::Postgres, on('['), "SELECT a[$0 FROM t")
            .items
            .is_empty()
    );
    assert!(has(&run_with(Dialect::Sqlite, on('['), "SELECT * FROM [$0"), "users"));
    assert!(run_with(Dialect::Mariadb, on('`'), "SELECT '`$0'").items.is_empty());
}

#[test]
fn a_name_with_a_quote_in_it_doubles_the_quote() {
    let layer = |schema: &str| {
        let json = r#"{ "formatVersion": 1, "defaultSchema": "%S%", "schemas": [ { "name": "%S%", "tables": [ { "name": "a`b", "columns": [ { "name": "x]y" } ] } ] } ] }"#;
        let snapshot = sql_catalog::read_snapshot(&json.replace("%S%", schema)).expect("a snapshot");
        crate::catalog::Layer::new(crate::catalog::Origin::Snapshot, snapshot)
    };
    let mysql = layer("app");
    let list = run_in(target(Dialect::Mysql), with_snapshot(&mysql), "SELECT * FROM `a$0`");
    assert_eq!(item(&list, "a`b").edit.new_text, "`a``b`");
    let sqlite = layer("main");
    let list = run_in(
        target(Dialect::Sqlite),
        with_snapshot(&sqlite),
        "SELECT [$0 FROM \"a`b\"",
    );
    assert_eq!(
        item(&list, "x]y").edit.new_text,
        "\"x]y\"",
        "a bracket cannot hold a bracket"
    );
}
