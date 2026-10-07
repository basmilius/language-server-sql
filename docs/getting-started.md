# Getting started

## Build

The workspace needs Rust 1.85 or newer. From a checkout:

```sh
cargo build --release --locked
```

The first build fetches the crates from the registry and `basmilius/language-server-core` from GitHub. The binary is `target/release/sql-language-server`, with `.exe` on Windows.

## Start it

```sh
sql-language-server --stdio
```

Without arguments it starts the same way. `--version` (or `-V`) prints `sql-language-server 0.1.0` and `--help` the usage; an unknown argument exits with code 2. Stdout carries only the protocol, so read stderr apart.

## Connect

Spawn the binary and connect an LSP client over its stdio. The server picks UTF-8 positions when a client offers them and UTF-16 otherwise. Tell it the dialect in `initializationOptions`:

```json
{
    "processId": null,
    "rootUri": "file:///home/me/project",
    "capabilities": {},
    "initializationOptions": { "dialect": "postgres", "version": "18" }
}
```

Without a dialect, a document is read without one: it is lexed so that dumps of every dialect read well and only syntax no dialect accepts is reported. See [Configuration](./configuration.md) for versions, overrides per folder and schema snapshots.

## From an agent

An agent CLI that speaks LSP over stdio starts the server the same way and opens the files it wants checked with `textDocument/didOpen`. Diagnostics arrive as `textDocument/publishDiagnostics` after the open, or on `textDocument/diagnostic` for a client that announces it pulls them. Each diagnostic has `source: "sql"` and a `code`: `syntax`, `reserved-word`, or the id of a row of the feature table, such as `on-conflict` or `cast-operator`.
