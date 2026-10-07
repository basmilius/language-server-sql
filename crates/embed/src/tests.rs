//! Fragments built the way a PHP host builds them: from the literals, interpolations and
//! concatenations of a line of PHP, with every answer checked against the PHP text.

use std::path::PathBuf;

use crate::*;

const SHOP: &str = r#"{
    "formatVersion": 1,
    "source": { "dialect": "mysql" },
    "defaultSchema": "app",
    "schemas": [{
        "name": "app",
        "tables": [
            { "name": "orgs", "columns": [
                { "name": "id", "type": "int", "nullable": false },
                { "name": "title", "type": "varchar(100)" }
            ], "primaryKey": { "columns": ["id"] } },
            { "name": "users", "comment": "People who log in", "columns": [
                { "name": "id", "type": "int", "nullable": false, "autoIncrement": true },
                { "name": "org_id", "type": "int" },
                { "name": "email", "type": "varchar(255)", "nullable": false },
                { "name": "name", "type": "varchar(100)" },
                { "name": "status", "type": "enum('active','banned')" },
                { "name": "created_at", "type": "datetime" }
            ], "primaryKey": { "columns": ["id"] },
              "foreignKeys": [{ "columns": ["org_id"], "referencedTable": "orgs", "referencedColumns": ["id"] }] }
        ]
    }]
}"#;

fn env_with(dialect: Dialect, change: impl FnOnce(&mut Settings)) -> Environment {
    let mut settings = Settings {
        dialect,
        ..Settings::default()
    };
    change(&mut settings);
    Environment::new(settings, Some(Snapshot::parse(SHOP).unwrap()), None)
}

fn mysql() -> Environment {
    env_with(Dialect::Mysql, |_| {})
}

/// The fragment of the first string expression in a line of PHP: concatenated single-quoted,
/// double-quoted and heredoc literals, interpolations and other expressions as holes of `holes`.
fn php(source: &str, kind: FragmentKind, holes: HoleKind) -> Fragment {
    let bytes = source.as_bytes();
    let mut fragment = Fragment::new(kind);
    let mut at = source
        .find(['\'', '"'])
        .into_iter()
        .chain(source.find("<<<"))
        .min()
        .expect("a string");
    loop {
        while bytes[at] == b' ' {
            at += 1;
        }
        if bytes[at] == b'\'' {
            let mut end = at + 1;
            while bytes[end] != b'\'' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            fragment.literal(&source[at + 1..end], at as u32 + 1, EscapeStyle::SingleQuoted);
            at = end + 1;
        } else if bytes[at] == b'"' {
            let mut end = at + 1;
            while bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            interpolated(
                &mut fragment,
                source,
                at + 1,
                end,
                EscapeStyle::DoubleQuoted,
                None,
                holes,
            );
            at = end + 1;
        } else if source[at..].starts_with("<<<") {
            let label_start = at + 3;
            let label_end = label_start + source[label_start..].find('\n').unwrap();
            let label = &source[label_start..label_end];
            let body_start = label_end + 1;
            let closing = body_start + source[body_start..].find(&format!("{label};")).unwrap();
            let line_start = source[..closing].rfind('\n').unwrap() + 1;
            let indent = (closing - line_start) as u32;
            // The newline before the closing line is not part of the string.
            interpolated(
                &mut fragment,
                source,
                body_start,
                line_start - 1,
                EscapeStyle::Heredoc,
                Some(indent),
                holes,
            );
            at = closing + label.len();
        } else {
            let mut end = at;
            let mut depth = 0;
            while end < bytes.len() {
                match bytes[end] {
                    b'(' | b'[' => depth += 1,
                    b')' | b']' if depth == 0 => break,
                    b')' | b']' => depth -= 1,
                    b';' if depth == 0 => break,
                    b'.' if depth == 0 && bytes[end - 1] == b' ' => break,
                    _ => {}
                }
                end += 1;
            }
            let trimmed = source[at..end].trim_end();
            fragment.hole(Span::new(at as u32, (at + trimmed.len()) as u32), holes);
            at = end;
        }
        while at < bytes.len() && bytes[at] == b' ' {
            at += 1;
        }
        if at < bytes.len() && bytes[at] == b'.' {
            at += 1;
        } else {
            return fragment;
        }
    }
}

/// The text of a double-quoted string or a heredoc between `start` and `end`, split at `$name` and
/// `{$...}`.
fn interpolated(
    fragment: &mut Fragment,
    source: &str,
    start: usize,
    end: usize,
    style: EscapeStyle,
    indent: Option<u32>,
    holes: HoleKind,
) {
    let bytes = source.as_bytes();
    let mut text_start = start;
    let mut at = start;
    let push = |fragment: &mut Fragment, from: usize, to: usize| {
        if from == to && from != start {
            return;
        }
        match indent {
            Some(indent) => {
                let at_line_start = from == start || bytes[from - 1] == b'\n';
                fragment.literal_dedented(&source[from..to], from as u32, style, indent, at_line_start);
            }
            None => {
                fragment.literal(&source[from..to], from as u32, style);
            }
        }
    };
    while at < end {
        if bytes[at] == b'\\' {
            at += 2;
            continue;
        }
        let hole_end = if bytes[at] == b'{' && bytes.get(at + 1) == Some(&b'$') {
            Some(at + source[at..].find('}').unwrap() + 1)
        } else if bytes[at] == b'$' && bytes.get(at + 1).is_some_and(|byte| byte.is_ascii_alphabetic()) {
            let length = source[at + 1..]
                .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .unwrap_or(0);
            Some(at + 1 + length)
        } else {
            None
        };
        match hole_end {
            Some(hole_end) => {
                push(fragment, text_start, at);
                fragment.hole(Span::new(at as u32, hole_end as u32), holes);
                at = hole_end;
                text_start = at;
            }
            None => at += 1,
        }
    }
    if text_start < end || text_start == start {
        push(fragment, text_start, end);
    }
}

/// The host text of a span.
fn host(source: &str, span: Span) -> &str {
    &source[span.start as usize..span.end as usize]
}

fn offset(source: &str, needle: &str) -> u32 {
    source.find(needle).unwrap_or_else(|| panic!("{needle} in {source}")) as u32
}

fn codes(analysis: &Analysis, source: &str) -> Vec<String> {
    analysis
        .diagnostics()
        .iter()
        .map(|diagnostic| format!("{} '{}'", diagnostic.code, host(source, diagnostic.span)))
        .collect()
}

fn apply(source: &str, edits: &[Edit]) -> String {
    let mut text = source.to_string();
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    for edit in sorted {
        text.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.new_text);
    }
    text
}

#[test]
fn a_concatenation_with_a_value_reads_clean_and_maps_its_escapes() {
    let source = r"$db->query('SELECT * FROM users WHERE id = ' . $id . ' AND status = \'active\'');";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    assert_eq!(analysis.sql(), "SELECT * FROM users WHERE id = ? AND status = 'active'");
    assert_eq!(codes(&analysis, source), Vec::<String>::new());
    let strings: Vec<&str> = analysis
        .semantic_tokens()
        .iter()
        .filter(|token| TOKEN_TYPES[token.ty as usize] == "string")
        .map(|token| host(source, token.span))
        .collect();
    assert_eq!(strings, [r"\'active\'"]);
    let columns: Vec<&str> = analysis
        .semantic_tokens()
        .iter()
        .filter(|token| TOKEN_TYPES[token.ty as usize] == "property")
        .map(|token| host(source, token.span))
        .collect();
    assert_eq!(columns, ["id", "status"]);
}

#[test]
fn an_unknown_column_is_reported_where_the_host_has_it() {
    let source = r#"$db->prepare("SELECT emial FROM users WHERE name = \"Ann\"\n AND id = $id");"#;
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    assert_eq!(
        codes(&analysis, source),
        ["unresolved-column 'emial'", r#"double-quoted-string '\"Ann\"'"#]
    );
    let at = offset(source, "emial");
    let fix = analysis
        .code_actions(Span::empty(at))
        .into_iter()
        .find(|action| action.title.contains("email"))
        .expect("a near miss");
    assert_eq!(
        apply(source, &fix.edits),
        r#"$db->prepare("SELECT email FROM users WHERE name = \"Ann\"\n AND id = $id");"#
    );
}

#[test]
fn completion_escapes_what_it_writes_into_the_host_string() {
    let source = "$db->query('SELECT * FROM users WHERE status = ');";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    let cursor = offset(source, "');");
    let list = analysis.completion(cursor, CompletionOptions::default());
    let active = list
        .items
        .iter()
        .find(|item| item.label == "'active'")
        .expect("an enum value");
    assert_eq!(active.edit.span, Span::empty(cursor));
    assert_eq!(active.edit.new_text, r"\'active\'");
    let source = r#"$db->query("SELECT * FROM users u WHERE u.");"#;
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    let cursor = offset(source, "\");");
    let list = analysis.completion(cursor, CompletionOptions::default());
    let labels: Vec<&str> = list.items.iter().take(3).map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["id", "org_id", "email"]);
}

#[test]
fn a_function_snippet_keeps_its_tab_stop_in_a_double_quoted_string() {
    let source = r#"$db->query("SELECT coun FROM users");"#;
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    let cursor = offset(source, "coun") + 4;
    let list = analysis.completion(cursor, CompletionOptions::default());
    let count = list
        .items
        .iter()
        .find(|item| item.label.eq_ignore_ascii_case("count"))
        .expect("COUNT");
    assert!(count.snippet);
    assert_eq!(host(source, count.edit.span), "coun");
    assert!(count.edit.new_text.contains("($"), "{}", count.edit.new_text);
}

#[test]
fn interpolated_names_and_values_are_never_reported() {
    let source = r#"$db->query("SELECT * FROM {$table} WHERE {$column} = $value AND x$suffix = 1");"#;
    let fragment = php(source, FragmentKind::Statements, HoleKind::Identifier);
    let analysis = Analysis::new(&mysql(), &fragment);
    assert_eq!(codes(&analysis, source), Vec::<String>::new());
    assert!(analysis.to_sql(offset(source, "$table") + 2).is_none());
    let tokens = analysis.semantic_tokens();
    assert!(
        tokens
            .iter()
            .all(|token| !host(source, token.span).contains('$') && !host(source, token.span).contains('{')),
        "no token in a hole"
    );
}

#[test]
fn a_heredoc_maps_past_its_indentation() {
    let source = "$sql = <<<SQL\n        SELECT u.id, u.nope\n          FROM users u\n         WHERE u.status = 'active'\n        SQL;";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    assert_eq!(
        analysis.sql(),
        "SELECT u.id, u.nope\n  FROM users u\n WHERE u.status = 'active'"
    );
    let diagnostics = analysis.diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].span.start, offset(source, "nope"));
    let hover = analysis.hover(offset(source, "status")).expect("a hover");
    assert!(hover.markdown.contains("enum('active','banned')"), "{}", hover.markdown);
    assert_eq!(host(source, hover.span), "status");
}

#[test]
fn query_builder_parts_resolve_against_the_tables_in_scope() {
    let env = mysql();
    let users = || ScopeTable::new("users").with_alias("u");
    let check = |source: &str, kind: FragmentKind| -> Vec<String> {
        let mut fragment = php(source, kind, HoleKind::Value);
        fragment.table(users());
        codes(&Analysis::new(&env, &fragment), source)
    };
    assert_eq!(
        check("$q->where('u.status = ? AND emial = ?');", FragmentKind::Condition),
        ["unresolved-column 'emial'"]
    );
    assert_eq!(
        check("$q->orderBy('created_at desc');", FragmentKind::OrderBy),
        Vec::<String>::new()
    );
    assert_eq!(
        check("$q->select('id, name, nope');", FragmentKind::SelectList),
        ["unresolved-column 'nope'"]
    );
    assert_eq!(
        check("$q->groupBy('org_id');", FragmentKind::GroupBy),
        Vec::<String>::new()
    );
    assert_eq!(
        check("$q->having('count(*) > 1');", FragmentKind::Having),
        Vec::<String>::new()
    );
    assert_eq!(
        check("$q->selectRaw('count(*) + 1');", FragmentKind::Expression),
        Vec::<String>::new()
    );
    assert_eq!(
        check(
            "$q->join('orgs o', 'o.id', '=', 'u.org_id');",
            FragmentKind::TableReference
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        check("$q->update('name = ?, email = ?');", FragmentKind::SetList),
        Vec::<String>::new()
    );
    assert_eq!(
        check("$q->where('1 = 1');", FragmentKind::Condition),
        Vec::<String>::new(),
        "what a builder writes for an empty condition"
    );
    let source = "$q->select('id,');";
    let mut fragment = php(source, FragmentKind::SelectList, HoleKind::Value);
    fragment.table(users());
    let diagnostics = Analysis::new(&env, &fragment).diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, "syntax");
    assert_eq!(diagnostics[0].span, Span::empty(offset(source, "');")));
    let source = "$q->where('whatever = 1');";
    let fragment = php(source, FragmentKind::Condition, HoleKind::Value);
    assert_eq!(
        codes(&Analysis::new(&env, &fragment), source),
        Vec::<String>::new(),
        "without tables nothing is known"
    );
}

#[test]
fn completion_in_a_condition_offers_the_columns_in_scope() {
    let source = "$q->where('');";
    let mut fragment = php(source, FragmentKind::Condition, HoleKind::Value);
    fragment.table(ScopeTable::new("users"));
    let analysis = Analysis::new(&mysql(), &fragment);
    let cursor = offset(source, "');");
    let list = analysis.completion(cursor, CompletionOptions::default());
    let labels: Vec<&str> = list.items.iter().take(2).map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["id", "org_id"]);
    assert_eq!(list.items[0].edit.span, Span::empty(cursor));
}

#[test]
fn a_hole_that_may_be_anything_never_makes_a_syntax_error() {
    for source in [
        "$db->query('SELECT * FROM users ' . $where . ' ORDER BY id');",
        "$db->query('SELECT * FROM users WHERE ' . $conditions);",
        "$db->query('SELECT ' . $columns . ' FROM users');",
        "$db->query('SELECT * FROM ' . $table . ' WHERE id = 1');",
        "$db->query('UPDATE users SET name = 1 ' . $where);",
        "$db->query('SELECT * FROM users WHERE id IN (' . implode(',', $ids) . ')');",
    ] {
        let fragment = php(source, FragmentKind::Statements, HoleKind::Unknown);
        assert_eq!(
            codes(&Analysis::new(&mysql(), &fragment), source),
            Vec::<String>::new(),
            "{source}"
        );
    }
    let source = "$db->query('SELECT * FROM users WHERE id IN (' . implode(',', $ids) . ')');";
    let fragment = php(source, FragmentKind::Statements, HoleKind::List);
    assert_eq!(codes(&Analysis::new(&mysql(), &fragment), source), Vec::<String>::new());
}

#[test]
fn placeholders_of_the_database_layer_are_fine_in_every_dialect() {
    let source = "$db->prepare('SELECT * FROM users WHERE id=? AND org_id = :org AND name = ?');";
    for dialect in [
        Dialect::Mysql,
        Dialect::Mariadb,
        Dialect::Postgres,
        Dialect::Sqlite,
        Dialect::Generic,
    ] {
        let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
        let analysis = Analysis::new(&env_with(dialect, |_| {}), &fragment);
        assert_eq!(codes(&analysis, source), Vec::<String>::new(), "{dialect:?}");
        let parameters: Vec<&str> = analysis
            .semantic_tokens()
            .iter()
            .filter(|token| TOKEN_TYPES[token.ty as usize] == "parameter")
            .map(|token| host(source, token.span))
            .collect();
        assert_eq!(parameters.first(), Some(&"?"), "{dialect:?}");
    }
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&env_with(Dialect::Postgres, |_| {}), &fragment);
    assert!(analysis.sql().contains("id=$1"), "{}", analysis.sql());
}

#[test]
fn renames_an_alias_inside_the_fragment_and_refuses_a_table() {
    let source = "$db->query('SELECT u.id FROM users u WHERE u.email = ' . $email);";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    let at = offset(source, "u.id");
    let (span, placeholder) = analysis.prepare_rename(at).expect("an alias");
    assert_eq!((host(source, span), placeholder.as_str()), ("u", "u"));
    let edits = analysis.rename(at, "usr").expect("edits");
    assert_eq!(
        apply(source, &edits),
        "$db->query('SELECT usr.id FROM users usr WHERE usr.email = ' . $email);"
    );
    assert!(analysis.rename(offset(source, "users"), "people").is_err());
    let highlights = analysis.highlights(at);
    assert_eq!(highlights.len(), 3);
}

#[test]
fn references_compare_across_fragments_and_files() {
    let env = mysql();
    let first_source = "$db->query('SELECT email FROM users');";
    let first = Analysis::new(&env, &php(first_source, FragmentKind::Statements, HoleKind::Value));
    let references = first.references(offset(first_source, "users")).expect("a table");
    assert_eq!(references.hits.len(), 1);
    let second_source = "$q->where('users.email = ?');";
    let mut fragment = php(second_source, FragmentKind::Condition, HoleKind::Value);
    fragment.table(ScopeTable::new("users"));
    let second = Analysis::new(&env, &fragment);
    let hits: Vec<&str> = second
        .hits(&references.symbol)
        .iter()
        .map(|hit| host(second_source, hit.span))
        .collect();
    assert_eq!(hits, ["users"]);
    let path = PathBuf::from("/work/db/report.sql");
    let found = first.references_in_files(
        &references.symbol,
        &[SqlFile {
            path: &path,
            text: "SELECT * FROM users;\nSELECT 1;",
        }],
    );
    assert_eq!(found[0].spans, [Span::new(14, 19)]);
}

#[test]
fn definition_goes_to_the_ddl_of_the_workspace() {
    let mut workspace = Workspace::new(Dialect::Postgres);
    let path = PathBuf::from("/work/db/001_teams.sql");
    workspace.set_file(path.clone(), "CREATE TABLE teams (id int PRIMARY KEY, title text);");
    let settings = Settings {
        dialect: Dialect::Postgres,
        ..Settings::default()
    };
    let env = Environment::new(settings, None, workspace.schema());
    let source = "$db->query('SELECT title FROM teams');";
    let analysis = Analysis::new(&env, &php(source, FragmentKind::Statements, HoleKind::Value));
    let found = analysis.definition(offset(source, "title"));
    assert_eq!(
        found,
        [Location::File {
            path,
            span: Span::new(40, 50),
            name: Span::new(40, 45)
        }]
    );
    let source = "$db->query('SELECT titel FROM teams');";
    let analysis = Analysis::new(&env, &php(source, FragmentKind::Statements, HoleKind::Value));
    assert_eq!(codes(&analysis, source), ["unresolved-column 'titel'"]);
}

#[test]
fn the_host_sql_mode_decides_what_mysql_rejects() {
    let source = "$db->query('SELECT name, count(*) FROM users GROUP BY org_id');";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    assert_eq!(
        codes(&Analysis::new(&mysql(), &fragment), source),
        ["nonaggregated-column 'name'"]
    );
    let relaxed = env_with(Dialect::Mysql, |settings| {
        settings.sql_mode = Some("STRICT_TRANS_TABLES".to_string())
    });
    assert_eq!(codes(&Analysis::new(&relaxed, &fragment), source), Vec::<String>::new());
}

#[test]
fn signature_help_and_inlay_hints_work_in_host_offsets() {
    let source = r"$db->query('INSERT INTO orgs VALUES (' . $id . ', LEFT(\'x\', 1))');";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    let help = analysis.signature_help(offset(source, ", 1)") + 1).expect("help");
    assert_eq!(help.signatures[0].active_parameter, Some(1));
    let hints: Vec<(String, u32)> = analysis
        .inlay_hints()
        .into_iter()
        .map(|hint| (hint.label, hint.offset))
        .collect();
    assert!(
        hints
            .iter()
            .any(|(label, at)| label.starts_with("title") && *at == offset(source, "LEFT")),
        "{hints:?}"
    );
}

#[test]
fn a_quick_fix_never_edits_across_a_concatenation() {
    let source = "$db->query('SELECT * FROM users WHERE name = ' . 'NULL');";
    let fragment = php(source, FragmentKind::Statements, HoleKind::Value);
    let analysis = Analysis::new(&mysql(), &fragment);
    assert_eq!(codes(&analysis, source), ["null-comparison 'name = ' . 'NULL'"]);
    let actions = analysis.code_actions(Span::empty(offset(source, "name")));
    assert!(
        actions.iter().all(|action| !action.title.starts_with("Replace with")),
        "{actions:?}"
    );
}

/// A generator of numbers that is the same on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

#[test]
fn no_fragment_makes_mapping_panic_or_leave_the_host() {
    const WORDS: [&str; 24] = [
        "SELECT ",
        "* ",
        "FROM ",
        "users ",
        "u ",
        "WHERE ",
        "u.id ",
        "= ",
        "? ",
        ":name ",
        "AND ",
        "(",
        ")",
        ", ",
        "'it''s' ",
        "\"x\" ",
        "-- c\n",
        "/* c */",
        "status ",
        "ORDER BY ",
        "é ",
        "\n",
        "$1 ",
        "count(*) ",
    ];
    const ESCAPES: [(&str, EscapeStyle); 6] = [
        (r"\'", EscapeStyle::SingleQuoted),
        (r"\\", EscapeStyle::SingleQuoted),
        (r"\n", EscapeStyle::DoubleQuoted),
        (r#"\""#, EscapeStyle::DoubleQuoted),
        (r"\x41", EscapeStyle::Heredoc),
        ("''", EscapeStyle::Doubled(b'\'')),
    ];
    const KINDS: [FragmentKind; 9] = [
        FragmentKind::Statements,
        FragmentKind::Condition,
        FragmentKind::Having,
        FragmentKind::SelectList,
        FragmentKind::OrderBy,
        FragmentKind::GroupBy,
        FragmentKind::TableReference,
        FragmentKind::SetList,
        FragmentKind::Expression,
    ];
    const HOLES: [HoleKind; 4] = [HoleKind::Value, HoleKind::Identifier, HoleKind::List, HoleKind::Unknown];
    let envs = [
        mysql(),
        env_with(Dialect::Postgres, |_| {}),
        env_with(Dialect::Sqlite, |_| {}),
        env_with(Dialect::Mariadb, |settings| settings.sql_mode = Some("ANSI".into())),
    ];
    let mut random = Random(0x5eed_1234_abcd_0001);
    for round in 0..1000 {
        let mut fragment = Fragment::new(*random.pick(&KINDS));
        if random.below(2) == 0 {
            fragment.table(ScopeTable::new("users").with_alias("u"));
        }
        // The host text has gaps between the pieces, as quotes and concatenations leave.
        let mut source = String::from("$x = ");
        let mut covered: Vec<Span> = Vec::new();
        for _ in 0..random.below(8) + 1 {
            source.push('\'');
            if random.below(4) == 0 {
                let start = source.len() as u32;
                source.push_str("$v");
                fragment.hole(Span::new(start, start + 2), *random.pick(&HOLES));
            } else {
                let style = random.pick(&ESCAPES).1;
                let mut raw = String::new();
                for _ in 0..random.below(5) {
                    if random.below(4) == 0 {
                        let (escape, own) = *random.pick(&ESCAPES);
                        if own == style {
                            raw.push_str(escape);
                            continue;
                        }
                    }
                    raw.push_str(random.pick(&WORDS));
                }
                let start = source.len() as u32;
                source.push_str(&raw);
                fragment.literal(&raw, start, style);
                covered.push(Span::new(start, source.len() as u32));
            }
            source.push_str("' . ");
        }
        source.push(';');
        let length = source.len() as u32;
        let within = |span: Span| span.start <= span.end && span.end <= length;
        let editable = |span: Span| {
            covered
                .iter()
                .any(|piece| piece.start <= span.start && span.end <= piece.end)
        };
        let env = random.pick(&envs);
        let analysis = Analysis::new(env, &fragment);
        for diagnostic in analysis.diagnostics() {
            assert!(within(diagnostic.span), "round {round}: {diagnostic:?} in {source}");
        }
        for token in analysis.semantic_tokens() {
            assert!(
                within(token.span) && editable(token.span),
                "round {round}: {token:?} in {source}"
            );
        }
        for hint in analysis.inlay_hints() {
            assert!(hint.offset <= length);
        }
        if let Some(action) = analysis.fix_all() {
            assert!(action.edits.iter().all(|edit| editable(edit.span)));
        }
        for _ in 0..6 {
            let at = random.below(length as usize + 2) as u32;
            let list = analysis.completion(at, CompletionOptions::default());
            for item in &list.items {
                assert!(
                    editable(item.edit.span),
                    "round {round}: {:?} at {at} in {source}",
                    item.edit
                );
            }
            if let Some(hover) = analysis.hover(at) {
                assert!(within(hover.span));
            }
            for location in analysis.definition(at) {
                if let Location::Fragment { span, name } = location {
                    assert!(within(span) && within(name));
                }
            }
            let _ = analysis.signature_help(at);
            for highlight in analysis.highlights(at) {
                assert!(within(highlight.span));
            }
            for action in analysis.code_actions(Span::new(at, (at + 3).min(length))) {
                assert!(action.edits.iter().all(|edit| editable(edit.span)), "{action:?}");
            }
            if let Ok(edits) = analysis.rename(at, "renamed") {
                assert!(edits.iter().all(|edit| editable(edit.span)));
            }
            if let Some(references) = analysis.references(at) {
                assert!(references.hits.iter().all(|hit| within(hit.span)));
            }
        }
        let _ = fragment.confidence(env.target().dialect);
    }
}
