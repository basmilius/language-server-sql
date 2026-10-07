use std::path::Path;

use expect_test::{Expect, expect};
use sql_syntax::Dialect;

use crate::catalog::Layer;
use crate::context::Schemas;
use crate::references::{Current, OtherFile};
use crate::rename::{apply, prepare_rename, rename};
use crate::testing::{shop, split_cursor, target, with_snapshot};
use crate::workspace::{build, extract};

/// The document after the rename, or why it was refused.
fn renamed(dialect: Dialect, schemas: Schemas, code: &str, new_name: &str) -> String {
    let (text, root, offset) = split_cursor(code, dialect);
    let current = Current {
        root: &root,
        target: target(dialect),
        schemas,
    };
    match rename(&current, offset, new_name, &[]) {
        Ok(files) => {
            let edits: Vec<_> = files
                .into_iter()
                .filter(|file| file.path.is_none())
                .flat_map(|file| file.edits)
                .collect();
            apply(&text, &edits)
        }
        Err(message) => format!("refused: {message}"),
    }
}

fn check(dialect: Dialect, code: &str, new_name: &str, expect: Expect) {
    expect.assert_eq(&renamed(dialect, Schemas::NONE, code, new_name));
}

fn check_shop(dialect: Dialect, code: &str, new_name: &str, expect: Expect) {
    let layer = shop(dialect);
    expect.assert_eq(&renamed(dialect, with_snapshot(&layer), code, new_name));
}

#[test]
fn renames_an_alias_in_its_statement() {
    check(
        Dialect::Postgres,
        "SELECT u.id FROM users $0u WHERE u.id > 1;\nSELECT u.id FROM users u;",
        "people",
        expect![[r#"
            SELECT people.id FROM users people WHERE people.id > 1;
            SELECT u.id FROM users u;"#]],
    );
}

#[test]
fn quotes_a_new_name_where_the_dialect_needs_it() {
    check(
        Dialect::Postgres,
        "SELECT u.id FROM users $0u;",
        "Order",
        expect![[r#"SELECT "Order".id FROM users "Order";"#]],
    );
    check(
        Dialect::Mysql,
        "SELECT u.id FROM users $0u;",
        "order",
        expect![[r#"SELECT `order`.id FROM users `order`;"#]],
    );
    check(
        Dialect::Mysql,
        "SELECT `u`.id FROM users $0u;",
        "x",
        expect![[r#"SELECT `x`.id FROM users x;"#]],
    );
}

#[test]
fn renames_a_common_table_expression_and_its_columns() {
    check(
        Dialect::Postgres,
        "WITH $0recent AS (SELECT 1 AS n) SELECT recent.n FROM recent;",
        "latest",
        expect![[r#"WITH latest AS (SELECT 1 AS n) SELECT latest.n FROM latest;"#]],
    );
    check(
        Dialect::Postgres,
        "WITH recent AS (SELECT 1 AS $0n) SELECT recent.n FROM recent ORDER BY n;",
        "total",
        expect![[r#"WITH recent AS (SELECT 1 AS total) SELECT recent.total FROM recent ORDER BY total;"#]],
    );
}

#[test]
fn refuses_a_name_that_would_change_what_other_names_stand_for() {
    check(
        Dialect::Postgres,
        "SELECT u.id, o.id FROM users $0u JOIN orders o ON true;",
        "o",
        expect![[
            r#"refused: 'o' is already a name in this scope: renaming the alias would change what other names stand for"#
        ]],
    );
    check(
        Dialect::Postgres,
        "WITH a AS (SELECT 1), $0b AS (SELECT 2) SELECT * FROM a, b;",
        "a",
        expect![[
            r#"refused: 'a' is already a name in this scope: renaming the common table expression would change what other names stand for"#
        ]],
    );
}

#[test]
fn renames_variables_and_parameters_of_a_routine() {
    check(
        Dialect::Mysql,
        "DELIMITER //\nCREATE PROCEDURE p(IN $0a INT)\nBEGIN\n  DECLARE x INT;\n  SET x = a + 1;\nEND//\nDELIMITER ;\n",
        "amount",
        expect![[r#"
            DELIMITER //
            CREATE PROCEDURE p(IN amount INT)
            BEGIN
              DECLARE x INT;
              SET x = amount + 1;
            END//
            DELIMITER ;
        "#]],
    );
    check(
        Dialect::Mysql,
        "SET @n = 1;\nSELECT $0@n + 1;",
        "count",
        expect![[r#"
            SET @count = 1;
            SELECT @count + 1;"#]],
    );
}

#[test]
fn renames_a_table_of_the_document_with_its_ddl() {
    check(
        Dialect::Postgres,
        "CREATE TABLE $0t (a int);\nCREATE INDEX t_a ON t (a);\nSELECT t.a FROM t;\nINSERT INTO public.t VALUES (1);",
        "items",
        expect![[r#"
            CREATE TABLE items (a int);
            CREATE INDEX t_a ON items (a);
            SELECT items.a FROM items;
            INSERT INTO public.items VALUES (1);"#]],
    );
    check(
        Dialect::Postgres,
        "CREATE TABLE t (a int);\nSELECT $0a, t.a FROM t WHERE a > 0;",
        "b",
        expect![[r#"
            CREATE TABLE t (b int);
            SELECT b, t.b FROM t WHERE b > 0;"#]],
    );
}

#[test]
fn a_column_renamed_follows_into_views_that_pass_it_on() {
    check(
        Dialect::Postgres,
        "CREATE TABLE t ($0a int);\nCREATE VIEW v AS SELECT a FROM t;\nSELECT a FROM v;\nCREATE VIEW w (x) AS SELECT a FROM t;\nSELECT x FROM w;",
        "b",
        expect![[r#"
            CREATE TABLE t (b int);
            CREATE VIEW v AS SELECT b FROM t;
            SELECT b FROM v;
            CREATE VIEW w (x) AS SELECT b FROM t;
            SELECT x FROM w;"#]],
    );
}

#[test]
fn refuses_a_taken_name_and_a_captured_one() {
    check(
        Dialect::Postgres,
        "CREATE TABLE t (a int, b int);\nSELECT $0a FROM t;",
        "b",
        expect![[r#"refused: The table 't' already has a column named 'b'"#]],
    );
    check(
        Dialect::Postgres,
        "CREATE TABLE t (a int);\nCREATE TABLE u (b int);\nSELECT $0a FROM t, u;",
        "b",
        expect![[
            r#"refused: 'b' already names something where the column is named, so the column would no longer be the one meant"#
        ]],
    );
    check(
        Dialect::Postgres,
        "CREATE TABLE $0t (a int);\nCREATE TABLE u (b int);",
        "u",
        expect![[r#"refused: There is already a table named 'u'"#]],
    );
    check(
        Dialect::Postgres,
        "CREATE TABLE $0t (a int);\nWITH u AS (SELECT 1) SELECT * FROM t;",
        "u",
        expect![[r#"refused: A common table expression named 'u' is in scope where the table is named"#]],
    );
}

#[test]
fn refuses_what_only_a_snapshot_or_the_database_has() {
    check_shop(
        Dialect::Postgres,
        "SELECT * FROM $0users;",
        "people",
        expect![[
            r#"refused: The table 'users' is defined only in the schema snapshot, not in a file, so renaming it here would not rename it in the database"#
        ]],
    );
    check_shop(
        Dialect::Postgres,
        "SELECT $0lower('A');",
        "low",
        expect![[r#"refused: 'lower' is a built-in function and cannot be renamed"#]],
    );
    check_shop(
        Dialect::Postgres,
        "SELECT relname FROM $0pg_class;",
        "x",
        expect![[r#"refused: The table 'pg_class' belongs to the database itself and cannot be renamed"#]],
    );
    check(
        Dialect::Postgres,
        "SELECT * FROM $0nowhere;",
        "x",
        expect![[r#"refused: No DDL in the workspace defines the table 'nowhere', so a rename cannot be complete"#]],
    );
    check(
        Dialect::Postgres,
        "SELECT $0;",
        "x",
        expect![[r#"refused: There is no name to rename here"#]],
    );
}

#[test]
fn refuses_a_name_also_in_a_string_of_sql() {
    check(
        Dialect::Postgres,
        "CREATE TABLE $0t (a int);\nCREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT count(*) FROM t $$;",
        "items",
        expect![[r#"refused: 't' is also named inside a string of SQL (line 2), which a rename cannot follow"#]],
    );
}

#[test]
fn prepare_offers_the_name_without_quotes() {
    let (text, root, offset) = split_cursor("SELECT \"u\".id FROM users AS $0\"u\";", Dialect::Postgres);
    let current = Current {
        root: &root,
        target: target(Dialect::Postgres),
        schemas: Schemas::NONE,
    };
    let prepared = prepare_rename(&current, offset).expect("renamable");
    assert_eq!(prepared.placeholder, "u");
    assert_eq!(&text[prepared.range], "\"u\"");
}

fn workspace(files: &[(&str, &str)], dialect: Dialect) -> Layer {
    let extracted: Vec<_> = files
        .iter()
        .map(|(path, text)| (Path::new(*path), extract(text, dialect)))
        .collect();
    build(dialect, extracted.iter().map(|(path, ddl)| (*path, ddl)))
}

#[test]
fn renames_a_table_across_the_workspace() {
    let files = [
        (
            "/w/1_users.sql",
            "CREATE TABLE users (id int, email text);\nCREATE SEQUENCE user_ids;\n",
        ),
        (
            "/w/report.sql",
            "SELECT u.email FROM users u;\nSELECT users.id FROM users;\n",
        ),
    ];
    let layer = workspace(&files, Dialect::Postgres);
    let schemas = Schemas {
        snapshot: None,
        workspace: Some(&layer),
    };
    let others: Vec<OtherFile> = files
        .iter()
        .map(|(path, text)| OtherFile {
            path: Path::new(path),
            text,
            target: target(Dialect::Postgres),
            schemas,
        })
        .collect();
    let run = |code: &str, new_name: &str| -> String {
        let (text, root, offset) = split_cursor(code, Dialect::Postgres);
        let current = Current {
            root: &root,
            target: target(Dialect::Postgres),
            schemas,
        };
        match rename(&current, offset, new_name, &others) {
            Ok(found) => found
                .iter()
                .map(|file| {
                    let (name, body) = match &file.path {
                        None => ("current".to_string(), text.clone()),
                        Some(path) => (
                            path.display().to_string(),
                            files
                                .iter()
                                .find(|(own, _)| Path::new(own) == path)
                                .expect("a file")
                                .1
                                .to_string(),
                        ),
                    };
                    format!("{name}: {}", apply(&body, &file.edits))
                })
                .collect(),
            Err(message) => format!("refused: {message}"),
        }
    };
    expect![[r#"
        current: SELECT email FROM people WHERE id = 1;
        /w/1_users.sql: CREATE TABLE people (id int, email text);
        CREATE SEQUENCE user_ids;
        /w/report.sql: SELECT u.email FROM people u;
        SELECT people.id FROM people;
    "#]]
    .assert_eq(&run("SELECT email FROM $0users WHERE id = 1;\n", "people"));
    expect![[r#"
        current: SELECT nextval('ids');
        /w/1_users.sql: CREATE TABLE users (id int, email text);
        CREATE SEQUENCE ids;
    "#]]
    .assert_eq(&run("SELECT nextval('$0user_ids');\n", "ids"));
}
