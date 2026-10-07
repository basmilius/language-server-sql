# What a client provides

The server knows nothing of the editor or agent that starts it. Everything it can do depends on what the client sends and announces over standard LSP. A client that leaves something out still gets a working server, only without that feature.

## Documents

- Send `textDocument/didOpen`, `didChange` and `didClose` for SQL files. A `languageId` of `mysql`, `mariadb`, `postgres` or `sqlite` names the dialect when the settings do not; `sql` names none.
- Changes may be incremental. Positions are UTF-16 unless the client offers `utf-8` in `general.positionEncodings`.

## Settings

Settings come from `initializationOptions`, `workspace/didChangeConfiguration` and `workspace/configuration` (section `sqlLanguageServer`, with the document as `scopeUri`). See [configuration](./configuration.md). A client that answers `workspace/configuration` is asked for every document it opens and again after every `didChangeConfiguration`.

## Diagnostics

Diagnostics are pushed after a burst of changes has settled, or pulled with `textDocument/diagnostic` by a client that announces `textDocument.diagnostic`; then nothing is pushed. `workspace.diagnostics.refreshSupport` lets the server ask for new pulled diagnostics when the settings change.

## Symbols

A client that announces `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport` gets columns inside their table; any other client gets a flat list with the table as `containerName`.

## Schema

- Support dynamic registration of `workspace/didChangeWatchedFiles` and send the changes the server registers for: `**/*.sql` and the path of each snapshot file in use (an absolute path as the glob). Without it the server checks each snapshot's modification time after answering what was queued, and reads a `.sql` file again when the client sends `textDocument/didSave` for it.
- Show `window/showMessage`: a snapshot that cannot be read is told there once, and logged.
- A snapshot's path comes from the `schema` setting; nothing else needs to be sent.

## Completion, hover and signature help

- Announce `textDocument.completion.completionItem.snippetSupport` to get functions with a tab stop between their parentheses and `VALUES` templates with a tab stop per column; without it the parentheses are inserted empty and the templates hold the column names.
- Announce `labelDetailsSupport` to see a column's table and a table's schema beside the label; without it they follow the type in `detail`.
- Completion lists are cut at 500 items and marked incomplete; ask again as the word grows. Trigger characters are `.` and `@`.
- Hover and documentation are markdown. Signature help is triggered by `(` and `,`.

## References, highlights and rename

- `textDocument/references` reads the document and, for a table, column, routine, type, sequence or schema, every `.sql` file of the workspace of a dialect the document can name: open documents as they are in the editor, the others from the disk. `context.includeDeclaration` decides whether the DDL that defines the name is among them.
- `textDocument/documentHighlight` gives `Write` (3) for a definition and a write (the target of `SET`, an `INSERT` column list, the table an `INSERT`, `UPDATE` or `DELETE` changes, a variable assigned) and `Read` (2) for the rest.
- `textDocument/prepareRename` answers the name's range with the name as a placeholder, without quotes, or fails with a message the client shows: a built-in function, an object only a snapshot or the database itself has, a name no DDL in the workspace defines, a name also inside a string of SQL the server does not read. Send a new name bare (`order_total`) or quoted (`"Order Total"`); it is quoted where the dialect needs it, and an occurrence written in quotes keeps them.
- `textDocument/rename` answers a `WorkspaceEdit` over every file it changes. Announce `workspace.workspaceEdit.documentChanges` to get `documentChanges` with the version of each open document; without it the edit is `changes`. A rename that would make another name ambiguous or have it stand for something else fails with a message instead.

## Semantic tokens

Full and range requests are answered; there is no delta. The legend:

| Token type  | What it carries                                                                 |
| ----------- | ------------------------------------------------------------------------------- |
| `keyword`   | Keywords, also `NULL`, `TRUE` and `FALSE`                                       |
| `comment`   | Line and block comments                                                         |
| `string`    | Strings of every form, a routine body kept as a string included                 |
| `number`    | Numbers                                                                          |
| `operator`  | Operators; punctuation is left to the client's grammar                           |
| `function`  | Built-in functions and the routines of the schema                               |
| `type`      | Data types, built in or of the schema                                           |
| `namespace` | Schemas and databases                                                           |
| `class`     | Tables                                                                           |
| `interface` | Views and materialized views                                                    |
| `struct`    | Common table expressions                                                        |
| `property`  | Columns and the names select items give                                         |
| `variable`  | Aliases of tables (SQL's range variables), windows, sequences, the variables of a routine, user and system variables |
| `parameter` | Parameters of a routine and placeholders such as `?`, `$1` and `:name`          |

| Modifier         | When                                                                     |
| ---------------- | ------------------------------------------------------------------------ |
| `declaration`    | Where a name is defined: `CREATE TABLE t`, a column definition, `AS alias`, a common table expression |
| `readonly`       | A generated column                                                       |
| `deprecated`     | Syntax the feature table deprecates at the target, such as MySQL's `SQL_CALC_FOUND_ROWS` |
| `defaultLibrary` | What the database brings: built-in functions and types, system tables, schemas and variables |

A name nothing resolves is colored by where it stands. A token over several lines is given once per line. Announce `workspace.semanticTokens.refreshSupport` to be asked to pull again when a snapshot or the workspace's DDL changes.

## Inlay hints

`textDocument/inlayHint` answers hints of kind `Parameter` with the label before the value (`email:`) and padding on the right. The [settings](./configuration.md#inlay-hints) switch each kind off. Announce `workspace.inlayHint.refreshSupport` to be asked to pull again when the schema changes.

## Formatting

`textDocument/formatting` and `rangeFormatting` answer edits of whitespace and of the case of keywords only; a range is widened to whole lines. `textDocument/onTypeFormatting` is triggered by `;` and lays out the statement it ends. The request's `insertSpaces` decides tabs or spaces and its `tabSize` the width, unless the [settings](./configuration.md#formatting) give one. A statement with a syntax error is left as it is, and so is the whole text when a layout would change a token.

## Code actions

`textDocument/codeAction` answers `quickfix` actions for `unresolved-table`, `unresolved-column`, `unresolved-function` (a near miss of a known name) and `ambiguous-column` (qualify it), each with the diagnostic it fixes when the client sent it in `context.diagnostics`, and `refactor.rewrite` actions: qualify a column with its table or alias, expand `*` into its columns, add an alias to a table, upper- or lowercase the keywords of a selection. `context.only` narrows them. Every action carries its edit; none needs a resolve.
