//! `cargo bench -p sql-embed` measures what a host pays per fragment against a snapshot of 5,000
//! tables: analyzing a query builder's condition and a statement of 50 lines, with their
//! diagnostics, completion and semantic tokens. Benchmarks are not part of `cargo test`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sql_embed::{
    Analysis, CompletionOptions, Dialect, Environment, EscapeStyle, Fragment, FragmentKind, HoleKind, ScopeTable,
    Settings, Snapshot, Span,
};

const TABLES: usize = 5_000;
const COLUMNS: usize = 20;

/// A snapshot of `TABLES` tables of `COLUMNS` columns, each table with a foreign key to the one
/// before it.
fn snapshot_json() -> String {
    let mut tables = Vec::with_capacity(TABLES);
    for table in 0..TABLES {
        let columns: Vec<String> = (0..COLUMNS)
            .map(|column| {
                let name = match column {
                    0 => "id".to_string(),
                    1 => "parent_id".to_string(),
                    _ => format!("column_{column}"),
                };
                format!(r#"{{ "name": "{name}", "type": "int", "nullable": false, "comment": "Column {column} of table {table}" }}"#)
            })
            .collect();
        let foreign_key = if table > 0 {
            format!(
                r#", "foreignKeys": [ {{ "columns": ["parent_id"], "referencedTable": "table_{}", "referencedColumns": ["id"] }} ]"#,
                table - 1
            )
        } else {
            String::new()
        };
        tables.push(format!(
            r#"{{ "name": "table_{table}", "comment": "Table {table}", "columns": [ {} ], "primaryKey": {{ "columns": ["id"] }}{foreign_key} }}"#,
            columns.join(", ")
        ));
    }
    format!(
        r#"{{ "formatVersion": 1, "defaultSchema": "app", "schemas": [ {{ "name": "app", "tables": [ {} ] }} ] }}"#,
        tables.join(", ")
    )
}

/// `->where('a.column_3 = ? AND (a.column_4 > ? OR a.column_5 IS NULL) AND a.')` on `table_10 a`,
/// at 100 in the host, with the cursor at its end.
fn condition() -> (Fragment, u32) {
    let text = "a.column_3 = ? AND (a.column_4 > ? OR a.column_5 IS NULL) AND a.";
    let mut fragment = Fragment::new(FragmentKind::Condition);
    fragment.literal(text, 100, EscapeStyle::SingleQuoted);
    fragment.table(ScopeTable::new("table_10").with_alias("a"));
    (fragment, 100 + text.len() as u32)
}

/// A heredoc of 50 lines: a report with common table expressions, joins and a subquery, with two
/// interpolated values, and the cursor in a column of the select list.
fn statement() -> (Fragment, u32) {
    let mut lines: Vec<String> = [
        "WITH recent AS (",
        "    SELECT t.id, t.parent_id, t.column_2, t.column_3",
        "    FROM table_20 t",
        "    WHERE t.column_4 > ",
        "),",
        "totals AS (",
        "    SELECT r.parent_id, count(*) AS n, sum(r.column_3) AS total",
        "    FROM recent r",
        "    GROUP BY r.parent_id",
        ")",
        "SELECT",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect();
    for column in 2..20 {
        lines.push(format!("    a.column_{column},"));
    }
    for line in [
        "    b.column_5,",
        "    totals.n,",
        "    totals.total",
        "FROM table_21 a",
        "JOIN table_22 b ON b.parent_id = a.id",
        "JOIN totals ON totals.parent_id = a.id",
        "LEFT JOIN table_23 c ON c.parent_id = b.id",
        "WHERE a.column_6 = ",
        "  AND b.column_7 IN (",
        "      SELECT d.column_8",
        "      FROM table_24 d",
        "      WHERE d.column_9 > 0",
        "  )",
        "  AND c.column_10 IS NULL",
    ] {
        lines.push(line.to_string());
    }
    while lines.len() < 49 {
        lines.push("  AND a.column_11 <> b.column_11".to_string());
    }
    lines.push("ORDER BY totals.total DESC".to_string());
    let mut fragment = Fragment::new(FragmentKind::Statements);
    let mut host = 1_000u32;
    let mut cursor = 0;
    for line in &lines {
        let text = format!("{line}\n");
        fragment.literal(&text, host, EscapeStyle::Heredoc);
        host += text.len() as u32;
        if line.ends_with("= ") || line.ends_with("> ") {
            fragment.hole(Span::new(host, host + 6), HoleKind::Value);
            host += 6;
        }
        if line == "    b.column_5," {
            cursor = host - 3;
        }
    }
    (fragment, cursor)
}

fn bench(criterion: &mut Criterion) {
    let snapshot = Snapshot::parse(&snapshot_json()).expect("reads");
    let settings = Settings {
        dialect: Dialect::Mysql,
        ..Settings::default()
    };
    let env = Environment::new(settings, Some(snapshot), None);
    let options = CompletionOptions::default();
    let (fragment, cursor) = condition();
    criterion.bench_function("a condition: analysis and diagnostics", |bencher| {
        bencher.iter(|| Analysis::new(&env, black_box(&fragment)).diagnostics())
    });
    let analysis = Analysis::new(&env, &fragment);
    assert_eq!(analysis.completion(cursor, options).items[0].label, "id");
    criterion.bench_function("a condition: completion after the alias", |bencher| {
        bencher.iter(|| analysis.completion(black_box(cursor), options))
    });
    let (fragment, cursor) = statement();
    criterion.bench_function("50 lines: analysis and diagnostics", |bencher| {
        bencher.iter(|| Analysis::new(&env, black_box(&fragment)).diagnostics())
    });
    let analysis = Analysis::new(&env, &fragment);
    assert_eq!(analysis.diagnostics(), [], "{}", analysis.sql());
    criterion.bench_function("50 lines: semantic tokens", |bencher| {
        bencher.iter(|| analysis.semantic_tokens())
    });
    criterion.bench_function("50 lines: completion of a column", |bencher| {
        bencher.iter(|| analysis.completion(black_box(cursor), options))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
