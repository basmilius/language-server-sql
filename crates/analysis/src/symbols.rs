use sql_syntax::SyntaxKind::*;
use sql_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, TextRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Table,
    View,
    Column,
    Constraint,
    Index,
    Sequence,
    Type,
    EnumValue,
    Domain,
    Function,
    Procedure,
    Trigger,
    Schema,
    Extension,
    CommonTableExpression,
    /// A statement that defines nothing, such as a query or an `INSERT`.
    Statement,
}

/// Something in a script an outline shows: what a statement creates, or the statement itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymbolKind,
    /// The whole statement or definition.
    pub range: TextRange,
    /// What to select when the symbol is picked: its name, or the first words of a statement.
    pub selection_range: TextRange,
    pub children: Vec<Symbol>,
}

/// The symbols of a script: one per statement, in order, with what a definition holds as its
/// children. Statements inside a routine body stay inside their routine.
pub fn document_symbols(root: &SyntaxNode) -> Vec<Symbol> {
    root.children()
        .filter_map(|statement| statement_symbol(&statement))
        .collect()
}

fn tokens(node: &SyntaxNode) -> impl Iterator<Item = sql_syntax::SyntaxToken> + '_ {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| !token.kind().is_trivia())
}

fn child(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|child| child.kind() == kind)
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    tokens(node).any(|token| token.kind() == kind)
}

/// The text of a node with its whitespace and comments folded to single spaces.
fn compact(node: &SyntaxNode) -> String {
    let mut out = String::new();
    for token in node.descendants_with_tokens().filter_map(SyntaxElement::into_token) {
        if token.kind().is_trivia() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        out.push_str(token.text());
    }
    out.trim().to_string()
}

/// The range of the statement without its `;`.
fn statement_range(node: &SyntaxNode) -> TextRange {
    let mut end = node.text_range().end();
    for element in node.children_with_tokens() {
        if !element.kind().is_trivia() && !matches!(element.kind(), SEMICOLON | CUSTOM_DELIMITER) {
            end = element.text_range().end();
        }
    }
    TextRange::new(node.text_range().start(), end)
}

/// The leading keywords of a statement up to its first node, as `CREATE OR REPLACE VIEW`.
fn leading_words(node: &SyntaxNode) -> (String, TextRange) {
    let mut words = Vec::new();
    let mut range: Option<TextRange> = None;
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind().is_trivia() => {}
            SyntaxElement::Token(token) if token.kind().is_keyword() && words.len() < 4 => {
                words.push(token.text().to_ascii_uppercase());
                range = Some(range.map_or(token.text_range(), |range| range.cover(token.text_range())));
            }
            _ => break,
        }
    }
    let range = range.unwrap_or_else(|| TextRange::empty(node.text_range().start()));
    (words.join(" "), range)
}

fn named(
    node: &SyntaxNode,
    kind: SymbolKind,
    name: Option<SyntaxNode>,
    detail: Option<String>,
    children: Vec<Symbol>,
) -> Symbol {
    let range = statement_range(node);
    let (words, words_range) = leading_words(node);
    let (name, selection_range) = match name {
        Some(name) => (compact(&name), name.text_range()),
        None => (words, words_range),
    };
    Symbol {
        name,
        detail,
        kind,
        range,
        selection_range,
        children,
    }
}

fn statement_symbol(node: &SyntaxNode) -> Option<Symbol> {
    let name = child(node, QUALIFIED_NAME);
    match node.kind() {
        CREATE_TABLE_STMT => {
            let children = child(node, TABLE_ELEMENT_LIST)
                .map(|list| table_elements(&list))
                .unwrap_or_default();
            let detail = has_token(node, TEMPORARY_KW) || has_token(node, TEMP_KW);
            Some(named(
                node,
                SymbolKind::Table,
                name,
                detail.then(|| "temporary".to_string()),
                children,
            ))
        }
        CREATE_VIEW_STMT => {
            let materialized = has_token(node, MATERIALIZED_KW);
            let detail = materialized.then(|| "materialized".to_string());
            Some(named(node, SymbolKind::View, name, detail, Vec::new()))
        }
        CREATE_INDEX_STMT => {
            let table = node.children().filter(|child| child.kind() == QUALIFIED_NAME).last();
            let own = node
                .children_with_tokens()
                .take_while(|element| element.kind() != ON_KW)
                .filter_map(SyntaxElement::into_node)
                .find(|child| child.kind() == QUALIFIED_NAME);
            let detail = table
                .filter(|_| own.is_some())
                .map(|table| format!("on {}", compact(&table)));
            Some(named(node, SymbolKind::Index, own, detail, Vec::new()))
        }
        CREATE_SEQUENCE_STMT => Some(named(node, SymbolKind::Sequence, name, None, Vec::new())),
        CREATE_TYPE_STMT => {
            let mut children = Vec::new();
            if let Some(values) = child(node, ENUM_VALUE_LIST) {
                for value in values.children() {
                    let text = compact(&value);
                    children.push(Symbol {
                        name: text.trim_matches('\'').to_string(),
                        detail: None,
                        kind: SymbolKind::EnumValue,
                        range: value.text_range(),
                        selection_range: value.text_range(),
                        children: Vec::new(),
                    });
                }
            }
            if let Some(list) = child(node, TABLE_ELEMENT_LIST) {
                children = table_elements(&list);
            }
            let detail = child(node, ENUM_VALUE_LIST).map(|_| "enum".to_string());
            Some(named(node, SymbolKind::Type, name, detail, children))
        }
        CREATE_DOMAIN_STMT => {
            let detail = child(node, TYPE).map(|ty| compact(&ty));
            Some(named(node, SymbolKind::Domain, name, detail, Vec::new()))
        }
        CREATE_FUNCTION_STMT => {
            let kind = if has_token(node, PROCEDURE_KW) {
                SymbolKind::Procedure
            } else {
                SymbolKind::Function
            };
            let parameters = child(node, PARAM_LIST).map(|list| compact(&list)).unwrap_or_default();
            let returns = child(node, RETURNS_CLAUSE).map(|clause| format!(" {}", compact(&clause)));
            let detail = format!("{parameters}{}", returns.unwrap_or_default());
            Some(named(node, kind, name, Some(detail), Vec::new()))
        }
        CREATE_TRIGGER_STMT => {
            let table = node
                .children_with_tokens()
                .skip_while(|element| element.kind() != ON_KW)
                .filter_map(SyntaxElement::into_node)
                .find(|child| child.kind() == QUALIFIED_NAME);
            let detail = table.map(|table| format!("on {}", compact(&table)));
            Some(named(node, SymbolKind::Trigger, name, detail, Vec::new()))
        }
        CREATE_SCHEMA_STMT => {
            let detail = has_token(node, DATABASE_KW).then(|| "database".to_string());
            Some(named(node, SymbolKind::Schema, child(node, NAME), detail, Vec::new()))
        }
        CREATE_EXTENSION_STMT => Some(named(node, SymbolKind::Extension, name, None, Vec::new())),
        EMPTY_STMT | DELIMITER_STMT | META_COMMAND_STMT | UNKNOWN_STMT | ERROR => None,
        _ => Some(statement(node)),
    }
}

/// A statement that defines nothing: its leading words and what it works on, as `INSERT INTO users`
/// or `SELECT FROM orders`, with its common table expressions as children.
fn statement(node: &SyntaxNode) -> Symbol {
    let range = statement_range(node);
    let body = match node.kind() {
        SELECT_STMT => node.children().next(),
        _ => Some(node.clone()),
    };
    let first_token = node
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| !token.kind().is_trivia() && token.kind() != WITH_KW && !inside_with(token));
    let (words, selection_range) = match node.kind() {
        SELECT_STMT => {
            let token = first_token.clone();
            let word = token
                .as_ref()
                .map_or_else(String::new, |token| token.text().to_ascii_uppercase());
            let range = token.map_or(TextRange::empty(range.start()), |token| token.text_range());
            (word, range)
        }
        _ => leading_words(node),
    };
    let target = body.as_ref().and_then(target_of);
    let name = match (node.kind(), target) {
        (SELECT_STMT, Some(table)) => format!("{words} FROM {table}"),
        (_, Some(table)) if !words.is_empty() => format!("{words} {table}"),
        _ if words.is_empty() => compact(node).chars().take(40).collect(),
        _ => words,
    };
    let children = body
        .iter()
        .flat_map(|body| body.children().filter(|child| child.kind() == WITH_CLAUSE))
        .chain(node.children().filter(|child| child.kind() == WITH_CLAUSE))
        .flat_map(|with| with.children().filter(|child| child.kind() == CTE).collect::<Vec<_>>())
        .map(|cte| {
            let name = child(&cte, NAME);
            Symbol {
                name: name.as_ref().map(compact).unwrap_or_default(),
                detail: None,
                kind: SymbolKind::CommonTableExpression,
                range: cte.text_range(),
                selection_range: name.map_or(cte.text_range(), |name| name.text_range()),
                children: Vec::new(),
            }
        })
        .collect();
    Symbol {
        name,
        detail: None,
        kind: SymbolKind::Statement,
        range,
        selection_range,
        children,
    }
}

fn inside_with(token: &sql_syntax::SyntaxToken) -> bool {
    token.parent_ancestors().any(|ancestor| ancestor.kind() == WITH_CLAUSE)
}

/// The table a statement works on: the target of `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `ALTER`,
/// `DROP` and the like, or the first table of a query.
fn target_of(node: &SyntaxNode) -> Option<String> {
    if let Some(name) = child(node, QUALIFIED_NAME) {
        return Some(compact(&name));
    }
    let table = node
        .descendants()
        .filter(|descendant| !descendant.ancestors().any(|ancestor| ancestor.kind() == WITH_CLAUSE))
        .find(|descendant| descendant.kind() == TABLE_REF)?;
    child(&table, QUALIFIED_NAME).map(|name| compact(&name))
}

/// The columns and named constraints of a table.
fn table_elements(list: &SyntaxNode) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for element in list.children() {
        match element.kind() {
            COLUMN_DEF => {
                let Some(name) = child(&element, NAME) else {
                    continue;
                };
                symbols.push(Symbol {
                    name: compact(&name),
                    detail: child(&element, TYPE).map(|ty| compact(&ty)),
                    kind: SymbolKind::Column,
                    range: element.text_range(),
                    selection_range: name.text_range(),
                    children: Vec::new(),
                });
            }
            TABLE_CONSTRAINT => {
                let Some(name) = child(&element, NAME) else {
                    continue;
                };
                let (words, _) = leading_words(&element);
                let detail = words.trim_start_matches("CONSTRAINT").trim().to_string();
                symbols.push(Symbol {
                    name: compact(&name),
                    detail: (!detail.is_empty()).then_some(detail),
                    kind: SymbolKind::Constraint,
                    range: element.text_range(),
                    selection_range: name.text_range(),
                    children: Vec::new(),
                });
            }
            _ => {}
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_syntax::{Dialect, parse};

    fn outline(text: &str, dialect: Dialect) -> String {
        fn write(out: &mut String, symbols: &[Symbol], text: &str, depth: usize) {
            for symbol in symbols {
                let selected = &text[symbol.selection_range];
                let detail = symbol
                    .detail
                    .as_deref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{}{:?} {}{detail} [{selected}]\n",
                    "  ".repeat(depth),
                    symbol.kind,
                    symbol.name
                ));
                write(out, &symbol.children, text, depth + 1);
            }
        }
        let parsed = parse(text, dialect);
        let mut out = String::new();
        write(&mut out, &document_symbols(&parsed.syntax()), text, 0);
        out
    }

    #[test]
    fn definitions_and_statements_in_order() {
        let text = "CREATE TABLE app.users (id int PRIMARY KEY, email text NOT NULL, CONSTRAINT uq UNIQUE (email));\n\
                    CREATE INDEX users_email ON app.users (email);\n\
                    CREATE OR REPLACE VIEW active AS SELECT * FROM app.users;\n\
                    CREATE TYPE mood AS ENUM ('sad', 'ok');\n\
                    CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE sql RETURN a + b;\n\
                    WITH recent AS (SELECT 1) SELECT * FROM orders o JOIN recent r ON true;\n\
                    INSERT INTO app.users (id) VALUES (1);\n\
                    UPDATE users SET id = 2;\n\
                    DROP TABLE old;\n\
                    ;\n";
        expect_test::expect![[r#"
            Table app.users [app.users]
              Column id (int) [id]
              Column email (text) [email]
              Constraint uq [uq]
            Index users_email (on app.users) [users_email]
            View active [active]
            Type mood (enum) [mood]
              EnumValue sad ['sad']
              EnumValue ok ['ok']
            Function add ((a int, b int) RETURNS int) [add]
            Statement SELECT FROM orders [SELECT]
              CommonTableExpression recent [recent]
            Statement INSERT INTO app.users [INSERT INTO]
            Statement UPDATE users [UPDATE]
            Statement DROP TABLE old [DROP TABLE]
        "#]]
        .assert_eq(&outline(text, Dialect::Postgres));
    }

    #[test]
    fn a_routine_keeps_its_statements_to_itself() {
        let text = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; INSERT INTO t VALUES (1); END //\nDELIMITER ;\nSELECT 2;\n";
        expect_test::expect![[r#"
            Procedure p (()) [p]
            Statement SELECT [SELECT]
        "#]]
        .assert_eq(&outline(text, Dialect::Mysql));
    }
}
