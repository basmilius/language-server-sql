# sql-language-server

A language server for SQL, written in Rust. It reads SQLite, MySQL, MariaDB and PostgreSQL with one parser that keeps every byte of a script and goes on past a syntax error, and it judges a script by the dialect and version it is written for. The aim is the insight a full database IDE gives (completion, navigation, inspections, refactors) from a schema snapshot, for people and for agents, without ever connecting to a database.

This file says how the server works and why. What it was measured to do is in [MEASUREMENTS.md](./MEASUREMENTS.md), what a client has to send is in [docs/clients.md](./docs/clients.md), and the phases still to come are in [CLAUDE.md](./CLAUDE.md).

## Where the code comes from

Everything here is written from scratch. The sources it learns from are the official documentation of SQLite, MySQL, MariaDB and PostgreSQL, and what their servers do with a statement: the corpus check runs every case on real servers, and the reserved words come from the servers' own catalogs. No code was taken from another SQL parser, formatter or language server.

## Layout

A Cargo workspace with three crates, of which only the server knows LSP. What every language server does the same way comes from `basmilius/language-server-core`, a Git dependency pinned to a tag: `lsc-text` (line index and position encodings), `lsc-syntax` (the token cursor and tree builder the parser is written on) and `lsc-server` (documents and their incremental sync, `file:` URIs, encoding negotiation, request dispatch, the main loop and `main`).

| Crate | Holds |
| --- | --- |
| `crates/syntax` (`sql-syntax`) | Dialects, versions and targets, the lexer, the parser and the tree (on `rowan`), the feature table, the reserved words and the pass that reports what a target does not accept. |
| `crates/analysis` (`sql-analysis`) | Questions about a tree: diagnostics, document symbols, folding and selection ranges. Later the catalog, name resolution, completion and the rest. |
| `crates/server` (`sql-language-server`) | The LSP front end over stdio: documents, the settings of each document, and the conversion of everything above to LSP. Library and binary. |

The crates still to come have their place: `sql-catalog` for schema snapshots and the built-in catalogs of each dialect, `sql-format` for the formatter.

## Dialects and targets

`Dialect` is `Generic`, `Sqlite`, `Mysql`, `Mariadb` or `Postgres`. `Version` is major, minor and patch, ordered, and reads `8.0.36`, `18` and what a server reports, such as `11.4.2-MariaDB`. A `Target` is a dialect with an optional version; without one it is the newest. Each dialect has a minimum (SQLite 3.47, MySQL 8.0, MariaDB 11.0, PostgreSQL 18): syntax older than the minimum counts as always there.

The dialect decides two things: how a text is lexed, and what the feature table reports. The version only decides the second.

## Lexer

`lexer.rs` cuts a text into tokens that cover every byte. What a byte means differs between the dialects, so the lexer takes `LexOptions`, which each dialect fills in:

- `#` starts a comment in MySQL and MariaDB and is an operator in PostgreSQL;
- a double quote starts a string in MySQL and MariaDB and an identifier elsewhere;
- a backslash escapes in MySQL's strings and not in the standard's;
- `[a]` is an identifier in SQLite and a subscript in PostgreSQL;
- `/* /* */ */` nests in PostgreSQL;
- `$$...$$` and `$tag$...$tag$` are strings in PostgreSQL, `$name` a parameter in SQLite and an identifier in MySQL;
- `@a` and `@@a` are variables in MySQL, `@a` a parameter in SQLite and the absolute value of `a` in PostgreSQL;
- PostgreSQL reads any run of operator characters as one operator, by its rules (`a=-1` is `=` and `-`, `a!=-1` is the operator `!=-`); the others have a fixed set, and `<=>` is null-safe equality in MySQL and an operator of an extension in PostgreSQL;
- `--` starts a comment in MySQL only when whitespace follows;
- MySQL words may start with digits (`1st`), and `1_000` is one number in PostgreSQL and SQLite.

Everything else is the union of the dialects: `E''`, `N''`, `X''`, `B''`, `U&''`, `U&""`, backticks, every number form, every parameter form (`?`, `?1`, `$1`, `:name`, `@name`, `$name`), lexed in every dialect and reported by the feature table where a dialect lacks it.

The lexer also reads what the clients add around SQL. MySQL's `DELIMITER //` at the start of a statement sets a delimiter of its own, after which `//` ends statements (`CUSTOM_DELIMITER`) and `;` still separates the statements of a routine body. psql's backslash commands and MySQL's `\G` run to the end of the line (`META_COMMAND`), as do the dot commands of SQLite's shell at the start of a line. After `COPY ... FROM STDIN;` the lines up to `\.` are one `COPY_DATA` token.

Keywords are the words the grammar looks for, in one sorted table the lexer searches after putting a word in capitals in a stack buffer. A keyword that names something keeps its kind and stands in a `NAME` node: `SELECT date FROM t` has a `DATE_KW` token inside `(COLUMN_REF (NAME "date"))`. The core's parser has no way to give a token another kind yet; a `bump_remap` in `lsc-syntax` would let the parser turn such keywords into `IDENT`, and then `NAME` always holds an identifier.

A script without a dialect is lexed so dumps of every dialect read: MySQL's backslash escapes, with one guess (a backslash right before a quote that ends the string where it stands, before whitespace, `,`, `)`, `;` or the end, is a backslash, so `ESCAPE '\'` and `'C:\'` read as the standard has them and `'it\'s'` as MySQL has it), PostgreSQL's dollar quotes, nested comments and operators, MySQL's variables and `DELIMITER`, and the backslash commands. A double quote is an identifier and `#` an operator, as in the standard.

## Parser

`parser/` is recursive descent with a Pratt parser for expressions, over the union of the four dialects. It never looks at a version.

- `stmt.rs`: scripts, statement dispatch, how a statement ends and how a broken one is recovered, and the small statements (transactions, `SET`, `SHOW`, `USE`, `EXPLAIN`, `PRAGMA`, `GRANT`, `COPY`, `PREPARE`, `EXECUTE`, `CALL`, `DO`, `ATTACH`, and utility commands read leniently).
- `query.rs`: `WITH`, the set operations, `SELECT` and its clauses, `VALUES`, `TABLE`, and the tables and joins of `FROM`.
- `expr.rs`: expressions, function calls with the standard's special forms (`EXTRACT(... FROM ...)`, `TRIM(LEADING ... FROM ...)`, `SUBSTRING(... FROM ... FOR ...)`, `POSITION(... IN ...)`), aggregates with `DISTINCT`, `ORDER BY` and `SEPARATOR`, `FILTER`, `WITHIN GROUP`, `OVER`, and the clauses of SQL/JSON.
- `types.rs`: type names with their arguments, multi-word types, MySQL's attributes and PostgreSQL's arrays.
- `dml.rs`: `INSERT` and `REPLACE`, `UPDATE`, `DELETE`, `MERGE`.
- `ddl.rs`: `CREATE`, `ALTER` and `DROP` of tables, indexes, views, schemas, sequences, types, domains and extensions; `TRUNCATE`, `RENAME TABLE`, `COMMENT ON`, `REFRESH`.
- `routine.rs`: functions, procedures and triggers, and the compound statements of SQL/PSM in their bodies.

Options whose details differ in every dialect (MySQL's table options, storage parameters, sequence options, the characteristics of a routine) are read as runs of words and values, so a dump of any dialect reads without errors, at the price of not checking them yet. An unknown statement becomes an `UNKNOWN_STMT` with its tokens in an `ERROR` node.

### The tree

A node starts at its first token and ends at its last, so trivia sit between nodes. Every statement is a `*_STMT` node that holds its own `;` or delimiter.

- Names: `NAME` wraps one identifier or keyword token. `QUALIFIED_NAME` is `NAME (. NAME)*` for an object (tables, views, functions, types, indexes). `COLUMN_REF` is the same shape for a column in an expression (`t.a`, `s.t.a`). `WILDCARD` is `*` or `t.*`. `ALIAS` is `[AS] NAME [NAME_LIST | TABLE_ELEMENT_LIST]`. `NAME_LIST` is a parenthesized list of names.
- Queries: a query is one node whose kind says what it is: `SELECT` (with `SELECT_LIST` of `SELECT_ITEM`, `INTO_CLAUSE`, `FROM_CLAUSE`, `WHERE_CLAUSE`, `GROUP_BY_CLAUSE`, `HAVING_CLAUSE`, `WINDOW_CLAUSE`), `COMPOUND_SELECT` (operands and the set operator, nested to the left with `INTERSECT` binding tighter), `VALUES` (of `ROW_EXPR`), `TABLE_QUERY` or `PAREN_QUERY`. The outermost node of a query holds its `WITH_CLAUSE` (of `CTE`) first and `ORDER_BY_CLAUSE`, `LIMIT_CLAUSE`, `OFFSET_CLAUSE`, `FETCH_CLAUSE` and `LOCKING_CLAUSE` last, so `WITH` and `ORDER BY` of `SELECT ... UNION SELECT ... ORDER BY` sit on the `COMPOUND_SELECT`. A statement wraps its query in `SELECT_STMT`; a subquery is a `PAREN_QUERY` where an expression or a table stands.
- Tables: `TABLE_REF` (a name with its alias, partitions, index hints, `TABLESAMPLE`), `DERIVED_TABLE` (a `PAREN_QUERY` with its alias, `LATERAL`), `TABLE_FUNCTION` (a `FUNCTION_CALL` with `WITH ORDINALITY` and an alias), `PAREN_JOIN` and `JOIN_EXPR` (left operand, join words, right operand, `ON_CLAUSE` or `USING_CLAUSE`, nested to the left).
- Expressions: `LITERAL`, `TYPED_LITERAL` (`DATE '...'`, `json '...'`), `INTERVAL_EXPR`, `PARAMETER`, `VARIABLE_REF`, `COLUMN_REF`, `WILDCARD`, `FUNCTION_CALL` (`QUALIFIED_NAME`, `ARG_LIST`, then `WITHIN_GROUP_CLAUSE`, `FILTER_CLAUSE`, `OVER_CLAUSE` with a `WINDOW_SPEC`), `CAST_EXPR`, `TYPECAST_EXPR` (`::`), `CASE_EXPR` with `WHEN_CLAUSE` and `ELSE_CLAUSE`, `BINARY_EXPR`, `PREFIX_EXPR`, `IS_EXPR`, `BETWEEN_EXPR`, `IN_EXPR` with an `IN_LIST` or a `PAREN_QUERY`, `LIKE_EXPR` (also `ILIKE`, `SIMILAR TO`, `GLOB`, `REGEXP`, `RLIKE`, `MATCH`, `SOUNDS LIKE`), `MEMBER_OF_EXPR`, `EXISTS_EXPR`, `QUANTIFIED_EXPR` (`ANY`, `SOME`, `ALL`), `PAREN_EXPR`, `ROW_EXPR`, `ARRAY_EXPR`, `INDEX_EXPR`, `FIELD_EXPR`, `COLLATE_EXPR`, `AT_TIME_ZONE_EXPR`, `VALUE_FUNCTION` (`CURRENT_DATE` and the others without parentheses), `DEFAULT_EXPR`, `MATCH_AGAINST_EXPR`, `JSON_KEY_VALUE` and `JSON_CLAUSE`.
- Changing data: `INSERT_STMT` (target, `NAME_LIST`, `VALUES` or a query or `SET_CLAUSE`, `ON_DUPLICATE_KEY_CLAUSE`, `UPSERT_CLAUSE` with a `CONFLICT_TARGET`, `RETURNING_CLAUSE`), `UPDATE_STMT` (tables, `SET_CLAUSE` of `ASSIGNMENT`, `FROM_CLAUSE`, `WHERE_CLAUSE`), `DELETE_STMT` (targets, `FROM_CLAUSE`, `USING_CLAUSE`), `MERGE_STMT` with `MERGE_WHEN_CLAUSE`.
- Definitions: `CREATE_TABLE_STMT` with a `TABLE_ELEMENT_LIST` of `COLUMN_DEF` (`NAME`, `TYPE`, `COLUMN_CONSTRAINT`) and `TABLE_CONSTRAINT` (`INDEX_COLUMN_LIST` of `INDEX_COLUMN`, `REFERENCES_CLAUSE`), `TABLE_OPTION`, `TABLE_PARTITION_CLAUSE` and `PARTITION_DEF`; `ALTER_TABLE_STMT` with `ADD_COLUMN_ACTION`, `DROP_COLUMN_ACTION`, `ALTER_COLUMN_ACTION`, `MODIFY_COLUMN_ACTION`, `RENAME_COLUMN_ACTION`, `RENAME_TABLE_ACTION`, `ADD_CONSTRAINT_ACTION`, `DROP_CONSTRAINT_ACTION` and `ALTER_TABLE_ACTION` for the rest; `CREATE_INDEX_STMT`, `CREATE_VIEW_STMT`, `CREATE_SCHEMA_STMT`, `CREATE_SEQUENCE_STMT`, `CREATE_TYPE_STMT` (`ENUM_VALUE_LIST` or attributes), `CREATE_DOMAIN_STMT`, `CREATE_EXTENSION_STMT`, `DROP_STMT`, `CREATE_STMT` for other objects.
- Routines: `CREATE_FUNCTION_STMT` with `PARAM_LIST` of `PARAM_DEF`, `RETURNS_CLAUSE` and a `ROUTINE_BODY` that holds a string (kept whole), a `RETURN_STMT`, a `BLOCK` or a statement. `CREATE_TRIGGER_STMT` with a `ROUTINE_BODY`. `BLOCK` holds a `STATEMENT_LIST`; `IF_STMT` (with `ELSEIF_CLAUSE` and `ELSE_CLAUSE`), `CASE_STMT`, `LOOP_STMT`, `WHILE_STMT`, `REPEAT_STMT` (each with a `LABEL` when labeled), `LEAVE_STMT`, `ITERATE_STMT`, `RETURN_STMT`, `DECLARE_STMT`, `SIGNAL_STMT`, `OPEN_STMT`, `FETCH_STMT`, `CLOSE_STMT`.

### Precedence

PostgreSQL's where the dialects differ, from loose to tight: `:=`, `OR`, `XOR`, `AND`, `NOT`, `IS`, the comparisons, `BETWEEN`/`IN`/`LIKE` and the other predicates, every other operator (`||`, `->`, `@>`, custom operators), `+ -`, `* / % DIV MOD`, `^`, `AT TIME ZONE`, `COLLATE`, the unary operators, and `::`, `[...]` and `.field`. A different precedence in another dialect changes the shape of a tree, not whether it parses.

### Error recovery

The parser never fails and never panics: every input gives a tree whose text equals the input, with `ERROR` nodes around what it could not read and a list of errors with ranges. A missing token is reported without being consumed. A statement ends at its `;` or delimiter; a missing one is reported when the next statement starts on a line of its own, and anything else is wrapped in an error up to the next `;` or line that starts a statement, so one broken statement never swallows the next. Inside a `BEGIN ... END` body, `END`, `ELSE`, `ELSEIF`, `WHEN` and `UNTIL` also end a statement. A second error at the offset of the first follows from it and is left out, and the parser leaves a token the lexer already reported alone. Nesting is limited to 200 levels and the parser stops after 200,000 lookups that consume nothing, both from the core.

## The feature table

`features.rs` has one row per piece of syntax that not every dialect has: an id, the name it has in a message, the support in SQLite, MySQL, MariaDB and PostgreSQL (`Always`, `Since(version)`, `Never` or `DeprecatedSince(version)`), the kinds of node or token to look at, a function that finds the construct and gives the range to report, and an example. `check_features(tree, target)` walks the tree once and reports syntax the dialect never has, syntax the version does not have yet, and deprecated syntax as a warning with the deprecated tag. Without a dialect only rows no dialect has are reported, such as `QUALIFY`.

Supporting a new version means adding rows or moving the `Since` of a row; no version check sits anywhere else. A row whose target has the syntax cannot report, so its detection is not run; tokens are only looked at where a node's children hold a kind an active row wants.

A test holds every row against its example in every dialect: parsed without errors and not reported where the row says `Always`, reported just before and accepted at its `Since`, warned at its `DeprecatedSince`, and rejected (or read as something else entirely, as `@total` is a parameter in SQLite) where it says `Never`.

## Reserved words

The parser reads any keyword as a name where a name must stand, and only words that would make a statement ambiguous in any dialect (`SELECT`, `FROM`, `WHERE`, `ORDER` and the like) never stand for a column or an implicit alias. Which words a dialect reserves is the dialect's question: `reserved_words.rs` has the lists, which `scripts/reserved-words.py` takes from the servers (MySQL's `information_schema.KEYWORDS` for 8.0 and 8.4, PostgreSQL's `pg_get_keywords()` for the reserved words and those that may only name a function or type, and for MariaDB and SQLite every candidate word tried as a column name). `check_features` reports an unquoted reserved word where it names a column, a table, an alias, a constraint, a parameter or a common table expression, not where it names a function or a type, not after a dot in MySQL, MariaDB and PostgreSQL, and not after `AS` in PostgreSQL, as the servers do.

## The corpus against real servers

The table and the parser are held to the servers themselves. `scripts/dialect-corpus.py` runs the example of every row and every case of `crates/syntax/tests/data/dialects.sql` on SQLite, MySQL 8.0 and 8.4, MariaDB 11 and PostgreSQL 18, each from a fresh database with the same fixture tables, and records whether each ran it, rejected it as a syntax error or failed on something else. A failure that is not a syntax error fits either verdict: MySQL reads `a FULL JOIN b` as the table `a` with the alias `full` and then fails on a column, and the parser rightly rejects it. `cargo test` holds the parser and the table to that record without a server, and to `dialects-known.txt`, the differences that are deliberate:

- a `?` placeholder is accepted in MySQL and MariaDB as in a prepared statement, which is what a file of queries holds, though a plain script rejects it;
- PostgreSQL reads `t PARTITION (p0)` as an alias with a column list; the parser reads MySQL's partition selection;
- SQLite takes any words after a column's type as part of the type name, so `INT AUTO_INCREMENT`, `INT[]`, `INT COMMENT 'x'`, `INT INVISIBLE` and a missing comma all run there and mean nothing; the parser reports them.

## The server

`sql-language-server --stdio` (the flag is the default) speaks LSP over stdin and stdout. It negotiates the position encoding (UTF-8 when the client offers it, else UTF-16) and syncs documents incrementally. A change is applied to the text and the whole script is parsed again on the next question; the server answers every message already queued before it publishes diagnostics, so a burst of keystrokes costs one parse.

### Settings

Only standard LSP channels are used: `initializationOptions`, `workspace/configuration` for the section `sqlLanguageServer` with the document as `scopeUri`, and `workspace/didChangeConfiguration`. The settings are a `dialect`, a `version` and a `schema` snapshot at the top level and per file or folder in `overrides`; [docs/configuration.md](./docs/configuration.md) has the details. The most specific level that names a field wins, and a version only counts at a level that names no dialect or the same one, so a MySQL version never applies to a folder set to PostgreSQL. Without a dialect in the settings, a `languageId` such as `mysql` decides. Each document keeps its target; when its dialect changes, its tree is dropped, since the dialect changes the tokens. The schema snapshot is resolved and kept for the next phase.

### What it answers

- diagnostics: syntax errors, what the feature table reports and reserved words, with `source: "sql"` and the id as `code`, pushed after a burst of changes or pulled with `textDocument/diagnostic` by a client that announces it;
- `textDocument/documentSymbol`: one symbol per statement, definitions named after what they create with their columns, constraints, enum values or attributes as children, other statements by their first words and what they work on, with their common table expressions; flat for a client that cannot nest;
- `textDocument/foldingRange`: statements, parenthesized lists, blocks and bodies, `CASE`, comments and regions;
- `textDocument/selectionRange`: from the token through every enclosing node to the script.

## Limits of this phase

- Options of tables, sequences and routines, `SHOW`, `GRANT` and the utility commands are read as runs of words, so a misspelled option is not reported.
- The body of a routine in a string (PostgreSQL's `AS $$ ... $$`) is kept whole and not parsed, even when its language is SQL; the fragment interface of the last phase is how it will be read.
- Semantic restrictions are not checked: a function's arguments, the types a `CAST` takes beyond the rows of the table, which expressions MySQL takes in `LIMIT` beyond literals and parameters, a `GROUP BY` that misses a column.
- A keyword used as a name keeps its keyword kind inside its `NAME` node until the core can give a token another kind.
