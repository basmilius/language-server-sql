//! Hover and definition: what the name at a cursor stands for, described, and where it was defined.
//! A table or column of a snapshot has no place in a file, so definition gives nothing for it
//! rather than a place that is not there.

use std::path::PathBuf;

use sql_catalog::model::{Location, TypeKind};
use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxNode, Target, TextRange, TextSize};

use crate::ast::{alias_of, child, compact, inner_query, parts};
use crate::catalog::{Catalog, Place as CatalogPlace};
use crate::context::{DocumentSchema, Schemas};
use crate::render;
use crate::resolve::{ColumnOrigin, Referent, Resolution, Resolver, Source, SourceKind};

/// Where something is defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The file; `None` is the document asked about.
    pub path: Option<PathBuf>,
    /// The whole definition.
    pub range: TextRange,
    /// Its name.
    pub name: TextRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    /// The name hovered.
    pub range: TextRange,
    pub markdown: String,
}

/// The `NAME` at an offset: the one the offset is in, or the one it is right after.
fn name_at(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    let at = TextSize::from(offset);
    let tokens: Vec<_> = root.token_at_offset(at).collect();
    tokens
        .iter()
        .rev()
        .chain(tokens.iter())
        .find_map(|token| token.parent().filter(|parent| parent.kind() == NAME))
}

fn place_of(location: &Location) -> Place {
    Place {
        path: location.path.clone(),
        range: TextRange::new(location.range.0.into(), location.range.1.into()),
        name: TextRange::new(location.name.0.into(), location.name.1.into()),
    }
}

fn local(node: &SyntaxNode, name: &SyntaxNode) -> Place {
    Place {
        path: None,
        range: node.text_range(),
        name: name.text_range(),
    }
}

/// Describes and places what the name at an offset stands for.
struct Navigator<'c, 'a> {
    catalog: &'c Catalog<'a>,
    resolver: Resolver<'c, 'a>,
    target: Target,
}

/// What the name at an offset is, in markdown.
pub fn hover(root: &SyntaxNode, offset: u32, target: Target, schemas: Schemas) -> Option<Hover> {
    let name = name_at(root, offset)?;
    let document = DocumentSchema::before(root, offset, target, schemas);
    let catalog = document.catalog();
    let navigator = Navigator {
        catalog: &catalog,
        resolver: Resolver::new(&catalog),
        target,
    };
    let markdown = match navigator.resolver.resolve_name(&name) {
        Some(Resolution::Found(referent)) => navigator.describe(&referent)?,
        Some(Resolution::Ambiguous(all)) => {
            let mut out = String::from("**Ambiguous**: it can be\n");
            for referent in &all {
                if let Referent::Column { source, column } = referent {
                    out.push_str(&format!("\n- `{}.{}`", source.name.text, column.name));
                }
            }
            out
        }
        _ => navigator.describe_type(&name)?,
    };
    Some(Hover {
        range: name.text_range(),
        markdown,
    })
}

/// Where what the name at an offset stands for is defined.
pub fn definition(root: &SyntaxNode, offset: u32, target: Target, schemas: Schemas) -> Vec<Place> {
    let Some(name) = name_at(root, offset) else {
        return Vec::new();
    };
    let document = DocumentSchema::before(root, offset, target, schemas);
    let catalog = document.catalog();
    let navigator = Navigator {
        catalog: &catalog,
        resolver: Resolver::new(&catalog),
        target,
    };
    match navigator.resolver.resolve_name(&name) {
        Some(Resolution::Found(referent)) => navigator.places(&referent),
        Some(Resolution::Ambiguous(all)) => all.iter().flat_map(|referent| navigator.places(referent)).collect(),
        _ => navigator.type_place(&name).into_iter().collect(),
    }
}

impl Navigator<'_, '_> {
    fn table_title(&self, id: crate::catalog::TableId) -> String {
        let schema = self.catalog.schema_name(id.place, id.schema);
        let table = self.catalog.table(id);
        if schema.is_empty() {
            table.name.clone()
        } else {
            format!("{schema}.{}", table.name)
        }
    }

    fn describe(&self, referent: &Referent) -> Option<String> {
        Some(match referent {
            Referent::Table(id) => self.describe_table(*id),
            Referent::Cte(cte) => describe_cte(cte),
            Referent::Source(source) => self.describe_source(source),
            Referent::Column { source, column } => self.describe_column(source, column),
            Referent::SelectAlias(item) => {
                let (alias, _) = alias_of(item, self.target.dialect)?;
                let expression = item.children().next().map(|inner| compact(&inner)).unwrap_or_default();
                format!("**alias** `{}`\n\n```sql\n{expression}\n```", alias.ident.text)
            }
            Referent::Variable(name) => {
                let declaration = name.parent()?;
                let data_type = child(&declaration, TYPE).map(|data_type| compact(&data_type));
                let kind = if declaration.kind() == PARAM_DEF {
                    "parameter"
                } else {
                    "variable"
                };
                let mut out = format!("**{kind}** `{}`", compact(name));
                if let Some(data_type) = data_type {
                    out.push_str(&format!("\n\n```sql\n{} {data_type}\n```", compact(name)));
                }
                out
            }
            Referent::Schema(name) => {
                let label = match self.target.dialect {
                    sql_syntax::Dialect::Mysql | sql_syntax::Dialect::Mariadb => "database",
                    _ => "schema",
                };
                let count = self.catalog.tables(Some(name)).len();
                format!("**{label}** `{name}`\n\n{count} tables and views")
            }
            Referent::Function(name) => self.describe_function(name)?,
            Referent::Routines(ids) => {
                let routines: Vec<_> = ids.iter().map(|id| self.catalog.routine_at(*id)).collect();
                let first = routines.first()?;
                let mut out = format!("**{}** `{}`\n\n```sql\n", first.kind.label(), first.name);
                for routine in &routines {
                    out.push_str(&render::routine_signature(routine));
                    out.push('\n');
                }
                out.push_str("```");
                if let Some(comment) = routines.iter().find_map(|routine| routine.comment.clone()) {
                    out.push_str(&format!("\n\n{comment}"));
                }
                if let Some(language) = &first.language {
                    out.push_str(&format!("\n\nLanguage: {language}"));
                }
                out
            }
        })
    }

    fn describe_table(&self, id: crate::catalog::TableId) -> String {
        let table = self.catalog.table(id);
        let mut out = format!("**{}** `{}`", table.kind.label(), self.table_title(id));
        if let Some(comment) = &table.comment {
            out.push_str(&format!("\n\n{comment}"));
        }
        if table.kind.is_view() {
            if let Some(definition) = &table.definition {
                out.push_str(&format!("\n\n```sql\n{definition}\n```"));
            }
        }
        let columns = render::table_markdown(table);
        if !columns.is_empty() {
            out.push_str("\n\n");
            out.push_str(&columns);
        }
        if table.open {
            out.push_str("\n\nIts columns are not all known.");
        }
        out
    }

    fn describe_source(&self, source: &Source) -> String {
        match &source.kind {
            SourceKind::Table(id) if source.aliased => format!(
                "**alias** `{}` for {} `{}`",
                source.name.text,
                self.catalog.table(*id).kind.label(),
                self.table_title(*id)
            ),
            SourceKind::Table(id) => self.describe_table(*id),
            SourceKind::Cte(cte) if source.aliased => {
                format!("**alias** `{}` for\n\n{}", source.name.text, describe_cte(cte))
            }
            SourceKind::Cte(cte) => describe_cte(cte),
            SourceKind::Derived(query) => format!(
                "**subquery** `{}`\n\n```sql\n{}\n```",
                source.name.text,
                shorten(&compact(query))
            ),
            SourceKind::Function(node) => format!(
                "**table function** `{}`\n\n```sql\n{}\n```",
                source.name.text,
                shorten(&compact(node))
            ),
            SourceKind::Defined(_) => format!("**table** `{}`, being defined", source.name.text),
            SourceKind::Unknown => format!("**alias** `{}` for a table that is not known", source.name.text),
        }
    }

    fn describe_column(&self, source: &Source, column: &crate::resolve::OutputColumn) -> String {
        match &column.origin {
            ColumnOrigin::Table(id, position) => {
                let definition = &self.catalog.table(*id).columns[*position];
                let mut out = format!(
                    "**column** of {} `{}`\n\n```sql\n{}\n```",
                    self.catalog.table(*id).kind.label(),
                    self.table_title(*id),
                    render::column_line(definition)
                );
                if let Some(comment) = &definition.comment {
                    out.push_str(&format!("\n\n{comment}"));
                }
                let table = self.catalog.table(*id);
                if table
                    .primary_key
                    .as_ref()
                    .is_some_and(|key| key.columns.iter().any(|name| name == &definition.name))
                {
                    out.push_str("\n\nPart of the primary key.");
                }
                for key in table
                    .foreign_keys
                    .iter()
                    .filter(|key| key.columns.iter().any(|name| name == &definition.name))
                {
                    out.push_str(&format!(
                        "\n\nReferences `{}`{}.",
                        key.referenced_table,
                        if key.referenced_columns.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", key.referenced_columns.join(", "))
                        }
                    ));
                }
                out
            }
            ColumnOrigin::Item(item) => {
                let expression = item.children().next().map(|inner| compact(&inner)).unwrap_or_default();
                format!(
                    "**column** `{}` of `{}`\n\n```sql\n{}\n```",
                    column.name,
                    source.name.text,
                    shorten(&expression)
                )
            }
            ColumnOrigin::Declared(name) => {
                let data_type = name
                    .parent()
                    .filter(|parent| parent.kind() == COLUMN_DEF)
                    .and_then(|definition| child(&definition, TYPE))
                    .map(|data_type| compact(&data_type));
                let mut out = format!("**column** `{}` of `{}`", column.name, source.name.text);
                if let Some(data_type) = data_type {
                    out.push_str(&format!("\n\n```sql\n{} {data_type}\n```", column.name));
                }
                out
            }
            ColumnOrigin::Implicit => format!(
                "**column** `{}` of `{}`, which the database gives every row",
                column.name, source.name.text
            ),
        }
    }

    fn describe_function(&self, name: &str) -> Option<String> {
        let function = self.catalog.builtins.function(name)?;
        let overloads: Vec<_> = function.overloads_at(self.target).collect();
        let shown = if overloads.is_empty() {
            function.overloads.iter().collect()
        } else {
            overloads
        };
        let mut out = format!("**{}** `{}`\n\n```sql\n", function.kind().label(), function.name);
        for overload in shown.iter().take(12) {
            out.push_str(&render::overload_signature(&function.name, overload));
            out.push('\n');
        }
        if shown.len() > 12 {
            out.push_str(&format!("-- and {} more\n", shown.len() - 12));
        }
        out.push_str("```");
        if let Some(description) = &function.description {
            out.push_str(&format!("\n\n{description}"));
        }
        Some(out)
    }

    /// A type a definition names: an enum, domain or composite of the schema.
    fn describe_type(&self, name: &SyntaxNode) -> Option<String> {
        let (_, user_type) = self.user_type_at(name)?;
        let mut out = format!("**{}** `{}`", user_type.kind.label(), user_type.name);
        match user_type.kind {
            TypeKind::Enum => {
                let values: Vec<String> = user_type.values.iter().map(|value| format!("'{value}'")).collect();
                out.push_str(&format!("\n\n```sql\n{}\n```", values.join(", ")));
            }
            TypeKind::Domain => {
                if let Some(base) = &user_type.base_type {
                    out.push_str(&format!("\n\nBased on `{base}`"));
                    if user_type.nullable == Some(false) {
                        out.push_str(", not null");
                    }
                }
            }
            TypeKind::Composite => {
                for attribute in &user_type.attributes {
                    out.push_str(&format!(
                        "\n- `{}` {}",
                        attribute.name,
                        attribute.data_type.as_deref().unwrap_or("")
                    ));
                }
            }
            _ => {}
        }
        if let Some(comment) = &user_type.comment {
            out.push_str(&format!("\n\n{comment}"));
        }
        Some(out)
    }

    fn user_type_at(&self, name: &SyntaxNode) -> Option<(crate::catalog::ObjectId, &sql_catalog::model::UserType)> {
        let qualified = name.parent().filter(|parent| parent.kind() == QUALIFIED_NAME)?;
        qualified.parent().filter(|owner| owner.kind() == TYPE)?;
        let all = parts(&qualified, self.target.dialect);
        let last = all.last()?;
        let schema = (all.len() >= 2).then(|| all[all.len() - 2].ident.clone());
        self.catalog.user_type(schema.as_ref(), &last.ident.text)
    }

    fn type_place(&self, name: &SyntaxNode) -> Option<Place> {
        let (_, user_type) = self.user_type_at(name)?;
        user_type.location.as_ref().map(place_of)
    }

    fn table_place(&self, id: crate::catalog::TableId) -> Option<Place> {
        if id.place == CatalogPlace::System {
            return None;
        }
        self.catalog.table(id).location.as_ref().map(place_of)
    }

    fn places(&self, referent: &Referent) -> Vec<Place> {
        let place = match referent {
            Referent::Table(id) => self.table_place(*id),
            Referent::Cte(cte) => child(cte, NAME).map(|name| local(cte, &name)),
            Referent::Source(source) => match (&source.kind, source.aliased) {
                (SourceKind::Table(id), false) => self.table_place(*id),
                (SourceKind::Cte(cte), false) => child(cte, NAME).map(|name| local(cte, &name)),
                _ => source.name_node.as_ref().map(|name| local(&source.node, name)),
            },
            Referent::Column { column, .. } => match &column.origin {
                ColumnOrigin::Table(id, position) => {
                    if id.place == CatalogPlace::System {
                        None
                    } else {
                        self.catalog.table(*id).columns[*position]
                            .location
                            .as_ref()
                            .map(place_of)
                    }
                }
                ColumnOrigin::Item(item) => Some(match alias_of(item, self.target.dialect) {
                    Some((alias, _)) => local(item, &alias.node),
                    None => local(item, item),
                }),
                ColumnOrigin::Declared(name) => Some(local(&name.parent().unwrap_or_else(|| name.clone()), name)),
                ColumnOrigin::Implicit => None,
            },
            Referent::SelectAlias(item) => {
                alias_of(item, self.target.dialect).map(|(alias, _)| local(item, &alias.node))
            }
            Referent::Variable(name) => Some(local(&name.parent().unwrap_or_else(|| name.clone()), name)),
            Referent::Schema(name) => self
                .catalog
                .layers
                .iter()
                .flat_map(|layer| layer.snapshot.schemas.iter())
                .find(|schema| self.catalog.table_case.eq(&schema.name, name))
                .and_then(|schema| schema.location.as_ref())
                .map(place_of),
            Referent::Function(_) => None,
            Referent::Routines(ids) => {
                return ids
                    .iter()
                    .filter_map(|id| self.catalog.routine_at(*id).location.as_ref().map(place_of))
                    .collect();
            }
        };
        place.into_iter().collect()
    }
}

fn describe_cte(cte: &SyntaxNode) -> String {
    let name = child(cte, NAME).map(|name| compact(&name)).unwrap_or_default();
    let query = inner_query(cte)
        .or_else(|| cte.children().find(|inner| !matches!(inner.kind(), NAME | NAME_LIST)))
        .map(|query| query.text().to_string())
        .unwrap_or_default();
    format!(
        "**common table expression** `{name}`\n\n```sql\n{}\n```",
        shorten(query.trim())
    )
}

/// A long text cut down to its first lines.
fn shorten(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 20 {
        format!("{}\n...", lines[..20].join("\n"))
    } else if text.len() > 2000 {
        let cut = text
            .char_indices()
            .take_while(|(index, _)| *index < 2000)
            .last()
            .map_or(0, |(index, character)| index + character.len_utf8());
        format!("{}...", &text[..cut])
    } else {
        text.to_string()
    }
}
