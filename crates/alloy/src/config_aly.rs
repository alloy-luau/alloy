//! `.config.aly`: the project configuration written in Alloy.
//!
//! The file is a module. It compiles like any source and runs in an
//! embedded Luau VM, so code above the export runs on load. It gives
//! the configuration in one of three ways:
//!
//! ```alloy
//! export const build = { out = "dist" }      -- by value, one table a key
//! export default { build = { out = "dist" } } -- a default export
//! return { build = { out = "dist" } }         -- a return
//! ```
//!
//! The value becomes the same TOML table `alloy.toml` gives, so both
//! files deserialize through one path. The JSON schema of `alloy.toml`
//! guides the conversion: it says where an empty table is a list and
//! where a whole number is a float.

use std::collections::BTreeMap;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use mlua::{Lua, Table, Value};
use serde_json::Value as Json;

/// The file name, beside or instead of `alloy.toml`.
pub const FILE_NAME: &str = ".config.aly";

/// The `require` string the compiled config uses for the runtime. The
/// loader answers it with the embedded runtime.
#[cfg(not(target_arch = "wasm32"))]
const RUNTIME_SPEC: &str = "@alloy-runtime";

/// How long a config may run before the load stops it.
#[cfg(not(target_arch = "wasm32"))]
const TIME_LIMIT: Duration = Duration::from_secs(2);

/// Runs a `.config.aly` and gives back the configuration as a table.
pub fn evaluate(path: &Path) -> Result<toml::Table, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    evaluate_source(&source, path)
}

/// Runs the source of a `.config.aly` that lives at `path`. The path
/// places a relative `require`.
#[cfg(not(target_arch = "wasm32"))]
pub fn evaluate_source(source: &str, path: &Path) -> Result<toml::Table, String> {
    let lua = Lua::new();
    lua.sandbox(true).map_err(lua_error)?;
    let _ = lua.set_memory_limit(64 << 20);
    let deadline = Instant::now() + TIME_LIMIT;
    lua.set_interrupt(move |_| match Instant::now() > deadline {
        true => Err(mlua::Error::runtime(format!(
            "the config ran longer than {} seconds",
            TIME_LIMIT.as_secs()
        ))),

        false => Ok(mlua::VmState::Continue),
    });
    lua.set_named_registry_value("alloy_modules", lua.create_table().map_err(lua_error)?)
        .map_err(lua_error)?;

    let value = run_module(&lua, source, path)?;
    let config = pick(value)?;
    let schema = crate::schema::project(&[]);
    let mut problems = Vec::new();
    let table = convert(&Value::Table(config), Some(&schema), "", &mut problems)?;

    // Each value the schema refuses reports on the line of its key, the
    // way the editor reports it. The deserializer names only the first,
    // and neither its key nor its line.
    if !problems.is_empty() {
        let mut lines: Vec<(Option<usize>, &str)> = problems
            .iter()
            .map(|(at, message)| (key_line(source, at), message.as_str()))
            .collect();
        // A table gives its keys in no order, so the lines give it.
        lines.sort_by_key(|(line, _)| line.unwrap_or(usize::MAX));
        let shown: Vec<String> = lines
            .iter()
            .map(|(line, message)| match line {
                Some(line) => format!("{}:{line}: {message}", path.display()),

                None => format!("{}: {message}", path.display()),
            })
            .collect();

        return Err(shown.join("\n"));
    }

    match table {
        toml::Value::Table(table) => Ok(table),

        _ => Err("the config is no table".to_string()),
    }
}

/// The line that writes the key `at`, a dotted path such as
/// `lint.strict`: each key is the first one after the key before it.
/// `None` when the source writes no such key, since code builds it.
#[cfg(not(target_arch = "wasm32"))]
fn key_line(source: &str, at: &str) -> Option<usize> {
    let toks = alloy_syntax::lexer::lex(source).ok()?.toks;
    let text = |i: usize| {
        toks.get(i)
            .map_or("", |t| &source[t.start as usize..t.end as usize])
    };
    // `key =`, or `["key"] =`.
    let names = |i: usize, key: &str| {
        text(i) == key && text(i + 1) == "="
            || text(i).get(1..text(i).len().saturating_sub(1)) == Some(key)
                && matches!(toks[i].kind, alloy_syntax::lexer::TokKind::Str { .. })
                && text(i + 1) == "]"
                && text(i + 2) == "="
    };
    let mut from = 0;

    for key in at.split('.').map(|k| k.split('[').next().unwrap_or(k)) {
        from = (from..toks.len()).find(|&i| names(i, key))? + 1;
    }

    let start = toks[from - 1].start as usize;

    Some(source[..start].matches('\n').count() + 1)
}

/// The configuration a module gives: its default export, else its
/// export table or the table it returns.
#[cfg(not(target_arch = "wasm32"))]
fn pick(value: Value) -> Result<Table, String> {
    let table = match value {
        Value::Table(t) => t,

        Value::Nil => {
            return Err(
                "the config gives nothing; write `export default { ... }`, `return { ... }`, or `export const build = { ... }`"
                    .to_string(),
            );
        }

        other => {
            return Err(format!(
                "the config gives a {}; it must give a table",
                luau_type(&other)
            ));
        }
    };

    match table.raw_get::<Value>("default") {
        Ok(Value::Table(default)) => Ok(default),

        Ok(Value::Nil) => Ok(table),

        Ok(other) => Err(format!(
            "the default export is a {}; it must be a table",
            luau_type(&other)
        )),

        Err(e) => Err(lua_error(e)),
    }
}

/// Compiles and runs one module, with a `require` that reads paths
/// from the module's own folder.
#[cfg(not(target_arch = "wasm32"))]
fn run_module(lua: &Lua, source: &str, path: &Path) -> Result<Value, String> {
    let is_alloy = path.extension().is_some_and(|e| e == "aly" || e == "alx");
    let code = match is_alloy {
        true => compile(source, path)?,

        false => source.to_string(),
    };
    let env = module_env(lua, path.parent().unwrap_or(Path::new(".")))?;

    lua.load(code)
        .set_name(format!("@{}", path.display()))
        .set_environment(env)
        .eval::<Value>()
        .map_err(lua_error)
}

/// The Luau a config compiles to, or the first error, with its place.
#[cfg(not(target_arch = "wasm32"))]
fn compile(source: &str, path: &Path) -> Result<String, String> {
    let options = crate::EmitOptions {
        file_name: path.display().to_string(),
        std_require: RUNTIME_SPEC.to_string(),
        ..Default::default()
    };
    let out = crate::compile_with(source, &options).map_err(|e| {
        let (line, col) = crate::directives::line_col(source, e.offset);

        format!("{}:{line}:{col}: {}", path.display(), e.message)
    })?;

    match out.diagnostics.first() {
        Some(d) => {
            let (line, col) = crate::directives::line_col(source, d.start as usize);

            Err(format!("{}:{line}:{col}: {}", path.display(), d.message))
        }

        None => Ok(without_const(&out.ship)),
    }
}

/// The emit keeps `const`, which the analyzer reads and the embedded VM
/// does not. A config binds nothing twice, so `local` runs the same.
#[cfg(not(target_arch = "wasm32"))]
fn without_const(code: &str) -> String {
    let Ok(lexed) = alloy_syntax::lexer::lex_luau(code) else {
        return code.to_string();
    };
    let toks = &lexed.toks;
    let mut out = String::with_capacity(code.len());
    let mut cursor = 0;

    for (i, t) in toks.iter().enumerate() {
        let text = &code[t.start as usize..t.end as usize];
        let declares = toks
            .get(i + 1)
            .is_some_and(|n| n.kind == alloy_syntax::lexer::TokKind::Ident);

        if text == "const" && t.kind == alloy_syntax::lexer::TokKind::Ident && declares {
            out.push_str(&code[cursor..t.start as usize]);
            out.push_str("local");
            cursor = t.end as usize;
        }
    }

    out.push_str(&code[cursor..]);
    out
}

/// The globals of one module: the shared ones, and a `require` that
/// reads a relative path from `dir`.
#[cfg(not(target_arch = "wasm32"))]
fn module_env(lua: &Lua, dir: &Path) -> Result<Table, String> {
    let env = lua.create_table().map_err(lua_error)?;
    let meta = lua.create_table().map_err(lua_error)?;
    meta.set("__index", lua.globals()).map_err(lua_error)?;
    env.set_metatable(Some(meta)).map_err(lua_error)?;

    let dir = dir.to_path_buf();
    let require = lua
        .create_function(move |lua, spec: String| {
            require(lua, &dir, &spec).map_err(mlua::Error::runtime)
        })
        .map_err(lua_error)?;
    env.set("require", require).map_err(lua_error)?;

    Ok(env)
}

/// One `require` of a config: the runtime, or a module beside it. A
/// module runs once; the next `require` of it reads the same value.
#[cfg(not(target_arch = "wasm32"))]
fn require(lua: &Lua, dir: &Path, spec: &str) -> Result<Value, String> {
    let cache: Table = lua
        .named_registry_value("alloy_modules")
        .map_err(lua_error)?;
    let (key, run): (String, Box<dyn FnOnce() -> Result<Value, String>>) = if spec == RUNTIME_SPEC {
        (
            RUNTIME_SPEC.to_string(),
            Box::new(|| {
                lua.load(crate::RUNTIME)
                    .set_name("@alloy")
                    .eval::<Value>()
                    .map_err(lua_error)
            }),
        )
    } else if spec.starts_with("./") || spec.starts_with("../") {
        let path = module_file(&dir.join(spec)).ok_or_else(|| {
            format!(
                "\"{spec}\" names no module; no .aly, .luau, or .lua file at {}",
                dir.join(spec).display()
            )
        })?;
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

        (
            path.display().to_string(),
            Box::new(move || run_module(lua, &source, &path)),
        )
    } else {
        return Err(format!(
            "\"{spec}\" is no relative path; a config reaches another module by `./` or `../`"
        ));
    };

    if let Ok(held) = cache.raw_get::<Value>(key.as_str())
        && !held.is_nil()
    {
        return Ok(held);
    }

    let value = run()?;
    cache.raw_set(key, value.clone()).map_err(lua_error)?;

    Ok(value)
}

/// The file a relative module path names: the path with a source
/// extension, or the `init` file of the folder it names.
#[cfg(not(target_arch = "wasm32"))]
fn module_file(base: &Path) -> Option<PathBuf> {
    let exts = ["aly", "luau", "lua"];

    exts.iter()
        .map(|e| PathBuf::from(format!("{}.{e}", base.display())))
        .chain(exts.iter().map(|e| base.join(format!("init.{e}"))))
        .find(|p| p.is_file())
}

/// The byte ranges of the tables that give the config, in order: the
/// one a `return` or an `export default` gives, each exported `const`
/// or `local`, and a local the file gives by name.
fn tables(src: &str) -> Vec<(usize, usize)> {
    use alloy_syntax::ast::{DefaultExport, Expr, Stmt};

    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let toks = &lexed.toks;
    let options = alloy_syntax::parser::ParseOptions::for_path(Path::new(FILE_NAME));
    let (chunk, _) = alloy_syntax::parser::parse_lenient(src, toks, options);
    let stmts = &chunk.block.stmts;
    let name = |e: &Expr| match e {
        Expr::Name(n) => Some(n.text(src, toks)),

        _ => None,
    };
    let given: Vec<&str> = stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Return(r) => r.values.first().and_then(name),

            Stmt::ExportDefault {
                value: DefaultExport::Value(e),
                ..
            } => name(e),

            _ => None,
        })
        .collect();
    let mut out = Vec::new();

    for stmt in stmts {
        let values: Vec<&Expr> = match stmt.under_default() {
            Stmt::Return(r) => r.values.iter().collect(),

            Stmt::ExportDefault {
                value: DefaultExport::Value(e),
                ..
            } => vec![e],

            Stmt::Local(l)
                if l.exported
                    || matches!(stmt, Stmt::ExportDefault { .. })
                    || l.names
                        .iter()
                        .any(|b| given.contains(&b.name.text(src, toks))) =>
            {
                l.values.iter().collect()
            }

            _ => Vec::new(),
        };

        for e in values {
            if let Expr::Table { span, .. } = e {
                out.push((
                    toks[span.start as usize].start as usize,
                    toks[span.end as usize - 1].end as usize,
                ));
            }
        }
    }

    out
}

/// Formats a `.config.aly`. Each config table keeps the text its author
/// wrote, and the code around it formats as any source does.
pub fn format(src: &str, options: &crate::config::FmtConfig) -> Result<String, String> {
    let tables = tables(src);
    // A name stands in for each table while the rest formats. The
    // trailing `__` keeps `_1__` from matching inside `_10__`.
    let stub = |i: usize| format!("__alloy_config_{i}__");
    let mut text = src.to_string();

    for (i, &(start, end)) in tables.iter().enumerate().rev() {
        text.replace_range(start..end, &stub(i));
    }

    // The loader's Luau has no `const`, and a config names its values
    // once anyway, so its locals stay as written.
    let options = crate::config::FmtConfig {
        prefer_const: false,
        ..options.clone()
    };
    let mut out = crate::fmt::format_file(&text, &options)?;

    for (i, &(start, end)) in tables.iter().enumerate() {
        out = out.replacen(&stub(i), &src[start..end], 1);
    }

    Ok(out)
}

/// A key as a config writes it: bare when it is a name or a reserved
/// word, which a config takes as a key, else in brackets.
pub fn written_key(key: &str) -> String {
    match alloy_syntax::contextual::is_luau_reserved(key) {
        true => key.to_string(),

        false => crate::data::luau_key(key),
    }
}

/// `alloy.toml` as a `.config.aly`, laid out by the `[fmt]` table it
/// holds, with its comments. The result loads to the same config, or
/// the error says it does not.
pub fn from_toml(text: &str) -> Result<String, String> {
    let path = Path::new(crate::config::FILE_NAME);
    let config = crate::config::Config::parse(text, path).map_err(|e| e.to_string())?;
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut notes = Notes::read(text);
    let mut out = String::from("export default {\n");
    write_table(&table, "", 1, &mut notes, &mut out);
    out.push_str("}\n");

    // A comment whose line the table does not hold still comes along.
    let rest = notes.above.into_values().chain(notes.below.into_values());

    for c in rest
        .flatten()
        .chain(notes.trailing.into_values())
        .chain(notes.end)
    {
        out.push_str(&comment("", &c));
    }

    let out = crate::fmt::format_file(&out, &config.fmt.for_source(&out))?;
    let loaded = evaluate_source(&out, Path::new(FILE_NAME))
        .and_then(|t| crate::config::Config::from_table(t, path).map_err(|e| e.to_string()))?;

    match loaded == config {
        true => Ok(out),

        false => Err(format!(
            "the {FILE_NAME} this writes would load to another config"
        )),
    }
}

/// The comments of an `alloy.toml`, each by the dotted path of the line
/// it sits by. A block right under a line belongs to it; a block after
/// a blank line goes above the next one.
#[derive(Default)]
struct Notes {
    above: BTreeMap<String, Vec<String>>,
    below: BTreeMap<String, Vec<String>>,
    trailing: BTreeMap<String, String>,
    /// The blocks after the last line.
    end: Vec<String>,
}

impl Notes {
    // ponytail: a line scan, not a TOML parse. A `#` line inside a
    // multi-line string reads as a comment. The load check still holds
    // the values; a TOML parser with spans fixes the comments.
    fn read(text: &str) -> Self {
        let mut notes = Self::default();
        let mut table = String::new();
        let mut last: Option<String> = None;
        let mut pending: Vec<String> = Vec::new();

        for line in text.lines().map(str::trim) {
            let (code, note) = split_comment(line);
            let at = if code.starts_with('[') {
                table = dotted(code.trim_matches(['[', ']']));

                Some(table.clone())
            } else {
                code.split_once('=').map(|(key, _)| match table.is_empty() {
                    true => dotted(key),

                    false => format!("{table}.{}", dotted(key)),
                })
            };

            match (at, note) {
                (Some(at), note) => {
                    notes
                        .above
                        .entry(at.clone())
                        .or_default()
                        .append(&mut pending);

                    if let Some(note) = note {
                        notes.trailing.insert(at.clone(), note.to_string());
                    }

                    last = Some(at);
                }

                // `#:schema` names the JSON schema, which the new file
                // does not read.
                (None, Some(note)) if code.is_empty() && !note.starts_with(':') => match &last {
                    Some(at) => notes
                        .below
                        .entry(at.clone())
                        .or_default()
                        .push(note.to_string()),

                    None => pending.push(note.to_string()),
                },

                _ if line.is_empty() => {
                    last = None;

                    if pending.last().is_some_and(|c| !c.is_empty()) {
                        pending.push(String::new());
                    }
                }

                // The rest of a value that spans lines.
                _ => {}
            }
        }

        notes.end = pending;
        notes
    }
}

/// A line's code and its comment: the text after the first `#` outside
/// a string.
fn split_comment(line: &str) -> (&str, Option<&str>) {
    let mut quote = None;
    let mut escaped = false;

    for (i, c) in line.char_indices() {
        match (quote, c) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;

                continue;
            }

            (Some(q), c) if c == q && !escaped => quote = None,

            (None, '"' | '\'') => quote = Some(c),

            (None, '#') => return (line[..i].trim_end(), Some(line[i + 1..].trim())),

            _ => {}
        }

        escaped = false;
    }

    (line, None)
}

/// A TOML key, `a."b".c`, as the dotted path `a.b.c`.
fn dotted(key: &str) -> String {
    key.split('.')
        .map(|part| part.trim().trim_matches(['"', '\'']))
        .collect::<Vec<_>>()
        .join(".")
}

/// One comment line; an empty one is the blank line between blocks.
fn comment(pad: &str, text: &str) -> String {
    match text.is_empty() {
        true => "\n".to_string(),

        false => format!("{pad}-- {text}\n"),
    }
}

/// The fields of one table, one a line. The formatter lays them out
/// after, so the indent here only has to be whole.
fn write_table(table: &toml::Table, at: &str, depth: usize, notes: &mut Notes, out: &mut String) {
    let pad = "    ".repeat(depth);

    for (key, value) in table {
        let path = match at.is_empty() {
            true => key.clone(),

            false => format!("{at}.{key}"),
        };

        for c in notes.above.remove(&path).unwrap_or_default() {
            out.push_str(&comment(&pad, &c));
        }

        out.push_str(&format!("{pad}{} = ", written_key(key)));
        write_value(value, &path, depth, notes, out);
        out.push(',');

        if let Some(c) = notes.trailing.remove(&path) {
            out.push_str(&format!(" -- {c}"));
        }

        out.push('\n');

        // A table holds the block under its header at its top.
        if !value.is_table() {
            for c in notes.below.remove(&path).unwrap_or_default() {
                out.push_str(&comment(&pad, &c));
            }
        }
    }
}

fn write_value(value: &toml::Value, at: &str, depth: usize, notes: &mut Notes, out: &mut String) {
    match value {
        toml::Value::Table(t) => {
            out.push_str("{\n");

            for c in notes.below.remove(at).unwrap_or_default() {
                out.push_str(&comment(&"    ".repeat(depth + 1), &c));
            }

            write_table(t, at, depth + 1, notes, out);
            out.push_str(&"    ".repeat(depth));
            out.push('}');
        }

        toml::Value::Array(items) => {
            out.push('{');

            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }

                write_value(item, at, depth, notes, out);
            }

            out.push('}');
        }

        toml::Value::String(s) => out.push_str(&crate::data::luau_string(s)),

        toml::Value::Datetime(d) => out.push_str(&crate::data::luau_string(&d.to_string())),

        // An integer, a float, or a boolean reads the same in both.
        other => out.push_str(&other.to_string()),
    }
}

/// A Luau value as TOML, shaped by the schema node that describes it.
/// `at` is the dotted key path, for the report. A value of a type the
/// schema refuses adds its key and message to `problems`, and the walk
/// goes on. A word a key does not take is left to the deserializer,
/// whose message names the word: `` `Sgnal` is no std name ``.
#[cfg(not(target_arch = "wasm32"))]
fn convert(
    value: &Value,
    schema: Option<&Json>,
    at: &str,
    problems: &mut Vec<(String, String)>,
) -> Result<toml::Value, String> {
    let kind = schema.and_then(|s| schema_type(s, value));
    let (got, whole) = match value {
        Value::Boolean(_) => ("boolean", false),

        Value::Integer(_) => ("number", true),

        Value::Number(n) => ("number", n.fract() == 0.0),

        Value::String(_) => ("string", false),

        Value::Table(_) if kind == Some("array") => ("array", false),

        Value::Table(_) => ("object", false),

        // No config holds it, and the match below says so.
        _ => ("", false),
    };

    if let Some(node) = schema.filter(|_| !got.is_empty())
        && let Some(message) = misfit(node, &format!("`{at}`"), got, whole, None)
    {
        problems.push((at.to_string(), message));
    }

    match value {
        Value::Boolean(b) => Ok(toml::Value::Boolean(*b)),

        Value::Integer(n) => Ok(match kind {
            Some("number") => toml::Value::Float(*n as f64),

            _ => toml::Value::Integer(*n),
        }),

        Value::Number(n) => {
            let whole = n.fract() == 0.0 && n.is_finite() && n.abs() < 9.0e15;

            Ok(match (kind, whole) {
                (Some("number"), _) | (_, false) => toml::Value::Float(*n),

                _ => toml::Value::Integer(*n as i64),
            })
        }

        Value::String(s) => Ok(toml::Value::String(
            s.to_str().map_err(lua_error)?.to_string(),
        )),

        Value::Table(t) => {
            let len = t.raw_len();
            let pairs: Vec<(Value, Value)> = t
                .clone()
                .pairs::<Value, Value>()
                .collect::<Result<_, _>>()
                .map_err(lua_error)?;
            let is_list = match kind {
                Some("array") => true,

                Some("object") => false,

                _ => len > 0 && pairs.len() == len,
            };

            if is_list {
                let items = schema.and_then(item_schema);
                let mut out = Vec::with_capacity(len);

                for i in 1..=len {
                    let v: Value = t.raw_get(i).map_err(lua_error)?;
                    out.push(convert(&v, items, &format!("{at}[{i}]"), problems)?);
                }

                return Ok(toml::Value::Array(out));
            }

            let mut out = toml::Table::new();

            for (k, v) in pairs {
                let key = match &k {
                    Value::String(s) => s.to_str().map_err(lua_error)?.to_string(),

                    other => {
                        return Err(format!(
                            "`{at}` has a {} key; a config table takes names",
                            luau_type(other)
                        ));
                    }
                };
                let path = match at.is_empty() {
                    true => key.clone(),

                    false => format!("{at}.{key}"),
                };
                let child = schema.and_then(|s| property(s, &key));
                out.insert(key, convert(&v, child, &path, problems)?);
            }

            Ok(toml::Value::Table(out))
        }

        other => Err(format!(
            "`{at}` holds a {}, which a config cannot hold",
            luau_type(other)
        )),
    }
}

/// The schema of one property of an object node: its own entry, else
/// the shape every other key takes.
pub fn property<'a>(node: &'a Json, key: &str) -> Option<&'a Json> {
    node.get("properties")
        .and_then(|p| p.get(key))
        .or_else(|| node.get("additionalProperties").filter(|a| a.is_object()))
}

/// The schema of the items of a list node, directly or through the one
/// `anyOf` branch that is a list.
pub fn item_schema(node: &Json) -> Option<&Json> {
    node.get("items").or_else(|| {
        branches(node)
            .iter()
            .find(|b| b.get("type").and_then(Json::as_str) == Some("array"))
            .and_then(|b| b.get("items"))
    })
}

/// The `anyOf` and `oneOf` branches of a node.
pub fn branches(node: &Json) -> Vec<&Json> {
    ["anyOf", "oneOf"]
        .iter()
        .filter_map(|k| node.get(*k).and_then(Json::as_array))
        .flatten()
        .collect()
}

/// The JSON types a node takes, its own or its branches'.
pub fn types(node: &Json) -> Vec<&str> {
    match node.get("type") {
        Some(Json::String(t)) => vec![t.as_str()],

        Some(Json::Array(ts)) => ts.iter().filter_map(Json::as_str).collect(),

        _ => branches(node).into_iter().flat_map(types).collect(),
    }
}

/// The values a node takes by name: its own `enum`, else those of its
/// `oneOf` or `anyOf` branches, so a level that may also be a table
/// still lists its words.
pub fn enum_values(node: &Json) -> Vec<&Json> {
    match node.get("enum").and_then(Json::as_array) {
        Some(values) => values.iter().collect(),

        None => branches(node)
            .into_iter()
            .filter_map(|b| b.get("enum").and_then(Json::as_array))
            .flatten()
            .collect(),
    }
}

/// How a type reads in a report: `string`, `"a" | "b"`, `table`.
pub fn type_label(node: &Json) -> String {
    let values = enum_values(node);

    if !values.is_empty() {
        return values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
    }

    let words: Vec<String> = types(node)
        .into_iter()
        .map(|t| match t {
            "object" => "table".to_string(),

            "array" => match item_schema(node).map(types).as_deref() {
                Some([one]) => format!("{{ {one} }}"),

                _ => "list".to_string(),
            },

            "integer" => "number".to_string(),

            other => other.to_string(),
        })
        .collect();

    match words.is_empty() {
        true => "any".to_string(),

        false => words.join(" | "),
    }
}

/// A value as Alloy writes it: a string in quotes, the rest as JSON.
pub fn alloy_value(v: &Json) -> String {
    match v {
        Json::String(s) => format!("\"{s}\""),

        Json::Array(items) if items.is_empty() => "{}".to_string(),

        Json::Object(map) if map.is_empty() => "{}".to_string(),

        other => other.to_string(),
    }
}

/// Why a value does not fit its schema node, or `None` when it fits.
/// `got` is its JSON kind: `string`, `number`, `boolean`, `array`, or
/// `object`. `whole` says a number has no fraction, and `string` is the
/// text of a string; `None` skips the check of the words a key takes.
/// `name` is the key as the report writes it.
pub fn misfit(
    node: &Json,
    name: &str,
    got: &str,
    whole: bool,
    string: Option<&str>,
) -> Option<String> {
    let kinds = types(node);
    let fits = kinds.iter().any(|k| match *k {
        "integer" => got == "number" && whole,

        "number" => got == "number",

        other => other == got,
    });

    if !kinds.is_empty() && !fits {
        let wanted = match kinds.as_slice() {
            ["integer"] => "whole number".to_string(),

            _ => type_label(node),
        };
        let a = crate::desugar::article(&wanted);
        let got = match got {
            "array" | "object" => "table",

            other => other,
        };

        return Some(format!("{name} takes {a} {wanted}; this is a {got}"));
    }

    let values = enum_values(node);
    // A branch that takes any string makes the list a set of hints: a
    // lint name lists the known ones and still takes an ingot's.
    let open = branches(node)
        .iter()
        .any(|b| b.get("type").and_then(Json::as_str) == Some("string") && b.get("enum").is_none());
    let s = string?;

    if open || values.is_empty() || values.iter().any(|v| v.as_str() == Some(s)) {
        return None;
    }

    let list: Vec<String> = values.iter().map(|v| alloy_value(v)).collect();

    Some(format!(
        "{name} takes one of {}; `\"{s}\"` is none of them",
        list.join(", ")
    ))
}

/// The JSON type a node names for a value: its own `type`, or the one
/// branch of an `anyOf` or a `oneOf` the value's kind fits.
#[cfg(not(target_arch = "wasm32"))]
fn schema_type<'a>(node: &'a Json, value: &Value) -> Option<&'a str> {
    let fits = |t: &str| match value {
        Value::Table(_) => matches!(t, "array" | "object"),

        Value::String(_) => t == "string",

        Value::Boolean(_) => t == "boolean",

        Value::Integer(_) | Value::Number(_) => matches!(t, "integer" | "number"),

        _ => false,
    };

    if let Some(t) = node.get("type").and_then(Json::as_str) {
        return Some(t);
    }

    ["anyOf", "oneOf"]
        .iter()
        .filter_map(|k| node.get(*k).and_then(Json::as_array))
        .flatten()
        .filter_map(|branch| branch.get("type").and_then(Json::as_str))
        .find(|t| fits(t))
}

/// A value's type as Luau names it: a whole number is a `number` too.
#[cfg(not(target_arch = "wasm32"))]
fn luau_type(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) | Value::Number(_) => "number",

        Value::Function(_) => "function",

        other => other.type_name(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn lua_error(e: mlua::Error) -> String {
    match e {
        // The traceback names the VM's frames and the file again, and
        // the reader of a config needs the line and the message alone.
        mlua::Error::RuntimeError(m) | mlua::Error::SyntaxError { message: m, .. } => m
            .split("\nstack traceback:")
            .next()
            .unwrap_or_default()
            .to_string(),

        mlua::Error::CallbackError { cause, .. } => lua_error((*cause).clone()),

        other => other.to_string(),
    }
}

/// The wasm build has no Luau VM (see Cargo.toml).
#[cfg(target_arch = "wasm32")]
pub fn evaluate_source(_: &str, _: &Path) -> Result<toml::Table, String> {
    Err(format!("`{FILE_NAME}` needs the native build of alloy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> Result<toml::Table, String> {
        evaluate_source(src, Path::new("/tmp/project/.config.aly"))
    }

    #[test]
    fn the_three_styles_give_one_table() {
        let by_value = eval("local n = 2 + 2\nexport const build = { out = \"dist\" }\nexport local fmt = { indent_width = n }\n").unwrap();
        let default =
            eval("export default { build = { out = \"dist\" }, fmt = { indent_width = 4 } }\n")
                .unwrap();
        let named = eval("export default const config = { build = { out = \"dist\" }, fmt = { indent_width = 4 } }\n").unwrap();
        let returned =
            eval("return { build = { out = \"dist\" }, fmt = { indent_width = 4 } }\n").unwrap();

        for table in [&by_value, &default, &named, &returned] {
            assert_eq!(table["build"]["out"].as_str(), Some("dist"), "{table:?}");
            assert_eq!(
                table["fmt"]["indent_width"].as_integer(),
                Some(4),
                "{table:?}"
            );
        }
    }

    #[test]
    fn code_runs_on_load() {
        let t = eval("local parts = {}\nfor i = 1, 3 do\n    table.insert(parts, `src{i}`)\nend\nreturn { build = { [\"in\"] = parts[3], exclude = {} } }\n").unwrap();
        assert_eq!(t["build"]["in"].as_str(), Some("src3"));
        // The schema says `exclude` is a list, so `{}` is an empty one.
        assert_eq!(t["build"]["exclude"].as_array().map(Vec::len), Some(0));
    }

    /// `lint.naming` takes a style or a list, as `[lint.naming]` does.
    #[test]
    fn a_naming_style_is_a_string_or_a_list() {
        use crate::naming::{Style, Styles};

        let t = eval("export const lint = { naming = { variable = \"camelCase\", const = { \"SCREAMING_SNAKE_CASE\" } } }\n").unwrap();
        let config = crate::config::Config::from_table(t, Path::new(FILE_NAME)).unwrap();
        assert_eq!(config.lint.naming.variable, Styles(vec![Style::Camel]));
        assert_eq!(config.lint.naming.r#const, Styles(vec![Style::Screaming]));

        let bad = eval("export const lint = { naming = { variable = \"kebab-case\" } }\n").unwrap();
        assert!(crate::config::Config::from_table(bad, Path::new(FILE_NAME)).is_err());
    }

    #[test]
    fn a_reserved_word_is_a_bare_key() {
        let t = eval("export default { build = { in = \"lib\" } }\n").unwrap();
        assert_eq!(t["build"]["in"].as_str(), Some("lib"));
        // Outside a config the word still needs its brackets.
        let plain = crate::compile("local t = { in = 1 }\n").unwrap();
        assert_eq!(plain.diagnostics.len(), 1);
    }

    #[test]
    fn fmt_keeps_the_config_tables_as_written() {
        let options = crate::config::FmtConfig::default();
        let src = "local   x=1\nlocal extra = {  in = x }\nexport default {\n  build = {\n    in = \"src\",\n      exclude = { \"a\" ,\"b\"}\n  }\n}\n";
        let out = format(src, &options).unwrap();
        assert!(
            out.starts_with("local x = 1\nlocal extra = { in = x }\n"),
            "{out}"
        );
        assert!(out.ends_with(&src[src.find("export").unwrap()..]), "{out}");
    }

    #[test]
    fn the_template_migrates_with_its_comments_and_its_style() {
        let out = from_toml(crate::config::TEMPLATE).unwrap();
        // The template asks for single quotes and two spaces.
        assert!(out.contains("\n  build = {\n    in = 'src',\n"), "{out}");
        assert!(out.contains("wait_timeout = 5,\n    -- std_require = \"@alloy\"\n    -- erase_type_imports = false\n  },"), "{out}");
        assert!(out.contains("-- tailwind = \"ingots/tailwind\"\n"), "{out}");
        assert!(!out.contains("schema"), "{out}");

        // A key on its own line keeps its note; `[fmt]` sets the layout.
        let out = from_toml("[fmt]\nquote_style = \"force-double\" # house style\nindent_type = \"tabs\"\n\n[build]\n# where the code lives\nin = \"lib\"\n").unwrap();
        assert!(
            out.contains("\tfmt = {\n\t\tquote_style = \"force-double\", -- house style\n"),
            "{out}"
        );
        assert!(
            out.contains("\t\t-- where the code lives\n\t\tin = \"lib\",\n"),
            "{out}"
        );
    }

    #[test]
    fn a_float_key_takes_a_whole_number() {
        let t = eval("return { emit = { wait_timeout = 5 } }\n").unwrap();
        assert_eq!(t["emit"]["wait_timeout"].as_float(), Some(5.0));
    }

    #[test]
    fn the_errors_say_where() {
        assert!(eval("local x = 1\n").unwrap_err().contains("gives nothing"));
        let five = eval("return 5\n").unwrap_err();
        assert!(five.contains("gives a number"), "{five}");
        assert!(
            eval("while true do end\nreturn {}\n")
                .unwrap_err()
                .contains("ran longer")
        );
        assert!(
            eval("return { build = { out = print } }\n")
                .unwrap_err()
                .contains("`build.out` holds a function")
        );
        assert!(
            eval("local x: number = \n")
                .unwrap_err()
                .starts_with("/tmp/project/.config.aly:")
        );

        // A runtime error names the line once, with no traceback: the
        // traceback printed the path twice more.
        let runtime = eval("local t = nil\nreturn { build = { out = t.x } }\n").unwrap_err();
        assert_eq!(
            runtime,
            "/tmp/project/.config.aly:2: attempt to index nil with 'x'"
        );

        // A value of the wrong type named neither its key nor its line,
        // and only the first one showed. Each one reports, in line order.
        let typed = eval("local w = \"two\"\nexport const lint = { strict = \"yes\" }\nexport const fmt = {\n    quote_style = \"force-double\",\n    indent_width = w,\n    column_width = { 1 },\n}\n").unwrap_err();
        assert_eq!(
            typed,
            [
                "/tmp/project/.config.aly:2: `lint.strict` takes a boolean; this is a string",
                "/tmp/project/.config.aly:5: `fmt.indent_width` takes a whole number; this is a string",
                "/tmp/project/.config.aly:6: `fmt.column_width` takes a whole number; this is a table",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_config_runs_alloy_syntax_and_the_runtime() {
        let t = eval("local names = [ \"a\", \"b\" ]\nreturn { build = { exclude = names:map(function(n) return `{n}/*` end) } }\n").unwrap();
        let exclude: Vec<&str> = t["build"]["exclude"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(exclude, ["a/*", "b/*"]);
    }
}
