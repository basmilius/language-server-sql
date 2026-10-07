//! `textDocument/codeAction`: rewrites at a cursor or a selection, and quick fixes for unknown names.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse, NumberOrString, TextEdit,
    WorkspaceEdit,
};
use sql_analysis::actions::{ActionKind, code_actions};

use crate::documents::ParseDocument;
use crate::server::Server;

impl Server {
    // `WorkspaceEdit::changes` is keyed by `Uri`, which hashes by its text alone.
    #[allow(clippy::mutable_key_type)]
    pub(crate) fn code_actions(&mut self, params: CodeActionParams) -> Option<CodeActionResponse> {
        let uri = params.text_document.uri;
        let against = self.schema_of(&uri)?;
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let range = mapper.text_range(params.range);
        let only = params.context.only.as_deref();
        let wanted = |kind: &CodeActionKind| {
            only.is_none_or(|only| {
                only.iter().any(|asked| {
                    kind.as_str() == asked.as_str() || kind.as_str().starts_with(&format!("{}.", asked.as_str()))
                })
            })
        };
        let mut out = Vec::new();
        for action in code_actions(&root, against.target, against.schemas(), range) {
            let kind = match action.kind {
                ActionKind::QuickFix => CodeActionKind::QUICKFIX,
                ActionKind::Rewrite => CodeActionKind::REFACTOR_REWRITE,
            };
            if !wanted(&kind) {
                continue;
            }
            let diagnostics = action.fixes.map(|(fixed, code)| {
                let fixed = mapper.range(fixed);
                params
                    .context
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        diagnostic.range == fixed && diagnostic.code == Some(NumberOrString::String(code.to_string()))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let edits = action
                .edits
                .iter()
                .map(|edit| TextEdit::new(mapper.range(edit.range), edit.text.clone()))
                .collect();
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: action.title,
                kind: Some(kind),
                diagnostics: diagnostics.filter(|found| !found.is_empty()),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri.clone(), edits)])),
                    ..WorkspaceEdit::default()
                }),
                is_preferred: action.preferred.then_some(true),
                ..CodeAction::default()
            }));
        }
        Some(out)
    }
}
