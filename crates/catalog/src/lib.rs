//! What the SQL language server knows about schemas: the model of a schema, snapshot files that
//! hold one, and the built-in catalogs of each dialect (functions, types, system schemas and
//! settings), version by version.

mod builtin;
pub mod model;
mod snapshot;

pub use builtin::{BuiltinType, Builtins, Function, FunctionKind, Overload, Param, Setting, Versions, builtins};
pub use model::FORMAT_VERSION;
pub use snapshot::{SnapshotError, load_snapshot, read_snapshot};
