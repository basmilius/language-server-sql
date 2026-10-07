//! Questions about a syntax tree that the SQL language server answers: which diagnostics a script
//! has for its dialect and version, where the symbols are, what folds and how a selection grows,
//! what a name stands for, what can be typed at a cursor and what a name is. Nothing here knows
//! about LSP or about processes, so the same functions serve any front end.

pub mod actions;
mod ast;
pub mod catalog;
pub mod completion;
pub mod context;
pub mod ddl;
mod diagnostics;
mod folding;
pub mod ident;
pub mod inlay_hints;
pub mod nav;
pub mod references;
pub mod refs;
pub mod rename;
pub mod render;
pub mod resolve;
mod selection;
pub mod semantic_tokens;
pub mod signature;
mod symbols;
pub mod unresolved;
pub mod workspace;

#[cfg(test)]
mod completion_tests;
#[cfg(test)]
mod nav_tests;
#[cfg(test)]
mod references_tests;
#[cfg(test)]
mod rename_tests;
#[cfg(test)]
mod resolve_tests;
#[cfg(test)]
mod testing;

pub use diagnostics::{Diagnostic, DiagnosticSeverity, diagnostics};
pub use folding::{Fold, FoldKind, folding_ranges};
pub use lsc_text::{LineCol, LineIndex, PositionEncoding};
pub use selection::selection_ranges;
pub use symbols::{Symbol, SymbolKind, document_symbols};
