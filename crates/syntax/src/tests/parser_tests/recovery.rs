use super::{check, check_in};
use crate::Dialect;
use expect_test::expect;

#[test]
fn a_missing_semicolon_is_reported_where_the_next_statement_starts() {
    check(
        "SELECT 1\nSELECT 2;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))))
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "2")))) ";"))
        error 8..8: ';' expected
    "#]],
    );
}

#[test]
fn a_broken_statement_stops_at_the_next_line_that_starts_a_statement() {
    check(
        "SELECT * FROM t WHERE\nINSERT INTO t VALUES (1);",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (WILDCARD "*"))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (WHERE_CLAUSE "WHERE")))
          (INSERT_STMT "INSERT" "INTO" (QUALIFIED_NAME (NAME "t")) (VALUES "VALUES" (ROW_EXPR "(" (LITERAL "1") ")")) ";"))
        error 21..21: Expression expected
    "#]],
    );
    check(
        "SELECT a b c d FROM t;\nSELECT 2;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a")) (ALIAS (NAME "b"))))) (ERROR "c" "d" "FROM" "t") ";")
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "2")))) ";"))
        error 11..12: Unexpected 'c'
    "#]],
    );
}

#[test]
fn an_unknown_statement_becomes_an_error_and_the_next_one_reads() {
    check(
        "FROBNICATE the table;\nSELECT 1;",
        expect![[r#"
        (SOURCE_FILE
          (UNKNOWN_STMT (ERROR "FROBNICATE" "the" "table") ";")
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ";"))
        error 0..10: Unknown statement 'FROBNICATE'
    "#]],
    );
}

#[test]
fn missing_pieces_are_reported_without_swallowing_what_follows() {
    check(
        "SELECT a, FROM t;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a"))) "," (SELECT_ITEM)) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t"))))) ";"))
        error 9..9: Expression expected
    "#]],
    );
    check(
        "INSERT INTO t (a, b VALUES (1, 2);",
        expect![[r#"
        (SOURCE_FILE
          (INSERT_STMT "INSERT" "INTO" (QUALIFIED_NAME (NAME "t")) (NAME_LIST "(" (NAME "a") "," (NAME "b")) (VALUES "VALUES" (ROW_EXPR "(" (LITERAL "1") "," (LITERAL "2") ")")) ";"))
        error 19..19: ')' expected
    "#]],
    );
    check(
        "UPDATE t SET WHERE id = 1;",
        expect![[r#"
        (SOURCE_FILE
          (UPDATE_STMT "UPDATE" (TABLE_REF (QUALIFIED_NAME (NAME "t"))) (SET_CLAUSE "SET" (ASSIGNMENT)) (WHERE_CLAUSE "WHERE" (BINARY_EXPR (COLUMN_REF (NAME "id")) "=" (LITERAL "1"))) ";"))
        error 12..12: Column name expected
    "#]],
    );
    check(
        "SELECT (1 + 2;\nSELECT 3;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (PAREN_EXPR "(" (BINARY_EXPR (LITERAL "1") "+" (LITERAL "2")))))) ";")
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "3")))) ";"))
        error 13..13: ')' expected
    "#]],
    );
    check(
        "CREATE TABLE t (a int, b varchar(10) NOT NUL, c int);",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_TABLE_STMT "CREATE" "TABLE" (QUALIFIED_NAME (NAME "t")) (TABLE_ELEMENT_LIST "(" (COLUMN_DEF (NAME "a") (TYPE (QUALIFIED_NAME (NAME "int")))) "," (COLUMN_DEF (NAME "b") (TYPE (QUALIFIED_NAME (NAME "varchar")) (TYPE_ARGS "(" (LITERAL "10") ")")) (COLUMN_CONSTRAINT "NOT") (COLUMN_CONSTRAINT (ERROR "NUL"))) "," (COLUMN_DEF (NAME "c") (TYPE (QUALIFIED_NAME (NAME "int")))) ")") ";"))
        error 40..40: NULL expected
        error 41..44: Unexpected 'NUL'
    "#]],
    );
    check(
        "SELECT CASE WHEN a THEN 1;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (CASE_EXPR "CASE" (WHEN_CLAUSE "WHEN" (COLUMN_REF (NAME "a")) "THEN" (LITERAL "1")))))) ";"))
        error 25..25: END expected
    "#]],
    );
    check(
        "CREATE INDEX ON;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_INDEX_STMT "CREATE" "INDEX" "ON" (INDEX_COLUMN_LIST (INDEX_COLUMN)) ";"))
        error 15..15: Table name expected
    "#]],
    );
}

#[test]
fn an_unclosed_block_ends_at_the_end_of_the_text() {
    check_in(
        Dialect::Mysql,
        "CREATE PROCEDURE p() BEGIN SELECT 1;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_FUNCTION_STMT "CREATE" "PROCEDURE" (QUALIFIED_NAME (NAME "p")) (PARAM_LIST "(" ")") (ROUTINE_BODY (BLOCK "BEGIN" (STATEMENT_LIST
                  (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ";"))))))
        error 36..36: END expected
    "#]],
    );
}

#[test]
fn lexer_errors_become_syntax_errors() {
    check(
        "SELECT 'open;\nSELECT 2;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "'open;\nSELECT 2;"))))))
        error 7..23: Unterminated string
    "#]],
    );
    check_in(
        Dialect::Sqlite,
        "SELECT 1 # 2;",
        expect![[r##"
            (SOURCE_FILE
              (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) (ERROR "#" "2") ";"))
            error 9..10: Unexpected character
        "##]],
    );
}
