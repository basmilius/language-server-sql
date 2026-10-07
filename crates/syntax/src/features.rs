//! Placeholder, replaced below.
use rowan::TextRange;

use crate::{Dialect, SyntaxElement, SyntaxKind, SyntaxNode, Target, Version};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Always,
    Since(Version),
    Never,
    DeprecatedSince(Version),
}

pub struct Feature {
    pub id: &'static str,
    pub name: &'static str,
    pub plural: bool,
    pub support: [Support; 4],
    pub kinds: &'static [SyntaxKind],
    pub detect: fn(&SyntaxElement) -> Option<TextRange>,
    pub example: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureDiagnostic {
    pub range: TextRange,
    pub message: String,
    pub severity: FeatureSeverity,
    pub deprecated: bool,
    pub feature: &'static str,
}

pub static FEATURES: &[Feature] = &[];

pub fn check_features(_root: &SyntaxNode, _target: Target) -> Vec<FeatureDiagnostic> {
    let _ = Dialect::Generic;
    Vec::new()
}
