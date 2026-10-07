//! What each dialect accepts: one table with a row per piece of syntax that not every dialect has,
//! saying per dialect since which version it is there (or that it never is, or that it is
//! deprecated), and a pass over a tree that reports what the target does not accept.
//!
//! The lexer and the parser read the union of all dialects and never look at a version.
//! Supporting a new version of a dialect means adding rows here, or moving `Since` of a row.

use std::sync::OnceLock;

use rowan::TextRange;

use crate::SyntaxKind::{self, *};
use crate::{Dialect, SqlLanguage, SyntaxElement, SyntaxNode, SyntaxToken, Target, Version};

/// Whether a dialect has a piece of syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// Since before the oldest supported version.
    Always,
    /// Since this version.
    Since(Version),
    Never,
    /// There, and deprecated since this version.
    DeprecatedSince(Version),
}

const A: Support = Support::Always;
const N: Support = Support::Never;

const fn since(major: u16, minor: u16, patch: u16) -> Support {
    Support::Since(Version::new(major, minor, patch))
}

const fn deprecated(major: u16, minor: u16, patch: u16) -> Support {
    Support::DeprecatedSince(Version::new(major, minor, patch))
}

/// One piece of syntax and where it is supported.
pub struct Feature {
    /// A stable identifier, the code of a diagnostic and what a client switches a check off by.
    pub id: &'static str,
    /// What the feature is called in a message, as a subject.
    pub name: &'static str,
    /// Whether `name` is plural, which picks "are" over "is".
    pub plural: bool,
    /// Support in SQLite, MySQL, MariaDB and PostgreSQL, in that order.
    pub support: [Support; 4],
    /// The kinds of element `detect` wants to see.
    pub kinds: &'static [SyntaxKind],
    /// The range to report when the element is an instance of the feature.
    pub detect: fn(&SyntaxElement) -> Option<TextRange>,
    /// A statement that uses the feature, which a test parses and holds the row against.
    pub example: &'static str,
}

impl Feature {
    /// The support in a dialect; `None` for `Generic`, which is not a database.
    pub fn support_in(&self, dialect: Dialect) -> Option<Support> {
        match dialect {
            Dialect::Generic => None,
            Dialect::Sqlite => Some(self.support[0]),
            Dialect::Mysql => Some(self.support[1]),
            Dialect::Mariadb => Some(self.support[2]),
            Dialect::Postgres => Some(self.support[3]),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureSeverity {
    Error,
    Warning,
}

/// A piece of syntax the target does not accept, or deprecates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureDiagnostic {
    pub range: TextRange,
    pub message: String,
    pub severity: FeatureSeverity,
    /// Set for a deprecation, which a client may draw struck through.
    pub deprecated: bool,
    pub feature: &'static str,
}

/// Reports what `target` does not accept in the tree, reserved words used as names included.
/// Without a dialect only what no dialect accepts is reported.
pub fn check_features(root: &SyntaxNode, target: Target) -> Vec<FeatureDiagnostic> {
    let index = dispatch_index();
    // A row reports nothing when the target has the syntax, so its detection is not run at all.
    let active: Vec<bool> = FEATURES.iter().map(|feature| can_report(feature, target)).collect();
    let wanted = |kind: SyntaxKind| index[kind as usize].iter().any(|row| active[*row]);
    let mut found = Vec::new();
    let run = |element: &SyntaxElement, found: &mut Vec<FeatureDiagnostic>| {
        for &row in &index[element.kind() as usize] {
            if !active[row] {
                continue;
            }
            let feature = &FEATURES[row];
            let Some(range) = (feature.detect)(element) else {
                continue;
            };
            if let Some(diagnostic) = judge(feature, range, target) {
                found.push(diagnostic);
            }
        }
    };
    for node in root.descendants() {
        if wanted(node.kind()) {
            run(&SyntaxElement::Node(node.clone()), &mut found);
        }
        let token_wanted = node.green().children().any(|child| {
            child
                .as_token()
                .is_some_and(|token| wanted(<SqlLanguage as rowan::Language>::kind_from_raw(token.kind())))
        });
        if token_wanted {
            for element in node.children_with_tokens() {
                if element.as_token().is_some() && wanted(element.kind()) {
                    run(&element, &mut found);
                }
            }
        }
        if node.kind() == NAME {
            found.extend(crate::reserved::judge_name(&node, target));
        }
    }
    found.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    found.dedup_by(|second, first| second.range == first.range && second.feature == first.feature);
    found
}

/// Whether a target has the syntax of the row with an id; without a dialect, whether any dialect
/// has it. An id the table does not have counts as there.
pub fn supports(id: &str, target: Target) -> bool {
    let Some(feature) = FEATURES.iter().find(|feature| feature.id == id) else {
        return true;
    };
    match feature.support_in(target.dialect) {
        None => feature.support.iter().any(|support| *support != Support::Never),
        Some(Support::Always) | Some(Support::DeprecatedSince(_)) => true,
        Some(Support::Never) => false,
        Some(Support::Since(version)) => target.at_least(version),
    }
}

/// Whether a row can report anything at the target, which a row of syntax the target has cannot.
fn can_report(feature: &Feature, target: Target) -> bool {
    match feature.support_in(target.dialect) {
        None => feature.support.iter().all(|support| *support == Support::Never),
        Some(Support::Always) => false,
        Some(Support::Never) => true,
        Some(Support::Since(version)) => !target.at_least(version),
        Some(Support::DeprecatedSince(version)) => target.at_least(version),
    }
}

fn judge(feature: &'static Feature, range: TextRange, target: Target) -> Option<FeatureDiagnostic> {
    let verb = |plural: &'static str, singular: &'static str| if feature.plural { plural } else { singular };
    let name = feature.name;
    let diagnostic = |message: String, severity: FeatureSeverity| {
        Some(FeatureDiagnostic {
            range,
            message,
            severity,
            deprecated: severity == FeatureSeverity::Warning,
            feature: feature.id,
        })
    };
    let Some(support) = feature.support_in(target.dialect) else {
        if feature.support.iter().all(|support| *support == Support::Never) {
            return diagnostic(
                format!(
                    "{name} {} not supported by SQLite, MySQL, MariaDB or PostgreSQL",
                    verb("are", "is")
                ),
                FeatureSeverity::Error,
            );
        }
        return None;
    };
    let dialect = target.dialect;
    match support {
        Support::Always => None,
        Support::Never => diagnostic(
            format!("{name} {} not supported by {dialect}", verb("are", "is")),
            FeatureSeverity::Error,
        ),
        Support::Since(version) if !target.at_least(version) => diagnostic(
            format!("{name} {} only available since {dialect} {version}", verb("are", "is")),
            FeatureSeverity::Error,
        ),
        Support::Since(_) => None,
        Support::DeprecatedSince(version) if target.at_least(version) => diagnostic(
            format!("{name} {} deprecated since {dialect} {version}", verb("are", "is")),
            FeatureSeverity::Warning,
        ),
        Support::DeprecatedSince(_) => None,
    }
}

fn dispatch_index() -> &'static Vec<Vec<usize>> {
    static INDEX: OnceLock<Vec<Vec<usize>>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = vec![Vec::new(); SyntaxKind::ALL.len()];
        for (row, feature) in FEATURES.iter().enumerate() {
            for kind in feature.kinds {
                index[*kind as usize].push(row);
            }
        }
        index
    })
}

fn node_of(element: &SyntaxElement) -> Option<&SyntaxNode> {
    element.as_node()
}

fn token_range(element: &SyntaxElement) -> Option<TextRange> {
    element.as_token().map(SyntaxToken::text_range)
}

fn node_range(element: &SyntaxElement) -> Option<TextRange> {
    node_of(element).map(SyntaxNode::text_range)
}

fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> + '_ {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| !token.kind().is_trivia())
}

fn first_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.first_token().filter(|token| !token.kind().is_trivia())
}

fn first_token_range(element: &SyntaxElement) -> Option<TextRange> {
    first_token(node_of(element)?).map(|token| token.text_range())
}

fn child_token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    tokens(node).find(|token| token.kind() == kind)
}

fn child_node(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|child| child.kind() == kind)
}

/// The range of the direct child token `kind` of the element's node.
fn keyword(element: &SyntaxElement, kind: SyntaxKind) -> Option<TextRange> {
    child_token(node_of(element)?, kind).map(|token| token.text_range())
}

/// From the direct child token `kind` to the end of the node.
fn from_keyword(element: &SyntaxElement, kind: SyntaxKind) -> Option<TextRange> {
    let node = node_of(element)?;
    let token = child_token(node, kind)?;
    Some(TextRange::new(token.text_range().start(), node.text_range().end()))
}

/// The range of two adjacent child tokens of these kinds.
fn keyword_pair(element: &SyntaxElement, first: SyntaxKind, second: SyntaxKind) -> Option<TextRange> {
    let node = node_of(element)?;
    let words: Vec<SyntaxToken> = tokens(node).collect();
    words
        .windows(2)
        .find(|pair| pair[0].kind() == first && pair[1].kind() == second)
        .map(|pair| TextRange::new(pair[0].text_range().start(), pair[1].text_range().end()))
}

fn parent_kind(element: &SyntaxElement) -> Option<SyntaxKind> {
    element.parent().map(|parent| parent.kind())
}

/// The last name of a `QUALIFIED_NAME` child, in lower case: `int` for `pg_catalog.int`.
fn last_name(node: &SyntaxNode) -> Option<String> {
    let name = child_node(node, QUALIFIED_NAME)?;
    let last = name.children().filter(|child| child.kind() == NAME).last()?;
    Some(last.text().to_string().to_ascii_lowercase())
}

/// Whether the node is inside a function, procedure or trigger.
fn in_routine(node: &SyntaxNode) -> bool {
    node.ancestors()
        .any(|ancestor| matches!(ancestor.kind(), CREATE_FUNCTION_STMT | CREATE_TRIGGER_STMT))
}

fn is_query(kind: SyntaxKind) -> bool {
    matches!(kind, SELECT | COMPOUND_SELECT | VALUES | TABLE_QUERY | PAREN_QUERY)
}

/// The `WITH` keyword of a statement that starts with a `WITH` clause.
fn leading_with(element: &SyntaxElement) -> Option<TextRange> {
    let with = child_node(node_of(element)?, WITH_CLAUSE)?;
    first_token(&with).map(|token| token.text_range())
}

/// The first of the first three tokens of a statement that has one of `kinds`.
fn modifier(element: &SyntaxElement, kinds: &[SyntaxKind]) -> Option<TextRange> {
    tokens(node_of(element)?)
        .take(3)
        .find(|token| kinds.contains(&token.kind()))
        .map(|token| token.text_range())
}

/// The syntax that sets the dialects apart. Every row's `example` parses in every dialect that
/// accepts it; a test holds each row against its example, and `scripts/dialect-corpus.py` holds the
/// corpus against real servers.
pub static FEATURES: &[Feature] = &[
    // Names and literals
    Feature {
        id: "backtick-identifiers",
        name: "Backtick-quoted identifiers",
        plural: true,
        support: [A, A, A, N],
        kinds: &[BACKTICK_IDENT],
        detect: token_range,
        example: "SELECT `a` FROM t",
    },
    Feature {
        id: "escape-strings",
        name: "Escape strings (E'...')",
        plural: true,
        support: [N, N, N, A],
        kinds: &[ESCAPE_STRING],
        detect: token_range,
        example: "SELECT E'a\\nb'",
    },
    Feature {
        id: "unicode-strings",
        name: "Unicode escape strings (U&'...')",
        plural: true,
        support: [N, N, N, A],
        kinds: &[UNICODE_STRING],
        detect: token_range,
        example: "SELECT U&'d\\0061t'",
    },
    Feature {
        id: "national-strings",
        name: "National strings (N'...')",
        plural: true,
        support: [N, A, A, A],
        kinds: &[NATIONAL_STRING],
        detect: token_range,
        example: "SELECT N'abc'",
    },
    Feature {
        id: "bit-strings",
        name: "Bit strings (B'...')",
        plural: true,
        support: [N, A, A, A],
        kinds: &[BIT_STRING],
        detect: token_range,
        example: "SELECT B'101'",
    },
    Feature {
        id: "binary-literals",
        name: "Binary integer literals (0b...)",
        plural: true,
        support: [N, A, A, A],
        kinds: &[INT_NUMBER],
        detect: |element| {
            let token = element.as_token()?;
            let text = token.text().as_bytes();
            (text.len() > 2 && text[0] == b'0' && matches!(text[1], b'b' | b'B')).then(|| token.text_range())
        },
        example: "SELECT 0b101",
    },
    Feature {
        id: "octal-literals",
        name: "Octal integer literals (0o...)",
        plural: true,
        support: [N, N, N, A],
        kinds: &[INT_NUMBER],
        detect: |element| {
            let token = element.as_token()?;
            let text = token.text().as_bytes();
            (text.len() > 2 && text[0] == b'0' && matches!(text[1], b'o' | b'O')).then(|| token.text_range())
        },
        example: "SELECT 0o17",
    },
    Feature {
        id: "dollar-quoted-strings",
        name: "Dollar-quoted strings",
        plural: true,
        support: [N, N, N, A],
        kinds: &[DOLLAR_STRING],
        detect: token_range,
        example: "SELECT $$text$$",
    },
    Feature {
        id: "typed-literals",
        name: "Literals of a named type",
        plural: true,
        support: [N, N, N, A],
        kinds: &[TYPED_LITERAL],
        detect: |element| {
            let node = node_of(element)?;
            child_node(node, TYPE).map(|_| node.text_range())
        },
        example: "SELECT json '{}'",
    },
    Feature {
        id: "datetime-literals",
        name: "DATE, TIME and TIMESTAMP literals",
        plural: true,
        support: [N, A, A, A],
        kinds: &[TYPED_LITERAL],
        detect: |element| {
            let node = node_of(element)?;
            matches!(first_token(node)?.kind(), DATE_KW | TIME_KW | TIMESTAMP_KW).then(|| node.text_range())
        },
        example: "SELECT DATE '2024-01-31'",
    },
    Feature {
        id: "string-aliases",
        name: "Aliases written as strings",
        plural: true,
        support: [A, A, A, N],
        kinds: &[ALIAS],
        detect: |element| {
            let name = child_node(node_of(element)?, NAME)?;
            let token = first_token(&name)?;
            token.kind().is_string().then(|| token.text_range())
        },
        example: "SELECT 1 AS 'one'",
    },
    // Parameters and variables
    Feature {
        id: "numbered-parameters",
        name: "Numbered parameters ($1)",
        plural: true,
        support: [A, N, N, A],
        kinds: &[PARAMETER],
        detect: |element| {
            let token = first_token(node_of(element)?)?;
            let text = token.text();
            (token.kind() == PARAM && text.starts_with('$') && text[1..].starts_with(|c: char| c.is_ascii_digit()))
                .then(|| token.text_range())
        },
        example: "SELECT * FROM t WHERE a = $1",
    },
    Feature {
        id: "question-parameters",
        name: "Question mark parameters",
        plural: true,
        support: [A, A, A, N],
        kinds: &[PARAMETER],
        detect: |element| {
            let token = first_token(node_of(element)?)?;
            (token.kind() == QUESTION).then(|| token.text_range())
        },
        example: "SELECT * FROM t WHERE a = ?",
    },
    Feature {
        id: "named-parameters",
        name: "Named parameters (:name)",
        plural: true,
        support: [A, N, N, N],
        kinds: &[PARAMETER],
        detect: |element| {
            let node = node_of(element)?;
            (first_token(node)?.kind() == COLON).then(|| node.text_range())
        },
        example: "SELECT * FROM t WHERE a = :id",
    },
    Feature {
        id: "user-variables",
        name: "User and system variables",
        plural: true,
        support: [N, A, A, N],
        kinds: &[VARIABLE_REF],
        detect: node_range,
        example: "SELECT @total",
    },
    Feature {
        id: "assignment-operator",
        name: "Assignments in expressions (:=)",
        plural: true,
        support: [N, deprecated(8, 0, 13), A, N],
        kinds: &[BINARY_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            let operator = child_token(node, COLON_EQ)?;
            let in_set = node.ancestors().any(|ancestor| ancestor.kind() == SET_STMT);
            (!in_set).then(|| operator.text_range())
        },
        example: "SELECT @n := 1",
    },
    // Operators
    Feature {
        id: "cast-operator",
        name: "The :: cast",
        plural: false,
        support: [N, N, N, A],
        kinds: &[DOUBLE_COLON],
        detect: token_range,
        example: "SELECT a::text FROM t",
    },
    Feature {
        id: "null-safe-equal",
        name: "The <=> operator",
        plural: false,
        support: [N, A, A, N],
        kinds: &[NULL_SAFE_EQ],
        detect: token_range,
        example: "SELECT * FROM t WHERE a <=> b",
    },
    Feature {
        id: "double-equals",
        name: "The == operator",
        plural: false,
        support: [A, N, N, N],
        kinds: &[EQ_EQ],
        detect: token_range,
        example: "SELECT * FROM t WHERE a == b",
    },
    Feature {
        id: "caret-operator",
        name: "The ^ operator",
        plural: false,
        support: [N, A, A, A],
        kinds: &[CARET],
        detect: token_range,
        example: "SELECT 2 ^ 3",
    },
    Feature {
        id: "double-ampersand",
        name: "The && operator",
        plural: false,
        support: [N, deprecated(8, 0, 17), A, A],
        kinds: &[AMP_AMP],
        detect: token_range,
        example: "SELECT * FROM t WHERE a && b",
    },
    Feature {
        id: "double-pipe",
        name: "The || operator",
        plural: false,
        support: [A, deprecated(8, 0, 17), A, A],
        kinds: &[PIPE_PIPE],
        detect: token_range,
        example: "SELECT a || b FROM t",
    },
    Feature {
        id: "bang-not",
        name: "The ! operator",
        plural: false,
        support: [N, A, A, N],
        kinds: &[PREFIX_EXPR],
        detect: |element| keyword(element, BANG),
        example: "SELECT !a FROM t",
    },
    Feature {
        id: "div-mod-operators",
        name: "The DIV and MOD operators",
        plural: true,
        support: [N, A, A, N],
        kinds: &[BINARY_EXPR],
        detect: |element| keyword(element, DIV_KW).or_else(|| keyword(element, MOD_KW)),
        example: "SELECT 7 DIV 2, 7 MOD 2",
    },
    Feature {
        id: "xor-operator",
        name: "XOR",
        plural: false,
        support: [N, A, A, N],
        kinds: &[BINARY_EXPR],
        detect: |element| keyword(element, XOR_KW),
        example: "SELECT * FROM t WHERE a XOR b",
    },
    Feature {
        id: "json-arrow-operators",
        name: "The -> and ->> operators",
        plural: true,
        support: [A, A, N, A],
        kinds: &[ARROW, LONG_ARROW],
        detect: token_range,
        example: "SELECT j ->> 'a' FROM t",
    },
    Feature {
        id: "postgres-operators",
        name: "PostgreSQL's operators",
        plural: true,
        support: [N, N, N, A],
        kinds: &[
            AT_GT,
            LT_AT,
            QUESTION_PIPE,
            QUESTION_AMP,
            HASH,
            HASH_ARROW,
            HASH_LONG_ARROW,
            HASH_MINUS,
            AT_AT,
            TILDE_STAR,
            BANG_TILDE,
            BANG_TILDE_STAR,
            CUSTOM_OP,
            OPERATOR_NAME,
        ],
        detect: |element| token_range(element).or_else(|| node_range(element)),
        example: "SELECT a @> b FROM t",
    },
    Feature {
        id: "infix-tilde",
        name: "Regular expression matches with ~",
        plural: true,
        support: [N, N, N, A],
        kinds: &[BINARY_EXPR],
        detect: |element| keyword(element, TILDE),
        example: "SELECT * FROM t WHERE a ~ 'x'",
    },
    Feature {
        id: "infix-question",
        name: "The ? operator",
        plural: false,
        support: [N, N, N, A],
        kinds: &[BINARY_EXPR],
        detect: |element| keyword(element, QUESTION),
        example: "SELECT j ? 'k' FROM t",
    },
    Feature {
        id: "ilike",
        name: "ILIKE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[LIKE_EXPR],
        detect: |element| keyword(element, ILIKE_KW),
        example: "SELECT * FROM t WHERE a ILIKE 'x%'",
    },
    Feature {
        id: "similar-to",
        name: "SIMILAR TO",
        plural: false,
        support: [N, N, N, A],
        kinds: &[LIKE_EXPR],
        detect: |element| keyword(element, SIMILAR_KW),
        example: "SELECT * FROM t WHERE a SIMILAR TO 'x%'",
    },
    Feature {
        id: "regexp-operator",
        name: "REGEXP",
        plural: false,
        support: [A, A, A, N],
        kinds: &[LIKE_EXPR],
        detect: |element| keyword(element, REGEXP_KW),
        example: "SELECT * FROM t WHERE a REGEXP 'x'",
    },
    Feature {
        id: "rlike-operator",
        name: "RLIKE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[LIKE_EXPR],
        detect: |element| keyword(element, RLIKE_KW),
        example: "SELECT * FROM t WHERE a RLIKE 'x'",
    },
    Feature {
        id: "glob-match-operators",
        name: "GLOB and MATCH",
        plural: true,
        support: [A, N, N, N],
        kinds: &[LIKE_EXPR],
        detect: |element| keyword(element, GLOB_KW).or_else(|| keyword(element, MATCH_KW)),
        example: "SELECT * FROM t WHERE a GLOB 'x*'",
    },
    Feature {
        id: "sounds-like",
        name: "SOUNDS LIKE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[LIKE_EXPR],
        detect: |element| keyword(element, SOUNDS_KW),
        example: "SELECT * FROM t WHERE a SOUNDS LIKE 'x'",
    },
    Feature {
        id: "is-distinct-from",
        name: "IS DISTINCT FROM",
        plural: false,
        support: [A, N, N, A],
        kinds: &[IS_EXPR],
        detect: |element| keyword(element, DISTINCT_KW).and_then(|_| from_keyword(element, IS_KW)),
        example: "SELECT * FROM t WHERE a IS DISTINCT FROM b",
    },
    Feature {
        id: "is-json",
        name: "IS JSON",
        plural: false,
        support: [N, N, N, A],
        kinds: &[IS_EXPR],
        detect: |element| keyword(element, JSON_KW).and_then(|_| from_keyword(element, IS_KW)),
        example: "SELECT j IS JSON FROM t",
    },
    Feature {
        id: "between-symmetric",
        name: "BETWEEN SYMMETRIC",
        plural: false,
        support: [N, N, N, A],
        kinds: &[BETWEEN_EXPR],
        detect: |element| keyword(element, SYMMETRIC_KW).or_else(|| keyword(element, ASYMMETRIC_KW)),
        example: "SELECT * FROM t WHERE a BETWEEN SYMMETRIC 2 AND 1",
    },
    Feature {
        id: "in-table",
        name: "IN with a table",
        plural: false,
        support: [A, N, N, N],
        kinds: &[IN_EXPR],
        detect: |element| {
            node_of(element)?
                .children()
                .find(|child| matches!(child.kind(), QUALIFIED_NAME | FUNCTION_CALL))
                .map(|child| child.text_range())
        },
        example: "SELECT * FROM t WHERE a IN u",
    },
    Feature {
        id: "member-of",
        name: "MEMBER OF",
        plural: false,
        support: [N, since(8, 0, 17), N, N],
        kinds: &[MEMBER_OF_EXPR],
        detect: |element| keyword_pair(element, MEMBER_KW, OF_KW),
        example: "SELECT * FROM t WHERE 1 MEMBER OF (j)",
    },
    Feature {
        id: "overlaps",
        name: "OVERLAPS",
        plural: false,
        support: [N, N, N, A],
        kinds: &[BINARY_EXPR],
        detect: |element| keyword(element, OVERLAPS_KW),
        example: "SELECT (a, b) OVERLAPS (c, d) FROM t",
    },
    Feature {
        id: "at-time-zone",
        name: "AT TIME ZONE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[AT_TIME_ZONE_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            (node.parent()?.kind() != CAST_EXPR).then(|| keyword(element, AT_KW))?
        },
        example: "SELECT ts AT TIME ZONE 'UTC' FROM t",
    },
    Feature {
        id: "binary-operator",
        name: "The BINARY operator",
        plural: false,
        support: [N, deprecated(8, 0, 27), A, N],
        kinds: &[PREFIX_EXPR],
        detect: |element| keyword(element, BINARY_KW),
        example: "SELECT * FROM t WHERE BINARY a = 'x'",
    },
    // Expressions
    Feature {
        id: "arrays",
        name: "Arrays",
        plural: true,
        support: [N, N, N, A],
        kinds: &[ARRAY_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            (node.parent()?.kind() != ARRAY_EXPR).then(|| node.text_range())
        },
        example: "SELECT ARRAY[1, 2]",
    },
    Feature {
        id: "subscripts",
        name: "Subscripts",
        plural: true,
        support: [N, N, N, A],
        kinds: &[INDEX_EXPR],
        detect: |element| keyword(element, LBRACKET),
        example: "SELECT a[1] FROM t",
    },
    Feature {
        id: "row-constructors",
        name: "ROW(...)",
        plural: false,
        support: [N, A, A, A],
        kinds: &[ROW_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            (node.parent()?.kind() != VALUES).then(|| keyword(element, ROW_KW))?
        },
        example: "SELECT * FROM t WHERE (a, b) = ROW(1, 2)",
    },
    Feature {
        id: "quantified-arrays",
        name: "ANY, SOME and ALL with an array",
        plural: false,
        support: [N, N, N, A],
        kinds: &[QUANTIFIED_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            child_node(node, PAREN_QUERY).is_none().then(|| node.text_range())
        },
        example: "SELECT * FROM t WHERE a = ANY(b)",
    },
    Feature {
        id: "intervals",
        name: "INTERVAL",
        plural: false,
        support: [N, A, A, A],
        kinds: &[INTERVAL_EXPR],
        detect: |element| keyword(element, INTERVAL_KW),
        example: "SELECT now() + INTERVAL '1' DAY",
    },
    Feature {
        id: "interval-expressions",
        name: "INTERVAL with an expression",
        plural: false,
        support: [N, A, A, N],
        kinds: &[INTERVAL_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            let value = node.children().next()?;
            let string = value.kind() == LITERAL && first_token(&value)?.kind().is_string();
            (!string).then(|| node.text_range())
        },
        example: "SELECT now() + INTERVAL 1 DAY",
    },
    Feature {
        id: "match-against",
        name: "MATCH ... AGAINST",
        plural: false,
        support: [N, A, A, N],
        kinds: &[MATCH_AGAINST_EXPR],
        detect: node_range,
        example: "SELECT * FROM t WHERE MATCH (a) AGAINST ('x')",
    },
    Feature {
        id: "named-arguments",
        name: "Named arguments",
        plural: true,
        support: [N, N, N, A],
        kinds: &[NAMED_ARG],
        detect: node_range,
        example: "SELECT f(a => 1)",
    },
    Feature {
        id: "filter-clause",
        name: "FILTER",
        plural: false,
        support: [A, N, N, A],
        kinds: &[FILTER_CLAUSE],
        detect: first_token_range,
        example: "SELECT count(*) FILTER (WHERE a > 1) FROM t",
    },
    Feature {
        id: "within-group",
        name: "WITHIN GROUP",
        plural: false,
        support: [N, N, N, A],
        kinds: &[WITHIN_GROUP_CLAUSE],
        detect: |element| {
            let call = node_of(element)?.parent()?;
            child_node(&call, OVER_CLAUSE)
                .is_none()
                .then(|| keyword_pair(element, WITHIN_KW, GROUP_KW))?
        },
        example: "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY a) FROM t",
    },
    Feature {
        id: "within-group-over",
        name: "WITHIN GROUP with OVER",
        plural: false,
        support: [N, N, A, N],
        kinds: &[WITHIN_GROUP_CLAUSE],
        detect: |element| {
            let call = node_of(element)?.parent()?;
            child_node(&call, OVER_CLAUSE).and_then(|_| keyword_pair(element, WITHIN_KW, GROUP_KW))
        },
        example: "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY a) OVER () FROM t",
    },
    Feature {
        id: "aggregate-order-by",
        name: "ORDER BY in the arguments of a function",
        plural: false,
        support: [A, N, N, A],
        kinds: &[ARG_LIST],
        detect: |element| {
            let node = node_of(element)?;
            let order = child_node(node, ORDER_BY_CLAUSE)?;
            let name = last_name(&node.parent()?)?;
            (!matches!(name.as_str(), "group_concat" | "json_arrayagg" | "json_objectagg")).then(|| order.text_range())
        },
        example: "SELECT string_agg(a, ',' ORDER BY a) FROM t",
    },
    Feature {
        id: "json-arrayagg-order-by",
        name: "ORDER BY in JSON_ARRAYAGG",
        plural: false,
        support: [N, N, A, A],
        kinds: &[ARG_LIST],
        detect: |element| {
            let node = node_of(element)?;
            let order = child_node(node, ORDER_BY_CLAUSE)?;
            (last_name(&node.parent()?)? == "json_arrayagg").then(|| order.text_range())
        },
        example: "SELECT JSON_ARRAYAGG(a ORDER BY a) FROM t",
    },
    Feature {
        id: "group-concat-separator",
        name: "SEPARATOR",
        plural: false,
        support: [N, A, A, N],
        kinds: &[ARG_LIST],
        detect: |element| keyword(element, SEPARATOR_KW),
        example: "SELECT GROUP_CONCAT(a SEPARATOR ',') FROM t",
    },
    Feature {
        id: "json-key-value",
        name: "Key and value pairs in JSON constructors",
        plural: true,
        support: [N, N, N, A],
        kinds: &[JSON_KEY_VALUE],
        detect: node_range,
        example: "SELECT JSON_OBJECT('a' VALUE 1)",
    },
    Feature {
        id: "default-expressions",
        name: "DEFAULT as a value",
        plural: false,
        support: [N, A, A, A],
        kinds: &[DEFAULT_EXPR],
        detect: node_range,
        example: "INSERT INTO t (a) VALUES (DEFAULT)",
    },
    Feature {
        id: "standard-function-forms",
        name: "FROM, FOR, IN and PLACING in the arguments of a function",
        plural: false,
        support: [N, A, A, A],
        kinds: &[ARG_LIST],
        detect: |element| {
            tokens(node_of(element)?)
                .find(|token| {
                    matches!(
                        token.kind(),
                        FROM_KW | FOR_KW | IN_KW | PLACING_KW | LEADING_KW | TRAILING_KW | BOTH_KW
                    )
                })
                .map(|token| token.text_range())
        },
        example: "SELECT substring(name FROM 1 FOR 2) FROM t",
    },
    Feature {
        id: "cast-to-integer",
        name: "CAST to an integer type or VARCHAR",
        plural: false,
        support: [A, N, A, A],
        kinds: &[CAST_EXPR],
        detect: |element| {
            let ty = child_node(node_of(element)?, TYPE)?;
            let name = last_name(&ty)?;
            matches!(
                name.as_str(),
                "int" | "integer" | "bigint" | "smallint" | "tinyint" | "mediumint" | "varchar"
            )
            .then(|| ty.text_range())
        },
        example: "SELECT CAST(a AS INTEGER) FROM t",
    },
    Feature {
        id: "cast-to-text",
        name: "CAST to TEXT or BOOLEAN",
        plural: false,
        support: [A, N, N, A],
        kinds: &[CAST_EXPR],
        detect: |element| {
            let ty = child_node(node_of(element)?, TYPE)?;
            matches!(last_name(&ty)?.as_str(), "text" | "boolean" | "bool").then(|| ty.text_range())
        },
        example: "SELECT CAST(a AS TEXT) FROM t",
    },
    Feature {
        id: "quantified-subqueries",
        name: "ANY, SOME and ALL",
        plural: false,
        support: [N, A, A, A],
        kinds: &[QUANTIFIED_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            child_node(node, PAREN_QUERY).map(|_| node.text_range())
        },
        example: "SELECT * FROM t WHERE a > ANY (SELECT a FROM u)",
    },
    Feature {
        id: "is-expression",
        name: "IS with an expression",
        plural: false,
        support: [A, N, N, N],
        kinds: &[IS_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            let words: Vec<SyntaxKind> = tokens(node).map(|token| token.kind()).collect();
            if words.iter().any(|kind| matches!(kind, DISTINCT_KW | JSON_KW | OF_KW)) {
                return None;
            }
            node.children().nth(1).map(|right| right.text_range())
        },
        example: "SELECT * FROM t WHERE a IS b",
    },
    // Queries
    Feature {
        id: "sql-calc-found-rows",
        name: "SQL_CALC_FOUND_ROWS",
        plural: false,
        support: [N, deprecated(8, 0, 17), A, N],
        kinds: &[SQL_CALC_FOUND_ROWS_KW],
        detect: token_range,
        example: "SELECT SQL_CALC_FOUND_ROWS a FROM t",
    },
    Feature {
        id: "qualify",
        name: "QUALIFY",
        plural: false,
        support: [N, N, N, N],
        kinds: &[QUALIFY_CLAUSE],
        detect: first_token_range,
        example: "SELECT a FROM t QUALIFY a > 1",
    },
    Feature {
        id: "distinct-on",
        name: "DISTINCT ON",
        plural: false,
        support: [N, N, N, A],
        kinds: &[DISTINCT_CLAUSE],
        detect: |element| keyword_pair(element, DISTINCT_KW, ON_KW),
        example: "SELECT DISTINCT ON (a) a, b FROM t",
    },
    Feature {
        id: "empty-select-list",
        name: "A select list without columns",
        plural: false,
        support: [N, N, N, A],
        kinds: &[SELECT_LIST],
        detect: |element| {
            let node = node_of(element)?;
            let select = node.parent()?;
            (node.children().next().is_none() && select.kind() == SELECT)
                .then(|| first_token(&select).map(|token| token.text_range()))?
        },
        example: "SELECT FROM t",
    },
    Feature {
        id: "select-into-variables",
        name: "INTO variables or a file",
        plural: false,
        support: [N, A, A, N],
        kinds: &[INTO_CLAUSE],
        detect: |element| {
            let node = node_of(element)?;
            let variables = child_node(node, VARIABLE_REF).is_some();
            let file = tokens(node).nth(1).is_some_and(|token| {
                token.text().eq_ignore_ascii_case("outfile") || token.text().eq_ignore_ascii_case("dumpfile")
            });
            (variables || file).then(|| node.text_range())
        },
        example: "SELECT a INTO @a FROM t",
    },
    Feature {
        id: "full-join",
        name: "FULL JOIN",
        plural: false,
        support: [A, N, N, A],
        kinds: &[JOIN_EXPR],
        detect: |element| keyword(element, FULL_KW),
        example: "SELECT * FROM a FULL JOIN b ON a.x = b.x",
    },
    Feature {
        id: "straight-join",
        name: "STRAIGHT_JOIN",
        plural: false,
        support: [N, A, A, N],
        kinds: &[STRAIGHT_JOIN_KW],
        detect: token_range,
        example: "SELECT * FROM a STRAIGHT_JOIN b ON a.x = b.x",
    },
    Feature {
        id: "join-without-condition",
        name: "JOIN without ON or USING",
        plural: false,
        support: [A, A, A, N],
        kinds: &[JOIN_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            let words: Vec<SyntaxKind> = tokens(node).map(|token| token.kind()).collect();
            let free = words
                .iter()
                .any(|kind| matches!(kind, CROSS_KW | NATURAL_KW | STRAIGHT_JOIN_KW));
            let outer = words.iter().any(|kind| matches!(kind, LEFT_KW | RIGHT_KW | FULL_KW));
            let condition = child_node(node, ON_CLAUSE).is_some() || child_node(node, USING_CLAUSE).is_some();
            (!free && !outer && !condition).then(|| keyword(element, JOIN_KW))?
        },
        example: "SELECT * FROM a JOIN b",
    },
    Feature {
        id: "outer-join-without-condition",
        name: "An outer join without ON or USING",
        plural: false,
        support: [A, N, N, N],
        kinds: &[JOIN_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            let words: Vec<SyntaxKind> = tokens(node).map(|token| token.kind()).collect();
            let natural = words.contains(&NATURAL_KW);
            let outer = words.iter().any(|kind| matches!(kind, LEFT_KW | RIGHT_KW | FULL_KW));
            let condition = child_node(node, ON_CLAUSE).is_some() || child_node(node, USING_CLAUSE).is_some();
            (outer && !natural && !condition).then(|| keyword(element, JOIN_KW))?
        },
        example: "SELECT * FROM a LEFT JOIN b",
    },
    Feature {
        id: "join-using-alias",
        name: "An alias for USING",
        plural: false,
        support: [N, N, N, A],
        kinds: &[USING_CLAUSE],
        detect: |element| (parent_kind(element)? == JOIN_EXPR).then(|| from_keyword(element, AS_KW))?,
        example: "SELECT * FROM a JOIN b USING (x) AS j",
    },
    Feature {
        id: "lateral",
        name: "LATERAL",
        plural: false,
        support: [N, since(8, 0, 14), N, A],
        kinds: &[LATERAL_KW],
        detect: token_range,
        example: "SELECT * FROM a, LATERAL (SELECT a.x) AS l",
    },
    Feature {
        id: "derived-table-without-alias",
        name: "A subquery in FROM without an alias",
        plural: false,
        support: [A, N, N, A],
        kinds: &[DERIVED_TABLE],
        detect: |element| {
            let node = node_of(element)?;
            child_node(node, ALIAS).is_none().then(|| node.text_range())
        },
        example: "SELECT * FROM (SELECT 1)",
    },
    Feature {
        id: "table-functions",
        name: "Functions in FROM",
        plural: true,
        support: [A, N, N, A],
        kinds: &[TABLE_FUNCTION],
        detect: |element| {
            let call = child_node(node_of(element)?, FUNCTION_CALL)?;
            (last_name(&call)? != "json_table").then(|| call.text_range())
        },
        example: "SELECT * FROM generate_series(1, 3) AS g",
    },
    Feature {
        id: "json-table",
        name: "JSON_TABLE",
        plural: false,
        support: [N, since(8, 0, 4), A, A],
        kinds: &[TABLE_FUNCTION],
        detect: |element| {
            let call = child_node(node_of(element)?, FUNCTION_CALL)?;
            (last_name(&call)? == "json_table").then(|| call.text_range())
        },
        example: "SELECT * FROM JSON_TABLE('[1]', '$[*]' COLUMNS (a INT PATH '$')) AS jt",
    },
    Feature {
        id: "with-ordinality",
        name: "WITH ORDINALITY and ROWS FROM",
        plural: false,
        support: [N, N, N, A],
        kinds: &[TABLE_FUNCTION],
        detect: |element| {
            keyword_pair(element, WITH_KW, ORDINALITY_KW).or_else(|| keyword_pair(element, ROWS_KW, FROM_KW))
        },
        example: "SELECT * FROM unnest(ARRAY[1]) WITH ORDINALITY AS u (a, n)",
    },
    Feature {
        id: "tablesample",
        name: "TABLESAMPLE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[TABLESAMPLE_CLAUSE],
        detect: first_token_range,
        example: "SELECT * FROM t TABLESAMPLE SYSTEM (10)",
    },
    Feature {
        id: "index-hints",
        name: "Index hints",
        plural: true,
        support: [N, A, A, N],
        kinds: &[INDEX_HINT],
        detect: |element| {
            let node = node_of(element)?;
            matches!(first_token(node)?.kind(), USE_KW | IGNORE_KW | FORCE_KW).then(|| node.text_range())
        },
        example: "SELECT * FROM t USE INDEX (i)",
    },
    Feature {
        id: "indexed-by",
        name: "INDEXED BY and NOT INDEXED",
        plural: false,
        support: [A, N, N, N],
        kinds: &[INDEX_HINT],
        detect: |element| {
            let node = node_of(element)?;
            matches!(first_token(node)?.kind(), INDEXED_KW | NOT_KW).then(|| node.text_range())
        },
        example: "SELECT * FROM t INDEXED BY i",
    },
    Feature {
        id: "partition-selection",
        name: "Selecting partitions",
        plural: false,
        support: [N, A, A, N],
        kinds: &[PARTITION_SELECTION],
        detect: node_range,
        example: "SELECT * FROM t PARTITION (p0)",
    },
    Feature {
        id: "grouping-sets",
        name: "GROUPING SETS and CUBE",
        plural: true,
        support: [N, N, N, A],
        kinds: &[GROUPING_SET],
        detect: |element| {
            let node = node_of(element)?;
            let rollup = first_token(node)?.kind() == ROLLUP_KW;
            (!rollup && node.parent()?.kind() != GROUPING_SET).then(|| node.text_range())
        },
        example: "SELECT a, count(*) FROM t GROUP BY CUBE (a)",
    },
    Feature {
        id: "rollup",
        name: "ROLLUP (...)",
        plural: false,
        support: [N, since(8, 4, 0), N, A],
        kinds: &[GROUPING_SET],
        detect: |element| {
            let node = node_of(element)?;
            let rollup = first_token(node)?.kind() == ROLLUP_KW;
            (rollup && node.parent()?.kind() != GROUPING_SET).then(|| node.text_range())
        },
        example: "SELECT a, count(*) FROM t GROUP BY ROLLUP (a)",
    },
    Feature {
        id: "with-rollup",
        name: "WITH ROLLUP",
        plural: false,
        support: [N, A, A, N],
        kinds: &[GROUP_BY_CLAUSE],
        detect: |element| keyword_pair(element, WITH_KW, ROLLUP_KW),
        example: "SELECT a, count(*) FROM t GROUP BY a WITH ROLLUP",
    },
    Feature {
        id: "groups-frames",
        name: "GROUPS frames",
        plural: true,
        support: [A, N, N, A],
        kinds: &[FRAME_CLAUSE],
        detect: |element| keyword(element, GROUPS_KW),
        example: "SELECT sum(a) OVER (ORDER BY a GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t",
    },
    Feature {
        id: "frame-exclusion",
        name: "EXCLUDE in a window frame",
        plural: false,
        support: [A, N, N, A],
        kinds: &[FRAME_CLAUSE],
        detect: |element| from_keyword(element, EXCLUDE_KW),
        example: "SELECT sum(a) OVER (ORDER BY a ROWS UNBOUNDED PRECEDING EXCLUDE CURRENT ROW) FROM t",
    },
    Feature {
        id: "nulls-ordering",
        name: "NULLS FIRST and NULLS LAST",
        plural: true,
        support: [A, N, N, A],
        kinds: &[ORDER_ITEM],
        detect: |element| from_keyword(element, NULLS_KW),
        example: "SELECT * FROM t ORDER BY a NULLS LAST",
    },
    Feature {
        id: "index-nulls-ordering",
        name: "NULLS FIRST and NULLS LAST in an index",
        plural: true,
        support: [N, N, N, A],
        kinds: &[INDEX_COLUMN],
        detect: |element| from_keyword(element, NULLS_KW),
        example: "CREATE INDEX i ON t (a DESC NULLS LAST)",
    },
    Feature {
        id: "intersect-except",
        name: "INTERSECT and EXCEPT",
        plural: true,
        support: [A, since(8, 0, 31), A, A],
        kinds: &[COMPOUND_SELECT],
        detect: |element| keyword(element, INTERSECT_KW).or_else(|| keyword(element, EXCEPT_KW)),
        example: "SELECT a FROM t INTERSECT SELECT a FROM u",
    },
    Feature {
        id: "intersect-except-all",
        name: "INTERSECT ALL and EXCEPT ALL",
        plural: true,
        support: [N, since(8, 0, 31), A, A],
        kinds: &[COMPOUND_SELECT],
        detect: |element| {
            keyword_pair(element, INTERSECT_KW, ALL_KW).or_else(|| keyword_pair(element, EXCEPT_KW, ALL_KW))
        },
        example: "SELECT a FROM t EXCEPT ALL SELECT a FROM u",
    },
    Feature {
        id: "limit-with-comma",
        name: "LIMIT with an offset before a comma",
        plural: false,
        support: [A, A, A, N],
        kinds: &[LIMIT_CLAUSE],
        detect: |element| keyword(element, COMMA).and_then(|_| node_range(element)),
        example: "SELECT * FROM t LIMIT 5, 10",
    },
    Feature {
        id: "limit-all",
        name: "LIMIT ALL",
        plural: false,
        support: [N, N, N, A],
        kinds: &[LIMIT_CLAUSE],
        detect: |element| keyword_pair(element, LIMIT_KW, ALL_KW),
        example: "SELECT * FROM t LIMIT ALL",
    },
    Feature {
        id: "offset-without-limit",
        name: "OFFSET without LIMIT",
        plural: false,
        support: [N, N, A, A],
        kinds: &[OFFSET_CLAUSE],
        detect: |element| {
            let node = node_of(element)?;
            let after_limit = node
                .prev_sibling()
                .is_some_and(|previous| previous.kind() == LIMIT_CLAUSE);
            (!after_limit).then(|| node.text_range())
        },
        example: "SELECT * FROM t OFFSET 5 ROWS",
    },
    Feature {
        id: "fetch-first",
        name: "FETCH FIRST",
        plural: false,
        support: [N, N, A, A],
        kinds: &[FETCH_CLAUSE],
        detect: node_range,
        example: "SELECT * FROM t FETCH FIRST 5 ROWS ONLY",
    },
    Feature {
        id: "for-update",
        name: "FOR UPDATE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[LOCKING_CLAUSE],
        detect: |element| keyword_pair(element, FOR_KW, UPDATE_KW),
        example: "SELECT * FROM t FOR UPDATE",
    },
    Feature {
        id: "for-share",
        name: "FOR SHARE",
        plural: false,
        support: [N, A, N, A],
        kinds: &[LOCKING_CLAUSE],
        detect: |element| keyword_pair(element, FOR_KW, SHARE_KW),
        example: "SELECT * FROM t FOR SHARE",
    },
    Feature {
        id: "key-locks",
        name: "FOR NO KEY UPDATE and FOR KEY SHARE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[LOCKING_CLAUSE],
        detect: |element| keyword(element, KEY_KW).and_then(|_| node_range(element)),
        example: "SELECT * FROM t FOR NO KEY UPDATE",
    },
    Feature {
        id: "locking-of",
        name: "OF in a locking clause",
        plural: false,
        support: [N, A, N, A],
        kinds: &[LOCKING_CLAUSE],
        detect: |element| keyword(element, OF_KW),
        example: "SELECT * FROM t FOR UPDATE OF t",
    },
    Feature {
        id: "skip-locked",
        name: "SKIP LOCKED and NOWAIT",
        plural: false,
        support: [N, A, A, A],
        kinds: &[LOCKING_CLAUSE],
        detect: |element| keyword(element, SKIP_KW).or_else(|| keyword(element, NOWAIT_KW)),
        example: "SELECT * FROM t FOR UPDATE SKIP LOCKED",
    },
    Feature {
        id: "lock-in-share-mode",
        name: "LOCK IN SHARE MODE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[LOCKING_CLAUSE],
        detect: |element| keyword(element, LOCK_KW).and_then(|_| node_range(element)),
        example: "SELECT * FROM t LOCK IN SHARE MODE",
    },
    Feature {
        id: "materialized-cte",
        name: "MATERIALIZED in WITH",
        plural: false,
        support: [A, N, N, A],
        kinds: &[CTE],
        detect: |element| keyword(element, MATERIALIZED_KW),
        example: "WITH x AS MATERIALIZED (SELECT 1) SELECT * FROM x",
    },
    Feature {
        id: "cte-search-cycle",
        name: "SEARCH and CYCLE in WITH",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CTE],
        detect: |element| keyword(element, SEARCH_KW).or_else(|| keyword(element, CYCLE_KW)),
        example: "WITH RECURSIVE x (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM x WHERE n < 3) CYCLE n SET c USING p SELECT * FROM x",
    },
    Feature {
        id: "data-modifying-cte",
        name: "INSERT, UPDATE and DELETE in WITH",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CTE],
        detect: |element| {
            node_of(element)?
                .children()
                .find(|child| matches!(child.kind(), INSERT_STMT | UPDATE_STMT | DELETE_STMT | MERGE_STMT))
                .map(|child| child.text_range())
        },
        example: "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
    },
    Feature {
        id: "with-insert",
        name: "WITH before INSERT",
        plural: false,
        support: [A, N, N, A],
        kinds: &[INSERT_STMT],
        detect: leading_with,
        example: "WITH x AS (SELECT 1 AS a) INSERT INTO t (a) SELECT a FROM x",
    },
    Feature {
        id: "with-update-delete",
        name: "WITH before UPDATE and DELETE",
        plural: false,
        support: [A, A, N, A],
        kinds: &[UPDATE_STMT, DELETE_STMT],
        detect: leading_with,
        example: "WITH x AS (SELECT 1 AS a) DELETE FROM t WHERE a IN (SELECT a FROM x)",
    },
    Feature {
        id: "values-without-row",
        name: "VALUES as a query without ROW",
        plural: false,
        support: [A, N, A, A],
        kinds: &[VALUES],
        detect: |element| {
            let node = node_of(element)?;
            if matches!(node.parent()?.kind(), INSERT_STMT | MERGE_WHEN_CLAUSE) {
                return None;
            }
            let bare = node
                .children()
                .filter(|child| child.kind() == ROW_EXPR)
                .any(|row| child_token(&row, ROW_KW).is_none());
            bare.then(|| first_token(node).map(|token| token.text_range()))?
        },
        example: "SELECT * FROM (VALUES (1, 2)) AS v",
    },
    Feature {
        id: "values-row",
        name: "VALUES with ROW",
        plural: false,
        support: [N, since(8, 0, 19), N, N],
        kinds: &[ROW_EXPR],
        detect: |element| (parent_kind(element)? == VALUES).then(|| keyword(element, ROW_KW))?,
        example: "VALUES ROW(1, 2)",
    },
    Feature {
        id: "table-statement",
        name: "TABLE as a query",
        plural: false,
        support: [N, since(8, 0, 19), N, A],
        kinds: &[TABLE_QUERY],
        detect: first_token_range,
        example: "TABLE t",
    },
    Feature {
        id: "parenthesized-set-operands",
        name: "A parenthesized query as an operand",
        plural: false,
        support: [N, A, A, A],
        kinds: &[PAREN_QUERY],
        detect: |element| {
            matches!(parent_kind(element)?, COMPOUND_SELECT | SELECT_STMT).then(|| first_token_range(element))?
        },
        example: "(SELECT a FROM t) UNION (SELECT a FROM u)",
    },
    Feature {
        id: "derived-column-aliases",
        name: "Column aliases for a subquery in FROM",
        plural: true,
        support: [N, A, A, A],
        kinds: &[ALIAS],
        detect: |element| {
            (parent_kind(element)? == DERIVED_TABLE)
                .then(|| child_node(node_of(element)?, NAME_LIST).map(|list| list.text_range()))?
        },
        example: "SELECT * FROM (SELECT 1) AS d (x)",
    },
    Feature {
        id: "limit-expressions",
        name: "Expressions in LIMIT and OFFSET",
        plural: true,
        support: [A, N, N, A],
        kinds: &[LIMIT_CLAUSE, OFFSET_CLAUSE],
        detect: |element| {
            node_of(element)?
                .children()
                .find(|child| !matches!(child.kind(), LITERAL | PARAMETER | VARIABLE_REF | COLUMN_REF))
                .map(|child| child.text_range())
        },
        example: "SELECT * FROM t LIMIT 1 + 1",
    },
    // Changing data
    Feature {
        id: "insert-returning",
        name: "RETURNING in INSERT",
        plural: false,
        support: [A, N, A, A],
        kinds: &[RETURNING_CLAUSE],
        detect: |element| (parent_kind(element)? == INSERT_STMT).then(|| first_token_range(element))?,
        example: "INSERT INTO t (a) VALUES (1) RETURNING a",
    },
    Feature {
        id: "update-returning",
        name: "RETURNING in UPDATE",
        plural: false,
        support: [A, N, N, A],
        kinds: &[RETURNING_CLAUSE],
        detect: |element| (parent_kind(element)? == UPDATE_STMT).then(|| first_token_range(element))?,
        example: "UPDATE t SET a = 1 RETURNING a",
    },
    Feature {
        id: "delete-returning",
        name: "RETURNING in DELETE",
        plural: false,
        support: [A, N, A, A],
        kinds: &[RETURNING_CLAUSE],
        detect: |element| (parent_kind(element)? == DELETE_STMT).then(|| first_token_range(element))?,
        example: "DELETE FROM t RETURNING a",
    },
    Feature {
        id: "on-conflict",
        name: "ON CONFLICT",
        plural: false,
        support: [A, N, N, A],
        kinds: &[UPSERT_CLAUSE],
        detect: |element| keyword_pair(element, ON_KW, CONFLICT_KW),
        example: "INSERT INTO t (a) VALUES (1) ON CONFLICT DO NOTHING",
    },
    Feature {
        id: "on-duplicate-key-update",
        name: "ON DUPLICATE KEY UPDATE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[ON_DUPLICATE_KEY_CLAUSE],
        detect: |element| keyword_pair(element, ON_KW, DUPLICATE_KW),
        example: "INSERT INTO t (a) VALUES (1) ON DUPLICATE KEY UPDATE a = 2",
    },
    Feature {
        id: "values-function",
        name: "VALUES() in ON DUPLICATE KEY UPDATE",
        plural: false,
        support: [N, deprecated(8, 0, 20), A, N],
        kinds: &[FUNCTION_CALL],
        detect: |element| {
            let node = node_of(element)?;
            let inside = node
                .ancestors()
                .any(|ancestor| ancestor.kind() == ON_DUPLICATE_KEY_CLAUSE);
            (inside && last_name(node)? == "values").then(|| node.text_range())
        },
        example: "INSERT INTO t (a) VALUES (1) ON DUPLICATE KEY UPDATE a = VALUES(a)",
    },
    Feature {
        id: "insert-row-alias",
        name: "A row alias in INSERT",
        plural: false,
        support: [N, since(8, 0, 19), N, N],
        kinds: &[ALIAS],
        detect: |element| {
            let node = node_of(element)?;
            if node.parent()?.kind() != INSERT_STMT {
                return None;
            }
            let previous = node.prev_sibling()?.kind();
            (matches!(previous, VALUES | SET_CLAUSE) || is_query(previous)).then(|| node.text_range())
        },
        example: "INSERT INTO t (a) VALUES (1) AS new ON DUPLICATE KEY UPDATE a = new.a",
    },
    Feature {
        id: "insert-table-alias",
        name: "A table alias in INSERT",
        plural: false,
        support: [A, N, N, A],
        kinds: &[ALIAS],
        detect: |element| {
            let node = node_of(element)?;
            if node.parent()?.kind() != INSERT_STMT {
                return None;
            }
            (node.prev_sibling()?.kind() == QUALIFIED_NAME).then(|| node.text_range())
        },
        example: "INSERT INTO t AS x (a) VALUES (1)",
    },
    Feature {
        id: "statement-modifiers",
        name: "IGNORE, QUICK and the priority modifiers",
        plural: true,
        support: [N, A, A, N],
        kinds: &[INSERT_STMT, UPDATE_STMT, DELETE_STMT],
        // SQLite's `OR IGNORE` is its own row.
        detect: |element| {
            modifier(
                element,
                &[IGNORE_KW, LOW_PRIORITY_KW, HIGH_PRIORITY_KW, DELAYED_KW, QUICK_KW],
            )
            .filter(|_| modifier(element, &[OR_KW]).is_none())
        },
        example: "INSERT IGNORE INTO t (a) VALUES (1)",
    },
    Feature {
        id: "or-conflict-action",
        name: "INSERT OR and UPDATE OR",
        plural: false,
        support: [A, N, N, N],
        kinds: &[INSERT_STMT, UPDATE_STMT],
        detect: |element| modifier(element, &[OR_KW]),
        example: "INSERT OR REPLACE INTO t (a) VALUES (1)",
    },
    Feature {
        id: "replace",
        name: "REPLACE",
        plural: false,
        support: [A, A, A, N],
        kinds: &[INSERT_STMT],
        detect: |element| keyword(element, REPLACE_KW).filter(|_| modifier(element, &[OR_KW]).is_none()),
        example: "REPLACE INTO t (a) VALUES (1)",
    },
    Feature {
        id: "insert-set",
        name: "INSERT with SET",
        plural: false,
        support: [N, A, A, N],
        kinds: &[INSERT_STMT],
        detect: |element| child_node(node_of(element)?, SET_CLAUSE).map(|set| set.text_range()),
        example: "INSERT INTO t SET a = 1",
    },
    Feature {
        id: "insert-value-keyword",
        name: "VALUE in INSERT",
        plural: false,
        support: [N, A, A, N],
        kinds: &[VALUES],
        detect: |element| keyword(element, VALUE_KW),
        example: "INSERT INTO t (a) VALUE (1)",
    },
    Feature {
        id: "empty-rows",
        name: "An empty row in VALUES",
        plural: false,
        support: [N, A, A, N],
        kinds: &[ROW_EXPR],
        detect: |element| {
            let node = node_of(element)?;
            (node.parent()?.kind() == VALUES && node.children().next().is_none()).then(|| node.text_range())
        },
        example: "INSERT INTO t () VALUES ()",
    },
    Feature {
        id: "default-values",
        name: "DEFAULT VALUES",
        plural: true,
        support: [A, N, N, A],
        kinds: &[INSERT_STMT],
        detect: |element| keyword_pair(element, DEFAULT_KW, VALUES_KW),
        example: "INSERT INTO t DEFAULT VALUES",
    },
    Feature {
        id: "update-from",
        name: "FROM in UPDATE",
        plural: false,
        support: [A, N, N, A],
        kinds: &[UPDATE_STMT],
        detect: |element| {
            let from = child_node(node_of(element)?, FROM_CLAUSE)?;
            first_token(&from).map(|token| token.text_range())
        },
        example: "UPDATE t SET a = u.a FROM u WHERE t.id = u.id",
    },
    Feature {
        id: "multi-table-update",
        name: "UPDATE of several tables",
        plural: false,
        support: [N, A, A, N],
        kinds: &[UPDATE_STMT],
        detect: |element| {
            let mut range = None;
            for child in node_of(element)?.children_with_tokens() {
                match child.kind() {
                    SET_CLAUSE => break,
                    JOIN_EXPR | COMMA => range = Some(child.text_range()),
                    _ => {}
                }
            }
            range
        },
        example: "UPDATE t JOIN u ON t.id = u.id SET t.a = u.a",
    },
    Feature {
        id: "delete-using",
        name: "USING in DELETE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[DELETE_STMT],
        detect: |element| {
            let using = child_node(node_of(element)?, USING_CLAUSE)?;
            first_token(&using).map(|token| token.text_range())
        },
        example: "DELETE FROM t USING u WHERE t.id = u.id",
    },
    Feature {
        id: "multi-table-delete",
        name: "DELETE from several tables",
        plural: false,
        support: [N, A, A, N],
        kinds: &[DELETE_STMT],
        detect: |element| {
            let node = node_of(element)?;
            if let Some(targets) = child_node(node, QUALIFIED_NAME) {
                return Some(targets.text_range());
            }
            let from = child_node(node, FROM_CLAUSE)?;
            from.children_with_tokens()
                .find(|child| matches!(child.kind(), JOIN_EXPR | COMMA))
                .map(|child| child.text_range())
        },
        example: "DELETE t FROM t JOIN u ON t.id = u.id",
    },
    Feature {
        id: "update-delete-order-limit",
        name: "ORDER BY and LIMIT in UPDATE and DELETE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[ORDER_BY_CLAUSE, LIMIT_CLAUSE],
        detect: |element| {
            matches!(parent_kind(element)?, UPDATE_STMT | DELETE_STMT).then(|| first_token_range(element))?
        },
        example: "DELETE FROM t ORDER BY a LIMIT 1",
    },
    Feature {
        id: "merge",
        name: "MERGE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[MERGE_STMT],
        detect: |element| keyword(element, MERGE_KW),
        example: "MERGE INTO t USING s ON t.a = s.a WHEN MATCHED THEN DELETE",
    },
    Feature {
        id: "truncate",
        name: "TRUNCATE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[TRUNCATE_STMT],
        detect: first_token_range,
        example: "TRUNCATE TABLE t",
    },
    // Tables
    Feature {
        id: "auto-increment",
        name: "AUTO_INCREMENT",
        plural: false,
        support: [N, A, A, N],
        kinds: &[AUTO_INCREMENT_KW],
        detect: token_range,
        example: "CREATE TABLE n (id INT AUTO_INCREMENT PRIMARY KEY)",
    },
    Feature {
        id: "autoincrement",
        name: "AUTOINCREMENT",
        plural: false,
        support: [A, N, N, N],
        kinds: &[AUTOINCREMENT_KW],
        detect: token_range,
        example: "CREATE TABLE n (id INTEGER PRIMARY KEY AUTOINCREMENT)",
    },
    Feature {
        id: "identity-columns",
        name: "Identity columns",
        plural: true,
        support: [N, N, N, A],
        kinds: &[COLUMN_CONSTRAINT],
        detect: |element| keyword(element, IDENTITY_KW).and_then(|_| node_range(element)),
        example: "CREATE TABLE n (id INT GENERATED ALWAYS AS IDENTITY)",
    },
    Feature {
        id: "generated-as-shorthand",
        name: "Generated columns without GENERATED ALWAYS",
        plural: true,
        support: [A, A, A, N],
        kinds: &[COLUMN_CONSTRAINT],
        detect: |element| (first_token(node_of(element)?)?.kind() == AS_KW).then(|| node_range(element))?,
        example: "CREATE TABLE n (a INT, b INT AS (a * 2))",
    },
    Feature {
        id: "unsigned-types",
        name: "UNSIGNED and SIGNED",
        plural: false,
        support: [A, A, A, N],
        kinds: &[TYPE],
        detect: |element| keyword(element, UNSIGNED_KW).or_else(|| keyword(element, SIGNED_KW)),
        example: "CREATE TABLE n (a INT UNSIGNED)",
    },
    Feature {
        id: "zerofill",
        name: "ZEROFILL",
        plural: false,
        support: [A, deprecated(8, 0, 17), A, N],
        kinds: &[TYPE],
        detect: |element| keyword(element, ZEROFILL_KW),
        example: "CREATE TABLE n (a INT ZEROFILL)",
    },
    Feature {
        id: "integer-display-width",
        name: "A display width of an integer type",
        plural: false,
        support: [A, deprecated(8, 0, 17), A, N],
        kinds: &[TYPE],
        detect: |element| {
            let node = node_of(element)?;
            let name = last_name(node)?;
            let integer = matches!(
                name.as_str(),
                "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint"
            );
            integer.then(|| child_node(node, TYPE_ARGS).map(|args| args.text_range()))?
        },
        example: "CREATE TABLE n (a INT(11))",
    },
    Feature {
        id: "enum-set-types",
        name: "ENUM and SET types",
        plural: true,
        support: [N, A, A, N],
        kinds: &[TYPE],
        detect: |element| {
            let node = node_of(element)?;
            child_node(node, TYPE_ARGS)?;
            let named = last_name(node).is_some_and(|name| name == "enum" || name == "set");
            named.then(|| node.text_range())
        },
        example: "CREATE TABLE n (a ENUM('x', 'y'))",
    },
    Feature {
        id: "array-types",
        name: "Array types",
        plural: true,
        support: [N, N, N, A],
        kinds: &[TYPE],
        detect: |element| keyword(element, LBRACKET).or_else(|| keyword(element, ARRAY_KW)),
        example: "CREATE TABLE n (a INT[])",
    },
    Feature {
        id: "character-set-attributes",
        name: "CHARACTER SET on a column",
        plural: false,
        support: [N, A, A, N],
        kinds: &[TYPE, COLUMN_CONSTRAINT],
        detect: |element| keyword_pair(element, CHARACTER_KW, SET_KW).or_else(|| keyword(element, CHARSET_KW)),
        example: "CREATE TABLE n (a VARCHAR(10) CHARACTER SET utf8mb4)",
    },
    Feature {
        id: "column-comments",
        name: "COMMENT on a column",
        plural: false,
        support: [N, A, A, N],
        kinds: &[COLUMN_CONSTRAINT],
        detect: |element| (first_token(node_of(element)?)?.kind() == COMMENT_KW).then(|| node_range(element))?,
        example: "CREATE TABLE n (a INT COMMENT 'x')",
    },
    Feature {
        id: "on-update",
        name: "ON UPDATE on a column",
        plural: false,
        support: [N, A, A, N],
        kinds: &[COLUMN_CONSTRAINT],
        detect: |element| keyword_pair(element, ON_KW, UPDATE_KW).and_then(|_| node_range(element)),
        example: "CREATE TABLE n (a TIMESTAMP DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP)",
    },
    Feature {
        id: "invisible-columns",
        name: "Invisible columns",
        plural: true,
        support: [N, since(8, 0, 23), A, N],
        kinds: &[COLUMN_CONSTRAINT],
        detect: |element| keyword(element, INVISIBLE_KW).or_else(|| keyword(element, VISIBLE_KW)),
        example: "CREATE TABLE n (a INT, b INT INVISIBLE)",
    },
    Feature {
        id: "conflict-clauses",
        name: "ON CONFLICT in a constraint",
        plural: false,
        support: [A, N, N, N],
        kinds: &[COLUMN_CONSTRAINT, TABLE_CONSTRAINT],
        detect: |element| keyword_pair(element, ON_KW, CONFLICT_KW),
        example: "CREATE TABLE n (a INT NOT NULL ON CONFLICT IGNORE)",
    },
    Feature {
        id: "deferrable-constraints",
        name: "Deferrable constraints",
        plural: true,
        support: [A, N, N, A],
        kinds: &[DEFERRABLE_KW, INITIALLY_KW],
        detect: token_range,
        example: "CREATE TABLE n (a INT REFERENCES u (a) DEFERRABLE INITIALLY DEFERRED)",
    },
    Feature {
        id: "enforced-constraints",
        name: "ENFORCED and NOT ENFORCED",
        plural: false,
        support: [N, since(8, 0, 16), N, A],
        kinds: &[ENFORCED_KW],
        detect: token_range,
        example: "CREATE TABLE n (a INT, CHECK (a > 0) NOT ENFORCED)",
    },
    Feature {
        id: "nulls-not-distinct",
        name: "NULLS NOT DISTINCT",
        plural: false,
        support: [N, N, N, A],
        kinds: &[TABLE_CONSTRAINT, COLUMN_CONSTRAINT, CREATE_INDEX_STMT],
        detect: |element| {
            let nulls = keyword(element, NULLS_KW)?;
            let end = keyword(element, DISTINCT_KW).map_or(nulls.end(), |distinct| distinct.end());
            Some(TextRange::new(nulls.start(), end))
        },
        example: "CREATE TABLE n (a INT, UNIQUE NULLS NOT DISTINCT (a))",
    },
    Feature {
        id: "exclusion-constraints",
        name: "Exclusion constraints",
        plural: true,
        support: [N, N, N, A],
        kinds: &[TABLE_CONSTRAINT],
        detect: |element| keyword(element, EXCLUDE_KW),
        example: "CREATE TABLE n (a INT, EXCLUDE USING gist (a WITH =))",
    },
    Feature {
        id: "inline-indexes",
        name: "Indexes in CREATE TABLE",
        plural: true,
        support: [N, A, A, N],
        kinds: &[TABLE_CONSTRAINT],
        detect: |element| {
            let words: Vec<SyntaxToken> = tokens(node_of(element)?).collect();
            let start = usize::from(words.first()?.kind() == CONSTRAINT_KW && words.len() > 1);
            let first = words.get(start)?;
            let unique_index = first.kind() == UNIQUE_KW
                && words
                    .get(start + 1)
                    .is_some_and(|next| matches!(next.kind(), INDEX_KW | KEY_KW));
            let index = matches!(first.kind(), INDEX_KW | KEY_KW | FULLTEXT_KW | SPATIAL_KW) || unique_index;
            index.then(|| first.text_range())
        },
        example: "CREATE TABLE n (a INT, KEY idx (a))",
    },
    Feature {
        id: "table-options",
        name: "Table options",
        plural: true,
        support: [N, A, A, N],
        kinds: &[TABLE_OPTION],
        detect: |element| {
            let node = node_of(element)?;
            let word = first_token(node)?.text().to_ascii_lowercase();
            let mysql = matches!(
                word.as_str(),
                "engine"
                    | "default"
                    | "charset"
                    | "character"
                    | "collate"
                    | "auto_increment"
                    | "comment"
                    | "row_format"
                    | "stats_persistent"
                    | "key_block_size"
                    | "avg_row_length"
                    | "max_rows"
                    | "min_rows"
                    | "pack_keys"
                    | "checksum"
                    | "compression"
                    | "encryption"
            );
            mysql.then(|| node.text_range())
        },
        example: "CREATE TABLE n (a INT) ENGINE = InnoDB",
    },
    Feature {
        id: "without-rowid-strict",
        name: "WITHOUT ROWID and STRICT",
        plural: false,
        support: [A, N, N, N],
        kinds: &[TABLE_OPTION],
        detect: |element| {
            let node = node_of(element)?;
            let first = first_token(node)?;
            let rowid = first.kind() == WITHOUT_KW && tokens(node).nth(1).is_some_and(|token| token.kind() == ROWID_KW);
            (rowid || first.kind() == STRICT_KW).then(|| node.text_range())
        },
        example: "CREATE TABLE n (a INT PRIMARY KEY) WITHOUT ROWID",
    },
    Feature {
        id: "inherits",
        name: "INHERITS",
        plural: false,
        support: [N, N, N, A],
        kinds: &[TABLE_OPTION],
        detect: |element| keyword(element, INHERITS_KW).and_then(|_| node_range(element)),
        example: "CREATE TABLE n (a INT) INHERITS (u)",
    },
    Feature {
        id: "unlogged-tables",
        name: "Unlogged tables",
        plural: true,
        support: [N, N, N, A],
        kinds: &[CREATE_TABLE_STMT],
        detect: |element| keyword(element, UNLOGGED_KW),
        example: "CREATE UNLOGGED TABLE n (a INT)",
    },
    Feature {
        id: "create-or-replace-table",
        name: "CREATE OR REPLACE TABLE",
        plural: false,
        support: [N, N, A, N],
        kinds: &[CREATE_TABLE_STMT],
        detect: |element| keyword_pair(element, OR_KW, REPLACE_KW),
        example: "CREATE OR REPLACE TABLE n (a INT)",
    },
    Feature {
        id: "create-table-like",
        name: "CREATE TABLE ... LIKE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[LIKE_CLAUSE],
        detect: |element| (parent_kind(element)? == CREATE_TABLE_STMT).then(|| node_range(element))?,
        example: "CREATE TABLE n LIKE u",
    },
    Feature {
        id: "like-in-columns",
        name: "LIKE among the columns",
        plural: false,
        support: [N, N, N, A],
        kinds: &[LIKE_CLAUSE],
        detect: |element| (parent_kind(element)? == TABLE_ELEMENT_LIST).then(|| node_range(element))?,
        example: "CREATE TABLE n (LIKE u INCLUDING ALL)",
    },
    Feature {
        id: "table-partitioning",
        name: "PARTITION BY in CREATE TABLE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[TABLE_PARTITION_CLAUSE],
        detect: |element| keyword_pair(element, PARTITION_KW, BY_KW),
        example: "CREATE TABLE n (a INT) PARTITION BY HASH (a)",
    },
    Feature {
        id: "partition-definitions",
        name: "Partition definitions in CREATE TABLE",
        plural: true,
        support: [N, A, A, N],
        kinds: &[PARTITION_DEF],
        detect: node_range,
        example: "CREATE TABLE n (a INT) PARTITION BY RANGE (a) (PARTITION p0 VALUES LESS THAN (10))",
    },
    Feature {
        id: "partition-of",
        name: "PARTITION OF",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CREATE_TABLE_STMT],
        detect: |element| keyword_pair(element, PARTITION_KW, OF_KW),
        example: "CREATE TABLE t1 PARTITION OF t FOR VALUES FROM (1) TO (10)",
    },
    Feature {
        id: "untyped-columns",
        name: "Columns without a type",
        plural: true,
        support: [A, N, N, N],
        kinds: &[COLUMN_DEF],
        detect: |element| {
            let node = node_of(element)?;
            child_node(node, TYPE)
                .is_none()
                .then(|| child_node(node, NAME).map(|name| name.text_range()))?
        },
        example: "CREATE TABLE n (a, b)",
    },
    // Changing tables
    Feature {
        id: "alter-multiple-actions",
        name: "Several actions in one ALTER TABLE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[ALTER_TABLE_STMT],
        detect: |element| keyword(element, COMMA),
        example: "ALTER TABLE t ADD COLUMN a INT, ADD COLUMN b INT",
    },
    Feature {
        id: "alter-column",
        name: "ALTER COLUMN",
        plural: false,
        support: [N, A, A, A],
        kinds: &[ALTER_COLUMN_ACTION],
        detect: first_token_range,
        example: "ALTER TABLE t ALTER COLUMN a SET DEFAULT 1",
    },
    Feature {
        id: "alter-column-type",
        name: "Changing a column's type with ALTER COLUMN",
        plural: false,
        support: [N, N, N, A],
        kinds: &[ALTER_COLUMN_ACTION],
        detect: |element| keyword(element, TYPE_KW).and_then(|_| node_range(element)),
        example: "ALTER TABLE t ALTER COLUMN a TYPE bigint",
    },
    Feature {
        id: "modify-column",
        name: "MODIFY and CHANGE",
        plural: true,
        support: [N, A, A, N],
        kinds: &[MODIFY_COLUMN_ACTION],
        detect: first_token_range,
        example: "ALTER TABLE t MODIFY a BIGINT",
    },
    Feature {
        id: "alter-constraints",
        name: "Adding and dropping constraints",
        plural: false,
        support: [N, A, A, A],
        kinds: &[ADD_CONSTRAINT_ACTION, DROP_CONSTRAINT_ACTION],
        detect: first_token_range,
        example: "ALTER TABLE t ADD CONSTRAINT u UNIQUE (a)",
    },
    Feature {
        id: "add-column-if-not-exists",
        name: "ADD COLUMN IF NOT EXISTS",
        plural: false,
        support: [N, N, A, A],
        kinds: &[ADD_COLUMN_ACTION],
        detect: |element| keyword(element, IF_KW),
        example: "ALTER TABLE t ADD COLUMN IF NOT EXISTS a INT",
    },
    Feature {
        id: "drop-column-if-exists",
        name: "DROP COLUMN IF EXISTS",
        plural: false,
        support: [N, N, A, A],
        kinds: &[DROP_COLUMN_ACTION],
        detect: |element| keyword(element, IF_KW),
        example: "ALTER TABLE t DROP COLUMN IF EXISTS a",
    },
    Feature {
        id: "column-position",
        name: "FIRST and AFTER",
        plural: true,
        support: [N, A, A, N],
        kinds: &[FIRST_KW, AFTER_KW],
        detect: |element| {
            matches!(parent_kind(element)?, ADD_COLUMN_ACTION | MODIFY_COLUMN_ACTION).then(|| token_range(element))?
        },
        example: "ALTER TABLE t ADD COLUMN a INT FIRST",
    },
    Feature {
        id: "rename-table-statement",
        name: "RENAME TABLE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[RENAME_TABLE_STMT],
        detect: first_token_range,
        example: "RENAME TABLE a TO b",
    },
    // Other objects
    Feature {
        id: "create-index-if-not-exists",
        name: "CREATE INDEX IF NOT EXISTS",
        plural: false,
        support: [A, N, A, A],
        kinds: &[CREATE_INDEX_STMT],
        detect: |element| keyword(element, IF_KW),
        example: "CREATE INDEX IF NOT EXISTS i ON t (a)",
    },
    Feature {
        id: "expression-indexes",
        name: "Indexes on expressions",
        plural: true,
        support: [A, since(8, 0, 13), N, A],
        kinds: &[INDEX_COLUMN],
        detect: |element| {
            let node = node_of(element)?;
            let index = node
                .ancestors()
                .any(|ancestor| matches!(ancestor.kind(), CREATE_INDEX_STMT | TABLE_CONSTRAINT));
            let elsewhere = node
                .ancestors()
                .any(|ancestor| matches!(ancestor.kind(), CONFLICT_TARGET | TABLE_PARTITION_CLAUSE));
            let value = node.children().next()?;
            (index && !elsewhere && value.kind() != COLUMN_REF).then(|| value.text_range())
        },
        example: "CREATE INDEX i ON t ((a + b))",
    },
    Feature {
        id: "index-method-before-table",
        name: "USING before ON in CREATE INDEX",
        plural: false,
        support: [N, A, A, N],
        kinds: &[CREATE_INDEX_STMT],
        detect: |element| {
            let words: Vec<SyntaxToken> = tokens(node_of(element)?).collect();
            let using = words.iter().position(|token| token.kind() == USING_KW)?;
            let on = words.iter().position(|token| token.kind() == ON_KW)?;
            (using < on).then(|| words[using].text_range())
        },
        example: "CREATE INDEX i USING BTREE ON t (a)",
    },
    Feature {
        id: "index-method-after-table",
        name: "USING between the table and the columns of CREATE INDEX",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CREATE_INDEX_STMT],
        detect: |element| {
            let node = node_of(element)?;
            let mut seen_on = false;
            for child in node.children_with_tokens() {
                match child.kind() {
                    ON_KW => seen_on = true,
                    USING_KW if seen_on => return Some(child.text_range()),
                    INDEX_COLUMN_LIST => return None,
                    _ => {}
                }
            }
            None
        },
        example: "CREATE INDEX i ON t USING btree (a)",
    },
    Feature {
        id: "index-method-after-columns",
        name: "USING after the columns of an index",
        plural: false,
        support: [N, A, A, N],
        kinds: &[CREATE_INDEX_STMT, TABLE_CONSTRAINT],
        detect: |element| {
            let node = node_of(element)?;
            let mut seen_columns = false;
            for child in node.children_with_tokens() {
                match child.kind() {
                    INDEX_COLUMN_LIST => seen_columns = true,
                    USING_KW if seen_columns => return Some(child.text_range()),
                    _ => {}
                }
            }
            None
        },
        example: "CREATE INDEX i ON t (a) USING BTREE",
    },
    Feature {
        id: "partial-indexes",
        name: "Partial indexes",
        plural: true,
        support: [A, N, N, A],
        kinds: &[WHERE_CLAUSE],
        detect: |element| (parent_kind(element)? == CREATE_INDEX_STMT).then(|| first_token_range(element))?,
        example: "CREATE INDEX i ON t (a) WHERE a > 0",
    },
    Feature {
        id: "index-include",
        name: "INCLUDE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[INCLUDE_KW],
        detect: token_range,
        example: "CREATE INDEX i ON t (a) INCLUDE (b)",
    },
    Feature {
        id: "concurrently",
        name: "CONCURRENTLY",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CONCURRENTLY_KW],
        detect: token_range,
        example: "CREATE INDEX CONCURRENTLY i ON t (a)",
    },
    Feature {
        id: "create-view-if-not-exists",
        name: "CREATE VIEW IF NOT EXISTS",
        plural: false,
        support: [A, N, A, N],
        kinds: &[CREATE_VIEW_STMT],
        detect: |element| {
            keyword(element, MATERIALIZED_KW)
                .is_none()
                .then(|| keyword(element, IF_KW))?
        },
        example: "CREATE VIEW IF NOT EXISTS v AS SELECT 1",
    },
    Feature {
        id: "create-or-replace-view",
        name: "CREATE OR REPLACE VIEW",
        plural: false,
        support: [N, A, A, A],
        kinds: &[CREATE_VIEW_STMT],
        detect: |element| keyword_pair(element, OR_KW, REPLACE_KW),
        example: "CREATE OR REPLACE VIEW v AS SELECT 1",
    },
    Feature {
        id: "check-option",
        name: "WITH CHECK OPTION",
        plural: false,
        support: [N, A, A, A],
        kinds: &[CREATE_VIEW_STMT],
        detect: |element| {
            let check = keyword(element, CHECK_KW)?;
            let end = keyword(element, OPTION_KW).map_or(check.end(), |option| option.end());
            Some(TextRange::new(check.start(), end))
        },
        example: "CREATE VIEW v AS SELECT a FROM t WITH CHECK OPTION",
    },
    Feature {
        id: "materialized-views",
        name: "Materialized views",
        plural: true,
        support: [N, N, N, A],
        kinds: &[CREATE_VIEW_STMT, REFRESH_STMT],
        detect: |element| keyword(element, MATERIALIZED_KW),
        example: "CREATE MATERIALIZED VIEW v AS SELECT 1",
    },
    Feature {
        id: "create-schema",
        name: "CREATE SCHEMA and CREATE DATABASE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[CREATE_SCHEMA_STMT],
        detect: |element| keyword(element, SCHEMA_KW).or_else(|| keyword(element, DATABASE_KW)),
        example: "CREATE SCHEMA s",
    },
    Feature {
        id: "sequences",
        name: "Sequences",
        plural: true,
        support: [N, N, A, A],
        kinds: &[CREATE_SEQUENCE_STMT],
        detect: |element| keyword(element, SEQUENCE_KW),
        example: "CREATE SEQUENCE s",
    },
    Feature {
        id: "create-type",
        name: "CREATE TYPE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CREATE_TYPE_STMT],
        detect: |element| keyword(element, TYPE_KW),
        example: "CREATE TYPE mood AS ENUM ('sad', 'happy')",
    },
    Feature {
        id: "create-domain",
        name: "CREATE DOMAIN",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CREATE_DOMAIN_STMT],
        detect: |element| keyword(element, DOMAIN_KW),
        example: "CREATE DOMAIN positive AS INT CHECK (VALUE > 0)",
    },
    Feature {
        id: "extensions",
        name: "Extensions",
        plural: true,
        support: [N, N, N, A],
        kinds: &[CREATE_EXTENSION_STMT],
        detect: |element| keyword(element, EXTENSION_KW),
        example: "CREATE EXTENSION IF NOT EXISTS pgcrypto",
    },
    Feature {
        id: "drop-several",
        name: "Dropping several objects at once",
        plural: false,
        support: [N, A, A, A],
        kinds: &[DROP_STMT],
        detect: |element| keyword(element, COMMA),
        example: "DROP TABLE a, b",
    },
    Feature {
        id: "drop-cascade",
        name: "CASCADE and RESTRICT",
        plural: true,
        support: [N, A, A, A],
        kinds: &[DROP_STMT],
        detect: |element| keyword(element, CASCADE_KW).or_else(|| keyword(element, RESTRICT_KW)),
        example: "DROP TABLE t CASCADE",
    },
    Feature {
        id: "drop-index-on-table",
        name: "DROP INDEX ... ON",
        plural: false,
        support: [N, A, A, N],
        kinds: &[DROP_STMT],
        detect: |element| keyword(element, INDEX_KW).and_then(|_| from_keyword(element, ON_KW)),
        example: "DROP INDEX i ON t",
    },
    Feature {
        id: "drop-index-without-table",
        name: "DROP INDEX without ON",
        plural: false,
        support: [A, N, N, A],
        kinds: &[DROP_STMT],
        detect: |element| {
            let index = keyword(element, INDEX_KW)?;
            let whole = node_range(element)?;
            keyword(element, ON_KW)
                .is_none()
                .then(|| TextRange::new(index.start(), whole.end()))
        },
        example: "DROP INDEX i",
    },
    Feature {
        id: "comment-on",
        name: "COMMENT ON",
        plural: false,
        support: [N, N, N, A],
        kinds: &[COMMENT_STMT],
        detect: |element| keyword_pair(element, COMMENT_KW, ON_KW),
        example: "COMMENT ON TABLE t IS 'x'",
    },
    // Routines and triggers
    Feature {
        id: "routines",
        name: "Functions and procedures",
        plural: true,
        support: [N, A, A, A],
        kinds: &[CREATE_FUNCTION_STMT],
        detect: |element| keyword(element, FUNCTION_KW).or_else(|| keyword(element, PROCEDURE_KW)),
        example: "CREATE PROCEDURE p() BEGIN END",
    },
    Feature {
        id: "create-or-replace-routine",
        name: "CREATE OR REPLACE for a function or a procedure",
        plural: false,
        support: [N, N, A, A],
        kinds: &[CREATE_FUNCTION_STMT],
        detect: |element| keyword_pair(element, OR_KW, REPLACE_KW),
        example: "CREATE OR REPLACE PROCEDURE p() BEGIN END",
    },
    Feature {
        id: "create-or-replace-trigger",
        name: "CREATE OR REPLACE TRIGGER",
        plural: false,
        support: [N, N, A, A],
        kinds: &[CREATE_TRIGGER_STMT],
        detect: |element| keyword_pair(element, OR_KW, REPLACE_KW),
        example: "CREATE OR REPLACE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
    },
    Feature {
        id: "trigger-functions",
        name: "EXECUTE FUNCTION in a trigger",
        plural: false,
        support: [N, N, N, A],
        kinds: &[CREATE_TRIGGER_STMT],
        detect: |element| keyword(element, EXECUTE_KW),
        example: "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
    },
    Feature {
        id: "trigger-when",
        name: "WHEN in a trigger",
        plural: false,
        support: [A, N, N, A],
        kinds: &[CREATE_TRIGGER_STMT],
        detect: |element| keyword(element, WHEN_KW),
        example: "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW WHEN (NEW.a > 0) EXECUTE FUNCTION f()",
    },
    Feature {
        id: "statement-triggers",
        name: "Triggers for each statement",
        plural: true,
        support: [N, N, N, A],
        kinds: &[CREATE_TRIGGER_STMT],
        detect: |element| keyword(element, STATEMENT_KW),
        example: "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH STATEMENT EXECUTE FUNCTION f()",
    },
    Feature {
        id: "compound-statements",
        name: "Compound statements outside a routine",
        plural: true,
        support: [N, N, A, N],
        kinds: &[BLOCK, IF_STMT, CASE_STMT, LOOP_STMT, WHILE_STMT, REPEAT_STMT],
        detect: |element| {
            let node = node_of(element)?;
            let nested = node.ancestors().skip(1).any(|ancestor| {
                matches!(
                    ancestor.kind(),
                    BLOCK | IF_STMT | CASE_STMT | LOOP_STMT | WHILE_STMT | REPEAT_STMT
                )
            });
            (!nested && !in_routine(node)).then(|| first_token(node).map(|token| token.text_range()))?
        },
        example: "BEGIN NOT ATOMIC SELECT 1; END",
    },
    Feature {
        id: "routine-statement-bodies",
        name: "A routine body of statements",
        plural: false,
        support: [N, A, A, N],
        kinds: &[ROUTINE_BODY],
        detect: |element| {
            let node = node_of(element)?;
            if node.parent()?.kind() != CREATE_FUNCTION_STMT {
                return None;
            }
            let body = node.children().next()?;
            let atomic =
                body.kind() == BLOCK && child_token(&body, ATOMIC_KW).is_some() && child_token(&body, NOT_KW).is_none();
            (!atomic && body.kind() != RETURN_STMT).then(|| first_token(&body).map(|token| token.text_range()))?
        },
        example: "CREATE PROCEDURE p() SELECT 1",
    },
    Feature {
        id: "begin-atomic",
        name: "BEGIN ATOMIC",
        plural: false,
        support: [N, N, N, A],
        kinds: &[BLOCK],
        detect: |element| {
            let node = node_of(element)?;
            (child_token(node, NOT_KW).is_none()).then(|| keyword_pair(element, BEGIN_KW, ATOMIC_KW))?
        },
        example: "CREATE PROCEDURE p() LANGUAGE sql BEGIN ATOMIC SELECT 1; END",
    },
    Feature {
        id: "routine-string-bodies",
        name: "A routine body in a string",
        plural: false,
        support: [N, N, N, A],
        kinds: &[ROUTINE_BODY],
        detect: |element| {
            let node = node_of(element)?;
            let token = first_token(node)?;
            (node.parent()?.kind() == CREATE_FUNCTION_STMT && token.kind().is_string()).then(|| token.text_range())
        },
        example: "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
    },
    Feature {
        id: "routine-characteristics",
        name: "DETERMINISTIC and the SQL data access of a routine",
        plural: true,
        support: [N, A, A, N],
        kinds: &[CREATE_FUNCTION_STMT],
        detect: |element| {
            tokens(node_of(element)?)
                .find(|token| {
                    let text = token.text();
                    ["deterministic", "contains", "reads", "modifies"]
                        .iter()
                        .any(|word| text.eq_ignore_ascii_case(word))
                })
                .map(|token| token.text_range())
        },
        example: "CREATE FUNCTION f() RETURNS INT DETERMINISTIC RETURN 1",
    },
    Feature {
        id: "trigger-statement-bodies",
        name: "A trigger body of statements",
        plural: false,
        support: [A, A, A, N],
        kinds: &[ROUTINE_BODY],
        detect: |element| {
            let node = node_of(element)?;
            let body = node.children().next()?;
            (node.parent()?.kind() == CREATE_TRIGGER_STMT && body.kind() != FUNCTION_CALL)
                .then(|| first_token(&body).map(|token| token.text_range()))?
        },
        example: "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW BEGIN DELETE FROM u; END",
    },
    Feature {
        id: "trigger-without-each-row",
        name: "A trigger without FOR EACH ROW",
        plural: false,
        support: [A, N, N, A],
        kinds: &[CREATE_TRIGGER_STMT],
        detect: |element| {
            let node = node_of(element)?;
            let each = child_token(node, ROW_KW).is_some() || child_token(node, STATEMENT_KW).is_some();
            (!each).then(|| child_node(node, QUALIFIED_NAME).map(|name| name.text_range()))?
        },
        example: "CREATE TRIGGER tr AFTER INSERT ON t BEGIN DELETE FROM u; END",
    },
    Feature {
        id: "call",
        name: "CALL",
        plural: false,
        support: [N, A, A, A],
        kinds: &[CALL_STMT],
        detect: first_token_range,
        example: "CALL p()",
    },
    Feature {
        id: "do-block",
        name: "DO with a block of code",
        plural: false,
        support: [N, N, N, A],
        kinds: &[DO_STMT],
        detect: |element| child_node(node_of(element)?, ROUTINE_BODY).and_then(|_| first_token_range(element)),
        example: "DO $$ BEGIN END $$",
    },
    Feature {
        id: "do-expressions",
        name: "DO with expressions",
        plural: false,
        support: [N, A, A, N],
        kinds: &[DO_STMT],
        detect: |element| {
            child_node(node_of(element)?, ROUTINE_BODY)
                .is_none()
                .then(|| first_token_range(element))?
        },
        example: "DO 1 + 1",
    },
    // Sessions, transactions and tools
    Feature {
        id: "start-transaction",
        name: "START TRANSACTION",
        plural: false,
        support: [N, A, A, A],
        kinds: &[BEGIN_STMT],
        detect: |element| keyword(element, START_KW),
        example: "START TRANSACTION",
    },
    Feature {
        id: "transaction-modes",
        name: "DEFERRED, IMMEDIATE and EXCLUSIVE transactions",
        plural: true,
        support: [A, N, N, N],
        kinds: &[BEGIN_STMT],
        detect: |element| {
            let node = node_of(element)?;
            tokens(node)
                .find(|token| {
                    matches!(token.kind(), DEFERRED_KW | IMMEDIATE_KW) || token.text().eq_ignore_ascii_case("exclusive")
                })
                .map(|token| token.text_range())
        },
        example: "BEGIN IMMEDIATE",
    },
    Feature {
        id: "end-transaction",
        name: "END as COMMIT",
        plural: false,
        support: [A, N, N, A],
        kinds: &[COMMIT_STMT],
        detect: |element| keyword(element, END_KW),
        example: "END",
    },
    Feature {
        id: "set",
        name: "SET",
        plural: false,
        support: [N, A, A, A],
        kinds: &[SET_STMT],
        detect: first_token_range,
        example: "SET autocommit = 1",
    },
    Feature {
        id: "show",
        name: "SHOW",
        plural: false,
        support: [N, A, A, A],
        kinds: &[SHOW_STMT],
        detect: first_token_range,
        example: "SHOW TABLES",
    },
    Feature {
        id: "show-statement-forms",
        name: "SHOW of anything but a setting",
        plural: false,
        support: [N, A, A, N],
        kinds: &[SHOW_STMT],
        detect: |element| {
            let node = node_of(element)?;
            let words: Vec<SyntaxToken> = tokens(node)
                .skip(1)
                .filter(|token| !matches!(token.kind(), SEMICOLON | CUSTOM_DELIMITER))
                .collect();
            let first = words.first()?.text().to_ascii_lowercase();
            let setting_forms = ["time", "transaction", "session"];
            let last = words.last()?.text_range().end();
            (words.len() > 1 && !setting_forms.contains(&first.as_str()))
                .then(|| TextRange::new(words[0].text_range().start(), last))
        },
        example: "SHOW CREATE TABLE t",
    },
    Feature {
        id: "show-setting-forms",
        name: "SHOW TIME ZONE, TRANSACTION ISOLATION LEVEL and SESSION AUTHORIZATION",
        plural: false,
        support: [N, N, N, A],
        kinds: &[SHOW_STMT],
        detect: |element| {
            let node = node_of(element)?;
            let words: Vec<String> = tokens(node)
                .skip(1)
                .take(2)
                .map(|token| token.text().to_ascii_lowercase())
                .collect();
            let pair = (words.first()?.as_str(), words.get(1).map_or("", String::as_str));
            matches!(
                pair,
                ("time", "zone") | ("transaction", "isolation") | ("session", "authorization")
            )
            .then(|| node.text_range())
        },
        example: "SHOW TRANSACTION ISOLATION LEVEL",
    },
    Feature {
        id: "set-names-word",
        name: "SET NAMES with an unquoted name",
        plural: false,
        support: [N, A, A, N],
        kinds: &[SET_STMT],
        detect: |element| {
            let words: Vec<SyntaxToken> = tokens(node_of(element)?).collect();
            let names = words.get(1)?;
            let value = words.get(2)?;
            (names.kind() == NAMES_KW && !value.kind().is_string()).then(|| value.text_range())
        },
        example: "SET NAMES utf8mb4",
    },
    Feature {
        id: "set-time-zone",
        name: "SET TIME ZONE",
        plural: false,
        support: [N, N, N, A],
        kinds: &[SET_ASSIGNMENT],
        detect: |element| keyword_pair(element, TIME_KW, ZONE_KW),
        example: "SET TIME ZONE 'UTC'",
    },
    Feature {
        id: "use",
        name: "USE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[USE_STMT],
        detect: first_token_range,
        example: "USE db",
    },
    Feature {
        id: "describe",
        name: "DESCRIBE",
        plural: false,
        support: [N, A, A, N],
        kinds: &[EXPLAIN_STMT],
        detect: |element| keyword(element, DESCRIBE_KW).or_else(|| keyword(element, DESC_KW)),
        example: "DESCRIBE t",
    },
    Feature {
        id: "explain-query-plan",
        name: "EXPLAIN QUERY PLAN",
        plural: false,
        support: [A, N, N, N],
        kinds: &[EXPLAIN_STMT],
        detect: |element| keyword_pair(element, QUERY_KW, PLAN_KW),
        example: "EXPLAIN QUERY PLAN SELECT 1",
    },
    Feature {
        id: "pragma",
        name: "PRAGMA",
        plural: false,
        support: [A, N, N, N],
        kinds: &[PRAGMA_STMT],
        detect: first_token_range,
        example: "PRAGMA foreign_keys = ON",
    },
    Feature {
        id: "attach-detach",
        name: "ATTACH and DETACH",
        plural: false,
        support: [A, N, N, N],
        kinds: &[ATTACH_STMT, DETACH_STMT],
        detect: first_token_range,
        example: "ATTACH DATABASE 'x.db' AS x",
    },
    Feature {
        id: "grant-revoke",
        name: "GRANT and REVOKE",
        plural: false,
        support: [N, A, A, A],
        kinds: &[GRANT_STMT, REVOKE_STMT],
        detect: first_token_range,
        example: "GRANT SELECT ON t TO PUBLIC",
    },
    Feature {
        id: "copy",
        name: "COPY",
        plural: false,
        support: [N, N, N, A],
        kinds: &[COPY_STMT],
        detect: first_token_range,
        example: "COPY t TO STDOUT",
    },
    Feature {
        id: "prepare-as",
        name: "PREPARE ... AS",
        plural: false,
        support: [N, N, N, A],
        kinds: &[PREPARE_STMT],
        detect: |element| keyword(element, AS_KW).and_then(|_| first_token_range(element)),
        example: "PREPARE q AS SELECT 1",
    },
    Feature {
        id: "prepare-from",
        name: "PREPARE ... FROM",
        plural: false,
        support: [N, A, A, N],
        kinds: &[PREPARE_STMT],
        detect: |element| keyword(element, FROM_KW).and_then(|_| first_token_range(element)),
        example: "PREPARE q FROM 'SELECT 1'",
    },
    Feature {
        id: "lock-tables",
        name: "LOCK TABLES and UNLOCK TABLES",
        plural: false,
        support: [N, A, A, N],
        kinds: &[LOCK_STMT, UNLOCK_STMT],
        detect: |element| {
            let node = node_of(element)?;
            let second = tokens(node).nth(1)?;
            second.text().eq_ignore_ascii_case("tables").then(|| node.text_range())
        },
        example: "LOCK TABLES t WRITE",
    },
];
