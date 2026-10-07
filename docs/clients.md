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
