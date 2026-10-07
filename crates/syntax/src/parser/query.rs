//! Queries: `WITH`, the set operations, `SELECT` with its clauses, `VALUES`, `TABLE`, and the
//! tables and joins of `FROM`.
//!
//! A query is one node whose kind says what it is (`SELECT`, `COMPOUND_SELECT`, `VALUES`,
//! `TABLE_QUERY` or `PAREN_QUERY`). The outermost node of a query also holds its `WITH` clause, first,
//! and the clauses that apply to the whole query, last: `ORDER BY`, `LIMIT`, `OFFSET`, `FETCH` and
//! the locking clauses. The children are parsed first and wrapped once it is known which node
//! is outermost.

use rowan::Checkpoint;

use super::{Parser, alias, ddl, expr, is_name_token, is_soft_name, name, name_list, qualified_name};
use crate::SyntaxKind::{self, *};

/// Whether a query starts at the `n`th token: `SELECT`, `VALUES`, `WITH`, `TABLE name` or any of
/// those behind opening parentheses.
pub(crate) fn at_query_start(p: &Parser, n: usize) -> bool {
    let mut at = n;
    while p.nth(at) == LPAREN {
        at += 1;
    }
    match p.nth(at) {
        SELECT_KW | VALUES_KW | WITH_KW => true,
        TABLE_KW => is_name_token(p.nth(at + 1)),
        _ => false,
    }
}

/// A whole query, `WITH` included. Gives whether there was one.
pub(crate) fn query(p: &mut Parser) -> bool {
    if !at_query_start(p, 0) {
        p.error_expected("Query");
        return false;
    }
    let outer = p.checkpoint();
    if p.at(WITH_KW) {
        with_clause(p);
    }
    query_after_with(p, outer);
    true
}

/// The rest of a query whose `WITH` clause, if any, was parsed after `outer`.
pub(crate) fn query_after_with(p: &mut Parser, outer: Checkpoint) {
    if !p.enter() {
        p.bump_until_statement_end();
        return;
    }
    let first = p.checkpoint();
    let Some(mut pending) = primary(p) else {
        p.error_expected("Query");
        p.leave();
        return;
    };
    while let Some(precedence) = set_operator_precedence(p) {
        p.start_at(first, pending);
        p.finish_node();
        set_operator(p);
        set_operand(p, precedence + 1);
        pending = COMPOUND_SELECT;
    }
    trailing_clauses(p);
    p.start_at(outer, pending);
    p.finish_node();
    p.leave();
}

fn set_operator_precedence(p: &Parser) -> Option<u8> {
    match p.current() {
        UNION_KW | EXCEPT_KW => Some(1),
        INTERSECT_KW => Some(2),
        _ => None,
    }
}

fn set_operator(p: &mut Parser) {
    p.bump();
    let _ = p.eat(ALL_KW) || p.eat(DISTINCT_KW);
}

/// The right side of a set operation: the operations that bind at least as tightly, wrapped.
fn set_operand(p: &mut Parser, min_precedence: u8) {
    let first = p.checkpoint();
    let Some(mut pending) = primary(p) else {
        p.error_expected("Query");
        return;
    };
    while let Some(precedence) = set_operator_precedence(p) {
        if precedence < min_precedence {
            break;
        }
        p.start_at(first, pending);
        p.finish_node();
        set_operator(p);
        set_operand(p, precedence + 1);
        pending = COMPOUND_SELECT;
    }
    p.start_at(first, pending);
    p.finish_node();
}

/// The children of one query operand, unwrapped, and the kind of node they make.
fn primary(p: &mut Parser) -> Option<SyntaxKind> {
    match p.current() {
        SELECT_KW => {
            select_core(p);
            Some(SELECT)
        }
        VALUES_KW => {
            values_rows(p);
            Some(VALUES)
        }
        TABLE_KW => {
            p.bump();
            qualified_name(p, "Table name");
            Some(TABLE_QUERY)
        }
        LPAREN => {
            p.bump();
            query(p);
            p.expect(RPAREN, "')'");
            Some(PAREN_QUERY)
        }
        _ => None,
    }
}

/// `( query )` as a `PAREN_QUERY`, for a subquery in an expression or a table.
pub(crate) fn paren_query(p: &mut Parser) {
    p.start(PAREN_QUERY);
    p.expect(LPAREN, "'('");
    query(p);
    p.expect(RPAREN, "')'");
    p.finish_node();
}

/// `WITH [RECURSIVE] name [(columns)] AS [[NOT] MATERIALIZED] (statement), ...`.
pub(crate) fn with_clause(p: &mut Parser) {
    p.start(WITH_CLAUSE);
    p.bump();
    p.eat(RECURSIVE_KW);
    loop {
        p.start(CTE);
        name(p, "Name");
        if p.at(LPAREN) {
            name_list(p);
        }
        p.expect_kw(AS_KW);
        if p.at(NOT_KW) && p.nth(1) == MATERIALIZED_KW {
            p.bump();
        }
        p.eat(MATERIALIZED_KW);
        if p.expect(LPAREN, "'('") {
            match p.current() {
                INSERT_KW | REPLACE_KW => super::dml::insert_inner(p, None),
                UPDATE_KW => super::dml::update_inner(p, None),
                DELETE_KW => super::dml::delete_inner(p, None),
                MERGE_KW => super::dml::merge_inner(p, None),
                _ => {
                    query(p);
                }
            }
            p.expect(RPAREN, "')'");
        }
        while p.at(SEARCH_KW) || p.at(CYCLE_KW) {
            while !p.at_statement_end() && !p.at(COMMA) && !at_query_start(p, 0) && !p.at(RPAREN) {
                if super::stmt::at_inner_statement(p) {
                    break;
                }
                p.bump();
            }
        }
        p.finish_node();
        if !p.eat(COMMA) {
            break;
        }
    }
    p.finish_node();
}

/// `VALUES (...), ...` or MySQL's `VALUES ROW(...), ...`, unwrapped.
fn values_rows(p: &mut Parser) {
    p.bump();
    loop {
        row(p);
        if !p.eat(COMMA) {
            break;
        }
    }
}

/// A row of `VALUES`: `(a, b)`, `ROW(a, b)` or MySQL's empty `()`.
pub(crate) fn row(p: &mut Parser) {
    p.start(ROW_EXPR);
    p.eat(ROW_KW);
    if p.expect(LPAREN, "'('") {
        if !p.at(RPAREN) {
            expr::expr_list(p);
        }
        p.expect(RPAREN, "')'");
    }
    p.finish_node();
}

/// `SELECT` and the clauses of one query operand, unwrapped.
fn select_core(p: &mut Parser) {
    p.bump();
    loop {
        match p.current() {
            DISTINCT_KW if p.nth(1) == ON_KW => {
                p.start(DISTINCT_CLAUSE);
                p.bump();
                p.bump();
                if p.expect(LPAREN, "'('") {
                    expr::expr_list(p);
                    p.expect(RPAREN, "')'");
                }
                p.finish_node();
            }
            ALL_KW if p.nth(1) != LPAREN => p.bump(),
            DISTINCT_KW | DISTINCTROW_KW | HIGH_PRIORITY_KW | STRAIGHT_JOIN_KW | SQL_CALC_FOUND_ROWS_KW => p.bump(),
            IDENT
                if [
                    "sql_small_result",
                    "sql_big_result",
                    "sql_buffer_result",
                    "sql_no_cache",
                    "sql_cache",
                ]
                .iter()
                .any(|word| p.at_word(word)) =>
            {
                p.bump()
            }
            _ => break,
        }
    }
    select_list(p);
    if p.at(INTO_KW) {
        into_clause(p);
    }
    if p.at(FROM_KW) {
        p.start(FROM_CLAUSE);
        p.bump();
        table_list(p);
        p.finish_node();
    }
    if p.at(WHERE_KW) {
        where_clause(p);
    }
    if p.at(GROUP_KW) {
        group_by(p);
    }
    if p.at(HAVING_KW) {
        p.start(HAVING_CLAUSE);
        p.bump();
        expr::expr(p);
        p.finish_node();
    }
    if p.at(WINDOW_KW) {
        window_clause(p);
    }
    if p.at(QUALIFY_KW) {
        p.start(QUALIFY_CLAUSE);
        p.bump();
        expr::expr(p);
        p.finish_node();
    }
}

/// Whether a select list ends here, which PostgreSQL also allows before it has an item.
fn at_select_list_end(p: &Parser) -> bool {
    p.at_statement_end()
        || matches!(
            p.current(),
            FROM_KW | RPAREN | INTO_KW | WHERE_KW | UNION_KW | EXCEPT_KW | INTERSECT_KW | ORDER_KW | LIMIT_KW
        )
}

fn select_list(p: &mut Parser) {
    p.start(SELECT_LIST);
    if !at_select_list_end(p) {
        loop {
            select_item(p);
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.finish_node();
}

/// An item of a select list or of `RETURNING`: an expression or a wildcard, with its alias.
pub(crate) fn select_item(p: &mut Parser) {
    p.start(SELECT_ITEM);
    if expr::expr(p) {
        alias(p, false);
    }
    p.finish_node();
}

/// `INTO` of PostgreSQL (a new table) or of MySQL (variables, `OUTFILE`, `DUMPFILE`).
fn into_clause(p: &mut Parser) {
    p.start(INTO_CLAUSE);
    p.bump();
    if p.at_word("outfile") || p.at_word("dumpfile") {
        p.bump();
        expr::expr(p);
        while !p.at_statement_end()
            && !matches!(
                p.current(),
                FROM_KW | WHERE_KW | GROUP_KW | HAVING_KW | ORDER_KW | LIMIT_KW | FOR_KW | UNION_KW | RPAREN
            )
        {
            p.bump();
        }
        p.finish_node();
        return;
    }
    let _ = p.eat(TEMP_KW) || p.eat(TEMPORARY_KW) || p.eat(UNLOGGED_KW);
    p.eat(TABLE_KW);
    loop {
        match p.current() {
            VARIABLE | SYSTEM_VARIABLE => {
                p.start(VARIABLE_REF);
                p.bump();
                p.finish_node();
            }
            _ => {
                qualified_name(p, "Target");
            }
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.finish_node();
}

pub(crate) fn where_clause(p: &mut Parser) {
    p.start(WHERE_CLAUSE);
    p.bump();
    if p.at(CURRENT_KW) && p.nth(1) == OF_KW {
        p.bump();
        p.bump();
        name(p, "Cursor name");
    } else {
        expr::expr(p);
    }
    p.finish_node();
}

/// `GROUP BY [ALL | DISTINCT] item, ... [WITH ROLLUP]`, where an item may be `ROLLUP (...)`,
/// `CUBE (...)`, `GROUPING SETS (...)` or `()`.
fn group_by(p: &mut Parser) {
    p.start(GROUP_BY_CLAUSE);
    p.bump();
    p.expect_kw(BY_KW);
    let _ = p.eat(ALL_KW) || p.eat(DISTINCT_KW);
    loop {
        grouping_element(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    if p.at(WITH_KW) && p.nth(1) == ROLLUP_KW {
        p.bump();
        p.bump();
    }
    p.finish_node();
}

fn grouping_element(p: &mut Parser) {
    let set = match p.current() {
        ROLLUP_KW | CUBE_KW => p.nth(1) == LPAREN,
        GROUPING_KW => p.nth(1) == SETS_KW,
        LPAREN => p.nth(1) == RPAREN,
        _ => false,
    };
    if !set {
        expr::expr(p);
        return;
    }
    p.start(GROUPING_SET);
    if p.eat(GROUPING_KW) {
        p.bump();
    } else {
        let _ = p.eat(ROLLUP_KW) || p.eat(CUBE_KW);
    }
    p.expect(LPAREN, "'('");
    if !p.at(RPAREN) {
        loop {
            grouping_element(p);
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

/// `WINDOW name AS (spec), ...`.
fn window_clause(p: &mut Parser) {
    p.start(WINDOW_CLAUSE);
    p.bump();
    loop {
        p.start(WINDOW_DEF);
        name(p, "Window name");
        p.expect_kw(AS_KW);
        expr::window_spec(p);
        p.finish_node();
        if !p.eat(COMMA) {
            break;
        }
    }
    p.finish_node();
}

/// The clauses after the last operand of a query, which apply to all of it.
pub(crate) fn trailing_clauses(p: &mut Parser) {
    loop {
        match p.current() {
            ORDER_KW if p.nth(1) == BY_KW => order_by(p),
            LIMIT_KW => limit(p),
            OFFSET_KW => offset(p),
            FETCH_KW if matches!(p.nth(1), FIRST_KW | NEXT_KW) => fetch(p),
            FOR_KW if matches!(p.nth(1), UPDATE_KW | SHARE_KW | NO_KW | KEY_KW) => locking(p),
            LOCK_KW if p.nth(1) == IN_KW => locking(p),
            INTO_KW => into_clause(p),
            _ => break,
        }
    }
}

/// `ORDER BY item, ...`, where an item is an expression with `ASC`, `DESC`, `USING op` and
/// `NULLS FIRST` or `NULLS LAST`.
pub(crate) fn order_by(p: &mut Parser) {
    p.start(ORDER_BY_CLAUSE);
    p.bump();
    p.expect_kw(BY_KW);
    loop {
        p.start(ORDER_ITEM);
        expr::expr(p);
        if p.eat(USING_KW) {
            p.bump();
        } else {
            let _ = p.eat(ASC_KW) || p.eat(DESC_KW);
        }
        if p.eat(NULLS_KW) && !p.eat(FIRST_KW) && !p.eat(LAST_KW) {
            p.error_expected("FIRST or LAST");
        }
        p.finish_node();
        if !p.eat(COMMA) {
            break;
        }
    }
    if p.at(WITH_KW) && p.nth(1) == ROLLUP_KW {
        p.bump();
        p.bump();
    }
    p.finish_node();
}

/// `LIMIT count`, `LIMIT ALL` or MySQL's `LIMIT offset, count`.
pub(crate) fn limit(p: &mut Parser) {
    p.start(LIMIT_CLAUSE);
    p.bump();
    if !p.eat(ALL_KW) {
        expr::expr(p);
        if p.eat(COMMA) {
            expr::expr(p);
        }
    }
    p.finish_node();
}

fn offset(p: &mut Parser) {
    p.start(OFFSET_CLAUSE);
    p.bump();
    expr::expr(p);
    let _ = p.eat(ROW_KW) || p.eat(ROWS_KW);
    p.finish_node();
}

/// `FETCH FIRST | NEXT [count [PERCENT]] ROW | ROWS ONLY | WITH TIES`.
fn fetch(p: &mut Parser) {
    p.start(FETCH_CLAUSE);
    p.bump();
    p.bump();
    if !p.at(ROW_KW) && !p.at(ROWS_KW) {
        expr::expr(p);
        p.eat(PERCENT_KW);
    }
    if !p.eat(ROW_KW) && !p.eat(ROWS_KW) {
        p.error_expected("ROWS");
    }
    if p.at(WITH_KW) && p.nth(1) == TIES_KW {
        p.bump();
        p.bump();
    } else {
        p.expect_kw(ONLY_KW);
    }
    p.finish_node();
}

/// `FOR UPDATE | NO KEY UPDATE | SHARE | KEY SHARE [OF tables] [NOWAIT | SKIP LOCKED]` and
/// MySQL's `LOCK IN SHARE MODE`.
fn locking(p: &mut Parser) {
    p.start(LOCKING_CLAUSE);
    if p.eat(LOCK_KW) {
        p.bump();
        p.expect_kw(SHARE_KW);
        p.expect_kw(MODE_KW);
        p.finish_node();
        return;
    }
    p.bump();
    if p.eat(NO_KW) {
        p.expect_kw(KEY_KW);
    } else {
        p.eat(KEY_KW);
    }
    if !p.eat(UPDATE_KW) && !p.eat(SHARE_KW) {
        p.error_expected("UPDATE or SHARE");
    }
    if p.eat(OF_KW) {
        loop {
            qualified_name(p, "Table name");
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    if !p.eat(NOWAIT_KW) && p.eat(SKIP_KW) {
        p.expect_kw(LOCKED_KW);
    }
    p.finish_node();
}

/// Tables separated by commas, each with its joins.
pub(crate) fn table_list(p: &mut Parser) {
    loop {
        table_expr(p);
        if !p.eat(COMMA) {
            break;
        }
    }
}

fn at_join(p: &Parser) -> bool {
    match p.current() {
        JOIN_KW | INNER_KW | CROSS_KW | NATURAL_KW | STRAIGHT_JOIN_KW => true,
        LEFT_KW | RIGHT_KW | FULL_KW => matches!(p.nth(1), JOIN_KW | OUTER_KW),
        _ => false,
    }
}

/// A table and the joins that follow it, nested to the left.
fn table_expr(p: &mut Parser) {
    let checkpoint = p.checkpoint();
    if !table_primary(p) {
        return;
    }
    while at_join(p) {
        p.start_at(checkpoint, JOIN_EXPR);
        p.eat(NATURAL_KW);
        match p.current() {
            INNER_KW | CROSS_KW => p.bump(),
            LEFT_KW | RIGHT_KW | FULL_KW => {
                p.bump();
                p.eat(OUTER_KW);
            }
            _ => {}
        }
        if !p.eat(JOIN_KW) && !p.eat(STRAIGHT_JOIN_KW) {
            p.error_expected("JOIN");
        }
        table_primary(p);
        if p.at(ON_KW) {
            p.start(ON_CLAUSE);
            p.bump();
            expr::expr(p);
            p.finish_node();
        } else if p.at(USING_KW) {
            p.start(USING_CLAUSE);
            p.bump();
            name_list(p);
            if p.eat(AS_KW) {
                name(p, "Alias");
            }
            p.finish_node();
        }
        p.finish_node();
    }
}

/// Whether a function call, not a table, starts here: a dotted name followed by `(`.
pub(crate) fn at_function_call(p: &Parser) -> bool {
    let mut at = 0;
    loop {
        if !is_name_token(p.nth(at)) {
            return false;
        }
        at += 1;
        if p.nth(at) == DOT {
            at += 1;
            continue;
        }
        return p.nth(at) == LPAREN;
    }
}

/// One table of `FROM`: a table name, a subquery, a function, a parenthesized join. Gives
/// whether it found one.
pub(crate) fn table_primary(p: &mut Parser) -> bool {
    match p.current() {
        LPAREN if at_query_start(p, 0) => {
            p.start(DERIVED_TABLE);
            paren_query(p);
            alias(p, true);
            p.finish_node();
        }
        LPAREN => {
            p.start(PAREN_JOIN);
            p.bump();
            table_list(p);
            p.expect(RPAREN, "')'");
            alias(p, true);
            p.finish_node();
        }
        LATERAL_KW if at_query_start(p, 1) => {
            p.start(DERIVED_TABLE);
            p.bump();
            paren_query(p);
            alias(p, true);
            p.finish_node();
        }
        LATERAL_KW => {
            p.start(TABLE_FUNCTION);
            p.bump();
            table_function_rest(p);
            p.finish_node();
        }
        ROWS_KW if p.nth(1) == FROM_KW => {
            p.start(TABLE_FUNCTION);
            p.bump();
            p.bump();
            if p.expect(LPAREN, "'('") {
                loop {
                    expr::expr(p);
                    if p.eat(AS_KW) {
                        ddl::table_element_list(p);
                    }
                    if !p.eat(COMMA) {
                        break;
                    }
                }
                p.expect(RPAREN, "')'");
            }
            table_function_tail(p);
            p.finish_node();
        }
        ONLY_KW => {
            p.start(TABLE_REF);
            p.bump();
            let parenthesized = p.eat(LPAREN);
            qualified_name(p, "Table name");
            if parenthesized {
                p.expect(RPAREN, "')'");
            }
            table_ref_tail(p);
            p.finish_node();
        }
        kind if is_soft_name(kind) || (is_name_token(kind) && p.nth(1) == DOT) || at_function_call(p) => {
            if at_function_call(p) {
                p.start(TABLE_FUNCTION);
                table_function_rest(p);
                p.finish_node();
            } else {
                p.start(TABLE_REF);
                qualified_name(p, "Table name");
                p.eat(STAR);
                table_ref_tail(p);
                p.finish_node();
            }
        }
        _ => {
            p.error_expected("Table name");
            return false;
        }
    }
    true
}

fn table_function_rest(p: &mut Parser) {
    if at_function_call(p) {
        expr::function_call(p);
    } else {
        p.error_expected("Function");
    }
    table_function_tail(p);
}

fn table_function_tail(p: &mut Parser) {
    if p.at(WITH_KW) && p.nth(1) == ORDINALITY_KW {
        p.bump();
        p.bump();
    }
    alias(p, true);
}

/// What may follow a table name: MySQL's partitions and index hints, an alias, SQLite's
/// `INDEXED BY` and PostgreSQL's `TABLESAMPLE`.
fn table_ref_tail(p: &mut Parser) {
    if p.at(PARTITION_KW) && p.nth(1) == LPAREN {
        p.start(PARTITION_SELECTION);
        p.bump();
        name_list(p);
        p.finish_node();
    }
    alias(p, true);
    loop {
        match p.current() {
            USE_KW | IGNORE_KW | FORCE_KW if matches!(p.nth(1), INDEX_KW | KEY_KW) => {
                p.start(INDEX_HINT);
                p.bump();
                p.bump();
                if p.eat(FOR_KW) {
                    if p.at(ORDER_KW) || p.at(GROUP_KW) {
                        p.bump();
                        p.expect_kw(BY_KW);
                    } else {
                        p.expect_kw(JOIN_KW);
                    }
                }
                p.start(NAME_LIST);
                p.expect(LPAREN, "'('");
                if !p.at(RPAREN) {
                    loop {
                        name(p, "Index name");
                        if !p.eat(COMMA) {
                            break;
                        }
                    }
                }
                p.expect(RPAREN, "')'");
                p.finish_node();
                p.finish_node();
            }
            INDEXED_KW => {
                p.start(INDEX_HINT);
                p.bump();
                p.expect_kw(BY_KW);
                name(p, "Index name");
                p.finish_node();
            }
            NOT_KW if p.nth(1) == INDEXED_KW => {
                p.start(INDEX_HINT);
                p.bump();
                p.bump();
                p.finish_node();
            }
            TABLESAMPLE_KW => {
                p.start(TABLESAMPLE_CLAUSE);
                p.bump();
                expr::function_call(p);
                if p.at(REPEATABLE_KW) {
                    p.bump();
                    if p.expect(LPAREN, "'('") {
                        expr::expr(p);
                        p.expect(RPAREN, "')'");
                    }
                }
                p.finish_node();
            }
            _ => break,
        }
    }
}
