//! `cargo run -p sql-analysis --example inspection_corpus -- cases <inspections.sql>` prints the
//! cases of the inspection corpus with the fixture each server runs first, as JSON.
//! `scripts/inspection-corpus.py` runs them on real servers.

#[allow(dead_code)]
#[path = "../tests/support/inspection_corpus.rs"]
mod corpus;

use serde_json::{Value, json};
use sql_syntax::Dialect;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(command), Some(path)) = (args.first(), args.get(1)) else {
        eprintln!("usage: inspection_corpus cases <inspections.sql>");
        std::process::exit(2);
    };
    let file = std::fs::read_to_string(path).unwrap_or_else(|error| {
        eprintln!("{path}: {error}");
        std::process::exit(1);
    });
    if command != "cases" {
        eprintln!("unknown command {command}");
        std::process::exit(2);
    }
    let fixtures = corpus::fixtures(&file);
    let cases: Vec<Value> = corpus::cases(&file)
        .iter()
        .map(|case| {
            let dialects: Vec<&str> = Dialect::DATABASES
                .into_iter()
                .filter(|dialect| case.runs_in(*dialect))
                .map(Dialect::id)
                .collect();
            json!({ "id": case.id, "text": case.text, "dialects": dialects })
        })
        .collect();
    let fixtures: serde_json::Map<String, Value> = fixtures
        .into_iter()
        .map(|(name, text)| (name, Value::String(text)))
        .collect();
    println!("{}", json!({ "fixtures": fixtures, "cases": cases }));
}
