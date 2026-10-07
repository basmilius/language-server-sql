//! The schema a statement is read against: the layers the server hands in (a snapshot, the DDL of
//! the workspace) under the DDL of the document itself, up to the statement at hand.

use sql_catalog::model::Table;
use sql_syntax::{SyntaxNode, Target, TextSize};

use crate::catalog::{Catalog, Layer, Origin, ScriptState};
use crate::ddl::{DdlContext, apply_statement};

/// What the server knows about the schema of a document besides the document itself.
#[derive(Clone, Copy, Default)]
pub struct Schemas<'a> {
    pub snapshot: Option<&'a Layer>,
    pub workspace: Option<&'a Layer>,
}

impl<'a> Schemas<'a> {
    pub const NONE: Schemas<'static> = Schemas {
        snapshot: None,
        workspace: None,
    };

    fn layers(&self) -> Vec<&'a Layer> {
        self.snapshot.into_iter().chain(self.workspace).collect()
    }
}

/// The DDL of a document applied in order, statement by statement.
pub struct DocumentSchema<'a> {
    pub target: Target,
    pub schemas: Schemas<'a>,
    pub layer: Layer,
    pub state: ScriptState,
}

impl<'a> DocumentSchema<'a> {
    pub fn new(target: Target, schemas: Schemas<'a>) -> DocumentSchema<'a> {
        DocumentSchema {
            target,
            schemas,
            layer: Layer::empty(Origin::Document),
            state: ScriptState::default(),
        }
    }

    /// The document's DDL before the statement at `offset`.
    pub fn before(root: &SyntaxNode, offset: u32, target: Target, schemas: Schemas<'a>) -> DocumentSchema<'a> {
        let mut schema = DocumentSchema::new(target, schemas);
        for statement in root.children() {
            if statement.text_range().end() > TextSize::from(offset) {
                break;
            }
            schema.apply(&statement);
        }
        schema
    }

    /// Takes in what a statement defines.
    pub fn apply(&mut self, statement: &SyntaxNode) {
        let below = self.schemas.layers();
        let catalog = Catalog::new(self.target, below.clone(), &self.state);
        let case = catalog.table_case;
        let base = |schema: Option<&str>, name: &str| -> Option<Table> {
            let schema = schema.map(crate::ident::Ident::new);
            let id = catalog.find_table(schema.as_ref(), &crate::ident::Ident::new(name))?;
            matches!(id.place, crate::catalog::Place::Layer(_)).then(|| catalog.table(id).clone())
        };
        let context = DdlContext {
            dialect: self.target.dialect,
            path: None,
            case,
            base: &base,
            shift: 0,
        };
        apply_statement(&mut self.layer, &mut self.state, statement, &context);
    }

    /// The catalog with the document's layer on top.
    pub fn catalog(&self) -> Catalog<'_> {
        let mut layers = vec![&self.layer];
        layers.extend(self.schemas.layers());
        Catalog::new(self.target, layers, &self.state)
    }
}

/// Whether anything defines a schema for the document: a snapshot, or DDL in the workspace or in
/// the document. Without one no name is reported as unknown.
pub fn knows_schema(catalog: &Catalog) -> bool {
    catalog
        .layers
        .iter()
        .any(|layer| layer.origin == Origin::Snapshot || !layer.is_empty())
}
