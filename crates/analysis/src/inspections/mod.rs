//! Inspections: what a database IDE flags in a script, each with a stable id, a default severity,
//! a switch and, where an obvious one exists, a quick fix. An inspection that cannot be sure stays
//! silent: one that needs the schema says nothing without one, and one about a dialect says nothing
//! in another, so what is reported is something to look at and not a guess.
//!
//! Severities follow what the server does: an error only where the server rejects the statement,
//! a warning for a likely bug, information or a hint for style. A setting changes either per id.
//! A comment `-- sql-suppress <id> ...` before or in a statement silences ids for that statement,
//! and `-- sql-suppress-file <id> ...` for the whole script; `all` stands for every id.

mod conditions;
mod grouping;
mod literals;
mod names;
mod pitfalls;
mod suppress;
mod syntax;
mod tree;
mod unused;
mod writes;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::BTreeMap;

use sql_syntax::SyntaxKind::*;
use sql_syntax::{Dialect, SyntaxNode, Target, TextRange};

use crate::catalog::{Catalog, ScriptState};
use crate::context::{DocumentSchema, Schemas, knows_schema};
use crate::diagnostics::{Diagnostic, DiagnosticSeverity, Related};
use crate::rename::TextEdit;
use crate::resolve::Resolver;
use crate::sql_mode::SqlMode;

pub(crate) use names::qualifier_of;
pub use suppress::{FILE_DIRECTIVE, STATEMENT_DIRECTIVE, suppress_fixes};

/// What an inspection is, apart from where it found something.
#[derive(Clone, Copy, Debug)]
pub struct InspectionInfo {
    pub id: &'static str,
    /// The severity where the finding gives none of its own; some say per dialect whether the
    /// server rejects what they find.
    pub severity: DiagnosticSeverity,
    pub enabled: bool,
    /// Whether its fix may be applied to every finding of a script at once: the fix is the only
    /// one and keeps what the statement was meant to do.
    pub fix_all: bool,
    pub summary: &'static str,
}

const fn info(id: &'static str, severity: DiagnosticSeverity, fix_all: bool, summary: &'static str) -> InspectionInfo {
    InspectionInfo {
        id,
        severity,
        enabled: true,
        fix_all,
        summary,
    }
}

use DiagnosticSeverity::{Error, Hint, Information, Warning};

pub const UNRESOLVED_TABLE: &str = "unresolved-table";
pub const UNRESOLVED_COLUMN: &str = "unresolved-column";
pub const UNRESOLVED_FUNCTION: &str = "unresolved-function";
pub const AMBIGUOUS_COLUMN: &str = "ambiguous-column";
pub const UNSUPPORTED_SYNTAX: &str = "unsupported-syntax";
pub const DEPRECATED_SYNTAX: &str = "deprecated-syntax";
pub const RESERVED_WORD: &str = "reserved-word";
pub const MISSING_WHERE: &str = "missing-where";
pub const NULL_COMPARISON: &str = "null-comparison";
pub const LIKE_WITHOUT_WILDCARD: &str = "like-without-wildcard";
pub const IMPLICIT_CROSS_JOIN: &str = "implicit-cross-join";
pub const NOT_IN_NULLABLE: &str = "not-in-nullable";
pub const NONAGGREGATED_COLUMN: &str = "nonaggregated-column";
pub const DISTINCT_WITH_GROUP_BY: &str = "distinct-with-group-by";
pub const COUNT_NOT_NULL_COLUMN: &str = "count-not-null-column";
pub const INSERT_COLUMN_COUNT: &str = "insert-column-count";
pub const SET_OPERATION_COLUMN_COUNT: &str = "set-operation-column-count";
pub const INVALID_LITERAL: &str = "invalid-literal";
pub const UNKNOWN_ENUM_VALUE: &str = "unknown-enum-value";
pub const NOT_NULL_VIOLATION: &str = "not-null-violation";
pub const GENERATED_COLUMN_WRITE: &str = "generated-column-write";
pub const MISSING_REQUIRED_COLUMN: &str = "missing-required-column";
pub const UNUSED_CTE: &str = "unused-cte";
pub const UNUSED_ALIAS: &str = "unused-alias";
pub const DUPLICATE_ALIAS: &str = "duplicate-alias";
pub const DUPLICATE_CTE: &str = "duplicate-cte";
pub const DUPLICATE_COLUMN: &str = "duplicate-column";
pub const PIPES_AS_OR: &str = "pipes-as-or";
pub const DOUBLE_QUOTED_STRING: &str = "double-quoted-string";
pub const LIMIT_IN_SUBQUERY: &str = "limit-in-subquery";
pub const ORDER_BY_IN_SUBQUERY: &str = "order-by-in-subquery";

/// Every inspection, in the order `docs/inspections.md` lists them.
pub const INSPECTIONS: &[InspectionInfo] = &[
    info(
        UNRESOLVED_TABLE,
        Error,
        false,
        "A table, view or qualifier the schema does not have",
    ),
    info(UNRESOLVED_COLUMN, Error, false, "A column no table in scope has"),
    info(
        UNRESOLVED_FUNCTION,
        Error,
        false,
        "A function that is neither built in nor in the schema",
    ),
    info(
        AMBIGUOUS_COLUMN,
        Error,
        false,
        "An unqualified column that more than one table in scope has",
    ),
    info(
        UNSUPPORTED_SYNTAX,
        Error,
        true,
        "Syntax the dialect or its version does not have",
    ),
    info(DEPRECATED_SYNTAX, Warning, true, "Syntax the version deprecates"),
    info(
        RESERVED_WORD,
        Error,
        true,
        "A reserved word used as a name without quotes",
    ),
    info(
        MISSING_WHERE,
        Warning,
        false,
        "A DELETE or UPDATE without WHERE, which changes every row",
    ),
    info(
        NULL_COMPARISON,
        Warning,
        true,
        "A comparison with NULL through =, <> or !=, which is never true",
    ),
    info(
        NOT_IN_NULLABLE,
        Information,
        false,
        "NOT IN over a subquery of a column that may be NULL",
    ),
    info(
        IMPLICIT_CROSS_JOIN,
        Warning,
        true,
        "Tables joined by a comma that no condition links",
    ),
    info(
        LIKE_WITHOUT_WILDCARD,
        Information,
        true,
        "LIKE with a pattern that has no wildcard",
    ),
    info(
        NONAGGREGATED_COLUMN,
        Error,
        false,
        "A column neither grouped nor aggregated where the server requires one",
    ),
    info(
        DISTINCT_WITH_GROUP_BY,
        Information,
        true,
        "DISTINCT over a select list that GROUP BY already makes unique",
    ),
    info(
        COUNT_NOT_NULL_COLUMN,
        Hint,
        true,
        "COUNT of a column that cannot be NULL, which counts every row",
    ),
    info(
        INSERT_COLUMN_COUNT,
        Error,
        false,
        "An INSERT with more or fewer values than columns",
    ),
    info(
        SET_OPERATION_COLUMN_COUNT,
        Error,
        false,
        "Queries of UNION, INTERSECT or EXCEPT with different numbers of columns",
    ),
    info(
        INVALID_LITERAL,
        Error,
        false,
        "A string a numeric or date column cannot read",
    ),
    info(
        UNKNOWN_ENUM_VALUE,
        Error,
        false,
        "A value that is not in the column's enum",
    ),
    info(
        NOT_NULL_VIOLATION,
        Error,
        false,
        "NULL written to a column that is NOT NULL",
    ),
    info(
        GENERATED_COLUMN_WRITE,
        Error,
        true,
        "A value written to a generated column",
    ),
    info(
        MISSING_REQUIRED_COLUMN,
        Error,
        false,
        "An INSERT that leaves out a column that needs a value",
    ),
    info(UNUSED_CTE, Warning, true, "A common table expression nothing reads"),
    info(UNUSED_ALIAS, Hint, true, "A table alias nothing qualifies a name with"),
    info(
        DUPLICATE_ALIAS,
        Error,
        false,
        "Two tables of one FROM under the same name",
    ),
    info(
        DUPLICATE_CTE,
        Error,
        false,
        "Two common table expressions of one WITH with the same name",
    ),
    info(
        DUPLICATE_COLUMN,
        Error,
        false,
        "Two columns of a table or view with the same name",
    ),
    info(
        PIPES_AS_OR,
        Warning,
        true,
        "|| meant as concatenation where it is a logical OR",
    ),
    info(
        DOUBLE_QUOTED_STRING,
        Hint,
        true,
        "A string in double quotes, which means a name under ANSI_QUOTES",
    ),
    info(
        LIMIT_IN_SUBQUERY,
        Error,
        true,
        "LIMIT in a subquery of IN, ANY, SOME or ALL",
    ),
    info(
        ORDER_BY_IN_SUBQUERY,
        Information,
        true,
        "ORDER BY in a subquery without LIMIT, which the server ignores",
    ),
];

pub fn inspection_info(id: &str) -> Option<&'static InspectionInfo> {
    INSPECTIONS.iter().find(|info| info.id == id)
}

/// What a client chose for one inspection, or for one row of the feature table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Override {
    pub enabled: Option<bool>,
    pub severity: Option<DiagnosticSeverity>,
}

/// The switches and severities a client configured, by inspection id or by the id of a row of the
/// feature table. What is not named keeps its default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InspectionSettings {
    overrides: BTreeMap<String, Override>,
}

impl InspectionSettings {
    pub fn set(&mut self, id: &str, choice: Override) {
        self.overrides.insert(id.to_string(), choice);
    }

    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    fn choice(&self, id: &str) -> Override {
        self.overrides.get(id).copied().unwrap_or_default()
    }

    /// The severity a finding is reported at, or `None` while its inspection is switched off. A
    /// row of the feature table goes before its inspection.
    pub fn severity_of(
        &self,
        info: &InspectionInfo,
        feature: Option<&str>,
        default: DiagnosticSeverity,
    ) -> Option<DiagnosticSeverity> {
        let general = self.choice(info.id);
        let specific = feature.map(|feature| self.choice(feature)).unwrap_or_default();
        if !specific.enabled.or(general.enabled).unwrap_or(info.enabled) {
            return None;
        }
        Some(specific.severity.or(general.severity).unwrap_or(default))
    }

    /// Whether an inspection may report anything: it is on, or a row of it was switched on.
    fn may_report(&self, info: &InspectionInfo) -> bool {
        if self.choice(info.id).enabled.unwrap_or(info.enabled) {
            return true;
        }
        matches!(info.id, UNSUPPORTED_SYNTAX | DEPRECATED_SYNTAX)
            && self.overrides.values().any(|choice| choice.enabled == Some(true))
    }
}

/// An edit that mends a finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickFix {
    pub title: String,
    pub edits: Vec<TextEdit>,
}

/// A diagnostic with the fixes that mend it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub diagnostic: Diagnostic,
    pub fixes: Vec<QuickFix>,
}

/// What an inspection run reads and how much of it to do.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub target: Target,
    pub schemas: Schemas<'a>,
    pub settings: &'a InspectionSettings,
    /// Only the statements this range touches, and the findings in it.
    pub range: Option<TextRange>,
    /// Whether to work out the fixes, which diagnostics alone do not need.
    pub fixes: bool,
    /// Only the inspection of this id.
    pub only: Option<&'static str>,
}

impl<'a> Request<'a> {
    pub fn new(target: Target, schemas: Schemas<'a>, settings: &'a InspectionSettings) -> Request<'a> {
        Request {
            target,
            schemas,
            settings,
            range: None,
            fixes: false,
            only: None,
        }
    }
}

/// What every inspection of a run shares: the script, what the settings say and where findings go.
pub(crate) struct Cx<'a> {
    pub target: Target,
    pub root: SyntaxNode,
    request: &'a Request<'a>,
    on: Vec<&'static str>,
    found: RefCell<Vec<Finding>>,
}

/// A finding on its way out, which takes a severity, related places and fixes before `emit`.
#[must_use]
pub(crate) struct Pending<'c, 'a> {
    cx: &'c Cx<'a>,
    id: &'static str,
    range: TextRange,
    message: String,
    severity: Option<DiagnosticSeverity>,
    feature: Option<&'static str>,
    related: Vec<Related>,
    fixes: Vec<QuickFix>,
}

impl Pending<'_, '_> {
    /// The severity of this finding where it differs from its inspection's default, such as an
    /// error in the dialect that rejects it and a warning in one that does not.
    pub fn severity(mut self, severity: DiagnosticSeverity) -> Self {
        self.severity = Some(severity);
        self
    }

    pub fn feature(mut self, feature: &'static str) -> Self {
        self.feature = Some(feature);
        self
    }

    pub fn related(mut self, range: TextRange, message: impl Into<String>) -> Self {
        self.related.push(Related {
            range,
            message: message.into(),
        });
        self
    }

    /// One fix, made only when the run asks for fixes.
    pub fn fix(mut self, title: impl Into<String>, edits: impl FnOnce() -> Option<Vec<TextEdit>>) -> Self {
        if self.cx.request.fixes {
            if let Some(edits) = edits() {
                self.fixes.push(QuickFix {
                    title: title.into(),
                    edits,
                });
            }
        }
        self
    }

    /// Several fixes, made only when the run asks for fixes.
    pub fn fixes(mut self, fixes: impl FnOnce() -> Vec<QuickFix>) -> Self {
        if self.cx.request.fixes {
            self.fixes.extend(fixes());
        }
        self
    }

    pub fn emit(self) {
        let Some(info) = inspection_info(self.id) else {
            return;
        };
        let default = self.severity.unwrap_or(info.severity);
        let Some(severity) = self.cx.request.settings.severity_of(info, self.feature, default) else {
            return;
        };
        self.cx.found.borrow_mut().push(Finding {
            diagnostic: Diagnostic {
                range: self.range,
                message: self.message,
                severity,
                code: self.id,
                deprecated: self.id == DEPRECATED_SYNTAX,
                unnecessary: matches!(
                    self.id,
                    UNUSED_CTE | UNUSED_ALIAS | DISTINCT_WITH_GROUP_BY | ORDER_BY_IN_SUBQUERY
                ),
                feature: self.feature,
                related: self.related,
            },
            fixes: self.fixes,
        });
    }
}

impl<'a> Cx<'a> {
    fn new(root: &SyntaxNode, request: &'a Request<'a>) -> Cx<'a> {
        let on = INSPECTIONS
            .iter()
            .filter(|info| request.only.is_none_or(|only| only == info.id))
            .filter(|info| request.settings.may_report(info))
            .map(|info| info.id)
            .collect();
        Cx {
            target: request.target,
            root: root.clone(),
            request,
            on,
            found: RefCell::new(Vec::new()),
        }
    }

    pub fn dialect(&self) -> Dialect {
        self.target.dialect
    }

    pub fn on(&self, id: &str) -> bool {
        self.on.contains(&id)
    }

    pub fn fixes(&self) -> bool {
        self.request.fixes
    }

    pub fn wants(&self, range: TextRange) -> bool {
        self.request
            .range
            .is_none_or(|wanted| wanted.intersect(range).is_some())
    }

    pub fn report(&self, id: &'static str, range: TextRange, message: impl Into<String>) -> Pending<'_, 'a> {
        Pending {
            cx: self,
            id,
            range,
            message: message.into(),
            severity: None,
            feature: None,
            related: Vec::new(),
            fixes: Vec::new(),
        }
    }
}

/// One statement of the script with what it is read against.
pub(crate) struct Stmt<'s, 'c, 'a> {
    pub node: SyntaxNode,
    pub catalog: &'c Catalog<'a>,
    pub resolver: &'s Resolver<'c, 'a>,
    pub state: &'c ScriptState,
    /// Whether anything defines a schema; without one nothing that needs it is reported.
    pub known: bool,
    pub mode: SqlMode,
    /// Whether the statement parsed without an error; a broken one is left to its error.
    pub clean: bool,
}

/// The inspections that read statements one by one, against the schema before each.
const PER_STATEMENT: &[&str] = &[
    UNRESOLVED_TABLE,
    UNRESOLVED_COLUMN,
    UNRESOLVED_FUNCTION,
    AMBIGUOUS_COLUMN,
    MISSING_WHERE,
    NULL_COMPARISON,
    NOT_IN_NULLABLE,
    IMPLICIT_CROSS_JOIN,
    LIKE_WITHOUT_WILDCARD,
    NONAGGREGATED_COLUMN,
    DISTINCT_WITH_GROUP_BY,
    COUNT_NOT_NULL_COLUMN,
    INSERT_COLUMN_COUNT,
    SET_OPERATION_COLUMN_COUNT,
    INVALID_LITERAL,
    UNKNOWN_ENUM_VALUE,
    NOT_NULL_VIOLATION,
    GENERATED_COLUMN_WRITE,
    MISSING_REQUIRED_COLUMN,
    UNUSED_CTE,
    UNUSED_ALIAS,
    DUPLICATE_ALIAS,
    DUPLICATE_CTE,
    DUPLICATE_COLUMN,
    PIPES_AS_OR,
    DOUBLE_QUOTED_STRING,
    LIMIT_IN_SUBQUERY,
    ORDER_BY_IN_SUBQUERY,
];

/// Runs the inspections the settings leave on over a script, statement by statement with the
/// script's DDL before each applied, and gives what they find in source order, without what a
/// suppression comment silences.
pub fn inspect(root: &SyntaxNode, request: &Request) -> Vec<Finding> {
    let cx = Cx::new(root, request);
    syntax::run(&cx);
    if PER_STATEMENT.iter().any(|id| cx.on(id)) {
        let mut document = DocumentSchema::new(request.target, request.schemas);
        for statement in root.children() {
            if cx.wants(statement.text_range()) && !is_trivial(&statement) {
                let catalog = document.catalog();
                let resolver = Resolver::new(&catalog);
                let stmt = Stmt {
                    clean: !statement.descendants().any(|node| node.kind() == ERROR),
                    known: knows_schema(&catalog),
                    mode: SqlMode::of(&catalog, &document.state),
                    node: statement.clone(),
                    catalog: &catalog,
                    resolver: &resolver,
                    state: &document.state,
                };
                names::run(&cx, &stmt);
                writes::run(&cx, &stmt);
                grouping::run(&cx, &stmt);
                conditions::run(&cx, &stmt);
                unused::run(&cx, &stmt);
                pitfalls::run(&cx, &stmt);
            }
            document.apply(&statement);
        }
    }
    let mut found = cx.found.into_inner();
    let concatenations: Vec<TextRange> = found
        .iter()
        .filter(|finding| finding.diagnostic.code == PIPES_AS_OR)
        .map(|finding| finding.diagnostic.range)
        .collect();
    // `||` meant as concatenation says more than its deprecation as OR.
    found.retain(|finding| {
        finding.diagnostic.feature != Some("double-pipe") || !concatenations.contains(&finding.diagnostic.range)
    });
    if let Some(range) = request.range {
        found.retain(|finding| finding.diagnostic.range.intersect(range).is_some());
    }
    suppress::apply(root, &mut found);
    found.sort_by_key(|finding| (finding.diagnostic.range.start(), finding.diagnostic.range.end()));
    found.dedup_by(|later, earlier| {
        later.diagnostic.range == earlier.diagnostic.range && later.diagnostic.code == earlier.diagnostic.code
    });
    found
}

/// A statement no inspection reads, such as a transaction's `BEGIN`.
fn is_trivial(statement: &SyntaxNode) -> bool {
    matches!(
        statement.kind(),
        EMPTY_STMT
            | BEGIN_STMT
            | COMMIT_STMT
            | ROLLBACK_STMT
            | SAVEPOINT_STMT
            | RELEASE_STMT
            | USE_STMT
            | SHOW_STMT
            | PRAGMA_STMT
            | META_COMMAND_STMT
            | DELIMITER_STMT
    )
}

/// The edits of one fix per finding of an inspection, or of every inspection that allows it,
/// leaving out a fix that overlaps one already taken.
pub fn fix_all(findings: &[Finding], id: Option<&str>) -> Vec<TextEdit> {
    let mut taken: Vec<TextEdit> = Vec::new();
    for finding in findings {
        let code = finding.diagnostic.code;
        if id.is_some_and(|id| id != code) || !inspection_info(code).is_some_and(|info| info.fix_all) {
            continue;
        }
        let [fix] = finding.fixes.as_slice() else {
            continue;
        };
        let overlaps = fix.edits.iter().any(|edit| {
            taken.iter().any(|other| {
                (edit.range.start() < other.range.end() && other.range.start() < edit.range.end())
                    || edit.range == other.range
            })
        });
        if !overlaps {
            taken.extend(fix.edits.iter().cloned());
        }
    }
    taken.sort_by_key(|edit| (edit.range.start(), edit.range.end()));
    taken
}
