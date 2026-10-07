# Measurements

What the server was measured to do, and when. [NATIVE.md](./NATIVE.md) says how the server works and why; the numbers live here, so that a new measurement replaces an old one without touching the design notes. Each section names its date.

All measurements are release builds on an Apple M4 Max laptop with 16 cores and 64 GB, unless a section says otherwise.

## The tools

```sh
cargo bench -p sql-syntax                       # lexing, parsing and the feature table
cargo bench -p sql-analysis                     # a snapshot of 5,000 tables: reading, completion, hover, unknown names
python3 scripts/catalog.py [--keep]             # the built-in catalogs from the servers
python3 scripts/dialect-corpus.py [--keep]      # the corpus on real servers
python3 scripts/reserved-words.py [--keep]      # the reserved words of each dialect
```

## Lexing and parsing

2026-10-07, `cargo bench -p sql-syntax`. The large script is 1,000 copies of a unit with a `CREATE TABLE` of seven columns and constraints, a `CREATE INDEX`, an `INSERT` of three rows, a query with a common table expression, joins, a window function and `CASE`, an `UPDATE` with a subquery, a `DELETE` and an `ALTER TABLE`: 1.67 MB, 7,000 statements. The typical script is 25 copies, 41 KB.

| | |
| --- | --- |
| Lexing (PostgreSQL) | 11.8 ms, 135 MiB/s |
| Parsing without a dialect | 25.8 ms, 62 MiB/s |
| Parsing as MySQL | 26.1 ms, 61 MiB/s |
| Parsing as PostgreSQL | 25.2 ms, 63 MiB/s |
| The feature table and reserved words, MySQL | 25.8 ms, 62 MiB/s |
| A typical script, parsed and checked as PostgreSQL | 1.29 ms |

A change to a document parses the whole script again; for a file a person edits that is cheaper than anything a patch would save. Running only the rows of the table that can report for the target, and building token elements only where a node holds a kind an active row wants, took the feature table from 57 ms to 26 ms on the large script.

The release binary is 4.5 MB on macOS (2026-10-07), of which about 2 MB are the built-in catalogs it embeds.

## A large schema

2026-10-07, `cargo bench -p sql-analysis`. The snapshot has 5,000 tables of 20 columns, each with a comment on every column, a primary key and a foreign key to the table before it: 11.0 MB of JSON.

| | |
| --- | --- |
| Reading the snapshot and indexing it | 22.6 ms |
| Completion after `FROM`, every table offered and cut at 500 | 3.4 ms |
| Completion after `FROM` with a typed prefix | 3.6 ms |
| Completion after `JOIN`, with the conditions of foreign keys | 4.0 ms |
| Completion of the columns of two joined tables | 0.41 ms |
| Unknown names of 100 queries with joins and a subquery | 16.7 ms, 0.17 ms a query |
| Hover on a column | 4.6 µs |

A snapshot is read once and shared by every document that names it; completion walks the tables of the search path and builds an item for each before it filters and ranks them, which stays well within what a keystroke allows at this size.

## The built-in catalogs

2026-10-07, `python3 scripts/catalog.py`, with 426 hand-written description lines: PostgreSQL 18 has 734 functions a person calls (1,225 overloads), 99 types, 213 tables and views in `pg_catalog` and `information_schema` with 2,119 columns, and 399 settings; MySQL 8.0 and 8.4 together 360 functions, 54 types, 332 system tables and views with 3,598 columns and 662 variables; MariaDB 11.0 to 11.8 451 functions, 57 types, 299 system tables and views with 3,400 columns and 719 variables; SQLite 3.48 to 3.53 154 functions (the shell's own extensions left out), 28 type names, 6 schema tables and 66 pragmas.

## The corpus on real servers

2026-10-07, `python3 scripts/dialect-corpus.py`, on SQLite 3.50.4 (Python's), MySQL 8.0.46 and 8.4.11, MariaDB 11.8.9 and PostgreSQL 18.6 in Docker. 398 statements: the example of each of the 231 rows of the feature table and 167 cases of `crates/syntax/tests/data/dialects.sql`, 1,990 verdicts in all.

| | |
| --- | --- |
| The parser and the server agree | 1,751 |
| The server failed on something other than syntax, which fits either verdict | 229 |
| Deliberate differences, listed in `dialects-known.txt` | 10 |
| Contradictions | 0 |

The first run of the corpus found 206 differences; most came from the runner itself (PostgreSQL's errors carry a line only with `-f -`, and a semantic error says nothing about syntax), and the rest moved rows of the table and added new ones: `WITHIN GROUP` works in MariaDB only with `OVER`, `ROLLUP (...)` runs on MySQL 8.4.11 and not on 8.0.46, SQLite has no `ANY`, no `DEFAULT` as a value, no parenthesized operands of `UNION`, no `NULLS LAST` in an index and no column aliases for a subquery, MySQL takes only literals and parameters in `LIMIT`, `CAST(... AS INTEGER)` fails on MySQL and not on MariaDB, and `CAST(... AS TEXT)` fails on both.

## Reserved words

2026-10-07, `python3 scripts/reserved-words.py`, on the same servers: SQLite reserves 58 words, MySQL 260 in 8.0 and 8.4 alike (8.0.46 also `MASTER_BIND` and `MASTER_SSL_VERIFY_SERVER_CERT`, 8.4.11 also `QUALIFY` and `TABLESAMPLE`), MariaDB 247 and PostgreSQL 101 (78 reserved, 23 only allowed as the name of a function or type).
