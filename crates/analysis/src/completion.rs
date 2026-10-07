//! Completion: what can be typed at a cursor. The word being typed is replaced by a placeholder
//! name, so the parser sees a whole statement and the tree says what kind of thing belongs there:
//! a table after `FROM`, a column in an expression, a type in a column definition, a keyword where
//! a clause may follow. The scope of the name gives the columns, the catalog the tables, functions
//! and types of the dialect and version.
//!
//! Items are ranked the way a database IDE ranks them: what the scope holds first (join conditions
//! from foreign keys, the columns of the tables in scope, their aliases), then the tables of the
//! default schema, schemas, routines, built-in functions, and keywords last.

use std::collections::HashSet;

use sql_catalog::model::{Column, Table, TableKind, TypeKind};
use sql_catalog::{FunctionKind, Overload};
use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, SyntaxToken, Target, TextSize, parse, supports};

use crate::ast::{child, children, compact, has_token, is_query, object_name, parts};
use crate::catalog::{Catalog, Place, TableId};
use crate::context::{DocumentSchema, Schemas};
use crate::ident::{Ident, quote_name};
use crate::resolve::{ColumnOrigin, Level, Resolver, Source, SourceKind};

const PLACEHOLDER: &str = "zzcompletionzz";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Keyword,
    Table,
    View,
    Column,
    /// An alias or a common table expression of the scope.
    Alias,
    Schema,
    Function,
    Procedure,
    Type,
    Sequence,
    /// A value of an enum.
    Value,
    /// Text built from the schema: a join condition, a column list, a `VALUES` template.
    Snippet,
    Setting,
}

/// A replacement of a byte range of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start: u32,
    pub end: u32,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: ItemKind,
    /// The type of a column, the signature of a function, the kind of a table.
    pub detail: Option<String>,
    /// Where it is from: the table of a column, the schema of a table.
    pub description: Option<String>,
    /// Markdown.
    pub documentation: Option<String>,
    pub edit: TextEdit,
    /// The new text is a snippet with tab stops.
    pub snippet: bool,
    pub sort_text: String,
    pub filter_text: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionList {
    pub items: Vec<CompletionItem>,
    /// More items match than were returned; ask again as the word grows.
    pub incomplete: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct CompletionOptions {
    pub limit: usize,
    /// The client takes snippets: functions get their parentheses with a tab stop inside.
    pub snippets: bool,
}

impl Default for CompletionOptions {
    fn default() -> CompletionOptions {
        CompletionOptions {
            limit: 500,
            snippets: true,
        }
    }
}

/// The word being typed: from its start (an opening quote included) to the cursor.
struct Word {
    start: usize,
    text: String,
    quote: Option<char>,
}

fn word_before(text: &str, offset: usize) -> Word {
    let bytes = text.as_bytes();
    let mut start = offset.min(text.len());
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80 {
            start -= 1;
        } else {
            break;
        }
    }
    while start < offset && !text.is_char_boundary(start) {
        start += 1;
    }
    let mut quote = None;
    if start > 0 && matches!(bytes[start - 1], b'"' | b'`' | b'[') {
        let before_quote = &text[..start - 1];
        let opens = before_quote
            .chars()
            .filter(|character| *character == bytes[start - 1] as char)
            .count();
        if bytes[start - 1] == b'[' || opens % 2 == 0 {
            quote = Some(bytes[start - 1] as char);
            start -= 1;
        }
    }
    let typed_start = if quote.is_some() { start + 1 } else { start };
    Word {
        start,
        text: text[typed_start..offset].to_string(),
        quote,
    }
}

/// What can be typed at an offset.
pub fn complete(
    text: &str,
    offset: u32,
    target: Target,
    schemas: Schemas,
    options: CompletionOptions,
) -> CompletionList {
    let offset = (offset as usize).min(text.len());
    let word = word_before(text, offset);
    let dialect = target.dialect;
    let original = parse(text, dialect).syntax();
    if let Some(list) = complete_in_string(&original, text, offset, target, schemas) {
        return list;
    }
    if in_comment_or_string(&original, offset) {
        return CompletionList::default();
    }
    let patched = format!("{}{PLACEHOLDER}{}", &text[..word.start], &text[offset..]);
    let root = parse(&patched, dialect).syntax();
    let Some(token) = root
        .token_at_offset(TextSize::from(word.start as u32 + 1))
        .find(|token| token.text().contains(PLACEHOLDER))
    else {
        return CompletionList::default();
    };
    let document = DocumentSchema::before(&root, word.start as u32, target, schemas);
    let catalog = document.catalog();
    let mut collector = Collector {
        catalog: &catalog,
        resolver: Resolver::new(&catalog),
        target,
        word: &word,
        edit_start: word.start as u32,
        edit_end: offset as u32,
        options,
        items: Vec::new(),
        before: &text[..word.start],
    };
    collector.collect(&token);
    collector.finish()
}

fn in_comment_or_string(root: &SyntaxNode, offset: usize) -> bool {
    let at = TextSize::from(offset as u32);
    root.token_at_offset(at).any(|token| {
        let range = token.text_range();
        let inside = range.start() < at && at < range.end();
        let comment_end = token.kind() == LINE_COMMENT && range.start() < at;
        (inside && (token.kind().is_string() || token.kind() == BLOCK_COMMENT)) || comment_end
    })
}

struct Collector<'c, 'a, 'w> {
    catalog: &'c Catalog<'a>,
    resolver: Resolver<'c, 'a>,
    target: Target,
    word: &'w Word,
    edit_start: u32,
    edit_end: u32,
    options: CompletionOptions,
    items: Vec<CompletionItem>,
    before: &'w str,
}

/// Ranks: lower comes first.
mod rank {
    pub const TEMPLATE: u8 = 0;
    pub const LOCAL: u8 = 1;
    pub const QUALIFIER: u8 = 2;
    pub const TABLE: u8 = 3;
    pub const SCHEMA: u8 = 4;
    pub const ROUTINE: u8 = 5;
    pub const KEYWORD: u8 = 6;
    pub const FUNCTION: u8 = 7;
    pub const OTHER: u8 = 8;
}

impl Collector<'_, '_, '_> {
    fn dialect(&self) -> Dialect {
        self.target.dialect
    }

    fn push(
        &mut self,
        label: impl Into<String>,
        kind: ItemKind,
        rank: u8,
        insert: impl Into<String>,
    ) -> &mut CompletionItem {
        let label = label.into();
        let order = self.items.len();
        self.items.push(CompletionItem {
            sort_text: format!("{rank}{order:05}"),
            label,
            kind,
            detail: None,
            description: None,
            documentation: None,
            edit: TextEdit {
                start: self.edit_start,
                end: self.edit_end,
                new_text: insert.into(),
            },
            snippet: false,
            filter_text: None,
        });
        self.items.last_mut().expect("just pushed")
    }

    /// A name as it is inserted: quoted when it has to be, or when the word was begun with a quote.
    fn name(&self, name: &str) -> String {
        if let Some(quote) = self.word.quote {
            let close = if quote == '[' { ']' } else { quote };
            return format!("{quote}{}{close}", name.replace(close, &format!("{close}{close}")));
        }
        quote_name(name, self.target)
    }

    /// Keywords in the case the word is typed in: lower case when it is, capitals otherwise.
    fn keyword_case(&self, keyword: &str) -> String {
        let typed = &self.word.text;
        if !typed.is_empty() && typed.chars().all(|character| !character.is_ascii_uppercase()) {
            keyword.to_ascii_lowercase()
        } else {
            keyword.to_string()
        }
    }

    fn keywords(&mut self, keywords: &[&str]) {
        for keyword in keywords {
            let text = self.keyword_case(keyword);
            self.push(text.clone(), ItemKind::Keyword, rank::KEYWORD, text);
        }
    }

    fn finish(mut self) -> CompletionList {
        let typed = self.word.text.to_lowercase();
        let mut seen = HashSet::new();
        self.items.retain(|item| {
            seen.insert((
                item.label.to_lowercase(),
                item.kind as u8,
                item.edit.new_text.clone(),
                item.description.clone(),
            ))
        });
        if !typed.is_empty() {
            self.items.retain(|item| {
                let filter = item.filter_text.as_deref().unwrap_or(&item.label).to_lowercase();
                subsequence(&typed, &filter)
            });
            for item in &mut self.items {
                let filter = item.filter_text.as_deref().unwrap_or(&item.label).to_lowercase();
                let prefix = if filter.starts_with(&typed) { '0' } else { '1' };
                item.sort_text.insert(1, prefix);
            }
        }
        self.items.sort_by(|a, b| a.sort_text.cmp(&b.sort_text));
        let incomplete = self.items.len() > self.options.limit;
        self.items.truncate(self.options.limit);
        CompletionList {
            items: self.items,
            incomplete,
        }
    }

    fn collect(&mut self, token: &SyntaxToken) {
        let Some(parent) = token.parent() else {
            return;
        };
        if self.before.ends_with("@@") && matches!(self.dialect(), Dialect::Mysql | Dialect::Mariadb) {
            self.settings();
            return;
        }
        if parent.kind() == NAME {
            let Some(owner) = parent.parent() else {
                return;
            };
            match owner.kind() {
                COLUMN_REF => self.column_ref(&owner, &parent),
                QUALIFIED_NAME => self.qualified(&owner, &parent),
                ALIAS => self.after_operand(&parent),
                NAME_LIST => self.name_list(&owner, &parent),
                COLUMN_DEF if child(&owner, NAME).as_ref() != Some(&parent) => self.column_constraints(),
                _ => {}
            }
            return;
        }
        if self.at_statement_start(token) {
            self.statement_keywords();
            return;
        }
        self.after_operand_token(token);
    }

    fn at_statement_start(&self, token: &SyntaxToken) -> bool {
        let previous = previous_significant(token);
        match previous {
            None => true,
            Some(previous) => {
                matches!(
                    previous.kind(),
                    SEMICOLON | CUSTOM_DELIMITER | DELIMITER_VALUE | META_COMMAND
                ) || (matches!(
                    previous.kind(),
                    BEGIN_KW | THEN_KW | ELSE_KW | DO_KW | LOOP_KW | REPEAT_KW
                ) && token
                    .parent_ancestors()
                    .any(|ancestor| ancestor.kind() == STATEMENT_LIST || ancestor.kind() == ROUTINE_BODY))
            }
        }
    }

    // Contexts

    fn column_ref(&mut self, reference: &SyntaxNode, name: &SyntaxNode) {
        let all = parts(reference, self.dialect());
        let position = all.iter().position(|part| part.node == *name).unwrap_or(0);
        let missing_set = reference
            .ancestors()
            .find(|ancestor| ancestor.kind() == SET_CLAUSE)
            .is_some_and(|clause| {
                !has_token(&clause, SET_KW) && clause.parent().is_some_and(|parent| parent.kind() == UPDATE_STMT)
            });
        if missing_set {
            if let Some(token) = name.first_token() {
                self.after_operand_token(&token);
            }
            return;
        }
        let levels = self.resolver.scope(reference);
        if position == 0 {
            if self.in_set_target(reference) {
                self.level_columns(&levels, true);
                return;
            }
            self.join_conditions(reference);
            self.enum_values(reference);
            self.level_columns(&levels, false);
            self.star_expansion(reference, &levels);
            self.qualifiers(&levels);
            self.functions(false);
            self.expression_keywords(reference);
            return;
        }
        let qualifiers = &all[..position];
        if let [qualifier] = qualifiers {
            if let Some(source) = self.resolver.find_source(&levels, &qualifier.ident) {
                self.source_columns(&source, 0, None);
                return;
            }
            if self.catalog.is_schema(&qualifier.ident) {
                self.tables(Some(&qualifier.ident.text), false);
                self.schema_functions(&qualifier.ident);
            }
        }
    }

    fn in_set_target(&self, reference: &SyntaxNode) -> bool {
        reference
            .parent()
            .is_some_and(|parent| parent.kind() == ASSIGNMENT && parent.children().next().as_ref() == Some(reference))
    }

    fn qualified(&mut self, qualified: &SyntaxNode, name: &SyntaxNode) {
        let Some(owner) = qualified.parent() else {
            return;
        };
        let all = parts(qualified, self.dialect());
        let position = all.iter().position(|part| part.node == *name).unwrap_or(0);
        let schema = (position > 0).then(|| all[position - 1].ident.clone());
        match owner.kind() {
            TABLE_REF => {
                if let Some(schema) = &schema {
                    self.tables(Some(&schema.text), false);
                    return;
                }
                let join_left = self.join_left_sources(&owner);
                if !join_left.is_empty() {
                    self.join_tables(&join_left);
                }
                self.ctes(qualified);
                self.tables(None, false);
                self.schemas();
                if owner
                    .parent()
                    .is_some_and(|parent| parent.kind() == FROM_CLAUSE || parent.kind() == JOIN_EXPR)
                {
                    let mut words = vec!["LATERAL"];
                    words.retain(|_| supports("lateral", self.target));
                    self.keywords(&words);
                }
            }
            INSERT_STMT | UPDATE_STMT | DELETE_STMT | MERGE_STMT | ALTER_TABLE_STMT | TRUNCATE_STMT
            | REFERENCES_CLAUSE | LIKE_CLAUSE | CREATE_INDEX_STMT | CREATE_TRIGGER_STMT | DROP_STMT | TABLE_QUERY
            | LOCKING_CLAUSE | EXPLAIN_STMT | RENAME_TABLE_STMT => {
                let defining = match owner.kind() {
                    CREATE_INDEX_STMT => crate::resolve::table_after_on(&owner).as_ref() != Some(qualified),
                    CREATE_TRIGGER_STMT => children(&owner, QUALIFIED_NAME).next().as_ref() == Some(qualified),
                    _ => false,
                };
                if defining {
                    return;
                }
                if let Some(schema) = &schema {
                    self.tables(Some(&schema.text), false);
                    return;
                }
                if owner.kind() == INSERT_STMT {
                    self.ctes(qualified);
                }
                self.tables(None, owner.kind() == INSERT_STMT || owner.kind() == ALTER_TABLE_STMT);
                self.schemas();
            }
            FUNCTION_CALL => {
                if let Some(schema) = &schema {
                    self.schema_functions(schema);
                    return;
                }
                let levels = self.resolver.scope(&owner);
                self.level_columns(&levels, false);
                self.qualifiers(&levels);
                self.functions(false);
            }
            TYPE => {
                if schema.is_none() {
                    self.types();
                }
            }
            CALL_STMT => {
                if schema.is_none() {
                    self.functions(true);
                }
            }
            SET_ASSIGNMENT => {
                if position == 0 && matches!(self.dialect(), Dialect::Postgres | Dialect::Generic) {
                    self.settings();
                }
            }
            PRAGMA_STMT | SHOW_STMT => self.settings(),
            _ => {}
        }
    }

    fn name_list(&mut self, list: &SyntaxNode, name: &SyntaxNode) {
        let Some(owner) = list.parent() else {
            return;
        };
        match owner.kind() {
            INSERT_STMT | MERGE_WHEN_CLAUSE => {
                let levels = self.resolver.scope(name);
                let Some(target) = levels.first().and_then(|level| level.sources.first()).cloned() else {
                    return;
                };
                let listed: Vec<String> = parts(list, self.dialect())
                    .into_iter()
                    .filter(|part| part.node != *name)
                    .map(|part| part.ident.text.to_lowercase())
                    .collect();
                let columns: Vec<String> = self
                    .resolver
                    .columns(&target, 0)
                    .columns
                    .into_iter()
                    .filter(|column| column.origin != ColumnOrigin::Implicit)
                    .map(|column| column.name)
                    .collect();
                let first = listed.is_empty();
                self.source_columns(&target, 0, Some(&listed));
                if first && columns.len() > 1 {
                    let all: Vec<String> = columns.iter().map(|column| self.name(column)).collect();
                    let label = all.join(", ");
                    let item = self.push(label.clone(), ItemKind::Snippet, rank::TEMPLATE, label);
                    item.detail = Some("all columns".to_string());
                    item.filter_text = Some(columns[0].clone());
                }
            }
            USING_CLAUSE => {
                let Some(join) = owner.parent() else {
                    return;
                };
                let mut sources = Vec::new();
                for operand in join
                    .children()
                    .filter(|inner| !matches!(inner.kind(), ON_CLAUSE | USING_CLAUSE))
                {
                    self.resolver.from_sources(&operand, &mut sources, &mut Vec::new());
                }
                let split = sources.len().saturating_sub(1);
                let left: Vec<String> = sources[..split]
                    .iter()
                    .flat_map(|source| self.resolver.columns(source, 0).columns)
                    .map(|column| column.name.to_lowercase())
                    .collect();
                if let Some(right) = sources.get(split) {
                    for column in self.resolver.columns(right, 0).columns {
                        if left.contains(&column.name.to_lowercase()) {
                            let insert = self.name(&column.name);
                            self.push(column.name.clone(), ItemKind::Column, rank::LOCAL, insert);
                        }
                    }
                }
            }
            REFERENCES_CLAUSE => {
                let Some((schema, table)) =
                    child(&owner, QUALIFIED_NAME).and_then(|name| object_name(&name, self.dialect()))
                else {
                    return;
                };
                if let Some(id) = self
                    .catalog
                    .find_table(schema.as_ref().map(|part| &part.ident), &table.ident)
                {
                    let table = self.catalog.table(id).clone();
                    for column in &table.columns {
                        let insert = self.name(&column.name);
                        let item = self.push(column.name.clone(), ItemKind::Column, rank::LOCAL, insert);
                        item.detail = column.data_type.clone();
                    }
                }
            }
            TABLE_CONSTRAINT => {
                if let Some(elements) = owner.ancestors().find(|ancestor| ancestor.kind() == TABLE_ELEMENT_LIST) {
                    for column in children(&elements, COLUMN_DEF) {
                        if let Some(column_name) =
                            child(&column, NAME).and_then(|name| Ident::of_name(&name, self.dialect()))
                        {
                            let insert = self.name(&column_name.text);
                            let item = self.push(column_name.text.clone(), ItemKind::Column, rank::LOCAL, insert);
                            item.detail = child(&column, TYPE).map(|data_type| compact(&data_type));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Items from the scope

    fn level_columns(&mut self, levels: &[Level], first_only: bool) {
        let mut source_index = 0u32;
        for (depth, level) in levels.iter().enumerate() {
            if first_only && depth > 0 {
                break;
            }
            for source in &level.sources {
                self.source_columns_ranked(source, source_index);
                source_index += 1;
            }
            if let Some(select) = &level.select {
                if matches!(
                    level.clause,
                    crate::resolve::Clause::OrderBy | crate::resolve::Clause::GroupBy | crate::resolve::Clause::Having
                ) {
                    self.select_aliases(select);
                }
            }
            for variable in level.variables.clone() {
                let insert = self.name(&variable.ident.text);
                let item = self.push(variable.ident.text.clone(), ItemKind::Alias, rank::LOCAL, insert);
                item.detail = Some("variable".to_string());
            }
        }
    }

    fn select_aliases(&mut self, select: &SyntaxNode) {
        let Some(list) = child(select, SELECT_LIST) else {
            return;
        };
        for item in children(&list, SELECT_ITEM) {
            if let Some((alias, _)) = crate::ast::alias_of(&item, self.dialect()) {
                let insert = self.name(&alias.ident.text);
                let expression = item.children().next().map(|inner| compact(&inner));
                let entry = self.push(alias.ident.text.clone(), ItemKind::Alias, rank::LOCAL, insert);
                entry.detail = Some("alias".to_string());
                entry.documentation = expression.map(|text| format!("```sql\n{text}\n```"));
            }
        }
    }

    fn source_columns(&mut self, source: &Source, index: u32, skip: Option<&[String]>) {
        let columns = self.resolver.columns(source, 0);
        for (position, column) in columns.columns.iter().enumerate() {
            if column.origin == ColumnOrigin::Implicit && !matches!(source.kind, SourceKind::Derived(_)) {
                continue;
            }
            if skip.is_some_and(|skip| skip.contains(&column.name.to_lowercase())) {
                continue;
            }
            self.column_item(source, column, index, position);
        }
    }

    fn source_columns_ranked(&mut self, source: &Source, index: u32) {
        let columns = self.resolver.columns(source, 0);
        for (position, column) in columns.columns.iter().enumerate() {
            if column.origin == ColumnOrigin::Implicit {
                continue;
            }
            self.column_item(source, column, index, position);
        }
    }

    fn column_item(&mut self, source: &Source, column: &crate::resolve::OutputColumn, index: u32, position: usize) {
        let insert = self.name(&column.name);
        let details = match &column.origin {
            ColumnOrigin::Table(id, at) => Some(self.catalog.table(*id).columns[*at].clone()),
            _ => None,
        };
        let source_name = if source.name.text.is_empty() {
            None
        } else {
            Some(source.name.text.clone())
        };
        let order = index * 10_000 + position as u32;
        let label = column.name.clone();
        let item = self.push(label, ItemKind::Column, rank::LOCAL, insert);
        item.sort_text = format!("{}{order:09}", rank::LOCAL);
        item.description = source_name;
        if let Some(details) = details {
            item.detail = details.data_type.clone();
            item.documentation = column_documentation(&details);
        }
    }

    fn qualifiers(&mut self, levels: &[Level]) {
        for level in levels {
            for source in &level.sources {
                if source.name.text.is_empty() || (!source.aliased && level.sources.len() < 2) {
                    continue;
                }
                let insert = self.name(&source.name.text);
                let detail = match &source.kind {
                    SourceKind::Table(id) => Some(self.catalog.table(*id).name.clone()),
                    SourceKind::Cte(_) => Some("common table expression".to_string()),
                    SourceKind::Derived(_) => Some("subquery".to_string()),
                    _ => None,
                };
                let item = self.push(source.name.text.clone(), ItemKind::Alias, rank::QUALIFIER, insert);
                item.detail = detail;
            }
        }
    }

    fn star_expansion(&mut self, reference: &SyntaxNode, levels: &[Level]) {
        let alone_in_select = reference
            .parent()
            .is_some_and(|item| item.kind() == SELECT_ITEM && item.children().count() == 1)
            && levels
                .first()
                .is_some_and(|level| level.clause == crate::resolve::Clause::Select);
        if !alone_in_select {
            return;
        }
        let Some(level) = levels.first() else {
            return;
        };
        if level.sources.is_empty() {
            return;
        }
        let qualify = level.sources.len() > 1;
        let mut names = Vec::new();
        for source in &level.sources {
            let columns = self.resolver.columns(source, 0);
            if columns.open {
                return;
            }
            for column in columns.columns {
                if self.resolver.hidden_from_wildcard(&column) {
                    continue;
                }
                let name = self.name(&column.name);
                names.push(if qualify {
                    format!("{}.{name}", self.name(&source.name.text))
                } else {
                    name
                });
            }
        }
        if names.is_empty() {
            return;
        }
        let text = names.join(", ");
        let label = if text.len() > 60 {
            format!(
                "{}...",
                &text[..text
                    .char_indices()
                    .take_while(|(index, _)| *index < 57)
                    .last()
                    .map_or(0, |(index, character)| index + character.len_utf8())]
            )
        } else {
            text.clone()
        };
        let item = self.push(label, ItemKind::Snippet, rank::QUALIFIER, text);
        item.detail = Some("all columns".to_string());
        item.filter_text = Some("*".to_string());
    }

    fn ctes(&mut self, at: &SyntaxNode) {
        for cte in self.resolver.ctes(at) {
            let insert = self.name(&cte.name.text);
            let item = self.push(cte.name.text.clone(), ItemKind::Alias, rank::LOCAL, insert);
            item.detail = Some("common table expression".to_string());
        }
    }

    // Items from the catalog

    fn tables(&mut self, schema: Option<&str>, only_tables: bool) {
        let ids = self.catalog.tables(schema);
        for id in ids {
            let table = self.catalog.table(id);
            if only_tables && table.kind.is_view() {
                continue;
            }
            let system = matches!(id.place, Place::System);
            let insert = self.name(&table.name);
            let kind = if table.kind.is_view() {
                ItemKind::View
            } else {
                ItemKind::Table
            };
            let rank = if system { rank::OTHER } else { rank::TABLE };
            let schema_name = self.catalog.schema_name(id.place, id.schema).to_string();
            let documentation = table_documentation(table);
            let label = table.name.clone();
            let detail = table.kind.label().to_string();
            let item = self.push(label, kind, rank, insert);
            item.detail = Some(detail);
            item.description = (!schema_name.is_empty()).then_some(schema_name);
            item.documentation = documentation;
        }
    }

    fn schemas(&mut self) {
        let detail = match self.dialect() {
            Dialect::Mysql | Dialect::Mariadb => "database",
            _ => "schema",
        };
        for name in self.catalog.schema_names() {
            let insert = self.name(&name);
            let item = self.push(name, ItemKind::Schema, rank::SCHEMA, insert);
            item.detail = Some(detail.to_string());
        }
    }

    fn call_text(&self, name: &str, takes_arguments: bool) -> (String, bool) {
        let name = if needs_quoting_as_function(name) {
            self.name(name)
        } else {
            name.to_string()
        };
        if !self.options.snippets {
            return (format!("{name}()"), false);
        }
        if takes_arguments {
            (format!("{name}($1)$0"), true)
        } else {
            (format!("{name}()$0"), true)
        }
    }

    fn functions(&mut self, procedures: bool) {
        let routines = self.catalog.all(|schema| &schema.routines, |routine| &routine.name);
        for routine in routines {
            let procedure = routine.kind == sql_catalog::model::RoutineKind::Procedure;
            if procedure != procedures {
                continue;
            }
            let (insert, snippet) = self.call_text(&routine.name, !routine.parameters.is_empty());
            let signature = crate::render::routine_signature(routine);
            let comment = routine.comment.clone();
            let kind = if procedure {
                ItemKind::Procedure
            } else {
                ItemKind::Function
            };
            let item = self.push(routine.name.clone(), kind, rank::ROUTINE, insert);
            item.snippet = snippet;
            item.detail = Some(signature);
            item.documentation = comment;
        }
        if procedures {
            return;
        }
        let target = self.target;
        let builtins = self.catalog.builtins;
        for function in &builtins.functions {
            let overloads: Vec<&Overload> = function.overloads_at(target).collect();
            if overloads.is_empty()
                || overloads
                    .iter()
                    .all(|overload| overload.kind == FunctionKind::Procedure)
            {
                continue;
            }
            let takes_arguments = overloads.iter().any(|overload| !overload.params.is_empty());
            let (insert, snippet) = self.call_text(&function.name, takes_arguments);
            let label = self.function_case(&function.name);
            let insert = if label != function.name {
                insert.replacen(&function.name, &label, 1)
            } else {
                insert
            };
            let detail = crate::render::overload_signature(&label, overloads[0]);
            let documentation = function.description.clone();
            let item = self.push(label, ItemKind::Function, rank::FUNCTION, insert);
            item.snippet = snippet;
            item.detail = Some(detail);
            item.description = Some(function.kind().label().to_string());
            item.documentation = documentation;
        }
    }

    fn function_case(&self, name: &str) -> String {
        match self.dialect() {
            Dialect::Postgres => name.to_string(),
            _ => self.keyword_case(&name.to_ascii_uppercase()),
        }
    }

    fn schema_functions(&mut self, schema: &Ident) {
        let routines = self.catalog.routines_in(schema);
        for routine in routines {
            let (insert, snippet) = self.call_text(&routine.name, !routine.parameters.is_empty());
            let signature = crate::render::routine_signature(routine);
            let item = self.push(routine.name.clone(), ItemKind::Function, rank::ROUTINE, insert);
            item.snippet = snippet;
            item.detail = Some(signature);
        }
    }

    fn types(&mut self) {
        let target = self.target;
        let builtins = self.catalog.builtins;
        for known in &builtins.types {
            if !known.versions.contains(target) || known.category == "pseudo" {
                continue;
            }
            let label = match self.dialect() {
                Dialect::Postgres => known.name.clone(),
                _ => self.keyword_case(&known.name.to_ascii_uppercase()),
            };
            let item = self.push(label.clone(), ItemKind::Type, rank::FUNCTION, label);
            item.detail = Some(known.category.clone());
        }
        let user_types = self.catalog.all(|schema| &schema.types, |user_type| &user_type.name);
        for user_type in user_types {
            let insert = self.name(&user_type.name);
            let detail = user_type.kind.label().to_string();
            let values = (user_type.kind == TypeKind::Enum).then(|| {
                user_type
                    .values
                    .iter()
                    .map(|value| format!("'{value}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            let item = self.push(user_type.name.clone(), ItemKind::Type, rank::TABLE, insert);
            item.detail = Some(detail);
            item.documentation = values;
        }
    }

    fn settings(&mut self) {
        let target = self.target;
        for setting in &self.catalog.builtins.settings {
            if !setting.versions.contains(target) {
                continue;
            }
            let item = self.push(
                setting.name.clone(),
                ItemKind::Setting,
                rank::LOCAL,
                setting.name.clone(),
            );
            item.detail = setting.data_type.clone();
        }
    }

    // Join conditions from foreign keys

    /// The sources a `JOIN` adds to, when the table at the cursor is the right side of one.
    fn join_left_sources(&self, table_ref: &SyntaxNode) -> Vec<Source> {
        let Some(join) = table_ref.parent().filter(|parent| parent.kind() == JOIN_EXPR) else {
            return Vec::new();
        };
        let Some(left) = join.children().next().filter(|first| first != table_ref) else {
            return Vec::new();
        };
        let mut sources = Vec::new();
        self.resolver.from_sources(&left, &mut sources, &mut Vec::new());
        sources
    }

    /// Tables that a foreign key links to a source already joined, with the condition.
    fn join_tables(&mut self, left: &[Source]) {
        let mut found = Vec::new();
        for source in left {
            let SourceKind::Table(left_id) = source.kind else {
                continue;
            };
            let left_table = self.catalog.table(left_id).clone();
            for id in self.catalog.tables(None) {
                let right = self.catalog.table(id);
                for (_, _, from_columns, to_columns) in links(self.catalog, left_id, &left_table, id, right) {
                    found.push((
                        id,
                        right.name.clone(),
                        source.name.text.clone(),
                        from_columns,
                        to_columns,
                    ));
                }
            }
        }
        for (_, table_name, left_name, left_columns, right_columns) in found {
            let right_name = self.name(&table_name);
            let left_name = self.name(&left_name);
            let condition = condition_text(&left_name, &left_columns, &right_name, &right_columns, |name| {
                self.name(name)
            });
            let text = format!("{right_name} ON {condition}");
            let item = self.push(text.clone(), ItemKind::Snippet, rank::TEMPLATE, text);
            item.detail = Some("join on a foreign key".to_string());
            item.filter_text = Some(table_name);
        }
    }

    /// In `JOIN x ON |`, the conditions foreign keys give between `x` and what it joins.
    fn join_conditions(&mut self, reference: &SyntaxNode) {
        let Some(on) = reference.parent().filter(|parent| parent.kind() == ON_CLAUSE) else {
            return;
        };
        let Some(join) = on.parent().filter(|parent| parent.kind() == JOIN_EXPR) else {
            return;
        };
        let operands: Vec<SyntaxNode> = join.children().filter(|inner| inner.kind() != ON_CLAUSE).collect();
        let [left, right] = operands.as_slice() else {
            return;
        };
        let mut left_sources = Vec::new();
        self.resolver.from_sources(left, &mut left_sources, &mut Vec::new());
        let mut right_sources = Vec::new();
        self.resolver.from_sources(right, &mut right_sources, &mut Vec::new());
        let Some(right_source) = right_sources.first() else {
            return;
        };
        let SourceKind::Table(right_id) = right_source.kind else {
            return;
        };
        let right_table = self.catalog.table(right_id).clone();
        let mut conditions = Vec::new();
        for source in &left_sources {
            let SourceKind::Table(left_id) = source.kind else {
                continue;
            };
            let left_table = self.catalog.table(left_id).clone();
            for (_, _, left_columns, right_columns) in links(self.catalog, left_id, &left_table, right_id, &right_table)
            {
                let condition = condition_text(
                    &self.name(&right_source.name.text),
                    &right_columns,
                    &self.name(&source.name.text),
                    &left_columns,
                    |name| self.name(name),
                );
                conditions.push(condition);
            }
        }
        for condition in conditions {
            let item = self.push(condition.clone(), ItemKind::Snippet, rank::TEMPLATE, condition);
            item.detail = Some("foreign key".to_string());
        }
    }

    // Enum values

    fn enum_values(&mut self, reference: &SyntaxNode) {
        let Some(column) = compared_column(reference) else {
            return;
        };
        for value in self.enum_values_of(&column) {
            let text = format!("'{}'", value.replace('\'', "''"));
            let item = self.push(text.clone(), ItemKind::Value, rank::TEMPLATE, text);
            item.detail = Some("enum value".to_string());
            item.filter_text = Some(value);
        }
    }

    fn enum_values_of(&self, column: &SyntaxNode) -> Vec<String> {
        enum_values_of(&self.resolver, self.catalog, column)
    }

    // Keywords

    fn statement_keywords(&mut self) {
        let target = self.target;
        let rows: &[(&str, &str)] = &[
            ("SELECT", ""),
            ("WITH", ""),
            ("INSERT INTO", ""),
            ("UPDATE", ""),
            ("DELETE FROM", ""),
            ("REPLACE INTO", "replace"),
            ("MERGE INTO", "merge"),
            ("VALUES", ""),
            ("CREATE TABLE", ""),
            ("CREATE VIEW", ""),
            ("CREATE INDEX", ""),
            ("CREATE UNIQUE INDEX", ""),
            ("CREATE TRIGGER", ""),
            ("CREATE FUNCTION", "routines"),
            ("CREATE PROCEDURE", "routines"),
            ("CREATE SCHEMA", "create-schema"),
            ("CREATE SEQUENCE", "sequences"),
            ("CREATE TYPE", "create-type"),
            ("ALTER TABLE", ""),
            ("DROP TABLE", ""),
            ("DROP VIEW", ""),
            ("DROP INDEX", ""),
            ("TRUNCATE TABLE", "truncate"),
            ("BEGIN", ""),
            ("START TRANSACTION", "start-transaction"),
            ("COMMIT", ""),
            ("ROLLBACK", ""),
            ("EXPLAIN", ""),
            ("SET", "set"),
            ("SHOW", "show"),
            ("USE", "use"),
            ("CALL", "call"),
            ("PRAGMA", "pragma"),
            ("ATTACH DATABASE", "attach-detach"),
            ("GRANT", "grant-revoke"),
            ("REVOKE", "grant-revoke"),
            ("COPY", "copy"),
        ];
        let words: Vec<&str> = rows
            .iter()
            .filter(|(_, feature)| feature.is_empty() || supports(feature, target))
            .map(|(word, _)| *word)
            .collect();
        self.keywords(&words);
    }

    fn expression_keywords(&mut self, reference: &SyntaxNode) {
        let mut words = vec![
            "CASE",
            "NOT",
            "NULL",
            "TRUE",
            "FALSE",
            "EXISTS",
            "CAST",
            "INTERVAL",
            "CURRENT_DATE",
            "CURRENT_TIMESTAMP",
        ];
        let first_in_select = reference
            .parent()
            .and_then(|item| item.parent())
            .filter(|list| list.kind() == SELECT_LIST)
            .is_some_and(|list| {
                list.children()
                    .next()
                    .is_some_and(|first| first.text_range().contains_range(reference.text_range()))
            });
        if first_in_select {
            words.push("DISTINCT");
            if supports("distinct-on", self.target) && self.dialect() == Dialect::Postgres {
                words.push("DISTINCT ON");
            }
        }
        if self.dialect() == Dialect::Sqlite {
            words.retain(|word| *word != "INTERVAL");
        }
        self.keywords(&words);
    }

    fn column_constraints(&mut self) {
        let target = self.target;
        let rows: &[(&str, &str)] = &[
            ("NOT NULL", ""),
            ("NULL", ""),
            ("DEFAULT", ""),
            ("PRIMARY KEY", ""),
            ("UNIQUE", ""),
            ("REFERENCES", ""),
            ("CHECK", ""),
            ("COLLATE", ""),
            ("GENERATED ALWAYS AS", ""),
            ("GENERATED ALWAYS AS IDENTITY", "identity-columns"),
            ("AUTO_INCREMENT", "auto-increment"),
            ("AUTOINCREMENT", "autoincrement"),
            ("COMMENT", "column-comments"),
            ("UNSIGNED", "unsigned-types"),
            ("ON UPDATE", "on-update"),
        ];
        let words: Vec<&str> = rows
            .iter()
            .filter(|(_, feature)| feature.is_empty() || supports(feature, target))
            .map(|(word, _)| *word)
            .collect();
        self.keywords(&words);
    }

    /// After an alias position: the alias of a table or select item, or a keyword that goes on.
    fn after_operand(&mut self, name: &SyntaxNode) {
        let Some(token) = name.first_token() else {
            return;
        };
        self.after_operand_token(&token);
    }

    /// What may follow a whole operand: the clauses of the query or statement still to come.
    fn after_operand_token(&mut self, token: &SyntaxToken) {
        if let Some(column) = token.parent_ancestors().find(|ancestor| ancestor.kind() == COLUMN_DEF) {
            if child(&column, TYPE).is_some_and(|data_type| data_type.text_range().end() <= token.text_range().start())
            {
                self.column_constraints();
            }
            return;
        }
        let Some(owner) = operand_owner(token) else {
            return;
        };
        let offset = token.text_range().start();
        let target = self.target;
        let mut words: Vec<&'static str> = Vec::new();
        let feature = |id: &str| supports(id, target);
        let present = |kind| child(&owner, kind).is_some_and(|clause: SyntaxNode| clause.text_range().start() < offset);
        let after = |kind| child(&owner, kind).is_none_or(|clause: SyntaxNode| clause.text_range().start() >= offset);
        let current = owner
            .children()
            .filter(|inner| inner.text_range().start() < offset)
            .last()
            .map(|inner| inner.kind());
        match owner.kind() {
            SELECT => {
                match current {
                    Some(SELECT_LIST) => {
                        words.extend(["AS", "FROM"]);
                    }
                    Some(FROM_CLAUSE) => {
                        let in_join_without_condition = token
                            .parent_ancestors()
                            .find(|ancestor| ancestor.kind() == JOIN_EXPR)
                            .is_some_and(|join| {
                                child(&join, ON_CLAUSE).is_none()
                                    && child(&join, USING_CLAUSE).is_none()
                                    && !has_token(&join, CROSS_KW)
                                    && !has_token(&join, NATURAL_KW)
                            });
                        if in_join_without_condition {
                            words.extend(["ON", "USING"]);
                        }
                        words.extend(["AS", "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "CROSS JOIN"]);
                        if feature("full-join") {
                            words.push("FULL JOIN");
                        }
                        words.push("NATURAL JOIN");
                        if feature("lateral") {
                            words.push("LEFT JOIN LATERAL");
                        }
                    }
                    Some(WHERE_CLAUSE) | Some(HAVING_CLAUSE) => {
                        words.extend(["AND", "OR", "IS NULL", "IS NOT NULL", "IN", "NOT IN", "LIKE", "BETWEEN"])
                    }
                    Some(GROUP_BY_CLAUSE) => {
                        if feature("with-rollup") {
                            words.push("WITH ROLLUP");
                        }
                    }
                    Some(ORDER_BY_CLAUSE) => words.extend(["ASC", "DESC"]),
                    _ => {}
                }
                let clauses: [(&'static str, sql_syntax::SyntaxKind, &str); 4] = [
                    ("WHERE", WHERE_CLAUSE, ""),
                    ("GROUP BY", GROUP_BY_CLAUSE, ""),
                    ("HAVING", HAVING_CLAUSE, ""),
                    ("WINDOW", WINDOW_CLAUSE, ""),
                ];
                if present(FROM_CLAUSE) || current == Some(FROM_CLAUSE) {
                    for (word, kind, _) in clauses {
                        if after(kind) && !present(kind) && current != Some(kind) {
                            words.push(word);
                        }
                    }
                }
                let query = outermost_query(&owner);
                let trailing = |kind| child(&query, kind).is_none();
                if trailing(ORDER_BY_CLAUSE) {
                    words.push("ORDER BY");
                }
                if trailing(LIMIT_CLAUSE) {
                    words.push("LIMIT");
                }
                if feature("fetch-first") && trailing(FETCH_CLAUSE) && trailing(LIMIT_CLAUSE) {
                    words.push("FETCH FIRST");
                }
                if feature("for-update") {
                    words.push("FOR UPDATE");
                }
                words.extend(["UNION", "UNION ALL"]);
                if feature("intersect-except") {
                    words.extend(["INTERSECT", "EXCEPT"]);
                }
            }
            UPDATE_STMT => {
                if !child(&owner, SET_CLAUSE).is_some_and(|clause| has_token(&clause, SET_KW)) {
                    words.extend(["AS", "SET"]);
                } else {
                    if feature("update-from") && child(&owner, FROM_CLAUSE).is_none() {
                        words.push("FROM");
                    }
                    words.push("WHERE");
                    if feature("update-returning") {
                        words.push("RETURNING");
                    }
                }
            }
            DELETE_STMT => {
                words.push("WHERE");
                if feature("delete-using") {
                    words.push("USING");
                }
                if feature("delete-returning") {
                    words.push("RETURNING");
                }
            }
            INSERT_STMT => {
                let has_values = owner
                    .children()
                    .any(|inner| inner.kind() == VALUES || is_query(inner.kind()));
                if !has_values {
                    self.insert_templates(&owner);
                    words.extend(["VALUES", "SELECT"]);
                    if feature("insert-set") {
                        words.push("SET");
                    }
                    if feature("default-values") {
                        words.push("DEFAULT VALUES");
                    }
                } else {
                    if feature("on-conflict") {
                        words.extend(["ON CONFLICT", "ON CONFLICT DO NOTHING"]);
                    }
                    if feature("on-duplicate-key-update") {
                        words.push("ON DUPLICATE KEY UPDATE");
                    }
                    if feature("insert-returning") {
                        words.push("RETURNING");
                    }
                }
            }
            _ => {}
        }
        self.keywords(&words);
    }

    /// After `INSERT INTO t` or `INSERT INTO t (a, b)`: the column list and a `VALUES` template.
    fn insert_templates(&mut self, statement: &SyntaxNode) {
        let Some((schema, table)) =
            child(statement, QUALIFIED_NAME).and_then(|name| object_name(&name, self.dialect()))
        else {
            return;
        };
        let listed: Option<Vec<String>> = child(statement, NAME_LIST).map(|list| {
            parts(&list, self.dialect())
                .into_iter()
                .map(|part| part.ident.text)
                .collect()
        });
        let columns: Vec<String> = match &listed {
            Some(listed) => listed.clone(),
            None => {
                let Some(id) = self
                    .catalog
                    .find_table(schema.as_ref().map(|part| &part.ident), &table.ident)
                else {
                    return;
                };
                self.catalog
                    .table(id)
                    .columns
                    .iter()
                    .filter(|column| !column.auto_increment && column.generated.is_none() && !column.invisible)
                    .map(|column| column.name.clone())
                    .collect()
            }
        };
        if columns.is_empty() {
            return;
        }
        let names: Vec<String> = columns.iter().map(|column| self.name(column)).collect();
        let values: Vec<String> = if self.options.snippets {
            names
                .iter()
                .enumerate()
                .map(|(index, name)| format!("${{{}:{}}}", index + 1, name.replace(['$', '}', '\\'], "")))
                .collect()
        } else {
            names.clone()
        };
        let values_word = self.keyword_case("VALUES");
        let (label, text) = match listed {
            Some(_) => (
                format!("{values_word} ({})", names.join(", ")),
                format!("{values_word} ({})", values.join(", ")),
            ),
            None => (
                format!("({}) {values_word} (...)", names.join(", ")),
                format!("({}) {values_word} ({})", names.join(", "), values.join(", ")),
            ),
        };
        let snippet = self.options.snippets;
        let item = self.push(label, ItemKind::Snippet, rank::TEMPLATE, text);
        item.snippet = snippet;
        item.detail = Some("columns and values".to_string());
        item.filter_text = Some(values_word);
    }
}

/// A statement whose clauses completion continues after an operand.
fn is_dml(kind: sql_syntax::SyntaxKind) -> bool {
    matches!(kind, UPDATE_STMT | DELETE_STMT | INSERT_STMT)
}

/// The query or statement a token after an operand continues: the `SELECT` or DML statement it
/// is in, or the last `SELECT` of the query before it when the parser left it outside.
fn operand_owner(token: &SyntaxToken) -> Option<SyntaxNode> {
    if let Some(owner) = token
        .parent_ancestors()
        .find(|ancestor| ancestor.kind() == SELECT || is_dml(ancestor.kind()))
    {
        return Some(owner);
    }
    let statement = token
        .parent_ancestors()
        .find(|ancestor| ancestor.kind() == SELECT_STMT)?;
    let mut query = statement
        .children()
        .filter(|inner| is_query(inner.kind()) && inner.text_range().end() <= token.text_range().start())
        .last()?;
    loop {
        if query.kind() == SELECT {
            return Some(query);
        }
        query = query.children().filter(|inner| is_query(inner.kind())).last()?;
    }
}

fn needs_quoting_as_function(name: &str) -> bool {
    !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '$')
}

fn outermost_query(select: &SyntaxNode) -> SyntaxNode {
    let mut current = select.clone();
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), COMPOUND_SELECT) {
            current = parent;
        } else {
            break;
        }
    }
    current
}

fn previous_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut current = token.prev_token();
    while let Some(previous) = current {
        if !previous.kind().is_trivia() {
            return Some(previous);
        }
        current = previous.prev_token();
    }
    None
}

fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut letters = haystack.chars();
    needle.chars().all(|wanted| letters.any(|letter| letter == wanted))
}

/// The foreign keys between two tables, either way: the table that holds the key, the table it
/// references, and the columns on each side as the left table and the right table have them.
fn links(
    catalog: &Catalog,
    left_id: TableId,
    left: &Table,
    right_id: TableId,
    right: &Table,
) -> Vec<(TableId, TableId, Vec<String>, Vec<String>)> {
    let mut found = Vec::new();
    let refers = |key: &sql_catalog::model::ForeignKey, to: &Table, to_id: TableId| {
        catalog.table_case.eq(&key.referenced_table, &to.name)
            && key.referenced_schema.as_deref().is_none_or(|schema| {
                catalog
                    .table_case
                    .eq(schema, catalog.schema_name(to_id.place, to_id.schema))
            })
    };
    for key in &left.foreign_keys {
        if refers(key, right, right_id) {
            let to = if key.referenced_columns.is_empty() {
                right
                    .primary_key
                    .as_ref()
                    .map(|key| key.columns.clone())
                    .unwrap_or_default()
            } else {
                key.referenced_columns.clone()
            };
            found.push((left_id, right_id, key.columns.clone(), to));
        }
    }
    if left_id != right_id {
        for key in &right.foreign_keys {
            if refers(key, left, left_id) {
                let to = if key.referenced_columns.is_empty() {
                    left.primary_key
                        .as_ref()
                        .map(|key| key.columns.clone())
                        .unwrap_or_default()
                } else {
                    key.referenced_columns.clone()
                };
                found.push((right_id, left_id, to, key.columns.clone()));
            }
        }
    }
    found.retain(|(_, _, a, b)| !a.is_empty() && a.len() == b.len());
    found
}

fn condition_text(
    left: &str,
    left_columns: &[String],
    right: &str,
    right_columns: &[String],
    name: impl Fn(&str) -> String,
) -> String {
    left_columns
        .iter()
        .zip(right_columns)
        .map(|(a, b)| format!("{left}.{} = {right}.{}", name(a), name(b)))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The column a value is compared with: the other side of `=`, `<>`, `IN`, or `CASE x WHEN`.
fn compared_column(value: &SyntaxNode) -> Option<SyntaxNode> {
    let mut current = value.clone();
    loop {
        let parent = current.parent()?;
        match parent.kind() {
            BINARY_EXPR => {
                return parent
                    .children()
                    .find(|other| *other != current && other.kind() == COLUMN_REF);
            }
            IN_LIST => current = parent,
            IN_EXPR => {
                return parent.children().next().filter(|first| first.kind() == COLUMN_REF);
            }
            _ => return None,
        }
    }
}

/// The values of the enum type of a column: MySQL's `enum('a','b')` or a PostgreSQL enum type.
pub fn enum_values_of(resolver: &Resolver, catalog: &Catalog, column: &SyntaxNode) -> Vec<String> {
    let Some(crate::resolve::Resolution::Found(crate::resolve::Referent::Column { column, .. })) =
        resolver.resolve_column_ref(column).pop()
    else {
        return Vec::new();
    };
    let ColumnOrigin::Table(id, position) = column.origin else {
        return Vec::new();
    };
    let Some(data_type) = catalog.table(id).columns[position].data_type.clone() else {
        return Vec::new();
    };
    let lower = data_type.trim().to_ascii_lowercase();
    if lower.starts_with("enum(") || lower.starts_with("enum (") || lower.starts_with("set(") {
        return quoted_values(&data_type);
    }
    let name = data_type.rsplit('.').next().unwrap_or(&data_type).trim_matches('"');
    catalog
        .user_type(None, name)
        .filter(|(_, user_type)| user_type.kind == TypeKind::Enum)
        .map(|(_, user_type)| user_type.values.clone())
        .unwrap_or_default()
}

fn quoted_values(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\'' {
            continue;
        }
        let mut value = String::new();
        while let Some(inner) = chars.next() {
            if inner == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    value.push('\'');
                    continue;
                }
                break;
            }
            value.push(inner);
        }
        values.push(value);
    }
    values
}

fn column_documentation(column: &Column) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(comment) = &column.comment {
        lines.push(comment.clone());
    }
    let facts = crate::render::column_facts(column);
    if !facts.is_empty() {
        lines.push(facts);
    }
    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

fn table_documentation(table: &Table) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(comment) = &table.comment {
        lines.push(comment.clone());
    }
    if !table.columns.is_empty() && table.kind != TableKind::System {
        let names: Vec<&str> = table
            .columns
            .iter()
            .take(12)
            .map(|column| column.name.as_str())
            .collect();
        let more = if table.columns.len() > 12 { ", ..." } else { "" };
        lines.push(format!("Columns: {}{more}", names.join(", ")));
    }
    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

/// Completion inside a string: the values of an enum column compared with it, and the sequences
/// of `nextval`, `currval` and `setval`.
fn complete_in_string(
    root: &SyntaxNode,
    text: &str,
    offset: usize,
    target: Target,
    schemas: Schemas,
) -> Option<CompletionList> {
    let at = TextSize::from(offset as u32);
    let token = root
        .token_at_offset(at)
        .find(|token| token.kind() == STRING && token.text_range().start() < at)?;
    let range = token.text_range();
    let closed = token.text().len() >= 2 && token.text().ends_with('\'');
    if closed && range.end() <= at {
        return None;
    }
    let content_start = u32::from(range.start()) + 1;
    let content_end = if closed {
        u32::from(range.end()) - 1
    } else {
        u32::from(range.end())
    };
    let typed = text[content_start as usize..offset].to_lowercase();
    let literal = token.parent()?;
    let document = DocumentSchema::before(root, u32::from(range.start()), target, schemas);
    let catalog = document.catalog();
    let resolver = Resolver::new(&catalog);
    let mut values: Vec<(String, ItemKind, &str)> = Vec::new();
    if let Some(column) = compared_column(&literal) {
        for value in enum_values_of(&resolver, &catalog, &column) {
            values.push((value, ItemKind::Value, "enum value"));
        }
    }
    let call = literal
        .parent()
        .filter(|list| list.kind() == ARG_LIST)
        .and_then(|list| list.parent())
        .filter(|call| call.kind() == FUNCTION_CALL);
    if let Some(call) = call {
        let function = child(&call, QUALIFIED_NAME).map(|name| compact(&name).to_ascii_lowercase());
        if matches!(function.as_deref(), Some("nextval" | "currval" | "setval")) {
            for sequence in catalog.all(|schema| &schema.sequences, |sequence| &sequence.name) {
                values.push((sequence.name.clone(), ItemKind::Sequence, "sequence"));
            }
        }
    }
    let mut list = CompletionList::default();
    for (order, (value, kind, detail)) in values.into_iter().enumerate() {
        if !typed.is_empty() && !subsequence(&typed, &value.to_lowercase()) {
            continue;
        }
        list.items.push(CompletionItem {
            label: value.clone(),
            kind,
            detail: Some(detail.to_string()),
            description: None,
            documentation: None,
            edit: TextEdit {
                start: content_start,
                end: content_end.max(offset as u32),
                new_text: value.replace('\'', "''"),
            },
            snippet: false,
            sort_text: format!("{order:05}"),
            filter_text: None,
        });
    }
    Some(list)
}
