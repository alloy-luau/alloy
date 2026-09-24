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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mlua::{Lua, Table, Value};
use serde_json::Value as Json;

/// The file name, beside or instead of `alloy.toml`.
pub const FILE_NAME: &str = ".config.aly";

/// The `require` string the compiled config uses for the runtime. The
/// loader answers it with the embedded runtime.
const RUNTIME_SPEC: &str = "@alloy-runtime";

/// How long a config may run before the load stops it.
const TIME_LIMIT: Duration = Duration::from_secs(2);

/// Runs a `.config.aly` and gives back the configuration as a table.
pub fn evaluate(path: &Path) -> Result<toml::Table, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    evaluate_source(&source, path)
}

/// Runs the source of a `.config.aly` that lives at `path`. The path
/// places a relative `require`.
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

    match convert(&Value::Table(config), Some(&schema), "")? {
        toml::Value::Table(table) => Ok(table),

        _ => Err("the config is no table".to_string()),
    }
}

/// The configuration a module gives: its default export, else its
/// export table or the table it returns.
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
fn module_file(base: &Path) -> Option<PathBuf> {
    let exts = ["aly", "luau", "lua"];

    exts.iter()
        .map(|e| PathBuf::from(format!("{}.{e}", base.display())))
        .chain(exts.iter().map(|e| base.join(format!("init.{e}"))))
        .find(|p| p.is_file())
}

/// A Luau value as TOML, shaped by the schema node that describes it.
/// `at` is the dotted key path, for the report.
fn convert(value: &Value, schema: Option<&Json>, at: &str) -> Result<toml::Value, String> {
    let kind = schema.and_then(|s| schema_type(s, value));

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
                let items = schema.and_then(|s| s.get("items"));
                let mut out = Vec::with_capacity(len);

                for i in 1..=len {
                    let v: Value = t.raw_get(i).map_err(lua_error)?;
                    out.push(convert(&v, items, &format!("{at}[{i}]"))?);
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
                out.insert(key, convert(&v, child, &path)?);
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

/// The JSON type a node names for a value: its own `type`, or the one
/// branch of an `anyOf` or a `oneOf` the value's kind fits.
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
fn luau_type(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) | Value::Number(_) => "number",

        Value::Function(_) => "function",

        other => other.type_name(),
    }
}

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
