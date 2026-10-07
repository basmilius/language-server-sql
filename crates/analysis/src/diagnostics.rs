use sql_syntax::{FeatureSeverity, Parse, Target, TextRange, check_features};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// A problem in a script, from the syntax or from what its dialect and version accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: TextRange,
    pub message: String,
    pub severity: DiagnosticSeverity,
    /// Set for deprecated syntax, which a client may draw struck through.
    pub deprecated: bool,
    /// `syntax` for a parse error, `reserved-word`, or the id of a row of the feature table.
    pub code: &'static str,
}

/// The syntax errors of a parse and what the target does not accept, in source order.
pub fn diagnostics(parse: &Parse, target: Target) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = parse
        .errors()
        .iter()
        .map(|error| Diagnostic {
            range: error.range,
            message: error.message.clone(),
            severity: DiagnosticSeverity::Error,
            deprecated: false,
            code: "syntax",
        })
        .collect();
    found.extend(
        check_features(&parse.syntax(), target)
            .into_iter()
            .map(|finding| Diagnostic {
                range: finding.range,
                message: finding.message,
                severity: match finding.severity {
                    FeatureSeverity::Error => DiagnosticSeverity::Error,
                    FeatureSeverity::Warning => DiagnosticSeverity::Warning,
                },
                deprecated: finding.deprecated,
                code: finding.feature,
            }),
    );
    found.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_syntax::{Dialect, Version, parse};

    #[test]
    fn merges_syntax_errors_and_findings_in_order() {
        let text = "SELECT a FROM t INTERSECT SELECT a FROM u;\nSELECT a FROM;\nCREATE TABLE k (key INT);";
        let parsed = parse(text, Dialect::Mysql);
        let found = diagnostics(&parsed, Target::new(Dialect::Mysql, Version::parse("8.0.30")));
        let codes: Vec<_> = found.iter().map(|found| found.code).collect();
        assert_eq!(codes, ["intersect-except", "syntax", "reserved-word"]);
        let newest = diagnostics(&parsed, Target::new(Dialect::Mysql, None));
        let codes: Vec<_> = newest.iter().map(|found| found.code).collect();
        assert_eq!(codes, ["syntax", "reserved-word"]);
    }

    #[test]
    fn a_deprecation_is_a_warning() {
        let parsed = parse("SELECT SQL_CALC_FOUND_ROWS a FROM t WHERE a && b;", Dialect::Mysql);
        let found = diagnostics(&parsed, Target::new(Dialect::Mysql, Version::parse("8.4")));
        let messages: Vec<&str> = found.iter().map(|found| found.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "SQL_CALC_FOUND_ROWS is deprecated since MySQL 8.0.17",
                "The && operator is deprecated since MySQL 8.0.17"
            ]
        );
        assert!(
            found
                .iter()
                .all(|found| found.severity == DiagnosticSeverity::Warning && found.deprecated)
        );
    }
}
