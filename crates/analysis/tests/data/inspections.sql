-- Statements an inspection reports as an error, which says the server rejects them, and statements
-- that look like them and run. `scripts/inspection-corpus.py` runs each case on real servers after
-- the fixture of its dialect and records what each did in `inspections-verified.txt`; `cargo test`
-- holds the inspections to that record.
--
-- `-- expect: <id>` names the inspection a case is about: on each server it must report an error
-- exactly where the server rejects the case. `-- expect: none` says no inspection may report an
-- error. Any inspection that reports an error claims the server rejects the case. `-- only:` keeps
-- a case to some dialects. The fixture has one row in `orgs` and one in `people`, with id 1.

-- fixture: postgres
CREATE TYPE mood AS ENUM ('happy', 'sad');
CREATE TABLE orgs (id INT PRIMARY KEY, title TEXT);
CREATE TABLE people (
    id INT PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    email VARCHAR(100) NOT NULL UNIQUE,
    org INT,
    age INT,
    born DATE,
    feeling mood,
    shout VARCHAR(200) GENERATED ALWAYS AS (upper(name)) STORED,
    serial_no INT GENERATED ALWAYS AS IDENTITY
);
CREATE TABLE notes (id INT PRIMARY KEY, body TEXT NOT NULL DEFAULT '', person INT NOT NULL);
INSERT INTO orgs (id, title) VALUES (1, 'One');
INSERT INTO people (id, name, email) VALUES (1, 'Ann', 'ann@example.com');
-- fixture: mysql
CREATE TABLE orgs (id INT PRIMARY KEY, title TEXT);
CREATE TABLE people (
    id INT PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    email VARCHAR(100) NOT NULL UNIQUE,
    org INT,
    age INT,
    born DATE,
    feeling ENUM('happy', 'sad'),
    shout VARCHAR(200) GENERATED ALWAYS AS (upper(name)) STORED,
    serial_no INT AUTO_INCREMENT UNIQUE
);
CREATE TABLE notes (id INT PRIMARY KEY, body VARCHAR(100) NOT NULL DEFAULT '', person INT NOT NULL);
INSERT INTO orgs (id, title) VALUES (1, 'One');
INSERT INTO people (id, name, email) VALUES (1, 'Ann', 'ann@example.com');
-- fixture: sqlite
CREATE TABLE orgs (id INTEGER PRIMARY KEY, title TEXT);
CREATE TABLE people (
    id INTEGER PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    email VARCHAR(100) NOT NULL UNIQUE,
    org INT,
    age INT,
    born DATE,
    feeling TEXT,
    shout VARCHAR(200) GENERATED ALWAYS AS (upper(name)) STORED,
    serial_no INT
);
CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL DEFAULT '', person INT NOT NULL);
INSERT INTO orgs (id, title) VALUES (1, 'One');
INSERT INTO people (id, name, email) VALUES (1, 'Ann', 'ann@example.com');

-- case: nonaggregated-group-by
-- expect: nonaggregated-column
SELECT name, count(*) FROM people GROUP BY org;
-- case: nonaggregated-primary-key
-- expect: none
SELECT name, count(*) FROM people GROUP BY id;
-- case: nonaggregated-unique-key
-- expect: nonaggregated-column
SELECT name, count(*) FROM people GROUP BY email;
-- case: nonaggregated-without-group-by
-- expect: nonaggregated-column
SELECT name, count(*) FROM people;
-- case: nonaggregated-order-by
-- expect: nonaggregated-column
SELECT org FROM people GROUP BY org ORDER BY age;
-- case: nonaggregated-having
-- expect: nonaggregated-column
SELECT org FROM people GROUP BY org HAVING age > 1;
-- case: nonaggregated-having-selected
-- expect: nonaggregated-column
SELECT org, age FROM people GROUP BY org HAVING age > 1;
-- case: nonaggregated-having-aggregated
-- expect: none
SELECT org FROM people GROUP BY org HAVING max(age) > 1 AND org > 0;
-- case: nonaggregated-fixed-by-where
-- expect: nonaggregated-column
SELECT name, count(*) FROM people WHERE name = 'Ann' GROUP BY org;
-- case: nonaggregated-in-aggregate
-- expect: none
SELECT org, max(name), count(DISTINCT age) FROM people GROUP BY org;
-- case: nonaggregated-by-position-and-alias
-- expect: none
SELECT org AS o, lower(name) AS n, count(*) FROM people GROUP BY 1, lower(name);
-- case: nonaggregated-joined-primary-key
-- expect: none
SELECT o.title, count(*) FROM people p JOIN orgs o ON o.id = p.org GROUP BY o.id;
-- case: nonaggregated-mode-set
-- expect: nonaggregated-column
-- only: mysql mariadb
SET sql_mode = 'ONLY_FULL_GROUP_BY'; SELECT name, count(*) FROM people GROUP BY org;
-- case: nonaggregated-mode-cleared
-- expect: nonaggregated-column
-- only: mysql mariadb
SET sql_mode = ''; SELECT name, count(*) FROM people GROUP BY org;

-- case: insert-more-values
-- expect: insert-column-count
INSERT INTO orgs (id) VALUES (2, 'Two');
-- case: insert-fewer-values
-- expect: insert-column-count
INSERT INTO orgs (id, title) VALUES (2);
-- case: insert-uneven-rows
-- expect: insert-column-count
INSERT INTO orgs (id, title) VALUES (2, 'Two'), (3);
-- case: insert-fewer-without-columns
-- expect: insert-column-count
INSERT INTO orgs VALUES (2);
-- case: insert-more-without-columns
-- expect: insert-column-count
INSERT INTO orgs VALUES (2, 'Two', 3);
-- case: insert-query-wider
-- expect: insert-column-count
INSERT INTO orgs (id) SELECT 2, 'Two';
-- case: insert-matching
-- expect: none
INSERT INTO orgs (id, title) VALUES (2, 'Two'), (3, 'Three');
-- case: union-wider
-- expect: set-operation-column-count
SELECT id FROM orgs UNION SELECT id, title FROM orgs;
-- case: intersect-narrower
-- expect: set-operation-column-count
SELECT id, title FROM orgs INTERSECT SELECT id FROM orgs;

-- case: literal-text-into-integer
-- expect: invalid-literal
INSERT INTO people (id, name, email, age) VALUES (2, 'Bo', 'bo@example.com', 'abc');
-- case: literal-empty-into-integer
-- expect: invalid-literal
INSERT INTO people (id, name, email, age) VALUES (2, 'Bo', 'bo@example.com', '');
-- case: literal-decimal-into-integer
-- expect: invalid-literal
INSERT INTO people (id, name, email, age) VALUES (2, 'Bo', 'bo@example.com', '1.5');
-- case: literal-number-into-integer
-- expect: none
INSERT INTO people (id, name, email, age) VALUES (2, 'Bo', 'bo@example.com', ' 42 ');
-- case: literal-impossible-date
-- expect: invalid-literal
INSERT INTO people (id, name, email, born) VALUES (2, 'Bo', 'bo@example.com', '2024-02-30');
-- case: literal-leap-day
-- expect: none
INSERT INTO people (id, name, email, born) VALUES (2, 'Bo', 'bo@example.com', '2024-02-29');
-- case: literal-text-for-date
-- expect: invalid-literal
INSERT INTO people (id, name, email, born) VALUES (2, 'Bo', 'bo@example.com', 'soon');
-- case: literal-impossible-date-compared
-- expect: invalid-literal
SELECT * FROM people WHERE born = '2024-02-30';
-- case: literal-update
-- expect: invalid-literal
UPDATE people SET age = 'old' WHERE id = 1;
-- case: literal-compared-with-integer
-- expect: invalid-literal
SELECT * FROM people WHERE age = 'abc';
-- case: literal-compared-with-date
-- expect: invalid-literal
SELECT * FROM people WHERE born < 'soon';
-- case: literal-not-strict
-- expect: invalid-literal
-- only: mysql mariadb
SET sql_mode = ''; INSERT INTO people (id, name, email, age) VALUES (2, 'Bo', 'bo@example.com', 'abc');

-- case: enum-value-written
-- expect: unknown-enum-value
INSERT INTO people (id, name, email, feeling) VALUES (2, 'Bo', 'bo@example.com', 'angry');
-- case: enum-value-compared
-- expect: unknown-enum-value
SELECT * FROM people WHERE feeling = 'angry';
-- case: enum-value-known
-- expect: none
INSERT INTO people (id, name, email, feeling) VALUES (2, 'Bo', 'bo@example.com', 'sad');

-- case: null-inserted
-- expect: not-null-violation
INSERT INTO people (id, name, email) VALUES (2, NULL, 'bo@example.com');
-- case: null-updated
-- expect: not-null-violation
UPDATE people SET name = NULL WHERE id = 1;
-- case: null-into-a-key
-- expect: not-null-violation
INSERT INTO orgs (id, title) VALUES (NULL, 'Two');
-- case: null-in-a-later-row
-- expect: not-null-violation
INSERT INTO people (id, name, email) VALUES (2, 'Bo', 'bo@example.com'), (3, NULL, 'cy@example.com');
-- case: null-where-allowed
-- expect: none
INSERT INTO people (id, name, email, age) VALUES (2, 'Bo', 'bo@example.com', NULL);

-- case: generated-inserted
-- expect: generated-column-write
INSERT INTO people (id, name, email, shout) VALUES (2, 'Bo', 'bo@example.com', 'BO');
-- case: generated-updated
-- expect: generated-column-write
UPDATE people SET shout = 'ANN' WHERE id = 1;
-- case: generated-default
-- expect: none
-- only: postgres mysql mariadb
INSERT INTO people (id, name, email, shout) VALUES (2, 'Bo', 'bo@example.com', DEFAULT);
-- case: identity-inserted
-- expect: generated-column-write
-- only: postgres
INSERT INTO people (id, name, email, serial_no) VALUES (2, 'Bo', 'bo@example.com', 7);
-- case: identity-overridden
-- expect: none
-- only: postgres
INSERT INTO people (id, name, email, serial_no) OVERRIDING SYSTEM VALUE VALUES (2, 'Bo', 'bo@example.com', 7);
-- case: generated-left-out
-- expect: none
INSERT INTO people (id, name, email) VALUES (2, 'Bo', 'bo@example.com');

-- case: required-left-out
-- expect: missing-required-column
INSERT INTO people (id, name) VALUES (2, 'Bo');
-- case: required-with-default
-- expect: none
INSERT INTO notes (id, person) VALUES (1, 1);
-- case: required-not-strict
-- expect: missing-required-column
-- only: mysql mariadb
SET sql_mode = ''; INSERT INTO people (id, name) VALUES (2, 'Bo');

-- case: duplicate-alias-joined
-- expect: duplicate-alias
SELECT 1 FROM people p JOIN orgs p ON p.id = 1;
-- case: duplicate-alias-unread
-- expect: duplicate-alias
SELECT 1 FROM people p JOIN orgs p ON true;
-- case: duplicate-table
-- expect: duplicate-alias
SELECT 1 FROM orgs, orgs;
-- case: duplicate-cte
-- expect: duplicate-cte
WITH a AS (SELECT 1 AS x), a AS (SELECT 2 AS x) SELECT x FROM a;
-- case: duplicate-column-of-table
-- expect: duplicate-column
CREATE TABLE twice (a INT, A INT);
-- case: duplicate-column-of-view
-- expect: duplicate-column
CREATE VIEW twice AS SELECT id, name AS id FROM people;
-- case: duplicate-column-of-table-as
-- expect: duplicate-column
CREATE TABLE twice AS SELECT id, name AS id FROM people;
-- case: duplicate-column-of-insert
-- expect: duplicate-column
INSERT INTO orgs (id, id) VALUES (2, 3);
-- case: duplicate-column-renamed
-- expect: none
CREATE VIEW twice AS SELECT id, name AS id2 FROM people;

-- case: limit-in-in
-- expect: limit-in-subquery
SELECT * FROM people WHERE org IN (SELECT id FROM orgs LIMIT 1);
-- case: limit-in-any
-- expect: limit-in-subquery
-- only: postgres mysql mariadb
SELECT * FROM people WHERE org = ANY (SELECT id FROM orgs LIMIT 1);
-- case: limit-in-derived-table
-- expect: none
SELECT * FROM people WHERE org IN (SELECT * FROM (SELECT id FROM orgs LIMIT 1) AS limited);
-- case: limit-in-exists
-- expect: none
SELECT * FROM people WHERE EXISTS (SELECT 1 FROM orgs LIMIT 1);

-- case: unknown-column
-- expect: unresolved-column
SELECT nickname FROM people;
-- case: ambiguous-column
-- expect: ambiguous-column
SELECT id FROM people JOIN orgs ON orgs.id = people.org;
-- case: qualified-column
-- expect: none
SELECT people.id FROM people JOIN orgs ON orgs.id = people.org;

-- case: reserved-word-as-column
-- expect: reserved-word
CREATE TABLE words (rank INT);
