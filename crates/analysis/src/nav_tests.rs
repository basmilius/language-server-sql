use std::path::PathBuf;

use expect_test::{Expect, expect};
use sql_syntax::{Dialect, Version};

use crate::catalog::{Layer, Origin, ScriptState};
use crate::context::Schemas;
use crate::ddl::{DdlContext, apply_statement};
use crate::ident::Case;
use crate::nav::{definition, hover};
use crate::signature::signature_help;
use crate::testing::{shop, split_cursor, target, with_snapshot};
use crate::unresolved::unresolved;

fn hover_text(dialect: Dialect, schemas: Schemas, code: &str) -> String {
    let (_, root, offset) = split_cursor(code, dialect);
    hover(&root, offset, target(dialect), schemas).map_or_else(|| "none".to_string(), |found| found.markdown)
}

fn check_hover(dialect: Dialect, code: &str, expect: Expect) {
    let layer = shop(dialect);
    expect.assert_eq(&hover_text(dialect, with_snapshot(&layer), code));
}

#[test]
fn hover_on_a_table_shows_its_columns_and_keys() {
    check_hover(
        Dialect::Postgres,
        "SELECT * FROM $0users",
        expect![[r#"
            **table** `public.users`

            People who log in

            | Column | Type | |
            | --- | --- | --- |
            | `id` | integer | PK, NOT NULL, auto-increment |
            | `org_id` | integer | FK orgs, NOT NULL |
            | `email` | varchar(255) | NOT NULL |
            | `status` | enum('active','blocked') | DEFAULT 'active' |
            | `name` | text | NULL |

            - Primary key (id)
            - Foreign key (org_id) references orgs (id), on delete cascade"#]],
    );
}

#[test]
fn hover_on_a_column_shows_its_type_table_and_comment() {
    check_hover(
        Dialect::Postgres,
        "SELECT u.$0email FROM users u",
        expect![[r#"
            **column** of table `public.users`

            ```sql
            email varchar(255) NOT NULL
            ```

            Where mail goes"#]],
    );
    check_hover(
        Dialect::Postgres,
        "SELECT u.org_$0id FROM users u",
        expect![[r#"
            **column** of table `public.users`

            ```sql
            org_id integer NOT NULL
            ```

            References `orgs` (id)."#]],
    );
}

#[test]
fn hover_on_aliases_ctes_and_functions() {
    check_hover(
        Dialect::Postgres,
        "SELECT $0u.id FROM users u",
        expect!["**alias** `u` for table `public.users`"],
    );
    check_hover(
        Dialect::Postgres,
        "WITH recent AS (SELECT id FROM users) SELECT * FROM $0recent",
        expect![[r#"
            **common table expression** `recent`

            ```sql
            SELECT id FROM users
            ```"#]],
    );
    check_hover(
        Dialect::Postgres,
        "SELECT org_id AS o FROM users ORDER BY $0o",
        expect![[r#"
            **alias** `o`

            ```sql
            org_id
            ```"#]],
    );
    check_hover(
        Dialect::Mysql,
        "SELECT $0concat_ws(',', name) FROM users",
        expect![[r#"
            **function** `concat_ws`

            ```sql
            concat_ws(separator, ...value): string
            ```

            Its arguments joined with a separator, nulls left out."#]],
    );
    check_hover(
        Dialect::Postgres,
        "SELECT $0order_total(1)",
        expect![[r#"
            **function** `order_total`

            ```sql
            order_total(order_id integer): numeric
            ```

            The sum of an order"#]],
    );
    check_hover(
        Dialect::Postgres,
        "CREATE TABLE t (m $0mood)",
        expect![[r#"
            **enum** `mood`

            ```sql
            'happy', 'sad'
            ```"#]],
    );
}

#[test]
fn definition_finds_aliases_ctes_and_ddl_but_not_snapshot_objects() {
    let layer = shop(Dialect::Postgres);
    let place = |code: &str, schemas: Schemas| {
        let (text, root, offset) = split_cursor(code, Dialect::Postgres);
        definition(&root, offset, target(Dialect::Postgres), schemas)
            .into_iter()
            .map(|place| match place.path {
                Some(path) => format!(
                    "{}: {}..{}",
                    path.display(),
                    u32::from(place.name.start()),
                    u32::from(place.name.end())
                ),
                None => text[usize::from(place.name.start())..usize::from(place.name.end())].to_string(),
            })
            .collect::<Vec<_>>()
    };
    let schemas = with_snapshot(&layer);
    assert_eq!(place("SELECT $0u.id FROM users u", schemas), ["u"]);
    assert_eq!(
        place("WITH r AS (SELECT 1 AS one) SELECT r.$0one FROM r", schemas),
        ["one"]
    );
    assert_eq!(place("WITH r AS (SELECT 1) SELECT * FROM $0r", schemas), ["r"]);
    assert_eq!(place("SELECT * FROM $0users", schemas), Vec::<String>::new());
    assert_eq!(
        place(
            "CREATE TABLE notes (body text);\nSELECT $0body FROM notes;",
            Schemas::NONE
        ),
        ["body"]
    );
    let mut workspace = Layer::empty(Origin::Workspace);
    let root = sql_syntax::parse("CREATE TABLE logs (\n  line text\n);", Dialect::Postgres).syntax();
    let base = |_: Option<&str>, _: &str| None;
    let context = DdlContext {
        dialect: Dialect::Postgres,
        path: Some(PathBuf::from("/work/migrations/1.sql")),
        case: Case::Exact,
        base: &base,
        shift: 0,
    };
    let mut state = ScriptState::default();
    for statement in root.children() {
        apply_statement(&mut workspace, &mut state, &statement, &context);
    }
    let schemas = Schemas {
        snapshot: Some(&layer),
        workspace: Some(&workspace),
    };
    assert_eq!(
        place("SELECT * FROM $0logs", schemas),
        ["/work/migrations/1.sql: 13..17"]
    );
    assert_eq!(
        place("SELECT $0line FROM logs", schemas),
        ["/work/migrations/1.sql: 22..26"]
    );
}

fn help(dialect: Dialect, code: &str) -> String {
    let layer = shop(dialect);
    let (_, root, offset) = split_cursor(code, dialect);
    match signature_help(&root, offset, target(dialect), with_snapshot(&layer)) {
        None => "none".to_string(),
        Some(found) => found
            .signatures
            .iter()
            .enumerate()
            .map(|(position, signature)| {
                let marker = if position == found.active_signature { ">" } else { " " };
                format!(
                    "{marker} {} [{}]\n",
                    signature.label,
                    signature
                        .active_parameter
                        .map_or("-".to_string(), |active| active.to_string())
                )
            })
            .collect(),
    }
}

#[test]
fn signature_help_marks_the_parameter_under_the_cursor() {
    assert_eq!(
        help(Dialect::Mysql, "SELECT substring(name, $0) FROM users"),
        "> substring(string, start, length?): string [1]\n"
    );
    assert_eq!(
        help(Dialect::Mysql, "SELECT concat_ws(',', a, b, $0"),
        "> concat_ws(separator, ...value): string [1]\n"
    );
    assert_eq!(
        help(Dialect::Postgres, "SELECT order_total($0)"),
        "> order_total(order_id integer): numeric [0]\n"
    );
    assert_eq!(help(Dialect::Mysql, "CALL archive($0)"), "> archive(before date) [0]\n");
    let left = help(Dialect::Postgres, "SELECT left(name, $0) FROM users");
    assert_eq!(left, "> left(text, integer): text [1]\n");
    assert_eq!(help(Dialect::Postgres, "SELECT left(name, 2)$0 FROM users"), "none");
}

fn problems(dialect: Dialect, schemas: Schemas, text: &str) -> Vec<String> {
    let root = sql_syntax::parse(text, dialect).syntax();
    unresolved(&root, target(dialect), schemas)
        .into_iter()
        .map(|found| format!("{}: {}", found.code, found.message))
        .collect()
}

#[test]
fn unknown_names_are_reported_where_the_schema_is_known() {
    let layer = shop(Dialect::Postgres);
    let schemas = with_snapshot(&layer);
    let found = problems(
        Dialect::Postgres,
        schemas,
        "SELECT u.nope, x.id, id, mail FROM users u JOIN orgs o ON o.id = u.org_id JOIN missing m ON true;\nSELECT no_such_fn(1), count(*), coalesce(1, 2) FROM orgs;\nINSERT INTO users (email, nickname) VALUES ('a', 'b');\nDROP TABLE IF EXISTS gone;",
    );
    assert_eq!(
        found,
        [
            "unresolved-column: Unknown column 'nope' in 'u'",
            "unresolved-table: Unknown table or alias 'x'",
            "ambiguous-column: Column 'id' is ambiguous: 'u' and 'o' have it",
            "unresolved-table: Unknown table 'missing'",
            "unresolved-function: Unknown function 'no_such_fn'",
            "unresolved-column: Unknown column 'nickname'",
        ]
    );
}

#[test]
fn nothing_is_reported_without_a_schema() {
    assert!(
        problems(
            Dialect::Postgres,
            Schemas::NONE,
            "SELECT a, b.c FROM t JOIN u ON t.x = u.y;"
        )
        .is_empty()
    );
    assert_eq!(
        problems(
            Dialect::Postgres,
            Schemas::NONE,
            "CREATE TABLE t (a int);\nSELECT a, b FROM t;\nSELECT * FROM elsewhere;"
        ),
        ["unresolved-column: Unknown column 'b'"],
        "the columns of a table DDL defines are known; a table without a snapshot is not reported"
    );
}

#[test]
fn routines_and_open_sources_are_left_alone() {
    let layer = shop(Dialect::Mysql);
    let schemas = with_snapshot(&layer);
    assert!(
        problems(
            Dialect::Mysql,
            schemas,
            "CREATE PROCEDURE p(IN wanted INT) BEGIN DECLARE n INT; SET n = wanted + 1; SELECT email FROM users WHERE id = n; END"
        )
        .is_empty()
    );
    assert!(
        problems(
            Dialect::Postgres,
            Schemas::NONE,
            "CREATE TABLE t (a int);\nSELECT g, x.one FROM t, generate_series(1, 3) g, LATERAL (SELECT 1 AS one) x;"
        )
        .is_empty()
    );
    let mysql_8_0 = sql_syntax::Target::new(Dialect::Mysql, Version::parse("8.0"));
    let root = sql_syntax::parse("SELECT JSON_VALUE('{}', '$.a') FROM users;", Dialect::Mysql).syntax();
    assert!(unresolved(&root, mysql_8_0, schemas).is_empty());
}
