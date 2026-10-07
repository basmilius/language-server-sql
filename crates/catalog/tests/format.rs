//! The documentation of the snapshot format holds to the model: its examples read, and the JSON
//! Schema names exactly the fields the model knows.

use std::collections::BTreeSet;

use serde_json::Value;
use sql_catalog::read_snapshot;

const FORMAT: &str = include_str!("../../../docs/snapshot-format.md");
const SCHEMA: &str = include_str!("../../../docs/snapshot.schema.json");

fn json_blocks() -> Vec<&'static str> {
    FORMAT
        .split("```json\n")
        .skip(1)
        .map(|block| block.split("\n```").next().expect("a closed block"))
        .collect()
}

fn keys(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                out.insert(key.clone());
                keys(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
        _ => {}
    }
}

fn schema_properties(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                out.extend(properties.keys().cloned());
            }
            map.values().for_each(|inner| schema_properties(inner, out));
        }
        Value::Array(items) => items.iter().for_each(|item| schema_properties(item, out)),
        _ => {}
    }
}

#[test]
fn the_examples_read() {
    let blocks = json_blocks();
    assert_eq!(blocks.len(), 2);
    for block in blocks {
        read_snapshot(block).unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn the_full_example_the_model_and_the_json_schema_name_the_same_fields() {
    let full = json_blocks()[1];
    let written: Value = serde_json::from_str(full).expect("JSON");
    let mut example = BTreeSet::new();
    keys(&written, &mut example);
    let snapshot = read_snapshot(full).expect("a snapshot");
    let mut model = BTreeSet::new();
    keys(&serde_json::to_value(&snapshot).expect("serializes"), &mut model);
    assert_eq!(example, model, "the model keeps every field of the full example");
    let schema: Value = serde_json::from_str(SCHEMA).expect("the JSON Schema is JSON");
    let mut described = BTreeSet::new();
    schema_properties(&schema, &mut described);
    // The example is of PostgreSQL, which has neither lower_case_table_names nor sql_mode.
    model.insert("lowerCaseTableNames".to_string());
    model.insert("sqlMode".to_string());
    assert_eq!(
        described, model,
        "the JSON Schema describes exactly the fields of the model"
    );
}
