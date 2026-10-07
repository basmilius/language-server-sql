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
