//! `cargo bench -p sql-analysis --bench editing` measures what editing asks of a script: semantic
//! tokens, inlay hints and inspections of a large script, the highlights of a table in it, and the
//! references and rename of a table across a workspace of many files. Benchmarks are not part of `cargo test`.

use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use sql_analysis::context::Schemas;
use sql_analysis::inlay_hints::{HintOptions, inlay_hints};
use sql_analysis::inspections::{InspectionSettings, Request, inspect};
use sql_analysis::references::{Current, OtherFile, highlights, references};
use sql_analysis::rename::rename;
use sql_analysis::semantic_tokens::semantic_tokens;
use sql_analysis::workspace::{build, extract};
use sql_syntax::{Dialect, Target, parse};

const UNIT: &str = r#"
-- The orders of a customer, with what they bought.
CREATE TABLE IF NOT EXISTS orders_N (
    id bigint NOT NULL PRIMARY KEY,
    customer_id bigint NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    status varchar(20) NOT NULL DEFAULT 'new' CHECK (status IN ('new', 'paid', 'shipped')),
    total numeric(12, 2) NOT NULL DEFAULT 0,
    note text,
    created_at timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT orders_N_total CHECK (total >= 0)
);
CREATE INDEX orders_N_customer ON orders_N (customer_id, created_at DESC);
INSERT INTO orders_N (id, customer_id, status, total, note) VALUES
    (1, 10, 'new', 19.95, 'first'),
    (2, 11, 'paid', 5.00, NULL),
    (3, 12, 'shipped', 120.50, 'it''s here');
WITH recent AS (
    SELECT customer_id, sum(total) AS spent, count(*) AS orders
    FROM orders_N
    WHERE created_at > CURRENT_DATE - 30 AND status <> 'new'
    GROUP BY customer_id
    HAVING sum(total) > 100
)
SELECT c.id, c.name, r.spent, r.orders,
       rank() OVER (ORDER BY r.spent DESC) AS position,
       CASE WHEN r.spent > 1000 THEN 'gold' WHEN r.spent > 500 THEN 'silver' ELSE 'bronze' END AS tier
FROM customers AS c
JOIN recent AS r ON r.customer_id = c.id
LEFT JOIN addresses AS a ON a.customer_id = c.id AND a.kind = 'billing'
WHERE c.active = TRUE AND (c.country = 'NL' OR c.country IS NULL)
ORDER BY r.spent DESC, c.name
LIMIT 50 OFFSET 0;
UPDATE orders_N SET status = 'paid', total = total * 1.21 WHERE id IN (SELECT id FROM payments WHERE settled);
DELETE FROM orders_N WHERE created_at < CURRENT_DATE - 365 AND status = 'shipped';
ALTER TABLE orders_N ADD COLUMN shipped_at timestamp, DROP COLUMN note;
"#;

fn large_script(units: usize) -> String {
    let mut text = String::new();
    for index in 0..units {
        text.push_str(&UNIT.replace("_N", &format!("_{index}")));
    }
    text
}

/// How many files the workspace has: half migrations that create a table, half queries.
const FILES: usize = 2_000;

fn workspace_files() -> Vec<(PathBuf, String)> {
    let mut files = vec![(
        PathBuf::from("/w/migrations/0000_users.sql"),
        "CREATE TABLE users (\n    id bigint PRIMARY KEY,\n    email text NOT NULL,\n    name text\n);\n".to_string(),
    )];
    for index in 1..FILES / 2 {
        files.push((
            PathBuf::from(format!("/w/migrations/{index:04}_table_{index}.sql")),
            format!(
                "CREATE TABLE table_{index} (\n    id bigint PRIMARY KEY,\n    user_id bigint REFERENCES users (id),\n    note text\n);\nCREATE INDEX table_{index}_user ON table_{index} (user_id);\n"
            ),
        ));
    }
    for index in 0..FILES / 2 {
        files.push((
            PathBuf::from(format!("/w/queries/report_{index}.sql")),
            format!(
                "SELECT u.email, t.note\nFROM users AS u\nJOIN table_{} AS t ON t.user_id = u.id\nWHERE u.name IS NOT NULL;\nSELECT count(*) FROM table_{};\n",
                index + 1,
                index + 1
            ),
        ));
    }
    files
}

fn benchmarks(criterion: &mut Criterion) {
    let target = Target::new(Dialect::Postgres, None);
    let large = large_script(1000);
    let root = parse(&large, Dialect::Postgres).syntax();
    let mut group = criterion.benchmark_group("large script");
    group.sample_size(10);
    group.bench_function("semantic tokens", |bencher| {
        bencher.iter(|| black_box(semantic_tokens(black_box(&root), target, Schemas::NONE, None)))
    });
    group.bench_function("inlay hints", |bencher| {
        bencher.iter(|| {
            black_box(inlay_hints(
                black_box(&root),
                target,
                Schemas::NONE,
                None,
                HintOptions::default(),
            ))
        })
    });
    let at = large.rfind("orders_999 (customer_id").expect("the last index") as u32;
    group.bench_function("highlights of a table", |bencher| {
        bencher.iter(|| black_box(highlights(black_box(&root), at, target, Schemas::NONE)))
    });
    let settings = InspectionSettings::default();
    let request = Request::new(target, Schemas::NONE, &settings);
    group.bench_function("every inspection", |bencher| {
        bencher.iter(|| black_box(inspect(black_box(&root), &request)))
    });
    let fixes = Request { fixes: true, ..request };
    group.bench_function("every inspection with its fixes", |bencher| {
        bencher.iter(|| black_box(inspect(black_box(&root), &fixes)))
    });
    group.finish();

    let typical = large_script(25);
    let typical_root = parse(&typical, Dialect::Postgres).syntax();
    let screen = sql_syntax::TextRange::new(0.into(), 4000.into());
    let typical_request = Request::new(target, Schemas::NONE, &settings);
    criterion.bench_function("every inspection of a typical script", |bencher| {
        bencher.iter(|| black_box(inspect(black_box(&typical_root), &typical_request)))
    });
    criterion.bench_function("semantic tokens of a screen of a typical script", |bencher| {
        bencher.iter(|| {
            black_box(semantic_tokens(
                black_box(&typical_root),
                target,
                Schemas::NONE,
                Some(screen),
            ))
        })
    });

    let files = workspace_files();
    let extracted: Vec<_> = files
        .iter()
        .map(|(path, text)| (path.clone(), extract(text, Dialect::Postgres)))
        .collect();
    let layer = build(
        Dialect::Postgres,
        extracted.iter().map(|(path, ddl)| (Path::new(path), ddl)),
    );
    let schemas = Schemas {
        snapshot: None,
        workspace: Some(&layer),
    };
    let others: Vec<OtherFile> = files
        .iter()
        .map(|(path, text)| OtherFile {
            path,
            text,
            target,
            schemas,
        })
        .collect();
    let query = "SELECT email FROM users WHERE id = 1;\n";
    let query_root = parse(query, Dialect::Postgres).syntax();
    let current = Current {
        root: &query_root,
        target,
        schemas,
    };
    let mut group = criterion.benchmark_group("workspace of 2,000 files");
    group.sample_size(10);
    group.bench_function("references of a table", |bencher| {
        bencher.iter(|| black_box(references(&current, 19, true, black_box(&others))))
    });
    group.bench_function("references of a column", |bencher| {
        bencher.iter(|| black_box(references(&current, 8, true, black_box(&others))))
    });
    group.bench_function("rename a table", |bencher| {
        bencher.iter(|| black_box(rename(&current, 19, "people", black_box(&others))))
    });
    group.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
