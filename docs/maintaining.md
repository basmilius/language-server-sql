# Maintaining

The server is a Cargo workspace of three crates under the Functional Source License (`FSL-1.1-MIT`), with `unsafe` code forbidden.

| Crate                 | Folder            | What it holds                                                                           |
| --------------------- | ----------------- | --------------------------------------------------------------------------------------- |
| `sql-syntax`          | `crates/syntax`   | Dialects and versions, the lexer, a lossless parser with error recovery, the feature table and the reserved words |
| `sql-analysis`        | `crates/analysis` | Diagnostics, document symbols, folding and selection ranges, without LSP types          |
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

## Releases

A release is tagged `v<version>`, the version in `Cargo.toml` and `native-source.json`, so bump both first. Releases are immutable once published, so a release starts as a draft:

```sh
gh release create v0.1.0 --draft --notes-file notes.md
gh workflow run release.yml -f version=0.1.0
```

The workflow tags the commit it runs on, builds and checks every platform, attaches the archives, checksums and descriptor to the draft, and publishes the release last.
