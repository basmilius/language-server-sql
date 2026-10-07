//! Formatting a document, a range of it and a statement as its `;` is typed.

use lsc_server::testing::TestClient as Client;
use serde_json::{Value, json};

const URI: &str = "file:///work/query.sql";

fn start(options: Value) -> Client {
    Client::connect(
        sql_language_server::run,
        json!({ "processId": null, "rootUri": "file:///work", "capabilities": {}, "initializationOptions": options }),
    )
    .0
}

/// The text with LSP edits applied, for edits on one line each as the formatter's are not.
fn apply(text: &str, edits: &Value) -> String {
    let offset = |position: &Value| -> usize {
        let line = position["line"].as_u64().unwrap_or_default() as usize;
        let character = position["character"].as_u64().unwrap_or_default() as usize;
        let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
        start + character
    };
    let mut edits: Vec<(usize, usize, String)> = edits
        .as_array()
        .expect("edits")
        .iter()
        .map(|edit| {
            (
                offset(&edit["range"]["start"]),
                offset(&edit["range"]["end"]),
                edit["newText"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
    let mut out = text.to_string();
    for (start, end, new) in edits {
        out.replace_range(start..end, &new);
    }
    out
}

#[test]
fn formats_a_document_a_range_and_a_typed_statement() {
    let mut client =
        start(json!({ "dialect": "postgres", "format": { "keywordCase": "lower", "commaPosition": "leading" } }));
    let text = "SELECT a, b FROM t;\nselect c from u;\n";
    client.open_document(URI, "sql", text);
    let options = json!({ "tabSize": 2, "insertSpaces": true });
    let edits = client.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": URI }, "options": options }),
    );
    assert_eq!(apply(text, &edits), "select\n  a\n  , b\nfrom t;\nselect c\nfrom u;\n");
    let edits = client.request(
        "textDocument/rangeFormatting",
        json!({ "textDocument": { "uri": URI }, "options": options, "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 3 } } }),
    );
    assert_eq!(apply(text, &edits), "SELECT a, b FROM t;\nselect c\nfrom u;\n");
    let edits = client.request(
        "textDocument/onTypeFormatting",
        json!({ "textDocument": { "uri": URI }, "options": { "tabSize": 4, "insertSpaces": false }, "position": { "line": 0, "character": 19 }, "ch": ";" }),
    );
    assert_eq!(apply(text, &edits), "select\n\ta\n\t, b\nfrom t;\nselect c from u;\n");
    client.shutdown();
}

#[test]
fn a_broken_statement_is_left_alone() {
    let mut client = start(json!({ "dialect": "mysql" }));
    let text = "select  1;\nselect from where;\n";
    client.open_document(URI, "sql", text);
    let edits = client.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": URI }, "options": { "tabSize": 4, "insertSpaces": true } }),
    );
    assert_eq!(apply(text, &edits), "SELECT 1;\nselect from where;\n");
    client.shutdown();
}
