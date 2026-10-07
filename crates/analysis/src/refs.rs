//! What a name stands for, in a form that compares across statements and files, and every place a
//! script names it. A name the resolver knows (a column, a table, an alias) is asked of it; a name
//! that defines something (`CREATE TABLE`, a column definition, `RENAME ... TO`) is read here,
//! since nothing resolves a definition.
//!
//! A local symbol (an alias, a common table expression, a window, a variable) is the name that
//! declares it, by its range in the document; it never leaves the statement it is declared in. An
//! object of the schema (a table, a column, a routine, a type, a sequence, a schema) is its kind,
//! schema and name, which is what two files agree on.

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, Target, TextRange, TextSize};

use crate::ast::{alias_of, child, children, object_name, parts, tokens};
use crate::catalog::{Catalog, ObjectId, Place, ScriptState, TableId};
use crate::context::{DocumentSchema, Schemas};
use crate::ident::{Case, Ident, unquote};
use crate::resolve::{ColumnOrigin, Referent, Resolution, Resolver, Source, SourceKind, table_after_on};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalKind {
    /// An alias of a table, a subquery or a function in `FROM`, or of the target of a statement.
    Alias,
    CommonTableExpression,
    /// An alias of a select item, or a column name an alias or a common table expression gives.
    ColumnAlias,
    Window,
    Variable,
    Parameter,
    /// A user variable of MySQL or MariaDB, `@name`, which lives as long as the session.
    UserVariable,
}

impl LocalKind {
    pub fn label(self) -> &'static str {
        match self {
            LocalKind::Alias => "alias",
            LocalKind::CommonTableExpression => "common table expression",
            LocalKind::ColumnAlias => "column alias",
            LocalKind::Window => "window",
            LocalKind::Variable => "variable",
            LocalKind::Parameter => "parameter",
            LocalKind::UserVariable => "user variable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    Routine,
    Type,
    Sequence,
}

impl ObjectKind {
    pub fn label(self) -> &'static str {
        match self {
            ObjectKind::Table => "table",
            ObjectKind::View => "view",
            ObjectKind::Routine => "routine",
            ObjectKind::Type => "type",
            ObjectKind::Sequence => "sequence",
        }
    }

    /// Tables and views share one namespace.
    fn family(self) -> ObjectKind {
        match self {
            ObjectKind::View => ObjectKind::Table,
            other => other,
        }
    }
}

/// What a name stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Symbol {
    /// Declared by a name of the document, at `declaration`.
    Local {
        kind: LocalKind,
        name: String,
        declaration: TextRange,
    },
    /// An object of the schema. `schema` is `None` where nothing says which schema, which matches
    /// any.
    Object {
        kind: ObjectKind,
        schema: Option<String>,
        name: String,
    },
    Column {
        schema: Option<String>,
        table: String,
        name: String,
    },
    Schema(String),
    /// A built-in function, by its name in lower case.
    Builtin(String),
}

impl Symbol {
    pub fn name(&self) -> &str {
        match self {
            Symbol::Local { name, .. } | Symbol::Object { name, .. } | Symbol::Column { name, .. } => name,
            Symbol::Schema(name) | Symbol::Builtin(name) => name,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Symbol::Local { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Symbol::Local { kind, .. } => kind.label(),
            Symbol::Object { kind, .. } => kind.label(),
            Symbol::Column { .. } => "column",
            Symbol::Schema(_) => "schema",
            Symbol::Builtin(_) => "built-in function",
        }
    }

    /// The same symbol with another name, as it is after a rename.
    pub fn renamed(&self, new_name: &str) -> Symbol {
        let mut renamed = self.clone();
        match &mut renamed {
            Symbol::Local { name, .. } | Symbol::Object { name, .. } | Symbol::Column { name, .. } => {
                *name = new_name.to_string()
            }
            Symbol::Schema(name) | Symbol::Builtin(name) => *name = new_name.to_string(),
        }
        renamed
    }

    /// Whether two symbols are the same, compared the way the dialect compares names.
    pub fn same(&self, other: &Symbol, cases: Cases) -> bool {
        let schemas = |a: &Option<String>, b: &Option<String>| match (a, b) {
            (Some(a), Some(b)) => cases.table.eq(a, b),
            _ => true,
        };
        match (self, other) {
            (
                Symbol::Local {
                    kind: a,
                    declaration: x,
                    ..
                },
                Symbol::Local {
                    kind: b,
                    declaration: y,
                    ..
                },
            ) => a == b && x == y,
            (
                Symbol::Object {
                    kind: a,
                    schema: s,
                    name: x,
                },
                Symbol::Object {
                    kind: b,
                    schema: t,
                    name: y,
                },
            ) => {
                let case = if a.family() == ObjectKind::Table {
                    cases.table
                } else {
                    cases.name
                };
                a.family() == b.family() && case.eq(x, y) && schemas(s, t)
            }
            (
                Symbol::Column {
                    schema: s,
                    table: a,
                    name: x,
                },
                Symbol::Column {
                    schema: t,
                    table: b,
                    name: y,
                },
            ) => cases.table.eq(a, b) && cases.name.eq(x, y) && schemas(s, t),
            (Symbol::Schema(a), Symbol::Schema(b)) => cases.table.eq(a, b),
            (Symbol::Builtin(a), Symbol::Builtin(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

/// How the names of a document compare: tables and schemas, and everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cases {
    pub table: Case,
    pub name: Case,
}

impl Cases {
    pub fn of(catalog: &Catalog) -> Cases {
        Cases {
            table: catalog.table_case,
            name: catalog.name_case,
        }
    }
}

/// How a place uses a name, which document highlights tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    Declaration,
    Read,
    Write,
}

/// One place a script names a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The name as written, quotes included; inside a string, the name within it.
    pub range: TextRange,
    pub access: Access,
    /// The name stands inside a string, as the sequence of `nextval('orders_id')`.
    pub in_string: bool,
}

/// Reads what names stand for in one statement, against the schema before it.
pub struct Namer<'c, 'a> {
    pub resolver: Resolver<'c, 'a>,
    pub catalog: &'c Catalog<'a>,
    pub state: &'c ScriptState,
    pub dialect: Dialect,
}

/// The `NAME` at an offset: the one the offset is in, or the one it is right after.
pub fn name_at(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    let at = TextSize::from(offset.min(u32::from(root.text_range().end())));
    let tokens: Vec<_> = root.token_at_offset(at).collect();
    tokens
        .iter()
        .rev()
        .chain(tokens.iter())
        .find_map(|token| token.parent().filter(|parent| parent.kind() == NAME))
}

/// The user variable token at an offset, `@name` of MySQL and MariaDB.
pub fn variable_at(root: &SyntaxNode, offset: u32, dialect: Dialect) -> Option<SyntaxToken> {
    if !matches!(dialect, Dialect::Mysql | Dialect::Mariadb) {
        return None;
    }
    let at = TextSize::from(offset.min(u32::from(root.text_range().end())));
    root.token_at_offset(at)
        .find(|token| token.kind() == VARIABLE && token.text().len() > 1)
}

impl<'c, 'a> Namer<'c, 'a> {
    pub fn new(catalog: &'c Catalog<'a>, state: &'c ScriptState) -> Namer<'c, 'a> {
        Namer {
            resolver: Resolver::new(catalog),
            catalog,
            state,
            dialect: catalog.dialect(),
        }
    }

    fn ident(&self, name: &SyntaxNode) -> Option<Ident> {
        Ident::of_name(name, self.dialect)
    }

    fn local(&self, kind: LocalKind, name: &SyntaxNode) -> Option<Symbol> {
        Some(Symbol::Local {
            kind,
            name: self.ident(name)?.text,
            declaration: name.text_range(),
        })
    }

    /// A schema name as a symbol compares it: the default schema is no schema at all.
    fn schema_key(&self, schema: &str) -> Option<String> {
        if schema.is_empty() {
            return None;
        }
        if self
            .catalog
            .default_schema
            .as_deref()
            .is_some_and(|default| self.catalog.table_case.eq(default, schema))
        {
            return None;
        }
        Some(schema.to_string())
    }

    /// The schema an object a definition names goes in.
    fn defined_schema(&self, explicit: Option<&Ident>) -> Option<String> {
        match explicit {
            Some(schema) => self.schema_key(&schema.text),
            None => {
                let implied = self
                    .state
                    .database
                    .clone()
                    .or_else(|| self.state.search_path.first().cloned())?;
                self.schema_key(&implied)
            }
        }
    }

    pub fn table_symbol(&self, id: TableId) -> Symbol {
        let table = self.catalog.table(id);
        let schema = self.catalog.schema_name(id.place, id.schema).to_string();
        Symbol::Object {
            kind: if table.kind.is_view() {
                ObjectKind::View
            } else {
                ObjectKind::Table
            },
            schema: self.schema_key(&schema),
            name: table.name.clone(),
        }
    }

    fn object_schema(&self, id: ObjectId) -> Option<String> {
        let name = self.catalog.schema_name(Place::Layer(id.layer), id.schema).to_string();
        self.schema_key(&name)
    }

    /// The table a `QUALIFIED_NAME` of a definition names, as a symbol would compare it.
    fn defined_table(&self, name: &SyntaxNode, kind: ObjectKind) -> Option<Symbol> {
        let (schema, table) = object_name(name, self.dialect)?;
        Some(Symbol::Object {
            kind,
            schema: self.defined_schema(schema.as_ref().map(|part| &part.ident)),
            name: table.ident.text,
        })
    }

    /// A column of the table a definition names.
    fn column_of_defined(&self, table: &SyntaxNode, column: &SyntaxNode) -> Option<Symbol> {
        let Symbol::Object { schema, name, .. } = self.defined_table(table, ObjectKind::Table)? else {
            return None;
        };
        Some(Symbol::Column {
            schema,
            table: name,
            name: self.ident(column)?.text,
        })
    }

    /// A column of the table an `ALTER TABLE` or `COMMENT ON` names, as the catalog has it when it
    /// does, else as written.
    fn column_of_existing(&self, table: &SyntaxNode, column: &SyntaxNode) -> Option<Symbol> {
        let (schema, part) = object_name(table, self.dialect)?;
        let name = self.ident(column)?.text;
        if let Some(id) = self
            .catalog
            .find_table(schema.as_ref().map(|part| &part.ident), &part.ident)
        {
            if let Symbol::Object {
                schema, name: table, ..
            } = self.table_symbol(id)
            {
                return Some(Symbol::Column { schema, table, name });
            }
        }
        Some(Symbol::Column {
            schema: self.defined_schema(schema.as_ref().map(|part| &part.ident)),
            table: part.ident.text,
            name,
        })
    }

    fn existing_table(&self, name: &SyntaxNode) -> Option<Symbol> {
        let (schema, part) = object_name(name, self.dialect)?;
        match self
            .catalog
            .find_table(schema.as_ref().map(|part| &part.ident), &part.ident)
        {
            Some(id) => Some(self.table_symbol(id)),
            None => self.defined_table(name, ObjectKind::Table),
        }
    }

    /// What a `NAME` stands for, and how it is used there.
    pub fn symbol_at(&self, name: &SyntaxNode) -> Option<(Symbol, Access)> {
        let parent = name.parent()?;
        let declared = |symbol: Option<Symbol>| symbol.map(|symbol| (symbol, Access::Declaration));
        match parent.kind() {
            CTE => return declared(self.local(LocalKind::CommonTableExpression, name)),
            WINDOW_DEF => return declared(self.local(LocalKind::Window, name)),
            OVER_CLAUSE | WINDOW_SPEC => return self.window(name).map(|symbol| (symbol, Access::Read)),
            ALIAS => {
                let owner = parent.parent()?;
                let kind = if owner.kind() == SELECT_ITEM {
                    LocalKind::ColumnAlias
                } else {
                    LocalKind::Alias
                };
                return declared(self.local(kind, name));
            }
            NAME_LIST => {
                let owner = parent.parent()?;
                match owner.kind() {
                    ALIAS | CTE => return declared(self.local(LocalKind::ColumnAlias, name)),
                    CREATE_VIEW_STMT => {
                        let view = child(&owner, QUALIFIED_NAME)?;
                        let Symbol::Object {
                            schema, name: table, ..
                        } = self.defined_table(&view, ObjectKind::View)?
                        else {
                            return None;
                        };
                        return declared(Some(Symbol::Column {
                            schema,
                            table,
                            name: self.ident(name)?.text,
                        }));
                    }
                    _ => {}
                }
            }
            COLUMN_DEF => return self.column_definition(name, &parent),
            JSON_TABLE_COLUMN => return declared(self.local(LocalKind::ColumnAlias, name)),
            PARAM_DEF => return declared(self.local(LocalKind::Parameter, name)),
            DECLARE_STMT => return declared(self.local(LocalKind::Variable, name)),
            RENAME_COLUMN_ACTION => {
                let table = parent
                    .parent()
                    .and_then(|statement| child(&statement, QUALIFIED_NAME))?;
                let first = child(&parent, NAME).as_ref() == Some(name);
                return if first {
                    self.column_of_existing(&table, name)
                        .map(|symbol| (symbol, Access::Write))
                } else {
                    declared(self.column_of_existing(&table, name))
                };
            }
            DROP_COLUMN_ACTION | ALTER_COLUMN_ACTION | MODIFY_COLUMN_ACTION => {
                let table = parent
                    .parent()
                    .and_then(|statement| child(&statement, QUALIFIED_NAME))?;
                return self
                    .column_of_existing(&table, name)
                    .map(|symbol| (symbol, Access::Write));
            }
            CREATE_SCHEMA_STMT => return declared(Some(Symbol::Schema(self.ident(name)?.text))),
            QUALIFIED_NAME => {
                if let Some(found) = self.qualified(name, &parent) {
                    return found;
                }
            }
            _ => {}
        }
        let resolution = self.resolver.resolve_name(name)?;
        let referent = match resolution {
            Resolution::Found(referent) => referent,
            Resolution::Unknown { .. } => {
                let symbol = self.unknown_table(name, &parent)?;
                let access = self.access(name, &symbol);
                return Some((symbol, access));
            }
            Resolution::Ambiguous(_) => return None,
        };
        let symbol = self.referent_symbol(&referent, 0)?;
        let access = self.access(name, &symbol);
        Some((symbol, access))
    }

    /// A table name nothing defines, as written: a script without a schema still names the same
    /// table in each place.
    fn unknown_table(&self, name: &SyntaxNode, qualified: &SyntaxNode) -> Option<Symbol> {
        if qualified.kind() != QUALIFIED_NAME || children(qualified, NAME).last().as_ref() != Some(name) {
            return None;
        }
        let owner = qualified.parent()?;
        let table_position = match owner.kind() {
            TABLE_REF | TRUNCATE_STMT | REFERENCES_CLAUSE | LIKE_CLAUSE | TABLE_QUERY | LOCKING_CLAUSE
            | UPDATE_STMT | DELETE_STMT => true,
            INSERT_STMT | ALTER_TABLE_STMT | MERGE_STMT => {
                children(&owner, QUALIFIED_NAME).next().as_ref() == Some(qualified)
            }
            CREATE_INDEX_STMT => table_after_on(&owner).as_ref() == Some(qualified),
            CREATE_TRIGGER_STMT => children(&owner, QUALIFIED_NAME).nth(1).as_ref() == Some(qualified),
            _ => false,
        };
        if !table_position {
            return None;
        }
        self.defined_table(qualified, ObjectKind::Table)
    }

    fn column_definition(&self, name: &SyntaxNode, definition: &SyntaxNode) -> Option<(Symbol, Access)> {
        let owner = definition.parent()?;
        let declared = |symbol: Option<Symbol>| symbol.map(|symbol| (symbol, Access::Declaration));
        let (holder, statement) = if owner.kind() == TABLE_ELEMENT_LIST {
            let holder = owner.parent()?;
            (holder.clone(), holder)
        } else {
            (owner.clone(), owner.parent()?)
        };
        match holder.kind() {
            CREATE_TABLE_STMT => declared(self.column_of_defined(&child(&holder, QUALIFIED_NAME)?, name)),
            ADD_COLUMN_ACTION => declared(self.column_of_existing(&child(&statement, QUALIFIED_NAME)?, name)),
            MODIFY_COLUMN_ACTION => {
                let table = child(&statement, QUALIFIED_NAME)?;
                if child(&holder, NAME).is_some() {
                    declared(self.column_of_existing(&table, name))
                } else {
                    self.column_of_existing(&table, name)
                        .map(|symbol| (symbol, Access::Write))
                }
            }
            ALIAS => declared(self.local(LocalKind::ColumnAlias, name)),
            _ => None,
        }
    }

    /// A `NAME` in a `QUALIFIED_NAME` whose owner the resolver does not cover: definitions, types,
    /// routines and sequences by name, and variables. `None` leaves it to the resolver.
    fn qualified(&self, name: &SyntaxNode, qualified: &SyntaxNode) -> Option<Option<(Symbol, Access)>> {
        let owner = qualified.parent()?;
        let all = parts(qualified, self.dialect);
        let position = all.iter().position(|part| part.node == *name)?;
        let last = position + 1 == all.len();
        let declared = |symbol: Option<Symbol>| Some(symbol.map(|symbol| (symbol, Access::Declaration)));
        let schema_part = |position: usize| {
            (position + 2 == all.len()).then(|| (Symbol::Schema(all[position].ident.text.clone()), Access::Read))
        };
        let schema_of = || (all.len() >= 2).then(|| all[all.len() - 2].ident.clone());
        match owner.kind() {
            CREATE_TABLE_STMT | CREATE_VIEW_STMT | CREATE_FUNCTION_STMT | CREATE_TYPE_STMT | CREATE_DOMAIN_STMT
            | CREATE_SEQUENCE_STMT | RENAME_TABLE_ACTION => {
                if !last {
                    return Some(schema_part(position));
                }
                let kind = match owner.kind() {
                    CREATE_TABLE_STMT | RENAME_TABLE_ACTION => ObjectKind::Table,
                    CREATE_VIEW_STMT => ObjectKind::View,
                    CREATE_FUNCTION_STMT => ObjectKind::Routine,
                    CREATE_SEQUENCE_STMT => ObjectKind::Sequence,
                    _ => ObjectKind::Type,
                };
                if owner.kind() == RENAME_TABLE_ACTION {
                    let altered = owner.parent().and_then(|statement| child(&statement, QUALIFIED_NAME))?;
                    let kind = match self.existing_table(&altered) {
                        Some(Symbol::Object { kind, .. }) => kind,
                        _ => kind,
                    };
                    return declared(self.defined_table(qualified, kind));
                }
                declared(self.defined_table(qualified, kind))
            }
            RENAME_TABLE_STMT => {
                let index = children(&owner, QUALIFIED_NAME).position(|other| other == *qualified)?;
                if index % 2 == 1 {
                    if !last {
                        return Some(schema_part(position));
                    }
                    let from = children(&owner, QUALIFIED_NAME).nth(index - 1)?;
                    let kind = match self.existing_table(&from) {
                        Some(Symbol::Object { kind, .. }) => kind,
                        _ => ObjectKind::Table,
                    };
                    return declared(self.defined_table(qualified, kind));
                }
                None
            }
            CREATE_INDEX_STMT if table_after_on(&owner).as_ref() != Some(qualified) => Some(None),
            CREATE_TRIGGER_STMT if children(&owner, QUALIFIED_NAME).next().as_ref() == Some(qualified) => Some(None),
            TYPE => {
                if !last {
                    return Some(schema_part(position));
                }
                let schema = schema_of();
                let (id, user_type) = self.catalog.user_type(schema.as_ref(), &all[position].ident.text)?;
                Some(Some((
                    Symbol::Object {
                        kind: ObjectKind::Type,
                        schema: self.object_schema(id),
                        name: user_type.name.clone(),
                    },
                    Access::Read,
                )))
            }
            DROP_STMT => {
                let object = tokens(&owner).map(|token| token.kind()).find(|kind| {
                    matches!(
                        kind,
                        TABLE_KW
                            | VIEW_KW
                            | FUNCTION_KW
                            | PROCEDURE_KW
                            | TYPE_KW
                            | DOMAIN_KW
                            | SEQUENCE_KW
                            | SCHEMA_KW
                            | DATABASE_KW
                            | INDEX_KW
                            | TRIGGER_KW
                    )
                });
                match object {
                    Some(TABLE_KW | VIEW_KW) => None,
                    Some(SCHEMA_KW | DATABASE_KW) => {
                        Some(Some((Symbol::Schema(all[position].ident.text.clone()), Access::Write)))
                    }
                    Some(FUNCTION_KW | PROCEDURE_KW | TYPE_KW | DOMAIN_KW | SEQUENCE_KW) => {
                        if !last {
                            return Some(schema_part(position));
                        }
                        let kind = match object {
                            Some(FUNCTION_KW | PROCEDURE_KW) => ObjectKind::Routine,
                            Some(SEQUENCE_KW) => ObjectKind::Sequence,
                            _ => ObjectKind::Type,
                        };
                        Some(
                            self.object_by_name(kind, schema_of().as_ref(), &all[position].ident.text)
                                .map(|symbol| (symbol, Access::Write)),
                        )
                    }
                    _ => Some(None),
                }
            }
            COMMENT_STMT => {
                let words: Vec<SyntaxKind> = tokens(&owner).map(|token| token.kind()).collect();
                if words.contains(&COLUMN_KW) {
                    if all.len() < 2 {
                        return Some(None);
                    }
                    let table_end = all.len() - 1;
                    if last {
                        let table_part = &all[table_end - 1];
                        let schema = (table_end >= 2).then(|| all[table_end - 2].ident.clone());
                        let symbol = match self.catalog.find_table(schema.as_ref(), &table_part.ident) {
                            Some(id) => match self.table_symbol(id) {
                                Symbol::Object { schema, name, .. } => Symbol::Column {
                                    schema,
                                    table: name,
                                    name: all[position].ident.text.clone(),
                                },
                                _ => return Some(None),
                            },
                            None => Symbol::Column {
                                schema: self.defined_schema(schema.as_ref()),
                                table: table_part.ident.text.clone(),
                                name: all[position].ident.text.clone(),
                            },
                        };
                        return Some(Some((symbol, Access::Write)));
                    }
                    if position + 2 == all.len() {
                        let schema = (position >= 1).then(|| all[position - 1].ident.clone());
                        return Some(
                            self.catalog
                                .find_table(schema.as_ref(), &all[position].ident)
                                .map(|id| (self.table_symbol(id), Access::Read)),
                        );
                    }
                    return Some(
                        (position + 3 == all.len())
                            .then(|| (Symbol::Schema(all[position].ident.text.clone()), Access::Read)),
                    );
                }
                if words.contains(&TABLE_KW) || words.contains(&VIEW_KW) {
                    if !last {
                        return Some(schema_part(position));
                    }
                    return Some(self.existing_table(qualified).map(|symbol| (symbol, Access::Write)));
                }
                Some(None)
            }
            SET_ASSIGNMENT | INTO_CLAUSE if all.len() == 1 => {
                let levels = self.resolver.scope(qualified);
                let found = levels.iter().flat_map(|level| level.variables.iter()).find(|variable| {
                    crate::ident::name_case(self.dialect).eq(&variable.ident.text, &all[position].ident.text)
                });
                Some(found.and_then(|variable| {
                    let kind = if variable.node.parent().is_some_and(|parent| parent.kind() == PARAM_DEF) {
                        LocalKind::Parameter
                    } else {
                        LocalKind::Variable
                    };
                    self.local(kind, &variable.node).map(|symbol| (symbol, Access::Write))
                }))
            }
            USE_STMT => Some(Some((Symbol::Schema(all[position].ident.text.clone()), Access::Read))),
            FUNCTION_CALL | CALL_STMT if !last => Some(schema_part(position)),
            _ => None,
        }
    }

    fn object_by_name(&self, kind: ObjectKind, schema: Option<&Ident>, name: &str) -> Option<Symbol> {
        let found = match kind {
            ObjectKind::Routine => self
                .catalog
                .routines(schema, name)
                .first()
                .map(|(id, routine)| (*id, routine.name.clone())),
            ObjectKind::Type => self
                .catalog
                .user_type(schema, name)
                .map(|(id, user_type)| (id, user_type.name.clone())),
            ObjectKind::Sequence => self
                .catalog
                .sequence(schema, name)
                .map(|(id, sequence)| (id, sequence.name.clone())),
            _ => None,
        };
        Some(match found {
            Some((id, name)) => Symbol::Object {
                kind,
                schema: self.object_schema(id),
                name,
            },
            None => Symbol::Object {
                kind,
                schema: self.defined_schema(schema),
                name: name.to_string(),
            },
        })
    }

    /// The window a name in `OVER w` or `(w ORDER BY ...)` stands for.
    fn window(&self, name: &SyntaxNode) -> Option<Symbol> {
        let ident = self.ident(name)?;
        let select = name.ancestors().find(|ancestor| ancestor.kind() == SELECT)?;
        let clause = child(&select, WINDOW_CLAUSE)?;
        let definition = children(&clause, WINDOW_DEF)
            .filter_map(|definition| child(&definition, NAME))
            .find(|declared| {
                self.ident(declared)
                    .is_some_and(|other| crate::ident::name_case(self.dialect).eq(&other.text, &ident.text))
            })?;
        self.local(LocalKind::Window, &definition)
    }

    /// What a referent of the resolver is as a symbol. A column a query passes on unchanged is the
    /// column it reads, so renaming a table's column follows it through common table expressions
    /// and subqueries.
    pub fn referent_symbol(&self, referent: &Referent, depth: u32) -> Option<Symbol> {
        if depth > 16 {
            return None;
        }
        match referent {
            Referent::Table(id) => Some(self.table_symbol(*id)),
            Referent::Cte(cte) => self.local(LocalKind::CommonTableExpression, &child(cte, NAME)?),
            Referent::Source(source) => self.source_symbol(source),
            Referent::Column { source, column } => match &column.origin {
                ColumnOrigin::Table(id, position) => {
                    let Symbol::Object { schema, name, .. } = self.table_symbol(*id) else {
                        return None;
                    };
                    Some(Symbol::Column {
                        schema,
                        table: name,
                        name: self.catalog.table(*id).columns[*position].name.clone(),
                    })
                }
                ColumnOrigin::Item(item) => {
                    if let Some((alias, _)) = alias_of(item, self.dialect) {
                        return self.local(LocalKind::ColumnAlias, &alias.node);
                    }
                    let expression = item.children().next()?;
                    if expression.kind() != COLUMN_REF {
                        return None;
                    }
                    let last = children(&expression, NAME).last()?;
                    match self.resolver.resolve_name(&last)? {
                        Resolution::Found(inner) => self.referent_symbol(&inner, depth + 1),
                        _ => None,
                    }
                }
                ColumnOrigin::Declared(name) => {
                    let parent = name.parent()?;
                    if parent.kind() == COLUMN_DEF {
                        if let SourceKind::Defined(_) = source.kind {
                            return self.column_definition(name, &parent).map(|(symbol, _)| symbol);
                        }
                    }
                    self.symbol_at(name).map(|(symbol, _)| symbol)
                }
                ColumnOrigin::Implicit => None,
            },
            Referent::SelectAlias(item) => {
                let (alias, _) = alias_of(item, self.dialect)?;
                self.local(LocalKind::ColumnAlias, &alias.node)
            }
            Referent::Variable(name) => {
                let kind = if name.parent().is_some_and(|parent| parent.kind() == PARAM_DEF) {
                    LocalKind::Parameter
                } else {
                    LocalKind::Variable
                };
                self.local(kind, name)
            }
            Referent::Schema(name) => Some(Symbol::Schema(name.clone())),
            Referent::Function(name) => Some(Symbol::Builtin(name.to_lowercase())),
            Referent::Routines(ids) => {
                let id = *ids.first()?;
                Some(Symbol::Object {
                    kind: ObjectKind::Routine,
                    schema: self.object_schema(id),
                    name: self.catalog.routine_at(id).name.clone(),
                })
            }
        }
    }

    fn source_symbol(&self, source: &Source) -> Option<Symbol> {
        if source.aliased {
            return self.local(LocalKind::Alias, source.name_node.as_ref()?);
        }
        match &source.kind {
            SourceKind::Table(id) => Some(self.table_symbol(*id)),
            SourceKind::Cte(cte) => self.local(LocalKind::CommonTableExpression, &child(cte, NAME)?),
            SourceKind::Defined(_) => {
                let statement = source.node.clone();
                self.defined_table(&child(&statement, QUALIFIED_NAME)?, ObjectKind::Table)
            }
            SourceKind::Unknown => Some(Symbol::Object {
                kind: ObjectKind::Table,
                schema: source.schema.as_ref().and_then(|schema| self.schema_key(&schema.text)),
                name: source.name.text.clone(),
            }),
            SourceKind::Derived(_) | SourceKind::Function(_) => None,
        }
    }

    /// Whether a name that is not a definition reads or writes what it stands for.
    fn access(&self, name: &SyntaxNode, symbol: &Symbol) -> Access {
        match symbol {
            Symbol::Column { .. } => {
                let in_target_list = name
                    .parent()
                    .filter(|parent| parent.kind() == NAME_LIST)
                    .and_then(|list| list.parent())
                    .is_some_and(|owner| matches!(owner.kind(), INSERT_STMT | MERGE_WHEN_CLAUSE));
                let assigned = name
                    .ancestors()
                    .find(|ancestor| ancestor.kind() == ASSIGNMENT)
                    .is_some_and(|assignment| {
                        assignment
                            .children()
                            .next()
                            .is_some_and(|first| first.text_range().contains_range(name.text_range()))
                    });
                if in_target_list || assigned {
                    Access::Write
                } else {
                    Access::Read
                }
            }
            Symbol::Object {
                kind: ObjectKind::Table | ObjectKind::View,
                ..
            } => {
                let Some(qualified) = name.parent().filter(|parent| parent.kind() == QUALIFIED_NAME) else {
                    return Access::Read;
                };
                let Some(owner) = qualified.parent() else {
                    return Access::Read;
                };
                let written = match owner.kind() {
                    INSERT_STMT | MERGE_STMT => children(&owner, QUALIFIED_NAME).next().as_ref() == Some(&qualified),
                    TRUNCATE_STMT | DELETE_STMT => true,
                    TABLE_REF => owner.parent().is_some_and(|holder| {
                        let first = holder.children().find(|inner| inner.kind() == TABLE_REF).as_ref() == Some(&owner);
                        let deleted = holder.kind() == FROM_CLAUSE
                            && holder.parent().is_some_and(|statement| statement.kind() == DELETE_STMT);
                        first && (holder.kind() == UPDATE_STMT || deleted)
                    }),
                    ALTER_TABLE_STMT | DROP_STMT | RENAME_TABLE_STMT => true,
                    _ => false,
                };
                if written { Access::Write } else { Access::Read }
            }
            _ => Access::Read,
        }
    }
}

/// The statement of the script a node is in.
pub fn statement_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors()
        .find(|ancestor| ancestor.parent().is_some_and(|parent| parent.kind() == SOURCE_FILE))
}

/// The text of a `NAME` without its quotes, as a person reads it.
pub fn bare_text(name: &SyntaxNode) -> String {
    let token = name
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| !token.kind().is_trivia());
    match token {
        Some(token) => unquote(token.kind(), token.text()).0,
        None => String::new(),
    }
}

/// Whether a name, as written, may name a symbol called `name`; a cheap test before resolving it.
fn may_name(name: &SyntaxNode, wanted: &[String]) -> bool {
    let text = bare_text(name);
    wanted.iter().any(|word| word.eq_ignore_ascii_case(&text))
}

/// The symbols a script names, each place once, in order, with which of `symbols` each is.
pub fn find_hits(root: &SyntaxNode, target: Target, schemas: Schemas, symbols: &[Symbol]) -> Vec<(usize, Hit)> {
    let wanted: Vec<String> = symbols.iter().map(|symbol| symbol.name().to_string()).collect();
    let locals: Vec<TextRange> = symbols
        .iter()
        .filter_map(|symbol| match symbol {
            Symbol::Local {
                kind: LocalKind::UserVariable,
                ..
            } => Some(root.text_range()),
            Symbol::Local { declaration, .. } => statement_range(root, *declaration),
            _ => None,
        })
        .collect();
    let only_local = symbols.iter().all(Symbol::is_local);
    let sequences = symbols.iter().any(|symbol| {
        matches!(
            symbol,
            Symbol::Object {
                kind: ObjectKind::Sequence,
                ..
            }
        )
    });
    let user_variables: Vec<usize> = symbols
        .iter()
        .enumerate()
        .filter(|(_, symbol)| {
            matches!(
                symbol,
                Symbol::Local {
                    kind: LocalKind::UserVariable,
                    ..
                }
            )
        })
        .map(|(index, _)| index)
        .collect();
    let mut document = DocumentSchema::new(target, schemas);
    let mut found = Vec::new();
    for statement in root.children() {
        let range = statement.text_range();
        let in_scope = !only_local
            || locals
                .iter()
                .any(|local| local.contains_range(range) || *local == range);
        if in_scope {
            let names: Vec<SyntaxNode> = statement
                .descendants()
                .filter(|node| node.kind() == NAME && may_name(node, &wanted))
                .collect();
            let strings: Vec<SyntaxToken> = if sequences {
                sequence_strings(&statement)
            } else {
                Vec::new()
            };
            if !names.is_empty() || !strings.is_empty() {
                let catalog = document.catalog();
                let namer = Namer::new(&catalog, &document.state);
                let cases = Cases::of(&catalog);
                for name in names {
                    let Some((symbol, access)) = namer.symbol_at(&name) else {
                        continue;
                    };
                    if let Some(index) = symbols.iter().position(|wanted| wanted.same(&symbol, cases)) {
                        found.push((
                            index,
                            Hit {
                                range: name.text_range(),
                                access,
                                in_string: false,
                            },
                        ));
                    }
                }
                for string in strings {
                    let Some((schema, name, range)) = sequence_in_string(&string, target.dialect) else {
                        continue;
                    };
                    let symbol = namer.object_by_name(ObjectKind::Sequence, schema.map(Ident::new).as_ref(), &name);
                    let Some(symbol) = symbol else {
                        continue;
                    };
                    if let Some(index) = symbols.iter().position(|wanted| wanted.same(&symbol, cases)) {
                        found.push((
                            index,
                            Hit {
                                range,
                                access: Access::Read,
                                in_string: true,
                            },
                        ));
                    }
                }
            }
            if !user_variables.is_empty() {
                for token in statement
                    .descendants_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .filter(|token| token.kind() == VARIABLE)
                {
                    for index in &user_variables {
                        if symbols[*index]
                            .name()
                            .eq_ignore_ascii_case(user_variable_name(token.text()))
                        {
                            found.push((
                                *index,
                                Hit {
                                    range: token.text_range(),
                                    access: user_variable_access(&token),
                                    in_string: false,
                                },
                            ));
                        }
                    }
                }
            }
        }
        document.apply(&statement);
    }
    found.sort_by_key(|(_, hit)| (hit.range.start(), hit.range.end()));
    found.dedup_by_key(|(_, hit)| hit.range);
    found
}

/// The range of the statement a declaration is in.
fn statement_range(root: &SyntaxNode, declaration: TextRange) -> Option<TextRange> {
    root.children()
        .find(|statement| statement.text_range().contains_range(declaration))
        .map(|statement| statement.text_range())
}

/// The name of a user variable without its `@` and quotes.
pub fn user_variable_name(text: &str) -> &str {
    let bare = text.trim_start_matches('@');
    let quoted = bare.len() >= 2
        && (bare.starts_with('`') || bare.starts_with('\'') || bare.starts_with('"'))
        && bare.ends_with(&bare[..1]);
    if quoted { &bare[1..bare.len() - 1] } else { bare }
}

fn user_variable_access(token: &SyntaxToken) -> Access {
    let Some(reference) = token.parent() else {
        return Access::Read;
    };
    let assigned = reference.parent().is_some_and(|parent| {
        (parent.kind() == SET_ASSIGNMENT || parent.kind() == INTO_CLAUSE)
            && parent.children().next().as_ref() == Some(&reference)
    });
    let walrus = reference
        .siblings_with_tokens(sql_syntax::Direction::Next)
        .skip(1)
        .find(|element| !element.kind().is_trivia())
        .is_some_and(|element| element.kind() == COLON_EQ);
    if assigned || walrus {
        Access::Write
    } else {
        Access::Read
    }
}

/// The strings of a statement that name a sequence: the first argument of `nextval`, `currval`
/// and `setval`.
fn sequence_strings(statement: &SyntaxNode) -> Vec<SyntaxToken> {
    statement
        .descendants()
        .filter(|node| node.kind() == FUNCTION_CALL)
        .filter(|call| {
            child(call, QUALIFIED_NAME)
                .and_then(|name| children(&name, NAME).last())
                .is_some_and(|name| {
                    let text = bare_text(&name).to_ascii_lowercase();
                    matches!(text.as_str(), "nextval" | "currval" | "setval")
                })
        })
        .filter_map(|call| child(&call, ARG_LIST))
        .filter_map(|list| list.children().next())
        .filter_map(|argument| {
            argument
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|token| token.kind() == STRING)
        })
        .collect()
}

/// The schema and name a string of `nextval('...')` names, with the range of the name in it.
pub fn sequence_in_string(token: &SyntaxToken, dialect: Dialect) -> Option<(Option<String>, String, TextRange)> {
    let text = token.text();
    let inner = text.strip_prefix('\'')?.strip_suffix('\'')?;
    if inner.contains('\'') {
        return None;
    }
    let start = u32::from(token.text_range().start()) + 1;
    let mut quoted_part = false;
    let mut last_dot = None;
    for (index, character) in inner.char_indices() {
        match character {
            '"' => quoted_part = !quoted_part,
            '.' if !quoted_part => last_dot = Some(index),
            _ => {}
        }
    }
    let (schema, name_start) = match last_dot {
        Some(dot) => (Some(&inner[..dot]), dot + 1),
        None => (None, 0),
    };
    let raw = &inner[name_start..];
    let (name, quoted) = if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        (raw[1..raw.len() - 1].replace("\"\"", "\""), true)
    } else {
        (raw.to_string(), false)
    };
    if name.is_empty() {
        return None;
    }
    let name = crate::ident::fold(dialect, &name, quoted);
    let schema = schema.map(|schema| crate::ident::fold(dialect, schema.trim_matches('"'), schema.starts_with('"')));
    let range = TextRange::new(
        TextSize::from(start + name_start as u32),
        TextSize::from(start + inner.len() as u32),
    );
    Some((schema, name, range))
}

/// Whether a statement has SQL in strings that the server does not read: a routine body kept as a
/// string, `PREPARE`, `EXECUTE` and `DO`. Gives the strings whose text has `word` in it as a whole
/// word, in any case.
pub fn dynamic_sql_mentions(root: &SyntaxNode, word: &str) -> Vec<TextRange> {
    let mut found = Vec::new();
    for node in root.descendants() {
        let strings: Vec<SyntaxToken> = match node.kind() {
            ROUTINE_BODY => node
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .chain(
                    children(&node, LITERAL)
                        .flat_map(|literal| literal.children_with_tokens().filter_map(SyntaxElement::into_token)),
                )
                .collect(),
            PREPARE_STMT | EXECUTE_STMT | DO_STMT => node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .collect(),
            _ => continue,
        };
        for token in strings {
            if token.kind().is_string() && has_word(token.text(), word) {
                found.push(token.text_range());
            }
        }
    }
    found
}

/// Whether `text` has `word` in it, in any case, with no letter, digit or `_` on either side.
pub fn has_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let wanted = word.to_lowercase();
    let is_word = |character: char| character.is_alphanumeric() || character == '_' || character == '$';
    let mut from = 0;
    while let Some(found) = lower[from..].find(&wanted) {
        let start = from + found;
        let end = start + wanted.len();
        let before = lower[..start].chars().next_back();
        let after = lower[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            return true;
        }
        from = start + wanted.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Whether a node names a statement's target `QUALIFIED_NAME`, which a definition has.
pub fn is_definition_name(name: &SyntaxNode) -> bool {
    name.parent()
        .filter(|parent| parent.kind() == QUALIFIED_NAME)
        .and_then(|qualified| qualified.parent())
        .is_some_and(|owner| {
            matches!(
                owner.kind(),
                CREATE_TABLE_STMT
                    | CREATE_VIEW_STMT
                    | CREATE_FUNCTION_STMT
                    | CREATE_TYPE_STMT
                    | CREATE_DOMAIN_STMT
                    | CREATE_SEQUENCE_STMT
            )
        })
}

/// The sequence a string of `nextval('...')` at an offset names.
fn sequence_at(
    root: &SyntaxNode,
    offset: u32,
    target: Target,
    schemas: Schemas,
) -> Option<(TextRange, Symbol, Access)> {
    let at = TextSize::from(offset.min(u32::from(root.text_range().end())));
    let token = root.token_at_offset(at).find(|token| token.kind() == STRING)?;
    let statement = statement_of(&token.parent()?)?;
    if !sequence_strings(&statement).contains(&token) {
        return None;
    }
    let (schema, name, range) = sequence_in_string(&token, target.dialect)?;
    let document = DocumentSchema::before(root, u32::from(statement.text_range().start()), target, schemas);
    let catalog = document.catalog();
    let namer = Namer::new(&catalog, &document.state);
    let symbol = namer.object_by_name(ObjectKind::Sequence, schema.map(Ident::new).as_ref(), &name)?;
    Some((range, symbol, Access::Read))
}

/// What the name at an offset stands for, read against the document's DDL before it.
pub fn symbol_at_offset(
    root: &SyntaxNode,
    offset: u32,
    target: Target,
    schemas: Schemas,
) -> Option<(TextRange, Symbol, Access)> {
    if let Some(token) = variable_at(root, offset, target.dialect) {
        let name = user_variable_name(token.text()).to_string();
        let first = root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|other| other.kind() == VARIABLE && user_variable_name(other.text()).eq_ignore_ascii_case(&name))?;
        return Some((
            token.text_range(),
            Symbol::Local {
                kind: LocalKind::UserVariable,
                name,
                declaration: first.text_range(),
            },
            user_variable_access(&token),
        ));
    }
    if let Some(found) = sequence_at(root, offset, target, schemas) {
        return Some(found);
    }
    let name = name_at(root, offset)?;
    let statement = statement_of(&name)?;
    let document = DocumentSchema::before(root, u32::from(statement.text_range().start()), target, schemas);
    let catalog = document.catalog();
    let namer = Namer::new(&catalog, &document.state);
    let (symbol, access) = namer.symbol_at(&name)?;
    Some((name.text_range(), symbol, access))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_whole_words() {
        assert!(has_word("SELECT * FROM users", "USERS"));
        assert!(!has_word("SELECT * FROM users_old", "users"));
        assert!(has_word("x.users.id", "users"));
        assert!(!has_word("", "users"));
    }

    #[test]
    fn reads_the_sequence_a_string_names() {
        let root = sql_syntax::parse("SELECT nextval('\"a.b\"'), nextval('app.\"Ids\"');", Dialect::Postgres).syntax();
        let strings: Vec<_> = root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() == STRING)
            .collect();
        let read = |token: &SyntaxToken| {
            let (schema, name, _) = sequence_in_string(token, Dialect::Postgres).expect("a name");
            (schema, name)
        };
        assert_eq!(read(&strings[0]), (None, "a.b".to_string()));
        assert_eq!(read(&strings[1]), (Some("app".to_string()), "Ids".to_string()));
    }

    #[test]
    fn reads_the_name_of_a_user_variable() {
        assert_eq!(user_variable_name("@total"), "total");
        assert_eq!(user_variable_name("@`my var`"), "my var");
        assert_eq!(user_variable_name("@'x'"), "x");
    }
}
