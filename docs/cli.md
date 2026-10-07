# The command line

Besides the server, the binary has three commands for a person or an agent with only a shell: `check` prints diagnostics, `format` lays files out, and `describe` says what is known about a table, a column or a function. Without a command, or with `--stdio`, it is the language server; `--version` and `--help` work as before.

```sh
sql-language-server check [options] [--format human|json] [--severity <level>] <paths...>
sql-language-server format [options] [--check | --write] <paths...>
sql-language-server describe [options] [--format human|json] <name> [paths...]
```

A path is a file, a folder (every `.sql` file under it, leaving out hidden folders, `node_modules`, `vendor`, `target`, `dist` and `build`) or `-` for stdin. The DDL of the files named is the workspace schema, as the server reads the workspace's `.sql` files: `check db queries` resolves the queries against the tables the migrations in `db` create.

## Options

| Option | What it does |
| --- | --- |
| `--config <file>` | Settings as JSON, the shape the server takes over LSP ([configuration](./configuration.md)): `dialect`, `version`, `schema`, `overrides` per path, `inspections`, `format`. Relative paths in it are relative to the current folder. |
| `--dialect <name>` | `sqlite`, `mysql`, `mariadb`, `postgres` or `generic`, over the settings. |
| `--version <version>` | The version of the dialect, such as `8.4`, over the settings. |
| `--schema <file>` | A [schema snapshot](./snapshot-format.md), over the settings. |

## check

Prints every diagnostic: syntax errors and the findings of the [inspections](./inspections.md), with the settings' choices.

```text
$ sql-language-server check --dialect mysql db query.sql
query.sql:1:8: error[unresolved-column]: Unknown column 'emial'
query.sql:2:1: warning[missing-where]: DELETE without WHERE removes every row of 'users'
```

Lines and columns count from 1, columns in characters. A count goes to stderr. `--severity warning` leaves out information and hints. `--format json` prints one object:

```json
{
  "files": 2, "errors": 1, "warnings": 1, "information": 0, "hints": 0,
  "diagnostics": [
    {
      "path": "query.sql",
      "start": { "line": 1, "column": 8, "offset": 7 },
      "end": { "line": 1, "column": 13, "offset": 12 },
      "severity": "error",
      "code": "unresolved-column",
      "feature": null,
      "message": "Unknown column 'emial'"
    }
  ]
}
```

`offset` is in bytes. `code` is `syntax` or an inspection id, `feature` the row of the feature table behind `unsupported-syntax` and `deprecated-syntax`.

## format

Lays files out as the server's formatting does, with the `format` settings (`keywordCase`, `indentWidth`, `commaPosition`) and four spaces unless they say otherwise. Without a flag it prints the one file or stdin formatted; `--check` prints the files that would change; `--write` rewrites them and prints their names. A file whose layout would change a token is left as it is, with a note on stderr.

## describe

Prints what the server knows about a name, from the snapshot, the DDL of the paths after the name and the built-in catalog of the dialect and version, as hover shows it: a table with its columns, keys and comment, a column with its type and table, a function with its signatures and description. The name is `table`, `schema.table`, `table.column`, `schema.table.column`, a function (`lower`, or `lower()` for a function only) or a type.

```text
$ sql-language-server describe --dialect postgres lower()
**function** `lower`
...
```

`--format json` prints `{ "name", "kind", "markdown", "definition" }`, where `kind` is what the name is (`table`, `view`, `column`, `built-in function`, ...) and `definition` the table or column as the [snapshot format](./snapshot-format.md) writes it, or `null`.

## Exit codes

| Code | When |
| --- | --- |
| 0 | Success: no error found, every file formatted already or written, the name found |
| 1 | `check` found an error, `format --check` found a file to format, `describe` found nothing |
| 2 | The arguments, the settings, a file or the snapshot could not be read |
