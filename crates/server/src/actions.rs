//! `textDocument/codeAction`: rewrites at a cursor or a selection, the quick fixes and suppressions
//! of the inspections, and fixes applied to the whole document.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse, NumberOrString, TextEdit,
    WorkspaceEdit,
};
use sql_analysis::actions::{Action, ActionKind, code_actions, fix_all_action};

use crate::documents::ParseDocument;
use crate::server::Server;

/// The kind of the actions that apply a fix to every finding of a document.
pub(crate) const FIX_ALL: &str = "source.fixAll.sql";

impl Server {
    // `WorkspaceEdit::changes` is keyed by `Uri`, which hashes by its text alone.
    #[allow(clippy::mutable_key_type)]
    pub(crate) fn code_actions(&mut self, params: CodeActionParams) -> Option<CodeActionResponse> {
        let uri = params.text_document.uri;
        let against = self.schema_of(&uri)?;
        let settings = self.settings_for(&uri).inspections.clone();
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
        // A client that asks for `source.fixAll` by name, as one does on save, gets every safe fix
        // as one action; the fixes of one inspection at a time come with the quick fixes.
        let fix_all_asked = only.is_some_and(|only| {
            only.iter()
                .any(|asked| FIX_ALL == asked.as_str() || FIX_ALL.starts_with(&format!("{}.", asked.as_str())))
        });
        let mut actions: Vec<Action> = code_actions(&root, against.target, against.schemas(), &settings, range)
            .into_iter()
            .filter(|action| action.kind != ActionKind::FixAll || !fix_all_asked)
            .collect();
        if fix_all_asked {
            actions.extend(fix_all_action(&root, against.target, against.schemas(), &settings));
        }
        let mut out = Vec::new();
        for action in actions {
            let kind = match action.kind {
                ActionKind::QuickFix => CodeActionKind::QUICKFIX,
                ActionKind::Rewrite => CodeActionKind::REFACTOR_REWRITE,
                ActionKind::FixAll => CodeActionKind::new(FIX_ALL),
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
