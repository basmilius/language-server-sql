//! Expressions, as a Pratt parser over the union of the operators of every dialect, with
//! PostgreSQL's precedence where the dialects differ. Function calls take the special forms of the
//! standard (`EXTRACT(... FROM ...)`, `TRIM(LEADING ... FROM ...)`, aggregates with `ORDER BY`)
//! and the clauses of SQL/JSON in their argument list.

use super::{Parser, is_name_token, is_reserved, is_soft_name, name, qualified_name, query, types};
use crate::SyntaxKind::{self, *};

/// One expression. Gives whether there was one.
pub(crate) fn expr(p: &mut Parser) -> bool {
    expr_bp(p, 0)
}

/// Expressions separated by commas.
pub(crate) fn expr_list(p: &mut Parser) {
    loop {
        expr(p);
        if !p.eat(COMMA) {
            break;
        }
    }
}

/// The binding powers, from loosest to tightest. An infix operator has a left power that decides
/// whether it may continue what is to its left, and a right power for what follows it.
mod bp {
    pub(super) const ASSIGN: (u8, u8) = (2, 1);
    pub(super) const OR: (u8, u8) = (3, 4);
    pub(super) const XOR: (u8, u8) = (5, 6);
    pub(super) const AND: (u8, u8) = (7, 8);
    pub(super) const NOT: u8 = 9;
    pub(super) const IS: u8 = 11;
    pub(super) const COMPARE: (u8, u8) = (13, 14);
    pub(super) const PREDICATE: (u8, u8) = (15, 16);
    pub(super) const OTHER: (u8, u8) = (17, 18);
    pub(super) const ADD: (u8, u8) = (19, 20);
    pub(super) const MULTIPLY: (u8, u8) = (21, 22);
    pub(super) const POWER: (u8, u8) = (23, 24);
    pub(super) const AT_TIME_ZONE: (u8, u8) = (25, 26);
    pub(super) const COLLATE: u8 = 27;
    pub(super) const UNARY: u8 = 29;
}

/// The power below which a column default stops: it takes arithmetic, casts and other operators
/// but no comparison, so `DEFAULT 0 NOT NULL` ends before `NOT`.
pub(crate) const DEFAULT_BP: u8 = bp::OTHER.0;

fn infix(kind: SyntaxKind) -> Option<(u8, u8)> {
    Some(match kind {
        COLON_EQ => bp::ASSIGN,
        OR_KW => bp::OR,
        XOR_KW => bp::XOR,
        AND_KW => bp::AND,
        EQ | EQ_EQ | NEQ | BANG_EQ | LT | GT | LTE | GTE | NULL_SAFE_EQ => bp::COMPARE,
        OVERLAPS_KW => bp::PREDICATE,
        PIPE_PIPE | AMP_AMP | PIPE | AMP | HASH | SHL | SHR | ARROW | LONG_ARROW | HASH_ARROW | HASH_LONG_ARROW
        | HASH_MINUS | AT_GT | LT_AT | AT_AT | QUESTION | QUESTION_PIPE | QUESTION_AMP | TILDE | TILDE_STAR
        | BANG_TILDE | BANG_TILDE_STAR | CUSTOM_OP => bp::OTHER,
        PLUS | MINUS => bp::ADD,
        STAR | SLASH | PERCENT | DIV_KW | MOD_KW => bp::MULTIPLY,
        CARET => bp::POWER,
        _ => return None,
    })
}

fn is_predicate_word(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        BETWEEN_KW | IN_KW | LIKE_KW | ILIKE_KW | GLOB_KW | MATCH_KW | REGEXP_KW | RLIKE_KW | SIMILAR_KW
    )
}

pub(crate) fn expr_bp(p: &mut Parser, min_bp: u8) -> bool {
    if !p.enter() {
        if !p.at_statement_end() {
            p.start(ERROR);
            p.bump();
            p.finish_node();
        }
        return false;
    }
    let checkpoint = p.checkpoint();
    if !prefix_or_primary(p) {
        p.leave();
        return false;
    }
    loop {
        let kind = p.current();
        if let Some((left, right)) = infix(kind) {
            if left < min_bp {
                break;
            }
            p.start_at(checkpoint, BINARY_EXPR);
            p.bump();
            expr_bp(p, right);
            p.finish_node();
            continue;
        }
        if kind == OPERATOR_KW && p.nth(1) == LPAREN {
            if bp::OTHER.0 < min_bp {
                break;
            }
            p.start_at(checkpoint, BINARY_EXPR);
            operator_name(p);
            expr_bp(p, bp::OTHER.1);
            p.finish_node();
            continue;
        }
        let negated = kind == NOT_KW && is_predicate_word(p.nth(1));
        let predicate = if negated { p.nth(1) } else { kind };
        match predicate {
            _ if negated || is_predicate_word(predicate) => {
                if bp::PREDICATE.0 < min_bp {
                    break;
                }
                predicate_rest(p, checkpoint, predicate);
            }
            SOUNDS_KW if p.nth(1) == LIKE_KW => {
                if bp::PREDICATE.0 < min_bp {
                    break;
                }
                p.start_at(checkpoint, LIKE_EXPR);
                p.bump();
                p.bump();
                expr_bp(p, bp::PREDICATE.1);
                p.finish_node();
            }
            MEMBER_KW if p.nth(1) == OF_KW => {
                if bp::PREDICATE.0 < min_bp {
                    break;
                }
                p.start_at(checkpoint, MEMBER_OF_EXPR);
                p.bump();
                p.bump();
                if p.expect(LPAREN, "'('") {
                    expr(p);
                    p.expect(RPAREN, "')'");
                }
                p.finish_node();
            }
            IS_KW | ISNULL_KW | NOTNULL_KW => {
                if bp::IS < min_bp {
                    break;
                }
                is_rest(p, checkpoint);
            }
            AT_KW if matches!(p.nth(1), TIME_KW | LOCAL_KW) => {
                if bp::AT_TIME_ZONE.0 < min_bp {
                    break;
                }
                p.start_at(checkpoint, AT_TIME_ZONE_EXPR);
                p.bump();
                if p.eat(TIME_KW) {
                    p.expect_kw(ZONE_KW);
                    expr_bp(p, bp::AT_TIME_ZONE.1);
                } else {
                    p.bump();
                }
                p.finish_node();
            }
            COLLATE_KW => {
                if bp::COLLATE < min_bp {
                    break;
                }
                p.start_at(checkpoint, COLLATE_EXPR);
                p.bump();
                collation(p);
                p.finish_node();
            }
            DOUBLE_COLON => {
                p.start_at(checkpoint, TYPECAST_EXPR);
                p.bump();
                types::type_name(p);
                p.finish_node();
            }
            LBRACKET => {
                p.start_at(checkpoint, INDEX_EXPR);
                p.bump();
                if !p.at(COLON) && !p.at(RBRACKET) {
                    expr(p);
                }
                if p.eat(COLON) && !p.at(RBRACKET) {
                    expr(p);
                }
                p.expect(RBRACKET, "']'");
                p.finish_node();
            }
            DOT if p.nth(1) == STAR || is_name_token(p.nth(1)) => {
                p.start_at(checkpoint, FIELD_EXPR);
                p.bump();
                if !p.eat(STAR) {
                    name(p, "Field name");
                }
                p.finish_node();
            }
            _ => break,
        }
    }
    p.leave();
    true
}

/// `[NOT] BETWEEN`, `[NOT] IN`, `[NOT] LIKE` and the other pattern matches, after their left side.
fn predicate_rest(p: &mut Parser, checkpoint: rowan::Checkpoint, word: SyntaxKind) {
    let kind = match word {
        BETWEEN_KW => BETWEEN_EXPR,
        IN_KW => IN_EXPR,
        _ => LIKE_EXPR,
    };
    p.start_at(checkpoint, kind);
    p.eat(NOT_KW);
    p.bump();
    match word {
        BETWEEN_KW => {
            let _ = p.eat(SYMMETRIC_KW) || p.eat(ASYMMETRIC_KW);
            expr_bp(p, bp::PREDICATE.1);
            p.expect_kw(AND_KW);
            expr_bp(p, bp::PREDICATE.1);
        }
        IN_KW => {
            if p.at(LPAREN) && query::at_query_start(p, 0) {
                query::paren_query(p);
            } else if p.at(LPAREN) {
                p.start(IN_LIST);
                p.bump();
                if !p.at(RPAREN) {
                    expr_list(p);
                }
                p.expect(RPAREN, "')'");
                p.finish_node();
            } else if is_name_token(p.current()) {
                if query::at_function_call(p) {
                    function_call(p);
                } else {
                    qualified_name(p, "Table name");
                }
            } else {
                p.error_expected("'('");
            }
        }
        _ => {
            if word == SIMILAR_KW {
                p.expect_kw(TO_KW);
            }
            expr_bp(p, bp::PREDICATE.1);
            if p.eat(ESCAPE_KW) {
                expr_bp(p, bp::PREDICATE.1);
            }
        }
    }
    p.finish_node();
}

/// `IS [NOT] NULL | TRUE | FALSE | UNKNOWN | DISTINCT FROM x | JSON ... | NORMALIZED`, `ISNULL` and
/// `NOTNULL`.
fn is_rest(p: &mut Parser, checkpoint: rowan::Checkpoint) {
    p.start_at(checkpoint, IS_EXPR);
    if p.eat(ISNULL_KW) || p.eat(NOTNULL_KW) {
        p.finish_node();
        return;
    }
    p.bump();
    p.eat(NOT_KW);
    match p.current() {
        NULL_KW | TRUE_KW | FALSE_KW | UNKNOWN_KW => p.bump(),
        DISTINCT_KW => {
            p.bump();
            p.expect_kw(FROM_KW);
            expr_bp(p, bp::IS + 1);
        }
        JSON_KW => {
            p.bump();
            let _ = p.eat(VALUE_KW) || p.eat(ARRAY_KW) || p.eat(OBJECT_KW) || p.eat(SCALAR_KW);
            if (p.at(WITH_KW) || p.at(WITHOUT_KW)) && p.nth(1) == UNIQUE_KW {
                p.bump();
                p.bump();
                p.eat(KEYS_KW);
            }
        }
        OF_KW => {
            p.bump();
            if p.expect(LPAREN, "'('") {
                loop {
                    types::type_name(p);
                    if !p.eat(COMMA) {
                        break;
                    }
                }
                p.expect(RPAREN, "')'");
            }
        }
        IDENT if p.at_word("normalized") || p.at_word("document") => p.bump(),
        IDENT if ["nfc", "nfd", "nfkc", "nfkd"].iter().any(|form| p.at_word(form)) => {
            p.bump();
            if p.at_word("normalized") {
                p.bump();
            }
        }
        _ => {
            // `a IS b` compares in SQLite.
            expr_bp(p, bp::IS + 1);
        }
    }
    p.finish_node();
}

fn collation(p: &mut Parser) {
    if p.current().is_string() {
        p.start(NAME);
        p.bump();
        p.finish_node();
    } else {
        qualified_name(p, "Collation");
    }
}

/// PostgreSQL's `OPERATOR(schema.op)`.
fn operator_name(p: &mut Parser) {
    p.start(OPERATOR_NAME);
    p.bump();
    p.bump();
    while is_name_token(p.current()) && p.nth(1) == DOT {
        name(p, "Schema name");
        p.bump();
    }
    if !p.at(RPAREN) {
        p.bump();
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

fn prefix_or_primary(p: &mut Parser) -> bool {
    let kind = p.current();
    let prefix = match kind {
        NOT_KW => Some(bp::NOT),
        MINUS | PLUS | TILDE | BANG | AT | CUSTOM_OP => Some(bp::UNARY),
        BINARY_KW if p.nth(1) != LPAREN && !p.at_statement_end() => Some(bp::UNARY),
        OPERATOR_KW if p.nth(1) == LPAREN => {
            p.start(PREFIX_EXPR);
            operator_name(p);
            expr_bp(p, bp::UNARY);
            p.finish_node();
            return true;
        }
        _ => None,
    };
    if let Some(power) = prefix {
        p.start(PREFIX_EXPR);
        p.bump();
        expr_bp(p, power);
        p.finish_node();
        return true;
    }
    primary(p)
}

/// Types whose name may stand before a string to make a literal of it, as `json '{}'`.
const LITERAL_TYPES: &[&str] = &[
    "int",
    "int2",
    "int4",
    "int8",
    "integer",
    "bigint",
    "smallint",
    "numeric",
    "decimal",
    "real",
    "float",
    "float4",
    "float8",
    "bool",
    "boolean",
    "text",
    "varchar",
    "json",
    "jsonb",
    "uuid",
    "bytea",
    "timestamptz",
    "timetz",
    "inet",
    "cidr",
    "macaddr",
    "point",
    "line",
    "lseg",
    "box",
    "circle",
    "polygon",
    "path",
    "tsvector",
    "tsquery",
    "money",
    "xml",
    "bit",
    "varbit",
    "oid",
    "regclass",
    "regtype",
    "regproc",
    "regprocedure",
    "regnamespace",
    "regrole",
    "datetime",
];

/// Standard functions that take no parentheses.
fn is_value_function(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        CURRENT_DATE_KW
            | CURRENT_TIME_KW
            | CURRENT_TIMESTAMP_KW
            | LOCALTIME_KW
            | LOCALTIMESTAMP_KW
            | CURRENT_USER_KW
            | SESSION_USER_KW
            | CURRENT_ROLE_KW
            | CURRENT_CATALOG_KW
            | SYSTEM_USER_KW
    )
}

/// Reserved words that are still the names of functions when a `(` follows.
fn is_reserved_function(kind: SyntaxKind) -> bool {
    matches!(kind, LEFT_KW | RIGHT_KW | INSERT_KW | VALUES_KW | DEFAULT_KW | CHECK_KW)
}

fn literal(p: &mut Parser) {
    p.start(LITERAL);
    let first = p.current();
    p.bump();
    if first.is_string() {
        while p.current().is_string() {
            p.bump();
        }
    }
    p.finish_node();
}

fn primary(p: &mut Parser) -> bool {
    let kind = p.current();
    match kind {
        INT_NUMBER | FLOAT_NUMBER | TRUE_KW | FALSE_KW | NULL_KW => literal(p),
        _ if kind.is_string() => literal(p),
        UNKNOWN_KW if p.nth(1) != LPAREN => literal(p),
        IDENT if p.current_text().starts_with('_') && p.nth(1).is_string() => {
            p.start(LITERAL);
            p.bump();
            while p.current().is_string() {
                p.bump();
            }
            p.finish_node();
        }
        PARAM | QUESTION => {
            p.start(PARAMETER);
            p.bump();
            p.finish_node();
        }
        COLON if is_name_token(p.nth(1)) && p.nth_touches_next(0) => {
            p.start(PARAMETER);
            p.bump();
            p.bump();
            p.finish_node();
        }
        VARIABLE | SYSTEM_VARIABLE => {
            p.start(VARIABLE_REF);
            p.bump();
            p.finish_node();
        }
        STAR => {
            p.start(WILDCARD);
            p.bump();
            p.finish_node();
        }
        LPAREN if query::at_query_start(p, 0) => query::paren_query(p),
        LPAREN => {
            let checkpoint = p.checkpoint();
            p.bump();
            expr(p);
            let mut node = PAREN_EXPR;
            if p.eat(COMMA) {
                node = ROW_EXPR;
                expr_list(p);
            }
            p.expect(RPAREN, "')'");
            p.start_at(checkpoint, node);
            p.finish_node();
        }
        CASE_KW => case_expr(p),
        CAST_KW if p.nth(1) == LPAREN => {
            p.start(CAST_EXPR);
            p.bump();
            p.bump();
            expr(p);
            if p.expect_kw(AS_KW) {
                types::type_name(p);
            }
            p.expect(RPAREN, "')'");
            p.finish_node();
        }
        EXISTS_KW => {
            p.start(EXISTS_EXPR);
            p.bump();
            if p.at(LPAREN) {
                query::paren_query(p);
            } else {
                p.error_expected("'('");
            }
            p.finish_node();
        }
        ARRAY_KW if p.nth(1) == LBRACKET => {
            p.start(ARRAY_EXPR);
            p.bump();
            array_elements(p);
            p.finish_node();
        }
        ARRAY_KW if p.nth(1) == LPAREN && query::at_query_start(p, 1) => {
            p.start(ARRAY_EXPR);
            p.bump();
            query::paren_query(p);
            p.finish_node();
        }
        ROW_KW if p.nth(1) == LPAREN => query::row(p),
        INTERVAL_KW if p.nth(1) != LPAREN || p.nth(2) == INT_NUMBER && p.nth(3) == RPAREN && p.nth(4).is_string() => {
            interval(p)
        }
        DATE_KW | TIME_KW | TIMESTAMP_KW if p.nth(1).is_string() => {
            p.start(TYPED_LITERAL);
            p.bump();
            p.bump();
            p.finish_node();
        }
        TIME_KW | TIMESTAMP_KW
            if matches!(p.nth(1), WITH_KW | WITHOUT_KW) && p.nth(2) == TIME_KW && p.nth(4).is_string() =>
        {
            p.start(TYPED_LITERAL);
            for _ in 0..5 {
                p.bump();
            }
            p.finish_node();
        }
        _ if p.nth(1).is_string() && LITERAL_TYPES.iter().any(|name| p.at_word(name)) => {
            p.start(TYPED_LITERAL);
            p.start(TYPE);
            p.start(QUALIFIED_NAME);
            name(p, "Type");
            p.finish_node();
            p.finish_node();
            p.bump();
            p.finish_node();
        }
        _ if is_value_function(kind) && p.nth(1) != LPAREN => {
            p.start(VALUE_FUNCTION);
            p.bump();
            p.finish_node();
        }
        DEFAULT_KW if p.nth(1) != LPAREN => {
            p.start(DEFAULT_EXPR);
            p.bump();
            p.finish_node();
        }
        ANY_KW | SOME_KW | ALL_KW if p.nth(1) == LPAREN => {
            p.start(QUANTIFIED_EXPR);
            p.bump();
            if query::at_query_start(p, 0) {
                query::paren_query(p);
            } else {
                p.bump();
                expr(p);
                p.expect(RPAREN, "')'");
            }
            p.finish_node();
        }
        MATCH_KW if p.nth(1) == LPAREN => match_against(p),
        _ if is_name_token(kind) && (is_soft_name(kind) || p.nth(1) == DOT || p.nth(1) == LPAREN) => {
            if is_reserved(kind) && p.nth(1) == LPAREN && !is_reserved_function(kind) {
                p.error_expected("Expression");
                return false;
            }
            name_or_call(p);
        }
        _ => {
            p.error_expected("Expression");
            return false;
        }
    }
    true
}

/// A column, `table.column`, `t.*` or a function call, decided by what follows the dotted name.
fn name_or_call(p: &mut Parser) {
    let checkpoint = p.checkpoint();
    name(p, "Name");
    let mut wildcard = false;
    while p.at(DOT) {
        if p.nth(1) == STAR {
            p.bump();
            p.bump();
            wildcard = true;
            break;
        }
        if !is_name_token(p.nth(1)) {
            break;
        }
        p.bump();
        name(p, "Name");
    }
    if wildcard {
        p.start_at(checkpoint, WILDCARD);
        p.finish_node();
        return;
    }
    if p.at(LPAREN) {
        p.start_at(checkpoint, QUALIFIED_NAME);
        p.finish_node();
        p.start_at(checkpoint, FUNCTION_CALL);
        call_rest(p);
        p.finish_node();
        return;
    }
    p.start_at(checkpoint, COLUMN_REF);
    p.finish_node();
}

/// A function call at the cursor: its dotted name, arguments and what follows them.
pub(crate) fn function_call(p: &mut Parser) {
    p.start(FUNCTION_CALL);
    qualified_name(p, "Function name");
    if p.at(LPAREN) {
        call_rest(p);
    } else {
        p.error_expected("'('");
    }
    p.finish_node();
}

/// A column name as a `COLUMN_REF`, for the target of an assignment.
pub(crate) fn column_ref(p: &mut Parser) {
    p.start(COLUMN_REF);
    name(p, "Column name");
    while p.at(DOT) && is_name_token(p.nth(1)) {
        p.bump();
        name(p, "Column name");
    }
    p.finish_node();
}

/// The arguments of a call and `WITHIN GROUP`, `FILTER`, `RESPECT NULLS` and `OVER` after them.
fn call_rest(p: &mut Parser) {
    arg_list(p);
    if p.at(WITHIN_KW) && p.nth(1) == GROUP_KW {
        p.start(WITHIN_GROUP_CLAUSE);
        p.bump();
        p.bump();
        if p.expect(LPAREN, "'('") {
            if p.at(ORDER_KW) {
                query::order_by(p);
            } else {
                p.error_expected("ORDER BY");
            }
            p.expect(RPAREN, "')'");
        }
        p.finish_node();
    }
    if p.at(FILTER_KW) && p.nth(1) == LPAREN {
        p.start(FILTER_CLAUSE);
        p.bump();
        p.bump();
        if p.at(WHERE_KW) {
            query::where_clause(p);
        } else {
            p.error_expected("WHERE");
        }
        p.expect(RPAREN, "')'");
        p.finish_node();
    }
    if (p.at(IGNORE_KW) || p.at_word("respect")) && p.nth(1) == NULLS_KW {
        p.bump();
        p.bump();
    }
    if p.at(OVER_KW) {
        p.start(OVER_CLAUSE);
        p.bump();
        if p.at(LPAREN) {
            window_spec(p);
        } else {
            name(p, "Window name");
        }
        p.finish_node();
    }
}

/// `( [name] [PARTITION BY ...] [ORDER BY ...] [frame] )`.
pub(crate) fn window_spec(p: &mut Parser) {
    p.start(WINDOW_SPEC);
    p.expect(LPAREN, "'('");
    if is_name_token(p.current()) && !matches!(p.current(), PARTITION_KW | ORDER_KW | ROWS_KW | RANGE_KW | GROUPS_KW) {
        name(p, "Window name");
    }
    if p.at(PARTITION_KW) {
        p.start(PARTITION_BY_CLAUSE);
        p.bump();
        p.expect_kw(BY_KW);
        expr_list(p);
        p.finish_node();
    }
    if p.at(ORDER_KW) {
        query::order_by(p);
    }
    if matches!(p.current(), ROWS_KW | RANGE_KW | GROUPS_KW) {
        p.start(FRAME_CLAUSE);
        p.bump();
        if p.eat(BETWEEN_KW) {
            frame_bound(p);
            p.expect_kw(AND_KW);
            frame_bound(p);
        } else {
            frame_bound(p);
        }
        if p.eat(EXCLUDE_KW) {
            match p.current() {
                CURRENT_KW => {
                    p.bump();
                    p.expect_kw(ROW_KW);
                }
                NO_KW => {
                    p.bump();
                    p.expect_kw(OTHERS_KW);
                }
                GROUP_KW | TIES_KW => p.bump(),
                _ => p.error_expected("CURRENT ROW, GROUP, TIES or NO OTHERS"),
            }
        }
        p.finish_node();
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

fn frame_bound(p: &mut Parser) {
    match p.current() {
        UNBOUNDED_KW => {
            p.bump();
            if !p.eat(PRECEDING_KW) && !p.eat(FOLLOWING_KW) {
                p.error_expected("PRECEDING or FOLLOWING");
            }
        }
        CURRENT_KW => {
            p.bump();
            p.expect_kw(ROW_KW);
        }
        _ => {
            expr_bp(p, bp::OTHER.0);
            if !p.eat(PRECEDING_KW) && !p.eat(FOLLOWING_KW) {
                p.error_expected("PRECEDING or FOLLOWING");
            }
        }
    }
}

/// The parenthesized arguments of a call, in an `ARG_LIST`.
pub(crate) fn arg_list(p: &mut Parser) {
    p.start(ARG_LIST);
    let function = function_name_before(p);
    p.bump();
    let _ = p.eat(DISTINCT_KW) || (p.nth(1) != LPAREN && p.eat(ALL_KW));
    let mut first = true;
    while !p.at(RPAREN) && !p.at_statement_end() {
        let before = p.position();
        argument(p, function, first);
        first = false;
        argument_tails(p);
        if p.at(ORDER_KW) && p.nth(1) == BY_KW {
            query::order_by(p);
        }
        if p.eat(SEPARATOR_KW) {
            expr(p);
        }
        if p.at(LIMIT_KW) {
            query::limit(p);
        }
        if p.eat(COMMA) {
            continue;
        }
        if p.position() == before {
            break;
        }
        if !p.at(RPAREN) {
            p.error_expected("')'");
            break;
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

/// Which special form a call is in: the name of the function, when the argument list follows a
/// plain name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Special {
    None,
    Position,
    Trim,
    Extract,
}

fn function_name_before(p: &Parser) -> Special {
    let offset = p.current_offset() as usize;
    let before = p.text()[..offset].trim_end();
    let word: String = before
        .chars()
        .rev()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    match word.to_ascii_lowercase().as_str() {
        "position" => Special::Position,
        "trim" => Special::Trim,
        "extract" => Special::Extract,
        _ => Special::None,
    }
}

fn argument(p: &mut Parser, function: Special, first: bool) {
    if is_name_token(p.current()) && matches!(p.nth(1), FAT_ARROW | COLON_EQ) {
        p.start(NAMED_ARG);
        name(p, "Parameter name");
        p.bump();
        expr(p);
        p.finish_node();
        return;
    }
    if p.eat(VARIADIC_KW) {
        expr(p);
        return;
    }
    if first && function == Special::Trim && matches!(p.current(), LEADING_KW | TRAILING_KW | BOTH_KW) {
        p.bump();
        if !p.at(FROM_KW) {
            expr(p);
        }
        return;
    }
    if first && function == Special::Extract && is_name_token(p.current()) && p.nth(1) == FROM_KW {
        p.start(NAME);
        p.bump();
        p.finish_node();
        return;
    }
    if first && function == Special::Position {
        expr_bp(p, bp::OTHER.0);
        return;
    }
    if p.at(FROM_KW) {
        return;
    }
    let checkpoint = p.checkpoint();
    expr(p);
    if p.at(VALUE_KW) || p.at(COLON) {
        p.start_at(checkpoint, JSON_KEY_VALUE);
        p.bump();
        expr(p);
        p.finish_node();
    }
}

/// What may follow an argument besides a comma: the keywords of the standard's special forms
/// (`FROM`, `FOR`, `IN`, `PLACING`, `USING`) and the clauses of SQL/JSON.
fn argument_tails(p: &mut Parser) {
    loop {
        match p.current() {
            FROM_KW | FOR_KW | IN_KW | PLACING_KW => {
                p.bump();
                expr(p);
            }
            USING_KW => {
                p.bump();
                qualified_name(p, "Character set");
            }
            AS_KW => {
                p.bump();
                if is_name_token(p.current()) && matches!(p.nth(1), COMMA | RPAREN | COLUMNS_KW | PASSING_KW) {
                    name(p, "Name");
                } else {
                    types::type_name(p);
                }
            }
            FORMAT_KW if p.nth(1) == JSON_KW => json_clause(p),
            RETURNING_KW | PASSING_KW | WRAPPER_KW | KEEP_KW | OMIT_KW => json_clause(p),
            WITH_KW | WITHOUT_KW
                if matches!(
                    p.nth(1),
                    UNIQUE_KW | WRAPPER_KW | CONDITIONAL_KW | UNCONDITIONAL_KW | ARRAY_KW
                ) =>
            {
                json_clause(p)
            }
            NULL_KW | ABSENT_KW if p.nth(1) == ON_KW && p.nth(2) == NULL_KW => json_clause(p),
            NULL_KW | ERROR_KW | TRUE_KW | FALSE_KW | UNKNOWN_KW if p.nth(1) == ON_KW => json_clause(p),
            EMPTY_KW if p.nth(1) == ON_KW || matches!(p.nth(1), ARRAY_KW | OBJECT_KW) => json_clause(p),
            DEFAULT_KW => json_clause(p),
            COLUMNS_KW if p.nth(1) == LPAREN => json_table_columns(p),
            _ => break,
        }
    }
}

/// One clause of SQL/JSON: `FORMAT JSON`, `RETURNING type`, `PASSING ...`, `NULL ON NULL`,
/// `WITH UNIQUE KEYS`, `WITH WRAPPER`, `KEEP QUOTES` or a behavior `... ON EMPTY | ERROR`.
fn json_clause(p: &mut Parser) {
    p.start(JSON_CLAUSE);
    match p.current() {
        FORMAT_KW => {
            p.bump();
            p.bump();
            if p.eat(ENCODING_KW) {
                name(p, "Encoding");
            }
        }
        RETURNING_KW => {
            p.bump();
            types::type_name(p);
            if p.at(FORMAT_KW) && p.nth(1) == JSON_KW {
                p.bump();
                p.bump();
            }
        }
        PASSING_KW => {
            p.bump();
            loop {
                expr(p);
                if p.eat(AS_KW) {
                    name(p, "Name");
                }
                if !p.eat(COMMA) {
                    break;
                }
            }
        }
        WITH_KW | WITHOUT_KW => {
            p.bump();
            if p.eat(UNIQUE_KW) {
                p.eat(KEYS_KW);
            } else {
                let _ = p.eat(CONDITIONAL_KW) || p.eat(UNCONDITIONAL_KW);
                p.eat(ARRAY_KW);
                p.expect_kw(WRAPPER_KW);
            }
        }
        KEEP_KW | OMIT_KW => {
            p.bump();
            p.expect_kw(QUOTES_KW);
            if p.at(ON_KW) {
                p.bump();
                p.expect_kw(SCALAR_KW);
                if p.at_word("string") {
                    p.bump();
                }
            }
        }
        DEFAULT_KW => {
            p.bump();
            expr_bp(p, bp::OTHER.0);
            behavior_target(p);
        }
        _ => {
            let behavior = p.current();
            p.bump();
            if behavior == EMPTY_KW {
                let _ = p.eat(ARRAY_KW) || p.eat(OBJECT_KW);
            }
            behavior_target(p);
        }
    }
    p.finish_node();
}

fn behavior_target(p: &mut Parser) {
    if p.expect_kw(ON_KW) && !p.eat(NULL_KW) && !p.eat(EMPTY_KW) && !p.eat(ERROR_KW) {
        p.error_expected("EMPTY or ERROR");
    }
}

/// `COLUMNS (column, ...)` of `JSON_TABLE`.
fn json_table_columns(p: &mut Parser) {
    p.start(JSON_TABLE_COLUMNS);
    p.bump();
    p.bump();
    loop {
        p.start(JSON_TABLE_COLUMN);
        if p.at(NESTED_KW) {
            p.bump();
            p.eat(PATH_KW);
            expr_bp(p, bp::OTHER.0);
            if p.eat(AS_KW) {
                name(p, "Name");
            }
            if p.at(COLUMNS_KW) {
                json_table_columns(p);
            } else {
                p.error_expected("COLUMNS");
            }
        } else {
            name(p, "Column name");
            if p.at(FOR_KW) && p.nth(1) == ORDINALITY_KW {
                p.bump();
                p.bump();
            } else {
                types::type_name(p);
                loop {
                    match p.current() {
                        EXISTS_KW => p.bump(),
                        PATH_KW => {
                            p.bump();
                            expr_bp(p, bp::OTHER.0);
                        }
                        COMMA | RPAREN => break,
                        _ => {
                            let before = p.position();
                            argument_tails(p);
                            if p.position() == before {
                                break;
                            }
                        }
                    }
                }
            }
        }
        p.finish_node();
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

fn case_expr(p: &mut Parser) {
    p.start(CASE_EXPR);
    p.bump();
    if !p.at(WHEN_KW) {
        expr(p);
    }
    while p.at(WHEN_KW) {
        p.start(WHEN_CLAUSE);
        p.bump();
        expr(p);
        p.expect_kw(THEN_KW);
        expr(p);
        p.finish_node();
    }
    if p.at(ELSE_KW) {
        p.start(ELSE_CLAUSE);
        p.bump();
        expr(p);
        p.finish_node();
    }
    p.expect_kw(END_KW);
    p.finish_node();
}

fn array_elements(p: &mut Parser) {
    p.expect(LBRACKET, "'['");
    if !p.at(RBRACKET) {
        loop {
            if p.at(LBRACKET) {
                p.start(ARRAY_EXPR);
                array_elements(p);
                p.finish_node();
            } else {
                expr(p);
            }
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.expect(RBRACKET, "']'");
}

/// Whether the word at the cursor names a unit of an interval.
pub(crate) fn at_interval_unit(p: &Parser) -> bool {
    matches!(
        p.current(),
        YEAR_KW
            | QUARTER_KW
            | MONTH_KW
            | WEEK_KW
            | DAY_KW
            | HOUR_KW
            | MINUTE_KW
            | SECOND_KW
            | MICROSECOND_KW
            | YEAR_MONTH_KW
            | DAY_HOUR_KW
            | DAY_MINUTE_KW
            | DAY_SECOND_KW
            | DAY_MICROSECOND_KW
            | HOUR_MINUTE_KW
            | HOUR_SECOND_KW
            | HOUR_MICROSECOND_KW
            | MINUTE_SECOND_KW
            | MINUTE_MICROSECOND_KW
            | SECOND_MICROSECOND_KW
    )
}

/// `INTERVAL '1 day'`, `INTERVAL '1' DAY`, `INTERVAL '1' YEAR TO MONTH` and MySQL's
/// `INTERVAL 1 + 1 DAY`.
fn interval(p: &mut Parser) {
    p.start(INTERVAL_EXPR);
    p.bump();
    if p.at(LPAREN) {
        p.bump();
        p.bump();
        p.bump();
    }
    if p.current().is_string() {
        literal(p);
    } else {
        expr_bp(p, bp::ADD.0);
    }
    interval_fields(p);
    p.finish_node();
}

/// The unit of an interval: `DAY`, `YEAR TO MONTH`, `SECOND(3)`.
pub(crate) fn interval_fields(p: &mut Parser) {
    if !at_interval_unit(p) {
        return;
    }
    p.bump();
    if p.at(LPAREN) && p.nth(1) == INT_NUMBER {
        p.bump();
        p.bump();
        p.expect(RPAREN, "')'");
    }
    if p.eat(TO_KW) {
        if at_interval_unit(p) {
            p.bump();
        } else {
            p.error_expected("Unit");
        }
        if p.at(LPAREN) && p.nth(1) == INT_NUMBER {
            p.bump();
            p.bump();
            p.expect(RPAREN, "')'");
        }
    }
}

/// MySQL's `MATCH (columns) AGAINST (expr [modifier])`, or a plain call of a function `match`.
fn match_against(p: &mut Parser) {
    let checkpoint = p.checkpoint();
    p.start(QUALIFIED_NAME);
    name(p, "Name");
    p.finish_node();
    arg_list(p);
    if !p.at_word("against") {
        p.start_at(checkpoint, FUNCTION_CALL);
        p.finish_node();
        return;
    }
    p.start_at(checkpoint, MATCH_AGAINST_EXPR);
    p.bump();
    if p.expect(LPAREN, "'('") {
        expr_bp(p, bp::OTHER.0);
        while !p.at(RPAREN) && !p.at_statement_end() {
            if p.at(WITH_KW) || p.at(IN_KW) || p.at(MODE_KW) || p.at(QUERY_KW) || is_name_token(p.current()) {
                p.bump();
            } else {
                break;
            }
        }
        p.expect(RPAREN, "')'");
    }
    p.finish_node();
}
