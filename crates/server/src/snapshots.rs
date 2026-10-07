//! Schema snapshot files: read when a document first needs one, read again when the file changes,
//! and a file that cannot be read is told to the client once, until it reads again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use sql_analysis::catalog::{Layer, Origin};
use sql_catalog::load_snapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

struct Entry {
    stamp: Option<Stamp>,
    layer: Option<Arc<Layer>>,
    /// The problem last told to the client, so it is told once.
    reported: Option<String>,
}

#[derive(Default)]
pub struct Snapshots {
    entries: HashMap<PathBuf, Entry>,
}

/// What reading a snapshot came to.
pub struct Loaded {
    pub layer: Option<Arc<Layer>>,
    /// A problem the client has not been told yet.
    pub problem: Option<String>,
}

impl Snapshots {
    /// The snapshot at a path, read the first time it is asked for.
    pub fn get(&mut self, path: &Path) -> Loaded {
        if let Some(entry) = self.entries.get(path) {
            return Loaded {
                layer: entry.layer.clone(),
                problem: None,
            };
        }
        self.read(path)
    }

    /// Reads a snapshot again.
    pub fn read(&mut self, path: &Path) -> Loaded {
        let stamp = stamp(path);
        let previous = self.entries.remove(path).and_then(|entry| entry.reported);
        let (layer, problem) = match load_snapshot(path) {
            Ok(snapshot) => (Some(Arc::new(Layer::new(Origin::Snapshot, snapshot))), None),
            Err(error) => (None, Some(error.message)),
        };
        let new_problem = problem.clone().filter(|problem| previous.as_ref() != Some(problem));
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                stamp,
                layer: layer.clone(),
                reported: problem,
            },
        );
        Loaded {
            layer,
            problem: new_problem,
        }
    }

    pub fn is_known(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    /// Reads again every snapshot whose file changed since it was read, for a client that does not
    /// watch files. Gives the paths read again and the problems to tell.
    pub fn refresh_changed(&mut self) -> (Vec<PathBuf>, Vec<String>) {
        let changed: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|(path, entry)| stamp(path) != entry.stamp)
            .map(|(path, _)| path.clone())
            .collect();
        let mut problems = Vec::new();
        for path in &changed {
            problems.extend(self.read(path).problem);
        }
        (changed, problems)
    }
}
