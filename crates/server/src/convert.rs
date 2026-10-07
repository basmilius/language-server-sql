//! From what `sql-analysis` answers to the shapes of LSP.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionList, CompletionTextEdit, Diagnostic,
    DiagnosticSeverity, DiagnosticTag, DocumentSymbol, Documentation, FoldingRange, FoldingRangeKind, Hover,
    HoverContents, InsertTextFormat, Location, MarkupContent, MarkupKind, NumberOrString, ParameterInformation,
    ParameterLabel, Range, SelectionRange, SignatureHelp, SignatureInformation, SymbolInformation, SymbolKind,
    TextEdit, Uri,
};
use sql_analysis::{Fold, FoldKind, Symbol};
use sql_syntax::TextRange;

pub use lsc_server::Mapper;

pub fn diagnostic(mapper: &Mapper, found: &sql_analysis::Diagnostic) -> Diagnostic {
    Diagnostic {
        range: mapper.visible_range(found.range),
        severity: Some(match found.severity {
            sql_analysis::DiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
            sql_analysis::DiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
            sql_analysis::DiagnosticSeverity::Information => DiagnosticSeverity::INFORMATION,
            sql_analysis::DiagnosticSeverity::Hint => DiagnosticSeverity::HINT,
        }),
        code: Some(NumberOrString::String(found.code.to_string())),
        source: Some("sql".to_string()),
        message: found.message.clone(),
        tags: found.deprecated.then(|| vec![DiagnosticTag::DEPRECATED]),
        ..Diagnostic::default()
    }
}

fn symbol_kind(kind: sql_analysis::SymbolKind) -> SymbolKind {
    use sql_analysis::SymbolKind as Kind;
    match kind {
        Kind::Table => SymbolKind::STRUCT,
        Kind::View => SymbolKind::INTERFACE,
        Kind::Column => SymbolKind::FIELD,
        Kind::Constraint => SymbolKind::PROPERTY,
        Kind::Index => SymbolKind::KEY,
        Kind::Sequence => SymbolKind::NUMBER,
        Kind::Type => SymbolKind::ENUM,
        Kind::EnumValue => SymbolKind::ENUM_MEMBER,
        Kind::Domain => SymbolKind::TYPE_PARAMETER,
        Kind::Function => SymbolKind::FUNCTION,
        Kind::Procedure => SymbolKind::METHOD,
        Kind::Trigger => SymbolKind::EVENT,
        Kind::Schema => SymbolKind::NAMESPACE,
        Kind::Extension => SymbolKind::PACKAGE,
        Kind::CommonTableExpression => SymbolKind::VARIABLE,
        Kind::Statement => SymbolKind::OBJECT,
    }
}

#[allow(deprecated)]
pub fn hierarchical_symbols(mapper: &Mapper, symbols: &[Symbol]) -> Vec<DocumentSymbol> {
    symbols
        .iter()
        .map(|symbol| DocumentSymbol {
            name: symbol.name.clone(),
            detail: symbol.detail.clone(),
            kind: symbol_kind(symbol.kind),
            tags: None,
            deprecated: None,
            range: mapper.range(symbol.range),
            selection_range: mapper.range(symbol.selection_range),
            children: (!symbol.children.is_empty()).then(|| hierarchical_symbols(mapper, &symbol.children)),
        })
        .collect()
}

#[allow(deprecated)]
pub fn flat_symbols(
    mapper: &Mapper,
    uri: &Uri,
    symbols: &[Symbol],
    container: Option<&str>,
    out: &mut Vec<SymbolInformation>,
) {
    for symbol in symbols {
        out.push(SymbolInformation {
            name: symbol.name.clone(),
            kind: symbol_kind(symbol.kind),
            tags: None,
            deprecated: None,
            location: Location::new(uri.clone(), mapper.range(symbol.range)),
            container_name: container.map(str::to_string),
        });
        flat_symbols(mapper, uri, &symbol.children, Some(&symbol.name), out);
    }
}

pub fn folding_range(fold: &Fold) -> FoldingRange {
    FoldingRange {
        start_line: fold.start_line,
        start_character: None,
        end_line: fold.end_line,
        end_character: None,
        kind: fold.kind.map(|kind| match kind {
            FoldKind::Comment => FoldingRangeKind::Comment,
            FoldKind::Region => FoldingRangeKind::Region,
        }),
        collapsed_text: None,
    }
}

/// A chain from the smallest range to the largest, as LSP nests it: each one's parent is the next.
pub fn selection_chain(mapper: &Mapper, ranges: &[TextRange]) -> SelectionRange {
    let mut parent: Option<Box<SelectionRange>> = None;
    for range in ranges.iter().rev() {
        parent = Some(Box::new(SelectionRange {
            range: mapper.range(*range),
            parent,
        }));
    }
    match parent {
        Some(chain) => *chain,
        None => SelectionRange {
            range: Range::default(),
            parent: None,
        },
    }
}

fn completion_kind(kind: sql_analysis::completion::ItemKind) -> CompletionItemKind {
    use sql_analysis::completion::ItemKind as Kind;
    match kind {
        Kind::Keyword => CompletionItemKind::KEYWORD,
        Kind::Table => CompletionItemKind::STRUCT,
        Kind::View => CompletionItemKind::INTERFACE,
        Kind::Column => CompletionItemKind::FIELD,
        Kind::Alias => CompletionItemKind::VARIABLE,
        Kind::Schema => CompletionItemKind::MODULE,
        Kind::Function => CompletionItemKind::FUNCTION,
        Kind::Procedure => CompletionItemKind::METHOD,
        Kind::Type => CompletionItemKind::TYPE_PARAMETER,
        Kind::Sequence => CompletionItemKind::VALUE,
        Kind::Value => CompletionItemKind::ENUM_MEMBER,
        Kind::Snippet => CompletionItemKind::SNIPPET,
        Kind::Setting => CompletionItemKind::PROPERTY,
    }
}

/// A completion list as LSP has it. A client without label details gets the description in
/// `detail` after the type.
pub fn completion_list(
    mapper: &Mapper,
    list: sql_analysis::completion::CompletionList,
    label_details: bool,
) -> CompletionList {
    let items = list
        .items
        .into_iter()
        .map(|item| {
            let range = mapper.range(TextRange::new(item.edit.start.into(), item.edit.end.into()));
            let (detail, label_details) = if label_details {
                (
                    item.detail.clone(),
                    Some(CompletionItemLabelDetails {
                        detail: None,
                        description: item.description.clone().or_else(|| item.detail.clone()),
                    }),
                )
            } else {
                let detail = match (&item.detail, &item.description) {
                    (Some(detail), Some(description)) => Some(format!("{detail} ({description})")),
                    (Some(detail), None) => Some(detail.clone()),
                    (None, description) => description.clone(),
                };
                (detail, None)
            };
            CompletionItem {
                label: item.label,
                label_details,
                kind: Some(completion_kind(item.kind)),
                detail,
                documentation: item.documentation.map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
                sort_text: Some(item.sort_text),
                filter_text: item.filter_text,
                insert_text_format: Some(if item.snippet {
                    InsertTextFormat::SNIPPET
                } else {
                    InsertTextFormat::PLAIN_TEXT
                }),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(range, item.edit.new_text))),
                ..CompletionItem::default()
            }
        })
        .collect();
    CompletionList {
        is_incomplete: list.incomplete,
        items,
    }
}

pub fn hover(mapper: &Mapper, hover: sql_analysis::nav::Hover) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: hover.markdown,
        }),
        range: Some(mapper.range(hover.range)),
    }
}

pub fn signature_help(help: sql_analysis::signature::SignatureHelp) -> SignatureHelp {
    let active_parameter = help
        .signatures
        .get(help.active_signature)
        .and_then(|signature| signature.active_parameter)
        .map(|active| active as u32);
    SignatureHelp {
        signatures: help
            .signatures
            .into_iter()
            .map(|signature| SignatureInformation {
                label: signature.label,
                documentation: signature.documentation.map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
                parameters: Some(
                    signature
                        .parameters
                        .into_iter()
                        .map(|parameter| ParameterInformation {
                            label: ParameterLabel::Simple(parameter.label),
                            documentation: None,
                        })
                        .collect(),
                ),
                active_parameter: signature.active_parameter.map(|active| active as u32),
            })
            .collect(),
        active_signature: Some(help.active_signature as u32),
        active_parameter,
    }
}
