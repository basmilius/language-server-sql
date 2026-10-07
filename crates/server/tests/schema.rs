//! The server with a schema: a snapshot file it reads and reads again, and the DDL of the
//! workspace's `.sql` files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use lsc_server::testing::TestClient as Client;
use lsp_server::Message;
use serde_json::{Value, json};

/// A folder of its own for a test, removed when the test ends.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Folder {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "sql-language-server-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("a test folder");
        Folder(path)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a folder");
        std::fs::write(&path, text).expect("a test file");
        path
    }

    fn uri(&self, name: &str) -> String {
        format!("file://{}", self.0.join(name).display())
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SNAPSHOT: &str = r#"{
    "formatVersion": 1,
    "source": { "dialect": "postgres", "version": "18.1" },
    "defaultSchema": "public",
    "schemas": [ {
        "name": "public",
        "tables": [
            { "name": "users", "comment": "People", "columns": [
                { "name": "id", "type": "integer", "nullable": false },
                { "name": "email", "type": "text", "comment": "Where mail goes" }
            ] }
        ]
    } ]
}"#;

fn start(folder: &Folder, capabilities: Value, options: Value) -> Client {
    Client::connect(
        sql_language_server::run,
        json!({
            "processId": null,
            "rootUri": format!("file://{}", folder.0.display()),
            "capabilities": capabilities,
            "initializationOptions": options
        }),
    )
    .0
}

/// Diagnostics of a document, waiting until they are what `done` wants.
fn diagnostics_until(client: &mut Client, uri: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    loop {
        let found = client.diagnostics(uri);
        if done(&found) {
            return found;
        }
    }
}

fn codes(found: &[Value]) -> Vec<String> {
    found
        .iter()
        .map(|found| found["code"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn labels(result: &Value) -> Vec<String> {
    result["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn watched(client: &Client, path: &Path, kind: u32) {
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [ { "uri": format!("file://{}", path.display()), "type": kind } ] }),
    );
}

#[test]
fn a_snapshot_gives_completion_hover_and_diagnostics_and_is_read_again_when_it_changes() {
    let folder = Folder::new();
    let snapshot = folder.write("schema.json", SNAPSHOT);
    let mut client = start(
        &folder,
        json!({
            "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } },
            "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } }
        }),
        json!({ "dialect": "postgres", "schema": "schema.json" }),
    );
    let registration = client.wait_for(|message| match message {
        Message::Request(request) if request.method == "client/registerCapability" => Some(request.params.clone()),
        _ => None,
    });
    assert_eq!(
        registration["registrations"][0]["registerOptions"]["watchers"][0]["globPattern"],
        "**/*.sql"
    );
    let uri = folder.uri("query.sql");
    client.open_document(&uri, "sql", "SELECT emial FROM users;\nSELECT * FROM orders;\n");
    let found = diagnostics_until(&mut client, &uri, |found| !found.is_empty());
    assert_eq!(codes(&found), ["unresolved-column", "unresolved-table"]);
    assert_eq!(found[0]["message"], "Unknown column 'emial'");
    let snapshot_watch = client.wait_for(|message| match message {
        Message::Request(request)
            if request.method == "client/registerCapability"
                && request.params["registrations"][0]["id"] != "sql-files" =>
        {
            Some(request.params.clone())
        }
        _ => None,
    });
    assert_eq!(
        snapshot_watch["registrations"][0]["registerOptions"]["watchers"][0]["globPattern"],
        snapshot.display().to_string()
    );

    let completion = client.at("textDocument/completion", &uri, 1, 14);
    assert_eq!(labels(&completion)[0], "users");
    let hover = client.at("textDocument/hover", &uri, 0, 19);
    let text = hover["contents"]["value"].as_str().expect("markdown");
    assert!(text.starts_with("**table** `public.users`\n\nPeople"), "{text}");
    assert!(
        client.at("textDocument/definition", &uri, 0, 19).is_null(),
        "a snapshot table has no place in a file"
    );

    folder.write(
        "schema.json",
        &SNAPSHOT
            .replace(
                r#"{ "name": "id", "type": "integer", "nullable": false },"#,
                r#"{ "name": "id", "type": "integer", "nullable": false }, { "name": "emial" },"#,
            )
            .replace(r#""tables": ["#, r#""tables": [ { "name": "orders", "columns": [] },"#),
    );
    watched(&client, &snapshot, 2);
    let found = diagnostics_until(&mut client, &uri, |found| found.is_empty());
    assert!(found.is_empty());
    client.shutdown();
}

#[test]
fn a_broken_snapshot_is_told_once_and_does_not_stop_the_server() {
    let folder = Folder::new();
    folder.write(
        "schema.json",
        "{ \"formatVersion\": 1, \"schemas\": [ { \"tables\": 3 } ] }",
    );
    let mut client = start(
        &folder,
        json!({}),
        json!({ "dialect": "postgres", "schema": "schema.json" }),
    );
    let uri = folder.uri("a.sql");
    client.open_document(&uri, "sql", "SELECT 1 FROM t;\n");
    let message = client.wait_for(|message| match message {
        Message::Notification(notification) if notification.method == "window/showMessage" => {
            Some(notification.params["message"].as_str().unwrap_or_default().to_string())
        }
        _ => None,
    });
    assert!(message.starts_with("Not a schema snapshot: "), "{message}");
    assert!(client.diagnostics(&uri).is_empty());
    client.open_document(&folder.uri("b.sql"), "sql", "SELECT 2;\n");
    assert!(client.diagnostics(&folder.uri("b.sql")).is_empty());
    let shown = client
        .backlog
        .iter()
        .filter(|message| matches!(message, Message::Notification(notification) if notification.method == "window/showMessage"))
        .count();
    assert_eq!(shown, 0, "told once");
    let completion = client.at("textDocument/completion", &uri, 0, 14);
    assert!(labels(&completion).is_empty() || !labels(&completion).contains(&"users".to_string()));
    client.shutdown();
}

#[test]
fn without_watching_a_changed_snapshot_is_noticed_on_the_next_message() {
    let folder = Folder::new();
    folder.write("schema.json", SNAPSHOT);
    let mut client = start(
        &folder,
        json!({}),
        json!({ "dialect": "postgres", "schema": "schema.json" }),
    );
    let uri = folder.uri("query.sql");
    client.open_document(&uri, "sql", "SELECT name FROM users;\n");
    let found = diagnostics_until(&mut client, &uri, |found| !found.is_empty());
    assert_eq!(codes(&found), ["unresolved-column"]);
    folder.write(
        "schema.json",
        &SNAPSHOT.replace(
            r#""comment": "Where mail goes" }"#,
            r#""comment": "Where mail goes" }, { "name": "name", "type": "text" }"#,
        ),
    );
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "text": " " }]
        }),
    );
    let found = diagnostics_until(&mut client, &uri, |found| found.is_empty());
    assert!(found.is_empty());
    client.shutdown();
}

#[test]
fn the_ddl_of_the_workspace_defines_tables_without_a_snapshot() {
    let folder = Folder::new();
    folder.write(
        "migrations/001_users.sql",
        "CREATE TABLE users (\n    id integer PRIMARY KEY,\n    email text NOT NULL\n);\n",
    );
    folder.write("migrations/002_name.sql", "ALTER TABLE users ADD COLUMN name text;\n");
    let mut client = start(&folder, json!({}), json!({ "dialect": "postgres" }));
    let uri = folder.uri("query.sql");
    client.open_document(&uri, "sql", "SELECT name, nickname FROM users;\n");
    let found = diagnostics_until(&mut client, &uri, |found| !found.is_empty());
    assert_eq!(found[0]["message"], "Unknown column 'nickname'");
    let definition = client.at("textDocument/definition", &uri, 0, 28);
    assert_eq!(definition[0]["uri"], folder.uri("migrations/001_users.sql"));
    assert_eq!(definition[0]["range"]["start"], json!({ "line": 0, "character": 13 }));
    let definition = client.at("textDocument/definition", &uri, 0, 8);
    assert_eq!(definition[0]["uri"], folder.uri("migrations/002_name.sql"));
    let completion = client.at("textDocument/completion", &uri, 0, 7);
    let items = labels(&completion);
    assert_eq!(&items[..3], ["id", "email", "name"]);
    let help = client.request(
        "textDocument/signatureHelp",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 7 } }),
    );
    assert!(help.is_null());
    client.shutdown();
}

#[test]
fn signature_help_for_a_built_in_function() {
    let folder = Folder::new();
    let mut client = start(&folder, json!({}), json!({ "dialect": "postgres" }));
    let uri = folder.uri("query.sql");
    client.open_document(&uri, "sql", "SELECT left('abc', 2);\n");
    let help = client.request(
        "textDocument/signatureHelp",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 19 } }),
    );
    assert_eq!(help["signatures"][0]["label"], "left(text, integer): text");
    assert_eq!(help["activeParameter"], 1);
    client.shutdown();
}

#[test]
fn a_quote_triggers_completion_of_the_whole_quoted_name() {
    let folder = Folder::new();
    folder.write("schema.json", SNAPSHOT);
    let mut client = start(
        &folder,
        json!({}),
        json!({ "dialect": "postgres", "schema": "schema.json", "completion": { "quoteIdentifiers": "always" } }),
    );
    let uri = folder.uri("query.sql");
    client.open_document(
        &uri,
        "sql",
        "SELECT * FROM \"\"\nWHERE id = 1;\nSELECT a[1] FROM t;\nSELECT * FROM us",
    );
    let mut complete = |line: u32, character: u32, trigger: Option<&str>| {
        let context = match trigger {
            Some(trigger) => json!({ "triggerKind": 2, "triggerCharacter": trigger }),
            None => json!({ "triggerKind": 1 }),
        };
        client.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": context
            }),
        )
    };
    let quoted = complete(0, 15, Some("\""));
    let users = &quoted["items"][0];
    assert_eq!(users["label"], "users");
    assert_eq!(users["filterText"], "\"users");
    assert_eq!(users["textEdit"]["newText"], "\"users\"");
    assert_eq!(
        users["textEdit"]["range"],
        json!({ "start": { "line": 0, "character": 14 }, "end": { "line": 0, "character": 16 } })
    );
    assert_eq!(labels(&complete(2, 9, Some("["))), Vec::<String>::new());
    let bare = complete(3, 16, None);
    assert_eq!(
        bare["items"][0]["textEdit"]["newText"], "\"users\"",
        "the setting quotes every name"
    );
}
