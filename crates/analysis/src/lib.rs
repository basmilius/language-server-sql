//! Questions about a syntax tree that the SQL language server answers: which diagnostics a script
//! has for its dialect and version, where the symbols are, what folds and how a selection grows.
//! Nothing here knows about LSP or about processes, so the same functions serve any front end.

mod diagnostics;
mod folding;
mod selection;
mod symbols;

pub use diagnostics::{Diagnostic, DiagnosticSeverity, diagnostics};
pub use folding::{Fold, FoldKind, folding_ranges};
pub use lsc_text::{LineCol, LineIndex, PositionEncoding};
pub use selection::selection_ranges;
pub use symbols::{Symbol, SymbolKind, document_symbols};
