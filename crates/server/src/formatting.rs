//! `textDocument/formatting`, `rangeFormatting` and `onTypeFormatting` after a `;`.

use lsp_types::{
    DocumentFormattingParams, DocumentOnTypeFormattingParams, DocumentRangeFormattingParams, FormattingOptions,
    TextEdit, Uri,
};
use sql_format::{Edit, FormatOptions, Indent, KeywordCase};
use sql_syntax::{Dialect, TextRange, TextSize};

use crate::config::KeywordCase as SettingCase;
use crate::server::Server;

impl Server {
    /// What the client asks for decides tabs or spaces; the settings decide the rest, a width of
    /// their own included.
    fn format_options_for(&self, uri: &Uri, asked: &FormattingOptions) -> FormatOptions {
        let settings = self.settings_for(uri).format;
        let width = settings
            .indent_width
            .unwrap_or_else(|| usize::try_from(asked.tab_size).unwrap_or(4).clamp(1, 16));
        FormatOptions {
            indent: if asked.insert_spaces {
                Indent::Spaces(width)
            } else {
                Indent::Tab
            },
            keyword_case: match settings.keyword_case {
                None | Some(SettingCase::Upper) => KeywordCase::Upper,
                Some(SettingCase::Lower) => KeywordCase::Lower,
                Some(SettingCase::Preserve) => KeywordCase::Preserve,
            },
            leading_commas: settings.leading_commas.unwrap_or(false),
        }
    }

    /// Runs the formatter over an open document and turns what it changes into edits. `None` when
    /// the text is left as it is, which a client shows as nothing to do.
    fn formatted_edits(
        &self,
        uri: &Uri,
        asked: &FormattingOptions,
        run: impl FnOnce(&str, Dialect, &FormatOptions) -> Option<Vec<Edit>>,
    ) -> Option<Vec<TextEdit>> {
        let options = self.format_options_for(uri, asked);
        let document = self.documents.get(uri)?;
        let edits = run(&document.text, document.state.target.dialect, &options)?;
        let mapper = document.mapper(self.encoding);
        Some(
            edits
                .into_iter()
                .map(|edit| TextEdit {
                    range: mapper.range(TextRange::new(
                        TextSize::from(edit.start as u32),
                        TextSize::from(edit.end as u32),
                    )),
                    new_text: edit.text,
                })
                .collect(),
        )
    }

    pub(crate) fn formatting(&mut self, params: DocumentFormattingParams) -> Option<Vec<TextEdit>> {
        self.formatted_edits(&params.text_document.uri, &params.options, sql_format::edits)
    }

    pub(crate) fn range_formatting(&mut self, params: DocumentRangeFormattingParams) -> Option<Vec<TextEdit>> {
        let uri = params.text_document.uri;
        let document = self.documents.get(&uri)?;
        let mapper = document.mapper(self.encoding);
        let start = usize::from(mapper.offset(params.range.start));
        let end = usize::from(mapper.offset(params.range.end));
        self.formatted_edits(&uri, &params.options, |text, dialect, options| {
            sql_format::range_edits(text, dialect, start, end, options)
        })
    }

    /// The edits for a typed `;`: the statement it ends is laid out.
    pub(crate) fn on_type_formatting(&mut self, params: DocumentOnTypeFormattingParams) -> Option<Vec<TextEdit>> {
        let typed = params.ch.chars().next().filter(|typed| *typed == ';')?;
        let uri = &params.text_document_position.text_document.uri;
        let document = self.documents.get(uri)?;
        let mapper = document.mapper(self.encoding);
        let offset = usize::from(mapper.offset(params.text_document_position.position));
        self.formatted_edits(uri, &params.options, |text, dialect, options| {
            sql_format::on_type_edits(text, dialect, offset, typed, options)
        })
    }
}
