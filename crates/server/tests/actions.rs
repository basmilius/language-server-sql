//! Code actions over an in-memory connection.

use lsc_server::testing::TestClient as Client;
use serde_json::{Value, json};

const URI: &str = "file:///work/query.sql";

fn titles(actions: &Value) -> Vec<String> {
    actions
        .as_array()
        .expect("actions")
        .iter()
        .map(|action| action["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn quick_fixes_carry_their_diagnostic_and_rewrites_their_edits() {
    let (mut client, result) = Client::connect(
        sql_language_server::run,
        json!({ "processId": null, "rootUri": "file:///work", "capabilities": {}, "initializationOptions": { "dialect": "postgres" } }),
    );
    assert_eq!(
        result["capabilities"]["codeActionProvider"]["codeActionKinds"],
        json!(["quickfix", "refactor.rewrite"])
    );
    client.open_document(
        URI,
        "sql",
        "CREATE TABLE users (id int, email text);\nSELECT emial FROM users;\n",
    );
    let diagnostics = client.diagnostics(URI);
    assert_eq!(diagnostics.len(), 1);
    let range = json!({ "start": { "line": 1, "character": 7 }, "end": { "line": 1, "character": 7 } });
    let actions = client.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": URI }, "range": range, "context": { "diagnostics": diagnostics } }),
    );
    assert_eq!(titles(&actions), ["Change to 'email'"]);
    assert_eq!(actions[0]["kind"], "quickfix");
    assert_eq!(actions[0]["diagnostics"][0]["code"], "unresolved-column");
    assert_eq!(actions[0]["isPreferred"], true);
    assert_eq!(actions[0]["edit"]["changes"][URI][0]["newText"], "email");

    let range = json!({ "start": { "line": 1, "character": 18 }, "end": { "line": 1, "character": 18 } });
    let actions = client.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": URI }, "range": range, "context": { "diagnostics": [], "only": ["refactor"] } }),
    );
    assert_eq!(titles(&actions), ["Add the alias 'u'"]);
    let actions = client.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": URI }, "range": range, "context": { "diagnostics": [], "only": ["quickfix"] } }),
    );
    assert!(titles(&actions).is_empty());
    client.shutdown();
}
