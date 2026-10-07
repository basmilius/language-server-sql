//! A SQL language server over LSP: document sync, the dialect and version of each document from the
//! settings, schema snapshots and the DDL of the workspace, diagnostics for syntax, for what the
//! dialect accepts and for unknown names, completion, hover, definition, signature help, document
//! symbols, folding and selection ranges.

mod config;
mod convert;
mod documents;
mod files;
mod server;
mod snapshots;

pub use server::run;
