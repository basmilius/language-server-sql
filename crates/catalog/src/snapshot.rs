//! Reading a snapshot file.

use std::fmt;
use std::path::Path;

use crate::model::Snapshot;

/// Why a snapshot could not be read, as a person reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotError {
    pub message: String,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SnapshotError {}

/// Reads the JSON of a snapshot. Unknown fields are ignored; a missing `formatVersion` means the
/// file is no snapshot.
pub fn read_snapshot(text: &str) -> Result<Snapshot, SnapshotError> {
    let mut snapshot: Snapshot = serde_json::from_str(text).map_err(|error| SnapshotError {
        message: format!("Not a schema snapshot: {error}"),
    })?;
    if snapshot.format_version == 0 {
        return Err(SnapshotError {
            message: "Not a schema snapshot: formatVersion is missing".to_string(),
        });
    }
    if let Some(missing) = unnamed(&snapshot) {
        return Err(SnapshotError {
            message: format!("Not a schema snapshot: {missing} has no name"),
        });
    }
    for schema in &mut snapshot.schemas {
        for table in &mut schema.tables {
            if table.columns.iter().all(|column| column.ordinal.is_some()) {
                table.columns.sort_by_key(|column| column.ordinal);
            }
        }
    }
    Ok(snapshot)
}

/// The first object without a name, as a message names it.
fn unnamed(snapshot: &Snapshot) -> Option<String> {
    for (position, schema) in snapshot.schemas.iter().enumerate() {
        if schema.name.is_empty() {
            return Some(format!("schema {}", position + 1));
        }
        let within = |kind: &str, position: usize| format!("{kind} {} of schema {}", position + 1, schema.name);
        for (position, table) in schema.tables.iter().enumerate() {
            if table.name.is_empty() {
                return Some(within("table", position));
            }
            if let Some(column) = table.columns.iter().position(|column| column.name.is_empty()) {
                return Some(format!("column {} of {}.{}", column + 1, schema.name, table.name));
            }
        }
        let names = [
            (
                "sequence",
                schema
                    .sequences
                    .iter()
                    .map(|object| object.name.as_str())
                    .collect::<Vec<_>>(),
            ),
            ("type", schema.types.iter().map(|object| object.name.as_str()).collect()),
            (
                "routine",
                schema.routines.iter().map(|object| object.name.as_str()).collect(),
            ),
            (
                "trigger",
                schema.triggers.iter().map(|object| object.name.as_str()).collect(),
            ),
        ];
        for (kind, names) in names {
            if let Some(position) = names.iter().position(|name| name.is_empty()) {
                return Some(within(kind, position));
            }
        }
    }
    None
}

/// Reads a snapshot file.
pub fn load_snapshot(path: &Path) -> Result<Snapshot, SnapshotError> {
    let text = std::fs::read_to_string(path).map_err(|error| SnapshotError {
        message: format!("Cannot read the schema snapshot {}: {error}", path.display()),
    })?;
    read_snapshot(&text).map_err(|error| SnapshotError {
        message: format!("{} ({})", error.message, path.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Generated, TableKind, TypeKind};

    #[test]
    fn reads_a_minimal_snapshot() {
        let snapshot = read_snapshot(
            r#"{ "formatVersion": 1, "schemas": [ { "name": "public", "tables": [ { "name": "users", "columns": [ { "name": "id" } ] } ] } ] }"#,
        )
        .expect("a snapshot");
        let table = &snapshot.schemas[0].tables[0];
        assert_eq!(table.kind, TableKind::Table);
        assert_eq!(table.columns[0].name, "id");
        assert_eq!(table.columns[0].data_type, None);
    }

    #[test]
    fn ignores_what_it_does_not_know() {
        let snapshot = read_snapshot(
            r#"{
                "formatVersion": 3,
                "futureField": { "a": 1 },
                "schemas": [ {
                    "name": "s",
                    "tables": [ { "name": "t", "kind": "hypertable", "columns": [ { "name": "a", "generated": "somehow", "extra": true } ] } ],
                    "types": [ { "name": "e", "kind": "multirange" } ]
                } ]
            }"#,
        )
        .expect("a newer snapshot reads as far as it goes");
        let schema = &snapshot.schemas[0];
        assert_eq!(schema.tables[0].kind, TableKind::Other);
        assert_eq!(schema.tables[0].columns[0].generated, Some(Generated::Other));
        assert_eq!(schema.types[0].kind, TypeKind::Other);
    }

    #[test]
    fn orders_columns_by_their_ordinal() {
        let snapshot = read_snapshot(
            r#"{ "formatVersion": 1, "schemas": [ { "name": "s", "tables": [ { "name": "t", "columns": [ { "name": "b", "ordinal": 2 }, { "name": "a", "ordinal": 1 } ] } ] } ] }"#,
        )
        .expect("a snapshot");
        let names: Vec<&str> = snapshot.schemas[0].tables[0]
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn says_why_a_file_is_no_snapshot() {
        let error = read_snapshot("{ \"schemas\": [] }").expect_err("no version");
        assert_eq!(error.message, "Not a schema snapshot: formatVersion is missing");
        let error =
            read_snapshot("{ \"formatVersion\": 1, \"schemas\": [ { \"tables\": [] } ] }").expect_err("no name");
        assert_eq!(error.message, "Not a schema snapshot: schema 1 has no name");
        let error = read_snapshot(
            r#"{ "formatVersion": 1, "schemas": [ { "name": "s", "tables": [ { "name": "t", "columns": [ { "type": "int" } ] } ] } ] }"#,
        )
        .expect_err("a column without a name");
        assert_eq!(error.message, "Not a schema snapshot: column 1 of s.t has no name");
        let error = read_snapshot("[1, 2").expect_err("broken");
        assert!(
            error.message.starts_with("Not a schema snapshot: "),
            "{}",
            error.message
        );
    }
}
