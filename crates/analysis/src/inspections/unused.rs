//! Names declared and never read, and names declared twice: a common table expression nothing
//! reads, a table alias nothing qualifies a name with, two tables of one `FROM` or two common
//! table expressions of one `WITH` under one name, and two columns of a table, a view or an
//! `INSERT` with one name.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, TextRange};

use super::tree::{nested_in_query, with_space_before};
use super::{Cx, DUPLICATE_ALIAS, DUPLICATE_COLUMN, DUPLICATE_CTE, Stmt, UNUSED_ALIAS, UNUSED_CTE};
use crate::ast::{alias_of, child, children, inner_query, is_query, object_name, parts};
use crate::diagnostics::DiagnosticSeverity;
use crate::ident::{Case, Ident, name_case};
use crate::refs::{Namer, Symbol, bare_text};
use crate::rename::TextEdit;
use crate::resolve::output_name;

pub(super) fn run(cx: &Cx, stmt: &Stmt) {
    if !stmt.clean {
        return;
    }
    if cx.on(UNUSED_CTE) || cx.on(UNUSED_ALIAS) {
        unused(cx, stmt);
    }
    let case = name_case(cx.dialect());
    for node in stmt.node.descendants() {
        if !cx.wants(node.text_range()) {
            continue;
        }
        match node.kind() {
            WITH_CLAUSE if cx.on(DUPLICATE_CTE) => {
                let names: Vec<(Ident, SyntaxNode)> = children(&node, CTE)
                    .filter_map(|cte| child(&cte, NAME))
                    .filter_map(|name| Some((Ident::of_name(&name, cx.dialect())?, name)))
                    .collect();
                duplicates(
                    cx,
                    DUPLICATE_CTE,
                    &names,
                    case,
                    "common table expression",
                    DiagnosticSeverity::Error,
                );
            }
            SELECT if cx.on(DUPLICATE_ALIAS) => duplicate_aliases(cx, stmt, &node),
            CREATE_TABLE_STMT | CREATE_VIEW_STMT | INSERT_STMT if cx.on(DUPLICATE_COLUMN) => {
                duplicate_columns(cx, &node, case)
            }
            _ => {}
        }
    }
}

/// Reports the second and later of names that are the same.
fn duplicates(
    cx: &Cx,
    id: &'static str,
    names: &[(Ident, SyntaxNode)],
    case: Case,
    what: &str,
    severity: DiagnosticSeverity,
) {
    for (position, (ident, node)) in names.iter().enumerate() {
        let Some((_, first)) = names[..position]
            .iter()
            .find(|(earlier, _)| case.eq(&earlier.text, &ident.text))
        else {
            continue;
        };
        cx.report(
            id,
            node.text_range(),
            format!("The {what} '{}' is already declared", ident.text),
        )
        .severity(severity)
        .related(first.text_range(), format!("The first {what} '{}'", ident.text))
        .emit();
    }
}

/// The name a table of `FROM` is known by: its alias, or the table's name with its schema.
fn exposed_name(item: &SyntaxNode, dialect: Dialect) -> Option<(Ident, SyntaxNode)> {
    if let Some((alias, _)) = alias_of(item, dialect) {
        return Some((alias.ident, alias.node));
    }
    if item.kind() != TABLE_REF {
        return None;
    }
    let name = child(item, QUALIFIED_NAME)?;
    let (schema, table) = object_name(&name, dialect)?;
    let text = match schema {
        Some(schema) => format!("{}.{}", schema.ident.text, table.ident.text),
        None => table.ident.text,
    };
    Some((
        Ident {
            text,
            quoted: table.ident.quoted,
        },
        name,
    ))
}

fn duplicate_aliases(cx: &Cx, stmt: &Stmt, select: &SyntaxNode) {
    let Some(from) = child(select, FROM_CLAUSE) else {
        return;
    };
    let names: Vec<(Ident, SyntaxNode)> = from
        .descendants()
        .filter(|node| matches!(node.kind(), TABLE_REF | DERIVED_TABLE | TABLE_FUNCTION))
        .filter(|node| !nested_in_query(node, &from))
        .filter_map(|node| exposed_name(&node, cx.dialect()))
        .collect();
    let what = "table or alias";
    // SQLite takes two tables of one name, and only refuses a column that could be of either.
    let severity = if cx.dialect() == Dialect::Sqlite {
        DiagnosticSeverity::Warning
    } else {
        DiagnosticSeverity::Error
    };
    duplicates(cx, DUPLICATE_ALIAS, &names, stmt.catalog.table_case, what, severity);
}

fn duplicate_columns(cx: &Cx, statement: &SyntaxNode, case: Case) {
    let dialect = cx.dialect();
    let mut names: Vec<(Ident, SyntaxNode)> = Vec::new();
    let mut severity = DiagnosticSeverity::Error;
    match statement.kind() {
        INSERT_STMT => {
            // SQLite takes a column twice in an INSERT, the last value winning.
            if dialect == Dialect::Sqlite {
                severity = DiagnosticSeverity::Warning;
            }
            if let Some(list) = child(statement, NAME_LIST) {
                names = children(&list, NAME)
                    .filter_map(|name| Some((Ident::of_name(&name, dialect)?, name)))
                    .collect();
            }
        }
        _ => {
            if let Some(list) = child(statement, TABLE_ELEMENT_LIST) {
                names = children(&list, COLUMN_DEF)
                    .filter_map(|column| child(&column, NAME))
                    .filter_map(|name| Some((Ident::of_name(&name, dialect)?, name)))
                    .collect();
            } else if child(statement, NAME_LIST).is_none() {
                let query = statement.children().find(|node| is_query(node.kind()));
                let mut select = query;
                while let Some(inner) = select.clone().filter(|node| node.kind() != SELECT) {
                    select = inner_query(&inner);
                }
                if let Some(list) = select.and_then(|select| child(&select, SELECT_LIST)) {
                    names = children(&list, SELECT_ITEM)
                        .filter_map(|item| {
                            let name = output_name(&item, dialect)?;
                            let at = alias_of(&item, dialect)
                                .map(|(alias, _)| alias.node)
                                .or_else(|| item.children().next())?;
                            Some((Ident::new(name), at))
                        })
                        .collect();
                }
                // SQLite gives a view or a table made from a query a column of each name, numbering the later ones.
                if dialect == Dialect::Sqlite {
                    severity = DiagnosticSeverity::Warning;
                }
            }
        }
    }
    let case = if dialect == Dialect::Postgres {
        case
    } else {
        Case::Insensitive
    };
    duplicates(cx, DUPLICATE_COLUMN, &names, case, "column", severity);
}

/// A declaration and whether anything reads it.
struct Declared {
    symbol: Symbol,
    name: SyntaxNode,
    /// The node a read inside does not count, as a recursive query reading itself.
    owner: SyntaxNode,
    used: bool,
}

fn unused(cx: &Cx, stmt: &Stmt) {
    let namer = Namer::new(stmt.catalog, stmt.state);
    let mut declared: Vec<Declared> = Vec::new();
    for node in stmt.node.descendants() {
        let local = |name: &SyntaxNode| namer.symbol_at(name).map(|(symbol, _)| symbol);
        match node.kind() {
            CTE if cx.on(UNUSED_CTE) => {
                let modifies = node
                    .children()
                    .any(|inner| matches!(inner.kind(), INSERT_STMT | UPDATE_STMT | DELETE_STMT | MERGE_STMT));
                let Some(name) = child(&node, NAME) else {
                    continue;
                };
                if modifies {
                    continue;
                }
                if let Some(symbol) = local(&name) {
                    declared.push(Declared {
                        symbol,
                        name,
                        owner: node.clone(),
                        used: false,
                    });
                }
            }
            ALIAS if cx.on(UNUSED_ALIAS) => {
                if let Some(name) = alias_to_judge(cx, stmt, &node) {
                    if let Some(symbol) = local(&name) {
                        declared.push(Declared {
                            symbol,
                            name,
                            owner: node.clone(),
                            used: false,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if declared.is_empty() {
        return;
    }
    let words: Vec<String> = declared.iter().map(|declared| bare_text(&declared.name)).collect();
    for name in stmt.node.descendants().filter(|node| node.kind() == NAME) {
        let text = bare_text(&name);
        if !words.iter().any(|word| word.eq_ignore_ascii_case(&text)) {
            continue;
        }
        if declared.iter().any(|declared| declared.name == name) {
            continue;
        }
        let Some((symbol, _)) = namer.symbol_at(&name) else {
            continue;
        };
        for entry in declared.iter_mut() {
            let inside = entry.owner.text_range().contains_range(name.text_range()) && entry.owner.kind() == CTE;
            if entry.symbol == symbol && !inside {
                entry.used = true;
            }
        }
    }
    for entry in declared.iter().filter(|entry| !entry.used) {
        if !cx.wants(entry.name.text_range()) {
            continue;
        }
        let text = bare_text(&entry.name);
        if entry.owner.kind() == CTE {
            cx.report(
                UNUSED_CTE,
                entry.name.text_range(),
                format!("The common table expression '{text}' is never used"),
            )
            .fix(format!("Remove '{text}'"), || remove_cte(&entry.owner))
            .emit();
        } else {
            cx.report(
                UNUSED_ALIAS,
                entry.name.text_range(),
                format!("The alias '{text}' is never used"),
            )
            .fix(format!("Remove the alias '{text}'"), || {
                Some(vec![TextEdit {
                    range: with_space_before(&entry.owner),
                    text: String::new(),
                }])
            })
            .emit();
        }
    }
}

/// The name of an alias whose removal changes nothing when nothing reads it: of a table, without
/// column names, and not of a table the statement reads twice, where the alias tells the two
/// apart. A subquery keeps its alias, which MySQL and MariaDB require.
fn alias_to_judge(cx: &Cx, stmt: &Stmt, alias: &SyntaxNode) -> Option<SyntaxNode> {
    let owner = alias.parent()?;
    if owner.kind() != TABLE_REF || child(alias, NAME_LIST).is_some() || child(alias, TABLE_ELEMENT_LIST).is_some() {
        return None;
    }
    let table = child(&owner, QUALIFIED_NAME).and_then(|name| parts(&name, cx.dialect()).pop())?;
    let twice = stmt
        .node
        .descendants()
        .filter(|node| node.kind() == TABLE_REF && *node != owner)
        .filter_map(|node| child(&node, QUALIFIED_NAME).and_then(|name| parts(&name, cx.dialect()).pop()))
        .any(|other| other.ident.text.eq_ignore_ascii_case(&table.ident.text));
    if twice {
        return None;
    }
    child(alias, NAME)
}

/// The edit that removes a common table expression: the whole `WITH` when it is the only one,
/// else it and the comma that goes with it.
fn remove_cte(cte: &SyntaxNode) -> Option<Vec<TextEdit>> {
    let with = cte.parent()?;
    let all: Vec<SyntaxNode> = children(&with, CTE).collect();
    let position = all.iter().position(|other| other == cte)?;
    if all.len() == 1 {
        let end = with
            .last_token()
            .and_then(|token| token.next_token())
            .filter(|next| next.kind() == sql_syntax::SyntaxKind::WHITESPACE)
            .map_or(with.text_range().end(), |next| next.text_range().end());
        return Some(vec![TextEdit {
            range: TextRange::new(with.text_range().start(), end),
            text: String::new(),
        }]);
    }
    Some(vec![super::writes::list_item_removal(&all, position)?])
}
