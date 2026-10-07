//! Holds the parser and the feature table to what real servers did with the dialect corpus, as
//! `scripts/dialect-corpus.py` recorded it in `data/dialects-verified.txt`. No server is needed.

mod support;

use std::collections::HashMap;

use support::corpus;

const CORPUS: &str = include_str!("data/dialects.sql");
const VERIFIED: &str = include_str!("data/dialects-verified.txt");
const KNOWN: &str = include_str!("data/dialects-known.txt");

#[test]
fn every_case_is_judged_as_the_servers_judged_it() {
    let mut servers = Vec::new();
    let mut outcomes: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for line in VERIFIED.lines() {
        if let Some(spec) = line.strip_prefix("# server ") {
            servers.push(corpus::server(spec.trim()).expect("a server line"));
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            let (id, rest) = line.split_once('\t').expect("an id and the outcomes");
            let per_server = rest
                .split_whitespace()
                .filter_map(|pair| pair.split_once('='))
                .collect();
            outcomes.insert(id, per_server);
        }
    }
    let known: Vec<(&str, &str)> = KNOWN
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            (fields.next().expect("a case"), fields.next().expect("a server"))
        })
        .collect();
    assert!(!servers.is_empty(), "the record names its servers");
    let mut problems = Vec::new();
    let mut differences = Vec::new();
    for case in corpus::cases(CORPUS) {
        let Some(recorded) = outcomes.get(case.id.as_str()) else {
            problems.push(format!(
                "{}: not run on the servers yet (scripts/dialect-corpus.py)",
                case.id
            ));
            continue;
        };
        for server in &servers {
            let outcome = recorded.get(server.name.as_str()).copied().unwrap_or("missing");
            let accepts = corpus::accepts(&case.text, server.target);
            // Another error means the server parsed the text in some way, which fits either verdict.
            let contradicts = match outcome {
                "ok" => !accepts,
                "syntax" => accepts,
                _ => false,
            };
            if !contradicts {
                continue;
            }
            differences.push((case.id.clone(), server.name.clone()));
            if !known.contains(&(case.id.as_str(), server.name.as_str())) {
                let verdict = if outcome == "ok" { "runs" } else { "rejects" };
                problems.push(format!(
                    "{} on {}: the server {verdict} {:?}",
                    case.id, server.name, case.text
                ));
            }
        }
    }
    for (case, server) in &known {
        if !differences.iter().any(|(id, name)| id == case && name == server) {
            problems.push(format!(
                "{case} on {server} is listed as a known difference and no longer differs"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
