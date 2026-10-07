use sql_syntax::{Parse, Target, TextRange};

use crate::context::Schemas;
use crate::inspections::{InspectionSettings, Request, inspect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Another place a diagnostic is about, such as the first of two names that clash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Related {
    pub range: TextRange,
    pub message: String,
}

/// A problem in a script: a syntax error, or what an inspection found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: TextRange,
    pub message: String,
    pub severity: DiagnosticSeverity,
    /// `syntax` for a parse error, else the id of the inspection.
    pub code: &'static str,
    /// Set for deprecated syntax, which a client may draw struck through.
    pub deprecated: bool,
    /// Set for what can go without changing anything, which a client may fade out.
    pub unnecessary: bool,
    /// The row of the feature table behind an `unsupported-syntax` or `deprecated-syntax` finding.
    pub feature: Option<&'static str>,
    pub related: Vec<Related>,
}

/// The code of a parse error, which no setting switches off.
pub const SYNTAX: &str = "syntax";

/// What the parser could not read.
pub fn syntax_errors(parse: &Parse) -> Vec<Diagnostic> {
    parse
        .errors()
        .iter()
        .map(|error| Diagnostic {
            range: error.range,
            message: error.message.clone(),
            severity: DiagnosticSeverity::Error,
            code: SYNTAX,
            deprecated: false,
            unnecessary: false,
            feature: None,
            related: Vec::new(),
        })
        .collect()
}

/// Every diagnostic of a script in source order: its syntax errors and what the inspections the
/// settings leave on find.
pub fn diagnostics(parse: &Parse, target: Target, schemas: Schemas, settings: &InspectionSettings) -> Vec<Diagnostic> {
    let mut found = syntax_errors(parse);
    let request = Request::new(target, schemas, settings);
    found.extend(
        inspect(&parse.syntax(), &request)
            .into_iter()
            .map(|finding| finding.diagnostic),
    );
    found.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_syntax::{Dialect, Version, parse};

    fn codes(text: &str, target: Target) -> Vec<&'static str> {
        let parsed = parse(text, target.dialect);
        diagnostics(&parsed, target, Schemas::NONE, &InspectionSettings::default())
            .iter()
            .map(|found| found.code)
            .collect()
    }

    #[test]
    fn merges_syntax_errors_and_findings_in_order() {
        let text = "SELECT a FROM t INTERSECT SELECT a FROM u;\nSELECT a FROM;\nCREATE TABLE k (key INT);";
        assert_eq!(
            codes(text, Target::new(Dialect::Mysql, Version::parse("8.0.30"))),
            ["unsupported-syntax", "syntax", "reserved-word"]
        );
        assert_eq!(
            codes(text, Target::new(Dialect::Mysql, None)),
            ["syntax", "reserved-word"]
        );
    }

    #[test]
    fn a_deprecation_is_a_warning() {
        let parsed = parse("SELECT SQL_CALC_FOUND_ROWS a FROM t WHERE a && b;", Dialect::Mysql);
        let target = Target::new(Dialect::Mysql, Version::parse("8.4"));
        let found = diagnostics(&parsed, target, Schemas::NONE, &InspectionSettings::default());
        let messages: Vec<&str> = found.iter().map(|found| found.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "SQL_CALC_FOUND_ROWS is deprecated since MySQL 8.0.17",
                "The && operator is deprecated since MySQL 8.0.17"
            ]
        );
        assert!(found.iter().all(|found| found.severity == DiagnosticSeverity::Warning
            && found.deprecated
            && found.code == "deprecated-syntax"));
        assert_eq!(found[0].feature, Some("sql-calc-found-rows"));
    }
}
