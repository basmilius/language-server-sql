//! What the DDL of a script defines, applied statement by statement to a layer: tables and views
//! with their columns and keys, indexes, types, domains, sequences, routines, triggers and
//! schemas, and the changes `ALTER TABLE`, `RENAME TABLE`, `COMMENT ON` and `DROP` make to them.
//! `USE`, `SET search_path` and `ATTACH` change where later names resolve.

use std::path::PathBuf;

use sql_catalog::model::{
    Attribute, Check, Column, ForeignKey, Generated, Index, Key, Location, Parameter, ParameterMode, Routine,
    RoutineKind, Sequence, Table, TableKind, Trigger, TypeKind, UserType,
};
use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxElement, SyntaxNode, SyntaxToken, TextRange};

use crate::ast::{child, children, compact, has_token, inner_query, object_name, parts, tokens};
use crate::catalog::{DropKind, Layer, ScriptState};
use crate::ident::{Case, Ident, unquote};

/// What applying DDL needs besides the layer.
pub struct DdlContext<'a> {
    pub dialect: Dialect,
    /// The file the DDL is in; `None` for the document being analyzed.
    pub path: Option<PathBuf>,
    /// How table and schema names compare.
    pub case: Case,
    /// A table the layer does not have yet, from the layers below it, which `ALTER TABLE` and
    /// `COMMENT ON` change in a copy.
    pub base: &'a dyn Fn(Option<&str>, &str) -> Option<Table>,
}

fn location(context: &DdlContext, node: &SyntaxNode, name: TextRange) -> Option<Location> {
    let range = node.text_range();
    Some(Location {
        path: context.path.clone(),
        range: (range.start().into(), range.end().into()),
        name: (name.start().into(), name.end().into()),
    })
}

/// The schema DDL puts an object in that names none: the database of `USE`, the first schema of
/// `SET search_path`, or the unnamed default schema.
fn schema_for(state: &ScriptState, schema: Option<&Ident>) -> String {
    if let Some(schema) = schema {
        return schema.text.clone();
    }
    state
        .database
        .clone()
        .or_else(|| state.search_path.first().cloned())
        .unwrap_or_default()
}

/// Applies one statement to a layer and to the state of the script.
pub fn apply_statement(layer: &mut Layer, state: &mut ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    match statement.kind() {
        CREATE_TABLE_STMT => create_table(layer, state, statement, context),
        CREATE_VIEW_STMT => create_view(layer, state, statement, context),
        CREATE_INDEX_STMT => create_index(layer, state, statement, context),
        ALTER_TABLE_STMT => alter_table(layer, state, statement, context),
        RENAME_TABLE_STMT => rename_tables(layer, state, statement, context),
        DROP_STMT => drop(layer, state, statement, context),
        CREATE_TYPE_STMT => create_type(layer, state, statement, context),
        CREATE_DOMAIN_STMT => create_domain(layer, state, statement, context),
        CREATE_SEQUENCE_STMT => create_sequence(layer, state, statement, context),
        CREATE_FUNCTION_STMT => create_routine(layer, state, statement, context),
        CREATE_TRIGGER_STMT => create_trigger(layer, state, statement, context),
        CREATE_SCHEMA_STMT => {
            if let Some(name) = child(statement, NAME).and_then(|name| Ident::of_name(&name, context.dialect)) {
                let position = layer.ensure_schema(&name.text, context.case);
                let range = child(statement, NAME).map_or(statement.text_range(), |name| name.text_range());
                layer.snapshot.schemas[position].location = location(context, statement, range);
            }
        }
        COMMENT_STMT => comment(layer, state, statement, context),
        USE_STMT => {
            if let Some((_, name)) =
                child(statement, QUALIFIED_NAME).and_then(|name| object_name(&name, context.dialect))
            {
                state.database = Some(name.ident.text);
            }
        }
        SET_STMT => set_search_path(state, statement, context),
        ATTACH_STMT => {
            if let Some(name) = child(statement, NAME).and_then(|name| Ident::of_name(&name, context.dialect)) {
                if !state.attached.contains(&name.text) {
                    state.attached.push(name.text);
                }
            }
        }
        _ => {}
    }
}

/// The schema position and the name a `QUALIFIED_NAME` of a definition gives, with its range.
fn target_of(
    layer: &mut Layer,
    state: &ScriptState,
    name: &SyntaxNode,
    context: &DdlContext,
) -> Option<(usize, String, TextRange, Option<String>)> {
    let (schema, name) = object_name(name, context.dialect)?;
    let schema_name = schema_for(state, schema.as_ref().map(|part| &part.ident));
    let position = layer.ensure_schema(&schema_name, context.case);
    let explicit = schema.map(|part| part.ident.text);
    let range = name.range();
    Some((position, name.ident.text, range, explicit))
}

fn first_string(node: &SyntaxNode) -> Option<String> {
    node.descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| token.kind().is_string())
        .map(|token| unquote(STRING, token.text()).0)
}

fn create_table(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, table_name, name_range, _)) = target_of(layer, state, &name, context) else {
        return;
    };
    let temporary = has_token(statement, TEMPORARY_KW) || has_token(statement, TEMP_KW);
    let mut table = Table {
        name: table_name,
        kind: if temporary {
            TableKind::Temporary
        } else {
            TableKind::Table
        },
        location: location(context, statement, name_range),
        ..Table::default()
    };
    if let Some(list) = child(statement, TABLE_ELEMENT_LIST) {
        table_elements(&mut table, &list, context);
    } else if let Some(query) = inner_query(statement) {
        query_columns(&mut table, &query, None, context);
    } else if child(statement, LIKE_CLAUSE).is_some() {
        table.open = true;
    }
    if child(statement, TABLE_ELEMENT_LIST).is_some_and(|list| child(&list, LIKE_CLAUSE).is_some()) {
        table.open = true;
    }
    for option in children(statement, TABLE_OPTION) {
        if has_token(&option, COMMENT_KW) {
            table.comment = first_string(&option);
        }
    }
    layer.put_table(schema, table, context.case);
}

fn table_elements(table: &mut Table, list: &SyntaxNode, context: &DdlContext) {
    for element in list.children() {
        match element.kind() {
            COLUMN_DEF => {
                if let Some(column) = column_def(table, &element, context) {
                    table.columns.push(column);
                }
            }
            TABLE_CONSTRAINT => table_constraint(table, &element, context),
            _ => {}
        }
    }
    for (position, column) in table.columns.iter_mut().enumerate() {
        column.ordinal = Some(position as u32 + 1);
    }
}

/// A column of `CREATE TABLE` or `ADD COLUMN`; its column constraints become keys of the table.
fn column_def(table: &mut Table, node: &SyntaxNode, context: &DdlContext) -> Option<Column> {
    let name_node = child(node, NAME)?;
    let name = Ident::of_name(&name_node, context.dialect)?.text;
    let mut column = Column {
        name: name.clone(),
        data_type: child(node, TYPE).map(|data_type| compact(&data_type)),
        location: location(context, node, name_node.text_range()),
        ..Column::default()
    };
    for constraint in children(node, COLUMN_CONSTRAINT) {
        let words: Vec<SyntaxToken> = tokens(&constraint).collect();
        let kinds: Vec<_> = words.iter().map(SyntaxToken::kind).collect();
        let first = kinds.iter().copied().find(|kind| *kind != CONSTRAINT_KW);
        match first {
            Some(NOT_KW) if kinds.contains(&NULL_KW) => column.nullable = Some(false),
            Some(NULL_KW) => column.nullable = Some(true),
            Some(PRIMARY_KW) => {
                column.nullable = Some(false);
                table.primary_key = Some(Key {
                    name: None,
                    columns: vec![name.clone()],
                });
                if kinds.contains(&AUTOINCREMENT_KW) {
                    column.auto_increment = true;
                }
            }
            Some(UNIQUE_KW) => table.unique_keys.push(Key {
                name: None,
                columns: vec![name.clone()],
            }),
            Some(DEFAULT_KW) => {
                column.default = constraint
                    .children()
                    .next()
                    .map(|value| compact(&value))
                    .or_else(|| words.get(1).map(|token| token.text().to_string()));
            }
            Some(AUTO_INCREMENT_KW) | Some(AUTOINCREMENT_KW) => column.auto_increment = true,
            Some(COMMENT_KW) => column.comment = first_string(&constraint),
            Some(GENERATED_KW) | Some(AS_KW) => {
                if kinds.contains(&IDENTITY_KW) {
                    column.generated = Some(if kinds.contains(&ALWAYS_KW) {
                        Generated::IdentityAlways
                    } else {
                        Generated::IdentityByDefault
                    });
                } else {
                    let stored = kinds.contains(&STORED_KW)
                        || words
                            .iter()
                            .any(|token| token.text().eq_ignore_ascii_case("persistent"));
                    column.generated = Some(if stored { Generated::Stored } else { Generated::Virtual });
                    column.generation_expression = constraint.children().next().map(|value| compact(&value));
                }
            }
            _ => {}
        }
        if let Some(references) = child(&constraint, REFERENCES_CLAUSE) {
            if let Some(foreign_key) = foreign_key(vec![name.clone()], &references, None, context) {
                table.foreign_keys.push(foreign_key);
            }
        }
    }
    let serial = column.data_type.as_deref().is_some_and(|data_type| {
        matches!(
            data_type.to_ascii_lowercase().as_str(),
            "serial" | "bigserial" | "smallserial"
        )
    });
    if serial {
        column.auto_increment = true;
    }
    Some(column)
}

fn names_of(list: &SyntaxNode, context: &DdlContext) -> Vec<String> {
    match list.kind() {
        INDEX_COLUMN_LIST => children(list, INDEX_COLUMN)
            .map(|column| {
                column
                    .children()
                    .find(|inner| inner.kind() == COLUMN_REF)
                    .and_then(|reference| parts(&reference, context.dialect).pop())
                    .map_or_else(|| compact(&column), |part| part.ident.text)
            })
            .collect(),
        _ => parts(list, context.dialect)
            .into_iter()
            .map(|part| part.ident.text)
            .collect(),
    }
}

fn foreign_key(
    columns: Vec<String>,
    references: &SyntaxNode,
    name: Option<String>,
    context: &DdlContext,
) -> Option<ForeignKey> {
    let (schema, table) = object_name(&child(references, QUALIFIED_NAME)?, context.dialect)?;
    let referenced_columns = child(references, NAME_LIST)
        .map(|list| names_of(&list, context))
        .unwrap_or_default();
    let mut on_delete = None;
    let mut on_update = None;
    let words: Vec<SyntaxToken> = tokens(references).collect();
    for (index, token) in words.iter().enumerate() {
        if token.kind() != ON_KW {
            continue;
        }
        let Some(event) = words.get(index + 1) else {
            continue;
        };
        let action: Vec<String> = words[index + 2..]
            .iter()
            .take_while(|word| !matches!(word.kind(), ON_KW | MATCH_KW | DEFERRABLE_KW | NOT_KW | INITIALLY_KW))
            .map(|word| word.text().to_ascii_lowercase())
            .collect();
        let action = Some(action.join(" ")).filter(|action| !action.is_empty());
        match event.kind() {
            DELETE_KW => on_delete = action,
            UPDATE_KW => on_update = action,
            _ => {}
        }
    }
    Some(ForeignKey {
        name,
        columns,
        referenced_schema: schema.map(|part| part.ident.text),
        referenced_table: table.ident.text,
        referenced_columns,
        on_delete,
        on_update,
    })
}

fn table_constraint(table: &mut Table, node: &SyntaxNode, context: &DdlContext) {
    let name = child(node, NAME)
        .and_then(|name| Ident::of_name(&name, context.dialect))
        .map(|ident| ident.text);
    let words: Vec<_> = tokens(node).map(|token| token.kind()).collect();
    let kind = words.iter().copied().find(|kind| *kind != CONSTRAINT_KW);
    let columns = child(node, INDEX_COLUMN_LIST)
        .or_else(|| child(node, NAME_LIST))
        .map(|list| names_of(&list, context))
        .unwrap_or_default();
    match kind {
        Some(PRIMARY_KW) => {
            for column in &mut table.columns {
                if columns.iter().any(|name| context.case.eq(name, &column.name)) {
                    column.nullable = Some(false);
                }
            }
            table.primary_key = Some(Key { name, columns });
        }
        Some(UNIQUE_KW) => table.unique_keys.push(Key { name, columns }),
        Some(FOREIGN_KW) => {
            if let Some(references) = child(node, REFERENCES_CLAUSE) {
                if let Some(foreign_key) = foreign_key(columns, &references, name, context) {
                    table.foreign_keys.push(foreign_key);
                }
            }
        }
        Some(CHECK_KW) => {
            if let Some(expression) = node.children().find(|inner| inner.kind() != NAME) {
                table.checks.push(Check {
                    name,
                    expression: compact(&expression),
                });
            }
        }
        Some(INDEX_KW) | Some(KEY_KW) | Some(FULLTEXT_KW) | Some(SPATIAL_KW) => table.indexes.push(Index {
            name,
            columns,
            unique: false,
            method: matches!(kind, Some(FULLTEXT_KW) | Some(SPATIAL_KW)).then(|| {
                if kind == Some(FULLTEXT_KW) {
                    "fulltext"
                } else {
                    "spatial"
                }
                .to_string()
            }),
            predicate: None,
        }),
        _ => {}
    }
}

/// The output columns of a query, as far as the select list names them; a wildcard leaves the
/// table open.
fn query_columns(table: &mut Table, query: &SyntaxNode, names: Option<Vec<String>>, context: &DdlContext) {
    let mut found = Vec::new();
    let mut open = false;
    let mut select = query.clone();
    while select.kind() != SELECT {
        match select.children().find(|inner| crate::ast::is_query(inner.kind())) {
            Some(inner) => select = inner,
            None => break,
        }
    }
    if let Some(list) = child(&select, SELECT_LIST) {
        for item in children(&list, SELECT_ITEM) {
            match crate::resolve::output_name(&item, context.dialect) {
                Some(name) => found.push(name),
                None if item.children().any(|inner| inner.kind() == WILDCARD) => open = true,
                None => found.push(compact(&item)),
            }
        }
    } else {
        open = true;
    }
    if let Some(names) = names {
        found = names;
        open = false;
    }
    table.open = open;
    table.columns = found
        .into_iter()
        .enumerate()
        .map(|(position, name)| Column {
            name,
            ordinal: Some(position as u32 + 1),
            ..Column::default()
        })
        .collect();
}

fn create_view(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, view_name, name_range, _)) = target_of(layer, state, &name, context) else {
        return;
    };
    let mut table = Table {
        name: view_name,
        kind: if has_token(statement, MATERIALIZED_KW) {
            TableKind::MaterializedView
        } else {
            TableKind::View
        },
        location: location(context, statement, name_range),
        ..Table::default()
    };
    let names = child(statement, NAME_LIST).map(|list| names_of(&list, context));
    match inner_query(statement) {
        Some(query) => {
            table.definition = Some(compact(&query));
            query_columns(&mut table, &query, names, context);
        }
        None => table.open = true,
    }
    layer.put_table(schema, table, context.case);
}

/// A table of this layer by a `QUALIFIED_NAME`, copied from the layers below when only they have it.
fn table_in_layer(
    layer: &mut Layer,
    state: &ScriptState,
    name: &SyntaxNode,
    context: &DdlContext,
) -> Option<(usize, usize)> {
    let (schema, table_name, _, explicit) = target_of(layer, state, name, context)?;
    if let Some(table) = layer.find_table(schema, &table_name, context.case) {
        return Some((schema, table));
    }
    let base = (context.base)(explicit.as_deref(), &table_name)?;
    layer.put_table(schema, base, context.case);
    Some((schema, layer.find_table(schema, &table_name, context.case)?))
}

fn create_index(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let mut index_name = None;
    let mut table_name = None;
    let mut after_on = false;
    for element in statement.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind() == ON_KW => after_on = true,
            SyntaxElement::Node(node) if node.kind() == QUALIFIED_NAME => {
                if after_on {
                    table_name = Some(node);
                } else {
                    index_name = object_name(&node, context.dialect).map(|(_, name)| name.ident.text);
                }
            }
            _ => {}
        }
    }
    let Some(table_name) = table_name else {
        return;
    };
    let columns = child(statement, INDEX_COLUMN_LIST)
        .map(|list| names_of(&list, context))
        .unwrap_or_default();
    let Some((schema, table)) = table_in_layer(layer, state, &table_name, context) else {
        return;
    };
    let unique = has_token(statement, UNIQUE_KW);
    let predicate =
        child(statement, WHERE_CLAUSE).and_then(|clause| clause.children().next().map(|inner| compact(&inner)));
    layer.table_mut(schema, table).indexes.push(Index {
        name: index_name,
        columns,
        unique,
        method: None,
        predicate,
    });
}

fn alter_table(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, mut position)) = table_in_layer(layer, state, &name, context) else {
        return;
    };
    let dialect = context.dialect;
    let column_case = crate::ident::name_case(dialect);
    for action in statement.children() {
        let table = layer.table_mut(schema, position);
        match action.kind() {
            ADD_COLUMN_ACTION => {
                let mut added = Vec::new();
                if let Some(definition) = child(&action, COLUMN_DEF) {
                    added.extend(column_def(table, &definition, context));
                }
                if let Some(list) = child(&action, TABLE_ELEMENT_LIST) {
                    for definition in children(&list, COLUMN_DEF) {
                        added.extend(column_def(table, &definition, context));
                    }
                }
                for column in added {
                    table.columns.retain(|known| !column_case.eq(&known.name, &column.name));
                    table.columns.push(column);
                }
            }
            DROP_COLUMN_ACTION => {
                if let Some(dropped) = child(&action, NAME).and_then(|name| Ident::of_name(&name, dialect)) {
                    table
                        .columns
                        .retain(|known| !column_case.eq(&known.name, &dropped.text));
                }
            }
            RENAME_COLUMN_ACTION => {
                let names: Vec<Ident> = children(&action, NAME)
                    .filter_map(|name| Ident::of_name(&name, dialect))
                    .collect();
                if let [from, to] = names.as_slice() {
                    for column in &mut table.columns {
                        if column_case.eq(&column.name, &from.text) {
                            column.name = to.text.clone();
                        }
                    }
                }
            }
            MODIFY_COLUMN_ACTION => {
                let old = child(&action, NAME).and_then(|name| Ident::of_name(&name, dialect));
                if let Some(definition) = child(&action, COLUMN_DEF) {
                    if let Some(column) = column_def(table, &definition, context) {
                        let replaced = old.map_or_else(|| column.name.clone(), |old| old.text);
                        match table
                            .columns
                            .iter()
                            .position(|known| column_case.eq(&known.name, &replaced))
                        {
                            Some(at) => table.columns[at] = column,
                            None => table.columns.push(column),
                        }
                    }
                }
            }
            ALTER_COLUMN_ACTION => {
                let Some(altered) = child(&action, NAME).and_then(|name| Ident::of_name(&name, dialect)) else {
                    continue;
                };
                let Some(column) = table
                    .columns
                    .iter_mut()
                    .find(|known| column_case.eq(&known.name, &altered.text))
                else {
                    continue;
                };
                let kinds: Vec<_> = tokens(&action).map(|token| token.kind()).collect();
                if let Some(data_type) = child(&action, TYPE) {
                    column.data_type = Some(compact(&data_type));
                } else if kinds.contains(&NOT_KW) && kinds.contains(&NULL_KW) {
                    column.nullable = Some(!kinds.contains(&DROP_KW));
                } else if kinds.contains(&DEFAULT_KW) {
                    column.default = if kinds.contains(&DROP_KW) {
                        None
                    } else {
                        action.children().last().map(|value| compact(&value))
                    };
                }
            }
            ADD_CONSTRAINT_ACTION => {
                if let Some(constraint) = child(&action, TABLE_CONSTRAINT) {
                    table_constraint(table, &constraint, context);
                }
            }
            RENAME_TABLE_ACTION => {
                if let Some((_, renamed)) = child(&action, QUALIFIED_NAME).and_then(|name| object_name(&name, dialect))
                {
                    layer.rename_table(schema, position, renamed.ident.text.clone());
                    if let Some(found) = layer.find_table(schema, &renamed.ident.text, context.case) {
                        position = found;
                    }
                }
            }
            _ => {}
        }
    }
    let table = layer.table_mut(schema, position);
    for (index, column) in table.columns.iter_mut().enumerate() {
        column.ordinal = Some(index as u32 + 1);
    }
}

fn rename_tables(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let names: Vec<SyntaxNode> = children(statement, QUALIFIED_NAME).collect();
    for pair in names.chunks(2) {
        let [from, to] = pair else {
            continue;
        };
        let Some((schema, position)) = table_in_layer(layer, state, from, context) else {
            continue;
        };
        if let Some((_, renamed)) = object_name(to, context.dialect) {
            layer.rename_table(schema, position, renamed.ident.text);
        }
    }
}

fn drop(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let object = tokens(statement).map(|token| token.kind()).find(|kind| {
        matches!(
            kind,
            TABLE_KW
                | VIEW_KW
                | FUNCTION_KW
                | PROCEDURE_KW
                | TYPE_KW
                | DOMAIN_KW
                | SEQUENCE_KW
                | INDEX_KW
                | TRIGGER_KW
                | SCHEMA_KW
                | DATABASE_KW
                | EXTENSION_KW
        )
    });
    let kind = match object {
        Some(TABLE_KW | VIEW_KW) => Some(DropKind::Table),
        Some(FUNCTION_KW | PROCEDURE_KW) => Some(DropKind::Routine),
        Some(TYPE_KW | DOMAIN_KW) => Some(DropKind::Type),
        Some(SEQUENCE_KW) => Some(DropKind::Sequence),
        _ => None,
    };
    let Some(kind) = kind else {
        return;
    };
    for name in children(statement, QUALIFIED_NAME) {
        let Some((schema, part)) = object_name(&name, context.dialect) else {
            continue;
        };
        let schema_name = schema_for(state, schema.as_ref().map(|part| &part.ident));
        if let Some(position) = layer.schema_position(&schema_name, context.case) {
            layer.drop_object(position, &part.ident.text, context.case, kind);
        }
    }
}

fn create_type(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, type_name, name_range, _)) = target_of(layer, state, &name, context) else {
        return;
    };
    let mut user_type = UserType {
        name: type_name,
        location: location(context, statement, name_range),
        ..UserType::default()
    };
    if let Some(values) = child(statement, ENUM_VALUE_LIST) {
        user_type.kind = TypeKind::Enum;
        user_type.values = values
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind().is_string())
            .map(|token| unquote(STRING, token.text()).0)
            .collect();
    } else if let Some(list) = child(statement, TABLE_ELEMENT_LIST) {
        user_type.kind = TypeKind::Composite;
        user_type.attributes = children(&list, COLUMN_DEF)
            .filter_map(|column| {
                Some(Attribute {
                    name: Ident::of_name(&child(&column, NAME)?, context.dialect)?.text,
                    data_type: child(&column, TYPE).map(|data_type| compact(&data_type)),
                })
            })
            .collect();
    } else if has_token(statement, RANGE_KW) {
        user_type.kind = TypeKind::Range;
    }
    layer.put_type(schema, user_type, context.case);
}

fn create_domain(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, type_name, name_range, _)) = target_of(layer, state, &name, context) else {
        return;
    };
    let not_null = children(statement, COLUMN_CONSTRAINT).any(|constraint| {
        let kinds: Vec<_> = tokens(&constraint).map(|token| token.kind()).collect();
        kinds.contains(&NOT_KW) && kinds.contains(&NULL_KW)
    });
    layer.put_type(
        schema,
        UserType {
            name: type_name,
            kind: TypeKind::Domain,
            base_type: child(statement, TYPE).map(|data_type| compact(&data_type)),
            nullable: not_null.then_some(false),
            location: location(context, statement, name_range),
            ..UserType::default()
        },
        context.case,
    );
}

fn create_sequence(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, sequence_name, name_range, _)) = target_of(layer, state, &name, context) else {
        return;
    };
    layer.put_sequence(
        schema,
        Sequence {
            name: sequence_name,
            location: location(context, statement, name_range),
            ..Sequence::default()
        },
        context.case,
    );
}

fn create_routine(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    let Some((schema, routine_name, name_range, _)) = target_of(layer, state, &name, context) else {
        return;
    };
    let procedure = has_token(statement, PROCEDURE_KW);
    let parameters = child(statement, PARAM_LIST)
        .map(|list| {
            children(&list, PARAM_DEF)
                .map(|param| {
                    let mode = tokens(&param).find_map(|token| match token.kind() {
                        IN_KW => Some(ParameterMode::In),
                        OUT_KW => Some(ParameterMode::Out),
                        INOUT_KW => Some(ParameterMode::Inout),
                        VARIADIC_KW => Some(ParameterMode::Variadic),
                        _ => None,
                    });
                    let default = has_token(&param, DEFAULT_KW) || has_token(&param, EQ);
                    Parameter {
                        name: child(&param, NAME)
                            .and_then(|name| Ident::of_name(&name, context.dialect))
                            .map(|ident| ident.text),
                        data_type: child(&param, TYPE).map(|data_type| compact(&data_type)),
                        mode,
                        default: default
                            .then(|| {
                                param
                                    .children()
                                    .filter(|inner| inner.kind() != NAME && inner.kind() != TYPE)
                                    .last()
                            })
                            .flatten()
                            .map(|value| compact(&value)),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let returns = child(statement, RETURNS_CLAUSE).map(|clause| {
        let text = compact(&clause);
        text.split_once(' ').map_or(text.clone(), |(_, rest)| rest.to_string())
    });
    let language = tokens(statement)
        .any(|token| token.kind() == LANGUAGE_KW)
        .then(|| {
            statement
                .children()
                .filter(|inner| inner.kind() == NAME)
                .last()
                .and_then(|name| Ident::of_name(&name, context.dialect))
                .map(|ident| ident.text)
        })
        .flatten();
    layer.push_routine(
        schema,
        Routine {
            name: routine_name,
            kind: if procedure {
                RoutineKind::Procedure
            } else {
                RoutineKind::Function
            },
            parameters,
            returns,
            language,
            comment: None,
            location: location(context, statement, name_range),
        },
        context.case,
    );
}

fn create_trigger(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let names: Vec<SyntaxNode> = children(statement, QUALIFIED_NAME).collect();
    let [name, table, ..] = names.as_slice() else {
        return;
    };
    let Some((schema, trigger_name, name_range, _)) = target_of(layer, state, name, context) else {
        return;
    };
    let table_name = object_name(table, context.dialect)
        .map(|(_, part)| part.ident.text)
        .unwrap_or_default();
    let words: Vec<_> = tokens(statement).map(|token| token.kind()).collect();
    let timing = if words.contains(&BEFORE_KW) {
        Some("before")
    } else if words.contains(&AFTER_KW) {
        Some("after")
    } else if words.contains(&INSTEAD_KW) {
        Some("instead of")
    } else {
        None
    };
    let events = [
        (INSERT_KW, "insert"),
        (UPDATE_KW, "update"),
        (DELETE_KW, "delete"),
        (TRUNCATE_KW, "truncate"),
    ]
    .iter()
    .filter(|(kind, _)| words.contains(kind))
    .map(|(_, event)| event.to_string())
    .collect();
    layer.put_trigger(
        schema,
        Trigger {
            name: trigger_name,
            table: table_name,
            timing: timing.map(str::to_string),
            events,
            comment: None,
            location: location(context, statement, name_range),
        },
    );
}

fn comment(layer: &mut Layer, state: &ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    let words: Vec<_> = tokens(statement).map(|token| token.kind()).collect();
    let text = child(statement, LITERAL).and_then(|literal| first_string(&literal));
    let Some(name) = child(statement, QUALIFIED_NAME) else {
        return;
    };
    if words.contains(&COLUMN_KW) {
        let mut parts = parts(&name, context.dialect);
        let Some(column) = parts.pop() else {
            return;
        };
        let Some(table) = parts.pop() else {
            return;
        };
        let schema = parts.pop();
        let explicit = schema.as_ref().map(|part| part.ident.text.clone());
        let schema_name = schema_for(state, schema.as_ref().map(|part| &part.ident));
        let position = layer.ensure_schema(&schema_name, context.case);
        let found = match layer.find_table(position, &table.ident.text, context.case) {
            Some(found) => Some(found),
            None => (context.base)(explicit.as_deref(), &table.ident.text).and_then(|base| {
                layer.put_table(position, base, context.case);
                layer.find_table(position, &table.ident.text, context.case)
            }),
        };
        if let Some(found) = found {
            let column_case = crate::ident::name_case(context.dialect);
            for known in &mut layer.table_mut(position, found).columns {
                if column_case.eq(&known.name, &column.ident.text) {
                    known.comment = text.clone();
                }
            }
        }
    } else if words.contains(&TABLE_KW) || words.contains(&VIEW_KW) {
        if let Some((position, found)) = table_in_layer(layer, state, &name, context) {
            layer.table_mut(position, found).comment = text;
        }
    }
}

fn set_search_path(state: &mut ScriptState, statement: &SyntaxNode, context: &DdlContext) {
    for assignment in children(statement, SET_ASSIGNMENT) {
        let Some(setting) = child(&assignment, QUALIFIED_NAME) else {
            continue;
        };
        if !compact(&setting).eq_ignore_ascii_case("search_path") {
            continue;
        }
        let mut schemas = Vec::new();
        for token in assignment
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.text_range().start() >= setting.text_range().end())
        {
            if token.kind().is_identifier() || token.kind() == STRING || token.kind().is_keyword() {
                if matches!(token.kind(), TO_KW | DEFAULT_KW) {
                    continue;
                }
                let (text, quoted) = unquote(token.kind(), token.text());
                for piece in text.split(',') {
                    let piece = piece.trim();
                    if !piece.is_empty() && piece != "$user" && piece != "\"$user\"" {
                        schemas.push(crate::ident::fold(
                            context.dialect,
                            piece,
                            quoted && token.kind() != STRING,
                        ));
                    }
                }
            }
        }
        state.search_path = schemas;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Origin;
    use sql_catalog::model::TableKind;
    use sql_syntax::parse;

    fn layer_of(text: &str, dialect: Dialect) -> (Layer, ScriptState) {
        let root = parse(text, dialect).syntax();
        let mut layer = Layer::empty(Origin::Document);
        let mut state = ScriptState::default();
        let base = |_: Option<&str>, _: &str| None;
        let context = DdlContext {
            dialect,
            path: None,
            case: if dialect == Dialect::Postgres {
                Case::Exact
            } else {
                Case::Insensitive
            },
            base: &base,
        };
        for statement in root.children() {
            apply_statement(&mut layer, &mut state, &statement, &context);
        }
        (layer, state)
    }

    fn table<'a>(layer: &'a Layer, name: &str) -> &'a Table {
        layer
            .snapshot
            .schemas
            .iter()
            .flat_map(|schema| &schema.tables)
            .find(|table| table.name == name)
            .unwrap_or_else(|| panic!("no table {name}"))
    }

    fn columns(table: &Table) -> Vec<String> {
        table
            .columns
            .iter()
            .map(|column| {
                let mut text = column.name.clone();
                if let Some(data_type) = &column.data_type {
                    text.push_str(&format!(" {data_type}"));
                }
                if column.nullable == Some(false) {
                    text.push_str(" not null");
                }
                if let Some(default) = &column.default {
                    text.push_str(&format!(" default {default}"));
                }
                if column.auto_increment {
                    text.push_str(" auto");
                }
                if let Some(comment) = &column.comment {
                    text.push_str(&format!(" -- {comment}"));
                }
                text
            })
            .collect()
    }

    #[test]
    fn reads_tables_with_their_columns_and_keys() {
        let (layer, _) = layer_of(
            "CREATE TABLE orgs (id INT PRIMARY KEY);
             CREATE TABLE Users (
                 id BIGINT AUTO_INCREMENT,
                 org_id INT NOT NULL REFERENCES orgs (id) ON DELETE CASCADE,
                 email VARCHAR(255) DEFAULT '' COMMENT 'Where we write',
                 PRIMARY KEY (id),
                 UNIQUE KEY users_email (email),
                 KEY users_org (org_id)
             ) COMMENT = 'People';",
            Dialect::Mysql,
        );
        let users = table(&layer, "Users");
        assert_eq!(
            columns(users),
            [
                "id BIGINT not null auto",
                "org_id INT not null",
                "email VARCHAR(255) default '' -- Where we write"
            ]
        );
        assert_eq!(users.comment.as_deref(), Some("People"));
        assert_eq!(
            users.primary_key.as_ref().map(|key| key.columns.clone()),
            Some(vec!["id".to_string()])
        );
        assert_eq!(users.unique_keys[0].name.as_deref(), Some("users_email"));
        assert_eq!(users.indexes[0].columns, ["org_id"]);
        let foreign_key = &users.foreign_keys[0];
        assert_eq!(foreign_key.referenced_table, "orgs");
        assert_eq!(foreign_key.referenced_columns, ["id"]);
        assert_eq!(foreign_key.on_delete.as_deref(), Some("cascade"));
        let location = users.location.as_ref().expect("a place");
        assert_eq!(location.path, None);
    }

    #[test]
    fn alter_rename_and_drop_change_what_is_there() {
        let (layer, _) = layer_of(
            "CREATE TABLE t (a int, b text);
             ALTER TABLE t ADD COLUMN c int NOT NULL, DROP COLUMN a, RENAME COLUMN b TO bee;
             ALTER TABLE t ALTER COLUMN c TYPE bigint, ALTER COLUMN c SET DEFAULT 1;
             ALTER TABLE t RENAME TO u;
             COMMENT ON COLUMN u.bee IS 'The bee';
             CREATE TABLE gone (x int);
             DROP TABLE gone;",
            Dialect::Postgres,
        );
        assert_eq!(
            columns(table(&layer, "u")),
            ["bee text -- The bee", "c bigint not null default 1"]
        );
        assert!(
            layer
                .snapshot
                .schemas
                .iter()
                .all(|schema| schema.tables.iter().all(|table| table.name != "gone"))
        );
    }

    #[test]
    fn views_types_routines_and_where_names_go() {
        let (layer, state) = layer_of(
            "CREATE TABLE app.t (a int);
             CREATE VIEW app.v (x, y) AS SELECT a, a + 1 FROM app.t;
             CREATE MATERIALIZED VIEW w AS SELECT a AS first, t.* FROM app.t;
             CREATE TYPE mood AS ENUM ('sad', 'ok');
             CREATE DOMAIN email AS text NOT NULL;
             CREATE FUNCTION add(a int, b int DEFAULT 1) RETURNS int LANGUAGE sql AS 'SELECT a + b';
             SET search_path TO app, public;
             CREATE TABLE later (z int);",
            Dialect::Postgres,
        );
        let view = table(&layer, "v");
        assert_eq!(view.kind, TableKind::View);
        assert_eq!(columns(view), ["x", "y"]);
        let materialized = table(&layer, "w");
        assert_eq!(materialized.kind, TableKind::MaterializedView);
        assert!(materialized.open, "a wildcard leaves the columns open");
        let schema_of = |name: &str| {
            layer
                .snapshot
                .schemas
                .iter()
                .find(|schema| schema.tables.iter().any(|table| table.name == name))
                .map(|schema| schema.name.clone())
        };
        assert_eq!(schema_of("t").as_deref(), Some("app"));
        assert_eq!(schema_of("w").as_deref(), Some(""));
        assert_eq!(schema_of("later").as_deref(), Some("app"));
        assert_eq!(state.search_path, ["app", "public"]);
        let default = &layer.snapshot.schemas[layer.schema_position("", Case::Exact).expect("default")];
        assert_eq!(default.types[0].values, ["sad", "ok"]);
        assert_eq!(default.types[1].kind, TypeKind::Domain);
        let routine = &default.routines[0];
        assert_eq!(routine.returns.as_deref(), Some("int"));
        assert_eq!(routine.language.as_deref(), Some("sql"));
        assert_eq!(routine.parameters[1].default.as_deref(), Some("1"));
    }

    #[test]
    fn use_names_the_database_of_what_follows() {
        let (layer, state) = layer_of("USE shop; CREATE TABLE items (id INT);", Dialect::Mysql);
        assert_eq!(state.database.as_deref(), Some("shop"));
        assert_eq!(layer.snapshot.schemas[0].name, "shop");
    }
}
