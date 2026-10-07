//! Code actions: rewrites a person asks for at a cursor or a selection (qualify a column with its
//! table, expand `*` into the columns it stands for, give a table an alias, put the keywords of a
//! selection in upper or lower case), the quick fixes of the inspections at the cursor with the
//! fixes that suppress them, and for an inspection whose fix is safe everywhere, its fix applied
//! to the whole script.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxElement, SyntaxNode, Target, TextRange, TextSize};

use crate::ast::{child, children, parts};
use crate::context::{DocumentSchema, Schemas};
use crate::ident::{Ident, quote_name};
use crate::inspections::qualifier_of;
use crate::inspections::{Finding, InspectionSettings, Request, fix_all, inspect, inspection_info, suppress_fixes};
use crate::refs::{bare_text, name_at, statement_of};
use crate::rename::TextEdit;
use crate::resolve::{ColumnOrigin, Referent, Resolution, Resolver, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    QuickFix,
    Rewrite,
    /// A source action that applies the fixes of many findings at once.
    FixAll,
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
pub fn code_actions(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    settings: &InspectionSettings,
    range: TextRange,
) -> Vec<Action> {
    let mut out = quick_fixes(root, target, schemas, settings, range);
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

/// The fixes of every inspection whose fix is safe everywhere, applied to the whole script at
/// once: what a client asks for with `source.fixAll`.
pub fn fix_all_action(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    settings: &InspectionSettings,
) -> Option<Action> {
    let request = Request {
        fixes: true,
        ..Request::new(target, schemas, settings)
    };
    let edits = fix_all(&inspect(root, &request), None);
    (!edits.is_empty()).then(|| Action {
        title: "Fix all problems that have a safe fix".to_string(),
        kind: ActionKind::FixAll,
        edits,
        preferred: false,
        fixes: None,
    })
}

fn quick_fixes(
    root: &SyntaxNode,
    target: Target,
    schemas: Schemas,
    settings: &InspectionSettings,
    range: TextRange,
) -> Vec<Action> {
    let request = Request {
        range: Some(range),
        fixes: true,
        ..Request::new(target, schemas, settings)
    };
    let found = inspect(root, &request);
    let mut out = Vec::new();
    let mut whole: Vec<&'static str> = Vec::new();
    for finding in &found {
        let diagnostic = &finding.diagnostic;
        for fix in &finding.fixes {
            out.push(Action {
                title: fix.title.clone(),
                kind: ActionKind::QuickFix,
                edits: fix.edits.clone(),
                preferred: finding.fixes.len() == 1,
                fixes: Some((diagnostic.range, diagnostic.code)),
            });
        }
        let safe = inspection_info(diagnostic.code).is_some_and(|info| info.fix_all);
        if safe && finding.fixes.len() == 1 && !whole.contains(&diagnostic.code) {
            whole.push(diagnostic.code);
        }
    }
    for finding in &found {
        for fix in suppress_fixes(root, &finding.diagnostic) {
            out.push(Action {
                title: fix.title,
                kind: ActionKind::QuickFix,
                edits: fix.edits,
                preferred: false,
                fixes: Some((finding.diagnostic.range, finding.diagnostic.code)),
            });
        }
    }
    if !whole.is_empty() {
        let everything: Vec<Finding> = inspect(
            root,
            &Request {
                fixes: true,
                ..Request::new(target, schemas, settings)
            },
        );
        for id in whole {
            let count = everything
                .iter()
                .filter(|finding| finding.diagnostic.code == id && finding.fixes.len() == 1)
                .count();
            if count < 2 {
                continue;
            }
            out.push(Action {
                title: format!("Fix all '{id}' problems in the file ({count})"),
                kind: ActionKind::FixAll,
                edits: fix_all(&everything, Some(id)),
                preferred: false,
                fixes: None,
            });
        }
    }
    out
}

fn wildcard_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxNode> {
    root.token_at_offset(offset.min(root.text_range().end()))
        .filter_map(|token| token.parent_ancestors().find(|node| node.kind() == WILDCARD))
        .next()
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
                ActionKind::FixAll => "fix all",
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
        let settings = InspectionSettings::default();
        let actions = code_actions(&root, target(dialect), schemas, &settings, range);
        let actions: Vec<Action> = actions
            .into_iter()
            .filter(|action| !action.title.starts_with("Suppress"))
            .collect();
        expect.assert_eq(&render(&text, &actions));
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
    fn makes_an_alias_of_initials() {
        assert_eq!(
            new_alias("order_items", &["oi".to_string()], target(Dialect::Postgres)),
            "oi2"
        );
        assert_eq!(new_alias("as", &[], target(Dialect::Postgres)), "a");
    }
}
