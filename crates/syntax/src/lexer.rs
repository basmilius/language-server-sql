//! Cuts a text into tokens that together hold every byte of it. What a byte means differs between
//! the dialects (`#` starts a comment in MySQL and is an operator in PostgreSQL, a double quote
//! starts a string in one and an identifier in the other), so the lexer takes [`LexOptions`],
//! which [`crate::Dialect::lex_options`] gives for each dialect. Everything else is the union of
//! the dialects: a construct one dialect lacks is lexed anyway and reported by the feature table.

use lsc_syntax::Token;

use crate::SyntaxKind::{self, *};

/// What differs between the dialects in how a text is cut into tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexOptions {
    /// `#` starts a comment to the end of the line.
    pub hash_comments: bool,
    /// A backslash escapes the next character in a string.
    pub backslash_escapes: bool,
    /// With `backslash_escapes`, a backslash right before a quote that ends the string where it
    /// stands (before whitespace, `,`, `)`, `;` or the end) is a backslash: `'C:\'` and `ESCAPE '\'`
    /// read as in the standard while `'it\'s'` reads as in MySQL. For a text of unknown dialect.
    pub guess_backslash_quotes: bool,
    /// `"..."` is a string, not an identifier.
    pub double_quoted_strings: bool,
    /// `[...]` is an identifier.
    pub bracket_identifiers: bool,
    /// `/* /* */ */` nests.
    pub nested_comments: bool,
    /// `$$...$$` and `$tag$...$tag$` are strings.
    pub dollar_quotes: bool,
    /// Operators are read the way PostgreSQL reads them: any run of operator characters.
    pub postgres_operators: bool,
    /// `--` starts a comment only when whitespace follows it.
    pub dash_comment_needs_space: bool,
    /// `@name` and `@@name` are variables.
    pub mysql_variables: bool,
    /// `@name`, `$name` and `?NNN` are parameters.
    pub prefixed_parameters: bool,
    /// A `?` is a placeholder of its own and never the end of an operator, so that `a=?` is `=`
    /// and `?`. PostgreSQL itself reads `=?` as one operator.
    pub question_placeholders: bool,
    /// `$name` is an identifier.
    pub dollar_identifiers: bool,
    /// A word may start with digits, as in `1st_place`.
    pub digit_identifiers: bool,
    /// `1_000` is one number.
    pub digit_separators: bool,
    /// `DELIMITER //` at the start of a statement changes what ends a statement.
    pub delimiter_command: bool,
    /// A backslash outside a string starts a command of the client, to the end of the line.
    pub backslash_commands: bool,
    /// A line starting with `.` at the start of a statement is a command of the shell.
    pub sqlite_dot_commands: bool,
}

/// A piece of the text the lexer could not read as it should, such as an unterminated string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub start: u32,
    pub end: u32,
    pub message: &'static str,
}

/// The tokens of a text and the errors found while cutting it.
#[derive(Clone, Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token<SyntaxKind>>,
    pub errors: Vec<LexError>,
}

/// Cuts `text` into tokens.
pub fn lex(text: &str, options: LexOptions) -> Lexed {
    let mut lexer = Lexer {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        options,
        tokens: Vec::with_capacity(text.len() / 4),
        errors: Vec::new(),
        delimiter: None,
        statement_start: true,
        copy: Copy::None,
        copy_data_pending: false,
    };
    lexer.run();
    Lexed {
        tokens: lexer.tokens,
        errors: lexer.errors,
    }
}

/// How far a `COPY ... FROM STDIN` has come, after which the lines up to `\.` are data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copy {
    None,
    Started,
    From,
    Stdin,
}

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    options: LexOptions,
    tokens: Vec<Token<SyntaxKind>>,
    errors: Vec<LexError>,
    /// What `DELIMITER` set, unless that is `;`.
    delimiter: Option<String>,
    /// Nothing but trivia since the last statement ended.
    statement_start: bool,
    copy: Copy,
    copy_data_pending: bool,
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The characters PostgreSQL builds an operator from.
fn is_operator_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'+' | b'-' | b'*' | b'/' | b'<' | b'>' | b'=' | b'~' | b'!' | b'@' | b'#' | b'%' | b'^' | b'&' | b'|' | b'?'
    )
}

/// The operators of MySQL, MariaDB and SQLite, longest first.
const FIXED_OPERATORS: &[(&str, SyntaxKind)] = &[
    ("<=>", NULL_SAFE_EQ),
    ("->>", LONG_ARROW),
    ("->", ARROW),
    ("<<", SHL),
    (">>", SHR),
    ("<=", LTE),
    (">=", GTE),
    ("<>", NEQ),
    ("!=", BANG_EQ),
    ("==", EQ_EQ),
    ("||", PIPE_PIPE),
    ("&&", AMP_AMP),
    ("=", EQ),
    ("<", LT),
    (">", GT),
    ("!", BANG),
    ("~", TILDE),
    ("^", CARET),
    ("&", AMP),
    ("|", PIPE),
    ("+", PLUS),
    ("-", MINUS),
    ("*", STAR),
    ("/", SLASH),
    ("%", PERCENT),
];

fn postgres_operator_kind(text: &str) -> SyntaxKind {
    match text {
        "+" => PLUS,
        "-" => MINUS,
        "*" => STAR,
        "/" => SLASH,
        "%" => PERCENT,
        "^" => CARET,
        "=" => EQ,
        "<" => LT,
        ">" => GT,
        "<=" => LTE,
        ">=" => GTE,
        "<>" => NEQ,
        "!=" => BANG_EQ,
        "||" => PIPE_PIPE,
        "&&" => AMP_AMP,
        "|" => PIPE,
        "&" => AMP,
        "~" => TILDE,
        "!" => BANG,
        "<<" => SHL,
        ">>" => SHR,
        "->" => ARROW,
        "->>" => LONG_ARROW,
        "=>" => FAT_ARROW,
        "#" => HASH,
        "#>" => HASH_ARROW,
        "#>>" => HASH_LONG_ARROW,
        "#-" => HASH_MINUS,
        "@>" => AT_GT,
        "<@" => LT_AT,
        "@@" => AT_AT,
        "@" => AT,
        "?" => QUESTION,
        "?|" => QUESTION_PIPE,
        "?&" => QUESTION_AMP,
        "~*" => TILDE_STAR,
        "!~" => BANG_TILDE,
        "!~*" => BANG_TILDE_STAR,
        _ => CUSTOM_OP,
    }
}

impl Lexer<'_> {
    fn byte(&self, at: usize) -> u8 {
        self.bytes.get(at).copied().unwrap_or(0)
    }

    fn error(&mut self, start: usize, message: &'static str) {
        self.errors.push(LexError {
            start: start as u32,
            end: self.pos as u32,
            message,
        });
    }

    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            let start = self.pos;
            if self.copy_data_pending && self.copy_data() {
                continue;
            }
            if self.statement_start && self.script_line() {
                continue;
            }
            let kind = self.token();
            debug_assert!(self.pos > start, "every token takes at least one byte");
            self.push(kind, start);
        }
    }

    fn push(&mut self, kind: SyntaxKind, start: usize) {
        self.tokens.push(Token {
            kind,
            len: (self.pos - start) as u32,
        });
        if kind.is_trivia() {
            return;
        }
        match kind {
            SEMICOLON | CUSTOM_DELIMITER => {
                self.copy_data_pending = self.copy == Copy::Stdin;
                self.copy = Copy::None;
                self.statement_start = true;
            }
            META_COMMAND | COPY_DATA | DELIMITER_VALUE => {
                self.copy = Copy::None;
                self.statement_start = true;
            }
            _ => {
                self.copy = match (self.statement_start, self.copy, kind) {
                    (true, _, COPY_KW) => Copy::Started,
                    (true, _, _) => Copy::None,
                    (false, Copy::Started, FROM_KW) => Copy::From,
                    (false, Copy::From, STDIN_KW) => Copy::Stdin,
                    (false, Copy::From, _) => Copy::Started,
                    (false, state, _) => state,
                };
                self.statement_start = false;
            }
        }
    }

    /// After `COPY ... FROM STDIN;`, the lines after the one with the `;` are data up to a line
    /// that is `\.`. Gives whether it consumed anything.
    fn copy_data(&mut self) -> bool {
        let start = self.pos;
        let byte = self.byte(self.pos);
        if byte == b' ' || byte == b'\t' || byte == b'\r' {
            while matches!(self.byte(self.pos), b' ' | b'\t' | b'\r') {
                self.pos += 1;
            }
            if self.byte(self.pos) == b'\n' {
                self.pos += 1;
                self.push(WHITESPACE, start);
                self.copy_data_pending = false;
                self.copy_data_body();
            } else {
                self.push(WHITESPACE, start);
            }
            return true;
        }
        if byte == b'\n' {
            self.pos += 1;
            self.push(WHITESPACE, start);
            self.copy_data_pending = false;
            self.copy_data_body();
            return true;
        }
        if self.text[self.pos..].starts_with("--") || self.text[self.pos..].starts_with("/*") {
            return false;
        }
        self.copy_data_pending = false;
        false
    }

    fn copy_data_body(&mut self) {
        let start = self.pos;
        if start >= self.bytes.len() {
            return;
        }
        loop {
            let line_end = self.text[self.pos..]
                .find('\n')
                .map_or(self.bytes.len(), |offset| self.pos + offset);
            let line = self.text[self.pos..line_end].trim_end_matches('\r');
            if line == "\\." {
                self.pos += 2;
                break;
            }
            if line_end >= self.bytes.len() {
                self.pos = self.bytes.len();
                break;
            }
            self.pos = line_end + 1;
        }
        self.push(COPY_DATA, start);
    }

    /// What a client reads at the start of a statement besides SQL: `DELIMITER` and the dot
    /// commands of SQLite's shell. Gives whether it consumed anything.
    fn script_line(&mut self) -> bool {
        let rest = &self.text[self.pos..];
        if self.options.delimiter_command && rest.len() >= 9 && rest.as_bytes()[..9].eq_ignore_ascii_case(b"delimiter")
        {
            let after = self.byte(self.pos + 9);
            if after == b' ' || after == b'\t' {
                let start = self.pos;
                self.pos += 9;
                self.push(DELIMITER_KW, start);
                let start = self.pos;
                while matches!(self.byte(self.pos), b' ' | b'\t') {
                    self.pos += 1;
                }
                self.push(WHITESPACE, start);
                let start = self.pos;
                while self.pos < self.bytes.len() && !is_whitespace(self.byte(self.pos)) {
                    self.pos += 1;
                }
                if self.pos > start {
                    let value = &self.text[start..self.pos];
                    self.delimiter = (value != ";").then(|| value.to_string());
                    self.push(DELIMITER_VALUE, start);
                }
                return true;
            }
        }
        if self.options.sqlite_dot_commands
            && rest.starts_with('.')
            && self.byte(self.pos + 1).is_ascii_alphabetic()
            && (self.pos == 0 || self.byte(self.pos - 1) == b'\n')
        {
            let start = self.pos;
            self.skip_to_line_end();
            self.push(META_COMMAND, start);
            return true;
        }
        false
    }

    fn skip_to_line_end(&mut self) {
        while self.pos < self.bytes.len() && !matches!(self.byte(self.pos), b'\n' | b'\r') {
            self.pos += 1;
        }
    }

    fn token(&mut self) -> SyntaxKind {
        if let Some(delimiter) = &self.delimiter {
            if self.text[self.pos..].starts_with(delimiter.as_str()) {
                self.pos += delimiter.len();
                return CUSTOM_DELIMITER;
            }
        }
        let byte = self.byte(self.pos);
        let next = self.byte(self.pos + 1);
        match byte {
            _ if is_whitespace(byte) => {
                while self.pos < self.bytes.len() && is_whitespace(self.byte(self.pos)) {
                    self.pos += 1;
                }
                WHITESPACE
            }
            b'-' if next == b'-' => {
                let after = self.byte(self.pos + 2);
                if !self.options.dash_comment_needs_space || after <= b' ' {
                    self.skip_to_line_end();
                    LINE_COMMENT
                } else {
                    self.operator()
                }
            }
            b'#' if self.options.hash_comments => {
                self.skip_to_line_end();
                LINE_COMMENT
            }
            b'/' if next == b'*' => self.block_comment(),
            b'\'' => self.string(STRING, self.options.backslash_escapes),
            b'"' if self.options.double_quoted_strings => self.string(STRING, self.options.backslash_escapes),
            b'"' => self.quoted(b'"', QUOTED_IDENT),
            b'`' => self.quoted(b'`', BACKTICK_IDENT),
            b'[' if self.options.bracket_identifiers => self.bracketed(),
            b'0'..=b'9' => self.number(),
            b'.' if next.is_ascii_digit() => self.number(),
            b'$' => self.dollar(),
            b'@' => self.at(),
            b'?' => self.question(),
            b':' => {
                self.pos += 1;
                match next {
                    b':' => {
                        self.pos += 1;
                        DOUBLE_COLON
                    }
                    b'=' => {
                        self.pos += 1;
                        COLON_EQ
                    }
                    _ => COLON,
                }
            }
            b'\\' if self.options.backslash_commands => {
                self.skip_to_line_end();
                META_COMMAND
            }
            b'(' => self.single(LPAREN),
            b')' => self.single(RPAREN),
            b'[' => self.single(LBRACKET),
            b']' => self.single(RBRACKET),
            b'{' => self.single(LBRACE),
            b'}' => self.single(RBRACE),
            b',' => self.single(COMMA),
            b';' => self.single(SEMICOLON),
            b'.' => self.single(DOT),
            _ if is_word_start(byte) => self.word(),
            _ => self.operator(),
        }
    }

    fn single(&mut self, kind: SyntaxKind) -> SyntaxKind {
        self.pos += 1;
        kind
    }

    fn block_comment(&mut self) -> SyntaxKind {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1u32;
        loop {
            if self.pos >= self.bytes.len() {
                self.error(start, "Unterminated comment");
                break;
            }
            let byte = self.byte(self.pos);
            let next = self.byte(self.pos + 1);
            if byte == b'*' && next == b'/' {
                self.pos += 2;
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if byte == b'/' && next == b'*' && self.options.nested_comments {
                self.pos += 2;
                depth += 1;
            } else {
                self.pos += 1;
            }
        }
        BLOCK_COMMENT
    }

    /// A string from the quote at the cursor to its closing quote, where a doubled quote stands
    /// for one and, with `escapes`, a backslash escapes the next character.
    fn string(&mut self, kind: SyntaxKind, escapes: bool) -> SyntaxKind {
        let start = self.pos;
        let quote = self.byte(self.pos);
        self.pos += 1;
        loop {
            if self.pos >= self.bytes.len() {
                self.pos = self.bytes.len();
                self.error(start, "Unterminated string");
                break;
            }
            let byte = self.byte(self.pos);
            if escapes && byte == b'\\' {
                let ends_here = self.byte(self.pos + 1) == quote
                    && matches!(
                        self.byte(self.pos + 2),
                        0 | b' ' | b'\t' | b'\n' | b'\r' | b',' | b')' | b';'
                    );
                if !(ends_here && self.options.guess_backslash_quotes && kind == STRING) {
                    self.pos += 2;
                    continue;
                }
            }
            if byte == quote {
                if self.byte(self.pos + 1) == quote {
                    self.pos += 2;
                    continue;
                }
                self.pos += 1;
                break;
            }
            self.pos += 1;
        }
        kind
    }

    fn quoted(&mut self, quote: u8, kind: SyntaxKind) -> SyntaxKind {
        let start = self.pos;
        self.pos += 1;
        loop {
            if self.pos >= self.bytes.len() {
                self.error(start, "Unterminated quoted identifier");
                break;
            }
            if self.byte(self.pos) == quote {
                if self.byte(self.pos + 1) == quote {
                    self.pos += 2;
                    continue;
                }
                self.pos += 1;
                break;
            }
            self.pos += 1;
        }
        kind
    }

    fn bracketed(&mut self) -> SyntaxKind {
        let start = self.pos;
        match self.text[self.pos..].find(']') {
            Some(offset) => self.pos += offset + 1,
            None => {
                self.pos = self.bytes.len();
                self.error(start, "Unterminated quoted identifier");
            }
        }
        BRACKET_IDENT
    }

    fn digits(&mut self, accept: fn(u8) -> bool) {
        while self.pos < self.bytes.len() {
            let byte = self.byte(self.pos);
            let separator = byte == b'_'
                && self.options.digit_separators
                && self.pos > 0
                && accept(self.byte(self.pos - 1))
                && accept(self.byte(self.pos + 1));
            if !accept(byte) && !separator {
                break;
            }
            self.pos += 1;
        }
    }

    fn number(&mut self) -> SyntaxKind {
        let start = self.pos;
        let prefix = self.byte(self.pos + 1).to_ascii_lowercase();
        let based: Option<fn(u8) -> bool> = match prefix {
            b'x' => Some(|byte: u8| byte.is_ascii_hexdigit()),
            b'b' => Some(|byte: u8| byte == b'0' || byte == b'1'),
            b'o' => Some(|byte: u8| (b'0'..=b'7').contains(&byte)),
            _ => None,
        };
        let mut kind = INT_NUMBER;
        match based {
            Some(accept) if self.byte(self.pos) == b'0' && accept(self.byte(self.pos + 2)) => {
                self.pos += 2;
                self.digits(accept);
            }
            _ => {
                self.digits(|byte| byte.is_ascii_digit());
                if self.byte(self.pos) == b'.' && self.byte(self.pos + 1) != b'.' {
                    self.pos += 1;
                    self.digits(|byte| byte.is_ascii_digit());
                    kind = FLOAT_NUMBER;
                }
                let exponent = self.byte(self.pos);
                if exponent == b'e' || exponent == b'E' {
                    let sign = self.byte(self.pos + 1);
                    let first = if sign == b'+' || sign == b'-' { 2 } else { 1 };
                    if self.byte(self.pos + first).is_ascii_digit() {
                        self.pos += first;
                        self.digits(|byte| byte.is_ascii_digit());
                        kind = FLOAT_NUMBER;
                    }
                }
            }
        }
        let after = self.byte(self.pos);
        if is_word_byte(after) && after != b'$' {
            self.word_rest();
            if self.options.digit_identifiers {
                return IDENT;
            }
            self.error(start, "Trailing junk after numeric literal");
        }
        kind
    }

    fn word_rest(&mut self) {
        while self.pos < self.bytes.len() && is_word_byte(self.byte(self.pos)) {
            self.pos += 1;
        }
    }

    fn word(&mut self) -> SyntaxKind {
        let first = self.byte(self.pos).to_ascii_uppercase();
        let next = self.byte(self.pos + 1);
        if next == b'\'' {
            let prefixed = match first {
                b'E' => Some((ESCAPE_STRING, true)),
                b'N' => Some((NATIONAL_STRING, self.options.backslash_escapes)),
                b'X' => Some((HEX_STRING, false)),
                b'B' => Some((BIT_STRING, false)),
                _ => None,
            };
            if let Some((kind, escapes)) = prefixed {
                self.pos += 1;
                return self.string(kind, escapes);
            }
        }
        if first == b'U' && next == b'&' {
            match self.byte(self.pos + 2) {
                b'\'' => {
                    self.pos += 2;
                    return self.string(UNICODE_STRING, false);
                }
                b'"' => {
                    self.pos += 2;
                    return self.quoted(b'"', QUOTED_IDENT);
                }
                _ => {}
            }
        }
        let start = self.pos;
        self.word_rest();
        SyntaxKind::from_keyword(&self.text[start..self.pos]).unwrap_or(IDENT)
    }

    fn dollar(&mut self) -> SyntaxKind {
        let start = self.pos;
        let next = self.byte(self.pos + 1);
        if self.options.dollar_quotes {
            let mut end = self.pos + 1;
            if is_word_start(next) {
                end += 1;
                while end < self.bytes.len() && is_word_byte(self.byte(end)) && self.byte(end) != b'$' {
                    end += 1;
                }
            }
            if self.byte(end) == b'$' {
                let tag = &self.text[self.pos..=end];
                self.pos = end + 1;
                match self.text[self.pos..].find(tag) {
                    Some(offset) => self.pos += offset + tag.len(),
                    None => {
                        self.pos = self.bytes.len();
                        self.error(start, "Unterminated dollar-quoted string");
                    }
                }
                return DOLLAR_STRING;
            }
        }
        if next.is_ascii_digit() {
            self.pos += 1;
            while self.byte(self.pos).is_ascii_digit() {
                self.pos += 1;
            }
            return PARAM;
        }
        // SQLite's parameters may hold dollar signs, as `$$a$$`.
        let parameter_start = is_word_start(next) || next == b'$' && self.options.prefixed_parameters;
        if parameter_start && (self.options.prefixed_parameters || self.options.dollar_identifiers) {
            self.pos += 1;
            self.word_rest();
            return if self.options.prefixed_parameters { PARAM } else { IDENT };
        }
        self.pos += 1;
        self.error(start, "Unexpected character");
        UNKNOWN
    }

    fn at(&mut self) -> SyntaxKind {
        let start = self.pos;
        let next = self.byte(self.pos + 1);
        if self.options.mysql_variables {
            if next == b'@' && is_word_start(self.byte(self.pos + 2)) {
                self.pos += 2;
                self.word_rest();
                if self.byte(self.pos) == b'.' && is_word_start(self.byte(self.pos + 1)) {
                    self.pos += 1;
                    self.word_rest();
                }
                return SYSTEM_VARIABLE;
            }
            if is_word_byte(next) && next != b'$' {
                self.pos += 1;
                self.word_rest();
                return VARIABLE;
            }
            if matches!(next, b'\'' | b'"' | b'`') {
                self.pos += 1;
                if next == b'`' {
                    self.quoted(b'`', VARIABLE);
                } else {
                    self.string(VARIABLE, self.options.backslash_escapes);
                }
                return VARIABLE;
            }
        }
        if self.options.prefixed_parameters && is_word_start(next) {
            self.pos += 1;
            self.word_rest();
            return PARAM;
        }
        if self.options.postgres_operators {
            return self.operator();
        }
        self.pos += 1;
        self.error(start, "Unexpected character");
        UNKNOWN
    }

    fn question(&mut self) -> SyntaxKind {
        if self.byte(self.pos + 1).is_ascii_digit() {
            self.pos += 1;
            while self.byte(self.pos).is_ascii_digit() {
                self.pos += 1;
            }
            return PARAM;
        }
        if self.options.postgres_operators {
            return self.operator();
        }
        self.pos += 1;
        QUESTION
    }

    fn operator(&mut self) -> SyntaxKind {
        let start = self.pos;
        if self.options.postgres_operators && is_operator_byte(self.byte(self.pos)) {
            return self.postgres_operator();
        }
        let rest = &self.text[self.pos..];
        for (text, kind) in FIXED_OPERATORS {
            if rest.starts_with(text) {
                self.pos += text.len();
                return *kind;
            }
        }
        self.pos += rest.chars().next().map_or(1, char::len_utf8);
        self.error(start, "Unexpected character");
        UNKNOWN
    }

    /// An operator by PostgreSQL's rules: the longest run of operator characters that holds no
    /// `--` or `/*`, without trailing `+` and `-` unless it holds one of `~!@#%^&|?`, so that
    /// `a=-1` is `=` and `-`.
    fn postgres_operator(&mut self) -> SyntaxKind {
        let start = self.pos;
        let mut end = self.pos;
        while end < self.bytes.len() && is_operator_byte(self.byte(end)) {
            if end > start {
                let pair = (self.byte(end), self.byte(end + 1));
                if pair == (b'-', b'-') || pair == (b'/', b'*') {
                    break;
                }
                if self.options.question_placeholders && self.byte(end) == b'?' {
                    break;
                }
            }
            end += 1;
        }
        let special = |byte: &u8| matches!(byte, b'~' | b'!' | b'@' | b'#' | b'%' | b'^' | b'&' | b'|' | b'?');
        if end - start > 1 && !self.bytes[start..end].iter().any(special) {
            while end - start > 1 && matches!(self.byte(end - 1), b'+' | b'-') {
                end -= 1;
            }
        }
        self.pos = end;
        postgres_operator_kind(&self.text[start..end])
    }
}
