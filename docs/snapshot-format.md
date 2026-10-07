# Schema snapshots

The server never connects to a database. What it knows about a database's schema comes from a snapshot: a JSON file a host writes, such as an editor that holds the connection, a script in a project, or an agent's tooling. The `schema` setting points a file or folder at a snapshot ([configuration](./configuration.md)). [`snapshot.schema.json`](./snapshot.schema.json) is the JSON Schema of the format.

## Versions

`formatVersion` is required; this page describes version 1. A server reads a snapshot of any version as far as it knows the fields: fields it does not know are ignored, and so is a kind it does not know (a table of kind `hypertable` reads as a table). A host can therefore write a newer version, with more in it, and an older server still reads the rest. A change an older server would misread is never made under this key.

## The smallest snapshot

Everything but the names is optional:

```json
{
  "formatVersion": 1,
  "schemas": [
    { "name": "public", "tables": [ { "name": "users", "columns": [ { "name": "id" }, { "name": "email" } ] } ] }
  ]
}
```

That is enough for completion of tables and columns, for hover, and for diagnostics of unknown tables and columns. Types, keys, comments and routines make the answers richer: types and comments in completion and hover, join conditions from foreign keys, `VALUES` templates without generated columns, enum values in comparisons, signature help for routines.

## A full snapshot

```json
{
  "formatVersion": 1,
  "source": {
    "dialect": "postgres",
    "product": "PostgreSQL",
    "version": "18.1",
    "database": "shop",
    "takenAt": "2026-10-07T09:00:00Z"
  },
  "defaultSchema": "public",
  "searchPath": ["public"],
  "schemas": [
    {
      "name": "public",
      "comment": "The shop",
      "tables": [
        {
          "name": "orgs",
          "columns": [
            { "name": "id", "type": "integer", "nullable": false, "generated": "identityByDefault", "ordinal": 1 },
            { "name": "name", "type": "text", "nullable": false, "ordinal": 2 }
          ],
          "primaryKey": { "name": "orgs_pkey", "columns": ["id"] }
        },
        {
          "name": "users",
          "kind": "table",
          "comment": "People who log in",
          "columns": [
            { "name": "id", "type": "bigint", "nullable": false, "default": "nextval('users_id_seq'::regclass)", "autoIncrement": true, "ordinal": 1 },
            { "name": "org_id", "type": "integer", "nullable": false, "ordinal": 2 },
            { "name": "email", "type": "character varying(255)", "nullable": false, "comment": "Where mail goes", "ordinal": 3 },
            { "name": "mood", "type": "mood", "ordinal": 4 },
            { "name": "email_domain", "type": "text", "generated": "stored", "generationExpression": "split_part(email, '@', 2)", "ordinal": 5 }
          ],
          "primaryKey": { "name": "users_pkey", "columns": ["id"] },
          "uniqueKeys": [ { "name": "users_email_key", "columns": ["email"] } ],
          "indexes": [ { "name": "users_email_lower", "columns": ["lower((email)::text)"], "unique": true, "method": "btree", "predicate": "org_id IS NOT NULL" } ],
          "foreignKeys": [
            { "name": "users_org_fkey", "columns": ["org_id"], "referencedSchema": "public", "referencedTable": "orgs", "referencedColumns": ["id"], "onDelete": "cascade", "onUpdate": "no action" }
          ],
          "checks": [ { "name": "users_email_check", "expression": "email <> ''" } ]
        },
        {
          "name": "active_users",
          "kind": "view",
          "columns": [ { "name": "id", "type": "bigint" }, { "name": "email", "type": "character varying(255)" } ],
          "definition": "SELECT id, email FROM users WHERE mood <> 'sad'"
        }
      ],
      "sequences": [
        { "name": "users_id_seq", "type": "bigint", "start": 1, "increment": 1, "minValue": 1, "maxValue": 9223372036854775807, "cycle": false, "ownedBy": "users.id", "comment": "Ids of users" }
      ],
      "types": [
        { "name": "mood", "kind": "enum", "values": ["happy", "ok", "sad"], "comment": "How a user feels" },
        { "name": "email_address", "kind": "domain", "baseType": "text", "nullable": false, "default": "''" },
        { "name": "money_amount", "kind": "composite", "attributes": [ { "name": "amount", "type": "numeric" }, { "name": "currency", "type": "text" } ] }
      ],
      "routines": [
        {
          "name": "user_count",
          "kind": "function",
          "parameters": [ { "name": "org", "type": "integer", "mode": "in", "default": "NULL" } ],
          "returns": "bigint",
          "language": "sql",
          "comment": "How many users an organization has"
        }
      ],
      "triggers": [
        { "name": "users_touch", "table": "users", "timing": "before", "events": ["update"], "comment": "Keeps updated_at" }
      ]
    }
  ]
}
```

## What the fields mean

The JSON Schema describes every field. What a host should know besides:

- **Names** are written as the database stores them: PostgreSQL's unquoted names in lower case, a quoted `"Users"` as `Users`. The server folds unquoted names in a script the way the dialect does and compares them with these.
- **`source.dialect`** names the dialect of the server. The dialect a document is read in comes from the settings, not from the snapshot, so a snapshot of MariaDB can serve documents set to MySQL.
- **`defaultSchema`** is what an unqualified name means: the current database of MySQL and MariaDB (`DATABASE()`), the first schema of PostgreSQL's search path, `main` in SQLite. Without it, PostgreSQL uses `public`, SQLite `main`, and MySQL and MariaDB the only database of the snapshot, or every database when there are several.
- **`searchPath`** is PostgreSQL's effective path as `current_schemas(false)` gives it. `pg_catalog` comes first unless the path names it, as on the server.
- **`source.lowerCaseTableNames`** is MySQL's and MariaDB's `lower_case_table_names`. With `0` table and database names compare with case, as on Linux; with `1`, `2` or nothing they compare without case. Column names always compare without case there.
- **`source.sqlMode`** is MySQL's and MariaDB's `@@sql_mode` as the server gives it. Inspections that depend on it (a column missing from `GROUP BY`, `||` as `OR`, double quotes, strict inserts) follow it; without it they assume the server's default: `ONLY_FULL_GROUP_BY` and strict in MySQL, strict without `ONLY_FULL_GROUP_BY` in MariaDB.
- **SQLite** has a schema `main`, `temp` when the snapshot has temporary tables, and one per attached database.
- **`columns`** are in the order of the table. With an `ordinal` on every column the server orders them by it.
- **`type`** of a column is the declared type as the server writes it. MySQL's `enum('a','b')` and `set(...)` give their values to completion; so does a PostgreSQL column whose type names an enum of `types`.
- **`autoIncrement`** and **`generated`** leave a column out of the `VALUES` template completion offers.
- **`invisible`** marks a MySQL or MariaDB invisible column (`EXTRA` says `INVISIBLE`): `SELECT *` leaves it out, an `INSERT` without a column list does not count it, and neither does the list of every column completion offers, the `VALUES` template or expanding `*`. A statement may still name it.
- **`foreignKeys`** give the join conditions completion offers after `JOIN`; without `referencedColumns` the referenced table's primary key is meant.
- **`routines`** with the same name are overloads.

## Taking a snapshot

Any way that fills the fields works. The sources on the servers:

- PostgreSQL: `pg_namespace`, `pg_class` with `pg_attribute` (or `information_schema.columns`), `format_type()` for declared types, `pg_get_expr()` for defaults, `pg_constraint` for keys and checks, `pg_index`, `pg_sequence`, `pg_type` with `pg_enum`, `pg_proc` with `pg_get_function_arguments()` and `pg_get_function_result()`, `pg_trigger`, `obj_description()` and `col_description()` for comments.
- MySQL and MariaDB: `information_schema.SCHEMATA`, `TABLES`, `COLUMNS` (`COLUMN_TYPE`, `COLUMN_DEFAULT`, `EXTRA`, `COLUMN_COMMENT`), `STATISTICS`, `KEY_COLUMN_USAGE` with `REFERENTIAL_CONSTRAINTS`, `CHECK_CONSTRAINTS`, `ROUTINES` with `PARAMETERS`, `TRIGGERS`, and `@@lower_case_table_names`.
- SQLite: `sqlite_schema`, `pragma_table_xinfo()`, `pragma_index_list()` with `pragma_index_info()`, `pragma_foreign_key_list()`, and `pragma_database_list()` for attached databases.

## When the file changes

The server reads a snapshot when a document first needs it and asks the client to watch the file. When the client reports a change, the server reads it again and publishes the diagnostics of every open document again; a client that does not watch files has the file's modification time checked whenever the server has answered what was queued. A snapshot that cannot be read (not JSON, no `formatVersion`, an object without a name) is told once with `window/showMessage` and a log message, and the documents are read as if there were none, until the file reads again.
