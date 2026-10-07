# basmilius/language-server-sql

A SQL language server in Rust that speaks LSP over stdio, for SQLite, MySQL, MariaDB and PostgreSQL. `README.md` and `docs/` are for people who use it, `NATIVE.md` says how the implementation works and why, `MEASUREMENTS.md` holds what it was measured to do (dated), `docs/clients.md` lists what a client has to send and announce, and this file is for agents who work on it.

It is built in phases by agents working one after another; the phases and their status are below. Read `NATIVE.md` before changing the grammar, and the section on the corpus before changing the feature table.

## The product

A schema-aware SQL language server for people and for AI agents. Ruimte starts it for SQL files, and agent CLIs start it over stdio. Decisions that stand:

- Dialects SQLite, MySQL, MariaDB and PostgreSQL, from SQLite 3.47, MySQL 8.0, MariaDB 11.0 and PostgreSQL 18. The parser reads the union of all of them; the feature table in `crates/syntax/src/features.rs`, keyed by dialect and version, decides what is reported. A new version is new rows or a moved `Since`, nothing else.
- The server never opens a database connection and never holds a credential. Schema knowledge comes from schema snapshot files, a documented and versioned JSON format a host (such as Ruimte's daemon) writes, which the server reads and watches.
- Configuration only through standard LSP (`initializationOptions`, `workspace/didChangeConfiguration`, `workspace/configuration`): per file or folder a dialect, a version and a snapshot path, with a default. Nothing here knows Ruimte.
- Without a dialect a script is read permissively: lexed so dumps of every dialect read, and only what no dialect accepts is reported (`docs/configuration.md`).
- How it should behave follows how a top professional database IDE behaves (what completion offers and in which order, what an inspection flags). Clean-room: official database documentation and observed behavior only; never copy code from another SQL parser, formatter or language server, and never name another product in anything that goes into Git.

## Phases

| Phase | What | Status |
| --- | --- | --- |
| 1 | Repository, lexer and parser for the union of the dialects, feature table per dialect and version, reserved words, corpus against real servers, server with settings per file or folder, diagnostics, document symbols, folding and selection ranges, CI and release workflow | Done |
| 2 | Schema snapshots and settings, built-in catalogs, name resolution, completion, hover, definition, signature help, unresolved names | To do |
| 3 | References, rename, document highlights, semantic tokens, inlay hints, formatting | To do |
| 4 | Inspections with quick fixes | To do |
| 5 | A library interface for SQL embedded in another language, the release, measurements | To do |

### Phase 2: schema and resolution

- The snapshot format: a JSON document with a `formatVersion`, the dialect and version of the server it was taken from, and per schema the tables and views with their columns (name, type, nullability, default, generated, comment), keys, foreign keys, indexes, and the routines, types, enums, domains and sequences. Document it in `docs/snapshot-format.md` with a JSON Schema in the repository, versioned so a host can write the next version while an older server still reads what it knows. The setting `schema` (top level or per override) already resolves to `DocumentState::schema` in the server; read the file, watch it (`workspace/didChangeWatchedFiles` with dynamic registration, or polling the modification time as a fallback), and parse it again on change.
- A crate `sql-catalog`: the snapshot model and loader, and built-in catalogs per dialect and version (functions with their signatures, types, system schemas such as `information_schema` and `pg_catalog`), generated or written from the official documentation. A row of a catalog may carry a version range like the feature table does.
- Name resolution in `sql-analysis` over the tree described in `NATIVE.md`: scopes per query (the `FROM` tables and their aliases, `WITH` common table expressions visible to later ones and to the body, `LATERAL`, correlated subqueries reaching outward, select aliases visible in `ORDER BY` and, per dialect, in `GROUP BY` and `HAVING`), DML targets, `NEW` and `OLD` in triggers, routine parameters and variables in routine bodies, the default schema (`search_path` in PostgreSQL, the database of `USE` in MySQL, `main` and attached schemas in SQLite), and the tables a script itself creates before the statement at hand. Case rules per dialect (PostgreSQL folds unquoted names to lower case, MySQL table names depend on `lower_case_table_names`, SQLite is case-insensitive).
- Completion (keywords that fit the position, tables, columns of the tables in scope ranked first, aliases, functions with snippets, join conditions from foreign keys), hover (a column's type and table, a table's columns, a function's signature and documentation), definition (into the snapshot or the `CREATE` in the workspace), signature help, and diagnostics for unresolved tables and columns that only report when a snapshot is loaded.
- Keywords used as names keep their keyword kind inside `NAME` nodes; resolve on the text of the token in the `NAME`, not on its kind.

### Phase 3: editing

References and rename of tables, columns, aliases and common table expressions within a script and across the workspace's SQL files; document highlights; semantic tokens (keywords, names by what they resolve to, parameters, variables); inlay hints (the column a value of an `INSERT` goes to, parameter names of calls); a formatter in a crate `sql-format` that decides only the whitespace between tokens, with options for keyword case and the indentation of clauses.

### Phase 4: inspections

Inspections with quick fixes, for what a database IDE flags: unresolved names (already a diagnostic), ambiguous columns, a missing `GROUP BY` column, a `DELETE` or `UPDATE` without `WHERE`, `= NULL`, a constant condition, unused common table expressions and aliases, a type that does not fit, a `CAST` to a type the dialect does not take (MySQL takes a short list), reserved words as names with a fix that quotes them, deprecated syntax with a fix that rewrites it. Settings to switch an inspection off or change its severity, like the PHP server's `inspections` setting.

### Phase 5: embedding and release

A library interface so the PHP server can analyze SQL in PHP strings: a fragment with its offsets in the host document, placeholders (`?`, `:name`, `$1`, and interpolated host expressions that stand for a value or a name), and the mapping of every result back to the host. The lexer already takes `LexOptions`; a host will want to set them itself (MySQL's `ANSI_QUOTES` and `NO_BACKSLASH_ESCAPES`, which placeholders count). Then the release pipeline end to end (the workflows exist), measurements of the server on real projects in `MEASUREMENTS.md`, and the first release with a tag. Only the final phase tags.

## The core

What every language server does the same way comes from `basmilius/language-server-core` (usually checked out beside this one as `../core`): the line index and position encodings (`lsc-text`), the token cursor and tree builder the parser is written on (`lsc-syntax`), and documents, URIs, encoding negotiation, dispatch, the main loop and `main` (`lsc-server`). It is a Git dependency pinned to a tag in `[workspace.dependencies]` of `Cargo.toml`. What SQL means stays here; something every server needs goes to the core first, gets a new tag there, and this workspace moves its pin.

Two things this server would use from the core are not published yet: `Parser::bump_remap`, which consumes a token as another kind (so a keyword used as a name becomes an `IDENT` in the tree), and `Document::invalidate`, which drops a document's tree (`documents.rs` drops it now by applying no changes). Both were written and tested in a local checkout of the core during phase 1, which could not be pushed; ask Bas before relying on them, and move the pin once a tag has them.

To build against a local checkout of the core, put a `[patch]` in `.cargo/config.toml`, which `.gitignore` keeps out of Git:

```toml
[patch."https://github.com/basmilius/language-server-core"]
lsc-server = { path = "../core/crates/server" }
lsc-syntax = { path = "../core/crates/syntax" }
lsc-text = { path = "../core/crates/text" }
```

While it is there, run the checks without `--locked` and do not commit `Cargo.lock`.

## Checks

All of these pass before a commit; CI (`.github/workflows/ci.yml`) runs them on every push to main and every PR, the handshake on every platform.

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 scripts/test-native-release.py
cargo build --release --locked && python3 scripts/handshake.py target/release/sql-language-server
```

## The corpus

A change to the feature table or to what the parser accepts is checked against real servers before it is committed: `python3 scripts/dialect-corpus.py` (Docker, about five minutes) runs every case and every row's example on SQLite, MySQL 8.0 and 8.4, MariaDB 11 and PostgreSQL 18 and rewrites `crates/syntax/tests/data/dialects-verified.txt`; commit that file with the change. `cargo test` fails when a case was not run yet, when the parser contradicts a server, and when a difference in `dialects-known.txt` no longer differs. Add a case to `dialects.sql` for every construct you add to the grammar, with the fixture tables `t`, `u`, `s`, `a` and `b` of the script. `scripts/reserved-words.py` regenerates the reserved words from the servers. Stop any container you start (the scripts stop theirs unless `--keep`).

## Releases

- The version lives in `Cargo.toml` and `native-source.json`; `test-native-release.py` fails when they differ. The tag is `v<version>`.
- A release starts as a draft: `gh release create v<version> --draft --notes-file <notes>`, then `gh workflow run release.yml -f version=<version>`. The workflow tags the commit, builds every platform, attaches the archives, checksums and descriptor, and publishes the release last.
- Push and release only when Bas asks. The phases before the last do not tag.

## Documentation

- A change in behavior updates `NATIVE.md` (how and why, no numbers), `docs/` where a user sees it, and `docs/clients.md` when a client has to do something new.
- A measurement goes into `MEASUREMENTS.md` with its date, replacing the one it supersedes.

## Conventions

- Rust 2024, `rustfmt.toml`, `unsafe` forbidden, `rust-version` 1.85, so no let chains. `.editorconfig` is the rule for the rest.
- Tests are deterministic: no sleeps, no wall clock, no database. Snapshot tests use `expect-test`; `UPDATE_EXPECT=1 cargo test` rewrites them, and a rewritten snapshot is read before it is committed.
- American English everywhere. Never an em dash or an en dash.
- Comments say why, never what the code already says.
- Conventional commits in English. No attribution lines.
- Never name another product in code, comments, docs, test names or commits; the database products are of course named.
- Git: never `git stash`, `reset`, `checkout -- <path>`, `restore` or `clean`, and never rewrite a commit; a wrong commit gets a follow-up commit.
