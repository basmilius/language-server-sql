use expect_test::{Expect, expect};
use sql_syntax::{Dialect, FEATURES};

use super::*;

fn check_with(dialect: Dialect, options: FormatOptions, text: &str, expect: Expect) {
    let formatted = match try_format(text, dialect, &options) {
        Ok(formatted) => formatted,
        Err(refusal) => format!("refused: {refusal:?}"),
    };
    expect.assert_eq(&formatted);
    if let Ok(again) = try_format(&formatted, dialect, &options) {
        assert_eq!(again, formatted, "formatting twice changes nothing");
    }
}

fn check(dialect: Dialect, text: &str, expect: Expect) {
    check_with(dialect, FormatOptions::default(), text, expect);
}

#[test]
fn a_query_puts_each_clause_on_a_line() {
    check(
        Dialect::Postgres,
        "select u.id,u.email , count(*) as n from users u join orders o on o.user_id=u.id and o.total>0 left join orgs on orgs.id = u.org_id where u.status='active' and (u.name is not null or u.id<10) group by u.id, u.email having count(*)>1 order by n desc limit 10;",
        expect![[r#"
            SELECT
                u.id,
                u.email,
                count(*) AS n
            FROM users u
                JOIN orders o ON o.user_id = u.id
                    AND o.total > 0
                LEFT JOIN orgs ON orgs.id = u.org_id
            WHERE u.status = 'active'
                AND (u.name IS NOT NULL OR u.id < 10)
            GROUP BY u.id, u.email
            HAVING count(*) > 1
            ORDER BY n DESC
            LIMIT 10;"#]],
    );
}

#[test]
fn subqueries_and_common_table_expressions_go_one_level_in() {
    check(
        Dialect::Postgres,
        "WITH recent AS (SELECT * FROM orders WHERE created > now() - interval '1 day'), big AS MATERIALIZED (SELECT id FROM recent WHERE total > 100)\nSELECT * FROM users WHERE id IN (SELECT user_id FROM big) AND EXISTS (SELECT 1 FROM recent r WHERE r.user_id = users.id) UNION ALL SELECT * FROM (SELECT * FROM archived) AS a ORDER BY 1;",
        expect![[r#"
            WITH recent AS (
                SELECT *
                FROM orders
                WHERE created > now() - INTERVAL '1 day'
            ),
            big AS MATERIALIZED (
                SELECT id
                FROM recent
                WHERE total > 100
            )
            SELECT *
            FROM users
            WHERE id IN (
                SELECT user_id
                FROM big
            )
                AND EXISTS (
                    SELECT 1
                    FROM recent r
                    WHERE r.user_id = users.id
                )
            UNION ALL
            SELECT *
            FROM (
                SELECT *
                FROM archived
            ) AS a
            ORDER BY 1;"#]],
    );
}

#[test]
fn changing_data() {
    check(
        Dialect::Postgres,
        "insert into users (id, email) values (1, 'a'), (2, 'b') on conflict (id) do update set email = excluded.email, name = excluded.name returning id;\nupdate users set email = lower(email), name = 'x' from orgs where orgs.id = users.org_id returning *;\ndelete from users using orgs where orgs.id = users.org_id;\ninsert into archive select * from users where id < 10;",
        expect![[r#"
            INSERT INTO users (id, email)
            VALUES
                (1, 'a'),
                (2, 'b')
            ON CONFLICT (id) DO UPDATE SET
                email = excluded.email,
                name = excluded.name
            RETURNING id;
            UPDATE users
            SET
                email = lower(email),
                name = 'x'
            FROM orgs
            WHERE orgs.id = users.org_id
            RETURNING *;
            DELETE FROM users
            USING orgs
            WHERE orgs.id = users.org_id;
            INSERT INTO archive
            SELECT *
            FROM users
            WHERE id < 10;"#]],
    );
}

#[test]
fn definitions() {
    check(
        Dialect::Mysql,
        "create table t(id int not null auto_increment,email varchar(255) default null,total decimal(10,2),primary key(id),key idx_email(email))engine=InnoDB default charset=utf8mb4;\nalter table t add column x int, drop column y;\ncreate view v as select id from t;\ncreate index i on t (email);",
        expect![[r#"
            CREATE TABLE t (
                id int NOT NULL AUTO_INCREMENT,
                email varchar(255) DEFAULT NULL,
                total decimal(10, 2),
                PRIMARY KEY (id),
                KEY idx_email (email)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
            ALTER TABLE t
                ADD COLUMN x int,
                DROP COLUMN y;
            CREATE VIEW v AS
            SELECT id
            FROM t;
            CREATE INDEX i ON t (email);"#]],
    );
}

#[test]
fn routines_and_their_blocks() {
    check(
        Dialect::Mysql,
        "DELIMITER //\ncreate procedure p(in a int) begin declare x int default 0; if a > 1 then set x = 1; elseif a < 0 then set x = 2; else set x = 3; end if; while x < 10 do set x = x + 1; end while; select case when x > 5 then 'big' when x > 2 then 'mid' else 'small' end as size; end//\nDELIMITER ;\n",
        expect![[r#"
            DELIMITER //
            CREATE PROCEDURE p(IN a int)
            BEGIN
                DECLARE x int DEFAULT 0;
                IF a > 1 THEN
                    SET x = 1;
                ELSEIF a < 0 THEN
                    SET x = 2;
                ELSE
                    SET x = 3;
                END IF;
                WHILE x < 10 DO
                    SET x = x + 1;
                END WHILE;
                SELECT CASE
                    WHEN x > 5 THEN 'big'
                    WHEN x > 2 THEN 'mid'
                    ELSE 'small'
                END AS size;
            END //
            DELIMITER ;
        "#]],
    );
}

#[test]
fn comments_keep_their_place() {
    check(
        Dialect::Postgres,
        "-- the users\nselect id, -- the key\n  /* the address */ email\nfrom users -- all of them\n\n\n-- active only\nwhere status = 'active';\n\n\nselect 1; /* done */\n",
        expect![[r#"
            -- the users
            SELECT
                id, -- the key
                /* the address */ email
            FROM users -- all of them

            -- active only
            WHERE status = 'active';

            SELECT 1; /* done */
        "#]],
    );
}

#[test]
fn keyword_case_indentation_and_commas_are_options() {
    let options = FormatOptions {
        indent: Indent::Tab,
        keyword_case: KeywordCase::Lower,
        leading_commas: true,
    };
    check_with(
        Dialect::Postgres,
        options,
        "SELECT a, b FROM t WHERE x = 1 AND y = 2;",
        expect![[r#"
            select
            	a
            	, b
            from t
            where x = 1
            	and y = 2;"#]],
    );
    check_with(
        Dialect::Postgres,
        FormatOptions {
            keyword_case: KeywordCase::Preserve,
            indent: Indent::Spaces(2),
            ..FormatOptions::default()
        },
        "Select a, b From t;",
        expect![[r#"
            Select
              a,
              b
            From t;"#]],
    );
}

#[test]
fn a_broken_statement_is_left_as_it_is() {
    check(
        Dialect::Postgres,
        "select   a from t;\nselect from  where ;\nselect b   from u;",
        expect![[r#"
            SELECT a
            FROM t;
            select from  where ;
            SELECT b
            FROM u;"#]],
    );
}

#[test]
fn spacing_follows_what_tokens_are() {
    check(
        Dialect::Postgres,
        "SELECT -a, a-1, a::int[], arr[1:2], cast(x as text), row(1,2), array[1,2], f( ), count(DISTINCT a) FILTER(WHERE a>1) OVER(PARTITION BY b), $1, a->>'k', not b FROM t;",
        expect![[r#"
            SELECT
                -a,
                a - 1,
                a::int[],
                arr[1:2],
                CAST(x AS text),
                ROW(1, 2),
                ARRAY[1, 2],
                f(),
                count(DISTINCT a) FILTER (WHERE a > 1) OVER (PARTITION BY b),
                $1,
                a ->> 'k',
                NOT b
            FROM t;"#]],
    );
    check(
        Dialect::Sqlite,
        "select * from t where a=:name and b = ?1 and c=@x;",
        expect![[r#"
            SELECT *
            FROM t
            WHERE a = :name
                AND b = ?1
                AND c = @x;"#]],
    );
    check(
        Dialect::Mysql,
        "grant select on shop.* to 'reader'@'localhost';",
        expect![[r#"GRANT SELECT ON shop.* TO 'reader'@'localhost';"#]],
    );
    check(
        Dialect::Mysql,
        "SELECT _utf8mb4'abc', @a := 1, @@sql_mode, b'101';",
        expect![[r#"
            SELECT
                _utf8mb4'abc',
                @a := 1,
                @@sql_mode,
                b'101';"#]],
    );
}

#[test]
fn data_and_client_commands_stay_as_they_are() {
    check(
        Dialect::Postgres,
        "copy t from stdin;\n1\t2\n\\.\n\\set x 1\nselect 1;\n",
        expect![[r#"
            copy t from stdin;
            1	2
            \.
            \set x 1
            SELECT 1;
        "#]],
    );
}

#[test]
fn a_range_formats_only_its_lines() {
    let text = "select a from t;\nselect b from u;\n";
    let options = FormatOptions::default();
    let edits = range_edits(text, Dialect::Postgres, 17, 20, &options).expect("formats");
    assert_eq!(apply(text, &edits), "select a from t;\nSELECT b\nFROM u;\n");
    let typed = on_type_edits(text, Dialect::Postgres, 16, ';', &options).expect("formats");
    assert_eq!(apply(text, &typed), "SELECT a\nFROM t;\nselect b from u;\n");
    assert!(on_type_edits(text, Dialect::Postgres, 3, ';', &options).is_none());
}

/// Every case of the corpus and the example of every row of the feature table, in every dialect
/// and with every option: the tokens stay the same, and formatting twice changes nothing.
#[test]
fn the_corpus_keeps_its_tokens_and_formats_once() {
    let corpus = include_str!("../../syntax/tests/data/dialects.sql");
    let mut texts: Vec<String> = vec![corpus.to_string()];
    texts.extend(FEATURES.iter().map(|feature| feature.example.to_string()));
    texts.extend(corpus.split("\n-- case:").map(|case| format!("-- case:{case}")));
    let options = [
        FormatOptions::default(),
        FormatOptions {
            indent: Indent::Tab,
            keyword_case: KeywordCase::Lower,
            leading_commas: true,
        },
        FormatOptions {
            keyword_case: KeywordCase::Preserve,
            ..FormatOptions::default()
        },
    ];
    let mut refused = Vec::new();
    for dialect in [
        Dialect::Generic,
        Dialect::Sqlite,
        Dialect::Mysql,
        Dialect::Mariadb,
        Dialect::Postgres,
    ] {
        for options in &options {
            for text in &texts {
                match try_format(text, dialect, options) {
                    Ok(formatted) => {
                        assert!(
                            same_tokens(text, &formatted, dialect),
                            "{dialect:?}: {text}\n{formatted}"
                        );
                        let again = try_format(&formatted, dialect, options).expect("formats again");
                        assert_eq!(again, formatted, "{dialect:?}: formats once\n{text}");
                    }
                    Err(_) => refused.push(format!("{dialect:?}: {text}")),
                }
            }
        }
    }
    assert!(refused.is_empty(), "refused:\n{}", refused.join("\n"));
}

/// Scripts as people write them, in their own dialect and without one.
#[test]
fn the_samples_keep_their_tokens_and_format_once() {
    let samples = [
        (Dialect::Postgres, include_str!("../tests/data/postgres.sql")),
        (Dialect::Mysql, include_str!("../tests/data/mysql.sql")),
        (Dialect::Mariadb, include_str!("../tests/data/mysql.sql")),
        (Dialect::Sqlite, include_str!("../tests/data/sqlite.sql")),
        (Dialect::Generic, include_str!("../tests/data/postgres.sql")),
    ];
    for (dialect, text) in samples {
        for options in [
            FormatOptions::default(),
            FormatOptions {
                indent: Indent::Tab,
                keyword_case: KeywordCase::Lower,
                leading_commas: true,
            },
        ] {
            let formatted =
                try_format(text, dialect, &options).unwrap_or_else(|refusal| panic!("{dialect:?}: {refusal:?}"));
            assert!(same_tokens(text, &formatted, dialect));
            assert_eq!(
                try_format(&formatted, dialect, &options).as_deref(),
                Ok(formatted.as_str()),
                "{dialect:?}"
            );
        }
    }
}
