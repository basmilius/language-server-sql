//! `cargo bench -p sql-format` measures the formatter on the large synthetic script of the parser's
//! benchmark. Benchmarks are not part of `cargo test`.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use sql_format::{FormatOptions, format};
use sql_syntax::Dialect;

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

fn benchmarks(criterion: &mut Criterion) {
    let large = large_script(1000);
    let typical = large_script(25);
    let options = FormatOptions::default();
    assert!(
        format(&large, Dialect::Postgres, &options).is_some(),
        "the script formats"
    );
    let mut group = criterion.benchmark_group("format");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(large.len() as u64));
    group.bench_function("large script", |bencher| {
        bencher.iter(|| black_box(format(black_box(&large), Dialect::Postgres, &options)))
    });
    group.finish();
    criterion.bench_function("format a typical script", |bencher| {
        bencher.iter(|| black_box(format(black_box(&typical), Dialect::Postgres, &options)))
    });
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
