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

## References

Every place that names a table, view, column, alias, common table expression, column alias, window, routine, parameter, variable, type, sequence or schema: in the document, and for an object of the schema in every `.sql` file of the workspace of a dialect the document can name. The references of a column follow it through common table expressions and subqueries that pass it on (`WITH a AS (SELECT email FROM users) SELECT a.email FROM a` names `users.email` twice). DDL counts: `CREATE TABLE`, column definitions, `ALTER TABLE` and its actions, `RENAME ... TO`, `COMMENT ON`, `DROP`, indexes and foreign keys, and the sequence in `nextval('orders_id')`. A table only a snapshot has is found wherever the workspace names it; a table nothing defines is still found by its name.

## Document highlights

The references in the document of what is under the cursor, each a read or a write: a column is written in the targets of `SET` and in the column list of an `INSERT`, a table by `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE` and DDL, a variable by `SET` and `INTO`; a definition counts as a write.

## Rename

What is local to a statement is renamed there: an alias, a common table expression, a column alias, a window, and the parameters and variables of a routine; MySQL's user variables (`@total`) in the whole document. What DDL in the document or the workspace defines is renamed in every file that names it: tables and views, columns (also in the views and `CREATE TABLE ... AS` that pass the column on under its name), routines, types, sequences, schemas. The new name is quoted where the dialect needs it (a reserved word, capitals in PostgreSQL, a space), and an occurrence written in quotes keeps its quotes.

A rename is refused, with a message, when it cannot be complete or would change what a statement means:

- a built-in function, a system table, or an object only the schema snapshot has (renaming files does not rename the database);
- an object no DDL in the workspace defines;
- a name that also stands inside a string of SQL the server does not read: a routine body kept as a string (`AS $$ ... $$`), `PREPARE`, `EXECUTE`, `DO`;
- a new name that is taken: a table, column, routine, type or sequence of that name, in the catalog or defined by another file;
- a new name that something else in scope already has, so a name would become ambiguous or stand for something else: a column another table of the query has, a common table expression or alias of the table's new name, an alias or common table expression of the same statement.

## Semantic tokens

Every token is colored by what it is, and a name by what it stands for: a table, a view, a column, an alias, a common table expression, a routine or built-in function, a type, a schema, a parameter or placeholder, a variable. A definition, a generated column, deprecated syntax and what the database brings each carry a modifier. [Clients](./clients.md#semantic-tokens) has the legend.

## Inlay hints

The column each value of an `INSERT ... VALUES` row goes to, when the statement names no columns or four or more; the column each item of an `INSERT ... SELECT` fills, where the item's name differs; the parameter each argument of a call goes to. [Configuration](./configuration.md#inlay-hints) switches each off.

```sql
INSERT INTO users VALUES (id: 1, email: 'a@b.c', name: 'A');
SELECT make_date(year: 2026, month: 10, day: 7);
```

## Formatting

The formatter decides the whitespace between tokens and the case of keywords, and nothing else: strings, quoted names and the text of comments stay as they are, and a layout that would change a single token is not applied. A statement with a syntax error is left as it is, and so are the data of `COPY` and the commands of a client.

```sql
WITH recent AS (
    SELECT
        customer_id,
        sum(total) AS spent
    FROM orders
    WHERE status <> 'new'
    GROUP BY customer_id
)
SELECT
    c.name,
    r.spent,
    CASE
        WHEN r.spent > 1000 THEN 'gold'
        ELSE 'bronze'
    END AS tier
FROM customers AS c
    JOIN recent AS r ON r.customer_id = c.id
    LEFT JOIN addresses AS a ON a.customer_id = c.id
        AND a.kind = 'billing'
WHERE c.active
    AND (c.country = 'NL' OR c.country IS NULL)
ORDER BY r.spent DESC
LIMIT 50;
```

Every statement starts a line, and so does every clause of a query. A list of several items (a select list, the assignments of `SET`, the rows of `VALUES`, the elements of `CREATE TABLE`, the actions of `ALTER TABLE`) has an item per line, one level in; a single item stays on the clause's line. Joins go one level in and the `AND` and `OR` of `WHERE`, `HAVING` and `ON` start lines a level further. A subquery and the query of a common table expression go a level in between their parentheses, a `CASE` with several branches has a branch per line, and the blocks, `IF`, loops and `CASE` of a routine body indent the statements they hold. Comments keep their place: one on a line of its own stays there, one after code stays after it. A blank line between statements or before a clause is kept, more than one becomes one.

Keywords are upper case by default; [configuration](./configuration.md#formatting) sets lower case or leaves them as written, the width of a level and commas at the start of a line. `onTypeFormatting` lays out a statement when its `;` is typed.

## Code actions

| Action | What it does |
| ------ | ------------ |
| Qualify with 'u' | `email` becomes `u.email`, the alias or table the column is of |
| Expand '*' into its columns | `*` or `u.*` becomes the columns it stands for, when every one is known |
| Add the alias 'u' | `FROM users` becomes `FROM users AS u`, and the statement's `users.id` becomes `u.id` |
| Uppercase keywords, Lowercase keywords | The keywords of a selection |
| Change to 'users' | A quick fix for an unknown table, column or function that is one or two letters off a known one |
| Qualify with 'u' | A quick fix for an ambiguous column, one per table that has it |

## Signature help

The parameters of the built-in function or the routine being called, every overload at the version, and the parameter under the cursor.

## Document symbols

One symbol per statement, in order. What a statement creates is named after it: a table with its columns and named constraints as children (or, for `CREATE TABLE ... AS`, the columns its query gives), a view with the columns of its column list or select list, an index, a sequence, a type with its enum values or attributes, a domain, a function or procedure with its parameters and return type as detail, a trigger with its table, a schema or database, an extension. Other statements are named by their first words and what they work on (`INSERT INTO users`, `SELECT FROM orders`). The common table expressions of a statement are its children wherever they stand, in a subquery or inside another common table expression, which holds its own as children.

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
