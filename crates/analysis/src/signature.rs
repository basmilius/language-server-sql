//! Signature help: the parameters of the function or procedure a cursor is calling, every way it
//! can be called at the dialect and version, and the parameter under the cursor.

use sql_catalog::Param;
use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxElement, SyntaxNode, Target, TextSize};

use crate::ast::{child, parts};
use crate::context::{DocumentSchema, Schemas};
use crate::render;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterItem {
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureItem {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<ParameterItem>,
    /// The parameter the cursor is in for this way of calling the function; a variadic last
    /// parameter takes every argument after it.
    pub active_parameter: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureItem>,
    pub active_signature: usize,
}

/// The argument list the offset is in: after its `(` and not past its `)`.
fn argument_list_at(root: &SyntaxNode, offset: u32) -> Option<SyntaxNode> {
    let at = TextSize::from(offset);
    let mut token = root
        .token_at_offset(at)
        .left_biased()
        .or_else(|| root.token_at_offset(at).right_biased())?;
    while token.kind().is_trivia() || token.text_range().start() >= at {
        token = token.prev_token()?;
    }
    token.parent_ancestors().find(|node| {
        if node.kind() != ARG_LIST {
            return false;
        }
        let closed = node.last_token().is_some_and(|last| last.kind() == RPAREN);
        !closed || at < node.text_range().end()
    })
}

/// How many commas of the list itself stand before the offset.
fn argument_position(list: &SyntaxNode, offset: u32) -> usize {
    list.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| token.kind() == COMMA && token.text_range().end() <= TextSize::from(offset))
        .count()
}

fn active(params: &[Param], position: usize) -> Option<usize> {
    if position < params.len() {
        return Some(position);
    }
    params.iter().position(|param| param.variadic).or(Some(position))
}

/// The signatures of the call around the offset.
pub fn signature_help(root: &SyntaxNode, offset: u32, target: Target, schemas: Schemas) -> Option<SignatureHelp> {
    let list = argument_list_at(root, offset)?;
    let call = list.parent()?;
    let name = match call.kind() {
        FUNCTION_CALL | CALL_STMT => child(&call, QUALIFIED_NAME)?,
        _ => return None,
    };
    let all = parts(&name, target.dialect);
    let function = all.last()?;
    let schema = (all.len() >= 2).then(|| all[all.len() - 2].ident.clone());
    let position = argument_position(&list, offset);
    let document = DocumentSchema::before(root, offset, target, schemas);
    let catalog = document.catalog();
    let mut signatures = Vec::new();
    for (_, routine) in catalog.routines(schema.as_ref(), &function.ident.text) {
        let parameters: Vec<ParameterItem> = routine
            .parameters
            .iter()
            .filter(|parameter| !matches!(parameter.mode, Some(sql_catalog::model::ParameterMode::Out)))
            .map(|parameter| ParameterItem {
                label: render::routine_param(parameter),
            })
            .collect();
        let count = parameters.len();
        signatures.push((
            SignatureItem {
                label: render::routine_signature(routine),
                documentation: routine.comment.clone(),
                active_parameter: Some(position),
                parameters,
            },
            count,
            false,
        ));
    }
    if signatures.is_empty() && call.kind() == FUNCTION_CALL {
        if let Some(builtin) = catalog.builtins.function(&function.ident.text) {
            let mut overloads: Vec<_> = builtin.overloads_at(target).collect();
            if overloads.is_empty() {
                overloads = builtin.overloads.iter().collect();
            }
            for overload in overloads {
                let label_name = if target.dialect == sql_syntax::Dialect::Postgres {
                    builtin.name.clone()
                } else {
                    function.ident.text.clone()
                };
                signatures.push((
                    SignatureItem {
                        label: render::overload_signature(&label_name, overload),
                        documentation: builtin.description.clone(),
                        active_parameter: active(&overload.params, position),
                        parameters: overload
                            .params
                            .iter()
                            .map(|param| ParameterItem { label: param.label() })
                            .collect(),
                    },
                    overload.params.len(),
                    overload.params.iter().any(|param| param.variadic),
                ));
            }
        }
    }
    if signatures.is_empty() {
        return None;
    }
    let active_signature = signatures
        .iter()
        .position(|(_, count, variadic)| *variadic || position < *count)
        .unwrap_or(0);
    Some(SignatureHelp {
        signatures: signatures.into_iter().map(|(signature, _, _)| signature).collect(),
        active_signature,
    })
}
