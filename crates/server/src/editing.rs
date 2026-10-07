//! Find references, document highlights and rename, over the open document and the workspace's
//! other `.sql` files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsc_server::paths::{path_to_uri, uri_to_path};
use lsp_types::{
    DocumentChangeOperation, DocumentChanges, DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
    Location, OneOf, OptionalVersionedTextDocumentIdentifier, PrepareRenameResponse, ReferenceParams, RenameParams,
    TextDocumentEdit, TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit,
};
use sql_analysis::references::{Current, OtherFile, highlights, references};
use sql_analysis::refs::{Access, symbol_at_offset};
use sql_analysis::rename::{FileEdits, prepare_rename, rename};
use sql_syntax::{Dialect, TextRange};

use crate::documents::ParseDocument;
use crate::server::{ReadAgainst, Server};

/// A range of a file that is not the document asked about, in the client's positions.
pub(crate) fn location_in(
    path: &Path,
    index: &lsc_server::LineIndex,
    text: &str,
    range: TextRange,
    encoding: lsc_server::PositionEncoding,
) -> Option<Location> {
    let uri = path_to_uri(path)?;
    let (start, end) = index.range(text, range, encoding);
    Some(Location::new(
        uri,
        lsp_types::Range::new(
            lsp_types::Position::new(start.line, start.col),
            lsp_types::Position::new(end.line, end.col),
        ),
    ))
}

/// Another `.sql` file as a search reads it: its text, from the editor when it is open.
pub(crate) struct Loaded {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) against: ReadAgainst,
}

impl Server {
    /// What a file the client has not opened is read against: the settings for its path.
    fn against_path(&mut self, path: &Path) -> ReadAgainst {
        let resolved = self.settings.resolve(Some(path), self.root.as_deref(), None);
        let snapshot = resolved.schema.and_then(|schema| self.snapshot(&schema));
        let workspace = self.workspace.layer(resolved.target.dialect);
        ReadAgainst {
            target: resolved.target,
            snapshot,
            workspace,
        }
    }

    /// The workspace's `.sql` files and the open documents a document of a dialect can name, other
    /// than itself, whose text has one of the words in it in any case: files of its dialect and those without
    /// one, every file for a document without one.
    pub(crate) fn other_files(&mut self, uri: &Uri, dialect: Dialect, words: &[String]) -> Vec<Loaded> {
        let words: Vec<String> = words
            .iter()
            .filter(|word| !word.is_empty())
            .map(|word| word.to_lowercase())
            .collect();
        let mentions = |text: &str| {
            let text = text.to_lowercase();
            words.iter().any(|word| text.contains(word))
        };
        let own = uri_to_path(uri);
        let fits = |other: Dialect| dialect == Dialect::Generic || other == dialect || other == Dialect::Generic;
        let mut found = Vec::new();
        let open: Vec<Uri> = self.documents.uris().into_iter().filter(|other| other != uri).collect();
        let mut seen: Vec<PathBuf> = Vec::new();
        for other in open {
            let Some(path) = uri_to_path(&other) else {
                continue;
            };
            let Some(against) = self.schema_of(&other) else {
                continue;
            };
            if !fits(against.target.dialect) {
                continue;
            }
            let Some(document) = self.documents.get(&other) else {
                continue;
            };
            seen.push(path.clone());
            if !mentions(&document.text) {
                continue;
            }
            found.push(Loaded {
                path,
                text: document.text.clone(),
                against,
            });
        }
        let paths: Vec<(PathBuf, Dialect)> = self
            .workspace
            .paths()
            .filter(|(path, other)| {
                fits(*other) && own.as_deref() != Some(*path) && !seen.iter().any(|known| known == path)
            })
            .map(|(path, other)| (path.to_path_buf(), other))
            .collect();
        for (path, _) in paths {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !mentions(&text) {
                continue;
            }
            let against = self.against_path(&path);
            found.push(Loaded { path, text, against });
        }
        found
    }

    /// The name of what the name at a position stands for, when another file can name it too; an
    /// empty name for a symbol of the document alone, which no other file is read for.
    fn wanted_name(&mut self, uri: &Uri, against: &ReadAgainst, position: lsp_types::Position) -> Option<String> {
        let encoding = self.encoding;
        let document = self.documents.get_mut(uri)?;
        let root = document.parse().syntax();
        let offset = document.mapper(encoding).offset(position);
        let (_, symbol, _) = symbol_at_offset(&root, offset.into(), against.target, against.schemas())?;
        Some(if symbol.is_local() {
            String::new()
        } else {
            symbol.name().to_string()
        })
    }

    pub(crate) fn references(&mut self, params: ReferenceParams) -> Option<Vec<Location>> {
        let position = params.text_document_position;
        let uri = position.text_document.uri;
        let against = self.schema_of(&uri)?;
        let wanted = self.wanted_name(&uri, &against, position.position)?;
        let others = self.other_files(&uri, against.target.dialect, &[wanted]);
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(position.position);
        let current = Current {
            root: &root,
            target: against.target,
            schemas: against.schemas(),
        };
        let files: Vec<OtherFile> = others
            .iter()
            .map(|other| OtherFile {
                path: &other.path,
                text: &other.text,
                target: other.against.target,
                schemas: other.against.schemas(),
            })
            .collect();
        let found = references(&current, offset.into(), params.context.include_declaration, &files)?;
        let mut locations = Vec::new();
        for file in &found.files {
            match &file.path {
                None => locations.extend(
                    file.hits
                        .iter()
                        .map(|hit| Location::new(uri.clone(), mapper.range(hit.range))),
                ),
                Some(path) => {
                    let Some(other) = others.iter().find(|other| &other.path == path) else {
                        continue;
                    };
                    let index = lsc_server::LineIndex::new(&other.text);
                    locations.extend(
                        file.hits
                            .iter()
                            .filter_map(|hit| location_in(path, &index, &other.text, hit.range, encoding)),
                    );
                }
            }
        }
        Some(locations)
    }

    pub(crate) fn document_highlight(&mut self, params: DocumentHighlightParams) -> Option<Vec<DocumentHighlight>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let against = self.schema_of(&uri)?;
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(position.position);
        let hits = highlights(&root, offset.into(), against.target, against.schemas());
        Some(
            hits.iter()
                .map(|hit| DocumentHighlight {
                    range: mapper.range(hit.range),
                    kind: Some(match hit.access {
                        Access::Read => DocumentHighlightKind::READ,
                        Access::Write | Access::Declaration => DocumentHighlightKind::WRITE,
                    }),
                })
                .collect(),
        )
    }

    pub(crate) fn prepare_rename(
        &mut self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>, String> {
        let uri = params.text_document.uri;
        let Some(against) = self.schema_of(&uri) else {
            return Ok(None);
        };
        let encoding = self.encoding;
        let Some(document) = self.documents.get_mut(&uri) else {
            return Ok(None);
        };
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(params.position);
        let current = Current {
            root: &root,
            target: against.target,
            schemas: against.schemas(),
        };
        let prepared = prepare_rename(&current, offset.into())?;
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: mapper.range(prepared.range),
            placeholder: prepared.placeholder,
        }))
    }

    // `WorkspaceEdit::changes` is keyed by `Uri`, which hashes by its text alone.
    #[allow(clippy::mutable_key_type)]
    pub(crate) fn rename(&mut self, params: RenameParams) -> Result<Option<WorkspaceEdit>, String> {
        let position = params.text_document_position;
        let uri = position.text_document.uri;
        let Some(against) = self.schema_of(&uri) else {
            return Ok(None);
        };
        // A file that defines the new name is read too, which the rename must not clash with.
        let wanted = self.wanted_name(&uri, &against, position.position).unwrap_or_default();
        let new_name = if wanted.is_empty() {
            String::new()
        } else {
            params.new_name.trim().trim_matches(['"', '`', '[', ']']).to_string()
        };
        let others = self.other_files(&uri, against.target.dialect, &[wanted, new_name]);
        let encoding = self.encoding;
        let document_changes = self.document_changes_support;
        let versions: Vec<(Uri, i32)> = self
            .documents
            .uris()
            .into_iter()
            .filter_map(|open| {
                let version = self.documents.get(&open)?.version;
                Some((open, version))
            })
            .collect();
        let Some(document) = self.documents.get_mut(&uri) else {
            return Ok(None);
        };
        let version = document.version;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(position.position);
        let current = Current {
            root: &root,
            target: against.target,
            schemas: against.schemas(),
        };
        let files: Vec<OtherFile> = others
            .iter()
            .map(|other| OtherFile {
                path: &other.path,
                text: &other.text,
                target: other.against.target,
                schemas: other.against.schemas(),
            })
            .collect();
        let found = rename(&current, offset.into(), &params.new_name, &files)?;
        let mut edits: Vec<(Uri, Option<i32>, Vec<TextEdit>)> = Vec::new();
        for FileEdits { path, edits: changes } in found {
            match path {
                None => edits.push((
                    uri.clone(),
                    Some(version),
                    changes
                        .into_iter()
                        .map(|edit| TextEdit::new(mapper.range(edit.range), edit.text))
                        .collect(),
                )),
                Some(path) => {
                    let Some(other) = others.iter().find(|other| other.path == path) else {
                        continue;
                    };
                    let Some(other_uri) = path_to_uri(&path) else {
                        continue;
                    };
                    let open_version = versions
                        .iter()
                        .find(|(open, _)| *open == other_uri)
                        .map(|(_, version)| *version);
                    let index = lsc_server::LineIndex::new(&other.text);
                    let converted = changes
                        .into_iter()
                        .filter_map(|edit| {
                            let location = location_in(&path, &index, &other.text, edit.range, encoding)?;
                            Some(TextEdit::new(location.range, edit.text))
                        })
                        .collect();
                    edits.push((other_uri, open_version, converted));
                }
            }
        }
        if edits.is_empty() {
            return Ok(None);
        }
        if document_changes {
            let operations = edits
                .into_iter()
                .map(|(uri, version, changes)| {
                    DocumentChangeOperation::Edit(TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier { uri, version },
                        edits: changes.into_iter().map(OneOf::Left).collect(),
                    })
                })
                .collect();
            return Ok(Some(WorkspaceEdit {
                document_changes: Some(DocumentChanges::Operations(operations)),
                ..WorkspaceEdit::default()
            }));
        }
        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
        for (uri, _, found) in edits {
            changes.entry(uri).or_default().extend(found);
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }))
    }
}
