//! `cargo run -p sql-format --example format -- file.sql [--dialect mysql] [--lower] [--leading]`
//! Prints a file formatted, or why it is left as it is.

use sql_format::{FormatOptions, KeywordCase, format_unchecked, try_format};
use sql_syntax::Dialect;
use sql_syntax::SyntaxElement;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dialect = args
        .iter()
        .position(|arg| arg == "--dialect")
        .and_then(|index| args.get(index + 1))
        .and_then(|name| Dialect::parse(name))
        .unwrap_or(Dialect::Generic);
    let Some(path) = args.iter().find(|arg| arg.ends_with(".sql")) else {
        eprintln!("usage: format <file.sql> [--dialect name] [--lower] [--leading]");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| {
        eprintln!("{path}: {error}");
        std::process::exit(1);
    });
    let options = FormatOptions {
        keyword_case: if args.iter().any(|arg| arg == "--lower") {
            KeywordCase::Lower
        } else {
            KeywordCase::Upper
        },
        leading_commas: args.iter().any(|arg| arg == "--leading"),
        ..FormatOptions::default()
    };
    match try_format(&text, dialect, &options) {
        Ok(formatted) => print!("{formatted}"),
        Err(refusal) => {
            eprintln!("left as it is: {refusal:?}");
            let unchecked = format_unchecked(&text, dialect, &options);
            let tokens = |text: &str| -> Vec<String> {
                sql_syntax::parse(text, dialect)
                    .syntax()
                    .descendants_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .filter(|token| !token.kind().is_trivia())
                    .map(|token| {
                        let text = if token.kind().is_keyword() {
                            token.text().to_ascii_lowercase()
                        } else {
                            token.text().to_string()
                        };
                        format!("{:?} {text:?}", token.kind())
                    })
                    .collect()
            };
            let (before, after) = (tokens(&text), tokens(&unchecked));
            if let Some(index) =
                (0..before.len().max(after.len())).find(|index| before.get(*index) != after.get(*index))
            {
                eprintln!(
                    "first difference at token {index}: {:?} became {:?}",
                    before.get(index),
                    after.get(index)
                );
            }
            std::process::exit(1);
        }
    }
}
