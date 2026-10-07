//! Each inspection against a script that has the problem and one that looks like it and does not,
//! per dialect where they differ, and what its fix makes of the script.

use expect_test::{Expect, expect};
use sql_syntax::{Dialect, Target, Version, parse};

use super::{InspectionSettings, Override, Request, fix_all, inspect};
use crate::context::Schemas;
use crate::diagnostics::DiagnosticSeverity;
use crate::rename::apply;
use crate::testing::{shop, with_snapshot};

/// The findings of a script as `id severity 'text': message`, one per line, without the ids of
/// `skip`.
fn findings(target: Target, schemas: Schemas, settings: &InspectionSettings, text: &str) -> String {
    let root = parse(text, target.dialect).syntax();
    let request = Request::new(target, schemas, settings);
    let mut out = String::new();
    for finding in inspect(&root, &request) {
        let diagnostic = finding.diagnostic;
        let severity = match diagnostic.severity {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
            DiagnosticSeverity::Information => "info",
            DiagnosticSeverity::Hint => "hint",
        };
        out.push_str(&format!(
            "{} {severity} '{}': {}\n",
            diagnostic.code, &text[diagnostic.range], diagnostic.message
        ));
    }
    out
}

fn check(dialect: Dialect, text: &str, expect: Expect) {
    let settings = InspectionSettings::default();
    expect.assert_eq(&findings(Target::new(dialect, None), Schemas::NONE, &settings, text));
}

fn check_shop(dialect: Dialect, text: &str, expect: Expect) {
    let layer = shop(dialect);
    let settings = InspectionSettings::default();
    expect.assert_eq(&findings(
        Target::new(dialect, None),
        with_snapshot(&layer),
        &settings,
        text,
    ));
}

/// Every fix of the findings of one inspection, each applied alone to the script.
fn fixes(dialect: Dialect, schemas: Schemas, id: &'static str, text: &str) -> String {
    let root = parse(text, dialect).syntax();
    let settings = InspectionSettings::default();
    let request = Request {
        fixes: true,
        only: Some(id),
        ..Request::new(Target::new(dialect, None), schemas, &settings)
    };
    let mut out = String::new();
    for finding in inspect(&root, &request) {
        for fix in finding.fixes {
            out.push_str(&format!("{}\n  {}\n", fix.title, apply(text, &fix.edits)));
        }
    }
    out
}

fn check_fix(dialect: Dialect, id: &'static str, text: &str, expect: Expect) {
    expect.assert_eq(&fixes(dialect, Schemas::NONE, id, text));
}

fn check_shop_fix(dialect: Dialect, id: &'static str, text: &str, expect: Expect) {
    let layer = shop(dialect);
    expect.assert_eq(&fixes(dialect, with_snapshot(&layer), id, text));
}

#[test]
fn every_inspection_is_listed_once_with_a_summary() {
    let mut ids: Vec<&str> = super::INSPECTIONS.iter().map(|info| info.id).collect();
    let count = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), count);
    for info in super::INSPECTIONS {
        assert!(!info.summary.is_empty() && !info.summary.ends_with('.'), "{}", info.id);
    }
    let documented = include_str!("../../../../docs/inspections.md");
    for info in super::INSPECTIONS {
        assert!(
            documented.contains(&format!("`{}`", info.id)),
            "docs/inspections.md lists {}",
            info.id
        );
    }
}

#[test]
fn settings_switch_inspections_off_and_change_their_severity() {
    let text = "DELETE FROM t;\nSELECT a FROM t WHERE b = NULL;\nSELECT `a` FROM t;";
    let target = Target::new(Dialect::Postgres, None);
    let mut settings = InspectionSettings::default();
    settings.set(
        "missing-where",
        Override {
            enabled: Some(false),
            severity: None,
        },
    );
    settings.set(
        "null-comparison",
        Override {
            enabled: None,
            severity: Some(DiagnosticSeverity::Error),
        },
    );
    settings.set(
        "backtick-identifiers",
        Override {
            enabled: None,
            severity: Some(DiagnosticSeverity::Hint),
        },
    );
    expect![[r#"
        null-comparison error 'b = NULL': A comparison with NULL through = is never true; use IS NULL
        unsupported-syntax hint '`a`': Backtick-quoted identifiers are not supported by PostgreSQL
    "#]]
    .assert_eq(&findings(target, Schemas::NONE, &settings, text));
    let mut off = InspectionSettings::default();
    off.set(
        "unsupported-syntax",
        Override {
            enabled: Some(false),
            severity: None,
        },
    );
    off.set(
        "backtick-identifiers",
        Override {
            enabled: Some(true),
            severity: None,
        },
    );
    expect![[r#"
        unsupported-syntax error '`a`': Backtick-quoted identifiers are not supported by PostgreSQL
    "#]]
    .assert_eq(&findings(
        target,
        Schemas::NONE,
        &off,
        "SELECT `a` FROM t WHERE x::text LIKE 'a%';",
    ));
}

#[test]
fn a_comment_suppresses_ids_for_a_statement_or_the_file() {
    check(
        Dialect::Postgres,
        "-- sql-suppress missing-where because the table is a queue\nDELETE FROM jobs;\nDELETE FROM jobs; -- sql-suppress all\nDELETE /* sql-suppress null-comparison */ FROM jobs;\nUPDATE jobs SET a = 1;",
        expect![[r#"
            missing-where warning 'DELETE /* sql-suppress null-comparison */ FROM jobs': DELETE without WHERE removes every row of 'jobs'
            missing-where warning 'UPDATE jobs': UPDATE without WHERE changes every row of 'jobs'
        "#]],
    );
    check(
        Dialect::Mysql,
        "SELECT 1;\n-- sql-suppress-file missing-where, double-ampersand\nDELETE FROM jobs;\nSELECT a && b FROM t;",
        expect![[""]],
    );
}

#[test]
fn suppressing_fixes_write_the_comment() {
    let text = "SELECT 1; DELETE FROM jobs;\n  UPDATE jobs SET a = 1;\n";
    let root = parse(text, Dialect::Postgres).syntax();
    let settings = InspectionSettings::default();
    let request = Request::new(Target::new(Dialect::Postgres, None), Schemas::NONE, &settings);
    let mut out = String::new();
    for finding in inspect(&root, &request) {
        for fix in super::suppress_fixes(&root, &finding.diagnostic) {
            out.push_str(&format!("{}\n{}---\n", fix.title, apply(text, &fix.edits)));
        }
    }
    expect![[r#"
        Suppress 'missing-where' for this statement
        SELECT 1; 
        -- sql-suppress missing-where
        DELETE FROM jobs;
          UPDATE jobs SET a = 1;
        ---
        Suppress 'missing-where' for the file
        -- sql-suppress-file missing-where
        SELECT 1; DELETE FROM jobs;
          UPDATE jobs SET a = 1;
        ---
        Suppress 'missing-where' for this statement
        SELECT 1; DELETE FROM jobs;
          -- sql-suppress missing-where
          UPDATE jobs SET a = 1;
        ---
        Suppress 'missing-where' for the file
        -- sql-suppress-file missing-where
        SELECT 1; DELETE FROM jobs;
          UPDATE jobs SET a = 1;
        ---
    "#]]
    .assert_eq(&out);
    let existing = "-- sql-suppress-file null-comparison\n-- sql-suppress null-comparison: reason\nDELETE FROM jobs;\n";
    let root = parse(existing, Dialect::Postgres).syntax();
    let finding = inspect(&root, &request).remove(0);
    let edits: Vec<String> = super::suppress_fixes(&root, &finding.diagnostic)
        .into_iter()
        .map(|fix| apply(existing, &fix.edits))
        .collect();
    assert_eq!(
        edits,
        [
            "-- sql-suppress-file null-comparison\n-- sql-suppress null-comparison missing-where: reason\nDELETE FROM jobs;\n",
            "-- sql-suppress-file null-comparison missing-where\n-- sql-suppress null-comparison: reason\nDELETE FROM jobs;\n",
        ]
    );
}

#[test]
fn fix_all_applies_one_safe_fix_per_finding() {
    let text = "SELECT a FROM t WHERE a = NULL OR b <> NULL OR c != NULL;";
    let root = parse(text, Dialect::Postgres).syntax();
    let settings = InspectionSettings::default();
    let request = Request {
        fixes: true,
        ..Request::new(Target::new(Dialect::Postgres, None), Schemas::NONE, &settings)
    };
    let found = inspect(&root, &request);
    assert_eq!(
        apply(text, &fix_all(&found, Some("null-comparison"))),
        "SELECT a FROM t WHERE a IS NULL OR b IS NOT NULL OR c IS NOT NULL;"
    );
}

#[test]
fn unknown_names_keep_their_near_miss_fixes() {
    check_shop_fix(
        Dialect::Postgres,
        "unresolved-table",
        "SELECT 1 FROM usres;",
        expect![[r#"
            Change to 'users'
              SELECT 1 FROM users;
        "#]],
    );
    check_shop_fix(
        Dialect::Postgres,
        "ambiguous-column",
        "SELECT id FROM users u JOIN orgs o ON o.id = u.org_id;",
        expect![[r#"
            Qualify with 'u'
              SELECT u.id FROM users u JOIN orgs o ON o.id = u.org_id;
            Qualify with 'o'
              SELECT o.id FROM users u JOIN orgs o ON o.id = u.org_id;
        "#]],
    );
}

#[test]
fn syntax_the_target_lacks_or_deprecates() {
    let target = Target::new(Dialect::Mysql, Version::parse("8.0.30"));
    let settings = InspectionSettings::default();
    expect![[r#"
        unsupported-syntax error 'INTERSECT': INTERSECT and EXCEPT are only available since MySQL 8.0.31
        deprecated-syntax warning 'SQL_CALC_FOUND_ROWS': SQL_CALC_FOUND_ROWS is deprecated since MySQL 8.0.17
        reserved-word error 'key': 'key' is a reserved word in MySQL; quote it to use it as a name
    "#]]
    .assert_eq(&findings(
        target,
        Schemas::NONE,
        &settings,
        "SELECT a FROM t INTERSECT SELECT a FROM u;\nSELECT SQL_CALC_FOUND_ROWS a FROM t;\nCREATE TABLE k (key INT);",
    ));
}

#[test]
fn syntax_fixes_rewrite_what_the_server_documents() {
    check_fix(
        Dialect::Mysql,
        "deprecated-syntax",
        "SELECT SQL_CALC_FOUND_ROWS a FROM t WHERE b > 1 ORDER BY a LIMIT 10;\nSELECT FOUND_ROWS();",
        expect![[r#"
            Count the rows with COUNT(*) in place of FOUND_ROWS()
              SELECT a FROM t WHERE b > 1 ORDER BY a LIMIT 10;
            SELECT COUNT(*) FROM t WHERE b > 1;
        "#]],
    );
    check_fix(
        Dialect::Mysql,
        "deprecated-syntax",
        "SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();\nSELECT a FROM t WHERE a && b;\nINSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;\nCREATE TABLE n (a INT(11) ZEROFILL, b TINYINT(1));\nSELECT BINARY a FROM t;",
        expect![[r#"
            Count the rows with COUNT(*) in place of FOUND_ROWS()
              SELECT DISTINCT a FROM t LIMIT 10; SELECT COUNT(*) FROM (SELECT DISTINCT a FROM t) AS counted;
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;
            CREATE TABLE n (a INT(11) ZEROFILL, b TINYINT(1));
            SELECT BINARY a FROM t;
            Replace && with AND
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a AND b;
            INSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;
            CREATE TABLE n (a INT(11) ZEROFILL, b TINYINT(1));
            SELECT BINARY a FROM t;
            Use a row alias in place of VALUES()
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) AS new ON DUPLICATE KEY UPDATE a = new.a, b = new.b + 1;
            CREATE TABLE n (a INT(11) ZEROFILL, b TINYINT(1));
            SELECT BINARY a FROM t;
            Use a row alias in place of VALUES()
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) AS new ON DUPLICATE KEY UPDATE a = new.a, b = new.b + 1;
            CREATE TABLE n (a INT(11) ZEROFILL, b TINYINT(1));
            SELECT BINARY a FROM t;
            Remove the display width
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;
            CREATE TABLE n (a INT ZEROFILL, b TINYINT(1));
            SELECT BINARY a FROM t;
            Remove ZEROFILL
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;
            CREATE TABLE n (a INT(11), b TINYINT(1));
            SELECT BINARY a FROM t;
            Remove the display width
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;
            CREATE TABLE n (a INT(11) ZEROFILL, b TINYINT);
            SELECT BINARY a FROM t;
            Rewrite as CAST(... AS BINARY)
              SELECT SQL_CALC_FOUND_ROWS DISTINCT a FROM t LIMIT 10; SELECT FOUND_ROWS();
            SELECT a FROM t WHERE a && b;
            INSERT INTO t (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE a = VALUES(a), b = VALUES(b) + 1;
            CREATE TABLE n (a INT(11) ZEROFILL, b TINYINT(1));
            SELECT CAST(a AS BINARY) FROM t;
        "#]],
    );
    check_fix(
        Dialect::Postgres,
        "unsupported-syntax",
        "SELECT `a` FROM t LIMIT 5, 10;\nSELECT a FROM t WHERE a <=> b;\nINSERT INTO t (a) VALUE (1);",
        expect![[r#"
            Quote with double quotes
              SELECT "a" FROM t LIMIT 5, 10;
            SELECT a FROM t WHERE a <=> b;
            INSERT INTO t (a) VALUE (1);
            Rewrite as LIMIT ... OFFSET
              SELECT `a` FROM t LIMIT 10 OFFSET 5;
            SELECT a FROM t WHERE a <=> b;
            INSERT INTO t (a) VALUE (1);
            Replace VALUE with VALUES
              SELECT `a` FROM t LIMIT 5, 10;
            SELECT a FROM t WHERE a <=> b;
            INSERT INTO t (a) VALUES (1);
        "#]],
    );
    check_fix(
        Dialect::Mysql,
        "unsupported-syntax",
        "SELECT a::int, CAST(a AS INTEGER), CAST(b AS VARCHAR(5)), CAST(c AS TEXT) FROM t WHERE a IS NOT DISTINCT FROM b AND a IS DISTINCT FROM c AND a == b;",
        expect![[r#"
            Rewrite as CAST
              SELECT CAST(a AS int), CAST(a AS INTEGER), CAST(b AS VARCHAR(5)), CAST(c AS TEXT) FROM t WHERE a IS NOT DISTINCT FROM b AND a IS DISTINCT FROM c AND a == b;
            Cast to SIGNED
              SELECT a::int, CAST(a AS SIGNED), CAST(b AS VARCHAR(5)), CAST(c AS TEXT) FROM t WHERE a IS NOT DISTINCT FROM b AND a IS DISTINCT FROM c AND a == b;
            Cast to CHAR(5)
              SELECT a::int, CAST(a AS INTEGER), CAST(b AS CHAR(5)), CAST(c AS TEXT) FROM t WHERE a IS NOT DISTINCT FROM b AND a IS DISTINCT FROM c AND a == b;
            Cast to CHAR
              SELECT a::int, CAST(a AS INTEGER), CAST(b AS VARCHAR(5)), CAST(c AS CHAR) FROM t WHERE a IS NOT DISTINCT FROM b AND a IS DISTINCT FROM c AND a == b;
            Rewrite with <=>
              SELECT a::int, CAST(a AS INTEGER), CAST(b AS VARCHAR(5)), CAST(c AS TEXT) FROM t WHERE a <=> b AND a IS DISTINCT FROM c AND a == b;
            Rewrite with <=>
              SELECT a::int, CAST(a AS INTEGER), CAST(b AS VARCHAR(5)), CAST(c AS TEXT) FROM t WHERE a IS NOT DISTINCT FROM b AND NOT (a <=> c) AND a == b;
            Replace == with =
              SELECT a::int, CAST(a AS INTEGER), CAST(b AS VARCHAR(5)), CAST(c AS TEXT) FROM t WHERE a IS NOT DISTINCT FROM b AND a IS DISTINCT FROM c AND a = b;
        "#]],
    );
    check_fix(
        Dialect::Postgres,
        "reserved-word",
        "CREATE TABLE x (Order INT);",
        expect![[r#"
            Quote the name
              CREATE TABLE x ("order" INT);
        "#]],
    );
    check_fix(
        Dialect::Mysql,
        "reserved-word",
        "CREATE TABLE x (`key` INT, Rank INT);",
        expect![[r#"
            Quote the name
              CREATE TABLE x (`key` INT, `Rank` INT);
        "#]],
    );
}

#[test]
fn delete_and_update_without_where() {
    check(
        Dialect::Mysql,
        "DELETE FROM jobs;\nUPDATE jobs SET done = 1;\nDELETE FROM jobs WHERE id = 1;\nDELETE FROM jobs LIMIT 100;\nDELETE j FROM jobs j JOIN runs r ON r.job = j.id;\nUPDATE jobs SET done = (SELECT 1 FROM runs WHERE runs.id = 1) WHERE id = 2;\nTRUNCATE jobs;",
        expect![[r#"
            missing-where warning 'DELETE FROM jobs': DELETE without WHERE removes every row of 'jobs'
            missing-where warning 'UPDATE jobs': UPDATE without WHERE changes every row of 'jobs'
        "#]],
    );
    check(
        Dialect::Postgres,
        "UPDATE jobs SET done = true FROM runs;\nDELETE FROM jobs USING runs WHERE runs.job = jobs.id;\nWITH gone AS (DELETE FROM jobs RETURNING id) SELECT count(*) FROM gone;",
        expect![[r#"
            missing-where warning 'UPDATE jobs': UPDATE without WHERE changes every row of 'jobs'
            missing-where warning 'DELETE FROM jobs': DELETE without WHERE removes every row of 'jobs'
        "#]],
    );
}

#[test]
fn comparisons_with_null() {
    check(
        Dialect::Postgres,
        "SELECT a = NULL, NULL <> b, c != (NULL), d IS NULL, e <=> NULL, CASE f WHEN NULL THEN 1 END, CASE WHEN g IS NULL THEN 1 END FROM t;\nUPDATE t SET a = NULL WHERE b = 1;",
        expect![[r#"
            null-comparison warning 'a = NULL': A comparison with NULL through = is never true; use IS NULL
            null-comparison warning 'NULL <> b': A comparison with NULL through <> is never true; use IS NOT NULL
            null-comparison warning 'c != (NULL)': A comparison with NULL through != is never true; use IS NOT NULL
            null-comparison warning 'NULL': CASE f WHEN NULL never matches; use CASE WHEN f IS NULL
        "#]],
    );
    check_fix(
        Dialect::Mysql,
        "null-comparison",
        "SELECT * FROM t WHERE a = NULL OR NULL != b;",
        expect![[r#"
            Replace with IS NULL
              SELECT * FROM t WHERE a IS NULL OR NULL != b;
            Replace with IS NOT NULL
              SELECT * FROM t WHERE a = NULL OR b IS NOT NULL;
        "#]],
    );
}

#[test]
fn like_without_a_wildcard() {
    check(
        Dialect::Postgres,
        "SELECT * FROM t WHERE a LIKE 'abc' AND b NOT LIKE 'x' AND c LIKE 'a%' AND d LIKE 'a_c' AND e ILIKE 'abc' AND f LIKE 'x' ESCAPE '!' AND g LIKE h;",
        expect![[r#"
            like-without-wildcard info 'LIKE 'abc'': The pattern 'abc' has no wildcard, so LIKE compares like =
            like-without-wildcard info 'NOT LIKE 'x'': The pattern 'x' has no wildcard, so NOT LIKE compares like <>
        "#]],
    );
    check(Dialect::Sqlite, "SELECT * FROM t WHERE a LIKE 'abc';", expect![[""]]);
    check_fix(
        Dialect::Mysql,
        "like-without-wildcard",
        "SELECT * FROM t WHERE a LIKE 'abc' AND b NOT LIKE 'x';",
        expect![[r#"
            Replace with =
              SELECT * FROM t WHERE a = 'abc' AND b NOT LIKE 'x';
            Replace with <>
              SELECT * FROM t WHERE a LIKE 'abc' AND b <> 'x';
        "#]],
    );
}

#[test]
fn tables_joined_by_a_comma_without_a_condition() {
    check_shop(
        Dialect::Postgres,
        "SELECT * FROM users, orgs;\nSELECT * FROM users u, orgs o WHERE u.org_id = o.id;\nSELECT * FROM users, orgs WHERE org_id = orgs.id;\nSELECT * FROM users u, orgs o, orders r WHERE u.org_id = o.id;\nSELECT * FROM users u, orgs o WHERE EXISTS (SELECT 1 FROM orders r WHERE r.user_id = u.id AND o.id = 1);\nSELECT * FROM users u, (SELECT 1) x;\nSELECT * FROM users CROSS JOIN orgs;",
        expect![[r#"
            implicit-cross-join warning 'orgs': 'orgs' is joined by a comma without a condition that links it to the tables before it, which pairs every row with every row
            implicit-cross-join warning 'orders r': 'orders' is joined by a comma without a condition that links it to the tables before it, which pairs every row with every row
            unused-alias hint 'r': The alias 'r' is never used
            unused-alias hint 'u': The alias 'u' is never used
        "#]],
    );
    check(Dialect::Postgres, "SELECT * FROM a, b WHERE x = y;", expect![[""]]);
    check_shop_fix(
        Dialect::Postgres,
        "implicit-cross-join",
        "SELECT * FROM users u, orgs o;",
        expect![[r#"
            Write it as CROSS JOIN
              SELECT * FROM users u CROSS JOIN orgs o;
        "#]],
    );
}

#[test]
fn not_in_over_a_column_that_may_be_null() {
    check_shop(
        Dialect::Postgres,
        "SELECT * FROM orgs WHERE id NOT IN (SELECT org_id FROM users);\nSELECT * FROM users WHERE name NOT IN (SELECT name FROM users WHERE id > 1);\nSELECT * FROM users WHERE name NOT IN (SELECT name FROM users WHERE name IS NOT NULL);\nSELECT * FROM users WHERE name IN (SELECT name FROM users);",
        expect![[r#"
            not-in-nullable info '(SELECT name FROM users WHERE id > 1)': 'name' may be NULL, and NOT IN matches no row once the subquery gives a NULL
        "#]],
    );
    check_shop_fix(
        Dialect::Postgres,
        "not-in-nullable",
        "SELECT * FROM users WHERE name NOT IN (SELECT name FROM users);\nSELECT * FROM users WHERE name NOT IN (SELECT name FROM users WHERE id = 1 OR id = 2);",
        expect![[r#"
            Leave out the NULLs of 'name'
              SELECT * FROM users WHERE name NOT IN (SELECT name FROM users WHERE name IS NOT NULL);
            SELECT * FROM users WHERE name NOT IN (SELECT name FROM users WHERE id = 1 OR id = 2);
            Leave out the NULLs of 'name'
              SELECT * FROM users WHERE name NOT IN (SELECT name FROM users);
            SELECT * FROM users WHERE name NOT IN (SELECT name FROM users WHERE (id = 1 OR id = 2) AND name IS NOT NULL);
        "#]],
    );
}

#[test]
fn columns_neither_grouped_nor_aggregated() {
    let schema = "CREATE TABLE people (id INT PRIMARY KEY, name TEXT NOT NULL, org INT, email TEXT UNIQUE NOT NULL);\nCREATE TABLE orgs (id INT PRIMARY KEY, title TEXT);\n";
    let queries = "SELECT name, count(*) FROM people GROUP BY org;\nSELECT p.name, count(*) FROM people p GROUP BY p.id;\nSELECT name, count(*) FROM people GROUP BY email;\nSELECT org, max(name) FROM people GROUP BY 1;\nSELECT org AS o, sum(id) FROM people GROUP BY o;\nSELECT lower(name), count(*) FROM people GROUP BY lower(name);\nSELECT name FROM people GROUP BY org HAVING count(*) > 1 ORDER BY id;\nSELECT name, count(*) FROM people;\nSELECT count(*) FROM people WHERE name = 'x';\nSELECT (SELECT max(title) FROM orgs WHERE orgs.id = 1), count(*) FROM people;\nSELECT name, row_number() OVER (ORDER BY id) FROM people;\nSELECT name, count(*) FROM people WHERE name = 'x' GROUP BY org;\nSELECT o.title, count(*) FROM people p JOIN orgs o ON o.id = p.org GROUP BY o.id;";
    let text = format!("{schema}{queries}");
    check(
        Dialect::Postgres,
        &text,
        expect![[r#"
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'id': 'id' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'name': 'name' is not in an aggregate function, though the query aggregates without GROUP BY
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
        "#]],
    );
    check(
        Dialect::Mysql,
        &text,
        expect![[r#"
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'id': 'id' is neither in GROUP BY nor in an aggregate function
            nonaggregated-column error 'name': 'name' is not in an aggregate function, though the query aggregates without GROUP BY
        "#]],
    );
    check(Dialect::Mariadb, &text, expect![[""]]);
    check(Dialect::Sqlite, &text, expect![[""]]);
    check(
        Dialect::Mariadb,
        &format!(
            "{schema}SET sql_mode = 'ONLY_FULL_GROUP_BY';\nSELECT name, count(*) FROM people GROUP BY org;\nSET SESSION sql_mode = DEFAULT;\nSELECT name, count(*) FROM people GROUP BY org;"
        ),
        expect![[r#"
            nonaggregated-column error 'name': 'name' is neither in GROUP BY nor in an aggregate function
        "#]],
    );
    check_fix(
        Dialect::Postgres,
        "nonaggregated-column",
        "CREATE TABLE t (a INT, b INT);\nSELECT a, b, count(*) FROM t GROUP BY a;",
        expect![[r#"
            Add 'b' to GROUP BY
              CREATE TABLE t (a INT, b INT);
            SELECT a, b, count(*) FROM t GROUP BY a, b;
        "#]],
    );
}

#[test]
fn distinct_that_group_by_makes_unnecessary() {
    check(
        Dialect::Postgres,
        "SELECT DISTINCT a, b FROM t GROUP BY a, b;\nSELECT DISTINCT a, count(*) FROM t GROUP BY a;\nSELECT DISTINCT a FROM t GROUP BY a, b;\nSELECT DISTINCT a AS x FROM t GROUP BY x;\nSELECT DISTINCT a FROM t GROUP BY ROLLUP (a);",
        expect![[r#"
            distinct-with-group-by info 'DISTINCT': DISTINCT changes nothing: GROUP BY already gives one row per group, and the select list holds every grouped expression
            distinct-with-group-by info 'DISTINCT': DISTINCT changes nothing: GROUP BY already gives one row per group, and the select list holds every grouped expression
            distinct-with-group-by info 'DISTINCT': DISTINCT changes nothing: GROUP BY already gives one row per group, and the select list holds every grouped expression
        "#]],
    );
    check_fix(
        Dialect::Postgres,
        "distinct-with-group-by",
        "SELECT DISTINCT a FROM t GROUP BY 1;",
        expect![[r#"
            Remove DISTINCT
              SELECT a FROM t GROUP BY 1;
        "#]],
    );
}

#[test]
fn count_of_a_column_that_cannot_be_null() {
    check_shop(
        Dialect::Postgres,
        "SELECT count(email), count(name), count(DISTINCT email), count(*) FROM users;\nSELECT count(u.email) FROM orgs o LEFT JOIN users u ON u.org_id = o.id;",
        expect![[r#"
            count-not-null-column hint 'email': 'email' is NOT NULL, so COUNT(email) counts every row, as COUNT(*) does
        "#]],
    );
    check_shop_fix(
        Dialect::Postgres,
        "count-not-null-column",
        "SELECT count(email) FROM users;",
        expect![[r#"
            Replace with COUNT(*)
              SELECT count(*) FROM users;
        "#]],
    );
}

#[test]
fn inserts_with_more_or_fewer_values_than_columns() {
    let schema = "CREATE TABLE t (a INT, b INT, c INT);\n";
    let text = format!(
        "{schema}INSERT INTO t (a, b) VALUES (1, 2, 3), (4, 5), (6);\nINSERT INTO t (a) SELECT x, y FROM u;\nINSERT INTO t (a, b) SELECT * FROM u;\nINSERT INTO t VALUES (1, 2);\nINSERT INTO t VALUES (1, 2, 3, 4);\nINSERT INTO t VALUES (1, 2, 3);\nSELECT a FROM t UNION SELECT a, b FROM t;\nSELECT a, b FROM t UNION ALL SELECT a, b FROM t INTERSECT SELECT a FROM t;\nSELECT * FROM t UNION SELECT a FROM t;\nVALUES (1, 2) UNION VALUES (3);"
    );
    check(
        Dialect::Postgres,
        &text,
        expect![[r#"
        insert-column-count error '(1, 2, 3)': 3 values for 2 columns
        insert-column-count error '(6)': 1 value for 2 columns
        insert-column-count error 'x, y': The query gives 2 columns for 1 column
        insert-column-count error '(1, 2, 3, 4)': 4 values for the 3 columns of 't'
        set-operation-column-count error 'a, b': This query gives 2 columns where the first gives 1 column
        set-operation-column-count error 'a': This query gives 1 column where the first gives 2 columns
        set-operation-column-count error '(3)': This query gives 1 column where the first gives 2 columns
    "#]],
    );
    check(
        Dialect::Mysql,
        &text,
        expect![[r#"
        insert-column-count error '(1, 2, 3)': 3 values for 2 columns
        insert-column-count error '(6)': 1 value for 2 columns
        insert-column-count error 'x, y': The query gives 2 columns for 1 column
        insert-column-count error '(1, 2)': 2 values for the 3 columns of 't'
        insert-column-count error '(1, 2, 3, 4)': 4 values for the 3 columns of 't'
        set-operation-column-count error 'a, b': This query gives 2 columns where the first gives 1 column
        set-operation-column-count error 'a': This query gives 1 column where the first gives 2 columns
        unsupported-syntax error 'VALUES': VALUES as a query without ROW is not supported by MySQL
        unsupported-syntax error 'VALUES': VALUES as a query without ROW is not supported by MySQL
        set-operation-column-count error '(3)': This query gives 1 column where the first gives 2 columns
    "#]],
    );
}

#[test]
fn values_a_column_cannot_hold() {
    let schema = "CREATE TYPE mood AS ENUM ('happy', 'sad');\nCREATE TABLE t (n INT, d NUMERIC(5,2), day DATE, at TIMESTAMP, m mood, s TEXT, f mood NOT NULL);\n";
    check(
        Dialect::Postgres,
        &format!(
            "{schema}INSERT INTO t (n, d, day, at, m, s) VALUES ('12', 'x', '2024-02-30', 'now', 'angry', 'abc'), (' 7 ', '1.5e3', 'today', '2024-01-01 10:00', 'happy', '');\nSELECT * FROM t WHERE n = 'abc' OR 'nope' < d OR m IN ('sad', 'glad') OR day = '2024-13-01' OR s = 'x';\nUPDATE t SET n = '1.5', m = 'Happy' WHERE n = 1;"
        ),
        expect![[r#"
            missing-required-column error '(n, d, day, at, m, s)': 'f' is NOT NULL and has no default, so the INSERT must give it a value
            invalid-literal error ''x'': 'x' is not a number, which the column 'd' (NUMERIC(5,2)) holds
            invalid-literal error ''2024-02-30'': '2024-02-30' is not a date, which the column 'day' (DATE) holds
            unknown-enum-value error ''angry'': 'angry' is not a value of 'm', which takes 'happy', 'sad'
            invalid-literal error ''abc'': 'abc' is not a number, which the column 'n' (INT) holds
            invalid-literal error ''nope'': 'nope' is not a number, which the column 'd' (NUMERIC(5,2)) holds
            unknown-enum-value error ''glad'': 'glad' is not a value of 'm', which takes 'happy', 'sad'
            invalid-literal error ''2024-13-01'': '2024-13-01' is not a date, which the column 'day' (DATE) holds
            invalid-literal error ''1.5'': '1.5' is not a number, which the column 'n' (INT) holds
            unknown-enum-value error ''Happy'': 'Happy' is not a value of 'm', which takes 'happy', 'sad'
        "#]],
    );
    let mysql = "CREATE TABLE t (n INT, day DATE, e ENUM('a','b') NOT NULL, s VARCHAR(10));\n";
    check(
        Dialect::Mysql,
        &format!(
            "{mysql}INSERT INTO t (n, day, e) VALUES ('1.5', 'soon', 'A'), ('abc', '2024-02-29', 'c');\nSELECT * FROM t WHERE n = 'abc' AND e = 'z';\nSET sql_mode = '';\nINSERT INTO t (n, e) VALUES ('abc', 'a');"
        ),
        expect![[r#"
            invalid-literal error ''soon'': 'soon' is not a date, which the column 'day' (DATE) holds
            invalid-literal error ''abc'': 'abc' is not a number, which the column 'n' (INT) holds
            unknown-enum-value error ''c'': 'c' is not a value of 'e', which takes 'a', 'b'
            invalid-literal warning ''abc'': 'abc' is not a number, which the column 'n' (INT) holds
            unknown-enum-value warning ''z'': 'z' is not a value of 'e', which takes 'a', 'b'
            invalid-literal warning ''abc'': 'abc' is not a number, which the column 'n' (INT) holds
        "#]],
    );
    check(
        Dialect::Sqlite,
        "CREATE TABLE t (n INTEGER, s TEXT);\nINSERT INTO t (n, s) VALUES ('abc', 1);",
        expect![[r#"
            invalid-literal warning ''abc'': 'abc' is not a number, which the column 'n' (INTEGER) holds
        "#]],
    );
    check_shop_fix(
        Dialect::Postgres,
        "unknown-enum-value",
        "SELECT * FROM orders WHERE mood = 'hapy';",
        expect![[r#"
            Change to 'happy'
              SELECT * FROM orders WHERE mood = 'happy';
        "#]],
    );
}

#[test]
fn null_written_to_a_not_null_column() {
    let schema = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT, c INT NOT NULL DEFAULT 0, n SERIAL);\n";
    check(
        Dialect::Postgres,
        &format!(
            "{schema}INSERT INTO t (id, a, b, c, n) VALUES (1, NULL, NULL, NULL, NULL);\nUPDATE t SET a = NULL, b = NULL WHERE id = 1;"
        ),
        expect![[r#"
            not-null-violation error 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
            not-null-violation error 'NULL': 'c' is NOT NULL, so it cannot be set to NULL
            not-null-violation error 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
        "#]],
    );
    check(
        Dialect::Mysql,
        "CREATE TABLE t (id INT AUTO_INCREMENT PRIMARY KEY, a INT NOT NULL);\nINSERT INTO t (id, a) VALUES (NULL, NULL);\nINSERT INTO t (id, a) VALUES (1, 1), (2, NULL);\nINSERT IGNORE INTO t (id, a) VALUES (3, NULL);\nSET sql_mode = '';\nINSERT INTO t (id, a) VALUES (1, 1), (2, NULL);\nUPDATE t SET a = NULL WHERE id = 1;",
        expect![[r#"
            not-null-violation error 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
            not-null-violation error 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
            not-null-violation warning 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
            not-null-violation warning 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
        "#]],
    );
    check(
        Dialect::Sqlite,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INT NOT NULL);\nINSERT INTO t (id, a) VALUES (NULL, NULL);\nINSERT OR IGNORE INTO t (id, a) VALUES (NULL, NULL);",
        expect![[r#"
            not-null-violation error 'NULL': 'a' is NOT NULL, so it cannot be set to NULL
        "#]],
    );
    check(
        Dialect::Postgres,
        "CREATE TABLE t (a INT NOT NULL);\nCREATE TRIGGER fill BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION fill();\nINSERT INTO t (a) VALUES (NULL);",
        expect![[""]],
    );
}

#[test]
fn values_written_to_generated_columns() {
    let schema = "CREATE TABLE t (a INT, b INT GENERATED ALWAYS AS (a * 2) STORED, c INT GENERATED ALWAYS AS IDENTITY, d INT GENERATED BY DEFAULT AS IDENTITY);\n";
    check(
        Dialect::Postgres,
        &format!(
            "{schema}INSERT INTO t (a, b) VALUES (1, 2);\nINSERT INTO t (a, b) VALUES (1, DEFAULT);\nINSERT INTO t (a, c, d) VALUES (1, 2, 3);\nINSERT INTO t (a, c) OVERRIDING SYSTEM VALUE VALUES (1, 2);\nUPDATE t SET b = 3, c = DEFAULT WHERE a = 1;"
        ),
        expect![[r#"
            generated-column-write error 'b': 'b' is generated, stored, so no value can be written to it
            generated-column-write error 'c': 'c' is identity, always, so no value can be written to it
            generated-column-write error 'b': 'b' is generated, stored, so no value can be written to it
        "#]],
    );
    check_fix(
        Dialect::Postgres,
        "generated-column-write",
        &format!("{schema}INSERT INTO t (a, b, d) VALUES (1, 2, 3), (4, 5, 6);"),
        expect![[r#"
            Leave out 'b'
              CREATE TABLE t (a INT, b INT GENERATED ALWAYS AS (a * 2) STORED, c INT GENERATED ALWAYS AS IDENTITY, d INT GENERATED BY DEFAULT AS IDENTITY);
            INSERT INTO t (a, d) VALUES (1, 3), (4, 6);
        "#]],
    );
}

#[test]
fn inserts_that_leave_out_a_column_that_needs_a_value() {
    let schema = "CREATE TABLE t (id INT PRIMARY KEY, a INT NOT NULL, b INT, c INT NOT NULL DEFAULT 0, g INT GENERATED ALWAYS AS (a + 1) STORED);\n";
    let text = format!(
        "{schema}INSERT INTO t (a) VALUES (1);\nINSERT INTO t (id, a) VALUES (1, 2);\nINSERT INTO t (b) VALUES (1);\nINSERT INTO t DEFAULT VALUES;\nINSERT INTO t (id, a) SELECT 1, 2;"
    );
    check(
        Dialect::Postgres,
        &text,
        expect![[r#"
        missing-required-column error '(a)': 'id' is NOT NULL and has no default, so the INSERT must give it a value
        missing-required-column error '(b)': 'id', 'a' are NOT NULL and have no default, so the INSERT must give them values
        missing-required-column error 'DEFAULT VALUES': 'id', 'a' are NOT NULL and have no default, so the INSERT must give them values
    "#]],
    );
    check(
        Dialect::Sqlite,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INT NOT NULL);\nINSERT INTO t (a) VALUES (1);\nINSERT INTO t (id) VALUES (1);",
        expect![[r#"
            missing-required-column error '(id)': 'a' is NOT NULL and has no default, so the INSERT must give it a value
        "#]],
    );
    check(
        Dialect::Mysql,
        "CREATE TABLE t (id INT AUTO_INCREMENT PRIMARY KEY, a INT NOT NULL);\nINSERT INTO t SET id = 1;\nINSERT IGNORE INTO t (id) VALUES (1);",
        expect![[r#"
            missing-required-column error 'SET id = 1': 'a' is NOT NULL and has no default, so the INSERT must give it a value
        "#]],
    );
}

#[test]
fn common_table_expressions_and_aliases_nothing_reads() {
    check(
        Dialect::Postgres,
        "WITH a AS (SELECT 1 AS x), b AS (SELECT x FROM a), c AS (SELECT 2) SELECT * FROM b;\nWITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT 1;\nWITH d AS (DELETE FROM t RETURNING id) SELECT 1;\nSELECT x.a FROM t x JOIN u y ON true;\nSELECT a FROM t AS x;\nSELECT * FROM (SELECT 1) AS s;\nSELECT 1 FROM t a JOIN t b ON a.id = b.id;\nSELECT x.* FROM t x;\nSELECT 1 FROM t x (p, q);",
        expect![[r#"
            unused-cte warning 'c': The common table expression 'c' is never used
            unused-cte warning 'r': The common table expression 'r' is never used
            missing-where warning 'DELETE FROM t': DELETE without WHERE removes every row of 't'
            unused-alias hint 'y': The alias 'y' is never used
            unused-alias hint 'x': The alias 'x' is never used
        "#]],
    );
    check(Dialect::Mysql, "SELECT 1 FROM (SELECT 1) AS s;", expect![[""]]);
    check_fix(
        Dialect::Postgres,
        "unused-cte",
        "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM b;\nWITH c AS (SELECT 1) SELECT 2;",
        expect![[r#"
            Remove 'a'
              WITH b AS (SELECT 2) SELECT * FROM b;
            WITH c AS (SELECT 1) SELECT 2;
            Remove 'c'
              WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM b;
            SELECT 2;
        "#]],
    );
    check_fix(
        Dialect::Postgres,
        "unused-alias",
        "SELECT a FROM t AS x;",
        expect![[r#"
        Remove the alias 'x'
          SELECT a FROM t;
    "#]],
    );
}

#[test]
fn names_declared_twice() {
    check(
        Dialect::Postgres,
        "WITH a AS (SELECT 1), A AS (SELECT 2) SELECT 1;\nWITH a AS (SELECT 1), \"A\" AS (SELECT 2) SELECT 1;\nSELECT 1 FROM t x JOIN u x ON true;\nSELECT 1 FROM t, t;\nSELECT 1 FROM s1.t, s2.t;\nSELECT 1 FROM t, (SELECT 1 FROM t) AS u;\nCREATE TABLE k (a INT, b INT, A INT);\nCREATE VIEW v AS SELECT a, b AS a FROM t;\nCREATE VIEW w (x, y) AS SELECT a, a FROM t;\nCREATE TABLE c AS SELECT a, a FROM t;\nINSERT INTO t (a, b, a) VALUES (1, 2, 3);",
        expect![[r#"
            unused-cte warning 'a': The common table expression 'a' is never used
            unused-cte warning 'A': The common table expression 'A' is never used
            duplicate-cte error 'A': The common table expression 'a' is already declared
            unused-cte warning 'a': The common table expression 'a' is never used
            unused-cte warning '"A"': The common table expression 'A' is never used
            unused-alias hint 'x': The alias 'x' is never used
            unused-alias hint 'x': The alias 'x' is never used
            duplicate-alias error 'x': The table or alias 'x' is already declared
            implicit-cross-join warning 't': 't' is joined by a comma without a condition that links it to the tables before it, which pairs every row with every row
            duplicate-alias error 't': The table or alias 't' is already declared
            implicit-cross-join warning 's2.t': 's2.t' is joined by a comma without a condition that links it to the tables before it, which pairs every row with every row
            duplicate-column error 'A': The column 'a' is already declared
            duplicate-column error 'a': The column 'a' is already declared
            duplicate-column error 'a': The column 'a' is already declared
            duplicate-column error 'a': The column 'a' is already declared
        "#]],
    );
    check(
        Dialect::Sqlite,
        "CREATE VIEW v AS SELECT a, b AS A FROM t;\nCREATE TABLE k (a INT, A INT);",
        expect![[r#"
            duplicate-column warning 'A': The column 'A' is already declared
            duplicate-column error 'A': The column 'A' is already declared
        "#]],
    );
}

#[test]
fn pipes_as_a_logical_or() {
    let text = "CREATE TABLE t (n INT, s VARCHAR(10));\nSELECT s || 'x' || n, n || n, 'a' || 'b' FROM t;\nSELECT CONCAT(s, 'x') || n FROM t;";
    check(
        Dialect::Mysql,
        text,
        expect![[r#"
        pipes-as-or warning '||': MySQL reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
        pipes-as-or warning '||': MySQL reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
        deprecated-syntax warning '||': The || operator is deprecated since MySQL 8.0.17
        pipes-as-or warning '||': MySQL reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
        pipes-as-or warning '||': MySQL reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
    "#]],
    );
    check(
        Dialect::Mariadb,
        text,
        expect![[r#"
        pipes-as-or warning '||': MariaDB reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
        pipes-as-or warning '||': MariaDB reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
        pipes-as-or warning '||': MariaDB reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
        pipes-as-or warning '||': MariaDB reads || as a logical OR, not as concatenation, unless PIPES_AS_CONCAT is on
    "#]],
    );
    check(
        Dialect::Mysql,
        "SET sql_mode = 'PIPES_AS_CONCAT';\nSELECT 'a' || 'b';",
        expect![[r#"
            deprecated-syntax warning '||': The || operator is deprecated since MySQL 8.0.17
        "#]],
    );
    check(Dialect::Postgres, "SELECT 'a' || 'b';", expect![[""]]);
    check_fix(
        Dialect::Mysql,
        "pipes-as-or",
        "SELECT s || '-' || n FROM t;",
        expect![[r#"
        Join the strings with CONCAT()
          SELECT CONCAT(s, '-', n) FROM t;
        Join the strings with CONCAT()
          SELECT CONCAT(s, '-', n) FROM t;
    "#]],
    );
}

#[test]
fn double_quoted_strings() {
    check(
        Dialect::Mysql,
        "CREATE TABLE t (name TEXT, a INT);\nSELECT \"name\", \"it's\", 'x', a AS \"alias\" FROM t WHERE a = \"1\";",
        expect![[r#"
            double-quoted-string warning '"name"': "name" is a string in MySQL, not the column 'name'; write a column name bare or in backticks
            double-quoted-string hint '"it's"': A double-quoted string is a name where ANSI_QUOTES is on, as in standard SQL; single quotes are a string everywhere
            double-quoted-string hint '"1"': A double-quoted string is a name where ANSI_QUOTES is on, as in standard SQL; single quotes are a string everywhere
        "#]],
    );
    check(
        Dialect::Mysql,
        "SET sql_mode = 'ANSI_QUOTES';\nSELECT 1;",
        expect![[""]],
    );
    check_fix(
        Dialect::Mysql,
        "double-quoted-string",
        "CREATE TABLE t (name TEXT);\nSELECT \"name\", \"it's \"\"x\"\"\" FROM t;",
        expect![[r#"
            Read the column 'name'
              CREATE TABLE t (name TEXT);
            SELECT `name`, "it's ""x""" FROM t;
            Use single quotes
              CREATE TABLE t (name TEXT);
            SELECT 'name', "it's ""x""" FROM t;
            Use single quotes
              CREATE TABLE t (name TEXT);
            SELECT "name", 'it''s "x"' FROM t;
        "#]],
    );
}

#[test]
fn limit_in_a_subquery_of_in() {
    let text = "SELECT * FROM t WHERE a IN (SELECT a FROM u LIMIT 3) OR b = ANY (SELECT b FROM u LIMIT 1) OR EXISTS (SELECT 1 FROM u LIMIT 1) OR c = (SELECT c FROM u LIMIT 1);";
    check(
        Dialect::Mysql,
        text,
        expect![[r#"
        limit-in-subquery error 'LIMIT 3': MySQL does not take LIMIT in a subquery of IN
        limit-in-subquery error 'LIMIT 1': MySQL does not take LIMIT in a subquery of ANY, SOME and ALL
    "#]],
    );
    check(Dialect::Postgres, text, expect![[""]]);
    check_fix(
        Dialect::Mariadb,
        "limit-in-subquery",
        "SELECT * FROM t WHERE a IN (SELECT a FROM u ORDER BY a LIMIT 3);",
        expect![[r#"
            Wrap the subquery in a derived table
              SELECT * FROM t WHERE a IN (SELECT * FROM (SELECT a FROM u ORDER BY a LIMIT 3) AS limited);
        "#]],
    );
}

#[test]
fn order_by_in_a_subquery_without_limit() {
    let text = "SELECT * FROM (SELECT a FROM t ORDER BY a) AS x;\nSELECT count(*) FROM (SELECT a FROM t ORDER BY a) AS x;\nSELECT * FROM t WHERE a IN (SELECT a FROM u ORDER BY a);\nSELECT * FROM (SELECT a FROM t ORDER BY a LIMIT 5) AS x;\n(SELECT a FROM t ORDER BY a);\n(SELECT a FROM t ORDER BY a) UNION (SELECT a FROM u);\nWITH c AS (SELECT a FROM t ORDER BY a) SELECT * FROM c;";
    check(
        Dialect::Mysql,
        text,
        expect![[r#"
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
    "#]],
    );
    check(
        Dialect::Mariadb,
        text,
        expect![[r#"
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
        order-by-in-subquery info 'ORDER BY a': ORDER BY in a subquery without LIMIT does not order the result, and the server may drop it
    "#]],
    );
    check(Dialect::Postgres, text, expect![""]);
    check_fix(
        Dialect::Mariadb,
        "order-by-in-subquery",
        "SELECT * FROM (SELECT a FROM t ORDER BY a DESC) AS x;",
        expect![[r#"
            Remove ORDER BY
              SELECT * FROM (SELECT a FROM t) AS x;
        "#]],
    );
}
