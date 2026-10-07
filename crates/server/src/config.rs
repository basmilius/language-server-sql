use std::path::{Path, PathBuf};

use serde_json::Value;
use sql_syntax::{Dialect, Target, Version};

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

/// The settings the server reads. Both `{ "dialect": "mysql" }` and the same object under
/// [`SECTION`] are understood, so a client may pass the settings as it likes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub default: Choice,
    pub overrides: Vec<Override>,
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
        (Settings { default, overrides }, problems)
    }

    /// Whether the settings say anything at all, as opposed to an empty answer.
    pub fn is_empty(&self) -> bool {
        self.default == Choice::default() && self.overrides.is_empty()
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
