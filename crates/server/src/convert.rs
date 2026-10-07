//! From what `sql-analysis` answers to the shapes of LSP.

use lsp_types::{
    Diagnostic, DiagnosticSeverity, DiagnosticTag, DocumentSymbol, FoldingRange, FoldingRangeKind, Location,
    NumberOrString, Range, SelectionRange, SymbolInformation, SymbolKind, Uri,
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
