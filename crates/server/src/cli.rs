//! The commands besides the server, for a person or an agent with a shell: `check` prints the
//! diagnostics of files, `format` lays them out, and `describe` says what is known about a table,
//! a column or a function. They read the same settings as the server, from `--config`, with
//! `--dialect`, `--version` and `--schema` over them.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::{Value, json};
use sql_analysis::catalog::Layer;
use sql_analysis::context::{DocumentSchema, Schemas};
use sql_analysis::ident::Ident;
use sql_analysis::refs::{Symbol, symbol_at_offset};
use sql_analysis::{DiagnosticSeverity, diagnostics, nav};
use sql_embed::{read_sql_file, sql_files};
use sql_format::{FormatOptions, Indent, KeywordCase};
use sql_syntax::{Dialect, Target, Version, parse};

use crate::config::{KeywordCase as SettingCase, Settings};
use crate::files::{WorkspaceFiles, is_sql};
use crate::snapshots::Snapshots;

pub const USAGE: &str = "\
sql-language-server [--stdio]
sql-language-server check [options] [--format human|json] [--severity <level>] <paths...>
sql-language-server format [options] [--check | --write] <paths...>
sql-language-server describe [options] [--format human|json] <name> [paths...]

Without a command it speaks LSP over stdin and stdout. A path is a file, a folder (its .sql files)
or - for stdin; the DDL of the files named is the workspace schema.

Options:
  --config <file>     settings as JSON, the shape the server takes over LSP
  --dialect <name>    sqlite, mysql, mariadb, postgres or generic
  --version <version> the version of the dialect, such as 8.4
  --schema <file>     a schema snapshot

Exit codes: 0 success; 1 check found an error, format --check found a file to format, describe
found nothing; 2 the arguments, settings, a file or the snapshot could not be read.";

/// The stack the commands run on: a recursive descent parser recurses as deeply as the input nests.
const STACK_SIZE: usize = 64 << 20;

const COMMANDS: [&str; 3] = ["check", "format", "describe"];

/// Runs a command when the arguments name one; `None` leaves them to the server.
pub fn run(args: &[String]) -> Option<ExitCode> {
    let command = args.first().filter(|first| COMMANDS.contains(&first.as_str()))?.clone();
    let rest = args[1..].to_vec();
    if rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Some(ExitCode::SUCCESS);
    }
    let worker = std::thread::Builder::new()
        .name(command.clone())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let mut out = std::io::stdout().lock();
            let result = Options::read(&command, &rest).and_then(|options| match command.as_str() {
                "check" => check(&options, &mut out),
                "format" => format(&options, &mut out),
                _ => describe(&options, &mut out),
            });
            let _ = out.flush();
            match result {
                Ok(code) => code,
                Err(problem) => {
                    eprintln!("sql-language-server {command}: {problem}");
                    2
                }
            }
        });
    let code = worker.ok().and_then(|worker| worker.join().ok()).unwrap_or(2);
    Some(ExitCode::from(code))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormatMode {
    Print,
    Check,
    Write,
}

struct Options {
    settings: Settings,
    dialect: Option<Dialect>,
    version: Option<Version>,
    schema: Option<PathBuf>,
    json: bool,
    severity: DiagnosticSeverity,
    mode: FormatMode,
    paths: Vec<String>,
    root: PathBuf,
}

impl Options {
    fn read(command: &str, args: &[String]) -> Result<Options, String> {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let mut options = Options {
            settings: Settings::default(),
            dialect: None,
            version: None,
            schema: None,
            json: false,
            severity: DiagnosticSeverity::Hint,
            mode: FormatMode::Print,
            paths: Vec::new(),
            root,
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value\n\n{USAGE}"))
            };
            match arg.as_str() {
                "--config" => {
                    let path = value("--config")?;
                    let text = std::fs::read_to_string(&path).map_err(|error| format!("{path}: {error}"))?;
                    let json: Value = serde_json::from_str(&text).map_err(|error| format!("{path}: {error}"))?;
                    let (settings, problems) = Settings::from_value(&json);
                    if let Some(problem) = problems.first() {
                        return Err(format!("{path}: {problem}"));
                    }
                    options.settings = settings;
                }
                "--dialect" => {
                    let name = value("--dialect")?;
                    options.dialect = Some(Dialect::parse(&name).ok_or_else(|| {
                        format!("unknown dialect '{name}': use sqlite, mysql, mariadb, postgres or generic")
                    })?);
                }
                "--version" => {
                    let text = value("--version")?;
                    options.version = Some(Version::parse(&text).ok_or_else(|| format!("unknown version '{text}'"))?);
                }
                "--schema" => options.schema = Some(PathBuf::from(value("--schema")?)),
                "--format" if command != "format" => match value("--format")?.as_str() {
                    "json" => options.json = true,
                    "human" => options.json = false,
                    other => return Err(format!("unknown output format '{other}': use human or json")),
                },
                "--severity" if command == "check" => {
                    options.severity = match value("--severity")?.to_ascii_lowercase().as_str() {
                        "error" => DiagnosticSeverity::Error,
                        "warning" => DiagnosticSeverity::Warning,
                        "information" | "info" => DiagnosticSeverity::Information,
                        "hint" => DiagnosticSeverity::Hint,
                        other => {
                            return Err(format!(
                                "unknown severity '{other}': use error, warning, information or hint"
                            ));
                        }
                    }
                }
                "--check" if command == "format" => options.mode = FormatMode::Check,
                "--write" if command == "format" => options.mode = FormatMode::Write,
                flag if flag.starts_with("--") => return Err(format!("unknown option '{flag}'\n\n{USAGE}")),
                path => options.paths.push(path.to_string()),
            }
        }
        if options.paths.is_empty() {
            return Err(format!(
                "{command} needs {}\n\n{USAGE}",
                match command {
                    "describe" => "a name",
                    _ => "a path",
                }
            ));
        }
        Ok(options)
    }

    /// The target and snapshot of a file: the settings for its path, the flags over them.
    fn resolve(&self, path: Option<&Path>) -> (Target, Option<PathBuf>) {
        let resolved = self.settings.resolve(path, Some(&self.root), None);
        let mut target = resolved.target;
        if let Some(dialect) = self.dialect {
            if dialect != target.dialect {
                target = Target::new(dialect, None);
            }
        }
        if let Some(version) = self.version {
            target = Target::new(target.dialect, Some(version));
        }
        let schema = self
            .schema
            .as_ref()
            .map(|schema| self.root.join(schema))
            .or(resolved.schema);
        (target, schema)
    }
}

/// A file named on the command line, read.
struct Input {
    /// `None` for stdin.
    path: Option<PathBuf>,
    text: String,
}

impl Input {
    fn name(&self) -> String {
        self.path
            .as_ref()
            .map_or_else(|| "<stdin>".to_string(), |path| path.display().to_string())
    }
}

fn read_inputs(paths: &[String]) -> Result<Vec<Input>, String> {
    let mut inputs = Vec::new();
    for path in paths {
        if path == "-" {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|error| format!("stdin: {error}"))?;
            inputs.push(Input { path: None, text });
            continue;
        }
        let path = PathBuf::from(path);
        if path.is_dir() {
            let mut found = sql_files(std::slice::from_ref(&path));
            found.sort();
            for file in found {
                let text = read_sql_file(&file).ok_or_else(|| format!("{}: cannot be read", file.display()))?;
                inputs.push(Input { path: Some(file), text });
            }
        } else {
            let text = read_sql_file(&path).ok_or_else(|| format!("{}: cannot be read", path.display()))?;
            inputs.push(Input { path: Some(path), text });
        }
    }
    Ok(inputs)
}

/// A snapshot and the workspace's DDL, either of which may be missing.
type Layers = (Option<Arc<Layer>>, Option<Arc<Layer>>);

/// The snapshots and the workspace's DDL the inputs are read against.
struct Schema {
    snapshots: Snapshots,
    workspace: WorkspaceFiles,
}

impl Schema {
    fn of(options: &Options, inputs: &[Input]) -> Schema {
        let mut workspace = WorkspaceFiles::default();
        for input in inputs {
            if let Some(path) = &input.path {
                if is_sql(path) {
                    let (target, _) = options.resolve(Some(path));
                    let ddl = sql_analysis::workspace::extract(&input.text, target.dialect);
                    workspace.set(path.clone(), Some((target.dialect, ddl)));
                }
            }
        }
        Schema {
            snapshots: Snapshots::default(),
            workspace,
        }
    }

    fn layers(&mut self, target: Target, snapshot: Option<&Path>) -> Result<Layers, String> {
        let snapshot = match snapshot {
            Some(path) => {
                let loaded = self.snapshots.get(path);
                if let Some(problem) = loaded.problem {
                    return Err(format!("{}: {problem}", path.display()));
                }
                loaded.layer
            }
            None => None,
        };
        Ok((snapshot, self.workspace.layer(target.dialect)))
    }
}

fn severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Information => "information",
        DiagnosticSeverity::Hint => "hint",
    }
}

/// The line and column of a byte offset, both from 1, the column in characters.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    (line, before[line_start..].chars().count() + 1)
}

fn check(options: &Options, out: &mut impl Write) -> Result<u8, String> {
    let inputs = read_inputs(&options.paths)?;
    let mut schema = Schema::of(options, &inputs);
    let mut counts = [0usize; 4];
    let mut found = Vec::new();
    for input in &inputs {
        let (target, snapshot) = options.resolve(input.path.as_deref());
        let (snapshot, workspace) = schema.layers(target, snapshot.as_deref())?;
        let schemas = Schemas {
            snapshot: snapshot.as_deref(),
            workspace: workspace.as_deref(),
        };
        let parsed = parse(&input.text, target.dialect);
        for diagnostic in diagnostics(&parsed, target, schemas, &options.settings.inspections) {
            if diagnostic.severity > options.severity {
                continue;
            }
            counts[diagnostic.severity as usize] += 1;
            let start = usize::from(diagnostic.range.start());
            let end = usize::from(diagnostic.range.end());
            let (line, column) = line_column(&input.text, start);
            let (end_line, end_column) = line_column(&input.text, end);
            if options.json {
                found.push(json!({
                    "path": input.name(),
                    "start": { "line": line, "column": column, "offset": start },
                    "end": { "line": end_line, "column": end_column, "offset": end },
                    "severity": severity_name(diagnostic.severity),
                    "code": diagnostic.code,
                    "feature": diagnostic.feature,
                    "message": diagnostic.message,
                }));
            } else {
                writeln!(
                    out,
                    "{}:{line}:{column}: {}[{}]: {}",
                    input.name(),
                    severity_name(diagnostic.severity),
                    diagnostic.code,
                    diagnostic.message
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }
    let [errors, warnings, information, hints] = counts;
    if options.json {
        let report = json!({
            "files": inputs.len(),
            "errors": errors,
            "warnings": warnings,
            "information": information,
            "hints": hints,
            "diagnostics": found,
        });
        writeln!(out, "{report}").map_err(|error| error.to_string())?;
    } else {
        let files = match inputs.len() {
            1 => "1 file".to_string(),
            count => format!("{count} files"),
        };
        eprintln!("{files}: {errors} errors, {warnings} warnings, {information} information, {hints} hints");
    }
    Ok(u8::from(errors > 0))
}

fn format_options(settings: &Settings) -> FormatOptions {
    FormatOptions {
        indent: Indent::Spaces(settings.format.indent_width.unwrap_or(4)),
        keyword_case: match settings.format.keyword_case {
            None | Some(SettingCase::Upper) => KeywordCase::Upper,
            Some(SettingCase::Lower) => KeywordCase::Lower,
            Some(SettingCase::Preserve) => KeywordCase::Preserve,
        },
        leading_commas: settings.format.leading_commas.unwrap_or(false),
    }
}

fn format(options: &Options, out: &mut impl Write) -> Result<u8, String> {
    let inputs = read_inputs(&options.paths)?;
    if options.mode == FormatMode::Print && inputs.len() != 1 {
        return Err("give --check or --write to format more than one file".to_string());
    }
    if options.mode == FormatMode::Write && inputs.iter().any(|input| input.path.is_none()) {
        return Err("--write cannot write stdin back".to_string());
    }
    let layout = format_options(&options.settings);
    let mut changed = 0;
    for input in &inputs {
        let (target, _) = options.resolve(input.path.as_deref());
        let formatted = match sql_format::try_format(&input.text, target.dialect, &layout) {
            Ok(formatted) => formatted,
            Err(_) => {
                eprintln!("{}: left as it is, since the layout would change a token", input.name());
                input.text.clone()
            }
        };
        let differs = formatted != input.text;
        match options.mode {
            FormatMode::Print => write!(out, "{formatted}").map_err(|error| error.to_string())?,
            FormatMode::Check if differs => {
                changed += 1;
                writeln!(out, "{}", input.name()).map_err(|error| error.to_string())?;
            }
            FormatMode::Write if differs => {
                changed += 1;
                if let Some(path) = &input.path {
                    std::fs::write(path, &formatted).map_err(|error| format!("{}: {error}", path.display()))?;
                }
                writeln!(out, "{}", input.name()).map_err(|error| error.to_string())?;
            }
            _ => {}
        }
    }
    Ok(u8::from(options.mode == FormatMode::Check && changed > 0))
}

/// Statements that name `name` in each way it may be meant, with where the name's last part
/// stands: a table, a column of a table, a function, a type.
fn probes(name: &str) -> Vec<(String, usize)> {
    let call = name.strip_suffix("()");
    let name = call.unwrap_or(name);
    let parts: Vec<&str> = name.split('.').collect();
    let last = parts.last().copied().unwrap_or_default();
    let mut out = Vec::new();
    let mut probe = |text: String, needle: &str| {
        if let Some(at) = text.rfind(needle) {
            out.push((text.clone(), at));
        }
    };
    if call.is_some() {
        probe(format!("SELECT {name}()"), last);
        return out;
    }
    match parts.as_slice() {
        [_] => {
            probe(format!("SELECT * FROM {name}"), last);
            probe(format!("SELECT {name}()"), last);
            probe(format!("SELECT CAST(NULL AS {name})"), last);
        }
        [table, column] => {
            probe(format!("SELECT * FROM {name}"), last);
            probe(format!("SELECT {column} FROM {table}"), column);
            probe(format!("SELECT {name}()"), last);
        }
        [schema, table, column] => probe(format!("SELECT {column} FROM {schema}.{table}"), column),
        _ => {}
    }
    out
}

fn describe(options: &Options, out: &mut impl Write) -> Result<u8, String> {
    let name = options.paths[0].clone();
    let inputs = read_inputs(&options.paths[1..])?;
    let mut schema = Schema::of(options, &inputs);
    let (target, snapshot) = options.resolve(None);
    let (snapshot, workspace) = schema.layers(target, snapshot.as_deref())?;
    let schemas = Schemas {
        snapshot: snapshot.as_deref(),
        workspace: workspace.as_deref(),
    };
    for (text, at) in probes(&name) {
        let root = parse(&text, target.dialect).syntax();
        let Some(hover) = nav::hover(&root, at as u32, target, schemas) else {
            continue;
        };
        let symbol = symbol_at_offset(&root, at as u32, target, schemas).map(|(_, symbol, _)| symbol);
        if options.json {
            let definition = symbol
                .as_ref()
                .and_then(|symbol| definition_of(symbol, target, schemas));
            let report = json!({
                "name": name,
                "kind": symbol.as_ref().map(Symbol::label),
                "markdown": hover.markdown,
                "definition": definition,
            });
            writeln!(out, "{report}").map_err(|error| error.to_string())?;
        } else {
            writeln!(out, "{}", hover.markdown).map_err(|error| error.to_string())?;
        }
        return Ok(0);
    }
    eprintln!("nothing named '{name}' is known");
    Ok(1)
}

/// The model of a table or a column as the snapshot format writes it.
fn definition_of(symbol: &Symbol, target: Target, schemas: Schemas) -> Option<Value> {
    let document = DocumentSchema::new(target, schemas);
    let catalog = document.catalog();
    let find = |schema: &Option<String>, table: &str| {
        let schema = schema.as_deref().map(Ident::new);
        catalog.find_table(schema.as_ref(), &Ident::new(table))
    };
    match symbol {
        Symbol::Object { schema, name, .. } => {
            let id = find(schema, name)?;
            serde_json::to_value(catalog.table(id)).ok()
        }
        Symbol::Column { schema, table, name } => {
            let id = find(schema, table)?;
            let column = catalog
                .table(id)
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))?;
            serde_json::to_value(column).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines_and_columns_from_one_in_characters() {
        assert_eq!(line_column("SELECT é, x", 11), (1, 11));
        assert_eq!(line_column("a\nbc", 3), (2, 2));
    }

    #[test]
    fn probes_a_name_in_every_way_it_may_be_meant() {
        assert_eq!(probes("users.email").len(), 3);
        assert_eq!(probes("lower()"), [("SELECT lower()".to_string(), 7)]);
    }
}
