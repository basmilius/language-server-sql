//! `cargo run -p sql-syntax --example parse -- file.sql [--dialect mysql] [--version 8.0] [--tree] [--trivia] [--compact]`
//! Prints the syntax errors and the findings of the feature table for a file and, with `--tree`, its tree.

use sql_syntax::{Dialect, DumpOptions, Target, Version, check_features, dump, dump_compact, parse};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let flags_with_values = ["--dialect", "--version"];
    let path = args
        .iter()
        .enumerate()
        .find(|(index, arg)| {
            !arg.starts_with("--") && (*index == 0 || !flags_with_values.contains(&args[index - 1].as_str()))
        })
        .map(|(_, arg)| arg.clone());
    let Some(path) = path else {
        eprintln!("usage: parse <file> [--dialect name] [--version x.y] [--tree] [--trivia] [--compact]");
        std::process::exit(2);
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(1);
        }
    };
    let dialect = value("--dialect")
        .and_then(|name| Dialect::parse(&name))
        .unwrap_or(Dialect::Generic);
    let version = value("--version").and_then(|text| Version::parse(&text));
    let parsed = parse(&text, dialect);
    if args.iter().any(|arg| arg == "--compact") {
        print!("{}", dump_compact(&parsed.syntax()));
    }
    if args.iter().any(|arg| arg == "--tree") {
        let options = DumpOptions {
            trivia: args.iter().any(|arg| arg == "--trivia"),
            ranges: true,
        };
        print!("{}", dump(&parsed.syntax(), options));
    }
    let line_of = |offset: u32| text[..offset as usize].matches('\n').count() + 1;
    for error in parsed.errors() {
        println!(
            "{path}:{}: {} ({:?})",
            line_of(error.range.start().into()),
            error.message,
            error.range
        );
    }
    for finding in check_features(&parsed.syntax(), Target::new(dialect, version)) {
        println!(
            "{path}:{}: {} [{}] ({:?})",
            line_of(finding.range.start().into()),
            finding.message,
            finding.feature,
            finding.range
        );
    }
    assert_eq!(
        parsed.syntax().text().to_string(),
        text,
        "the tree must hold every byte"
    );
}
