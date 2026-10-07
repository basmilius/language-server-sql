//! The `.sql` files of the workspace, whose DDL defines objects a document can name: read once in
//! the background when the server starts, again when a watched file changes or a document is
//! saved, and replayed per dialect into a layer when a document asks.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sql_analysis::catalog::Layer;
use sql_analysis::workspace::{FileDdl, build, extract};
use sql_syntax::Dialect;

/// Folders a walk does not go into: dependencies, build output and version control.
const SKIPPED: [&str; 5] = ["node_modules", "vendor", "target", "dist", "build"];

/// A file larger than this is a data dump rather than a schema.
const MOST_BYTES: u64 = 16 * 1024 * 1024;

/// How many files a walk reads at most.
const MOST_FILES: usize = 10_000;

pub fn is_sql(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sql"))
}

/// The DDL of a file on disk, empty for a file of queries, or nothing when it cannot be read.
pub fn read_file(path: &Path, dialect: Dialect) -> Option<FileDdl> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MOST_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(extract(&text, dialect))
}

/// Every `.sql` file under the folders, with its DDL.
pub fn scan(roots: &[PathBuf], dialect_of: impl Fn(&Path) -> Dialect) -> Vec<(PathBuf, Dialect, FileDdl)> {
    let mut found = Vec::new();
    let mut pending: Vec<PathBuf> = roots.to_vec();
    let mut seen = 0usize;
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIPPED.contains(&name.as_str()) {
                    pending.push(path);
                }
            } else if kind.is_file() && is_sql(&path) {
                seen += 1;
                if seen > MOST_FILES {
                    return found;
                }
                let dialect = dialect_of(&path);
                if let Some(ddl) = read_file(&path, dialect) {
                    found.push((path, dialect, ddl));
                }
            }
        }
    }
    found
}

#[derive(Default)]
pub struct WorkspaceFiles {
    files: BTreeMap<PathBuf, (Dialect, FileDdl)>,
    layers: HashMap<Dialect, Option<Arc<Layer>>>,
}

impl WorkspaceFiles {
    /// Puts in or takes out the DDL of a file.
    pub fn set(&mut self, path: PathBuf, ddl: Option<(Dialect, FileDdl)>) {
        match ddl {
            Some(ddl) => {
                self.files.insert(path, ddl);
            }
            None => {
                self.files.remove(&path);
            }
        }
        self.layers.clear();
    }

    /// Every `.sql` file of the workspace, with the dialect it is read in.
    pub fn paths(&self) -> impl Iterator<Item = (&Path, Dialect)> {
        self.files.iter().map(|(path, (dialect, _))| (path.as_path(), *dialect))
    }

    /// The objects the files define for a document of a dialect: files of that dialect and files
    /// without one; a document without a dialect sees every file.
    pub fn layer(&mut self, dialect: Dialect) -> Option<Arc<Layer>> {
        if let Some(layer) = self.layers.get(&dialect) {
            return layer.clone();
        }
        let files = self
            .files
            .iter()
            .filter(|(_, (own, _))| dialect == Dialect::Generic || *own == dialect || *own == Dialect::Generic)
            .map(|(path, (_, ddl))| (path.as_path(), ddl));
        let layer = build(dialect, files);
        let layer = (!layer.is_empty()).then(|| Arc::new(layer));
        self.layers.insert(dialect, layer.clone());
        layer
    }
}
