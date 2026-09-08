//! The playground's compiler: the desugar, the completion contexts,
//! the keyword and declaration hovers, and the shape folds, as wasm.
//! The page pairs it with Luau's analyzer, also wasm, which reads the
//! check artifact; the span map here turns its positions back into
//! the source's. One session at a time: `set_source` compiles, and the
//! other calls read what it compiled.

#[allow(dead_code)]
#[path = "../../alloy-lsp/src/context.rs"]
mod context;
#[allow(dead_code)]
#[path = "../../alloy-lsp/src/keywords.rs"]
mod keywords;
#[allow(dead_code)]
#[path = "../../alloy-lsp/src/shapes.rs"]
mod shapes;

use std::cell::RefCell;

use alloy::declarations::{Declaration, Shape};
use alloy::{EmitOptions, Output};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use context::Context;

struct Session {
    source: String,
    output: Option<Output>,
    decls: Vec<Declaration>,
    shapes: Vec<Shape>,
}

thread_local! {
    static SESSION: RefCell<Session> = const {
        RefCell::new(Session {
            source: String::new(),
            output: None,
            decls: Vec::new(),
            shapes: Vec::new(),
        })
    };
}

/// The runtime the check artifact requires as `@alloy`.
#[wasm_bindgen]
pub fn runtime() -> String {
    alloy::RUNTIME.to_string()
}

/// Compiles the source and keeps it. The JSON holds the ship and check
/// artifacts, the compiler's diagnostics, and the lints, each with byte
/// offsets in the source; a source that does not parse gives `error`.
#[wasm_bindgen]
pub fn set_source(source: &str) -> String {
    let options = EmitOptions {
        file_name: "play.aly".to_string(),
        ..EmitOptions::default()
    };
    let compiled = alloy::compile_with(source, &options);
    let decls = alloy::declarations::summaries(source, false);
    let shapes = alloy::declarations::shapes(source);
    let lint_config = alloy::config::LintConfig::default();

    let result = match &compiled {
        Ok(out) => {
            let diagnostics: Vec<Value> = out
                .diagnostics
                .iter()
                .map(|d| {
                    json!({
                        "start": d.start,
                        "end": d.end.max(d.start),
                        "message": alloy::docs::labeled(&d.message),
                        "code": alloy::docs::code_for(&d.message),
                    })
                })
                .collect();
            let lints: Vec<Value> = out
                .lints
                .iter()
                .filter_map(|l| {
                    let level = match alloy::lint::level_of(&lint_config, l.name) {
                        alloy::lint::Level::Allow => return None,
                        alloy::lint::Level::Warn => "warning",
                        alloy::lint::Level::Deny => "error",
                    };

                    Some(json!({
                        "name": l.name,
                        "level": level,
                        "start": l.start,
                        "end": l.end.max(l.start),
                        "message": l.message,
                        "fix": l.fix.as_ref().map(|f| json!({ "start": f.start, "end": f.end, "replacement": f.replacement })),
                    }))
                })
                .collect();

            json!({
                "ship": out.ship,
                "check": out.check,
                "diagnostics": diagnostics,
                "lints": lints,
            })
        }

        Err(e) => json!({
            "error": { "offset": e.offset, "message": alloy::docs::labeled(&e.message) },
        }),
    };

    SESSION.with(|s| {
        let mut s = s.borrow_mut();
        s.source = source.to_string();
        s.output = compiled.ok();
        s.decls = decls;
        s.shapes = shapes;
    });

    result.to_string()
}

/// A byte offset in the check artifact as one in the source.
#[wasm_bindgen]
pub fn to_source(check_offset: u32) -> u32 {
    SESSION.with(|s| {
        s.borrow()
            .output
            .as_ref()
            .map_or(check_offset, |o| o.map.to_source(check_offset))
    })
}

/// A byte offset in the source as one in the check artifact, or -1 for
/// a byte the emit dropped.
#[wasm_bindgen]
pub fn to_check(source_offset: u32) -> i32 {
    SESSION.with(|s| {
        s.borrow()
            .output
            .as_ref()
            .and_then(|o| o.map.to_output(source_offset))
            .map_or(-1, |o| i32::try_from(o).unwrap_or(-1))
    })
}

/// Whether a byte of the check artifact is text the compiler wrote.
#[wasm_bindgen]
pub fn generated_at(check_offset: u32) -> bool {
    SESSION.with(|s| {
        s.borrow()
            .output
            .as_ref()
            .is_some_and(|o| o.map.is_generated(check_offset))
    })
}

/// The completion at a byte offset: the items Alloy answers itself, and
/// whether the analyzer's list belongs beside them. The editor filters
/// by the word at the cursor; `from` is where each item's text starts.
#[wasm_bindgen]
pub fn complete(offset: u32) -> String {
    SESSION.with(|s| {
        let s = s.borrow();
        let source = &s.source;
        let offset = (offset as usize).min(source.len());
        let mut items = Vec::new();
        let word = |label: &str, kind: &str, doc_text: Option<String>, from: usize| {
            json!({ "label": label, "kind": kind, "doc": doc_text, "from": from })
        };

        // A comment: the directives.
        let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);

        if source[line_start..offset].contains("--") {
            let head = &source[line_start..offset];
            let from = head.rfind("--").map_or(line_start, |i| line_start + i);

            for (name, what) in [
                ("--@alloy-ignore", "Silences the next line that holds code, or this line at its end."),
                ("--@alloy-expect-error", "Silences the next line, and is an error when that line has none."),
                ("--@alloy-nocheck", "Silences every diagnostic in this file."),
                ("--!strict", "The checker's strict mode for this file."),
                ("--!nonstrict", "The checker's nonstrict mode for this file."),
                ("--!nocheck", "No type checking for this file."),
            ] {
                items.push(word(name, "directive", Some(what.to_string()), from));
            }

            return json!({ "items": items, "luau": false }).to_string();
        }

        if declares_a_name_at(source, offset) {
            return json!({ "items": items, "luau": false }).to_string();
        }

        let Some(ctx) = context::detect(source, offset) else {
            // The analyzer lists the names; Alloy adds its keywords.
            for k in keywords::ALLOY_KEYWORDS {
                items.push(word(k, "keyword", keywords::doc(k).map(str::to_string), word_start(source, offset)));
            }

            return json!({ "items": items, "luau": true }).to_string();
        };

        match &ctx {
            Context::Attribute { sigil, target, .. } => {
                let fits = |targets: &[&str]| target.is_none_or(|t| targets.contains(&t));

                for key in keywords::keys_with_prefix("@") {
                    if fits(builtin_attribute_targets(key)) {
                        items.push(word(key, "attribute", keywords::doc(key).map(str::to_string), *sigil));
                    }
                }

                for d in &s.decls {
                    if d.name.starts_with('@') {
                        items.push(word(&d.name, "attribute", Some(d.hover.clone()), *sigil));
                    }
                }
            }

            Context::Macro { sigil, .. } => {
                for key in keywords::keys_with_prefix("$") {
                    items.push(word(key, "macro", keywords::doc(key).map(str::to_string), *sigil));
                }

                for d in &s.decls {
                    if d.name.starts_with('$') {
                        items.push(word(&d.name, "macro", Some(d.hover.clone()), *sigil));
                    }
                }
            }

            Context::DeriveArg { prefix } => {
                for key in keywords::keys_with_prefix("derive:") {
                    let name = &key["derive:".len()..];
                    items.push(word(name, "constant", keywords::doc(key).map(str::to_string), offset - prefix.len()));
                }
            }

            Context::CfgArg { prefix } => {
                let from = offset - prefix.len();

                for (name, what) in [
                    ("server", "RunService:IsServer()"),
                    ("client", "RunService:IsClient()"),
                    ("studio", "RunService:IsStudio()"),
                    ("edit", "RunService:IsEdit()"),
                    ("running", "RunService:IsRunning()"),
                    ("test", "an `alloy test` run"),
                ] {
                    items.push(word(name, "constant", Some(format!("`@cfg({name})` holds under {what}.")), from));
                }

                for (name, what) in [
                    ("not", "the condition after it fails"),
                    ("and", "both hold"),
                    ("or", "either holds"),
                    ("any(", "any of the list holds"),
                    ("all(", "all of the list hold"),
                ] {
                    items.push(word(name, "keyword", Some(format!("`{name}`: {what}.")), from));
                }
            }

            Context::RemoteSide { prefix, after } => {
                let from = offset - prefix.len();
                let sides: Vec<(&str, &str)> = match after.as_deref() {
                    None => vec![
                        ("client", "The client fires it; the server handles it."),
                        ("server", "The server fires it; the client handles it."),
                    ],
                    Some("client ") | Some("server ") => vec![("or", "Either side fires it: `client or server`.")],
                    Some("client or") => vec![("server", "Either side fires it, and either side handles it.")],
                    Some("server or") => vec![("client", "Either side fires it, and either side handles it.")],
                    _ => Vec::new(),
                };

                for (side, what) in sides {
                    items.push(word(side, "keyword", Some(what.to_string()), from));
                }
            }

            Context::RemoteFrom { prefix } => {
                items.push(word("from", "keyword", Some("The side that fires the remote: `from client`, `from server`, or `from client or server`.".to_string()), offset - prefix.len()));
            }

            Context::ImportHead { prefix, type_only } => {
                let from = offset - prefix.len();

                if !*type_only {
                    items.push(word("type", "keyword", Some("A type-only import: it costs nothing at runtime.".to_string()), from));
                    items.push(word("* as", "keyword", Some("The whole module under one name.".to_string()), from));
                }

                items.push(word("{", "keyword", Some("Named exports, one or more, `as` to rename.".to_string()), from));
            }

            Context::ImportNames { prefix, after_name, .. } => {
                if *after_name {
                    items.push(word("as", "keyword", Some("Renames the import.".to_string()), offset - prefix.len()));
                }
            }

            Context::ImportStar => {
                items.push(word("as", "keyword", Some("The name the module takes here.".to_string()), offset));
            }

            Context::ImportFrom => {
                items.push(word("from", "keyword", Some("The module path, as a string.".to_string()), offset));
            }

            Context::DeclarationAs { prefix, interface } => {
                let from = offset - prefix.len();
                items.push(word("as", "keyword", Some("Opens the body: the fields of a struct, the variants of an enum.".to_string()), from));

                if *interface {
                    items.push(word("extends", "keyword", Some("The interfaces this one takes its fields from.".to_string()), from));
                }
            }

            Context::EnumPayload { prefix } => {
                let from = offset - prefix.len();

                for name in ["number", "string", "boolean", "any", "unknown", "nil", "thread", "buffer"] {
                    items.push(word(name, "type", None, from));
                }

                for d in &s.decls {
                    if !d.name.starts_with(['@', '$']) && d.hover.contains("```alloy\n") && (d.hover.contains("struct ") || d.hover.contains("enum ") || d.hover.contains("interface ") || d.hover.contains("type ")) {
                        items.push(word(&d.name, "type", Some(d.hover.clone()), from));
                    }
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES.iter().chain(alloy::roblox_classes::DATATYPES) {
                    items.push(word(name, "class", None, from));
                }
            }

            Context::AttributeOn => {
                items.push(word("on", "keyword", Some("What the attribute goes on: `on struct, field`.".to_string()), offset));
            }

            Context::AttributeTarget { prefix } => {
                let from = offset - prefix.len();

                for name in ["function", "struct", "enum", "variant", "field", "param", "remote", "interface", "type", "local"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::TypeSlot { prefix } => {
                let from = offset - prefix.len();

                for name in ["number", "string", "boolean", "any", "unknown", "nil", "thread", "buffer"] {
                    items.push(word(name, "type", None, from));
                }

                for d in &s.decls {
                    let head = d.hover.lines().nth(1).unwrap_or("");

                    if !d.name.starts_with(['@', '$']) && !d.name.contains('.') && (head.contains("struct ") || head.contains("enum ") || head.contains("interface ") || head.contains("trait ") || head.contains("type ")) {
                        items.push(word(&d.name, "type", Some(d.hover.clone()), from));
                    }
                }

                for name in ["Array", "HashMap", "Set", "Queue", "Heap", "Scope", "Iter", "Result", "Future", "Signal", "Partial", "Readonly", "Sink"] {
                    items.push(word(name, "type", keywords::doc(name).map(str::to_string), from));
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES.iter().chain(alloy::roblox_classes::DATATYPES) {
                    items.push(word(name, "class", None, from));
                }
            }

            Context::NewTarget { prefix } => {
                let from = offset - prefix.len();

                for d in &s.decls {
                    let head = d.hover.lines().nth(1).unwrap_or("");

                    if head.contains("struct ") {
                        items.push(word(&d.name, "class", Some(d.hover.clone()), from));
                    }
                }

                for name in ["HashMap", "Set", "Queue", "Heap", "Scope", "Signal", "Symbol", "Array"] {
                    items.push(word(name, "class", keywords::doc(name).map(str::to_string), from));
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES.iter().chain(alloy::roblox_classes::DATATYPES) {
                    items.push(word(name, "class", None, from));
                }
            }

            Context::MatchCase { prefix } => {
                let from = offset - prefix.len();

                for d in &s.decls {
                    if let Some((_, variant)) = d.name.split_once('.') {
                        items.push(word(variant, "constant", Some(d.hover.clone()), from));
                    }
                }

                for name in ["Ok", "Err", "default", "_"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::FieldStart { prefix } => {
                let from = offset - prefix.len();

                for name in ["read", "write", "private", "public", "end"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::MemberStart { prefix } => {
                let from = offset - prefix.len();

                for name in ["function", "async function", "private function", "public", "end"] {
                    items.push(word(name, "keyword", None, from));
                }
            }

            Context::ImportSpec { .. } | Context::Nothing => {}
        }

        json!({ "items": items, "luau": false }).to_string()
    })
}

/// The hover Alloy answers itself at a byte offset: a keyword, an
/// operator, or a declaration of the file. `null` when the analyzer's
/// type is the answer.
#[wasm_bindgen]
pub fn hover(offset: u32) -> String {
    SESSION.with(|s| {
        let s = s.borrow();
        let source = &s.source;
        let offset = (offset as usize).min(source.len());

        if let Some((start, end, text)) = keywords::hover(source, offset) {
            return json!({ "from": start, "to": end, "markdown": text }).to_string();
        }

        if keywords::is_word_at(source, offset) {
            let (start, end) = keywords::word_range(source, offset);
            let name = &source[start..end];

            if let Some(d) = s
                .decls
                .iter()
                .find(|d| d.name == name || d.name.trim_start_matches(['@', '$']) == name)
            {
                return json!({ "from": start, "to": end, "markdown": d.hover }).to_string();
            }

            if let Some(text) = keywords::doc(name)
                && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            {
                return json!({ "from": start, "to": end, "markdown": text }).to_string();
            }
        }

        "null".to_string()
    })
}

/// A type the analyzer printed, as the source reads it: the runtime's
/// prefix goes, and a struct or a collection reads by name.
#[wasm_bindgen]
pub fn fold(text: &str) -> String {
    SESSION.with(|s| {
        let s = s.borrow();
        let known = shapes::Known {
            shapes: s.shapes.clone(),
        };
        let mut out = text.to_string();

        for primitive in alloy::desugar::PRIMITIVES {
            out = out.replace(&format!("__alloy_{primitive}."), &format!("{primitive}."));
        }

        let out = out
            .replace("__alloy.", "")
            .replace("__mapped_optional<", "Partial<")
            .replace("__mapped_read<", "Readonly<")
            .replace("__mapped_write<", "Sink<")
            .replace("Future<nil>", "Future<()>");

        shapes::fold(&out, &known)
    })
}

/// The documentation of a std name or a keyword, for a hover the
/// analyzer answered with a type.
#[wasm_bindgen]
pub fn doc_of(name: &str) -> String {
    keywords::doc(name).unwrap_or("").to_string()
}

fn word_start(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut start = offset;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    start
}

/// Whether `offset` sits in the name a declaring keyword introduces.
fn declares_a_name_at(source: &str, offset: usize) -> bool {
    const DECLARERS: &[&str] = &[
        "enum",
        "struct",
        "trait",
        "interface",
        "type",
        "function",
        "local",
        "const",
        "macro",
        "attribute",
        "remote",
        "impl",
        "class",
        "import",
    ];
    let bytes = source.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let start = word_start(source, offset);

    if offset < bytes.len() && is_word(bytes[offset]) && start == offset {
        return false;
    }

    let mut end = start;

    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }

    if end == start {
        return false;
    }

    let mut ws = end;

    while ws > 0 && is_word(bytes[ws - 1]) {
        ws -= 1;
    }

    DECLARERS.contains(&&source[ws..end])
}

fn builtin_attribute_targets(key: &str) -> &'static [&'static str] {
    match key {
        "@derive" | "@sealed" => &["struct", "enum"],
        "@cfg" => &["function", "local"],
        "@test" | "@native" | "@checked" | "@deprecated" | "@inline" | "@noinline" => &["function"],
        "@unreliable" | "@ratelimit" | "@timeout" | "@validate" => &["remote"],
        "@u8" | "@u16" | "@u32" | "@i8" | "@i16" | "@i32" | "@f32" => &["param", "field"],
        "@rename" | "@skip" => &["field"],
        _ => &[
            "function",
            "struct",
            "enum",
            "variant",
            "field",
            "param",
            "remote",
            "interface",
            "type",
        ],
    }
}
