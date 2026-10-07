//! Code actions: rewrites a person asks for at a cursor or a selection (qualify a column with its
//! table, expand `*` into the columns it stands for, give a table an alias, put the keywords of a
//! selection in upper or lower case) and quick fixes for unknown names that are a near miss of a
//! known one.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxElement, SyntaxNode, Target, TextRange, TextSize};

use crate::ast::{alias_of, child, children, parts};
use crate::catalog::Catalog;
use crate::context::{DocumentSchema, Schemas};
use crate::ident::{Ident, quote_name};
use crate::refs::{bare_text, name_at, statement_of};
use crate::rename::TextEdit;
use crate::resolve::{ColumnOrigin, Referent, Resolution, Resolver, Source, SourceKind};
use crate::unresolved::{AMBIGUOUS_COLUMN, UNRESOLVED_COLUMN, UNRESOLVED_FUNCTION, UNRESOLVED_TABLE, unresolved_in};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    QuickFix,
    Rewrite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub title: String,
    pub kind: ActionKind,
    pub edits: Vec<TextEdit>,
    /// The fix a client may apply without asking which.
    pub preferred: bool,
    /// The diagnostic a quick fix fixes: its range and code.
    pub fixes: Option<(TextRange, &'static str)>,
}

/// The actions at a cursor or for a selection.
pub fn code_actions(root: &SyntaxNode, target: Target, schemas: Schemas, range: TextRange) -> Vec<Action> {
    let mut out = Vec::new();
    out.extend(quick_fixes(root, target, schemas, range));
    if let Some(name) = name_at(root, range.start().into()) {
        if let Some(statement) = statement_of(&name) {
            let document = DocumentSchema::before(root, u32::from(statement.text_range().start()), target, schemas);
            let catalog = document.catalog();
            let resolver = Resolver::new(&catalog);
            out.extend(qualify(&resolver, &name, target));
            out.extend(add_alias(&resolver, &name, &statement, target));
        }
    }
    if let Some(wildcard) = wildcard_at(root, range.start()) {
        if let Some(statement) = statement_of(&wildcard) {
            let document = DocumentSchema::before(root, u32::from(statement.text_range().start()), target, schemas);
            let catalog = document.catalog();
            let resolver = Resolver::new(&catalog);
            out.extend(expand_wildcard(&resolver, &wildcard, target));
        }
    }
    if !range.is_empty() {
        out.extend(keyword_case(root, range));
    }
    out
}

fn wildcard_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxNode> {
    root.token_at_offset(offset.min(root.text_range().end()))
        .filter_map(|token| token.parent_ancestors().find(|node| node.kind() == WILDCARD))
        .next()
}

/// The name a source is qualified by, as written.
fn qualifier_of(source: &Source) -> Option<String> {
    let name = source.name_node.as_ref()?;
    let text = name.text().to_string();
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

/// `email` becomes `u.email`, the alias or table it is a column of.
fn qualify(resolver: &Resolver, name: &SyntaxNode, target: Target) -> Option<Action> {
    let reference = name.parent().filter(|parent| parent.kind() == COLUMN_REF)?;
    if parts(&reference, target.dialect).len() != 1 {
        return None;
    }
    let Some(Resolution::Found(Referent::Column { source, .. })) = resolver.resolve_name(name) else {
        return None;
    };
    let qualifier = qualifier_of(&source)?;
    Some(Action {
        title: format!("Qualify with '{qualifier}'"),
        kind: ActionKind::Rewrite,
        edits: vec![TextEdit {
            range: TextRange::empty(name.text_range().start()),
            text: format!("{qualifier}."),
        }],
        preferred: false,
        fixes: None,
    })
}

/// `*` becomes the columns it stands for, when every one is known.
fn expand_wildcard(resolver: &Resolver, wildcard: &SyntaxNode, target: Target) -> Option<Action> {
    let item = wildcard.parent().filter(|parent| parent.kind() == SELECT_ITEM)?;
    item.parent()?.parent().filter(|select| select.kind() == SELECT)?;
    let levels = resolver.scope(&item);
    let level = levels.first()?;
    let qualifier = parts(wildcard, target.dialect).pop();
    let sources: Vec<&Source> = level
        .sources
        .iter()
        .filter(|source| {
            qualifier
                .as_ref()
                .is_none_or(|qualifier| resolver.source_named(source, &qualifier.ident))
        })
        .collect();
    if sources.is_empty() {
        return None;
    }
    let prefix = qualifier.is_some() || sources.len() > 1;
    let mut names = Vec::new();
    for source in &sources {
        let columns = resolver.columns(source, 0);
        if columns.open {
            return None;
        }
        let qualifier = if prefix { Some(qualifier_of(source)?) } else { None };
        for column in columns.columns {
            if column.origin == ColumnOrigin::Implicit {
                continue;
            }
            let quoted = quote_name(&column.name, target);
            names.push(match &qualifier {
                Some(qualifier) => format!("{qualifier}.{quoted}"),
                None => quoted,
            });
        }
    }
    if names.is_empty() {
        return None;
    }
    Some(Action {
        title: "Expand '*' into its columns".to_string(),
        kind: ActionKind::Rewrite,
        edits: vec![TextEdit {
            range: wildcard.text_range(),
            text: names.join(", "),
        }],
        preferred: false,
        fixes: None,
    })
}

/// An alias for a table name: the first letters of its words, made unique in the statement.
fn new_alias(table: &str, taken: &[String], target: Target) -> String {
    let initials: String = table
        .split(['_', ' ', '-'])
        .filter_map(|word| word.chars().next())
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let base = if initials.is_empty() || !initials.starts_with(|character: char| character.is_alphabetic()) {
        "t".to_string()
    } else {
        initials
    };
    let free = |candidate: &str| {
        !taken.iter().any(|name| name.eq_ignore_ascii_case(candidate))
            && !sql_syntax::is_reserved_word(&candidate.to_ascii_uppercase(), target)
    };
    if free(&base) {
        return base;
    }
    (2..)
        .map(|number| format!("{base}{number}"))
        .find(|candidate| free(candidate))
        .unwrap_or(base)
}

/// `FROM users` becomes `FROM users AS u`, and the columns the statement qualifies with `users`
/// are qualified with `u`.
fn add_alias(resolver: &Resolver, name: &SyntaxNode, statement: &SyntaxNode, target: Target) -> Option<Action> {
    let qualified = name.parent().filter(|parent| parent.kind() == QUALIFIED_NAME)?;
    let table_ref = qualified.parent().filter(|parent| parent.kind() == TABLE_REF)?;
    if child(&table_ref, ALIAS).is_some() || children(&qualified, NAME).last().as_ref() != Some(name) {
        return None;
    }
    let taken: Vec<String> = statement
        .descendants()
        .filter(|node| matches!(node.kind(), ALIAS | CTE))
        .filter_map(|node| child(&node, NAME))
        .map(|name| bare_text(&name))
        .chain(
            statement
                .descendants()
                .filter(|node| node.kind() == TABLE_REF)
                .filter_map(|node| child(&node, QUALIFIED_NAME))
                .filter_map(|name| children(&name, NAME).last())
                .map(|name| bare_text(&name)),
        )
        .collect();
    let table = Ident::of_name(name, target.dialect)?.text;
    let alias = new_alias(&table, &taken, target);
    let mut edits = vec![TextEdit {
        range: TextRange::empty(qualified.text_range().end()),
        text: format!(" AS {alias}"),
    }];
    for reference in statement
        .descendants()
        .filter(|node| matches!(node.kind(), COLUMN_REF | WILDCARD))
    {
        let names: Vec<SyntaxNode> = children(&reference, NAME).collect();
        let qualifiers = if reference.kind() == WILDCARD {
            names.len()
        } else {
            names.len().saturating_sub(1)
        };
        if qualifiers == 0 {
            continue;
        }
        let table_part = &names[qualifiers - 1];
        let Some(Resolution::Found(Referent::Source(source))) = resolver.resolve_name(table_part) else {
            continue;
        };
        if source.node != table_ref {
            continue;
        }
        edits.push(TextEdit {
            range: TextRange::new(names[0].text_range().start(), table_part.text_range().end()),
            text: alias.clone(),
        });
    }
    edits.sort_by_key(|edit| edit.range.start());
    Some(Action {
        title: format!("Add the alias '{alias}'"),
        kind: ActionKind::Rewrite,
        edits,
        preferred: false,
        fixes: None,
    })
}

/// The keywords of a selection in upper or lower case, whichever they are not all in.
fn keyword_case(root: &SyntaxNode, range: TextRange) -> Vec<Action> {
    let keywords: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| token.kind().is_keyword() && range.contains_range(token.text_range()))
        .collect();
    let mut out = Vec::new();
    for (title, upper) in [("Uppercase keywords", true), ("Lowercase keywords", false)] {
        let edits: Vec<TextEdit> = keywords
            .iter()
            .filter_map(|token| {
                let spelled = if upper {
                    token.text().to_ascii_uppercase()
                } else {
                    token.text().to_ascii_lowercase()
                };
                (spelled != token.text()).then(|| TextEdit {
                    range: token.text_range(),
                    text: spelled,
                })
            })
            .collect();
        if !edits.is_empty() {
            out.push(Action {
                title: title.to_string(),
                kind: ActionKind::Rewrite,
                edits,
                preferred: false,
                fixes: None,
            });
        }
    }
    out
}

/// How many single-character edits make one name the other, without case; swapping two letters
/// next to each other is one edit, the commonest slip of all.
fn distance(a: &str, b: &str) -> usize {
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
fn near_misses(wanted: &str, known: impl IntoIterator<Item = String>) -> Vec<String> {
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

fn quick_fixes(root: &SyntaxNode, target: Target, schemas: Schemas, range: TextRange) -> Vec<Action> {
    let found: Vec<_> = unresolved_in(root, target, schemas, Some(range))
        .into_iter()
        .filter(|diagnostic| diagnostic.range.intersect(range).is_some())
        .collect();
    let mut out = Vec::new();
    for diagnostic in found {
        let Some(name) = root
            .token_at_offset(diagnostic.range.start())
            .right_biased()
            .and_then(|token| token.parent())
            .filter(|node| node.kind() == NAME)
        else {
            continue;
        };
        let Some(statement) = statement_of(&name) else {
            continue;
        };
        let document = DocumentSchema::before(root, u32::from(statement.text_range().start()), target, schemas);
        let catalog = document.catalog();
        let resolver = Resolver::new(&catalog);
        let written = bare_text(&name);
        let fixes = match diagnostic.code {
            UNRESOLVED_TABLE => near_misses(&written, table_names(&resolver, &catalog, &name)),
            UNRESOLVED_COLUMN => near_misses(&written, column_names(&resolver, &name, target)),
            UNRESOLVED_FUNCTION => near_misses(&written, function_names(&catalog)),
            AMBIGUOUS_COLUMN => {
                out.extend(ambiguous_fixes(&resolver, &name, diagnostic.range));
                continue;
            }
            _ => continue,
        };
        for (position, fix) in fixes.iter().enumerate() {
            let quoted = name.first_token().map_or_else(
                || quote_name(fix, target),
                |token| crate::rename::spell(&token, fix, target),
            );
            out.push(Action {
                title: format!("Change to '{fix}'"),
                kind: ActionKind::QuickFix,
                edits: vec![TextEdit {
                    range: name.text_range(),
                    text: quoted,
                }],
                preferred: position == 0 && fixes.len() == 1,
                fixes: Some((diagnostic.range, diagnostic.code)),
            });
        }
    }
    out
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

fn column_names(resolver: &Resolver, name: &SyntaxNode, target: Target) -> Vec<String> {
    let Some(holder) = name.parent() else {
        return Vec::new();
    };
    let levels = resolver.scope(&holder);
    let all = if holder.kind() == COLUMN_REF {
        parts(&holder, target.dialect)
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
                        .filter_map(|item| alias_of(&item, target.dialect))
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

fn ambiguous_fixes(resolver: &Resolver, name: &SyntaxNode, range: TextRange) -> Vec<Action> {
    let Some(Resolution::Ambiguous(all)) = resolver.resolve_name(name) else {
        return Vec::new();
    };
    all.iter()
        .filter_map(|referent| match referent {
            Referent::Column { source, .. } if !matches!(source.kind, SourceKind::Unknown) => qualifier_of(source),
            _ => None,
        })
        .map(|qualifier| Action {
            title: format!("Qualify with '{qualifier}'"),
            kind: ActionKind::QuickFix,
            edits: vec![TextEdit {
                range: TextRange::empty(name.text_range().start()),
                text: format!("{qualifier}."),
            }],
            preferred: false,
            fixes: Some((range, AMBIGUOUS_COLUMN)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use sql_syntax::{Dialect, parse};

    use super::*;
    use crate::rename::apply;
    use crate::testing::{shop, target, with_snapshot};

    /// Each action: its title, then the text after it.
    fn render(text: &str, actions: &[Action]) -> String {
        let mut out = String::new();
        for action in actions {
            let kind = match action.kind {
                ActionKind::QuickFix => "fix",
                ActionKind::Rewrite => "rewrite",
            };
            out.push_str(&format!("{kind}: {}\n  {}\n", action.title, apply(text, &action.edits)));
        }
        out
    }

    fn check(dialect: Dialect, schemas: Schemas, code: &str, expect: Expect) {
        let (start, rest) = lsc_text::testing::cursor(code);
        let (end, text) = match rest.find("$1") {
            Some(end) => (end as u32, rest.replacen("$1", "", 1)),
            None => (start, rest),
        };
        let root = parse(&text, dialect).syntax();
        let range = TextRange::new(start.into(), end.into());
        expect.assert_eq(&render(&text, &code_actions(&root, target(dialect), schemas, range)));
    }

    #[test]
    fn qualifies_a_column_and_adds_an_alias() {
        let layer = shop(Dialect::Postgres);
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT $0email, users.id FROM users JOIN orders o ON o.user_id = users.id;",
            expect![[r#"
                rewrite: Qualify with 'users'
                  SELECT users.email, users.id FROM users JOIN orders o ON o.user_id = users.id;
            "#]],
        );
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT email, users.id, users.* FROM $0users JOIN orders o ON o.user_id = public.users.id;",
            expect![[r#"
                rewrite: Add the alias 'u'
                  SELECT email, u.id, u.* FROM users AS u JOIN orders o ON o.user_id = u.id;
            "#]],
        );
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT 1 FROM $0orders, users u;",
            expect![[r#"
                rewrite: Add the alias 'o'
                  SELECT 1 FROM orders AS o, users u;
            "#]],
        );
    }

    #[test]
    fn expands_a_wildcard_when_its_columns_are_known() {
        let layer = shop(Dialect::Postgres);
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT $0* FROM orgs;",
            expect![[r#"
                rewrite: Expand '*' into its columns
                  SELECT id, name FROM orgs;
            "#]],
        );
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT o.$0* FROM orgs o JOIN users u ON u.org_id = o.id;",
            expect![[r#"
                rewrite: Expand '*' into its columns
                  SELECT o.id, o.name FROM orgs o JOIN users u ON u.org_id = o.id;
            "#]],
        );
        check(
            Dialect::Postgres,
            Schemas::NONE,
            "SELECT $0* FROM nowhere;",
            expect![[""]],
        );
    }

    #[test]
    fn changes_the_case_of_keywords_in_a_selection() {
        check(
            Dialect::Postgres,
            Schemas::NONE,
            "$0select a From t$1 where b;",
            expect![[r#"
                rewrite: Uppercase keywords
                  SELECT a FROM t where b;
                rewrite: Lowercase keywords
                  select a from t where b;
            "#]],
        );
    }

    #[test]
    fn fixes_a_near_miss() {
        let layer = shop(Dialect::Postgres);
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT emial, u.nmae FROM $0usres u;",
            expect![[r#"
                fix: Change to 'users'
                  SELECT emial, u.nmae FROM users u;
            "#]],
        );
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT $0emial, u.nmae$1 FROM users u;",
            expect![[r#"
                fix: Change to 'email'
                  SELECT email, u.nmae FROM users u;
                fix: Change to 'name'
                  SELECT emial, u.name FROM users u;
            "#]],
        );
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT $0id FROM users JOIN orders ON true;",
            expect![[r#"
                fix: Qualify with 'users'
                  SELECT users.id FROM users JOIN orders ON true;
                fix: Qualify with 'orders'
                  SELECT orders.id FROM users JOIN orders ON true;
            "#]],
        );
        check(
            Dialect::Postgres,
            with_snapshot(&layer),
            "SELECT $0lowr(email) FROM users;",
            expect![[r#"
                fix: Change to 'lower'
                  SELECT lower(email) FROM users;
            "#]],
        );
    }

    #[test]
    fn measures_how_far_names_are() {
        assert_eq!(distance("users", "usres"), 1);
        assert_eq!(distance("users", "user"), 1);
        assert_eq!(distance("email", "EMAIL"), 0);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(
            new_alias("order_items", &["oi".to_string()], target(Dialect::Postgres)),
            "oi2"
        );
        assert_eq!(new_alias("as", &[], target(Dialect::Postgres)), "a");
    }
}
