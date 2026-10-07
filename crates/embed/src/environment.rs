//! What every fragment of a host is read against: the dialect and version, the schema snapshot,
//! the DDL of the workspace's `.sql` files and the settings. One environment serves any number of
//! fragments and threads; a changed file means a new snapshot or workspace schema and a new
//! environment, while fragments already analyzed keep the one they had.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use sql_analysis::DiagnosticSeverity;
use sql_analysis::catalog::{Layer, Origin};
use sql_analysis::completion::QuoteIdentifiers;
use sql_analysis::context::Schemas;
use sql_analysis::inlay_hints::HintOptions;
use sql_analysis::inspections::{InspectionSettings, Override, inspection_info};
use sql_analysis::workspace::{FileDdl, build, extract};
use sql_catalog::{load_snapshot, read_snapshot};
use sql_syntax::{Dialect, FEATURES, Target, Version};

/// How fragments are read. [`Settings::from_json`] reads the shape the language server's own
/// settings have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub dialect: Dialect,
    pub version: Option<Version>,
    /// The path of a schema snapshot as the settings give it; the host resolves and loads it.
    pub schema: Option<String>,
    /// MySQL's and MariaDB's `sql_mode` as the host knows the connection has it. Without it the
    /// snapshot's, else the server's default.
    pub sql_mode: Option<String>,
    pub inspections: InspectionSettings,
    pub hints: HintOptions,
    /// A `?` is a placeholder of the host's database layer, also in PostgreSQL, which would read
    /// `a=?` as an operator. On by default, as PHP's database layers have it.
    pub question_placeholders: bool,
    /// `completion.quoteIdentifiers`, which a host's own options leave to the settings.
    pub quote_identifiers: QuoteIdentifiers,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            dialect: Dialect::Generic,
            version: None,
            schema: None,
            sql_mode: None,
            inspections: InspectionSettings::default(),
            hints: HintOptions::default(),
            question_placeholders: true,
            quote_identifiers: QuoteIdentifiers::Auto,
        }
    }
}

impl Settings {
    pub fn target(&self) -> Target {
        Target::new(self.dialect, self.version)
    }

    /// The settings in a JSON object of the language server's shape (`dialect`, `version`,
    /// `schema`, `sqlMode`, `inspections`, `inlayHints`, `questionPlaceholders`, `completion`), bare or under
    /// `sqlLanguageServer`, and what in it could not be read.
    pub fn from_json(value: &Value) -> (Settings, Vec<String>) {
        let object = value.get("sqlLanguageServer").unwrap_or(value);
        let mut problems = Vec::new();
        let mut settings = Settings::default();
        if let Some(dialect) = dialect_from_json(object.get("dialect"), &mut problems) {
            settings.dialect = dialect;
        }
        settings.version = version_from_json(object.get("version"), &mut problems);
        settings.schema = object.get("schema").and_then(Value::as_str).map(str::to_string);
        settings.sql_mode = object.get("sqlMode").and_then(Value::as_str).map(str::to_string);
        if let Some(inspections) = object.get("inspections") {
            settings.inspections = inspections_from_json(inspections, &mut problems);
        }
        if let Some(hints) = object.get("inlayHints") {
            let flag = |key: &str, default: bool| hints.get(key).and_then(Value::as_bool).unwrap_or(default);
            settings.hints = HintOptions {
                insert_columns: flag("insertColumns", true),
                select_columns: flag("selectColumns", true),
                parameter_names: flag("parameterNames", true),
            };
        }
        if let Some(flag) = object.get("questionPlaceholders").and_then(Value::as_bool) {
            settings.question_placeholders = flag;
        }
        if let Some(choice) = quote_identifiers_from_json(object, &mut problems) {
            settings.quote_identifiers = choice;
        }
        (settings, problems)
    }
}

/// `completion.quoteIdentifiers` of the settings: `auto`, `always` or `never`.
pub fn quote_identifiers_from_json(object: &Value, problems: &mut Vec<String>) -> Option<QuoteIdentifiers> {
    let text = object.get("completion")?.get("quoteIdentifiers")?.as_str()?;
    let choice = QuoteIdentifiers::parse(text);
    if choice.is_none() {
        problems.push(format!(
            "Unknown choice '{text}' for quoteIdentifiers: use auto, always or never"
        ));
    }
    choice
}

/// A `dialect` of the settings, or nothing with a problem for a name no dialect has.
pub fn dialect_from_json(value: Option<&Value>, problems: &mut Vec<String>) -> Option<Dialect> {
    let name = value?.as_str()?;
    let dialect = Dialect::parse(name);
    if dialect.is_none() {
        problems.push(format!(
            "Unknown dialect '{name}': use sqlite, mysql, mariadb, postgres or generic"
        ));
    }
    dialect
}

/// A `version` of the settings, a string or a number.
pub fn version_from_json(value: Option<&Value>, problems: &mut Vec<String>) -> Option<Version> {
    match value? {
        Value::String(text) => Version::parse(text).or_else(|| {
            problems.push(format!("Unknown version '{text}': use a version such as 8.4 or 3.47.2"));
            None
        }),
        Value::Number(number) => Version::parse(&number.to_string()),
        _ => None,
    }
}

/// `inspections`: per inspection id, or per id of a row of the feature table, `false` or `"off"`,
/// a severity, or `{ "enabled": ..., "severity": ... }`.
pub fn inspections_from_json(value: &Value, problems: &mut Vec<String>) -> InspectionSettings {
    let mut settings = InspectionSettings::default();
    let Some(map) = value.as_object() else {
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
            Value::Bool(enabled) => Override {
                enabled: Some(*enabled),
                severity: None,
            },
            Value::String(text) => named(text),
            Value::Object(fields) => {
                let severity = fields.get("severity").and_then(Value::as_str).map(&mut named);
                Override {
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

fn override_of(text: &str) -> Option<Override> {
    let severity = match text.to_ascii_lowercase().as_str() {
        "off" | "none" | "false" => {
            return Some(Override {
                enabled: Some(false),
                severity: None,
            });
        }
        "on" | "true" => {
            return Some(Override {
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
    Some(Override {
        enabled: Some(true),
        severity: Some(severity),
    })
}

/// A schema snapshot, read once and shared: cloning is cheap.
#[derive(Clone)]
pub struct Snapshot {
    pub(crate) layer: Arc<Layer>,
}

impl Snapshot {
    /// Reads a snapshot file; the error says what is wrong with it.
    pub fn load(path: &Path) -> Result<Snapshot, String> {
        let snapshot = load_snapshot(path).map_err(|error| error.message)?;
        Ok(Snapshot::of(snapshot))
    }

    /// Reads the JSON of a snapshot.
    pub fn parse(json: &str) -> Result<Snapshot, String> {
        let snapshot = read_snapshot(json).map_err(|error| error.message)?;
        Ok(Snapshot::of(snapshot))
    }

    pub fn of(snapshot: sql_catalog::model::Snapshot) -> Snapshot {
        Snapshot {
            layer: Arc::new(Layer::new(Origin::Snapshot, snapshot)),
        }
    }
}

/// Folders a walk does not go into: dependencies, build output and version control.
const SKIPPED: [&str; 5] = ["node_modules", "vendor", "target", "dist", "build"];

/// A file larger than this is a data dump rather than a schema.
const MOST_BYTES: u64 = 16 * 1024 * 1024;

/// How many files a walk finds at most.
const MOST_FILES: usize = 10_000;

pub fn is_sql_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sql"))
}

/// Every `.sql` file under the folders, leaving out hidden folders, `node_modules`, `vendor`,
/// `target`, `dist` and `build`, and stopping after 10,000 files.
pub fn sql_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending: Vec<PathBuf> = roots.to_vec();
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIPPED.contains(&name.as_str()) {
                    pending.push(path);
                }
            } else if kind.is_file() && is_sql_file(&path) {
                if found.len() == MOST_FILES {
                    return found;
                }
                found.push(path);
            }
        }
    }
    found
}

/// The text of a `.sql` file, or nothing when it cannot be read or is a data dump of over 16 MB.
pub fn read_sql_file(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MOST_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The DDL of the workspace's `.sql` files for one dialect, kept per file so a changed file is read
/// again alone.
#[derive(Clone, Debug)]
pub struct Workspace {
    dialect: Dialect,
    files: BTreeMap<PathBuf, FileDdl>,
}

impl Workspace {
    pub fn new(dialect: Dialect) -> Workspace {
        Workspace {
            dialect,
            files: BTreeMap::new(),
        }
    }

    /// Reads every `.sql` file under the folders (see [`sql_files`]).
    pub fn scan(dialect: Dialect, roots: &[PathBuf]) -> Workspace {
        let mut workspace = Workspace::new(dialect);
        for path in sql_files(roots) {
            if let Some(text) = read_sql_file(&path) {
                workspace.set_file(path, &text);
            }
        }
        workspace
    }

    /// Puts in the text of a file, or reads it again.
    pub fn set_file(&mut self, path: PathBuf, text: &str) {
        self.files.insert(path, extract(text, self.dialect));
    }

    pub fn remove_file(&mut self, path: &Path) {
        self.files.remove(path);
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.keys().map(PathBuf::as_path)
    }

    /// What the files define, replayed in the order of their paths, as migrations run; nothing
    /// when they define nothing.
    pub fn schema(&self) -> Option<WorkspaceSchema> {
        let layer = build(self.dialect, self.files.iter().map(|(path, ddl)| (path.as_path(), ddl)));
        (!layer.is_empty()).then(|| WorkspaceSchema { layer: Arc::new(layer) })
    }
}

/// The objects the workspace's DDL defines, shared: cloning is cheap.
#[derive(Clone)]
pub struct WorkspaceSchema {
    pub(crate) layer: Arc<Layer>,
}

/// Everything a fragment is read against. Cloning is cheap and an environment may be shared
/// between threads.
#[derive(Clone)]
pub struct Environment {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub settings: Settings,
    pub snapshot: Option<Snapshot>,
    pub workspace: Option<WorkspaceSchema>,
}

impl Environment {
    pub fn new(settings: Settings, snapshot: Option<Snapshot>, workspace: Option<WorkspaceSchema>) -> Environment {
        Environment {
            inner: Arc::new(Inner {
                settings,
                snapshot,
                workspace,
            }),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    pub fn target(&self) -> Target {
        self.inner.settings.target()
    }

    pub(crate) fn schemas(&self) -> Schemas<'_> {
        Schemas {
            snapshot: self.inner.snapshot.as_ref().map(|snapshot| snapshot.layer.as_ref()),
            workspace: self.inner.workspace.as_ref().map(|workspace| workspace.layer.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_settings_of_the_language_server() {
        let (settings, problems) = Settings::from_json(&json!({
            "sqlLanguageServer": {
                "dialect": "mysql",
                "version": "8.4",
                "schema": "db/schema.json",
                "sqlMode": "ANSI_QUOTES",
                "inspections": { "missing-where": "off", "named-parameters": true },
                "inlayHints": { "parameterNames": false },
                "questionPlaceholders": false
            }
        }));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.target(), Target::new(Dialect::Mysql, Version::parse("8.4")));
        assert_eq!(settings.schema.as_deref(), Some("db/schema.json"));
        assert_eq!(settings.sql_mode.as_deref(), Some("ANSI_QUOTES"));
        assert!(!settings.hints.parameter_names && settings.hints.insert_columns);
        assert!(!settings.question_placeholders);
        assert!(!settings.inspections.is_empty());
        let (_, problems) = Settings::from_json(&json!({ "dialect": "oracle", "inspections": { "nope": "off" } }));
        assert_eq!(problems.len(), 2);
    }
}
