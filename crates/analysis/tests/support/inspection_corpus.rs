//! The cases of the inspection corpus and what the inspections report on each, shared by
//! `tests/inspection_corpus.rs` and `examples/inspection_corpus.rs`.

use sql_analysis::DiagnosticSeverity;
use sql_analysis::context::Schemas;
use sql_analysis::inspections::{InspectionSettings, Request, inspect};
use sql_syntax::{Dialect, Target, Version, parse};

/// A case of the corpus: its id, its text, the inspection it is about and the dialects it is for.
pub struct Case {
    pub id: String,
    pub text: String,
    /// The inspection that must report an error exactly where a server rejects the case; `None`
    /// for a case no inspection may report as an error.
    pub expect: Option<String>,
    /// The dialects the case runs in; empty for all.
    pub only: Vec<Dialect>,
}

impl Case {
    pub fn runs_in(&self, dialect: Dialect) -> bool {
        self.only.is_empty() || self.only.contains(&dialect)
    }
}

/// The name of the fixture a dialect reads: MariaDB shares MySQL's.
pub fn fixture_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Postgres => "postgres",
        Dialect::Sqlite => "sqlite",
        _ => "mysql",
    }
}

/// The fixtures by name.
pub fn fixtures(file: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in file.lines() {
        if let Some(name) = line.strip_prefix("-- fixture: ") {
            found.extend(current.take());
            current = Some((name.trim().to_string(), String::new()));
        } else if line.starts_with("-- case: ") {
            found.extend(current.take());
        } else if let Some((_, text)) = current.as_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    found.extend(current);
    found
}

pub fn fixture(fixtures: &[(String, String)], dialect: Dialect) -> &str {
    fixtures
        .iter()
        .find(|(name, _)| name == fixture_name(dialect))
        .map_or("", |(_, text)| text.as_str())
}

pub fn cases(file: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    let mut current: Option<Case> = None;
    for line in file.lines() {
        if let Some(id) = line.strip_prefix("-- case: ") {
            cases.extend(current.take());
            current = Some(Case {
                id: id.trim().to_string(),
                text: String::new(),
                expect: None,
                only: Vec::new(),
            });
        } else if line.starts_with("-- fixture: ") {
            cases.extend(current.take());
        } else if let Some(case) = current.as_mut() {
            if let Some(expect) = line.strip_prefix("-- expect: ") {
                let expect = expect.trim();
                case.expect = (expect != "none").then(|| expect.to_string());
            } else if let Some(only) = line.strip_prefix("-- only: ") {
                case.only = only.split_whitespace().filter_map(Dialect::parse).collect();
            } else if !line.trim().is_empty() {
                case.text.push_str(line);
                case.text.push('\n');
            }
        }
    }
    cases.extend(current);
    for case in &mut cases {
        case.text = case.text.trim().to_string();
    }
    cases
}

/// A server of the record: its name, dialect and version.
pub struct Server {
    pub name: String,
    pub target: Target,
}

/// Reads `mysql-8.4=mysql@8.4.6`.
pub fn server(spec: &str) -> Option<Server> {
    let (name, target) = spec.split_once('=')?;
    let (dialect, version) = target.split_once('@')?;
    Some(Server {
        name: name.to_string(),
        target: Target::new(Dialect::parse(dialect)?, Some(Version::parse(version)?)),
    })
}

/// The inspections that report an error in a case, read after its dialect's fixture.
pub fn errors(fixtures: &[(String, String)], case: &Case, target: Target) -> Vec<&'static str> {
    let prefix = fixture(fixtures, target.dialect);
    let text = format!("{prefix}{}\n", case.text);
    let root = parse(&text, target.dialect).syntax();
    let settings = InspectionSettings::default();
    let request = Request::new(target, Schemas::NONE, &settings);
    let start = prefix.len() as u32;
    let mut found: Vec<&'static str> = inspect(&root, &request)
        .into_iter()
        .filter(|finding| u32::from(finding.diagnostic.range.start()) >= start)
        .filter(|finding| finding.diagnostic.severity == DiagnosticSeverity::Error)
        .map(|finding| finding.diagnostic.code)
        .collect();
    found.dedup();
    found
}
