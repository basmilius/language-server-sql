//! `cargo bench -p sql-analysis` measures what a schema costs: reading a large snapshot,
//! completion, hover and the diagnostics of unknown names on it. Benchmarks are not part of
//! `cargo test`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sql_analysis::catalog::{Layer, Origin};
use sql_analysis::completion::{CompletionOptions, complete};
use sql_analysis::context::Schemas;
use sql_analysis::nav::hover;
use sql_analysis::unresolved::unresolved;
use sql_catalog::read_snapshot;
use sql_syntax::{Dialect, Target, parse};

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
                format!(r#"{{ "name": "{name}", "type": "integer", "nullable": false, "comment": "Column {column} of table {table}" }}"#)
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
        r#"{{ "formatVersion": 1, "defaultSchema": "public", "schemas": [ {{ "name": "public", "tables": [ {} ] }} ] }}"#,
        tables.join(", ")
    )
}

const QUERY: &str = "SELECT a.id, b.column_5, c.column_7\nFROM table_10 a\nJOIN table_11 b ON b.parent_id = a.id\nJOIN table_12 c ON c.parent_id = b.id\nWHERE a.column_3 > 1 AND b.column_4 IN (SELECT column_2 FROM table_13 WHERE column_9 = 2)\nORDER BY c.column_8;\n";

fn bench(criterion: &mut Criterion) {
    let json = snapshot_json();
    let target = Target::new(Dialect::Postgres, None);
    criterion.bench_function("read a snapshot of 5,000 tables", |bencher| {
        bencher.iter(|| Layer::new(Origin::Snapshot, read_snapshot(black_box(&json)).expect("reads")))
    });
    let layer = Layer::new(Origin::Snapshot, read_snapshot(&json).expect("reads"));
    let schemas = Schemas {
        snapshot: Some(&layer),
        workspace: None,
    };
    let options = CompletionOptions::default();
    let from = "SELECT * FROM ";
    criterion.bench_function("complete a table after FROM", |bencher| {
        bencher.iter(|| complete(black_box(from), from.len() as u32, target, schemas, options))
    });
    let typed = "SELECT * FROM table_12";
    criterion.bench_function("complete a table after FROM with a prefix", |bencher| {
        bencher.iter(|| complete(black_box(typed), typed.len() as u32, target, schemas, options))
    });
    let join = "SELECT * FROM table_10 a JOIN ";
    criterion.bench_function("complete a join with conditions from foreign keys", |bencher| {
        bencher.iter(|| complete(black_box(join), join.len() as u32, target, schemas, options))
    });
    let columns = "SELECT  FROM table_10 a JOIN table_11 b ON b.parent_id = a.id";
    criterion.bench_function("complete columns of two joined tables", |bencher| {
        bencher.iter(|| complete(black_box(columns), 7, target, schemas, options))
    });
    let script: String = QUERY.repeat(100);
    let root = parse(&script, Dialect::Postgres).syntax();
    criterion.bench_function("unknown names of 100 queries", |bencher| {
        bencher.iter(|| unresolved(black_box(&root), target, schemas))
    });
    let one = parse(QUERY, Dialect::Postgres).syntax();
    criterion.bench_function("hover a column", |bencher| {
        bencher.iter(|| hover(black_box(&one), 17, target, schemas))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
