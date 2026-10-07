//! Type names: a dotted name or one of the standard's multi-word types, with its arguments, MySQL's
//! attributes and PostgreSQL's array bounds, in a `TYPE` node.

use super::{Parser, expr, is_name_token, qualified_name};
use crate::SyntaxKind::*;

/// One type. Gives whether there was one.
pub(crate) fn type_name(p: &mut Parser) -> bool {
    p.start(TYPE);
    let found = type_base(p);
    if found {
        type_suffix(p);
    }
    p.finish_node();
    found
}

fn type_base(p: &mut Parser) -> bool {
    match p.current() {
        DOUBLE_KW => {
            p.bump();
            p.eat(PRECISION_KW);
        }
        CHARACTER_KW | CHAR_KW => {
            p.bump();
            p.eat(VARYING_KW);
        }
        NATIONAL_KW => {
            p.bump();
            let _ = p.eat(CHARACTER_KW) || p.eat(CHAR_KW);
            p.eat(VARYING_KW);
        }
        TIME_KW | TIMESTAMP_KW => {
            p.bump();
            type_args(p);
            if (p.at(WITH_KW) || p.at(WITHOUT_KW)) && p.nth(1) == TIME_KW {
                p.bump();
                p.bump();
                p.expect_kw(ZONE_KW);
            }
            return true;
        }
        INTERVAL_KW => {
            p.bump();
            expr::interval_fields(p);
            type_args(p);
            return true;
        }
        IDENT if p.at_word("bit") && p.nth(1) == VARYING_KW => {
            p.bump();
            p.bump();
        }
        IDENT if p.at_word("long") && matches!(p.nth(1), IDENT | BINARY_KW | CHAR_KW) && p.nth(1) != LPAREN => {
            p.bump();
            if p.at(IDENT) && (p.at_word("varchar") || p.at_word("varbinary")) || p.at(BINARY_KW) {
                p.bump();
            }
        }
        SIGNED_KW | UNSIGNED_KW => {
            p.bump();
            if p.at_word("integer") || p.at_word("int") {
                p.bump();
            }
            return true;
        }
        kind if is_name_token(kind) => {
            qualified_name(p, "Type");
        }
        _ => {
            p.error_expected("Type");
            return false;
        }
    }
    type_args(p);
    true
}

/// `(10)`, `(10, 2)` or the values of MySQL's `ENUM(...)` and `SET(...)`.
fn type_args(p: &mut Parser) {
    if !p.at(LPAREN) {
        return;
    }
    p.start(TYPE_ARGS);
    p.bump();
    if !p.at(RPAREN) {
        loop {
            expr::expr(p);
            if p.at(IDENT) && (p.at_word("char") || p.at_word("byte")) {
                p.bump();
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

/// What may follow the name of a type: MySQL's `UNSIGNED`, `ZEROFILL`, `CHARACTER SET` and
/// `BINARY`, and PostgreSQL's `[]`, `[3]` and `ARRAY`.
fn type_suffix(p: &mut Parser) {
    loop {
        match p.current() {
            UNSIGNED_KW | SIGNED_KW | ZEROFILL_KW => p.bump(),
            BINARY_KW if p.nth(1) != LPAREN => p.bump(),
            CHARACTER_KW if p.nth(1) == SET_KW => {
                p.bump();
                p.bump();
                qualified_name(p, "Character set");
            }
            CHARSET_KW => {
                p.bump();
                qualified_name(p, "Character set");
            }
            LBRACKET => {
                p.bump();
                p.eat(INT_NUMBER);
                p.expect(RBRACKET, "']'");
            }
            ARRAY_KW => {
                p.bump();
                if p.at(LBRACKET) {
                    p.bump();
                    p.eat(INT_NUMBER);
                    p.expect(RBRACKET, "']'");
                }
            }
            _ => break,
        }
    }
}
