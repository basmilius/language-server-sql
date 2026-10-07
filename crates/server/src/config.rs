use std::path::{Path, PathBuf};

use serde_json::Value;
use sql_analysis::DiagnosticSeverity;
use sql_analysis::inspections::{InspectionSettings, Override as InspectionChoice, inspection_info};
use sql_syntax::{Dialect, FEATURES, Target, Version};

/// The section a client answers `workspace/configuration` for, and pushes in `didChangeConfiguration`.
pub const SECTION: &str = "sqlLanguageServer";

/// What one level of the settings says: the top level, or one entry of `overrides`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub dialect: Option<Dialect>,
    pub version: Option<Version>,
    /// A schema snapshot, kept for the resolution of names that comes after syntax.
    pub schema: Option<String>,
}

/// A choice for a file or a folder, by path: absolute, a `file:` URI, or relative to the first
/// workspace folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    pub path: String,
    pub choice: Choice,
}

/// Which inlay hints are shown; a key left out is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HintSettings {
    pub insert_columns: Option<bool>,
    pub select_columns: Option<bool>,
    pub parameter_names: Option<bool>,
}

/// The case keywords are written in by the formatter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeywordCase {
    Upper,
    Lower,
    Preserve,
}

/// How the formatter lays a script out; a key left out takes its default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatSettings {
    pub keyword_case: Option<KeywordCase>,
    /// Spaces per level, in place of the editor's tab size.
    pub indent_width: Option<usize>,
    /// Commas at the start of a line rather than at the end.
    pub leading_commas: Option<bool>,
}

/// The settings the server reads. Both `{ "dialect": "mysql" }` and the same object under
/// [`SECTION`] are understood, so a client may pass the settings as it likes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub default: Choice,
    pub overrides: Vec<Override>,
    pub hints: HintSettings,
    pub format: FormatSettings,
    pub inspections: InspectionSettings,
}

/// `inspections`: per inspection id, or per id of a row of the feature table, `false` or `"off"`,
/// a severity, or `{ "enabled": ..., "severity": ... }`.
fn inspections_from(object: &Value, problems: &mut Vec<String>) -> InspectionSettings {
    let mut settings = InspectionSettings::default();
    let Some(map) = object.get("inspections").and_then(Value::as_object) else {
        return settings;
    };
    for (id, choice) in map {
        if inspection_info(id).is_none() && !FEATURES.iter().any(|feature| feature.id == id) {
            problems.push(format!("Unknown inspection '{id}'"));
            continue;
        }
        let mut named = |text: &str| {
            let found = override_of(text);
            if found.is_none() {
                problems.push(format!(
                    "Unknown choice '{text}' for the inspection '{id}': use off, error, warning, information or hint"
                ));
            }
            found.unwrap_or_default()
        };
        let choice = match choice {
            Value::Bool(enabled) => InspectionChoice {
                enabled: Some(*enabled),
                severity: None,
            },
            Value::String(text) => named(text),
            Value::Object(fields) => {
                let severity = fields.get("severity").and_then(Value::as_str).map(&mut named);
                InspectionChoice {
                    enabled: fields
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .or_else(|| severity.and_then(|severity| severity.enabled)),
                    severity: severity.and_then(|severity| severity.severity),
                }
            }
            _ => continue,
        };
        settings.set(id, choice);
    }
    settings
}

fn override_of(text: &str) -> Option<InspectionChoice> {
    let severity = match text.to_ascii_lowercase().as_str() {
        "off" | "none" | "false" => {
            return Some(InspectionChoice {
                enabled: Some(false),
                severity: None,
            });
        }
        "on" | "true" => {
            return Some(InspectionChoice {
                enabled: Some(true),
                severity: None,
            });
        }
        "error" => DiagnosticSeverity::Error,
        "warning" | "warn" => DiagnosticSeverity::Warning,
        "information" | "info" => DiagnosticSeverity::Information,
        "hint" => DiagnosticSeverity::Hint,
        _ => return None,
    };
    Some(InspectionChoice {
        enabled: Some(true),
        severity: Some(severity),
    })
}

fn hints_from(object: &Value) -> HintSettings {
    let Some(hints) = object.get("inlayHints") else {
        return HintSettings::default();
    };
    let flag = |key: &str| hints.get(key).and_then(Value::as_bool);
    HintSettings {
        insert_columns: flag("insertColumns"),
        select_columns: flag("selectColumns"),
        parameter_names: flag("parameterNames"),
    }
}

fn format_from(object: &Value, problems: &mut Vec<String>) -> FormatSettings {
    let Some(format) = object.get("format") else {
        return FormatSettings::default();
    };
    let keyword_case = match format.get("keywordCase").and_then(Value::as_str) {
        None => None,
        Some(text) => match text.to_ascii_lowercase().as_str() {
            "upper" => Some(KeywordCase::Upper),
            "lower" => Some(KeywordCase::Lower),
            "preserve" => Some(KeywordCase::Preserve),
            _ => {
                problems.push(format!("Unknown keyword case '{text}': use upper, lower or preserve"));
                None
            }
        },
    };
    let indent_width = format
        .get("indentWidth")
        .and_then(Value::as_u64)
        .map(|width| width.clamp(1, 16) as usize);
    let leading_commas = match format.get("commaPosition").and_then(Value::as_str) {
        None => None,
        Some(text) => match text.to_ascii_lowercase().as_str() {
            "leading" => Some(true),
            "trailing" => Some(false),
            _ => {
                problems.push(format!("Unknown comma position '{text}': use trailing or leading"));
                None
            }
        },
    };
    FormatSettings {
        keyword_case,
        indent_width,
        leading_commas,
    }
}

/// What a document is read as, once the settings are applied to its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub target: Target,
    pub schema: Option<PathBuf>,
}

fn choice_from(object: &Value, problems: &mut Vec<String>) -> Choice {
    let text = |key: &str| object.get(key).and_then(Value::as_str);
    let dialect = text("dialect").and_then(|name| {
        let dialect = Dialect::parse(name);
        if dialect.is_none() {
            problems.push(format!(
                "Unknown dialect '{name}': use sqlite, mysql, mariadb, postgres or generic"
            ));
        }
        dialect
    });
    let version = match object.get("version") {
        Some(Value::String(text)) => Version::parse(text).or_else(|| {
            problems.push(format!("Unknown version '{text}': use a version such as 8.4 or 3.47.2"));
            None
        }),
        Some(Value::Number(number)) => Version::parse(&number.to_string()),
        _ => None,
    };
    Choice {
        dialect,
        version,
        schema: text("schema").map(str::to_string),
    }
}

impl Settings {
    /// The settings in a value, and what in it could not be read.
    pub fn from_value(value: &Value) -> (Settings, Vec<String>) {
        let object = value.get(SECTION).unwrap_or(value);
        let mut problems = Vec::new();
        let default = choice_from(object, &mut problems);
        let overrides = object
            .get("overrides")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        let path = entry.get("path").and_then(Value::as_str)?;
                        Some(Override {
                            path: path.to_string(),
                            choice: choice_from(entry, &mut problems),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let hints = hints_from(object);
        let format = format_from(object, &mut problems);
        let inspections = inspections_from(object, &mut problems);
        (
            Settings {
                default,
                overrides,
                hints,
                format,
                inspections,
            },
            problems,
        )
    }

    /// Whether the settings say anything at all, as opposed to an empty answer.
    pub fn is_empty(&self) -> bool {
        self.default == Choice::default()
            && self.overrides.is_empty()
            && self.hints == HintSettings::default()
            && self.format == FormatSettings::default()
            && self.inspections.is_empty()
    }

    /// What a document at `path` is read as. The most specific override that names a field wins,
    /// then the top level, then the dialect of the client's language id; a version only counts at
    /// a level that names no dialect or the dialect chosen.
    pub fn resolve(&self, path: Option<&Path>, root: Option<&Path>, language: Option<Dialect>) -> Resolved {
        let mut levels: Vec<(usize, &Choice)> = Vec::new();
        if let Some(path) = path {
            for entry in &self.overrides {
                let Some(target) = resolve_path(&entry.path, root) else {
                    continue;
                };
                if path.starts_with(&target) {
                    levels.push((target.components().count(), &entry.choice));
                }
            }
        }
        levels.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        levels.push((0, &self.default));
        let dialect = levels
            .iter()
            .find_map(|(_, choice)| choice.dialect)
            .or(language)
            .unwrap_or(Dialect::Generic);
        let version = levels
            .iter()
            .filter(|(_, choice)| choice.dialect.is_none_or(|own| own == dialect))
            .find_map(|(_, choice)| choice.version);
        let schema = levels
            .iter()
            .find_map(|(_, choice)| choice.schema.as_deref())
            .and_then(|schema| resolve_path(schema, root));
        Resolved {
            target: Target::new(dialect, version),
            schema,
        }
    }
}

/// A path of the settings as an absolute path: a `file:` URI, an absolute path, or one relative to
/// the workspace root.
fn resolve_path(text: &str, root: Option<&Path>) -> Option<PathBuf> {
    if text.starts_with("file:") {
        let uri: lsp_types::Uri = text.parse().ok()?;
        return lsc_server::paths::uri_to_path(&uri);
    }
    let path = PathBuf::from(text);
    if path.is_absolute() {
        return Some(path);
    }
    root.map(|root| root.join(path))
}

/// The dialect a language id names, such as `mysql` or `postgres`; `sql` names none.
pub fn dialect_of_language(language_id: &str) -> Option<Dialect> {
    match Dialect::parse(language_id) {
        Some(Dialect::Generic) | None => None,
        found => found,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resolve(value: Value, path: &str, language: Option<Dialect>) -> Resolved {
        let (settings, problems) = Settings::from_value(&value);
        assert!(problems.is_empty(), "{problems:?}");
        settings.resolve(Some(Path::new(path)), Some(Path::new("/work")), language)
    }

    #[test]
    fn reads_either_shape_and_reports_what_it_cannot_read() {
        let bare = Settings::from_value(&json!({ "dialect": "postgres", "version": "18" })).0;
        let nested =
            Settings::from_value(&json!({ "sqlLanguageServer": { "dialect": "PostgreSQL", "version": 18 } })).0;
        assert_eq!(bare, nested);
        assert_eq!(bare.default.dialect, Some(Dialect::Postgres));
        let (_, problems) = Settings::from_value(&json!({ "dialect": "oracle", "version": "latest" }));
        assert_eq!(problems.len(), 2);
        assert!(Settings::from_value(&json!(null)).0.is_empty());
    }

    #[test]
    fn the_most_specific_override_wins_field_by_field() {
        let settings = json!({
            "dialect": "mysql",
            "version": "8.0.30",
            "schema": "schemas/main.json",
            "overrides": [
                { "path": "db", "version": "8.4" },
                { "path": "db/legacy", "dialect": "mariadb" },
                { "path": "/elsewhere/report.sql", "dialect": "postgres", "schema": "file:///snapshots/pg.json" }
            ]
        });
        let top = resolve(settings.clone(), "/work/app/query.sql", None);
        assert_eq!(top.target, Target::new(Dialect::Mysql, Version::parse("8.0.30")));
        assert_eq!(top.schema.as_deref(), Some(Path::new("/work/schemas/main.json")));
        let db = resolve(settings.clone(), "/work/db/a.sql", None);
        assert_eq!(db.target, Target::new(Dialect::Mysql, Version::parse("8.4")));
        let legacy = resolve(settings.clone(), "/work/db/legacy/b.sql", None);
        assert_eq!(
            legacy.target,
            Target::new(Dialect::Mariadb, Version::parse("8.4")),
            "a version without a dialect applies"
        );
        let report = resolve(settings, "/elsewhere/report.sql", None);
        assert_eq!(
            report.target,
            Target::new(Dialect::Postgres, None),
            "MySQL's version does not carry over"
        );
        assert_eq!(report.schema.as_deref(), Some(Path::new("/snapshots/pg.json")));
    }

    #[test]
    fn reads_the_settings_of_hints_and_formatting() {
        let (settings, problems) = Settings::from_value(&json!({
            "inlayHints": { "parameterNames": false },
            "format": { "keywordCase": "lower", "indentWidth": 2, "commaPosition": "leading" }
        }));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.hints.parameter_names, Some(false));
        assert_eq!(settings.hints.insert_columns, None);
        assert_eq!(settings.format.keyword_case, Some(KeywordCase::Lower));
        assert_eq!(settings.format.indent_width, Some(2));
        assert_eq!(settings.format.leading_commas, Some(true));
        assert!(!settings.is_empty());
        let (_, problems) =
            Settings::from_value(&json!({ "format": { "keywordCase": "title", "commaPosition": "x" } }));
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn reads_the_settings_of_inspections() {
        let (settings, problems) = Settings::from_value(&json!({
            "inspections": {
                "missing-where": "off",
                "null-comparison": "error",
                "double-pipe": { "severity": "hint" },
                "unused-alias": false,
                "nonsense": "off",
                "unused-cte": "loud"
            }
        }));
        assert_eq!(problems.len(), 2, "{problems:?}");
        let info = inspection_info("missing-where").expect("an inspection");
        assert_eq!(settings.inspections.severity_of(info, None, info.severity), None);
        let info = inspection_info("null-comparison").expect("an inspection");
        assert_eq!(
            settings.inspections.severity_of(info, None, info.severity),
            Some(DiagnosticSeverity::Error)
        );
        let info = inspection_info("deprecated-syntax").expect("an inspection");
        assert_eq!(
            settings
                .inspections
                .severity_of(info, Some("double-pipe"), info.severity),
            Some(DiagnosticSeverity::Hint)
        );
        assert_eq!(
            settings.inspections.severity_of(info, Some("zerofill"), info.severity),
            Some(DiagnosticSeverity::Warning)
        );
        assert!(!settings.is_empty());
    }

    #[test]
    fn a_language_id_picks_the_dialect_when_nothing_else_does() {
        let found = resolve(json!({}), "/work/a.sql", dialect_of_language("sqlite"));
        assert_eq!(found.target, Target::new(Dialect::Sqlite, None));
        let found = resolve(
            json!({ "dialect": "postgres" }),
            "/work/a.sql",
            dialect_of_language("mysql"),
        );
        assert_eq!(found.target.dialect, Dialect::Postgres);
        let found = resolve(json!({}), "/work/a.sql", dialect_of_language("sql"));
        assert_eq!(found.target, Target::GENERIC);
    }
}
