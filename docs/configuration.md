# Configuration

Settings come over standard LSP only: `initializationOptions`, `workspace/didChangeConfiguration` and, for a client that announces `workspace.configuration`, `workspace/configuration` with the section `sqlLanguageServer` and the document as `scopeUri`. Each takes the settings bare or under `sqlLanguageServer`.

```json
{
    "dialect": "mysql",
    "version": "8.0.36",
    "schema": "schemas/app.json",
    "overrides": [
        { "path": "db/analytics", "dialect": "postgres", "version": "18" },
        { "path": "db/analytics/legacy.sql", "dialect": "mariadb" },
        { "path": "/srv/local/cache.sql", "dialect": "sqlite", "schema": "file:///srv/local/schema.json" }
    ]
}
```

| Key          | What it says                                                                                                              |
| ------------ | ------------------------------------------------------------------------------------------------------------------------- |
| `dialect`    | `sqlite`, `mysql`, `mariadb`, `postgres` (also `postgresql`, `pgsql`) or `generic`                                        |
| `version`    | The version of the server, such as `8.4`, `8.0.36`, `11.4` or `3.47.2`. Without one, the newest the server knows          |
| `schema`     | A [schema snapshot](./snapshot-format.md) file, absolute, a `file:` URI or relative to the first workspace folder         |
| `overrides`  | Entries with a `path` (a file or folder: absolute, a `file:` URI or relative to the first workspace folder) and the keys above |
| `inlayHints` | Which inlay hints show, [below](#inlay-hints)                                                                             |
| `format`     | How the formatter lays a script out, [below](#formatting)                                                                 |
| `inspections` | Per inspection id, or per row of the feature table: `false` or `"off"`, a severity, or `{ "enabled", "severity" }`; [inspections](./inspections.md#settings) |

## How a document's dialect is found

1. The client's answer to `workspace/configuration` for the document, when it gives one, takes the place of the settings it pushed.
2. In those settings, the most specific override whose `path` holds the document wins, field by field; then the top level.
3. A `version` counts only at a level that names no dialect or the same dialect, so a MySQL version never applies to a folder set to PostgreSQL.
4. Without a dialect in the settings, the `languageId` of `textDocument/didOpen` decides when it names one (`mysql`, `mariadb`, `postgres`, `sqlite`); `sql` names none.
5. Otherwise the document is read without a dialect.

## Schema

A document's schema is what its `schema` snapshot holds, what the `.sql` files of the workspace define, and what the document itself defines before the statement at hand. The snapshot is read when a document first needs it and again when it changes; [the snapshot format](./snapshot-format.md) describes it and how a change is noticed. The `.sql` files are read in the background when the server starts; each is read in the dialect the settings give its path, and a document sees the files of its own dialect and those without one.

## Without a dialect

A script that nothing configures is lexed the way a dump of any dialect needs: backslash escapes in strings (with a guess at a backslash right before a closing quote, so `ESCAPE '\'` reads as the standard has it), dollar-quoted strings, nested comments and PostgreSQL's operators, MySQL's variables and `DELIMITER`, backticks, and the backslash commands of the clients. A double quote is an identifier and `#` an operator. Only what no dialect accepts is reported, such as `QUALIFY`, and words every dialect reserves.

A setting that cannot be read (an unknown dialect, a version that is no version) is logged with `window/logMessage` and left out.

## Inlay hints

```json
{ "inlayHints": { "insertColumns": true, "selectColumns": true, "parameterNames": false } }
```

| Key              | Shows                                                                                                                   | Default |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------- | ------- |
| `insertColumns`  | The column each value of an `INSERT ... VALUES` row goes to, when the statement lists no columns or four or more        | on      |
| `selectColumns`  | The column each item of an `INSERT ... SELECT` fills, where the item's own name differs                                 | on      |
| `parameterNames` | The parameter each argument of a call goes to, for the routines of the schema and for built-in functions of two or more arguments whose parameters the catalog names | on |

A hint that would repeat what is written, such as the value `name` for the column `name`, is left out. `inlayHints`, `format` and `inspections` are read at the top level of the settings, not in `overrides`; a client that answers `workspace/configuration` can still give a folder its own.

## Formatting

```json
{ "format": { "keywordCase": "upper", "indentWidth": 4, "commaPosition": "trailing" } }
```

| Key             | What it decides                                                                                 | Default            |
| --------------- | ----------------------------------------------------------------------------------------------- | ------------------ |
| `keywordCase`   | `upper`, `lower` or `preserve`: the case keywords are written in                                 | `upper`            |
| `indentWidth`   | Spaces per level of indentation                                                                  | the editor's tab size |
| `commaPosition` | `trailing` or `leading`: where the comma goes in a list laid out a line per item                 | `trailing`         |

Tabs or spaces come from the `insertSpaces` of the client's formatting request. [Features](./features.md#formatting) shows the layout.

## Inspections

```json
{ "inspections": { "missing-where": "off", "null-comparison": "error", "double-pipe": { "severity": "hint" } } }
```

Each key is the id of an inspection or of a row of the feature table, and each value `false` or `"off"`, `true` or `"on"`, `"error"`, `"warning"`, `"information"` or `"hint"`, or an object with `enabled` and `severity`. [Inspections](./inspections.md) lists the ids and their defaults. MySQL's and MariaDB's `sql_mode` is not a setting: it comes from the script's `SET sql_mode` or from the snapshot's `source.sqlMode`, and is the server's default otherwise.
