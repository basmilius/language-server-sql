use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = sql_language_server::cli::run(&args) {
        return code;
    }
    lsc_server::run_stdio(
        "sql-language-server",
        env!("CARGO_PKG_VERSION"),
        sql_language_server::cli::USAGE,
        sql_language_server::run,
    )
}
