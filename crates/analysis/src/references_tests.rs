use std::path::Path;

use expect_test::{Expect, expect};
use sql_syntax::{Dialect, TextRange};

use crate::catalog::Layer;
use crate::context::Schemas;
use crate::references::{Current, OtherFile, highlights, references};
use crate::refs::{Access, Hit};
use crate::testing::{shop, split_cursor, target, with_snapshot};
use crate::workspace::{build, extract};

/// The text with every hit in brackets, `:d` after a declaration and `:w` after a write.
pub fn marked(text: &str, hits: &[Hit]) -> String {
    let mut ranges: Vec<(TextRange, Access)> = hits.iter().map(|hit| (hit.range, hit.access)).collect();
    ranges.sort_by_key(|(range, _)| range.start());
    let mut out = String::new();
    let mut at = 0usize;
    for (range, access) in ranges {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        out.push_str(&text[at..start]);
        let suffix = match access {
            Access::Declaration => ":d",
            Access::Write => ":w",
            Access::Read => "",
        };
        out.push_str(&format!("[{}{suffix}]", &text[start..end]));
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

fn check_highlights(dialect: Dialect, schemas: Schemas, code: &str, expect: Expect) {
    let (text, root, offset) = split_cursor(code, dialect);
    let hits = highlights(&root, offset, target(dialect), schemas);
    expect.assert_eq(&marked(&text, &hits));
}

fn local(dialect: Dialect, code: &str, expect: Expect) {
    check_highlights(dialect, Schemas::NONE, code, expect);
}

fn with_shop(dialect: Dialect, code: &str, expect: Expect) {
    let layer = shop(dialect);
    check_highlights(dialect, with_snapshot(&layer), code, expect);
}

#[test]
fn an_alias_is_named_where_it_qualifies_a_column() {
    local(
        Dialect::Postgres,
        "SELECT u.id, u.email FROM users AS $0u JOIN orders o ON o.user_id = u.id WHERE u.id > 1;\nSELECT u.id FROM users u;",
        expect![[r#"
            SELECT [u].id, [u].email FROM users AS [u:d] JOIN orders o ON o.user_id = [u].id WHERE [u].id > 1;
            SELECT u.id FROM users u;"#]],
    );
}

#[test]
fn a_common_table_expression_is_named_by_its_uses_and_its_columns() {
    local(
        Dialect::Postgres,
        "WITH recent (uid) AS (SELECT id FROM orders) SELECT r.uid, recent.uid FROM recent r JOIN $0recent ON true;",
        expect![[
            r#"WITH [recent:d] (uid) AS (SELECT id FROM orders) SELECT r.uid, [recent].uid FROM [recent] r JOIN [recent] ON true;"#
        ]],
    );
    local(
        Dialect::Postgres,
        "WITH recent (uid) AS (SELECT id FROM orders) SELECT r.$0uid FROM recent r WHERE uid > 1 ORDER BY uid;",
        expect![[
            r#"WITH recent ([uid:d]) AS (SELECT id FROM orders) SELECT r.[uid] FROM recent r WHERE [uid] > 1 ORDER BY [uid];"#
        ]],
    );
}

#[test]
fn a_select_alias_is_named_where_the_dialect_sees_it() {
    local(
        Dialect::Postgres,
        "SELECT count(*) AS $0n FROM t GROUP BY a ORDER BY n;",
        expect![[r#"SELECT count(*) AS [n:d] FROM t GROUP BY a ORDER BY [n];"#]],
    );
    local(
        Dialect::Sqlite,
        "SELECT a + 1 AS $0b FROM t WHERE b > 2 ORDER BY b;",
        expect![[r#"SELECT a + 1 AS [b:d] FROM t WHERE [b] > 2 ORDER BY [b];"#]],
    );
}

#[test]
fn a_window_is_named_by_over_and_by_other_windows() {
    local(
        Dialect::Postgres,
        "SELECT sum(a) OVER w, rank() OVER (w ORDER BY b) FROM t WINDOW $0w AS (PARTITION BY c), w2 AS (w);",
        expect![[
            r#"SELECT sum(a) OVER [w], rank() OVER ([w] ORDER BY b) FROM t WINDOW [w:d] AS (PARTITION BY c), w2 AS ([w]);"#
        ]],
    );
}

#[test]
fn a_column_is_read_in_where_and_written_in_set() {
    with_shop(
        Dialect::Postgres,
        "UPDATE users SET $0email = lower(email) WHERE email LIKE '%@x' RETURNING email;\nINSERT INTO users (id, email) SELECT id, email FROM users;",
        expect![[r#"
            UPDATE users SET [email:w] = lower([email]) WHERE [email] LIKE '%@x' RETURNING [email];
            INSERT INTO users (id, [email:w]) SELECT id, [email] FROM users;"#]],
    );
}

#[test]
fn a_table_is_named_across_statements_with_its_writes() {
    with_shop(
        Dialect::Postgres,
        "SELECT * FROM $0users;\nDELETE FROM users WHERE id = 1;\nSELECT users.id FROM public.users JOIN orders ON true;\nTRUNCATE users;",
        expect![[r#"
            SELECT * FROM [users];
            DELETE FROM [users:w] WHERE id = 1;
            SELECT [users].id FROM public.[users] JOIN orders ON true;
            TRUNCATE [users:w];"#]],
    );
}

#[test]
fn a_column_is_followed_through_common_table_expressions_and_subqueries() {
    with_shop(
        Dialect::Postgres,
        "WITH a AS (SELECT id, email FROM users) SELECT x.email FROM (SELECT email FROM a) x WHERE x.$0email <> '';",
        expect![[
            r#"WITH a AS (SELECT id, [email] FROM users) SELECT x.[email] FROM (SELECT [email] FROM a) x WHERE x.[email] <> '';"#
        ]],
    );
}

#[test]
fn ddl_of_the_document_defines_tables_and_columns() {
    local(
        Dialect::Postgres,
        "CREATE TABLE t ($0a int, b int GENERATED ALWAYS AS (a * 2) STORED, CHECK (a > 0));\nCREATE INDEX t_a ON t (a);\nALTER TABLE t RENAME COLUMN a TO c;\nSELECT a FROM t;",
        expect![[r#"
            CREATE TABLE t ([a:d] int, b int GENERATED ALWAYS AS ([a] * 2) STORED, CHECK ([a] > 0));
            CREATE INDEX t_a ON t ([a]);
            ALTER TABLE t RENAME COLUMN [a:w] TO c;
            SELECT a FROM t;"#]],
    );
    local(
        Dialect::Postgres,
        "CREATE TABLE t (a int);\nCOMMENT ON TABLE t IS 'x';\nCREATE VIEW v AS SELECT a FROM t;\nALTER TABLE t ADD COLUMN b int;\nDROP TABLE $0t;",
        expect![[r#"
            CREATE TABLE [t:d] (a int);
            COMMENT ON TABLE [t:w] IS 'x';
            CREATE VIEW v AS SELECT a FROM [t];
            ALTER TABLE [t:w] ADD COLUMN b int;
            DROP TABLE [t:w];"#]],
    );
}

#[test]
fn routines_types_and_sequences_by_their_definitions() {
    local(
        Dialect::Postgres,
        "CREATE TYPE mood AS ENUM ('sad');\nCREATE TABLE t (m mood, n public.mood);\nSELECT 'sad'::$0mood;\nDROP TYPE mood;",
        expect![[r#"
            CREATE TYPE [mood:d] AS ENUM ('sad');
            CREATE TABLE t (m [mood], n public.[mood]);
            SELECT 'sad'::[mood];
            DROP TYPE [mood:w];"#]],
    );
    local(
        Dialect::Postgres,
        "CREATE SEQUENCE ids;\nSELECT nextval('ids'), currval('public.ids');\nDROP SEQUENCE $0ids;",
        expect![[r#"
            CREATE SEQUENCE [ids:d];
            SELECT nextval('[ids]'), currval('public.[ids]');
            DROP SEQUENCE [ids:w];"#]],
    );
    local(
        Dialect::Postgres,
        "CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE sql RETURN $0a + b;\nSELECT add(1, 2);",
        expect![[r#"
            CREATE FUNCTION add([a:d] int, b int) RETURNS int LANGUAGE sql RETURN [a] + b;
            SELECT add(1, 2);"#]],
    );
    local(
        Dialect::Postgres,
        "CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE sql RETURN a + b;\nSELECT $0add(1, 2), public.add(3, 4);",
        expect![[r#"
            CREATE FUNCTION [add:d](a int, b int) RETURNS int LANGUAGE sql RETURN a + b;
            SELECT [add](1, 2), public.[add](3, 4);"#]],
    );
}

#[test]
fn routine_variables_are_read_and_written() {
    local(
        Dialect::Mysql,
        "DELIMITER //\nCREATE PROCEDURE p(IN a INT, OUT total INT)\nBEGIN\n  DECLARE x INT DEFAULT 0;\n  SET x = a + 1;\n  SELECT count(*) INTO total FROM t WHERE c = $0x;\nEND//\nDELIMITER ;\n",
        expect![[r#"
            DELIMITER //
            CREATE PROCEDURE p(IN a INT, OUT total INT)
            BEGIN
              DECLARE [x:d] INT DEFAULT 0;
              SET [x:w] = a + 1;
              SELECT count(*) INTO total FROM t WHERE c = [x];
            END//
            DELIMITER ;
        "#]],
    );
}

#[test]
fn user_variables_of_mysql_live_in_the_document() {
    local(
        Dialect::Mysql,
        "SET @total = 1;\nSELECT @total := @total + 1, @other;\nSELECT $0@TOTAL;",
        expect![[r#"
            SET [@total:w] = 1;
            SELECT [@total:w] := [@total] + 1, @other;
            SELECT [@TOTAL];"#]],
    );
}

#[test]
fn rename_statements_declare_the_new_name() {
    local(
        Dialect::Mysql,
        "CREATE TABLE t (a INT);\nRENAME TABLE t TO u;\nSELECT a FROM $0u;\nALTER TABLE u RENAME TO v;",
        expect![[r#"
            CREATE TABLE t (a INT);
            RENAME TABLE t TO [u:d];
            SELECT a FROM [u];
            ALTER TABLE [u:w] RENAME TO v;"#]],
    );
}

#[test]
fn nothing_for_a_keyword_or_a_literal() {
    local(Dialect::Postgres, "SEL$0ECT 1;", expect![[r#"SELECT 1;"#]]);
    local(Dialect::Postgres, "SELECT 1$0;", expect![[r#"SELECT 1;"#]]);
}

fn workspace(files: &[(&str, &str)], dialect: Dialect) -> Layer {
    let extracted: Vec<_> = files
        .iter()
        .map(|(path, text)| (Path::new(*path), extract(text, dialect)))
        .collect();
    build(dialect, extracted.iter().map(|(path, ddl)| (*path, ddl)))
}

#[test]
fn references_of_a_table_cover_the_workspace_files() {
    let files = [
        ("/w/1_users.sql", "CREATE TABLE users (id int, email text);\n"),
        ("/w/2_more.sql", "ALTER TABLE users ADD COLUMN name text;\n"),
        (
            "/w/report.sql",
            "SELECT u.email FROM users u;\nSELECT 1 FROM users_old;\n",
        ),
        ("/w/other.sql", "SELECT 1;\n"),
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
    let (text, root, offset) = split_cursor("SELECT email FROM $0users WHERE id = 1;\n", Dialect::Postgres);
    let current = Current {
        root: &root,
        target: target(Dialect::Postgres),
        schemas,
    };
    let found = references(&current, offset, true, &others).expect("references");
    let mut out = String::new();
    for file in &found.files {
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
        out.push_str(&format!("{name}: {}", marked(&body, &file.hits)));
    }
    expect![[r#"
        current: SELECT email FROM [users] WHERE id = 1;
        /w/1_users.sql: CREATE TABLE [users:d] (id int, email text);
        /w/2_more.sql: ALTER TABLE [users:w] ADD COLUMN name text;
        /w/report.sql: SELECT u.email FROM [users] u;
        SELECT 1 FROM users_old;
    "#]]
    .assert_eq(&out);
    let without = references(&current, offset, false, &others).expect("references");
    assert_eq!(without.files.len(), 3, "the file with only the declaration drops out");

    let (_, root, offset) = split_cursor("SELECT $0email FROM users;\n", Dialect::Postgres);
    let current = Current {
        root: &root,
        target: target(Dialect::Postgres),
        schemas,
    };
    let found = references(&current, offset, true, &others).expect("references");
    let counts: Vec<usize> = found.files.iter().map(|file| file.hits.len()).collect();
    assert_eq!(counts, [1, 1, 1], "the query, the definition and the report");
}

#[test]
fn a_snapshot_column_is_named_in_every_file() {
    let layer = shop(Dialect::Mysql);
    let schemas = with_snapshot(&layer);
    let others = [OtherFile {
        path: Path::new("/w/a.sql"),
        text: "SELECT Email FROM users WHERE status = 'active';\nUPDATE orders SET total = 1;\n",
        target: target(Dialect::Mysql),
        schemas,
    }];
    let (_, root, offset) = split_cursor("SELECT u.$0email FROM users u;", Dialect::Mysql);
    let current = Current {
        root: &root,
        target: target(Dialect::Mysql),
        schemas,
    };
    let found = references(&current, offset, true, &others).expect("references");
    assert_eq!(found.files.len(), 2);
    assert_eq!(found.files[1].hits.len(), 1, "MySQL compares columns without case");
}

/// Every name of the corpus and of the examples of the feature table, in every dialect: asking
/// what it stands for, highlighting it and renaming it never panics.
#[test]
fn every_name_of_the_corpus_can_be_asked_about() {
    let corpus = include_str!("../../syntax/tests/data/dialects.sql");
    let examples: Vec<&str> = sql_syntax::FEATURES.iter().map(|feature| feature.example).collect();
    for dialect in [
        Dialect::Generic,
        Dialect::Sqlite,
        Dialect::Mysql,
        Dialect::Mariadb,
        Dialect::Postgres,
    ] {
        let layer = shop(dialect);
        let schemas = with_snapshot(&layer);
        for text in std::iter::once(corpus).chain(examples.iter().copied()) {
            let root = sql_syntax::parse(text, dialect).syntax();
            let current = Current {
                root: &root,
                target: target(dialect),
                schemas,
            };
            for name in root
                .descendants()
                .filter(|node| node.kind() == sql_syntax::SyntaxKind::NAME)
            {
                let offset = u32::from(name.text_range().start());
                let _ = highlights(&root, offset, target(dialect), schemas);
                if crate::rename::prepare_rename(&current, offset).is_ok() {
                    let _ = crate::rename::rename(&current, offset, "renamed", &[]);
                }
            }
        }
    }
}
