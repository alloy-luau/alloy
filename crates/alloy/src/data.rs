//! JSON and TOML files as Luau modules.
//!
//! `import data from "./data.json"` reads a static table. The emit
//! drops the extension, `require("./data")`, and the build writes the
//! document as `data.luau`: `return { ... }`. The type check and the
//! language server write the same file into their mirrors, so the
//! checker types the fields.
//!
//! Key order follows the document: both parsers run with
//! `preserve_order`. The module text is for people to read, one entry
//! per line; it does not keep the document's lines.

use std::path::Path;

/// The two document formats a require can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Toml,
}

impl Format {
    /// The format of a module spec or a file name, by its extension.
    pub fn of(spec: &str) -> Option<Format> {
        if spec.ends_with(".json") {
            Some(Format::Json)
        } else if spec.ends_with(".toml") {
            Some(Format::Toml)
        } else {
            None
        }
    }

    pub fn of_path(path: &Path) -> Option<Format> {
        Format::of(&path.to_string_lossy())
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Json => "JSON",

            Format::Toml => "TOML",
        }
    }
}

/// The module beside a data file that builds to the same `.luau`:
/// `x.aly`, `x.alx`, `x.luau`, or `x.lua` next to `x.json`.
pub fn module_beside(path: &Path) -> Option<std::path::PathBuf> {
    ["aly", "alx", "luau", "lua"]
        .iter()
        .map(|ext| path.with_extension(ext))
        .find(|p| p.is_file())
}

/// A module spec without its data extension: `./data.json` is
/// `./data`. Any other spec comes back as it is.
pub fn strip_spec(spec: &str) -> &str {
    match Format::of(spec) {
        Some(_) => &spec[..spec.len() - 5],

        None => spec,
    }
}

/// A project or tool file that happens to be JSON or TOML: never data,
/// so no mirror or build turns `alloy.toml` into an `alloy.luau` beside
/// the runtime.
pub fn is_project_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    matches!(
        name,
        "alloy.toml"
            | "luaux.toml"
            | "lest.toml"
            | "wally.toml"
            | "wally.lock"
            | "rokit.toml"
            | "aftman.toml"
            | "foreman.toml"
            | "pesde.toml"
            | "pesde.lock"
            | "ember.toml"
            | "ember.lock"
            | "lpm.toml"
            | "lpm.lock"
            | "larvae.toml"
            | "stylua.toml"
            | "selene.toml"
            | "Cargo.toml"
            | "package.json"
            | "package-lock.json"
            | "tsconfig.json"
            | "sourcemap.json"
            | "biome.json"
            | "deno.json"
    ) || name.ends_with(".project.json")
        || name.starts_with('.')
}

/// A quoted path literal with its data extension stripped: `"./x.json"`
/// is `"./x"`. Text that is not such a literal comes back as it is.
pub fn strip_literal(literal: &str) -> String {
    let quote = literal.chars().next();

    if let Some(q @ ('"' | '\'')) = quote
        && literal.len() >= 2
        && literal.ends_with(q)
    {
        let inner = &literal[1..literal.len() - 1];

        return format!("{q}{}{q}", strip_spec(inner));
    }

    literal.to_string()
}

/// A quoted path at `at` in `text`: the byte range of the literal with
/// its quotes, and the path inside. The literal ends on its line.
fn literal_at(text: &str, at: usize) -> Option<(usize, usize, &str)> {
    let q = text[at..].chars().next()?;

    if q != '"' && q != '\'' {
        return None;
    }

    let body = &text[at + 1..];
    let close = body.find(q)?;
    let inner = &body[..close];

    if inner.contains('\n') {
        return None;
    }

    Some((at, at + 1 + close + 1, inner))
}

/// Every `require("...")` of a data file in an emitted text with the
/// extension stripped. The text keeps its lines: only the literal
/// shrinks.
pub fn strip_requires(text: &str) -> String {
    if !text.contains(".json") && !text.contains(".toml") {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut from = 0;

    while let Some(i) = text[from..].find("require") {
        let start = from + i;
        let end = start + "require".len();
        let word_before = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let mut at = end;

        // `require("x")`, `require "x"`, `require ("x")`.
        at += text[at..].len() - text[at..].trim_start_matches([' ', '\t']).len();

        if text[at..].starts_with('(') {
            at += 1;
            at += text[at..].len() - text[at..].trim_start_matches([' ', '\t']).len();
        }

        match (word_before, literal_at(text, at)) {
            (false, Some((a, b, inner))) if Format::of(inner).is_some() => {
                out.push_str(&text[from..a]);
                out.push_str(&strip_literal(&text[a..b]));
                from = b;
            }

            _ => {
                out.push_str(&text[from..end]);
                from = end;
            }
        }
    }

    out.push_str(&text[from..]);

    out
}

/// The data files a source names: the path literal after `from`,
/// `import(`, or `require(`, with its byte range. A line that is a
/// comment does not count.
pub fn references(source: &str) -> Vec<crate::ImportRef> {
    let mut out = Vec::new();
    let mut line_start = 0;

    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();

        if trimmed.starts_with("--") {
            line_start += line.len();

            continue;
        }

        let mut from = 0;

        while let Some(i) = line[from..].find(['"', '\'']) {
            let at = from + i;
            let Some((a, b, inner)) = literal_at(line, at) else {
                break;
            };
            let head = line[..a].trim_end();
            let head = head.strip_suffix('(').unwrap_or(head).trim_end();
            let opens =
                head.ends_with("from") || head.ends_with("require") || head.ends_with("import");

            if opens && Format::of(inner).is_some() {
                out.push(crate::ImportRef {
                    start: (line_start + a) as u32,
                    end: (line_start + b) as u32,
                    path: inner.to_string(),
                });
            }

            from = b;
        }

        line_start += line.len();
    }

    out
}

/// One value of a document, in a shape both formats lower to.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

fn from_json(v: &serde_json::Value) -> Node {
    use serde_json::Value;

    match v {
        Value::Null => Node::Nil,

        Value::Bool(b) => Node::Bool(*b),

        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Node::Int(i),

            (None, Some(f)) => Node::Float(f),

            (None, None) => Node::Nil,
        },

        Value::String(s) => Node::Str(s.clone()),

        Value::Array(items) => Node::Array(items.iter().map(from_json).collect()),

        Value::Object(map) => {
            Node::Object(map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}

fn from_toml(v: &toml::Value) -> Node {
    use toml::Value;

    match v {
        Value::String(s) => Node::Str(s.clone()),

        Value::Integer(i) => Node::Int(*i),

        Value::Float(f) => Node::Float(*f),

        Value::Boolean(b) => Node::Bool(*b),

        // A datetime has no Luau shape; its text does.
        Value::Datetime(d) => Node::Str(d.to_string()),

        Value::Array(items) => Node::Array(items.iter().map(from_toml).collect()),

        Value::Table(map) => {
            Node::Object(map.iter().map(|(k, v)| (k.clone(), from_toml(v))).collect())
        }
    }
}

/// The document as a tree, or the parser's message on one line.
fn parse(text: &str, format: Format) -> Result<Node, String> {
    match format {
        Format::Json => serde_json::from_str::<serde_json::Value>(text)
            .map(|v| from_json(&v))
            .map_err(|e| e.to_string()),

        // `Table` reads a document; `Value` would read one value.
        Format::Toml => text
            .parse::<toml::Table>()
            .map(|t| from_toml(&toml::Value::Table(t)))
            .map_err(|e| {
                let message = e.message().trim().to_string();

                match e.span() {
                    Some(span) => {
                        let (line, col) = crate::directives::line_col(text, span.start);

                        format!("{message} at line {line} column {col}")
                    }

                    None => message,
                }
            }),
    }
}

/// Lowers a document to the text of a Luau module: `return { ... }`.
pub fn convert(text: &str, format: Format) -> Result<String, String> {
    let node = parse(text, format)?;
    let mut out = String::from("return ");
    render(&node, 0, &mut out);
    out.push('\n');

    Ok(out)
}

/// The top-level keys of a document and the Luau type of each value.
/// A document whose root is not an object has no keys.
pub fn keys(text: &str, format: Format) -> Result<Vec<(String, String)>, String> {
    match parse(text, format)? {
        Node::Object(fields) => Ok(fields
            .into_iter()
            .map(|(k, v)| (k, luau_type(&v)))
            .collect()),

        _ => Ok(Vec::new()),
    }
}

/// The zero-based line where a document defines a top-level key. For
/// TOML the key may head a table, `[key]` or `[[key]]`.
pub fn key_line(text: &str, format: Format, key: &str) -> Option<usize> {
    match format {
        Format::Json => json_key_line(text, key),

        Format::Toml => toml_key_line(text, key),
    }
}

fn json_key_line(text: &str, key: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut line = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => line += 1,

            b'{' | b'[' => depth += 1,

            b'}' | b']' => depth -= 1,

            b'"' => {
                let start = i + 1;
                let mut end = start;

                while end < bytes.len() && bytes[end] != b'"' {
                    if bytes[end] == b'\\' {
                        end += 1;
                    }

                    end += 1;
                }

                let inner = text.get(start..end).unwrap_or("");
                let after = text[end.min(text.len())..]
                    .trim_start_matches('"')
                    .trim_start();

                if depth == 1 && after.starts_with(':') && json_unescape(inner) == key {
                    return Some(line);
                }

                line += inner.matches('\n').count();
                i = end;
            }

            _ => {}
        }

        i += 1;
    }

    None
}

/// The key as written in a JSON string, with the common escapes undone.
fn json_unescape(raw: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string())
}

fn toml_key_line(text: &str, key: &str) -> Option<usize> {
    let bare = |k: &str| k.trim().trim_matches(['"', '\'']).to_string();
    // After the first header every plain key belongs to a table.
    let mut in_table = false;

    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();

        if let Some(header) = t.strip_prefix('[') {
            in_table = true;
            let header = header.strip_prefix('[').unwrap_or(header);
            let name = header.split([']', '.']).next().unwrap_or("");

            if bare(name) == key {
                return Some(i);
            }

            continue;
        }

        if in_table || t.starts_with('#') || t.is_empty() {
            continue;
        }

        let name = t.split(['=', '.']).next().unwrap_or("");

        if bare(name) == key && t[name.len()..].trim_start().starts_with(['=', '.']) {
            return Some(i);
        }
    }

    None
}

fn luau_type(node: &Node) -> String {
    match node {
        Node::Nil => "nil".to_string(),

        Node::Bool(_) => "boolean".to_string(),

        Node::Int(_) | Node::Float(_) => "number".to_string(),

        Node::Str(_) => "string".to_string(),

        Node::Array(items) => {
            let mut kinds: Vec<String> = Vec::new();

            for item in items {
                let t = luau_type(item);

                if !kinds.contains(&t) {
                    kinds.push(t);
                }
            }

            match kinds.len() {
                0 => "unknown[]".to_string(),

                1 => format!("{}[]", kinds[0]),

                _ => format!("({})[]", kinds.join(" | ")),
            }
        }

        Node::Object(fields) if fields.is_empty() => "{}".to_string(),

        Node::Object(_) => "{ ... }".to_string(),
    }
}

const INDENT: &str = "    ";

fn render(node: &Node, depth: usize, out: &mut String) {
    match node {
        Node::Nil => out.push_str("nil"),

        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),

        Node::Int(i) => out.push_str(&i.to_string()),

        Node::Float(f) => out.push_str(&luau_float(*f)),

        Node::Str(s) => out.push_str(&luau_string(s)),

        Node::Array(items) if items.is_empty() => out.push_str("{}"),

        Node::Array(items) => {
            out.push_str("{\n");

            for item in items {
                out.push_str(&INDENT.repeat(depth + 1));
                render(item, depth + 1, out);
                out.push_str(",\n");
            }

            out.push_str(&INDENT.repeat(depth));
            out.push('}');
        }

        Node::Object(fields) if fields.is_empty() => out.push_str("{}"),

        Node::Object(fields) => {
            out.push_str("{\n");

            for (key, value) in fields {
                out.push_str(&INDENT.repeat(depth + 1));
                out.push_str(&luau_key(key));
                out.push_str(" = ");
                render(value, depth + 1, out);
                out.push_str(",\n");
            }

            out.push_str(&INDENT.repeat(depth));
            out.push('}');
        }
    }
}

/// A number the way Luau reads it: a whole value without `.0`, and the
/// values JSON cannot write through `math.huge` and `0/0`.
fn luau_float(f: f64) -> String {
    if f.is_nan() {
        "0/0".to_string()
    } else if f.is_infinite() {
        if f > 0.0 {
            "math.huge".to_string()
        } else {
            "-math.huge".to_string()
        }
    } else if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f:?}")
    }
}

const LUAU_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// A table key: bare when it is a name Luau accepts, else in brackets.
pub fn luau_key(key: &str) -> String {
    let mut chars = key.chars();
    let is_name = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');

    if is_name && !LUAU_KEYWORDS.contains(&key) {
        key.to_string()
    } else {
        format!("[{}]", luau_string(key))
    }
}

/// A Luau string literal for any text. Non-ASCII text stays as it is;
/// a control character becomes a `\x` escape.
fn luau_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');

    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),

            '"' => out.push_str("\\\""),

            '\n' => out.push_str("\\n"),

            '\r' => out.push_str("\\r"),

            '\t' => out.push_str("\\t"),

            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }

            c => out.push(c),
        }
    }

    out.push('"');

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_objects_keep_their_order() {
        let text = r#"{ "zeta": 1, "alpha": { "b": true, "a": null }, "mid": "x" }"#;
        assert_eq!(
            convert(text, Format::Json).unwrap(),
            "return {\n    zeta = 1,\n    alpha = {\n        b = true,\n        a = nil,\n    },\n    mid = \"x\",\n}\n"
        );
    }

    #[test]
    fn arrays_of_objects_and_empty_containers() {
        let text =
            r#"{ "pets": [ { "name": "cat" }, { "name": "dog" } ], "none": {}, "empty": [] }"#;
        assert_eq!(
            convert(text, Format::Json).unwrap(),
            "return {\n    pets = {\n        {\n            name = \"cat\",\n        },\n        {\n            name = \"dog\",\n        },\n    },\n    none = {},\n    empty = {},\n}\n"
        );
        assert_eq!(convert("[]", Format::Json).unwrap(), "return {}\n");
        assert_eq!(
            convert("[1, 2]", Format::Json).unwrap(),
            "return {\n    1,\n    2,\n}\n"
        );
    }

    #[test]
    fn keys_that_are_not_names_go_in_brackets() {
        let text = r#"{ "end": 1, "with-dash": 2, "9lives": 3, "ok_1": 4, "": 5, "a b": 6 }"#;
        assert_eq!(
            convert(text, Format::Json).unwrap(),
            "return {\n    [\"end\"] = 1,\n    [\"with-dash\"] = 2,\n    [\"9lives\"] = 3,\n    ok_1 = 4,\n    [\"\"] = 5,\n    [\"a b\"] = 6,\n}\n"
        );
    }

    #[test]
    fn strings_escape_and_unicode_stays() {
        let text = "{ \"q\": \"say \\\"hi\\\"\\n\\ttab\\\\\", \"u\": \"héllo wörld ✓\", \"c\": \"\\u0001\" }";
        assert_eq!(
            convert(text, Format::Json).unwrap(),
            "return {\n    q = \"say \\\"hi\\\"\\n\\ttab\\\\\",\n    u = \"héllo wörld ✓\",\n    c = \"\\x01\",\n}\n"
        );
    }

    #[test]
    fn numbers_read_as_luau_literals() {
        let text = r#"{ "i": 3, "n": -7, "f": 1.5, "whole": 2.0, "big": 1e21, "tiny": 1e-7 }"#;
        assert_eq!(
            convert(text, Format::Json).unwrap(),
            "return {\n    i = 3,\n    n = -7,\n    f = 1.5,\n    whole = 2,\n    big = 1e21,\n    tiny = 1e-7,\n}\n"
        );
        assert_eq!(luau_float(f64::INFINITY), "math.huge");
        assert_eq!(luau_float(f64::NAN), "0/0");
    }

    #[test]
    fn toml_tables_arrays_and_datetimes() {
        let text = "title = \"cfg\"\nwhen = 2024-01-02T03:04:05Z\ninf = inf\n\n[limits]\nmax = 10\nratio = 0.5\n\n[[pets]]\nname = \"cat\"\n\n[[pets]]\nname = \"dog\"\n";
        assert_eq!(
            convert(text, Format::Toml).unwrap(),
            "return {\n    title = \"cfg\",\n    when = \"2024-01-02T03:04:05Z\",\n    inf = math.huge,\n    limits = {\n        max = 10,\n        ratio = 0.5,\n    },\n    pets = {\n        {\n            name = \"cat\",\n        },\n        {\n            name = \"dog\",\n        },\n    },\n}\n"
        );
        assert_eq!(
            keys(text, Format::Toml).unwrap(),
            vec![
                ("title".to_string(), "string".to_string()),
                ("when".to_string(), "string".to_string()),
                ("inf".to_string(), "number".to_string()),
                ("limits".to_string(), "{ ... }".to_string()),
                ("pets".to_string(), "{ ... }[]".to_string()),
            ]
        );
        assert_eq!(key_line(text, Format::Toml, "limits"), Some(4));
        assert_eq!(key_line(text, Format::Toml, "pets"), Some(8));
        assert_eq!(key_line(text, Format::Toml, "when"), Some(1));
        assert_eq!(key_line(text, Format::Toml, "max"), None);
    }

    #[test]
    fn a_broken_document_names_the_place() {
        let err = convert("{ \"a\": 1, }", Format::Json).unwrap_err();
        assert!(err.contains("line 1"), "{err}");
        let err = convert("a = \n", Format::Toml).unwrap_err();
        assert!(err.contains("line 1"), "{err}");
        assert!(!err.contains('\n'), "{err:?}");
    }

    #[test]
    fn keys_carry_their_types() {
        let text = r#"{ "n": 1, "s": "x", "b": true, "z": null, "xs": [1, 2], "mixed": [1, "a"], "o": { "k": 1 }, "e": [], "eo": {} }"#;
        let list = keys(text, Format::Json).unwrap();
        let get = |k: &str| list.iter().find(|(n, _)| n == k).map(|(_, t)| t.as_str());
        assert_eq!(get("n"), Some("number"));
        assert_eq!(get("s"), Some("string"));
        assert_eq!(get("b"), Some("boolean"));
        assert_eq!(get("z"), Some("nil"));
        assert_eq!(get("xs"), Some("number[]"));
        assert_eq!(get("mixed"), Some("(number | string)[]"));
        assert_eq!(get("o"), Some("{ ... }"));
        assert_eq!(get("e"), Some("unknown[]"));
        assert_eq!(get("eo"), Some("{}"));
        assert_eq!(keys("[1]", Format::Json).unwrap(), Vec::new());
    }

    #[test]
    fn json_key_lines_are_top_level_only() {
        let text = "{\n  \"a\": { \"b\": 1 },\n  \"b\": [\n    { \"c\": 2 }\n  ],\n  \"c\": 3\n}\n";
        assert_eq!(key_line(text, Format::Json, "a"), Some(1));
        assert_eq!(key_line(text, Format::Json, "b"), Some(2));
        assert_eq!(key_line(text, Format::Json, "c"), Some(5));
        assert_eq!(key_line(text, Format::Json, "d"), None);
    }

    #[test]
    fn specs_and_requires_lose_the_extension() {
        assert_eq!(strip_spec("./data.json"), "./data");
        assert_eq!(strip_spec("../cfg.toml"), "../cfg");
        assert_eq!(strip_spec("./mod"), "./mod");
        assert_eq!(strip_literal("\"./x.json\""), "\"./x\"");
        assert_eq!(strip_literal("'./x.toml'"), "'./x'");
        assert_eq!(strip_literal("\"./x\""), "\"./x\"");
        assert_eq!(
            strip_requires(
                "local a = require(\"./a.json\") local b = require './b.toml'\nlocal c = require(\"./c\")\n"
            ),
            "local a = require(\"./a\") local b = require './b'\nlocal c = require(\"./c\")\n"
        );
        assert_eq!(
            strip_requires("local x = myrequire(\"./a.json\")\n"),
            "local x = myrequire(\"./a.json\")\n"
        );
        assert_eq!(
            strip_requires("local t = { require = 1 } print(\"x.json\")\n"),
            "local t = { require = 1 } print(\"x.json\")\n"
        );
    }

    #[test]
    fn references_find_the_three_forms() {
        let src = "import data from \"./data.json\"\n-- import old from \"./old.json\"\nlocal x = import('./x.toml')\nlocal y = require(\"./y.json\")\nlocal z = require(\"./z\")\nprint(\"./no.json\")\n";
        let refs = references(src);
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["./data.json", "./x.toml", "./y.json"]);
        assert_eq!(
            &src[refs[0].start as usize..refs[0].end as usize],
            "\"./data.json\""
        );
        assert_eq!(
            &src[refs[1].start as usize..refs[1].end as usize],
            "'./x.toml'"
        );
    }
}
