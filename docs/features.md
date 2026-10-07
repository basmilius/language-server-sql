# Features

## Diagnostics

Every diagnostic has `source: "sql"` and one of these codes:

| Code                 | What it reports                                                                                                                |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `syntax`             | What the parser cannot read: a missing token (`')' expected`), an unexpected one, an unknown statement, an unterminated string |
| `reserved-word`      | A word the dialect reserves where it names a column, a table or an alias without quotes                                        |
| a feature id         | Syntax the dialect never has (`MERGE is not supported by MySQL`), not yet has at the version (`INTERSECT and EXCEPT are only available since MySQL 8.0.31`), or deprecates (a warning with the deprecated tag) |
| `unresolved-table`   | A table, view or qualifier that is not known: `Unknown table 'orders'`, `Unknown table or alias 'x'` |
| `unresolved-column`  | A column no table in scope has: `Unknown column 'emial'`, `Unknown column 'nope' in 'u'` |
| `unresolved-function`| A function that is neither built in at any version of the dialect nor in the snapshot |
| `ambiguous-column`   | A column more than one table in scope has, without a qualifier: `Column 'id' is ambiguous: 'u' and 'o' have it` |

The feature ids are the rows of the table in `crates/syntax/src/features.rs`: `backtick-identifiers`, `cast-operator`, `on-conflict`, `on-duplicate-key-update`, `insert-returning`, `lateral`, `full-join`, `qualify` and the rest of its 231 rows. Without a dialect only syntax that no dialect accepts is reported.

Unknown names are reported only where the schema is known, so a file without one gets none: an unknown table only in a schema a [snapshot](./snapshot-format.md) covers, an unknown column only when every table in scope has all its columns known (from a snapshot or from DDL in the script or the workspace), an unknown function only with a snapshot and a dialect. Nothing is reported in `DROP` statements, and unqualified names in routine bodies are left alone, since they can be variables.

A missing semicolon between two statements is reported where the next statement starts; a statement the parser cannot finish is cut off at the next `;` or at the next line that starts with a statement keyword, so it never swallows the statement after it.

## What names stand for

Names resolve the way the dialect resolves them: the tables of `FROM` with their aliases, common table expressions (also recursive ones and with column lists), subqueries in `FROM` and `LATERAL`, correlated subqueries, `JOIN ... USING` and `NATURAL JOIN`, the targets of `INSERT`, `UPDATE`, `DELETE` and `MERGE`, `RETURNING`, `excluded` in `ON CONFLICT`, the row alias and `VALUES()` in `ON DUPLICATE KEY UPDATE`, `NEW` and `OLD` in triggers, the parameters and variables of routines, and the aliases of the select list where the dialect lets a clause see them. Unquoted names fold to lower case in PostgreSQL; MySQL and MariaDB compare table names without case unless the snapshot says `lower_case_table_names` is 0. [NATIVE.md](../NATIVE.md) has the rules.

What a script defines counts as schema: the tables, views, types, sequences and routines its DDL creates before the statement at hand, and those of the `.sql` files in the workspace (migrations, schema dumps), file by file in the order of their paths. A snapshot wins over the workspace for an object both have; the script's own DDL wins over both.

## Completion

| Where | What is offered |
| ----- | --------------- |
| `FROM`, `JOIN`, `INTO`, `UPDATE`, `ALTER TABLE`, `REFERENCES` | Common table expressions, tables and views, schemas, system tables; after `JOIN` first the tables a foreign key links to a joined table, with the condition: `orgs ON u.org_id = orgs.id` |
| `schema.` | The tables of that schema |
| An expression | The columns of the tables in scope first (type as detail, table as description, the comment as documentation), all columns at once in a select list, aliases, functions with their parentheses, keywords such as `CASE` and `EXISTS` |
| `alias.` | The columns of that table or subquery |
| `JOIN x ON` | The conditions the foreign keys between `x` and the joined tables give |
| `status = ` or inside `'...'` | The values of an enum column (MySQL's `enum(...)`, a PostgreSQL enum type) |
| `INSERT INTO t (` | The columns not listed yet, and all of them at once |
| `INSERT INTO t ` | `(columns) VALUES (...)` as a snippet, leaving out generated and auto-increment columns, and `VALUES`, `SELECT`, `DEFAULT VALUES` where the dialect has them |
| A column definition | Types of the dialect and version and of the schema; after the type, the column constraints the dialect has |
| `CALL` | Procedures |
| `nextval('` | Sequences |
| `@@` (MySQL, MariaDB), `SET`, `SHOW`, `PRAGMA` | Settings and system variables |
| A statement's start, after an operand | The statements of the dialect; the keywords that continue a clause and the clauses that may still follow |

Keywords and functions are those of the dialect and version: MySQL is offered no `FULL JOIN`, MariaDB 11.4 no `UUID_V7`. Names are quoted where the dialect needs it. Keywords follow the case of what is typed.

## Hover

A table or view: its comment, its columns with types, keys, nullability and defaults, its foreign keys and indexes, and a view's query. A column: its definition, its table, its comment and the keys it is in. An alias: the table or subquery it stands for. A common table expression: its query. A function: its signatures at the version and a description. A routine of the schema: its signatures, comment and language. A type of the schema: its values or what it is based on.

## Definition

An alias, a common table expression, a select alias, a parameter or variable, a column of a subquery or common table expression, and whatever DDL in the document or in a workspace file defines: tables, views, columns, types, routines. An object only a snapshot holds has no place in a file, so definition gives nothing for it.

## Signature help

The parameters of the built-in function or the routine being called, every overload at the version, and the parameter under the cursor.

## Document symbols

One symbol per statement, in order. What a statement creates is named after it: a table with its columns and named constraints as children, a view, an index, a sequence, a type with its enum values or attributes, a domain, a function or procedure with its parameters and return type as detail, a trigger with its table, a schema or database, an extension. Other statements are named by their first words and what they work on (`INSERT INTO users`, `SELECT FROM orders`), with their common table expressions as children.

| Kind                     | LSP symbol kind |
| ------------------------ | --------------- |
| Table                    | Struct          |
| View                     | Interface       |
| Column                   | Field           |
| Constraint               | Property        |
| Index                    | Key             |
| Sequence                 | Number          |
| Type                     | Enum            |
| Enum value               | EnumMember      |
| Domain                   | TypeParameter   |
| Function                 | Function        |
| Procedure                | Method          |
| Trigger                  | Event           |
| Schema or database       | Namespace       |
| Extension                | Package         |
| Common table expression  | Variable        |
| Any other statement      | Object          |

## Folding ranges

Statements of more than one line, parenthesized lists and subqueries, `BEGIN ... END` and the bodies of `IF`, `CASE` and the loops, `CASE` expressions, block comments, runs of line comments (`comment`), and `-- region` to `-- endregion` (`region`). A fold ends on the line before a closing parenthesis or `END` that starts its line.

## Selection ranges

From the token at the cursor through every node around it to the whole script.

## Scripts

The parser reads scripts of any number of statements, with what the clients add around SQL: MySQL's `DELIMITER` (after which the new delimiter ends statements and `;` separates the statements of a routine body), psql's backslash commands and the data of `COPY ... FROM STDIN` up to `\.`, and the dot commands of SQLite's shell.
