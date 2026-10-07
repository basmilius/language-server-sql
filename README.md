# language-server-sql

A language server for SQL, written in Rust, that speaks LSP over stdio. It reads SQLite 3.47 and newer, MySQL 8.0 and newer, MariaDB 11.0 and newer and PostgreSQL 18, with one parser for all of them and a table that says which syntax each version of each dialect accepts. It never connects to a database and never holds a credential.

This is the first phase: syntax. It reports syntax errors, syntax a dialect or version does not accept and reserved words used as names, and answers document symbols, folding ranges and selection ranges. Knowledge of a schema, completion, navigation and the rest follow; [CLAUDE.md](./CLAUDE.md) lists the phases.

## Install

Build it with Rust 1.85 or newer:

```sh
cargo build --release --locked
```

The binary lands in `target/release/sql-language-server`. No database is needed, to build or to run.

```sh
sql-language-server --stdio
```

## Documentation

| Page                                          | What it covers                                                      |
| --------------------------------------------- | ------------------------------------------------------------------- |
| [Getting started](./docs/getting-started.md)  | Building, starting and connecting                                   |
| [Configuration](./docs/configuration.md)      | Dialect, version and schema snapshot per file or folder             |
| [Features](./docs/features.md)                | What it answers and what it reports                                 |
| [Clients](./docs/clients.md)                  | What an editor sends and announces for each feature                 |
| [Distribution](./docs/distribution.md)        | Release archives and the descriptor an installer pins               |
| [Maintaining](./docs/maintaining.md)          | The crates, checks, the corpus against real servers and releases    |

[NATIVE.md](./NATIVE.md) describes how the implementation works and why, and [MEASUREMENTS.md](./MEASUREMENTS.md) what it was measured to do.

## License

[Functional Source License, Version 1.1, MIT Future License](./LICENSE). The database servers the corpus check runs are downloaded separately and keep their own licenses; see [THIRD-PARTY.md](./THIRD-PARTY.md).
