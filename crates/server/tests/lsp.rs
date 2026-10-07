//! Talks to the server over an in-memory connection, the way an editor talks to it over stdio.

use lsc_server::testing::TestClient as Client;
use lsp_server::{Message, Response};
use serde_json::{Value, json};

const URI: &str = "file:///work/db/query.sql";

fn start(capabilities: Value, options: Value) -> (Client, Value) {
    Client::connect(
        sql_language_server::run,
        json!({
            "processId": null,
            "rootUri": "file:///work",
            "capabilities": capabilities,
            "initializationOptions": options
        }),
    )
}

fn messages(diagnostics: &[Value]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|found| found["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn codes(diagnostics: &[Value]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|found| found["code"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn initializes_with_the_capabilities_it_has() {
    let (client, result) = start(json!({}), Value::Null);
    let capabilities = &result["capabilities"];
    assert_eq!(capabilities["textDocumentSync"]["change"], 2);
    assert_eq!(capabilities["documentSymbolProvider"], true);
    assert_eq!(capabilities["foldingRangeProvider"], true);
    assert_eq!(capabilities["selectionRangeProvider"], true);
    assert_eq!(capabilities["hoverProvider"], true);
    assert_eq!(capabilities["definitionProvider"], true);
    assert_eq!(capabilities["referencesProvider"], true);
    assert_eq!(capabilities["documentHighlightProvider"], true);
    assert_eq!(capabilities["renameProvider"]["prepareProvider"], true);
    assert_eq!(
        capabilities["completionProvider"]["triggerCharacters"],
        json!([".", "@"])
    );
    assert_eq!(
        capabilities["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","])
    );
    assert_eq!(capabilities["positionEncoding"], "utf-16");
    assert!(capabilities["diagnosticProvider"].is_null());
    assert_eq!(result["serverInfo"]["name"], "sql-language-server");
    client.shutdown();
}

#[test]
fn negotiates_utf8_positions() {
    let (client, result) = start(
        json!({ "general": { "positionEncodings": ["utf-16", "utf-8"] } }),
        Value::Null,
    );
    assert_eq!(result["capabilities"]["positionEncoding"], "utf-8");
    client.shutdown();
}

#[test]
fn publishes_syntax_errors_and_clears_them_after_an_incremental_change() {
    let (mut client, _) = start(json!({}), Value::Null);
    client.open_document(URI, "sql", "SELECT * FROM;\n");
    let found = client.diagnostics(URI);
    assert_eq!(messages(&found), ["Table name expected"]);
    assert_eq!(found[0]["severity"], 1);
    assert_eq!(found[0]["source"], "sql");
    assert_eq!(found[0]["code"], "syntax");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": URI, "version": 2 },
            "contentChanges": [{ "range": { "start": { "line": 0, "character": 13 }, "end": { "line": 0, "character": 13 } }, "text": " t" }]
        }),
    );
    assert!(client.diagnostics(URI).is_empty());
    client.notify("textDocument/didClose", json!({ "textDocument": { "uri": URI } }));
    assert!(client.diagnostics(URI).is_empty(), "closing clears the diagnostics");
    client.shutdown();
}

#[test]
fn the_dialect_and_version_come_from_the_initialization_options() {
    let text = "SELECT a FROM t INTERSECT SELECT a FROM u LIMIT 5, 10;\n";
    let (mut client, _) = start(json!({}), json!({ "dialect": "mysql", "version": "8.0.30" }));
    client.open_document(URI, "sql", text);
    assert_eq!(
        messages(&client.diagnostics(URI)),
        ["INTERSECT and EXCEPT are only available since MySQL 8.0.31"]
    );
    client.shutdown();
    let (mut client, _) = start(json!({}), json!({ "sqlLanguageServer": { "dialect": "postgres" } }));
    client.open_document(URI, "sql", text);
    assert_eq!(codes(&client.diagnostics(URI)), ["limit-with-comma"]);
    client.shutdown();
}

#[test]
fn an_override_by_folder_wins_over_the_default() {
    let options = json!({
        "dialect": "postgres",
        "overrides": [{ "path": "db", "dialect": "sqlite" }]
    });
    let (mut client, _) = start(json!({}), options);
    let text = "SELECT a::int FROM t;\n";
    client.open_document(URI, "sql", text);
    assert_eq!(codes(&client.diagnostics(URI)), ["cast-operator"]);
    client.open_document("file:///work/other/query.sql", "sql", text);
    assert!(client.diagnostics("file:///work/other/query.sql").is_empty());
    client.shutdown();
}

#[test]
fn the_language_id_names_a_dialect_when_the_settings_do_not() {
    let (mut client, _) = start(json!({}), Value::Null);
    client.open_document(URI, "mysql", "SELECT \"a string\" FROM t RETURNING a;\n");
    let found = client.diagnostics(URI);
    assert_eq!(codes(&found), ["syntax"]);
    client.open_document("file:///work/b.sql", "sql", "SELECT a FROM t QUALIFY a > 1;\n");
    assert_eq!(codes(&client.diagnostics("file:///work/b.sql")), ["qualify"]);
    client.shutdown();
}

#[test]
fn pushed_settings_read_every_open_document_again() {
    let (mut client, _) = start(json!({}), Value::Null);
    client.open_document(URI, "sql", "SELECT `a` FROM t;\n");
    assert!(client.diagnostics(URI).is_empty());
    client.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "sqlLanguageServer": { "dialect": "postgres" } } }),
    );
    assert_eq!(codes(&client.diagnostics(URI)), ["backtick-identifiers"]);
    client.shutdown();
}

#[test]
fn asks_a_client_that_answers_per_document() {
    let (mut client, _) = start(json!({ "workspace": { "configuration": true } }), Value::Null);
    client.open_document(
        URI,
        "sql",
        "MERGE INTO t USING s ON t.a = s.a WHEN MATCHED THEN DELETE;\n",
    );
    let request = client.wait_for(|message| match message {
        Message::Request(request) if request.method == "workspace/configuration" => Some(request.clone()),
        _ => None,
    });
    assert_eq!(request.params["items"][0]["section"], "sqlLanguageServer");
    assert_eq!(request.params["items"][0]["scopeUri"], URI);
    assert!(
        client.diagnostics(URI).is_empty(),
        "read without a dialect until the answer arrives"
    );
    client.send(Response::new_ok(
        request.id,
        json!([{ "dialect": "mariadb", "version": "11.4" }]),
    ));
    assert_eq!(codes(&client.diagnostics(URI)), ["merge"]);
    client.shutdown();
}

#[test]
fn a_client_that_pulls_diagnostics_gets_them_on_request() {
    let (mut client, result) = start(
        json!({ "textDocument": { "diagnostic": {} } }),
        json!({ "dialect": "sqlite" }),
    );
    assert_eq!(result["capabilities"]["diagnosticProvider"]["identifier"], "sql");
    client.open_document(URI, "sql", "TRUNCATE TABLE t;\n");
    let report = client.request("textDocument/diagnostic", json!({ "textDocument": { "uri": URI } }));
    assert_eq!(report["kind"], "full");
    assert_eq!(codes(report["items"].as_array().expect("items")), ["truncate"]);
    client.shutdown();
}

#[test]
fn unreadable_settings_are_logged() {
    let (mut client, _) = start(json!({}), json!({ "dialect": "oracle" }));
    let message = client.wait_for(|message| match message {
        Message::Notification(notification) if notification.method == "window/logMessage" => {
            Some(notification.params["message"].as_str().unwrap_or_default().to_string())
        }
        _ => None,
    });
    assert!(message.contains("Unknown dialect 'oracle'"), "{message}");
    client.shutdown();
}

#[test]
fn answers_symbols_folds_and_selection_ranges() {
    let (mut client, _) = start(
        json!({ "textDocument": { "documentSymbol": { "hierarchicalDocumentSymbolSupport": true } } }),
        Value::Null,
    );
    let text = "CREATE TABLE users (\n  id int,\n  name text\n);\nSELECT id\nFROM users;\n";
    client.open_document(URI, "sql", text);
    let symbols = client.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": URI } }));
    assert_eq!(symbols[0]["name"], "users");
    assert_eq!(symbols[0]["kind"], 23, "a table is a struct");
    assert_eq!(symbols[0]["children"][1]["name"], "name");
    assert_eq!(symbols[0]["children"][1]["detail"], "text");
    assert_eq!(symbols[1]["name"], "SELECT FROM users");
    let folds = client.request("textDocument/foldingRange", json!({ "textDocument": { "uri": URI } }));
    let lines: Vec<(u64, u64)> = folds
        .as_array()
        .expect("folds")
        .iter()
        .map(|fold| (fold["startLine"].as_u64().unwrap(), fold["endLine"].as_u64().unwrap()))
        .collect();
    assert_eq!(lines, [(0, 3), (0, 2), (4, 5)]);
    let selection = client.request(
        "textDocument/selectionRange",
        json!({ "textDocument": { "uri": URI }, "positions": [{ "line": 1, "character": 3 }] }),
    );
    assert_eq!(selection[0]["range"]["start"], json!({ "line": 1, "character": 2 }));
    assert_eq!(selection[0]["range"]["end"], json!({ "line": 1, "character": 4 }));
    client.shutdown();
}

#[test]
fn flat_symbols_for_a_client_that_cannot_nest() {
    let (mut client, _) = start(json!({}), Value::Null);
    client.open_document(URI, "sql", "CREATE TABLE t (a int);\n");
    let symbols = client.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": URI } }));
    let names: Vec<&str> = symbols
        .as_array()
        .expect("symbols")
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, ["t", "a"]);
    assert_eq!(symbols[1]["containerName"], "t");
    client.shutdown();
}

#[test]
fn a_request_for_an_unknown_method_fails() {
    let (mut client, _) = start(json!({}), Value::Null);
    let message = client.request_error("textDocument/codeLens", json!({ "textDocument": { "uri": URI } }));
    assert!(message.contains("textDocument/codeLens"), "{message}");
    client.shutdown();
}
