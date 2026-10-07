//! Fixtures for tests: a snapshot from compact JSON, a cursor marker in the text, and the name at it.

use sql_catalog::read_snapshot;
use sql_syntax::{Dialect, SyntaxNode, Target, TextSize, parse};

use crate::catalog::{Layer, Origin};
use crate::context::Schemas;
use crate::diagnostics::Diagnostic;
use crate::inspections::{InspectionSettings, Request, inspect};

/// A snapshot of a shop in a dialect, with users, orgs, orders and a few routines and types.
pub fn shop(dialect: Dialect) -> Layer {
    let default = match dialect {
        Dialect::Mysql | Dialect::Mariadb => "shop",
        Dialect::Sqlite => "main",
        _ => "public",
    };
    let json = SHOP.replace("%DEFAULT%", default);
    Layer::new(Origin::Snapshot, read_snapshot(&json).expect("the fixture reads"))
}

const SHOP: &str = r#"{
    "formatVersion": 1,
    "defaultSchema": "%DEFAULT%",
    "schemas": [
        {
            "name": "%DEFAULT%",
            "tables": [
                {
                    "name": "orgs",
                    "comment": "Organizations that buy",
                    "columns": [
                        { "name": "id", "type": "integer", "nullable": false },
                        { "name": "name", "type": "text", "nullable": false }
                    ],
                    "primaryKey": { "name": "orgs_pkey", "columns": ["id"] }
                },
                {
                    "name": "users",
                    "comment": "People who log in",
                    "columns": [
                        { "name": "id", "type": "integer", "nullable": false, "autoIncrement": true },
                        { "name": "org_id", "type": "integer", "nullable": false },
                        { "name": "email", "type": "varchar(255)", "nullable": false, "comment": "Where mail goes" },
                        { "name": "status", "type": "enum('active','blocked')", "default": "'active'" },
                        { "name": "name", "type": "text", "nullable": true }
                    ],
                    "primaryKey": { "columns": ["id"] },
                    "foreignKeys": [ { "name": "users_org", "columns": ["org_id"], "referencedTable": "orgs", "referencedColumns": ["id"], "onDelete": "cascade" } ]
                },
                {
                    "name": "orders",
                    "columns": [
                        { "name": "id", "type": "integer" },
                        { "name": "user_id", "type": "integer" },
                        { "name": "total", "type": "numeric(10,2)" },
                        { "name": "mood", "type": "mood" }
                    ],
                    "foreignKeys": [ { "columns": ["user_id"], "referencedTable": "users", "referencedColumns": ["id"] } ]
                },
                { "name": "active_users", "kind": "view", "columns": [ { "name": "id" }, { "name": "email" } ] }
            ],
            "types": [ { "name": "mood", "kind": "enum", "values": ["happy", "sad"] } ],
            "sequences": [ { "name": "order_numbers" } ],
            "routines": [
                { "name": "order_total", "kind": "function", "parameters": [ { "name": "order_id", "type": "integer" } ], "returns": "numeric", "comment": "The sum of an order" },
                { "name": "archive", "kind": "procedure", "parameters": [ { "name": "before", "type": "date" } ] }
            ]
        },
        {
            "name": "audit",
            "tables": [ { "name": "events", "columns": [ { "name": "at", "type": "timestamp" }, { "name": "what", "type": "text" } ] } ]
        }
    ]
}"#;

/// The text without the cursor marker, its tree and the offset where the marker was.
pub fn split_cursor(text: &str, dialect: Dialect) -> (String, SyntaxNode, u32) {
    let (offset, clean) = lsc_text::testing::cursor(text);
    let root = parse(&clean, dialect).syntax();
    (clean, root, offset)
}

/// The `NAME` the offset is in or right after.
pub fn name_at(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    let token = root.token_at_offset(TextSize::from(offset)).find(|token| {
        token
            .parent()
            .is_some_and(|parent| parent.kind() == sql_syntax::SyntaxKind::NAME)
    })?;
    token.parent()
}

pub fn target(dialect: Dialect) -> Target {
    Target::new(dialect, None)
}

pub fn with_snapshot(layer: &Layer) -> Schemas<'_> {
    Schemas {
        snapshot: Some(layer),
        workspace: None,
    }
}

/// The findings of the inspections of unknown and ambiguous names alone.
pub fn unknown_names(root: &SyntaxNode, target: Target, schemas: Schemas) -> Vec<Diagnostic> {
    let settings = InspectionSettings::default();
    let request = Request::new(target, schemas, &settings);
    inspect(root, &request)
        .into_iter()
        .map(|finding| finding.diagnostic)
        .filter(|diagnostic| diagnostic.code.starts_with("unresolved-") || diagnostic.code == "ambiguous-column")
        .collect()
}
