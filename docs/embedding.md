# Embedding

The crate `sql-embed` (`crates/embed`) is how another language server reads SQL inside its own strings: the queries a PHP project writes in `$db->query('...')`, `$statement = $db->prepare("...")`, heredocs and the methods of a query builder. The host finds the strings and decides which of them are SQL; `sql-embed` answers what the SQL language server answers for a `.sql` file (diagnostics, completion, hover, definition, signature help, semantic tokens, highlights, inlay hints, code actions, references and rename), in byte offsets of the host document, with every edit escaped for the host string. Nothing in its signatures is LSP.

A host depends on it as a Git dependency pinned to a tag of this repository:

```toml
sql-embed = { git = "https://github.com/basmilius/language-server-sql", tag = "v0.1.2" }
```

## The parts

| Type | What it is |
| --- | --- |
| `Settings` | Dialect, version, snapshot path, `sql_mode`, inspection settings, inlay hint options and whether `?` is a placeholder. `Settings::from_json` reads the server's own settings shape. |
| `Snapshot` | A schema snapshot read once (`Snapshot::load(path)`, `Snapshot::parse(json)`), shared by cloning (an `Arc`). |
| `Workspace` | The DDL of the workspace's `.sql` files for one dialect, per file: `scan(dialect, roots)`, `set_file(path, text)`, `remove_file(path)`, and `schema()` for the shared `WorkspaceSchema`. |
| `Environment` | `Environment::new(settings, snapshot, workspace_schema)`: what every fragment is read against. Cheap to clone, `Send + Sync`. |
| `Fragment` | The SQL as the host holds it: pieces, holes, the kind, the tables in scope. |
| `Analysis` | `Analysis::new(&env, &fragment)`: the fragment parsed once; every question is a method. `Send + Sync`. |
| `confidence(text, dialect)`, `Fragment::confidence(dialect)` | How much a string looks like SQL, from 0 to 1. |

A file change is a new `Snapshot` or `Workspace::schema()` and a new `Environment`; analyses already made keep the environment they were made with, so a host can rebuild without locking. `Analysis` holds no reference into the fragment and may be cached per string until the document changes.

## Building a fragment

A fragment is a list of pieces in the order of the host document, each with its host offsets:

| Method | For |
| --- | --- |
| `literal(raw, host_start, style)` | The source text of (part of) a literal, without its quotes, as it stands in the host. Its escapes are read by `style`. |
| `literal_dedented(raw, host_start, style, indent, at_line_start)` | The body of a heredoc whose closing marker is indented by `indent` bytes: that much leading space or tab is left out of every line, as PHP leaves it out of the value. `at_line_start` says whether `raw` starts a line (it does not right after an interpolation). |
| `text(text, host_start, style)` | Text the host already knows has no escapes: SQL and host bytes are the same. |
| `escape(decoded, host_span, style)` | One escape sequence the host decoded itself. |
| `hole(host_span, kind)` | An interpolation (`$id`, `{$user->id}`) or a concatenated part that is not a literal (`. $where .`, `. implode(',', $ids) .`). |
| `table(ScopeTable)` | A table the fragment sees, for a partial fragment. |

The escape styles are named after PHP's literals; another host picks the one whose rules its strings follow:

| `EscapeStyle` | Escapes it reads | What an edit writes escaped |
| --- | --- | --- |
| `SingleQuoted` | `\'`, `\\`; any other backslash is itself | `'` and `\` |
| `DoubleQuoted` | `\n \t \r \v \e \f \\ \$ \"`, octal `\0` to `\377`, `\xHH`, `\u{HHHH}` | `"`, `\` and `$` |
| `Heredoc` | as `DoubleQuoted`, but `\"` keeps its backslash | `\` and `$` |
| `Doubled(quote)` | the quote written twice | the quote, doubled |
| `Verbatim` | none (nowdoc) | nothing |

### The contract

- Pieces go in in increasing host order. Whatever lies between them in the host (quotes, ` . `, the code of an interpolation) is never in the SQL and never edited.
- Text pieces are mapped byte for byte; an escape is mapped as a whole: an offset inside `\'` is its start, an answer that covers the escape covers all of it. So a host offset maps back exactly, whatever the escapes.
- Holes are written into the SQL as a placeholder of their kind and nothing is reported about them: no diagnostic that touches a hole, no semantic token in a hole, no edit into or across one.
- An edit is only made within one stretch of one literal: contiguous in the host, one style, no hole inside. An action any of whose edits would reach further is left out; a completion item whose word runs into a hole is left out.
- An empty literal still goes in (`literal("", offset, style)`), so a cursor between its quotes maps.
- Offsets are bytes of the host document, `u32`. A host offset inside a UTF-8 character is read as the start of it.

### Holes

| `HoleKind` | For | Written as |
| --- | --- | --- |
| `Value` | `"... WHERE id = $id"` | a parameter (`?`, `$1` in PostgreSQL) |
| `Identifier` | `"FROM {$prefix}users"` | a name, `hole__1`, joined to the text around it |
| `List` | `IN (" . implode(',', $ids) . ")` | a name, which reads as a value and as a column |
| `Unknown` | `$sql . $where` | nothing, a value or a name, whichever reads with the fewest syntax errors and unsupported syntax |

A statement with a `List` or `Unknown` hole is not judged as a whole: the inspections that count columns or values, read grouping, want a `WHERE` or find unused names stay silent for it. An `Unknown` hole may join a table (`'SELECT o.title FROM users ' . $join`), so in its statement no column and no qualifier is reported as unknown; a table of `FROM` still is. A statement whose `Unknown` hole still leaves a syntax error reports nothing at all.

### Kinds

`FragmentKind::Statements` is one or more whole statements. The other kinds are the parts a query builder takes; the fragment is read inside a statement written around it, from the tables `table()` names (with their aliases), or from a table nothing defines when there are none, so its columns are open and nothing is reported about them:

| Kind | For | Read as |
| --- | --- | --- |
| `Condition` | `->where('status = ?')`, a join's condition | `SELECT * FROM <tables> WHERE <fragment>` |
| `Having` | `->having('count(*) > ?')` | `SELECT * FROM <tables> HAVING <fragment>` |
| `SelectList` | `->select('id, name')` | `SELECT <fragment> FROM <tables>` |
| `Expression` | `->selectRaw('count(*)')` | as `SelectList` |
| `OrderBy` | `->orderBy('created_at desc')` | `SELECT * FROM <tables> ORDER BY <fragment>` |
| `GroupBy` | `->groupBy('org_id')` | `SELECT * FROM <tables> GROUP BY <fragment>` |
| `TableReference` | `->from('users u')`, `->join('orgs o', ...)` | `SELECT * FROM <tables>, <fragment>` |
| `SetList` | the assignments of an update | `UPDATE <first table> SET <fragment>` |
| `Clauses` | `$sql .= ' WHERE status = ? ORDER BY id'`: joins, `WHERE`, `GROUP BY`, `HAVING`, `ORDER BY` or `LIMIT` written apart from the query they end | `SELECT * FROM <tables> <fragment>` |

A qualifier a partial fragment does not define (`orders.status` in `->where()`) may name a table of the query around it that the host does not see, and is not reported. Nothing is reported about the text written around a fragment: a syntax error there is reported at the start or the end of the fragment, on its side (`->select('id,')` ends where `FROM` follows, `->select(', id')` misses an item before its comma). The inspections that judge a whole statement are silent for every partial kind; `-- sql-suppress` comments work in a fragment as in a file, and the quick fixes that would write one before a partial fragment's statement are left out.

### Placeholders

`?`, `?1`, `:name`, `$1` and SQLite's `@name` are what a host's database layer takes, so the rows of the feature table about them are off unless the settings name them. With `question_placeholders` (the default), a `?` outside strings and comments is a placeholder also in PostgreSQL, which would read `a=?` as an operator: the SQL text has `$1` there, mapped to the `?`.

## Settings

`Settings::from_json` reads `dialect`, `version`, `schema` (a path the host resolves and loads with `Snapshot::load`), `sqlMode`, `inspections` (as in [configuration](./configuration.md#inspections)), `inlayHints` and `questionPlaceholders`, bare or under `sqlLanguageServer`, and gives what it could not read. `sql_mode` is MySQL's and MariaDB's mode of the connection, which decides some inspections (a column missing from `GROUP BY`, `||`, double quotes, strict inserts); without it the snapshot's `source.sqlMode` or the server's default holds.

## The answers

All offsets are host offsets; `Span` is `{ start, end }`.

| Method | Gives |
| --- | --- |
| `diagnostics()` | `Diagnostic { span, message, severity, code, deprecated, unnecessary, feature, related }`; `code` is `syntax` or an [inspection](./inspections.md) id |
| `completion(offset, CompletionOptions)` | `CompletionList { items, incomplete }`; each item has `edit: Edit { span, new_text }` with `new_text` escaped for the host string, and for a snippet escaped once more for the snippet syntax |
| `hover(offset)` | `Hover { span, markdown }` |
| `definition(offset)` | `Location::Fragment { span, name }` in the host, or `Location::File { path, span, name }` in a `.sql` file of the workspace (byte offsets of the file as it was read); what only a snapshot has has no place |
| `signature_help(offset)` | `SignatureHelp`, as the server gives it |
| `semantic_tokens()` | `SemanticToken { span, ty, modifiers }`, indexing `TOKEN_TYPES` and `TOKEN_MODIFIERS` (the [legend](./clients.md#semantic-tokens)); a token over an escape is one span, a token over two literals one span per literal |
| `highlights(offset)` | `Highlight { span, access }` with `Access::{Declaration, Read, Write}` |
| `inlay_hints()` | `InlayHint { offset, label, kind }` |
| `code_actions(span)` | `CodeAction { title, kind, edits, preferred, fixes }`: quick fixes, suppressions and rewrites |
| `fix_all()` | One action with the fixes that are safe everywhere |
| `references(offset)` | `References { span, symbol, hits }` within the fragment |
| `hits(&symbol)` | Where this fragment names a symbol another fragment or file found |
| `references_in_files(&symbol, &[SqlFile])` | Where `.sql` files the host reads name it |
| `prepare_rename(offset)`, `rename(offset, new_name)` | The rename of a name the fragment declares (an alias, a common table expression, a column alias); a table or column of the schema is refused, since its definition is elsewhere |
| `sql()`, `body()`, `to_sql(offset)` | The SQL text, where the fragment's own part of it is, and where a host offset is in it |

A `Symbol` of the schema (`Object`, `Column`, `Schema`, `Builtin`) compares equal across fragments and files, so a host finds the references of a table across its project by asking each analysis for `hits(&symbol)`; a `Local` symbol never leaves its fragment.

## Is it SQL?

`confidence(text, dialect)` gives 0 unless the text starts with the first word of a statement (after whitespace, comments and parentheses), and more for words of other clauses, the case of SQL rather than of a sentence (`SELECT` and `select`, not `Select`), its punctuation, and parsing without errors. `"SELECT * FROM users WHERE id = ?"` is 1, `"Select a file"` 0.45, `"Update failed"` 0.15. It is one signal; which strings are SQL stays the host's decision (the method a string is passed to, a variable's name, the user's choice). 0.6 is a reasonable line.

## A host, in outline

```text
on settings or a watched file change:
    settings = Settings::from_json(host_settings.sql)
    snapshot = settings.schema.map(|path| Snapshot::load(resolve(path)))     # report an Err once
    workspace = Workspace::scan(settings.dialect, workspace_folders)          # once; set_file on change
    env = Environment::new(settings, snapshot, workspace.schema())

for each string expression the host treats as SQL in a PHP document:
    fragment = Fragment::new(kind)        # Statements, or Condition for ->where(...), ...
    for each part of the expression, in order:
        'single quoted'      -> fragment.literal(inner, inner_start, SingleQuoted)
        "double quoted"      -> for each STRING_CONTENT: fragment.literal(text, start, DoubleQuoted)
                                for each $var or {$expr}: fragment.hole(span, Value or Identifier)
        <<<HEREDOC           -> fragment.literal_dedented(text, start, Heredoc, indent, at_line_start)
                                (leave out the newline before the closing marker)
        <<<'NOWDOC'          -> fragment.literal_dedented(text, start, Verbatim, indent, true)
        anything else        -> fragment.hole(span, Unknown, or List for implode(...))
    for a query builder: fragment.table(ScopeTable::new(model_table).with_alias(alias))
    analysis = Analysis::new(&env, &fragment)       # cache by document version and span

publish:     analysis.diagnostics()  -> host diagnostics, source "sql", code as given
completion:  if the cursor is inside the span of a fragment:
                 analysis.completion(cursor, options).items -> host items, edits as given
tokens:      analysis.semantic_tokens() -> map ty and modifiers through TOKEN_TYPES and
             TOKEN_MODIFIERS to the host's own legend; each span lies on one line of the host
```

A semantic token never crosses a line break of the host: SQL's own tokens are given per line, an escape such as `\n` is a line break of the SQL only, and spans from two literals are never joined.
