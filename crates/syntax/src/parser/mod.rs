//! A recursive descent parser with a Pratt parser for expressions, over the union of the syntax of
//! every dialect. It always produces a tree that holds every byte of the input: what it cannot make
//! sense of ends up in `ERROR` nodes. It never looks at a version; the dialect only decides how the
//! text is lexed.

mod ddl;
mod dml;
mod expr;
mod query;
mod routine;
mod stmt;
mod types;

use std::ops::{Deref, DerefMut};

use rowan::{TextRange, TextSize};

use crate::Dialect;
use crate::kind::{SqlLanguage, SyntaxKind};
use crate::lexer;
use SyntaxKind::*;

pub use lsc_syntax::SyntaxError;

/// The tree of a text and the syntax errors found while building it.
pub type Parse = lsc_syntax::Parse<SqlLanguage>;

/// Parses a script of any number of statements as `dialect` lexes it. Never fails: a broken script
/// gives a tree with `ERROR` nodes and a list of errors.
pub fn parse(text: &str, dialect: Dialect) -> Parse {
    let lexed = lexer::lex(text, dialect.lex_options());
    let mut parser = Parser {
        inner: lsc_syntax::Parser::new(text, lexed.tokens),
        block_depth: 0,
        last_error: None,
    };
    for error in &lexed.errors {
        parser.error_at(
            TextRange::new(TextSize::from(error.start), TextSize::from(error.end)),
            error.message,
        );
    }
    stmt::source_file(&mut parser);
    parser.inner.finish()
}

/// The shared parser with what only SQL's grammar asks of it.
pub(crate) struct Parser<'a> {
    inner: lsc_syntax::Parser<'a, SqlLanguage>,
    /// How many `BEGIN ... END` bodies the parser is in, where `END` closes a list of statements.
    pub(crate) block_depth: u32,
    /// Where the last error was reported. A second one there follows from the first and is left out.
    last_error: Option<TextSize>,
}

impl<'a> Deref for Parser<'a> {
    type Target = lsc_syntax::Parser<'a, SqlLanguage>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Parser<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Parser<'_> {
    fn first_error_at(&mut self, offset: TextSize) -> bool {
        if self.last_error == Some(offset) {
            return false;
        }
        self.last_error = Some(offset);
        true
    }

    pub(crate) fn error_expected(&mut self, what: &str) {
        if self.first_error_at(self.after_previous_range().start()) {
            self.inner.error_expected(what);
        }
    }

    /// Reports `message` at the current token, unless the lexer reported that token already.
    pub(crate) fn error_here(&mut self, message: impl Into<String>) {
        if self.at(UNKNOWN) {
            return;
        }
        if self.first_error_at(self.current_range().start()) {
            self.inner.error_here(message);
        }
    }

    /// Consumes `kind` or reports `what` missing, in which case nothing is consumed.
    pub(crate) fn expect(&mut self, kind: SyntaxKind, what: &str) -> bool {
        if self.eat(kind) {
            return true;
        }
        self.error_expected(what);
        false
    }

    /// Wraps the current token in an `ERROR` node with a message about it.
    pub(crate) fn error_bump(&mut self) {
        if self.eof() {
            return;
        }
        let message = format!("Unexpected '{}'", self.current_text().escape_debug());
        self.error_here(message);
        self.start(ERROR);
        self.bump();
        self.finish_node();
    }

    pub(crate) fn in_block(&self) -> bool {
        self.block_depth > 0
    }

    /// Whether the `n`th upcoming token is a word that spells `word`, in any case. For the words a
    /// grammar meets too rarely to be keywords.
    pub(crate) fn nth_is_word(&self, n: usize, word: &str) -> bool {
        let kind = self.nth(n);
        (kind == IDENT || kind.is_keyword()) && self.nth_text(n).eq_ignore_ascii_case(word)
    }

    pub(crate) fn at_word(&self, word: &str) -> bool {
        self.nth_is_word(0, word)
    }

    /// Consumes `kind` or reports `what` missing.
    pub(crate) fn expect_kw(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let name = kind.name().trim_end_matches("_KW");
        self.error_expected(name);
        false
    }

    /// Whether the statement ends here: at a `;`, a delimiter `DELIMITER` set, a client command or
    /// the end of the text.
    pub(crate) fn at_statement_end(&self) -> bool {
        matches!(self.current(), SEMICOLON | CUSTOM_DELIMITER | META_COMMAND | EOF)
    }

    /// Whether nothing but whitespace stands between the current token and the start of its line.
    pub(crate) fn at_line_start(&self) -> bool {
        let offset = self.current_offset() as usize;
        let before = &self.text()[..offset];
        for byte in before.bytes().rev() {
            match byte {
                b'\n' => return true,
                b' ' | b'\t' | b'\r' => {}
                _ => return false,
            }
        }
        true
    }

    /// Consumes a run of tokens up to the end of the statement, keeping parentheses balanced, for
    /// the options of a statement whose every detail the grammar does not follow.
    pub(crate) fn bump_until_statement_end(&mut self) {
        let mut depth = 0u32;
        while !self.eof() {
            match self.current() {
                SEMICOLON | CUSTOM_DELIMITER | META_COMMAND if depth == 0 => break,
                LPAREN => depth += 1,
                RPAREN if depth == 0 => break,
                RPAREN => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    /// Consumes a balanced run of tokens from a `(` to its `)`.
    pub(crate) fn bump_balanced(&mut self) {
        if !self.at(LPAREN) {
            return;
        }
        let mut depth = 0u32;
        while !self.eof() {
            match self.current() {
                LPAREN => depth += 1,
                RPAREN => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return;
                    }
                }
                SEMICOLON | CUSTOM_DELIMITER if depth <= 1 => break,
                _ => {}
            }
            self.bump();
        }
        self.error_expected("')'");
    }
}

/// A word that can name something where a name must come: any identifier and any keyword. Whether a
/// reserved word may stand there is a question for the dialect, not for the grammar.
pub(crate) fn is_name_token(kind: SyntaxKind) -> bool {
    kind.is_identifier() || kind.is_keyword()
}

/// Keywords that never stand for a column or a table on their own in any dialect, so that an
/// expression or an implicit alias stops before them.
pub(crate) fn is_reserved(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        ALL_KW
            | AND_KW
            | AS_KW
            | ASC_KW
            | BETWEEN_KW
            | BY_KW
            | CASE_KW
            | CHECK_KW
            | COLLATE_KW
            | CONSTRAINT_KW
            | CREATE_KW
            | CROSS_KW
            | DEFAULT_KW
            | DELETE_KW
            | DESC_KW
            | DISTINCT_KW
            | DROP_KW
            | ELSE_KW
            | END_KW
            | EXCEPT_KW
            | EXISTS_KW
            | FETCH_KW
            | FOR_KW
            | FOREIGN_KW
            | FROM_KW
            | FULL_KW
            | GRANT_KW
            | GROUP_KW
            | HAVING_KW
            | IN_KW
            | INNER_KW
            | INSERT_KW
            | INTERSECT_KW
            | INTO_KW
            | IS_KW
            | JOIN_KW
            | LEFT_KW
            | LIKE_KW
            | LIMIT_KW
            | NATURAL_KW
            | NOT_KW
            | NULL_KW
            | ON_KW
            | OR_KW
            | ORDER_KW
            | OUTER_KW
            | PRIMARY_KW
            | REFERENCES_KW
            | RIGHT_KW
            | SELECT_KW
            | SET_KW
            | TABLE_KW
            | THEN_KW
            | TO_KW
            | UNION_KW
            | UNIQUE_KW
            | UPDATE_KW
            | USING_KW
            | VALUES_KW
            | WHEN_KW
            | WHERE_KW
            | WITH_KW
    )
}

/// Keywords that may name a column but end a table or a select item, so that they never become
/// an implicit alias.
pub(crate) fn is_clause_word(kind: SyntaxKind) -> bool {
    is_reserved(kind)
        || matches!(
            kind,
            OFFSET_KW
                | WINDOW_KW
                | QUALIFY_KW
                | LOCK_KW
                | STRAIGHT_JOIN_KW
                | LATERAL_KW
                | RETURNING_KW
                | ILIKE_KW
                | GLOB_KW
                | MATCH_KW
                | REGEXP_KW
                | RLIKE_KW
                | SIMILAR_KW
                | ESCAPE_KW
                | NULLS_KW
                | TABLESAMPLE_KW
                | PARTITION_KW
                | USE_KW
                | IGNORE_KW
                | FORCE_KW
                | INDEXED_KW
                | DO_KW
                | OVER_KW
                | FILTER_KW
                | WITHIN_KW
                | SEPARATOR_KW
                | SOUNDS_KW
                | MEMBER_KW
                | XOR_KW
                | DIV_KW
                | MOD_KW
                | ISNULL_KW
                | NOTNULL_KW
                | OVERLAPS_KW
                | AT_KW
                | ROWS_KW
                | ROW_KW
                | ONLY_KW
                | CONFLICT_KW
                | DUPLICATE_KW
                | MINUTE_SECOND_KW
                | UNTIL_KW
                | LOOP_KW
                | ELSEIF_KW
                | ELSIF_KW
                | REPEAT_KW
                | WHILE_KW
                | BEGIN_KW
                | OF_KW
                | NOWAIT_KW
                | SKIP_KW
                | CASCADE_KW
                | RESTRICT_KW
        )
}

/// A word that can stand for a column in an expression or follow a select item as its alias,
/// without quoting.
pub(crate) fn is_soft_name(kind: SyntaxKind) -> bool {
    kind.is_identifier() || (kind.is_keyword() && !is_reserved(kind))
}

/// A word that can be an implicit alias: a name that is no clause word.
pub(crate) fn is_alias_name(kind: SyntaxKind) -> bool {
    kind.is_identifier() || (kind.is_keyword() && !is_clause_word(kind))
}

/// Consumes a name in a `NAME` node, or reports `what` missing.
pub(crate) fn name(p: &mut Parser, what: &str) -> bool {
    if !is_name_token(p.current()) {
        p.error_expected(what);
        return false;
    }
    p.start(NAME);
    p.bump();
    p.finish_node();
    true
}

/// A name that may be qualified, `schema.table` or `db.schema.table`, in a `QUALIFIED_NAME`.
pub(crate) fn qualified_name(p: &mut Parser, what: &str) -> bool {
    if !is_name_token(p.current()) {
        p.error_expected(what);
        return false;
    }
    p.start(QUALIFIED_NAME);
    qualified_name_rest(p);
    p.finish_node();
    true
}

fn qualified_name_rest(p: &mut Parser) {
    name(p, "Name");
    while p.at(DOT) && is_name_token(p.nth(1)) {
        p.bump();
        name(p, "Name");
    }
}

/// `( name, ... )` in a `NAME_LIST`.
pub(crate) fn name_list(p: &mut Parser) {
    p.start(NAME_LIST);
    p.expect(LPAREN, "'('");
    if !p.at(RPAREN) {
        loop {
            name(p, "Column name");
            if !p.eat(COMMA) {
                break;
            }
        }
    }
    p.expect(RPAREN, "')'");
    p.finish_node();
}

/// `[AS] alias [(columns)]` after a table or a select item. With `implicit`, a name without `AS`
/// counts as an alias.
pub(crate) fn alias(p: &mut Parser, columns: bool) -> bool {
    let explicit = p.at(AS_KW);
    if !explicit && !is_alias_name(p.current()) && !(p.at(STRING) && !columns) {
        return false;
    }
    p.start(ALIAS);
    if explicit {
        p.bump();
        if p.current().is_string() {
            p.start(NAME);
            p.bump();
            p.finish_node();
        } else {
            name(p, "Alias");
        }
    } else {
        p.start(NAME);
        p.bump();
        p.finish_node();
    }
    if columns && p.at(LPAREN) {
        if column_list_has_types(p) {
            ddl::table_element_list(p);
        } else {
            name_list(p);
        }
    }
    p.finish_node();
    true
}

/// Whether a parenthesized alias list declares types, as `AS t(a int, b text)` after a function.
fn column_list_has_types(p: &Parser) -> bool {
    is_name_token(p.nth(1)) && is_name_token(p.nth(2))
}

/// An account in MySQL: `'user'@'host'`, `user@host` or `CURRENT_USER`.
pub(crate) fn account_name(p: &mut Parser) {
    p.start(ACCOUNT_NAME);
    if p.current().is_string() || is_name_token(p.current()) {
        p.bump();
        if p.at(VARIABLE) && p.nth_touches_prev() {
            p.bump();
        }
        if p.at(LPAREN) && p.nth(1) == RPAREN {
            p.bump();
            p.bump();
        }
    } else {
        p.error_expected("Account");
    }
    p.finish_node();
}
