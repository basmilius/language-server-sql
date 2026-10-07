# Inspections

Every diagnostic other than a syntax error comes from an inspection. An inspection has a stable id (the diagnostic's `code`), a default severity and, where an obvious one exists, a quick fix. It reports only what is certain and stays silent where it cannot be: one that needs the schema says nothing without a [snapshot](./snapshot-format.md) or DDL, and one about a dialect says nothing in another.

Severities follow the server. An **error** is something the server rejects; a **warning** is a likely bug; **information** and **hint** are style. A few inspections give a finding its own severity: an error in the dialect that rejects it, a warning where the server lets it pass.

## Settings

The `inspections` setting switches an inspection off or changes its severity, by id. For `unsupported-syntax` and `deprecated-syntax`, the id of a row of the feature table (`backtick-identifiers`, `double-pipe`, ...) works too and goes before the inspection's own setting.

```json
{
    "inspections": {
        "missing-where": "off",
        "null-comparison": "error",
        "unused-alias": false,
        "double-pipe": { "severity": "hint" },
        "deprecated-syntax": { "enabled": true, "severity": "information" }
    }
}
```

A value is `false` or `"off"`, `true` or `"on"`, a severity (`"error"`, `"warning"`, `"information"`, `"hint"`), or an object with `enabled` and `severity`. An unknown id or value is logged and left out. [Configuration](./configuration.md) says where settings come from.

## Suppressing

A comment silences ids where it stands:

```sql
-- sql-suppress missing-where: the table is a queue
DELETE FROM jobs;

UPDATE jobs SET done = 1; -- sql-suppress missing-where

-- sql-suppress-file unused-alias, double-pipe
```

- `sql-suppress <id> ...` silences the ids for one statement: the statement the comment stands in, the one whose last line it ends, or else the next one.
- `sql-suppress-file <id> ...` anywhere in a script silences the ids in all of it.
- An id is an inspection's or a row's of the feature table; `all` stands for every one. Ids are separated by spaces or commas, and the list ends at the first word that is not an id, so a reason may follow it.
- `--`, `#` and `/* */` comments all work.

Every finding has two quick fixes that write such a comment: for its statement and for the file.

## Fixing all

For the inspections marked _fix all_ below, whose fix is the only one and keeps what the statement means, the quick fixes come with a source action (`source.fixAll.sql`) that applies the fix to every finding of the inspection in the document. A client that asks for `source.fixAll` by name, as an editor does on save, gets one action with the fixes of every such inspection.

## The inspections

| Id | Severity | Dialects | Fix |
| --- | --- | --- | --- |
| `unresolved-table` | error | all, with a schema | a near miss |
| `unresolved-column` | error | all, with a schema | a near miss |
| `unresolved-function` | error | all, with a snapshot and a dialect | a near miss |
| `ambiguous-column` | error | all, with a schema | qualify it |
| `unsupported-syntax` | error | all | a rewrite where one exists, fix all |
| `deprecated-syntax` | warning | MySQL | a rewrite where one exists, fix all |
| `reserved-word` | error | all | quote it, fix all |
| `missing-where` | warning | all | |
| `null-comparison` | warning | all | `IS NULL`, fix all |
| `constant-condition` | information | all | |
| `not-in-nullable` | information | all, with a schema | leave out the NULLs |
| `implicit-cross-join` | warning | all | `CROSS JOIN`, fix all |
| `like-without-wildcard` | information | all but SQLite | `=`, fix all |
| `nonaggregated-column` | error | PostgreSQL; MySQL and MariaDB with `ONLY_FULL_GROUP_BY` | add it to `GROUP BY` |
| `distinct-with-group-by` | information | all | remove `DISTINCT`, fix all |
| `count-not-null-column` | hint | all, with a schema | `COUNT(*)`, fix all |
| `insert-column-count` | error | all | |
| `set-operation-column-count` | error | all | |
| `invalid-literal` | error or warning | all, with a schema | |
| `unknown-enum-value` | error or warning | MySQL, MariaDB, PostgreSQL, with a schema | a near miss |
| `not-null-violation` | error or warning | all, with a schema | |
| `generated-column-write` | error | all, with a schema | leave the column out, fix all |
| `missing-required-column` | error or warning | all, with a schema | |
| `unused-cte` | warning | all | remove it, fix all |
| `unused-alias` | hint | all | remove it, fix all |
| `duplicate-alias` | error | all | |
| `duplicate-cte` | error | all | |
| `duplicate-column` | error | all; a warning for a SQLite view | |
| `pipes-as-or` | warning | MySQL and MariaDB without `PIPES_AS_CONCAT` | `CONCAT()`, fix all |
| `double-quoted-string` | hint | MySQL and MariaDB without `ANSI_QUOTES` | single quotes, fix all |
| `limit-in-subquery` | error | MySQL, MariaDB | wrap it in a derived table, fix all |
| `order-by-in-subquery` | information | MySQL, MariaDB | remove `ORDER BY`, fix all |

"With a schema" means a snapshot or DDL in the script or the workspace defines the tables involved, and with all their columns known.

### Names

**`unresolved-table`**, **`unresolved-column`**, **`unresolved-function`**: a name the schema does not have, only where the schema is known: a table only in a schema a snapshot covers or a system schema, a column only when every table in scope has all its columns known, a function only with a snapshot and a dialect. Nothing is reported in `DROP`, nor for unqualified names in routine bodies, which can be variables. The fix is the known name nearest to the one written (a slip of at most a third of its letters).

```sql
SELECT emial FROM users;          -- Unknown column 'emial'; fix: email
```

**`ambiguous-column`**: an unqualified column that more than one table in scope has; the related information points at each table. The fixes qualify it with each.

```sql
SELECT id FROM users u JOIN orgs o ON o.id = u.org_id;
```

### Syntax

**`unsupported-syntax`**: syntax the dialect never has, or that its version does not have yet, by the rows of the feature table; the row is in the diagnostic's `data.feature`. Rewrites: `` `name` `` to `"name"` in PostgreSQL and SQLite, `a::int` to `CAST(a AS int)`, `==` to `=`, `IS [NOT] DISTINCT FROM` to `<=>` in MySQL and MariaDB, `LIMIT 5, 10` to `LIMIT 10 OFFSET 5`, `VALUE` to `VALUES`, an alias written as a string to a quoted name, `CAST(a AS INTEGER)` to `SIGNED`, `VARCHAR(n)` to `CHAR(n)` and `TEXT` to `CHAR` in MySQL, `START TRANSACTION` to `BEGIN TRANSACTION` in SQLite, `END` to `COMMIT`, `AUTOINCREMENT` to `AUTO_INCREMENT`.

**`deprecated-syntax`**: syntax the version deprecates, with the deprecated tag. Rewrites: `&&` to `AND`, `||` to `OR`, `BINARY a` to `CAST(a AS BINARY)`, `VALUES(c)` in `ON DUPLICATE KEY UPDATE` to a row alias (`VALUES (...) AS new ... new.c`), `ZEROFILL` and a display width removed, and `SQL_CALC_FOUND_ROWS` removed, with a `SELECT FOUND_ROWS()` that follows it rewritten as `SELECT COUNT(*)` of the same rows.

```sql
SELECT SQL_CALC_FOUND_ROWS * FROM t WHERE a > 1 LIMIT 10;
SELECT FOUND_ROWS();
-- fix:
SELECT * FROM t WHERE a > 1 LIMIT 10;
SELECT COUNT(*) FROM t WHERE a > 1;
```

**`reserved-word`**: a word the dialect reserves, unquoted where it names a column, a table, an alias, a constraint or a parameter. The fix quotes it the dialect's way (in PostgreSQL in lower case, as the unquoted word folds).

```sql
CREATE TABLE k (key INT);         -- MySQL; fix: `key`
```

### Conditions

**`missing-where`**: a `DELETE` or `UPDATE` without `WHERE`, which changes every row. Not reported with `LIMIT` or a join condition (`DELETE t FROM t JOIN u ON ...`).

**`null-comparison`**: `a = NULL`, `a <> NULL` or `a != NULL`, which is never true, and `CASE a WHEN NULL`, which never matches. The fix writes `IS NULL` or `IS NOT NULL`.

**`constant-condition`**: a comparison of two numbers, or of a column with itself, in `WHERE`, `ON`, `HAVING` or a `WHEN`: `2 = 2` always holds, `2 = 0` never does, and `a = a` holds wherever `a` is not NULL. A comparison in the select list is a value and is left alone, and so are `1 = 1`, `0 = 1` and the other comparisons of `0` and `1` with `=` or `<>`, which query builders write for a condition with nothing in it.

**`not-in-nullable`**: `x NOT IN (SELECT c ...)` where `c` may be NULL (a nullable column, or one on the outer side of a join): one NULL makes the condition unknown for every row, so nothing matches. The fix adds `c IS NOT NULL` to the subquery.

**`implicit-cross-join`**: tables joined by a comma that no condition of `WHERE` links, which pairs every row with every row. Only plain tables are judged, and only when every column of the condition is known. The fix writes `CROSS JOIN`, which says it is meant.

```sql
SELECT * FROM users, orgs;        -- fix: users CROSS JOIN orgs
```

**`like-without-wildcard`**: `LIKE` with a pattern without `%` or `_`, which compares like `=`. Not in SQLite, where `LIKE` ignores the case of letters and `=` does not.

### Grouping

**`nonaggregated-column`**: a column of a grouped query (by `GROUP BY`, or by an aggregate without it) that the select list, `HAVING` or `ORDER BY` names outside an aggregate and that is not grouped. PostgreSQL rejects it, unless the primary key of its table is grouped. MySQL rejects it under `ONLY_FULL_GROUP_BY`, on by default, unless a primary key or a unique key of NOT NULL columns is grouped or `WHERE` or `ON` fixes the column with an equality; the select list and `ORDER BY` are checked there. MariaDB has `ONLY_FULL_GROUP_BY` off by default, and SQLite takes the column from some row of the group. `sql_mode` comes from the script's `SET sql_mode`, the snapshot's `sqlMode` or the default. The fix adds the column to `GROUP BY`.

```sql
SELECT name, count(*) FROM people GROUP BY org;
```

**`distinct-with-group-by`**: `DISTINCT` on a query whose select list holds every grouped expression, so the groups are distinct already.

**`count-not-null-column`**: `COUNT(c)` where `c` is NOT NULL and not on the outer side of a join, which counts every row, as `COUNT(*)` does.

### Writes

**`insert-column-count`**: an `INSERT` whose rows or query give more or fewer values than its column list names. Without a column list the values fill the table's columns: MySQL, MariaDB and SQLite want one for each, PostgreSQL takes fewer and rejects more.

**`set-operation-column-count`**: the queries of `UNION`, `INTERSECT` or `EXCEPT` with different numbers of columns. A query with `*` is not counted.

**`invalid-literal`**: a string compared with or written to a numeric or date column that its type cannot read: `'abc'` or `''` for a number, `'1.5'` for an integer in PostgreSQL, a date without digits or with a month or day that cannot be (`'2024-02-30'`). PostgreSQL's special words (`'now'`, `'today'`, `'infinity'`) are read as it reads them. An error in PostgreSQL, and in MySQL and MariaDB where a strict mode is on and the value is written; a warning where it is compared there, and in SQLite, which stores anything.

```sql
INSERT INTO people (age) VALUES ('ten');
```

**`unknown-enum-value`**: a string that is not a value of a MySQL `enum(...)` or a PostgreSQL enum type, compared or written. Severities as for `invalid-literal`. The fix is the nearest value.

**`not-null-violation`**: `NULL` written to a NOT NULL column by `INSERT` or `UPDATE`. Not reported for a column that numbers itself (auto-increment, SQLite's `INTEGER PRIMARY KEY`), for a table with a `BEFORE` trigger that may fill it in, or with `INSERT IGNORE` or `OR IGNORE`. A warning in MySQL and MariaDB without a strict mode, except for a single row, which they reject either way.

**`generated-column-write`**: a value other than `DEFAULT` written to a generated column, or to an identity column `GENERATED ALWAYS` without `OVERRIDING SYSTEM VALUE`. The fix leaves the column and its values out of the `INSERT`.

**`missing-required-column`**: an `INSERT` with a column list, `SET` or `DEFAULT VALUES` that leaves out a NOT NULL column without a default that is neither generated nor numbers itself. A warning in MySQL and MariaDB without a strict mode.

### Declarations

**`unused-cte`**: a common table expression nothing reads (a read inside itself does not count). One that changes data (`WITH d AS (DELETE ...)`) runs anyway and is not reported. The fix removes it, or the whole `WITH`.

**`unused-alias`**: a table alias nothing qualifies a name with. Not reported for a table the statement reads twice, where the alias tells them apart, nor for an alias with column names or of a subquery, which MySQL and MariaDB require. The fix removes it.

**`duplicate-alias`**: two tables of one `FROM` under the same name, as alias or as table; the related information points at the first.

**`duplicate-cte`**: two common table expressions of one `WITH` with the same name.

**`duplicate-column`**: two columns with one name in `CREATE TABLE`, in the select list of `CREATE VIEW` or `CREATE TABLE ... AS` without a column list, or in the column list of `INSERT`. SQLite numbers the later column of a view instead of rejecting it, so there it is a warning.

### MySQL and MariaDB

**`pipes-as-or`**: `||` where an operand is text (a string, a text column, a string function), which MySQL and MariaDB read as a logical `OR` unless `PIPES_AS_CONCAT` is on. It takes the place of the deprecation of `||`. The fix joins the operands with `CONCAT()`.

```sql
SELECT first_name || ' ' || last_name FROM people;   -- fix: CONCAT(first_name, ' ', last_name)
```

**`double-quoted-string`**: a string in double quotes, which is a name where `ANSI_QUOTES` is on and in standard SQL. A warning when the text is the name of a column in scope, which was probably meant; the fixes then read the column or write single quotes.

**`limit-in-subquery`**: `LIMIT` in a subquery of `IN`, `ANY`, `SOME` or `ALL`, which MySQL and MariaDB reject. The fix wraps the subquery in a derived table, which they accept.

```sql
SELECT * FROM t WHERE a IN (SELECT a FROM u LIMIT 3);
-- fix:
SELECT * FROM t WHERE a IN (SELECT * FROM (SELECT a FROM u LIMIT 3) AS limited);
```

**`order-by-in-subquery`**: `ORDER BY` in a subquery without `LIMIT`, which does not order the result and which the server may drop. Not reported where MySQL passes the order on: a derived table that is the only table of a query that neither groups, aggregates, removes duplicates nor orders by itself.
