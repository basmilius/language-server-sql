use sql_syntax::Dialect;

use crate::catalog::Catalog;
use crate::context::{DocumentSchema, Schemas};
use crate::resolve::{ColumnOrigin, Referent, Resolution, Resolver, SourceKind};
use crate::testing::{name_at, shop, split_cursor, target, with_snapshot};

fn describe_referent(catalog: &Catalog, referent: &Referent) -> String {
    match referent {
        Referent::Table(id) => format!(
            "table {}.{}",
            catalog.schema_name(id.place, id.schema),
            catalog.table(*id).name
        ),
        Referent::Cte(node) => format!(
            "cte {}",
            crate::ast::compact(&crate::ast::child(node, sql_syntax::SyntaxKind::NAME).expect("a name"))
        ),
        Referent::Source(source) => {
            let kind = match &source.kind {
                SourceKind::Table(id) => format!("table {}", catalog.table(*id).name),
                SourceKind::Cte(_) => "cte".to_string(),
                SourceKind::Derived(_) => "subquery".to_string(),
                SourceKind::Function(_) => "function".to_string(),
                SourceKind::Defined(_) => "defined".to_string(),
                SourceKind::Unknown => "unknown".to_string(),
            };
            format!("source {} ({kind})", source.name.text)
        }
        Referent::Column { source, column } => {
            let origin = match &column.origin {
                ColumnOrigin::Table(id, _) => format!("of table {}", catalog.table(*id).name),
                ColumnOrigin::Item(item) => format!("of item `{}`", crate::ast::compact(item)),
                ColumnOrigin::Declared(_) => "declared".to_string(),
                ColumnOrigin::Implicit => "implicit".to_string(),
            };
            format!("column {}.{} {origin}", source.name.text, column.name)
        }
        Referent::SelectAlias(item) => format!("alias of `{}`", crate::ast::compact(item)),
        Referent::Variable(name) => format!("variable {}", crate::ast::compact(name)),
        Referent::Schema(name) => format!("schema {name}"),
        Referent::Function(name) => format!("function {name}"),
        Referent::Routines(ids) => format!("routine {}", catalog.routine_at(ids[0]).name),
    }
}

fn describe(catalog: &Catalog, resolution: &Resolution) -> String {
    match resolution {
        Resolution::Found(referent) => describe_referent(catalog, referent),
        Resolution::Ambiguous(all) => format!(
            "ambiguous: {}",
            all.iter()
                .map(|referent| describe_referent(catalog, referent))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Resolution::Unknown { complete: true } => "unknown".to_string(),
        Resolution::Unknown { complete: false } => "unknown, maybe defined elsewhere".to_string(),
    }
}

fn resolve_in(dialect: Dialect, schemas: Schemas, text: &str) -> String {
    let (_, root, offset) = split_cursor(text, dialect);
    let document = DocumentSchema::before(&root, offset, target(dialect), schemas);
    let catalog = document.catalog();
    let resolver = Resolver::new(&catalog);
    let name = name_at(&root, offset).expect("a name at the cursor");
    match resolver.resolve_name(&name) {
        Some(resolution) => describe(&catalog, &resolution),
        None => "nothing".to_string(),
    }
}

fn resolve(dialect: Dialect, text: &str) -> String {
    let layer = shop(dialect);
    resolve_in(dialect, with_snapshot(&layer), text)
}

#[test]
fn columns_resolve_through_aliases_and_table_names() {
    let pg = Dialect::Postgres;
    assert_eq!(
        resolve(pg, "SELECT u.$0email FROM users u"),
        "column u.email of table users"
    );
    assert_eq!(resolve(pg, "SELECT $0u.email FROM users u"), "source u (table users)");
    assert_eq!(
        resolve(pg, "SELECT users.$0email FROM users"),
        "column users.email of table users"
    );
    assert_eq!(
        resolve(pg, "SELECT $0email FROM users"),
        "column users.email of table users"
    );
    assert_eq!(resolve(pg, "SELECT * FROM $0users"), "table public.users");
    assert_eq!(resolve(pg, "SELECT * FROM $0audit.events"), "schema audit");
    assert_eq!(resolve(pg, "SELECT * FROM audit.$0events"), "table audit.events");
    assert_eq!(resolve(pg, "SELECT * FROM users AS $0u"), "source u (table users)");
    assert_eq!(resolve(pg, "SELECT u.$0nope FROM users u"), "unknown");
    assert_eq!(
        resolve(pg, "SELECT x.$0id FROM users u"),
        "unknown, maybe defined elsewhere"
    );
    assert_eq!(resolve(pg, "SELECT $0x.id FROM users u"), "unknown");
    assert_eq!(
        resolve(pg, "SELECT public.users.$0email FROM users"),
        "column users.email of table users"
    );
}

#[test]
fn an_alias_hides_the_table_name_and_case_folds_per_dialect() {
    assert_eq!(resolve(Dialect::Postgres, "SELECT $0users.id FROM users u"), "unknown");
    assert_eq!(
        resolve(Dialect::Postgres, "SELECT U.$0EMAIL FROM USERS u"),
        "column u.email of table users"
    );
    assert_eq!(resolve(Dialect::Postgres, "SELECT * FROM $0\"Users\""), "unknown");
    assert_eq!(
        resolve(Dialect::Mysql, "SELECT * FROM $0USERS"),
        "table shop.USERS".replace("USERS", "users")
    );
    assert_eq!(
        resolve(Dialect::Sqlite, "SELECT $0EMAIL FROM [users]"),
        "column users.email of table users"
    );
    assert_eq!(
        resolve(Dialect::Sqlite, "SELECT $0rowid FROM users"),
        "column users.rowid implicit"
    );
}

#[test]
fn ambiguous_columns_and_what_joins_merge() {
    let pg = Dialect::Postgres;
    assert_eq!(
        resolve(pg, "SELECT $0id FROM users JOIN orgs ON orgs.id = users.org_id"),
        "ambiguous: column users.id of table users, column orgs.id of table orgs"
    );
    assert_eq!(
        resolve(pg, "SELECT $0id FROM users JOIN orders USING (id)"),
        "column users.id of table users"
    );
    assert_eq!(
        resolve(pg, "SELECT $0name FROM users NATURAL JOIN orgs"),
        "column users.name of table users"
    );
    assert_eq!(
        resolve(pg, "SELECT * FROM users JOIN orders USING ($0id)"),
        "column users.id of table users"
    );
}

#[test]
fn common_table_expressions_and_subqueries() {
    let pg = Dialect::Postgres;
    assert_eq!(
        resolve(
            pg,
            "WITH recent AS (SELECT id, email AS mail FROM users) SELECT $0mail FROM recent"
        ),
        "column recent.mail of item `email AS mail`"
    );
    assert_eq!(
        resolve(
            pg,
            "WITH recent (a, b) AS (SELECT id, email FROM users) SELECT r.$0b FROM recent r"
        ),
        "column r.b declared"
    );
    assert_eq!(
        resolve(pg, "WITH recent AS (SELECT 1) SELECT * FROM $0recent"),
        "cte recent"
    );
    assert_eq!(
        resolve(
            pg,
            "WITH RECURSIVE t (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM $0t) SELECT n FROM t"
        ),
        "cte t"
    );
    assert_eq!(
        resolve(pg, "WITH a AS (SELECT * FROM $0b), b AS (SELECT 1) SELECT 1"),
        "unknown"
    );
    assert_eq!(
        resolve(pg, "SELECT d.$0total FROM (SELECT sum(total) AS total FROM orders) d"),
        "column d.total of item `sum(total) AS total`"
    );
    assert_eq!(
        resolve(
            pg,
            "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.$0id)"
        ),
        "column u.id of table users"
    );
    assert_eq!(resolve(pg, "SELECT * FROM users u, (SELECT $0u.id) d"), "unknown",);
    assert_eq!(
        resolve(pg, "SELECT * FROM users u, LATERAL (SELECT u.$0id) d"),
        "column u.id of table users"
    );
    assert_eq!(
        resolve(pg, "SELECT * FROM (VALUES (1, 2)) AS v (a, b) WHERE $0b > 1"),
        "column v.b declared"
    );
}

#[test]
fn select_aliases_follow_the_rules_of_each_dialect() {
    let text = |clause: &str| format!("SELECT org_id AS o FROM users {clause}");
    assert_eq!(
        resolve(Dialect::Postgres, &text("ORDER BY $0o")),
        "alias of `org_id AS o`"
    );
    assert_eq!(resolve(Dialect::Postgres, &text("ORDER BY $0o + 1")), "unknown");
    assert_eq!(
        resolve(Dialect::Mysql, &text("ORDER BY $0o + 1")),
        "alias of `org_id AS o`"
    );
    assert_eq!(
        resolve(Dialect::Postgres, &text("GROUP BY $0o")),
        "alias of `org_id AS o`"
    );
    assert_eq!(
        resolve(Dialect::Postgres, &text("GROUP BY o HAVING $0o > 1")),
        "unknown"
    );
    assert_eq!(
        resolve(Dialect::Mysql, &text("GROUP BY o HAVING $0o > 1")),
        "alias of `org_id AS o`"
    );
    assert_eq!(
        resolve(Dialect::Mariadb, &text("GROUP BY o HAVING $0o > 1")),
        "alias of `org_id AS o`"
    );
    assert_eq!(resolve(Dialect::Postgres, &text("WHERE $0o > 1")), "unknown");
    assert_eq!(
        resolve(Dialect::Sqlite, &text("WHERE $0o > 1")),
        "alias of `org_id AS o`"
    );
    assert_eq!(
        resolve(Dialect::Sqlite, "SELECT id + 1 AS id FROM users WHERE $0id = 1"),
        "column users.id of table users"
    );
    assert_eq!(
        resolve(Dialect::Postgres, "SELECT id + 1 AS id FROM users ORDER BY $0id"),
        "alias of `id + 1 AS id`"
    );
    assert_eq!(
        resolve(
            Dialect::Postgres,
            "SELECT email AS e FROM users UNION SELECT name FROM orgs ORDER BY $0e"
        ),
        "alias of `email AS e`"
    );
}

#[test]
fn statements_that_change_data() {
    let pg = Dialect::Postgres;
    assert_eq!(
        resolve(pg, "INSERT INTO users ($0email) VALUES ('a')"),
        "column users.email of table users"
    );
    assert_eq!(resolve(pg, "INSERT INTO users (nope$0) VALUES ('a')"), "unknown");
    assert_eq!(
        resolve(
            pg,
            "INSERT INTO users (email) VALUES ('a') ON CONFLICT (email) DO UPDATE SET name = excluded.$0name"
        ),
        "column excluded.name of table users"
    );
    assert_eq!(
        resolve(pg, "INSERT INTO users (email) VALUES ('a') RETURNING $0id"),
        "column users.id of table users"
    );
    assert_eq!(
        resolve(
            Dialect::Mysql,
            "INSERT INTO users (email) VALUES ('a') AS new ON DUPLICATE KEY UPDATE name = new.$0name"
        ),
        "column new.name of table users"
    );
    assert_eq!(
        resolve(
            pg,
            "UPDATE users u SET $0name = o.name FROM orgs o WHERE o.id = u.org_id"
        ),
        "column u.name of table users"
    );
    assert_eq!(
        resolve(
            pg,
            "UPDATE users u SET name = o.$0name FROM orgs o WHERE o.id = u.org_id"
        ),
        "column o.name of table orgs"
    );
    assert_eq!(
        resolve(pg, "DELETE FROM users WHERE $0status = 'x'"),
        "column users.status of table users"
    );
    assert_eq!(
        resolve(
            pg,
            "MERGE INTO users u USING orgs o ON u.org_id = o.id WHEN MATCHED THEN UPDATE SET name = o.$0name"
        ),
        "column o.name of table orgs"
    );
    assert_eq!(
        resolve(
            pg,
            "MERGE INTO users u USING orgs o ON u.org_id = o.id WHEN NOT MATCHED THEN INSERT ($0email) VALUES (o.name)"
        ),
        "column u.email of table users"
    );
    assert_eq!(
        resolve(Dialect::Mysql, "DELETE $0u FROM users u JOIN orgs o ON o.id = u.org_id"),
        "source u (table users)"
    );
}

#[test]
fn ddl_of_the_document_defines_what_follows() {
    let none = Schemas::NONE;
    let pg = Dialect::Postgres;
    assert_eq!(
        resolve_in(pg, none, "CREATE TABLE t (a int, b text); SELECT $0b FROM t;"),
        "column t.b of table t"
    );
    assert_eq!(
        resolve_in(pg, none, "SELECT $0b FROM t; CREATE TABLE t (a int, b text);"),
        "unknown, maybe defined elsewhere"
    );
    assert_eq!(
        resolve_in(pg, none, "CREATE TABLE t (a int, b text, CHECK ($0a > 0));"),
        "column t.a declared"
    );
    assert_eq!(
        resolve_in(pg, none, "CREATE TABLE t (a int, PRIMARY KEY ($0a));"),
        "column t.a declared"
    );
    assert_eq!(
        resolve_in(pg, none, "CREATE TABLE t (a int); CREATE INDEX t_a ON t ($0a);"),
        "column t.a of table t"
    );
    assert_eq!(
        resolve_in(
            pg,
            none,
            "CREATE TABLE p (id int); CREATE TABLE c (p_id int REFERENCES p ($0id));"
        ),
        "column p.id of table p"
    );
    assert_eq!(
        resolve_in(
            pg,
            none,
            "CREATE TABLE t (a int); ALTER TABLE t ADD COLUMN b int; SELECT $0b FROM t;"
        ),
        "column t.b of table t"
    );
    let layer = shop(pg);
    assert_eq!(
        resolve_in(
            pg,
            with_snapshot(&layer),
            "ALTER TABLE users ADD COLUMN age int; SELECT $0age FROM users;"
        ),
        "column users.age of table users"
    );
}

#[test]
fn triggers_routines_and_functions() {
    let mysql = Dialect::Mysql;
    assert_eq!(
        resolve(
            mysql,
            "CREATE TRIGGER t BEFORE INSERT ON users FOR EACH ROW SET NEW.$0email = LOWER(NEW.email);"
        ),
        "column new.email of table users"
    );
    assert_eq!(
        resolve(
            mysql,
            "CREATE PROCEDURE p(IN wanted INT) BEGIN DECLARE n INT; SELECT count(*) INTO n FROM users WHERE id = $0wanted; END"
        ),
        "variable wanted"
    );
    assert_eq!(resolve(mysql, "SELECT $0count(*) FROM users"), "function count");
    assert_eq!(resolve(mysql, "SELECT $0order_total(1)"), "routine order_total");
    assert_eq!(resolve(mysql, "SELECT $0no_such_fn(1)"), "unknown");
    assert_eq!(
        resolve(Dialect::Postgres, "SELECT $0coalesce(name, '') FROM users"),
        "function coalesce"
    );
}
