//! The command line of the binary.

use std::process::Command;

#[test]
fn prints_its_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_sql-language-server"))
        .arg("--version")
        .output()
        .expect("the binary runs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("sql-language-server {}", env!("CARGO_PKG_VERSION"))
    );
}

/// A folder of its own under the temporary folder, removed when dropped.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new(name: &str) -> Folder {
        let path = std::env::temp_dir().join(format!("sql-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("db")).expect("a folder");
        std::fs::write(
            path.join("db/001_users.sql"),
            "CREATE TABLE users (id int PRIMARY KEY, email varchar(255) NOT NULL);\n",
        )
        .expect("a file");
        std::fs::write(path.join("query.sql"), "select emial from users where id = 1;\n").expect("a file");
        std::fs::write(
            path.join("settings.json"),
            r#"{ "dialect": "mysql", "format": { "keywordCase": "lower" } }"#,
        )
        .expect("a file");
        Folder(path)
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_sql-language-server"))
            .args(args)
            .current_dir(&self.0)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary runs");
        let mut input = child.stdin.take().expect("stdin");
        input.write_all(stdin.unwrap_or_default().as_bytes()).expect("writes");
        drop(input);
        let output = child.wait_with_output().expect("it ends");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn check_prints_diagnostics_and_fails_on_an_error() {
    let folder = Folder::new("check");
    let (code, out, err) = folder.run(&["check", "--config", "settings.json", "db", "query.sql"], None);
    assert_eq!(code, 1, "{err}");
    assert_eq!(out, "query.sql:1:8: error[unresolved-column]: Unknown column 'emial'\n");
    assert!(err.contains("2 files: 1 errors"), "{err}");
    let (code, out, _) = folder.run(
        &["check", "--dialect", "mysql", "--format", "json", "query.sql", "db"],
        None,
    );
    assert_eq!(code, 1);
    let report: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    assert_eq!(report["errors"], 1);
    assert_eq!(report["diagnostics"][0]["code"], "unresolved-column");
    assert_eq!(report["diagnostics"][0]["start"]["column"], 8);
    let (code, out, _) = folder.run(&["check", "--dialect", "postgres", "-"], Some("SELECT 1;\n"));
    assert_eq!((code, out.as_str()), (0, ""));
    let (code, _, err) = folder.run(&["check", "--dialect", "oracle", "query.sql"], None);
    assert_eq!(code, 2);
    assert!(err.contains("unknown dialect"), "{err}");
    let (code, _, err) = folder.run(&["check", "--schema", "missing.json", "query.sql"], None);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn format_prints_checks_or_writes() {
    let folder = Folder::new("format");
    let (code, out, _) = folder.run(&["format", "--config", "settings.json", "query.sql"], None);
    assert_eq!(code, 0);
    assert_eq!(out, "select emial\nfrom users\nwhere id = 1;\n");
    let (code, out, _) = folder.run(&["format", "--check", "query.sql", "db"], None);
    assert_eq!(code, 1);
    assert_eq!(out.lines().collect::<Vec<_>>(), ["query.sql", "db/001_users.sql"]);
    let (code, _, _) = folder.run(&["format", "--write", "query.sql"], None);
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(folder.0.join("query.sql")).expect("reads");
    assert_eq!(written, "SELECT emial\nFROM users\nWHERE id = 1;\n");
    let (code, out, _) = folder.run(&["format", "--check", "query.sql"], None);
    assert_eq!((code, out.as_str()), (0, ""));
    let (code, out, _) = folder.run(&["format", "-"], Some("select 1"));
    assert_eq!((code, out.as_str()), (0, "SELECT 1"));
    let (code, _, _) = folder.run(&["format", "query.sql", "db"], None);
    assert_eq!(code, 2, "printing more than one file is refused");
}

#[test]
fn describe_says_what_is_known_about_a_name() {
    let folder = Folder::new("describe");
    let (code, out, _) = folder.run(&["describe", "--dialect", "mysql", "users", "db"], None);
    assert_eq!(code, 0);
    assert!(out.starts_with("**table** `users`"), "{out}");
    let (code, out, _) = folder.run(
        &[
            "describe",
            "--dialect",
            "mysql",
            "--format",
            "json",
            "users.email",
            "db",
        ],
        None,
    );
    assert_eq!(code, 0);
    let report: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    assert_eq!(report["kind"], "column");
    assert_eq!(report["definition"]["type"], "varchar(255)");
    let (code, out, _) = folder.run(&["describe", "--dialect", "postgres", "lower()"], None);
    assert_eq!(code, 0);
    assert!(out.contains("lower(text): text"), "{out}");
    let (code, _, err) = folder.run(&["describe", "--dialect", "mysql", "nothing"], None);
    assert_eq!(code, 1);
    assert!(err.contains("nothing named 'nothing'"), "{err}");
}
