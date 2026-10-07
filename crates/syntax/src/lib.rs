//! Lexer, error-tolerant parser and lossless syntax tree for SQL in the SQLite, MySQL, MariaDB and
//! PostgreSQL dialects, and the table of what each version of each dialect accepts.

mod dialect;
mod dump;
mod features;
mod kind;
pub mod lexer;
mod parser;
mod reserved;
mod reserved_words;

pub use dialect::{Dialect, Target, Version};
pub use dump::{DumpOptions, dump, dump_compact};
pub use features::{FEATURES, Feature, FeatureDiagnostic, FeatureSeverity, Support, check_features, supports};
pub use kind::{SqlLanguage, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
pub use parser::{Parse, SyntaxError, parse};
pub use reserved::{RESERVED_WORD, check_reserved_words, is_reserved_word};
pub use rowan::{Direction, TextRange, TextSize, TokenAtOffset, WalkEvent};

#[cfg(test)]
mod tests;
