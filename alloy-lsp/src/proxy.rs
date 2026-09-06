//! The proxy: shadow documents for the child, position and URI mapping
//! for every message that crosses, and the features the child cannot
//! give an Alloy file: its settings, auto-imports, rename follow-up, and
//! markup intellisense.
//!
//! An Alloy buffer never reaches the child as itself. The server keeps
//! the source, compiles the check artifact, and gives the child that text
//! as a shadow `.luau` document. The child resolves a `require` only to a
//! file on disk, so the shadows live in a mirror of the workspace under
//! the temp directory, beside a copy of every plain Luau file; the child's
//! root is the mirror. Every URI and position in a message crossing
//! either way is mapped, so the editor only ever sees its own files.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alloy::EmitOptions;
use alloy::config::Config;
use serde_json::{Map, Value, json};

use crate::doc::{Doc, offset_of, position_of};
use crate::imports::{self, Rename};
use crate::{block_end, context, keywords, log, markup, settings, tokens};

pub struct Server {
    state: Mutex<State>,
    child_in: Mutex<Box<dyn Write + Send>>,
    client_out: Mutex<Box<dyn Write + Send>>,
}

/// A request the editor sent, waiting for the child's answer.
struct Pending {
    method: String,
    /// The source URI the request was about, if an Alloy file.
    ctx: Option<String>,
    /// The source position of the request, for completion.
    position: Option<(u32, u32)>,
    /// The character that triggered a completion, when one did.
    trigger: Option<String>,
    /// The source range of the request, for code actions.
    range: Option<((u32, u32), (u32, u32))>,
}

/// A question the server asked the editor.
enum Asked {
    /// Apply this workspace edit if the answer is the update action.
    Rename(Value),
}

#[derive(Default)]
struct State {
    /// Source URI -> document.
    docs: HashMap<String, Doc>,
    /// Shadow URI -> source URI.
    shadows: HashMap<String, String>,
    /// Source URIs the editor holds open; the rest mirror the disk.
    editor_open: HashSet<String>,
    pending: HashMap<String, Pending>,
    /// The id of the `initialize` request, whose result is edited.
    initialize_id: Option<String>,
    /// Latest child diagnostics per source URI, already mapped.
    child_diagnostics: HashMap<String, Vec<Value>>,
    root: Option<PathBuf>,
    /// Extensions declared anywhere under the root, read at startup.
    extensions: Vec<alloy::extensions::Extension>,
    /// The mirror workspace the child works in.
    mirror: PathBuf,
    /// Plain Luau documents the editor holds open, by real URI: their
    /// text keeps the mirror copy current.
    plain: HashMap<String, String>,
    /// The mirror URI of the runtime module; its diagnostics stay inside.
    runtime_uri: Option<String>,
    /// The places the runtime was written to, one per project input.
    runtimes: std::cell::RefCell<std::collections::HashSet<PathBuf>>,
    /// The child's settings, answered on `workspace/configuration`.
    settings: Value,
    /// Questions in flight, by request id.
    asked: HashMap<String, Asked>,
    next_id: u64,
}

impl State {
    /// The `[lint]` table of the workspace's alloy.toml, or the defaults.
    fn lint_config(&self) -> alloy::config::LintConfig {
        self.root
            .as_deref()
            .and_then(alloy::config::Config::find)
            .and_then(|p| alloy::config::Config::load(&p).ok())
            .map(|c| c.lint)
            .unwrap_or_default()
    }

    /// The compiler's diagnostics of one document as LSP diagnostics.
    fn alloy_diagnostics(&self, uri: &str) -> Vec<Value> {
        let mut diagnostics = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return diagnostics;
        };

        if let Some(out) = &doc.output {
            for d in &out.diagnostics {
                let (sl, sc) = position_of(&doc.source, d.start as usize);
                let (el, ec) = position_of(&doc.source, d.end.max(d.start) as usize);
                let (el, ec) = if (el, ec) == (sl, sc) {
                    (sl, sc + 1)
                } else {
                    (el, ec)
                };
                // `Alloy(4.2)`: the book section as the code, and the
                // number links to the section.
                let mut item = json!({
                    "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                    "severity": 1,
                    "source": "Alloy",
                    "message": d.message,
                });

                if let Some(code) = alloy::docs::code_for(&d.message)
                    && let Some(url) = alloy::docs::book_url(code)
                {
                    item["code"] = json!(code);
                    item["codeDescription"] = json!({ "href": url });
                }

                diagnostics.push(item);
            }

            // Lints at their `[lint]` level: a warning, or an error for
            // a denied one. The table comes from the project's alloy.toml.
            let lint_config = self.lint_config();

            for l in &out.lints {
                let level = alloy::lint::level_of(&lint_config, l.name);

                if level == alloy::lint::Level::Allow {
                    continue;
                }

                let (sl, sc) = position_of(&doc.source, l.start as usize);
                let (el, ec) = position_of(&doc.source, l.end.max(l.start) as usize);
                let (el, ec) = if (el, ec) == (sl, sc) {
                    (sl, sc + 1)
                } else {
                    (el, ec)
                };
                let severity = if level == alloy::lint::Level::Deny {
                    1
                } else {
                    2
                };
                diagnostics.push(json!({
                    "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                    "severity": severity,
                    "source": "Alloy",
                    "code": alloy::docs::LINT_CODE,
                    "codeDescription": { "href": alloy::docs::book_url(alloy::docs::LINT_CODE).unwrap_or_default() },
                    "message": if l.fix.is_some() {
                        format!("{}: {}\n`alloy flux --fix` rewrites it.", l.name, l.message)
                    } else {
                        format!("{}: {}", l.name, l.message)
                    },
                }));
            }
        }

        diagnostics
    }

    /// The code actions of the lints: a quick fix per rewrite whose lint
    /// touches the range, each tied to its diagnostic so the editor's
    /// light bulb finds it, and `source.fixAll` for the whole file.
    fn lint_actions(&self, uri: &str, range: ((u32, u32), (u32, u32))) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let Some(out) = &doc.output else {
            return actions;
        };
        let lint_config = self.lint_config();
        let ((from_line, _), (to_line, _)) = range;
        let mut all_edits: Vec<Value> = Vec::new();

        for l in &out.lints {
            let Some(fix) = &l.fix else { continue };

            if alloy::lint::level_of(&lint_config, l.name) == alloy::lint::Level::Allow {
                continue;
            }

            let (sl, sc) = position_of(&doc.source, fix.start as usize);
            let (el, ec) = position_of(&doc.source, fix.end as usize);
            let edit = json!({
                "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                "newText": fix.replacement,
            });
            all_edits.push(edit.clone());

            let (ll, lc) = position_of(&doc.source, l.start as usize);
            let (le, lec) = position_of(&doc.source, l.end.max(l.start) as usize);

            if le < from_line || ll > to_line {
                continue;
            }

            let one_line: String = fix
                .replacement
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let shown = if one_line.chars().count() > 40 {
                format!("{}…", one_line.chars().take(40).collect::<String>())
            } else {
                one_line
            };
            actions.push(json!({
                "title": format!("Rewrite as `{shown}` ({})", l.name),
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": { "start": { "line": ll, "character": lc }, "end": { "line": le, "character": lec } },
                    "severity": 2,
                    "source": "Alloy",
                    "code": alloy::docs::LINT_CODE,
                    "message": format!("{}: {}\n`alloy flux --fix` rewrites it.", l.name, l.message),
                }],
                "edit": { "changes": { uri: [edit] } },
            }));
        }

        if all_edits.len() > 1 {
            // Two rewrites that overlap keep the first, as `--fix` does.
            let mut kept: Vec<Value> = Vec::new();
            let mut last_end: Option<(u64, u64)> = None;

            for e in &all_edits {
                let start = (
                    e["range"]["start"]["line"].as_u64().unwrap_or(0),
                    e["range"]["start"]["character"].as_u64().unwrap_or(0),
                );
                let end = (
                    e["range"]["end"]["line"].as_u64().unwrap_or(0),
                    e["range"]["end"]["character"].as_u64().unwrap_or(0),
                );

                if last_end.is_none_or(|l| l <= start) {
                    kept.push(e.clone());
                    last_end = Some(end);
                }
            }

            actions.push(json!({
                "title": format!("Apply every Alloy rewrite in this file ({})", kept.len()),
                "kind": "source.fixAll",
                "edit": { "changes": { uri: kept } },
            }));
        }

        actions
    }

    /// After `Msg.`, the child lists a variant as the function or the
    /// string the emit made of it. Each one becomes an enum member with
    /// the variant's signature as its detail.
    fn mark_enum_members(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return;
        };
        let head = doc.source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
        let Some(before_dot) = head.strip_suffix('.') else {
            return;
        };
        let enum_name: String = before_dot
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        if enum_name.is_empty() {
            return;
        }

        let is_enum = doc.decls.iter().any(|d| {
            d.name == enum_name && d.hover.lines().nth(1).is_some_and(|l| l.contains("enum "))
        });

        if !is_enum {
            return;
        }

        let items = match result {
            Value::Array(items) => items,

            Value::Object(obj) => match obj.get_mut("items").and_then(Value::as_array_mut) {
                Some(items) => items,

                None => return,
            },

            _ => return,
        };

        for item in items.iter_mut() {
            let Some(label) = item["label"].as_str().map(str::to_string) else {
                continue;
            };
            let full = format!("{enum_name}.{label}");

            if let Some(d) = doc.decls.iter().find(|d| d.name == full) {
                let signature = d
                    .hover
                    .lines()
                    .find(|l| l.starts_with(&full))
                    .unwrap_or(&full)
                    .to_string();
                item["kind"] = json!(20);
                item["detail"] = json!(signature.clone());
                item["documentation"] = json!({ "kind": "markdown", "value": d.hover });

                // The child inserts `Move(_1, _2)`; the payload types read
                // better as the placeholders.
                let payload = payload_types(&signature);
                let insert = if payload.is_empty() {
                    label.to_string()
                } else {
                    let slots: Vec<String> = payload
                        .iter()
                        .enumerate()
                        .map(|(i, t)| format!("${{{}:{t}}}", i + 1))
                        .collect();

                    format!("{label}({})", slots.join(", "))
                };

                if item.get("textEdit").is_some() {
                    item["textEdit"]["newText"] = json!(insert);
                } else {
                    item["insertText"] = json!(insert);
                }

                item["insertTextFormat"] = json!(if payload.is_empty() { 1 } else { 2 });
                item.as_object_mut().map(|o| o.remove("command"));
            }
        }
    }

    /// Signature help on a variant constructor: the child shows the
    /// emit's `_1: Player`; the variant's own shape, `Msg.Move(Player,
    /// number)`, replaces it, one parameter per payload type.
    fn rewrite_variant_signatures(&self, uri: &str, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(signatures) = result.get_mut("signatures").and_then(Value::as_array_mut) else {
            return;
        };

        for sig in signatures.iter_mut() {
            let Some(label) = sig["label"].as_str() else {
                continue;
            };
            let Some(d) = doc
                .decls
                .iter()
                .filter(|d| d.name.contains('.'))
                .find(|d| label.contains(&format!("{}(", d.name)))
            else {
                continue;
            };
            let Some(shape) = d.hover.lines().find(|l| l.starts_with(&d.name)) else {
                continue;
            };
            let params: Vec<Value> = payload_types(shape)
                .into_iter()
                .map(|t| json!({ "label": t }))
                .collect();
            sig["label"] = json!(shape);
            sig["parameters"] = json!(params);
            sig["documentation"] = json!({ "kind": "markdown", "value": d.hover });
        }
    }

    /// Completion items for the extensions on a primitive. The child does
    /// not know them, so the proxy adds them when the receiver is a
    /// string: after `:` when the child listed the string methods, and
    /// after `string.` for the statics.
    fn primitive_completions(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &Value,
    ) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };

        let before = &doc.source[..offset];
        let before = before.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
        let labels: Vec<&str> = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array())
            .map(|items| items.iter().filter_map(|i| i["label"].as_str()).collect())
            .unwrap_or_default();

        let is_method =
            before.ends_with(':') && labels.contains(&"upper") && labels.contains(&"sub");
        let is_static = before.ends_with("string.");

        if !is_method && !is_static {
            return Vec::new();
        }

        self.extensions
            .iter()
            .filter(|e| e.target == "string" && e.is_static == is_static)
            .filter(|e| !labels.contains(&e.name.as_str()))
            .map(|e| {
                let ret = e.ret.as_deref().unwrap_or("()");

                json!({
                    "label": e.name,
                    "kind": if is_static { 3 } else { 2 },
                    "detail": format!("({}) -> {ret}", e.params),
                    "documentation": format!("Alloy extension on {}", e.target),
                })
            })
            .collect()
    }

    /// Completion items for the ambient std names, `HashMap` and the
    /// rest. The child knows them only as `__alloy.Name`, so a name typed
    /// at the start of an expression never reaches its list.
    fn std_completions(&self, uri: &str, line: u32, character: u32, result: &Value) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };

        let before =
            doc.source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

        // After a sigil the intrinsics and attributes decide; after a
        // `.` or a method `:` the receiver does. After a type `:`, the
        // colon with a space, or a `->`, the types of the workspace and
        // the std join the child's, which lists classes alone.
        let raw = &doc.source[..offset];
        let raw_head = raw.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
        let type_slot = (raw_head.ends_with(": ")
            || raw_head.ends_with("-> ")
            || raw_head.ends_with(": read ")
            || raw_head.ends_with(": write "))
            && !raw_head.trim_end().ends_with("::");

        let labels: Vec<&str> = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array())
            .map(|items| items.iter().filter_map(|i| i["label"].as_str()).collect())
            .unwrap_or_default();

        if type_slot {
            return self.type_completions(&labels);
        }

        if before.ends_with(['.', ':', '$', '@']) {
            return Vec::new();
        }

        let mut items: Vec<Value> = alloy::desugar::AMBIENT
            .iter()
            .filter(|name| !labels.contains(name))
            .map(|name| {
                let kind = if matches!(*name, "Ok" | "Err") { 3 } else { 7 };

                json!({
                    "label": name,
                    "kind": kind,
                    "detail": "alloy:std",
                    "documentation": crate::keywords::doc(name).map(|d| json!({ "kind": "markdown", "value": d })),
                })
            })
            .collect();

        // The Alloy keywords: the child lists Luau's own.
        items.extend(
            keywords::ALLOY_KEYWORDS
                .iter()
                .filter(|k| !labels.contains(k))
                .map(|k| {
                    json!({
                        "label": k,
                        "kind": 14,
                        "detail": "Alloy keyword",
                        "documentation": keywords::doc(k).map(|d| json!({ "kind": "markdown", "value": d })),
                    })
                }),
        );

        items
    }

    /// The type names for an annotation: every struct, interface, enum,
    /// trait, and type alias of the workspace, the std types, and the
    /// primitives.
    fn type_completions(&self, labels: &[&str]) -> Vec<Value> {
        let mut items = Vec::new();
        let mut seen: HashSet<String> = labels.iter().map(|l| l.to_string()).collect();
        let mut push = |name: &str, kind: u64, detail: &str, doc_text: Option<String>| {
            if seen.insert(name.to_string()) {
                let mut item = json!({ "label": name, "kind": kind, "detail": detail });

                if let Some(d) = doc_text {
                    item["documentation"] = json!({ "kind": "markdown", "value": d });
                }

                items.push(item);
            }
        };

        for d in self.docs.values().flat_map(|d| d.decls.iter()) {
            if d.name.starts_with(['$', '@']) || d.name.contains('.') {
                continue;
            }

            let head = d.hover.lines().nth(1).unwrap_or("");
            let kind = if head.contains("struct ") || head.contains("class ") {
                Some(("struct", 7))
            } else if head.contains("interface ") {
                Some(("interface", 8))
            } else if head.contains("enum ") {
                Some(("enum", 13))
            } else if head.contains("trait ") {
                Some(("trait", 8))
            } else if head.contains("type ") {
                Some(("type", 7))
            } else {
                None
            };

            if let Some((detail, kind)) = kind {
                push(&d.name, kind, detail, Some(d.hover.clone()));
            }
        }

        for name in [
            "Future",
            "Result",
            "Array",
            "HashMap",
            "Set",
            "Signal",
            "SignalConnection",
            "Signalish",
            "Partial",
            "Readonly",
            "Sink",
            "Queue",
            "Heap",
            "Scope",
            "Iter",
        ] {
            push(
                name,
                7,
                "alloy:std",
                keywords::doc(name).map(str::to_string),
            );
        }

        for name in [
            "string", "number", "boolean", "nil", "any", "unknown", "never", "thread", "buffer",
            "table", "vector",
        ] {
            push(name, 14, "primitive", None);
        }

        items
    }

    /// The items for a completion context. A sigil item replaces the
    /// sigil too, since the editor's word never includes it.
    fn context_items(&self, uri: &str, offset: usize, ctx: &context::Context) -> Vec<Value> {
        use crate::context::Context;

        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let cursor = position_of(&doc.source, offset);
        let word = |label: &str, kind: u64, doc_text: Option<String>, from: usize| {
            let start = position_of(&doc.source, from);
            let mut item = json!({
                "label": label,
                "kind": kind,
                "textEdit": {
                    "range": range_value(start, cursor),
                    "newText": label,
                },
            });

            if let Some(d) = doc_text {
                item["documentation"] = json!({ "kind": "markdown", "value": d });
            }

            item
        };
        let mut items = Vec::new();

        match ctx {
            Context::Attribute { sigil, target, .. } => {
                // Only the attributes that go on what the position names;
                // every one when the position says nothing.
                let fits = |targets: &[&str]| target.is_none_or(|t| targets.contains(&t));

                for key in keywords::keys_with_prefix("@") {
                    if fits(builtin_attribute_targets(key)) {
                        items.push(word(
                            key,
                            14,
                            keywords::doc(key).map(str::to_string),
                            *sigil,
                        ));
                    }
                }

                let mut seen = HashSet::new();

                for d in self.docs.values().flat_map(|d| d.decls.iter()) {
                    if d.name.starts_with('@')
                        && fits(&declared_attribute_targets(&d.hover))
                        && seen.insert(d.name.clone())
                    {
                        items.push(word(&d.name, 7, Some(d.hover.clone()), *sigil));
                    }
                }
            }

            Context::Macro { sigil, .. } => {
                for key in keywords::keys_with_prefix("$") {
                    items.push(word(
                        key,
                        14,
                        keywords::doc(key).map(str::to_string),
                        *sigil,
                    ));
                }

                let mut seen = HashSet::new();

                for d in self.docs.values().flat_map(|d| d.decls.iter()) {
                    if d.name.starts_with('$') && seen.insert(d.name.clone()) {
                        items.push(word(&d.name, 3, Some(d.hover.clone()), *sigil));
                    }
                }
            }

            Context::DeriveArg { prefix } => {
                for key in keywords::keys_with_prefix("derive:") {
                    let name = &key["derive:".len()..];
                    items.push(word(
                        name,
                        21,
                        keywords::doc(key).map(str::to_string),
                        offset - prefix.len(),
                    ));
                }
            }

            Context::RemoteSide { prefix, after } => {
                let from = offset - prefix.len();
                let sides: Vec<(&str, &str)> = match after.as_deref() {
                    None => vec![
                        ("client", "The client fires it; the server handles it."),
                        ("server", "The server fires it; the client handles it."),
                    ],

                    Some("client ") | Some("server ") => vec![(
                        "or",
                        "Either side fires it, and either side handles it: `client or server`.",
                    )],

                    Some("client or") => vec![(
                        "server",
                        "Either side fires it, and either side handles it.",
                    )],

                    Some("server or") => vec![(
                        "client",
                        "Either side fires it, and either side handles it.",
                    )],

                    _ => Vec::new(),
                };

                for (side, doc_text) in sides {
                    items.push(word(side, 14, Some(doc_text.to_string()), from));
                }
            }

            Context::ImportHead { prefix, type_only } => {
                let from = offset - prefix.len();

                if !*type_only {
                    items.push(word(
                        "type",
                        14,
                        Some("A type-only import: it costs nothing at runtime.".to_string()),
                        from,
                    ));
                    items.push(word(
                        "* as",
                        14,
                        Some("The whole module under one name.".to_string()),
                        from,
                    ));
                }

                items.push(word(
                    "{",
                    14,
                    Some("Named exports, one or more, `as` to rename.".to_string()),
                    from,
                ));
            }

            Context::ImportNames {
                prefix,
                type_only,
                spec,
                after_name,
            } => {
                let from = offset - prefix.len();

                if *after_name {
                    items.push(word(
                        "as",
                        14,
                        Some("Renames the import.".to_string()),
                        from,
                    ));

                    return items;
                }

                if !*type_only {
                    items.push(word(
                        "type",
                        14,
                        Some("A type-only name in a value import.".to_string()),
                        from,
                    ));
                }

                if let Some(spec) = spec
                    && let Some(path) = uri_to_path(uri)
                    && let Some(dir) = path.parent()
                {
                    // `@alias/x` goes through the nearest .luaurc; a
                    // relative spec is path arithmetic.
                    let resolved = match spec.strip_prefix('@') {
                        Some(rest) => {
                            let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));

                            luaurc_aliases(dir, self.root.as_deref())
                                .into_iter()
                                .find(|(a, _)| a == alias)
                                .map(|(_, base)| imports::lexical(&base, tail))
                        }

                        None => Some(imports::lexical(dir, spec)),
                    };
                    let mut exports: Vec<imports::Export> = Vec::new();

                    if let Some(resolved) = resolved {
                        let target = imports::module_path(&resolved);

                        // An open document first; else the file on disk,
                        // which a plain Luau module in a package is.
                        for (u, d) in &self.docs {
                            let Some(p) = uri_to_path(u) else { continue };

                            if imports::module_path(&p) == target {
                                exports.extend(d.exports.iter().cloned());
                            }
                        }

                        if exports.is_empty()
                            && let Some(file) = imports::module_file(&target)
                        {
                            exports = imports::exports_of_file(&file, 0);
                        }
                    }

                    for e in &exports {
                        if *type_only && !e.is_type {
                            continue;
                        }

                        let label = if e.is_type && !*type_only {
                            format!("type {}", e.name)
                        } else {
                            e.name.clone()
                        };
                        items.push(word(&label, e.kind, None, from));
                    }
                }
            }

            // A finished statement wants no list: after the closing quote
            // of an import path, Enter is a newline.
            Context::Nothing => {}

            Context::ImportStar => {
                items.push(word(
                    "as",
                    14,
                    Some("The name the module takes here.".to_string()),
                    offset,
                ));
            }

            Context::ImportFrom => {
                items.push(word(
                    "from",
                    14,
                    Some(
                        "The module path, as a string: `\"./m\"` or `\"@packages/m\"`.".to_string(),
                    ),
                    offset,
                ));
            }

            Context::DeclarationAs { prefix, interface } => {
                items.push(word(
                    "as",
                    14,
                    Some(
                        "Opens the body: the fields of a struct, the variants of an enum."
                            .to_string(),
                    ),
                    offset - prefix.len(),
                ));

                if *interface {
                    items.push(word(
                        "extends",
                        14,
                        Some("The interfaces this one takes its fields from: `interface Entity extends Named as`.".to_string()),
                        offset - prefix.len(),
                    ));
                }
            }

            Context::EnumPayload { prefix } => {
                // The primitives, the types of the workspace and the std,
                // then the Roblox classes and datatypes.
                for name in [
                    "number", "string", "boolean", "any", "unknown", "nil", "thread", "buffer",
                ] {
                    items.push(word(name, 14, None, offset - prefix.len()));
                }

                items.extend(self.type_completions(&[]));

                for name in alloy::roblox_classes::INSTANCE_CLASSES
                    .iter()
                    .chain(alloy::roblox_classes::DATATYPES)
                {
                    let mut item = word(name, 7, None, offset - prefix.len());
                    item["detail"] = json!("roblox");
                    items.push(item);
                }
            }

            Context::RemoteFrom { prefix } => {
                items.push(word(
                    "from",
                    14,
                    Some("The side that fires the remote: `from client`, `from server`, or `from client or server`.".to_string()),
                    offset - prefix.len(),
                ));
            }

            Context::AttributeOn => {
                items.push(word(
                    "on",
                    14,
                    Some("The targets the attribute goes on.".to_string()),
                    offset,
                ));
            }

            Context::AttributeTarget { prefix } => {
                let from = offset - prefix.len();

                for (target, doc_text) in [
                    ("function", "A function or method."),
                    ("struct", "A struct declaration."),
                    ("enum", "An enum declaration."),
                    ("variant", "One variant of an enum."),
                    ("field", "A field of a struct."),
                    ("param", "A parameter, on a function or a remote."),
                    ("remote", "A remote declaration."),
                    ("interface", "An interface declaration."),
                    ("type", "A type alias."),
                ] {
                    items.push(word(target, 21, Some(doc_text.to_string()), from));
                }
            }

            Context::ImportSpec { text, start } => {
                let Some(path) = uri_to_path(uri) else {
                    return items;
                };
                let Some(dir) = path.parent() else {
                    return items;
                };
                // The segment being typed replaces from the last `/`.
                let cut = text.rfind('/').map(|i| i + 1).unwrap_or(0);
                let head = &text[..cut];

                let sourcemap = self
                    .settings
                    .pointer("/sourcemap/sourcemapFile")
                    .and_then(Value::as_str)
                    .unwrap_or("sourcemap.json");

                for (label, kind, detail) in
                    module_entries(dir, self.root.as_deref(), head, sourcemap)
                {
                    let mut item = word(&label, kind, None, start + cut);
                    item["detail"] = json!(detail);
                    items.push(item);
                }
            }
        }

        items
    }

    /// The emit options and luaux config for a file, from the nearest
    /// `alloy.toml` and `luaux.toml`.
    fn options_for(&self, uri: &str) -> (EmitOptions, alloy::luaux::Config) {
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let config = Config::find(&dir).and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let file_name = path.to_string_lossy().into_owned();
        let definitions = file_name.ends_with(".d.aly");
        let config_dir = config
            .as_ref()
            .and_then(|(p, _)| p.parent().map(Path::to_path_buf))
            .or_else(|| self.root.clone())
            .unwrap_or_else(|| dir.clone());
        let jsx = alloy::luaux::Config::load(&config_dir).unwrap_or_default();

        let options = match config {
            Some((config_path, config)) => {
                let root = normalize(config_path.parent().unwrap_or(Path::new(".")));
                let input = normalize(&root.join(&config.build.input));
                let file = normalize(&path);
                // The runtime sits at the input root of the file's own
                // project, as the build puts it at the output root; a file
                // outside that input gets it beside itself.
                let (depth, runtime_dir) = match file.strip_prefix(&input) {
                    Ok(rel) => (rel.components().count().saturating_sub(1), input.clone()),

                    Err(_) => (0, normalize(&dir)),
                };
                self.ensure_runtime(&runtime_dir);
                let std_require = config.emit.std_require.clone().unwrap_or_else(|| {
                    if depth == 0 {
                        "./alloy".to_string()
                    } else {
                        format!("{}alloy", "../".repeat(depth))
                    }
                });

                EmitOptions {
                    wait_timeout: config.emit.wait_timeout,
                    file_name,
                    std_require,
                    definitions,
                    erase_type_imports: config.emit.erase_type_imports,
                    extensions: self.extensions.clone(),
                    ..EmitOptions::default()
                }
            }

            None => {
                self.ensure_runtime(&normalize(&dir));

                EmitOptions {
                    file_name,
                    std_require: "./alloy".to_string(),
                    definitions,
                    extensions: self.extensions.clone(),
                    ..EmitOptions::default()
                }
            }
        };

        (options, jsx)
    }

    fn fresh_id(&mut self) -> String {
        self.next_id += 1;

        format!("alloy:{}", self.next_id)
    }

    /// The mirror path of a real path: the same place under the mirror,
    /// with an Alloy extension swapped to Luau. A path outside the root
    /// goes under `_outside`.
    fn mirror_path(&self, real: &Path) -> PathBuf {
        let real = normalize(real);
        let root = self.root.as_deref().map(normalize);
        let rel = match root.as_deref().and_then(|r| real.strip_prefix(r).ok()) {
            Some(rel) => rel.to_path_buf(),

            None => {
                let mut p = PathBuf::from("_outside");

                for c in real.components() {
                    if let std::path::Component::Normal(n) = c {
                        p.push(n);
                    }
                }

                p
            }
        };
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let swapped = if let Some(b) = name.strip_suffix(".d.aly") {
            format!("{b}.d.luau")
        } else if let Some(b) = name
            .strip_suffix(".aly")
            .or_else(|| name.strip_suffix(".alx"))
        {
            format!("{b}.luau")
        } else {
            name
        };

        self.mirror.join(rel.with_file_name(swapped))
    }

    /// The real path of a mirror path, for a plain file. An Alloy file
    /// resolves through `shadows` first, since its extension changed.
    fn real_path(&self, mirror: &Path) -> Option<PathBuf> {
        let rel = mirror.strip_prefix(&self.mirror).ok()?;

        if let Ok(outside) = rel.strip_prefix("_outside") {
            return Some(Path::new("/").join(outside));
        }

        Some(self.root.as_deref()?.join(rel))
    }

    /// The URI the child sees for a real URI.
    fn child_uri(&self, real: &str) -> String {
        match uri_to_path(real) {
            Some(path) => path_to_uri(&self.mirror_path(&path)),

            None => real.to_string(),
        }
    }

    /// The URI the editor sees for a child URI, and whether it names an
    /// Alloy document.
    fn editor_uri(&self, child: &str) -> (String, bool) {
        if let Some(source) = self.shadows.get(child) {
            return (source.clone(), true);
        }

        let real = uri_to_path(child)
            .and_then(|p| self.real_path(&p))
            .map(|p| path_to_uri(&p))
            .unwrap_or_else(|| child.to_string());

        (real, false)
    }

    /// A real path as a message shows it: relative to the root when it
    /// is under it, else as it is.
    fn friendly_path(&self, path: &Path) -> String {
        let path = normalize(path);
        let shown = match self.root.as_deref().map(normalize) {
            Some(root) => path
                .strip_prefix(&root)
                .map(Path::to_path_buf)
                .unwrap_or(path),

            None => path,
        };

        shown.to_string_lossy().replace('\\', "/")
    }

    /// Writes the runtime into the mirror under `dir` once, so the
    /// `require` of a file there resolves for the child.
    fn ensure_runtime(&self, dir: &Path) {
        let real = dir.join("alloy.luau");

        if self.runtimes.borrow_mut().insert(real.clone()) {
            self.write_mirror(&real, alloy::RUNTIME);
        }
    }

    /// Writes a mirror file, creating its directories.
    fn write_mirror(&self, real: &Path, text: &str) {
        let target = self.mirror_path(real);

        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if std::fs::read_to_string(&target).ok().as_deref() != Some(text) {
            let _ = std::fs::write(&target, text);
        }
    }

    fn remove_mirror(&self, real: &Path) {
        let _ = std::fs::remove_file(self.mirror_path(real));
    }
}

impl Server {
    pub fn new(
        child_in: Box<dyn Write + Send>,
        client_out: Box<dyn Write + Send>,
        extensions: Vec<alloy::extensions::Extension>,
    ) -> Self {
        let state = State {
            settings: settings::defaults(),
            extensions,
            ..State::default()
        };

        Self {
            state: Mutex::new(state),
            child_in: Mutex::new(child_in),
            client_out: Mutex::new(client_out),
        }
    }

    fn to_child(&self, message: &Value) {
        let mut w = self.child_in.lock().expect("child stdin");

        if let Err(e) = crate::rpc::write_message(&mut *w, message) {
            log::error(&format!("write to child failed: {e}"));
        }
    }

    fn to_client(&self, message: &Value) {
        let mut w = self.client_out.lock().expect("client stdout");

        if let Err(e) = crate::rpc::write_message(&mut *w, message) {
            log::error(&format!("write to client failed: {e}"));
        }
    }

    fn respond(&self, id: &Value, result: Value) {
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// `textDocument/formatting`: `alloy fmt` over the open document, as
    /// one edit that replaces the whole text. An `.alx` file, a file
    /// that does not lex, and one already formatted get no edits.
    fn format_document(&self, uri: &str, id: &Value) {
        let source = {
            let st = self.state.lock().expect("state");

            st.docs.get(uri).map(|d| d.source.clone())
        };
        let Some(source) = source else {
            self.respond(id, Value::Null);

            return;
        };

        if uri.ends_with(".alx") {
            self.respond(id, json!([]));

            return;
        }

        match alloy::fmt::format(&source) {
            Ok(formatted) if formatted != source => {
                let (el, ec) = position_of(&source, source.len());
                self.respond(
                    id,
                    json!([{
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": el, "character": ec } },
                        "newText": formatted,
                    }]),
                );
            }

            _ => self.respond(id, json!([])),
        }
    }

    // --- editor -> child ------------------------------------------------------

    /// Handles one message from the editor. Returns false on `exit`.
    pub fn handle_client(&self, mut message: Value) -> bool {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        log::trace(&format!(
            "client -> {} id={}",
            method.as_deref().unwrap_or("(response)"),
            message.get("id").map(id_key).unwrap_or_default()
        ));

        match method.as_deref() {
            Some("initialize") => {
                let root = message
                    .pointer("/params/rootUri")
                    .and_then(Value::as_str)
                    .and_then(uri_to_path)
                    .or_else(|| {
                        message
                            .pointer("/params/workspaceFolders/0/uri")
                            .and_then(Value::as_str)
                            .and_then(uri_to_path)
                    });
                let mut st = self.state.lock().expect("state");
                st.mirror = mirror_dir(root.as_deref());
                let _ = std::fs::remove_dir_all(&st.mirror);
                let _ = std::fs::create_dir_all(&st.mirror);
                st.root = root;
                st.initialize_id = message.get("id").map(id_key);
                let mirror_uri = path_to_uri(&st.mirror);
                let mirror_path = st.mirror.to_string_lossy().into_owned();

                if let Some(options) = message.pointer("/params/initializationOptions") {
                    let over = settings::from_editor(options);
                    settings::merge(&mut st.settings, &over);
                }

                // The child reads its own shape from here, and asks for
                // it over `workspace/configuration`, which the proxy
                // answers: the editor need not support that request.
                let child_settings = st.settings.clone();
                drop(st);

                if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
                    params.insert("initializationOptions".to_string(), child_settings);

                    // The child's workspace is the mirror.
                    if params.contains_key("rootUri") {
                        params.insert("rootUri".to_string(), Value::String(mirror_uri.clone()));
                    }

                    if params.contains_key("rootPath") {
                        params.insert("rootPath".to_string(), Value::String(mirror_path));
                    }

                    // Every folder of the editor maps into the one mirror,
                    // so the child gets one folder. With the same URI
                    // listed twice it never finishes configuring its
                    // workspaces, and every request waits forever.
                    if let Some(folders) = params
                        .get_mut("workspaceFolders")
                        .and_then(Value::as_array_mut)
                    {
                        let name = folders
                            .first()
                            .and_then(|f| f.get("name"))
                            .cloned()
                            .unwrap_or_else(|| Value::String("workspace".to_string()));
                        folders.clear();
                        folders.push(json!({ "uri": mirror_uri.clone(), "name": name }));
                    }

                    let caps = params.entry("capabilities").or_insert_with(|| json!({}));

                    if let Some(caps) = caps.as_object_mut() {
                        let ws = caps.entry("workspace").or_insert_with(|| json!({}));

                        if let Some(ws) = ws.as_object_mut() {
                            ws.insert("configuration".to_string(), Value::Bool(true));
                        }
                    }
                }

                self.to_child(&message);
            }

            Some("initialized") => {
                self.to_child(&message);
                self.open_workspace();
            }

            Some("exit") => {
                self.to_child(&message);

                return false;
            }

            Some("workspace/didChangeConfiguration") => {
                let mut st = self.state.lock().expect("state");

                if let Some(settings) = message.pointer("/params/settings") {
                    let over = settings::from_editor(settings);
                    settings::merge(&mut st.settings, &over);
                }

                let child_settings = st.settings.clone();
                drop(st);
                self.to_child(&json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": { "settings": child_settings }
                }));
            }

            Some("textDocument/didOpen") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri) {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let version = message
                        .pointer("/params/textDocument/version")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.open_doc(&uri, text, version, true);
                } else {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.plain_changed(&uri, Some(text), &[]);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didChange") => {
                let uri = text_document_uri(&message).unwrap_or_default();
                let changes = message
                    .pointer("/params/contentChanges")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                if is_alloy_uri(&uri) {
                    let version = message
                        .pointer("/params/textDocument/version")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.change_doc(&uri, version, &changes);
                } else {
                    self.plain_changed(&uri, None, &changes);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didClose") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri) {
                    // The shadow stays open so other files still resolve
                    // it; its text goes back to the disk version.
                    let mut st = self.state.lock().expect("state");
                    st.editor_open.remove(&uri);
                    drop(st);

                    match uri_to_path(&uri).and_then(|p| std::fs::read_to_string(p).ok()) {
                        Some(text) => self.change_doc(&uri, 0, &[json!({ "text": text })]),

                        None => self.close_shadow(&uri),
                    }
                } else {
                    // Back to the disk version in the mirror.
                    let mut st = self.state.lock().expect("state");
                    st.plain.remove(&uri);

                    if let Some(path) = uri_to_path(&uri) {
                        match std::fs::read_to_string(&path) {
                            Ok(text) => st.write_mirror(&path, &text),

                            Err(_) => st.remove_mirror(&path),
                        }
                    }

                    drop(st);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didSave") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri)
                    && let Some(params) = message.get_mut("params").and_then(Value::as_object_mut)
                {
                    params.remove("text");
                }

                self.forward_plain(message);
            }

            Some("workspace/didChangeWatchedFiles") => {
                let changes = message
                    .pointer("/params/changes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                for change in &changes {
                    let uri = change
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let kind = change.get("type").and_then(Value::as_i64).unwrap_or(2);

                    if !is_alloy_uri(&uri) {
                        // A plain file: the mirror copy follows the disk
                        // unless the editor holds the file open.
                        let st = self.state.lock().expect("state");

                        if let Some(path) = uri_to_path(&uri)
                            && !st.plain.contains_key(&uri)
                        {
                            match (kind, std::fs::read_to_string(&path)) {
                                (3, _) | (_, Err(_)) => st.remove_mirror(&path),

                                (_, Ok(text)) => st.write_mirror(&path, &text),
                            }
                        }

                        drop(st);

                        continue;
                    }

                    let open = self.state.lock().expect("state").editor_open.contains(&uri);

                    match kind {
                        3 => self.close_shadow(&uri),

                        _ if !open => {
                            if let Some(text) =
                                uri_to_path(&uri).and_then(|p| std::fs::read_to_string(p).ok())
                            {
                                self.open_doc(&uri, text, 0, false);
                            }
                        }

                        _ => {}
                    }
                }

                self.forward_plain(message);
            }

            // The mirror is the child's one folder, whatever the editor
            // adds or removes on its side.
            Some("workspace/didChangeWorkspaceFolders") => {}

            Some("workspace/didRenameFiles") => {
                let files = message
                    .pointer("/params/files")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self.renamed(&files);
            }

            Some("textDocument/formatting") => {
                if let Some(id) = message.get("id").cloned() {
                    let uri = text_document_uri(&message).unwrap_or_default();
                    self.format_document(&uri, &id);
                }
            }

            Some(m @ ("textDocument/hover" | "textDocument/completion")) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if uri.ends_with(".alx")
                    && let Some(id) = message.get("id").cloned()
                    && self.markup_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && (self.field_hover(&uri, &message, &id)
                        || self.declaration_hover(&uri, &message, &id)
                        || self.keyword_hover(&uri, &message, &id))
                {
                    return true;
                }

                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && self.context_completion(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, method.as_deref());
            }

            Some("alloy/blockEnd") => {
                // The editor asks after Enter: the line before the new one.
                let uri = text_document_uri(&message).unwrap_or_default();
                let line = message
                    .pointer("/params/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let indent = self
                    .state
                    .lock()
                    .expect("state")
                    .docs
                    .get(&uri)
                    .and_then(|d| block_end::needs_end(&d.source, line));

                if let Some(id) = message.get("id") {
                    let result = match indent {
                        Some(indent) => json!({ "indent": indent }),

                        None => Value::Null,
                    };
                    self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }

            Some(
                m @ ("textDocument/definition"
                | "textDocument/declaration"
                | "textDocument/typeDefinition"),
            ) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.definition_answer(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            Some(_) => self.forward_request(message, method.as_deref()),

            None => {
                // A response from the editor: to a question of ours, or to
                // the child's, whose URIs then move into the mirror.
                let key = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let asked = self.state.lock().expect("state").asked.remove(&key);

                match asked {
                    Some(Asked::Rename(edit)) => {
                        let chosen = message
                            .pointer("/result/title")
                            .and_then(Value::as_str)
                            .unwrap_or_default();

                        if chosen == UPDATE_IMPORTS {
                            let id = self.state.lock().expect("state").fresh_id();
                            self.to_client(&json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "method": "workspace/applyEdit",
                                "params": { "label": "Update imports", "edit": edit }
                            }));
                        }
                    }

                    None => self.forward_plain(message),
                }
            }
        }

        true
    }

    /// Forwards a message whose URIs name real files: each becomes its
    /// mirror URI.
    fn forward_plain(&self, mut message: Value) {
        let st = self.state.lock().expect("state");
        map_uris_into_mirror(&mut message, &st);
        drop(st);
        self.to_child(&message);
    }

    /// A plain Luau document changed in the editor: the mirror copy
    /// follows the text.
    fn plain_changed(&self, uri: &str, whole: Option<String>, changes: &[Value]) {
        let mut st = self.state.lock().expect("state");
        let mut text = whole
            .or_else(|| st.plain.get(uri).cloned())
            .unwrap_or_default();

        for change in changes {
            let piece = change
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let range = change.get("range").and_then(range_of);
            crate::doc::apply_change(&mut text, range, piece);
        }

        if let Some(path) = uri_to_path(uri) {
            st.write_mirror(&path, &text);
        }

        st.plain.insert(uri.to_string(), text);
    }

    /// Maps a request about an Alloy document into its shadow and
    /// forwards it, remembering what it was about.
    fn forward_request(&self, message: Value, method: Option<&str>) {
        self.forward_request_with(message, method, None);
    }

    /// Forwards a request whose shadow position is already known.
    fn forward_request_at(&self, message: Value, method: Option<&str>, shadow: (u32, u32)) {
        self.forward_request_with(message, method, Some(shadow));
    }

    fn forward_request_with(
        &self,
        mut message: Value,
        method: Option<&str>,
        shadow: Option<(u32, u32)>,
    ) {
        let uri = text_document_uri(&message);

        // Semantic tokens for markup come from the lowered code, whose
        // columns are not the source's; the grammar colors `.alx`.
        if let Some(u) = &uri
            && u.ends_with(".alx")
            && method.is_some_and(|m| m.starts_with("textDocument/semanticTokens"))
            && let Some(id) = message.get("id").cloned()
        {
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": { "data": [] } }));

            return;
        }

        // The child never sees a `.d.aly` document, so a request on one
        // gets an empty answer here instead of the child's error.
        if let Some(u) = &uri
            && !child_sees(u)
            && let Some(id) = message.get("id").cloned()
        {
            let result = match method {
                Some("textDocument/diagnostic") => json!({ "kind": "full", "items": [] }),

                Some("textDocument/semanticTokens/full") => json!({ "data": [] }),

                _ => Value::Null,
            };
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return;
        }
        let ctx = uri.filter(|u| is_alloy_uri(u));
        let position = message
            .pointer("/params/position")
            .and_then(position_of_value);
        let trigger = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let range = message.pointer("/params/range").and_then(range_of);

        if let Some(id) = message.get("id") {
            let key = id_key(id);
            self.state.lock().expect("state").pending.insert(
                key,
                Pending {
                    method: method.unwrap_or_default().to_string(),
                    ctx: ctx.clone(),
                    position,
                    trigger,
                    range,
                },
            );
        }

        let st = self.state.lock().expect("state");

        if let Some(ctx) = &ctx
            && let Some(doc) = st.docs.get(ctx)
            && let Some(params) = message.get_mut("params")
        {
            map_into_shadow(params, doc);

            if let Some((line, character)) = shadow {
                params["position"] = json!({ "line": line, "character": character });
            }
        }

        map_uris_into_mirror(&mut message, &st);
        drop(st);
        self.to_child(&message);
    }

    /// Answers a hover on bytes the desugar replaced: an Alloy keyword or
    /// operator gets its own text, other punctuation gets nothing. A word
    /// with no entry, such as a hoisted name, still goes to the child.
    /// Returns false when the child should answer.
    /// A hover on the name of a struct, interface, enum, or trait shows
    /// the declaration as the source wrote it. The child would show a
    /// table or a type alias. This file's declarations win; then any
    /// open file's, since an import brings the name in unchanged.
    fn declaration_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];

        // A sigil names a macro or an attribute of this project. After a
        // dot the name is the receiver's: `Msg.Join` finds the variant
        // through the enum's name; any other field is not ours.
        let before = doc.source[..start].trim_end();
        let key = if doc.source[..start].ends_with('$') || before.ends_with("macro") {
            format!("${word}")
        } else if doc.source[..start].ends_with('@') || before.ends_with("attribute") {
            format!("@{word}")
        } else if let Some(head) = before.strip_suffix('.') {
            let at = head.len().saturating_sub(1);

            if head.is_empty() || !keywords::is_word_at(&doc.source, at) {
                return false;
            }

            let (hs, he) = keywords::word_range(&doc.source, at);

            format!("{}.{word}", &doc.source[hs..he])
        } else if doc.source[..start].ends_with(':') {
            // `obj:method`, not the `x: T` of an annotation.
            return false;
        } else {
            word.to_string()
        };

        let found = doc.decls.iter().find(|d| d.name == key).or_else(|| {
            st.docs
                .values()
                .flat_map(|d| d.decls.iter())
                .find(|d| d.name == key)
        });

        let Some(decl) = found else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": decl.hover },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// A key in a struct's raw constructor, `Menu { button = ... }`, hovers
    /// as the struct's field. The child sees a table key and answers with
    /// the key's string type.
    fn field_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];

        // The key sits before `=` inside the braces of `Name { ... }`.
        if !doc.source[end..].trim_start().starts_with('=')
            || doc.source[end..].trim_start().starts_with("==")
        {
            return false;
        }

        let Some(open) = enclosing_brace(&doc.source, start) else {
            return false;
        };
        let head = doc.source[..open].trim_end();

        if !head.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
            return false;
        }

        let (hs, he) = keywords::word_range(&doc.source, head.len() - 1);
        let struct_name = &doc.source[hs..he];
        let field_line = doc
            .decls
            .iter()
            .chain(st.docs.values().flat_map(|d| d.decls.iter()))
            .filter(|d| d.name == struct_name && d.hover.contains("struct "))
            .find_map(|d| {
                d.hover
                    .lines()
                    .find(|l| {
                        let t = l.trim_start();
                        let t = t
                            .strip_prefix("read ")
                            .or_else(|| t.strip_prefix("write "))
                            .unwrap_or(t);

                        t.starts_with(&format!("{word}:"))
                    })
                    .map(|l| l.trim().to_string())
            });

        let Some(field_line) = field_line else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": {
                "kind": "markdown",
                "value": format!("```alloy\n{field_line}\n```\nA field of `struct {struct_name}`."),
            },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// Go to definition for a name Alloy declares: a struct, an enum or a
    /// variant, a trait, an interface, a type alias, a macro, or an
    /// attribute, in this file first and then any file of the workspace.
    /// The child answers for everything the emit keeps as written.
    fn definition_answer(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];
        let raw_before = &doc.source[..start];
        let key = if raw_before.ends_with('$') || raw_before.trim_end().ends_with("macro") {
            format!("${word}")
        } else if raw_before.ends_with('@') || raw_before.trim_end().ends_with("attribute") {
            format!("@{word}")
        } else if let Some(head) = raw_before.trim_end().strip_suffix('.') {
            let at = head.len().saturating_sub(1);

            if head.is_empty() || !keywords::is_word_at(&doc.source, at) {
                return false;
            }

            let (hs, he) = keywords::word_range(&doc.source, at);

            format!("{}.{word}", &doc.source[hs..he])
        } else if raw_before.ends_with(':') {
            return false;
        } else {
            word.to_string()
        };

        let found = doc
            .decls
            .iter()
            .find(|d| d.name == key)
            .map(|d| (uri.to_string(), doc, d))
            .or_else(|| {
                st.docs.iter().find_map(|(u, d)| {
                    d.decls
                        .iter()
                        .find(|x| x.name == key)
                        .map(|x| (u.clone(), d, x))
                })
            });

        let Some((target_uri, target_doc, decl)) = found else {
            return false;
        };

        let bare = decl.name.rsplit('.').next().unwrap_or(&decl.name);
        let name_len = bare.trim_start_matches(['$', '@']).len();
        let s = position_of(&target_doc.source, decl.offset);
        let e = position_of(&target_doc.source, decl.offset + name_len);
        let result = json!([{ "uri": target_uri, "range": range_value(s, e) }]);
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// Answers a completion inside an attribute, a macro call, a remote's
    /// side, or an import, where the child would list globals.
    fn context_completion(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        let Some(ctx) = context::detect(&doc.source, offset) else {
            return false;
        };

        let items = st.context_items(uri, offset, &ctx);
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": items }));

        true
    }

    fn keyword_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(out) = &doc.output else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        // A copied byte has a shadow position; the child answers there.
        if out.map.to_output(offset as u32).is_some() {
            return false;
        }

        // A std name the file binds itself, through an import or a
        // declaration, is the file's: the child answers for that one.
        let hit = keywords::hover(&doc.source, offset).filter(|(start, end, _)| {
            let word = &doc.source[*start..*end];
            let is_std = alloy::desugar::AMBIENT.contains(&word)
                || matches!(
                    word,
                    "SignalConnection" | "Signalish" | "Partial" | "Readonly" | "Sink"
                );

            !(is_std && doc_binds(doc, word))
        });

        let result = match hit {
            Some((start, end, text)) => {
                let (sl, sc) = position_of(&doc.source, start);
                let (el, ec) = position_of(&doc.source, end);

                json!({
                    "contents": { "kind": "markdown", "value": text },
                    "range": {
                        "start": { "line": sl, "character": sc },
                        "end": { "line": el, "character": ec },
                    },
                })
            }

            // A replaced word such as a struct name has a home in the
            // shadow: on the same line when the emit kept it there, else
            // where the declaration landed. The child answers there.
            None if keywords::is_word_at(&doc.source, offset) => {
                let (start, end) = keywords::word_range(&doc.source, offset);
                let word = &doc.source[start..end];

                let Some(target) = shadow_home(&doc.shadow, line, word) else {
                    return false;
                };

                drop(st);
                self.forward_request_at(message.clone(), Some("textDocument/hover"), target);

                return true;
            }

            None => Value::Null,
        };

        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// Answers a hover or completion inside `.alx` markup. Returns false
    /// when the cursor is not on markup, so the child answers.
    fn markup_answer(&self, method: &str, uri: &str, message: &Value, id: &Value) -> bool {
        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };
        let bound = markup_bound(&doc.source);

        let result = match method {
            "textDocument/hover" => match markup::hover_spot(&doc.source, offset) {
                Some(spot) => markup::hover(&spot, &bound).unwrap_or(Value::Null),

                None => return false,
            },

            _ => match markup::completion_spot(&doc.source, offset) {
                Some(spot) => Value::Array(markup::completions(&spot, &bound)),

                None => return false,
            },
        };

        drop(st);
        self.respond(id, result);

        true
    }

    // --- child -> editor ------------------------------------------------------

    /// Handles one message from the child.
    pub fn handle_child(&self, mut message: Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        log::trace(&format!(
            "child -> {} id={}",
            method.as_deref().unwrap_or("(response)"),
            message.get("id").map(id_key).unwrap_or_default()
        ));

        match method.as_deref() {
            Some("textDocument/publishDiagnostics") => {
                let uri = message
                    .pointer("/params/uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let st = self.state.lock().expect("state");

                if st.runtime_uri.as_deref() == Some(uri.as_str()) {
                    return;
                }

                let (source, is_alloy) = st.editor_uri(&uri);
                drop(st);

                match is_alloy {
                    true => {
                        let diagnostics = message
                            .pointer("/params/diagnostics")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let mut st = self.state.lock().expect("state");
                        let mapped: Vec<Value> = match st.docs.get(&source) {
                            Some(doc) => {
                                let mut out: Vec<Value> = Vec::new();

                                for mut d in diagnostics {
                                    if !keep_diagnostic(&d, doc) {
                                        continue;
                                    }

                                    map_from_shadow(&mut d, Some(&source), &st);
                                    friendly_message(&mut d, doc, &st);

                                    // Two references in one desugar map to
                                    // one source token: report it once.
                                    if !out.iter().any(|o| {
                                        o["range"] == d["range"] && o["message"] == d["message"]
                                    }) {
                                        out.push(d);
                                    }
                                }

                                out
                            }

                            None => Vec::new(),
                        };
                        st.child_diagnostics.insert(source.clone(), mapped);
                        drop(st);
                        self.publish(&source);
                    }

                    false => {
                        if let Some(p) = message.pointer_mut("/params/uri") {
                            *p = Value::String(source);
                        }

                        self.to_client(&message);
                    }
                }
            }

            Some("workspace/configuration") => {
                // The child's settings are ours to answer.
                let count = message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map_or(1, Vec::len);
                let settings = self.state.lock().expect("state").settings.clone();
                let result: Vec<Value> = (0..count).map(|_| settings.clone()).collect();

                if let Some(id) = message.get("id") {
                    self.to_child(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }

            Some("client/registerCapability") => {
                // Dynamic semantic token registration would bypass the
                // static one the proxy edits; the static one stays.
                if let Some(list) = message
                    .pointer_mut("/params/registrations")
                    .and_then(Value::as_array_mut)
                {
                    list.retain(|r| {
                        !r.get("method")
                            .and_then(Value::as_str)
                            .is_some_and(|m| m.starts_with("textDocument/semanticTokens"))
                    });
                }

                self.to_client(&message);
            }

            Some(_) => {
                // A server request or notification: map any locations.
                let st = self.state.lock().expect("state");

                if let Some(params) = message.get_mut("params") {
                    map_from_shadow(params, None, &st);
                }

                drop(st);
                self.to_client(&message);
            }

            None => self.child_response(message),
        }
    }

    fn child_response(&self, mut message: Value) {
        let key = message.get("id").map(id_key);
        let mut st = self.state.lock().expect("state");
        let pending = key.as_ref().and_then(|k| st.pending.remove(k));
        let is_init = key.is_some() && key == st.initialize_id;

        if is_init {
            st.initialize_id = None;
            edit_capabilities(&mut message);
        }

        let (method, ctx, position, trigger, range) = match pending {
            Some(p) => (p.method, p.ctx, p.position, p.trigger, p.range),

            None => (String::new(), None, None, None, None),
        };

        // The child answers null when it has no action; the Alloy
        // rewrites need a list to join.
        if method == "textDocument/codeAction"
            && ctx.is_some()
            && message.get("result").is_some_and(Value::is_null)
        {
            message["result"] = json!([]);
        }

        if let Some(result) = message.get_mut("result") {
            // Hints and tokens in generated text describe temps: gone
            // before the mapping moves what is left.
            if let Some(uri) = &ctx
                && let Some(doc) = st.docs.get(uri)
            {
                match method.as_str() {
                    // The child says `local` or `function`; the source may
                    // have said `const`, `export`, or `async`.
                    "textDocument/hover" => {
                        if let Some((line, character)) = position
                            && let Some(value) = result
                                .pointer("/contents/value")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        {
                            let mut text = value.clone();

                            if let Some(rewritten) = restyle_hover(&text, doc, line, character) {
                                text = rewritten;
                            }

                            if let Some(kept) = keep_annotation(&text, doc, line, character) {
                                text = kept;
                            }

                            text = fold_std_shapes(&text);

                            if let Some(with_init) = append_initializer(&text, doc, line, character)
                            {
                                text = with_init;
                            }

                            if text != value {
                                result["contents"]["value"] = json!(text);
                            }
                        }
                    }

                    // A pulled report gets the filter the push path has.
                    "textDocument/diagnostic" => {
                        if let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) {
                            items.retain(|d| keep_diagnostic(d, doc));

                            for d in items.iter_mut() {
                                friendly_message(d, doc, &st);
                            }
                        }
                    }

                    "textDocument/inlayHint" => {
                        if let Some(hints) = result.as_array_mut() {
                            // A hint attaches to the byte before it, so
                            // that byte decides: the `)` of a generated
                            // inner function is generated even when the
                            // copied newline follows. A hint that names an
                            // error type describes the emit, not the source.
                            hints.retain(|h| {
                                let error_type = hint_label(h).contains("*error-type*");
                                let generated = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, c)| offset_of(&doc.shadow, l, c))
                                    .is_some_and(|o| {
                                        doc.output.as_ref().is_some_and(|out| {
                                            out.map.is_generated(o.saturating_sub(1) as u32)
                                        })
                                    });

                                !error_type && !generated
                            });

                            // An async function declares the inner type;
                            // the child infers the Future around it.
                            for h in hints.iter_mut() {
                                let async_line = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, _)| doc.shadow.lines().nth(l as usize))
                                    .is_some_and(|line| line.contains(".future(function"));

                                if async_line {
                                    unwrap_future_hint(h);
                                }
                            }
                        }
                    }

                    "textDocument/semanticTokens/full" => {
                        if let Some(data) = result.get("data").and_then(Value::as_array) {
                            let raw: Vec<u64> = data.iter().filter_map(Value::as_u64).collect();
                            let mapped = tokens::remap(&raw, doc);
                            log::debug(&format!(
                                "semantic tokens: {} in, {} out",
                                raw.len() / 5,
                                mapped.len() / 5
                            ));
                            result["data"] = json!(mapped);
                        }

                        drop(st);
                        self.to_client(&message);

                        return;
                    }

                    _ => {}
                }
            }

            if method == "workspace/diagnostic"
                && let Some(reports) = result.get_mut("items").and_then(Value::as_array_mut)
            {
                for report in reports {
                    let doc = report
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(|shadow| st.shadows.get(shadow))
                        .and_then(|source| st.docs.get(source));

                    if let Some(doc) = doc
                        && let Some(items) = report.get_mut("items").and_then(Value::as_array_mut)
                    {
                        items.retain(|d| keep_diagnostic(d, doc));
                    }
                }
            }

            map_from_shadow(result, ctx.as_deref(), &st);

            // The editor never sees the runtime's table: `__alloy.Future<T>`
            // reads `Future<T>`, and `__alloy_string.trim` reads `string.trim`.
            if ctx.is_some()
                && matches!(
                    method.as_str(),
                    "textDocument/hover"
                        | "textDocument/inlayHint"
                        | "textDocument/completion"
                        | "completionItem/resolve"
                        | "textDocument/signatureHelp"
                )
            {
                strip_std_prefix(result);

                if let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u)) {
                    strip_import_temps(result, &doc.shadow);
                }
            }

            // The Alloy diagnostics travel on the push channel alone, in
            // `publish`; a pulled report that carried them too showed
            // each lint twice in an editor that reads both.
            match method.as_str() {
                // The rewrites of the lints in the range, as quick fixes,
                // and one action that applies every rewrite of the file.
                "textDocument/codeAction" => {
                    if let Some(uri) = &ctx
                        && let Some(range) = range
                        && let Some(actions) = result.as_array_mut()
                    {
                        actions.extend(st.lint_actions(uri, range));
                    }
                }

                "textDocument/signatureHelp" => {
                    if let Some(uri) = &ctx {
                        st.rewrite_variant_signatures(uri, result);
                    }
                }

                // A link sits on the require the emit wrote, which maps to
                // the start of the import; it moves to the quoted path of
                // that source line, and its target leaves the mirror.
                "textDocument/documentLink" => {
                    if let Some(uri) = &ctx
                        && let Some(doc) = st.docs.get(uri)
                        && let Some(links) = result.as_array_mut()
                    {
                        links.retain_mut(|link| {
                            if let Some(target) = link.get("target").and_then(Value::as_str)
                                && let Some(source) = st.shadows.get(target)
                            {
                                let source = source.clone();
                                link["target"] = json!(source);
                            }

                            let Some(((line, _), _)) = link.get("range").and_then(range_of) else {
                                return false;
                            };

                            match quoted_span_on_line(&doc.source, line) {
                                Some((s, e)) => {
                                    link["range"] = range_value((line, s), (line, e));

                                    true
                                }

                                None => false,
                            }
                        });
                    }
                }

                "workspace/diagnostic" => {
                    if let Some(reports) = result.get_mut("items").and_then(Value::as_array_mut) {
                        for report in reports {
                            let uri = report
                                .get("uri")
                                .and_then(Value::as_str)
                                .map(str::to_string);

                            if let Some(uri) = uri
                                && st.docs.contains_key(&uri)
                                && let Some(items) =
                                    report.get_mut("items").and_then(Value::as_array_mut)
                            {
                                items.extend(st.alloy_diagnostics(&uri));
                            }
                        }
                    }
                }

                "textDocument/completion" => {
                    // The child lists a newline as a trigger for its `end`
                    // completion. That request wants nothing else: adding
                    // names would pop a list of them on every Enter.
                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                        && trigger.as_deref() != Some("\n")
                    {
                        st.mark_enum_members(uri, line, character, result);
                        let mut extra = st.auto_imports(uri, line, character);
                        extra.extend(st.primitive_completions(uri, line, character, result));
                        extra.extend(st.std_completions(uri, line, character, result));

                        if !extra.is_empty() {
                            match result {
                                Value::Array(items) => items.extend(extra),

                                Value::Object(obj) => {
                                    if let Some(items) =
                                        obj.get_mut("items").and_then(Value::as_array_mut)
                                    {
                                        items.extend(extra);
                                    }
                                }

                                Value::Null => *result = Value::Array(extra),

                                _ => {}
                            }
                        }
                    }
                }

                _ => {}
            }
        }

        drop(st);
        self.to_client(&message);
    }

    // --- documents ------------------------------------------------------------

    /// Opens or replaces a document and its shadow.
    fn open_doc(&self, uri: &str, text: String, version: i64, by_editor: bool) {
        let (options, jsx) = self.state.lock().expect("state").options_for(uri);
        let doc = Doc::new(text, version, &options, &jsx);
        let (shadow, existed) = {
            let mut st = self.state.lock().expect("state");
            let shadow = st.child_uri(uri);

            if by_editor {
                st.editor_open.insert(uri.to_string());
            }

            if let Some(path) = uri_to_path(uri) {
                st.write_mirror(&path, &doc.shadow);
            }

            let existed = st.docs.contains_key(uri);
            st.shadows.insert(shadow.clone(), uri.to_string());
            st.docs.insert(uri.to_string(), doc);

            (shadow, existed)
        };

        let st = self.state.lock().expect("state");
        let doc = &st.docs[uri];

        let message = if existed {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": shadow, "version": doc.version },
                    "contentChanges": [{ "text": doc.shadow }]
                }
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": shadow,
                        "languageId": "luau",
                        "version": doc.version,
                        "text": doc.shadow
                    }
                }
            })
        };

        drop(st);

        if child_sees(uri) {
            self.to_child(&message);
        }

        self.publish(uri);
    }

    fn change_doc(&self, uri: &str, version: i64, changes: &[Value]) {
        let (options, jsx) = self.state.lock().expect("state").options_for(uri);
        let mut st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get_mut(uri) else {
            drop(st);

            if let Some(text) = changes
                .last()
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
            {
                self.open_doc(uri, text.to_string(), version, true);
            }

            return;
        };

        for change in changes {
            let text = change
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let range = change.get("range").and_then(range_of);
            doc.apply_change(range, text);
        }

        doc.version = version;
        doc.compile(&options, &jsx);
        let shadow_text = doc.shadow.clone();
        let shadow = st.child_uri(uri);

        if let Some(path) = uri_to_path(uri) {
            st.write_mirror(&path, &shadow_text);
        }

        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": shadow, "version": version },
                "contentChanges": [{ "text": shadow_text }]
            }
        });
        drop(st);

        if child_sees(uri) {
            self.to_child(&message);
        }

        self.publish(uri);
    }

    fn close_shadow(&self, uri: &str) {
        let mut st = self.state.lock().expect("state");
        let existed = st.docs.remove(uri).is_some();
        let shadow = st.child_uri(uri);
        st.shadows.remove(&shadow);
        st.child_diagnostics.remove(uri);

        if let Some(path) = uri_to_path(uri) {
            st.remove_mirror(&path);
        }

        drop(st);

        if !existed {
            return;
        }

        self.to_child(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": shadow } }
        }));
        self.to_client(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": [] }
        }));
    }

    /// Publishes the Alloy diagnostics and the mapped child diagnostics
    /// of one source document.
    fn publish(&self, uri: &str) {
        let st = self.state.lock().expect("state");

        if !st.docs.contains_key(uri) {
            return;
        }

        let mut diagnostics: Vec<Value> = st.alloy_diagnostics(uri);

        if let Some(mapped) = st.child_diagnostics.get(uri) {
            diagnostics.extend(mapped.iter().cloned());
        }

        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        });
        drop(st);
        self.to_client(&message);
    }

    /// Opens a shadow for every Alloy file under the root, and one for the
    /// runtime, so requires between them resolve.
    fn open_workspace(&self) {
        let root = self.state.lock().expect("state").root.clone();
        let Some(root) = root else {
            return;
        };

        let (input, out) =
            match Config::find(&root).and_then(|p| Config::load(&p).ok().map(|c| (p, c))) {
                Some((p, c)) => {
                    let base = p.parent().unwrap_or(&root).to_path_buf();

                    (base.join(&c.build.input), Some(base.join(&c.build.out)))
                }

                None => (root.clone(), None),
            };

        let mut files = Vec::new();
        let mut plain = Vec::new();
        walk(&root, out.as_deref(), &mut files, &mut plain);

        // An input outside the root, `in = "../examples"`, gets its
        // shadows too, or a require between its files finds nothing.
        let input = normalize(&input);

        if !input.starts_with(normalize(&root)) && input.is_dir() {
            walk(&input, out.as_deref(), &mut files, &mut plain);
        }

        files.sort();
        files.dedup();

        // Plain files copy into the mirror, so requires to them resolve.
        // The root's Luau configuration sets strict mode when it sets no
        // mode, and a root with no configuration gets one: strict is
        // the default.
        {
            let st = self.state.lock().expect("state");

            for path in plain {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let text = strict_config(&path, &root, text);
                    let text = if path.file_name().is_some_and(|n| n == "sourcemap.json") {
                        mirrored_sourcemap(&text, &input, out.as_deref(), &root)
                    } else {
                        text
                    };
                    st.write_mirror(&path, &text);

                    // `alloy build` writes `.alloy/sourcemap.json`; a root
                    // with no `sourcemap.json` of its own uses it.
                    if path == root.join(".alloy/sourcemap.json")
                        && !root.join("sourcemap.json").is_file()
                    {
                        st.write_mirror(&root.join("sourcemap.json"), &text);
                    }
                }
            }

            if !alloy::luau_config::has_config(&root) {
                let rc = root.join(".luaurc");
                st.write_mirror(&rc, "{ \"languageMode\": \"strict\" }\n");
            }
        }

        for path in files {
            let uri = path_to_uri(&path);
            let already = self.state.lock().expect("state").docs.contains_key(&uri);

            if already {
                continue;
            }

            if let Ok(text) = std::fs::read_to_string(&path) {
                self.open_doc(&uri, text, 0, false);
            }
        }

        let runtime = {
            let mut st = self.state.lock().expect("state");
            let real = normalize(&input.join("alloy.luau"));
            st.runtimes.borrow_mut().insert(real.clone());
            st.write_mirror(&real, alloy::RUNTIME);
            let uri = st.child_uri(&path_to_uri(&real));
            st.runtime_uri = Some(uri.clone());

            uri
        };
        self.to_child(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": runtime,
                    "languageId": "luau",
                    "version": 0,
                    "text": alloy::RUNTIME
                }
            }
        }));
        log::info("workspace shadows opened");
    }

    /// Files moved: the shadows follow at once, and the imports that
    /// named the old paths follow after the editor's answer.
    fn renamed(&self, files: &[Value]) {
        let mut renames = Vec::new();

        for f in files {
            let old = f
                .get("oldUri")
                .and_then(Value::as_str)
                .and_then(uri_to_path);
            let new = f
                .get("newUri")
                .and_then(Value::as_str)
                .and_then(uri_to_path);

            if let (Some(old), Some(new)) = (old, new) {
                renames.push(Rename { old, new });
            }
        }

        if renames.is_empty() {
            return;
        }

        // The edits come from the texts as they were before the move.
        let docs: Vec<(String, PathBuf, String)> = {
            let st = self.state.lock().expect("state");
            st.docs
                .iter()
                .filter_map(|(uri, doc)| {
                    uri_to_path(uri).map(|p| (uri.clone(), p, doc.source.clone()))
                })
                .collect()
        };
        let changes = imports::rename_edits(&docs, &renames);

        // Shadows move: the old one closes, the new one opens from disk
        // or from the text we hold.
        for (uri, path, source) in &docs {
            let moved = imports_map(path, &renames);

            if moved == *path {
                continue;
            }

            let text = std::fs::read_to_string(&moved).unwrap_or_else(|_| source.clone());
            let open = self.state.lock().expect("state").editor_open.contains(uri);
            self.close_shadow(uri);
            let new_uri = path_to_uri(&moved);
            self.open_doc(&new_uri, text, 0, open);
        }

        if changes.is_empty() {
            return;
        }

        let count: usize = changes.values().map(Vec::len).sum();
        let edit = json!({ "changes": changes });
        let id = {
            let mut st = self.state.lock().expect("state");
            let id = st.fresh_id();
            st.asked.insert(id.clone(), Asked::Rename(edit));

            id
        };
        self.to_client(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "window/showMessageRequest",
            "params": {
                "type": 3,
                "message": format!("Update {count} import path(s) for the moved file(s)?"),
                "actions": [{ "title": UPDATE_IMPORTS }, { "title": "Leave" }]
            }
        }));
    }
}

/// The answer that applies the rename edit.
const UPDATE_IMPORTS: &str = "Update imports";

fn imports_map(path: &Path, renames: &[Rename]) -> PathBuf {
    for r in renames {
        if path == r.old {
            return r.new.clone();
        }

        if let Ok(rest) = path.strip_prefix(&r.old) {
            return r.new.join(rest);
        }
    }

    path.to_path_buf()
}

impl State {
    /// Auto-import items for a completion at a source position.
    fn auto_imports(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };
        let prefix = imports::word_before(&doc.source, offset);

        if prefix.is_empty() {
            return Vec::new();
        }

        let bound = markup_bound(&doc.source);
        let files: Vec<(PathBuf, &[imports::Export])> = self
            .docs
            .iter()
            .filter_map(|(u, d)| uri_to_path(u).map(|p| (p, d.exports.as_slice())))
            .collect();

        imports::auto_import_items(&doc.source, &path, &files, &prefix, &bound)
    }
}

/// The names bound in a file, with markup blanked for `.alx`.
fn markup_bound(src: &str) -> HashSet<String> {
    let blanked = alloy::luaux::compile::markup_spans(src)
        .map(|spans| alloy::luaux::resolve::blank_luaux_regions(src, &spans))
        .unwrap_or_else(|_| src.to_string());

    alloy::alx::bound_names(&blanked)
}

/// Alloy files into `out`; the plain files a require or the child's
/// configuration can reach into `plain`.
fn walk(dir: &Path, skip: Option<&Path>, out: &mut Vec<PathBuf>, plain: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "node_modules" | "target")
                || Some(path.as_path()) == skip
            {
                continue;
            }

            walk(&path, skip, out, plain);
        } else if name.ends_with(".aly") || name.ends_with(".alx") {
            out.push(path);
        } else if [".luau", ".lua", ".json", ".toml", ".luaurc"]
            .iter()
            .any(|ext| name.ends_with(ext))
            || name == ".luaurc"
        {
            plain.push(path);
        }
    }
}

/// The mirror directory for a workspace root: stable per root, so a
/// restart finds the same place and starts it clean.
/// A path with its `.` and `..` components folded, so `crates/../examples`
/// and `examples` name one place.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }

            other => out.push(other),
        }
    }

    out
}

fn mirror_dir(root: Option<&Path>) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default()
        .hash(&mut hasher);

    std::env::temp_dir()
        .join("alloy-lsp")
        .join(format!("{:016x}", hasher.finish()))
}

/// Moves every URI in a message from the workspace into the mirror.
fn map_uris_into_mirror(value: &mut Value, st: &State) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                match (key.as_str(), v) {
                    ("uri" | "targetUri" | "oldUri" | "newUri", Value::String(uri)) => {
                        *uri = st.child_uri(uri);
                    }

                    (_, other) => map_uris_into_mirror(other, st),
                }
            }
        }

        Value::Array(items) => {
            for item in items {
                map_uris_into_mirror(item, st);
            }
        }

        _ => {}
    }
}

// --- mapping helpers ---------------------------------------------------------

/// Maps positions and ranges in request params into the shadow.
/// The shadow position of `word` for a hover: the same line first, then
/// the first line that holds it as a whole word.
fn shadow_home(shadow: &str, line: u32, word: &str) -> Option<(u32, u32)> {
    let lines: Vec<&str> = shadow.lines().collect();
    let same = lines.get(line as usize).copied().unwrap_or("");

    let (l, text, byte) = keywords::find_word(same, word)
        .map(|c| (line, same, c))
        .or_else(|| {
            lines
                .iter()
                .enumerate()
                .find_map(|(i, l)| keywords::find_word(l, word).map(|c| (i as u32, *l, c)))
        })?;

    Some((l, text[..byte].chars().count() as u32))
}

fn map_into_shadow(params: &mut Value, doc: &Doc) {
    match params {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "position" => {
                        if let Some((l, c)) = position_of_value(value) {
                            let (l, c) = doc.to_shadow(l, c);
                            *value = json!({ "line": l, "character": c });
                        }
                    }

                    "range" => {
                        if let Some(((sl, sc), (el, ec))) = range_of(value) {
                            let (sl, sc) = doc.to_shadow(sl, sc);
                            let (el, ec) = doc.to_shadow(el, ec);
                            *value = range_value((sl, sc), (el, ec));
                        }
                    }

                    _ => map_into_shadow(value, doc),
                }
            }
        }

        Value::Array(items) => {
            for item in items {
                map_into_shadow(item, doc);
            }
        }

        _ => {}
    }
}

/// Maps URIs and ranges in a result or notification back to sources.
/// `ctx` is the source URI ranges belong to until a `uri` key says
/// otherwise; a URI that is not a shadow clears it.
fn map_from_shadow(value: &mut Value, ctx: Option<&str>, st: &State) {
    match value {
        Value::Object(map) => {
            let mut here: Option<String> = ctx.map(str::to_string);

            if let Some(Value::String(uri)) = map.get_mut("uri") {
                let (real, is_alloy) = st.editor_uri(uri);
                *uri = real.clone();
                here = is_alloy.then_some(real);
            }

            let mut target: Option<String> = None;

            if let Some(Value::String(uri)) = map.get_mut("targetUri") {
                let (real, is_alloy) = st.editor_uri(uri);
                *uri = real.clone();
                target = is_alloy.then_some(real);
            }

            // A workspace edit keys changes by URI.
            if let Some(Value::Object(changes)) = map.get_mut("changes") {
                let mut rebuilt = Map::new();

                for (uri, mut edits) in std::mem::take(changes) {
                    let (real, is_alloy) = st.editor_uri(&uri);
                    map_from_shadow(&mut edits, is_alloy.then_some(real.as_str()), st);
                    rebuilt.insert(real, edits);
                }

                *changes = rebuilt;
            }

            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "changes" | "uri" | "targetUri" => {}

                    "targetRange" | "targetSelectionRange" => {
                        if let Some(doc) = target.as_deref().and_then(|u| st.docs.get(u)) {
                            map_range_value(value, doc);
                        }
                    }

                    "range" | "selectionRange" | "originSelectionRange" | "insert" | "replace" => {
                        if let Some(doc) = here.as_deref().and_then(|u| st.docs.get(u)) {
                            map_range_value(value, doc);
                        }
                    }

                    "position" => {
                        if let Some(doc) = here.as_deref().and_then(|u| st.docs.get(u))
                            && let Some((l, c)) = position_of_value(value)
                        {
                            let (l, c) = doc.to_source(l, c);
                            *value = json!({ "line": l, "character": c });
                        }
                    }

                    _ => map_from_shadow(value, here.as_deref(), st),
                }
            }
        }

        Value::Array(items) => {
            for item in items {
                map_from_shadow(item, ctx, st);
            }
        }

        _ => {}
    }
}

/// The child's hover header with the source's declaring keywords: the
/// `local m: T` of a `const` becomes `const m: T`, and `function f(` of
/// an `async function` becomes `async function f(`. The fence switches
/// to the Alloy grammar, which highlights `const`, `async`, and
/// `export`; the Luau grammar drops the highlight after them. None when
/// the header names something else or the source used the same keyword.
fn restyle_hover(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let binding = doc.bindings.iter().find(|b| b.name == word)?;
    let rest = value.strip_prefix("```luau\n")?;

    // A type function hovers as `function<a>(t): type`, nameless: the
    // name goes back in, behind `type function`.
    if (rest.starts_with("function<") || rest.starts_with("function("))
        && binding.prefix.ends_with("function")
    {
        let mut out = format!(
            "```alloy\n{} {word}{}",
            binding.prefix,
            &rest["function".len()..]
        );

        if let Some(doc) = &binding.doc {
            out.push_str("\n\n");
            out.push_str(doc);
        }

        return Some(out);
    }

    let (head, tail) = match rest {
        r if r.starts_with(&format!("local function {word}")) => {
            ("local function", &r["local function".len()..])
        }

        r if r.starts_with(&format!("local {word}")) => ("local", &r["local".len()..]),

        r if r.starts_with(&format!("function {word}")) => ("function", &r["function".len()..]),

        _ => return None,
    };

    if head == binding.prefix && binding.doc.is_none() {
        return None;
    }

    let mut out = format!("```alloy\n{}{tail}", binding.prefix);

    if let Some(doc) = &binding.doc {
        out.push_str("\n\n");
        out.push_str(doc);
    }

    Some(out)
}

/// Whether the file binds the name itself: a declaration, a local, a
/// function, or an import. A std name so bound belongs to the file.
fn doc_binds(doc: &Doc, name: &str) -> bool {
    doc.decls.iter().any(|d| d.name == name)
        || doc.bindings.iter().any(|b| b.name == name)
        || imports::bound_names(&doc.source).iter().any(|n| n == name)
}

/// The temps a named import binds a module to: `local _1 = require(...)`
/// and `_1 = require(...)` in the shadow.
fn import_temps(shadow: &str) -> Vec<String> {
    let mut temps = Vec::new();
    let mut from = 0;

    while let Some(i) = shadow[from..].find("= require(") {
        let at = from + i;
        let head = shadow[..at].trim_end();
        let name: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();

        if name.starts_with('_')
            && name[1..].chars().all(|c| c.is_ascii_digit())
            && !temps.contains(&name)
        {
            temps.push(name);
        }

        from = at + "= require(".len();
    }

    temps
}

/// The child names a module's types through the import's temp:
/// `_1.Inventory`. The temp goes, since the source knows the name.
fn strip_import_temps(value: &mut Value, shadow: &str) {
    let temps = import_temps(shadow);

    if temps.is_empty() {
        return;
    }

    fn walk(value: &mut Value, temps: &[String]) {
        match value {
            Value::String(s) => {
                for temp in temps {
                    let prefix = format!("{temp}.");

                    if s.contains(&prefix) {
                        *s = s.replace(&prefix, "");
                    }
                }
            }

            Value::Array(items) => items.iter_mut().for_each(|v| walk(v, temps)),

            Value::Object(map) => map.values_mut().for_each(|v| walk(v, temps)),

            _ => {}
        }
    }

    walk(value, &temps);
}

/// Removes the runtime's table from a type text, in every string of the
/// value: `__alloy.Future<T>` becomes `Future<T>`, and the primitive
/// helper `__alloy_string.trim` becomes `string.trim`.
fn strip_std_prefix(value: &mut Value) {
    match value {
        Value::String(s) => {
            if s.contains("__alloy") || s.contains("__mapped_") {
                let mut out = s.clone();

                for primitive in alloy::desugar::PRIMITIVES {
                    out = out.replace(&format!("__alloy_{primitive}."), &format!("{primitive}."));
                }

                *s = out
                    .replace("__alloy.", "")
                    .replace("__mapped_optional<", "Partial<")
                    .replace("__mapped_read<", "Readonly<")
                    .replace("__mapped_write<", "Sink<");
            }
        }

        Value::Array(items) => items.iter_mut().for_each(strip_std_prefix),

        Value::Object(map) => map.values_mut().for_each(strip_std_prefix),

        _ => {}
    }
}

/// The text of a hint label, whether a string or parts.
fn hint_label(hint: &Value) -> String {
    match hint.get("label") {
        Some(Value::String(s)) => s.clone(),

        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("value").and_then(Value::as_str))
            .collect(),

        _ => String::new(),
    }
}

/// A return type hint of `: Future<T>` becomes `: T`, in the label and
/// in the edit that inserts it, since `async function f(): T` is what
/// the source accepts.
fn unwrap_future_hint(hint: &mut Value) {
    fn unwrap(text: &str) -> Option<String> {
        let rest = text.strip_prefix(": ")?;
        let inner = rest
            .strip_prefix("__alloy.Future<")
            .or_else(|| rest.strip_prefix("Future<"))?
            .strip_suffix('>')?;

        Some(format!(": {inner}"))
    }

    if let Some(new) = unwrap(&hint_label(hint)) {
        hint["label"] = json!(new);
    }

    if let Some(edits) = hint.get_mut("textEdits").and_then(Value::as_array_mut) {
        for edit in edits {
            if let Some(text) = edit.get("newText").and_then(Value::as_str)
                && let Some(new) = unwrap(text)
            {
                edit["newText"] = json!(new);
            }
        }
    }
}

/// The entries a module path can continue with: the aliases of the
/// nearest `.luaurc` and `@self` when nothing is typed, the children of
/// the sourcemap under `@game/`, and otherwise the directories and the
/// modules of the resolved directory. Each is `(label, kind, detail)`.
fn module_entries(
    dir: &Path,
    root: Option<&Path>,
    head: &str,
    sourcemap: &str,
) -> Vec<(String, u64, String)> {
    let mut out = Vec::new();

    if head.is_empty() {
        out.push((
            "@self/".to_string(),
            19,
            "this file's directory".to_string(),
        ));

        for (name, target) in luaurc_aliases(dir, root) {
            out.push((
                format!("@{name}/"),
                19,
                format!("alias: {}", target.display()),
            ));
        }

        if root.is_some_and(|r| r.join(sourcemap).is_file()) {
            out.push((
                "@game/".to_string(),
                19,
                format!("the DataModel, from {sourcemap}"),
            ));
        }
    }

    if let Some(rest) = head.strip_prefix("@game/") {
        if let Some(root) = root
            && let Ok(text) = std::fs::read_to_string(root.join(sourcemap))
            && let Ok(tree) = serde_json::from_str::<Value>(&text)
        {
            let mut node = &tree;

            for part in rest.split('/').filter(|p| !p.is_empty()) {
                let Some(next) = node
                    .get("children")
                    .and_then(Value::as_array)
                    .and_then(|c| c.iter().find(|c| c["name"] == part))
                else {
                    return out;
                };
                node = next;
            }

            for child in node
                .get("children")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(name) = child["name"].as_str() {
                    let class = child["className"].as_str().unwrap_or("Instance");
                    let has_children = child
                        .get("children")
                        .and_then(Value::as_array)
                        .is_some_and(|c| !c.is_empty());
                    let label = if has_children {
                        format!("{name}/")
                    } else {
                        name.to_string()
                    };
                    out.push((label, 19, class.to_string()));
                }
            }
        }

        return out;
    }

    // A directory to list: relative, `@self`, or an alias.
    let base = if let Some(rest) = head.strip_prefix("@self/") {
        Some(imports::lexical(dir, rest))
    } else if let Some(rest) = head.strip_prefix('@') {
        let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));

        luaurc_aliases(dir, root)
            .into_iter()
            .find(|(n, _)| n == alias)
            .map(|(_, target)| imports::lexical(&target, tail))
    } else {
        Some(imports::lexical(dir, head))
    };

    let Some(base) = base else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return out;
    };
    let mut seen = HashSet::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();

        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }

        if path.is_dir() {
            if seen.insert(name.clone()) {
                out.push((format!("{name}/"), 19, "directory".to_string()));
            }

            continue;
        }

        let stem = ["d.aly", "aly", "alx", "luau", "lua"]
            .iter()
            .find_map(|ext| name.strip_suffix(&format!(".{ext}")));

        if let Some(stem) = stem
            && stem != "init"
            && seen.insert(stem.to_string())
        {
            out.push((stem.to_string(), 9, name.clone()));
        }
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));

    out
}

/// A sourcemap on its way into the mirror: every `.aly` and `.alx`
/// path becomes the mirror's `.luau`, and the runtime under the output
/// root becomes the mirror's copy under the input root.
fn mirrored_sourcemap(text: &str, input: &Path, out: Option<&Path>, root: &Path) -> String {
    let Ok(mut json) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let runtime_out = out.map(|o| rel(&o.join("alloy.luau")));
    let runtime_in = rel(&input.join("alloy.luau"));

    fn walk(v: &mut Value, f: &dyn Fn(&str) -> String) {
        match v {
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, f)),

            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    if k == "filePaths" {
                        if let Value::Array(paths) = v {
                            for p in paths.iter_mut() {
                                if let Value::String(s) = p {
                                    *s = f(s);
                                }
                            }
                        }
                    } else {
                        walk(v, f);
                    }
                }
            }

            _ => {}
        }
    }

    walk(&mut json, &|s: &str| {
        if runtime_out.as_deref() == Some(s) {
            return runtime_in.clone();
        }

        if let Some(b) = s.strip_suffix(".d.aly") {
            format!("{b}.d.luau")
        } else if let Some(b) = s.strip_suffix(".aly").or_else(|| s.strip_suffix(".alx")) {
            format!("{b}.luau")
        } else {
            s.to_string()
        }
    });

    serde_json::to_string(&json).unwrap_or_else(|_| text.to_string())
}

/// A Luau configuration file on its way into the mirror. One at the
/// workspace root that sets no language mode gets `strict`; any other
/// file copies as it is.
fn strict_config(path: &Path, root: &Path, text: String) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if path.parent() != Some(root) {
        return text;
    }

    match name {
        ".luaurc" => {
            let Ok(mut json) = serde_json::from_str::<Value>(&text) else {
                return text;
            };

            if let Some(map) = json.as_object_mut()
                && !map.contains_key("languageMode")
            {
                map.insert("languageMode".into(), Value::String("strict".into()));

                return serde_json::to_string_pretty(&json).unwrap_or(text);
            }

            text
        }

        ".config.luau" => {
            let parsed = alloy::luau_config::parse_config_luau(&text);

            if parsed.is_some_and(|c| c.language_mode.is_none())
                && let Some(i) = text.find("luau")
                && let Some(brace) = text[i..].find('{')
            {
                let at = i + brace + 1;

                return format!("{} languagemode = \"strict\",{}", &text[..at], &text[at..]);
            }

            text
        }

        _ => text,
    }
}

/// The `aliases` of the nearest `.luaurc` or `.config.luau` above
/// `dir`, up to the root, each resolved against the directory that
/// declares it.
fn luaurc_aliases(dir: &Path, root: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut cur = Some(dir.to_path_buf());

    while let Some(d) = cur {
        if let Some((_, config)) = alloy::luau_config::read_dir(&d) {
            let mut out: Vec<(String, PathBuf)> = config
                .aliases
                .iter()
                .map(|(k, p)| (k.clone(), imports::lexical(&d, p)))
                .collect();
            out.sort();

            return out;
        }

        if root.is_some_and(|r| r == d) {
            break;
        }

        cur = d.parent().map(Path::to_path_buf);
    }

    Vec::new()
}

/// The byte offset of the `{` that encloses `at`, at depth zero, when
/// the brace is on the same line or an earlier one within the statement.
fn enclosing_brace(source: &str, at: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in source[..at].char_indices().rev() {
        match c {
            '}' => depth += 1,

            '{' if depth == 0 => return Some(i),

            '{' => depth -= 1,

            _ => {}
        }
    }

    None
}

/// The child prints a std value's type as its whole shape. The shapes the
/// runtime builds read as their names instead: the Future table becomes
/// `Future<T>`, the Array metatable pair becomes `T[]`, and `Array<T>`
/// with a plain element becomes `T[]` too.
fn fold_std_shapes(value: &str) -> String {
    let mut out = value.to_string();

    // Future: `{ andThen: (self: any, on_resolve: ((T) -> ())?, ... is_settled: (self: any) -> boolean }`.
    while let Some(i) = out.find("andThen: (self: any, on_resolve: ((") {
        let Some(open) = out[..i].rfind('{') else {
            break;
        };
        let inner_start = i + "andThen: (self: any, on_resolve: ((".len();
        let Some(inner_len) = out[inner_start..].find(") -> ())?") else {
            break;
        };
        let inner = out[inner_start..inner_start + inner_len].to_string();
        let Some(settled) = out[i..].find("is_settled: (self: any) -> boolean") else {
            break;
        };
        let Some(close_rel) = out[i + settled..].find('}') else {
            break;
        };
        let close = i + settled + close_rel;
        out.replace_range(open..=close, &format!("Future<{inner}>"));
    }

    // Array, in either print: the metatable pair `t1 where t1 = { @metatable
    // t2, {T} } ; t2 = { __index: t2, concat: ...`, or the alias expanded to
    // `t1 where t1 = { [number]: T, concat: (self: t1, other: t1) -> t1, ...`.
    if let Some(i) = out.find(" where ")
        && out.contains("concat:")
    {
        let elem = if let Some(meta) = out[i..].find("{ @metatable ") {
            let elem_start = i + meta + "{ @metatable ".len();

            out[elem_start..]
                .find(',')
                .and_then(|comma| {
                    out[elem_start + comma..]
                        .find('{')
                        .map(|b| elem_start + comma + b + 1)
                })
                .and_then(|e| {
                    out[e..]
                        .find('}')
                        .map(|end| out[e..e + end].trim().to_string())
                })
        } else if let Some(k) = out[i..].find("[number]: ") {
            let elem_start = i + k + "[number]: ".len();

            out[elem_start..]
                .find(',')
                .map(|end| out[elem_start..elem_start + end].trim().to_string())
        } else {
            None
        };

        if let Some(elem) = elem {
            let type_start = out[..i]
                .rfind(char::is_whitespace)
                .map(|w| w + 1)
                .unwrap_or(0);
            let fence_end = out[i..].find("\n```").map(|f| i + f).unwrap_or(out.len());
            out.replace_range(type_start..fence_end, &format!("{elem}[]"));
        }
    }

    // A plain `Array<T>` reads as the sugar the source has.
    let mut from = 0;

    while let Some(i) = out[from..].find("Array<") {
        let start = from + i;
        let inner_start = start + "Array<".len();

        match out[inner_start..].find('>') {
            Some(n)
                if out[inner_start..inner_start + n]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '?') =>
            {
                let elem = out[inner_start..inner_start + n].to_string();
                out.replace_range(start..inner_start + n + 1, &format!("{elem}[]"));
                from = start + elem.len() + 2;
            }

            _ => from = inner_start,
        }
    }

    out
}

/// The fields of `local x = new T(...) { ... }`, under the hover of `x`.
fn append_initializer(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let mut from = 0;

    while let Some(i) = doc.source[from..].find(word) {
        let at = from + i;
        let line_start = doc.source[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
        let head = doc.source[line_start..at].trim();
        let after = &doc.source[at + word.len()..];
        let is_decl = matches!(head, "local" | "const" | "export local" | "export const")
            && !keywords::is_word_at(&doc.source, at + word.len());

        let eq = after.find('=');
        let between = eq.map(|e| after[..e].trim_start()).unwrap_or("x");

        if is_decl
            && (between.is_empty() || between.starts_with(':'))
            && let Some(eq) = eq
        {
            let rhs = after[eq + 1..].trim_start();

            if rhs.starts_with("new ")
                && let Some(open) = rhs.find('{')
                && let Some(close) = matching_brace(rhs, open)
            {
                let block = rhs[open..=close].trim();

                return Some(format!(
                    "{value}\n\nInitialized with\n```alloy\n{block}\n```"
                ));
            }

            return None;
        }

        from = at + word.len();
    }

    None
}

/// The index of the `}` that closes the `{` at `open`.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,

            '}' => {
                depth -= 1;

                if depth == 0 {
                    return Some(open + i);
                }
            }

            _ => {}
        }
    }

    None
}

/// A hover header keeps the type the source wrote: `items: Item[]`
/// instead of the child's expansion of the array. The annotation comes
/// from the first `name: T` in the source.
fn keep_annotation(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let annotation = declared_annotation(&doc.source, word)?;
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let header_end = body.find('\n').unwrap_or(body.len());
    let header = &body[..header_end];
    let colon = header.find(&format!("{word}: "))? + word.len();
    let head = &header[..colon];

    // The header names the word as `local x: T`, `x: T`, or `const x: T`.
    if head != word && !head.trim_end_matches(word).ends_with(' ') {
        return None;
    }

    Some(format!("{fence}\n{head}: {annotation}\n```{tail}"))
}

/// The type text after the first `name:` in the source: up to a `,`, a
/// `)`, an `=`, or the line's end at bracket depth zero.
fn declared_annotation(source: &str, name: &str) -> Option<String> {
    let mut from = 0;

    while let Some(i) = source[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let bounded = start
            .checked_sub(1)
            .is_none_or(|b| !keywords::is_word_at(source, b))
            && !keywords::is_word_at(source, end);
        let after = source[end..].trim_start();

        if bounded && after.starts_with(':') && !after.starts_with("::") {
            let text = after[1..].trim_start();
            let mut depth = 0i32;
            let mut stop = text.len();

            for (j, c) in text.char_indices() {
                match c {
                    '(' | '{' | '[' | '<' => depth += 1,

                    ')' | '}' | ']' | '>' if depth > 0 => depth -= 1,

                    ')' | ',' | '=' | '\n' => {
                        stop = j;

                        break;
                    }

                    _ => {}
                }
            }

            let annotation = text[..stop].trim();

            return (!annotation.is_empty()).then(|| annotation.to_string());
        }

        from = end;
    }

    None
}

/// What a built-in attribute goes on.
fn builtin_attribute_targets(key: &str) -> &'static [&'static str] {
    match key {
        "@derive" => &["struct", "enum"],

        "@test" | "@native" | "@checked" | "@deprecated" | "@inline" | "@noinline" => &["function"],

        "@unreliable" | "@ratelimit" | "@timeout" | "@immediate" | "@validate" => &["remote"],

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

/// The targets a declared attribute's hover names:
/// `**Applies to** \`field\` · \`struct\``.
fn declared_attribute_targets(hover: &str) -> Vec<&str> {
    hover
        .lines()
        .find_map(|l| l.strip_prefix("**Applies to** "))
        .map(|list| {
            list.split(" · ")
                .map(|t| t.trim().trim_matches('`'))
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The columns of the first quoted string on a line, quotes included.
fn quoted_span_on_line(source: &str, line: u32) -> Option<(u32, u32)> {
    let text = source.lines().nth(line as usize)?;
    let open = text.find(['"', '\''])?;
    let quote = text.as_bytes()[open] as char;
    let close = text[open + 1..].find(quote)? + open + 1;
    let col = |byte: usize| text[..byte].encode_utf16().count() as u32;

    Some((col(open), col(close + 1)))
}

/// Whether the child gets a document's shadow. A `.d.aly` compiles to
/// `declare` syntax, which the child reads only as a definitions file;
/// as a document it would report every line. The declarations reach it
/// through `--definitions` instead.
fn child_sees(uri: &str) -> bool {
    !uri.ends_with(".d.aly")
}

/// Drops a child diagnostic that reports the emit rather than the source:
/// a layout lint about hoisted statements, any warning whose range
/// touches generated text, or an unused-variable lint for a name that an
/// intrinsic such as `$nameof` consumed. Errors in generated text stay;
/// they map to the construct that produced them.
fn keep_diagnostic(d: &Value, doc: &Doc) -> bool {
    let message = d.get("message").and_then(Value::as_str).unwrap_or_default();

    // `--@alloy-nocheck` and `--@alloy-ignore` silence the checker too.
    // The shadow keeps the source's lines, so the line is the same.
    let silence = alloy::directives::scan(&doc.source);

    if !silence.is_empty()
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && !silence.allows(sl as usize)
    {
        return false;
    }

    if message
        .to_ascii_lowercase()
        .starts_with("samelinestatement")
    {
        return false;
    }

    let is_error = d.get("severity").and_then(Value::as_u64).unwrap_or(1) == 1;

    if is_error {
        return true;
    }

    if let Some(name) = unused_name(message)
        && consumed_by_intrinsic(&doc.source, name)
    {
        return false;
    }

    let Some(out) = &doc.output else {
        return true;
    };

    let Some(((sl, sc), (el, ec))) = d.get("range").and_then(range_of) else {
        return true;
    };

    let Some(start) = offset_of(&doc.shadow, sl, sc) else {
        return true;
    };
    let end = offset_of(&doc.shadow, el, ec).unwrap_or(doc.shadow.len());

    !(start..end.max(start + 1)).any(|o| out.map.is_generated(o as u32))
}

/// A child message as the editor should read it: a mirror path reads as
/// the real one, and an unresolved require is an `UnknownModule` error
/// over the whole import, naming the module the source asked for.
fn friendly_message(d: &mut Value, doc: &Doc, st: &State) {
    let Some(message) = d.get("message").and_then(Value::as_str) else {
        return;
    };

    // The child writes `TypeError: Unknown require: <path>`.
    if message.contains("Unknown require") {
        let line = d
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let spec = alloy::typecheck::quoted_on_line(&doc.source, line).unwrap_or_default();
        let source_rel = st
            .docs
            .iter()
            .find(|(_, other)| std::ptr::eq(*other, doc))
            .and_then(|(uri, _)| uri_to_path(uri))
            .map(|p| st.friendly_path(&p))
            .unwrap_or_default();
        d["message"] = json!(format!(
            "UnknownModule: {}",
            alloy::typecheck::unknown_module_message(&spec, Path::new(&source_rel))
        ));
        d["severity"] = json!(1);
        d["source"] = json!("Alloy");
        d["code"] = json!("3.2");

        // The whole statement, from its first word to the end of the path.
        if let Some(text) = doc.source.lines().nth(line) {
            let start = text.len() - text.trim_start().len();
            let start = text[..start].encode_utf16().count() as u32;
            let end = quoted_span_on_line(&doc.source, line as u32)
                .map(|(_, e)| e)
                .unwrap_or_else(|| text.encode_utf16().count() as u32);
            d["range"] = json!({
                "start": { "line": line, "character": start },
                "end": { "line": line, "character": end },
            });
        }

        if let Some(url) = alloy::docs::book_url("3.2") {
            d["codeDescription"] = json!({ "href": url });
        }

        return;
    }

    let mirror = st.mirror.to_string_lossy().into_owned();

    if message.contains(&mirror) {
        let outside = format!("{mirror}/_outside");
        let root = st
            .root
            .as_deref()
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rewritten = message.replace(&outside, "").replace(&mirror, &root);
        d["message"] = json!(rewritten);
    }
}

/// The payload types of a variant signature, `Msg.Move(Player, number)`
/// giving `["Player", "number"]`, split at the commas outside brackets.
fn payload_types(signature: &str) -> Vec<String> {
    let Some(open) = signature.find('(') else {
        return Vec::new();
    };
    let Some(close) = signature.rfind(')') else {
        return Vec::new();
    };
    let inner = &signature[open + 1..close];
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (i, c) in inner.char_indices() {
        match c {
            '(' | '{' | '<' | '[' => depth += 1,
            ')' | '}' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(inner[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }

    let last = inner[start..].trim();

    if !last.is_empty() {
        out.push(last.to_string());
    }

    out
}

/// The variable of a `LocalUnused` or `FunctionUnused` lint.
fn unused_name(message: &str) -> Option<&str> {
    let rest = message
        .strip_prefix("LocalUnused: Variable '")
        .or_else(|| message.strip_prefix("FunctionUnused: Function '"))?;

    rest.split('\'').next()
}

/// True when `$nameof(` or `$stringify(` names the variable in its
/// argument. The emit turns that argument into a string, so the child
/// sees no use, while the source plainly has one.
fn consumed_by_intrinsic(source: &str, name: &str) -> bool {
    for sigil in ["$nameof(", "$stringify("] {
        let mut from = 0;

        while let Some(i) = source[from..].find(sigil) {
            let start = from + i + sigil.len();
            let argument = source[start..]
                .split_once(')')
                .map(|(a, _)| a)
                .unwrap_or(&source[start..]);

            if crate::keywords::find_word(argument, name).is_some() {
                return true;
            }

            from = start;
        }
    }

    false
}

fn map_range_value(value: &mut Value, doc: &Doc) {
    if let Some(((sl, sc), (el, ec))) = range_of(value) {
        let start = doc.to_source(sl, sc);
        // The end is exclusive: map the last byte inside the range and
        // step past it, so an end in generated text does not fall back
        // to the anchor before the start.
        let end = if (el, ec) > (sl, sc) && ec > 0 {
            let (l, c) = doc.to_source(el, ec - 1);

            (l, c + 1)
        } else {
            doc.to_source(el, ec)
        };
        let end = if end < start { start } else { end };
        *value = range_value(start, end);
    }
}

/// The child's capabilities, as the editor should see them: no
/// formatting of a shadow, semantic tokens whole and never by range or
/// delta, and rename follow-up for Alloy files.
fn edit_capabilities(message: &mut Value) {
    let Some(caps) = message
        .pointer_mut("/result/capabilities")
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    for key in [
        "documentFormattingProvider",
        "documentRangeFormattingProvider",
        "documentOnTypeFormattingProvider",
    ] {
        caps.remove(key);
    }

    // The proxy formats `.aly` itself, with `alloy fmt`.
    caps.insert("documentFormattingProvider".into(), Value::Bool(true));

    // The lints' rewrites are code actions, whatever the child offers.
    let kinds = json!({ "codeActionKinds": ["quickfix", "source.fixAll"] });
    match caps.get_mut("codeActionProvider") {
        Some(Value::Object(existing)) => {
            existing.insert("codeActionKinds".into(), kinds["codeActionKinds"].clone());
        }

        _ => {
            caps.insert("codeActionProvider".into(), kinds);
        }
    }

    if let Some(Value::Object(tokens)) = caps.get_mut("semanticTokensProvider") {
        tokens.remove("range");
        tokens.insert("full".to_string(), Value::Bool(true));
    }

    // `@` and `$` open an attribute and a macro or intrinsic: the editor
    // asks on them only when the server lists them.
    if let Some(Value::Object(completion)) = caps.get_mut("completionProvider") {
        let list = completion
            .entry("triggerCharacters")
            .or_insert_with(|| json!([]));

        if let Some(chars) = list.as_array_mut() {
            for c in ["@", "$"] {
                if !chars.iter().any(|v| v == c) {
                    chars.push(json!(c));
                }
            }
        }
    }

    let workspace = caps.entry("workspace").or_insert_with(|| json!({}));

    if let Some(w) = workspace.as_object_mut() {
        w.insert(
            "fileOperations".to_string(),
            json!({
                "didRename": {
                    "filters": [
                        { "pattern": { "glob": "**/*.{aly,alx}", "matches": "file" } },
                        { "pattern": { "glob": "**", "matches": "folder" } }
                    ]
                }
            }),
        );
    }
}

fn position_of_value(v: &Value) -> Option<(u32, u32)> {
    let line = v.get("line")?.as_u64()? as u32;
    let character = v.get("character")?.as_u64()? as u32;

    Some((line, character))
}

pub fn range_of(v: &Value) -> Option<((u32, u32), (u32, u32))> {
    Some((
        position_of_value(v.get("start")?)?,
        position_of_value(v.get("end")?)?,
    ))
}

fn range_value(start: (u32, u32), end: (u32, u32)) -> Value {
    json!({
        "start": { "line": start.0, "character": start.1 },
        "end": { "line": end.0, "character": end.1 }
    })
}

fn text_document_uri(message: &Value) -> Option<String> {
    message
        .pointer("/params/textDocument/uri")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

pub fn is_alloy_uri(uri: &str) -> bool {
    uri.ends_with(".aly") || uri.ends_with(".alx")
}

/// The path of a `file:` URI, percent-decoded.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;

                continue;
            }
        }

        out.push(bytes[i]);
        i += 1;
    }

    let text = String::from_utf8(out).ok()?;

    // Windows: `file:///C:/x` carries a leading slash before the drive.
    let text = if text.len() > 2 && text.as_bytes()[0] == b'/' && text.as_bytes()[2] == b':' {
        text[1..].to_string()
    } else {
        text
    };

    Some(PathBuf::from(text))
}

/// The `file:` URI of a path, with the characters editors escape.
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");

    if !text.starts_with('/') {
        out.push('/');
    }

    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }

            _ => out.push_str(&format!("%{b:02X}")),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_temps_leave_the_type_names() {
        let shadow = "local _1 = require(\"./inventory\") local add = _1.add\n_2 = require(\"./x\")\nlocal m = require(\"./m\")\n";
        assert_eq!(import_temps(shadow), ["_1", "_2"]);
        let mut v =
            json!({ "contents": { "value": "function total(inv: _1.Inventory): _2.Item" } });
        strip_import_temps(&mut v, shadow);
        assert_eq!(
            v["contents"]["value"],
            "function total(inv: Inventory): Item"
        );
    }

    #[test]
    fn the_quoted_path_of_an_import_line() {
        let src = "import * as M from \"./inventory\"\nlocal x = 1\n";
        assert_eq!(quoted_span_on_line(src, 0), Some((19, 32)));
        assert_eq!(quoted_span_on_line(src, 1), None);
    }

    #[test]
    fn a_variant_signature_splits_into_its_payload_types() {
        assert_eq!(
            payload_types("Msg.Move(Player, number)"),
            vec!["Player", "number"]
        );
        assert_eq!(
            payload_types("Msg.Pair({ x: number, y: number }, Map<string, number>)"),
            vec!["{ x: number, y: number }", "Map<string, number>"]
        );
        assert!(payload_types("Msg.Quit").is_empty());
        assert!(payload_types("Msg.Unit()").is_empty());
    }

    #[test]
    fn a_declared_attribute_names_its_targets_in_the_hover() {
        let hover = "```alloy\n@icon(asset: string)\n```\n\n**Applies to** `struct` · `enum`";
        assert_eq!(declared_attribute_targets(hover), vec!["struct", "enum"]);
        assert!(declared_attribute_targets("```alloy\nlocal x\n```").is_empty());
    }

    #[test]
    fn unused_lints_name_the_variable() {
        assert_eq!(
            unused_name(
                "LocalUnused: Variable 'RunService' is never used; prefix with '_' to silence"
            ),
            Some("RunService")
        );
        assert_eq!(
            unused_name("FunctionUnused: Function 'f' is never used"),
            Some("f")
        );
        assert_eq!(unused_name("DeprecatedApi: Member 'x'"), None);
    }

    #[test]
    fn intrinsic_arguments_count_as_uses() {
        let src = "local RunService = game
local f = $nameof(RunService.Heartbeat)
";
        assert!(consumed_by_intrinsic(src, "RunService"));
        assert!(!consumed_by_intrinsic(src, "game"));
        assert!(consumed_by_intrinsic("$stringify(a + b)", "b"));
        assert!(!consumed_by_intrinsic("$stringify(ab)", "b"));
    }

    #[test]
    fn declaration_files_stay_out_of_the_child() {
        assert!(!child_sees("file:///w/globals.d.aly"));
        assert!(child_sees("file:///w/main.aly"));
    }

    #[test]
    fn uris_move_into_the_mirror_and_back() {
        let st = State {
            root: Some(PathBuf::from("/w")),
            mirror: PathBuf::from("/m"),
            ..State::default()
        };
        assert_eq!(st.child_uri("file:///w/a/b.aly"), "file:///m/a/b.luau");
        assert_eq!(st.child_uri("file:///w/b.d.aly"), "file:///m/b.d.luau");
        assert_eq!(st.child_uri("file:///w/ui.alx"), "file:///m/ui.luau");
        assert_eq!(st.child_uri("file:///w/x.luau"), "file:///m/x.luau");
        assert_eq!(
            st.child_uri("file:///else/x.luau"),
            "file:///m/_outside/else/x.luau"
        );
        assert_eq!(
            st.editor_uri("file:///m/x.luau"),
            ("file:///w/x.luau".to_string(), false)
        );
        assert_eq!(
            st.editor_uri("file:///m/_outside/else/x.luau"),
            ("file:///else/x.luau".to_string(), false)
        );
        let p = PathBuf::from("/a b/c.aly");
        assert_eq!(path_to_uri(&p), "file:///a%20b/c.aly");
        assert_eq!(uri_to_path("file:///a%20b/c.aly"), Some(p));
    }

    #[test]
    fn results_map_back_to_the_source() {
        let mut st = State {
            root: Some(PathBuf::from("/")),
            mirror: PathBuf::from("/m"),
            ..State::default()
        };
        let src = "local v = a ?? 0\nprint(v)\n";
        st.docs.insert(
            "file:///t.aly".to_string(),
            Doc::new(
                src.to_string(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
            ),
        );
        st.shadows
            .insert("file:///m/t.luau".to_string(), "file:///t.aly".to_string());

        let mut result = json!([{
            "uri": "file:///m/t.luau",
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
        }]);
        map_from_shadow(&mut result, None, &st);
        assert_eq!(result[0]["uri"], "file:///t.aly");
        assert_eq!(result[0]["range"]["end"]["character"], 5);

        // A range in a plain Luau file is left alone; its URI leaves the
        // mirror.
        let mut other = json!({ "uri": "file:///m/x.luau", "range": { "start": { "line": 9, "character": 9 }, "end": { "line": 9, "character": 9 } } });
        map_from_shadow(&mut other, None, &st);
        assert_eq!(other["range"]["start"]["line"], 9);
        assert_eq!(other["uri"], "file:///x.luau");
    }

    #[test]
    fn a_mirrored_sourcemap_points_at_luau() {
        let text = r#"{"name":"game","className":"DataModel","children":[{"name":"Shared","className":"ModuleScript","filePaths":["src/shared/init.aly"],"children":[{"name":"Alloy","className":"ModuleScript","filePaths":["build/alloy.luau"]}]}]}"#;
        let root = Path::new("/w");
        let out = mirrored_sourcemap(text, &root.join("src"), Some(&root.join("build")), root);
        let json: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["children"][0]["filePaths"][0], "src/shared/init.luau");
        assert_eq!(
            json["children"][0]["children"][0]["filePaths"][0],
            "src/alloy.luau"
        );
    }

    #[test]
    fn capabilities_lose_formatting_and_gain_renames() {
        let mut m = json!({ "result": { "capabilities": {
            "documentFormattingProvider": true,
            "semanticTokensProvider": { "legend": {}, "full": { "delta": true }, "range": true }
        } } });
        edit_capabilities(&mut m);
        let caps = &m["result"]["capabilities"];
        assert_eq!(caps["documentFormattingProvider"], true);
        assert_eq!(caps["semanticTokensProvider"]["full"], true);
        assert!(caps["semanticTokensProvider"].get("range").is_none());
        assert!(caps["workspace"]["fileOperations"]["didRename"].is_object());
    }
}
