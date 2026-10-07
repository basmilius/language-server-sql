//! Requests that color and annotate one document: semantic tokens and inlay hints.

use lsp_types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams, SemanticToken, SemanticTokenModifier, SemanticTokenType,
    SemanticTokens, SemanticTokensLegend, SemanticTokensParams, SemanticTokensRangeParams, SemanticTokensRangeResult,
    SemanticTokensResult, Uri,
};
use sql_analysis::inlay_hints::{HintOptions, inlay_hints};
use sql_analysis::semantic_tokens::{TOKEN_MODIFIERS, TOKEN_TYPES, semantic_tokens};
use sql_syntax::TextSize;

use crate::config::Settings;
use crate::documents::ParseDocument;
use crate::server::Server;

/// The token types and modifiers a client is told to expect, in the order the analysis numbers them.
pub(crate) fn semantic_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.iter().map(|name| SemanticTokenType::new(name)).collect(),
        token_modifiers: TOKEN_MODIFIERS
            .iter()
            .map(|name| SemanticTokenModifier::new(name))
            .collect(),
    }
}

impl Server {
    /// The settings a document is read with: the client's answer for it, else what it pushed.
    pub(crate) fn settings_for(&self, uri: &Uri) -> &Settings {
        self.documents
            .get(uri)
            .and_then(|document| document.state.settings.as_ref())
            .unwrap_or(&self.settings)
    }

    /// The tokens of a document, or of a range of it, in the relative encoding of LSP.
    fn tokens_of(&mut self, uri: &Uri, range: Option<lsp_types::Range>) -> Option<Vec<SemanticToken>> {
        let against = self.schema_of(uri)?;
        let encoding = self.encoding;
        let document = self.documents.get_mut(uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let range = range.map(|range| mapper.text_range(range));
        let tokens = semantic_tokens(&root, against.target, against.schemas(), range);
        let mut data = Vec::with_capacity(tokens.len());
        let (mut previous_line, mut previous_start) = (0, 0);
        for token in tokens {
            let start = mapper.position(TextSize::from(token.start));
            let end = mapper.position(TextSize::from(token.end));
            if end.line != start.line {
                continue;
            }
            let delta_line = start.line - previous_line;
            let delta_start = if delta_line == 0 {
                start.character - previous_start
            } else {
                start.character
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: end.character - start.character,
                token_type: token.ty,
                token_modifiers_bitset: token.modifiers,
            });
            previous_line = start.line;
            previous_start = start.character;
        }
        Some(data)
    }

    pub(crate) fn semantic_tokens_full(&mut self, params: SemanticTokensParams) -> Option<SemanticTokensResult> {
        let data = self.tokens_of(&params.text_document.uri, None)?;
        Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data }))
    }

    pub(crate) fn semantic_tokens_range(
        &mut self,
        params: SemanticTokensRangeParams,
    ) -> Option<SemanticTokensRangeResult> {
        let data = self.tokens_of(&params.text_document.uri, Some(params.range))?;
        Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        }))
    }

    pub(crate) fn inlay_hints(&mut self, params: InlayHintParams) -> Option<Vec<InlayHint>> {
        let uri = params.text_document.uri;
        let hints = self.settings_for(&uri).hints;
        let options = HintOptions {
            insert_columns: hints.insert_columns.unwrap_or(true),
            select_columns: hints.select_columns.unwrap_or(true),
            parameter_names: hints.parameter_names.unwrap_or(true),
        };
        let against = self.schema_of(&uri)?;
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let range = mapper.text_range(params.range);
        let found = inlay_hints(&root, against.target, against.schemas(), Some(range), options);
        Some(
            found
                .into_iter()
                .map(|hint| InlayHint {
                    position: mapper.position(TextSize::from(hint.offset)),
                    label: InlayHintLabel::String(hint.label),
                    // LSP knows types and parameters; the column a value goes to is the latter's kind.
                    kind: Some(InlayHintKind::PARAMETER),
                    text_edits: None,
                    tooltip: None,
                    padding_left: Some(false),
                    padding_right: Some(true),
                    data: None,
                })
                .collect(),
        )
    }
}
