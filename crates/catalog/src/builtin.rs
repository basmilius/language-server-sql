//! What every database of a dialect has before a schema adds anything: functions, data types, the
//! system schemas with their tables and views, and the settings. The lists come from the servers
//! (`scripts/catalog.py` writes `data/<dialect>.tsv`); the descriptions, and the signatures and
//! return types a server does not report, are written by hand in `data/descriptions.tsv`.

use std::collections::HashMap;
use std::sync::OnceLock;

use sql_syntax::{Dialect, Target, Version};

use crate::model::{Column, Schema, Table, TableKind};

const POSTGRES: &str = include_str!("../data/postgres.tsv");
const MYSQL: &str = include_str!("../data/mysql.tsv");
const MARIADB: &str = include_str!("../data/mariadb.tsv");
const SQLITE: &str = include_str!("../data/sqlite.tsv");
const DESCRIPTIONS: &str = include_str!("../data/descriptions.tsv");

/// The versions a row of a catalog holds for, both ends included and compared by major and minor;
/// an open end is unbounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Versions {
    pub since: Option<Version>,
    pub until: Option<Version>,
}

impl Versions {
    pub const ALL: Versions = Versions {
        since: None,
        until: None,
    };

    fn parse(text: &str) -> Versions {
        let Some((since, until)) = text.split_once('-') else {
            return Versions::ALL;
        };
        Versions {
            since: Version::parse(since),
            until: Version::parse(until),
        }
    }

    /// The versions from the first that any of these holds in to the last; no ranges at all is
    /// every version.
    fn span(all: impl Iterator<Item = Versions>) -> Versions {
        let mut span: Option<Versions> = None;
        for versions in all {
            span = Some(match span {
                None => versions,
                Some(known) => Versions {
                    since: known.since.zip(versions.since).map(|(a, b)| a.min(b)),
                    until: known.until.zip(versions.until).map(|(a, b)| a.max(b)),
                },
            });
        }
        span.unwrap_or(Versions::ALL)
    }

    /// Whether the row holds at a target; without a version the target is the newest.
    pub fn contains(self, target: Target) -> bool {
        let Some(version) = target.version else {
            return self.until.is_none();
        };
        let minor = (version.major, version.minor);
        self.since.is_none_or(|since| (since.major, since.minor) <= minor)
            && self.until.is_none_or(|until| minor <= (until.major, until.minor))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FunctionKind {
    Scalar,
    Aggregate,
    Window,
    Procedure,
}

impl FunctionKind {
    pub fn label(self) -> &'static str {
        match self {
            FunctionKind::Scalar => "function",
            FunctionKind::Aggregate => "aggregate function",
            FunctionKind::Window => "window function",
            FunctionKind::Procedure => "procedure",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// Empty when the server does not name the parameter.
    pub name: String,
    pub data_type: Option<String>,
    /// May be left out of a call.
    pub optional: bool,
    /// Takes any number of arguments, as the last parameter.
    pub variadic: bool,
}

impl Param {
    /// `name type`, `name`, or the type alone, with `...` before a variadic one.
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.variadic {
            out.push_str("...");
        }
        out.push_str(&self.name);
        if let Some(data_type) = &self.data_type {
            if !self.name.is_empty() {
                out.push(' ');
            }
            out.push_str(data_type);
        }
        if self.optional {
            out.push('?');
        }
        out
    }
}

/// One way to call a built-in function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overload {
    pub kind: FunctionKind,
    pub params: Vec<Param>,
    pub returns: Option<String>,
    pub versions: Versions,
}

impl Overload {
    /// The fewest and the most arguments, `None` for any number.
    pub fn arity(&self) -> (usize, Option<usize>) {
        let required = self
            .params
            .iter()
            .filter(|param| !param.optional && !param.variadic)
            .count();
        let variadic = self.params.iter().any(|param| param.variadic);
        (required, (!variadic).then_some(self.params.len()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// In lower case, as every dialect matches a function's name without case.
    pub name: String,
    pub overloads: Vec<Overload>,
    pub description: Option<String>,
    /// A form the grammar of the dialect reads, such as PostgreSQL's `coalesce`, which the server's
    /// catalog of functions does not list.
    pub grammar: bool,
}

impl Function {
    /// The overloads that hold at a target.
    pub fn overloads_at(&self, target: Target) -> impl Iterator<Item = &Overload> {
        self.overloads
            .iter()
            .filter(move |overload| overload.versions.contains(target))
    }

    pub fn kind(&self) -> FunctionKind {
        let kinds = || self.overloads.iter().map(|overload| overload.kind);
        if kinds().all(|kind| kind == FunctionKind::Procedure) {
            return FunctionKind::Procedure;
        }
        if kinds().any(|kind| kind == FunctionKind::Window) && kinds().all(|kind| kind != FunctionKind::Scalar) {
            return FunctionKind::Window;
        }
        if kinds().any(|kind| kind == FunctionKind::Aggregate) {
            return FunctionKind::Aggregate;
        }
        FunctionKind::Scalar
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinType {
    pub name: String,
    /// `numeric`, `string`, `datetime`, `boolean`, `json`, `binary`, `pseudo` and the like.
    pub category: String,
    pub versions: Versions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub name: String,
    pub data_type: Option<String>,
    pub versions: Versions,
}

/// The built-in catalog of a dialect.
#[derive(Debug, Default)]
pub struct Builtins {
    pub functions: Vec<Function>,
    by_name: HashMap<String, usize>,
    pub types: Vec<BuiltinType>,
    /// The system schemas with their tables and views; each table's columns hold their type.
    pub schemas: Vec<Schema>,
    /// The versions each system table holds for, by schema and table, in the order of `schemas`.
    pub table_versions: Vec<Vec<Versions>>,
    pub settings: Vec<Setting>,
    /// The tables of each system schema by their name in lower case.
    tables_by_name: Vec<HashMap<String, usize>>,
}

impl Builtins {
    /// The function of a name, in any case.
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.by_name
            .get(&name.to_ascii_lowercase())
            .map(|index| &self.functions[*index])
    }

    /// Whether a type of this name exists in any version, in any case.
    pub fn has_type(&self, name: &str) -> bool {
        self.types.iter().any(|known| known.name.eq_ignore_ascii_case(name))
    }

    /// The position of a table of the system schema at `schema`, by its name in any case.
    pub fn system_table(&self, schema: usize, name: &str) -> Option<usize> {
        self.tables_by_name.get(schema)?.get(&name.to_lowercase()).copied()
    }

    fn index_tables(mut self) -> Builtins {
        self.tables_by_name = self
            .schemas
            .iter()
            .map(|schema| {
                let mut found = HashMap::new();
                for (position, table) in schema.tables.iter().enumerate() {
                    found.entry(table.name.to_lowercase()).or_insert(position);
                }
                found
            })
            .collect();
        self
    }

    pub fn system_schema(&self, name: &str) -> Option<&Schema> {
        self.schemas
            .iter()
            .find(|schema| schema.name.eq_ignore_ascii_case(name))
    }
}

/// The built-in catalog of a dialect, read once. Without a dialect it is what any dialect has.
pub fn builtins(dialect: Dialect) -> &'static Builtins {
    static CATALOGS: [OnceLock<Builtins>; 5] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    let index = dialect as usize;
    CATALOGS[index].get_or_init(|| {
        match dialect {
            Dialect::Generic => union(),
            Dialect::Sqlite => read(Dialect::Sqlite, SQLITE),
            Dialect::Mysql => read(Dialect::Mysql, MYSQL),
            Dialect::Mariadb => read(Dialect::Mariadb, MARIADB),
            Dialect::Postgres => read(Dialect::Postgres, POSTGRES),
        }
        .index_tables()
    })
}

fn kind_of(code: &str) -> FunctionKind {
    match code {
        "a" => FunctionKind::Aggregate,
        "w" => FunctionKind::Window,
        "p" => FunctionKind::Procedure,
        _ => FunctionKind::Scalar,
    }
}

/// Reads `name:type` entries, `...` before a variadic one and `?` after an optional one.
fn params_of(text: &str) -> Vec<Param> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(',')
        .map(|entry| {
            let entry = entry.trim();
            let variadic = entry.starts_with("...");
            let entry = entry.trim_start_matches("...");
            let optional = entry.ends_with('?');
            let entry = entry.trim_end_matches('?');
            let (name, data_type) = entry.split_once(':').unwrap_or((entry, ""));
            Param {
                name: name.to_string(),
                data_type: (!data_type.is_empty()).then(|| data_type.to_string()),
                optional,
                variadic,
            }
        })
        .collect()
}

/// A line of `descriptions.tsv`.
struct Description {
    name: String,
    dialects: Vec<Dialect>,
    params: Option<Vec<Vec<Param>>>,
    returns: Option<String>,
    text: String,
}

fn descriptions() -> &'static [Description] {
    static PARSED: OnceLock<Vec<Description>> = OnceLock::new();
    PARSED.get_or_init(|| {
        DESCRIPTIONS
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let name = fields.next()?.to_ascii_lowercase();
                let dialects = fields.next()?;
                let params = fields.next().unwrap_or_default();
                let returns = fields.next().unwrap_or_default();
                let text = fields.next().unwrap_or_default();
                let dialects = if dialects == "*" {
                    Dialect::DATABASES.to_vec()
                } else {
                    dialects.split(',').filter_map(Dialect::parse).collect()
                };
                Some(Description {
                    name,
                    dialects,
                    params: (!params.is_empty())
                        .then(|| params.split(" | ").map(|overload| params_of(overload.trim())).collect()),
                    returns: (!returns.is_empty()).then(|| returns.to_string()),
                    text: text.to_string(),
                })
            })
            .collect()
    })
}

fn read(dialect: Dialect, data: &str) -> Builtins {
    let mut builtins = Builtins::default();
    let mut tables: Vec<(String, String, TableKind, Versions)> = Vec::new();
    let mut columns: HashMap<(String, String), Vec<Column>> = HashMap::new();
    for line in data.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let versions = Versions::parse(fields.last().copied().unwrap_or("*"));
        match fields.as_slice() {
            ["F", name, kind, params, returns, _] => {
                let overload = Overload {
                    kind: kind_of(kind),
                    params: params_of(params),
                    returns: (!returns.is_empty()).then(|| returns.to_string()),
                    versions,
                };
                match builtins.by_name.get(*name) {
                    Some(index) => builtins.functions[*index].overloads.push(overload),
                    None => {
                        builtins.by_name.insert(name.to_string(), builtins.functions.len());
                        builtins.functions.push(Function {
                            name: name.to_string(),
                            overloads: vec![overload],
                            description: None,
                            grammar: false,
                        });
                    }
                }
            }
            ["T", name, category, _] => builtins.types.push(BuiltinType {
                name: name.to_string(),
                category: category.to_string(),
                versions,
            }),
            ["R", schema, relation, kind, _] => tables.push((
                schema.to_string(),
                relation.to_string(),
                if *kind == "view" {
                    TableKind::View
                } else {
                    TableKind::System
                },
                versions,
            )),
            ["C", schema, relation, column, data_type, _] => {
                let list = columns.entry((schema.to_string(), relation.to_string())).or_default();
                if list.iter().all(|known| known.name != *column) {
                    list.push(Column {
                        name: column.to_string(),
                        data_type: (!data_type.is_empty()).then(|| data_type.to_string()),
                        ordinal: Some(list.len() as u32 + 1),
                        ..Column::default()
                    });
                }
            }
            ["V", name, data_type, _] => builtins.settings.push(Setting {
                name: name.to_string(),
                data_type: (!data_type.is_empty()).then(|| data_type.to_string()),
                versions,
            }),
            _ => {}
        }
    }
    for description in descriptions()
        .iter()
        .filter(|description| description.dialects.contains(&dialect))
    {
        let index = match builtins.by_name.get(&description.name) {
            Some(index) => *index,
            None => {
                builtins
                    .by_name
                    .insert(description.name.clone(), builtins.functions.len());
                builtins.functions.push(Function {
                    name: description.name.clone(),
                    overloads: Vec::new(),
                    description: None,
                    grammar: true,
                });
                builtins.functions.len() - 1
            }
        };
        let function = &mut builtins.functions[index];
        function.description = Some(description.text.clone());
        if let Some(signatures) = &description.params {
            let kind = function
                .overloads
                .first()
                .map_or(FunctionKind::Scalar, |overload| overload.kind);
            // The written signatures stand for every version the server's own overloads span.
            let versions = Versions::span(function.overloads.iter().map(|overload| overload.versions));
            function.overloads = signatures
                .iter()
                .map(|params| Overload {
                    kind,
                    params: params.clone(),
                    returns: description.returns.clone(),
                    versions,
                })
                .collect();
        } else if let Some(returns) = &description.returns {
            for overload in &mut function.overloads {
                if overload.returns.is_none() {
                    overload.returns = Some(returns.clone());
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..builtins.functions.len()).collect();
    order.sort_by(|a, b| builtins.functions[*a].name.cmp(&builtins.functions[*b].name));
    let functions = std::mem::take(&mut builtins.functions);
    let mut slots: Vec<Option<Function>> = functions.into_iter().map(Some).collect();
    builtins.functions = order.iter().filter_map(|index| slots[*index].take()).collect();
    builtins.by_name = builtins
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| (function.name.clone(), index))
        .collect();
    for (schema_name, table_name, kind, versions) in tables {
        let position = match builtins.schemas.iter().position(|schema| schema.name == schema_name) {
            Some(position) => position,
            None => {
                builtins.schemas.push(Schema {
                    name: schema_name.clone(),
                    ..Schema::default()
                });
                builtins.table_versions.push(Vec::new());
                builtins.schemas.len() - 1
            }
        };
        let schema = &mut builtins.schemas[position];
        if schema.tables.iter().any(|table| table.name == table_name) {
            continue;
        }
        schema.tables.push(Table {
            columns: columns.remove(&(schema_name, table_name.clone())).unwrap_or_default(),
            name: table_name,
            kind,
            ..Table::default()
        });
        builtins.table_versions[position].push(versions);
    }
    builtins
}

/// What a script without a dialect can call: every function of every dialect, the overloads of
/// the first dialect that has one.
fn union() -> Builtins {
    let mut all = Builtins::default();
    for dialect in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite, Dialect::Mariadb] {
        let one = builtins(dialect);
        for function in &one.functions {
            if all.by_name.contains_key(&function.name) {
                continue;
            }
            all.by_name.insert(function.name.clone(), all.functions.len());
            all.functions.push(Function {
                overloads: function
                    .overloads
                    .iter()
                    .map(|overload| Overload {
                        versions: Versions::ALL,
                        ..overload.clone()
                    })
                    .collect(),
                ..function.clone()
            });
        }
        for known in &one.types {
            if !all.has_type(&known.name) {
                all.types.push(BuiltinType {
                    versions: Versions::ALL,
                    ..known.clone()
                });
            }
        }
    }
    all.functions.sort_by(|a, b| a.name.cmp(&b.name));
    all.by_name = all
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| (function.name.clone(), index))
        .collect();
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dialect_reads_its_catalog() {
        for dialect in Dialect::DATABASES {
            let catalog = builtins(dialect);
            assert!(catalog.function("count").is_some(), "{dialect}: count");
            assert!(catalog.function("COUNT").is_some(), "{dialect}: any case");
            assert!(catalog.types.len() > 20, "{dialect}: types");
            assert!(!catalog.schemas.is_empty(), "{dialect}: system schemas");
            assert!(!catalog.settings.is_empty(), "{dialect}: settings");
        }
        assert!(builtins(Dialect::Postgres).system_schema("pg_catalog").is_some());
        assert!(builtins(Dialect::Mysql).system_schema("information_schema").is_some());
        let sqlite = builtins(Dialect::Sqlite).system_schema("main").expect("main");
        assert!(sqlite.tables.iter().any(|table| table.name == "sqlite_schema"));
    }

    #[test]
    fn postgres_functions_carry_their_signatures() {
        let catalog = builtins(Dialect::Postgres);
        let left = catalog.function("left").expect("left");
        let overload = &left.overloads[0];
        let labels: Vec<String> = overload.params.iter().map(Param::label).collect();
        assert_eq!(labels, ["text", "integer"]);
        assert_eq!(overload.returns.as_deref(), Some("text"));
        let concat = catalog.function("concat").expect("concat");
        assert!(concat.overloads[0].params[0].variadic);
        assert_eq!(concat.overloads[0].arity(), (0, None));
        assert_eq!(
            catalog.function("count").expect("count").kind(),
            FunctionKind::Aggregate
        );
        assert_eq!(
            catalog.function("row_number").expect("row_number").kind(),
            FunctionKind::Window
        );
        assert!(catalog.function("binary_upgrade_set_next_pg_type_oid").is_none());
        assert!(
            catalog.function("coalesce").is_some(),
            "a form of the grammar, described by hand"
        );
    }

    #[test]
    fn versions_decide_what_a_target_has() {
        let mariadb = builtins(Dialect::Mariadb);
        let uuid_v7 = mariadb.function("uuid_v7").expect("uuid_v7");
        let at = |version: &str| Target::new(Dialect::Mariadb, Version::parse(version));
        assert_eq!(uuid_v7.overloads_at(at("11.4")).count(), 0);
        assert_eq!(uuid_v7.overloads_at(at("11.8")).count(), 1);
        assert_eq!(uuid_v7.overloads_at(Target::new(Dialect::Mariadb, None)).count(), 1);
        let range = Versions::parse("-8.0");
        assert!(range.contains(Target::new(Dialect::Mysql, Version::parse("8.0.36"))));
        assert!(!range.contains(Target::new(Dialect::Mysql, Version::parse("8.4"))));
        assert!(!range.contains(Target::new(Dialect::Mysql, None)));
        let sqlite = builtins(Dialect::Sqlite);
        let sqlite_at = |version: &str| Target::new(Dialect::Sqlite, Version::parse(version));
        let condition = sqlite.function("if").expect("if");
        assert_eq!(
            condition.overloads_at(sqlite_at("3.47.2")).count(),
            0,
            "3.47 has only iif"
        );
        assert_eq!(condition.overloads_at(sqlite_at("3.48")).count(), 1);
    }

    #[test]
    fn a_function_only_the_descriptions_name_has_a_signature() {
        for description in descriptions() {
            for dialect in &description.dialects {
                let function = builtins(*dialect).function(&description.name).expect("described");
                assert!(
                    !function.grammar || description.params.is_some(),
                    "{} is not in the catalog of {dialect}: give it a signature or leave {dialect} out",
                    description.name
                );
            }
            assert!(!description.text.is_empty(), "{} has a description", description.name);
            assert!(
                !description.text.contains('\u{2014}') && !description.text.contains('\u{2013}'),
                "{}",
                description.name
            );
        }
    }

    #[test]
    fn without_a_dialect_every_function_is_known() {
        let generic = builtins(Dialect::Generic);
        assert!(generic.function("group_concat").is_some());
        assert!(generic.function("string_agg").is_some());
        assert!(generic.function("julianday").is_some());
    }
}
