# Maintaining

The server is a Cargo workspace of five crates under the Functional Source License (`FSL-1.1-MIT`), with `unsafe` code forbidden.

| Crate                 | Folder            | What it holds                                                                           |
| --------------------- | ----------------- | --------------------------------------------------------------------------------------- |
| `sql-syntax`          | `crates/syntax`   | Dialects and versions, the lexer, a lossless parser with error recovery, the feature table and the reserved words |
| `sql-catalog`         | `crates/catalog`  | The schema model, snapshot files and the built-in catalogs of each dialect and version  |
| `sql-analysis`        | `crates/analysis` | Diagnostics, symbols, folding, selection, DDL, name resolution, completion, hover, definition, signature help, unknown names, references, rename, highlights, semantic tokens, inlay hints and code actions, without LSP types |
| `sql-format`          | `crates/format`   | The formatter: whitespace and the case of keywords, held to the very same tokens          |
| `sql-language-server` | `crates/server`   | The stdio server: documents, settings per file or folder and LSP conversions            |

What any language server does the same way (the line index, the parser's token cursor and tree builder, documents, URIs, dispatch, the main loop) comes from [`basmilius/language-server-core`](https://github.com/basmilius/language-server-core), a Git dependency pinned to a tag in `[workspace.dependencies]`.

## Checks

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 scripts/test-native-release.py
cargo build --release --locked
python3 scripts/handshake.py target/release/sql-language-server
```

The Cargo tests need no database and no network. The handshake starts the real binary and checks its version, the handshake, UTF-8 positions, document symbols and a clean shutdown. CI (`.github/workflows/ci.yml`) runs all of them, the handshake on every platform.

## The corpus against real servers

`crates/syntax/tests/data/dialects.sql` holds statements, and the example of every row of the feature table counts as a statement too. `scripts/dialect-corpus.py` runs each on SQLite (Python's own), MySQL 8.0 and 8.4, MariaDB 11 and PostgreSQL 18 in Docker, against the same fixture tables and from a fresh database each time, and records in `dialects-verified.txt` whether each server ran it (`ok`), rejected it as a syntax error (`syntax`) or failed on something else (`error:<code>`). `cargo test` then holds the parser and the feature table to that record without any server: what the parser accepts must not be a syntax error on the server, and what it rejects must not run. Differences that are deliberate are listed with their reason in `dialects-known.txt`; a listed difference that no longer differs fails the test too.

```sh
python3 scripts/dialect-corpus.py            # starts the containers, runs every case, stops them
python3 scripts/dialect-corpus.py --keep --log outcomes.json
python3 scripts/reserved-words.py --keep     # regenerates crates/syntax/src/reserved_words.rs
```

Run the corpus after a change to the feature table or the grammar, and commit the record with it. A new row of the table brings its example to the corpus by itself; a new case goes into `dialects.sql`.

## The inspection corpus

`crates/analysis/tests/data/inspections.sql` holds statements an inspection reports as an error, each with the inspection it is about (`-- expect:`), and statements that look like them and run (`-- expect: none`), after a fixture per dialect. `scripts/inspection-corpus.py` runs each on the same servers as the dialect corpus, in a fresh database after the fixture, and records what each did in `inspections-verified.txt`. `cargo test` then holds the inspections to that record: an inspection that reports an error claims the server rejects the statement, and the inspection a case names must report an error exactly where the server rejects it. Deliberate differences are listed with their reason in `inspections-known.txt`.

```sh
python3 scripts/inspection-corpus.py            # starts the containers, runs every case, stops them
python3 scripts/inspection-corpus.py --keep --log outcomes.json
```

Run it after a change to an inspection that reports errors, or to its severities, and commit the record with it. A new error an inspection reports gets a case, and a case that runs next to it.

## The built-in catalogs

`crates/catalog/data/<dialect>.tsv` hold what the servers have: functions with their parameters and versions, types, system tables and views with their columns, settings. `scripts/catalog.py` takes them from PostgreSQL 18, MySQL 8.0 and 8.4, MariaDB 11.0, 11.4 and 11.8 in Docker and from the SQLite shell of three Alpine releases and of 3.47, which it builds from the amalgamation with the compile options of Alpine's package, in about a minute; it stops the containers it starts unless `--keep`, and `--only <dialect>` takes one dialect's catalog again. Run it when a server version is added, and commit the files with what changed. `crates/catalog/data/descriptions.tsv` is written by hand, a line per function: a one-line description of our own, and parameters and a return type where the server gives none. A test fails when a line adds a function to a dialect without a signature.

```sh
python3 scripts/catalog.py [--keep] [--only sqlite]
```

## Measuring

```sh
cargo bench -p sql-syntax                     # lexing, parsing and the feature table
cargo bench -p sql-analysis --bench schema    # a snapshot of 5,000 tables: reading it, completion, hover, inspections
cargo bench -p sql-analysis --bench editing   # semantic tokens, inlay hints, inspections, highlights; references and rename over 2,000 files
cargo bench -p sql-format                     # formatting a large and a typical script
```

`cargo run -p sql-format --example format -- file.sql --dialect mysql` prints a file formatted, or the first token a layout would change when it refuses one.

## Releases

A release is tagged `v<version>`, the version in `Cargo.toml` and `native-source.json`, so bump both first. Releases are immutable once published, so a release starts as a draft:

```sh
gh release create v0.1.0 --draft --notes-file notes.md
gh workflow run release.yml -f version=0.1.0
```

The workflow tags the commit it runs on, builds and checks every platform, attaches the archives, checksums and descriptor to the draft, and publishes the release last.
