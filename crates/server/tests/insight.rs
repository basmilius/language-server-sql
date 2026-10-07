//! Semantic tokens and inlay hints over an in-memory connection.

use lsc_server::testing::TestClient as Client;
use serde_json::{Value, json};

const URI: &str = "file:///work/query.sql";

fn start(options: Value) -> (Client, Value) {
    Client::connect(
        sql_language_server::run,
        json!({
            "processId": null,
            "rootUri": "file:///work",
            "capabilities": {},
            "initializationOptions": options
        }),
    )
}

#[test]
fn semantic_tokens_of_a_document_and_of_a_range() {
    let (mut client, result) = start(json!({ "dialect": "postgres" }));
    let legend = &result["capabilities"]["semanticTokensProvider"]["legend"];
    let types: Vec<&str> = legend["tokenTypes"]
        .as_array()
        .expect("types")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(types[0], "keyword");
    assert!(types.contains(&"class") && types.contains(&"property"));
    assert_eq!(result["capabilities"]["semanticTokensProvider"]["range"], true);
    client.open_document(URI, "sql", "CREATE TABLE t (a int);\nSELECT a FROM t;\n");
    let full = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": URI } }),
    );
    let data: Vec<u64> = full["data"]
        .as_array()
        .expect("data")
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let class = types.iter().position(|name| *name == "class").expect("class") as u64;
    assert_eq!(&data[..5], [0, 0, 6, 0, 0], "CREATE is a keyword");
    assert_eq!(&data[10..15], [0, 6, 1, class, 1], "t is a table, declared");
    let range = client.request(
        "textDocument/semanticTokens/range",
        json!({ "textDocument": { "uri": URI }, "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 8 } } }),
    );
    let data = range["data"].as_array().expect("data");
    assert_eq!(data.len(), 10, "SELECT and a");
    assert_eq!(data[0], 1, "the first token is on the second line");
    client.shutdown();
}

#[test]
fn inlay_hints_follow_the_settings() {
    let (mut client, _) = start(json!({ "dialect": "postgres" }));
    client.open_document(
        URI,
        "sql",
        "CREATE TABLE t (a int, b int);\nINSERT INTO t VALUES (1, 2);\nSELECT make_date(2026, 10, 7);\n",
    );
    let params = json!({ "textDocument": { "uri": URI }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 3, "character": 0 } } });
    let hints = client.request("textDocument/inlayHint", params.clone());
    let labels: Vec<&str> = hints
        .as_array()
        .expect("hints")
        .iter()
        .filter_map(|hint| hint["label"].as_str())
        .collect();
    assert_eq!(labels, ["a:", "b:", "year:", "month:", "day:"]);
    assert_eq!(hints[0]["position"], json!({ "line": 1, "character": 22 }));
    client.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "dialect": "postgres", "inlayHints": { "parameterNames": false } } }),
    );
    let hints = client.request("textDocument/inlayHint", params);
    assert_eq!(hints.as_array().map(Vec::len), Some(2));
    client.shutdown();
}
