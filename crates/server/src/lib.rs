//! A SQL language server over LSP: document sync, the dialect and version of each document from the
//! settings, diagnostics for syntax and for what the dialect accepts, document symbols, folding and
//! selection ranges.

mod config;
mod convert;
mod documents;
mod server;

pub use server::run;
