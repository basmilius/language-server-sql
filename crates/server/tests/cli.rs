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
