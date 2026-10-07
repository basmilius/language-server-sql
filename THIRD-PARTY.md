# Third-party material

The Cargo workspace declares FSL-1.1-MIT. Rust dependencies retain their own licenses; Cargo.lock pins their versions and registry checksums. Native asset generation follows the server's runtime dependency graph for each target and includes those crates' upstream license and notice files under `third-party/`, together with a dependency manifest and Cargo.lock. Development-only dependencies are excluded.

The parser, the feature table and the analysis are written from scratch from the official documentation of SQLite, MySQL, MariaDB and PostgreSQL and from what their servers do with a statement. No code was taken from another parser, formatter or language server.

`crates/syntax/src/reserved_words.rs` lists words: the reserved keywords each server reports in its own catalog (MySQL's `information_schema.KEYWORDS`, PostgreSQL's `pg_get_keywords()`) or rejects as a column name (MariaDB, SQLite). `crates/syntax/tests/data/dialects-verified.txt` records which servers accepted each statement of the corpus. Both are written by scripts in `scripts/` and hold facts about the servers, not their code.

`crates/catalog/data/<dialect>.tsv` lists names, parameters and versions of the functions, types, system tables and settings each server reports or accepts, taken by `scripts/catalog.py`. The descriptions in `crates/catalog/data/descriptions.tsv` are written for this server, not taken from any documentation.

The servers those scripts run (the `mysql`, `mariadb`, `postgres` and `alpine` Docker images, the SQLite of Alpine's packages and the SQLite library of Python) are downloaded or installed separately, keep their own licenses and are not part of this repository, its builds or its release archives.
