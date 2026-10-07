//! Name resolution: what a name in a statement stands for. A column is looked up in the scopes
//! around it, from the innermost query outward (a correlated subquery sees the query it is in);
//! a table in the common table expressions that are visible, then in the catalog.
//!
//! A scope is a level per query or statement the name is in, with the tables of its `FROM` (or the
//! target of an `INSERT`, `UPDATE`, `DELETE` or `MERGE`), the clause the name stands in, and the
//! select list whose aliases that clause may see. Which clauses see aliases is the dialect's rule,
//! as the servers answer: `ORDER BY` everywhere, before the columns; `GROUP BY` after the columns;
//! `HAVING` in MySQL, MariaDB and SQLite; `WHERE` in SQLite. PostgreSQL takes an alias only as a
//! whole item of `ORDER BY` or `GROUP BY`, and MySQL only as a whole item of `GROUP BY`.

use sql_catalog::model::Table;
use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode};

use crate::ast::{Part, alias_of, child, children, has_token, inner_query, is_query, object_name, parts};
use crate::catalog::{Catalog, ObjectId, TableId};
use crate::ident::{Case, Ident, name_case};

/// The name of the column a select item gives its query: its alias, the column it names, or in
/// PostgreSQL the function it calls.
pub fn output_name(item: &SyntaxNode, dialect: Dialect) -> Option<String> {
    if let Some((alias, _)) = alias_of(item, dialect) {
        return Some(alias.ident.text);
    }
    let expression = item.children().next()?;
    match expression.kind() {
        COLUMN_REF => parts(&expression, dialect).pop().map(|part| part.ident.text),
        FUNCTION_CALL if dialect == Dialect::Postgres => child(&expression, QUALIFIED_NAME)
            .and_then(|name| parts(&name, dialect).pop())
            .map(|part| part.ident.text),
        _ => None,
    }
}

/// What a query or a table of `FROM` reads rows from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Table(TableId),
    /// A common table expression, by its `CTE` node.
    Cte(SyntaxNode),
    /// A subquery in `FROM`, by its query.
    Derived(SyntaxNode),
    /// A function in `FROM`, by its `TABLE_FUNCTION`.
    Function(SyntaxNode),
    /// The table a `CREATE TABLE` or `CREATE TYPE` is defining, by its `TABLE_ELEMENT_LIST`.
    Defined(SyntaxNode),
    /// A table name that resolves to nothing known.
    Unknown,
}

/// A table of a scope under the name a qualifier uses for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub name: Ident,
    /// The schema of an unaliased table, which a three-part column name may give.
    pub schema: Option<Ident>,
    pub kind: SourceKind,
    /// The node that brings the source in: a `TABLE_REF`, `DERIVED_TABLE`, `TABLE_FUNCTION`, an
    /// `ALIAS`, or the statement for a target.
    pub node: SyntaxNode,
    /// The `NAME` of the alias, or of the table when it has none.
    pub name_node: Option<SyntaxNode>,
    pub aliased: bool,
    /// Column names the alias gives, in place of the source's own.
    pub renames: Vec<Part>,
}

/// Where a column of a source comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnOrigin {
    /// A column of a table of the catalog, by its position.
    Table(TableId, usize),
    /// A select item of a query.
    Item(SyntaxNode),
    /// A name in a column list: of an alias, a common table expression or a `CREATE TABLE`.
    Declared(SyntaxNode),
    /// A column only a dialect's rules give, as `rowid` or `column1` of `VALUES`.
    Implicit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputColumn {
    pub name: String,
    pub origin: ColumnOrigin,
}

/// The columns of a source, and whether there may be more than these.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Columns {
    pub columns: Vec<OutputColumn>,
    pub open: bool,
}

/// The clause a name stands in, which decides what it may see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clause {
    Select,
    From,
    Where,
    GroupBy,
    Having,
    Window,
    OrderBy,
    /// The columns an `INSERT` lists, the targets of `SET`.
    Target,
    Other,
}

/// One query or statement of a scope.
#[derive(Clone, Debug)]
pub struct Level {
    pub node: SyntaxNode,
    pub clause: Clause,
    pub sources: Vec<Source>,
    /// Columns that `USING` or `NATURAL` made one, which are not ambiguous.
    pub merged: Vec<Ident>,
    /// The `SELECT` whose aliases the clause may see.
    pub select: Option<SyntaxNode>,
    /// Parameters and variables of a routine body: their `NAME` nodes.
    pub variables: Vec<Part>,
    /// Whether the name stands alone in its clause item, as `ORDER BY a` and not `ORDER BY a + 1`.
    pub bare: bool,
}

/// A common table expression in scope.
#[derive(Clone, Debug)]
pub struct Cte {
    pub name: Ident,
    pub node: SyntaxNode,
}

/// What a name stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Referent {
    Table(TableId),
    Cte(SyntaxNode),
    /// A table of a scope under an alias, or a subquery or function in `FROM`.
    Source(Source),
    Column {
        source: Source,
        column: OutputColumn,
    },
    /// An alias of the select list, by its `SELECT_ITEM`.
    SelectAlias(SyntaxNode),
    /// A parameter or variable of a routine, by its `NAME`.
    Variable(SyntaxNode),
    Schema(String),
    /// A built-in function, by its name in lower case.
    Function(String),
    /// Routines of the schema of one name.
    Routines(Vec<ObjectId>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Found(Referent),
    Ambiguous(Vec<Referent>),
    /// Nothing known has the name. `complete` says everything it could be is known, so the name
    /// is surely wrong.
    Unknown {
        complete: bool,
    },
}

/// The deepest a query may nest before resolution stops following it.
const DEPTH: u32 = 32;

/// Resolves names of one statement against a catalog.
pub struct Resolver<'c, 'a> {
    pub catalog: &'c Catalog<'a>,
    pub dialect: Dialect,
}

impl<'c, 'a> Resolver<'c, 'a> {
    pub fn new(catalog: &'c Catalog<'a>) -> Resolver<'c, 'a> {
        Resolver {
            catalog,
            dialect: catalog.dialect(),
        }
    }

    fn column_case(&self) -> Case {
        name_case(self.dialect)
    }

    /// The common table expressions visible at a node, the innermost first.
    pub fn ctes(&self, node: &SyntaxNode) -> Vec<Cte> {
        let mut found = Vec::new();
        let mut inner = node.clone();
        while let Some(parent) = inner.parent() {
            // A script holds statements, never a `WITH` of its own; looking would cost a walk over
            // every statement for every name.
            if parent.kind() == SOURCE_FILE {
                break;
            }
            if let Some(with) = child(&parent, WITH_CLAUSE) {
                let recursive = has_token(&with, RECURSIVE_KW);
                let within = with.text_range().contains_range(node.text_range());
                for cte in children(&with, CTE) {
                    let here = cte.text_range().contains_range(node.text_range());
                    if within && here && !recursive {
                        break;
                    }
                    if let Some(name) = child(&cte, NAME).and_then(|name| Ident::of_name(&name, self.dialect)) {
                        found.push(Cte {
                            name,
                            node: cte.clone(),
                        });
                    }
                    if within && here {
                        break;
                    }
                }
            }
            inner = parent;
        }
        found
    }

    fn find_cte(&self, ctes: &[Cte], name: &Ident) -> Option<SyntaxNode> {
        ctes.iter()
            .find(|cte| {
                self.catalog.table_case.eq(&cte.name.text, &name.text)
                    || self.column_case().eq(&cte.name.text, &name.text)
            })
            .map(|cte| cte.node.clone())
    }

    /// The source a `TABLE_REF` brings in.
    fn table_ref_source(&self, node: &SyntaxNode) -> Option<Source> {
        let name = child(node, QUALIFIED_NAME)?;
        let (schema, table) = object_name(&name, self.dialect)?;
        let kind = self.table_kind(node, schema.as_ref().map(|part| &part.ident), &table.ident);
        Some(self.named_source(node, kind, table, schema.map(|part| part.ident)))
    }

    /// What a table name in `FROM` reads: a common table expression or a table of the catalog.
    pub fn table_kind(&self, at: &SyntaxNode, schema: Option<&Ident>, name: &Ident) -> SourceKind {
        if schema.is_none() {
            if let Some(cte) = self.find_cte(&self.ctes(at), name) {
                return SourceKind::Cte(cte);
            }
        }
        match self.catalog.find_table(schema, name) {
            Some(id) => SourceKind::Table(id),
            None => SourceKind::Unknown,
        }
    }

    fn named_source(&self, node: &SyntaxNode, kind: SourceKind, table: Part, schema: Option<Ident>) -> Source {
        match alias_of(node, self.dialect) {
            Some((alias, renames)) => Source {
                name: alias.ident,
                schema: None,
                kind,
                node: node.clone(),
                name_node: Some(alias.node),
                aliased: true,
                renames,
            },
            None => Source {
                name: table.ident,
                schema,
                kind,
                node: node.clone(),
                name_node: Some(table.node),
                aliased: false,
                renames: Vec::new(),
            },
        }
    }

    /// The sources a table expression of `FROM` brings in, in order, with the columns its joins
    /// merge.
    pub fn from_sources(&self, node: &SyntaxNode, sources: &mut Vec<Source>, merged: &mut Vec<Ident>) {
        match node.kind() {
            TABLE_REF => sources.extend(self.table_ref_source(node)),
            DERIVED_TABLE => {
                let query = child(node, PAREN_QUERY).unwrap_or_else(|| node.clone());
                let (name, renames, name_node) = match alias_of(node, self.dialect) {
                    Some((alias, renames)) => (alias.ident, renames, Some(alias.node)),
                    None => (Ident::new(""), Vec::new(), None),
                };
                sources.push(Source {
                    name,
                    schema: None,
                    kind: SourceKind::Derived(query),
                    node: node.clone(),
                    name_node,
                    aliased: true,
                    renames,
                });
            }
            TABLE_FUNCTION => {
                let function_name = node
                    .descendants()
                    .find(|inner| inner.kind() == FUNCTION_CALL)
                    .and_then(|call| child(&call, QUALIFIED_NAME))
                    .and_then(|name| parts(&name, self.dialect).pop());
                let (name, renames, name_node, aliased) = match alias_of(node, self.dialect) {
                    Some((alias, renames)) => (alias.ident, renames, Some(alias.node), true),
                    None => match function_name {
                        Some(part) => (part.ident, Vec::new(), Some(part.node), false),
                        None => (Ident::new(""), Vec::new(), None, false),
                    },
                };
                sources.push(Source {
                    name,
                    schema: None,
                    kind: SourceKind::Function(node.clone()),
                    node: node.clone(),
                    name_node,
                    aliased,
                    renames,
                });
            }
            JOIN_EXPR => {
                let start = sources.len();
                let operands: Vec<SyntaxNode> = node
                    .children()
                    .filter(|inner| !matches!(inner.kind(), ON_CLAUSE | USING_CLAUSE))
                    .collect();
                let mut middle = start;
                for (position, operand) in operands.iter().enumerate() {
                    if position == 1 {
                        middle = sources.len();
                    }
                    self.from_sources(operand, sources, merged);
                }
                if let Some(using) = child(node, USING_CLAUSE) {
                    if let Some(list) = child(&using, NAME_LIST) {
                        merged.extend(parts(&list, self.dialect).into_iter().map(|part| part.ident));
                    }
                }
                if has_token(node, NATURAL_KW) {
                    let names = |range: std::ops::Range<usize>, sources: &[Source]| -> Vec<String> {
                        sources[range]
                            .iter()
                            .flat_map(|source| self.columns(source, 0).columns)
                            .map(|column| column.name)
                            .collect()
                    };
                    let left = names(start..middle, sources);
                    let right = names(middle..sources.len(), sources);
                    for name in left {
                        if right.iter().any(|other| self.column_case().eq(other, &name)) {
                            merged.push(Ident::new(name));
                        }
                    }
                }
            }
            PAREN_JOIN | FROM_CLAUSE | USING_CLAUSE => {
                for inner in node.children() {
                    if matches!(
                        inner.kind(),
                        TABLE_REF | DERIVED_TABLE | TABLE_FUNCTION | JOIN_EXPR | PAREN_JOIN
                    ) {
                        self.from_sources(&inner, sources, merged);
                    }
                }
            }
            _ => {}
        }
    }

    /// The columns of a source.
    pub fn columns(&self, source: &Source, depth: u32) -> Columns {
        if depth > DEPTH {
            return Columns {
                columns: Vec::new(),
                open: true,
            };
        }
        let mut columns = match &source.kind {
            SourceKind::Table(id) => self.table_columns(*id),
            SourceKind::Unknown => Columns {
                columns: Vec::new(),
                open: true,
            },
            SourceKind::Cte(cte) => {
                let mut columns = inner_query(cte).map_or_else(
                    || Columns {
                        columns: Vec::new(),
                        open: true,
                    },
                    |query| self.outputs(&query, depth + 1),
                );
                if let Some(list) = child(cte, NAME_LIST) {
                    rename(&mut columns, parts(&list, self.dialect));
                }
                columns
            }
            SourceKind::Derived(query) => self.outputs(query, depth + 1),
            SourceKind::Function(node) => self.function_columns(node, source),
            SourceKind::Defined(list) => Columns {
                columns: children(list, COLUMN_DEF)
                    .filter_map(|column| child(&column, NAME))
                    .filter_map(|name| {
                        Some(OutputColumn {
                            name: Ident::of_name(&name, self.dialect)?.text,
                            origin: ColumnOrigin::Declared(name),
                        })
                    })
                    .collect(),
                open: false,
            },
        };
        if !source.renames.is_empty() {
            rename(&mut columns, source.renames.clone());
        }
        columns
    }

    fn table_columns(&self, id: TableId) -> Columns {
        let table: &Table = self.catalog.table(id);
        let mut columns: Vec<OutputColumn> = table
            .columns
            .iter()
            .enumerate()
            .map(|(position, column)| OutputColumn {
                name: column.name.clone(),
                origin: ColumnOrigin::Table(id, position),
            })
            .collect();
        let implicit: &[&str] = match (self.dialect, table.kind.is_view()) {
            (Dialect::Sqlite, false) => &["rowid", "oid", "_rowid_"],
            (Dialect::Postgres, false) => &["ctid", "xmin", "xmax", "cmin", "cmax", "tableoid"],
            _ => &[],
        };
        for name in implicit {
            if !columns.iter().any(|column| column.name.eq_ignore_ascii_case(name)) {
                columns.push(OutputColumn {
                    name: name.to_string(),
                    origin: ColumnOrigin::Implicit,
                });
            }
        }
        Columns {
            columns,
            open: table.open,
        }
    }

    fn function_columns(&self, node: &SyntaxNode, source: &Source) -> Columns {
        if let Some(list) = node.descendants().find(|inner| inner.kind() == JSON_TABLE_COLUMNS) {
            let columns = list
                .descendants()
                .filter(|inner| inner.kind() == JSON_TABLE_COLUMN)
                .filter_map(|column| child(&column, NAME))
                .filter_map(|name| {
                    Some(OutputColumn {
                        name: Ident::of_name(&name, self.dialect)?.text,
                        origin: ColumnOrigin::Declared(name),
                    })
                })
                .collect();
            return Columns { columns, open: false };
        }
        let _ = source;
        Columns {
            columns: Vec::new(),
            open: true,
        }
    }

    /// The columns a query gives.
    pub fn outputs(&self, query: &SyntaxNode, depth: u32) -> Columns {
        if depth > DEPTH {
            return Columns {
                columns: Vec::new(),
                open: true,
            };
        }
        match query.kind() {
            SELECT => self.select_outputs(query, depth),
            COMPOUND_SELECT | PAREN_QUERY | SELECT_STMT => {
                match query.children().find(|inner| is_query(inner.kind())) {
                    Some(first) => self.outputs(&first, depth + 1),
                    None => Columns {
                        columns: Vec::new(),
                        open: true,
                    },
                }
            }
            VALUES => {
                let count = child(query, ROW_EXPR).map_or(0, |row| row.children().count());
                let columns = (0..count)
                    .map(|index| OutputColumn {
                        name: match self.dialect {
                            Dialect::Mysql | Dialect::Mariadb => format!("column_{index}"),
                            _ => format!("column{}", index + 1),
                        },
                        origin: ColumnOrigin::Implicit,
                    })
                    .collect();
                Columns { columns, open: false }
            }
            TABLE_QUERY => {
                let Some((schema, table)) =
                    child(query, QUALIFIED_NAME).and_then(|name| object_name(&name, self.dialect))
                else {
                    return Columns {
                        columns: Vec::new(),
                        open: true,
                    };
                };
                let kind = self.table_kind(query, schema.as_ref().map(|part| &part.ident), &table.ident);
                let source = self.named_source(query, kind, table, None);
                self.columns(&source, depth + 1)
            }
            _ => Columns {
                columns: Vec::new(),
                open: true,
            },
        }
    }

    fn select_sources(&self, select: &SyntaxNode) -> (Vec<Source>, Vec<Ident>) {
        let mut sources = Vec::new();
        let mut merged = Vec::new();
        if let Some(from) = child(select, FROM_CLAUSE) {
            self.from_sources(&from, &mut sources, &mut merged);
        }
        (sources, merged)
    }

    fn select_outputs(&self, select: &SyntaxNode, depth: u32) -> Columns {
        let Some(list) = child(select, SELECT_LIST) else {
            return Columns {
                columns: Vec::new(),
                open: true,
            };
        };
        let mut columns = Vec::new();
        let mut open = false;
        let mut sources: Option<Vec<Source>> = None;
        for item in children(&list, SELECT_ITEM) {
            let expression = item.children().next();
            if let Some(wildcard) = expression.as_ref().filter(|inner| inner.kind() == WILDCARD) {
                let sources = sources.get_or_insert_with(|| self.select_sources(select).0);
                let qualifier = parts(wildcard, self.dialect).pop();
                for source in sources.iter() {
                    if let Some(qualifier) = &qualifier {
                        if !self.source_named(source, &qualifier.ident) {
                            continue;
                        }
                    }
                    let found = self.columns(source, depth + 1);
                    open |= found.open;
                    columns.extend(found.columns);
                }
                continue;
            }
            let name = output_name(&item, self.dialect).unwrap_or_else(|| match self.dialect {
                Dialect::Postgres => "?column?".to_string(),
                _ => crate::ast::compact(&item),
            });
            columns.push(OutputColumn {
                name,
                origin: ColumnOrigin::Item(item.clone()),
            });
        }
        Columns { columns, open }
    }

    /// Whether a qualifier names a source.
    pub fn source_named(&self, source: &Source, name: &Ident) -> bool {
        if source.name.text.is_empty() {
            return false;
        }
        let case = match source.kind {
            SourceKind::Table(_) | SourceKind::Unknown if !source.aliased => self.catalog.table_case,
            _ => self.column_case(),
        };
        case.eq(&source.name.text, &name.text)
            || (case == Case::Exact
                && self.dialect != Dialect::Postgres
                && source.name.text.eq_ignore_ascii_case(&name.text))
    }

    /// The levels of the scope of a node, the innermost first.
    pub fn scope(&self, node: &SyntaxNode) -> Vec<Level> {
        let mut levels = Vec::new();
        let mut inner = node.clone();
        let mut hide_next_from = false;
        while let Some(parent) = inner.parent() {
            match parent.kind() {
                SELECT => {
                    if let Some(level) = self.select_level(&parent, &inner, node, hide_next_from) {
                        levels.push(level);
                    }
                    hide_next_from = false;
                }
                COMPOUND_SELECT if inner.kind() == ORDER_BY_CLAUSE => {
                    let first = parent.children().find(|child| is_query(child.kind()));
                    // The columns of a set operation are those of its first query, by name.
                    let sources = first
                        .iter()
                        .map(|first| Source {
                            name: Ident::new(""),
                            schema: None,
                            kind: SourceKind::Derived(first.clone()),
                            node: parent.clone(),
                            name_node: None,
                            aliased: false,
                            renames: Vec::new(),
                        })
                        .collect();
                    levels.push(Level {
                        node: parent.clone(),
                        clause: Clause::OrderBy,
                        sources,
                        merged: Vec::new(),
                        select: first.and_then(|first| first_select(&first)),
                        variables: Vec::new(),
                        bare: bare_in(node, &inner),
                    });
                }
                DERIVED_TABLE if inner.kind() == PAREN_QUERY => {
                    hide_next_from = !has_token(&parent, LATERAL_KW);
                }
                UPDATE_STMT | DELETE_STMT | INSERT_STMT | MERGE_STMT => {
                    if let Some(level) = self.dml_level(&parent, &inner, node) {
                        levels.push(level);
                    }
                }
                CREATE_TABLE_STMT | CREATE_INDEX_STMT | ALTER_TABLE_STMT | CREATE_TRIGGER_STMT => {
                    if let Some(level) = self.ddl_level(&parent, &inner) {
                        levels.push(level);
                    }
                }
                CREATE_FUNCTION_STMT => {
                    let variables = child(&parent, PARAM_LIST)
                        .map(|list| {
                            children(&list, PARAM_DEF)
                                .filter_map(|param| child(&param, NAME))
                                .filter_map(|name| {
                                    Some(Part {
                                        ident: Ident::of_name(&name, self.dialect)?,
                                        node: name,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    levels.push(Level {
                        node: parent.clone(),
                        clause: Clause::Other,
                        sources: Vec::new(),
                        merged: Vec::new(),
                        select: None,
                        variables,
                        bare: false,
                    });
                }
                STATEMENT_LIST => {
                    let variables = children(&parent, DECLARE_STMT)
                        .filter(|declare| declare.text_range().end() <= node.text_range().start())
                        .flat_map(|declare| children(&declare, NAME).collect::<Vec<_>>())
                        .filter_map(|name| {
                            Some(Part {
                                ident: Ident::of_name(&name, self.dialect)?,
                                node: name,
                            })
                        })
                        .collect::<Vec<_>>();
                    if !variables.is_empty() {
                        levels.push(Level {
                            node: parent.clone(),
                            clause: Clause::Other,
                            sources: Vec::new(),
                            merged: Vec::new(),
                            select: None,
                            variables,
                            bare: false,
                        });
                    }
                }
                _ => {}
            }
            inner = parent;
        }
        levels
    }

    fn select_level(
        &self,
        select: &SyntaxNode,
        inner: &SyntaxNode,
        node: &SyntaxNode,
        hide_from: bool,
    ) -> Option<Level> {
        let clause = match inner.kind() {
            SELECT_LIST | DISTINCT_CLAUSE => Clause::Select,
            FROM_CLAUSE => Clause::From,
            WHERE_CLAUSE => Clause::Where,
            GROUP_BY_CLAUSE => Clause::GroupBy,
            HAVING_CLAUSE | QUALIFY_CLAUSE => Clause::Having,
            WINDOW_CLAUSE => Clause::Window,
            ORDER_BY_CLAUSE => Clause::OrderBy,
            WITH_CLAUSE | INTO_CLAUSE => return None,
            _ => Clause::Other,
        };
        let (sources, merged) = if hide_from && clause == Clause::From {
            (Vec::new(), Vec::new())
        } else {
            self.select_sources(select)
        };
        Some(Level {
            node: select.clone(),
            clause,
            sources,
            merged,
            select: Some(select.clone()),
            variables: Vec::new(),
            bare: bare_in(node, inner),
        })
    }

    /// The target of a DML statement as a source.
    fn dml_target(&self, statement: &SyntaxNode) -> Option<Source> {
        let name = child(statement, QUALIFIED_NAME)?;
        let (schema, table) = object_name(&name, self.dialect)?;
        let kind = self.table_kind(statement, schema.as_ref().map(|part| &part.ident), &table.ident);
        let alias = statement
            .children()
            .take_while(|inner| !matches!(inner.kind(), VALUES | SET_CLAUSE) && !is_query(inner.kind()))
            .find(|inner| inner.kind() == ALIAS);
        let mut source = Source {
            name: table.ident.clone(),
            schema: schema.map(|part| part.ident),
            kind,
            node: statement.clone(),
            name_node: Some(table.node.clone()),
            aliased: false,
            renames: Vec::new(),
        };
        if let Some(alias) = alias {
            if let Some(alias_name) = child(&alias, NAME) {
                if let Some(ident) = Ident::of_name(&alias_name, self.dialect) {
                    source.name = ident;
                    source.schema = None;
                    source.name_node = Some(alias_name);
                    source.aliased = true;
                }
            }
        }
        Some(source)
    }

    fn dml_level(&self, statement: &SyntaxNode, inner: &SyntaxNode, node: &SyntaxNode) -> Option<Level> {
        if inner.kind() == WITH_CLAUSE || (statement.kind() == INSERT_STMT && is_query(inner.kind())) {
            return None;
        }
        let mut sources = Vec::new();
        let mut merged = Vec::new();
        let mut clause = Clause::Other;
        match statement.kind() {
            UPDATE_STMT => {
                // The tables before `SET` are the ones it may write: one, or MySQL's joined tables.
                let mut targets = 0;
                for operand in statement.children() {
                    match operand.kind() {
                        TABLE_REF | DERIVED_TABLE | TABLE_FUNCTION | JOIN_EXPR | PAREN_JOIN => {
                            self.from_sources(&operand, &mut sources, &mut merged);
                            targets = sources.len();
                        }
                        FROM_CLAUSE => self.from_sources(&operand, &mut sources, &mut merged),
                        _ => {}
                    }
                }
                clause = match inner.kind() {
                    SET_CLAUSE => {
                        let target = node
                            .ancestors()
                            .find(|ancestor| ancestor.kind() == ASSIGNMENT)
                            .and_then(|assignment| assignment.children().next())
                            .is_some_and(|first| first.text_range().contains_range(node.text_range()));
                        if target { Clause::Target } else { Clause::Where }
                    }
                    WHERE_CLAUSE => Clause::Where,
                    ORDER_BY_CLAUSE => Clause::OrderBy,
                    _ => Clause::Other,
                };
                if clause == Clause::Target {
                    sources.truncate(targets.max(1));
                }
            }
            DELETE_STMT => {
                for operand in statement.children() {
                    if matches!(operand.kind(), FROM_CLAUSE | USING_CLAUSE) {
                        self.from_sources(&operand, &mut sources, &mut merged);
                    }
                }
                clause = match inner.kind() {
                    WHERE_CLAUSE => Clause::Where,
                    ORDER_BY_CLAUSE => Clause::OrderBy,
                    _ => Clause::Other,
                };
            }
            INSERT_STMT => {
                let target = self.dml_target(statement)?;
                match inner.kind() {
                    NAME_LIST => {
                        clause = Clause::Target;
                        sources.push(target);
                    }
                    ON_DUPLICATE_KEY_CLAUSE => {
                        let row_alias = statement
                            .children()
                            .skip_while(|inner| !matches!(inner.kind(), VALUES | SET_CLAUSE) && !is_query(inner.kind()))
                            .find(|inner| inner.kind() == ALIAS);
                        let target_kind = target.kind.clone();
                        sources.push(target);
                        if let Some(alias) = row_alias {
                            if let Some(name) = child(&alias, NAME) {
                                if let Some(ident) = Ident::of_name(&name, self.dialect) {
                                    let renames = child(&alias, NAME_LIST)
                                        .map(|list| parts(&list, self.dialect))
                                        .unwrap_or_default();
                                    sources.push(Source {
                                        name: ident,
                                        schema: None,
                                        kind: target_kind,
                                        node: alias.clone(),
                                        name_node: Some(name),
                                        aliased: true,
                                        renames,
                                    });
                                }
                            }
                        }
                        clause = if node
                            .ancestors()
                            .find(|ancestor| ancestor.kind() == ASSIGNMENT)
                            .and_then(|assignment| assignment.children().next())
                            .is_some_and(|first| first.text_range().contains_range(node.text_range()))
                        {
                            Clause::Target
                        } else {
                            Clause::Where
                        };
                        if clause == Clause::Target {
                            sources.truncate(1);
                        }
                    }
                    UPSERT_CLAUSE => {
                        let target_side = node.ancestors().any(|ancestor| ancestor.kind() == CONFLICT_TARGET)
                            || node
                                .ancestors()
                                .find(|ancestor| ancestor.kind() == ASSIGNMENT)
                                .and_then(|assignment| assignment.children().next())
                                .is_some_and(|first| first.text_range().contains_range(node.text_range()));
                        if target_side {
                            sources.push(target);
                            clause = Clause::Target;
                        } else {
                            // SQLite reads an unqualified column of `DO UPDATE` as the existing
                            // row's, where PostgreSQL finds it ambiguous with `excluded`.
                            if self.dialect == Dialect::Sqlite {
                                merged.extend(
                                    self.columns(&target, 0)
                                        .columns
                                        .into_iter()
                                        .map(|column| Ident::new(column.name)),
                                );
                            }
                            let target_kind = target.kind.clone();
                            sources.push(target);
                            sources.push(Source {
                                name: Ident::new("excluded"),
                                schema: None,
                                kind: target_kind,
                                node: inner.clone(),
                                name_node: None,
                                aliased: true,
                                renames: Vec::new(),
                            });
                            clause = Clause::Where;
                        }
                    }
                    RETURNING_CLAUSE => {
                        sources.push(target);
                        clause = Clause::Select;
                    }
                    // MySQL's `INSERT ... SET a = 1, b = a + 1`, whose values may read the columns set before.
                    SET_CLAUSE => {
                        sources.push(target);
                        let target_side = node
                            .ancestors()
                            .find(|ancestor| ancestor.kind() == ASSIGNMENT)
                            .and_then(|assignment| assignment.children().next())
                            .is_some_and(|first| first.text_range().contains_range(node.text_range()));
                        clause = if target_side { Clause::Target } else { Clause::Where };
                    }
                    _ => return None,
                }
            }
            MERGE_STMT => {
                let target = self.dml_target(statement)?;
                sources.push(target);
                for operand in statement.children() {
                    if matches!(operand.kind(), TABLE_REF | DERIVED_TABLE | TABLE_FUNCTION) {
                        self.from_sources(&operand, &mut sources, &mut merged);
                    }
                }
                let in_insert_list =
                    inner.kind() == MERGE_WHEN_CLAUSE && node.ancestors().any(|ancestor| ancestor.kind() == NAME_LIST);
                let in_set_target = node
                    .ancestors()
                    .find(|ancestor| ancestor.kind() == ASSIGNMENT)
                    .and_then(|assignment| assignment.children().next())
                    .is_some_and(|first| first.text_range().contains_range(node.text_range()));
                clause = if in_insert_list || in_set_target {
                    sources.truncate(1);
                    Clause::Target
                } else {
                    Clause::Where
                };
            }
            _ => {}
        }
        if matches!(inner.kind(), RETURNING_CLAUSE) {
            clause = Clause::Select;
        }
        Some(Level {
            node: statement.clone(),
            clause,
            sources,
            merged,
            select: None,
            variables: Vec::new(),
            bare: false,
        })
    }

    fn ddl_level(&self, statement: &SyntaxNode, inner: &SyntaxNode) -> Option<Level> {
        let source = match statement.kind() {
            CREATE_TABLE_STMT => {
                if inner.kind() != TABLE_ELEMENT_LIST {
                    return None;
                }
                let name = child(statement, QUALIFIED_NAME)?;
                let (_, table) = object_name(&name, self.dialect)?;
                Source {
                    name: table.ident,
                    schema: None,
                    kind: SourceKind::Defined(inner.clone()),
                    node: statement.clone(),
                    name_node: Some(table.node),
                    aliased: false,
                    renames: Vec::new(),
                }
            }
            CREATE_INDEX_STMT | ALTER_TABLE_STMT | CREATE_TRIGGER_STMT => {
                if inner.kind() == QUALIFIED_NAME {
                    return None;
                }
                let names: Vec<SyntaxNode> = children(statement, QUALIFIED_NAME).collect();
                let name = match statement.kind() {
                    CREATE_INDEX_STMT => table_after_on(statement)?,
                    CREATE_TRIGGER_STMT => names.get(1)?.clone(),
                    _ => names.first()?.clone(),
                };
                let (schema, table) = object_name(&name, self.dialect)?;
                let kind = self.table_kind(statement, schema.as_ref().map(|part| &part.ident), &table.ident);
                let mut sources = vec![Source {
                    name: table.ident.clone(),
                    schema: schema.map(|part| part.ident),
                    kind: kind.clone(),
                    node: statement.clone(),
                    name_node: Some(table.node.clone()),
                    aliased: false,
                    renames: Vec::new(),
                }];
                if statement.kind() == CREATE_TRIGGER_STMT {
                    for row in ["new", "old"] {
                        sources.push(Source {
                            name: Ident::new(row),
                            schema: None,
                            kind: kind.clone(),
                            node: statement.clone(),
                            name_node: None,
                            aliased: true,
                            renames: Vec::new(),
                        });
                    }
                    sources.remove(0);
                }
                return Some(Level {
                    node: statement.clone(),
                    clause: Clause::Where,
                    sources,
                    merged: Vec::new(),
                    select: None,
                    variables: Vec::new(),
                    bare: false,
                });
            }
            _ => return None,
        };
        Some(Level {
            node: statement.clone(),
            clause: Clause::Where,
            sources: vec![source],
            merged: Vec::new(),
            select: None,
            variables: Vec::new(),
            bare: false,
        })
    }

    /// Where an alias of the select list is seen in a clause: before the columns, after them, or
    /// not at all.
    fn alias_rule(&self, clause: Clause, bare: bool) -> AliasRule {
        let postgres = self.dialect == Dialect::Postgres;
        match clause {
            Clause::OrderBy if postgres && !bare => AliasRule::Never,
            Clause::OrderBy => AliasRule::First,
            Clause::GroupBy if (postgres || self.dialect == Dialect::Mysql) && !bare => AliasRule::Never,
            Clause::GroupBy => AliasRule::After,
            Clause::Having if postgres => AliasRule::Never,
            Clause::Having => AliasRule::After,
            Clause::Where if matches!(self.dialect, Dialect::Sqlite | Dialect::Generic) => AliasRule::After,
            _ => AliasRule::Never,
        }
    }

    fn select_alias(&self, select: &SyntaxNode, name: &Ident) -> Option<SyntaxNode> {
        let list = child(select, SELECT_LIST)?;
        children(&list, SELECT_ITEM).find(|item| {
            alias_of(item, self.dialect).is_some_and(|(alias, _)| self.column_case().eq(&alias.ident.text, &name.text))
        })
    }

    /// The column of a source with a name, if it has one.
    pub fn column_of(&self, source: &Source, name: &Ident) -> Option<OutputColumn> {
        self.columns(source, 0)
            .columns
            .into_iter()
            .find(|column| self.column_case().eq(&column.name, &name.text))
    }

    /// What a name without a qualifier stands for in a scope.
    pub fn resolve_unqualified(&self, levels: &[Level], name: &Ident) -> Resolution {
        let mut complete = true;
        for level in levels {
            if let Some(variable) = level
                .variables
                .iter()
                .find(|variable| self.column_case().eq(&variable.ident.text, &name.text))
            {
                return Resolution::Found(Referent::Variable(variable.node.clone()));
            }
            let rule = self.alias_rule(level.clause, level.bare);
            let alias = || level.select.as_ref().and_then(|select| self.select_alias(select, name));
            if rule == AliasRule::First {
                if let Some(item) = alias() {
                    return Resolution::Found(Referent::SelectAlias(item));
                }
            }
            let mut found = Vec::new();
            for source in &level.sources {
                let columns = self.columns(source, 0);
                complete &= !columns.open;
                if let Some(column) = columns
                    .columns
                    .into_iter()
                    .find(|column| self.column_case().eq(&column.name, &name.text))
                {
                    found.push(Referent::Column {
                        source: source.clone(),
                        column,
                    });
                }
            }
            match found.len() {
                0 => {}
                1 => return Resolution::Found(found.remove(0)),
                _ => {
                    let merged = level
                        .merged
                        .iter()
                        .any(|merged| self.column_case().eq(&merged.text, &name.text));
                    if merged {
                        return Resolution::Found(found.remove(0));
                    }
                    return Resolution::Ambiguous(found);
                }
            }
            if rule == AliasRule::After {
                if let Some(item) = alias() {
                    return Resolution::Found(Referent::SelectAlias(item));
                }
            }
            if self.dialect == Dialect::Postgres {
                if let Some(source) = level.sources.iter().find(|source| self.source_named(source, name)) {
                    return Resolution::Found(Referent::Source(source.clone()));
                }
            }
        }
        Resolution::Unknown { complete }
    }

    /// The source a qualifier names in a scope, the innermost first.
    pub fn find_source(&self, levels: &[Level], qualifier: &Ident) -> Option<Source> {
        levels
            .iter()
            .flat_map(|level| level.sources.iter())
            .find(|source| self.source_named(source, qualifier))
            .cloned()
    }

    /// What each part of a `COLUMN_REF` stands for: the qualifiers, then the column.
    pub fn resolve_column_ref(&self, reference: &SyntaxNode) -> Vec<Resolution> {
        let names = parts(reference, self.dialect);
        let levels = self.scope(reference);
        self.resolve_parts(&levels, &names)
    }

    pub fn resolve_parts(&self, levels: &[Level], names: &[Part]) -> Vec<Resolution> {
        match names {
            [] => Vec::new(),
            [column] => vec![self.resolve_unqualified(levels, &column.ident)],
            [qualifier, column] => match self.find_source(levels, &qualifier.ident) {
                Some(source) => {
                    let column = self.source_column(&source, &column.ident);
                    vec![Resolution::Found(Referent::Source(source)), column]
                }
                None => vec![
                    Resolution::Unknown {
                        complete: !in_routine_body(levels),
                    },
                    Resolution::Unknown { complete: false },
                ],
            },
            [schema, table, column] => {
                let source = levels.iter().flat_map(|level| level.sources.iter()).find(|source| {
                    !source.aliased
                        && self.source_named(source, &table.ident)
                        && source
                            .schema
                            .as_ref()
                            .is_none_or(|known| self.catalog.table_case.eq(&known.text, &schema.ident.text))
                });
                match source {
                    Some(source) => {
                        let column = self.source_column(source, &column.ident);
                        vec![
                            Resolution::Found(Referent::Schema(schema.ident.text.clone())),
                            Resolution::Found(Referent::Source(source.clone())),
                            column,
                        ]
                    }
                    None => vec![Resolution::Unknown { complete: false }; 3],
                }
            }
            _ => vec![Resolution::Unknown { complete: false }; names.len()],
        }
    }

    fn source_column(&self, source: &Source, name: &Ident) -> Resolution {
        let columns = self.columns(source, 0);
        match columns
            .columns
            .into_iter()
            .find(|column| self.column_case().eq(&column.name, &name.text))
        {
            Some(column) => Resolution::Found(Referent::Column {
                source: source.clone(),
                column,
            }),
            None => Resolution::Unknown {
                complete: !columns.open,
            },
        }
    }

    /// What a `NAME` anywhere in a statement stands for, with the range of the name.
    pub fn resolve_name(&self, name: &SyntaxNode) -> Option<Resolution> {
        let parent = name.parent()?;
        let ident = Ident::of_name(name, self.dialect)?;
        match parent.kind() {
            COLUMN_REF | WILDCARD => {
                let all = parts(&parent, self.dialect);
                let position = all.iter().position(|part| part.node == *name)?;
                if parent.kind() == WILDCARD {
                    let levels = self.scope(&parent);
                    return Some(match self.find_source(&levels, &all[position].ident) {
                        Some(source) => Resolution::Found(Referent::Source(source)),
                        None => Resolution::Unknown { complete: false },
                    });
                }
                self.resolve_column_ref(&parent).into_iter().nth(position)
            }
            QUALIFIED_NAME => self.resolve_qualified(&parent, name, &ident),
            ALIAS => {
                let owner = parent.parent()?;
                if owner.kind() == SELECT_ITEM {
                    return Some(Resolution::Found(Referent::SelectAlias(owner)));
                }
                let levels = self.scope(&owner);
                let source = levels
                    .iter()
                    .flat_map(|level| level.sources.iter())
                    .find(|source| source.name_node.as_ref() == Some(name))
                    .cloned()
                    .or_else(|| {
                        let mut sources = Vec::new();
                        self.from_sources(&owner, &mut sources, &mut Vec::new());
                        sources
                            .into_iter()
                            .find(|source| source.name_node.as_ref() == Some(name))
                    });
                source.map(|source| Resolution::Found(Referent::Source(source)))
            }
            CTE => Some(Resolution::Found(Referent::Cte(parent))),
            NAME_LIST => self.resolve_name_list(&parent, name, &ident),
            _ => None,
        }
    }

    fn resolve_qualified(&self, qualified: &SyntaxNode, name: &SyntaxNode, ident: &Ident) -> Option<Resolution> {
        let owner = qualified.parent()?;
        let all = parts(qualified, self.dialect);
        let position = all.iter().position(|part| part.node == *name)?;
        let last = position + 1 == all.len();
        match owner.kind() {
            TABLE_REF | INSERT_STMT | UPDATE_STMT | DELETE_STMT | MERGE_STMT | CREATE_INDEX_STMT | ALTER_TABLE_STMT
            | TRUNCATE_STMT | REFERENCES_CLAUSE | LIKE_CLAUSE | TABLE_QUERY | CREATE_TRIGGER_STMT | DROP_STMT
            | EXPLAIN_STMT | LOCKING_CLAUSE | RENAME_TABLE_STMT | COMMENT_STMT => {
                let is_target = match owner.kind() {
                    CREATE_INDEX_STMT => table_after_on(&owner).as_ref() == Some(qualified),
                    CREATE_TRIGGER_STMT => children(&owner, QUALIFIED_NAME).nth(1).as_ref() == Some(qualified),
                    INSERT_STMT | ALTER_TABLE_STMT | MERGE_STMT => {
                        children(&owner, QUALIFIED_NAME).next().as_ref() == Some(qualified)
                    }
                    DROP_STMT => tokens_say_table(&owner),
                    COMMENT_STMT => false,
                    _ => true,
                };
                if !is_target {
                    return None;
                }
                if !last {
                    if position + 2 == all.len() {
                        return Some(Resolution::Found(Referent::Schema(ident.text.clone())));
                    }
                    return None;
                }
                let schema = (all.len() >= 2).then(|| all[all.len() - 2].ident.clone());
                if owner.kind() == DELETE_STMT {
                    let levels = self.scope(qualified);
                    if let Some(source) = levels
                        .iter()
                        .flat_map(|level| level.sources.iter())
                        .find(|source| self.source_named(source, ident))
                    {
                        return Some(Resolution::Found(Referent::Source(source.clone())));
                    }
                }
                Some(match self.table_kind(qualified, schema.as_ref(), ident) {
                    SourceKind::Table(id) => Resolution::Found(Referent::Table(id)),
                    SourceKind::Cte(cte) => Resolution::Found(Referent::Cte(cte)),
                    _ => Resolution::Unknown {
                        complete: self.catalog.covers(schema.as_ref()),
                    },
                })
            }
            FUNCTION_CALL => {
                if !last {
                    return (position + 2 == all.len())
                        .then(|| Resolution::Found(Referent::Schema(ident.text.clone())));
                }
                let schema = (all.len() >= 2).then(|| all[all.len() - 2].ident.clone());
                let routines = self.catalog.routines(schema.as_ref(), &ident.text);
                if !routines.is_empty() {
                    return Some(Resolution::Found(Referent::Routines(
                        routines.into_iter().map(|(id, _)| id).collect(),
                    )));
                }
                let builtin_schema = schema.as_ref().is_none_or(|schema| {
                    self.catalog.builtins.system_schema(&schema.text).is_some()
                        || schema.text.eq_ignore_ascii_case("pg_catalog")
                });
                if builtin_schema {
                    if let Some(function) = self.catalog.builtins.function(&ident.text) {
                        return Some(Resolution::Found(Referent::Function(function.name.clone())));
                    }
                }
                let complete = self.catalog.has_snapshot()
                    && self.dialect != Dialect::Generic
                    && schema.is_none()
                    && !self.catalog.builtins.has_type(&ident.text)
                    && self.catalog.user_type(None, &ident.text).is_none();
                Some(Resolution::Unknown { complete })
            }
            SET_ASSIGNMENT if all.len() >= 2 => self
                .resolve_parts(&self.scope(qualified), &all)
                .into_iter()
                .nth(position),
            CALL_STMT => {
                let schema = (all.len() >= 2).then(|| all[all.len() - 2].ident.clone());
                if !last {
                    return None;
                }
                let routines = self.catalog.routines(schema.as_ref(), &ident.text);
                if routines.is_empty() {
                    return Some(Resolution::Unknown { complete: false });
                }
                Some(Resolution::Found(Referent::Routines(
                    routines.into_iter().map(|(id, _)| id).collect(),
                )))
            }
            _ => None,
        }
    }

    fn resolve_name_list(&self, list: &SyntaxNode, name: &SyntaxNode, ident: &Ident) -> Option<Resolution> {
        let owner = list.parent()?;
        match owner.kind() {
            INSERT_STMT | MERGE_WHEN_CLAUSE => {
                let levels = self.scope(name);
                let target = levels.first()?.sources.first()?.clone();
                Some(self.source_column(&target, ident))
            }
            USING_CLAUSE => {
                let join = owner.parent()?;
                let mut sources = Vec::new();
                for operand in join
                    .children()
                    .filter(|inner| !matches!(inner.kind(), ON_CLAUSE | USING_CLAUSE))
                {
                    self.from_sources(&operand, &mut sources, &mut Vec::new());
                }
                let mut complete = true;
                for source in &sources {
                    let columns = self.columns(source, 0);
                    complete &= !columns.open;
                    if let Some(column) = columns
                        .columns
                        .into_iter()
                        .find(|column| self.column_case().eq(&column.name, &ident.text))
                    {
                        return Some(Resolution::Found(Referent::Column {
                            source: source.clone(),
                            column,
                        }));
                    }
                }
                Some(Resolution::Unknown { complete })
            }
            REFERENCES_CLAUSE => {
                let (schema, table) = object_name(&child(&owner, QUALIFIED_NAME)?, self.dialect)?;
                let kind = self.table_kind(&owner, schema.as_ref().map(|part| &part.ident), &table.ident);
                let source = self.named_source(&owner, kind, table, schema.map(|part| part.ident));
                let source = Source {
                    renames: Vec::new(),
                    ..source
                };
                Some(self.source_column(&source, ident))
            }
            TABLE_CONSTRAINT => {
                let statement = owner
                    .ancestors()
                    .find(|ancestor| ancestor.kind() == CREATE_TABLE_STMT)?;
                let elements = child(&statement, TABLE_ELEMENT_LIST)?;
                let source = Source {
                    name: Ident::new(""),
                    schema: None,
                    kind: SourceKind::Defined(elements),
                    node: statement.clone(),
                    name_node: None,
                    aliased: true,
                    renames: Vec::new(),
                };
                Some(self.source_column(&source, ident))
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AliasRule {
    Never,
    First,
    After,
}

fn rename(columns: &mut Columns, names: Vec<Part>) {
    for (position, part) in names.into_iter().enumerate() {
        let column = OutputColumn {
            name: part.ident.text,
            origin: ColumnOrigin::Declared(part.node),
        };
        if position < columns.columns.len() {
            columns.columns[position] = column;
        } else {
            columns.columns.push(column);
        }
    }
}

/// The first `SELECT` of a query, whose aliases name the columns of a set operation.
fn first_select(query: &SyntaxNode) -> Option<SyntaxNode> {
    let mut current = query.clone();
    loop {
        if current.kind() == SELECT {
            return Some(current);
        }
        current = current.children().find(|inner| is_query(inner.kind()))?;
    }
}

/// Whether a name is a whole item of its clause, as `ORDER BY a` is and `ORDER BY a + 1` is not.
fn bare_in(node: &SyntaxNode, clause: &SyntaxNode) -> bool {
    let mut current = node.clone();
    while let Some(parent) = current.parent() {
        if parent == *clause {
            return true;
        }
        match parent.kind() {
            COLUMN_REF | ORDER_ITEM | PAREN_EXPR => {}
            _ => return false,
        }
        current = parent;
    }
    false
}

/// The table a `CREATE INDEX` is on: the name after `ON`.
pub fn table_after_on(statement: &SyntaxNode) -> Option<SyntaxNode> {
    let mut after_on = false;
    for element in statement.children_with_tokens() {
        match element {
            sql_syntax::SyntaxElement::Token(token) if token.kind() == ON_KW => after_on = true,
            sql_syntax::SyntaxElement::Node(node) if after_on && node.kind() == QUALIFIED_NAME => return Some(node),
            _ => {}
        }
    }
    None
}

fn tokens_say_table(statement: &SyntaxNode) -> bool {
    crate::ast::tokens(statement)
        .map(|token| token.kind())
        .find(|kind| {
            matches!(
                kind,
                TABLE_KW | VIEW_KW | INDEX_KW | FUNCTION_KW | PROCEDURE_KW | TYPE_KW | SEQUENCE_KW | TRIGGER_KW
            )
        })
        .is_some_and(|kind| matches!(kind, TABLE_KW | VIEW_KW))
}

/// Whether a scope is in the body of a routine or trigger, where names can be what the body
/// declares in ways the server does not follow.
fn in_routine_body(levels: &[Level]) -> bool {
    levels.iter().any(|level| {
        level.node.ancestors().any(|ancestor| {
            matches!(
                ancestor.kind(),
                ROUTINE_BODY | CREATE_FUNCTION_STMT | CREATE_TRIGGER_STMT
            )
        })
    })
}
