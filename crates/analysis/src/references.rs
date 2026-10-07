//! Find references and document highlights: the symbol under a position and every place the
//! document, and for an object of the schema the workspace's other `.sql` files, name it.

use std::path::{Path, PathBuf};

use sql_syntax::{SyntaxNode, Target, TextRange, parse};

use crate::context::Schemas;
use crate::refs::{Access, Hit, Symbol, find_hits, symbol_at_offset};

/// The document a question is asked in.
#[derive(Clone, Copy)]
pub struct Current<'a> {
    pub root: &'a SyntaxNode,
    pub target: Target,
    pub schemas: Schemas<'a>,
}

/// Another `.sql` file of the workspace, read at its own target and against its own schema.
#[derive(Clone, Copy)]
pub struct OtherFile<'a> {
    pub path: &'a Path,
    pub text: &'a str,
    pub target: Target,
    pub schemas: Schemas<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHits {
    /// The file; `None` is the document asked about.
    pub path: Option<PathBuf>,
    pub hits: Vec<Hit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct References {
    /// The name under the position.
    pub range: TextRange,
    pub symbol: Symbol,
    pub files: Vec<FileHits>,
}

/// Whether another file may name a symbol at all: its text has the name in it, in any case.
pub fn may_mention(text: &str, symbol: &Symbol) -> bool {
    let name = symbol.name();
    !name.is_empty() && text.to_lowercase().contains(&name.to_lowercase())
}

/// Every place that names what the name at an offset stands for: in the document, and for an
/// object of the schema in the other files.
pub fn references(
    current: &Current,
    offset: u32,
    include_declaration: bool,
    others: &[OtherFile],
) -> Option<References> {
    let (range, symbol, _) = symbol_at_offset(current.root, offset, current.target, current.schemas)?;
    let keep = |hits: Vec<(usize, Hit)>| -> Vec<Hit> {
        hits.into_iter()
            .map(|(_, hit)| hit)
            .filter(|hit| include_declaration || hit.access != Access::Declaration)
            .collect()
    };
    let wanted = std::slice::from_ref(&symbol);
    let mut files = vec![FileHits {
        path: None,
        hits: keep(find_hits(current.root, current.target, current.schemas, wanted)),
    }];
    if !symbol.is_local() {
        for other in others.iter().filter(|other| may_mention(other.text, &symbol)) {
            let root = parse(other.text, other.target.dialect).syntax();
            let hits = keep(find_hits(&root, other.target, other.schemas, wanted));
            if !hits.is_empty() {
                files.push(FileHits {
                    path: Some(other.path.to_path_buf()),
                    hits,
                });
            }
        }
    }
    Some(References { range, symbol, files })
}

/// The places in the document that name what the name at an offset stands for, with how each
/// uses it.
pub fn highlights(root: &SyntaxNode, offset: u32, target: Target, schemas: Schemas) -> Vec<Hit> {
    let Some((_, symbol, _)) = symbol_at_offset(root, offset, target, schemas) else {
        return Vec::new();
    };
    find_hits(root, target, schemas, std::slice::from_ref(&symbol))
        .into_iter()
        .map(|(_, hit)| hit)
        .collect()
}
