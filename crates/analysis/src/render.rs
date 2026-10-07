//! How a signature, a column or a table reads in a hover, a completion or signature help.

use sql_catalog::model::{Column, ParameterMode, Routine, Table};
use sql_catalog::{Overload, Param};

/// `name(a integer, b text?)`, with what it returns after a colon.
pub fn overload_signature(name: &str, overload: &Overload) -> String {
    let params: Vec<String> = overload.params.iter().map(Param::label).collect();
    let mut out = format!("{name}({})", params.join(", "));
    if let Some(returns) = &overload.returns {
        out.push_str(": ");
        out.push_str(returns);
    }
    out
}

/// The label of a parameter of a routine of the schema, `name type`, with its mode and default.
pub fn routine_param(parameter: &sql_catalog::model::Parameter) -> String {
    let mut parts = Vec::new();
    match parameter.mode {
        Some(ParameterMode::Out) => parts.push("OUT".to_string()),
        Some(ParameterMode::Inout) => parts.push("INOUT".to_string()),
        Some(ParameterMode::Variadic) => parts.push("VARIADIC".to_string()),
        _ => {}
    }
    if let Some(name) = &parameter.name {
        parts.push(name.clone());
    }
    if let Some(data_type) = &parameter.data_type {
        parts.push(data_type.clone());
    }
    let mut out = parts.join(" ");
    if let Some(default) = &parameter.default {
        out.push_str(&format!(" = {default}"));
    }
    out
}

pub fn routine_signature(routine: &Routine) -> String {
    let params: Vec<String> = routine.parameters.iter().map(routine_param).collect();
    let mut out = format!("{}({})", routine.name, params.join(", "));
    if let Some(returns) = &routine.returns {
        out.push_str(": ");
        out.push_str(returns);
    }
    out
}

/// What a column definition says besides its type: `NOT NULL`, the default, identity and the like.
pub fn column_facts(column: &Column) -> String {
    let mut facts = Vec::new();
    match column.nullable {
        Some(false) => facts.push("NOT NULL".to_string()),
        Some(true) => facts.push("NULL".to_string()),
        None => {}
    }
    if let Some(default) = &column.default {
        facts.push(format!("DEFAULT {default}"));
    }
    if column.auto_increment {
        facts.push("auto-increment".to_string());
    }
    if let Some(generated) = column.generated {
        let mut text = generated.label().to_string();
        if let Some(expression) = &column.generation_expression {
            text.push_str(&format!(" as ({expression})"));
        }
        facts.push(text);
    }
    facts.join(", ")
}

/// `name type facts`, a column as a definition writes it.
pub fn column_line(column: &Column) -> String {
    let mut out = column.name.clone();
    if let Some(data_type) = &column.data_type {
        out.push(' ');
        out.push_str(data_type);
    }
    let facts = column_facts(column);
    if !facts.is_empty() {
        out.push(' ');
        out.push_str(&facts);
    }
    out
}

/// A markdown table of the columns of a table, with the keys it has.
pub fn table_markdown(table: &Table) -> String {
    let mut out = String::new();
    if !table.columns.is_empty() {
        out.push_str("| Column | Type | |\n| --- | --- | --- |\n");
        for column in table.columns.iter().take(60) {
            let mut notes = Vec::new();
            if table
                .primary_key
                .as_ref()
                .is_some_and(|key| key.columns.iter().any(|name| name == &column.name))
            {
                notes.push("PK".to_string());
            }
            if let Some(key) = table
                .foreign_keys
                .iter()
                .find(|key| key.columns.iter().any(|name| name == &column.name))
            {
                notes.push(format!("FK {}", key.referenced_table));
            }
            let facts = column_facts(column);
            if !facts.is_empty() {
                notes.push(facts);
            }
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                column.name,
                column.data_type.as_deref().unwrap_or(""),
                escape_cell(&notes.join(", "))
            ));
        }
        if table.columns.len() > 60 {
            out.push_str(&format!("\n{} more columns\n", table.columns.len() - 60));
        }
    }
    let mut keys = Vec::new();
    if let Some(key) = &table.primary_key {
        keys.push(format!("Primary key ({})", key.columns.join(", ")));
    }
    for key in &table.unique_keys {
        keys.push(format!(
            "Unique{} ({})",
            key.name.as_ref().map(|name| format!(" {name}")).unwrap_or_default(),
            key.columns.join(", ")
        ));
    }
    for key in &table.foreign_keys {
        let mut text = format!(
            "Foreign key ({}) references {}",
            key.columns.join(", "),
            key.referenced_table
        );
        if !key.referenced_columns.is_empty() {
            text.push_str(&format!(" ({})", key.referenced_columns.join(", ")));
        }
        if let Some(action) = &key.on_delete {
            text.push_str(&format!(", on delete {action}"));
        }
        if let Some(action) = &key.on_update {
            text.push_str(&format!(", on update {action}"));
        }
        keys.push(text);
    }
    for index in &table.indexes {
        keys.push(format!(
            "{}Index{} ({})",
            if index.unique { "Unique " } else { "" },
            index.name.as_ref().map(|name| format!(" {name}")).unwrap_or_default(),
            index.columns.join(", ")
        ));
    }
    if !keys.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        for key in keys {
            out.push_str(&format!("- {}\n", escape_cell(&key)));
        }
    }
    out.trim_end().to_string()
}

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}
