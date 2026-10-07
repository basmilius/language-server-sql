//! The schema of a database as the server knows it: what a snapshot file holds, and what the DDL
//! of a script or of the workspace defines, in one shape. `docs/snapshot-format.md` describes the
//! JSON for host authors and `docs/snapshot.schema.json` is its JSON Schema.
//!
//! Every field beyond the names is optional, so a host may write as little as the tables and their
//! columns. Fields this version does not know are ignored, and so is an unknown value of a kind,
//! which reads as `Other`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The newest `formatVersion` this server reads in full. A newer snapshot is read as far as the
/// fields go that this version knows.
pub const FORMAT_VERSION: u32 = 1;

/// A schema snapshot: where it was taken, how unqualified names resolve, and the schemas.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    pub format_version: u32,
    pub source: Source,
    /// The schema (PostgreSQL, SQLite) or database (MySQL, MariaDB) an unqualified name means.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_schema: Option<String>,
    /// PostgreSQL's effective `search_path`, `pg_catalog` left out unless it was named.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub search_path: Vec<String>,
    pub schemas: Vec<Schema>,
}

/// The server a snapshot was taken from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Source {
    /// `sqlite`, `mysql`, `mariadb` or `postgres`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    /// The product as the server names itself, such as `PostgreSQL` or `MariaDB`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// What the server reports, such as `8.4.2` or `11.4.3-MariaDB`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The database a PostgreSQL snapshot was taken in, or the file of a SQLite one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// MySQL's and MariaDB's `lower_case_table_names`: 0 compares table and schema names case
    /// sensitively; 1, 2 or nothing compares them without case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lower_case_table_names: Option<u8>,
    /// MySQL's and MariaDB's `@@sql_mode` as the server reports it, such as
    /// `ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES`; without it the server's default is assumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_mode: Option<String>,
    /// When the snapshot was taken, as an RFC 3339 time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<String>,
}

/// A schema of PostgreSQL or SQLite, or a database of MySQL or MariaDB.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Schema {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<Sequence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<UserType>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routines: Vec<Routine>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    #[serde(skip)]
    pub location: Option<Location>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TableKind {
    #[default]
    Table,
    View,
    MaterializedView,
    ForeignTable,
    PartitionedTable,
    /// A table only the session that made it sees.
    Temporary,
    /// A table or view of the database itself, such as `information_schema.columns`.
    System,
    #[serde(other)]
    Other,
}

impl TableKind {
    /// How a hover or a completion names the kind.
    pub fn label(self) -> &'static str {
        match self {
            TableKind::Table | TableKind::Other => "table",
            TableKind::View => "view",
            TableKind::MaterializedView => "materialized view",
            TableKind::ForeignTable => "foreign table",
            TableKind::PartitionedTable => "partitioned table",
            TableKind::Temporary => "temporary table",
            TableKind::System => "system table",
        }
    }

    pub fn is_view(self) -> bool {
        matches!(self, TableKind::View | TableKind::MaterializedView)
    }
}

/// A table, a view or anything else a query reads rows from.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// In the order of the table; `ordinal` breaks a tie when a host writes them in another order.
    pub columns: Vec<Column>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unique_keys: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// The query of a view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// Set when the columns are not all known, as for `CREATE TABLE t AS SELECT ...`: then a column
    /// that is not listed is not reported.
    #[serde(skip)]
    pub open: bool,
    #[serde(skip)]
    pub location: Option<Location>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Generated {
    Stored,
    Virtual,
    IdentityAlways,
    IdentityByDefault,
    #[serde(other)]
    Other,
}

impl Generated {
    pub fn label(self) -> &'static str {
        match self {
            Generated::Stored => "generated, stored",
            Generated::Virtual => "generated, virtual",
            Generated::IdentityAlways => "identity, always",
            Generated::IdentityByDefault => "identity, by default",
            Generated::Other => "generated",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Column {
    pub name: String,
    /// The declared type as the server writes it, such as `character varying(255)`,
    /// `enum('a','b')` or `INTEGER`.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    /// The default as an expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<Generated>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_expression: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto_increment: bool,
    /// MySQL's and MariaDB's invisible column: `SELECT *` leaves it out and an `INSERT` without a
    /// column list does not count it, though a statement may still name it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub invisible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// The position in the table, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
    #[serde(skip)]
    pub location: Option<Location>,
}

/// A primary or unique key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Key {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Index {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Column names; an expression is written as the server writes it.
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    /// `btree`, `hash`, `gin`, `fulltext` and the like.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The `WHERE` of a partial index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ForeignKey {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub columns: Vec<String>,
    /// The schema of the referenced table; without one it is the schema of this table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referenced_schema: Option<String>,
    pub referenced_table: String,
    /// Without columns the primary key of the referenced table is meant.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub referenced_columns: Vec<String>,
    /// `cascade`, `restrict`, `set null`, `set default` or `no action`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Check {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub expression: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sequence {
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub increment: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle: Option<bool>,
    /// The column the sequence belongs to, as `table.column`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip)]
    pub location: Option<Location>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TypeKind {
    Enum,
    Domain,
    Composite,
    Range,
    #[default]
    Base,
    #[serde(other)]
    Other,
}

impl TypeKind {
    pub fn label(self) -> &'static str {
        match self {
            TypeKind::Enum => "enum",
            TypeKind::Domain => "domain",
            TypeKind::Composite => "composite type",
            TypeKind::Range => "range type",
            TypeKind::Base | TypeKind::Other => "type",
        }
    }
}

/// A type the schema defines: an enum, a domain, a composite or range type.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UserType {
    pub name: String,
    pub kind: TypeKind,
    /// The labels of an enum, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// What a domain is based on, or the subtype of a range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_type: Option<String>,
    /// The fields of a composite type.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<Attribute>,
    /// Whether a domain accepts null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip)]
    pub location: Option<Location>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Attribute {
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RoutineKind {
    #[default]
    Function,
    Procedure,
    Aggregate,
    Window,
    #[serde(other)]
    Other,
}

impl RoutineKind {
    pub fn label(self) -> &'static str {
        match self {
            RoutineKind::Function | RoutineKind::Other => "function",
            RoutineKind::Procedure => "procedure",
            RoutineKind::Aggregate => "aggregate function",
            RoutineKind::Window => "window function",
        }
    }
}

/// A function or procedure of the schema. Overloads are separate routines with the same name.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Routine {
    pub name: String,
    pub kind: RoutineKind,
    pub parameters: Vec<Parameter>,
    /// The type a function returns, such as `integer`, `SETOF users` or `TABLE(id integer)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip)]
    pub location: Option<Location>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ParameterMode {
    In,
    Out,
    Inout,
    Variadic,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Parameter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Without a mode the parameter is `in`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ParameterMode>,
    /// The default as an expression; a parameter with one may be left out of a call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Trigger {
    pub name: String,
    /// The table or view the trigger is on, in the same schema.
    pub table: String,
    /// `before`, `after` or `instead of`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<String>,
    /// `insert`, `update`, `delete` or `truncate`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip)]
    pub location: Option<Location>,
}

/// Where DDL defined an object: never set for what a snapshot holds, which has no place in a file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    /// The file; `None` is the document being analyzed.
    pub path: Option<PathBuf>,
    /// The definition, as byte offsets.
    pub range: (u32, u32),
    /// The name in the definition.
    pub name: (u32, u32),
}
