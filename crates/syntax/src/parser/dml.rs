//! `INSERT` and `REPLACE`, `UPDATE`, `DELETE` and `MERGE`, each optionally after a `WITH` clause
//! that the statement node holds first.

use rowan::Checkpoint;

use super::stmt::end_statement;
use super::{Parser, alias, expr, is_name_token, is_soft_name, name, name_list, qualified_name, query};
use crate::SyntaxKind::{self, *};

fn statement(p: &mut Parser, checkpoint: Option<Checkpoint>, kind: SyntaxKind, body: fn(&mut Parser), end: bool) {
    match checkpoint {
        Some(checkpoint) => p.start_at(checkpoint, kind),
        None => p.start(kind),
    }
    body(p);
    if end {
        end_statement(p);
    }
    p.finish_node();
}

pub(crate) fn insert(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, INSERT_STMT, insert_body, true);
}

pub(crate) fn insert_inner(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, INSERT_STMT, insert_body, false);
}

pub(crate) fn update(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, UPDATE_STMT, update_body, true);
}

pub(crate) fn update_inner(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, UPDATE_STMT, update_body, false);
}

pub(crate) fn delete(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, DELETE_STMT, delete_body, true);
}

pub(crate) fn delete_inner(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, DELETE_STMT, delete_body, false);
}

pub(crate) fn merge(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, MERGE_STMT, merge_body, true);
}

pub(crate) fn merge_inner(p: &mut Parser, checkpoint: Option<Checkpoint>) {
    statement(p, checkpoint, MERGE_STMT, merge_body, false);
}

/// SQLite's `OR ROLLBACK | ABORT | REPLACE | FAIL | IGNORE` after `INSERT` or `UPDATE`.
fn or_action(p: &mut Parser) {
    if p.at(OR_KW) && matches!(p.nth(1), ROLLBACK_KW | ABORT_KW | REPLACE_KW | FAIL_KW | IGNORE_KW) {
        p.bump();
        p.bump();
    }
}

fn insert_body(p: &mut Parser) {
    p.bump();
    while matches!(p.current(), LOW_PRIORITY_KW | DELAYED_KW | HIGH_PRIORITY_KW | IGNORE_KW) {
        p.bump();
    }
    or_action(p);
    p.eat(INTO_KW);
    qualified_name(p, "Table name");
    if p.at(AS_KW) && is_name_token(p.nth(1)) {
        alias(p, false);
    }
    if p.at(PARTITION_KW) && p.nth(1) == LPAREN {
        p.start(PARTITION_SELECTION);
        p.bump();
        name_list(p);
        p.finish_node();
    }
    if p.at(LPAREN) && !query::at_query_start(p, 0) {
        name_list(p);
    }
    if p.at(OVERRIDING_KW) {
        p.bump();
        p.bump();
        p.expect_kw(VALUE_KW);
    }
    match p.current() {
        VALUES_KW | VALUE_KW => {
            p.start(VALUES);
            p.bump();
            loop {
                query::row(p);
                if !p.eat(COMMA) {
                    break;
                }
            }
            p.finish_node();
        }
        DEFAULT_KW => {
            p.bump();
            p.expect_kw(VALUES_KW);
        }
        SET_KW => set_clause(p),
        _ if query::at_query_start(p, 0) => {
            query::query(p);
        }
        _ => p.error_expected("VALUES or a query"),
    }
    if p.at(AS_KW) {
        alias(p, true);
    }
    loop {
        if p.at(ON_KW) && p.nth(1) == DUPLICATE_KW {
            p.start(ON_DUPLICATE_KEY_CLAUSE);
            p.bump();
            p.bump();
            p.expect_kw(KEY_KW);
            p.expect_kw(UPDATE_KW);
            assignments(p);
            p.finish_node();
        } else if p.at(ON_KW) && p.nth(1) == CONFLICT_KW {
            upsert(p);
        } else {
            break;
        }
    }
    returning(p);
}

/// `ON CONFLICT [(columns) [WHERE ...] | ON CONSTRAINT name] DO NOTHING | DO UPDATE SET ... [WHERE ...]`.
fn upsert(p: &mut Parser) {
    p.start(UPSERT_CLAUSE);
    p.bump();
    p.bump();
    if p.at(LPAREN) {
        p.start(CONFLICT_TARGET);
        super::ddl::index_column_list(p);
        if p.at(WHERE_KW) {
            query::where_clause(p);
        }
        p.finish_node();
    } else if p.at(ON_KW) && p.nth(1) == CONSTRAINT_KW {
        p.start(CONFLICT_TARGET);
        p.bump();
        p.bump();
        name(p, "Constraint name");
        p.finish_node();
    }
    if p.expect_kw(DO_KW) && !p.eat(NOTHING_KW) && p.expect_kw(UPDATE_KW) {
        set_clause(p);
        if p.at(WHERE_KW) {
            query::where_clause(p);
        }
    }
    p.finish_node();
}

/// `RETURNING item, ...`, with PostgreSQL's `WITH (OLD AS o, NEW AS n)`.
pub(crate) fn returning(p: &mut Parser) {
    if !p.at(RETURNING_KW) {
        return;
    }
    p.start(RETURNING_CLAUSE);
    p.bump();
    if p.at(WITH_KW) && p.nth(1) == LPAREN {
        p.bump();
        p.bump_balanced();
    }
    p.start(SELECT_LIST);
    loop {
        query::select_item(p);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.finish_node();
    if p.at(INTO_KW) {
        p.bump();
        expr::expr_list(p);
    }
    p.finish_node();
}

/// `SET assignment, ...` in a `SET_CLAUSE`.
pub(crate) fn set_clause(p: &mut Parser) {
    p.start(SET_CLAUSE);
    p.expect_kw(SET_KW);
    assignment_list(p);
    p.finish_node();
}

fn assignments(p: &mut Parser) {
    p.start(SET_CLAUSE);
    assignment_list(p);
    p.finish_node();
}

fn assignment_list(p: &mut Parser) {
    loop {
        p.start(ASSIGNMENT);
        if p.at(LPAREN) {
            name_list(p);
        } else if is_soft_name(p.current()) || is_name_token(p.current()) && p.nth(1) == DOT {
            expr::column_ref(p);
            if p.at(LBRACKET) {
                p.bump();
                expr::expr(p);
                p.expect(RBRACKET, "']'");
            }
        } else {
            p.error_expected("Column name");
            p.finish_node();
            break;
        }
        p.expect(EQ, "'='");
        expr::expr(p);
        p.finish_node();
        if !p.eat(COMMA) {
            break;
        }
    }
}

fn update_body(p: &mut Parser) {
    p.bump();
    while matches!(p.current(), LOW_PRIORITY_KW | IGNORE_KW) {
        p.bump();
    }
    or_action(p);
    query::table_list(p);
    set_clause(p);
    if p.at(FROM_KW) {
        p.start(FROM_CLAUSE);
        p.bump();
        query::table_list(p);
        p.finish_node();
    }
    if p.at(WHERE_KW) {
        query::where_clause(p);
    }
    order_and_limit(p);
    returning(p);
}

fn order_and_limit(p: &mut Parser) {
    if p.at(ORDER_KW) && p.nth(1) == BY_KW {
        query::order_by(p);
    }
    if p.at(LIMIT_KW) {
        query::limit(p);
    }
}

fn delete_body(p: &mut Parser) {
    p.bump();
    while matches!(p.current(), LOW_PRIORITY_KW | QUICK_KW | IGNORE_KW) {
        p.bump();
    }
    if !p.at(FROM_KW) {
        // MySQL's `DELETE t1, t2 FROM ...`.
        loop {
            p.start(QUALIFIED_NAME);
            name(p, "Table name");
            while p.at(DOT) {
                p.bump();
                if !p.eat(STAR) {
                    name(p, "Name");
                }
            }
            p.finish_node();
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    if p.at(FROM_KW) {
        p.start(FROM_CLAUSE);
        p.bump();
        query::table_list(p);
        p.finish_node();
    } else {
        p.error_expected("FROM");
    }
    if p.at(USING_KW) {
        p.start(USING_CLAUSE);
        p.bump();
        query::table_list(p);
        p.finish_node();
    }
    if p.at(WHERE_KW) {
        query::where_clause(p);
    }
    order_and_limit(p);
    returning(p);
}

fn merge_body(p: &mut Parser) {
    p.bump();
    p.expect_kw(INTO_KW);
    p.eat(ONLY_KW);
    qualified_name(p, "Table name");
    alias(p, false);
    if p.expect_kw(USING_KW) {
        query::table_primary(p);
    }
    if p.at(ON_KW) {
        p.start(ON_CLAUSE);
        p.bump();
        expr::expr(p);
        p.finish_node();
    } else {
        p.error_expected("ON");
    }
    while p.at(WHEN_KW) {
        p.start(MERGE_WHEN_CLAUSE);
        p.bump();
        p.eat(NOT_KW);
        p.expect_kw(MATCHED_KW);
        if p.at(BY_KW) {
            p.bump();
            if p.at_word("source") || p.at_word("target") {
                p.bump();
            } else {
                p.error_expected("SOURCE or TARGET");
            }
        }
        if p.eat(AND_KW) {
            expr::expr(p);
        }
        p.expect_kw(THEN_KW);
        match p.current() {
            UPDATE_KW => {
                p.bump();
                set_clause(p);
            }
            DELETE_KW => p.bump(),
            INSERT_KW => {
                p.bump();
                if p.at(LPAREN) {
                    name_list(p);
                }
                if p.at(OVERRIDING_KW) {
                    p.bump();
                    p.bump();
                    p.expect_kw(VALUE_KW);
                }
                if p.at(DEFAULT_KW) {
                    p.bump();
                    p.expect_kw(VALUES_KW);
                } else if p.at(VALUES_KW) {
                    p.start(VALUES);
                    p.bump();
                    query::row(p);
                    p.finish_node();
                } else {
                    p.error_expected("VALUES");
                }
            }
            DO_KW => {
                p.bump();
                p.expect_kw(NOTHING_KW);
            }
            _ => p.error_expected("UPDATE, DELETE, INSERT or DO NOTHING"),
        }
        p.finish_node();
    }
    returning(p);
}
