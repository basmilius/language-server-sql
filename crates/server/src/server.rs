use std::collections::HashMap;
use std::path::PathBuf;

use lsc_server::paths::uri_to_path;
use lsc_server::{BoxError, Client, Handler, PositionEncoding};
use lsp_server::{Connection, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeConfiguration, DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    DocumentDiagnosticRequest, DocumentSymbolRequest, FoldingRangeRequest, Request as _, SelectionRangeRequest,
    WorkspaceConfiguration, WorkspaceDiagnosticRefresh,
};
use lsp_types::{
    ConfigurationItem, ConfigurationParams, DiagnosticOptions, DiagnosticServerCapabilities,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult, DocumentSymbolParams,
    DocumentSymbolResponse, FoldingRangeParams, FoldingRangeProviderCapability, FullDocumentDiagnosticReport,
    InitializeParams, InitializeResult, MessageType, OneOf, PublishDiagnosticsParams,
    RelatedFullDocumentDiagnosticReport, SelectionRangeParams, SelectionRangeProviderCapability, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sql_analysis::{diagnostics, document_symbols, folding_ranges, selection_ranges};

use crate::config::{SECTION, Settings, dialect_of_language};
use crate::convert;
use crate::documents::{Documents, ParseDocument};

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
    let (_sender, events) = crossbeam_channel::unbounded::<()>();
    lsc_server::main_loop(&connection, &events, &mut server)
}

struct Server {
    client: Client,
    documents: Documents,
    encoding: PositionEncoding,
    /// What `initializationOptions` and `didChangeConfiguration` said, for every document the client
    /// gives no answer of its own for.
    settings: Settings,
    /// What could not be read in the settings, told to the client once it is ready.
    problems: Vec<String>,
    /// The first workspace folder, which relative paths in the settings start from.
    root: Option<PathBuf>,
    /// The client pulls diagnostics, so the server does not push them.
    pull_diagnostics: bool,
    hierarchical_symbols: bool,
    configuration_support: bool,
    diagnostic_refresh_support: bool,
    /// Documents whose diagnostics are out of date, published once the queue of messages is empty.
    dirty: Vec<Uri>,
    /// Questions asked of the client: which document each `workspace/configuration` answer is for.
    pending_configuration: HashMap<RequestId, Uri>,
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
        }
    }

    fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            position_encoding: Some(lsc_server::encoding_kind(self.encoding)),
            text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                ..TextDocumentSyncOptions::default()
            })),
            document_symbol_provider: Some(OneOf::Left(true)),
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
        let document = self.documents.get_mut(uri)?;
        let target = document.state.target;
        let found = diagnostics(document.parse(), target);
        let mapper = document.mapper(encoding);
        Some(found.iter().map(|found| convert::diagnostic(&mapper, found)).collect())
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
        self.refresh_pulled_diagnostics()
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
            self.refresh_pulled_diagnostics()?;
        }
        Ok(())
    }

    fn refresh_pulled_diagnostics(&mut self) -> Result<(), BoxError> {
        if self.pull_diagnostics && self.diagnostic_refresh_support {
            self.client.request::<WorkspaceDiagnosticRefresh>(())?;
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
    type Event = ();

    fn request(&mut self, request: Request) -> Result<(), BoxError> {
        let id = request.id.clone();
        let response = match request.method.as_str() {
            DocumentSymbolRequest::METHOD => self.answer(id, request.params, Self::document_symbols),
            FoldingRangeRequest::METHOD => self.answer(id, request.params, Self::folding_ranges),
            SelectionRangeRequest::METHOD => self.answer(id, request.params, Self::selection_ranges),
            DocumentDiagnosticRequest::METHOD => self.answer(id, request.params, Self::pull_diagnostics_for),
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

    fn event(&mut self, _event: ()) -> Result<(), BoxError> {
        Ok(())
    }

    /// Typing sends a change per keystroke: they are all answered before time goes to diagnostics.
    fn idle(&mut self) -> Result<(), BoxError> {
        self.publish_dirty()
    }
}
