use std::process::ExitCode;

const USAGE: &str = "sql-language-server [--stdio]\n\nA SQL language server for SQLite, MySQL, MariaDB and PostgreSQL. It speaks LSP over stdin and stdout.";

fn main() -> ExitCode {
    lsc_server::run_stdio(
        "sql-language-server",
        env!("CARGO_PKG_VERSION"),
        USAGE,
        sql_language_server::run,
    )
}
