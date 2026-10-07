//! SQL inside the strings of another language, for a host language server that finds it there.
//!
//! The host builds a [`Fragment`] of the pieces of its strings: literal text with its offsets in
//! the host document, the escapes of the host string, and holes for interpolated expressions or
//! concatenated parts that are not literals. It says what kind of SQL the fragment is, whole
//! statements or a part of one that a query builder takes (a condition, a select list, an
//! `ORDER BY`), and for a part the tables it sees. An [`Analysis`] of the fragment against an
//! [`Environment`] (dialect, version, snapshot, the workspace's DDL, settings) answers what the
//! language server answers for a `.sql` file, in host offsets, with edits escaped for the host
//! string. Nothing here knows LSP; `docs/embedding.md` describes the contract.
//!
//! ```
//! use sql_embed::{Analysis, CompletionOptions, EscapeStyle, Environment, Fragment, FragmentKind};
//! use sql_embed::{HoleKind, Settings, Snapshot, Span};
//!
//! // $db->query('SELECT * FROM users WHERE id = ' . $id);
//! let host = "$db->query('SELECT * FROM users WHERE id = ' . $id);";
//! let start = host.find("SELECT").unwrap() as u32;
//! let mut fragment = Fragment::new(FragmentKind::Statements);
//! fragment.literal("SELECT * FROM users WHERE id = ", start, EscapeStyle::SingleQuoted);
//! let hole = host.find("$id").unwrap() as u32;
//! fragment.hole(Span::new(hole, hole + 3), HoleKind::Value);
//!
//! let snapshot = Snapshot::parse(r#"{"formatVersion": 1, "schemas": [{"name": "app", "tables": [
//!     {"name": "users", "columns": [{"name": "id"}, {"name": "email"}]}]}]}"#).unwrap();
//! let settings = Settings { dialect: sql_embed::Dialect::Mysql, ..Settings::default() };
//! let env = Environment::new(settings, Some(snapshot), None);
//! let analysis = Analysis::new(&env, &fragment);
//! assert!(analysis.diagnostics().is_empty());
//! let cursor = host.find("id =").unwrap() as u32;
//! let list = analysis.completion(cursor, CompletionOptions::default());
//! assert!(list.items.iter().any(|item| item.label == "email"));
//! ```

mod analysis;
mod detect;
mod environment;
mod fragment;
mod map;

pub use analysis::{
    Analysis, CodeAction, CompletionItem, CompletionList, Diagnostic, Edit, FileReferences, Highlight, Hover,
    InlayHint, Location, References, Related, SemanticToken, SqlFile,
};
pub use detect::confidence;
pub use environment::{
    Environment, Settings, Snapshot, Workspace, WorkspaceSchema, dialect_from_json, inspections_from_json, is_sql_file,
    quote_identifiers_from_json, read_sql_file, sql_files, version_from_json,
};
pub use fragment::{EscapeStyle, Fragment, FragmentKind, HoleKind, ScopeTable, Span};
pub use sql_analysis::DiagnosticSeverity;
pub use sql_analysis::actions::ActionKind;
pub use sql_analysis::completion::{CompletionOptions, ItemKind, QuoteIdentifiers};
pub use sql_analysis::inlay_hints::{HintKind, HintOptions};
pub use sql_analysis::inspections::{INSPECTIONS, InspectionSettings, Override};
pub use sql_analysis::refs::{Access, Symbol};
pub use sql_analysis::semantic_tokens::{TOKEN_MODIFIERS, TOKEN_TYPES};
pub use sql_analysis::signature::{ParameterItem, SignatureHelp, SignatureItem};
pub use sql_syntax::{Dialect, Target, Version};

#[cfg(test)]
mod tests;
