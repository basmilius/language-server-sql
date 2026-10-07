//! A fragment read as SQL: the text it stands for, with a statement written around a partial
//! fragment and placeholders in its holes, parsed once, and every question of the language server
//! asked of it with the answers moved back to the host.

use std::path::{Path, PathBuf};

use sql_analysis::actions::{ActionKind, code_actions, fix_all_action};
use sql_analysis::completion::{CompletionOptions, ItemKind, QuoteIdentifiers, complete};
use sql_analysis::ident::quote_name;
use sql_analysis::inlay_hints::{HintKind, inlay_hints};
use sql_analysis::inspections::{
    DISTINCT_WITH_GROUP_BY, IMPLICIT_CROSS_JOIN, INSERT_COLUMN_COUNT, InspectionSettings, MISSING_REQUIRED_COLUMN,
    MISSING_WHERE, NONAGGREGATED_COLUMN, Override, SET_OPERATION_COLUMN_COUNT, UNRESOLVED_COLUMN, UNRESOLVED_TABLE,
    UNUSED_ALIAS, UNUSED_CTE,
};
use sql_analysis::references::{Current, highlights, may_mention};
use sql_analysis::refs::{Access, Symbol, find_hits, symbol_at_offset};
use sql_analysis::semantic_tokens::semantic_tokens;
use sql_analysis::signature::{SignatureHelp, signature_help};
use sql_analysis::{DiagnosticSeverity, SYNTAX, diagnostics, nav, rename};
use sql_syntax::SyntaxKind::QUESTION;
use sql_syntax::lexer::{LexOptions, lex};
use sql_syntax::{Dialect, Parse, SyntaxNode, Target, TextRange, TextSize, check_features, parse};

use crate::environment::Environment;
use crate::fragment::{EscapeStyle, Fragment, FragmentKind, HoleKind, Piece, ScopeTable, Span};
use crate::map::{Bias, Builder, SourceMap};

/// The table a partial fragment reads from when the host names none: a name nothing defines, so
/// its columns are open and nothing is reported about them.
const SCOPE: &str = "fragment__scope";

/// The inspections that judge a statement as a whole. A partial fragment is not one, and a hole
/// that may hold anything (a list, a clause) may be what they miss.
const WHOLE_STATEMENT: [&str; 9] = [
    NONAGGREGATED_COLUMN,
    DISTINCT_WITH_GROUP_BY,
    INSERT_COLUMN_COUNT,
    SET_OPERATION_COLUMN_COUNT,
    MISSING_REQUIRED_COLUMN,
    MISSING_WHERE,
    UNUSED_CTE,
    UNUSED_ALIAS,
    IMPLICIT_CROSS_JOIN,
];

/// The rows of the feature table about placeholders: a host's database layer takes them whatever
/// the server does.
const PLACEHOLDER_ROWS: [&str; 3] = ["numbered-parameters", "question-parameters", "named-parameters"];

/// What a hole is written as in the SQL text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    Value,
    Name,
    Blank,
}

/// A text piece of the fragment and where it starts in an assembled text.
struct TextStart {
    piece: usize,
    sql: u32,
    len: u32,
}

struct Assembled {
    text: String,
    map: SourceMap,
    starts: Vec<TextStart>,
}

fn quoted_table(table: &ScopeTable, target: Target) -> String {
    let mut text = String::new();
    if let Some(schema) = &table.schema {
        text.push_str(&quote_name(schema, target));
        text.push('.');
    }
    text.push_str(&quote_name(&table.name, target));
    if let Some(alias) = &table.alias {
        text.push_str(" AS ");
        text.push_str(&quote_name(alias, target));
    }
    text
}

/// The statement written around a partial fragment: what comes before it and after it.
fn wrap(kind: FragmentKind, tables: &[ScopeTable], target: Target) -> (String, String) {
    let listed: Vec<String> = tables.iter().map(|table| quoted_table(table, target)).collect();
    let from = if listed.is_empty() {
        SCOPE.to_string()
    } else {
        listed.join(", ")
    };
    match kind {
        FragmentKind::Statements => (String::new(), String::new()),
        FragmentKind::Condition => (format!("SELECT * FROM {from} WHERE "), String::new()),
        FragmentKind::Having => (format!("SELECT * FROM {from} HAVING "), String::new()),
        FragmentKind::SelectList | FragmentKind::Expression => ("SELECT ".to_string(), format!("\nFROM {from}")),
        FragmentKind::OrderBy => (format!("SELECT * FROM {from} ORDER BY "), String::new()),
        FragmentKind::GroupBy => (format!("SELECT * FROM {from} GROUP BY "), String::new()),
        FragmentKind::Clauses => (format!("SELECT * FROM {from}\n"), String::new()),
        FragmentKind::TableReference if listed.is_empty() => ("SELECT * FROM ".to_string(), String::new()),
        FragmentKind::TableReference => (format!("SELECT * FROM {from}, "), String::new()),
        FragmentKind::SetList => {
            let table = listed.first().cloned().unwrap_or_else(|| SCOPE.to_string());
            (format!("UPDATE {table} SET "), String::new())
        }
    }
}

fn hole_text(fill: Fill, dialect: Dialect, number: usize) -> String {
    match fill {
        Fill::Value if dialect == Dialect::Postgres => "$1".to_string(),
        Fill::Value => "?".to_string(),
        Fill::Name => format!("hole__{number}"),
        Fill::Blank => " ".to_string(),
    }
}

/// The SQL text of a fragment with its holes filled as `fills` says, and each `?` at `cuts`
/// (pieces and offsets in them) written as PostgreSQL's `$1`.
fn assemble(env: &Environment, fragment: &Fragment, fills: &[Fill], cuts: &[(usize, u32)]) -> Assembled {
    let settings = env.settings();
    let target = env.target();
    let mut builder = Builder::default();
    if matches!(target.dialect, Dialect::Mysql | Dialect::Mariadb) {
        if let Some(mode) = &settings.sql_mode {
            builder.synthetic(&format!("SET sql_mode = '{}';\n", mode.replace('\'', "''")));
        }
    }
    let (prefix, suffix) = wrap(fragment.kind, &fragment.tables, target);
    if !prefix.is_empty() {
        builder.synthetic(&prefix);
    }
    let body_start = builder.offset();
    let mut starts = Vec::new();
    let mut hole = 0;
    for (index, piece) in fragment.pieces.iter().enumerate() {
        match piece {
            Piece::Text { text, host, style } => {
                starts.push(TextStart {
                    piece: index,
                    sql: builder.offset(),
                    len: text.len() as u32,
                });
                let mut from = 0u32;
                for (_, cut) in cuts.iter().filter(|(piece, _)| *piece == index) {
                    if *cut < from || *cut as usize >= text.len() {
                        continue;
                    }
                    if from < *cut {
                        builder.text(&text[from as usize..*cut as usize], host + from, *style);
                    }
                    builder.atomic("$1", Span::new(host + cut, host + cut + 1), *style);
                    from = cut + 1;
                }
                if (from as usize) < text.len() || text.is_empty() {
                    builder.text(&text[from as usize..], host + from, *style);
                }
            }
            Piece::Escape { text, host, style } => builder.atomic(text, *host, *style),
            Piece::Hole { host, .. } => {
                let fill = fills.get(hole).copied().unwrap_or(Fill::Blank);
                builder.hole(&hole_text(fill, target.dialect, hole + 1), *host);
                hole += 1;
            }
        }
    }
    let body_end = builder.offset();
    if !suffix.is_empty() {
        builder.synthetic(&suffix);
    }
    let (text, map) = builder.finish(Span::new(body_start, body_end));
    Assembled { text, map, starts }
}

/// What each hole is written as: what its kind says, and for a hole that may be anything, whatever
/// leaves the fewest syntax errors and syntax the target lacks (nothing, a value or a name, in
/// that order of preference).
fn choose_fills(env: &Environment, fragment: &Fragment) -> Vec<Fill> {
    let kinds: Vec<HoleKind> = fragment
        .pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::Hole { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    let mut fills: Vec<Fill> = kinds
        .iter()
        .map(|kind| match kind {
            HoleKind::Value => Fill::Value,
            HoleKind::Identifier | HoleKind::List => Fill::Name,
            HoleKind::Unknown => Fill::Blank,
        })
        .collect();
    let unknown: Vec<usize> = (0..kinds.len())
        .filter(|index| kinds[*index] == HoleKind::Unknown)
        .collect();
    // Each is tried three times; past this many, nothing is a fair guess for all of them.
    if unknown.len() > 16 {
        return fills;
    }
    let target = env.target();
    for index in unknown {
        let mut best: Option<(usize, Fill)> = None;
        for fill in [Fill::Blank, Fill::Value, Fill::Name] {
            fills[index] = fill;
            let text = assemble(env, fragment, &fills, &[]).text;
            let parsed = parse(&text, target.dialect);
            let lacking = check_features(&parsed.syntax(), target)
                .iter()
                .filter(|found| !found.deprecated && !PLACEHOLDER_ROWS.contains(&found.feature))
                .count();
            let errors = parsed.errors().len() + lacking;
            if best.is_none_or(|(fewest, _)| errors < fewest) {
                best = Some((errors, fill));
            }
        }
        fills[index] = best.map_or(Fill::Blank, |(_, fill)| fill);
    }
    fills
}

/// Where a `?` of the host's database layer stands in PostgreSQL text, which reads `?` as an
/// operator: outside strings, comments and quoted names.
fn question_cuts(assembled: &Assembled) -> Vec<(usize, u32)> {
    let options = LexOptions {
        question_placeholders: true,
        ..Dialect::Postgres.lex_options()
    };
    let mut cuts = Vec::new();
    let mut offset = 0u32;
    for token in lex(&assembled.text, options).tokens {
        if token.kind == QUESTION && token.len == 1 {
            let found = assembled
                .starts
                .iter()
                .find(|start| start.sql <= offset && offset < start.sql + start.len);
            if let Some(start) = found {
                cuts.push((start.piece, offset - start.sql));
            }
        }
        offset += token.len;
    }
    cuts
}

fn span_of(range: TextRange) -> Span {
    Span::new(range.start().into(), range.end().into())
}

fn range_of(span: Span) -> TextRange {
    TextRange::new(TextSize::from(span.start), TextSize::from(span.end))
}

/// A diagnostic in host offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
    pub severity: DiagnosticSeverity,
    /// `syntax` for a syntax error, else the id of the inspection.
    pub code: &'static str,
    pub deprecated: bool,
    pub unnecessary: bool,
    /// The row of the feature table behind `unsupported-syntax` or `deprecated-syntax`.
    pub feature: Option<&'static str>,
    pub related: Vec<Related>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Related {
    pub span: Span,
    pub message: String,
}

/// A replacement of a span of the host document; `new_text` is escaped for the host string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: ItemKind,
    pub detail: Option<String>,
    pub description: Option<String>,
    /// Markdown.
    pub documentation: Option<String>,
    pub edit: Edit,
    /// `edit.new_text` is a snippet with tab stops, escaped for the host and then for the snippet.
    pub snippet: bool,
    pub sort_text: String,
    pub filter_text: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionList {
    pub items: Vec<CompletionItem>,
    /// More items match than were given; ask again as the word grows.
    pub incomplete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub span: Span,
    pub markdown: String,
}

/// Where something is defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// In the fragment itself, in host offsets.
    Fragment { span: Span, name: Span },
    /// In a `.sql` file of the workspace, in byte offsets of the file as it was read.
    File { path: PathBuf, span: Span, name: Span },
}

/// A token in host offsets, never over a line break of the host; `ty` and `modifiers` index
/// [`crate::TOKEN_TYPES`] and [`crate::TOKEN_MODIFIERS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    pub span: Span,
    pub ty: u32,
    pub modifiers: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub span: Span,
    pub access: Access,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    /// The host offset the label goes before.
    pub offset: u32,
    pub label: String,
    pub kind: HintKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    pub kind: ActionKind,
    pub edits: Vec<Edit>,
    pub preferred: bool,
    /// The diagnostic a quick fix fixes: its span and code.
    pub fixes: Option<(Span, &'static str)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct References {
    /// The name asked about.
    pub span: Span,
    /// What it stands for; an object of the schema compares equal across fragments and files.
    pub symbol: Symbol,
    pub hits: Vec<Highlight>,
}

/// A `.sql` file to look for references in.
#[derive(Clone, Copy, Debug)]
pub struct SqlFile<'a> {
    pub path: &'a Path,
    pub text: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReferences {
    pub path: PathBuf,
    /// Byte offsets in the file.
    pub spans: Vec<Span>,
}

/// A snippet's own syntax (`$1`, `${1:name}`) stays as it is; every character it inserts is
/// escaped for the host and that again for the snippet.
fn encode_snippet(style: EscapeStyle, text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let literal = |character: char, out: &mut String| {
        for encoded in style.encode(character.encode_utf8(&mut [0; 4])).chars() {
            if matches!(encoded, '$' | '\\' | '}') {
                out.push('\\');
            }
            out.push(encoded);
        }
    };
    let mut at = 0;
    while at < chars.len() {
        let next = chars.get(at + 1).copied();
        match chars[at] {
            '\\' if next.is_some() => {
                literal(chars[at + 1], &mut out);
                at += 2;
            }
            '$' if next.is_some_and(|next| next.is_ascii_digit() || next == '{') => {
                out.push('$');
                at += 1;
                if chars[at] == '{' {
                    out.push('{');
                    at += 1;
                }
                while at < chars.len() && chars[at].is_ascii_digit() {
                    out.push(chars[at]);
                    at += 1;
                }
                if chars.get(at) == Some(&':') {
                    out.push(':');
                    at += 1;
                }
            }
            '}' => {
                out.push('}');
                at += 1;
            }
            character => {
                literal(character, &mut out);
                at += 1;
            }
        }
    }
    out
}

/// A fragment read as SQL against an environment. Built once per fragment (it parses once) and
/// asked any number of questions; it may move to and be shared between threads.
pub struct Analysis {
    env: Environment,
    text: String,
    map: SourceMap,
    parse: Parse,
    inspections: InspectionSettings,
    /// Statements with a hole that may be anything and syntax errors even so: their tree says
    /// nothing certain.
    quiet: Vec<Span>,
    /// Statements with a hole that may be anything, which may join the tables their names are
    /// of.
    open: Vec<Span>,
    partial: bool,
}

impl Analysis {
    pub fn new(env: &Environment, fragment: &Fragment) -> Analysis {
        let target = env.target();
        let fills = choose_fills(env, fragment);
        let mut assembled = assemble(env, fragment, &fills, &[]);
        if target.dialect == Dialect::Postgres && env.settings().question_placeholders {
            let cuts = question_cuts(&assembled);
            if !cuts.is_empty() {
                assembled = assemble(env, fragment, &fills, &cuts);
            }
        }
        let parse = parse(&assembled.text, target.dialect);
        let mut inspections = env.settings().inspections.clone();
        let off = Override {
            enabled: Some(false),
            severity: None,
        };
        for row in PLACEHOLDER_ROWS {
            if inspections.get(row).is_none() {
                inspections.set(row, off);
            }
        }
        let hole_kinds: Vec<HoleKind> = fragment
            .pieces
            .iter()
            .filter_map(|piece| match piece {
                Piece::Hole { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        let open_holes = hole_kinds
            .iter()
            .any(|kind| matches!(kind, HoleKind::List | HoleKind::Unknown));
        if fragment.kind.is_partial() || open_holes {
            for id in WHOLE_STATEMENT {
                inspections.set(id, off);
            }
        }
        let unknown: Vec<Span> = assembled
            .map
            .holes()
            .zip(&hole_kinds)
            .filter(|(_, kind)| **kind == HoleKind::Unknown)
            .map(|(span, _)| span)
            .collect();
        let mut quiet = Vec::new();
        let mut open = Vec::new();
        if !unknown.is_empty() {
            let statements: Vec<SyntaxNode> = parse.syntax().children().collect();
            let end = assembled.text.len() as u32;
            for (position, statement) in statements.iter().enumerate() {
                let span = span_of(statement.text_range());
                // A hole after a statement's last token, before the next one, still ends it:
                // `'SELECT * FROM t ' . $join`.
                let reach = statements
                    .get(position + 1)
                    .map_or(end, |next| u32::from(next.text_range().start()));
                let has_unknown = unknown.iter().any(|hole| span.start <= hole.start && hole.end <= reach);
                let broken = parse
                    .errors()
                    .iter()
                    .any(|error| span.touches(error.range.start().into()));
                if has_unknown {
                    open.push(Span::new(span.start, reach));
                }
                if has_unknown && broken {
                    quiet.push(span);
                }
            }
        }
        Analysis {
            env: env.clone(),
            text: assembled.text,
            map: assembled.map,
            parse,
            inspections,
            quiet,
            open,
            partial: fragment.kind.is_partial(),
        }
    }

    /// The SQL text the fragment reads as, with what was written around a partial fragment.
    pub fn sql(&self) -> &str {
        &self.text
    }

    /// Where the fragment's own SQL is in [`Analysis::sql`].
    pub fn body(&self) -> Span {
        self.map.body
    }

    /// The offset in [`Analysis::sql`] of a host offset in a piece of text, the cursor of a request;
    /// inside a character, the start of it.
    pub fn to_sql(&self, host: u32) -> Option<u32> {
        let mut offset = self.map.to_sql(host)? as usize;
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        Some(offset as u32)
    }

    fn root(&self) -> SyntaxNode {
        self.parse.syntax()
    }

    fn target(&self) -> Target {
        self.env.target()
    }

    fn edit(&self, span: Span, text: &str, snippet: bool) -> Option<Edit> {
        let (host, style) = self.map.edit(span)?;
        let new_text = if snippet {
            encode_snippet(style, text)
        } else {
            style.encode(text)
        };
        Some(Edit { span: host, new_text })
    }

    fn is_quiet(&self, span: Span) -> bool {
        self.quiet.iter().any(|statement| statement.touches(span.start))
    }

    /// Whether an unknown name of a column reference may be known after all: a qualifier of a part
    /// of a query, which may name a table of the query around it that the host did not see, or any
    /// name of a column in a statement whose hole may join another table.
    fn uncertain_name(&self, code: &str, sql: Span) -> bool {
        if code != UNRESOLVED_TABLE && code != UNRESOLVED_COLUMN {
            return false;
        }
        let name = match self.root().covering_element(range_of(sql)) {
            sql_syntax::SyntaxElement::Node(node) => node,
            sql_syntax::SyntaxElement::Token(token) => match token.parent() {
                Some(parent) => parent,
                None => return false,
            },
        };
        let Some(reference) = name
            .ancestors()
            .find(|node| node.kind() == sql_syntax::SyntaxKind::COLUMN_REF)
        else {
            return false;
        };
        let qualifier = reference
            .children()
            .filter(|child| child.kind() == sql_syntax::SyntaxKind::NAME)
            .last()
            .is_some_and(|last| last.text_range().end() > TextSize::from(sql.end));
        (self.partial && qualifier) || self.open.iter().any(|statement| statement.touches(sql.start))
    }

    /// Syntax errors, the findings of the inspections and unknown names, in host offsets. Nothing
    /// is said about a hole or what touches one, about the text written around a partial
    /// fragment, or about a statement whose hole may be anything and still does not parse.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let body = self.map.body;
        let mut out = Vec::new();
        for found in diagnostics(&self.parse, self.target(), self.env.schemas(), &self.inspections) {
            let mut sql = span_of(found.range);
            if self.map.touches_hole(sql) || self.is_quiet(sql) || self.uncertain_name(found.code, sql) {
                continue;
            }
            // A syntax error in the text written around a partial fragment is about where the
            // fragment starts or ends: `->select(', id')` misses an item before its comma, and
            // the parser says so at the end of the `SELECT` written before it.
            if found.code == SYNTAX && sql.start >= body.end {
                sql = Span::empty(body.end);
            } else if found.code == SYNTAX && sql.start < body.start {
                sql = Span::new(body.start, sql.end.clamp(body.start, body.end));
            }
            let Some(span) = self.map.range(sql) else {
                continue;
            };
            let related = found
                .related
                .iter()
                .filter_map(|related| {
                    Some(Related {
                        span: self.map.range(span_of(related.range))?,
                        message: related.message.clone(),
                    })
                })
                .collect();
            out.push(Diagnostic {
                span,
                message: found.message,
                severity: found.severity,
                code: found.code,
                deprecated: found.deprecated,
                unnecessary: found.unnecessary,
                feature: found.feature,
                related,
            });
        }
        out
    }

    /// What can be typed at a host offset, with edits in host offsets whose text is escaped for
    /// the host string. Nothing inside a hole or for a word that runs into one.
    pub fn completion(&self, host: u32, options: CompletionOptions) -> CompletionList {
        let Some(offset) = self.to_sql(host) else {
            return CompletionList::default();
        };
        let mut options = options;
        if options.quote_identifiers == QuoteIdentifiers::Auto {
            options.quote_identifiers = self.env.settings().quote_identifiers;
        }
        let list = complete(&self.text, offset, self.target(), self.env.schemas(), options);
        let items = list
            .items
            .into_iter()
            .filter_map(|item| {
                let span = Span::new(item.edit.start, item.edit.end);
                let (_, style) = self.map.edit(span)?;
                let edit = self.edit(span, &item.edit.new_text, item.snippet)?;
                // A client filters on the host's text of the edit, where a quote may be escaped.
                let filter_text = item.filter_text.map(|filter| style.encode(&filter));
                Some(CompletionItem {
                    label: item.label,
                    kind: item.kind,
                    detail: item.detail,
                    description: item.description,
                    documentation: item.documentation,
                    edit,
                    snippet: item.snippet,
                    sort_text: item.sort_text,
                    filter_text,
                })
            })
            .collect();
        CompletionList {
            items,
            incomplete: list.incomplete,
        }
    }

    pub fn hover(&self, host: u32) -> Option<Hover> {
        let offset = self.to_sql(host)?;
        let found = nav::hover(&self.root(), offset, self.target(), self.env.schemas())?;
        let span = self.map.range(span_of(found.range)).unwrap_or(Span::empty(host));
        Some(Hover {
            span,
            markdown: found.markdown,
        })
    }

    /// Where what the name at a host offset stands for is defined: in the fragment, or in DDL of
    /// the workspace. What only a snapshot has has no place.
    pub fn definition(&self, host: u32) -> Vec<Location> {
        let Some(offset) = self.to_sql(host) else {
            return Vec::new();
        };
        nav::definition(&self.root(), offset, self.target(), self.env.schemas())
            .into_iter()
            .filter_map(|place| match place.path {
                Some(path) => Some(Location::File {
                    path,
                    span: span_of(place.range),
                    name: span_of(place.name),
                }),
                None => Some(Location::Fragment {
                    span: self.map.range(span_of(place.range))?,
                    name: self.map.range(span_of(place.name))?,
                }),
            })
            .collect()
    }

    pub fn signature_help(&self, host: u32) -> Option<SignatureHelp> {
        let offset = self.to_sql(host)?;
        signature_help(&self.root(), offset, self.target(), self.env.schemas())
    }

    /// The tokens of the fragment, colored by what names stand for, in host offsets: a token
    /// that spans an escape or two pieces is given once per stretch of the host, holes not at all.
    pub fn semantic_tokens(&self) -> Vec<SemanticToken> {
        let body = self.map.body;
        let mut out = Vec::new();
        for token in semantic_tokens(&self.root(), self.target(), self.env.schemas(), Some(range_of(body))) {
            let sql = Span::new(token.start.max(body.start), token.end.min(body.end));
            if sql.start >= sql.end {
                continue;
            }
            for span in self.map.pieces(sql) {
                out.push(SemanticToken {
                    span,
                    ty: token.ty,
                    modifiers: token.modifiers,
                });
            }
        }
        out
    }

    /// The places in the fragment that name what the name at a host offset stands for.
    pub fn highlights(&self, host: u32) -> Vec<Highlight> {
        let Some(offset) = self.to_sql(host) else {
            return Vec::new();
        };
        highlights(&self.root(), offset, self.target(), self.env.schemas())
            .into_iter()
            .filter_map(|hit| {
                Some(Highlight {
                    span: self.map.range(span_of(hit.range))?,
                    access: hit.access,
                })
            })
            .collect()
    }

    pub fn inlay_hints(&self) -> Vec<InlayHint> {
        let body = self.map.body;
        let found = inlay_hints(
            &self.root(),
            self.target(),
            self.env.schemas(),
            Some(range_of(body)),
            self.env.settings().hints,
        );
        found
            .into_iter()
            .filter(|hint| body.touches(hint.offset) && !self.map.touches_hole(Span::empty(hint.offset)))
            .filter_map(|hint| {
                Some(InlayHint {
                    offset: self.map.to_host(hint.offset, Bias::Right)?,
                    label: hint.label,
                    kind: hint.kind,
                })
            })
            .collect()
    }

    fn action(&self, action: sql_analysis::actions::Action) -> Option<CodeAction> {
        let fixes = match action.fixes {
            Some((range, code)) => {
                let sql = span_of(range);
                if self.map.touches_hole(sql) {
                    return None;
                }
                Some((self.map.range(sql)?, code))
            }
            None => None,
        };
        let edits = action
            .edits
            .iter()
            .map(|edit| self.edit(span_of(edit.range), &edit.text, false))
            .collect::<Option<Vec<_>>>()?;
        Some(CodeAction {
            title: action.title,
            kind: action.kind,
            edits,
            preferred: action.preferred,
            fixes,
        })
    }

    /// The quick fixes and rewrites at a host span. An action any of whose edits would reach
    /// outside one host literal is left out.
    pub fn code_actions(&self, host: Span) -> Vec<CodeAction> {
        let (Some(start), Some(end)) = (self.to_sql(host.start), self.to_sql(host.end)) else {
            return Vec::new();
        };
        let range = range_of(Span::new(start.min(end), start.max(end)));
        code_actions(
            &self.root(),
            self.target(),
            self.env.schemas(),
            &self.inspections,
            range,
        )
        .into_iter()
        .filter_map(|action| self.action(action))
        .collect()
    }

    /// The fixes of every inspection whose fix is safe everywhere, applied to the whole fragment,
    /// leaving out an edit that would reach outside one host literal.
    pub fn fix_all(&self) -> Option<CodeAction> {
        let action = fix_all_action(&self.root(), self.target(), self.env.schemas(), &self.inspections)?;
        let edits: Vec<Edit> = action
            .edits
            .iter()
            .filter_map(|edit| self.edit(span_of(edit.range), &edit.text, false))
            .collect();
        (!edits.is_empty()).then_some(CodeAction {
            title: action.title,
            kind: action.kind,
            edits,
            preferred: action.preferred,
            fixes: None,
        })
    }

    fn hits_in(&self, symbol: &Symbol) -> Vec<Highlight> {
        find_hits(
            &self.root(),
            self.target(),
            self.env.schemas(),
            std::slice::from_ref(symbol),
        )
        .into_iter()
        .filter_map(|(_, hit)| {
            Some(Highlight {
                span: self.map.range(span_of(hit.range))?,
                access: hit.access,
            })
        })
        .collect()
    }

    /// What the name at a host offset stands for and every place of the fragment that names it.
    pub fn references(&self, host: u32) -> Option<References> {
        let offset = self.to_sql(host)?;
        let (range, symbol, _) = symbol_at_offset(&self.root(), offset, self.target(), self.env.schemas())?;
        let span = self.map.range(span_of(range))?;
        let hits = self.hits_in(&symbol);
        Some(References { span, symbol, hits })
    }

    /// The places of this fragment that name a symbol another fragment or file found.
    pub fn hits(&self, symbol: &Symbol) -> Vec<Highlight> {
        if symbol.is_local() {
            return Vec::new();
        }
        self.hits_in(symbol)
    }

    /// The places `.sql` files name a symbol, read in the environment's dialect and schema.
    pub fn references_in_files(&self, symbol: &Symbol, files: &[SqlFile]) -> Vec<FileReferences> {
        if symbol.is_local() {
            return Vec::new();
        }
        let target = self.target();
        files
            .iter()
            .filter(|file| may_mention(file.text, symbol))
            .filter_map(|file| {
                let root = parse(file.text, target.dialect).syntax();
                let spans: Vec<Span> = find_hits(&root, target, self.env.schemas(), std::slice::from_ref(symbol))
                    .into_iter()
                    .map(|(_, hit)| span_of(hit.range))
                    .collect();
                (!spans.is_empty()).then(|| FileReferences {
                    path: file.path.to_path_buf(),
                    spans,
                })
            })
            .collect()
    }

    fn local_symbol(&self, host: u32) -> Result<(u32, SyntaxNode), String> {
        let offset = self
            .to_sql(host)
            .ok_or_else(|| "There is no name to rename here".to_string())?;
        let root = self.root();
        let (_, symbol, _) = symbol_at_offset(&root, offset, self.target(), self.env.schemas())
            .ok_or_else(|| "There is no name to rename here".to_string())?;
        if !symbol.is_local() {
            return Err(format!(
                "The {} '{}' belongs to the schema; rename it where it is defined",
                symbol.label(),
                symbol.name()
            ));
        }
        Ok((offset, root))
    }

    /// The name at a host offset and the text to offer for renaming it. Only names the fragment
    /// declares itself (aliases, common table expressions, column aliases) are renamed here.
    pub fn prepare_rename(&self, host: u32) -> Result<(Span, String), String> {
        let (offset, root) = self.local_symbol(host)?;
        let current = Current {
            root: &root,
            target: self.target(),
            schemas: self.env.schemas(),
        };
        let prepared = rename::prepare_rename(&current, offset)?;
        let span = self
            .map
            .range(span_of(prepared.range))
            .ok_or_else(|| "The name is not in the fragment".to_string())?;
        Ok((span, prepared.placeholder))
    }

    /// The edits that rename the name at a host offset, escaped for the host.
    pub fn rename(&self, host: u32, new_name: &str) -> Result<Vec<Edit>, String> {
        let (offset, root) = self.local_symbol(host)?;
        let current = Current {
            root: &root,
            target: self.target(),
            schemas: self.env.schemas(),
        };
        let files = rename::rename(&current, offset, new_name, &[])?;
        files
            .into_iter()
            .filter(|file| file.path.is_none())
            .flat_map(|file| file.edits)
            .map(|edit| {
                self.edit(span_of(edit.range), &edit.text, false).ok_or_else(|| {
                    "The name also stands where the host's string cannot be edited, such as next to an interpolation"
                        .to_string()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snippet_keeps_its_tab_stops_and_escapes_the_rest() {
        assert_eq!(
            encode_snippet(EscapeStyle::DoubleQuoted, "count($1) AS \"n\""),
            "count($1) AS \\\\\"n\\\\\""
        );
        assert_eq!(
            encode_snippet(EscapeStyle::SingleQuoted, "VALUES (${1:it's}, $2)"),
            "VALUES (${1:it\\\\'s}, $2)"
        );
        assert_eq!(encode_snippet(EscapeStyle::Verbatim, "a \\$ b"), "a \\$ b");
    }

    #[test]
    fn analysis_moves_between_threads() {
        fn sendable<T: Send + Sync>() {}
        sendable::<Analysis>();
        sendable::<Environment>();
    }
}
