use crate::{Dialect, parse};

const SAMPLES: &[&str] = &[
    "",
    ";;;",
    "SELECT",
    "SELECT * FROM",
    "WITH",
    "WITH x AS (",
    "INSERT INTO t (a, b) VALUES (1, 'x'), (2,",
    "CREATE TABLE t (id int PRIMARY KEY, name text NOT NULL DEFAULT 'x',",
    "UPDATE t SET a = CASE WHEN b THEN 1",
    "SELECT a FROM t WHERE a IN (SELECT b FROM u WHERE",
    "DELIMITER //\nCREATE PROCEDURE p() BEGIN IF a THEN",
    "MERGE INTO t USING s ON",
    "ALTER TABLE t ADD",
    ") ) ) SELECT 1;",
    "END; END IF; ELSE",
    "SELECT $$ unterminated",
    "SELECT 'it''s', \"q\", `b`, [c], 'x\\'y' FROM t /* open",
    "CREATE FUNCTION f() RETURNS int AS $f$ SELECT 1 $f$ LANGUAGE sql;",
    "COPY t FROM stdin;\n1\n",
    "GRANT ON TO;",
    "SELECT f(a => , b := ) OVER (PARTITION BY ORDER BY ROWS BETWEEN AND)",
    "SELECT 1 \u{1f600} FROM t",
];

#[test]
fn every_prefix_of_broken_input_parses_into_a_tree_with_every_byte() {
    for sample in SAMPLES {
        for dialect in [Dialect::Generic, Dialect::Sqlite, Dialect::Mysql, Dialect::Postgres] {
            for end in (0..=sample.len()).filter(|end| sample.is_char_boundary(*end)) {
                let prefix = &sample[..end];
                assert_eq!(
                    parse(prefix, dialect).syntax().to_string(),
                    prefix,
                    "{dialect}: {prefix:?}"
                );
            }
        }
    }
}

#[test]
fn deep_nesting_ends_in_an_error_and_not_in_a_stack_overflow() {
    let depth = 5000;
    let text = format!("SELECT {}1{};", "(".repeat(depth), ")".repeat(depth));
    let parsed = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || {
            let parsed = parse(&text, Dialect::Generic);
            assert_eq!(parsed.syntax().to_string(), text);
            parsed
        })
        .expect("spawned")
        .join()
        .expect("no panic");
    assert!(
        parsed
            .errors()
            .iter()
            .any(|error| error.message == "Nesting is too deep")
    );
}

#[test]
fn deeply_nested_queries_and_cases_end_in_an_error_too() {
    let depth = 3000;
    let queries = format!("{}SELECT 1{}", "SELECT * FROM (".repeat(depth), ") x".repeat(depth));
    let cases = format!("SELECT {}1{}", "CASE WHEN a THEN ".repeat(depth), " END".repeat(depth));
    for text in [queries, cases] {
        let parsed = std::thread::Builder::new()
            .stack_size(16 << 20)
            .spawn(move || {
                let parsed = parse(&text, Dialect::Generic);
                assert_eq!(parsed.syntax().to_string(), text);
                parsed
            })
            .expect("spawned")
            .join()
            .expect("no panic");
        assert!(
            parsed
                .errors()
                .iter()
                .any(|error| error.message == "Nesting is too deep")
        );
    }
}
