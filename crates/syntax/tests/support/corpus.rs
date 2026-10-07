//! The cases of the dialect corpus and the verdict of the parser and the feature table on each,
//! shared by `tests/dialects.rs` and `examples/corpus.rs`.

use sql_syntax::{Dialect, FEATURES, FeatureSeverity, Target, Version, check_features, parse};

/// A statement of the corpus: an id and its text.
pub struct Case {
    pub id: String,
    pub text: String,
}

/// The example of every feature row as `feature:<id>`, then the cases of the file as `case:<id>`.
pub fn cases(file: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = FEATURES
        .iter()
        .map(|feature| Case {
            id: format!("feature:{}", feature.id),
            text: format!("{};", feature.example),
        })
        .collect();
    let mut current: Option<Case> = None;
    for line in file.lines() {
        if let Some(id) = line.strip_prefix("-- case: ") {
            cases.extend(current.take());
            current = Some(Case {
                id: format!("case:{}", id.trim()),
                text: String::new(),
            });
        } else if let Some(case) = current.as_mut() {
            case.text.push_str(line);
            case.text.push('\n');
        }
    }
    cases.extend(current);
    for case in &mut cases {
        case.text = case.text.trim().to_string();
    }
    cases
}

/// A server of the corpus: its name in the record, its dialect and its version.
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

/// Whether the parser and the feature table accept the text for a target: no syntax error and no
/// finding that is an error.
pub fn accepts(text: &str, target: Target) -> bool {
    let parsed = parse(text, target.dialect);
    parsed.errors().is_empty()
        && check_features(&parsed.syntax(), target)
            .iter()
            .all(|finding| finding.severity != FeatureSeverity::Error)
}
