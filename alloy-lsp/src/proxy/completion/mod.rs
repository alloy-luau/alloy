//! Completion: the context items, the std and keyword lists, member lists, auto-imports, and the value scope a position sees.
//!
//! Two files here carry names close to a crate module, so a bare path
//! to the real one keeps working: `std_completions.rs` is not `std.rs`
//! (it would shadow the standard library prelude), and
//! `keyword_first.rs` is not `keywords.rs` (it would shadow
//! `crate::keywords`, glob-imported into this module and every one
//! below it).

mod auto_imports;
mod context_items;
mod deprecated;
mod keyword_first;
mod members;
mod namespaces;
mod scope;
mod std_completions;

pub(crate) use context_items::MatchKind;
pub(crate) use members::{
    call_snippet, drop_receiver, hide_private, hide_record, lands_on_member, member_position,
    module_entries, payload_types, plain_snippet, private_fields, sep_of, set_call,
};
pub(crate) use namespaces::namespace_before;
pub(crate) use std_completions::complete_std_members;

use super::documents::{normalize, project_aliases};
use super::hints::writable_type;
use super::hover::{
    builtin_attribute_targets, declared_attribute_targets, remote_spec, std_receiver,
};
use super::navigation::module_file_of;
use super::*;

/// The kind words a declaration hover opens with. The list is the one
/// `alloy::declarations::summaries` writes, so a new declaration kind
/// adds its word here too.
const DECLARATION_WORDS: [&str; 8] = [
    "struct",
    "enum",
    "trait",
    "interface",
    "class",
    "namespace",
    "type",
    "declare",
];

/// The detail a project global shows: `global const MAX`, the way a
/// declaration of this file reads. The declaring file goes in the
/// documentation, where a long path costs the list nothing.
pub(crate) fn global_detail(g: &alloy::globals::Global) -> String {
    use alloy::globals::Kind;

    let word = match g.kind {
        Kind::Function => "function",

        Kind::Value if g.constant => "const",

        Kind::Value => "local",

        Kind::Struct => "struct",

        Kind::Enum => "enum",

        Kind::Trait => "trait",

        Kind::Interface => "interface",

        Kind::Class => "class",

        Kind::Type => "type",

        Kind::Remote => "remote",

        Kind::Macro => "macro",

        Kind::Attribute => "attribute",

        Kind::Impl => "impl",

        Kind::Namespace => "namespace",
    };

    format!("global {word} {}", g.name)
}

/// The detail a completion item shows for a declaration: the kind
/// first, then the name. `Test` alone says nothing about what `Test`
/// is, and the kind is what the reader is choosing between.
///
/// A function, a macro, an attribute, and a remote read as their whole
/// head instead: the parameters are what the reader writes next.
pub(crate) fn declaration_detail(hover: &str) -> Option<String> {
    let head = hover.lines().nth(1)?.trim();

    // An attribute hovers as its own use, `@icon(asset: string)`.
    if head.starts_with('@') {
        return Some(head.to_string());
    }

    let head = head.strip_suffix(" as").unwrap_or(head).trim();
    // `export` says where the name goes, not what it is. `global` says
    // both, so it stays in front of the kind.
    let head = head.strip_prefix("export ").unwrap_or(head);
    let (prefix, rest) = match head.strip_prefix("global ") {
        Some(rest) => ("global ", rest),

        None => ("", head),
    };
    let word = rest.split_whitespace().next()?;

    if matches!(
        word,
        "function" | "async" | "macro" | "attribute" | "remote" | "const" | "local"
    ) {
        return Some(format!("{prefix}{rest}"));
    }

    if !DECLARATION_WORDS.contains(&word) {
        return None;
    }

    let name: String = rest[word.len()..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    match name.is_empty() {
        true => None,

        false => Some(format!("{prefix}{word} {name}")),
    }
}

impl State {
    /// After `Msg.`, the child lists a variant as the function or the
    /// string the emit made of it. Each one becomes an enum member with
    /// the variant's signature as its detail.
    pub(crate) fn mark_enum_members(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
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

        let decls = self.decls_in_scope(uri);
        let is_enum = decls.iter().any(|d| {
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

            // The emit writes `is(v)` with no annotation, so the child
            // prints the solver's own `unknown`.
            if label == "is" {
                item["kind"] = json!(3);
                item["detail"] = json!("(any) -> boolean");
                item["documentation"] = json!({
                    "kind": "markdown",
                    "value": format!("Whether a value is a `{enum_name}`."),
                });

                continue;
            }

            if let Some(d) = decls.iter().find(|d| d.name == full) {
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

    /// A name the workspace declares completes as what it is: an
    /// attribute shows `@icon(asset: string)`, a struct `struct V`, in
    /// place of the table type the child sees.
    ///
    /// Only the declarations this file reaches: the child lists the
    /// locals the emit wrote, and a name another file keeps to itself
    /// says nothing about them.
    pub(crate) fn mark_declarations(&self, uri: &str, result: &mut Value) {
        let items = match result {
            Value::Array(items) => items,

            Value::Object(obj) => match obj.get_mut("items").and_then(Value::as_array_mut) {
                Some(items) => items,

                None => return,
            },

            _ => return,
        };

        // The emit's own names stay out of the list: the runtime local,
        // the import and temp locals, the private view tables, and the
        // raw constructor.
        items.retain(|item| {
            item["label"]
                .as_str()
                .is_none_or(|label| !is_internal_name(label))
        });

        let in_scope: HashMap<&str, &alloy::declarations::Declaration> = self
            .decls_in_scope(uri)
            .into_iter()
            .map(|d| (d.name.as_str(), d))
            .collect();

        for item in items.iter_mut() {
            let Some(label) = item["label"].as_str().map(str::to_string) else {
                continue;
            };

            if label.contains('.') || label.starts_with(['@', '$']) {
                continue;
            }

            let sigil = format!("@{label}");
            let Some(d) = in_scope
                .get(sigil.as_str())
                .or_else(|| in_scope.get(label.as_str()))
            else {
                continue;
            };
            let Some(detail) = declaration_detail(&d.hover) else {
                continue;
            };

            if d.name == sigil {
                item["kind"] = json!(21);
            }

            // A namespace is one table over a group, not a variable;
            // the child sees the local the emit wrote for it.
            if detail.starts_with("namespace ") || detail.starts_with("global namespace ") {
                item["kind"] = json!(9);
            }

            item["detail"] = json!(detail);
            item["documentation"] = json!({ "kind": "markdown", "value": d.hover });
        }
    }

    /// Signature help on a variant constructor: the child shows the
    /// emit's `_1: Player`; the variant's own shape, `Msg.Move(Player,
    /// number)`, replaces it, one parameter per payload type.
    pub(crate) fn rewrite_variant_signatures(&self, uri: &str, result: &mut Value) {
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
    pub(crate) fn primitive_completions(
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

        // `end` closes every block Alloy opens and is the likeliest word
        // at the start of a line inside one.
        let mut items = Vec::new();

        // Only at the start of a line: `end` opens no statement and
        // follows no expression, so the middle of one never wants it.
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let at_column = doc.source[line_start..offset]
            .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
            .trim()
            .is_empty();

        if at_column
            && !before.ends_with(['.', ':'])
            && before.ends_with(['\n', ' ', '\t'])
            && !labels.contains(&"end")
            && crate::block_end::open_before(&doc.source, offset)
        {
            items.push(json!({
                "label": "end",
                "kind": 14,
                "documentation": "Closes the block this line sits in.",
            }));
        }

        if !is_method && !is_static {
            return items;
        }

        items.extend(
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
                }),
        );

        items
    }

    /// The list a space opens: the sides after `--@alloy-side` and
    /// `--@alloy-file-side`, and nothing anywhere else.
    ///
    /// The editor asks on a space only when the server lists one as a
    /// trigger, so the space is in the list, and every other space
    /// answers an empty list, which opens no popup.
    pub(crate) fn side_word_completions(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };

        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let head = doc.source[line_start..offset].trim_start();
        let opens = ["--@alloy-file-side ", "--@alloy-side "].contains(&head);

        if !opens {
            return Vec::new();
        }

        alloy::directives::SIDE_WORDS
            .iter()
            .map(|(word, doc_text)| {
                json!({
                    "label": word,
                    "kind": 14,
                    "detail": "Alloy side",
                    "documentation": { "kind": "markdown", "value": doc_text },
                })
            })
            .collect()
    }

    /// Completion items for the comment directives. In a comment that
    /// holds nothing yet, `--`, `--@`, or `--!` lists them; on a line
    /// with nothing before the cursor, they come last, so a bare request
    /// at the top level finds them too. The edit replaces from the `--`
    /// to the cursor, so the typed part filters and never doubles.
    pub(crate) fn directive_completions(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };

        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let head = &doc.source[line_start..offset];
        let (edit_start, typed, last) = match head.find("--") {
            Some(i) => {
                let comment = &head[i + 2..];

                // Text past the sigil is the author's; a directive there
                // would land inside their words.
                if !comment.is_empty()
                    && !comment.starts_with('@')
                    && !comment.starts_with('!')
                    && !comment.trim().is_empty()
                {
                    return Vec::new();
                }

                (line_start + i, comment.trim_start(), false)
            }

            None if head.trim().is_empty() => (offset, "", true),

            None => return Vec::new(),
        };
        let (_, start_char) = position_of(&doc.source, edit_start);
        let alloy_only = typed.starts_with('@');
        let luau_only = typed.starts_with('!');
        let directives: [(&str, &str); 12] = [
            (
                "--@alloy-ignore",
                "Silences the next line that holds code, or this line when it sits at the end of one: the compiler's, the lints, and the checker's diagnostics. Text after the name is the reason.",
            ),
            (
                "--@alloy-ignore-start",
                "Opens a silent region, up to the matching `--@alloy-ignore-end`. A lint or a checker kind after the name limits the region to that one.",
            ),
            (
                "--@alloy-ignore-end",
                "Closes the innermost `--@alloy-ignore-start`, or the one with the same name.",
            ),
            (
                "--@alloy-expect-error",
                "Silences the next line that holds code the way `--@alloy-ignore` does, and is an error itself when that line has none. Text after the name is the reason, which comes back in that error.",
            ),
            (
                "--@alloy-nocheck",
                "Silences every diagnostic in this file.",
            ),
            (
                "--@alloy-lint",
                "Sets a lint's level for this file, over `[lint]` in alloy.toml: `--@alloy-lint raw_require=allow`. Several are separated by commas, and a group name sets its whole group.",
            ),
            (
                "--@alloy-file-side",
                "`client`, `server`, or `shared`: this file sees that side of every remote, the way a `.client.aly` or `.server.aly` name does.",
            ),
            (
                "--@alloy-side",
                "`client`, `server`, or `shared` for the `global` under it. It says nothing about the rest of the file; `--@alloy-file-side` is the one for that.",
            ),
            (
                "--@alloy-preserve",
                "`alloy flux --fix` writes no rewrite on the next line with code, or on this line when it sits at the end of one. The lint still reports.",
            ),
            (
                "--!strict",
                "The checker's strict mode for this file: every type must be known.",
            ),
            (
                "--!nonstrict",
                "The checker's nonstrict mode for this file.",
            ),
            ("--!nocheck", "The checker skips this file."),
        ];

        directives
            .iter()
            .filter(|(text, _)| {
                (!alloy_only || text.starts_with("--@")) && (!luau_only || text.starts_with("--!"))
            })
            .map(|(text, doc_text)| {
                json!({
                    "label": text,
                    "kind": 14,
                    "detail": "directive",
                    "documentation": { "kind": "markdown", "value": doc_text },
                    "filterText": text,
                    "sortText": if last { format!("zz{text}") } else { format!("0{text}") },
                    "textEdit": {
                        "range": {
                            "start": { "line": line, "character": start_char },
                            "end": { "line": line, "character": character },
                        },
                        "newText": text,
                    },
                })
            })
            .collect()
    }
}

/// The temps a named import binds a module to: `local _1 = require(...)`
/// and `_1 = require(...)` in the shadow.
pub(crate) fn import_temps(shadow: &str) -> Vec<String> {
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

        let digits = name
            .strip_prefix("_m")
            .or_else(|| name.strip_prefix('_'))
            .unwrap_or("");

        if !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
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
pub(crate) fn strip_import_temps(value: &mut Value, shadow: &str) {
    let temps = import_temps(shadow);

    if temps.is_empty() {
        return;
    }

    pub(crate) fn walk(value: &mut Value, temps: &[String]) {
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

/// A name the emit made: `__alloy`, `__alloy_string`, `_m1`, `_1`,
/// `Name__private`, `Name__all`, `__new`, and the mapped type functions.
pub fn is_internal_name(label: &str) -> bool {
    let digits_after = |prefix: &str| {
        label
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    };

    // The std splits a few types so the solver can follow them; the
    // numbered halves are names no source writes.
    const STD_HELPERS: &[&str] = &[
        "Array2",
        "Array3",
        "Iter2",
        "Iter3",
        "Result2",
        "ResultMethods",
        "ResultMethods2",
        "ResultOk",
        "ResultErr",
        "ReadArray",
        "WriteArray",
        "Awaitable",
    ];

    // Metamethods and the emit's helpers share the `__` prefix, and
    // neither is a name to complete.
    STD_HELPERS.contains(&label)
        || label.starts_with("__")
        || label == "__impl"
        || label.ends_with("__private")
        || label.ends_with("__all")
        || digits_after("_m")
        || digits_after("_")
        || digits_after("_c")
        || digits_after("_n")
}

/// Drops the names the emit made from a completion list: `__alloy`,
/// `_m1`, `Profile__all`, and the numbered halves of a std type. No
/// source writes one of them.
pub(crate) fn drop_internal_items(result: &mut Value) {
    let items = match result {
        Value::Array(v) => v,

        Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
            Some(v) => v,

            None => return,
        },

        _ => return,
    };

    items.retain(|i| {
        !i.get("label")
            .and_then(Value::as_str)
            .is_some_and(is_internal_name)
    });
}

/// Whether an item is the child's auto-import: a name no binding of the
/// file holds, offered with the `require` that would bring it in.
pub(crate) fn is_auto_import(item: &Value) -> bool {
    // The child writes `Auto-import`; the server's own items write the
    // import line they would add.
    if item
        .get("detail")
        .and_then(Value::as_str)
        .is_some_and(|d| d == "Auto-import" || d.starts_with("auto-import: "))
    {
        return true;
    }

    let doc = item
        .pointer("/documentation/value")
        .or_else(|| item.get("documentation"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    item.get("kind").and_then(Value::as_u64) == Some(9)
        && (doc.contains("= require(") || doc.contains("= game:GetService("))
}

pub(crate) fn strip_std_prefix(value: &mut Value) {
    match value {
        Value::String(s) => {
            // The compiler owns the strip, so the terminal and the
            // editor take the same names out.
            *s = alloy::typecheck::strip_std_prefix(s);
        }

        Value::Array(items) => items.iter_mut().for_each(strip_std_prefix),

        Value::Object(map) => map.values_mut().for_each(strip_std_prefix),

        _ => {}
    }
}

/// Whether the receiver of a member access is a name the scope binds:
/// a local, a parameter, a `for` variable, an arm's binding. A type's
/// own name binds none of those.
fn receiver_is_a_local(doc: &Doc, line: u32, character: u32) -> bool {
    let Some(offset) = offset_of(&doc.source, line, character) else {
        return false;
    };
    let Some((receiver, _, _, _)) = context::member_at(&doc.source, offset) else {
        return false;
    };

    if receiver.is_empty() || !receiver.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return false;
    }

    context::locals_in_scope(&doc.source, offset)
        .iter()
        .any(|l| l.name == receiver)
}

/// The list as the source can use it. A `:` names a member, so the
/// list holds members alone, each without the receiver its signature
/// carries. A detail the reader cannot write goes, and an empty
/// documentation, which opens an empty panel, goes too.
pub(crate) fn clean_completion(
    result: &mut Value,
    doc: &Doc,
    line: u32,
    character: u32,
    snippets: bool,
) {
    let items = match result {
        Value::Array(v) => v,

        Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
            Some(v) => v,

            None => return,
        },

        _ => return,
    };
    let colon = member_position(doc, line, character) == Some(':');

    if colon {
        // `new` and `from_table` take no `self`, so a colon would pass
        // the value as their first argument; the list offers what a
        // colon can call.
        items.retain(|i| {
            !matches!(
                i.get("label").and_then(Value::as_str),
                Some("new") | Some("from_table")
            )
        });
    }

    // `player.` names a value, and the constructor belongs to the
    // type: `Player.new(...)`. The emit puts `new` on the metatable, so
    // the child offers it on the value too, where it builds nothing.
    if !colon && receiver_is_a_local(doc, line, character) {
        let shapes: HashSet<&str> = doc
            .shapes
            .iter()
            .chain(doc.import_shapes.iter())
            .map(|s| s.name())
            .collect();

        items.retain(|i| {
            i.get("label").and_then(Value::as_str) != Some("new")
                || !i
                    .get("detail")
                    .and_then(Value::as_str)
                    .and_then(|d| d.rsplit("->").next())
                    .is_some_and(|ret| shapes.contains(ret.trim()))
        });
    }

    // A unit enum lowers to a union of strings. `"Playing"` is the
    // lowered form; `Phase.Playing` is what the source writes.
    let enums: HashSet<&str> = doc
        .shapes
        .iter()
        .chain(doc.import_shapes.iter())
        .filter(|s| matches!(s, alloy::declarations::Shape::Enum { .. }))
        .map(|s| s.name())
        .collect();

    items.retain(|i| {
        let quoted = i
            .get("label")
            .and_then(Value::as_str)
            .is_some_and(|l| l.starts_with('"'));

        !quoted
            || !i
                .get("detail")
                .and_then(Value::as_str)
                .is_some_and(|d| enums.contains(d))
    });

    // One row per name: the definitions file declares a few globals
    // twice, and a name the workspace also declares comes back with the
    // same text under two kinds. Two rows that read alike are one row.
    let mut seen: Vec<(String, Value, Value)> = Vec::new();

    items.retain(|i| {
        let key = (
            i.get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            i.get("detail").cloned().unwrap_or(Value::Null),
            i.get("documentation").cloned().unwrap_or(Value::Null),
        );

        match seen.contains(&key) {
            true => false,

            false => {
                seen.push(key);

                true
            }
        }
    });

    // An auto-import repeats a name the file already reads, and the two
    // rows read the same. The one already in scope stays.
    let taken: HashSet<String> = items
        .iter()
        .filter(|i| !is_auto_import(i))
        .filter_map(|i| i.get("label").and_then(Value::as_str).map(str::to_string))
        .collect();

    items.retain(|i| {
        !is_auto_import(i)
            || !i
                .get("label")
                .and_then(Value::as_str)
                .is_some_and(|l| taken.contains(l))
    });

    let private = private_fields(doc);
    let member = member_position(doc, line, character).is_some();

    for item in items.iter_mut() {
        let label = item
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // A method the child could not type is still a call: the list
        // inserts the parentheses whether the signature came or not.
        if member
            && item.get("kind").and_then(Value::as_u64) == Some(3)
            && item.get("insertText").is_none()
            && item.pointer("/textEdit/newText").is_none()
        {
            item["insertText"] = json!(format!("{label}()"));
        }

        if item.get("documentation").and_then(Value::as_str) == Some("")
            || item.pointer("/documentation/value").and_then(Value::as_str) == Some("")
        {
            item.as_object_mut().map(|o| o.remove("documentation"));
        }

        let Some(detail) = item
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };

        // `*error-type*` and a printed metatable are the solver's own
        // spellings; a popup that shows them says nothing.
        if !writable_type(&detail) {
            item.as_object_mut().map(|o| o.remove("detail"));

            continue;
        }

        // `new` and the derived table pair print every field, private
        // ones included, which names what the type hides.
        let detail = match label.as_str() {
            "new" => hide_private(&detail, &private),

            "to_table" | "from_table" => hide_record(&detail, &private),

            _ => detail,
        };
        // A colon passes the receiver, so the signature drops it.
        let detail = match colon {
            true => drop_receiver(&detail).unwrap_or(detail),

            false => detail,
        };
        item["detail"] = json!(detail);
        set_call(item, &label, &detail, snippets);
    }
}
