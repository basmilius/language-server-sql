//! What a script can name: the objects of the schema in layers (the document's own DDL, the
//! snapshot, the DDL of the workspace) over the built-in catalog of the dialect, and how an
//! unqualified or qualified name finds one of them.
//!
//! A layer that has an object hides the same object in the layers after it, whole: a table the
//! document creates is that table, whatever the snapshot says about it.

use std::collections::HashMap;

use sql_catalog::model::{Routine, Schema, Sequence, Snapshot, Table, UserType};
use sql_catalog::{Builtins, builtins};
use sql_syntax::{Dialect, Target};

use crate::ident::{Case, Ident, name_case};

/// Where a layer comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// DDL of the document being analyzed, before the statement at hand.
    Document,
    /// A snapshot file.
    Snapshot,
    /// DDL in the `.sql` files of the workspace.
    Workspace,
}

#[derive(Clone, Debug, Default)]
struct SchemaIndex {
    tables: HashMap<String, Vec<u32>>,
    routines: HashMap<String, Vec<u32>>,
    types: HashMap<String, Vec<u32>>,
    sequences: HashMap<String, Vec<u32>>,
}

fn key(name: &str) -> String {
    name.to_lowercase()
}

fn index_of<T>(items: &[T], name: impl Fn(&T) -> &str) -> HashMap<String, Vec<u32>> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        map.entry(key(name(item))).or_default().push(position as u32);
    }
    map
}

impl SchemaIndex {
    fn of(schema: &Schema) -> SchemaIndex {
        SchemaIndex {
            tables: index_of(&schema.tables, |table| &table.name),
            routines: index_of(&schema.routines, |routine| &routine.name),
            types: index_of(&schema.types, |user_type| &user_type.name),
            sequences: index_of(&schema.sequences, |sequence| &sequence.name),
        }
    }
}

/// Objects of one origin, indexed by name. A schema named `""` holds what DDL created without
/// naming a schema: it is the default schema, whatever its name turns out to be.
#[derive(Clone, Debug)]
pub struct Layer {
    pub origin: Origin,
    pub snapshot: Snapshot,
    index: Vec<SchemaIndex>,
}

impl Layer {
    pub fn new(origin: Origin, snapshot: Snapshot) -> Layer {
        let index = snapshot.schemas.iter().map(SchemaIndex::of).collect();
        Layer {
            origin,
            snapshot,
            index,
        }
    }

    pub fn empty(origin: Origin) -> Layer {
        Layer::new(origin, Snapshot::default())
    }

    pub fn is_empty(&self) -> bool {
        self.snapshot.schemas.iter().all(|schema| {
            schema.tables.is_empty()
                && schema.routines.is_empty()
                && schema.types.is_empty()
                && schema.sequences.is_empty()
        })
    }

    /// The position of a schema, made when it is not there.
    pub fn ensure_schema(&mut self, name: &str, case: Case) -> usize {
        if let Some(position) = self
            .snapshot
            .schemas
            .iter()
            .position(|schema| case.eq(&schema.name, name))
        {
            return position;
        }
        self.snapshot.schemas.push(Schema {
            name: name.to_string(),
            ..Schema::default()
        });
        self.index.push(SchemaIndex::default());
        self.snapshot.schemas.len() - 1
    }

    pub fn schema_position(&self, name: &str, case: Case) -> Option<usize> {
        self.snapshot
            .schemas
            .iter()
            .position(|schema| case.eq(&schema.name, name))
    }

    pub fn find_table(&self, schema: usize, name: &str, case: Case) -> Option<usize> {
        self.index[schema]
            .tables
            .get(&key(name))?
            .iter()
            .map(|position| *position as usize)
            .find(|position| case.eq(&self.snapshot.schemas[schema].tables[*position].name, name))
    }

    /// Adds a table, in place of one with the same name.
    pub fn put_table(&mut self, schema: usize, table: Table, case: Case) {
        match self.find_table(schema, &table.name, case) {
            Some(position) => self.snapshot.schemas[schema].tables[position] = table,
            None => {
                let tables = &mut self.snapshot.schemas[schema].tables;
                self.index[schema]
                    .tables
                    .entry(key(&table.name))
                    .or_default()
                    .push(tables.len() as u32);
                tables.push(table);
            }
        }
    }

    pub fn table_mut(&mut self, schema: usize, table: usize) -> &mut Table {
        &mut self.snapshot.schemas[schema].tables[table]
    }

    pub fn remove_table(&mut self, schema: usize, table: usize) {
        self.snapshot.schemas[schema].tables.remove(table);
        self.index[schema] = SchemaIndex::of(&self.snapshot.schemas[schema]);
    }

    /// Renames a table, keeping the index right.
    pub fn rename_table(&mut self, schema: usize, table: usize, name: String) {
        self.snapshot.schemas[schema].tables[table].name = name;
        self.index[schema] = SchemaIndex::of(&self.snapshot.schemas[schema]);
    }

    pub fn push_routine(&mut self, schema: usize, routine: Routine, case: Case) {
        let routines = &mut self.snapshot.schemas[schema].routines;
        let same_signature = |known: &Routine| {
            case.eq(&known.name, &routine.name)
                && known.parameters.len() == routine.parameters.len()
                && known
                    .parameters
                    .iter()
                    .zip(&routine.parameters)
                    .all(|(a, b)| a.data_type == b.data_type)
        };
        if let Some(position) = routines.iter().position(same_signature) {
            routines[position] = routine;
            return;
        }
        self.index[schema]
            .routines
            .entry(key(&routine.name))
            .or_default()
            .push(routines.len() as u32);
        routines.push(routine);
    }

    pub fn put_type(&mut self, schema: usize, user_type: UserType, case: Case) {
        let types = &mut self.snapshot.schemas[schema].types;
        if let Some(position) = types.iter().position(|known| case.eq(&known.name, &user_type.name)) {
            types[position] = user_type;
            return;
        }
        self.index[schema]
            .types
            .entry(key(&user_type.name))
            .or_default()
            .push(types.len() as u32);
        types.push(user_type);
    }

    pub fn put_sequence(&mut self, schema: usize, sequence: Sequence, case: Case) {
        let sequences = &mut self.snapshot.schemas[schema].sequences;
        if let Some(position) = sequences.iter().position(|known| case.eq(&known.name, &sequence.name)) {
            sequences[position] = sequence;
            return;
        }
        self.index[schema]
            .sequences
            .entry(key(&sequence.name))
            .or_default()
            .push(sequences.len() as u32);
        sequences.push(sequence);
    }

    pub fn put_trigger(&mut self, schema: usize, trigger: sql_catalog::model::Trigger) {
        self.snapshot.schemas[schema].triggers.push(trigger);
    }

    /// Drops what a `DROP` names, when this layer has it.
    pub fn drop_object(&mut self, schema: usize, name: &str, case: Case, kind: DropKind) {
        let entry = &mut self.snapshot.schemas[schema];
        match kind {
            DropKind::Table => entry.tables.retain(|table| !case.eq(&table.name, name)),
            DropKind::Routine => entry.routines.retain(|routine| !case.eq(&routine.name, name)),
            DropKind::Type => entry.types.retain(|user_type| !case.eq(&user_type.name, name)),
            DropKind::Sequence => entry.sequences.retain(|sequence| !case.eq(&sequence.name, name)),
        }
        self.index[schema] = SchemaIndex::of(&self.snapshot.schemas[schema]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    Table,
    Routine,
    Type,
    Sequence,
}

/// What the statements of a script before the one at hand said about where names resolve.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptState {
    /// MySQL's and MariaDB's `USE db`.
    pub database: Option<String>,
    /// PostgreSQL's `SET search_path`.
    pub search_path: Vec<String>,
    /// SQLite's attached databases.
    pub attached: Vec<String>,
}

/// Where an object was found: in a layer, or in the system schemas of the built-in catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Layer(u16),
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TableId {
    pub place: Place,
    pub schema: u32,
    pub table: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectId {
    pub layer: u16,
    pub schema: u32,
    pub index: u32,
}

/// The layers and the built-in catalog one statement is read against.
pub struct Catalog<'a> {
    pub target: Target,
    pub layers: Vec<&'a Layer>,
    pub builtins: &'static Builtins,
    /// The schema an unqualified name means, when anything says.
    pub default_schema: Option<String>,
    /// The schemas an unqualified name is looked up in, in order.
    pub search: Vec<String>,
    /// Search every schema for an unqualified name, as when nothing says which is the default.
    pub search_all: bool,
    pub table_case: Case,
    pub name_case: Case,
}

impl<'a> Catalog<'a> {
    /// A catalog of layers in the order they win, for a script whose earlier statements said `state`.
    pub fn new(target: Target, layers: Vec<&'a Layer>, state: &ScriptState) -> Catalog<'a> {
        let dialect = target.dialect;
        let snapshot_default = layers
            .iter()
            .filter(|layer| layer.origin == Origin::Snapshot)
            .find_map(|layer| layer.snapshot.default_schema.clone());
        let snapshot_path: Vec<String> = layers
            .iter()
            .filter(|layer| layer.origin == Origin::Snapshot)
            .map(|layer| layer.snapshot.search_path.clone())
            .find(|path| !path.is_empty())
            .unwrap_or_default();
        let only_schema = || {
            let named: Vec<&str> = layers
                .iter()
                .filter(|layer| layer.origin == Origin::Snapshot)
                .flat_map(|layer| layer.snapshot.schemas.iter().map(|schema| schema.name.as_str()))
                .filter(|name| !name.is_empty())
                .collect();
            (named.len() == 1).then(|| named[0].to_string())
        };
        let lower_case_table_names = layers
            .iter()
            .find_map(|layer| layer.snapshot.source.lower_case_table_names);
        let table_case = match dialect {
            Dialect::Postgres => Case::Exact,
            Dialect::Mysql | Dialect::Mariadb if lower_case_table_names == Some(0) => Case::Exact,
            _ => Case::Insensitive,
        };
        let (default_schema, search, search_all) = match dialect {
            Dialect::Postgres => {
                let mut path = if !state.search_path.is_empty() {
                    state.search_path.clone()
                } else if !snapshot_path.is_empty() {
                    snapshot_path
                } else {
                    vec![snapshot_default.clone().unwrap_or_else(|| "public".to_string())]
                };
                let default = path.first().cloned();
                if !path.iter().any(|schema| schema == "pg_catalog") {
                    path.insert(0, "pg_catalog".to_string());
                }
                (default, path, false)
            }
            Dialect::Mysql | Dialect::Mariadb => {
                let default = state.database.clone().or(snapshot_default).or_else(only_schema);
                let search = default.iter().cloned().collect();
                let all = default.is_none();
                (default, search, all)
            }
            Dialect::Sqlite => {
                let mut search = vec!["temp".to_string(), "main".to_string()];
                search.extend(state.attached.iter().cloned());
                (Some("main".to_string()), search, false)
            }
            Dialect::Generic => {
                let default = snapshot_default.or_else(only_schema);
                (default.clone(), default.into_iter().collect(), true)
            }
        };
        Catalog {
            target,
            layers,
            builtins: builtins(dialect),
            default_schema,
            search,
            search_all,
            table_case,
            name_case: name_case(dialect),
        }
    }

    pub fn dialect(&self) -> Dialect {
        self.target.dialect
    }

    /// Whether a snapshot is loaded, which is what lets the server call a name unknown.
    pub fn has_snapshot(&self) -> bool {
        self.layers.iter().any(|layer| layer.origin == Origin::Snapshot)
    }

    pub fn layer(&self, index: u16) -> &'a Layer {
        self.layers[index as usize]
    }

    fn schema_of(&self, place: Place, schema: u32) -> &Schema {
        match place {
            Place::Layer(layer) => &self.layers[layer as usize].snapshot.schemas[schema as usize],
            Place::System => &self.builtins.schemas[schema as usize],
        }
    }

    pub fn table(&self, id: TableId) -> &Table {
        &self.schema_of(id.place, id.schema).tables[id.table as usize]
    }

    /// The schema a table is in, the default schema's name for one DDL created without a schema.
    pub fn schema_name(&self, place: Place, schema: u32) -> &str {
        let name = &self.schema_of(place, schema).name;
        if name.is_empty() {
            return self.default_schema.as_deref().unwrap_or("");
        }
        name
    }

    pub fn origin(&self, place: Place) -> Option<Origin> {
        match place {
            Place::Layer(layer) => Some(self.layers[layer as usize].origin),
            Place::System => None,
        }
    }

    fn is_default(&self, name: &str) -> bool {
        self.default_schema
            .as_deref()
            .is_some_and(|default| self.table_case.eq(default, name) || Case::Insensitive.eq(default, name))
    }

    /// The schemas of a layer a schema name stands for: the schema itself, and the unnamed one when
    /// the name is the default schema's.
    fn layer_schemas(&self, layer: &Layer, name: &str) -> Vec<usize> {
        let mut found = Vec::new();
        for (position, schema) in layer.snapshot.schemas.iter().enumerate() {
            let matches = if schema.name.is_empty() {
                self.is_default(name)
            } else {
                self.table_case.eq(&schema.name, name)
            };
            if matches {
                found.push(position);
            }
        }
        found
    }

    /// The table a name stands for, qualified or looked up along the search path.
    pub fn find_table(&self, schema: Option<&Ident>, name: &Ident) -> Option<TableId> {
        match schema {
            Some(schema) => self.find_table_in(&schema.text, &name.text),
            None => {
                for schema in &self.search {
                    if let Some(found) = self.find_table_in(schema, &name.text) {
                        return Some(found);
                    }
                }
                self.find_table_anywhere(&name.text)
            }
        }
    }

    fn find_table_in(&self, schema: &str, name: &str) -> Option<TableId> {
        for (layer_index, layer) in self.layers.iter().enumerate() {
            for position in self.layer_schemas(layer, schema) {
                if let Some(table) = layer.find_table(position, name, self.table_case) {
                    return Some(TableId {
                        place: Place::Layer(layer_index as u16),
                        schema: position as u32,
                        table: table as u32,
                    });
                }
            }
        }
        self.find_system_table(schema, name)
    }

    fn find_system_table(&self, schema: &str, name: &str) -> Option<TableId> {
        let position = self
            .builtins
            .schemas
            .iter()
            .position(|known| Case::Insensitive.eq(&known.name, schema))?;
        let table = self.builtins.system_table(position, name)?;
        Some(TableId {
            place: Place::System,
            schema: position as u32,
            table: table as u32,
        })
    }

    /// An unqualified name where nothing says which schema is meant: the unnamed schema of DDL
    /// first, then any schema.
    fn find_table_anywhere(&self, name: &str) -> Option<TableId> {
        for (layer_index, layer) in self.layers.iter().enumerate() {
            for (position, schema) in layer.snapshot.schemas.iter().enumerate() {
                let reachable = schema.name.is_empty() || self.search_all;
                if !reachable {
                    continue;
                }
                if let Some(table) = layer.find_table(position, name, self.table_case) {
                    return Some(TableId {
                        place: Place::Layer(layer_index as u16),
                        schema: position as u32,
                        table: table as u32,
                    });
                }
            }
        }
        None
    }

    /// Whether the schema a name is looked up in is known whole, so a name missing from it is
    /// missing from the database: a snapshot lists the schema, or it is a system schema.
    pub fn covers(&self, schema: Option<&Ident>) -> bool {
        let snapshots = || self.layers.iter().filter(|layer| layer.origin == Origin::Snapshot);
        match schema {
            Some(schema) => {
                self.builtins.system_schema(&schema.text).is_some()
                    || snapshots().any(|layer| {
                        layer
                            .snapshot
                            .schemas
                            .iter()
                            .any(|known| self.table_case.eq(&known.name, &schema.text))
                    })
            }
            None => {
                if self.search_all {
                    return snapshots().next().is_some();
                }
                snapshots().any(|layer| {
                    layer.snapshot.schemas.iter().any(|known| {
                        self.search
                            .iter()
                            .any(|schema| schema != "pg_catalog" && self.table_case.eq(&known.name, schema))
                    })
                })
            }
        }
    }

    /// Every table a name can reach unqualified, or every table of a schema, each name once, the
    /// layer that wins first. System tables come last.
    pub fn tables(&self, schema: Option<&str>) -> Vec<TableId> {
        let mut found: Vec<TableId> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut take = |id: TableId, name: &str, found: &mut Vec<TableId>| {
            if seen.insert(name.to_lowercase()) {
                found.push(id);
            }
        };
        let schemas: Vec<String> = match schema {
            Some(schema) => vec![schema.to_string()],
            None => self.search.clone(),
        };
        for (layer_index, layer) in self.layers.iter().enumerate() {
            for (position, known) in layer.snapshot.schemas.iter().enumerate() {
                let wanted = if known.name.is_empty() {
                    schema.is_none_or(|schema| self.is_default(schema))
                } else {
                    schemas.iter().any(|schema| self.table_case.eq(&known.name, schema))
                        || (schema.is_none() && self.search_all)
                };
                if !wanted {
                    continue;
                }
                for (table, entry) in known.tables.iter().enumerate() {
                    take(
                        TableId {
                            place: Place::Layer(layer_index as u16),
                            schema: position as u32,
                            table: table as u32,
                        },
                        &entry.name,
                        &mut found,
                    );
                }
            }
        }
        for (position, known) in self.builtins.schemas.iter().enumerate() {
            if !schemas.iter().any(|schema| Case::Insensitive.eq(&known.name, schema)) {
                continue;
            }
            for (table, entry) in known.tables.iter().enumerate() {
                let versions = self.builtins.table_versions[position][table];
                if versions.contains(self.target) {
                    take(
                        TableId {
                            place: Place::System,
                            schema: position as u32,
                            table: table as u32,
                        },
                        &entry.name,
                        &mut found,
                    );
                }
            }
        }
        found
    }

    /// The schemas a script can name, the default first.
    pub fn schema_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let push = |name: &str, names: &mut Vec<String>| {
            if !name.is_empty() && !names.iter().any(|known| Case::Insensitive.eq(known, name)) {
                names.push(name.to_string());
            }
        };
        if let Some(default) = &self.default_schema {
            push(default, &mut names);
        }
        for layer in &self.layers {
            for schema in &layer.snapshot.schemas {
                push(&schema.name, &mut names);
            }
        }
        for schema in &self.builtins.schemas {
            push(&schema.name, &mut names);
        }
        names
    }

    pub fn is_schema(&self, name: &Ident) -> bool {
        self.schema_names()
            .iter()
            .any(|known| self.table_case.eq(known, &name.text))
    }

    fn objects<T>(
        &self,
        schema: Option<&Ident>,
        name: &str,
        pick: impl Fn(&'a Schema) -> &'a [T],
        named: impl Fn(&T) -> &str,
    ) -> Vec<(ObjectId, &'a T)> {
        let schemas: Vec<String> = match schema {
            Some(schema) => vec![schema.text.clone()],
            None => self.search.clone(),
        };
        let mut found = Vec::new();
        for (layer_index, layer) in self.layers.iter().enumerate() {
            for (position, known) in layer.snapshot.schemas.iter().enumerate() {
                let wanted = if known.name.is_empty() {
                    schema.is_none_or(|schema| self.is_default(&schema.text))
                } else {
                    schemas.iter().any(|schema| self.table_case.eq(&known.name, schema))
                        || (schema.is_none() && self.search_all)
                };
                if !wanted {
                    continue;
                }
                for (index, item) in pick(known).iter().enumerate() {
                    if self.name_case.eq(named(item), name) {
                        found.push((
                            ObjectId {
                                layer: layer_index as u16,
                                schema: position as u32,
                                index: index as u32,
                            },
                            item,
                        ));
                    }
                }
            }
            if !found.is_empty() {
                return found;
            }
        }
        found
    }

    /// The routines of a name, the overloads of the first layer that has any.
    pub fn routines(&self, schema: Option<&Ident>, name: &str) -> Vec<(ObjectId, &'a Routine)> {
        self.objects(schema, name, |known| &known.routines, |routine| &routine.name)
    }

    pub fn user_type(&self, schema: Option<&Ident>, name: &str) -> Option<(ObjectId, &'a UserType)> {
        self.objects(schema, name, |known| &known.types, |user_type| &user_type.name)
            .into_iter()
            .next()
    }

    pub fn sequence(&self, schema: Option<&Ident>, name: &str) -> Option<(ObjectId, &'a Sequence)> {
        self.objects(schema, name, |known| &known.sequences, |sequence| &sequence.name)
            .into_iter()
            .next()
    }

    /// Every object of a kind a script can name unqualified, each name once.
    pub fn all<T>(&self, pick: impl Fn(&'a Schema) -> &'a [T], named: impl Fn(&T) -> &str) -> Vec<&'a T> {
        let mut seen = std::collections::HashSet::new();
        let mut found = Vec::new();
        for layer in &self.layers {
            for known in &layer.snapshot.schemas {
                let reachable = known.name.is_empty()
                    || self.search_all
                    || self.search.iter().any(|schema| self.table_case.eq(&known.name, schema));
                if !reachable {
                    continue;
                }
                for item in pick(known) {
                    if seen.insert(named(item).to_lowercase()) {
                        found.push(item);
                    }
                }
            }
        }
        found
    }

    /// The routines of a schema named with a qualifier.
    pub fn routines_in(&self, schema: &Ident) -> Vec<&'a Routine> {
        let mut found = Vec::new();
        for layer in &self.layers {
            for known in &layer.snapshot.schemas {
                let wanted = if known.name.is_empty() {
                    self.is_default(&schema.text)
                } else {
                    self.table_case.eq(&known.name, &schema.text)
                };
                if wanted {
                    found.extend(known.routines.iter());
                }
            }
        }
        found
    }

    pub fn routine_at(&self, id: ObjectId) -> &'a Routine {
        &self.layers[id.layer as usize].snapshot.schemas[id.schema as usize].routines[id.index as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_catalog::read_snapshot;
    use sql_syntax::Version;

    fn snapshot(json: &str) -> Layer {
        Layer::new(Origin::Snapshot, read_snapshot(json).expect("a snapshot"))
    }

    const SHOP: &str = r#"{
        "formatVersion": 1,
        "defaultSchema": "public",
        "schemas": [
            { "name": "public", "tables": [ { "name": "users", "columns": [ { "name": "id" } ] }, { "name": "Orders", "columns": [] } ] },
            { "name": "audit", "tables": [ { "name": "users", "columns": [ { "name": "at" } ] } ] }
        ]
    }"#;

    #[test]
    fn postgres_reads_along_the_search_path_and_compares_exactly() {
        let layer = snapshot(SHOP);
        let catalog = Catalog::new(
            Target::new(Dialect::Postgres, None),
            vec![&layer],
            &ScriptState::default(),
        );
        let users = catalog.find_table(None, &Ident::new("users")).expect("users");
        assert_eq!(catalog.schema_name(users.place, users.schema), "public");
        let audit = catalog
            .find_table(Some(&Ident::new("audit")), &Ident::new("users"))
            .expect("audit.users");
        assert_eq!(catalog.table(audit).columns[0].name, "at");
        assert!(catalog.find_table(None, &Ident::new("orders")).is_none());
        assert!(catalog.find_table(None, &Ident::new("Orders")).is_some());
        assert!(
            catalog.find_table(None, &Ident::new("pg_class")).is_some(),
            "pg_catalog comes first"
        );
        let state = ScriptState {
            search_path: vec!["audit".to_string()],
            ..ScriptState::default()
        };
        let moved = Catalog::new(Target::new(Dialect::Postgres, None), vec![&layer], &state);
        let found = moved.find_table(None, &Ident::new("users")).expect("users");
        assert_eq!(moved.table(found).columns[0].name, "at");
        assert!(catalog.covers(None));
        assert!(catalog.covers(Some(&Ident::new("information_schema"))));
        assert!(!catalog.covers(Some(&Ident::new("elsewhere"))));
    }

    #[test]
    fn the_document_wins_over_the_snapshot() {
        let layer = snapshot(SHOP);
        let mut document = Layer::empty(Origin::Document);
        let schema = document.ensure_schema("", Case::Exact);
        document.put_table(
            schema,
            Table {
                name: "users".to_string(),
                ..Table::default()
            },
            Case::Exact,
        );
        let catalog = Catalog::new(
            Target::new(Dialect::Postgres, None),
            vec![&document, &layer],
            &ScriptState::default(),
        );
        let users = catalog.find_table(None, &Ident::new("users")).expect("users");
        assert_eq!(users.place, Place::Layer(0));
        let qualified = catalog
            .find_table(Some(&Ident::new("public")), &Ident::new("users"))
            .expect("public.users");
        assert_eq!(
            qualified.place,
            Place::Layer(0),
            "the unnamed schema is the default one"
        );
        let names: Vec<&str> = catalog
            .tables(None)
            .into_iter()
            .map(|id| catalog.table(id).name.as_str())
            .take(2)
            .collect();
        assert_eq!(names, ["users", "Orders"]);
    }

    #[test]
    fn mysql_takes_the_database_of_use_and_ignores_case() {
        let layer = snapshot(SHOP);
        let state = ScriptState {
            database: Some("audit".to_string()),
            ..ScriptState::default()
        };
        let target = Target::new(Dialect::Mysql, Some(Version::new(8, 4, 0)));
        let catalog = Catalog::new(target, vec![&layer], &state);
        let users = catalog.find_table(None, &Ident::new("USERS")).expect("users");
        assert_eq!(catalog.schema_name(users.place, users.schema), "audit");
        assert!(
            catalog.find_table(None, &Ident::new("orders")).is_none(),
            "not in audit"
        );
        let catalog = Catalog::new(target, vec![&layer], &ScriptState::default());
        assert!(
            catalog.find_table(None, &Ident::new("orders")).is_some(),
            "the snapshot's default"
        );
    }

    #[test]
    fn sqlite_knows_its_schema_tables() {
        let catalog = Catalog::new(Target::new(Dialect::Sqlite, None), Vec::new(), &ScriptState::default());
        assert!(catalog.find_table(None, &Ident::new("sqlite_master")).is_some());
        assert!(catalog.find_table(None, &Ident::new("SQLITE_SCHEMA")).is_some());
        assert!(!catalog.has_snapshot());
    }
}
