//! Functions, procedures and triggers, and the compound statements of their bodies: `BEGIN ...
//! END`, `DECLARE`, `IF`, `CASE`, the loops and the rest of SQL/PSM as MySQL and MariaDB have it.
//! A body written in another language, or in a string, is kept whole as a `ROUTINE_BODY`.

use rowan::Checkpoint;

use super::stmt::{at_inner_statement, end_statement, inner_statement, statement};
use super::{Parser, ddl, expr, is_name_token, name, qualified_name, types};
use crate::SyntaxKind::{self, *};

/// `CREATE FUNCTION | PROCEDURE name (parameters) [RETURNS ...] options body`, after the words up to
/// and including `FUNCTION` or `PROCEDURE`.
pub(crate) fn create_function(p: &mut Parser) {
    ddl::if_not_exists(p);
    qualified_name(p, "Function name");
    param_list(p);
    if p.at(RETURNS_KW) && p.nth(1) != NULL_KW {
        p.start(RETURNS_CLAUSE);
        p.bump();
        if p.at(TABLE_KW) && p.nth(1) == LPAREN {
            p.bump();
            ddl::table_element_list(p);
        } else {
            p.eat(SETOF_KW);
            types::type_name(p);
        }
        p.finish_node();
    }
    loop {
        match p.current() {
            AS_KW => {
                p.bump();
                p.start(ROUTINE_BODY);
                if p.current().is_string() {
                    p.bump();
                    if p.eat(COMMA) {
                        p.eat(STRING);
                    }
                } else {
                    p.error_expected("String");
                }
                p.finish_node();
            }
            BEGIN_KW => {
                p.start(ROUTINE_BODY);
                block(p, None);
                p.finish_node();
                break;
            }
            RETURN_KW => {
                p.start(ROUTINE_BODY);
                p.start(RETURN_STMT);
                p.bump();
                expr::expr(p);
                p.finish_node();
                p.finish_node();
                break;
            }
            LANGUAGE_KW => {
                p.bump();
                name(p, "Language");
            }
            _ if at_inner_statement(p) => {
                p.start(ROUTINE_BODY);
                inner_statement(p);
                p.finish_node();
                break;
            }
            _ if p.at_statement_end() => break,
            IF_KW | CASE_KW | LOOP_KW | WHILE_KW | REPEAT_KW | DECLARE_KW | CALL_KW => {
                p.start(ROUTINE_BODY);
                statement(p);
                p.finish_node();
                return;
            }
            _ if is_name_token(p.current()) && p.nth(1) == COLON => {
                p.start(ROUTINE_BODY);
                statement(p);
                p.finish_node();
                return;
            }
            LPAREN => p.bump_balanced(),
            _ => p.bump(),
        }
    }
}

/// `( [IN | OUT | INOUT | VARIADIC] [name] type [DEFAULT | = value], ... )`.
fn param_list(p: &mut Parser) {
    p.start(PARAM_LIST);
    if p.expect(LPAREN, "'('") {
        if !p.at(RPAREN) {
            loop {
                param(p);
                if !p.eat(COMMA) {
                    break;
                }
            }
        }
        p.expect(RPAREN, "')'");
    }
    p.finish_node();
}

/// Whether the words at the cursor are a type of two words, as `DOUBLE PRECISION`, and not a name
/// followed by a type.
fn at_two_word_type(p: &Parser) -> bool {
    matches!(
        (p.current(), p.nth(1)),
        (DOUBLE_KW, PRECISION_KW)
            | (
                CHARACTER_KW | CHAR_KW | NATIONAL_KW,
                VARYING_KW | CHARACTER_KW | CHAR_KW
            )
            | (TIME_KW | TIMESTAMP_KW, WITH_KW | WITHOUT_KW)
    )
}

fn param(p: &mut Parser) {
    p.start(PARAM_DEF);
    if matches!(p.current(), IN_KW | OUT_KW | INOUT_KW | VARIADIC_KW) {
        p.bump();
    }
    let named = is_name_token(p.current())
        && is_name_token(p.nth(1))
        && !at_two_word_type(p)
        && !matches!(p.nth(1), DEFAULT_KW | ARRAY_KW | UNSIGNED_KW | SIGNED_KW);
    if named {
        name(p, "Parameter name");
    }
    types::type_name(p);
    if p.eat(DEFAULT_KW) || p.eat(EQ) {
        expr::expr(p);
    }
    p.finish_node();
}

/// `CREATE TRIGGER name timing events ON table ... body`, after `TRIGGER`.
pub(crate) fn create_trigger(p: &mut Parser) {
    ddl::if_not_exists(p);
    qualified_name(p, "Trigger name");
    let mut table_seen = false;
    loop {
        match p.current() {
            INSERT_KW | DELETE_KW | TRUNCATE_KW if !table_seen => p.bump(),
            UPDATE_KW if !table_seen => {
                p.bump();
                if p.eat(OF_KW) {
                    loop {
                        name(p, "Column name");
                        if !p.eat(COMMA) {
                            break;
                        }
                    }
                }
            }
            ON_KW | FROM_KW => {
                p.bump();
                qualified_name(p, "Table name");
                table_seen = true;
            }
            WHEN_KW => {
                p.bump();
                expr::expr(p);
            }
            FOLLOWS_KW | PRECEDES_KW => {
                p.bump();
                name(p, "Trigger name");
            }
            EXECUTE_KW => {
                p.bump();
                let _ = p.eat(FUNCTION_KW) || p.eat(PROCEDURE_KW);
                p.start(ROUTINE_BODY);
                expr::function_call(p);
                p.finish_node();
                break;
            }
            BEGIN_KW => {
                p.start(ROUTINE_BODY);
                block(p, None);
                p.finish_node();
                break;
            }
            _ if at_inner_statement(p) => {
                p.start(ROUTINE_BODY);
                inner_statement(p);
                p.finish_node();
                break;
            }
            SET_KW | CALL_KW | IF_KW | CASE_KW | DECLARE_KW => {
                p.start(ROUTINE_BODY);
                statement(p);
                p.finish_node();
                return;
            }
            _ if p.at_statement_end() => break,
            _ => p.bump(),
        }
    }
}

/// A list of statements up to one of `stops`, in a `STATEMENT_LIST`.
fn statement_list(p: &mut Parser, stops: &[SyntaxKind]) {
    p.start(STATEMENT_LIST);
    p.block_depth += 1;
    while !p.eof() && !p.at_any(stops) {
        let before = p.position();
        statement(p);
        if p.position() == before {
            p.error_bump();
        }
    }
    p.block_depth -= 1;
    p.finish_node();
}

/// `[label:] BEGIN [[NOT] ATOMIC] statements END [label]`, without what ends it as a statement.
fn block(p: &mut Parser, label: Option<Checkpoint>) {
    match label {
        Some(checkpoint) => p.start_at(checkpoint, BLOCK),
        None => p.start(BLOCK),
    }
    block_rest(p);
    p.finish_node();
}

fn block_rest(p: &mut Parser) {
    p.bump();
    p.eat(NOT_KW);
    p.eat(ATOMIC_KW);
    statement_list(p, &[END_KW]);
    p.expect_kw(END_KW);
    if is_name_token(p.current()) && !p.at_statement_end() && !matches!(p.current(), IF_KW | LOOP_KW | CASE_KW) {
        name(p, "Label");
    }
}

/// A block as a statement of its own, with its `;`.
pub(crate) fn block_statement(p: &mut Parser) {
    p.start(BLOCK);
    block_rest(p);
    end_statement(p);
    p.finish_node();
}

/// Whether a label starts a block or a loop here: `name: BEGIN | LOOP | WHILE | REPEAT`.
pub(crate) fn at_labeled(p: &Parser) -> bool {
    matches!(p.nth(2), BEGIN_KW | LOOP_KW | WHILE_KW | REPEAT_KW)
}

/// The statements of SQL/PSM: `IF`, `CASE`, `LOOP`, `WHILE`, `REPEAT`, `LEAVE`, `ITERATE`,
/// `RETURN`, `SIGNAL`, `GET DIAGNOSTICS` and labeled blocks and loops.
pub(crate) fn procedural(p: &mut Parser) {
    let mut label = None;
    if is_name_token(p.current()) && p.nth(1) == COLON {
        let checkpoint = p.checkpoint();
        p.start(LABEL);
        name(p, "Label");
        p.bump();
        p.finish_node();
        label = Some(checkpoint);
    }
    let start = |p: &mut Parser, kind: SyntaxKind| match label {
        Some(checkpoint) => p.start_at(checkpoint, kind),
        None => p.start(kind),
    };
    match p.current() {
        BEGIN_KW => {
            start(p, BLOCK);
            block_rest(p);
        }
        IF_KW => {
            start(p, IF_STMT);
            p.bump();
            expr::expr(p);
            p.expect_kw(THEN_KW);
            statement_list(p, &[ELSEIF_KW, ELSIF_KW, ELSE_KW, END_KW]);
            while p.at(ELSEIF_KW) || p.at(ELSIF_KW) {
                p.start(ELSEIF_CLAUSE);
                p.bump();
                expr::expr(p);
                p.expect_kw(THEN_KW);
                statement_list(p, &[ELSEIF_KW, ELSIF_KW, ELSE_KW, END_KW]);
                p.finish_node();
            }
            if p.at(ELSE_KW) {
                p.start(ELSE_CLAUSE);
                p.bump();
                statement_list(p, &[END_KW]);
                p.finish_node();
            }
            p.expect_kw(END_KW);
            p.expect_kw(IF_KW);
        }
        CASE_KW => {
            start(p, CASE_STMT);
            p.bump();
            if !p.at(WHEN_KW) {
                expr::expr(p);
            }
            while p.at(WHEN_KW) {
                p.start(WHEN_CLAUSE);
                p.bump();
                expr::expr(p);
                p.expect_kw(THEN_KW);
                statement_list(p, &[WHEN_KW, ELSE_KW, END_KW]);
                p.finish_node();
            }
            if p.at(ELSE_KW) {
                p.start(ELSE_CLAUSE);
                p.bump();
                statement_list(p, &[END_KW]);
                p.finish_node();
            }
            p.expect_kw(END_KW);
            p.expect_kw(CASE_KW);
        }
        LOOP_KW => {
            start(p, LOOP_STMT);
            p.bump();
            statement_list(p, &[END_KW]);
            p.expect_kw(END_KW);
            p.expect_kw(LOOP_KW);
            end_label(p);
        }
        WHILE_KW => {
            start(p, WHILE_STMT);
            p.bump();
            expr::expr(p);
            p.expect_kw(DO_KW);
            statement_list(p, &[END_KW]);
            p.expect_kw(END_KW);
            p.expect_kw(WHILE_KW);
            end_label(p);
        }
        REPEAT_KW => {
            start(p, REPEAT_STMT);
            p.bump();
            statement_list(p, &[UNTIL_KW, END_KW]);
            if p.expect_kw(UNTIL_KW) {
                expr::expr(p);
            }
            p.expect_kw(END_KW);
            p.expect_kw(REPEAT_KW);
            end_label(p);
        }
        LEAVE_KW | ITERATE_KW => {
            let kind = if p.at(LEAVE_KW) { LEAVE_STMT } else { ITERATE_STMT };
            start(p, kind);
            p.bump();
            name(p, "Label");
        }
        RETURN_KW => {
            start(p, RETURN_STMT);
            p.bump();
            if !p.at_statement_end() {
                expr::expr(p);
            }
        }
        SIGNAL_KW | RESIGNAL_KW => {
            start(p, SIGNAL_STMT);
            p.bump();
            p.bump_until_statement_end();
        }
        GET_KW => {
            start(p, GET_DIAGNOSTICS_STMT);
            p.bump();
            p.bump_until_statement_end();
        }
        _ => {
            start(p, BLOCK);
            p.error_expected("BEGIN, LOOP, WHILE or REPEAT");
        }
    }
    end_statement(p);
    p.finish_node();
}

fn end_label(p: &mut Parser) {
    if is_name_token(p.current()) && !p.at_statement_end() {
        name(p, "Label");
    }
}

/// `DECLARE` of variables, conditions, cursors and handlers.
pub(crate) fn declare(p: &mut Parser) {
    p.start(DECLARE_STMT);
    p.bump();
    if matches!(p.current(), CONTINUE_KW | EXIT_KW) || p.at_word("undo") {
        p.bump();
        p.expect_kw(HANDLER_KW);
        p.expect_kw(FOR_KW);
        while !p.at_statement_end() && !p.at(BEGIN_KW) && !at_inner_statement(p) && !p.at(SET_KW) {
            p.bump();
        }
        if !p.at_statement_end() {
            statement(p);
        } else {
            end_statement(p);
        }
        p.finish_node();
        return;
    }
    loop {
        name(p, "Name");
        if !p.eat(COMMA) {
            break;
        }
    }
    let cursor = (0..8)
        .map(|n| p.nth(n))
        .take_while(|kind| !matches!(kind, SEMICOLON | CUSTOM_DELIMITER | EOF | FOR_KW))
        .position(|kind| kind == CURSOR_KW);
    if let Some(words) = cursor {
        for _ in 0..=words {
            p.bump();
        }
        while p.at(WITH_KW) || p.at(WITHOUT_KW) || p.at_word("hold") {
            p.bump();
        }
        if p.expect_kw(FOR_KW) {
            if at_inner_statement(p) {
                inner_statement(p);
            } else {
                p.error_expected("Query");
            }
        }
    } else if p.at(CONDITION_KW) {
        p.bump_until_statement_end();
    } else {
        types::type_name(p);
        if p.eat(DEFAULT_KW) {
            expr::expr(p);
        }
    }
    end_statement(p);
    p.finish_node();
}

/// `FETCH` and `MOVE` of a cursor.
pub(crate) fn fetch(p: &mut Parser) {
    p.start(FETCH_STMT);
    p.bump();
    while !p.at_statement_end() && !p.at(INTO_KW) {
        p.bump();
    }
    if p.eat(INTO_KW) {
        expr::expr_list(p);
    }
    end_statement(p);
    p.finish_node();
}
