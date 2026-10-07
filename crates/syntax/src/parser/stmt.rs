//! Scripts and statements: what starts a statement, how one ends, how a broken one is kept from
//! swallowing the next, and the statements too small for a module of their own.

use super::{Parser, account_name, ddl, dml, expr, is_name_token, name, qualified_name, query, routine};
use crate::SyntaxKind::{self, *};

pub(crate) fn source_file(p: &mut Parser) {
    p.start_root(SOURCE_FILE);
    while !p.eof() {
        let before = p.position();
        statement(p);
        if p.position() == before {
            p.error_bump();
        }
    }
    p.flush_rest();
    p.finish_node();
}

/// Words that start a statement. A broken statement is cut off before one that starts a line.
pub(crate) fn starts_statement(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SELECT_KW
            | INSERT_KW
            | UPDATE_KW
            | DELETE_KW
            | REPLACE_KW
            | MERGE_KW
            | CREATE_KW
            | ALTER_KW
            | DROP_KW
            | TRUNCATE_KW
            | WITH_KW
            | BEGIN_KW
            | START_KW
            | COMMIT_KW
            | ROLLBACK_KW
            | SAVEPOINT_KW
            | RELEASE_KW
            | SET_KW
            | SHOW_KW
            | USE_KW
            | EXPLAIN_KW
            | DESCRIBE_KW
            | PRAGMA_KW
            | GRANT_KW
            | REVOKE_KW
            | COMMENT_KW
            | COPY_KW
            | PREPARE_KW
            | EXECUTE_KW
            | DEALLOCATE_KW
            | CALL_KW
            | DECLARE_KW
            | VALUES_KW
            | LOCK_KW
            | UNLOCK_KW
            | ANALYZE_KW
            | VACUUM_KW
            | REFRESH_KW
            | RENAME_KW
            | ATTACH_KW
            | DETACH_KW
            | REINDEX_KW
            | DELIMITER_KW
            | END_KW
            | IF_KW
            | RETURN_KW
    )
}

/// One statement with what ends it.
pub(crate) fn statement(p: &mut Parser) {
    match p.current() {
        SEMICOLON | CUSTOM_DELIMITER => {
            p.start(EMPTY_STMT);
            p.bump();
            p.finish_node();
        }
        META_COMMAND => {
            p.start(META_COMMAND_STMT);
            p.bump();
            p.finish_node();
        }
        COPY_DATA => p.error_bump(),
        DELIMITER_KW => {
            p.start(DELIMITER_STMT);
            p.bump();
            if !p.eat(DELIMITER_VALUE) {
                p.error_expected("Delimiter");
            }
            p.finish_node();
        }
        SELECT_KW | VALUES_KW | LPAREN => select_stmt(p),
        TABLE_KW if is_name_token(p.nth(1)) => select_stmt(p),
        WITH_KW => with_stmt(p),
        INSERT_KW | REPLACE_KW => dml::insert(p, None),
        UPDATE_KW => dml::update(p, None),
        DELETE_KW => dml::delete(p, None),
        MERGE_KW => dml::merge(p, None),
        CREATE_KW => ddl::create(p),
        ALTER_KW => ddl::alter(p),
        DROP_KW => ddl::drop(p),
        TRUNCATE_KW => ddl::truncate(p),
        RENAME_KW => ddl::rename_table(p),
        COMMENT_KW if p.nth(1) == ON_KW => ddl::comment(p),
        BEGIN_KW if p.in_block() || p.nth(1) == NOT_KW && p.nth(2) == ATOMIC_KW => routine::block_statement(p),
        BEGIN_KW | START_KW => transaction(p, BEGIN_STMT),
        COMMIT_KW | END_KW => transaction(p, COMMIT_STMT),
        ROLLBACK_KW => transaction(p, ROLLBACK_STMT),
        SAVEPOINT_KW => transaction(p, SAVEPOINT_STMT),
        RELEASE_KW => transaction(p, RELEASE_STMT),
        SET_KW => set_stmt(p),
        SHOW_KW => lenient(p, SHOW_STMT),
        USE_KW => use_stmt(p),
        EXPLAIN_KW | DESCRIBE_KW | DESC_KW => explain(p),
        PRAGMA_KW => pragma(p),
        GRANT_KW => grant(p, GRANT_STMT),
        REVOKE_KW => grant(p, REVOKE_STMT),
        COPY_KW => copy(p),
        PREPARE_KW => prepare(p),
        EXECUTE_KW => execute(p),
        DEALLOCATE_KW => lenient(p, DEALLOCATE_STMT),
        CALL_KW => call(p),
        DO_KW => do_stmt(p),
        DECLARE_KW => routine::declare(p),
        OPEN_KW => lenient(p, OPEN_STMT),
        FETCH_KW | MOVE_KW => routine::fetch(p),
        CLOSE_KW => lenient(p, CLOSE_STMT),
        REFRESH_KW => ddl::refresh(p),
        LOCK_KW => lenient(p, LOCK_STMT),
        UNLOCK_KW => lenient(p, UNLOCK_STMT),
        ATTACH_KW => attach(p),
        DETACH_KW => lenient(p, DETACH_STMT),
        ANALYZE_KW if query::at_query_start(p, 1) || matches!(p.nth(1), INSERT_KW | UPDATE_KW | DELETE_KW) => {
            explain(p)
        }
        ANALYZE_KW | VACUUM_KW | REINDEX_KW | CLUSTER_KW | CHECKPOINT_KW | DISCARD_KW | LISTEN_KW | NOTIFY_KW
        | UNLISTEN_KW | LOAD_KW | OPTIMIZE_KW | REPAIR_KW | FLUSH_KW | KILL_KW | HANDLER_KW | RESET_KW => {
            lenient(p, UTILITY_STMT)
        }
        CHECK_KW if p.nth(1) == TABLE_KW => lenient(p, UTILITY_STMT),
        IF_KW | CASE_KW | LOOP_KW | WHILE_KW | REPEAT_KW | LEAVE_KW | ITERATE_KW | RETURN_KW | SIGNAL_KW
        | RESIGNAL_KW => routine::procedural(p),
        GET_KW if p.nth(1) == DIAGNOSTICS_KW || p.nth(2) == DIAGNOSTICS_KW => routine::procedural(p),
        kind if is_name_token(kind) && p.nth(1) == COLON && routine::at_labeled(p) => routine::procedural(p),
        _ => unknown(p),
    }
}

/// Ends a statement: its `;` or the delimiter `DELIMITER` set. A missing one is reported when the
/// next statement starts, and anything else is wrapped in an error up to where the next statement
/// can start, which is a `;` or a word that starts a statement at the start of a line.
pub(crate) fn end_statement(p: &mut Parser) {
    if p.eat(SEMICOLON) || p.eat(CUSTOM_DELIMITER) {
        return;
    }
    if p.at(META_COMMAND) || p.eof() {
        return;
    }
    if p.in_block()
        && matches!(
            p.current(),
            END_KW | ELSE_KW | ELSEIF_KW | ELSIF_KW | WHEN_KW | UNTIL_KW
        )
    {
        p.error_expected("';'");
        return;
    }
    if starts_statement(p.current()) && p.at_line_start() {
        p.error_expected("';'");
        return;
    }
    let message = format!("Unexpected '{}'", p.current_text().escape_debug());
    p.error_here(message);
    p.start(ERROR);
    loop {
        p.bump();
        if p.at_statement_end() {
            break;
        }
        if starts_statement(p.current()) && p.at_line_start() {
            break;
        }
        if p.in_block() && p.at(END_KW) && p.at_line_start() {
            break;
        }
    }
    p.finish_node();
    let _ = p.eat(SEMICOLON) || p.eat(CUSTOM_DELIMITER);
}

/// A statement whose words the grammar does not follow one by one: its first word, the rest up to
/// its end.
pub(crate) fn lenient(p: &mut Parser, kind: SyntaxKind) {
    p.start(kind);
    p.bump();
    p.bump_until_statement_end();
    end_statement(p);
    p.finish_node();
}

fn unknown(p: &mut Parser) {
    p.start(UNKNOWN_STMT);
    let message = format!("Unknown statement '{}'", p.current_text().escape_debug());
    p.error_here(message);
    p.start(ERROR);
    loop {
        p.bump();
        if p.at_statement_end() || p.at(RPAREN) {
            break;
        }
        if starts_statement(p.current()) && p.at_line_start() {
            break;
        }
    }
    if p.at(RPAREN) {
        p.bump();
        p.bump_until_statement_end();
    }
    p.finish_node();
    let _ = p.eat(SEMICOLON) || p.eat(CUSTOM_DELIMITER);
    p.finish_node();
}

pub(crate) fn select_stmt(p: &mut Parser) {
    let checkpoint = p.checkpoint();
    query::query(p);
    p.start_at(checkpoint, SELECT_STMT);
    end_statement(p);
    p.finish_node();
}

fn with_stmt(p: &mut Parser) {
    let checkpoint = p.checkpoint();
    query::with_clause(p);
    match p.current() {
        INSERT_KW | REPLACE_KW => dml::insert(p, Some(checkpoint)),
        UPDATE_KW => dml::update(p, Some(checkpoint)),
        DELETE_KW => dml::delete(p, Some(checkpoint)),
        MERGE_KW => dml::merge(p, Some(checkpoint)),
        _ => {
            query::query_after_with(p, checkpoint);
            p.start_at(checkpoint, SELECT_STMT);
            end_statement(p);
            p.finish_node();
        }
    }
}

/// Whether a statement that may hold another statement, as `EXPLAIN`, `PREPARE ... AS` or a
/// rule's action, can parse one here.
pub(crate) fn at_inner_statement(p: &Parser) -> bool {
    query::at_query_start(p, 0) || matches!(p.current(), INSERT_KW | UPDATE_KW | DELETE_KW | MERGE_KW | REPLACE_KW)
}

/// A statement inside another, which leaves the end to the outer one.
pub(crate) fn inner_statement(p: &mut Parser) {
    match p.current() {
        WITH_KW => {
            let checkpoint = p.checkpoint();
            query::with_clause(p);
            match p.current() {
                INSERT_KW | REPLACE_KW => dml::insert_inner(p, Some(checkpoint)),
                UPDATE_KW => dml::update_inner(p, Some(checkpoint)),
                DELETE_KW => dml::delete_inner(p, Some(checkpoint)),
                MERGE_KW => dml::merge_inner(p, Some(checkpoint)),
                _ => {
                    query::query_after_with(p, checkpoint);
                    p.start_at(checkpoint, SELECT_STMT);
                    p.finish_node();
                }
            }
        }
        INSERT_KW | REPLACE_KW => dml::insert_inner(p, None),
        UPDATE_KW => dml::update_inner(p, None),
        DELETE_KW => dml::delete_inner(p, None),
        MERGE_KW => dml::merge_inner(p, None),
        _ => {
            let checkpoint = p.checkpoint();
            query::query(p);
            p.start_at(checkpoint, SELECT_STMT);
            p.finish_node();
        }
    }
}

/// `BEGIN`, `START TRANSACTION`, `COMMIT`, `END`, `ROLLBACK [TO [SAVEPOINT] name]`, `SAVEPOINT name`
/// and `RELEASE [SAVEPOINT] name`, with their modes.
fn transaction(p: &mut Parser, kind: SyntaxKind) {
    p.start(kind);
    let first = p.current();
    p.bump();
    match first {
        ROLLBACK_KW => {
            let _ = p.eat(WORK_KW) || p.eat(TRANSACTION_KW);
            if p.eat(TO_KW) {
                p.eat(SAVEPOINT_KW);
                name(p, "Savepoint name");
            }
            p.bump_until_statement_end();
        }
        SAVEPOINT_KW => {
            name(p, "Savepoint name");
        }
        RELEASE_KW => {
            p.eat(SAVEPOINT_KW);
            name(p, "Savepoint name");
        }
        _ => p.bump_until_statement_end(),
    }
    end_statement(p);
    p.finish_node();
}

/// `SET`: assignments to variables and settings, `SET NAMES`, `SET TRANSACTION`, `SET ROLE` and
/// the rest, each item as a `SET_ASSIGNMENT` where it has a value.
fn set_stmt(p: &mut Parser) {
    p.start(SET_STMT);
    p.bump();
    if p.at_any(&[SESSION_KW, LOCAL_KW, GLOBAL_KW, PERSIST_KW, PERSIST_ONLY_KW])
        && !matches!(p.nth(1), EQ | COLON_EQ | TO_KW | DOT)
    {
        p.bump();
    }
    if p.at_any(&[TRANSACTION_KW, NAMES_KW, ROLE_KW, CHARACTER_KW, CHARSET_KW]) || p.at_word("password") {
        p.bump_until_statement_end();
        end_statement(p);
        p.finish_node();
        return;
    }
    loop {
        p.start(SET_ASSIGNMENT);
        if p.at_any(&[GLOBAL_KW, SESSION_KW, LOCAL_KW, PERSIST_KW, PERSIST_ONLY_KW]) && is_name_token(p.nth(1)) {
            p.bump();
        }
        match p.current() {
            VARIABLE | SYSTEM_VARIABLE => {
                p.start(VARIABLE_REF);
                p.bump();
                p.finish_node();
            }
            kind if is_name_token(kind) => {
                if p.at(TIME_KW) && p.nth(1) == ZONE_KW {
                    p.bump();
                    p.bump();
                } else {
                    qualified_name(p, "Setting");
                }
            }
            _ => p.error_expected("Variable"),
        }
        let assigned = p.eat(EQ) || p.eat(COLON_EQ) || p.eat(TO_KW);
        if assigned || !p.at_statement_end() && !p.at(COMMA) {
            set_value(p);
        }
        p.finish_node();
        if !p.eat(COMMA) {
            break;
        }
    }
    end_statement(p);
    p.finish_node();
}

/// The value of a setting: an expression, `DEFAULT`, or in PostgreSQL a list of words and literals.
fn set_value(p: &mut Parser) {
    if p.eat(DEFAULT_KW) || p.eat(ON_KW) {
        return;
    }
    loop {
        let before = p.position();
        expr::expr(p);
        if p.position() == before || p.at_statement_end() || p.at(RPAREN) {
            break;
        }
        // PostgreSQL's `SET search_path TO a, b` takes a list where MySQL's `SET a = 1, b = 2`
        // starts the next assignment.
        if p.at(COMMA) {
            let next_assigns = matches!(p.nth(2), EQ | COLON_EQ | TO_KW | DOT)
                || matches!(
                    p.nth(1),
                    VARIABLE | SYSTEM_VARIABLE | GLOBAL_KW | SESSION_KW | LOCAL_KW | PERSIST_KW | PERSIST_ONLY_KW
                );
            if next_assigns {
                break;
            }
            p.bump();
        }
    }
}

fn use_stmt(p: &mut Parser) {
    p.start(USE_STMT);
    p.bump();
    qualified_name(p, "Database name");
    end_statement(p);
    p.finish_node();
}

/// `EXPLAIN [ANALYZE] [VERBOSE] [(options)] [FORMAT=...] [QUERY PLAN] statement`, `DESCRIBE` a
/// table or a statement, and MariaDB's `ANALYZE statement`.
fn explain(p: &mut Parser) {
    p.start(EXPLAIN_STMT);
    p.bump();
    loop {
        if at_inner_statement(p) {
            break;
        }
        match p.current() {
            ANALYZE_KW | QUERY_KW | PLAN_KW => p.bump(),
            LPAREN => p.bump_balanced(),
            FORMAT_KW if p.nth(1) == EQ => {
                p.bump();
                p.bump();
                p.bump();
            }
            IDENT if p.at_word("verbose") || p.at_word("partitions") || p.at_word("extended") => p.bump(),
            _ => break,
        }
    }
    if at_inner_statement(p) {
        inner_statement(p);
    } else if is_name_token(p.current()) {
        qualified_name(p, "Table name");
        if is_name_token(p.current()) || p.current().is_string() {
            p.bump();
        }
    } else if p.at(FOR_KW) {
        p.bump_until_statement_end();
    } else {
        p.error_expected("Statement");
    }
    end_statement(p);
    p.finish_node();
}

/// `PRAGMA [schema.]name [= value | (value)]`.
fn pragma(p: &mut Parser) {
    p.start(PRAGMA_STMT);
    p.bump();
    qualified_name(p, "Pragma name");
    if p.eat(EQ) {
        pragma_value(p);
    } else if p.eat(LPAREN) {
        pragma_value(p);
        p.expect(RPAREN, "')'");
    }
    end_statement(p);
    p.finish_node();
}

fn pragma_value(p: &mut Parser) {
    if matches!(p.current(), PLUS | MINUS) {
        p.bump();
    }
    if is_name_token(p.current()) || p.current().is_string() || matches!(p.current(), INT_NUMBER | FLOAT_NUMBER) {
        p.start(LITERAL);
        p.bump();
        p.finish_node();
    } else {
        p.error_expected("Value");
    }
}

/// `GRANT` and `REVOKE`: privileges, `ON [kind] objects`, and the grantees.
fn grant(p: &mut Parser, kind: SyntaxKind) {
    p.start(kind);
    p.bump();
    while !p.at_statement_end() && !p.at(ON_KW) && !p.at(TO_KW) && !p.at(FROM_KW) {
        if p.at(LPAREN) {
            p.bump_balanced();
        } else {
            p.bump();
        }
    }
    if p.eat(ON_KW) {
        while matches!(
            p.current(),
            TABLE_KW | SEQUENCE_KW | FUNCTION_KW | PROCEDURE_KW | SCHEMA_KW | DATABASE_KW | DOMAIN_KW | TYPE_KW
        ) || p.at_word("tables")
            || p.at_word("sequences")
            || p.at_word("functions")
            || p.at_word("all")
            || p.at(ALL_KW)
            || p.at(IN_KW)
            || p.at_word("routine")
            || p.at_word("language")
        {
            p.bump();
        }
        loop {
            if p.at(STAR) {
                p.start(WILDCARD);
                p.bump();
                if p.eat(DOT) {
                    p.expect(STAR, "'*'");
                }
                p.finish_node();
            } else if is_name_token(p.current()) {
                if p.nth(1) == DOT && p.nth(2) == STAR {
                    p.start(WILDCARD);
                    name(p, "Name");
                    p.bump();
                    p.bump();
                    p.finish_node();
                } else {
                    qualified_name(p, "Object name");
                    if p.at(LPAREN) {
                        p.bump_balanced();
                    }
                }
            } else {
                p.error_expected("Object name");
                break;
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    if p.eat(TO_KW) || p.eat(FROM_KW) {
        loop {
            p.eat(GROUP_KW);
            if p.at_any(&[CURRENT_USER_KW, SESSION_USER_KW, CURRENT_ROLE_KW]) {
                p.bump();
            } else {
                account_name(p);
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.bump_until_statement_end();
    end_statement(p);
    p.finish_node();
}

/// `COPY table [(columns)] | (query) FROM | TO ...`, and the data of `FROM STDIN` after it.
fn copy(p: &mut Parser) {
    p.start(COPY_STMT);
    p.bump();
    if p.at(LPAREN) {
        query::paren_query(p);
    } else {
        qualified_name(p, "Table name");
        if p.at(LPAREN) {
            super::name_list(p);
        }
    }
    if !p.eat(FROM_KW) && !p.eat(TO_KW) {
        p.error_expected("FROM or TO");
    }
    p.bump_until_statement_end();
    end_statement(p);
    p.eat(COPY_DATA);
    p.finish_node();
}

/// `PREPARE name [(types)] AS statement` in PostgreSQL, `PREPARE name FROM 'text' | @var` in MySQL.
fn prepare(p: &mut Parser) {
    p.start(PREPARE_STMT);
    p.bump();
    if p.at(TRANSACTION_KW) {
        p.bump_until_statement_end();
        end_statement(p);
        p.finish_node();
        return;
    }
    name(p, "Statement name");
    if p.at(LPAREN) {
        p.bump();
        loop {
            super::types::type_name(p);
            if !p.eat(COMMA) {
                break;
            }
        }
        p.expect(RPAREN, "')'");
    }
    if p.eat(AS_KW) {
        if at_inner_statement(p) {
            inner_statement(p);
        } else {
            p.error_expected("Statement");
        }
    } else if p.eat(FROM_KW) {
        expr::expr(p);
    } else {
        p.error_expected("AS or FROM");
    }
    end_statement(p);
    p.finish_node();
}

/// `EXECUTE name [(arguments)]` and `EXECUTE name USING @a, @b`.
fn execute(p: &mut Parser) {
    p.start(EXECUTE_STMT);
    p.bump();
    name(p, "Statement name");
    if p.at(LPAREN) {
        expr::arg_list(p);
    }
    if p.eat(USING_KW) {
        expr::expr_list(p);
    }
    end_statement(p);
    p.finish_node();
}

fn call(p: &mut Parser) {
    p.start(CALL_STMT);
    p.bump();
    qualified_name(p, "Procedure name");
    if p.at(LPAREN) {
        expr::arg_list(p);
    }
    end_statement(p);
    p.finish_node();
}

/// `DO $$ ... $$ [LANGUAGE name]` in PostgreSQL, `DO expression, ...` in MySQL.
fn do_stmt(p: &mut Parser) {
    p.start(DO_STMT);
    p.bump();
    if p.at(LANGUAGE_KW) {
        p.bump();
        name(p, "Language");
    }
    if p.at(DOLLAR_STRING) || (p.at(STRING) && matches!(p.nth(1), SEMICOLON | EOF | LANGUAGE_KW)) {
        p.start(ROUTINE_BODY);
        p.bump();
        p.finish_node();
        if p.eat(LANGUAGE_KW) {
            name(p, "Language");
        }
    } else {
        expr::expr_list(p);
    }
    end_statement(p);
    p.finish_node();
}

/// `ATTACH [DATABASE] 'file' AS name`.
fn attach(p: &mut Parser) {
    p.start(ATTACH_STMT);
    p.bump();
    p.eat(DATABASE_KW);
    expr::expr(p);
    if p.expect_kw(AS_KW) {
        name(p, "Schema name");
    }
    p.bump_until_statement_end();
    end_statement(p);
    p.finish_node();
}
