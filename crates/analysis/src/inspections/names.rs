//! Names that stand for nothing known: unknown tables, columns and functions, and columns more
//! than one table in scope has. Only reported where the schema is known: a table only in a schema
//! a snapshot covers, a column only of a table whose columns are all known, a function only with a
//! snapshot loaded. A script with no snapshot and no DDL gets none of these. A name that is a slip
//! of a known one gets a fix for each of the nearest.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxNode, TextRange};

use super::{AMBIGUOUS_COLUMN, Cx, QuickFix, Stmt, UNRESOLVED_COLUMN, UNRESOLVED_FUNCTION, UNRESOLVED_TABLE};
use crate::ast::{alias_of, child, children, parts};
use crate::catalog::Catalog;
use crate::ident::{Ident, quote_name};
use crate::refs::bare_text;
use crate::rename::TextEdit;
use crate::resolve::{ColumnOrigin, Referent, Resolution, Resolver, Source, SourceKind};

pub(super) fn run(cx: &Cx, stmt: &Stmt) {
    let any = [
        UNRESOLVED_TABLE,
        UNRESOLVED_COLUMN,
        UNRESOLVED_FUNCTION,
        AMBIGUOUS_COLUMN,
    ]
    .iter()
    .any(|id| cx.on(id));
    if !any || !stmt.known || stmt.node.kind() == DROP_STMT {
        return;
    }
    let dialect = cx.dialect();
    let resolver = stmt.resolver;
    for name in stmt.node.descendants().filter(|node| node.kind() == NAME) {
        if name.ancestors().any(|ancestor| ancestor.kind() == ERROR) || !cx.wants(name.text_range()) {
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
                let all = parts(&parent, dialect);
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
                        cx.report(UNRESOLVED_COLUMN, name.text_range(), message)
                            .fixes(|| near_miss_fixes(cx, &name, column_names(resolver, &name, cx)))
                            .emit();
                    }
                    Some(Resolution::Unknown { complete: true }) => {
                        cx.report(
                            UNRESOLVED_TABLE,
                            name.text_range(),
                            format!("Unknown table or alias '{}'", text()),
                        )
                        .fixes(|| near_miss_fixes(cx, &name, table_names(resolver, stmt.catalog, &name)))
                        .emit();
                    }
                    Some(Resolution::Ambiguous(all)) => {
                        let sources: Vec<&Source> = all
                            .iter()
                            .filter_map(|referent| match referent {
                                Referent::Column { source, .. } => Some(source),
                                _ => None,
                            })
                            .collect();
                        let tables: Vec<String> =
                            sources.iter().map(|source| format!("'{}'", source.name.text)).collect();
                        let mut pending = cx.report(
                            AMBIGUOUS_COLUMN,
                            name.text_range(),
                            format!("Column '{}' is ambiguous: {} have it", text(), join_and(&tables)),
                        );
                        for source in &sources {
                            if let Some(declared) = &source.name_node {
                                pending = pending.related(
                                    declared.text_range(),
                                    format!("'{}' has a column '{}'", source.name.text, text()),
                                );
                            }
                        }
                        pending.fixes(|| ambiguous_fixes(&name, &sources)).emit();
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
                if code == UNRESOLVED_TABLE
                    && owner.kind() == REFERENCES_CLAUSE
                    && references_itself(&owner, &text(), cx)
                {
                    continue;
                }
                if let Some(Resolution::Unknown { complete: true }) = resolver.resolve_name(&name) {
                    if code == UNRESOLVED_FUNCTION {
                        cx.report(code, name.text_range(), format!("Unknown function '{}'", text()))
                            .fixes(|| near_miss_fixes(cx, &name, function_names(stmt.catalog)))
                            .emit();
                    } else {
                        cx.report(code, name.text_range(), format!("Unknown table '{}'", text()))
                            .fixes(|| near_miss_fixes(cx, &name, table_names(resolver, stmt.catalog, &name)))
                            .emit();
                    }
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
                    cx.report(
                        UNRESOLVED_COLUMN,
                        name.text_range(),
                        format!("Unknown column '{}'", text()),
                    )
                    .fixes(|| near_miss_fixes(cx, &name, column_names(resolver, &name, cx)))
                    .emit();
                }
            }
            _ => {}
        }
    }
}

fn in_routine_body(node: &SyntaxNode) -> bool {
    node.ancestors().any(|ancestor| {
        matches!(
            ancestor.kind(),
            ROUTINE_BODY | CREATE_FUNCTION_STMT | CREATE_TRIGGER_STMT
        )
    })
}

/// A foreign key of a `CREATE TABLE` to the table itself, which does not exist yet.
fn references_itself(clause: &SyntaxNode, name: &str, cx: &Cx) -> bool {
    clause
        .ancestors()
        .find(|ancestor| ancestor.kind() == CREATE_TABLE_STMT)
        .and_then(|statement| child(&statement, QUALIFIED_NAME))
        .and_then(|own| crate::ast::object_name(&own, cx.dialect()))
        .is_some_and(|(_, own)| own.ident.text.eq_ignore_ascii_case(name))
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// How many single-character edits make one name the other, without case; swapping two letters
/// next to each other is one edit, the commonest slip of all.
pub(crate) fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in table.iter_mut().enumerate() {
        row[0] = i;
    }
    table[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (table[i - 1][j] + 1)
                .min(table[i][j - 1] + 1)
                .min(table[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(table[i - 2][j - 2] + 1);
            }
            table[i][j] = best;
        }
    }
    table[a.len()][b.len()]
}

/// The known names closest to an unknown one, near enough to be a slip: at most a third of its
/// letters off, and never more than three.
pub(crate) fn near_misses(wanted: &str, known: impl IntoIterator<Item = String>) -> Vec<String> {
    let most = (wanted.chars().count() / 3).clamp(1, 3);
    let mut found: Vec<(usize, String)> = known
        .into_iter()
        .filter(|name| !name.eq_ignore_ascii_case(wanted))
        .map(|name| (distance(wanted, &name), name))
        .filter(|(distance, _)| *distance <= most)
        .collect();
    found.sort();
    found.dedup_by(|second, first| second.1.eq_ignore_ascii_case(&first.1));
    found.into_iter().take(3).map(|(_, name)| name).collect()
}

fn near_miss_fixes(cx: &Cx, name: &SyntaxNode, known: Vec<String>) -> Vec<QuickFix> {
    near_misses(&bare_text(name), known)
        .into_iter()
        .map(|fix| {
            let spelled = name.first_token().map_or_else(
                || quote_name(&fix, cx.target),
                |token| crate::rename::spell(&token, &fix, cx.target),
            );
            QuickFix {
                title: format!("Change to '{fix}'"),
                edits: vec![TextEdit {
                    range: name.text_range(),
                    text: spelled,
                }],
            }
        })
        .collect()
}

fn table_names(resolver: &Resolver, catalog: &Catalog, name: &SyntaxNode) -> Vec<String> {
    let mut names: Vec<String> = resolver.ctes(name).into_iter().map(|cte| cte.name.text).collect();
    let schema = name
        .parent()
        .filter(|parent| parent.kind() == QUALIFIED_NAME)
        .map(|qualified| parts(&qualified, catalog.dialect()))
        .filter(|all| all.len() >= 2)
        .map(|all| all[all.len() - 2].ident.text.clone());
    names.extend(
        catalog
            .tables(schema.as_deref())
            .into_iter()
            .map(|id| catalog.table(id).name.clone()),
    );
    if let Some(reference) = name.parent().filter(|parent| parent.kind() == COLUMN_REF) {
        let levels = resolver.scope(&reference);
        names.extend(
            levels
                .iter()
                .flat_map(|level| level.sources.iter())
                .map(|source| source.name.text.clone()),
        );
    }
    names
}

fn column_names(resolver: &Resolver, name: &SyntaxNode, cx: &Cx) -> Vec<String> {
    let Some(holder) = name.parent() else {
        return Vec::new();
    };
    let levels = resolver.scope(&holder);
    let all = if holder.kind() == COLUMN_REF {
        parts(&holder, cx.dialect())
    } else {
        Vec::new()
    };
    let sources: Vec<Source> = if all.len() >= 2 {
        resolver
            .find_source(&levels, &all[all.len() - 2].ident)
            .into_iter()
            .collect()
    } else {
        levels.iter().flat_map(|level| level.sources.iter().cloned()).collect()
    };
    let mut names: Vec<String> = sources
        .iter()
        .flat_map(|source| resolver.columns(source, 0).columns)
        .filter(|column| column.origin != ColumnOrigin::Implicit)
        .map(|column| column.name)
        .collect();
    if all.len() < 2 {
        for level in &levels {
            if let Some(list) = level.select.as_ref().and_then(|select| child(select, SELECT_LIST)) {
                names.extend(
                    children(&list, SELECT_ITEM)
                        .filter_map(|item| alias_of(&item, cx.dialect()))
                        .map(|(alias, _)| alias.ident.text),
                );
            }
        }
    }
    names
}

fn function_names(catalog: &Catalog) -> Vec<String> {
    let mut names: Vec<String> = catalog
        .builtins
        .functions
        .iter()
        .filter(|function| function.overloads_at(catalog.target).next().is_some())
        .map(|function| function.name.clone())
        .collect();
    names.extend(
        catalog
            .all(|schema| &schema.routines, |routine| &routine.name)
            .into_iter()
            .map(|routine| routine.name.clone()),
    );
    names
}

/// The name a source is qualified by, as written.
pub(crate) fn qualifier_of(source: &Source) -> Option<String> {
    let name = source.name_node.as_ref()?;
    let text = name.text().to_string();
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn ambiguous_fixes(name: &SyntaxNode, sources: &[&Source]) -> Vec<QuickFix> {
    sources
        .iter()
        .filter(|source| !matches!(source.kind, SourceKind::Unknown))
        .filter_map(|source| qualifier_of(source))
        .map(|qualifier| QuickFix {
            title: format!("Qualify with '{qualifier}'"),
            edits: vec![TextEdit {
                range: TextRange::empty(name.text_range().start()),
                text: format!("{qualifier}."),
            }],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_how_far_names_are() {
        assert_eq!(distance("users", "usres"), 1);
        assert_eq!(distance("users", "user"), 1);
        assert_eq!(distance("email", "EMAIL"), 0);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(
            near_misses("usres", ["users".to_string(), "orders".to_string()]),
            ["users"]
        );
    }
}
