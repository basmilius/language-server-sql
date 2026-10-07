//! Names that stand for nothing known: unknown tables, columns and functions, and columns more
//! than one table in scope has. Only reported where the schema is known: a table only in a schema
//! a snapshot covers, a column only of a table whose columns are all known, a function only with a
//! snapshot loaded. A script with no snapshot and no DDL gets none of these.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, Target};

use crate::context::{DocumentSchema, Schemas, knows_schema};
use crate::diagnostics::{Diagnostic, DiagnosticSeverity};
use crate::ident::Ident;
use crate::resolve::{Referent, Resolution, Resolver};

pub const UNRESOLVED_TABLE: &str = "unresolved-table";
pub const UNRESOLVED_COLUMN: &str = "unresolved-column";
pub const UNRESOLVED_FUNCTION: &str = "unresolved-function";
pub const AMBIGUOUS_COLUMN: &str = "ambiguous-column";

fn error(name: &SyntaxNode, message: String, code: &'static str) -> Diagnostic {
    Diagnostic {
        range: name.text_range(),
        message,
        severity: DiagnosticSeverity::Error,
        deprecated: false,
        code,
    }
}

/// The names of a script that resolve to nothing, statement by statement, each read against the
/// DDL of the statements before it.
pub fn unresolved(root: &SyntaxNode, target: Target, schemas: Schemas) -> Vec<Diagnostic> {
    unresolved_in(root, target, schemas, None)
}

/// The names that resolve to nothing in the statements a range touches, or in every statement.
pub fn unresolved_in(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    range: Option<sql_syntax::TextRange>,
) -> Vec<Diagnostic> {
    let mut document = DocumentSchema::new(target, schemas);
    let mut found = Vec::new();
    for statement in root.children() {
        let wanted = range.is_none_or(|range| range.intersect(statement.text_range()).is_some());
        if wanted && statement.kind() != DROP_STMT {
            let catalog = document.catalog();
            if knows_schema(&catalog) {
                let resolver = Resolver::new(&catalog);
                check_statement(&resolver, &statement, target.dialect, &mut found);
            }
        }
        document.apply(&statement);
    }
    found
}

fn in_routine_body(node: &SyntaxNode) -> bool {
    node.ancestors().any(|ancestor| {
        matches!(
            ancestor.kind(),
            ROUTINE_BODY | CREATE_FUNCTION_STMT | CREATE_TRIGGER_STMT
        )
    })
}

fn check_statement(resolver: &Resolver, statement: &SyntaxNode, dialect: Dialect, found: &mut Vec<Diagnostic>) {
    for name in statement.descendants().filter(|node| node.kind() == NAME) {
        if name.ancestors().any(|ancestor| ancestor.kind() == ERROR) {
            continue;
        }
        let Some(parent) = name.parent() else {
            continue;
        };
        let text = || {
            Ident::of_name(&name, dialect)
                .map(|ident| ident.text)
                .unwrap_or_default()
        };
        match parent.kind() {
            COLUMN_REF => {
                let all = crate::ast::parts(&parent, dialect);
                let Some(position) = all.iter().position(|part| part.node == name) else {
                    continue;
                };
                let last = position + 1 == all.len();
                if all.len() == 1 && in_routine_body(&name) {
                    continue;
                }
                match resolver.resolve_name(&name) {
                    Some(Resolution::Unknown { complete: true }) if last => {
                        let message = if position > 0 {
                            format!("Unknown column '{}' in '{}'", text(), all[position - 1].ident.text)
                        } else {
                            format!("Unknown column '{}'", text())
                        };
                        found.push(error(&name, message, UNRESOLVED_COLUMN));
                    }
                    Some(Resolution::Unknown { complete: true }) => found.push(error(
                        &name,
                        format!("Unknown table or alias '{}'", text()),
                        UNRESOLVED_TABLE,
                    )),
                    Some(Resolution::Ambiguous(all)) => {
                        let tables: Vec<String> = all
                            .iter()
                            .filter_map(|referent| match referent {
                                Referent::Column { source, .. } => Some(format!("'{}'", source.name.text)),
                                _ => None,
                            })
                            .collect();
                        found.push(error(
                            &name,
                            format!("Column '{}' is ambiguous: {} have it", text(), join_and(&tables)),
                            AMBIGUOUS_COLUMN,
                        ));
                    }
                    _ => {}
                }
            }
            QUALIFIED_NAME => {
                let Some(owner) = parent.parent() else {
                    continue;
                };
                let last = parent.children().filter(|child| child.kind() == NAME).last().as_ref() == Some(&name);
                if !last {
                    continue;
                }
                let code = match owner.kind() {
                    FUNCTION_CALL => UNRESOLVED_FUNCTION,
                    TABLE_REF | INSERT_STMT | UPDATE_STMT | DELETE_STMT | MERGE_STMT | CREATE_INDEX_STMT
                    | ALTER_TABLE_STMT | TRUNCATE_STMT | REFERENCES_CLAUSE | LIKE_CLAUSE | TABLE_QUERY
                    | CREATE_TRIGGER_STMT => UNRESOLVED_TABLE,
                    _ => continue,
                };
                if code == UNRESOLVED_TABLE && owner.kind() == REFERENCES_CLAUSE {
                    let own = owner
                        .ancestors()
                        .find(|ancestor| ancestor.kind() == CREATE_TABLE_STMT)
                        .and_then(|statement| crate::ast::child(&statement, QUALIFIED_NAME))
                        .and_then(|own| crate::ast::object_name(&own, dialect))
                        .is_some_and(|(_, own)| own.ident.text.eq_ignore_ascii_case(&text()));
                    if own {
                        continue;
                    }
                }
                if let Some(Resolution::Unknown { complete: true }) = resolver.resolve_name(&name) {
                    let message = if code == UNRESOLVED_FUNCTION {
                        format!("Unknown function '{}'", text())
                    } else {
                        format!("Unknown table '{}'", text())
                    };
                    found.push(error(&name, message, code));
                }
            }
            NAME_LIST => {
                let reported = parent
                    .parent()
                    .is_some_and(|owner| matches!(owner.kind(), INSERT_STMT | USING_CLAUSE | MERGE_WHEN_CLAUSE));
                if !reported {
                    continue;
                }
                if let Some(Resolution::Unknown { complete: true }) = resolver.resolve_name(&name) {
                    found.push(error(&name, format!("Unknown column '{}'", text()), UNRESOLVED_COLUMN));
                }
            }
            _ => {}
        }
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
