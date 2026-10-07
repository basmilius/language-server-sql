//! MySQL's and MariaDB's `sql_mode`, which decides some of what a server rejects: a column missing
//! from `GROUP BY`, a value a strict server refuses to insert, `||` as concatenation, double quotes
//! around names. A script's `SET sql_mode` wins, then the snapshot's `sqlMode`, then the server's
//! default.

use sql_syntax::Dialect;

use crate::catalog::{Catalog, Origin, ScriptState};

/// The modes in effect, or `Unknown` where a script set them to something only the server knows,
/// which keeps every inspection that depends on them silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlMode {
    Known(Vec<String>),
    Unknown,
}

/// What a combination mode stands for, as MySQL 8 and MariaDB 11 document them.
fn expand(dialect: Dialect, mode: &str) -> &'static [&'static str] {
    match (mode, dialect) {
        ("ANSI", Dialect::Mariadb) => &["REAL_AS_FLOAT", "PIPES_AS_CONCAT", "ANSI_QUOTES", "IGNORE_SPACE"],
        ("ANSI", _) => &[
            "REAL_AS_FLOAT",
            "PIPES_AS_CONCAT",
            "ANSI_QUOTES",
            "IGNORE_SPACE",
            "ONLY_FULL_GROUP_BY",
        ],
        ("TRADITIONAL", _) => &[
            "STRICT_TRANS_TABLES",
            "STRICT_ALL_TABLES",
            "NO_ZERO_IN_DATE",
            "NO_ZERO_DATE",
            "ERROR_FOR_DIVISION_BY_ZERO",
            "NO_ENGINE_SUBSTITUTION",
        ],
        ("ORACLE" | "POSTGRESQL" | "DB2" | "MSSQL" | "MAXDB", Dialect::Mariadb) => {
            &["PIPES_AS_CONCAT", "ANSI_QUOTES", "IGNORE_SPACE"]
        }
        _ => &[],
    }
}

impl SqlMode {
    /// The modes a comma-separated list names, with combination modes expanded.
    pub fn parse(text: &str, dialect: Dialect) -> SqlMode {
        let mut flags = Vec::new();
        for mode in text.split(',').map(|mode| mode.trim().to_ascii_uppercase()) {
            if mode.is_empty() {
                continue;
            }
            flags.extend(expand(dialect, &mode).iter().map(|flag| flag.to_string()));
            flags.push(mode);
        }
        SqlMode::Known(flags)
    }

    /// What a server starts with: MySQL 8.0 and 8.4, and MariaDB since 10.2.4.
    pub fn default_of(dialect: Dialect) -> SqlMode {
        match dialect {
            Dialect::Mysql => SqlMode::parse(
                "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION",
                dialect,
            ),
            Dialect::Mariadb => SqlMode::parse(
                "STRICT_TRANS_TABLES,ERROR_FOR_DIVISION_BY_ZERO,NO_AUTO_CREATE_USER,NO_ENGINE_SUBSTITUTION",
                dialect,
            ),
            _ => SqlMode::Known(Vec::new()),
        }
    }

    /// The modes a statement runs under.
    pub fn of(catalog: &Catalog, state: &ScriptState) -> SqlMode {
        let dialect = catalog.dialect();
        if let Some(mode) = &state.sql_mode {
            return mode.clone();
        }
        let snapshot = catalog
            .layers
            .iter()
            .filter(|layer| layer.origin == Origin::Snapshot)
            .find_map(|layer| layer.snapshot.source.sql_mode.as_deref());
        match snapshot {
            Some(text) => SqlMode::parse(text, dialect),
            None => SqlMode::default_of(dialect),
        }
    }

    /// Whether a mode is on; `None` when nobody can tell.
    pub fn has(&self, flag: &str) -> Option<bool> {
        match self {
            SqlMode::Known(flags) => Some(flags.iter().any(|known| known == flag)),
            SqlMode::Unknown => None,
        }
    }

    /// Whether a strict mode makes a server refuse a value it cannot store, rather than warn.
    pub fn strict(&self) -> Option<bool> {
        Some(self.has("STRICT_TRANS_TABLES")? || self.has("STRICT_ALL_TABLES")?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_combinations_and_knows_the_defaults() {
        let ansi = SqlMode::parse("ansi", Dialect::Mysql);
        assert_eq!(ansi.has("PIPES_AS_CONCAT"), Some(true));
        assert_eq!(ansi.has("ONLY_FULL_GROUP_BY"), Some(true));
        assert_eq!(
            SqlMode::parse("ANSI", Dialect::Mariadb).has("ONLY_FULL_GROUP_BY"),
            Some(false)
        );
        assert_eq!(
            SqlMode::default_of(Dialect::Mysql).has("ONLY_FULL_GROUP_BY"),
            Some(true)
        );
        assert_eq!(
            SqlMode::default_of(Dialect::Mariadb).has("ONLY_FULL_GROUP_BY"),
            Some(false)
        );
        assert_eq!(SqlMode::default_of(Dialect::Mariadb).strict(), Some(true));
        assert_eq!(SqlMode::parse("", Dialect::Mysql).strict(), Some(false));
        assert_eq!(SqlMode::Unknown.strict(), None);
    }
}
