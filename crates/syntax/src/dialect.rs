//! Which database a text is written for: the dialect, which decides how the text is lexed, and the
//! version, which together with the dialect decides what the feature table reports.

use std::fmt;

use crate::lexer::LexOptions;

/// A database whose SQL the server reads, or `Generic` when nothing says which.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dialect {
    /// No dialect configured: lexed so that the dumps of every dialect read well, and only syntax
    /// that no dialect accepts is reported.
    Generic,
    Sqlite,
    Mysql,
    Mariadb,
    Postgres,
}

impl Dialect {
    /// The dialects of a database, without `Generic`.
    pub const DATABASES: [Dialect; 4] = [Dialect::Sqlite, Dialect::Mysql, Dialect::Mariadb, Dialect::Postgres];

    /// Reads the name a setting or a language id gives a dialect, in any case.
    pub fn parse(text: &str) -> Option<Dialect> {
        match text.trim().to_ascii_lowercase().as_str() {
            "generic" | "sql" | "standard" => Some(Dialect::Generic),
            "sqlite" | "sqlite3" => Some(Dialect::Sqlite),
            "mysql" => Some(Dialect::Mysql),
            "mariadb" => Some(Dialect::Mariadb),
            "postgres" | "postgresql" | "pgsql" | "pg" | "psql" => Some(Dialect::Postgres),
            _ => None,
        }
    }

    /// The identifier of the dialect in settings and diagnostics.
    pub fn id(self) -> &'static str {
        match self {
            Dialect::Generic => "generic",
            Dialect::Sqlite => "sqlite",
            Dialect::Mysql => "mysql",
            Dialect::Mariadb => "mariadb",
            Dialect::Postgres => "postgres",
        }
    }

    /// The name of the database, as a message writes it.
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Generic => "SQL",
            Dialect::Sqlite => "SQLite",
            Dialect::Mysql => "MySQL",
            Dialect::Mariadb => "MariaDB",
            Dialect::Postgres => "PostgreSQL",
        }
    }

    /// The oldest version the server supports; syntax older than it counts as always there.
    pub fn minimum(self) -> Version {
        match self {
            Dialect::Generic => Version::new(0, 0, 0),
            Dialect::Sqlite => Version::new(3, 47, 0),
            Dialect::Mysql => Version::new(8, 0, 0),
            Dialect::Mariadb => Version::new(11, 0, 0),
            Dialect::Postgres => Version::new(18, 0, 0),
        }
    }

    /// How a text of this dialect is cut into tokens.
    pub fn lex_options(self) -> LexOptions {
        let mysql = LexOptions {
            hash_comments: true,
            backslash_escapes: true,
            guess_backslash_quotes: false,
            double_quoted_strings: true,
            bracket_identifiers: false,
            nested_comments: false,
            dollar_quotes: false,
            postgres_operators: false,
            dash_comment_needs_space: true,
            mysql_variables: true,
            prefixed_parameters: false,
            question_placeholders: true,
            dollar_identifiers: true,
            digit_identifiers: true,
            digit_separators: false,
            delimiter_command: true,
            backslash_commands: true,
            sqlite_dot_commands: false,
        };
        match self {
            Dialect::Mysql | Dialect::Mariadb => mysql,
            Dialect::Sqlite => LexOptions {
                hash_comments: false,
                backslash_escapes: false,
                double_quoted_strings: false,
                bracket_identifiers: true,
                dash_comment_needs_space: false,
                mysql_variables: false,
                prefixed_parameters: true,
                dollar_identifiers: false,
                digit_identifiers: false,
                digit_separators: true,
                delimiter_command: false,
                backslash_commands: false,
                sqlite_dot_commands: true,
                ..mysql
            },
            Dialect::Postgres => LexOptions {
                hash_comments: false,
                backslash_escapes: false,
                double_quoted_strings: false,
                nested_comments: true,
                dollar_quotes: true,
                postgres_operators: true,
                dash_comment_needs_space: false,
                mysql_variables: false,
                question_placeholders: false,
                dollar_identifiers: false,
                digit_identifiers: false,
                digit_separators: true,
                delimiter_command: false,
                ..mysql
            },
            // What a dump of any dialect needs: backslash escapes for MySQL's strings (with a guess at
            // a backslash before a closing quote), dollar quotes,
            // nested comments and the operators of PostgreSQL, variables, DELIMITER and the backslash
            // commands of the clients. A `#` is an operator and a double quote an identifier, as in the standard.
            Dialect::Generic => LexOptions {
                hash_comments: false,
                guess_backslash_quotes: true,
                double_quoted_strings: false,
                nested_comments: true,
                dollar_quotes: true,
                postgres_operators: true,
                dash_comment_needs_space: false,
                dollar_identifiers: false,
                digit_identifiers: false,
                digit_separators: true,
                ..mysql
            },
        }
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A version of a database: major, minor and patch, as far as syntax cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Version {
        Version { major, minor, patch }
    }

    /// Reads `8.0.36`, `8.4`, `18`, `3.47.2` or what a server reports, such as `11.4.2-MariaDB`.
    /// Missing parts are zero.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let mut parts = text.split('.');
        let number = |part: Option<&str>| -> Option<u16> {
            let digits: String = part?.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        };
        let major = number(parts.next())?;
        let minor = number(parts.next()).unwrap_or(0);
        let patch = number(parts.next()).unwrap_or(0);
        Some(Version { major, minor, patch })
    }

    /// The version right before this one, which is what a test needs to see a feature absent.
    pub fn previous(self) -> Version {
        match (self.major, self.minor, self.patch) {
            (major, minor, patch) if patch > 0 => Version::new(major, minor, patch - 1),
            (major, minor, _) if minor > 0 => Version::new(major, minor - 1, u16::MAX),
            (major, _, _) => Version::new(major.saturating_sub(1), u16::MAX, u16::MAX),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.patch == 0 {
            write!(f, "{}.{}", self.major, self.minor)
        } else {
            write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
        }
    }
}

/// What a text is read as: a dialect and the version of it, or the newest when none is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub dialect: Dialect,
    pub version: Option<Version>,
}

impl Target {
    pub const GENERIC: Target = Target {
        dialect: Dialect::Generic,
        version: None,
    };

    pub const fn new(dialect: Dialect, version: Option<Version>) -> Target {
        Target { dialect, version }
    }

    /// Whether the version is `version` or newer. Without a version it is the newest.
    pub fn at_least(self, version: Version) -> bool {
        self.version.is_none_or(|own| own >= version)
    }
}

impl Default for Target {
    fn default() -> Target {
        Target::GENERIC
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            Some(version) => write!(f, "{} {version}", self.dialect),
            None => write!(f, "{}", self.dialect),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dialect_names_and_versions() {
        assert_eq!(Dialect::parse("PostgreSQL"), Some(Dialect::Postgres));
        assert_eq!(Dialect::parse("pgsql"), Some(Dialect::Postgres));
        assert_eq!(Dialect::parse(" MariaDB "), Some(Dialect::Mariadb));
        assert_eq!(Dialect::parse("sqlite3"), Some(Dialect::Sqlite));
        assert_eq!(Dialect::parse("oracle"), None);
        assert_eq!(Version::parse("8.0.36"), Some(Version::new(8, 0, 36)));
        assert_eq!(Version::parse("18"), Some(Version::new(18, 0, 0)));
        assert_eq!(Version::parse("11.4.2-MariaDB-log"), Some(Version::new(11, 4, 2)));
        assert_eq!(Version::parse("v3.47"), Some(Version::new(3, 47, 0)));
        assert_eq!(Version::parse("latest"), None);
        assert_eq!(Version::new(8, 0, 19).to_string(), "8.0.19");
        assert_eq!(Version::new(8, 4, 0).to_string(), "8.4");
    }

    #[test]
    fn versions_order_and_step_back() {
        assert!(Version::new(8, 0, 31) < Version::new(8, 4, 0));
        assert!(Version::new(8, 0, 19).previous() < Version::new(8, 0, 19));
        assert!(Version::new(8, 0, 18) <= Version::new(8, 0, 19).previous());
        assert!(Version::new(3, 47, 0).previous() < Version::new(3, 47, 0));
        assert!(Version::new(3, 47, 0).previous() > Version::new(3, 46, 9));
        assert!(Version::new(18, 0, 0).previous() < Version::new(18, 0, 0));
        let newest = Target::new(Dialect::Mysql, None);
        assert!(newest.at_least(Version::new(99, 0, 0)));
        let old = Target::new(Dialect::Mysql, Some(Version::new(8, 0, 18)));
        assert!(!old.at_least(Version::new(8, 0, 19)));
        assert_eq!(old.to_string(), "MySQL 8.0.18");
    }
}
