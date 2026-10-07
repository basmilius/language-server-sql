use std::path::PathBuf;

use sql_syntax::{Dialect, Parse, Target, parse};

use crate::config::Settings;

/// An open document: its text, the tree of that text and what it is read as.
pub type Document = lsc_server::Document<Parse, DocumentState>;

pub type Documents = lsc_server::Documents<Parse, DocumentState>;

/// What the server keeps about an open document besides its text.
#[derive(Default)]
pub struct DocumentState {
    /// The dialect the client's language id names, when it names one.
    pub language: Option<Dialect>,
    /// The client's answer to `workspace/configuration` for this document, when it gave one.
    pub settings: Option<Settings>,
    /// The dialect and version the document is read at.
    pub target: Target,
    /// The schema snapshot that applies to the document.
    pub schema: Option<PathBuf>,
    /// The dialect the cached tree was parsed in.
    parsed_in: Option<Dialect>,
}

pub trait ParseDocument {
    /// The tree of the current text in the document's dialect.
    fn parse(&mut self) -> &Parse;

    /// Reads the document at another target, and forgets its tree when the dialect changed. Gives
    /// whether anything changed.
    fn retarget(&mut self, target: Target, schema: Option<PathBuf>) -> bool;
}

impl ParseDocument for Document {
    fn parse(&mut self) -> &Parse {
        let dialect = self.state.target.dialect;
        if self.cached().is_some() && self.state.parsed_in != Some(dialect) {
            self.forget_tree();
        }
        self.state.parsed_in = Some(dialect);
        self.parse_with(|text| parse(text, dialect))
    }

    fn retarget(&mut self, target: Target, schema: Option<PathBuf>) -> bool {
        let changed = self.state.target != target || self.state.schema != schema;
        self.state.target = target;
        self.state.schema = schema;
        changed
    }
}

trait ForgetTree {
    fn forget_tree(&mut self);
}

impl ForgetTree for Document {
    /// Applying no changes is how the shared document drops its tree while keeping its text.
    fn forget_tree(&mut self) {
        let version = self.version;
        self.apply_changes(version, &[], lsc_server::PositionEncoding::Utf16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
    use sql_syntax::SyntaxKind;

    #[test]
    fn parses_again_when_the_dialect_changes() {
        let mut document = Document::new(1, "SELECT \"a\";".to_string());
        let kind_of_string = |document: &mut Document| {
            document
                .parse()
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| token.text() == "\"a\"")
                .map(|token| token.kind())
        };
        assert_eq!(kind_of_string(&mut document), Some(SyntaxKind::QUOTED_IDENT));
        assert!(document.retarget(Target::new(Dialect::Mysql, None), None));
        assert_eq!(kind_of_string(&mut document), Some(SyntaxKind::STRING));
        assert!(!document.retarget(Target::new(Dialect::Mysql, None), None));
        assert_eq!(document.text, "SELECT \"a\";");
    }

    #[test]
    fn parses_the_text_after_incremental_changes() {
        let mut document = Document::new(1, "SELECT 1;\nSELECT 2;\n".to_string());
        document.apply_changes(
            2,
            &[TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 7), Position::new(1, 8))),
                range_length: None,
                text: "42".to_string(),
            }],
            lsc_server::PositionEncoding::Utf16,
        );
        assert_eq!(document.text, "SELECT 1;\nSELECT 42;\n");
        assert!(document.parse().errors().is_empty());
    }
}
