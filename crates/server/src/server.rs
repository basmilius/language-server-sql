use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::Sender;
use lsc_server::paths::{path_to_uri, uri_to_path};
use lsc_server::{BoxError, Client, Handler, PositionEncoding};
use lsp_server::{Connection, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    DidSaveTextDocument, Notification as _, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentDiagnosticRequest, DocumentHighlightRequest, DocumentSymbolRequest,
    FoldingRangeRequest, Formatting, GotoDefinition, HoverRequest, InlayHintRefreshRequest, InlayHintRequest,
    OnTypeFormatting, PrepareRenameRequest, RangeFormatting, References, RegisterCapability, Rename, Request as _,
    SelectionRangeRequest, SemanticTokensFullRequest, SemanticTokensRangeRequest, SemanticTokensRefresh,
    SignatureHelpRequest, WorkspaceConfiguration, WorkspaceDiagnosticRefresh,
};
use lsp_types::{
    CodeActionKind, CodeActionOptions, CodeActionProviderCapability, CompletionOptions, CompletionParams,
    CompletionResponse, ConfigurationItem, ConfigurationParams, DiagnosticOptions, DiagnosticServerCapabilities,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    DocumentOnTypeFormattingOptions, DocumentSymbolParams, DocumentSymbolResponse, FileChangeType, FileSystemWatcher,
    FoldingRangeParams, FoldingRangeProviderCapability, FullDocumentDiagnosticReport, GlobPattern,
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, HoverProviderCapability, InitializeParams,
    InitializeResult, Location, MessageType, OneOf, PublishDiagnosticsParams, Registration, RegistrationParams,
    RelatedFullDocumentDiagnosticReport, RenameOptions, SaveOptions, SelectionRangeParams,
    SelectionRangeProviderCapability, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, ShowMessageParams, SignatureHelpOptions,
    SignatureHelpParams, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, Uri,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sql_analysis::catalog::Layer;
use sql_analysis::completion::complete;
use sql_analysis::context::Schemas;
use sql_analysis::{diagnostics, document_symbols, folding_ranges, selection_ranges};
use sql_syntax::{Dialect, Target};

use crate::config::{SECTION, Settings, dialect_of_language};
use crate::convert;
use crate::documents::{Documents, ParseDocument};
use crate::files::{self, WorkspaceFiles};
use crate::snapshots::Snapshots;

/// What a document is read against: its target, its snapshot and the DDL of the workspace.
pub(crate) struct ReadAgainst {
    pub(crate) target: Target,
    pub(crate) snapshot: Option<Arc<Layer>>,
    pub(crate) workspace: Option<Arc<Layer>>,
}

impl ReadAgainst {
    pub(crate) fn schemas(&self) -> Schemas<'_> {
        Schemas {
            snapshot: self.snapshot.as_deref(),
            workspace: self.workspace.as_deref(),
        }
    }
}

/// What background work tells the server.
pub enum Event {
    /// The `.sql` files of the workspace, read at startup.
    Scanned(Vec<(PathBuf, Dialect, sql_analysis::workspace::FileDdl)>),
}

/// Runs the server on a connection until the client shuts it down.
pub fn run(connection: Connection) -> Result<(), BoxError> {
    let (id, raw) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(raw)?;
    let mut server = Server::new(&connection, &params);
    let result = InitializeResult {
        capabilities: server.capabilities(),
        server_info: Some(ServerInfo {
            name: "sql-language-server".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    // `initialize_finish` waits for and consumes the `initialized` notification.
    connection.initialize_finish(id, serde_json::to_value(result)?)?;
    server.report_problems();
    let (sender, events) = crossbeam_channel::unbounded::<Event>();
    server.initialized(sender)?;
    lsc_server::main_loop(&connection, &events, &mut server)
}

pub(crate) struct Server {
    pub(crate) client: Client,
    pub(crate) documents: Documents,
    pub(crate) encoding: PositionEncoding,
    /// What `initializationOptions` and `didChangeConfiguration` said, for every document the client
    /// gives no answer of its own for.
    pub(crate) settings: Settings,
    /// What could not be read in the settings, told to the client once it is ready.
    pub(crate) problems: Vec<String>,
    /// The first workspace folder, which relative paths in the settings start from.
    pub(crate) root: Option<PathBuf>,
    /// The client pulls diagnostics, so the server does not push them.
    pub(crate) pull_diagnostics: bool,
    pub(crate) hierarchical_symbols: bool,
    pub(crate) configuration_support: bool,
    pub(crate) diagnostic_refresh_support: bool,
    /// Documents whose diagnostics are out of date, published once the queue of messages is empty.
    pub(crate) dirty: Vec<Uri>,
    /// Questions asked of the client: which document each `workspace/configuration` answer is for.
    pub(crate) pending_configuration: HashMap<RequestId, Uri>,
    /// Every workspace folder, which the `.sql` files are read from.
    pub(crate) folders: Vec<PathBuf>,
    pub(crate) snapshots: Snapshots,
    pub(crate) workspace: WorkspaceFiles,
    /// The client watches files for the server, which it registers for.
    pub(crate) watch_support: bool,
    /// Snapshot files the client watches.
    pub(crate) watched: Vec<PathBuf>,
    pub(crate) snippet_support: bool,
    pub(crate) label_details_support: bool,
    pub(crate) semantic_tokens_refresh_support: bool,
    pub(crate) inlay_hint_refresh_support: bool,
    /// The client takes a `WorkspaceEdit` as `documentChanges`, with the version of each document.
    pub(crate) document_changes_support: bool,
}

fn folders_of(params: &InitializeParams) -> Vec<PathBuf> {
    let folders: Vec<PathBuf> = params
        .workspace_folders
        .iter()
        .flatten()
        .filter_map(|folder| uri_to_path(&folder.uri))
        .collect();
    if !folders.is_empty() {
        return folders;
    }
    root_of(params).into_iter().collect()
}

fn root_of(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(folder) = params.workspace_folders.iter().flatten().next() {
        return uri_to_path(&folder.uri);
    }
    #[allow(deprecated)]
    params.root_uri.as_ref().and_then(uri_to_path)
}

impl Server {
    fn new(connection: &Connection, params: &InitializeParams) -> Server {
        let capabilities = &params.capabilities;
        let (settings, problems) = params
            .initialization_options
            .as_ref()
            .map(Settings::from_value)
            .unwrap_or_default();
        let text_document = capabilities.text_document.as_ref();
        let workspace = capabilities.workspace.as_ref();
        Server {
            client: Client::new(connection.sender.clone()),
            documents: Documents::default(),
            encoding: lsc_server::choose_encoding(
                capabilities
                    .general
                    .as_ref()
                    .and_then(|general| general.position_encodings.as_deref()),
            ),
            settings,
            problems,
            root: root_of(params),
            pull_diagnostics: text_document.is_some_and(|text_document| text_document.diagnostic.is_some()),
            hierarchical_symbols: text_document
                .and_then(|text_document| text_document.document_symbol.as_ref())
                .and_then(|symbol| symbol.hierarchical_document_symbol_support)
                .unwrap_or(false),
            configuration_support: workspace.and_then(|workspace| workspace.configuration).unwrap_or(false),
            diagnostic_refresh_support: workspace
                .and_then(|workspace| workspace.diagnostic.as_ref())
                .and_then(|diagnostic| diagnostic.refresh_support)
                .unwrap_or(false),
            dirty: Vec::new(),
            pending_configuration: HashMap::new(),
            folders: folders_of(params),
            snapshots: Snapshots::default(),
            workspace: WorkspaceFiles::default(),
            watch_support: workspace
                .and_then(|workspace| workspace.did_change_watched_files.as_ref())
                .and_then(|watched| watched.dynamic_registration)
                .unwrap_or(false),
            watched: Vec::new(),
            snippet_support: text_document
                .and_then(|text_document| text_document.completion.as_ref())
                .and_then(|completion| completion.completion_item.as_ref())
                .and_then(|item| item.snippet_support)
                .unwrap_or(false),
            label_details_support: text_document
                .and_then(|text_document| text_document.completion.as_ref())
                .and_then(|completion| completion.completion_item.as_ref())
                .and_then(|item| item.label_details_support)
                .unwrap_or(false),
            semantic_tokens_refresh_support: workspace
                .and_then(|workspace| workspace.semantic_tokens.as_ref())
                .and_then(|tokens| tokens.refresh_support)
                .unwrap_or(false),
            inlay_hint_refresh_support: workspace
                .and_then(|workspace| workspace.inlay_hint.as_ref())
                .and_then(|hints| hints.refresh_support)
                .unwrap_or(false),
            document_changes_support: workspace
                .and_then(|workspace| workspace.workspace_edit.as_ref())
                .and_then(|edit| edit.document_changes)
                .unwrap_or(false),
        }
    }

    /// The client is ready: ask it to watch the `.sql` files and read them in the background.
    fn initialized(&mut self, events: Sender<Event>) -> Result<(), BoxError> {
        if self.watch_support {
            self.register_watchers("sql-files", vec!["**/*.sql".to_string()])?;
        }
        let folders = self.folders.clone();
        if folders.is_empty() {
            return Ok(());
        }
        let settings = self.settings.clone();
        let root = self.root.clone();
        std::thread::spawn(move || {
            let found = files::scan(&folders, |path| {
                settings.resolve(Some(path), root.as_deref(), None).target.dialect
            });
            if !found.is_empty() {
                let _ = events.send(Event::Scanned(found));
            }
        });
        Ok(())
    }

    fn register_watchers(&mut self, id: &str, globs: Vec<String>) -> Result<(), BoxError> {
        let watchers = globs
            .into_iter()
            .map(|glob| FileSystemWatcher {
                glob_pattern: GlobPattern::String(glob),
                kind: None,
            })
            .collect();
        let options = DidChangeWatchedFilesRegistrationOptions { watchers };
        self.client.request::<RegisterCapability>(RegistrationParams {
            registrations: vec![Registration {
                id: id.to_string(),
                method: DidChangeWatchedFiles::METHOD.to_string(),
                register_options: Some(serde_json::to_value(options)?),
            }],
        })?;
        Ok(())
    }

    /// Tells the client once that a snapshot cannot be read.
    fn report_snapshot_problem(&self, problem: String) {
        self.client
            .log(MessageType::WARNING, format!("sql-language-server: {problem}"));
        let _ = self.client.notify::<ShowMessage>(ShowMessageParams {
            typ: MessageType::WARNING,
            message: problem,
        });
    }

    /// The snapshot at a path, read when it is first asked for and then watched.
    pub(crate) fn snapshot(&mut self, path: &Path) -> Option<Arc<Layer>> {
        let first = !self.snapshots.is_known(path);
        let loaded = self.snapshots.get(path);
        if let Some(problem) = loaded.problem {
            self.report_snapshot_problem(problem);
        }
        if first && self.watch_support && !self.watched.iter().any(|known| known == path) {
            self.watched.push(path.to_path_buf());
            let id = format!("sql-snapshot-{}", self.watched.len());
            let _ = self.register_watchers(&id, vec![path.display().to_string()]);
        }
        loaded.layer
    }

    /// What a document is read against: its target, its snapshot and the DDL of the workspace.
    pub(crate) fn schema_of(&mut self, uri: &Uri) -> Option<ReadAgainst> {
        let document = self.documents.get(uri)?;
        let target = document.state.target;
        let path = document.state.schema.clone();
        let snapshot = path.and_then(|path| self.snapshot(&path));
        let workspace = self.workspace.layer(target.dialect);
        Some(ReadAgainst {
            target,
            snapshot,
            workspace,
        })
    }

    fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            position_encoding: Some(lsc_server::encoding_kind(self.encoding)),
            text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
                ..TextDocumentSyncOptions::default()
            })),
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some([".", "@", "`", "\"", "["].map(String::from).to_vec()),
                ..CompletionOptions::default()
            }),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            document_highlight_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: Default::default(),
            })),
            signature_help_provider: Some(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                retrigger_characters: None,
                work_done_progress_options: Default::default(),
            }),
            document_symbol_provider: Some(OneOf::Left(true)),
            semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                SemanticTokensOptions {
                    work_done_progress_options: Default::default(),
                    legend: crate::insight::semantic_legend(),
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                },
            )),
            inlay_hint_provider: Some(OneOf::Left(true)),
            code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                code_action_kinds: Some(vec![
                    CodeActionKind::QUICKFIX,
                    CodeActionKind::REFACTOR_REWRITE,
                    CodeActionKind::new(crate::actions::FIX_ALL),
                ]),
                resolve_provider: None,
                work_done_progress_options: Default::default(),
            })),
            document_formatting_provider: Some(OneOf::Left(true)),
            document_range_formatting_provider: Some(OneOf::Left(true)),
            document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
                first_trigger_character: ";".to_string(),
                more_trigger_character: None,
            }),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
            diagnostic_provider: self.pull_diagnostics.then(|| {
                DiagnosticServerCapabilities::Options(DiagnosticOptions {
                    identifier: Some("sql".to_string()),
                    inter_file_dependencies: false,
                    workspace_diagnostics: false,
                    work_done_progress_options: Default::default(),
                })
            }),
            ..ServerCapabilities::default()
        }
    }

    fn report_problems(&mut self) {
        for problem in std::mem::take(&mut self.problems) {
            self.client
                .log(MessageType::WARNING, format!("sql-language-server: {problem}"));
        }
    }

    fn answer<P, R>(&mut self, id: RequestId, params: Value, handler: fn(&mut Self, P) -> Option<R>) -> Response
    where
        P: DeserializeOwned,
        R: serde::Serialize,
    {
        lsc_server::answer(id, params, |params| handler(self, params))
    }

    fn document_symbols(&mut self, params: DocumentSymbolParams) -> Option<DocumentSymbolResponse> {
        let uri = params.text_document.uri;
        let (hierarchical, encoding) = (self.hierarchical_symbols, self.encoding);
        let document = self.documents.get_mut(&uri)?;
        let symbols = document_symbols(&document.parse().syntax());
        let mapper = document.mapper(encoding);
        if hierarchical {
            return Some(DocumentSymbolResponse::Nested(convert::hierarchical_symbols(
                &mapper, &symbols,
            )));
        }
        let mut flat = Vec::new();
        convert::flat_symbols(&mapper, &uri, &symbols, None, &mut flat);
        Some(DocumentSymbolResponse::Flat(flat))
    }

    fn folding_ranges(&mut self, params: FoldingRangeParams) -> Option<Vec<lsp_types::FoldingRange>> {
        let document = self.documents.get_mut(&params.text_document.uri)?;
        let root = document.parse().syntax();
        let folds = folding_ranges(&root, &document.text, &document.index);
        Some(folds.iter().map(convert::folding_range).collect())
    }

    fn selection_ranges(&mut self, params: SelectionRangeParams) -> Option<Vec<lsp_types::SelectionRange>> {
        let encoding = self.encoding;
        let document = self.documents.get_mut(&params.text_document.uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        Some(
            params
                .positions
                .iter()
                .map(|position| convert::selection_chain(&mapper, &selection_ranges(&root, mapper.offset(*position))))
                .collect(),
        )
    }

    fn pull_diagnostics_for(&mut self, params: DocumentDiagnosticParams) -> Option<DocumentDiagnosticReportResult> {
        let items = self.diagnostics_of(&params.text_document.uri).unwrap_or_default();
        Some(DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport { result_id: None, items },
            },
        )))
    }

    fn diagnostics_of(&mut self, uri: &Uri) -> Option<Vec<lsp_types::Diagnostic>> {
        let encoding = self.encoding;
        let against = self.schema_of(uri)?;
        let target = against.target;
        let settings = self.settings_for(uri).inspections.clone();
        let document = self.documents.get_mut(uri)?;
        let parse = document.parse();
        let found = diagnostics(parse, target, against.schemas(), &settings);
        let mapper = document.mapper(encoding);
        Some(
            found
                .iter()
                .map(|found| convert::diagnostic(&mapper, uri, found))
                .collect(),
        )
    }

    fn completion(&mut self, params: CompletionParams) -> Option<CompletionResponse> {
        let uri = params.text_document_position.text_document.uri;
        let against = self.schema_of(&uri)?;
        let target = against.target;
        let encoding = self.encoding;
        let trigger = params
            .context
            .as_ref()
            .and_then(|context| context.trigger_character.as_deref())
            .and_then(|text| text.chars().next());
        let options = sql_analysis::completion::CompletionOptions {
            snippets: self.snippet_support,
            quote_identifiers: self.settings_for(&uri).completion.quote_identifiers.unwrap_or_default(),
            trigger,
            ..sql_analysis::completion::CompletionOptions::default()
        };
        let label_details = self.label_details_support;
        let document = self.documents.get_mut(&uri)?;
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(params.text_document_position.position);
        let schemas = against.schemas();
        let list = complete(&document.text, offset.into(), target, schemas, options);
        Some(CompletionResponse::List(convert::completion_list(
            &mapper,
            list,
            label_details,
        )))
    }

    fn hover(&mut self, params: HoverParams) -> Option<lsp_types::Hover> {
        let uri = params.text_document_position_params.text_document.uri;
        let against = self.schema_of(&uri)?;
        let target = against.target;
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(params.text_document_position_params.position);
        let schemas = against.schemas();
        let found = sql_analysis::nav::hover(&root, offset.into(), target, schemas)?;
        Some(convert::hover(&mapper, found))
    }

    fn signature_help(&mut self, params: SignatureHelpParams) -> Option<lsp_types::SignatureHelp> {
        let uri = params.text_document_position_params.text_document.uri;
        let against = self.schema_of(&uri)?;
        let target = against.target;
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(params.text_document_position_params.position);
        let schemas = against.schemas();
        let found = sql_analysis::signature::signature_help(&root, offset.into(), target, schemas)?;
        Some(convert::signature_help(found))
    }

    fn definition(&mut self, params: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
        let uri = params.text_document_position_params.text_document.uri;
        let against = self.schema_of(&uri)?;
        let target = against.target;
        let encoding = self.encoding;
        let document = self.documents.get_mut(&uri)?;
        let root = document.parse().syntax();
        let mapper = document.mapper(encoding);
        let offset = mapper.offset(params.text_document_position_params.position);
        let schemas = against.schemas();
        let places = sql_analysis::nav::definition(&root, offset.into(), target, schemas);
        let mut locations = Vec::new();
        let mut elsewhere = Vec::new();
        for place in places {
            match place.path {
                None => locations.push(Location::new(uri.clone(), mapper.range(place.name))),
                Some(path) => elsewhere.push((path, place.name)),
            }
        }
        for (path, name) in elsewhere {
            let Some(other) = path_to_uri(&path) else {
                continue;
            };
            let text = match self.documents.get(&other) {
                Some(open) => open.text.clone(),
                None => match std::fs::read(&path) {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(_) => continue,
                },
            };
            let index = lsc_server::LineIndex::new(&text);
            let (start, end) = index.range(&text, name, encoding);
            let range = lsp_types::Range::new(
                lsp_types::Position::new(start.line, start.col),
                lsp_types::Position::new(end.line, end.col),
            );
            locations.push(Location::new(other, range));
        }
        (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
    }

    /// A `.sql` file changed on disk: read its DDL again, unless it is open, whose text is newer.
    fn file_changed(&mut self, path: &Path, deleted: bool) {
        if !files::is_sql(path) {
            return;
        }
        let dialect = self
            .settings
            .resolve(Some(path), self.root.as_deref(), None)
            .target
            .dialect;
        let ddl = if deleted {
            None
        } else {
            files::read_file(path, dialect).map(|ddl| (dialect, ddl))
        };
        self.workspace.set(path.to_path_buf(), ddl);
    }

    fn watched_files_changed(&mut self, params: DidChangeWatchedFilesParams) -> Result<(), BoxError> {
        let mut changed = false;
        for change in params.changes {
            let Some(path) = uri_to_path(&change.uri) else {
                continue;
            };
            if self.snapshots.is_known(&path) {
                if let Some(problem) = self.snapshots.read(&path).problem {
                    self.report_snapshot_problem(problem);
                }
                changed = true;
            }
            if files::is_sql(&path) {
                self.file_changed(&path, change.typ == FileChangeType::DELETED);
                changed = true;
            }
        }
        if changed {
            self.everything_changed()?;
        }
        Ok(())
    }

    /// What every document is read against changed: their diagnostics are published again.
    fn everything_changed(&mut self) -> Result<(), BoxError> {
        for uri in self.documents.uris() {
            self.mark_dirty(uri);
        }
        self.refresh_views()
    }

    /// Reads a document at what the settings say for it. Gives whether that changed.
    fn retarget(&mut self, uri: &Uri) -> bool {
        let path = uri_to_path(uri);
        let root = self.root.clone();
        let Some(document) = self.documents.get_mut(uri) else {
            return false;
        };
        let settings = document.state.settings.as_ref().unwrap_or(&self.settings);
        let resolved = settings.resolve(path.as_deref(), root.as_deref(), document.state.language);
        document.retarget(resolved.target, resolved.schema)
    }

    fn notification(&mut self, notification: Notification) -> Result<(), BoxError> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: DidOpenTextDocumentParams = serde_json::from_value(notification.params)?;
                let item = params.text_document;
                let document = self.documents.open(item.uri.clone(), item.version, item.text);
                document.state.language = dialect_of_language(&item.language_id);
                self.retarget(&item.uri);
                self.request_configuration(&item.uri)?;
                self.mark_dirty(item.uri);
            }
            DidChangeTextDocument::METHOD => {
                let params: DidChangeTextDocumentParams = serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;
                if let Some(document) = self.documents.get_mut(&uri) {
                    document.apply_changes(params.text_document.version, &params.content_changes, self.encoding);
                    self.mark_dirty(uri);
                }
            }
            DidCloseTextDocument::METHOD => {
                let params: DidCloseTextDocumentParams = serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;
                self.documents.close(&uri);
                self.dirty.retain(|dirty| *dirty != uri);
                if !self.pull_diagnostics {
                    self.publish(uri, None, Vec::new())?;
                }
            }
            DidChangeConfiguration::METHOD => {
                let params: DidChangeConfigurationParams = serde_json::from_value(notification.params)?;
                self.configuration_changed(&params.settings)?;
            }
            DidChangeWatchedFiles::METHOD => {
                let params: DidChangeWatchedFilesParams = serde_json::from_value(notification.params)?;
                self.watched_files_changed(params)?;
            }
            DidSaveTextDocument::METHOD => {
                let params: DidSaveTextDocumentParams = serde_json::from_value(notification.params)?;
                if !self.watch_support {
                    if let Some(path) = uri_to_path(&params.text_document.uri) {
                        if files::is_sql(&path) {
                            self.file_changed(&path, false);
                            self.everything_changed()?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// New settings arrived. A client that answers `workspace/configuration` is asked again for every
    /// document, since its answer may differ per folder; what it pushed is the default for
    /// documents it gives no answer for.
    fn configuration_changed(&mut self, value: &Value) -> Result<(), BoxError> {
        let (pushed, problems) = Settings::from_value(value);
        self.problems = problems;
        self.report_problems();
        if !pushed.is_empty() || !self.configuration_support {
            self.settings = pushed;
        }
        for uri in self.documents.uris() {
            if self.retarget(&uri) {
                self.mark_dirty(uri.clone());
            }
            self.request_configuration(&uri)?;
        }
        self.refresh_views()
    }

    fn request_configuration(&mut self, uri: &Uri) -> Result<(), BoxError> {
        if !self.configuration_support {
            return Ok(());
        }
        let params = ConfigurationParams {
            items: vec![ConfigurationItem {
                scope_uri: Some(uri.clone()),
                section: Some(SECTION.to_string()),
            }],
        };
        let id = self.client.request::<WorkspaceConfiguration>(params)?;
        self.pending_configuration.insert(id, uri.clone());
        Ok(())
    }

    fn response(&mut self, response: Response) -> Result<(), BoxError> {
        let Some(uri) = self.pending_configuration.remove(&response.id) else {
            return Ok(());
        };
        let answer = response
            .response_result
            .ok()
            .and_then(|result| result.as_array().and_then(|items| items.first().cloned()))
            .filter(|item| !item.is_null());
        let settings = answer.map(|item| {
            let (settings, problems) = Settings::from_value(&item);
            self.problems.extend(problems);
            settings
        });
        self.report_problems();
        let Some(document) = self.documents.get_mut(&uri) else {
            return Ok(());
        };
        document.state.settings = settings.filter(|settings| !settings.is_empty());
        if self.retarget(&uri) {
            self.mark_dirty(uri);
            self.refresh_views()?;
        }
        Ok(())
    }

    /// What a client pulls (diagnostics, semantic tokens, inlay hints) is out of date: ask it to
    /// pull again, where it says it can be asked.
    fn refresh_views(&mut self) -> Result<(), BoxError> {
        if self.pull_diagnostics && self.diagnostic_refresh_support {
            self.client.request::<WorkspaceDiagnosticRefresh>(())?;
        }
        if self.semantic_tokens_refresh_support {
            self.client.request::<SemanticTokensRefresh>(())?;
        }
        if self.inlay_hint_refresh_support {
            self.client.request::<InlayHintRefreshRequest>(())?;
        }
        Ok(())
    }

    fn mark_dirty(&mut self, uri: Uri) {
        if !self.dirty.contains(&uri) {
            self.dirty.push(uri);
        }
    }

    fn publish_dirty(&mut self) -> Result<(), BoxError> {
        if self.pull_diagnostics {
            self.dirty.clear();
            return Ok(());
        }
        for uri in std::mem::take(&mut self.dirty) {
            let version = self.documents.get(&uri).map(|document| document.version);
            if let Some(items) = self.diagnostics_of(&uri) {
                self.publish(uri, version, items)?;
            }
        }
        Ok(())
    }

    fn publish(&self, uri: Uri, version: Option<i32>, diagnostics: Vec<lsp_types::Diagnostic>) -> Result<(), BoxError> {
        self.client
            .notify::<PublishDiagnostics>(PublishDiagnosticsParams::new(uri, diagnostics, version))
    }
}

impl Handler for Server {
    type Event = Event;

    fn request(&mut self, request: Request) -> Result<(), BoxError> {
        let id = request.id.clone();
        let response = match request.method.as_str() {
            DocumentSymbolRequest::METHOD => self.answer(id, request.params, Self::document_symbols),
            FoldingRangeRequest::METHOD => self.answer(id, request.params, Self::folding_ranges),
            SelectionRangeRequest::METHOD => self.answer(id, request.params, Self::selection_ranges),
            DocumentDiagnosticRequest::METHOD => self.answer(id, request.params, Self::pull_diagnostics_for),
            Completion::METHOD => self.answer(id, request.params, Self::completion),
            HoverRequest::METHOD => self.answer(id, request.params, Self::hover),
            SignatureHelpRequest::METHOD => self.answer(id, request.params, Self::signature_help),
            GotoDefinition::METHOD => self.answer(id, request.params, Self::definition),
            References::METHOD => self.answer(id, request.params, Self::references),
            SemanticTokensFullRequest::METHOD => self.answer(id, request.params, Self::semantic_tokens_full),
            SemanticTokensRangeRequest::METHOD => self.answer(id, request.params, Self::semantic_tokens_range),
            InlayHintRequest::METHOD => self.answer(id, request.params, Self::inlay_hints),
            Formatting::METHOD => self.answer(id, request.params, Self::formatting),
            CodeActionRequest::METHOD => self.answer(id, request.params, Self::code_actions),
            RangeFormatting::METHOD => self.answer(id, request.params, Self::range_formatting),
            OnTypeFormatting::METHOD => self.answer(id, request.params, Self::on_type_formatting),
            DocumentHighlightRequest::METHOD => self.answer(id, request.params, Self::document_highlight),
            PrepareRenameRequest::METHOD => {
                lsc_server::answer_checked(id, request.params, |params| self.prepare_rename(params))
            }
            Rename::METHOD => lsc_server::answer_checked(id, request.params, |params| self.rename(params)),
            method => lsc_server::unsupported(id, method),
        };
        self.client.send(response)
    }

    fn notification(&mut self, notification: Notification) -> Result<(), BoxError> {
        Server::notification(self, notification)
    }

    fn response(&mut self, response: Response) -> Result<(), BoxError> {
        Server::response(self, response)
    }

    fn event(&mut self, event: Event) -> Result<(), BoxError> {
        match event {
            Event::Scanned(found) => {
                for (path, dialect, ddl) in found {
                    self.workspace.set(path, Some((dialect, ddl)));
                }
                self.everything_changed()?;
                self.publish_dirty()
            }
        }
    }

    /// Typing sends a change per keystroke: they are all answered before time goes to diagnostics.
    /// A client that does not watch files has the snapshots checked for a change here instead.
    fn idle(&mut self) -> Result<(), BoxError> {
        if !self.watch_support {
            let (changed, problems) = self.snapshots.refresh_changed();
            for problem in problems {
                self.report_snapshot_problem(problem);
            }
            if !changed.is_empty() {
                self.everything_changed()?;
            }
        }
        self.publish_dirty()
    }
}
