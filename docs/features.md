# Features

## Diagnostics

Every diagnostic has `source: "sql"` and one of these codes:

| Code                 | What it reports                                                                                                                |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `syntax`             | What the parser cannot read: a missing token (`')' expected`), an unexpected one, an unknown statement, an unterminated string |
| `reserved-word`      | A word the dialect reserves where it names a column, a table or an alias without quotes                                        |
| a feature id         | Syntax the dialect never has (`MERGE is not supported by MySQL`), not yet has at the version (`INTERSECT and EXCEPT are only available since MySQL 8.0.31`), or deprecates (a warning with the deprecated tag) |

The feature ids are the rows of the table in `crates/syntax/src/features.rs`: `backtick-identifiers`, `cast-operator`, `on-conflict`, `on-duplicate-key-update`, `insert-returning`, `lateral`, `full-join`, `qualify` and the rest of its 231 rows. Without a dialect only syntax that no dialect accepts is reported.

A missing semicolon between two statements is reported where the next statement starts; a statement the parser cannot finish is cut off at the next `;` or at the next line that starts with a statement keyword, so it never swallows the statement after it.

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
