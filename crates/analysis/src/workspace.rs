//! The schema the `.sql` files of a workspace define: the DDL of each file, kept when the file is
//! read, replayed file by file in the order of their paths (migrations are named so that this is
//! the order they run in) into one layer, so an `ALTER TABLE` in a later file changes a table an
//! earlier one created.

use std::path::{Path, PathBuf};

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxKind, parse};

use crate::catalog::{Layer, Origin, ScriptState};
use crate::ddl::{DdlContext, apply_statement};
use crate::ident::Case;

/// A statement of a file that defines something or changes where names resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    /// Where it starts in the file.
    offset: u32,
    text: String,
    /// The delimiter of MySQL's `DELIMITER` it was read under, when not `;`.
    delimiter: Option<String>,
}

/// The DDL of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDdl {
    statements: Vec<Kept>,
}

impl FileDdl {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    pub fn len(&self) -> usize {
        self.statements.len()
    }
}

fn kept(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        CREATE_TABLE_STMT
            | CREATE_VIEW_STMT
            | CREATE_INDEX_STMT
            | CREATE_TYPE_STMT
            | CREATE_DOMAIN_STMT
            | CREATE_SEQUENCE_STMT
            | CREATE_FUNCTION_STMT
            | CREATE_TRIGGER_STMT
            | CREATE_SCHEMA_STMT
            | ALTER_TABLE_STMT
            | RENAME_TABLE_STMT
            | DROP_STMT
            | COMMENT_STMT
            | USE_STMT
            | SET_STMT
            | ATTACH_STMT
    )
}

/// The statements of a script that define something, to replay later.
pub fn extract(text: &str, dialect: Dialect) -> FileDdl {
    let root = parse(text, dialect).syntax();
    let statements = root
        .children()
        .filter(|statement| kept(statement.kind()))
        .map(|statement| {
            let delimiter = statement
                .last_token()
                .filter(|token| token.kind() == CUSTOM_DELIMITER)
                .map(|token| token.text().to_string());
            Kept {
                offset: statement.text_range().start().into(),
                text: statement.text().to_string(),
                delimiter,
            }
        })
        .collect();
    FileDdl { statements }
}

/// Replays the DDL of files, in the order given, into one layer.
pub fn build<'a>(dialect: Dialect, files: impl IntoIterator<Item = (&'a Path, &'a FileDdl)>) -> Layer {
    let mut layer = Layer::empty(Origin::Workspace);
    let case = if dialect == Dialect::Postgres {
        Case::Exact
    } else {
        Case::Insensitive
    };
    let base = |_: Option<&str>, _: &str| None;
    for (path, file) in files {
        let mut state = ScriptState::default();
        for statement in &file.statements {
            let prefix = statement
                .delimiter
                .as_ref()
                .map(|delimiter| format!("DELIMITER {delimiter}\n"))
                .unwrap_or_default();
            let text = format!("{prefix}{}", statement.text);
            let root = parse(&text, dialect).syntax();
            let context = DdlContext {
                dialect,
                path: Some(PathBuf::from(path)),
                case,
                base: &base,
                shift: i64::from(statement.offset) - prefix.len() as i64,
            };
            for node in root.children() {
                apply_statement(&mut layer, &mut state, &node, &context);
            }
        }
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_files_in_order_with_their_places() {
        let first = "-- users\nCREATE TABLE users (id int);\nINSERT INTO users VALUES (1);\n";
        let second = "ALTER TABLE users ADD COLUMN email text;\nCREATE VIEW emails AS SELECT email FROM users;\n";
        let files = [
            (Path::new("/w/1_users.sql"), extract(first, Dialect::Postgres)),
            (Path::new("/w/2_email.sql"), extract(second, Dialect::Postgres)),
        ];
        assert_eq!(files[0].1.len(), 1, "an INSERT is not kept");
        let layer = build(Dialect::Postgres, files.iter().map(|(path, file)| (*path, file)));
        let schema = &layer.snapshot.schemas[0];
        let users = &schema.tables[0];
        let names: Vec<&str> = users.columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["id", "email"]);
        let location = users.location.as_ref().expect("a place");
        assert_eq!(location.path.as_deref(), Some(Path::new("/w/1_users.sql")));
        assert_eq!(&first[location.name.0 as usize..location.name.1 as usize], "users");
        let email = users.columns[1].location.as_ref().expect("a place");
        assert_eq!(&second[email.name.0 as usize..email.name.1 as usize], "email");
        assert_eq!(schema.tables[1].name, "emails");
    }

    #[test]
    fn keeps_routines_written_under_a_delimiter() {
        let text = "DELIMITER //\nCREATE PROCEDURE p(IN a INT)\nBEGIN\n  SELECT a;\nEND//\nDELIMITER ;\n";
        let file = extract(text, Dialect::Mysql);
        let layer = build(Dialect::Mysql, [(Path::new("/w/p.sql"), &file)]);
        let routine = &layer.snapshot.schemas[0].routines[0];
        assert_eq!(routine.name, "p");
        let location = routine.location.as_ref().expect("a place");
        assert_eq!(&text[location.name.0 as usize..location.name.1 as usize], "p");
    }
}
