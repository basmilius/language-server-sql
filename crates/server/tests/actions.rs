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
        json!(["quickfix", "refactor.rewrite", "source.fixAll.sql"])
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
    assert_eq!(
        titles(&actions),
        [
            "Change to 'email'",
            "Suppress 'unresolved-column' for this statement",
            "Suppress 'unresolved-column' for the file"
        ]
    );
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

#[test]
fn inspections_follow_the_settings_and_fix_a_whole_document() {
    let (mut client, _) = Client::connect(
        sql_language_server::run,
        json!({
            "processId": null,
            "rootUri": "file:///work",
            "capabilities": {},
            "initializationOptions": { "dialect": "mysql", "inspections": { "missing-where": "off", "double-pipe": "hint" } }
        }),
    );
    client.open_document(
        URI,
        "sql",
        "DELETE FROM jobs;\nSELECT a FROM t WHERE a = NULL OR b <> NULL OR c || d;\nSELECT `x` FROM t x JOIN t y ON x.a = y.a;\n",
    );
    let diagnostics = client.diagnostics(URI);
    let codes: Vec<&str> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(codes, ["null-comparison", "null-comparison", "deprecated-syntax"]);
    let pipes = &diagnostics[2];
    assert_eq!(pipes["severity"], 4, "a hint, as the settings say");
    assert_eq!(pipes["tags"], json!([2]), "deprecated");
    assert_eq!(pipes["data"]["feature"], "double-pipe");

    let at = json!({ "start": { "line": 1, "character": 22 }, "end": { "line": 1, "character": 22 } });
    let actions = client.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": URI }, "range": at, "context": { "diagnostics": diagnostics } }),
    );
    let all = actions
        .as_array()
        .expect("actions")
        .iter()
        .find(|action| action["kind"] == "source.fixAll.sql")
        .expect("a fix-all action next to the quick fix");
    assert_eq!(all["title"], "Fix all 'null-comparison' problems in the file (2)");
    assert_eq!(
        all["edit"]["changes"][URI],
        json!([
            { "range": { "start": { "line": 1, "character": 22 }, "end": { "line": 1, "character": 30 } }, "newText": "a IS NULL" },
            { "range": { "start": { "line": 1, "character": 34 }, "end": { "line": 1, "character": 43 } }, "newText": "b IS NOT NULL" }
        ])
    );

    let everything = json!({ "start": { "line": 0, "character": 0 }, "end": { "line": 3, "character": 0 } });
    let on_save = client.request(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": URI }, "range": everything, "context": { "diagnostics": [], "only": ["source.fixAll"] } }),
    );
    assert_eq!(titles(&on_save), ["Fix all problems that have a safe fix"]);
    assert_eq!(on_save[0]["edit"]["changes"][URI].as_array().map(Vec::len), Some(3));
    client.shutdown();
}

#[test]
fn a_duplicate_names_the_first_in_its_related_information() {
    let (mut client, _) = Client::connect(
        sql_language_server::run,
        json!({ "processId": null, "rootUri": "file:///work", "capabilities": {}, "initializationOptions": { "dialect": "postgres" } }),
    );
    client.open_document(URI, "sql", "WITH a AS (SELECT 1), a AS (SELECT 2) SELECT * FROM a;\n");
    let diagnostics = client.diagnostics(URI);
    let duplicate = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["code"] == "duplicate-cte")
        .expect("the duplicate");
    assert_eq!(duplicate["severity"], 1);
    assert_eq!(duplicate["relatedInformation"][0]["location"]["uri"], URI);
    assert_eq!(
        duplicate["relatedInformation"][0]["location"]["range"]["start"],
        json!({ "line": 0, "character": 5 })
    );
    client.shutdown();
}
