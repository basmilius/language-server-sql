use expect_test::{Expect, expect};

use crate::Dialect;
use crate::lexer::lex;

/// The tokens of a text, one per line as `KIND "text"`, then the errors.
fn tokens(text: &str, dialect: Dialect) -> String {
    let lexed = lex(text, dialect.lex_options());
    let mut out = String::new();
    let mut offset = 0usize;
    for token in &lexed.tokens {
        let piece = &text[offset..offset + token.len as usize];
        offset += token.len as usize;
        if token.kind == crate::SyntaxKind::WHITESPACE {
            continue;
        }
        out.push_str(&format!("{} {piece:?}\n", token.kind.name()));
    }
    assert_eq!(offset, text.len(), "the tokens cover every byte");
    for error in &lexed.errors {
        out.push_str(&format!("error {}..{}: {}\n", error.start, error.end, error.message));
    }
    out
}

fn check(text: &str, dialect: Dialect, expect: Expect) {
    expect.assert_eq(&tokens(text, dialect));
}

#[test]
fn words_are_keywords_in_any_case_or_identifiers() {
    check(
        "Select name, \"Quoted \"\" id\", `back``tick` frOm users_2",
        Dialect::Postgres,
        expect![[r#"
            SELECT_KW "Select"
            IDENT "name"
            COMMA ","
            QUOTED_IDENT "\"Quoted \"\" id\""
            COMMA ","
            BACKTICK_IDENT "`back``tick`"
            FROM_KW "frOm"
            IDENT "users_2"
        "#]],
    );
}

#[test]
fn double_quotes_are_strings_in_mysql_and_brackets_identifiers_in_sqlite() {
    check(
        "\"a\\\"b\" [x]",
        Dialect::Mysql,
        expect![[r#"
            STRING "\"a\\\"b\""
            LBRACKET "["
            IDENT "x"
            RBRACKET "]"
        "#]],
    );
    check(
        "\"a\" [x y]",
        Dialect::Sqlite,
        expect![[r#"
            QUOTED_IDENT "\"a\""
            BRACKET_IDENT "[x y]"
        "#]],
    );
}

#[test]
fn strings_with_prefixes_and_escapes() {
    check(
        r"'it''s' E'a\'b' X'1F' x'1f' B'01' N'n' U&'d\0061t' 'C:\' 'it\'s'",
        Dialect::Postgres,
        expect![[r#"
            STRING "'it''s'"
            ESCAPE_STRING "E'a\\'b'"
            HEX_STRING "X'1F'"
            HEX_STRING "x'1f'"
            BIT_STRING "B'01'"
            NATIONAL_STRING "N'n'"
            UNICODE_STRING "U&'d\\0061t'"
            STRING "'C:\\'"
            STRING "'it\\'"
            IDENT "s"
            STRING "'"
            error 63..64: Unterminated string
        "#]],
    );
    check(
        r"'it\'s' 'C:\\' _utf8mb4'x'",
        Dialect::Mysql,
        expect![[r#"
            STRING "'it\\'s'"
            STRING "'C:\\\\'"
            IDENT "_utf8mb4"
            STRING "'x'"
        "#]],
    );
}

#[test]
fn a_backslash_before_a_closing_quote_is_guessed_without_a_dialect() {
    check(
        r"'it\'s' LIKE 'a\_%' ESCAPE '\'",
        Dialect::Generic,
        expect![[r#"
            STRING "'it\\'s'"
            LIKE_KW "LIKE"
            STRING "'a\\_%'"
            ESCAPE_KW "ESCAPE"
            STRING "'\\'"
        "#]],
    );
}

#[test]
fn dollar_quotes_and_parameters() {
    check(
        "$$a$b$$ $fn$ x $$ y $fn$ $1 $name ?3 ? :name @a",
        Dialect::Postgres,
        expect![[r#"
            DOLLAR_STRING "$$a$b$$"
            DOLLAR_STRING "$fn$ x $$ y $fn$"
            PARAM "$1"
            UNKNOWN "$"
            IDENT "name"
            PARAM "?3"
            QUESTION "?"
            COLON ":"
            IDENT "name"
            AT "@"
            IDENT "a"
            error 28..29: Unexpected character
        "#]],
    );
    check(
        "$name @a ?3 :b",
        Dialect::Sqlite,
        expect![[r#"
            PARAM "$name"
            PARAM "@a"
            PARAM "?3"
            COLON ":"
            IDENT "b"
        "#]],
    );
    check(
        "@a @'b c' @@global.max_connections $id",
        Dialect::Mysql,
        expect![[r#"
            VARIABLE "@a"
            VARIABLE "@'b c'"
            SYSTEM_VARIABLE "@@global.max_connections"
            IDENT "$id"
        "#]],
    );
}

#[test]
fn numbers_of_every_form() {
    check(
        "1 1.5 .5 1e10 2.5E-3 0x1F 0b101 0o17 1_000",
        Dialect::Postgres,
        expect![[r#"
            INT_NUMBER "1"
            FLOAT_NUMBER "1.5"
            FLOAT_NUMBER ".5"
            FLOAT_NUMBER "1e10"
            FLOAT_NUMBER "2.5E-3"
            INT_NUMBER "0x1F"
            INT_NUMBER "0b101"
            INT_NUMBER "0o17"
            INT_NUMBER "1_000"
        "#]],
    );
    check(
        "1abc 12",
        Dialect::Mysql,
        expect![[r#"
            IDENT "1abc"
            INT_NUMBER "12"
        "#]],
    );
    check(
        "1abc",
        Dialect::Postgres,
        expect![[r#"
            INT_NUMBER "1abc"
            error 0..4: Trailing junk after numeric literal
        "#]],
    );
}

#[test]
fn comments_per_dialect() {
    check(
        "a -- line\n# hash\n/* a /* nested */ still */ b",
        Dialect::Postgres,
        expect![[r##"
            IDENT "a"
            LINE_COMMENT "-- line"
            HASH "#"
            IDENT "hash"
            BLOCK_COMMENT "/* a /* nested */ still */"
            IDENT "b"
        "##]],
    );
    check(
        "a --x\n-- line\n# hash\n/* a /* b */ c */ /*!40101 SET x */",
        Dialect::Mysql,
        expect![[r##"
            IDENT "a"
            MINUS "-"
            MINUS "-"
            IDENT "x"
            LINE_COMMENT "-- line"
            LINE_COMMENT "# hash"
            BLOCK_COMMENT "/* a /* b */"
            IDENT "c"
            STAR "*"
            SLASH "/"
            BLOCK_COMMENT "/*!40101 SET x */"
        "##]],
    );
    check(
        "/* open",
        Dialect::Sqlite,
        expect![[r#"
            BLOCK_COMMENT "/* open"
            error 0..7: Unterminated comment
        "#]],
    );
}

#[test]
fn postgres_operators_follow_its_rules() {
    check(
        "a=-1 a<@b a @> b j->>'k' j#>'{a}' x !~* y a<->b a::int a !=- b ?| ?& @@ |/ 2 a = ?",
        Dialect::Postgres,
        expect![[r##"
            IDENT "a"
            EQ "="
            MINUS "-"
            INT_NUMBER "1"
            IDENT "a"
            LT_AT "<@"
            IDENT "b"
            IDENT "a"
            AT_GT "@>"
            IDENT "b"
            IDENT "j"
            LONG_ARROW "->>"
            STRING "'k'"
            IDENT "j"
            HASH_ARROW "#>"
            STRING "'{a}'"
            IDENT "x"
            BANG_TILDE_STAR "!~*"
            IDENT "y"
            IDENT "a"
            CUSTOM_OP "<->"
            IDENT "b"
            IDENT "a"
            DOUBLE_COLON "::"
            IDENT "int"
            IDENT "a"
            CUSTOM_OP "!=-"
            IDENT "b"
            QUESTION_PIPE "?|"
            QUESTION_AMP "?&"
            AT_AT "@@"
            CUSTOM_OP "|/"
            INT_NUMBER "2"
            IDENT "a"
            EQ "="
            QUESTION "?"
        "##]],
    );
    check(
        "a=?",
        Dialect::Generic,
        expect![[r#"
            IDENT "a"
            EQ "="
            QUESTION "?"
        "#]],
    );
    check(
        "a<=>b a := 1 a->'$.x' !a",
        Dialect::Mysql,
        expect![[r#"
            IDENT "a"
            NULL_SAFE_EQ "<=>"
            IDENT "b"
            IDENT "a"
            COLON_EQ ":="
            INT_NUMBER "1"
            IDENT "a"
            ARROW "->"
            STRING "'$.x'"
            BANG "!"
            IDENT "a"
        "#]],
    );
}

#[test]
fn delimiter_changes_what_ends_a_statement() {
    check(
        "DELIMITER //\nSELECT 1; SELECT 2 //\ndelimiter ;\nSELECT 3;",
        Dialect::Mysql,
        expect![[r#"
            DELIMITER_KW "DELIMITER"
            DELIMITER_VALUE "//"
            SELECT_KW "SELECT"
            INT_NUMBER "1"
            SEMICOLON ";"
            SELECT_KW "SELECT"
            INT_NUMBER "2"
            CUSTOM_DELIMITER "//"
            DELIMITER_KW "delimiter"
            DELIMITER_VALUE ";"
            SELECT_KW "SELECT"
            INT_NUMBER "3"
            SEMICOLON ";"
        "#]],
    );
    check(
        "SELECT delimiter FROM t",
        Dialect::Mysql,
        expect![[r#"
            SELECT_KW "SELECT"
            DELIMITER_KW "delimiter"
            FROM_KW "FROM"
            IDENT "t"
        "#]],
    );
}

#[test]
fn client_commands_and_copy_data() {
    check(
        "\\connect db\nCOPY t (a) FROM stdin;\n1\tx\n\\.\nSELECT 1;",
        Dialect::Postgres,
        expect![[r#"
            META_COMMAND "\\connect db"
            COPY_KW "COPY"
            IDENT "t"
            LPAREN "("
            IDENT "a"
            RPAREN ")"
            FROM_KW "FROM"
            STDIN_KW "stdin"
            SEMICOLON ";"
            COPY_DATA "1\tx\n\\."
            SELECT_KW "SELECT"
            INT_NUMBER "1"
            SEMICOLON ";"
        "#]],
    );
    check(
        ".mode csv\nSELECT 1;",
        Dialect::Sqlite,
        expect![[r#"
            META_COMMAND ".mode csv"
            SELECT_KW "SELECT"
            INT_NUMBER "1"
            SEMICOLON ";"
        "#]],
    );
}

#[test]
fn every_prefix_of_a_text_lexes_into_tokens_that_cover_it() {
    let text = "SELECT $$a$$, 'x\\'y', \"q\", `b`, [c], E'\\n', /* c */ -- d\n@a ?1 $1 :n 1.5e3 <=> ::\n";
    for dialect in [
        Dialect::Generic,
        Dialect::Sqlite,
        Dialect::Mysql,
        Dialect::Mariadb,
        Dialect::Postgres,
    ] {
        for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
            let prefix = &text[..end];
            let lexed = lex(prefix, dialect.lex_options());
            let covered: u32 = lexed.tokens.iter().map(|token| token.len).sum();
            assert_eq!(covered as usize, prefix.len(), "{dialect}: {prefix:?}");
            assert!(lexed.tokens.iter().all(|token| token.len > 0));
        }
    }
}

#[test]
fn keywords_are_sorted_for_the_search() {
    let spellings: Vec<&str> = crate::kind::KEYWORDS.iter().map(|(spelling, _)| *spelling).collect();
    let mut sorted = spellings.clone();
    sorted.sort();
    assert_eq!(spellings, sorted);
    for (spelling, kind) in crate::kind::KEYWORDS {
        assert!(kind.is_keyword());
        assert_eq!(
            crate::SyntaxKind::from_keyword(&spelling.to_ascii_lowercase()),
            Some(*kind)
        );
        assert_eq!(kind.name(), format!("{spelling}_KW"));
    }
    assert_eq!(crate::SyntaxKind::from_keyword("users"), None);
}

#[test]
fn a_quoted_name_left_open_ends_with_its_line() {
    check(
        "SELECT * FROM `us\nWHERE `id` = 1",
        Dialect::Mariadb,
        expect![[r#"
            SELECT_KW "SELECT"
            STAR "*"
            FROM_KW "FROM"
            BACKTICK_IDENT "`us"
            WHERE_KW "WHERE"
            BACKTICK_IDENT "`id`"
            EQ "="
            INT_NUMBER "1"
            error 14..17: Unterminated quoted identifier
        "#]],
    );
    check(
        "SELECT \"em\r\nFROM \"users\"",
        Dialect::Postgres,
        expect![[r#"
            SELECT_KW "SELECT"
            QUOTED_IDENT "\"em"
            FROM_KW "FROM"
            QUOTED_IDENT "\"users\""
            error 7..10: Unterminated quoted identifier
        "#]],
    );
    check(
        "SELECT [na\nFROM [t]",
        Dialect::Sqlite,
        expect![[r#"
        SELECT_KW "SELECT"
        BRACKET_IDENT "[na"
        FROM_KW "FROM"
        BRACKET_IDENT "[t]"
        error 7..10: Unterminated quoted identifier
    "#]],
    );
    check(
        "SELECT `a\nb`",
        Dialect::Mysql,
        expect![[r#"
        SELECT_KW "SELECT"
        BACKTICK_IDENT "`a"
        IDENT "b"
        BACKTICK_IDENT "`"
        error 7..9: Unterminated quoted identifier
        error 11..12: Unterminated quoted identifier
    "#]],
    );
}
