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
| `schema`     | A schema snapshot file. It is read from the next phase on; until then it is accepted and kept                            |
| `overrides`  | Entries with a `path` (a file or folder: absolute, a `file:` URI or relative to the first workspace folder) and the keys above |

## How a document's dialect is found

1. The client's answer to `workspace/configuration` for the document, when it gives one, takes the place of the settings it pushed.
2. In those settings, the most specific override whose `path` holds the document wins, field by field; then the top level.
3. A `version` counts only at a level that names no dialect or the same dialect, so a MySQL version never applies to a folder set to PostgreSQL.
4. Without a dialect in the settings, the `languageId` of `textDocument/didOpen` decides when it names one (`mysql`, `mariadb`, `postgres`, `sqlite`); `sql` names none.
5. Otherwise the document is read without a dialect.

## Without a dialect

A script that nothing configures is lexed the way a dump of any dialect needs: backslash escapes in strings (with a guess at a backslash right before a closing quote, so `ESCAPE '\'` reads as the standard has it), dollar-quoted strings, nested comments and PostgreSQL's operators, MySQL's variables and `DELIMITER`, backticks, and the backslash commands of the clients. A double quote is an identifier and `#` an operator. Only what no dialect accepts is reported, such as `QUALIFY`, and words every dialect reserves.

A setting that cannot be read (an unknown dialect, a version that is no version) is logged with `window/logMessage` and left out.
