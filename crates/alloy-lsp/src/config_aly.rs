//! `.config.aly`: completion, hover, and checks for the configuration
//! written in Alloy.
//!
//! Every answer reads the JSON schema of `alloy.toml`, the one the TOML
//! editor reads, so a key documented once is documented in both files.
//! The code here finds where the caret sits in the config table: the
//! chain of keys down to it, and whether a key or a value goes there.
//! It reads tokens, not a tree, because the file is half written while
//! the list is open.
//!
//! A config table hangs off a statement at the start of a line:
//! `return {`, `export default {`, `export default const c = {`,
//! `export const build = {`, or a local the file returns or exports as
//! its default. A table anywhere else is ordinary code.

use alloy::config::{FmtConfig, IndentType};
use alloy::fmt::requote;
use alloy_syntax::lexer::{Tok, TokKind};
use serde_json::{Value, json};

/// One step down the config: a key of a table, or an item of a list.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Key(String),
    Item,
}

/// What goes at the caret.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// A key of the table, with the word typed so far.
    Key { prefix: String },
    /// A value: after `key =`, or an item of a list (`key` is `None`).
    Value {
        key: Option<String>,
        prefix: String,
        quoted: bool,
    },
}

/// Where the caret sits: the keys down to its table, what goes there,
/// and the keys the table already writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub path: Vec<Seg>,
    pub slot: Slot,
    pub present: Vec<String>,
}

/// One problem a check finds: a byte range and the message.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub start: usize,
    pub end: usize,
    pub message: String,
}

/// Whether a document is a project's configuration.
pub fn is_config(uri: &str) -> bool {
    uri.rsplit(['/', '\\']).next() == Some(alloy::config_aly::FILE_NAME)
}

/// The tokens of a source and the text they come from.
struct Lexed<'a> {
    src: &'a str,
    /// The whole file, when `src` stops at the caret.
    whole: &'a str,
    toks: Vec<Tok>,
}

impl<'a> Lexed<'a> {
    fn new(src: &'a str) -> Option<Self> {
        Self::within(src, src)
    }

    fn within(src: &'a str, whole: &'a str) -> Option<Self> {
        let toks = alloy_syntax::lexer::lex(src).ok()?.toks;

        Some(Self { src, whole, toks })
    }

    fn text(&self, i: usize) -> &'a str {
        self.toks
            .get(i)
            .map_or("", |t| &self.src[t.start as usize..t.end as usize])
    }

    fn is_name(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == TokKind::Ident)
    }

    /// The content of a string token, without its quotes.
    fn string(&self, i: usize) -> Option<&'a str> {
        match self.toks.get(i)?.kind {
            TokKind::Str {
                inner_start,
                inner_end,
            } => Some(&self.src[inner_start as usize..inner_end as usize]),

            _ => None,
        }
    }

    /// Whether the token opens its line, at column 0.
    fn at_line_start(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| {
            let before = &self.src[..t.start as usize];

            before.is_empty() || before.ends_with('\n')
        })
    }

    /// The key a field names when its `=` sits at `eq`: `name =` or
    /// `["name"] =`.
    fn key_before(&self, eq: usize) -> Option<(String, usize)> {
        if eq >= 1 && self.is_name(eq - 1) {
            return Some((self.text(eq - 1).to_string(), eq - 1));
        }

        if eq >= 3 && self.text(eq - 1) == "]" && self.text(eq - 3) == "[" {
            return self.string(eq - 2).map(|s| (s.to_string(), eq - 3));
        }

        None
    }

    /// Whether only spaces sit before the token on its line. `export`
    /// stands at the top level wherever it is indented; a `return` or a
    /// `local` inside a function is indented, so those two keep column 0.
    fn opens_line(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| {
            let before = &self.src[..t.start as usize];

            before[before.rfind('\n').map_or(0, |n| n + 1)..]
                .trim()
                .is_empty()
        })
    }

    /// The name a `const` or `local` binds when its `=` sits at `eq`:
    /// `NAME =`, or `NAME: T =` with the type on the same line.
    fn bound_name(&self, eq: usize) -> Option<usize> {
        let name = (0..eq)
            .rev()
            .find(|&k| matches!(self.text(k), "const" | "local"))?
            + 1;
        let one_line = !self.src[self.toks.get(name)?.end as usize..self.toks[eq].start as usize]
            .contains('\n');

        (self.is_name(name) && (name + 1 == eq || self.text(name + 1) == ":") && one_line)
            .then_some(name)
    }

    /// The path of the config the table opened at `open` is: the whole
    /// config, one exported key of it, or `None` for ordinary code.
    fn root_path(&self, open: usize) -> Option<Vec<Seg>> {
        let prev = open.checked_sub(1)?;

        if self.text(prev) == "return" && self.at_line_start(prev) {
            return Some(Vec::new());
        }

        if self.text(prev) == "default"
            && prev >= 1
            && self.text(prev - 1) == "export"
            && self.opens_line(prev - 1)
        {
            return Some(Vec::new());
        }

        // `NAME =` or `NAME: T =` after `const` or `local`, with
        // `export` and `default` in front or none.
        if self.text(prev) != "=" {
            return None;
        }

        let at = self.bound_name(prev)?;
        let name = self.text(at);
        let decl = at - 1;
        let before = |n: usize| decl.checked_sub(n).map(|i| self.text(i));

        if before(1) == Some("default") && before(2) == Some("export") && self.opens_line(decl - 2)
        {
            return Some(Vec::new());
        }

        if before(1) == Some("export") && self.opens_line(decl - 1) {
            return Some(vec![Seg::Key(name.to_string())]);
        }

        (self.at_line_start(decl) && self.gives(name)).then(Vec::new)
    }

    /// Whether the file gives the local `name` as its config: a line
    /// that reads `return name` or `export default name`. The line may
    /// sit below the caret, so the whole file answers.
    fn gives(&self, name: &str) -> bool {
        self.whole.lines().any(|line| {
            let line = line
                .split("--")
                .next()
                .unwrap_or("")
                .trim_end()
                .trim_end_matches(';');

            line == format!("return {name}") || line == format!("export default {name}")
        })
    }

    /// The step a nested table takes from its parent: the key before its
    /// `=`, or an item of a list.
    fn owner(&self, open: usize) -> Option<Seg> {
        let prev = open.checked_sub(1)?;

        match self.text(prev) {
            "=" => self.key_before(prev).map(|(k, _)| Seg::Key(k)),

            "{" | "[" | "," | ";" => Some(Seg::Item),

            _ => None,
        }
    }

    /// The token that closes the bracket at `open`, if the text has it.
    fn close_of(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;

        for i in open..self.toks.len() {
            match self.text(i) {
                "{" | "[" | "(" => depth += 1,

                "}" | "]" | ")" => {
                    depth -= 1;

                    if depth == 0 {
                        return Some(i);
                    }
                }

                _ => {}
            }
        }

        None
    }

    /// The fields of the table opened at `open`, each as the token of its
    /// key (or `None` for a list item), the key text, and the first token
    /// of its value.
    fn fields(&self, open: usize) -> Vec<(Option<usize>, Option<String>, usize)> {
        let end = self.close_of(open).unwrap_or(self.toks.len());
        let mut out = Vec::new();
        let mut i = open + 1;

        while i < end {
            // One field runs to the next `,` or `;` at this depth.
            let mut j = i;
            let mut depth = 0i32;
            let mut eq = None;

            while j < end {
                match self.text(j) {
                    "{" | "[" | "(" => depth += 1,

                    "}" | "]" | ")" => depth -= 1,

                    "=" if depth == 0 && eq.is_none() => eq = Some(j),

                    "," | ";" if depth == 0 => break,

                    _ => {}
                }

                j += 1;
            }

            if j > i {
                match eq.and_then(|e| self.key_before(e).map(|k| (k, e))) {
                    Some(((key, at), e)) if at == i => out.push((Some(at), Some(key), e + 1)),

                    _ => out.push((None, None, i)),
                }
            }

            i = j + 1;
        }

        out
    }
}

/// Where the caret at `offset` sits in the config, or `None` when it
/// sits in no config table.
pub fn site_at(src: &str, offset: usize) -> Option<Site> {
    let offset = offset.min(src.len());
    let mut quoted = None;
    // The source up to the caret, and what the caret typed so far. An
    // open string leaves nothing to lex past its quote, so the quote
    // ends the text and its content is the prefix.
    let head = match Lexed::new(&src[..offset]) {
        Some(_) => &src[..offset],

        None => {
            let q = src[..offset].rfind(['"', '\''])?;
            quoted = Some(src[q + 1..offset].to_string());

            &src[..q]
        }
    };
    let lexed = Lexed::within(head, src)?;
    let mut last = lexed.toks.len();
    let mut prefix = String::new();
    let mut partial = None;

    // A word or a closed string the caret touches is still being typed.
    if quoted.is_none()
        && let Some(t) = lexed.toks.last()
        && t.end as usize == head.len()
    {
        if t.kind == TokKind::Ident {
            prefix = lexed.text(last - 1).to_string();
            last -= 1;
            partial = Some(last);
        } else if let TokKind::Str { .. } = t.kind
            && head.ends_with(['"', '\''])
            && t.end as usize == offset
        {
            // After the closing quote: nothing is typed.
        }
    }

    let mut frames: Vec<usize> = Vec::new();

    for i in 0..last {
        match lexed.text(i) {
            "{" | "[" | "(" => frames.push(i),

            "}" | "]" | ")" => {
                frames.pop();
            }

            _ => {}
        }
    }

    let open = *frames.last()?;

    if frames.iter().any(|&f| lexed.text(f) == "(") || lexed.text(frames[0]) != "{" {
        return None;
    }

    let mut path = lexed.root_path(frames[0])?;

    for &f in &frames[1..] {
        path.push(lexed.owner(f)?);
    }

    // The field the caret is in: from the last separator at this depth.
    let mut depth = 0i32;
    let mut field_start = open + 1;
    let mut eq = None;

    for i in open + 1..last {
        match lexed.text(i) {
            "{" | "[" | "(" => depth += 1,

            "}" | "]" | ")" => depth -= 1,

            "," | ";" if depth == 0 => {
                field_start = i + 1;
                eq = None;
            }

            "=" if depth == 0 && eq.is_none() => eq = Some(i),

            _ => {}
        }
    }

    let slot = match (eq, quoted) {
        (Some(e), quoted) => {
            let (key, _) = lexed.key_before(e)?;
            // Past the first token of the value, the caret is in an
            // expression the schema says nothing about.
            if last > e + 1 && quoted.is_none() {
                return None;
            }

            Slot::Value {
                key: Some(key),
                prefix: quoted.clone().unwrap_or(prefix),
                quoted: quoted.is_some(),
            }
        }

        (None, Some(text)) if field_start == last => Slot::Value {
            key: None,
            prefix: text,
            quoted: true,
        },

        (None, None) if field_start == last => match lexed.text(open) {
            "[" => Slot::Value {
                key: None,
                prefix,
                quoted: false,
            },

            _ => Slot::Key { prefix },
        },

        _ => return None,
    };

    // The keys the table already writes, from the whole text when it
    // lexes, so a key below the caret counts too.
    let present = match Lexed::new(src) {
        Some(full) if full.toks.len() > open && full.text(open) == lexed.text(open) => full
            .fields(open)
            .into_iter()
            .filter(|(at, _, _)| at.is_some() && *at != partial)
            .filter_map(|(_, key, _)| key)
            .collect(),

        _ => lexed
            .fields(open)
            .into_iter()
            .filter_map(|(_, key, _)| key)
            .collect(),
    };

    Some(Site {
        path,
        slot,
        present,
    })
}

/// The schema node a path names, from the root of the config.
pub fn node_at<'a>(schema: &'a Value, path: &[Seg]) -> Option<&'a Value> {
    let mut node = schema;

    for seg in path {
        node = match seg {
            Seg::Key(k) => alloy::config_aly::property(node, k)?,

            Seg::Item => item_schema(node)?,
        };
    }

    Some(node)
}

/// The schema of the items of a list node, directly or through the one
/// `anyOf` branch that is a list.
fn item_schema(node: &Value) -> Option<&Value> {
    node.get("items").or_else(|| {
        branches(node)
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("array"))
            .and_then(|b| b.get("items"))
    })
}

fn branches(node: &Value) -> Vec<&Value> {
    ["anyOf", "oneOf"]
        .iter()
        .filter_map(|k| node.get(*k).and_then(Value::as_array))
        .flatten()
        .collect()
}

/// The JSON types a node takes, its own or its branches'.
fn types(node: &Value) -> Vec<&str> {
    match node.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],

        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),

        _ => branches(node).into_iter().flat_map(types).collect(),
    }
}

/// The values a node takes by name: its own `enum`, else those of its
/// `oneOf` or `anyOf` branches, so a level that may also be a table
/// still lists its words.
fn enum_values(node: &Value) -> Vec<&Value> {
    match node.get("enum").and_then(Value::as_array) {
        Some(values) => values.iter().collect(),

        None => branches(node)
            .into_iter()
            .filter_map(|b| b.get("enum").and_then(Value::as_array))
            .flatten()
            .collect(),
    }
}

/// How a type reads in a report: `string`, `"a" | "b"`, `table`.
fn type_label(node: &Value) -> String {
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

/// How `luau_type` lays out a table.
#[derive(Clone, Copy)]
enum Layout {
    /// One line. A table inside a table shows as `{ ... }`.
    Line { nested: bool },
    /// One field a line, at this depth.
    Block(usize),
}

/// The most fields a table type lists. `fmt` has 23 keys and fits;
/// `lint.rules` names every lint, and a hover that long hides the rest.
const MAX_FIELDS: usize = 30;

/// The Luau type of the values a node takes: `string`, `"a" | "b"`,
/// `{ string }`, or a table with its fields. A key the schema does not
/// require is optional, `in: string?`.
fn luau_type(node: &Value, layout: Layout) -> String {
    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        let words: Vec<String> = values.iter().map(Value::to_string).collect();

        return words.join(" | ");
    }

    let mut parts: Vec<String> = match node.get("type") {
        Some(Value::String(t)) => vec![named_type(node, t, layout)],

        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(|t| named_type(node, t, layout))
            .collect(),

        _ => branches(node)
            .into_iter()
            .map(|b| luau_type(b, layout))
            .collect(),
    };
    parts.dedup();

    // `string` takes every string, so the names beside it are hints the
    // completion lists, and the type reads as Luau prints it.
    if parts.iter().any(|p| p == "string") {
        parts.retain(|p| !p.starts_with('"'));
    }

    match parts.is_empty() {
        true => "any".to_string(),

        false => parts.join(" | "),
    }
}

fn named_type(node: &Value, name: &str, layout: Layout) -> String {
    match name {
        "object" => table_type(node, layout),

        // `items` is one schema, or a list of them for a fixed pair.
        "array" => {
            let items: Vec<&Value> = match node.get("items") {
                Some(Value::Array(list)) => list.iter().collect(),

                Some(one) => vec![one],

                None => Vec::new(),
            };
            let mut kinds: Vec<String> = items.iter().map(|i| luau_type(i, layout)).collect();
            kinds.dedup();

            match kinds.is_empty() {
                true => "{ any }".to_string(),

                false => format!("{{ {} }}", kinds.join(" | ")),
            }
        }

        "integer" => "number".to_string(),

        "null" => "nil".to_string(),

        other => other.to_string(),
    }
}

fn table_type(node: &Value, layout: Layout) -> String {
    let required: Vec<&str> = node
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let inner = match layout {
        Layout::Line { .. } => Layout::Line { nested: true },

        Layout::Block(depth) => Layout::Block(depth + 1),
    };
    let mut fields: Vec<String> = node
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, child)| {
            let ty = luau_type(child, inner);
            let ty = match required.contains(&key.as_str()) {
                true => ty,

                false => optional(&ty),
            };

            format!("{}: {ty}", alloy::config_aly::written_key(key))
        })
        .collect();

    // With no `additionalProperties`, or `true`, a table takes any key.
    match node.get("additionalProperties") {
        Some(open @ Value::Object(_)) => {
            fields.push(format!("[string]: {}", luau_type(open, inner)));
        }

        None | Some(Value::Bool(true)) if fields.is_empty() => {
            fields.push("[string]: any".to_string());
        }

        _ => {}
    }

    let more = fields.len().saturating_sub(MAX_FIELDS);
    fields.truncate(MAX_FIELDS);

    match layout {
        _ if fields.is_empty() => "{}".to_string(),

        Layout::Line { nested: true } => "{ ... }".to_string(),

        Layout::Line { nested: false } => {
            if more > 0 {
                fields.push("...".to_string());
            }

            format!("{{ {} }}", fields.join(", "))
        }

        Layout::Block(depth) => {
            let pad = "    ".repeat(depth + 1);
            let mut out = String::from("{\n");

            for field in &fields {
                out.push_str(&format!("{pad}{field},\n"));
            }

            if more > 0 {
                out.push_str(&format!("{pad}-- and {more} more\n"));
            }

            out.push_str(&"    ".repeat(depth));
            out.push('}');
            out
        }
    }
}

/// A type that may also be nil. A union takes parentheses first, so the
/// `?` covers all of it.
fn optional(ty: &str) -> String {
    let mut depth = 0i32;
    let union = ty.chars().any(|c| {
        match c {
            '{' | '(' => depth += 1,

            '}' | ')' => depth -= 1,

            _ => {}
        }

        c == '|' && depth == 0
    });

    match union {
        true => format!("({ty})?"),

        false => format!("{ty}?"),
    }
}

/// A value as Alloy writes it: a string in quotes, the rest as JSON.
fn alloy_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),

        Value::Array(items) if items.is_empty() => "{}".to_string(),

        Value::Object(map) if map.is_empty() => "{}".to_string(),

        other => other.to_string(),
    }
}

/// The documentation of one key: its type, its default, and the text
/// the schema gives it.
fn documentation(key: &str, node: &Value) -> String {
    let mut out = format!(
        "```alloy\n{key}: {}\n```",
        luau_type(node, Layout::Block(0))
    );

    if let Some(text) = node.get("description").and_then(Value::as_str) {
        out.push_str("\n\n");
        out.push_str(text);
    }

    if let Some(default) = node.get("default") {
        out.push_str(&format!("\n\nDefault: `{}`", alloy_value(default)));
    }

    out
}

/// A snippet placeholder's text, with the characters a snippet reads
/// escaped.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('$', "\\$")
        .replace('}', "\\}")
        .replace(',', "\\,")
        .replace('|', "\\|")
}

/// A value as the project writes it: a string in the quotes `[fmt]`
/// asks for.
fn written_value(v: &Value, fmt: &FmtConfig) -> String {
    requote(&alloy_value(v), fmt.quote_style)
}

/// The value a new key starts with, as a snippet, laid out the way the
/// project's `[fmt]` lays it out.
fn value_snippet(node: &Value, fmt: &FmtConfig) -> String {
    let values = enum_values(node);
    let default = node.get("default");

    if !values.is_empty() {
        // The default leads, so Tab keeps it.
        let mut ordered: Vec<&Value> = default.into_iter().filter(|d| values.contains(d)).collect();
        ordered.extend(values.iter().filter(|v| Some(**v) != default));
        let texts: Vec<String> = ordered
            .iter()
            .map(|v| escape(&written_value(v, fmt)))
            .collect();

        return format!("${{1|{}|}}", texts.join(","));
    }

    let kinds = types(node);

    match kinds.as_slice() {
        ["boolean"] => match default {
            Some(Value::Bool(false)) => "${1|false,true|}".to_string(),

            _ => "${1|true,false|}".to_string(),
        },

        ["string"] => requote(
            &format!(
                "\"${{1:{}}}\"",
                escape(default.and_then(Value::as_str).unwrap_or(""))
            ),
            fmt.quote_style,
        ),

        ["integer"] | ["number"] => format!(
            "${{1:{}}}",
            default
                .map(|d| d.to_string())
                .unwrap_or_else(|| "0".to_string())
        ),

        ["object"] => match fmt.indent_type {
            IndentType::Tabs => "{\n\t$0\n}".to_string(),

            IndentType::Spaces => format!("{{\n{}$0\n}}", " ".repeat(fmt.indent_width)),
        },

        ["array"] if fmt.space_inside_braces => "{ $1 }".to_string(),

        ["array"] => "{$1}".to_string(),

        _ => "$1".to_string(),
    }
}

/// The completion items at a site, in the layout `fmt` gives.
pub fn completions(schema: &Value, site: &Site, fmt: &FmtConfig) -> Vec<Value> {
    let Some(node) = node_at(schema, &site.path) else {
        return Vec::new();
    };

    match &site.slot {
        // A key slot of a list is an item.
        Slot::Key { .. } if types(node) == ["array"] => item_schema(node)
            .map(|n| value_items(n, false, fmt))
            .unwrap_or_default(),

        Slot::Key { .. } => key_items(node, &site.present, fmt),

        Slot::Value {
            key: Some(k),
            quoted,
            ..
        } => alloy::config_aly::property(node, k)
            .map(|n| value_items(n, *quoted, fmt))
            .unwrap_or_default(),

        Slot::Value {
            key: None, quoted, ..
        } => item_schema(node)
            .map(|n| value_items(n, *quoted, fmt))
            .unwrap_or_default(),
    }
}

fn key_items(node: &Value, present: &[String], fmt: &FmtConfig) -> Vec<Value> {
    let mut keys: Vec<(String, &Value)> = node
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.iter().map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default();

    // A table with open keys may name the ones it knows under
    // `propertyNames`; each takes the shape of every other key.
    if let (Some(names), Some(shape)) = (
        node.get("propertyNames"),
        node.get("additionalProperties").filter(|a| a.is_object()),
    ) {
        for name in enum_values(names).into_iter().filter_map(Value::as_str) {
            if !keys.iter().any(|(k, _)| k == name) {
                keys.push((name.to_string(), shape));
            }
        }
    }

    keys.into_iter()
        .enumerate()
        .filter(|(_, (k, _))| !present.contains(k))
        .map(|(n, (k, child))| {
            let written = match alloy::config_aly::written_key(&k) {
                w if w.starts_with('[') => {
                    format!("[{}]", requote(&w[1..w.len() - 1], fmt.quote_style))
                }

                w => w,
            };

            json!({
                "label": k,
                "kind": 10,
                "detail": luau_type(child, Layout::Line { nested: false }),
                "documentation": { "kind": "markdown", "value": documentation(&k, child) },
                "insertText": format!("{written} = {}", value_snippet(child, fmt)),
                "insertTextFormat": 2,
                "filterText": k,
                "sortText": format!("{n:04}"),
            })
        })
        .collect()
}

fn value_items(node: &Value, quoted: bool, fmt: &FmtConfig) -> Vec<Value> {
    let default = node.get("default");
    let item = |v: &Value, n: usize| {
        let text = match (v, quoted) {
            (Value::String(s), true) => s.clone(),

            _ => written_value(v, fmt),
        };
        let is_default = Some(v) == default;

        json!({
            "label": text,
            "kind": 12,
            "detail": if is_default { "default" } else { "" },
            "insertText": text,
            "sortText": format!("{}{n:04}", if is_default { "0" } else { "1" }),
            "preselect": is_default,
        })
    };
    let values = enum_values(node);

    if !values.is_empty() {
        return values.iter().enumerate().map(|(n, v)| item(v, n)).collect();
    }

    if types(node) == ["boolean"] && !quoted {
        return [Value::Bool(true), Value::Bool(false)]
            .iter()
            .enumerate()
            .map(|(n, v)| item(v, n))
            .collect();
    }

    Vec::new()
}

/// The hover of the key under the caret: its path, type, default, and
/// text.
pub fn hover(schema: &Value, src: &str, offset: usize) -> Option<String> {
    let lexed = Lexed::new(src)?;
    let at = lexed
        .toks
        .iter()
        .position(|t| (t.start as usize) <= offset && offset < t.end as usize)?;
    // `name =`, or the string of `["name"] =`.
    let (key_tok, eq) = match (lexed.is_name(at), lexed.string(at).is_some()) {
        (true, _) if lexed.text(at + 1) == "=" => (at, at + 1),

        (_, true)
            if lexed.text(at - 1) == "["
                && lexed.text(at + 1) == "]"
                && lexed.text(at + 2) == "=" =>
        {
            (at - 1, at + 2)
        }

        _ => return None,
    };
    let (key, _) = lexed.key_before(eq)?;
    let site = site_at(src, lexed.toks[key_tok].start as usize)?;
    let node = node_at(schema, &site.path)?;

    // A table of open keys lists the names it knows; a key outside the
    // list is a misspelling, and the check says so on the key. A name
    // with a `/` or a `.` is an ingot's or a prefix's, which the list
    // cannot hold.
    let known = node
        .get("propertyNames")
        .map(enum_values)
        .unwrap_or_default();

    if !known.is_empty()
        && !key.contains(['/', '.'])
        && !known.iter().any(|v| v.as_str() == Some(&key))
    {
        return None;
    }

    let child = alloy::config_aly::property(node, &key)?;
    let mut dotted: Vec<String> = site
        .path
        .iter()
        .map(|s| match s {
            Seg::Key(k) => k.clone(),

            Seg::Item => "[]".to_string(),
        })
        .collect();
    dotted.push(key.clone());

    Some(documentation(&dotted.join("."), child))
}

/// The keys and literal values that do not fit the schema. A value that
/// is an expression runs on load, and the load reports it.
pub fn check(schema: &Value, src: &str) -> Vec<Problem> {
    let Some(lexed) = Lexed::new(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for open in 0..lexed.toks.len() {
        if lexed.text(open) == "{"
            && let Some(path) = lexed.root_path(open)
            && let Some(node) = node_at(schema, &path)
        {
            check_table(&lexed, open, node, &path, &mut out);
        }
    }

    out
}

fn check_table(lexed: &Lexed, open: usize, node: &Value, path: &[Seg], out: &mut Vec<Problem>) {
    let is_list = types(node) == ["array"];

    for (key_tok, key, value) in lexed.fields(open) {
        let child = match (&key, key_tok) {
            (Some(k), Some(at)) => match alloy::config_aly::property(node, k) {
                Some(child) => child,

                None => {
                    if closed(node) {
                        let t = &lexed.toks[at];
                        out.push(Problem {
                            start: t.start as usize,
                            end: lexed.toks[lexed.key_end(at)].end as usize,
                            message: unknown_key(node, k, path),
                        });
                    }

                    continue;
                }
            },

            _ if is_list => match item_schema(node) {
                Some(items) => items,

                None => continue,
            },

            _ => continue,
        };
        let mut below = path.to_vec();
        below.push(match key {
            Some(k) => Seg::Key(k),

            None => Seg::Item,
        });

        if lexed.text(value) == "{" {
            check_table(lexed, value, child, &below, out);
        } else if let Some(problem) = check_literal(lexed, value, child, &below) {
            out.push(problem);
        }
    }
}

impl Lexed<'_> {
    /// The last token of the key that starts at `at`: the name, or the
    /// `]` of `["name"]`.
    fn key_end(&self, at: usize) -> usize {
        match self.text(at) {
            "[" => at + 2,

            _ => at,
        }
    }
}

/// Whether a table node takes only the keys it names.
fn closed(node: &Value) -> bool {
    node.get("properties").is_some()
        && !node
            .get("additionalProperties")
            .is_some_and(|a| a.is_object() || a == &Value::Bool(true))
}

fn table_name(path: &[Seg]) -> String {
    let keys: Vec<&str> = path
        .iter()
        .filter_map(|s| match s {
            Seg::Key(k) => Some(k.as_str()),

            Seg::Item => None,
        })
        .collect();

    match keys.is_empty() {
        true => "the config".to_string(),

        false => format!("`{}`", keys.join(".")),
    }
}

fn unknown_key(node: &Value, key: &str, path: &[Seg]) -> String {
    let names: Vec<&String> = node
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.keys().collect())
        .unwrap_or_default();
    let near = names
        .iter()
        .map(|n| (strsim_distance(n, key), n))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| format!("; did you mean `{n}`?"))
        .unwrap_or_default();

    format!("`{key}` is no key of {}{near}", table_name(path))
}

/// The edit distance of two short words.
fn strsim_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(here + 1);
            prev = here;
        }
    }

    row[b.len()]
}

/// A literal value that does not fit its key: a single token, followed
/// by the end of its field.
fn check_literal(lexed: &Lexed, at: usize, node: &Value, path: &[Seg]) -> Option<Problem> {
    if !matches!(lexed.text(at + 1), "," | ";" | "}" | "]" | "") {
        return None;
    }

    let text = lexed.text(at);
    let (got, string) = match lexed.toks.get(at)?.kind {
        TokKind::Str { .. } => ("string", lexed.string(at)),

        TokKind::Number => ("number", None),

        TokKind::Ident if matches!(text, "true" | "false") => ("boolean", None),

        _ => return None,
    };
    let kinds = types(node);
    let fits = kinds.iter().any(|k| match *k {
        "integer" => {
            got == "number" && !text.contains(['.', 'e', 'E'])
                || got == "number" && text.starts_with("0x")
        }

        "number" => got == "number",

        other => other == got,
    });
    let name = match path.last() {
        Some(Seg::Key(k)) => format!("`{k}`"),

        _ => "this item".to_string(),
    };
    let t = lexed.toks.get(at)?;
    let problem = |message: String| Problem {
        start: t.start as usize,
        end: t.end as usize,
        message,
    };

    if !kinds.is_empty() && !fits {
        let wanted = match kinds.as_slice() {
            ["integer"] => "whole number".to_string(),

            _ => type_label(node),
        };

        return Some(problem(format!("{name} takes a {wanted}; this is a {got}")));
    }

    let values = enum_values(node);
    // A branch that takes any string makes the list a set of hints: a
    // lint name lists the known ones and still takes an ingot's.
    let open = branches(node).iter().any(|b| {
        b.get("type").and_then(Value::as_str) == Some("string") && b.get("enum").is_none()
    });

    if let Some(s) = string
        && !open
        && !values.is_empty()
        && !values.iter().any(|v| v.as_str() == Some(s))
    {
        let list: Vec<String> = values.iter().map(|v| alloy_value(v)).collect();

        return Some(problem(format!(
            "{name} takes one of {}; `\"{s}\"` is none of them",
            list.join(", ")
        )));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        alloy::schema::project(&[])
    }

    /// The items under the default `[fmt]`: single quotes, two spaces.
    fn completions(schema: &Value, site: &Site) -> Vec<Value> {
        super::completions(schema, site, &FmtConfig::default())
    }

    /// The site at the `|` of a source.
    fn at(src: &str) -> Option<Site> {
        let offset = src.find('|').unwrap();

        site_at(&src.replace('|', ""), offset)
    }

    fn key(path: &[&str], prefix: &str) -> Option<(Vec<Seg>, Slot)> {
        Some((
            path.iter().map(|k| Seg::Key(k.to_string())).collect(),
            Slot::Key {
                prefix: prefix.to_string(),
            },
        ))
    }

    fn place(src: &str) -> Option<(Vec<Seg>, Slot)> {
        at(src).map(|s| (s.path, s.slot))
    }

    #[test]
    fn every_style_roots_the_config() {
        assert_eq!(place("export default {\n    |\n}\n"), key(&[], ""));
        assert_eq!(place("return {\n    bu|\n}\n"), key(&[], "bu"));
        assert_eq!(
            place("export default const config = {\n    |\n}\n"),
            key(&[], "")
        );
        assert_eq!(
            place("export const build = {\n    o|\n}\n"),
            key(&["build"], "o")
        );
        assert_eq!(place("export local fmt = { | }\n"), key(&["fmt"], ""));
        assert_eq!(
            place("local config = {\n    |\n}\nexport default config\n"),
            key(&[], "")
        );
        assert_eq!(
            place("local config = {\n    |\n}\nreturn config\n"),
            key(&[], "")
        );
        // The loader runs these too: an indented `export`, and a const
        // with a type.
        assert_eq!(place("  export default {\n    |\n  }\n"), key(&[], ""));
        assert_eq!(
            place("export default const config: any = {\n    |\n}\n"),
            key(&[], "")
        );
        assert_eq!(
            place("export const build: { out: string } = {\n    o|\n}\n"),
            key(&["build"], "o")
        );
    }

    #[test]
    fn ordinary_code_is_no_config() {
        assert_eq!(place("local t = {\n    |\n}\nprint(t)\n"), None);
        assert_eq!(place("local function f()\n    return { | }\nend\n"), None);
        assert_eq!(place("export default { build = f({ | }) }\n"), None);
        assert_eq!(place("print({ | })\n"), None);
    }

    #[test]
    fn nested_tables_walk_their_keys() {
        assert_eq!(
            place(
                "export default {\n    fmt = {\n        alx = {\n            |\n        },\n    },\n}\n"
            ),
            key(&["fmt", "alx"], "")
        );
        assert_eq!(
            place("export default { build = { out = \"dist\", | } }\n"),
            key(&["build"], "")
        );
        assert_eq!(
            place("export default { [\"build\"] = { | } }\n"),
            key(&["build"], "")
        );
    }

    #[test]
    fn a_value_slot_names_its_key() {
        assert_eq!(
            place("export default { fmt = { quote_style = | } }\n"),
            Some((
                vec![Seg::Key("fmt".into())],
                Slot::Value {
                    key: Some("quote_style".into()),
                    prefix: String::new(),
                    quoted: false
                }
            ))
        );
        assert_eq!(
            place("export default { fmt = { quote_style = \"auto-|\" } }\n"),
            Some((
                vec![Seg::Key("fmt".into())],
                Slot::Value {
                    key: Some("quote_style".into()),
                    prefix: "auto-".into(),
                    quoted: true
                }
            ))
        );
        // Past the first token of a value the caret is in an expression.
        assert_eq!(
            place("export default { build = { out = base .. | } }\n"),
            None
        );
    }

    #[test]
    fn the_keys_come_from_the_schema_and_skip_the_written_ones() {
        let site = at("export default {\n    build = {\n        out = \"dist\",\n        |\n        clean = true,\n    },\n}\n").unwrap();
        assert_eq!(site.present, vec!["out".to_string(), "clean".to_string()]);
        let items = completions(&schema(), &site);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"in"), "{labels:?}");
        assert!(labels.contains(&"exclude"), "{labels:?}");
        assert!(!labels.contains(&"out"), "{labels:?}");
        assert!(!labels.contains(&"clean"), "{labels:?}");

        // `in` is a Luau word, and a config still writes it bare.
        let input = items.iter().find(|i| i["label"] == "in").unwrap();
        assert_eq!(input["insertText"], "in = '${1:src}'");
        assert!(
            input["documentation"]["value"]
                .as_str()
                .unwrap()
                .contains("The source root"),
            "{input}"
        );

        // The top level offers the tables.
        let top = completions(&schema(), &at("return {\n    |\n}\n").unwrap());
        let labels: Vec<&str> = top.iter().filter_map(|i| i["label"].as_str()).collect();

        for table in ["build", "fmt", "lint", "emit", "mount", "project"] {
            assert!(labels.contains(&table), "{table}: {labels:?}");
        }

        let fmt = top.iter().find(|i| i["label"] == "fmt").unwrap();
        assert_eq!(fmt["insertText"], "fmt = {\n  $0\n}");

        // Another `[fmt]` gives other quotes, indent, and braces.
        let house = FmtConfig {
            quote_style: alloy::config::QuoteStyle::ForceDouble,
            indent_type: IndentType::Tabs,
            space_inside_braces: false,
            ..FmtConfig::default()
        };
        let keys = super::completions(&schema(), &site, &house);
        let text =
            |label: &str| keys.iter().find(|i| i["label"] == label).unwrap()["insertText"].clone();
        assert_eq!(text("in"), "in = \"${1:src}\"");
        assert_eq!(text("exclude"), "exclude = {$1}");
        let top = super::completions(&schema(), &at("return {\n    |\n}\n").unwrap(), &house);
        let fmt = top.iter().find(|i| i["label"] == "fmt").unwrap();
        assert_eq!(fmt["insertText"], "fmt = {\n\t$0\n}");
    }

    /// A table key reads as the Luau type of its table: each field, `?`
    /// on a key the schema does not require, and a long table cut short.
    #[test]
    fn a_table_key_shows_its_fields() {
        let src = "export default { build = {}, lint = {} }\n";
        let build = hover(&schema(), src, src.find("build").unwrap() + 1).unwrap();
        assert!(
            build.starts_with("```alloy\nbuild: {\n    in: string?,\n    out: string?,\n    exclude: { string }?,\n    clean: boolean?,\n    artifact: (\"ship\" | \"check\")?,\n}\n```"),
            "{build}"
        );

        // The lint names beside `string` are hints; `rules` names them
        // all, so it stops at the cap.
        let lint = hover(&schema(), src, src.find("lint").unwrap() + 1).unwrap();
        assert!(lint.contains("\n    deny: { string }?,\n"), "{lint}");
        assert!(lint.contains(" more\n    }?,\n}"), "{lint}");

        // The completion's detail is one line, with nested tables folded.
        let top = completions(&schema(), &at("return {\n    |\n}\n").unwrap());
        let detail =
            |label: &str| top.iter().find(|i| i["label"] == label).unwrap()["detail"].clone();
        assert_eq!(
            detail("emit"),
            "{ wait_timeout: number?, std_require: string?, erase_type_imports: boolean? }"
        );
        assert_eq!(detail("mount"), "{ [string]: { string } | string }");
        assert_eq!(detail("ingots"), "{ [string]: string | { ... } }");
    }

    #[test]
    fn a_value_offers_the_choices_the_schema_names() {
        let items = completions(
            &schema(),
            &at("export default { fmt = { quote_style = | } }\n").unwrap(),
        );
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"'force-single'"), "{labels:?}");
        assert!(labels.contains(&"'auto-prefer-double'"), "{labels:?}");

        let inside = completions(
            &schema(),
            &at("export default { fmt = { quote_style = \"|\" } }\n").unwrap(),
        );
        assert!(
            inside.iter().any(|i| i["label"] == "force-single"),
            "{inside:?}"
        );

        let flag = completions(
            &schema(),
            &at("export default { build = { clean = | } }\n").unwrap(),
        );
        let labels: Vec<&str> = flag.iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels, ["true", "false"]);
    }

    /// `[lint.rules]` names its lints under `properties`, and each takes
    /// a level; an ingot's lint passes through `propertyNames`.
    #[test]
    fn the_lint_rules_complete_their_names_and_levels() {
        let keys = completions(
            &schema(),
            &at("export default { lint = { rules = { | } } }\n").unwrap(),
        );
        let raw = keys
            .iter()
            .find(|i| i["label"] == "raw_require")
            .expect("raw_require");
        assert!(
            raw["documentation"]["value"]
                .as_str()
                .unwrap()
                .contains("Default: `warn`"),
            "{raw}"
        );
        assert_eq!(
            raw["insertText"],
            "raw_require = ${1|'allow','warn','deny'|}"
        );
        assert!(keys.iter().any(|i| i["label"] == "pedantic"), "the groups");
        assert!(keys.iter().any(|i| i["label"] == "alx"), "the markup table");

        let levels = completions(
            &schema(),
            &at("export default { lint = { rules = { raw_require = | } } }\n").unwrap(),
        );
        let labels: Vec<&str> = levels.iter().filter_map(|i| i["label"].as_str()).collect();
        assert_eq!(labels, ["'allow'", "'warn'", "'deny'"]);

        let problems: Vec<String> = check(&schema(), "export default { lint = { rules = { raw_require = \"alow\", my_ingot_lint = \"warn\" } } }\n")
            .into_iter()
            .map(|p| p.message)
            .collect();
        assert_eq!(
            problems,
            [
                "`raw_require` takes one of \"allow\", \"warn\", \"deny\"; `\"alow\"` is none of them"
            ]
        );
    }

    #[test]
    fn a_hover_reads_the_schema() {
        let src = "export default {\n    fmt = { indent_width = 2 },\n}\n";
        let text = hover(&schema(), src, src.find("indent_width").unwrap() + 2).unwrap();
        assert!(
            text.starts_with("```alloy\nfmt.indent_width: number\n```"),
            "{text}"
        );
        assert!(text.contains("Default: `2`"), "{text}");

        let src = "export const build = { [\"in\"] = \"src\" }\n";
        let text = hover(&schema(), src, src.find("\"in\"").unwrap() + 1).unwrap();
        assert!(
            text.starts_with("```alloy\nbuild.in: string\n```"),
            "{text}"
        );
        let src = "export const build = { in = \"src\" }\n";
        let text = hover(&schema(), src, src.find("in =").unwrap() + 1).unwrap();
        assert!(
            text.starts_with("```alloy\nbuild.in: string\n```"),
            "{text}"
        );

        // A lint name no lint has gets no documentation as a level.
        let src = "export default { lint = { rules = { unused_varible = \"deny\" } } }\n";
        assert_eq!(hover(&schema(), src, src.find("unused").unwrap() + 2), None);
    }

    #[test]
    fn the_check_names_what_does_not_fit() {
        let messages = |src: &str| -> Vec<String> {
            check(&schema(), src)
                .into_iter()
                .map(|p| p.message)
                .collect()
        };

        assert_eq!(
            messages("export default { buld = {} }\n"),
            ["`buld` is no key of the config; did you mean `build`?"]
        );
        assert_eq!(
            messages("export const fmt = { indent_width = \"2\", quote_style = \"single\" }\n"),
            [
                "`indent_width` takes a whole number; this is a string",
                "`quote_style` takes one of \"auto-prefer-double\", \"auto-prefer-single\", \"force-double\", \"force-single\", \"preserve\"; `\"single\"` is none of them"
            ]
        );
        assert_eq!(
            messages("export default { build = { clean = \"yes\", exclude = { 1 } } }\n"),
            [
                "`clean` takes a boolean; this is a string",
                "this item takes a string; this is a number"
            ]
        );
        // An expression runs on load; the check leaves it.
        assert!(
            messages("local w = 2\nexport default { fmt = { indent_width = w * 2 } }\n").is_empty()
        );
        // A table with open keys takes any name.
        assert!(messages("export default { mount = { shared = { \"src/shared\", \"@game/ReplicatedStorage/Shared\" } } }\n").is_empty());
        // Ordinary code is no config.
        assert!(messages("local t = { buld = 1 }\nprint(t)\nexport default {}\n").is_empty());
    }
}
