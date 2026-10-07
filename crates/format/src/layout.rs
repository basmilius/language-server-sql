//! Where lines break and how deep they are indented, read from the tree: every statement on its
//! own line, every clause of a query on its own line, the items of a list a line each when there
//! are several, joins and the conditions of `WHERE` one level in, and a subquery, a common table
//! expression and a block one level in between its parentheses or keywords.
//!
//! A break is decided for the token it comes before, as a level of indentation. Ancestors decide
//! the breaks before their children's first tokens before the children are walked, so the level of
//! the line a token is on is the level of the last break before it.

use std::collections::HashMap;

use sql_syntax::SyntaxKind::{self, *};
use sql_syntax::{SyntaxElement, SyntaxNode, SyntaxToken, TextRange, TextSize};

pub(crate) struct Layout {
    /// Every token but whitespace, comments included, in order.
    pub(crate) leaves: Vec<SyntaxToken>,
    /// The level of the line a leaf starts, when it starts one.
    pub(crate) breaks: Vec<Option<usize>>,
    /// The text before a leaf stays as it is: inside a statement that does not parse, or one read
    /// leniently.
    pub(crate) frozen: Vec<bool>,
    /// The leaf belongs to a statement that stays as it is, the case of its keywords included.
    pub(crate) verbatim: Vec<bool>,
    by_start: HashMap<TextSize, usize>,
    leading_commas: bool,
}

/// Clauses of a query that start a line at the level of the query.
fn is_query_clause(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        INTO_CLAUSE
            | FROM_CLAUSE
            | WHERE_CLAUSE
            | GROUP_BY_CLAUSE
            | HAVING_CLAUSE
            | WINDOW_CLAUSE
            | QUALIFY_CLAUSE
            | ORDER_BY_CLAUSE
            | LIMIT_CLAUSE
            | OFFSET_CLAUSE
            | FETCH_CLAUSE
            | LOCKING_CLAUSE
    )
}

fn is_query(kind: SyntaxKind) -> bool {
    matches!(kind, SELECT | COMPOUND_SELECT | VALUES | TABLE_QUERY | PAREN_QUERY)
}

/// Statements whose text stays as it is inside: commands of a client, data and what the parser
/// only reads leniently.
fn is_verbatim(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        UNKNOWN_STMT | META_COMMAND_STMT | COPY_STMT | DELIMITER_STMT | ERROR
    )
}

impl Layout {
    pub(crate) fn new(root: &SyntaxNode, errors: &[TextRange], leading_commas: bool) -> Layout {
        let leaves: Vec<SyntaxToken> = root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() != WHITESPACE)
            .collect();
        let by_start = leaves
            .iter()
            .enumerate()
            .map(|(index, token)| (token.text_range().start(), index))
            .collect();
        let count = leaves.len();
        let mut layout = Layout {
            leaves,
            breaks: vec![None; count],
            frozen: vec![false; count],
            verbatim: vec![false; count],
            by_start,
            leading_commas,
        };
        for statement in root.children() {
            let range = statement.text_range();
            let broken = errors
                .iter()
                .any(|error| error.start() >= range.start() && error.start() <= range.end())
                || statement.descendants().any(|node| is_verbatim(node.kind()));
            if broken {
                layout.freeze(&statement);
            }
        }
        layout
    }

    fn freeze(&mut self, node: &SyntaxNode) {
        let (Some(first), Some(last)) = (self.first_leaf(node), self.last_leaf(node)) else {
            return;
        };
        for index in first..=last {
            self.verbatim[index] = true;
            if index > first {
                self.frozen[index] = true;
            }
        }
    }

    pub(crate) fn index_of(&self, token: &SyntaxToken) -> Option<usize> {
        self.by_start.get(&token.text_range().start()).copied()
    }

    fn first_leaf(&self, node: &SyntaxNode) -> Option<usize> {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
            .and_then(|token| self.index_of(&token))
    }

    fn last_leaf(&self, node: &SyntaxNode) -> Option<usize> {
        self.index_of(&node.last_token()?)
    }

    fn break_before(&mut self, leaf: Option<usize>, level: usize) {
        if let Some(leaf) = leaf {
            if leaf > 0 && !self.frozen[leaf] {
                self.breaks[leaf] = Some(level);
            }
        }
    }

    fn break_node(&mut self, node: &SyntaxNode, level: usize) {
        let leaf = self.first_leaf(node);
        self.break_before(leaf, level);
    }

    fn break_token(&mut self, token: &SyntaxToken, level: usize) {
        let leaf = self.index_of(token);
        self.break_before(leaf, level);
    }

    /// The level of the line a leaf is on: of the last break at or before it.
    pub(crate) fn line_level(&self, leaf: usize) -> usize {
        self.breaks[..=leaf.min(self.breaks.len().saturating_sub(1))]
            .iter()
            .rev()
            .find_map(|level| *level)
            .unwrap_or(0)
    }

    fn level_of(&self, node: &SyntaxNode) -> usize {
        self.first_leaf(node).map_or(0, |leaf| self.line_level(leaf))
    }

    /// An item of a list laid out a line each: it starts the line, or its comma does.
    fn break_item(&mut self, item: &SyntaxNode, level: usize, first: bool) {
        if self.leading_commas && !first {
            let comma = item
                .siblings_with_tokens(sql_syntax::Direction::Prev)
                .skip(1)
                .filter_map(SyntaxElement::into_token)
                .find(|token| !token.kind().is_trivia());
            if let Some(comma) = comma.filter(|token| token.kind() == COMMA) {
                self.break_token(&comma, level);
                return;
            }
        }
        self.break_node(item, level);
    }

    fn list(&mut self, items: &[SyntaxNode], level: usize) {
        for (position, item) in items.iter().enumerate() {
            self.break_item(item, level, position == 0);
        }
    }

    pub(crate) fn walk_file(&mut self, root: &SyntaxNode) {
        for statement in root.children() {
            self.break_node(&statement, 0);
            let frozen = self.first_leaf(&statement).is_some_and(|first| self.verbatim[first]);
            if !frozen {
                self.walk(&statement);
            }
        }
    }

    /// After a `WITH` that leads a statement, the statement's own first word starts a line.
    fn after_with(&mut self, node: &SyntaxNode, level: usize) {
        if let Some(with) = node.children().find(|child| child.kind() == WITH_CLAUSE) {
            if let Some(last) = self.last_leaf(&with) {
                let next = (last + 1..self.leaves.len()).find(|index| !self.leaves[*index].kind().is_trivia());
                self.break_before(next, level);
            }
        }
    }

    fn walk(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SELECT => self.select(node),
            COMPOUND_SELECT => self.compound(node),
            VALUES => self.values(node),
            PAREN_QUERY | CTE => self.parenthesized_query(node),
            WITH_CLAUSE => self.with_clause(node),
            INSERT_STMT => self.insert(node),
            UPDATE_STMT => self.update(node),
            DELETE_STMT => self.delete(node),
            MERGE_STMT => self.merge(node),
            SET_CLAUSE | ON_DUPLICATE_KEY_CLAUSE => self.assignments(node),
            CREATE_TABLE_STMT | CREATE_TYPE_STMT => self.create_table(node),
            CREATE_VIEW_STMT => self.create_view(node),
            ALTER_TABLE_STMT => self.alter_table(node),
            CREATE_FUNCTION_STMT | CREATE_TRIGGER_STMT => self.routine(node),
            BLOCK | IF_STMT | LOOP_STMT | WHILE_STMT | REPEAT_STMT | CASE_STMT | ELSEIF_CLAUSE => {
                self.compound_statement(node)
            }
            ELSE_CLAUSE | WHEN_CLAUSE
                if node
                    .parent()
                    .is_some_and(|parent| matches!(parent.kind(), IF_STMT | CASE_STMT)) =>
            {
                self.compound_statement(node)
            }
            CASE_EXPR => self.case_expression(node),
            _ => self.children(node),
        }
    }

    fn children(&mut self, node: &SyntaxNode) {
        for child in node.children() {
            self.walk(&child);
        }
    }

    /// The clauses after a `WITH`, a set operation or a statement's first line, each on a line of
    /// its own at the level of the statement.
    fn clause(&mut self, clause: &SyntaxNode, level: usize) {
        let after_limit = clause.kind() == OFFSET_CLAUSE
            && clause
                .prev_sibling()
                .is_some_and(|previous| previous.kind() == LIMIT_CLAUSE);
        if !after_limit {
            self.break_node(clause, level);
        }
        match clause.kind() {
            WHERE_CLAUSE | HAVING_CLAUSE | QUALIFY_CLAUSE => {
                if let Some(condition) = clause.children().next() {
                    self.conditions(&condition, level + 1);
                }
            }
            FROM_CLAUSE | USING_CLAUSE => {
                for table in clause.children() {
                    self.joins(&table, level + 1);
                }
            }
            _ => {}
        }
        self.walk(clause);
    }

    fn select(&mut self, select: &SyntaxNode) {
        let level = self.level_of(select);
        let mut after_with = false;
        for element in select.children_with_tokens() {
            match element {
                SyntaxElement::Token(token) => {
                    if after_with && !token.kind().is_trivia() {
                        self.break_token(&token, level);
                        after_with = false;
                    }
                }
                SyntaxElement::Node(child) => {
                    if after_with {
                        self.break_node(&child, level);
                        after_with = false;
                    }
                    match child.kind() {
                        WITH_CLAUSE => {
                            self.walk(&child);
                            after_with = true;
                        }
                        SELECT_LIST => {
                            let items: Vec<SyntaxNode> =
                                child.children().filter(|item| item.kind() == SELECT_ITEM).collect();
                            if items.len() > 1 {
                                self.list(&items, level + 1);
                            }
                            self.walk(&child);
                        }
                        kind if is_query_clause(kind) => self.clause(&child, level),
                        _ => self.walk(&child),
                    }
                }
            }
        }
    }

    fn compound(&mut self, compound: &SyntaxNode) {
        let level = self.level_of(compound);
        self.after_with(compound, level);
        let mut seen_query = false;
        let mut operator_open = false;
        for element in compound.children_with_tokens() {
            match element {
                SyntaxElement::Token(token) if !token.kind().is_trivia() => {
                    if seen_query && !operator_open {
                        self.break_token(&token, level);
                        operator_open = true;
                    }
                }
                SyntaxElement::Token(_) => {}
                SyntaxElement::Node(child) => match child.kind() {
                    kind if is_query(kind) => {
                        if seen_query {
                            self.break_node(&child, level);
                        }
                        seen_query = true;
                        operator_open = false;
                        self.walk(&child);
                    }
                    kind if is_query_clause(kind) => self.clause(&child, level),
                    _ => self.walk(&child),
                },
            }
        }
    }

    fn values(&mut self, values: &SyntaxNode) {
        let level = self.level_of(values);
        let rows: Vec<SyntaxNode> = values.children().filter(|row| row.kind() == ROW_EXPR).collect();
        if rows.len() > 1 {
            self.list(&rows, level + 1);
        }
        for child in values.children() {
            if is_query_clause(child.kind()) {
                self.clause(&child, level);
            } else {
                self.walk(&child);
            }
        }
    }

    /// `(` and a query and `)`: the query one level in on lines of its own, the `)` back out.
    fn parenthesized_query(&mut self, node: &SyntaxNode) {
        let open = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| token.kind() == LPAREN);
        let query = node.children().find(|child| is_query(child.kind()));
        let close = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() == RPAREN)
            .last();
        if let (Some(open), Some(query), Some(close)) = (&open, &query, &close) {
            if let Some(open_leaf) = self.index_of(open) {
                let level = self.line_level(open_leaf);
                self.break_node(query, level + 1);
                self.break_token(close, level);
            }
        }
        self.children(node);
    }

    fn with_clause(&mut self, with: &SyntaxNode) {
        let level = self.level_of(with);
        let ctes: Vec<SyntaxNode> = with.children().filter(|cte| cte.kind() == CTE).collect();
        for (position, cte) in ctes.iter().enumerate() {
            if position > 0 {
                self.break_item(cte, level, false);
            }
        }
        self.children(with);
    }

    /// Joins of a table expression, each on a line of its own one level in, with the conditions
    /// of `ON` one level further.
    fn joins(&mut self, node: &SyntaxNode, level: usize) {
        if node.kind() != JOIN_EXPR {
            return;
        }
        let mut operands = 0;
        let mut join_word_seen = false;
        for element in node.children_with_tokens() {
            match element {
                SyntaxElement::Node(child) => match child.kind() {
                    ON_CLAUSE => {
                        if let Some(condition) = child.children().next() {
                            self.conditions(&condition, level + 1);
                        }
                    }
                    USING_CLAUSE => {}
                    _ => {
                        if operands == 0 {
                            self.joins(&child, level);
                        }
                        operands += 1;
                    }
                },
                SyntaxElement::Token(token) => {
                    if operands == 1 && !join_word_seen && !token.kind().is_trivia() {
                        join_word_seen = true;
                        self.break_token(&token, level);
                    }
                }
            }
        }
    }

    /// A chain of `AND` and `OR`: each operator starts a line.
    fn conditions(&mut self, condition: &SyntaxNode, level: usize) {
        if condition.kind() != BINARY_EXPR {
            return;
        }
        let operator = condition
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia());
        let Some(operator) = operator.filter(|token| matches!(token.kind(), AND_KW | OR_KW)) else {
            return;
        };
        let operands: Vec<SyntaxNode> = condition.children().collect();
        if let Some(left) = operands.first() {
            self.conditions(left, level);
        }
        self.break_token(&operator, level);
        if let Some(right) = operands.get(1) {
            self.conditions(right, level);
        }
    }

    fn insert(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        self.after_with(statement, level);
        for child in statement.children() {
            match child.kind() {
                VALUES | SET_CLAUSE | ON_DUPLICATE_KEY_CLAUSE | UPSERT_CLAUSE | RETURNING_CLAUSE => {
                    self.break_node(&child, level);
                }
                kind if is_query(kind) => self.break_node(&child, level),
                _ => {}
            }
            self.walk(&child);
        }
    }

    fn update(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        self.after_with(statement, level);
        for child in statement.children() {
            match child.kind() {
                SET_CLAUSE | RETURNING_CLAUSE => {
                    self.break_node(&child, level);
                    self.walk(&child);
                }
                JOIN_EXPR => {
                    self.joins(&child, level + 1);
                    self.walk(&child);
                }
                kind if is_query_clause(kind) => self.clause(&child, level),
                _ => self.walk(&child),
            }
        }
    }

    fn delete(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        self.after_with(statement, level);
        let mut first_from = true;
        for child in statement.children() {
            match child.kind() {
                FROM_CLAUSE if first_from => {
                    first_from = false;
                    for table in child.children() {
                        self.joins(&table, level + 1);
                    }
                    self.walk(&child);
                }
                USING_CLAUSE | RETURNING_CLAUSE => self.clause(&child, level),
                kind if is_query_clause(kind) => self.clause(&child, level),
                _ => self.walk(&child),
            }
        }
    }

    fn merge(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        self.after_with(statement, level);
        for element in statement.children_with_tokens() {
            match element {
                SyntaxElement::Token(token) if token.kind() == USING_KW => self.break_token(&token, level),
                SyntaxElement::Token(_) => {}
                SyntaxElement::Node(child) => {
                    if matches!(child.kind(), MERGE_WHEN_CLAUSE | RETURNING_CLAUSE) {
                        self.break_node(&child, level);
                    }
                    if child.kind() == ON_CLAUSE {
                        if let Some(condition) = child.children().next() {
                            self.conditions(&condition, level + 1);
                        }
                    }
                    self.walk(&child);
                }
            }
        }
    }

    /// `SET a = 1, b = 2` and `ON DUPLICATE KEY UPDATE`: several assignments a line each.
    fn assignments(&mut self, node: &SyntaxNode) {
        let level = self.level_of(node);
        let items: Vec<SyntaxNode> = node.children().filter(|item| item.kind() == ASSIGNMENT).collect();
        if items.len() > 1 {
            self.list(&items, level + 1);
        }
        self.children(node);
    }

    fn create_table(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        for child in statement.children() {
            match child.kind() {
                TABLE_ELEMENT_LIST => {
                    let items: Vec<SyntaxNode> = child.children().collect();
                    self.list(&items, level + 1);
                    if let Some(close) = child
                        .children_with_tokens()
                        .filter_map(SyntaxElement::into_token)
                        .filter(|token| token.kind() == RPAREN)
                        .last()
                    {
                        if !items.is_empty() {
                            self.break_token(&close, level);
                        }
                    }
                }
                kind if is_query(kind) => self.break_node(&child, level),
                _ => {}
            }
            self.walk(&child);
        }
    }

    fn create_view(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        let mut after_query = false;
        for element in statement.children_with_tokens() {
            match element {
                SyntaxElement::Node(child) => {
                    if is_query(child.kind()) {
                        self.break_node(&child, level);
                        after_query = true;
                    }
                    self.walk(&child);
                }
                SyntaxElement::Token(token) => {
                    if after_query && !token.kind().is_trivia() && !matches!(token.kind(), SEMICOLON | CUSTOM_DELIMITER)
                    {
                        self.break_token(&token, level);
                    }
                    after_query = false;
                }
            }
        }
    }

    fn alter_table(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        let actions: Vec<SyntaxNode> = statement
            .children()
            .filter(|child| child.kind().name().ends_with("_ACTION"))
            .collect();
        if actions.len() > 1 {
            self.list(&actions, level + 1);
        }
        self.children(statement);
    }

    fn routine(&mut self, statement: &SyntaxNode) {
        let level = self.level_of(statement);
        for child in statement.children() {
            if child.kind() == ROUTINE_BODY {
                let statement = |kind: SyntaxKind| kind == BLOCK || kind.name().ends_with("_STMT");
                if let Some(body) = child.children().next().filter(|body| statement(body.kind())) {
                    self.break_node(&body, level);
                }
            }
            self.walk(&child);
        }
    }

    /// `BEGIN ... END`, `IF`, the loops and `CASE` of a routine: the statements one level in, the
    /// words that close or continue them back out.
    fn compound_statement(&mut self, node: &SyntaxNode) {
        let level = self.level_of(node);
        let in_case = node.kind() == CASE_STMT;
        let mut after_list = false;
        for element in node.children_with_tokens() {
            match element {
                SyntaxElement::Node(child) => match child.kind() {
                    STATEMENT_LIST => {
                        for statement in child.children() {
                            self.break_node(&statement, level + 1);
                            self.walk(&statement);
                        }
                        after_list = true;
                    }
                    ELSEIF_CLAUSE | ELSE_CLAUSE | WHEN_CLAUSE if node.kind() != BLOCK => {
                        self.break_node(&child, if in_case { level + 1 } else { level });
                        self.walk(&child);
                        after_list = true;
                    }
                    _ => self.walk(&child),
                },
                SyntaxElement::Token(token) => {
                    if after_list && matches!(token.kind(), END_KW | UNTIL_KW) {
                        self.break_token(&token, level);
                        after_list = false;
                    } else if node.kind() == BLOCK && token.kind() == END_KW {
                        self.break_token(&token, level);
                    }
                }
            }
        }
    }

    fn case_expression(&mut self, case: &SyntaxNode) {
        let level = self.level_of(case);
        let branches: Vec<SyntaxNode> = case
            .children()
            .filter(|child| matches!(child.kind(), WHEN_CLAUSE | ELSE_CLAUSE))
            .collect();
        if branches.len() > 1 {
            for branch in &branches {
                self.break_node(branch, level + 1);
            }
            if let Some(end) = case
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|token| token.kind() == END_KW)
                .last()
            {
                self.break_token(&end, level);
            }
        }
        self.children(case);
    }
}
