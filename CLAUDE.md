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
| 2 | Schema snapshots and settings, built-in catalogs, name resolution, completion, hover, definition, signature help, unresolved names | Done |
| 3 | References, rename, document highlights, semantic tokens, inlay hints, formatting, code actions | Done |
| 4 | Inspections with quick fixes | Done |
| 5 | A library interface for SQL embedded in another language, the release, measurements | To do |

### Phase 2: schema and resolution (done)

What was built, and what a later phase builds on (`NATIVE.md` has the details):

- `sql-catalog`: the schema model and snapshot reader (`docs/snapshot-format.md`, `docs/snapshot.schema.json`, held to the model by `crates/catalog/tests/format.rs`), and the built-in catalogs in `crates/catalog/data`, taken from the servers by `scripts/catalog.py` with descriptions written by hand in `descriptions.tsv`. A new server version is a new sample in the script, run again.
- `sql-analysis`: `catalog.rs` (layers of schema and the lookup rules per dialect), `ddl.rs` (what DDL defines, statement by statement), `context.rs` (the document's DDL before a statement over the snapshot and the workspace), `resolve.rs` (scopes and what a name stands for), `completion.rs`, `nav.rs` (hover, definition), `signature.rs`, `unresolved.rs` (now `inspections/names.rs`), `workspace.rs` (the DDL of the `.sql` files replayed in path order). Phase 3's references and rename can walk every `NAME` and ask `Resolver::resolve_name`.
- Precedence: the document's own DDL, then the snapshot, then the workspace's DDL, then the system schemas; a layer hides an object of the same name in the layers after it, whole.
- Unknown names are only reported where the schema is known; keep it that way, a file without a schema must stay quiet.
- The server reads snapshots lazily, watches them (or checks their modification time without watching), tells a broken one once with `window/showMessage`, and scans the workspace's `.sql` files in a thread.

### Phase 3: editing (done)

What was built, and what a later phase builds on (`NATIVE.md` has the details):

- `refs.rs`: `Symbol`, what a name stands for in a form that compares across statements and files (a local symbol by the range of the name that declares it; an object of the schema by kind, schema and name), `Namer::symbol_at` for any `NAME` (definitions read there, the rest asked of the resolver), and `find_hits`, every place a script names a set of symbols with how it uses each. Phase 4's unused aliases and common table expressions are a `find_hits` with no hit but the declaration.
- `references.rs` (references over the document and the workspace's other files, highlights), `rename.rs` (prepare, the refusals, the checks that the new name is free and not captured, quoting per occurrence; `apply` and `spell` are reusable for any edit of names), `semantic_tokens.rs` (the legend is in `docs/clients.md`), `inlay_hints.rs`, `actions.rs` (rewrites and quick fixes, the edit distance for near misses; phase 4's quick fixes belong here).
- `sql-format` (`crates/format`): `layout.rs` decides breaks and levels from the tree, `spacing.rs` the space on a line; `try_format` refuses any layout that changes a token. Its tests hold every corpus case, every feature example and the samples in `crates/format/tests/data` to the same tokens and to formatting once; add a sample there for a construct the layout learns.
- The server reads every `.sql` file of the workspace again from the disk for references and rename (`editing.rs`), and has settings `inlayHints` and `format`.
- `scripts/catalog.py --only <dialect>` takes one catalog again; SQLite 3.47 is built from its amalgamation since no Alpine release ships it.

### Phase 4: inspections (done)

What was built, and what a later phase builds on (`NATIVE.md` has the details, `docs/inspections.md` the list):

- `inspections/` in `sql-analysis`: `mod.rs` (the registry `INSPECTIONS` with ids, default severities and whether a fix may be applied everywhere; `InspectionSettings` by inspection id or feature row; `Request` with a range, whether to make fixes and one id to run; `Cx::report` and its `Pending` builder with a severity of the finding's own, related places and fixes made only when asked; `inspect` and `fix_all`), `suppress.rs` (`-- sql-suppress` and `-- sql-suppress-file` comments and the fixes that write them), `syntax.rs` (the feature table and reserved words as `unsupported-syntax`, `deprecated-syntax` and `reserved-word`, with the rewrites), `names.rs` (phase 2's unknown names, moved), `writes.rs`, `literals.rs`, `grouping.rs`, `conditions.rs`, `unused.rs`, `pitfalls.rs`, and `tree.rs` for what they share. A new inspection is a constant, a row of `INSPECTIONS`, a section of `docs/inspections.md` (a test checks every id is there) and, when it reports errors, cases in the inspection corpus.
- An error says the server rejects the statement: `scripts/inspection-corpus.py` runs `crates/analysis/tests/data/inspections.sql` on the servers and `cargo test` holds every error to the record (`inspections-verified.txt`, `inspections-known.txt`). Run it after changing an inspection that reports errors.
- `sql_mode.rs`: MySQL's and MariaDB's modes from `SET sql_mode` (in `ScriptState`), the snapshot's `source.sqlMode` or the default.
- `diagnostics()` is the syntax errors plus `inspect`; the diagnostic carries the inspection id as `code`, the feature row, the deprecated and unnecessary tags and related information. Code actions run the inspections in range for their fixes, add the suppressions, and a `source.fixAll.sql` action per inspection whose fix is safe everywhere, or one for all of them when a client asks for `source.fixAll`.
- The server has the `inspections` setting and sends `relatedInformation`, tags and `data.feature`.
- Phase 5's fragments will want `Request` with the host's own settings, and the inspections that read a whole statement (counts, grouping, unused names) to stay silent on a fragment that is not one.

### Phase 5: embedding and release

A library interface so the PHP server can analyze SQL in PHP strings: a fragment with its offsets in the host document, placeholders (`?`, `:name`, `$1`, and interpolated host expressions that stand for a value or a name), and the mapping of every result back to the host. The lexer already takes `LexOptions`; a host will want to set them itself (MySQL's `ANSI_QUOTES` and `NO_BACKSLASH_ESCAPES`, which placeholders count). Then the release pipeline end to end (the workflows exist), measurements of the server on real projects in `MEASUREMENTS.md`, and the first release with a tag. Only the final phase tags.

## The core

What every language server does the same way comes from `basmilius/language-server-core` (usually checked out beside this one as `../core`): the line index and position encodings (`lsc-text`), the token cursor and tree builder the parser is written on (`lsc-syntax`), and documents, URIs, encoding negotiation, dispatch, the main loop and `main` (`lsc-server`). It is a Git dependency pinned to a tag in `[workspace.dependencies]` of `Cargo.toml`. What SQL means stays here; something every server needs goes to the core first, gets a new tag there, and this workspace moves its pin.

The parser turns a keyword that names something into an `IDENT` with the core's `Parser::bump_remap`, and a document drops its tree when its dialect changes with `Document::invalidate`.

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

## The catalogs

`python3 scripts/catalog.py` (Docker, about a minute) rewrites `crates/catalog/data/<dialect>.tsv` from the servers; commit them with the change. A description in `descriptions.tsv` is a sentence of our own, never one copied from documentation; it gives parameters and a return type where the server reports none, and a function the server's catalog does not list (a form of the grammar) must have parameters.

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
