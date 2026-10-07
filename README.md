# language-server-sql

A language server for SQL, written in Rust, that speaks LSP over stdio. It reads SQLite 3.47 and newer, MySQL 8.0 and newer, MariaDB 11.0 and newer and PostgreSQL 18, with one parser for all of them and a table that says which syntax each version of each dialect accepts. It never connects to a database and never holds a credential.

It reports syntax errors, syntax a dialect or version does not accept, reserved words used as names, and unknown tables, columns and functions. It completes tables, columns, join conditions from foreign keys, functions, types and keywords for the dialect and version, describes tables, columns and functions on hover, goes to definitions, and helps with the parameters of a call. It finds the references of a table, column, alias or routine across the workspace's `.sql` files, renames what DDL in them defines, highlights a name's reads and writes, colors names by what they stand for, shows the column each value of an `INSERT` goes to, formats scripts without ever changing a token, and offers rewrites and quick fixes. What it knows about a schema comes from a [snapshot file](./docs/snapshot-format.md) a host writes and from the DDL of the workspace's `.sql` files. Inspections follow; [CLAUDE.md](./CLAUDE.md) lists the phases.

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
| [Snapshot format](./docs/snapshot-format.md)  | The JSON a host writes for a database's schema                      |
| [Features](./docs/features.md)                | What it answers and what it reports                                 |
| [Clients](./docs/clients.md)                  | What an editor sends and announces for each feature                 |
| [Distribution](./docs/distribution.md)        | Release archives and the descriptor an installer pins               |
| [Maintaining](./docs/maintaining.md)          | The crates, checks, the corpus against real servers and releases    |

[NATIVE.md](./NATIVE.md) describes how the implementation works and why, and [MEASUREMENTS.md](./MEASUREMENTS.md) what it was measured to do.

## License

[Functional Source License, Version 1.1, MIT Future License](./LICENSE). The database servers the corpus check runs are downloaded separately and keep their own licenses; see [THIRD-PARTY.md](./THIRD-PARTY.md).
