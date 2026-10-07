//! The `.sql` files of the workspace, whose DDL defines objects a document can name: read once in
//! the background when the server starts, again when a watched file changes or a document is
//! saved, and replayed per dialect into a layer when a document asks.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sql_analysis::catalog::Layer;
use sql_analysis::workspace::{FileDdl, build, extract};
use sql_embed::{read_sql_file, sql_files};
use sql_syntax::Dialect;

pub use sql_embed::is_sql_file as is_sql;

/// The DDL of a file on disk, empty for a file of queries, or nothing when it cannot be read.
pub fn read_file(path: &Path, dialect: Dialect) -> Option<FileDdl> {
    read_sql_file(path).map(|text| extract(&text, dialect))
}

/// Every `.sql` file under the folders, with its DDL.
pub fn scan(roots: &[PathBuf], dialect_of: impl Fn(&Path) -> Dialect) -> Vec<(PathBuf, Dialect, FileDdl)> {
    sql_files(roots)
        .into_iter()
        .filter_map(|path| {
            let dialect = dialect_of(&path);
            let ddl = read_file(&path, dialect)?;
            Some((path, dialect, ddl))
        })
        .collect()
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
