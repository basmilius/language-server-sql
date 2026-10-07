# sql-language-server

A language server for SQL, written in Rust. It reads SQLite, MySQL, MariaDB and PostgreSQL with one parser that keeps every byte of a script and goes on past a syntax error, and it judges a script by the dialect and version it is written for. The aim is the insight a full database IDE gives (completion, navigation, inspections, refactors) from a schema snapshot, for people and for agents, without ever connecting to a database.

This file says how the server works and why. What it was measured to do is in [MEASUREMENTS.md](./MEASUREMENTS.md), what a client has to send is in [docs/clients.md](./docs/clients.md), and the phases still to come are in [CLAUDE.md](./CLAUDE.md).

## Where the code comes from

Everything here is written from scratch. The sources it learns from are the official documentation of SQLite, MySQL, MariaDB and PostgreSQL, and what their servers do with a statement: the corpus check runs every case on real servers, and the reserved words come from the servers' own catalogs. No code was taken from another SQL parser, formatter or language server.

## Layout

A Cargo workspace with four crates, of which only the server knows LSP. What every language server does the same way comes from `basmilius/language-server-core`, a Git dependency pinned to a tag: `lsc-text` (line index and position encodings), `lsc-syntax` (the token cursor and tree builder the parser is written on) and `lsc-server` (documents and their incremental sync, `file:` URIs, encoding negotiation, request dispatch, the main loop and `main`).

| Crate | Holds |
| --- | --- |
| `crates/syntax` (`sql-syntax`) | Dialects, versions and targets, the lexer, the parser and the tree (on `rowan`), the feature table, the reserved words and the pass that reports what a target does not accept. |
| `crates/catalog` (`sql-catalog`) | The model of a schema, the reader of snapshot files, and the built-in catalogs of each dialect and version: functions, types, system schemas and settings. |
| `crates/analysis` (`sql-analysis`) | Questions about a tree: diagnostics, document symbols, folding and selection ranges, the schema DDL defines, name resolution, completion, hover, definition, signature help and unknown names. |
| `crates/server` (`sql-language-server`) | The LSP front end over stdio: documents, the settings of each document, and the conversion of everything above to LSP. Library and binary. |

The crate still to come has its place: `sql-format` for the formatter.

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

Keywords are the words the grammar looks for, in one sorted table the lexer searches after putting a word in capitals in a stack buffer. The lexer cannot know whether a keyword names something; the parser decides, and a keyword that stands where a name stands goes into the tree as an `IDENT` (the core's `bump_remap`): `SELECT date FROM t` has an `IDENT` token inside `(COLUMN_REF (NAME "date"))`. A `NAME` holds an identifier, quoted or not, or a string where a dialect takes one as an alias or a collation.

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

## The schema

What a script can name comes in layers, each a `Snapshot` of the model in `sql-catalog` with an index by name (`catalog.rs`):

1. the DDL of the document itself, applied statement by statement up to the statement at hand, so a script that creates a table and then queries it resolves, and a query before the `CREATE` does not;
2. the snapshot the settings name for the document;
3. the DDL of the workspace's `.sql` files, replayed file by file in the order of their paths, which is the order migrations named by number or date run in, so an `ALTER TABLE` in a later file changes a table an earlier one created;
4. the system schemas of the built-in catalog.

A layer that has an object hides that object in the layers after it, whole: the document's `CREATE TABLE users` is the table, whatever the snapshot says, and a table the snapshot has is the snapshot's even when a migration says otherwise, since the snapshot is what the database holds and the workspace may hold migrations not yet run. `ALTER TABLE` and `COMMENT ON` in the document change a copy of the table from the layer below. DDL that names no schema puts the object in an unnamed schema that stands for the default schema, whichever that is.

`ddl.rs` reads `CREATE TABLE` (columns with type, nullability, default, generation, auto-increment and comment; primary, unique and foreign keys, checks and MySQL's indexes; `AS SELECT` and `LIKE`), `CREATE VIEW`, `CREATE INDEX`, `CREATE TYPE` (enums, composites, ranges), `CREATE DOMAIN`, `CREATE SEQUENCE`, functions and procedures with their parameters, triggers, schemas, `ALTER TABLE` (columns added, dropped, renamed, modified and altered, constraints, renames), `RENAME TABLE`, `COMMENT ON`, and `DROP` of what the same layer created. `USE`, `SET search_path` and SQLite's `ATTACH` change where later names resolve. A view's columns are the names of its select list; a wildcard leaves them open.

### Where an unqualified name looks

- PostgreSQL: `pg_catalog`, unless the path names it, then the search path: `SET search_path` of the script, the snapshot's `searchPath`, or its `defaultSchema`, or `public`.
- MySQL and MariaDB: the database of `USE`, the snapshot's `defaultSchema`, the only database of the snapshot; with none of those, every database.
- SQLite: `temp`, `main`, then the attached databases.
- Without a dialect: the snapshot's default schema, then every schema.

### Case

PostgreSQL folds an unquoted name to lower case and compares exactly, so `Users` finds `users` and not `"Users"`. MySQL and MariaDB compare columns, aliases and routines without case, and tables and databases without case unless the snapshot says `lower_case_table_names` is 0, as on Linux; without the setting they compare without case, which reports nothing a server on Linux would accept and leaves out what only a server on Windows or macOS would. SQLite and a script without a dialect compare without case. A quoted name keeps its case in every dialect (`ident.rs`).

## The built-in catalogs

`scripts/catalog.py` takes from the servers what each version has and writes `crates/catalog/data/<dialect>.tsv`, which the crate embeds and reads once per dialect:

- PostgreSQL 18: the functions of `pg_catalog` that a person calls, from `pg_proc` without the functions of operators, aggregates' support, type I/O, access methods and casts, with their parameters (`proargnames`, `proargtypes`, defaults, `VARIADIC`) and `pg_get_function_result()`; the types of `pg_type`, with the spellings the grammar adds (`integer`, `character varying`, `timestamp with time zone`); the tables and views of `pg_catalog` and `information_schema` with their columns; the settings of `pg_settings`.
- MySQL 8.0 and 8.4, MariaDB 11.0, 11.4 and 11.8: these have no catalog of their functions, so every candidate name (the help tables' topics, MariaDB's `SQL_FUNCTIONS`, the names the grammar reads, PostgreSQL's and the described ones) is prepared on the server as a call with 0 to 6 arguments inside a stored procedure that catches the error. An unknown function is error 1305 or 1630, a wrong number of arguments 1582; a window function is tried again with `OVER ()`. Nothing is executed. Parameter names come from the syntax the help tables show where it parses. Types are candidates prepared in a `CREATE TABLE`. The columns of `information_schema`, `mysql`, `performance_schema` and `sys`, and the system variables, come from `information_schema`.
- SQLite 3.48, 3.49 and 3.53, the command-line shell of Alpine 3.21, 3.22 and 3.23: `pragma_function_list` without the shell's own extensions, `pragma_pragma_list`, and the schema tables with `pragma_table_info`.

A row of a data file holds the versions it was seen in, as a range over the versions sampled; a version between two samples counts as the older one. `data/descriptions.tsv` is written by hand: a one-line description of each common function, and the parameters and return type where the server reports none (MySQL, MariaDB and SQLite report no types; SQLite no names) or a form of the grammar the server's catalog does not list (PostgreSQL's `coalesce`, `greatest`, `nullif`, `CAST`, `EXTRACT`, MySQL's `DATE_ADD`). A test fails when a description adds a function to a dialect without a signature.

## Name resolution

`resolve.rs` answers what a name stands for. A scope is a list of levels, from the innermost query or statement outward, found by walking up from the name:

- a `SELECT` brings the tables of its `FROM`; a name in a subquery of an expression sees the levels around it (a correlated subquery), a derived table in `FROM` does not see the query it is in unless it is `LATERAL`, and a function in `FROM` does;
- the target of `UPDATE` with its `FROM`, the tables of `DELETE` with `USING`, the target of `INSERT` for its column list, `RETURNING`, `ON CONFLICT` (with `excluded`) and `ON DUPLICATE KEY UPDATE` (with the row alias of `VALUES ... AS new`), and the target and source of `MERGE`; the targets of `SET` and of the column list see only the target;
- the table a `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE` or `CREATE TRIGGER` is about, with `NEW` and `OLD` in a trigger;
- the parameters of a routine and the variables `DECLARE` gives before the name in a block.

A source is a table of the catalog, a common table expression, a derived table, a function in `FROM`, a table being defined, or a name that resolves to nothing, whose columns are open. Its columns come from the catalog, from the select list of its query (a wildcard expands to the columns of the sources it names), from a column list of an alias or a common table expression, or from the dialect (`rowid` in SQLite, `ctid` and the other system columns in PostgreSQL, `column1` or `column_0` of `VALUES`).

Common table expressions are visible to the body of their query, to later ones in the same `WITH`, to themselves with `RECURSIVE`, and are hidden by an inner `WITH` of the same name. A column that `USING` or `NATURAL` merges is not ambiguous.

An alias of the select list is seen by the clause rules of each dialect, as the servers answer them: `ORDER BY` sees it before the columns in every dialect, `GROUP BY` after the columns, `HAVING` in MySQL, MariaDB and SQLite, `WHERE` only in SQLite (after the columns). PostgreSQL takes an alias only as a whole item of `ORDER BY` or `GROUP BY`, and MySQL only as a whole item of `GROUP BY`; MariaDB also within an expression. The `ORDER BY` of a set operation sees the names of its first query.

A resolution is a referent (a table, a common table expression, a source under an alias, a column of a source, a select alias, a variable, a schema, a built-in function, routines), an ambiguity, or nothing, which says whether everything the name could be is known: an unknown column is surely wrong only when every source in scope has all its columns known.

## Completion

`completion.rs` replaces the word being typed by a placeholder name, parses the result and reads the context from where the placeholder sits in the tree:

- a table name (`FROM`, `JOIN`, `INTO`, `UPDATE`, `ALTER TABLE`, `REFERENCES`): common table expressions first, the tables and views of the search path, schemas, system tables last; after `JOIN`, each table a foreign key links to a table already joined comes first with its condition (`orgs ON u.org_id = orgs.id`); after `schema.`, that schema's tables;
- a column in an expression: the columns of the sources in scope in their table's order, innermost first, then the whole list of columns as one item in a select list, the aliases as qualifiers, routines and the built-in functions of the version, and the keywords of an expression; after `alias.`, that source's columns; in `JOIN x ON`, the conditions foreign keys give first; next to an enum column (`status = `), its values;
- an `INSERT` column list: the target's columns not listed yet, and all of them as one item; after the target, `(columns) VALUES (...)` as a snippet without the generated and auto-increment columns, and after a column list `VALUES (...)` for those columns;
- a type in a column definition, a `CAST` or `::`: the built-in types of the version and the schema's types, enums and domains;
- after a type, the column constraints the dialect has; after `CALL`, the procedures; after `@@` in MySQL and MariaDB and in `SET`, `SHOW` and `PRAGMA`, the settings;
- inside a string compared with an enum column, its values; inside the string of `nextval`, `currval` or `setval`, the sequences;
- where a statement starts, the statements of the dialect; after an operand, the keywords that continue the clause and the clauses that may still follow (`WHERE`, `GROUP BY`, `ORDER BY`, `LIMIT`, joins, set operations, `RETURNING`, `ON CONFLICT`, `ON DUPLICATE KEY UPDATE`).

The keywords come from the clause at hand and the clauses after it rather than from the parser, which does not record what it expected; the feature table filters them (`supports` in `sql-syntax`), so MySQL is offered no `FULL JOIN` and MariaDB 11.4 no `UUID_V7`. Keywords follow the case of the word typed. A name is quoted when the dialect needs it (a reserved word, characters an unquoted name cannot hold, capitals in PostgreSQL) or when the word was begun with a quote. Functions insert their parentheses, with a tab stop inside when the client takes snippets. Items are ranked by kind (templates and join conditions, the scope's columns and aliases, tables, schemas, routines, keywords, built-in functions) and within a kind by their order in the schema; the items that start with the typed word come before those that only contain its letters in order. A list is cut at 500 items and marked incomplete.

## Hover, definition and signature help

Hover (`nav.rs`) describes a table with its comment, its columns as a table with keys, and its foreign keys and indexes; a column with its definition, its table, its comment and the key it is part of; an alias with what it stands for; a common table expression with its query; a select alias with its expression; a built-in function with the signatures of the version and its description; a routine with its signatures, comment and language; a type of the schema with its values or base. Definition goes to an alias, a common table expression, a select alias, a variable or a column of either, and to what DDL in the document or in a workspace file defines. What only a snapshot holds has no place in a file, so definition gives nothing for it. Signature help (`signature.rs`) shows the routines of the schema, or the overloads of a built-in function at the version, and the parameter under the cursor, a variadic one taking the rest.

## Unknown names

`unresolved.rs` reports an unknown table (`unresolved-table`, also for an unknown qualifier), an unknown column (`unresolved-column`), an unknown function (`unresolved-function`) and a column more than one table in scope has (`ambiguous-column`). It reports only what is surely wrong:

- nothing at all without a snapshot and without DDL, so a file without a schema is never flooded;
- an unknown table only in a schema a snapshot covers (the default schema or a schema it names) or a system schema;
- an unknown column only when every source in scope has all its columns known, from a snapshot or DDL; a function or a table that is not known leaves the scope open;
- an unknown function only with a snapshot loaded and a dialect set, and not when it names a type or is qualified with a schema;
- nothing in a `DROP` statement, and no unqualified column in a routine body, where names can be variables the server does not follow.

## The workspace

The server reads the `.sql` files under the workspace folders in a thread when it starts (skipping hidden folders, `node_modules`, `vendor`, `target`, `dist` and `build`, files over 16 MB, and stopping after 10,000 files), keeps only their statements that define something, and replays them per dialect when a document asks (`workspace.rs`, `files.rs`). A file's dialect comes from the settings for its path. A document sees the files of its dialect and those without one; a document without a dialect sees all. A watched change or, for a client that does not watch, a save reads the file again.

## The server

`sql-language-server --stdio` (the flag is the default) speaks LSP over stdin and stdout. It negotiates the position encoding (UTF-8 when the client offers it, else UTF-16) and syncs documents incrementally. A change is applied to the text and the whole script is parsed again on the next question; the server answers every message already queued before it publishes diagnostics, so a burst of keystrokes costs one parse.

### Settings

Only standard LSP channels are used: `initializationOptions`, `workspace/configuration` for the section `sqlLanguageServer` with the document as `scopeUri`, and `workspace/didChangeConfiguration`. The settings are a `dialect`, a `version` and a `schema` snapshot at the top level and per file or folder in `overrides`; [docs/configuration.md](./docs/configuration.md) has the details. The most specific level that names a field wins, and a version only counts at a level that names no dialect or the same one, so a MySQL version never applies to a folder set to PostgreSQL. Without a dialect in the settings, a `languageId` such as `mysql` decides. Each document keeps its target; when its dialect changes, its tree is dropped, since the dialect changes the tokens.

### Snapshots

A snapshot is read when a document first needs it (`snapshots.rs`), and the client is asked to watch the file with a registration of `workspace/didChangeWatchedFiles` of its own; the server registers `**/*.sql` as well. When the client reports a change, the snapshot is read again and every open document's diagnostics are published again. A client that cannot register watchers has the modification time and size of each snapshot checked once the queued messages are answered. A snapshot that cannot be read is told once with `window/showMessage` and logged, and documents read as if there were none until it reads again; telling it again only happens when the problem changes.

### What it answers

- diagnostics: syntax errors, what the feature table reports, reserved words and unknown names, with `source: "sql"` and the id as `code`, pushed after a burst of changes or pulled with `textDocument/diagnostic` by a client that announces it;
- `textDocument/completion` (triggered by `.` and `@`), with snippets for a client that takes them and the table or schema in `labelDetails` for one that shows them;
- `textDocument/hover` in markdown, `textDocument/definition`, and `textDocument/signatureHelp` (triggered by `(` and `,`);
- `textDocument/documentSymbol`: one symbol per statement, definitions named after what they create with their columns, constraints, enum values or attributes as children, other statements by their first words and what they work on, with their common table expressions; flat for a client that cannot nest;
- `textDocument/foldingRange`: statements, parenthesized lists, blocks and bodies, `CASE`, comments and regions;
- `textDocument/selectionRange`: from the token through every enclosing node to the script.

## Limits

- Options of tables, sequences and routines, `SHOW`, `GRANT` and the utility commands are read as runs of words, so a misspelled option is not reported.
- The body of a routine in a string (PostgreSQL's `AS $$ ... $$`) is kept whole and not parsed, even when its language is SQL; the fragment interface of the last phase is how it will be read.
- Semantic restrictions are not checked: a function's arguments, the types a `CAST` takes beyond the rows of the table, which expressions MySQL takes in `LIMIT` beyond literals and parameters, a `GROUP BY` that misses a column.
- Types of expressions are not inferred: hover on a select item shows its expression, not its type, and completion does not rank by type.
- A body of a routine in a string is not read, so its names are neither resolved nor reported; the variables of a MySQL or MariaDB routine are known only from `DECLARE` and parameters.
- The functions of MySQL, MariaDB and SQLite have no types from the servers; the descriptions give the return types of the common ones.
- A workspace file without a dialect in the settings is read without one.
