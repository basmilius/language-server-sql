-- Statements whose acceptance per dialect `scripts/dialect-corpus.py` records from real servers in
-- `dialects-verified.txt`, next to the example of every row of the feature table. Every case is one
-- statement against the fixture tables of that script: t, u, s, a and b.

-- case: select-basic
SELECT a, b AS c, 1 + 2 * 3 FROM t WHERE a > 1 AND b IS NOT NULL ORDER BY a DESC LIMIT 10;
-- case: select-without-from
SELECT 1, 'x', NULL, TRUE;
-- case: select-distinct-group-having
SELECT DISTINCT a, count(*) AS n FROM t GROUP BY a HAVING count(*) > 1;
-- case: select-qualified-wildcard
SELECT t.*, u.a FROM t, u WHERE t.id = u.id;
-- case: select-inner-left-join
SELECT * FROM t JOIN u ON t.id = u.id LEFT JOIN s ON s.id = t.id;
-- case: select-left-outer-join
SELECT * FROM t LEFT OUTER JOIN u ON t.id = u.id;
-- case: select-right-join
SELECT * FROM t RIGHT JOIN u ON t.id = u.id;
-- case: select-cross-natural-join
SELECT * FROM t CROSS JOIN u NATURAL JOIN s;
-- case: select-join-using
SELECT * FROM t JOIN u USING (id);
-- case: select-parenthesized-join
SELECT * FROM t JOIN (u JOIN s ON u.id = s.id) ON t.id = u.id;
-- case: select-derived-table
SELECT x.a FROM (SELECT a FROM t) AS x;
-- case: select-derived-table-implicit-alias
SELECT x.a FROM (SELECT a FROM t) x;
-- case: select-scalar-subquery
SELECT (SELECT max(a) FROM u) AS m FROM t;
-- case: select-in-subquery
SELECT * FROM t WHERE a IN (SELECT a FROM u) AND b NOT IN (1, 2);
-- case: select-exists
SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.id = t.id);
-- case: select-any-subquery
SELECT * FROM t WHERE a > ANY (SELECT a FROM u);
-- case: select-between-like
SELECT * FROM t WHERE a BETWEEN 1 AND 10 AND name LIKE 'x%' ESCAPE '!';
-- case: select-case
SELECT CASE WHEN a > 1 THEN 'big' WHEN a = 1 THEN 'one' ELSE 'small' END, CASE b WHEN 1 THEN 'x' END FROM t;
-- case: select-cast
SELECT CAST(a AS CHAR(10)), CAST(b AS DECIMAL(10, 2)) FROM t;
-- case: select-coalesce-nullif
SELECT coalesce(a, 0), nullif(b, 0), abs(-a), lower(name) FROM t;
-- case: select-string-concat-function
SELECT concat(name, 'x') FROM t;
-- case: select-union-all
SELECT a FROM t UNION ALL SELECT a FROM u;
-- case: select-union-order-limit
SELECT a FROM t UNION SELECT a FROM u ORDER BY a LIMIT 5;
-- case: select-parenthesized-union
(SELECT a FROM t) UNION (SELECT a FROM u);
-- case: select-cte
WITH x AS (SELECT a FROM t) SELECT * FROM x;
-- case: select-cte-columns
WITH x (v) AS (SELECT a FROM t) SELECT v FROM x;
-- case: select-recursive-cte
WITH RECURSIVE r (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT n FROM r;
-- case: select-window-function
SELECT a, row_number() OVER (PARTITION BY b ORDER BY a) FROM t;
-- case: select-window-frame
SELECT sum(a) OVER (ORDER BY a ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t;
-- case: select-window-range-unbounded
SELECT sum(a) OVER (ORDER BY a RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM t;
-- case: select-named-window
SELECT a, rank() OVER w FROM t WINDOW w AS (ORDER BY a);
-- case: select-limit-offset
SELECT * FROM t LIMIT 10 OFFSET 5;
-- case: select-count-distinct
SELECT count(DISTINCT a) FROM t;
-- case: select-row-comparison
SELECT * FROM t WHERE (a, b) = (1, 2);
-- case: select-is-null-not-null
SELECT * FROM t WHERE a IS NULL OR b IS NOT NULL;
-- case: select-not-equal
SELECT * FROM t WHERE a <> 1 AND b != 2;
-- case: select-modulo
SELECT a % 2, a / 2, -a FROM t;
-- case: select-hex-literal
SELECT X'1F';
-- case: select-quoted-identifier
SELECT "a" FROM t;
-- case: select-escaped-quote
SELECT 'it''s';
-- case: select-current-timestamp
SELECT CURRENT_TIMESTAMP, CURRENT_DATE;
-- case: select-reserved-alias-quoted
SELECT a AS "order" FROM t;
-- case: select-keyword-column
SELECT name, date, value FROM a;
-- case: select-implicit-keyword-alias
SELECT count(*) count FROM t;
-- case: select-group-by-position
SELECT a, count(*) FROM t GROUP BY 1 ORDER BY 2;
-- case: select-missing-from-table
SELECT * FROM;
-- case: select-trailing-comma
SELECT a, FROM t;
-- case: select-unbalanced
SELECT (a FROM t;
-- case: select-double-where
SELECT * FROM t WHERE a = 1 WHERE b = 2;
-- case: insert-values
INSERT INTO t (a, b) VALUES (1, 2), (3, 4);
-- case: insert-select
INSERT INTO t (a, b) SELECT a, id FROM u;
-- case: insert-default
INSERT INTO t (a, b) VALUES (1, DEFAULT);
-- case: insert-without-columns
INSERT INTO u VALUES (1, 2, 3);
-- case: insert-or-ignore
INSERT OR IGNORE INTO t (id, a) VALUES (1, 2);
-- case: update-basic
UPDATE t SET a = 1, b = b + 1 WHERE id = 3;
-- case: update-subquery
UPDATE t SET a = (SELECT max(a) FROM u) WHERE id IN (SELECT id FROM u);
-- case: delete-basic
DELETE FROM t WHERE a = 1;
-- case: delete-all
DELETE FROM t;
-- case: create-table-basic
CREATE TABLE n1 (id INTEGER PRIMARY KEY, name VARCHAR(100) NOT NULL, price DECIMAL(10, 2) DEFAULT 0, created TIMESTAMP);
-- case: create-table-constraints
CREATE TABLE n2 (id INT NOT NULL, other INT, CONSTRAINT pk PRIMARY KEY (id), CONSTRAINT uq UNIQUE (other), CONSTRAINT ck CHECK (other > 0), CONSTRAINT fk FOREIGN KEY (other) REFERENCES t (id) ON DELETE CASCADE);
-- case: create-table-if-not-exists
CREATE TABLE IF NOT EXISTS n3 (id INT);
-- case: create-table-as
CREATE TABLE n4 AS SELECT a, b FROM t;
-- case: create-temporary-table
CREATE TEMPORARY TABLE n5 (id INT);
-- case: create-table-references
CREATE TABLE n6 (id INT PRIMARY KEY, tid INT REFERENCES t (id));
-- case: create-table-generated-always
CREATE TABLE n7 (a INT, b INT GENERATED ALWAYS AS (a * 2) STORED);
-- case: create-table-unique-column
CREATE TABLE n8 (a INT UNIQUE, b INT NOT NULL DEFAULT 1);
-- case: create-table-check-column
CREATE TABLE n9 (a INT CHECK (a > 0));
-- case: create-table-missing-type-comma
CREATE TABLE n10 (a INT b INT);
-- case: create-table-timestamp-default
CREATE TABLE n11 (a TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
-- case: create-table-varchar-text
CREATE TABLE n12 (a VARCHAR(10), b TEXT, c CHAR(2), d DOUBLE PRECISION, e REAL, f BOOLEAN, g DATE);
-- case: create-index
CREATE INDEX i1 ON t (a);
-- case: create-unique-index-multi
CREATE UNIQUE INDEX i2 ON t (a, b DESC);
-- case: create-view
CREATE VIEW v1 AS SELECT a FROM t;
-- case: create-view-columns
CREATE VIEW v2 (x) AS SELECT a FROM t;
-- case: drop-table
DROP TABLE u;
-- case: drop-table-if-exists
DROP TABLE IF EXISTS nothing_here;
-- case: drop-view
DROP VIEW IF EXISTS v9;
-- case: alter-add-column
ALTER TABLE t ADD COLUMN z INT;
-- case: alter-add-column-without-keyword
ALTER TABLE t ADD z INT;
-- case: alter-drop-column
ALTER TABLE t DROP COLUMN c;
-- case: alter-rename-column
ALTER TABLE t RENAME COLUMN c TO c2;
-- case: alter-rename-table
ALTER TABLE u RENAME TO u2;
-- case: begin-commit
BEGIN;
-- case: commit
COMMIT;
-- case: rollback
ROLLBACK;
-- case: savepoint
SAVEPOINT sp1;
-- case: explain-select
EXPLAIN SELECT * FROM t;
-- case: select-into-outfile
SELECT a INTO OUTFILE '/tmp/x' FROM t;
-- case: select-json-function
SELECT json_extract(j, '$.a') FROM t;
-- case: select-interval-plus
SELECT ts + INTERVAL '1' DAY FROM t;
-- case: select-substring-from-for
SELECT substring(name FROM 1 FOR 2) FROM t;
-- case: select-extract
SELECT extract(YEAR FROM ts) FROM t;
-- case: select-trim-leading
SELECT trim(LEADING 'x' FROM name) FROM t;
-- case: select-position-in
SELECT position('a' IN name) FROM t;
-- case: select-greatest-least
SELECT greatest(a, b), least(a, b) FROM t;
-- case: select-if-function
SELECT if(a > 1, 'x', 'y') FROM t;
-- case: select-iif-function
SELECT iif(a > 1, 'x', 'y') FROM t;
-- case: select-cast-signed
SELECT CAST(a AS SIGNED) FROM t;
-- case: select-cast-integer
SELECT CAST(a AS INTEGER) FROM t;
-- case: select-cast-text
SELECT CAST(a AS TEXT) FROM t;
-- case: select-utf8mb4-introducer
SELECT _utf8mb4'x';
-- case: select-collate
SELECT name COLLATE "C" FROM t;
-- case: select-backslash-string
SELECT 'a\'b';
-- case: select-double-quoted-string
SELECT "hello world";
-- case: select-hash-comment
SELECT 1 # comment
;
-- case: select-dash-comment-without-space
SELECT 1 --1
;
-- case: select-nested-comment
SELECT 1 /* a /* b */ c */;
-- case: select-bracket-identifier
SELECT [a] FROM t;
-- case: select-dollar-identifier
SELECT a$b FROM t;
-- case: select-numeric-underscore
SELECT 1_000;
-- case: select-exponent
SELECT 1e3, 1.5E-2, .5;
-- case: insert-on-conflict-update
INSERT INTO u (id, a) VALUES (1, 2) ON CONFLICT (id) DO UPDATE SET a = excluded.a;
-- case: insert-returning-star
INSERT INTO t (a) VALUES (1) RETURNING *;
-- case: update-limit
UPDATE t SET a = 1 LIMIT 1;
-- case: delete-limit
DELETE FROM t LIMIT 1;
-- case: create-table-auto-increment-primary
CREATE TABLE n13 (id INT AUTO_INCREMENT PRIMARY KEY, name TEXT);
-- case: create-table-serial
CREATE TABLE n14 (id SERIAL PRIMARY KEY);
-- case: create-table-without-types
CREATE TABLE n15 (a, b);
-- case: create-table-json-column
CREATE TABLE n16 (data JSON);
-- case: create-table-key-column-name
CREATE TABLE n17 (key TEXT, value TEXT);
-- case: create-table-order-column-name
CREATE TABLE n18 (order INT);
-- case: alter-table-modify
ALTER TABLE t MODIFY COLUMN b BIGINT;
-- case: show-tables
SHOW TABLES;
-- case: show-create-table
SHOW CREATE TABLE t;
-- case: set-names
SET NAMES utf8mb4;
-- case: set-variable
SET @x = 1;
-- case: set-session-variable
SET SESSION sql_mode = '';
-- case: set-local-setting
SET LOCAL statement_timeout = 0;
-- case: set-time-zone
SET TIME ZONE 'UTC';
-- case: pragma-function-form
PRAGMA table_info(t);
-- case: values-two-rows
VALUES (1, 2), (3, 4);
-- case: select-from-values
SELECT * FROM (VALUES (1), (2)) AS v (x);
-- case: select-lateral-function
SELECT * FROM t, LATERAL generate_series(1, t.a) AS g;
-- case: select-json-each
SELECT * FROM json_each('[1,2]');
-- case: select-sum-over-empty
SELECT sum(a) OVER () FROM t;
-- case: select-for-update-nowait
SELECT * FROM t FOR UPDATE NOWAIT;
-- case: create-index-desc-nulls
CREATE INDEX i3 ON t (a DESC NULLS LAST);
-- case: create-index-expression
CREATE INDEX i4 ON t ((a + b));
-- case: create-index-using-btree
CREATE INDEX i5 ON t USING btree (a);
-- case: drop-index-if-exists
DROP INDEX IF EXISTS i9;
-- case: create-trigger-sqlite
CREATE TRIGGER tr1 AFTER INSERT ON t BEGIN UPDATE u SET a = a + 1; END;
-- case: create-trigger-mysql
CREATE TRIGGER tr2 BEFORE INSERT ON t FOR EACH ROW SET NEW.a = 1;
-- case: create-function-sql
CREATE FUNCTION f1 (x INT) RETURNS INT DETERMINISTIC RETURN x * 2;
-- case: create-function-plpgsql
CREATE FUNCTION f2 (x int) RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN x * 2; END $$;
-- case: create-procedure-empty
CREATE PROCEDURE p1 () BEGIN END;
-- case: call-procedure
CALL p1();
-- case: select-string-agg
SELECT string_agg(name, ',') FROM t;
-- case: select-group-concat
SELECT group_concat(name) FROM t;
-- case: select-array-agg
SELECT array_agg(a) FROM t;
-- case: select-regexp-like-function
SELECT regexp_like(name, 'x') FROM t;
-- case: select-distinct-from-is-not
SELECT * FROM t WHERE a IS NOT b;
-- case: select-limit-expression
SELECT * FROM t LIMIT 1 + 1;
-- case: select-order-by-collate
SELECT * FROM t ORDER BY name COLLATE "C";
-- case: select-fetch-first-only
SELECT * FROM t ORDER BY a FETCH FIRST 1 ROW ONLY;
-- case: select-where-subquery-limit
SELECT * FROM t WHERE a = (SELECT a FROM u ORDER BY a LIMIT 1);
-- case: select-tuple-in
SELECT * FROM t WHERE (a, b) IN ((1, 2), (3, 4));
-- case: select-unicode-identifier
SELECT a AS "naïve" FROM t;
-- case: select-derived-column-aliases
SELECT * FROM (SELECT 1, 2) AS d (x, y);
-- case: select-cast-varchar
SELECT CAST(a AS VARCHAR(10)) FROM t;
-- case: select-cast-boolean
SELECT CAST(a AS BOOLEAN) FROM t;
-- case: select-cast-datetime
SELECT CAST(ts AS DATETIME) FROM t;
-- case: select-group-by-grouping-sets
SELECT a, b, count(*) FROM t GROUP BY GROUPING SETS ((a), (b));
-- case: insert-values-row
INSERT INTO t (a, b) VALUES ROW(1, 2), ROW(3, 4);
-- case: select-within-group-over
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY a) OVER (PARTITION BY b) FROM t;
-- case: create-index-using-after-columns
CREATE INDEX i6 ON t (a) USING BTREE;
-- case: show-variables-like
SHOW VARIABLES LIKE 'max%';
-- case: set-names-string
SET NAMES 'utf8';
-- case: show-transaction-isolation
SHOW TRANSACTION ISOLATION LEVEL;
