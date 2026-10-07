//! `CREATE`, `ALTER` and `DROP` of tables, indexes, views, schemas, sequences, types, domains and
//! extensions, with the columns and constraints of a table, and `TRUNCATE`, `RENAME TABLE`,
//! `COMMENT ON` and `REFRESH MATERIALIZED VIEW`. Routines and triggers are in `routine.rs`.
//!
//! Options whose every detail differs per dialect (storage parameters, MySQL's table options, the
//! options of a sequence) are read as runs of words and values, so a dump of any dialect reads
//! without errors, at the price of not checking them.

use super::stmt::{end_statement, lenient};
use super::{Parser, expr, is_name_token, name, name_list, qualified_name, query, routine, types};
use crate::SyntaxKind::{self, *};

/// The object a `CREATE` makes and how many words stand before it: `OR REPLACE`, `TEMPORARY`,
/// `UNIQUE`, `DEFINER = ...` and the like.
fn create_object(p: &Parser) -> (SyntaxKind, usize) {
    let mut at = 1;
    while at < 24 {
        match p.nth(at) {
            OR_KW | REPLACE_KW | TEMP_KW | TEMPORARY_KW | UNLOGGED_KW | GLOBAL_KW | LOCAL_KW | UNIQUE_KW
            | FULLTEXT_KW | SPATIAL_KW | MATERIALIZED_KW | RECURSIVE_KW | CONSTRAINT_KW | SQL_KW | SECURITY_KW
            | INVOKER_KW => at += 1,
            DEFINER_KW | ALGORITHM_KW => {
                at += 1;
                if p.nth(at) == EQ {
                    at += 2;
                    if p.nth(at) == VARIABLE {
                        at += 1;
                    }
                    if p.nth(at) == LPAREN && p.nth(at + 1) == RPAREN {
                        at += 2;
                    }
                }
            }
            IDENT if p.nth_is_word(at, "aggregate") || p.nth_is_word(at, "trusted") => at += 1,
            kind => return (kind, at),
        }
    }
    (EOF, at)
}

pub(crate) fn create(p: &mut Parser) {
    let (object, prefix) = create_object(p);
    let kind = match object {
        TABLE_KW => CREATE_TABLE_STMT,
        INDEX_KW => CREATE_INDEX_STMT,
        VIEW_KW => CREATE_VIEW_STMT,
        SCHEMA_KW | DATABASE_KW => CREATE_SCHEMA_STMT,
        SEQUENCE_KW => CREATE_SEQUENCE_STMT,
        TYPE_KW => CREATE_TYPE_STMT,
        DOMAIN_KW => CREATE_DOMAIN_STMT,
        EXTENSION_KW => CREATE_EXTENSION_STMT,
        FUNCTION_KW | PROCEDURE_KW => CREATE_FUNCTION_STMT,
        TRIGGER_KW => CREATE_TRIGGER_STMT,
        _ => {
            lenient(p, CREATE_STMT);
            return;
        }
    };
    p.start(kind);
    for _ in 0..=prefix {
        p.bump();
    }
    match kind {
        CREATE_TABLE_STMT => create_table(p),
        CREATE_INDEX_STMT => create_index(p),
        CREATE_VIEW_STMT => create_view(p),
        CREATE_SCHEMA_STMT => create_schema(p),
        CREATE_TYPE_STMT => create_type(p),
        CREATE_DOMAIN_STMT => create_domain(p),
        CREATE_FUNCTION_STMT => routine::create_function(p),
        CREATE_TRIGGER_STMT => routine::create_trigger(p),
        _ => {
            if_not_exists(p);
            qualified_name(p, "Name");
            p.bump_until_statement_end();
        }
    }
    end_statement(p);
    p.finish_node();
}

pub(crate) fn if_not_exists(p: &mut Parser) {
    if p.at(IF_KW) && p.nth(1) == NOT_KW && p.nth(2) == EXISTS_KW {
        p.bump();
        p.bump();
        p.bump();
    }
}

fn if_exists(p: &mut Parser) {
    if p.at(IF_KW) && p.nth(1) == EXISTS_KW {
        p.bump();
        p.bump();
    }
}

fn create_table(p: &mut Parser) {
    if_not_exists(p);
    qualified_name(p, "Table name");
    match p.current() {
        LPAREN if query::at_query_start(p, 0) => {
            query::query(p);
        }
        LPAREN => table_element_list(p),
        LIKE_KW => {
            p.start(LIKE_CLAUSE);
            p.bump();
            qualified_name(p, "Table name");
            p.finish_node();
        }
        PARTITION_KW if p.nth(1) == OF_KW => {
            p.bump();
            p.bump();
            qualified_name(p, "Table name");
            if p.at(LPAREN) {
                table_element_list(p);
            }
            partition_bound(p);
        }
        _ => {}
    }
    table_options(p);
    if p.at(PARTITION_KW) && p.nth(1) == BY_KW {
        partition_by(p);
    }
    table_options(p);
    let _ = p.eat(IGNORE_KW) || p.eat(REPLACE_KW);
    let explicit = p.eat(AS_KW);
    if query::at_query_start(p, 0) || explicit && p.at(LPAREN) {
        query::query(p);
        if p.at(WITH_KW) && matches!(p.nth(1), DATA_KW | NO_KW) {
            p.bump();
            p.eat(NO_KW);
            p.expect_kw(DATA_KW);
        }
    } else if explicit {
        p.error_expected("Query");
    }
}

/// PostgreSQL's `FOR VALUES IN (...) | FROM (...) TO (...) | WITH (...)` or `DEFAULT`.
fn partition_bound(p: &mut Parser) {
    if p.eat(DEFAULT_KW) {
        return;
    }
    if !p.eat(FOR_KW) {
        p.error_expected("FOR VALUES");
        return;
    }
    p.expect_kw(VALUES_KW);
    while matches!(p.current(), IN_KW | FROM_KW | TO_KW | WITH_KW) {
        p.bump();
        p.bump_balanced();
    }
}

/// Whether the options after a table's columns end here.
fn at_table_options_end(p: &Parser) -> bool {
    p.at_statement_end()
        || matches!(p.current(), AS_KW | IGNORE_KW | REPLACE_KW | RPAREN)
        || (p.at(PARTITION_KW) && p.nth(1) == BY_KW)
        || (query::at_query_start(p, 0) && !(p.at(WITH_KW) && p.nth(1) == LPAREN))
}

fn table_options(p: &mut Parser) {
    while !at_table_options_end(p) {
        if p.eat(COMMA) {
            continue;
        }
        table_option(p);
    }
}

/// One option of a table: `ENGINE = InnoDB`, `DEFAULT CHARSET = utf8mb4`, `COMMENT 'x'`,
/// `WITHOUT ROWID`, `STRICT`, `INHERITS (...)`, `WITH (...)`, `TABLESPACE x`, `ON COMMIT ...`.
fn table_option(p: &mut Parser) {
    p.start(TABLE_OPTION);
    match p.current() {
        WITHOUT_KW => {
            p.bump();
            p.bump();
        }
        INHERITS_KW | WITH_KW if p.nth(1) == LPAREN => {
            p.bump();
            p.bump_balanced();
        }
        ON_KW => {
            p.bump();
            p.bump();
            while !at_table_options_end(p) && !p.at(COMMA) && is_name_token(p.current()) {
                p.bump();
            }
        }
        STRICT_KW => p.bump(),
        kind if is_name_token(kind) => {
            p.eat(DEFAULT_KW);
            p.bump();
            if (p.at(SET_KW) || is_name_token(p.current())) && matches!(p.nth(1), EQ | STRING | IDENT) && !p.at(EQ) {
                p.bump();
            }
            p.eat(EQ);
            if p.at(LPAREN) {
                p.bump_balanced();
            } else if !at_table_options_end(p) && !p.at(COMMA) {
                p.bump();
            }
        }
        _ => p.error_bump(),
    }
    p.finish_node();
}

/// `PARTITION BY RANGE | LIST | HASH | KEY [COLUMNS] (...) [PARTITIONS n] [SUBPARTITION ...]
/// [(PARTITION ..., ...)]`.
fn partition_by(p: &mut Parser) {
    p.start(TABLE_PARTITION_CLAUSE);
    p.bump();
    p.bump();
    if p.at_word("linear") {
        p.bump();
    }
    if is_name_token(p.current()) {
        p.bump();
    } else {
        p.error_expected("RANGE, LIST, HASH or KEY");
    }
    p.eat(COLUMNS_KW);
    if p.at(LPAREN) {
        index_column_list(p);
    }
    while !p.at_statement_end() && !p.at(LPAREN) && !p.at(AS_KW) && !query::at_query_start(p, 0) {
        if p.at(LPAREN) {
            p.bump_balanced();
        } else if is_name_token(p.current()) || p.at(INT_NUMBER) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(LPAREN) {
        p.bump();
        loop {
            p.start(PARTITION_DEF);
            if p.at(PARTITION_KW) || p.at_word("subpartition") {
                p.bump();
            } else {
                p.error_expected("PARTITION");
            }
            name(p, "Partition name");
            let mut depth = 0u32;
            while !p.at_statement_end() {
                match p.current() {
                    LPAREN => depth += 1,
                    RPAREN if depth == 0 => break,
                    RPAREN => depth -= 1,
                    COMMA if depth == 0 => break,
                    _ => {}
                }
                p.bump();
            }
            p.finish_node();
            if !p.eat(COMMA) {
                break;
            }
        }
        p.expect(RPAREN, "')'");
    }
    p.finish_node();
}

/// Whether a table constraint, not a column, starts the element at the cursor.
fn at_table_constraint(p: &Parser) -> bool {
    match p.current() {
        CONSTRAINT_KW | FOREIGN_KW | FULLTEXT_KW | SPATIAL_KW => true,
        PRIMARY_KW => p.nth(1) == KEY_KW,
        CHECK_KW | EXCLUDE_KW => matches!(p.nth(1), LPAREN | USING_KW),
        UNIQUE_KW => matches!(p.nth(1), LPAREN | KEY_KW | INDEX_KW | NULLS_KW | USING_KW) || p.nth(2) == LPAREN,
        INDEX_KW | KEY_KW => p.nth(1) == LPAREN || (is_name_token(p.nth(1)) && matches!(p.nth(2), LPAREN | USING_KW)),
        _ => false,
    }
}

/// `(column or constraint, ...)` of a table, of a composite type or of a function's columns.
pub(crate) fn table_element_list(p: &mut Parser) {
    p.start(TABLE_ELEMENT_LIST);
    p.expect(LPAREN, "'('");
    if !p.at(RPAREN) {
        loop {
            if at_table_constraint(p) {
                table_constraint(p);
            } else if p.at(LIKE_KW) {
                p.start(LIKE_CLAUSE);
                p.bump();
                qualified_name(p, "Table name");
                while p.at(INCLUDING_KW) || p.at(EXCLUDING_KW) {
                    p.bump();
                    if is_name_token(p.current()) {
                        p.bump();
                    }
                }
                p.finish_node();
            } else if p.at_word("period") && p.nth(1) == FOR_KW {
                p.start(TABLE_CONSTRAINT);
                p.bump();
                p.bump();
                name(p, "Period name");
                p.bump_balanced();
                p.finish_node();
            } else {
                column_def(p);
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

/// Words that start a column constraint, after which a column has no type.
fn at_column_constraint(p: &Parser) -> bool {
    matches!(
        p.current(),
        CONSTRAINT_KW
            | NOT_KW
            | NULL_KW
            | PRIMARY_KW
            | UNIQUE_KW
            | CHECK_KW
            | DEFAULT_KW
            | REFERENCES_KW
            | COLLATE_KW
            | GENERATED_KW
            | AS_KW
            | AUTO_INCREMENT_KW
            | AUTOINCREMENT_KW
            | COMMENT_KW
    )
}

/// `name [type] [constraint ...]`. SQLite allows a column without a type.
pub(crate) fn column_def(p: &mut Parser) {
    p.start(COLUMN_DEF);
    name(p, "Column name");
    if !at_column_constraint(p) && !p.at(COMMA) && !p.at(RPAREN) && !p.at_statement_end() {
        types::type_name(p);
    }
    column_constraints(p);
    p.finish_node();
}

fn column_constraints(p: &mut Parser) {
    loop {
        if p.at(COMMA) || p.at(RPAREN) || p.at_statement_end() || matches!(p.current(), FIRST_KW | AFTER_KW) {
            break;
        }
        if !column_constraint(p) {
            break;
        }
    }
}

/// SQLite's `ON CONFLICT ROLLBACK | ABORT | FAIL | IGNORE | REPLACE` after a constraint.
fn conflict_clause(p: &mut Parser) {
    if p.at(ON_KW) && p.nth(1) == CONFLICT_KW {
        p.bump();
        p.bump();
        if matches!(p.current(), ROLLBACK_KW | ABORT_KW | FAIL_KW | IGNORE_KW | REPLACE_KW) {
            p.bump();
        } else {
            p.error_expected("ROLLBACK, ABORT, FAIL, IGNORE or REPLACE");
        }
    }
}

/// `[NOT] DEFERRABLE`, `INITIALLY DEFERRED | IMMEDIATE`, `[NOT] ENFORCED`, `NOT VALID`, `NO INHERIT`.
fn constraint_attributes(p: &mut Parser) {
    loop {
        match p.current() {
            DEFERRABLE_KW | ENFORCED_KW => p.bump(),
            NOT_KW if matches!(p.nth(1), DEFERRABLE_KW | ENFORCED_KW) => {
                p.bump();
                p.bump();
            }
            NOT_KW if p.nth_is_word(1, "valid") => {
                p.bump();
                p.bump();
            }
            NO_KW if p.nth_is_word(1, "inherit") => {
                p.bump();
                p.bump();
            }
            INITIALLY_KW => {
                p.bump();
                if !p.eat(DEFERRED_KW) && !p.eat(IMMEDIATE_KW) {
                    p.error_expected("DEFERRED or IMMEDIATE");
                }
            }
            _ => break,
        }
    }
}

/// One constraint or attribute of a column. Gives whether it found one.
fn column_constraint(p: &mut Parser) -> bool {
    p.start(COLUMN_CONSTRAINT);
    if p.eat(CONSTRAINT_KW) && is_name_token(p.current()) && !at_column_constraint(p) {
        name(p, "Constraint name");
    }
    let found = match p.current() {
        NOT_KW if p.nth(1) == NULL_KW => {
            p.bump();
            p.bump();
            conflict_clause(p);
            true
        }
        NULL_KW => {
            p.bump();
            true
        }
        PRIMARY_KW => {
            p.bump();
            p.expect_kw(KEY_KW);
            let _ = p.eat(ASC_KW) || p.eat(DESC_KW);
            conflict_clause(p);
            p.eat(AUTOINCREMENT_KW);
            true
        }
        KEY_KW => {
            p.bump();
            true
        }
        UNIQUE_KW => {
            p.bump();
            p.eat(KEY_KW);
            nulls_distinct(p);
            conflict_clause(p);
            true
        }
        CHECK_KW => {
            p.bump();
            parenthesized_expr(p);
            true
        }
        DEFAULT_KW => {
            p.bump();
            expr::expr_bp(p, expr::DEFAULT_BP);
            true
        }
        COLLATE_KW => {
            p.bump();
            collation_name(p);
            true
        }
        REFERENCES_KW => {
            references(p);
            true
        }
        GENERATED_KW => {
            p.bump();
            if p.eat(BY_KW) {
                p.expect_kw(DEFAULT_KW);
            } else {
                p.expect_kw(ALWAYS_KW);
            }
            p.expect_kw(AS_KW);
            if p.eat(IDENTITY_KW) {
                if p.at(LPAREN) {
                    p.bump_balanced();
                }
            } else {
                parenthesized_expr(p);
                generated_kind(p);
            }
            true
        }
        AS_KW => {
            p.bump();
            parenthesized_expr(p);
            generated_kind(p);
            true
        }
        AUTO_INCREMENT_KW | AUTOINCREMENT_KW | VISIBLE_KW | INVISIBLE_KW => {
            p.bump();
            true
        }
        COMMENT_KW => {
            p.bump();
            expr::expr_bp(p, expr::DEFAULT_BP);
            true
        }
        ON_KW if p.nth(1) == UPDATE_KW => {
            p.bump();
            p.bump();
            expr::expr_bp(p, expr::DEFAULT_BP);
            true
        }
        CHARACTER_KW if p.nth(1) == SET_KW => {
            p.bump();
            p.bump();
            qualified_name(p, "Character set");
            true
        }
        CHARSET_KW => {
            p.bump();
            qualified_name(p, "Character set");
            true
        }
        STORAGE_KW => {
            p.bump();
            if is_name_token(p.current()) {
                p.bump();
            }
            true
        }
        IDENT if p.at_word("column_format") || p.at_word("srid") || p.at_word("compression") => {
            p.bump();
            if !p.at(COMMA) && !p.at(RPAREN) {
                p.bump();
            }
            true
        }
        IDENT if p.at_word("engine_attribute") || p.at_word("secondary_engine_attribute") => {
            p.bump();
            p.eat(EQ);
            p.bump();
            true
        }
        DEFERRABLE_KW | INITIALLY_KW | ENFORCED_KW => true,
        NOT_KW if matches!(p.nth(1), DEFERRABLE_KW | ENFORCED_KW) => true,
        NOT_KW => {
            p.bump();
            p.error_expected("NULL");
            true
        }
        _ => false,
    };
    if found {
        constraint_attributes(p);
    } else {
        let message = format!("Unexpected '{}'", p.current_text().escape_debug());
        p.error_here(message);
        p.start(ERROR);
        p.bump();
        p.finish_node();
    }
    p.finish_node();
    found || !p.at_statement_end()
}

fn generated_kind(p: &mut Parser) {
    if p.at(STORED_KW) || p.at(VIRTUAL_KW) || p.at_word("persistent") {
        p.bump();
    }
}

fn nulls_distinct(p: &mut Parser) {
    if p.at(NULLS_KW) {
        p.bump();
        p.eat(NOT_KW);
        p.expect_kw(DISTINCT_KW);
    }
}

fn collation_name(p: &mut Parser) {
    if p.current().is_string() {
        p.start(NAME);
        p.bump();
        p.finish_node();
    } else {
        qualified_name(p, "Collation");
    }
}

fn parenthesized_expr(p: &mut Parser) {
    if p.expect(LPAREN, "'('") {
        expr::expr(p);
        p.expect(RPAREN, "')'");
    }
}

/// `REFERENCES table [(columns)] [MATCH ...] [ON DELETE | UPDATE action] ...`.
fn references(p: &mut Parser) {
    p.start(REFERENCES_CLAUSE);
    p.bump();
    qualified_name(p, "Table name");
    if p.at(LPAREN) {
        name_list(p);
    }
    loop {
        if p.at(MATCH_KW) {
            p.bump();
            if p.at(FULL_KW) || is_name_token(p.current()) {
                p.bump();
            }
        } else if p.at(ON_KW) && matches!(p.nth(1), DELETE_KW | UPDATE_KW) {
            p.bump();
            p.bump();
            match p.current() {
                CASCADE_KW | RESTRICT_KW => p.bump(),
                SET_KW => {
                    p.bump();
                    if !p.eat(NULL_KW) && !p.eat(DEFAULT_KW) {
                        p.error_expected("NULL or DEFAULT");
                    }
                    if p.at(LPAREN) {
                        name_list(p);
                    }
                }
                NO_KW => {
                    p.bump();
                    p.expect_kw(ACTION_KW);
                }
                _ => p.error_expected("CASCADE, RESTRICT, SET NULL, SET DEFAULT or NO ACTION"),
            }
        } else {
            break;
        }
    }
    constraint_attributes(p);
    p.finish_node();
}

/// A constraint of a table, or one of MySQL's indexes in a table.
fn table_constraint(p: &mut Parser) {
    p.start(TABLE_CONSTRAINT);
    if p.eat(CONSTRAINT_KW) && is_name_token(p.current()) && !at_constraint_word(p) {
        name(p, "Constraint name");
    }
    match p.current() {
        PRIMARY_KW => {
            p.bump();
            p.expect_kw(KEY_KW);
            index_tail(p);
        }
        UNIQUE_KW => {
            p.bump();
            let _ = p.eat(INDEX_KW) || p.eat(KEY_KW);
            if is_name_token(p.current()) && !matches!(p.current(), USING_KW | NULLS_KW) {
                name(p, "Index name");
            }
            nulls_distinct(p);
            index_tail(p);
        }
        FOREIGN_KW => {
            p.bump();
            p.expect_kw(KEY_KW);
            if is_name_token(p.current()) {
                name(p, "Index name");
            }
            name_list(p);
            if p.at(REFERENCES_KW) {
                references(p);
            } else {
                p.error_expected("REFERENCES");
            }
        }
        CHECK_KW => {
            p.bump();
            parenthesized_expr(p);
        }
        EXCLUDE_KW => {
            p.bump();
            if p.eat(USING_KW) {
                name(p, "Index method");
            }
            index_column_list(p);
            index_options(p);
        }
        INDEX_KW | KEY_KW | FULLTEXT_KW | SPATIAL_KW => {
            let first = p.current();
            p.bump();
            if matches!(first, FULLTEXT_KW | SPATIAL_KW) {
                let _ = p.eat(INDEX_KW) || p.eat(KEY_KW);
            }
            if is_name_token(p.current()) && !p.at(USING_KW) {
                name(p, "Index name");
            }
            index_tail(p);
        }
        _ => p.error_expected("Constraint"),
    }
    constraint_attributes(p);
    conflict_clause(p);
    p.finish_node();
}

fn at_constraint_word(p: &Parser) -> bool {
    matches!(
        p.current(),
        PRIMARY_KW | UNIQUE_KW | FOREIGN_KW | CHECK_KW | EXCLUDE_KW | INDEX_KW | KEY_KW
    )
}

/// `[USING method] (columns) [options]` of an index.
fn index_tail(p: &mut Parser) {
    if p.eat(USING_KW) {
        name(p, "Index method");
    }
    index_column_list(p);
    index_options(p);
}

/// What may follow the columns of an index: `INCLUDE (...)`, `WITH (...)`, `USING INDEX
/// TABLESPACE x`, `USING BTREE`, `COMMENT 'x'`, `VISIBLE`, `KEY_BLOCK_SIZE = n`.
fn index_options(p: &mut Parser) {
    loop {
        match p.current() {
            INCLUDE_KW if p.nth(1) == LPAREN => {
                p.bump();
                name_list(p);
            }
            WITH_KW if p.nth(1) == LPAREN => {
                p.bump();
                p.bump_balanced();
            }
            USING_KW if p.nth(1) == INDEX_KW => {
                p.bump();
                p.bump();
                if p.at_word("tablespace") {
                    p.bump();
                    name(p, "Tablespace");
                }
            }
            USING_KW => {
                p.bump();
                name(p, "Index method");
            }
            COMMENT_KW => {
                p.bump();
                expr::expr_bp(p, expr::DEFAULT_BP);
            }
            VISIBLE_KW | INVISIBLE_KW => p.bump(),
            IDENT if p.at_word("key_block_size") || p.at_word("with_parser") => {
                p.bump();
                p.eat(EQ);
                p.bump();
            }
            _ => break,
        }
    }
}

/// `(column, ...)` of an index or a key, each an expression with a collation, an operator class,
/// a direction and the place of nulls; MySQL's `name(10)` is a prefix of the column.
pub(crate) fn index_column_list(p: &mut Parser) {
    p.start(INDEX_COLUMN_LIST);
    p.expect(LPAREN, "'('");
    if !p.at(RPAREN) {
        loop {
            index_column(p);
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

fn index_column(p: &mut Parser) {
    p.start(INDEX_COLUMN);
    if is_name_token(p.current()) && p.nth(1) == LPAREN && p.nth(2) == INT_NUMBER && p.nth(3) == RPAREN {
        expr::column_ref(p);
        p.bump();
        p.bump();
        p.bump();
    } else {
        expr::expr_bp(p, expr::DEFAULT_BP);
    }
    if p.eat(COLLATE_KW) {
        collation_name(p);
    }
    if p.at(IDENT) && !p.at_word("nulls") {
        qualified_name(p, "Operator class");
        if p.at(LPAREN) {
            p.bump_balanced();
        }
    }
    let _ = p.eat(ASC_KW) || p.eat(DESC_KW);
    if p.eat(NULLS_KW) && !p.eat(FIRST_KW) && !p.eat(LAST_KW) {
        p.error_expected("FIRST or LAST");
    }
    if p.eat(WITH_KW) {
        p.bump();
    }
    p.finish_node();
}

fn create_index(p: &mut Parser) {
    p.eat(CONCURRENTLY_KW);
    if_not_exists(p);
    if is_name_token(p.current()) && !p.at(ON_KW) && !p.at(USING_KW) {
        qualified_name(p, "Index name");
    }
    if p.eat(USING_KW) {
        name(p, "Index method");
    }
    if p.expect_kw(ON_KW) {
        p.eat(ONLY_KW);
        qualified_name(p, "Table name");
    }
    if p.eat(USING_KW) {
        name(p, "Index method");
    }
    index_column_list(p);
    loop {
        match p.current() {
            NULLS_KW => nulls_distinct(p),
            WHERE_KW => query::where_clause(p),
            ALGORITHM_KW | LOCK_KW => {
                p.bump();
                p.eat(EQ);
                p.bump();
            }
            IDENT if p.at_word("tablespace") => {
                p.bump();
                name(p, "Tablespace");
            }
            _ => {
                let before = p.position();
                index_options(p);
                if p.position() == before {
                    break;
                }
            }
        }
    }
}

fn create_view(p: &mut Parser) {
    if_not_exists(p);
    qualified_name(p, "View name");
    if p.at(LPAREN) {
        name_list(p);
    }
    while !p.at(AS_KW) && !p.at_statement_end() {
        if p.at(LPAREN) {
            p.bump_balanced();
        } else {
            p.bump();
        }
    }
    if p.expect_kw(AS_KW) {
        query::query(p);
    }
    if p.at(WITH_KW) {
        p.bump();
        if p.at(DATA_KW) || p.at(NO_KW) {
            p.eat(NO_KW);
            p.expect_kw(DATA_KW);
        } else {
            let _ = p.eat(CASCADED_KW) || p.eat(LOCAL_KW);
            p.expect_kw(CHECK_KW);
            p.expect_kw(OPTION_KW);
        }
    }
}

fn create_schema(p: &mut Parser) {
    if_not_exists(p);
    if is_name_token(p.current()) && !p.at(AUTHORIZATION_KW) {
        name(p, "Name");
    }
    p.bump_until_statement_end();
}

fn create_type(p: &mut Parser) {
    if_not_exists(p);
    qualified_name(p, "Type name");
    if p.eat(AS_KW) {
        if p.at_word("enum") {
            p.bump();
            p.start(ENUM_VALUE_LIST);
            if p.expect(LPAREN, "'('") {
                if !p.at(RPAREN) {
                    loop {
                        if p.current().is_string() {
                            p.start(LITERAL);
                            p.bump();
                            p.finish_node();
                        } else {
                            p.error_expected("String");
                        }
                        if !p.eat(COMMA) {
                            break;
                        }
                    }
                }
                p.expect(RPAREN, "')'");
            }
            p.finish_node();
        } else if p.at(RANGE_KW) {
            p.bump();
            p.bump_balanced();
        } else if p.at(LPAREN) {
            table_element_list(p);
        } else {
            p.error_expected("ENUM, RANGE or '('");
        }
    } else if p.at(LPAREN) {
        p.bump_balanced();
    }
}

fn create_domain(p: &mut Parser) {
    qualified_name(p, "Domain name");
    p.eat(AS_KW);
    types::type_name(p);
    column_constraints(p);
}

/// `ALTER TABLE` with its actions; other objects are read leniently.
pub(crate) fn alter(p: &mut Parser) {
    let mut at = 1;
    while p.nth(at) == IGNORE_KW || p.nth_is_word(at, "online") {
        at += 1;
    }
    if p.nth(at) != TABLE_KW {
        lenient(p, ALTER_STMT);
        return;
    }
    p.start(ALTER_TABLE_STMT);
    for _ in 0..=at {
        p.bump();
    }
    if_exists(p);
    p.eat(ONLY_KW);
    qualified_name(p, "Table name");
    p.eat(STAR);
    if !p.at_statement_end() {
        loop {
            alter_action(p);
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    end_statement(p);
    p.finish_node();
}

fn column_position(p: &mut Parser) {
    if p.eat(FIRST_KW) {
        return;
    }
    if p.eat(AFTER_KW) {
        name(p, "Column name");
    }
}

fn alter_action(p: &mut Parser) {
    match p.current() {
        ADD_KW => {
            let constraint = at_table_constraint_after(p, 1);
            if constraint {
                p.start(ADD_CONSTRAINT_ACTION);
                p.bump();
                table_constraint(p);
                if p.at(USING_KW) && p.nth(1) == INDEX_KW {
                    p.bump();
                    p.bump();
                    name(p, "Index name");
                }
                p.finish_node();
            } else if p.nth(1) == PARTITION_KW {
                generic_action(p);
            } else {
                p.start(ADD_COLUMN_ACTION);
                p.bump();
                p.eat(COLUMN_KW);
                if_not_exists(p);
                if p.at(LPAREN) {
                    table_element_list(p);
                } else {
                    column_def(p);
                    column_position(p);
                }
                p.finish_node();
            }
        }
        DROP_KW => match p.nth(1) {
            CONSTRAINT_KW | PRIMARY_KW | FOREIGN_KW | INDEX_KW | KEY_KW | CHECK_KW => {
                p.start(DROP_CONSTRAINT_ACTION);
                p.bump();
                let word = p.current();
                p.bump();
                if matches!(word, PRIMARY_KW | FOREIGN_KW) {
                    p.expect_kw(KEY_KW);
                }
                if_exists(p);
                if word != PRIMARY_KW {
                    name(p, "Name");
                }
                let _ = p.eat(CASCADE_KW) || p.eat(RESTRICT_KW);
                p.finish_node();
            }
            PARTITION_KW | DEFAULT_KW => generic_action(p),
            _ => {
                p.start(DROP_COLUMN_ACTION);
                p.bump();
                p.eat(COLUMN_KW);
                if_exists(p);
                name(p, "Column name");
                let _ = p.eat(CASCADE_KW) || p.eat(RESTRICT_KW);
                p.finish_node();
            }
        },
        ALTER_KW if !matches!(p.nth(1), INDEX_KW | CONSTRAINT_KW | CHECK_KW) => {
            p.start(ALTER_COLUMN_ACTION);
            p.bump();
            p.eat(COLUMN_KW);
            name(p, "Column name");
            alter_column_rest(p);
            p.finish_node();
        }
        MODIFY_KW => {
            p.start(MODIFY_COLUMN_ACTION);
            p.bump();
            p.eat(COLUMN_KW);
            column_def(p);
            column_position(p);
            p.finish_node();
        }
        CHANGE_KW => {
            p.start(MODIFY_COLUMN_ACTION);
            p.bump();
            p.eat(COLUMN_KW);
            name(p, "Column name");
            column_def(p);
            column_position(p);
            p.finish_node();
        }
        RENAME_KW => match p.nth(1) {
            TO_KW | AS_KW => {
                p.start(RENAME_TABLE_ACTION);
                p.bump();
                p.bump();
                qualified_name(p, "Table name");
                p.finish_node();
            }
            INDEX_KW | KEY_KW | CONSTRAINT_KW => generic_action(p),
            _ if p.nth(1) == COLUMN_KW || p.nth(2) == TO_KW => {
                p.start(RENAME_COLUMN_ACTION);
                p.bump();
                p.eat(COLUMN_KW);
                name(p, "Column name");
                p.expect_kw(TO_KW);
                name(p, "Column name");
                p.finish_node();
            }
            _ => {
                p.start(RENAME_TABLE_ACTION);
                p.bump();
                qualified_name(p, "Table name");
                p.finish_node();
            }
        },
        _ => generic_action(p),
    }
}

fn at_table_constraint_after(p: &Parser, offset: usize) -> bool {
    match p.nth(offset) {
        CONSTRAINT_KW | FOREIGN_KW | FULLTEXT_KW | SPATIAL_KW | EXCLUDE_KW => true,
        PRIMARY_KW => p.nth(offset + 1) == KEY_KW,
        CHECK_KW => p.nth(offset + 1) == LPAREN,
        UNIQUE_KW => true,
        INDEX_KW | KEY_KW => {
            p.nth(offset + 1) == LPAREN
                || (is_name_token(p.nth(offset + 1)) && matches!(p.nth(offset + 2), LPAREN | USING_KW))
        }
        _ => false,
    }
}

/// `TYPE | SET DATA TYPE type [USING expr]`, `SET DEFAULT x`, `DROP DEFAULT`, `SET | DROP NOT NULL`
/// and the rest, which is read leniently.
fn alter_column_rest(p: &mut Parser) {
    match p.current() {
        TYPE_KW => {
            p.bump();
            types::type_name(p);
            if p.eat(COLLATE_KW) {
                collation_name(p);
            }
            if p.eat(USING_KW) {
                expr::expr(p);
            }
        }
        SET_KW if p.nth(1) == DATA_KW => {
            p.bump();
            p.bump();
            p.expect_kw(TYPE_KW);
            types::type_name(p);
            if p.eat(COLLATE_KW) {
                collation_name(p);
            }
            if p.eat(USING_KW) {
                expr::expr(p);
            }
        }
        SET_KW if p.nth(1) == DEFAULT_KW => {
            p.bump();
            p.bump();
            expr::expr(p);
        }
        _ => action_rest(p),
    }
}

fn generic_action(p: &mut Parser) {
    p.start(ALTER_TABLE_ACTION);
    p.bump();
    action_rest(p);
    p.finish_node();
}

fn action_rest(p: &mut Parser) {
    let mut depth = 0u32;
    while !p.at_statement_end() {
        match p.current() {
            LPAREN => depth += 1,
            RPAREN if depth == 0 => break,
            RPAREN => depth -= 1,
            COMMA if depth == 0 => break,
            _ => {}
        }
        p.bump();
    }
}

/// `DROP kind [CONCURRENTLY] [IF EXISTS] name [(arguments)], ... [ON table] [CASCADE | RESTRICT]`.
pub(crate) fn drop(p: &mut Parser) {
    p.start(DROP_STMT);
    p.bump();
    let _ = p.eat(TEMPORARY_KW) || p.eat(TEMP_KW);
    if p.at_statement_end() {
        p.error_expected("Object kind");
    } else {
        let first = p.current();
        p.bump();
        if matches!(first, MATERIALIZED_KW | FOREIGN_KW) || p.at(VIEW_KW) && first != VIEW_KW {
            p.bump();
        }
    }
    p.eat(CONCURRENTLY_KW);
    if_exists(p);
    loop {
        if !qualified_name(p, "Name") {
            break;
        }
        if p.at(LPAREN) {
            p.bump_balanced();
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    if p.eat(ON_KW) {
        qualified_name(p, "Table name");
    }
    p.bump_until_statement_end();
    end_statement(p);
    p.finish_node();
}

pub(crate) fn truncate(p: &mut Parser) {
    p.start(TRUNCATE_STMT);
    p.bump();
    p.eat(TABLE_KW);
    p.eat(ONLY_KW);
    loop {
        qualified_name(p, "Table name");
        p.eat(STAR);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.bump_until_statement_end();
    end_statement(p);
    p.finish_node();
}

/// MySQL's `RENAME TABLE a TO b, ...`; `RENAME USER` is read leniently.
pub(crate) fn rename_table(p: &mut Parser) {
    if p.nth(1) != TABLE_KW {
        lenient(p, RENAME_TABLE_STMT);
        return;
    }
    p.start(RENAME_TABLE_STMT);
    p.bump();
    p.bump();
    loop {
        qualified_name(p, "Table name");
        p.expect_kw(TO_KW);
        qualified_name(p, "Table name");
        if !p.eat(COMMA) {
            break;
        }
    }
    end_statement(p);
    p.finish_node();
}

/// `COMMENT ON kind name IS 'text' | NULL`.
pub(crate) fn comment(p: &mut Parser) {
    p.start(COMMENT_STMT);
    p.bump();
    p.bump();
    while is_name_token(p.current()) && is_name_token(p.nth(1)) && !matches!(p.nth(1), IS_KW | ON_KW) {
        p.bump();
    }
    qualified_name(p, "Name");
    if p.at(LPAREN) {
        p.bump_balanced();
    }
    if p.eat(ON_KW) {
        qualified_name(p, "Name");
    }
    if p.expect_kw(IS_KW) && !p.eat(NULL_KW) {
        if p.current().is_string() {
            p.start(LITERAL);
            p.bump();
            p.finish_node();
        } else {
            p.error_expected("String");
        }
    }
    end_statement(p);
    p.finish_node();
}

/// `REFRESH MATERIALIZED VIEW [CONCURRENTLY] name [WITH [NO] DATA]`.
pub(crate) fn refresh(p: &mut Parser) {
    p.start(REFRESH_STMT);
    p.bump();
    p.expect_kw(MATERIALIZED_KW);
    p.expect_kw(VIEW_KW);
    p.eat(CONCURRENTLY_KW);
    qualified_name(p, "View name");
    if p.eat(WITH_KW) {
        p.eat(NO_KW);
        p.expect_kw(DATA_KW);
    }
    end_statement(p);
    p.finish_node();
}
