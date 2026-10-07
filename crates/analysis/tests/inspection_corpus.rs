//! Holds the inspections that report errors to what real servers did with the inspection corpus,
//! as `scripts/inspection-corpus.py` recorded it in `data/inspections-verified.txt`: an inspection
//! that reports an error claims the server rejects the statement. No server is needed.

#[path = "support/inspection_corpus.rs"]
mod corpus;

use std::collections::HashMap;

const CORPUS: &str = include_str!("data/inspections.sql");
const VERIFIED: &str = include_str!("data/inspections-verified.txt");
const KNOWN: &str = include_str!("data/inspections-known.txt");

#[test]
fn every_error_is_one_the_servers_give() {
    let mut servers = Vec::new();
    let mut outcomes: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for line in VERIFIED.lines() {
        if let Some(spec) = line.strip_prefix("# server ") {
            servers.push(corpus::server(spec.trim()).expect("a server line"));
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            let (id, rest) = line.split_once('\t').expect("an id and the outcomes");
            outcomes.insert(
                id,
                rest.split_whitespace()
                    .filter_map(|pair| pair.split_once('='))
                    .collect(),
            );
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
    let fixtures = corpus::fixtures(CORPUS);
    let mut problems = Vec::new();
    let mut differences = Vec::new();
    for case in corpus::cases(CORPUS) {
        let Some(recorded) = outcomes.get(case.id.as_str()) else {
            problems.push(format!(
                "{}: not run on the servers yet (scripts/inspection-corpus.py)",
                case.id
            ));
            continue;
        };
        for server in servers.iter().filter(|server| case.runs_in(server.target.dialect)) {
            let outcome = recorded.get(server.name.as_str()).copied().unwrap_or("missing");
            let rejected = outcome != "ok";
            let errors = corpus::errors(&fixtures, &case, server.target);
            let expected = case.expect.as_deref();
            let wrong = match expected {
                Some(id) => errors.contains(&id) != rejected || (!errors.is_empty() && !rejected),
                None => !errors.is_empty(),
            };
            if !wrong {
                continue;
            }
            differences.push((case.id.clone(), server.name.clone()));
            if !known.contains(&(case.id.as_str(), server.name.as_str())) {
                problems.push(format!(
                    "{} on {}: the server says {outcome}, the inspections report {errors:?}: {:?}",
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
