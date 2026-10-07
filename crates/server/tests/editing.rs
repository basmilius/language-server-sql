//! References, document highlights and rename, over the open document and the workspace's
//! `.sql` files.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use lsc_server::testing::TestClient as Client;
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

fn start(folder: &Folder, capabilities: Value) -> Client {
    Client::connect(
        sql_language_server::run,
        json!({
            "processId": null,
            "rootUri": format!("file://{}", folder.0.display()),
            "capabilities": capabilities,
            "initializationOptions": { "dialect": "postgres" }
        }),
    )
    .0
}

fn position(uri: &str, line: u32, character: u32) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } })
}

/// The references of a name, asked again until the background read of the workspace is in.
fn references_until(client: &mut Client, uri: &str, line: u32, character: u32, files: usize) -> Vec<Value> {
    loop {
        let mut params = position(uri, line, character);
        params["context"] = json!({ "includeDeclaration": true });
        let found = client.request("textDocument/references", params);
        let found = found.as_array().cloned().unwrap_or_default();
        let mut uris: Vec<&str> = found.iter().filter_map(|location| location["uri"].as_str()).collect();
        uris.dedup();
        if uris.len() >= files {
            return found;
        }
    }
}

#[test]
fn references_highlights_and_rename_across_the_workspace() {
    let folder = Folder::new();
    folder.write(
        "migrations/001_users.sql",
        "CREATE TABLE users (\n    id integer PRIMARY KEY,\n    email text NOT NULL\n);\n",
    );
    folder.write("reports/active.sql", "SELECT u.email FROM users AS u;\n");
    let mut client = start(
        &folder,
        json!({ "workspace": { "workspaceEdit": { "documentChanges": true } } }),
    );
    let uri = folder.uri("query.sql");
    client.open_document(
        &uri,
        "sql",
        "SELECT email FROM users WHERE email <> '';\nUPDATE users SET email = '';\n",
    );

    let found = references_until(&mut client, &uri, 0, 19, 3);
    let places: Vec<String> = found
        .iter()
        .map(|location| {
            let file = location["uri"]
                .as_str()
                .unwrap_or_default()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            format!(
                "{file}:{}:{}",
                location["range"]["start"]["line"], location["range"]["start"]["character"]
            )
        })
        .collect();
    assert_eq!(
        places,
        [
            "query.sql:0:18",
            "query.sql:1:7",
            "001_users.sql:0:13",
            "active.sql:0:20"
        ]
    );

    let highlights = client.request("textDocument/documentHighlight", position(&uri, 0, 8));
    let kinds: Vec<(u64, u64)> = highlights
        .as_array()
        .expect("highlights")
        .iter()
        .map(|highlight| {
            (
                highlight["range"]["start"]["character"].as_u64().unwrap_or_default(),
                highlight["kind"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(kinds, [(7, 2), (30, 2), (17, 3)], "read in the query, written by SET");

    let prepared = client.request("textDocument/prepareRename", position(&uri, 0, 20));
    assert_eq!(prepared["placeholder"], "users");
    let mut params = position(&uri, 0, 20);
    params["newName"] = json!("people");
    let edit = client.request("textDocument/rename", params);
    let changes = edit["documentChanges"].as_array().expect("document changes");
    let files: Vec<String> = changes
        .iter()
        .map(|change| {
            let file = change["textDocument"]["uri"].as_str().unwrap_or_default();
            format!(
                "{} {}",
                file.rsplit('/').next().unwrap_or_default(),
                change["edits"].as_array().map_or(0, Vec::len)
            )
        })
        .collect();
    assert_eq!(files, ["query.sql 2", "001_users.sql 1", "active.sql 1"]);
    assert_eq!(changes[0]["textDocument"]["version"], 1);
    assert!(changes[1]["textDocument"]["version"].is_null());
    assert_eq!(changes[2]["edits"][0]["newText"], "people");

    let mut params = position(&uri, 0, 20);
    client.open_document(&folder.uri("other.sql"), "sql", "CREATE TABLE people (id int);\n");
    params["newName"] = json!("people");
    let message = client.request_error("textDocument/rename", params);
    assert!(
        message.starts_with("There is already a table named 'people' ("),
        "{message}"
    );
    assert!(message.ends_with("other.sql)"), "{message}");
    client.shutdown();
}

#[test]
fn a_rename_without_document_changes_is_a_map_of_edits() {
    let folder = Folder::new();
    let mut client = start(&folder, json!({}));
    let uri = folder.uri("query.sql");
    client.open_document(&uri, "sql", "SELECT t.a FROM tbl AS t;\n");
    let mut params = position(&uri, 0, 23);
    params["newName"] = json!("x");
    let edit = client.request("textDocument/rename", params);
    let edits = edit["changes"][&uri].as_array().expect("edits");
    assert_eq!(edits.len(), 2);
    let message = client.request_error("textDocument/prepareRename", position(&uri, 0, 2));
    assert_eq!(message, "There is no name to rename here");
    client.shutdown();
}
