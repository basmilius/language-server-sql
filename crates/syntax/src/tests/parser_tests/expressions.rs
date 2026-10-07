use super::expr;
use expect_test::expect;

#[test]
fn precedence_of_logic_comparison_and_arithmetic() {
    expr(
        "a OR b AND NOT c = d + e * f",
        expect![[r#"
        (BINARY_EXPR (COLUMN_REF (NAME "a")) "OR" (BINARY_EXPR (COLUMN_REF (NAME "b")) "AND" (PREFIX_EXPR "NOT" (BINARY_EXPR (COLUMN_REF (NAME "c")) "=" (BINARY_EXPR (COLUMN_REF (NAME "d")) "+" (BINARY_EXPR (COLUMN_REF (NAME "e")) "*" (COLUMN_REF (NAME "f"))))))))
    "#]],
    );
    expr(
        "a = b IS NULL",
        expect![[r#"
        (IS_EXPR (BINARY_EXPR (COLUMN_REF (NAME "a")) "=" (COLUMN_REF (NAME "b"))) "IS" "NULL")
    "#]],
    );
    expr(
        "-a ^ 2 * 3 || 'x'",
        expect![[r#"
        (BINARY_EXPR (BINARY_EXPR (BINARY_EXPR (PREFIX_EXPR "-" (COLUMN_REF (NAME "a"))) "^" (LITERAL "2")) "*" (LITERAL "3")) "||" (LITERAL "'x'"))
    "#]],
    );
    expr(
        "@a := b XOR c",
        expect![[r#"
        (BINARY_EXPR (VARIABLE_REF "@a") ":=" (BINARY_EXPR (COLUMN_REF (NAME "b")) "XOR" (COLUMN_REF (NAME "c"))))
    "#]],
    );
}

#[test]
fn predicates() {
    expr(
        "a NOT BETWEEN 1 AND 2 AND b",
        expect![[r#"
        (BINARY_EXPR (BETWEEN_EXPR (COLUMN_REF (NAME "a")) "NOT" "BETWEEN" (LITERAL "1") "AND" (LITERAL "2")) "AND" (COLUMN_REF (NAME "b")))
    "#]],
    );
    expr(
        "a NOT IN (1, 2)",
        expect![[r#"
        (IN_EXPR (COLUMN_REF (NAME "a")) "NOT" "IN" (IN_LIST "(" (LITERAL "1") "," (LITERAL "2") ")"))
    "#]],
    );
    expr(
        "a IN (SELECT b FROM t)",
        expect![[r#"
        (IN_EXPR (COLUMN_REF (NAME "a")) "IN" (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "b")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t"))))) ")"))
    "#]],
    );
    expr(
        "a NOT LIKE 'x%' ESCAPE '!'",
        expect![[r#"
        (LIKE_EXPR (COLUMN_REF (NAME "a")) "NOT" "LIKE" (LITERAL "'x%'") "ESCAPE" (LITERAL "'!'"))
    "#]],
    );
    expr(
        "a IS NOT DISTINCT FROM b",
        expect![[r#"
        (IS_EXPR (COLUMN_REF (NAME "a")) "IS" "NOT" "DISTINCT" "FROM" (COLUMN_REF (NAME "b")))
    "#]],
    );
    expr(
        "a SIMILAR TO 'b'",
        expect![[r#"
        (LIKE_EXPR (COLUMN_REF (NAME "a")) "SIMILAR" "TO" (LITERAL "'b'"))
    "#]],
    );
    expr(
        "a ISNULL",
        expect![[r#"
        (IS_EXPR (COLUMN_REF (NAME "a")) "ISNULL")
    "#]],
    );
    expr(
        "j IS JSON OBJECT WITH UNIQUE KEYS",
        expect![[r#"
        (IS_EXPR (COLUMN_REF (NAME "j")) "IS" "JSON" "OBJECT" "WITH" "UNIQUE" "KEYS")
    "#]],
    );
    expr(
        "1 MEMBER OF (j)",
        expect![[r#"
        (MEMBER_OF_EXPR (LITERAL "1") "MEMBER" "OF" "(" (COLUMN_REF (NAME "j")) ")")
    "#]],
    );
    expr(
        "x = ANY (SELECT y FROM t)",
        expect![[r#"
        (BINARY_EXPR (COLUMN_REF (NAME "x")) "=" (QUANTIFIED_EXPR "ANY" (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "y")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t"))))) ")")))
    "#]],
    );
}

#[test]
fn literals_parameters_and_variables() {
    expr(
        "'a' 'b'",
        expect![[r#"
        (LITERAL "'a'" "'b'")
    "#]],
    );
    expr(
        "DATE '2024-01-01'",
        expect![[r#"
        (TYPED_LITERAL "DATE" "'2024-01-01'")
    "#]],
    );
    expr(
        "TIMESTAMP WITH TIME ZONE '2024-01-01 00:00'",
        expect![[r#"
        (TYPED_LITERAL "TIMESTAMP" "WITH" "TIME" "ZONE" "'2024-01-01 00:00'")
    "#]],
    );
    expr(
        "INTERVAL '1' DAY TO SECOND",
        expect![[r#"
        (INTERVAL_EXPR "INTERVAL" (LITERAL "'1'") "DAY" "TO" "SECOND")
    "#]],
    );
    expr(
        "INTERVAL 1 + 1 DAY",
        expect![[r#"
        (INTERVAL_EXPR "INTERVAL" (BINARY_EXPR (LITERAL "1") "+" (LITERAL "1")) "DAY")
    "#]],
    );
    expr(
        "_utf8mb4'x' COLLATE utf8mb4_bin",
        expect![[r#"
        (COLLATE_EXPR (LITERAL "_utf8mb4" "'x'") "COLLATE" (QUALIFIED_NAME (NAME "utf8mb4_bin")))
    "#]],
    );
    expr(
        "? + $1 + :name + @v + @@session.x",
        expect![[r#"
        (BINARY_EXPR (BINARY_EXPR (BINARY_EXPR (BINARY_EXPR (PARAMETER "?") "+" (PARAMETER "$1")) "+" (PARAMETER ":" "name")) "+" (VARIABLE_REF "@v")) "+" (VARIABLE_REF "@@session.x"))
    "#]],
    );
    expr(
        "CURRENT_TIMESTAMP",
        expect![[r#"
        (VALUE_FUNCTION "CURRENT_TIMESTAMP")
    "#]],
    );
}

#[test]
fn names_wildcards_and_calls() {
    expr(
        "s.t.c",
        expect![[r#"
        (COLUMN_REF (NAME "s") "." (NAME "t") "." (NAME "c"))
    "#]],
    );
    expr(
        "t.*",
        expect![[r#"
        (WILDCARD (NAME "t") "." "*")
    "#]],
    );
    expr(
        "pg_catalog.lower(name)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "pg_catalog") "." (NAME "lower")) (ARG_LIST "(" (COLUMN_REF (NAME "name")) ")"))
    "#]],
    );
    expr(
        "count(DISTINCT a)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "count")) (ARG_LIST "(" "DISTINCT" (COLUMN_REF (NAME "a")) ")"))
    "#]],
    );
    expr(
        "date",
        expect![[r#"
        (COLUMN_REF (NAME "date"))
    "#]],
    );
    expr(
        "left(name, 2)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "left")) (ARG_LIST "(" (COLUMN_REF (NAME "name")) "," (LITERAL "2") ")"))
    "#]],
    );
    expr(
        "f(a => 1, b := 2)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "f")) (ARG_LIST "(" (NAMED_ARG (NAME "a") "=>" (LITERAL "1")) "," (NAMED_ARG (NAME "b") ":=" (LITERAL "2")) ")"))
    "#]],
    );
}

#[test]
fn special_forms_of_calls() {
    expr(
        "CAST(a AS DECIMAL(10, 2))",
        expect![[r#"
        (CAST_EXPR "CAST" "(" (COLUMN_REF (NAME "a")) "AS" (TYPE (QUALIFIED_NAME (NAME "DECIMAL")) (TYPE_ARGS "(" (LITERAL "10") "," (LITERAL "2") ")")) ")")
    "#]],
    );
    expr(
        "a::numeric(10,2)[]",
        expect![[r#"
        (TYPECAST_EXPR (COLUMN_REF (NAME "a")) "::" (TYPE (QUALIFIED_NAME (NAME "numeric")) (TYPE_ARGS "(" (LITERAL "10") "," (LITERAL "2") ")") "[" "]"))
    "#]],
    );
    expr(
        "EXTRACT(YEAR FROM ts)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "EXTRACT")) (ARG_LIST "(" (NAME "YEAR") "FROM" (COLUMN_REF (NAME "ts")) ")"))
    "#]],
    );
    expr(
        "TRIM(LEADING '0' FROM s)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "TRIM")) (ARG_LIST "(" "LEADING" (LITERAL "'0'") "FROM" (COLUMN_REF (NAME "s")) ")"))
    "#]],
    );
    expr(
        "POSITION('a' IN s)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "POSITION")) (ARG_LIST "(" (LITERAL "'a'") "IN" (COLUMN_REF (NAME "s")) ")"))
    "#]],
    );
    expr(
        "CONVERT(s USING utf8mb4)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "CONVERT")) (ARG_LIST "(" (COLUMN_REF (NAME "s")) "USING" (QUALIFIED_NAME (NAME "utf8mb4")) ")"))
    "#]],
    );
    expr(
        "GROUP_CONCAT(a ORDER BY b DESC SEPARATOR ';')",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "GROUP_CONCAT")) (ARG_LIST "(" (COLUMN_REF (NAME "a")) (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (COLUMN_REF (NAME "b")) "DESC")) "SEPARATOR" (LITERAL "';'") ")"))
    "#]],
    );
    expr(
        "JSON_OBJECT('a' VALUE 1 NULL ON NULL)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "JSON_OBJECT")) (ARG_LIST "(" (JSON_KEY_VALUE (LITERAL "'a'") "VALUE" (LITERAL "1")) (JSON_CLAUSE "NULL" "ON" "NULL") ")"))
    "#]],
    );
    expr(
        "MATCH (a, b) AGAINST ('x' IN BOOLEAN MODE)",
        expect![[r#"
        (MATCH_AGAINST_EXPR (QUALIFIED_NAME (NAME "MATCH")) (ARG_LIST "(" (COLUMN_REF (NAME "a")) "," (COLUMN_REF (NAME "b")) ")") "AGAINST" "(" (LITERAL "'x'") "IN" "BOOLEAN" "MODE" ")")
    "#]],
    );
}

#[test]
fn window_functions_and_aggregates() {
    expr(
        "sum(a) FILTER (WHERE b) OVER (PARTITION BY c ORDER BY d RANGE BETWEEN INTERVAL '1' DAY PRECEDING AND CURRENT ROW EXCLUDE TIES)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "sum")) (ARG_LIST "(" (COLUMN_REF (NAME "a")) ")") (FILTER_CLAUSE "FILTER" "(" (WHERE_CLAUSE "WHERE" (COLUMN_REF (NAME "b"))) ")") (OVER_CLAUSE "OVER" (WINDOW_SPEC "(" (PARTITION_BY_CLAUSE "PARTITION" "BY" (COLUMN_REF (NAME "c"))) (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (COLUMN_REF (NAME "d")))) (FRAME_CLAUSE "RANGE" "BETWEEN" (INTERVAL_EXPR "INTERVAL" (LITERAL "'1'") "DAY") "PRECEDING" "AND" "CURRENT" "ROW" "EXCLUDE" "TIES") ")")))
    "#]],
    );
    expr(
        "rank() OVER w",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "rank")) (ARG_LIST "(" ")") (OVER_CLAUSE "OVER" (NAME "w")))
    "#]],
    );
    expr(
        "percentile_disc(0.5) WITHIN GROUP (ORDER BY a)",
        expect![[r#"
        (FUNCTION_CALL (QUALIFIED_NAME (NAME "percentile_disc")) (ARG_LIST "(" (LITERAL "0.5") ")") (WITHIN_GROUP_CLAUSE "WITHIN" "GROUP" "(" (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (COLUMN_REF (NAME "a")))) ")"))
    "#]],
    );
}

#[test]
fn case_rows_arrays_and_subscripts() {
    expr(
        "CASE a WHEN 1 THEN 'x' WHEN 2 THEN 'y' ELSE 'z' END",
        expect![[r#"
        (CASE_EXPR "CASE" (COLUMN_REF (NAME "a")) (WHEN_CLAUSE "WHEN" (LITERAL "1") "THEN" (LITERAL "'x'")) (WHEN_CLAUSE "WHEN" (LITERAL "2") "THEN" (LITERAL "'y'")) (ELSE_CLAUSE "ELSE" (LITERAL "'z'")) "END")
    "#]],
    );
    expr(
        "(a, b) = ROW(1, 2)",
        expect![[r#"
        (BINARY_EXPR (ROW_EXPR "(" (COLUMN_REF (NAME "a")) "," (COLUMN_REF (NAME "b")) ")") "=" (ROW_EXPR "ROW" "(" (LITERAL "1") "," (LITERAL "2") ")"))
    "#]],
    );
    expr(
        "ARRAY[[1, 2], [3]][1][1:2]",
        expect![[r#"
        (INDEX_EXPR (INDEX_EXPR (ARRAY_EXPR "ARRAY" "[" (ARRAY_EXPR "[" (LITERAL "1") "," (LITERAL "2") "]") "," (ARRAY_EXPR "[" (LITERAL "3") "]") "]") "[" (LITERAL "1") "]") "[" (LITERAL "1") ":" (LITERAL "2") "]")
    "#]],
    );
    expr(
        "(c).f",
        expect![[r#"
        (FIELD_EXPR (PAREN_EXPR "(" (COLUMN_REF (NAME "c")) ")") "." (NAME "f"))
    "#]],
    );
    expr(
        "EXISTS (SELECT 1)",
        expect![[r#"
        (EXISTS_EXPR "EXISTS" (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ")"))
    "#]],
    );
    expr(
        "j -> 'a' ->> 'b'",
        expect![[r#"
        (BINARY_EXPR (BINARY_EXPR (COLUMN_REF (NAME "j")) "->" (LITERAL "'a'")) "->>" (LITERAL "'b'"))
    "#]],
    );
    expr(
        "ts AT TIME ZONE 'UTC'",
        expect![[r#"
        (AT_TIME_ZONE_EXPR (COLUMN_REF (NAME "ts")) "AT" "TIME" "ZONE" (LITERAL "'UTC'"))
    "#]],
    );
    expr(
        "a OPERATOR(pg_catalog.+) b",
        expect![[r#"
        (BINARY_EXPR (COLUMN_REF (NAME "a")) (OPERATOR_NAME "OPERATOR" "(" (NAME "pg_catalog") "." "+" ")") (COLUMN_REF (NAME "b")))
    "#]],
    );
}
