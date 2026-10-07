//! `cargo run -p sql-syntax --example corpus -- cases <corpus.sql>` prints the cases of the dialect
//! corpus as JSON; `... -- verdicts <corpus.sql> <name>=<dialect>@<version> ...` prints whether the
//! parser and the feature table accept each case on each server. `scripts/dialect-corpus.py` runs
//! both against real servers.

#[path = "../tests/support/corpus.rs"]
mod corpus;

use serde_json::{Map, Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(command), Some(path)) = (args.first(), args.get(1)) else {
        eprintln!("usage: corpus cases|verdicts <corpus.sql> [<name>=<dialect>@<version> ...]");
        std::process::exit(2);
    };
    let file = std::fs::read_to_string(path).unwrap_or_else(|error| {
        eprintln!("{path}: {error}");
        std::process::exit(1);
    });
    let cases = corpus::cases(&file);
    match command.as_str() {
        "cases" => {
            let list: Vec<Value> = cases
                .iter()
                .map(|case| json!({ "id": case.id, "text": case.text }))
                .collect();
            println!("{}", Value::Array(list));
        }
        "verdicts" => {
            let servers: Vec<corpus::Server> = args[2..]
                .iter()
                .map(|spec| {
                    corpus::server(spec).unwrap_or_else(|| {
                        eprintln!("not a server: {spec}");
                        std::process::exit(2);
                    })
                })
                .collect();
            let mut out = Map::new();
            for case in &cases {
                let verdicts: Map<String, Value> = servers
                    .iter()
                    .map(|server| {
                        (
                            server.name.clone(),
                            Value::Bool(corpus::accepts(&case.text, server.target)),
                        )
                    })
                    .collect();
                out.insert(case.id.clone(), Value::Object(verdicts));
            }
            println!("{}", Value::Object(out));
        }
        _ => {
            eprintln!("unknown command {command}");
            std::process::exit(2);
        }
    }
}
