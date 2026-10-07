//! A fragment as the host holds it: the pieces of its strings with their offsets in the host
//! document, the holes between them, what kind of SQL it is and the tables a partial fragment
//! sees.

/// A range of bytes, in the host document or in the SQL text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    pub fn empty(at: u32) -> Span {
        Span { start: at, end: at }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether an offset is in the span or at its end.
    pub fn touches(self, offset: u32) -> bool {
        self.start <= offset && offset <= self.end
    }
}

/// The rules of the host string a piece comes from: which escapes its source text holds, and how
/// text an edit writes into it is escaped again. The names are those of PHP's literals; another
/// host picks the one whose rules its own string follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EscapeStyle {
    /// No escapes: a nowdoc, or text the host already decoded.
    #[default]
    Verbatim,
    /// `'...'`: `\'` and `\\` are escapes, any other backslash is itself.
    SingleQuoted,
    /// `"..."`: `\n`, `\t`, `\r`, `\v`, `\e`, `\f`, `\\`, `\$`, `\"`, octal `\0` to `\377`,
    /// `\xHH` and `\u{HHHH}`; any other backslash is itself.
    DoubleQuoted,
    /// A heredoc: the escapes of `DoubleQuoted` but `\"`, which keeps its backslash.
    Heredoc,
    /// A quote written twice is one, as in SQL's own `'it''s'`.
    Doubled(u8),
}

impl EscapeStyle {
    /// The source text that reads back as `text` in a string of this style.
    pub fn encode(self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for character in text.chars() {
            match (self, character) {
                (EscapeStyle::SingleQuoted, '\\' | '\'') => {
                    out.push('\\');
                    out.push(character);
                }
                (EscapeStyle::DoubleQuoted, '\\' | '"' | '$') | (EscapeStyle::Heredoc, '\\' | '$') => {
                    out.push('\\');
                    out.push(character);
                }
                (EscapeStyle::Doubled(quote), _) if character == quote as char => {
                    out.push(character);
                    out.push(character);
                }
                _ => out.push(character),
            }
        }
        out
    }

    /// The escape at the start of `raw`: its length in the source and the text it stands for.
    fn escape_at(self, raw: &[u8]) -> Option<(usize, String)> {
        match self {
            EscapeStyle::Verbatim => None,
            EscapeStyle::Doubled(quote) => {
                (raw.first() == Some(&quote) && raw.get(1) == Some(&quote)).then(|| (2, (quote as char).to_string()))
            }
            EscapeStyle::SingleQuoted => match (raw.first(), raw.get(1)) {
                (Some(b'\\'), Some(next @ (b'\\' | b'\''))) => Some((2, (*next as char).to_string())),
                _ => None,
            },
            EscapeStyle::DoubleQuoted | EscapeStyle::Heredoc => {
                if raw.first() != Some(&b'\\') {
                    return None;
                }
                let simple = match raw.get(1)? {
                    b'n' => Some('\n'),
                    b't' => Some('\t'),
                    b'r' => Some('\r'),
                    b'v' => Some('\u{b}'),
                    b'e' => Some('\u{1b}'),
                    b'f' => Some('\u{c}'),
                    b'\\' => Some('\\'),
                    b'$' => Some('$'),
                    b'"' if self == EscapeStyle::DoubleQuoted => Some('"'),
                    _ => None,
                };
                if let Some(character) = simple {
                    return Some((2, character.to_string()));
                }
                let digits = |from: usize, most: usize, radix: u32| {
                    raw[from..]
                        .iter()
                        .take(most)
                        .take_while(|byte| (**byte as char).is_digit(radix))
                        .count()
                };
                let byte_text = |value: u32| match char::from_u32(value) {
                    Some(character) if value < 0x80 => character.to_string(),
                    _ => char::REPLACEMENT_CHARACTER.to_string(),
                };
                match raw[1] {
                    b'0'..=b'7' => {
                        let count = digits(1, 3, 8);
                        let text = std::str::from_utf8(&raw[1..1 + count]).ok()?;
                        let value = u32::from_str_radix(text, 8).ok()? & 0xff;
                        Some((1 + count, byte_text(value)))
                    }
                    b'x' => {
                        let count = digits(2, 2, 16);
                        if count == 0 {
                            return None;
                        }
                        let text = std::str::from_utf8(&raw[2..2 + count]).ok()?;
                        Some((2 + count, byte_text(u32::from_str_radix(text, 16).ok()?)))
                    }
                    b'u' if raw.get(2) == Some(&b'{') => {
                        let count = digits(3, 8, 16);
                        if count == 0 || raw.get(3 + count) != Some(&b'}') {
                            return None;
                        }
                        let text = std::str::from_utf8(&raw[3..3 + count]).ok()?;
                        let character = char::from_u32(u32::from_str_radix(text, 16).ok()?)?;
                        Some((4 + count, character.to_string()))
                    }
                    _ => None,
                }
            }
        }
    }
}

/// What a hole stands for, as far as the host knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HoleKind {
    /// A value: `"... WHERE id = $id"`, `"... LIMIT " . $limit`.
    Value,
    /// A name: a table, a column, part of one (`"FROM {$prefix}users"`).
    Identifier,
    /// A list of unknown length, of values or of names: `"... IN (" . implode(',', $ids) . ")"`.
    List,
    /// Anything, also nothing or a whole clause: `"SELECT * FROM t $where"`.
    Unknown,
}

/// What a fragment holds: whole statements, or one part of a statement that a query builder puts
/// together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FragmentKind {
    /// One or more whole statements.
    Statements,
    /// A condition of `WHERE` or `ON`: `->where('status = ?')`.
    Condition,
    /// A condition of `HAVING`.
    Having,
    /// The items of a select list: `->select('id, name')`.
    SelectList,
    /// The items of `ORDER BY`: `->orderBy('created_at desc')`.
    OrderBy,
    /// The items of `GROUP BY`.
    GroupBy,
    /// A table of `FROM` or a join, with its alias: `->from('users u')`.
    TableReference,
    /// The assignments of `UPDATE ... SET`: `->set('count = count + 1')`.
    SetList,
    /// One expression: `->selectRaw('count(*)')`, `->orderByRaw('lower(name)')`.
    Expression,
}

impl FragmentKind {
    pub fn is_partial(self) -> bool {
        self != FragmentKind::Statements
    }
}

/// A table a partial fragment sees, as the host knows it from the rest of the query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScopeTable {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

impl ScopeTable {
    pub fn new(name: impl Into<String>) -> ScopeTable {
        ScopeTable {
            schema: None,
            name: name.into(),
            alias: None,
        }
    }

    pub fn with_alias(mut self, alias: impl Into<String>) -> ScopeTable {
        self.alias = Some(alias.into());
        self
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> ScopeTable {
        self.schema = Some(schema.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Text that is the same in the host and in SQL, byte for byte.
    Text {
        text: String,
        host: u32,
        style: EscapeStyle,
    },
    /// One escape sequence of the host: `\'` in the host, `'` in SQL.
    Escape {
        text: String,
        host: Span,
        style: EscapeStyle,
    },
    Hole {
        host: Span,
        kind: HoleKind,
    },
}

/// SQL as a host holds it in its strings. Pieces go in in the order of the host document; text
/// between them (quotes, `.`, the code of an interpolation) is left out and never edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fragment {
    pub(crate) kind: FragmentKind,
    pub(crate) pieces: Vec<Piece>,
    pub(crate) tables: Vec<ScopeTable>,
}

impl Fragment {
    pub fn new(kind: FragmentKind) -> Fragment {
        Fragment {
            kind,
            pieces: Vec::new(),
            tables: Vec::new(),
        }
    }

    pub fn kind(&self) -> FragmentKind {
        self.kind
    }

    /// The source text of (part of) a host literal, without its quotes, starting at `host_start`.
    /// Its escapes are read by `style`.
    pub fn literal(&mut self, raw: &str, host_start: u32, style: EscapeStyle) -> &mut Fragment {
        let bytes = raw.as_bytes();
        let mut run = 0;
        let mut at = 0;
        while at < bytes.len() {
            match style.escape_at(&bytes[at..]) {
                Some((length, decoded)) => {
                    if run < at {
                        self.text(&raw[run..at], host_start + run as u32, style);
                    }
                    let host = Span::new(host_start + at as u32, host_start + (at + length) as u32);
                    self.escape(&decoded, host, style);
                    at += length;
                    run = at;
                }
                None => at += 1,
            }
        }
        if run < bytes.len() || raw.is_empty() {
            self.text(&raw[run..], host_start + run as u32, style);
        }
        self
    }

    /// The source text of the body of a heredoc whose closing marker is indented by `indent`
    /// bytes: that much space or tab is left out at the start of each line, as the host language
    /// leaves it out of the string's value. `at_line_start` says whether `raw` itself starts a line
    /// (it does not when it follows an interpolation).
    pub fn literal_dedented(
        &mut self,
        raw: &str,
        host_start: u32,
        style: EscapeStyle,
        indent: u32,
        at_line_start: bool,
    ) -> &mut Fragment {
        let mut offset = 0usize;
        let mut first = true;
        for line in raw.split_inclusive('\n') {
            let mut skip = 0usize;
            if !first || at_line_start {
                skip = line
                    .bytes()
                    .take(indent as usize)
                    .take_while(|byte| matches!(byte, b' ' | b'\t'))
                    .count();
            }
            self.literal(&line[skip..], host_start + (offset + skip) as u32, style);
            offset += line.len();
            first = false;
        }
        self
    }

    /// Text that reads the same in the host and in SQL, with no escape in it.
    pub fn text(&mut self, text: &str, host_start: u32, style: EscapeStyle) -> &mut Fragment {
        self.pieces.push(Piece::Text {
            text: text.to_string(),
            host: host_start,
            style,
        });
        self
    }

    /// One escape sequence the host decoded itself: `host` is the sequence, `decoded` what it stands
    /// for.
    pub fn escape(&mut self, decoded: &str, host: Span, style: EscapeStyle) -> &mut Fragment {
        self.pieces.push(Piece::Escape {
            text: decoded.to_string(),
            host,
            style,
        });
        self
    }

    /// An interpolated expression or a concatenated part that is not a literal.
    pub fn hole(&mut self, host: Span, kind: HoleKind) -> &mut Fragment {
        self.pieces.push(Piece::Hole { host, kind });
        self
    }

    /// A table the fragment sees: for a partial fragment, the tables of the query around it.
    pub fn table(&mut self, table: ScopeTable) -> &mut Fragment {
        self.tables.push(table);
        self
    }

    /// The SQL the pieces read as, with each hole left empty.
    pub fn plain_text(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, .. } | Piece::Escape { text, .. } => text.as_str(),
                Piece::Hole { .. } => "",
            })
            .collect()
    }

    /// The end of the last piece in the host, or 0.
    pub fn host_end(&self) -> u32 {
        self.pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, host, .. } => host + text.len() as u32,
                Piece::Escape { host, .. } | Piece::Hole { host, .. } => host.end,
            })
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(raw: &str, style: EscapeStyle) -> Vec<Piece> {
        let mut fragment = Fragment::new(FragmentKind::Statements);
        fragment.literal(raw, 10, style);
        fragment.pieces
    }

    #[test]
    fn reads_the_escapes_of_each_style() {
        assert_eq!(
            pieces(r"it\'s \n", EscapeStyle::SingleQuoted),
            [
                Piece::Text {
                    text: "it".into(),
                    host: 10,
                    style: EscapeStyle::SingleQuoted
                },
                Piece::Escape {
                    text: "'".into(),
                    host: Span::new(12, 14),
                    style: EscapeStyle::SingleQuoted
                },
                Piece::Text {
                    text: r"s \n".into(),
                    host: 14,
                    style: EscapeStyle::SingleQuoted
                },
            ]
        );
        let mut fragment = Fragment::new(FragmentKind::Statements);
        fragment.literal(r#"a\tb\x41\101\u{e9}\"\q\$"#, 0, EscapeStyle::DoubleQuoted);
        assert_eq!(fragment.plain_text(), "a\tbAAé\"\\q$");
        let mut fragment = Fragment::new(FragmentKind::Statements);
        fragment.literal(r#"\"x\""#, 0, EscapeStyle::Heredoc);
        assert_eq!(fragment.plain_text(), r#"\"x\""#);
        let mut fragment = Fragment::new(FragmentKind::Statements);
        fragment.literal("it''s", 0, EscapeStyle::Doubled(b'\''));
        assert_eq!(fragment.plain_text(), "it's");
    }

    #[test]
    fn encodes_what_it_decodes() {
        for (style, text) in [
            (EscapeStyle::SingleQuoted, r"it's a \ path"),
            (EscapeStyle::DoubleQuoted, "say \"$1\" \\ now"),
            (EscapeStyle::Heredoc, "say \"$1\" \\ now"),
            (EscapeStyle::Doubled(b'\''), "it's"),
            (EscapeStyle::Verbatim, "it's"),
        ] {
            let mut fragment = Fragment::new(FragmentKind::Statements);
            fragment.literal(&style.encode(text), 0, style);
            assert_eq!(fragment.plain_text(), text, "{style:?}");
        }
    }

    #[test]
    fn leaves_the_indentation_of_a_heredoc_out() {
        let mut fragment = Fragment::new(FragmentKind::Statements);
        fragment.literal_dedented("    SELECT *\n      FROM t\n", 100, EscapeStyle::Heredoc, 4, true);
        assert_eq!(fragment.plain_text(), "SELECT *\n  FROM t\n");
        assert_eq!(
            fragment.pieces[1],
            Piece::Text {
                text: "  FROM t\n".into(),
                host: 117,
                style: EscapeStyle::Heredoc
            }
        );
    }
}
