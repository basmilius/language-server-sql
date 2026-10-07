use super::{check, check_in};
use crate::Dialect;
use expect_test::expect;

#[test]
fn select_with_every_clause() {
    check(
        "SELECT DISTINCT a, b AS c FROM t WHERE a > 1 GROUP BY a HAVING count(*) > 1 WINDOW w AS (ORDER BY a) ORDER BY a DESC NULLS LAST LIMIT 10 OFFSET 2 FOR UPDATE OF t SKIP LOCKED;",
        expect![[r#"
            (SOURCE_FILE
              (SELECT_STMT (SELECT "SELECT" "DISTINCT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a"))) "," (SELECT_ITEM (COLUMN_REF (NAME "b")) (ALIAS "AS" (NAME "c")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (WHERE_CLAUSE "WHERE" (BINARY_EXPR (COLUMN_REF (NAME "a")) ">" (LITERAL "1"))) (GROUP_BY_CLAUSE "GROUP" "BY" (COLUMN_REF (NAME "a"))) (HAVING_CLAUSE "HAVING" (BINARY_EXPR (FUNCTION_CALL (QUALIFIED_NAME (NAME "count")) (ARG_LIST "(" (WILDCARD "*") ")")) ">" (LITERAL "1"))) (WINDOW_CLAUSE "WINDOW" (WINDOW_DEF (NAME "w") "AS" (WINDOW_SPEC "(" (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (COLUMN_REF (NAME "a")))) ")"))) (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (COLUMN_REF (NAME "a")) "DESC" "NULLS" "LAST")) (LIMIT_CLAUSE "LIMIT" (LITERAL "10")) (OFFSET_CLAUSE "OFFSET" (LITERAL "2")) (LOCKING_CLAUSE "FOR" "UPDATE" "OF" (QUALIFIED_NAME (NAME "t")) "SKIP" "LOCKED")) ";"))
        "#]],
    );
}

#[test]
fn ctes_and_set_operations() {
    check(
        "WITH RECURSIVE r (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT n FROM r UNION SELECT 2 INTERSECT SELECT 3 ORDER BY 1 LIMIT 1;",
        expect![[r#"
            (SOURCE_FILE
              (SELECT_STMT (COMPOUND_SELECT (WITH_CLAUSE "WITH" "RECURSIVE" (CTE (NAME "r") (NAME_LIST "(" (NAME "n") ")") "AS" "(" (COMPOUND_SELECT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) "UNION" "ALL" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (BINARY_EXPR (COLUMN_REF (NAME "n")) "+" (LITERAL "1")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "r")))))) ")")) (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "n")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "r"))))) "UNION" (COMPOUND_SELECT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "2")))) "INTERSECT" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "3"))))) (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (LITERAL "1"))) (LIMIT_CLAUSE "LIMIT" (LITERAL "1"))) ";"))
        "#]],
    );
    check(
        "(SELECT 1) UNION (SELECT 2) LIMIT 1;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (COMPOUND_SELECT (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ")") "UNION" (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "2")))) ")") (LIMIT_CLAUSE "LIMIT" (LITERAL "1"))) ";"))
    "#]],
    );
    check(
        "VALUES (1, 'a'), (2, 'b') ORDER BY 1;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (VALUES "VALUES" (ROW_EXPR "(" (LITERAL "1") "," (LITERAL "'a'") ")") "," (ROW_EXPR "(" (LITERAL "2") "," (LITERAL "'b'") ")") (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (LITERAL "1")))) ";"))
    "#]],
    );
    check(
        "TABLE t;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (TABLE_QUERY "TABLE" (QUALIFIED_NAME (NAME "t"))) ";"))
    "#]],
    );
}

#[test]
fn joins_of_every_kind() {
    check(
        "SELECT * FROM a NATURAL JOIN b CROSS JOIN c LEFT OUTER JOIN d ON a.x = d.x RIGHT JOIN e USING (y) FULL JOIN f ON TRUE, g JOIN (h JOIN i ON h.z = i.z) ON TRUE;",
        expect![[r#"
            (SOURCE_FILE
              (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (WILDCARD "*"))) (FROM_CLAUSE "FROM" (JOIN_EXPR (JOIN_EXPR (JOIN_EXPR (JOIN_EXPR (JOIN_EXPR (TABLE_REF (QUALIFIED_NAME (NAME "a"))) "NATURAL" "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "b")))) "CROSS" "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "c")))) "LEFT" "OUTER" "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "d"))) (ON_CLAUSE "ON" (BINARY_EXPR (COLUMN_REF (NAME "a") "." (NAME "x")) "=" (COLUMN_REF (NAME "d") "." (NAME "x"))))) "RIGHT" "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "e"))) (USING_CLAUSE "USING" (NAME_LIST "(" (NAME "y") ")"))) "FULL" "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "f"))) (ON_CLAUSE "ON" (LITERAL "TRUE"))) "," (JOIN_EXPR (TABLE_REF (QUALIFIED_NAME (NAME "g"))) "JOIN" (PAREN_JOIN "(" (JOIN_EXPR (TABLE_REF (QUALIFIED_NAME (NAME "h"))) "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "i"))) (ON_CLAUSE "ON" (BINARY_EXPR (COLUMN_REF (NAME "h") "." (NAME "z")) "=" (COLUMN_REF (NAME "i") "." (NAME "z"))))) ")") (ON_CLAUSE "ON" (LITERAL "TRUE"))))) ";"))
        "#]],
    );
    check(
        "SELECT * FROM t1 AS x (a, b), LATERAL (SELECT 1) l, generate_series(1, 2) WITH ORDINALITY AS s (v, n), ONLY p;",
        expect![[r#"
            (SOURCE_FILE
              (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (WILDCARD "*"))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t1")) (ALIAS "AS" (NAME "x") (NAME_LIST "(" (NAME "a") "," (NAME "b") ")"))) "," (DERIVED_TABLE "LATERAL" (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ")") (ALIAS (NAME "l"))) "," (TABLE_FUNCTION (FUNCTION_CALL (QUALIFIED_NAME (NAME "generate_series")) (ARG_LIST "(" (LITERAL "1") "," (LITERAL "2") ")")) "WITH" "ORDINALITY" (ALIAS "AS" (NAME "s") (NAME_LIST "(" (NAME "v") "," (NAME "n") ")"))) "," (TABLE_REF "ONLY" (QUALIFIED_NAME (NAME "p"))))) ";"))
        "#]],
    );
    check_in(
        Dialect::Mysql,
        "SELECT * FROM t PARTITION (p1) AS x USE INDEX FOR JOIN (i) STRAIGHT_JOIN u;",
        expect![[r#"
            (SOURCE_FILE
              (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (WILDCARD "*"))) (FROM_CLAUSE "FROM" (JOIN_EXPR (TABLE_REF (QUALIFIED_NAME (NAME "t")) (PARTITION_SELECTION "PARTITION" (NAME_LIST "(" (NAME "p1") ")")) (ALIAS "AS" (NAME "x")) (INDEX_HINT "USE" "INDEX" "FOR" "JOIN" (NAME_LIST "(" (NAME "i") ")"))) "STRAIGHT_JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "u")))))) ";"))
        "#]],
    );
}

#[test]
fn grouping() {
    check(
        "SELECT a FROM t GROUP BY ROLLUP (a, b), CUBE (c), GROUPING SETS ((a), ());",
        expect![[r#"
            (SOURCE_FILE
              (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (GROUP_BY_CLAUSE "GROUP" "BY" (GROUPING_SET "ROLLUP" "(" (COLUMN_REF (NAME "a")) "," (COLUMN_REF (NAME "b")) ")") "," (GROUPING_SET "CUBE" "(" (COLUMN_REF (NAME "c")) ")") "," (GROUPING_SET "GROUPING" "SETS" "(" (PAREN_EXPR "(" (COLUMN_REF (NAME "a")) ")") "," (GROUPING_SET "(" ")") ")"))) ";"))
        "#]],
    );
    check(
        "SELECT a FROM t GROUP BY a WITH ROLLUP;",
        expect![[r#"
        (SOURCE_FILE
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (GROUP_BY_CLAUSE "GROUP" "BY" (COLUMN_REF (NAME "a")) "WITH" "ROLLUP")) ";"))
    "#]],
    );
}

#[test]
fn insert_in_every_form() {
    check(
        "INSERT INTO s.t (a, b) VALUES (1, DEFAULT), (2, 3) ON CONFLICT (a) WHERE b > 0 DO UPDATE SET b = excluded.b WHERE t.b < 9 RETURNING a, b AS c;",
        expect![[r#"
            (SOURCE_FILE
              (INSERT_STMT "INSERT" "INTO" (QUALIFIED_NAME (NAME "s") "." (NAME "t")) (NAME_LIST "(" (NAME "a") "," (NAME "b") ")") (VALUES "VALUES" (ROW_EXPR "(" (LITERAL "1") "," (DEFAULT_EXPR "DEFAULT") ")") "," (ROW_EXPR "(" (LITERAL "2") "," (LITERAL "3") ")")) (UPSERT_CLAUSE "ON" "CONFLICT" (CONFLICT_TARGET (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "a"))) ")") (WHERE_CLAUSE "WHERE" (BINARY_EXPR (COLUMN_REF (NAME "b")) ">" (LITERAL "0")))) "DO" "UPDATE" (SET_CLAUSE "SET" (ASSIGNMENT (COLUMN_REF (NAME "b")) "=" (COLUMN_REF (NAME "excluded") "." (NAME "b")))) (WHERE_CLAUSE "WHERE" (BINARY_EXPR (COLUMN_REF (NAME "t") "." (NAME "b")) "<" (LITERAL "9")))) (RETURNING_CLAUSE "RETURNING" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a"))) "," (SELECT_ITEM (COLUMN_REF (NAME "b")) (ALIAS "AS" (NAME "c"))))) ";"))
        "#]],
    );
    check(
        "INSERT INTO t SELECT * FROM u ON CONFLICT ON CONSTRAINT pk DO NOTHING;",
        expect![[r#"
        (SOURCE_FILE
          (INSERT_STMT "INSERT" "INTO" (QUALIFIED_NAME (NAME "t")) (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (WILDCARD "*"))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "u"))))) (UPSERT_CLAUSE "ON" "CONFLICT" (CONFLICT_TARGET "ON" "CONSTRAINT" (NAME "pk")) "DO" "NOTHING") ";"))
    "#]],
    );
    check(
        "INSERT INTO t DEFAULT VALUES;",
        expect![[r#"
        (SOURCE_FILE
          (INSERT_STMT "INSERT" "INTO" (QUALIFIED_NAME (NAME "t")) "DEFAULT" "VALUES" ";"))
    "#]],
    );
    check_in(
        Dialect::Mysql,
        "INSERT IGNORE INTO t SET a = 1, b = 2 ON DUPLICATE KEY UPDATE a = VALUES(a) + 1;",
        expect![[r#"
            (SOURCE_FILE
              (INSERT_STMT "INSERT" "IGNORE" "INTO" (QUALIFIED_NAME (NAME "t")) (SET_CLAUSE "SET" (ASSIGNMENT (COLUMN_REF (NAME "a")) "=" (LITERAL "1")) "," (ASSIGNMENT (COLUMN_REF (NAME "b")) "=" (LITERAL "2"))) (ON_DUPLICATE_KEY_CLAUSE "ON" "DUPLICATE" "KEY" "UPDATE" (SET_CLAUSE (ASSIGNMENT (COLUMN_REF (NAME "a")) "=" (BINARY_EXPR (FUNCTION_CALL (QUALIFIED_NAME (NAME "VALUES")) (ARG_LIST "(" (COLUMN_REF (NAME "a")) ")")) "+" (LITERAL "1"))))) ";"))
        "#]],
    );
    check_in(
        Dialect::Sqlite,
        "INSERT OR REPLACE INTO t (a) VALUES (1);",
        expect![[r#"
        (SOURCE_FILE
          (INSERT_STMT "INSERT" "OR" "REPLACE" "INTO" (QUALIFIED_NAME (NAME "t")) (NAME_LIST "(" (NAME "a") ")") (VALUES "VALUES" (ROW_EXPR "(" (LITERAL "1") ")")) ";"))
    "#]],
    );
    check_in(
        Dialect::Mysql,
        "REPLACE INTO t VALUE (1);",
        expect![[r#"
        (SOURCE_FILE
          (INSERT_STMT "REPLACE" "INTO" (QUALIFIED_NAME (NAME "t")) (VALUES "VALUE" (ROW_EXPR "(" (LITERAL "1") ")")) ";"))
    "#]],
    );
}

#[test]
fn update_delete_and_merge() {
    check(
        "UPDATE ONLY t AS x SET a = 1, (b, c) = (2, 3) FROM u WHERE x.id = u.id RETURNING *;",
        expect![[r#"
        (SOURCE_FILE
          (UPDATE_STMT "UPDATE" (TABLE_REF "ONLY" (QUALIFIED_NAME (NAME "t")) (ALIAS "AS" (NAME "x"))) (SET_CLAUSE "SET" (ASSIGNMENT (COLUMN_REF (NAME "a")) "=" (LITERAL "1")) "," (ASSIGNMENT (NAME_LIST "(" (NAME "b") "," (NAME "c") ")") "=" (ROW_EXPR "(" (LITERAL "2") "," (LITERAL "3") ")"))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "u")))) (WHERE_CLAUSE "WHERE" (BINARY_EXPR (COLUMN_REF (NAME "x") "." (NAME "id")) "=" (COLUMN_REF (NAME "u") "." (NAME "id")))) (RETURNING_CLAUSE "RETURNING" (SELECT_LIST (SELECT_ITEM (WILDCARD "*")))) ";"))
    "#]],
    );
    check_in(
        Dialect::Mysql,
        "UPDATE t JOIN u ON t.id = u.id SET t.a = u.a ORDER BY t.id LIMIT 5;",
        expect![[r#"
        (SOURCE_FILE
          (UPDATE_STMT "UPDATE" (JOIN_EXPR (TABLE_REF (QUALIFIED_NAME (NAME "t"))) "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "u"))) (ON_CLAUSE "ON" (BINARY_EXPR (COLUMN_REF (NAME "t") "." (NAME "id")) "=" (COLUMN_REF (NAME "u") "." (NAME "id"))))) (SET_CLAUSE "SET" (ASSIGNMENT (COLUMN_REF (NAME "t") "." (NAME "a")) "=" (COLUMN_REF (NAME "u") "." (NAME "a")))) (ORDER_BY_CLAUSE "ORDER" "BY" (ORDER_ITEM (COLUMN_REF (NAME "t") "." (NAME "id")))) (LIMIT_CLAUSE "LIMIT" (LITERAL "5")) ";"))
    "#]],
    );
    check(
        "DELETE FROM t USING u WHERE t.id = u.id;",
        expect![[r#"
        (SOURCE_FILE
          (DELETE_STMT "DELETE" (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (USING_CLAUSE "USING" (TABLE_REF (QUALIFIED_NAME (NAME "u")))) (WHERE_CLAUSE "WHERE" (BINARY_EXPR (COLUMN_REF (NAME "t") "." (NAME "id")) "=" (COLUMN_REF (NAME "u") "." (NAME "id")))) ";"))
    "#]],
    );
    check_in(
        Dialect::Mysql,
        "DELETE t1, t2 FROM t1 JOIN t2 ON t1.id = t2.id;",
        expect![[r#"
        (SOURCE_FILE
          (DELETE_STMT "DELETE" (QUALIFIED_NAME (NAME "t1")) "," (QUALIFIED_NAME (NAME "t2")) (FROM_CLAUSE "FROM" (JOIN_EXPR (TABLE_REF (QUALIFIED_NAME (NAME "t1"))) "JOIN" (TABLE_REF (QUALIFIED_NAME (NAME "t2"))) (ON_CLAUSE "ON" (BINARY_EXPR (COLUMN_REF (NAME "t1") "." (NAME "id")) "=" (COLUMN_REF (NAME "t2") "." (NAME "id")))))) ";"))
    "#]],
    );
    check(
        "DELETE FROM t WHERE CURRENT OF c;",
        expect![[r#"
        (SOURCE_FILE
          (DELETE_STMT "DELETE" (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (WHERE_CLAUSE "WHERE" "CURRENT" "OF" (NAME "c")) ";"))
    "#]],
    );
    check(
        "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED AND s.gone THEN DELETE WHEN MATCHED THEN UPDATE SET a = s.a WHEN NOT MATCHED BY TARGET THEN INSERT (id, a) VALUES (s.id, s.a) WHEN NOT MATCHED BY SOURCE THEN DO NOTHING;",
        expect![[r#"
            (SOURCE_FILE
              (MERGE_STMT "MERGE" "INTO" (QUALIFIED_NAME (NAME "t")) "USING" (TABLE_REF (QUALIFIED_NAME (NAME "s"))) (ON_CLAUSE "ON" (BINARY_EXPR (COLUMN_REF (NAME "t") "." (NAME "id")) "=" (COLUMN_REF (NAME "s") "." (NAME "id")))) (MERGE_WHEN_CLAUSE "WHEN" "MATCHED" "AND" (COLUMN_REF (NAME "s") "." (NAME "gone")) "THEN" "DELETE") (MERGE_WHEN_CLAUSE "WHEN" "MATCHED" "THEN" "UPDATE" (SET_CLAUSE "SET" (ASSIGNMENT (COLUMN_REF (NAME "a")) "=" (COLUMN_REF (NAME "s") "." (NAME "a"))))) (MERGE_WHEN_CLAUSE "WHEN" "NOT" "MATCHED" "BY" "TARGET" "THEN" "INSERT" (NAME_LIST "(" (NAME "id") "," (NAME "a") ")") (VALUES "VALUES" (ROW_EXPR "(" (COLUMN_REF (NAME "s") "." (NAME "id")) "," (COLUMN_REF (NAME "s") "." (NAME "a")) ")"))) (MERGE_WHEN_CLAUSE "WHEN" "NOT" "MATCHED" "BY" "SOURCE" "THEN" "DO" "NOTHING") ";"))
        "#]],
    );
    check(
        "WITH x AS (SELECT 1 AS id) DELETE FROM t WHERE id IN (SELECT id FROM x);",
        expect![[r#"
        (SOURCE_FILE
          (DELETE_STMT (WITH_CLAUSE "WITH" (CTE (NAME "x") "AS" "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1") (ALIAS "AS" (NAME "id"))))) ")")) "DELETE" (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))) (WHERE_CLAUSE "WHERE" (IN_EXPR (COLUMN_REF (NAME "id")) "IN" (PAREN_QUERY "(" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "id")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "x"))))) ")"))) ";"))
    "#]],
    );
}

#[test]
fn create_table_with_columns_and_constraints() {
    check(
        "CREATE TABLE IF NOT EXISTS app.users (id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, email varchar(255) NOT NULL UNIQUE, team_id int REFERENCES teams (id) ON DELETE SET NULL, total numeric(10, 2) GENERATED ALWAYS AS (price * 2) STORED, created timestamp with time zone DEFAULT now(), CONSTRAINT uq UNIQUE (email, team_id), CHECK (total >= 0), FOREIGN KEY (team_id) REFERENCES teams (id) MATCH FULL);",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_TABLE_STMT "CREATE" "TABLE" "IF" "NOT" "EXISTS" (QUALIFIED_NAME (NAME "app") "." (NAME "users")) (TABLE_ELEMENT_LIST "(" (COLUMN_DEF (NAME "id") (TYPE (QUALIFIED_NAME (NAME "bigint"))) (COLUMN_CONSTRAINT "GENERATED" "BY" "DEFAULT" "AS" "IDENTITY") (COLUMN_CONSTRAINT "PRIMARY" "KEY")) "," (COLUMN_DEF (NAME "email") (TYPE (QUALIFIED_NAME (NAME "varchar")) (TYPE_ARGS "(" (LITERAL "255") ")")) (COLUMN_CONSTRAINT "NOT" "NULL") (COLUMN_CONSTRAINT "UNIQUE")) "," (COLUMN_DEF (NAME "team_id") (TYPE (QUALIFIED_NAME (NAME "int"))) (COLUMN_CONSTRAINT (REFERENCES_CLAUSE "REFERENCES" (QUALIFIED_NAME (NAME "teams")) (NAME_LIST "(" (NAME "id") ")") "ON" "DELETE" "SET" "NULL"))) "," (COLUMN_DEF (NAME "total") (TYPE (QUALIFIED_NAME (NAME "numeric")) (TYPE_ARGS "(" (LITERAL "10") "," (LITERAL "2") ")")) (COLUMN_CONSTRAINT "GENERATED" "ALWAYS" "AS" "(" (BINARY_EXPR (COLUMN_REF (NAME "price")) "*" (LITERAL "2")) ")" "STORED")) "," (COLUMN_DEF (NAME "created") (TYPE "timestamp" "with" "time" "zone") (COLUMN_CONSTRAINT "DEFAULT" (FUNCTION_CALL (QUALIFIED_NAME (NAME "now")) (ARG_LIST "(" ")")))) "," (TABLE_CONSTRAINT "CONSTRAINT" (NAME "uq") "UNIQUE" (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "email"))) "," (INDEX_COLUMN (COLUMN_REF (NAME "team_id"))) ")")) "," (TABLE_CONSTRAINT "CHECK" "(" (BINARY_EXPR (COLUMN_REF (NAME "total")) ">=" (LITERAL "0")) ")") "," (TABLE_CONSTRAINT "FOREIGN" "KEY" (NAME_LIST "(" (NAME "team_id") ")") (REFERENCES_CLAUSE "REFERENCES" (QUALIFIED_NAME (NAME "teams")) (NAME_LIST "(" (NAME "id") ")") "MATCH" "FULL")) ")") ";"))
        "#]],
    );
    check_in(
        Dialect::Mysql,
        "CREATE TABLE `t` (`id` int(10) unsigned NOT NULL AUTO_INCREMENT COMMENT 'id', `s` enum('a','b') CHARACTER SET utf8mb4 DEFAULT 'a', PRIMARY KEY (`id`), UNIQUE KEY `u` (`s`), KEY `p` (`s`(10)) USING BTREE) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 PARTITION BY RANGE (id) (PARTITION p0 VALUES LESS THAN (10), PARTITION p1 VALUES LESS THAN MAXVALUE);",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_TABLE_STMT "CREATE" "TABLE" (QUALIFIED_NAME (NAME "`t`")) (TABLE_ELEMENT_LIST "(" (COLUMN_DEF (NAME "`id`") (TYPE (QUALIFIED_NAME (NAME "int")) (TYPE_ARGS "(" (LITERAL "10") ")") "unsigned") (COLUMN_CONSTRAINT "NOT" "NULL") (COLUMN_CONSTRAINT "AUTO_INCREMENT") (COLUMN_CONSTRAINT "COMMENT" (LITERAL "'id'"))) "," (COLUMN_DEF (NAME "`s`") (TYPE (QUALIFIED_NAME (NAME "enum")) (TYPE_ARGS "(" (LITERAL "'a'") "," (LITERAL "'b'") ")") "CHARACTER" "SET" (QUALIFIED_NAME (NAME "utf8mb4"))) (COLUMN_CONSTRAINT "DEFAULT" (LITERAL "'a'"))) "," (TABLE_CONSTRAINT "PRIMARY" "KEY" (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "`id`"))) ")")) "," (TABLE_CONSTRAINT "UNIQUE" "KEY" (NAME "`u`") (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "`s`"))) ")")) "," (TABLE_CONSTRAINT "KEY" (NAME "`p`") (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "`s`")) "(" "10" ")") ")") "USING" (NAME "BTREE")) ")") (TABLE_OPTION "ENGINE" "=" "InnoDB") (TABLE_OPTION "DEFAULT" "CHARSET" "=" "utf8mb4") (TABLE_PARTITION_CLAUSE "PARTITION" "BY" "RANGE" (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "id"))) ")") "(" (PARTITION_DEF "PARTITION" (NAME "p0") "VALUES" "LESS" "THAN" "(" "10" ")") "," (PARTITION_DEF "PARTITION" (NAME "p1") "VALUES" "LESS" "THAN" "MAXVALUE") ")") ";"))
        "#]],
    );
    check_in(
        Dialect::Sqlite,
        "CREATE TABLE kv (key TEXT PRIMARY KEY ON CONFLICT REPLACE, value, [order] INTEGER) WITHOUT ROWID, STRICT;",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_TABLE_STMT "CREATE" "TABLE" (QUALIFIED_NAME (NAME "kv")) (TABLE_ELEMENT_LIST "(" (COLUMN_DEF (NAME "key") (TYPE (QUALIFIED_NAME (NAME "TEXT"))) (COLUMN_CONSTRAINT "PRIMARY" "KEY" "ON" "CONFLICT" "REPLACE")) "," (COLUMN_DEF (NAME "value")) "," (COLUMN_DEF (NAME "[order]") (TYPE (QUALIFIED_NAME (NAME "INTEGER")))) ")") (TABLE_OPTION "WITHOUT" "ROWID") "," (TABLE_OPTION "STRICT") ";"))
        "#]],
    );
    check(
        "CREATE TEMPORARY TABLE t AS SELECT 1 WITH NO DATA;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_TABLE_STMT "CREATE" "TEMPORARY" "TABLE" (QUALIFIED_NAME (NAME "t")) "AS" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) "WITH" "NO" "DATA" ";"))
    "#]],
    );
    check(
        "CREATE TABLE p1 PARTITION OF p FOR VALUES FROM (1) TO (10);",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_TABLE_STMT "CREATE" "TABLE" (QUALIFIED_NAME (NAME "p1")) "PARTITION" "OF" (QUALIFIED_NAME (NAME "p")) "FOR" "VALUES" "FROM" "(" "1" ")" "TO" "(" "10" ")" ";"))
    "#]],
    );
}

#[test]
fn alter_and_drop() {
    check(
        "ALTER TABLE IF EXISTS ONLY t ADD COLUMN IF NOT EXISTS c int NOT NULL, DROP COLUMN d CASCADE, ALTER COLUMN e SET DATA TYPE text USING e::text, ALTER COLUMN f DROP NOT NULL, ADD CONSTRAINT k UNIQUE (g), DROP CONSTRAINT IF EXISTS h, RENAME COLUMN i TO j, RENAME TO u;",
        expect![[r#"
            (SOURCE_FILE
              (ALTER_TABLE_STMT "ALTER" "TABLE" "IF" "EXISTS" "ONLY" (QUALIFIED_NAME (NAME "t")) (ADD_COLUMN_ACTION "ADD" "COLUMN" "IF" "NOT" "EXISTS" (COLUMN_DEF (NAME "c") (TYPE (QUALIFIED_NAME (NAME "int"))) (COLUMN_CONSTRAINT "NOT" "NULL"))) "," (DROP_COLUMN_ACTION "DROP" "COLUMN" (NAME "d") "CASCADE") "," (ALTER_COLUMN_ACTION "ALTER" "COLUMN" (NAME "e") "SET" "DATA" "TYPE" (TYPE (QUALIFIED_NAME (NAME "text"))) "USING" (TYPECAST_EXPR (COLUMN_REF (NAME "e")) "::" (TYPE (QUALIFIED_NAME (NAME "text"))))) "," (ALTER_COLUMN_ACTION "ALTER" "COLUMN" (NAME "f") "DROP" "NOT" "NULL") "," (ADD_CONSTRAINT_ACTION "ADD" (TABLE_CONSTRAINT "CONSTRAINT" (NAME "k") "UNIQUE" (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "g"))) ")"))) "," (DROP_CONSTRAINT_ACTION "DROP" "CONSTRAINT" "IF" "EXISTS" (NAME "h")) "," (RENAME_COLUMN_ACTION "RENAME" "COLUMN" (NAME "i") "TO" (NAME "j")) "," (RENAME_TABLE_ACTION "RENAME" "TO" (QUALIFIED_NAME (NAME "u"))) ";"))
        "#]],
    );
    check_in(
        Dialect::Mysql,
        "ALTER TABLE t MODIFY COLUMN a int NOT NULL FIRST, CHANGE b c varchar(5) AFTER a, DROP PRIMARY KEY, DROP FOREIGN KEY fk, ADD INDEX i (a), ALGORITHM=INPLACE;",
        expect![[r#"
            (SOURCE_FILE
              (ALTER_TABLE_STMT "ALTER" "TABLE" (QUALIFIED_NAME (NAME "t")) (MODIFY_COLUMN_ACTION "MODIFY" "COLUMN" (COLUMN_DEF (NAME "a") (TYPE (QUALIFIED_NAME (NAME "int"))) (COLUMN_CONSTRAINT "NOT" "NULL")) "FIRST") "," (MODIFY_COLUMN_ACTION "CHANGE" (NAME "b") (COLUMN_DEF (NAME "c") (TYPE (QUALIFIED_NAME (NAME "varchar")) (TYPE_ARGS "(" (LITERAL "5") ")"))) "AFTER" (NAME "a")) "," (DROP_CONSTRAINT_ACTION "DROP" "PRIMARY" "KEY") "," (DROP_CONSTRAINT_ACTION "DROP" "FOREIGN" "KEY" (NAME "fk")) "," (ADD_CONSTRAINT_ACTION "ADD" (TABLE_CONSTRAINT "INDEX" (NAME "i") (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (COLUMN_REF (NAME "a"))) ")"))) "," (ALTER_TABLE_ACTION "ALGORITHM" "=" "INPLACE") ";"))
        "#]],
    );
    check(
        "DROP TABLE IF EXISTS a, b CASCADE;",
        expect![[r#"
        (SOURCE_FILE
          (DROP_STMT "DROP" "TABLE" "IF" "EXISTS" (QUALIFIED_NAME (NAME "a")) "," (QUALIFIED_NAME (NAME "b")) "CASCADE" ";"))
    "#]],
    );
    check(
        "DROP MATERIALIZED VIEW mv;",
        expect![[r#"
        (SOURCE_FILE
          (DROP_STMT "DROP" "MATERIALIZED" "VIEW" (QUALIFIED_NAME (NAME "mv")) ";"))
    "#]],
    );
    check(
        "DROP FUNCTION f(int, text);",
        expect![[r#"
        (SOURCE_FILE
          (DROP_STMT "DROP" "FUNCTION" (QUALIFIED_NAME (NAME "f")) "(" "int" "," "text" ")" ";"))
    "#]],
    );
    check(
        "DROP INDEX i ON t;",
        expect![[r#"
        (SOURCE_FILE
          (DROP_STMT "DROP" "INDEX" (QUALIFIED_NAME (NAME "i")) "ON" (QUALIFIED_NAME (NAME "t")) ";"))
    "#]],
    );
    check(
        "TRUNCATE TABLE a, b RESTART IDENTITY;",
        expect![[r#"
        (SOURCE_FILE
          (TRUNCATE_STMT "TRUNCATE" "TABLE" (QUALIFIED_NAME (NAME "a")) "," (QUALIFIED_NAME (NAME "b")) "RESTART" "IDENTITY" ";"))
    "#]],
    );
}

#[test]
fn indexes_views_and_other_objects() {
    check(
        "CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS i ON ONLY t USING btree (lower(a) text_pattern_ops DESC NULLS LAST, b) INCLUDE (c) WHERE a IS NOT NULL;",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_INDEX_STMT "CREATE" "UNIQUE" "INDEX" "CONCURRENTLY" "IF" "NOT" "EXISTS" (QUALIFIED_NAME (NAME "i")) "ON" "ONLY" (QUALIFIED_NAME (NAME "t")) "USING" (NAME "btree") (INDEX_COLUMN_LIST "(" (INDEX_COLUMN (FUNCTION_CALL (QUALIFIED_NAME (NAME "lower")) (ARG_LIST "(" (COLUMN_REF (NAME "a")) ")")) (QUALIFIED_NAME (NAME "text_pattern_ops")) "DESC" "NULLS" "LAST") "," (INDEX_COLUMN (COLUMN_REF (NAME "b"))) ")") "INCLUDE" (NAME_LIST "(" (NAME "c") ")") (WHERE_CLAUSE "WHERE" (IS_EXPR (COLUMN_REF (NAME "a")) "IS" "NOT" "NULL")) ";"))
        "#]],
    );
    check(
        "CREATE OR REPLACE RECURSIVE VIEW v (a) AS SELECT 1 WITH LOCAL CHECK OPTION;",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_VIEW_STMT "CREATE" "OR" "REPLACE" "RECURSIVE" "VIEW" (QUALIFIED_NAME (NAME "v")) (NAME_LIST "(" (NAME "a") ")") "AS" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) "WITH" "LOCAL" "CHECK" "OPTION" ";"))
        "#]],
    );
    check(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS mv AS SELECT 1 WITH DATA;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_VIEW_STMT "CREATE" "MATERIALIZED" "VIEW" "IF" "NOT" "EXISTS" (QUALIFIED_NAME (NAME "mv")) "AS" (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) "WITH" "DATA" ";"))
    "#]],
    );
    check(
        "REFRESH MATERIALIZED VIEW CONCURRENTLY mv;",
        expect![[r#"
        (SOURCE_FILE
          (REFRESH_STMT "REFRESH" "MATERIALIZED" "VIEW" "CONCURRENTLY" (QUALIFIED_NAME (NAME "mv")) ";"))
    "#]],
    );
    check(
        "CREATE SCHEMA IF NOT EXISTS s AUTHORIZATION u;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_SCHEMA_STMT "CREATE" "SCHEMA" "IF" "NOT" "EXISTS" (NAME "s") "AUTHORIZATION" "u" ";"))
    "#]],
    );
    check(
        "CREATE SEQUENCE s START WITH 1 INCREMENT BY 2;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_SEQUENCE_STMT "CREATE" "SEQUENCE" (QUALIFIED_NAME (NAME "s")) "START" "WITH" "1" "INCREMENT" "BY" "2" ";"))
    "#]],
    );
    check(
        "CREATE TYPE mood AS ENUM ('sad', 'ok');",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_TYPE_STMT "CREATE" "TYPE" (QUALIFIED_NAME (NAME "mood")) "AS" "ENUM" (ENUM_VALUE_LIST "(" (LITERAL "'sad'") "," (LITERAL "'ok'") ")") ";"))
    "#]],
    );
    check(
        "CREATE TYPE pair AS (a int, b text);",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_TYPE_STMT "CREATE" "TYPE" (QUALIFIED_NAME (NAME "pair")) "AS" (TABLE_ELEMENT_LIST "(" (COLUMN_DEF (NAME "a") (TYPE (QUALIFIED_NAME (NAME "int")))) "," (COLUMN_DEF (NAME "b") (TYPE (QUALIFIED_NAME (NAME "text")))) ")") ";"))
    "#]],
    );
    check(
        "CREATE DOMAIN d AS int NOT NULL CHECK (VALUE > 0);",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_DOMAIN_STMT "CREATE" "DOMAIN" (QUALIFIED_NAME (NAME "d")) "AS" (TYPE (QUALIFIED_NAME (NAME "int"))) (COLUMN_CONSTRAINT "NOT" "NULL") (COLUMN_CONSTRAINT "CHECK" "(" (BINARY_EXPR (COLUMN_REF (NAME "VALUE")) ">" (LITERAL "0")) ")") ";"))
    "#]],
    );
    check(
        "CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_EXTENSION_STMT "CREATE" "EXTENSION" "IF" "NOT" "EXISTS" (QUALIFIED_NAME (NAME "pgcrypto")) "WITH" "SCHEMA" "public" ";"))
    "#]],
    );
    check(
        "COMMENT ON COLUMN t.a IS 'The a';",
        expect![[r#"
        (SOURCE_FILE
          (COMMENT_STMT "COMMENT" "ON" "COLUMN" (QUALIFIED_NAME (NAME "t") "." (NAME "a")) "IS" (LITERAL "'The a'") ";"))
    "#]],
    );
}

#[test]
fn functions_procedures_and_triggers() {
    check(
        "CREATE OR REPLACE FUNCTION f(a int, OUT b text, VARIADIC c int[] DEFAULT '{}') RETURNS SETOF record LANGUAGE plpgsql STABLE AS $$ BEGIN RETURN; END $$;",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_FUNCTION_STMT "CREATE" "OR" "REPLACE" "FUNCTION" (QUALIFIED_NAME (NAME "f")) (PARAM_LIST "(" (PARAM_DEF (NAME "a") (TYPE (QUALIFIED_NAME (NAME "int")))) "," (PARAM_DEF "OUT" (NAME "b") (TYPE (QUALIFIED_NAME (NAME "text")))) "," (PARAM_DEF "VARIADIC" (NAME "c") (TYPE (QUALIFIED_NAME (NAME "int")) "[" "]") "DEFAULT" (LITERAL "'{}'")) ")") (RETURNS_CLAUSE "RETURNS" "SETOF" (TYPE (QUALIFIED_NAME (NAME "record")))) "LANGUAGE" (NAME "plpgsql") "STABLE" "AS" (ROUTINE_BODY "$$ BEGIN RETURN; END $$") ";"))
        "#]],
    );
    check(
        "CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE sql RETURN a + b;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_FUNCTION_STMT "CREATE" "FUNCTION" (QUALIFIED_NAME (NAME "add")) (PARAM_LIST "(" (PARAM_DEF (NAME "a") (TYPE (QUALIFIED_NAME (NAME "int")))) "," (PARAM_DEF (NAME "b") (TYPE (QUALIFIED_NAME (NAME "int")))) ")") (RETURNS_CLAUSE "RETURNS" (TYPE (QUALIFIED_NAME (NAME "int")))) "LANGUAGE" (NAME "sql") (ROUTINE_BODY (RETURN_STMT "RETURN" (BINARY_EXPR (COLUMN_REF (NAME "a")) "+" (COLUMN_REF (NAME "b"))))) ";"))
    "#]],
    );
    check(
        "CREATE PROCEDURE p() LANGUAGE sql BEGIN ATOMIC INSERT INTO t VALUES (1); END;",
        expect![[r#"
        (SOURCE_FILE
          (CREATE_FUNCTION_STMT "CREATE" "PROCEDURE" (QUALIFIED_NAME (NAME "p")) (PARAM_LIST "(" ")") "LANGUAGE" (NAME "sql") (ROUTINE_BODY (BLOCK "BEGIN" "ATOMIC" (STATEMENT_LIST
                  (INSERT_STMT "INSERT" "INTO" (QUALIFIED_NAME (NAME "t")) (VALUES "VALUES" (ROW_EXPR "(" (LITERAL "1") ")")) ";")) "END")) ";"))
    "#]],
    );
    check(
        "CREATE TRIGGER tr BEFORE UPDATE OF a, b ON t FOR EACH ROW WHEN (OLD.a IS DISTINCT FROM NEW.a) EXECUTE FUNCTION f();",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_TRIGGER_STMT "CREATE" "TRIGGER" (QUALIFIED_NAME (NAME "tr")) "BEFORE" "UPDATE" "OF" (NAME "a") "," (NAME "b") "ON" (QUALIFIED_NAME (NAME "t")) "FOR" "EACH" "ROW" "WHEN" (PAREN_EXPR "(" (IS_EXPR (COLUMN_REF (NAME "OLD") "." (NAME "a")) "IS" "DISTINCT" "FROM" (COLUMN_REF (NAME "NEW") "." (NAME "a"))) ")") "EXECUTE" "FUNCTION" (ROUTINE_BODY (FUNCTION_CALL (QUALIFIED_NAME (NAME "f")) (ARG_LIST "(" ")"))) ";"))
        "#]],
    );
    check_in(
        Dialect::Sqlite,
        "CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE u SET n = n + 1; DELETE FROM v; END;",
        expect![[r#"
            (SOURCE_FILE
              (CREATE_TRIGGER_STMT "CREATE" "TRIGGER" (QUALIFIED_NAME (NAME "tr")) "AFTER" "INSERT" "ON" (QUALIFIED_NAME (NAME "t")) (ROUTINE_BODY (BLOCK "BEGIN" (STATEMENT_LIST
                      (UPDATE_STMT "UPDATE" (TABLE_REF (QUALIFIED_NAME (NAME "u"))) (SET_CLAUSE "SET" (ASSIGNMENT (COLUMN_REF (NAME "n")) "=" (BINARY_EXPR (COLUMN_REF (NAME "n")) "+" (LITERAL "1")))) ";")
                      (DELETE_STMT "DELETE" (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "v")))) ";")) "END")) ";"))
        "#]],
    );
}

#[test]
fn compound_statements_in_a_procedure() {
    check_in(
        Dialect::Mysql,
        "DELIMITER //\nCREATE PROCEDURE p(IN n INT)\nBEGIN\n  DECLARE i INT DEFAULT 0;\n  DECLARE c CURSOR FOR SELECT a FROM t;\n  DECLARE CONTINUE HANDLER FOR NOT FOUND SET i = -1;\n  l: LOOP\n    IF i > n THEN LEAVE l; ELSEIF i = 0 THEN SET i = 1; ELSE ITERATE l; END IF;\n  END LOOP l;\n  WHILE i > 0 DO SET i = i - 1; END WHILE;\n  REPEAT SET i = i + 1; UNTIL i > 3 END REPEAT;\n  CASE i WHEN 1 THEN SELECT 1; ELSE BEGIN END; END CASE;\nEND //\nDELIMITER ;\n",
        expect![[r#"
            (SOURCE_FILE
              (DELIMITER_STMT "DELIMITER" "//")
              (CREATE_FUNCTION_STMT "CREATE" "PROCEDURE" (QUALIFIED_NAME (NAME "p")) (PARAM_LIST "(" (PARAM_DEF "IN" (NAME "n") (TYPE (QUALIFIED_NAME (NAME "INT")))) ")") (ROUTINE_BODY (BLOCK "BEGIN" (STATEMENT_LIST
                      (DECLARE_STMT "DECLARE" (NAME "i") (TYPE (QUALIFIED_NAME (NAME "INT"))) "DEFAULT" (LITERAL "0") ";")
                      (DECLARE_STMT "DECLARE" (NAME "c") "CURSOR" "FOR" (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (COLUMN_REF (NAME "a")))) (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t")))))) ";")
                      (DECLARE_STMT "DECLARE" "CONTINUE" "HANDLER" "FOR" "NOT" "FOUND" (SET_STMT "SET" (SET_ASSIGNMENT (QUALIFIED_NAME (NAME "i")) "=" (PREFIX_EXPR "-" (LITERAL "1"))) ";"))
                      (LOOP_STMT (LABEL (NAME "l") ":") "LOOP" (STATEMENT_LIST
                          (IF_STMT "IF" (BINARY_EXPR (COLUMN_REF (NAME "i")) ">" (COLUMN_REF (NAME "n"))) "THEN" (STATEMENT_LIST
                              (LEAVE_STMT "LEAVE" (NAME "l") ";")) (ELSEIF_CLAUSE "ELSEIF" (BINARY_EXPR (COLUMN_REF (NAME "i")) "=" (LITERAL "0")) "THEN" (STATEMENT_LIST
                                (SET_STMT "SET" (SET_ASSIGNMENT (QUALIFIED_NAME (NAME "i")) "=" (LITERAL "1")) ";"))) (ELSE_CLAUSE "ELSE" (STATEMENT_LIST
                                (ITERATE_STMT "ITERATE" (NAME "l") ";"))) "END" "IF" ";")) "END" "LOOP" (NAME "l") ";")
                      (WHILE_STMT "WHILE" (BINARY_EXPR (COLUMN_REF (NAME "i")) ">" (LITERAL "0")) "DO" (STATEMENT_LIST
                          (SET_STMT "SET" (SET_ASSIGNMENT (QUALIFIED_NAME (NAME "i")) "=" (BINARY_EXPR (COLUMN_REF (NAME "i")) "-" (LITERAL "1"))) ";")) "END" "WHILE" ";")
                      (REPEAT_STMT "REPEAT" (STATEMENT_LIST
                          (SET_STMT "SET" (SET_ASSIGNMENT (QUALIFIED_NAME (NAME "i")) "=" (BINARY_EXPR (COLUMN_REF (NAME "i")) "+" (LITERAL "1"))) ";")) "UNTIL" (BINARY_EXPR (COLUMN_REF (NAME "i")) ">" (LITERAL "3")) "END" "REPEAT" ";")
                      (CASE_STMT "CASE" (COLUMN_REF (NAME "i")) (WHEN_CLAUSE "WHEN" (LITERAL "1") "THEN" (STATEMENT_LIST
                            (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ";"))) (ELSE_CLAUSE "ELSE" (STATEMENT_LIST
                            (BLOCK "BEGIN" (STATEMENT_LIST) "END" ";"))) "END" "CASE" ";")) "END")) "//")
              (DELIMITER_STMT "DELIMITER" ";"))
        "#]],
    );
}

#[test]
fn sessions_transactions_and_tools() {
    check(
        "BEGIN; SAVEPOINT a; ROLLBACK TO SAVEPOINT a; RELEASE a; COMMIT;",
        expect![[r#"
        (SOURCE_FILE
          (BEGIN_STMT "BEGIN" ";")
          (SAVEPOINT_STMT "SAVEPOINT" (NAME "a") ";")
          (ROLLBACK_STMT "ROLLBACK" "TO" "SAVEPOINT" (NAME "a") ";")
          (RELEASE_STMT "RELEASE" (NAME "a") ";")
          (COMMIT_STMT "COMMIT" ";"))
    "#]],
    );
    check(
        "START TRANSACTION READ ONLY;",
        expect![[r#"
        (SOURCE_FILE
          (BEGIN_STMT "START" "TRANSACTION" "READ" "ONLY" ";"))
    "#]],
    );
    check(
        "SET search_path TO public, app; SET @a = 1, @@session.sql_mode = 'x'; SET TIME ZONE 'UTC';",
        expect![[r#"
            (SOURCE_FILE
              (SET_STMT "SET" (SET_ASSIGNMENT (QUALIFIED_NAME (NAME "search_path")) "TO" (COLUMN_REF (NAME "public")) "," (COLUMN_REF (NAME "app"))) ";")
              (SET_STMT "SET" (SET_ASSIGNMENT (VARIABLE_REF "@a") "=" (LITERAL "1")) "," (SET_ASSIGNMENT (VARIABLE_REF "@@session.sql_mode") "=" (LITERAL "'x'")) ";")
              (SET_STMT "SET" (SET_ASSIGNMENT "TIME" "ZONE" (LITERAL "'UTC'")) ";"))
        "#]],
    );
    check(
        "SHOW TABLES LIKE 'x%'; USE db;",
        expect![[r#"
        (SOURCE_FILE
          (SHOW_STMT "SHOW" "TABLES" "LIKE" "'x%'" ";")
          (USE_STMT "USE" (QUALIFIED_NAME (NAME "db")) ";"))
    "#]],
    );
    check(
        "EXPLAIN (ANALYZE, VERBOSE) SELECT 1; EXPLAIN QUERY PLAN DELETE FROM t; DESCRIBE t;",
        expect![[r#"
        (SOURCE_FILE
          (EXPLAIN_STMT "EXPLAIN" "(" "ANALYZE" "," "VERBOSE" ")" (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1"))))) ";")
          (EXPLAIN_STMT "EXPLAIN" "QUERY" "PLAN" (DELETE_STMT "DELETE" (FROM_CLAUSE "FROM" (TABLE_REF (QUALIFIED_NAME (NAME "t"))))) ";")
          (EXPLAIN_STMT "DESCRIBE" (QUALIFIED_NAME (NAME "t")) ";"))
    "#]],
    );
    check_in(
        Dialect::Sqlite,
        "PRAGMA main.cache_size = -2000; PRAGMA table_info(t);",
        expect![[r#"
        (SOURCE_FILE
          (PRAGMA_STMT "PRAGMA" (QUALIFIED_NAME (NAME "main") "." (NAME "cache_size")) "=" "-" (LITERAL "2000") ";")
          (PRAGMA_STMT "PRAGMA" (QUALIFIED_NAME (NAME "table_info")) "(" (LITERAL "t") ")" ";"))
    "#]],
    );
    check(
        "GRANT SELECT, UPDATE (a) ON TABLE t, u TO alice, bob WITH GRANT OPTION; REVOKE ALL ON SCHEMA s FROM PUBLIC;",
        expect![[r#"
            (SOURCE_FILE
              (GRANT_STMT "GRANT" "SELECT" "," "UPDATE" "(" "a" ")" "ON" "TABLE" (QUALIFIED_NAME (NAME "t")) "," (QUALIFIED_NAME (NAME "u")) "TO" (ACCOUNT_NAME "alice") "," (ACCOUNT_NAME "bob") "WITH" "GRANT" "OPTION" ";")
              (REVOKE_STMT "REVOKE" "ALL" "ON" "SCHEMA" (QUALIFIED_NAME (NAME "s")) "FROM" (ACCOUNT_NAME "PUBLIC") ";"))
        "#]],
    );
    check(
        "PREPARE q (int) AS SELECT $1; EXECUTE q (1); DEALLOCATE q;",
        expect![[r#"
        (SOURCE_FILE
          (PREPARE_STMT "PREPARE" (NAME "q") "(" (TYPE (QUALIFIED_NAME (NAME "int"))) ")" "AS" (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (PARAMETER "$1"))))) ";")
          (EXECUTE_STMT "EXECUTE" (NAME "q") (ARG_LIST "(" (LITERAL "1") ")") ";")
          (DEALLOCATE_STMT "DEALLOCATE" "q" ";"))
    "#]],
    );
    check_in(
        Dialect::Mysql,
        "PREPARE s FROM 'SELECT ?'; EXECUTE s USING @a;",
        expect![[r#"
        (SOURCE_FILE
          (PREPARE_STMT "PREPARE" (NAME "s") "FROM" (LITERAL "'SELECT ?'") ";")
          (EXECUTE_STMT "EXECUTE" (NAME "s") "USING" (VARIABLE_REF "@a") ";"))
    "#]],
    );
    check(
        "CALL p(1, 'x'); DO $$ BEGIN END $$;",
        expect![[r#"
        (SOURCE_FILE
          (CALL_STMT "CALL" (QUALIFIED_NAME (NAME "p")) (ARG_LIST "(" (LITERAL "1") "," (LITERAL "'x'") ")") ";")
          (DO_STMT "DO" (ROUTINE_BODY "$$ BEGIN END $$") ";"))
    "#]],
    );
    check(
        "LOCK TABLES t WRITE; UNLOCK TABLES; VACUUM ANALYZE t;",
        expect![[r#"
        (SOURCE_FILE
          (LOCK_STMT "LOCK" "TABLES" "t" "WRITE" ";")
          (UNLOCK_STMT "UNLOCK" "TABLES" ";")
          (UTILITY_STMT "VACUUM" "ANALYZE" "t" ";"))
    "#]],
    );
    check_in(
        Dialect::Sqlite,
        "ATTACH DATABASE 'x.db' AS x; DETACH x;",
        expect![[r#"
        (SOURCE_FILE
          (ATTACH_STMT "ATTACH" "DATABASE" (LITERAL "'x.db'") "AS" (NAME "x") ";")
          (DETACH_STMT "DETACH" "x" ";"))
    "#]],
    );
}

#[test]
fn script_commands_around_sql() {
    check_in(
        Dialect::Postgres,
        "\\connect app\nCOPY t (a, b) FROM stdin;\n1\tx\n\\.\nSELECT 1;\n",
        expect![[r#"
            (SOURCE_FILE
              (META_COMMAND_STMT "\\connect app")
              (COPY_STMT "COPY" (QUALIFIED_NAME (NAME "t")) (NAME_LIST "(" (NAME "a") "," (NAME "b") ")") "FROM" "stdin" ";" "1\tx\n\\.")
              (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ";"))
        "#]],
    );
    check_in(
        Dialect::Sqlite,
        ".headers on\nSELECT 1;\n",
        expect![[r#"
        (SOURCE_FILE
          (META_COMMAND_STMT ".headers on")
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ";"))
    "#]],
    );
    check_in(
        Dialect::Mysql,
        "/*!40101 SET NAMES utf8 */;\nSELECT 1;\n",
        expect![[r#"
        (SOURCE_FILE
          (EMPTY_STMT ";")
          (SELECT_STMT (SELECT "SELECT" (SELECT_LIST (SELECT_ITEM (LITERAL "1")))) ";"))
    "#]],
    );
}
