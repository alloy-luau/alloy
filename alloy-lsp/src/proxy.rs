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
    /// The watcher registration; the answer says nothing to act on.
    Watch,
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
    /// The ingots of the root's alloy.toml, started at workspace open
    /// and again when the file changes.
    ingots: Option<std::sync::Arc<alloy::ingot::Ingots>>,
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
    /// Whether the editor takes snippet text in a completion item.
    snippets: bool,
}

impl State {
    /// The completion items the ingots offer at a position.
    fn ingot_items(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        trigger: Option<&str>,
    ) -> Vec<Value> {
        let Some(ingots) = &self.ingots else {
            return Vec::new();
        };
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let items = ingots.complete(&path.to_string_lossy(), &doc.source, offset as u32, trigger);

        crate::ingots::completion_items(doc, &items)
    }

    /// The colors the ingots find in a document, as LSP color
    /// information.
    fn ingot_colors(&self, uri: &str) -> Vec<Value> {
        let Some(ingots) = &self.ingots else {
            return Vec::new();
        };
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let colors = ingots.colors(&path.to_string_lossy(), &doc.source);

        crate::ingots::colors(doc, &colors)
    }

    /// The code actions the ingots offer for a range.
    fn ingot_actions(&self, uri: &str, range: ((u32, u32), (u32, u32))) -> Vec<Value> {
        let Some(ingots) = &self.ingots else {
            return Vec::new();
        };
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let (Some(start), Some(end)) = (
            offset_of(&doc.source, range.0.0, range.0.1),
            offset_of(&doc.source, range.1.0, range.1.1),
        ) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let span = (start as u32, end as u32);
        let diagnostics = crate::ingots::diagnostics_in(doc, span);
        let actions = ingots.actions(&path.to_string_lossy(), &doc.source, span, &diagnostics);

        crate::ingots::actions(doc, uri, &actions)
    }

    /// The structs and enums of every open document, for the folds.
    fn known_shapes(&self) -> crate::shapes::Known {
        self.known_shapes_at(None)
    }

    /// The shapes of the workspace, with one document's own first: two
    /// structs of a shape print alike, and the file's own is the one
    /// its reader means.
    fn known_shapes_at(&self, uri: Option<&str>) -> crate::shapes::Known {
        let here = uri.and_then(|u| self.docs.get(u));
        let rest = self.docs.iter().filter(|(u, _)| Some(u.as_str()) != uri);

        crate::shapes::Known {
            shapes: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d))
                .flat_map(|d| d.shapes.iter().chain(&d.import_shapes).cloned())
                .collect(),
            interfaces: here
                .into_iter()
                .chain(rest.map(|(_, d)| d))
                .flat_map(|d| d.interfaces.iter().chain(&d.import_interfaces).cloned())
                .collect(),
        }
    }

    /// The `[lint]` table of the workspace's alloy.toml, or the defaults.
    fn lint_config(&self) -> alloy::config::LintConfig {
        self.root
            .as_deref()
            .and_then(|r| alloy::config::Config::find_within(r, r))
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

        // A compile that stopped leaves no output. Its one error is all
        // the file can say; the child reads Alloy source and reports
        // every line of it, so those reports are dropped.
        if let Some(e) = &doc.error {
            let (line, character) = position_of(&doc.source, e.offset.min(doc.source.len()));
            let end = doc
                .source
                .lines()
                .nth(line as usize)
                .map(|l| l.trim_end().encode_utf16().count() as u32)
                .unwrap_or(character + 1)
                .max(character + 1);
            diagnostics.push(json!({
                "range": {
                    "start": { "line": line, "character": character },
                    "end": { "line": line, "character": end },
                },
                "severity": 1,
                "source": "Alloy",
                "message": alloy::docs::labeled(&e.message),
            }));

            return diagnostics;
        }

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
                    "message": alloy::docs::labeled(&d.message),
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
            let directives = alloy::directives::scan(&doc.source);

            for l in &out.lints {
                // A `--@alloy-lint` in the file wins over the table.
                let level = alloy::lint::level_in(&lint_config, &directives, l.name);

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
                    "message": match &l.fix {
                        Some(f) if directives.preserves(alloy::directives::line_of(&doc.source, f.start as usize)) => {
                            format!("{}: {}\n`--@alloy-preserve` keeps this line, so `--fix` writes no rewrite here.", l.name, l.message)
                        }

                        Some(_) => format!("{}: {}\n`alloy flux --fix` rewrites it.", l.name, l.message),

                        None => format!("{}: {}", l.name, l.message),
                    },
                }));
            }
        }

        // A module that names no file, a name the module does not
        // export, and a name imported twice. The build reports these,
        // and the checker has no words for the last two, so the editor
        // reads them here.
        if doc.error.is_none()
            && let Some(path) = uri_to_path(uri)
        {
            let silence = alloy::directives::scan(&doc.source);

            let rel = PathBuf::from(self.friendly_path(&path));

            for problem in alloy::modules::import_problems_for_file(&path, Some(&rel), &doc.source)
            {
                if !silence.allows_named(
                    alloy::directives::line_of(&doc.source, problem.start as usize),
                    Some(problem.kind),
                ) {
                    continue;
                }

                let (sl, sc) = position_of(&doc.source, problem.start as usize);
                let (el, ec) = position_of(&doc.source, problem.end.max(problem.start) as usize);
                let mut item = json!({
                    "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                    "severity": 1,
                    "source": "Alloy",
                    "message": format!("{}: {}", problem.kind, problem.message),
                });

                if let Some(url) = alloy::docs::book_url("3.2") {
                    item["code"] = json!("3.2");
                    item["codeDescription"] = json!({ "href": url });
                }

                diagnostics.push(item);
            }
        }

        diagnostics
    }

    /// The quick fixes for an `impl` or a `trait` header without `as`:
    /// one per header in the range, and one that writes every header of
    /// the file. A file the parser reported on carries no lint, so this
    /// rewrite rides on the diagnostic, the way `alloy flux --fix` does.
    fn header_as_actions(&self, uri: &str, range: ((u32, u32), (u32, u32))) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let fixes = alloy::fmt::header_as_fixes(&doc.source);

        if fixes.is_empty() {
            return actions;
        }

        let ((from_line, _), (to_line, _)) = range;
        let mut all: Vec<Value> = Vec::new();

        for f in &fixes {
            let (line, at) = position_of(&doc.source, f.start as usize);
            let edit = json!({
                "range": {
                    "start": { "line": line, "character": at },
                    "end": { "line": line, "character": at },
                },
                "newText": f.replacement,
            });
            all.push(edit.clone());

            if line < from_line || line > to_line {
                continue;
            }

            let head = doc
                .output
                .as_ref()
                .map(|o| o.diagnostics.as_slice())
                .unwrap_or_default()
                .iter()
                .find(|d| {
                    d.message.ends_with(alloy::fmt::NEEDS_AS)
                        && position_of(&doc.source, d.start as usize).0 == line
                });
            let mut action = json!({
                "title": "Write `as` after the header",
                "kind": "quickfix",
                "isPreferred": true,
                "edit": { "changes": { uri: [edit] } },
            });

            if let Some(d) = head {
                let (sl, sc) = position_of(&doc.source, d.start as usize);
                action["diagnostics"] = json!([{
                    "range": {
                        "start": { "line": sl, "character": sc },
                        "end": { "line": line, "character": at },
                    },
                    "severity": 1,
                    "source": "Alloy",
                    "message": alloy::docs::labeled(&d.message),
                }]);
            }

            actions.push(action);
        }

        if all.len() > 1 {
            actions.push(json!({
                "title": format!("Write `as` after every header in this file ({})", all.len()),
                "kind": "source.fixAll",
                "edit": { "changes": { uri: all } },
            }));
        }

        actions
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
        let directives = alloy::directives::scan(&doc.source);
        let ((from_line, _), (to_line, _)) = range;
        let mut all_edits: Vec<Value> = Vec::new();

        for l in &out.lints {
            let Some(fix) = &l.fix else { continue };

            if alloy::lint::level_in(&lint_config, &directives, l.name) == alloy::lint::Level::Allow
            {
                continue;
            }

            // `--@alloy-preserve` keeps the line as the author wrote it,
            // in the editor as under `alloy flux --fix`.
            if directives.preserves(alloy::directives::line_of(&doc.source, fix.start as usize)) {
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
    fn mark_declarations(&self, result: &mut Value) {
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

        for item in items.iter_mut() {
            let Some(label) = item["label"].as_str().map(str::to_string) else {
                continue;
            };

            if label.contains('.') || label.starts_with(['@', '$']) {
                continue;
            }

            let sigil = format!("@{label}");
            let decl = self
                .docs
                .values()
                .flat_map(|d| d.decls.iter())
                .find(|d| d.name == sigil || d.name == label);
            let Some(d) = decl else { continue };
            let Some(head) = d.hover.lines().nth(1) else {
                continue;
            };

            if d.name == sigil {
                item["kind"] = json!(21);
                item["detail"] = json!(head);
                item["documentation"] = json!({ "kind": "markdown", "value": d.hover });
            } else if let Some(word) = ["struct", "enum", "trait", "interface"]
                .iter()
                .find(|w| head.contains(&format!("{w} ")))
            {
                item["detail"] = json!(format!("{word} {label}"));
                item["documentation"] = json!({ "kind": "markdown", "value": d.hover });
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

    /// Completion items for the comment directives. In a comment that
    /// holds nothing yet, `--`, `--@`, or `--!` lists them; on a line
    /// with nothing before the cursor, they come last, so a bare request
    /// at the top level finds them too. The edit replaces from the `--`
    /// to the cursor, so the typed part filters and never doubles.
    fn directive_completions(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
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
        let directives: [(&str, &str); 11] = [
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
                "--@alloy-side",
                "`client` or `server`: this file sees that side of every remote, the way a `.client.aly` or `.server.aly` name does.",
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
        // The ternary check reads the caret's own line: a `?` on an
        // earlier line says nothing about this `:`.
        let line_start = raw.rfind('\n').map_or(0, |i| i + 1);
        let line_head = &raw_head[line_start.min(raw_head.len())..];
        // `c ? a : b` ends its else with a `:` that takes a value.
        let type_slot = (raw_head.ends_with(": ")
            || raw_head.ends_with("-> ")
            || raw_head.ends_with(": read ")
            || raw_head.ends_with(": write "))
            && !raw_head.trim_end().ends_with("::")
            && !context::ternary_else(line_head);

        let labels: Vec<&str> = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array())
            .map(|items| items.iter().filter_map(|i| i["label"].as_str()).collect())
            .unwrap_or_default();

        if type_slot {
            return self.type_completions(uri, &labels);
        }

        if before.ends_with(['.', ':', '$', '@']) {
            return Vec::new();
        }

        // `local { na| } = player` names the fields of the value; a std
        // name or a keyword there is no binding the pattern can make.
        if context::in_destructure(&doc.source, offset) {
            return Vec::new();
        }

        // A position the child answers with nothing takes nothing: the
        // std names and the keywords belong where a name can begin.
        if labels.is_empty() {
            return Vec::new();
        }

        let mut items: Vec<Value> = alloy::desugar::AMBIENT
            .iter()
            .filter(|name| !labels.contains(name))
            .map(|name| {
                let kind = if matches!(*name, "Ok" | "Err") { 3 } else { 7 };

                // A std type reads with the names it carries, the way
                // its hover does.
                let doc = alloy::docs::type_markdown(name)
                    .or_else(|| crate::keywords::doc(name).map(str::to_string));

                json!({
                    "label": name,
                    "kind": kind,
                    "detail": "alloy:std",
                    "documentation": doc.map(|d| json!({ "kind": "markdown", "value": d })),
                })
            })
            .collect();

        // The Alloy keywords: the child lists Luau's own.
        items.extend(
            keywords::ALLOY_KEYWORDS
                .iter()
                .filter(|k| !labels.contains(k))
                .map(|k| {
                    let mut item = json!({
                        "label": k,
                        "kind": 14,
                        "detail": "Alloy keyword",
                        "documentation": keywords::doc(k).map(|d| json!({ "kind": "markdown", "value": d })),
                    });

                    // `case` is half an arm: the list opens again behind
                    // it for the pattern.
                    if *k == "case" {
                        item["insertText"] = json!("case ");
                        item["command"] = json!({
                            "title": "Suggest",
                            "command": "editor.action.triggerSuggest",
                        });
                    }

                    item
                }),
        );

        items
    }

    /// The names an expression at the caret may write: the locals and
    /// the parameters in scope, the declarations the file sees, the std
    /// names, and the keywords an expression takes. luau-lsp answers
    /// nothing inside an `if` expression and right before a literal,
    /// which is where an Alloy arm, a ternary, and a `default` land, so
    /// this list stands in for it there and nowhere else.
    fn value_scope(&self, uri: &str, line: u32, character: u32, result: &Value) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };

        let answered = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array())
            .is_some_and(|items| !items.is_empty());

        // The child answered: its list already holds the scope.
        if answered || !context::expression_start(&doc.source, offset) {
            return Vec::new();
        }

        let mut items = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // The locals come first: at an arm or a ternary they are what
        // the author reaches for.
        for local in context::locals_in_scope(&doc.source, offset) {
            let kind = match local.kind {
                context::LocalKind::Function => 3,
                context::LocalKind::Parameter => 6,
                context::LocalKind::Variable => 6,
            };
            let binding = doc.bindings.iter().find(|b| b.name == local.name);
            let detail = match (&local.annotation, binding) {
                (Some(a), _) => a.clone(),

                (None, Some(b)) => b.prefix.clone(),

                (None, None) => match local.kind {
                    context::LocalKind::Parameter => "parameter".to_string(),

                    _ => "local".to_string(),
                },
            };
            let mut item = json!({
                "label": local.name,
                "kind": kind,
                "detail": detail,
                "sortText": format!("0{}", local.name),
            });

            if let Some(text) = binding.and_then(|b| b.doc.clone()) {
                item["documentation"] = json!({ "kind": "markdown", "value": text });
            }

            if seen.insert(local.name.clone()) {
                items.push(item);
            }
        }

        let mut push = |name: &str, kind: u64, detail: String, doc_text: Option<String>| {
            if is_internal_name(name) || !seen.insert(name.to_string()) {
                return;
            }

            let mut item = json!({ "label": name, "kind": kind, "detail": detail });

            if let Some(d) = doc_text {
                item["documentation"] = json!({ "kind": "markdown", "value": d });
            }

            items.push(item);
        };

        // The declarations the file sees: its own, and what it imports.
        // A variant needs its enum in front, so it stays out.
        for d in self.decls_in_scope(uri) {
            if d.name.starts_with(['@', '$']) || d.name.contains('.') {
                continue;
            }

            // An interface, a trait, and a type alias name a type, not
            // a value; a variant needs its enum in front.
            let head = d.hover.lines().nth(1).unwrap_or("");
            let kind = if head.contains("struct ") || head.contains("class ") {
                7
            } else if head.contains("enum ") {
                13
            } else {
                continue;
            };
            push(&d.name, kind, "alloy".to_string(), Some(d.hover.clone()));
        }

        // A plain Luau module binds a name no declaration index holds.
        for name in imports::bound_names(&doc.source) {
            push(&name, 9, "import".to_string(), None);
        }

        for name in alloy::desugar::AMBIENT {
            let kind = if matches!(*name, "Ok" | "Err") { 3 } else { 7 };
            push(
                name,
                kind,
                "alloy:std".to_string(),
                keywords::doc(name).map(str::to_string),
            );
        }

        for name in EXPRESSION_GLOBALS {
            push(name, 6, "roblox".to_string(), None);
        }

        // The words an expression itself takes. `end`, `local`, and the
        // other statement words do not fit here.
        for name in [
            "if", "not", "new", "await", "try", "function", "true", "false", "nil",
        ] {
            push(
                name,
                14,
                "keyword".to_string(),
                keywords::doc(name).map(str::to_string),
            );
        }

        items
    }

    /// The type names for an annotation: every struct, interface, enum,
    /// trait, and type alias of the workspace, the std types, and the
    /// primitives.
    fn type_completions(&self, uri: &str, labels: &[&str]) -> Vec<Value> {
        let mut items = Vec::new();
        let mut seen: HashSet<String> = labels.iter().map(|l| l.to_string()).collect();
        let mut push = |name: &str, kind: u64, detail: &str, doc_text: Option<String>| {
            if !is_internal_name(name) && seen.insert(name.to_string()) {
                let mut item = json!({ "label": name, "kind": kind, "detail": detail });

                if let Some(d) = doc_text {
                    item["documentation"] = json!({ "kind": "markdown", "value": d });
                }

                items.push(item);
            }
        };

        for d in self.decls_in_scope(uri) {
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

        // The type parameters the file declares: `<T: Keyed>` puts `T`
        // in every type slot of that head and its body.
        if let Some(doc) = self.docs.get(uri) {
            for name in declared_type_parameters(&doc.source) {
                push(&name, 25, "type parameter", None);
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

        // The traits a bound and an `impl` take, which the std declares.
        for name in [
            "Display",
            "Debug",
            "Clone",
            "Eq",
            "PartialEq",
            "Ord",
            "Serialize",
            "Drop",
            "Deletable",
            "Add",
            "Sub",
            "Mul",
            "Div",
        ] {
            push(
                name,
                8,
                "alloy:std trait",
                keywords::doc(name).map(str::to_string),
            );
        }

        for name in [
            "string", "number", "boolean", "nil", "any", "unknown", "never", "thread", "buffer",
            "table", "vector",
        ] {
            push(name, 14, "primitive", None);
        }

        // The engine's classes and datatypes, which the child lists as
        // values alone.
        for name in alloy::roblox_classes::INSTANCE_CLASSES
            .iter()
            .chain(alloy::roblox_classes::DATATYPES)
        {
            push(name, 7, "roblox", None);
        }

        items
    }

    /// The variants of an enum, read from the one file that declares
    /// it. Another file with the same enum name has its own variants.
    fn enum_variants(&self, uri: &str, name: &str) -> Vec<&alloy::declarations::Declaration> {
        let prefix = format!("{name}.");
        let mine = self.docs.get(uri).into_iter().map(|d| (uri, d));

        mine.chain(self.docs.iter().map(|(u, d)| (u.as_str(), d)))
            .find(|(u, doc)| {
                doc.decls.iter().any(|d| {
                    d.name == name
                        && d.hover.lines().nth(1).is_some_and(|l| {
                            l.contains("enum ") && (*u == uri || l.starts_with("export "))
                        })
                })
            })
            .map(|(_, doc)| {
                doc.decls
                    .iter()
                    .filter(|d| d.name.starts_with(&prefix))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The type a `match` scrutinee has, which decides the arms. A
    /// plain name resolves from its annotation, from the variant it
    /// starts at, or from the declaration index; nothing else does.
    fn match_kind(&self, uri: &str, source: &str, offset: usize, scrutinee: &str) -> MatchKind {
        let name = scrutinee.trim();

        // Two scrutinees, a call, or an operator: the proxy reads none.
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return MatchKind::Unknown;
        }

        // `self.phase` and `props.phase`: the field of a value, whose
        // own type the owner's declaration gives.
        if let Some((owner, field)) = name.rsplit_once('.') {
            return self
                .value_type(source, offset, owner)
                .and_then(|t| self.field_type(uri, &t, field))
                .map_or(MatchKind::Unknown, |t| self.kind_of_type(uri, &t));
        }

        // `self` in an `impl` body is the type the block is for.
        if name == "self" {
            return context::impl_target(source, offset)
                .map_or(MatchKind::Unknown, |t| self.kind_of_type(uri, &t));
        }

        match context::declared(source, offset, name) {
            Some(context::Declared::Annotation(t)) => self.kind_of_type(uri, &t),

            Some(context::Declared::Init(v)) => self.kind_of_value(uri, &v),

            None => self.kind_of_name(uri, name),
        }
    }

    /// What the arms already written say the scrutinee is, when nothing
    /// else did: `case Ok(` names a `Result`, and a variant names its
    /// own enum.
    fn kind_of_arms(&self, uri: &str, source: &str, offset: usize) -> MatchKind {
        for arm in context::match_arms(source, offset) {
            if matches!(arm.as_str(), "Ok" | "Err") {
                return MatchKind::Result;
            }

            if let Some(d) = self
                .decls_in_scope(uri)
                .into_iter()
                .find(|d| d.name.ends_with(&format!(".{arm}")))
                && let Some((owner, _)) = d.name.split_once('.')
                && matches!(self.kind_of_name(uri, owner), MatchKind::Enum(_))
            {
                return MatchKind::Enum(owner.to_string());
            }
        }

        MatchKind::Unknown
    }

    /// What a type annotation names.
    fn kind_of_type(&self, uri: &str, text: &str) -> MatchKind {
        let t = text.trim().trim_end_matches('?').trim();

        if t == "Result" || t.starts_with("Result<") {
            return MatchKind::Result;
        }

        if t.ends_with("[]") || t.starts_with("Array<") {
            return MatchKind::Array;
        }

        if matches!(t, "string" | "number") {
            return MatchKind::Literal;
        }

        self.kind_of_name(uri, t.split('<').next().unwrap_or(t).trim())
    }

    /// What a declared name is, from the head line of its hover. A type
    /// alias stands for what it names.
    fn kind_of_name(&self, uri: &str, name: &str) -> MatchKind {
        let Some(d) = self
            .decls_in_scope(uri)
            .into_iter()
            .find(|d| d.name == name)
        else {
            return MatchKind::Unknown;
        };
        let head = d.hover.lines().nth(1).unwrap_or("");

        if head.contains("enum ") {
            return MatchKind::Enum(name.to_string());
        }

        if head.contains("Result<") {
            return MatchKind::Result;
        }

        if head.contains("[]") || head.contains("Array<") {
            return MatchKind::Array;
        }

        MatchKind::Unknown
    }

    /// What an initialiser says: `Msg.Join(p)` is that enum, and `Ok`
    /// or `Err` is a `Result`.
    fn kind_of_value(&self, uri: &str, text: &str) -> MatchKind {
        let t = text.trim();
        let head: String = t
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if matches!(head.as_str(), "Ok" | "Err") {
            return MatchKind::Result;
        }

        if head.is_empty() || !t[head.len()..].starts_with('.') {
            return MatchKind::Unknown;
        }

        match self.kind_of_name(uri, &head) {
            MatchKind::Enum(name) => MatchKind::Enum(name),

            _ => MatchKind::Unknown,
        }
    }

    /// The declarations a file sees: its own, and the exports of the
    /// modules it imports, under the names the `import` binds. A name no
    /// import brought in is not in scope, so a list never offers a type
    /// the file cannot write.
    fn decls_in_scope(&self, uri: &str) -> Vec<&alloy::declarations::Declaration> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let imported: HashSet<String> = self
            .docs
            .get(uri)
            .map(|d| imports::bound_names(&d.source).into_iter().collect())
            .unwrap_or_default();
        // The file's own declarations come first, so a name it declares
        // wins over the same name in another file.
        let mine = self.docs.get(uri).into_iter().map(|d| (uri, d));

        // A variant reads as `Enum.Variant`, and a sigil name as
        // `@clamp`: the enum's plain name is the one an import binds.
        let bound_name = |name: &str| {
            name.split('.')
                .next()
                .unwrap_or(name)
                .trim_start_matches(['@', '$'])
                .to_string()
        };

        for (u, doc) in mine.chain(self.docs.iter().map(|(u, d)| (u.as_str(), d))) {
            let own = u == uri;
            let exports: HashSet<String> = doc
                .decls
                .iter()
                .filter(|d| {
                    d.hover
                        .lines()
                        .nth(1)
                        .is_some_and(|l| l.starts_with("export "))
                })
                .map(|d| bound_name(&d.name))
                .collect();

            for d in &doc.decls {
                let bound = bound_name(&d.name);
                let reachable = own || (exports.contains(&bound) && imported.contains(&bound));

                if reachable && seen.insert(d.name.clone()) {
                    out.push(d);
                }
            }
        }

        out
    }

    /// The fields a struct or a record type declares, read from the
    /// declaration's hover. A private field stays out unless the caret
    /// sits in the impl of that same type.
    fn struct_fields(&self, uri: &str, name: &str, inside: bool) -> Vec<context::Field> {
        self.decls_in_scope(uri)
            .into_iter()
            .find(|d| d.name == name)
            .map(|d| context::record_entries(&d.hover))
            .unwrap_or_default()
            .into_iter()
            .filter(|f| inside || !f.private)
            .collect()
    }

    /// The type a name has at a position: its annotation, the type its
    /// first value constructs, or the declaration it names.
    fn value_type(&self, source: &str, offset: usize, name: &str) -> Option<String> {
        if name == "self" {
            return context::impl_target(source, offset);
        }

        match context::declared(source, offset, name) {
            Some(context::Declared::Annotation(t)) => Some(t),

            Some(context::Declared::Init(v)) => {
                let v = v.trim();
                let head = v.strip_prefix("new ").unwrap_or(v).trim_start();
                let word: String = head
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();

                (!word.is_empty()).then_some(word)
            }

            None => None,
        }
    }

    /// The type a field of a type holds: `self.phase` under `impl Round`
    /// reads `Phase`. The owner may be a struct, a record alias, or the
    /// record text of an inline annotation.
    fn field_type(&self, uri: &str, owner: &str, field: &str) -> Option<String> {
        let owner = owner.trim().trim_end_matches('?').trim();
        let text = match owner.starts_with('{') {
            true => owner.to_string(),

            false => {
                let name = owner.split('<').next().unwrap_or(owner).trim();

                self.decls_in_scope(uri)
                    .into_iter()
                    .find(|d| d.name == name)?
                    .hover
                    .clone()
            }
        };

        context::record_entries(&text)
            .into_iter()
            .find(|f| f.name == field)
            .map(|f| f.ty)
    }

    /// Narrows a remote's member list to what the file may reach. The
    /// emit types one surface for both sides, so `Toast.fire` is in the
    /// list of a `.client.aly` file that cannot reach it; the
    /// declaration and the file's side say which members stand.
    fn filter_remote_members(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
        let items = match result {
            Value::Array(items) => items,

            Value::Object(obj) => match obj.get_mut("items").and_then(Value::as_array_mut) {
                Some(items) => items,

                None => return,
            },

            _ => return,
        };
        let labels: HashSet<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        // Every remote carries these two; no other value in the
        // language does.
        if !(labels.contains("spec") && labels.contains("instance")) {
            return;
        }

        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return;
        };
        let Some((base, _, '.', _)) = context::member_at(&doc.source, offset) else {
            return;
        };

        if base.contains('.') {
            return;
        }

        let here = remote_spec(&doc.source, &base);
        let spec = match here {
            Some(spec) => Some(spec),

            // The declaration sits in the module the file imports it
            // from; a name no import bound is not this remote.
            None => imports::bound_names(&doc.source)
                .contains(&base)
                .then(|| {
                    self.docs
                        .values()
                        .find_map(|d| remote_spec(&d.source, &base))
                })
                .flatten(),
        };
        let Some(spec) = spec else {
            return;
        };
        let side = uri_to_path(uri)
            .map(|p| p.to_string_lossy().into_owned())
            .and_then(|name| alloy::directives::effective_side(&doc.source, &name));

        items.retain(|i| {
            i["label"]
                .as_str()
                .is_none_or(|label| spec.holds(label, side))
        });
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
        let snippets = self.snippets;
        let snippet = |label: &str,
                       insert: &str,
                       kind: u64,
                       detail: &str,
                       doc_text: Option<String>,
                       from: usize| {
            let mut item = word(label, kind, doc_text, from);
            item["detail"] = json!(detail);

            if snippets {
                item["textEdit"]["newText"] = json!(insert);
                item["insertTextFormat"] = json!(2);
            } else {
                item["textEdit"]["newText"] = json!(plain_snippet(insert));
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

                for d in self.decls_in_scope(uri) {
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

                for d in self.decls_in_scope(uri) {
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

            Context::CfgArg { prefix } => {
                let from = offset - prefix.len();
                let conditions = [
                    ("server", "RunService:IsServer()"),
                    ("client", "RunService:IsClient()"),
                    ("studio", "RunService:IsStudio()"),
                    ("edit", "RunService:IsEdit()"),
                    ("running", "RunService:IsRunning()"),
                    ("test", "an `alloy test` run"),
                ];

                for (name, what) in conditions {
                    items.push(word(
                        name,
                        21,
                        Some(format!("`@cfg({name})` holds under {what}.")),
                        from,
                    ));
                }

                for (name, what) in [
                    ("not", "the condition after it fails"),
                    ("and", "both hold"),
                    ("or", "either holds"),
                    ("any(", "any of the list holds"),
                    ("all(", "all of the list hold"),
                ] {
                    items.push(word(name, 14, Some(format!("`{name}`: {what}.")), from));
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

                let data_format = spec.as_deref().and_then(alloy::data::Format::of);

                // A data file exports no type.
                if !*type_only && data_format.is_none() {
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
                    // `@alias/x` goes through the project's aliases; a
                    // relative spec is path arithmetic.
                    let resolved = match spec.strip_prefix('@') {
                        Some(rest) => {
                            let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));

                            project_aliases(dir, self.root.as_deref())
                                .into_iter()
                                .find(|(a, _)| a == alias)
                                .map(|(_, base)| imports::lexical(&base, tail))
                        }

                        None => Some(imports::lexical(dir, spec)),
                    };
                    // A data file lists its top-level keys, each with
                    // the type its value reads as.
                    if let Some(format) = data_format {
                        if let Some(file) = resolved
                            && !*type_only
                            && let Ok(text) = std::fs::read_to_string(&file)
                            && let Ok(keys) = alloy::data::keys(&text, format)
                        {
                            for (key, ty) in keys {
                                let mut item = word(&key, 5, None, from);
                                item["detail"] = json!(ty);
                                items.push(item);
                            }
                        }

                        return items;
                    }

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

            Context::TypeSlot { prefix, prefers } => {
                let from = offset - prefix.len();

                for mut item in self.type_completions(uri, &[]) {
                    let label = item["label"].as_str().unwrap_or("").to_string();
                    let kind = item["kind"].as_u64().unwrap_or(7);
                    let doc_text = item["documentation"]["value"].as_str().map(str::to_string);
                    let detail = item["detail"].clone();
                    let rank = type_rank(*prefers, detail.as_str().unwrap_or(""));
                    item = word(&label, kind, doc_text, from);
                    item["detail"] = detail;
                    item["sortText"] = json!(format!("{rank}{label}"));
                    items.push(item);
                }
            }

            Context::NewTarget { prefix } => {
                let from = offset - prefix.len();

                for d in self.decls_in_scope(uri) {
                    let head = d.hover.lines().nth(1).unwrap_or("");

                    if head.contains("struct ") || head.contains("class ") {
                        items.push(word(&d.name, 7, Some(d.hover.clone()), from));
                    }
                }

                for name in [
                    "HashMap", "Set", "Queue", "Heap", "Scope", "Signal", "Symbol", "Array",
                ] {
                    items.push(word(name, 7, keywords::doc(name).map(str::to_string), from));
                }

                for name in alloy::roblox_classes::INSTANCE_CLASSES
                    .iter()
                    .chain(alloy::roblox_classes::DATATYPES)
                {
                    let mut item = word(name, 7, None, from);
                    item["detail"] = json!("roblox");
                    items.push(item);
                }
            }

            Context::MatchCase { prefix, scrutinee } => {
                let from = offset - prefix.len();
                let kind = scrutinee.as_deref().map_or(MatchKind::Unknown, |s| {
                    self.match_kind(uri, &doc.source, offset, s)
                });
                // Nothing named the scrutinee: the arms already written
                // still do.
                let kind = match kind {
                    MatchKind::Unknown => self.kind_of_arms(uri, &doc.source, offset),

                    other => other,
                };

                match &kind {
                    // The variants of the enum being matched, and only
                    // those: a global or a keyword is no arm.
                    MatchKind::Enum(name) => {
                        for d in self.enum_variants(uri, name) {
                            let variant = &d.name[name.len() + 1..];
                            let signature = d.hover.lines().nth(1).unwrap_or(&d.name);
                            let payload = payload_types(signature);
                            let insert = match payload.is_empty() {
                                true => variant.to_string(),

                                // One tab stop per value the variant
                                // carries, so the arity reads right.
                                false => {
                                    let slots: Vec<String> =
                                        (1..=payload.len()).map(|i| format!("${i}")).collect();

                                    format!("{variant}({})", slots.join(", "))
                                }
                            };
                            items.push(snippet(
                                variant,
                                &insert,
                                20,
                                signature,
                                Some(d.hover.clone()),
                                from,
                            ));
                        }
                    }

                    MatchKind::Result => {
                        for (label, insert, what) in [
                            ("Ok", "Ok(${1:v})", "The success case of a `Result`."),
                            ("Err", "Err(${1:e})", "The failure case of a `Result`."),
                        ] {
                            items.push(snippet(
                                label,
                                insert,
                                20,
                                &plain_snippet(insert),
                                Some(what.to_string()),
                                from,
                            ));
                        }
                    }

                    MatchKind::Array => {
                        for (label, insert, what) in [
                            (
                                "[ first, ...rest ]",
                                "[ ${1:first}, ...${2:rest} ]",
                                "An array with one item at least; `rest` takes the tail.",
                            ),
                            ("[ ]", "[ ]", "The empty array."),
                        ] {
                            items.push(snippet(
                                label,
                                insert,
                                20,
                                label,
                                Some(what.to_string()),
                                from,
                            ));
                        }
                    }

                    // A string or a number matches its own literals, and
                    // the child cannot list those.
                    MatchKind::Literal => {}

                    // Nothing named the scrutinee: the variants this
                    // file declares or imports stay, and no other.
                    MatchKind::Unknown => {
                        let mut seen = HashSet::new();

                        for d in self.decls_in_scope(uri) {
                            // A variant declares as `Enum.Variant`.
                            if let Some((_, variant)) = d.name.split_once('.')
                                && d.hover.contains("```alloy\n")
                                && seen.insert(variant.to_string())
                            {
                                items.push(word(variant, 20, Some(d.hover.clone()), from));
                            }
                        }

                        for (name, kind, what) in [
                            ("Ok", 20, "The success case of a `Result`."),
                            ("Err", 20, "The failure case of a `Result`."),
                            ("Enum", 7, "The engine's enums: `case Enum.KeyCode.W then`."),
                            ("_", 14, "Matches anything without binding it."),
                        ] {
                            if seen.insert(name.to_string()) {
                                items.push(word(name, kind, Some(what.to_string()), from));
                            }
                        }
                    }
                }

                items.push(word(
                    "default",
                    14,
                    Some("The arm that takes what no case did.".to_string()),
                    from,
                ));
            }

            Context::FieldStart { prefix } => {
                let from = offset - prefix.len();

                for (name, what) in [
                    ("read", "A read-only field."),
                    ("write", "A write-only field."),
                    ("private", "A field the impl alone sees."),
                    ("public", "A field everything sees, the default."),
                    ("end", "Closes the body."),
                ] {
                    items.push(word(name, 14, Some(what.to_string()), from));
                }
            }

            // A variant name is the author's own; the `end` closes the
            // body and is the other word this column takes.
            Context::VariantStart { prefix } => {
                items.push(word(
                    "end",
                    14,
                    Some("Closes the body.".to_string()),
                    offset - prefix.len(),
                ));
            }

            // `new Instance("|")`: the classes the engine builds.
            Context::ClassName { prefix } => {
                let from = offset - prefix.len();

                for name in alloy::luaux::roblox::creatable_classes() {
                    let mut item = word(name, 7, None, from);
                    item["detail"] = json!("Roblox class");
                    items.push(item);
                }
            }

            Context::MemberStart { prefix } => {
                let from = offset - prefix.len();

                for (name, what) in [
                    ("function", "A method; `self` first for an instance method."),
                    ("async function", "A method that returns a Future."),
                    ("private function", "A method the impl alone calls."),
                    ("public", "A method everything calls, the default."),
                    ("end", "Closes the body."),
                ] {
                    items.push(word(name, 14, Some(what.to_string()), from));
                }
            }

            // A trait declares a contract; every method in it is public,
            // so no visibility word belongs here.
            Context::TraitMemberStart { prefix } => {
                let from = offset - prefix.len();

                for (name, what) in [
                    ("function", "A method the impl must write."),
                    ("async function", "A method that returns a Future."),
                    ("end", "Closes the body."),
                ] {
                    items.push(word(name, 14, Some(what.to_string()), from));
                }
            }

            Context::StructField { prefix, target } => {
                let from = offset - prefix.len();
                let inside = context::impl_target(&doc.source, offset).as_deref() == Some(target);

                for field in self.struct_fields(uri, target, inside) {
                    let mut item = snippet(
                        &field.name,
                        &format!("{} = ${{1:{}}}", field.name, field.name),
                        5,
                        &format!("{}: {}", field.name, field.ty),
                        Some(format!("A field of `{target}`.")),
                        from,
                    );
                    item["sortText"] = json!(format!("0{}", field.name));
                    items.push(item);
                }
            }

            // `new Instance("Part") { |`: the properties and the events
            // of the class the string names.
            Context::InstanceField { prefix, class } => {
                let from = offset - prefix.len();

                for name in alloy::luaux::roblox::properties(class) {
                    let mut item = snippet(
                        name,
                        &format!("{name} = ${{1:{name}}}"),
                        5,
                        &format!("property of {class}"),
                        None,
                        from,
                    );
                    item["sortText"] = json!(format!("0{name}"));
                    items.push(item);
                }

                for name in alloy::luaux::roblox::events(class) {
                    let mut item = snippet(
                        name,
                        &format!("{name} = ${{1:handler}}"),
                        23,
                        &format!("event of {class}"),
                        None,
                        from,
                    );
                    item["sortText"] = json!(format!("1{name}"));
                    items.push(item);
                }
            }

            Context::ImportStar => {
                items.push(word(
                    "as",
                    14,
                    Some("The name the module takes here.".to_string()),
                    offset,
                ));
            }

            // A default binding took the first slot; only the braces
            // may follow the comma.
            Context::ImportBrace => {
                items.push(word(
                    "{",
                    14,
                    Some(
                        "The names the module exports: `import M, { a, b } from \"./m\"`."
                            .to_string(),
                    ),
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
                        "Opens the body: the fields of a struct, the variants of an enum, the methods of an `impl` or a `trait`."
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

                let mut seen: HashSet<String> = items
                    .iter()
                    .filter_map(|i| i["label"].as_str().map(str::to_string))
                    .collect();

                for item in self.type_completions(uri, &[]) {
                    if seen.insert(item["label"].as_str().unwrap_or("").to_string()) {
                        items.push(item);
                    }
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

                // A module never imports itself.
                let own = path.file_stem().map(|s| s.to_string_lossy().into_owned());

                for (label, kind, detail) in
                    module_entries(dir, self.root.as_deref(), head, sourcemap)
                {
                    if head.is_empty() && Some(&label) == own.as_ref() {
                        continue;
                    }

                    let mut item = word(&label, kind, None, start + cut);
                    item["detail"] = json!(detail);

                    // A sibling module resolves as `./name`; a bare name
                    // is an alias the project has to declare.
                    if head.is_empty() && !label.starts_with(['@', '.']) {
                        item["textEdit"]["newText"] = json!(format!("./{label}"));
                    }

                    items.push(item);
                }
            }
        }

        items
    }

    /// The emit options and markup config for a file, from the nearest
    /// `alloy.toml`: its `[alx]` table, or a `luaux.toml` beside it.
    fn options_for(&self, uri: &str) -> (EmitOptions, alloy::luaux::Config) {
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        // The climb stops at the workspace root: a sibling project
        // under the same parent must not lend its configuration.
        let found = match &self.root {
            Some(root) => Config::find_within(&dir, root),

            None => Config::find(&dir),
        };
        let config = found.and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let file_name = path.to_string_lossy().into_owned();
        let definitions = file_name.ends_with(".d.aly");
        let config_dir = config
            .as_ref()
            .and_then(|(p, _)| p.parent().map(Path::to_path_buf))
            .or_else(|| self.root.clone())
            .unwrap_or_else(|| dir.clone());
        let jsx = config
            .as_ref()
            .map(|(_, c)| c.markup(&config_dir))
            .unwrap_or_else(|| alloy::luaux::Config::load(&config_dir).map_err(|e| e.message))
            .unwrap_or_default();

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
                // A file in the tree sits in the sourcemap, and the child
                // resolves its requires in the DataModel tree: the runtime
                // is `../Alloy` there. A file outside the tree reaches the
                // runtime on disk.
                let source_rel = file.strip_prefix(&root).ok().map(Path::to_path_buf);
                let tree = alloy::project::Tree::load(&root, &config);
                let std_require = config.emit.std_require.clone().unwrap_or_else(|| {
                    source_rel
                        .as_deref()
                        .and_then(|rel| alloy::project::std_require_relative_for(&tree, rel))
                        .unwrap_or_else(|| {
                            if depth == 0 {
                                "./alloy".to_string()
                            } else {
                                format!("{}alloy", "../".repeat(depth))
                            }
                        })
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
                    match c {
                        // Windows: `C:` becomes one folder name, so the
                        // drive survives the trip through the mirror
                        // and `real_path` writes it back.
                        std::path::Component::Prefix(prefix) => {
                            let text = prefix.as_os_str().to_string_lossy();
                            let text = text.trim_end_matches([':', '/', '\\']);

                            if !text.is_empty() {
                                p.push(text);
                            }
                        }

                        std::path::Component::Normal(n) => p.push(n),

                        _ => {}
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
            return Some(outside_path(outside));
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
            .map(|p| path_to_uri(&data_source_of(p)))
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

    /// Writes a mirror file, creating its directories. A data file also
    /// writes the module the build makes of it, `x.json` as `x.luau`.
    fn write_mirror(&self, real: &Path, text: &str) {
        let target = self.mirror_path(real);

        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if std::fs::read_to_string(&target).ok().as_deref() != Some(text) {
            let _ = std::fs::write(&target, text);
        }

        if let Some(format) = alloy::data::Format::of_path(real)
            && !alloy::data::is_project_file(real)
        {
            self.mirror_data(real, text, format);
        }
    }

    /// The module of a data file, into the mirror. A module of the same
    /// stem beside it wins, as it does in the build. A document that
    /// does not parse keeps the last good module: a half-typed edit
    /// would drop every type at once.
    fn mirror_data(&self, real: &Path, text: &str, format: alloy::data::Format) {
        if alloy::data::module_beside(real).is_some() {
            return;
        }

        if let Ok(luau) = alloy::data::convert(text, format) {
            self.write_mirror(&real.with_extension("luau"), &luau);
        }
    }

    fn remove_mirror(&self, real: &Path) {
        let _ = std::fs::remove_file(self.mirror_path(real));

        if alloy::data::Format::of_path(real).is_some()
            && alloy::data::module_beside(real).is_none()
        {
            let _ = std::fs::remove_file(self.mirror_path(&real.with_extension("luau")));
        }
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

        let ingots = self.state.lock().expect("state").ingots.clone();
        let formatted = alloy::fmt::format(&source).map(|f| match (&ingots, uri_to_path(uri)) {
            (Some(ingots), Some(path)) => ingots.format(&path.to_string_lossy(), &f).0,

            _ => f,
        });

        match formatted {
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
                st.snippets = message
                    .pointer(
                        "/params/capabilities/textDocument/completion/completionItem/snippetSupport",
                    )
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mirror_uri = path_to_uri(&st.mirror);
                let mirror_path = st.mirror.to_string_lossy().into_owned();

                if let Some(options) = message.pointer("/params/initializationOptions") {
                    let over = settings::from_editor(options);
                    settings::merge(&mut st.settings, &over);
                }

                let mounts = mount_alias_settings(st.root.as_deref());
                settings::merge(&mut st.settings, &mounts);

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
                self.watch_data_files();
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

                let mounts = mount_alias_settings(st.root.as_deref());
                settings::merge(&mut st.settings, &mounts);

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
                // The child re-reads a `.luau` it hears about; a data
                // file's module joins the list under the module's name.
                let mut modules: Vec<Value> = Vec::new();
                let mut data_changed: Vec<PathBuf> = Vec::new();

                for change in &changes {
                    let uri = change
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let kind = change.get("type").and_then(Value::as_i64).unwrap_or(2);

                    if uri.ends_with("/alloy.toml") {
                        self.load_ingots();
                    }

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

                            if let Some(module) = data_module_of(&path) {
                                modules.push(json!({ "uri": path_to_uri(&module), "type": kind }));
                                data_changed.push(path);
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

                if let Some(list) = message
                    .pointer_mut("/params/changes")
                    .and_then(Value::as_array_mut)
                {
                    list.extend(modules);
                }

                self.forward_plain(message);

                for path in &data_changed {
                    self.refresh_dependents(path);
                }
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

            // A picked color: an ingot that colored the range names it;
            // else the child's `Color3` forms.
            Some("textDocument/colorPresentation") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.ingot_presentation(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, method.as_deref());
            }

            Some(m @ ("textDocument/hover" | "textDocument/completion")) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                // An ingot's hover comes first: a class in a `.alx` string
                // is the ingot's, not the markup's. Its completion items
                // likewise, where it has any.
                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && self.ingot_hover(&uri, &message, &id)
                {
                    return true;
                }

                // A closing quote asks for nothing: the editor sends the
                // quote as a trigger either way, and a list that pops up
                // there takes the next Enter.
                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && (self.closes_a_string(&uri, &message)
                        || self.names_a_declaration(&uri, &message))
                {
                    self.respond(&id, json!([]));

                    return true;
                }

                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && self.ingot_completion(&uri, &message, &id)
                {
                    return true;
                }

                if uri.ends_with(".alx")
                    && let Some(id) = message.get("id").cloned()
                    && self.markup_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && (self.case_binding_hover(&uri, &message, &id)
                        || self.field_hover(&uri, &message, &id)
                        || self.source_binding_hover(&uri, &message, &id)
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

                // `bx?.`, `p!.`, `await X.`: the lowering owns the member.
                if m == "textDocument/completion"
                    && let Some(home) = self.member_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // `Ok(v)` lowers to `__alloy.Ok(v)`: the caret maps past
                // a `.` the source never wrote.
                if m == "textDocument/completion"
                    && let Some(home) = self.expression_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // A binding the desugar moved, `if local c = ...`, maps
                // to a byte the child knows nothing about. The name has
                // a home in the shadow; the child answers there.
                if m == "textDocument/hover"
                    && let Some(home) = self.hover_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

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
                    Some(Asked::Watch) => {}

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

        let module = uri_to_path(uri).and_then(|path| {
            st.write_mirror(&path, &text);

            data_module_of(&path)
        });

        st.plain.insert(uri.to_string(), text);
        drop(st);

        // The child re-reads the module once it hears the module changed.
        if let Some(module) = module {
            self.forward_plain(json!({
                "jsonrpc": "2.0",
                "method": "workspace/didChangeWatchedFiles",
                "params": { "changes": [{ "uri": path_to_uri(&module), "type": 2 }] }
            }));

            if let Some(path) = uri_to_path(uri) {
                self.refresh_dependents(&path);
            }
        }
    }

    /// Resends every open document that names a data file, so the child
    /// checks it against the module it just re-read. A dirty dependency
    /// alone leaves the next completion on the document empty.
    fn refresh_dependents(&self, data: &Path) {
        let data = normalize(data);
        let messages: Vec<Value> = {
            let st = self.state.lock().expect("state");

            st.docs
                .iter()
                .filter_map(|(uri, doc)| {
                    let path = uri_to_path(uri)?;
                    let dir = path.parent()?;
                    let names = alloy::data::references(&doc.source)
                        .iter()
                        .any(|r| normalize(&imports::lexical(dir, &r.path)) == data);

                    if !names || !child_sees(uri) {
                        return None;
                    }

                    Some(json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didChange",
                        "params": {
                            "textDocument": { "uri": st.child_uri(uri), "version": doc.version },
                            "contentChanges": [{ "text": doc.shadow }]
                        }
                    }))
                })
                .collect()
        };

        for message in messages {
            self.to_child(&message);
        }
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
            // A markup hole that opens with a keyword lowers to the
            // keyword itself, where the child completes nothing. The
            // expression after it is the same scope, and the editor
            // still inserts where the caret is.
            if method == Some("textDocument/completion")
                && ctx.ends_with(".alx")
                && let Some((l, c)) = position
                && let Some(at) = offset_of(&doc.source, l, c)
                && let Some(moved) = markup::hole_expression_start(&doc.source, at)
            {
                let (ml, mc) = position_of(&doc.source, moved);
                params["position"] = json!({ "line": ml, "character": mc });
            }

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
    /// The shadow position of the word a hover asks about, when the
    /// mapped position lands off it. `None` when the map is already
    /// right, or the cursor is on no word.
    fn hover_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = message
            .pointer("/params/position")
            .and_then(position_of_value)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;

        if !keywords::is_word_at(&doc.source, offset) {
            return None;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];
        let (sl, sc) = doc.to_shadow(line, character);
        let landed = doc
            .shadow
            .lines()
            .nth(sl as usize)
            .and_then(|l| offset_of(l, 0, sc).map(|b| (l, b)))
            .is_some_and(|(l, b)| {
                keywords::is_word_at(l, b) && {
                    let (ws, we) = keywords::word_range(l, b);

                    &l[ws..we] == word
                }
            });

        match landed {
            true => None,

            false => shadow_home(&doc.shadow, line, word),
        }
    }

    /// The shadow position a member completion belongs at. `a?.b` and
    /// `a!.b` lower to text the compiler wrote, and `await X.m()` moves
    /// the receiver into a call, so the member the author is typing maps
    /// nowhere; the member the lowering wrote is the one with a type
    /// behind it. A plain access the child already reads keeps its own
    /// position.
    fn member_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = message
            .pointer("/params/position")
            .and_then(position_of_value)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;

        // A `.alx` lowering moves columns of its own; leave it alone.
        if doc.is_alx {
            return None;
        }

        let offset = offset_of(&doc.source, line, character)?;
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let source_line = doc.source.lines().nth(line as usize)?;
        // A field default moves to the line the struct's header takes,
        // so the member's line is the one the map reports, not this one.
        let (shadow_line_no, _) = doc.to_shadow(line, character);
        let shadow_line = doc.shadow.lines().nth(shadow_line_no as usize)?;
        let Some((base, access, sep, prefix)) = context::member_at(&doc.source, offset) else {
            // A call before the guard, `f(x)?:m()`: the lowering binds
            // the value to a name of its own, so no receiver of the
            // source is on the lowered line.
            let column = context::guarded_member_column(
                &doc.source[line_start..offset],
                shadow_line,
                sep_of(&doc.source, offset)?,
            )?;

            return Some((shadow_line_no, shadow_line[..column].chars().count() as u32));
        };

        if access == context::Access::Plain
            && lands_on_member(doc, line, character, &base, sep, prefix)
        {
            return None;
        }

        let column = context::member_column(
            source_line,
            shadow_line,
            &base,
            access,
            sep,
            prefix,
            offset - line_start,
        )?;

        Some((shadow_line_no, shadow_line[..column].chars().count() as u32))
    }

    /// The shadow position an expression completion belongs at. The emit
    /// qualifies a std name, `Ok(v)` to `__alloy.Ok(v)`, so a caret at
    /// that name maps past a `.` the source never wrote and the child
    /// lists the runtime's own table. The scope answers where the
    /// qualifier starts.
    fn expression_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = message
            .pointer("/params/position")
            .and_then(position_of_value)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;

        if doc.is_alx || member_position(doc, line, character).is_some() {
            return None;
        }

        let (sl, sc) = doc.to_shadow(line, character);
        let text = doc.shadow.lines().nth(sl as usize)?;
        let at = offset_of(text, 0, sc)?;
        // The map lands on the qualified name, at either end of it.
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let head = text[..at.min(text.len())].trim_end_matches(word);
        let qualifier = head.strip_suffix('.')?;
        let start = qualifier.trim_end_matches(word).len();

        // Only the runtime's own table: a temporary the desugar bound,
        // `_1`, stands for an expression the source did write.
        qualifier[start..]
            .starts_with("__alloy")
            .then(|| (sl, text[..start].chars().count() as u32))
    }

    /// The hover of a name the source declares and the child sees only
    /// as a table: a `remote`, and the namespace an `import * as` binds.
    fn source_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        let word = doc.source[start..end].to_string();
        let path = uri_to_path(uri);
        // A `remote` or an exported `const` the file imported is
        // declared somewhere else; the child reads the emitted local
        // and calls it a `local`.
        let imported = || {
            doc.import_sources
                .iter()
                .find_map(|text| remote_hover(text, &word).or_else(|| const_hover(text, &word)))
        };
        let answer = remote_hover(&doc.source, &word)
            .or_else(imported)
            .or_else(|| {
                let dir = path
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new("."))
                    .to_path_buf();
                let aliases = project_aliases(&dir, st.root.as_deref());

                module_hover(&doc.source, &word, path.as_deref(), &aliases)
            });

        let Some(answer) = answer else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": answer },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

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

        // An attribute or a macro is keyed by its sigil; a bare name that
        // an import bound finds it that way.
        let sigils = [format!("@{key}"), format!("${key}")];
        let lookup = |name: &str| {
            doc.decls.iter().find(|d| d.name == name).or_else(|| {
                st.docs
                    .values()
                    .flat_map(|d| d.decls.iter())
                    .find(|d| d.name == name)
            })
        };
        let found = lookup(&key).or_else(|| sigils.iter().find_map(|k| lookup(k)));

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

    /// A `case` pattern's binding hovers as the payload it names. A
    /// match lowers to one expression, so the binding has no local of
    /// its own and the child answers the arm's result instead.
    fn case_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        let word = doc.source[start..end].to_string();
        let known = st.known_shapes_at(Some(uri));

        let Some(answer) = case_binding_text(doc, line as usize, start, &word, &known) else {
            return false;
        };
        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": answer },
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

        // A field where it is declared, `read hp: number = 1` in a struct
        // body: the child sees the constructor's table, where a default
        // makes the field optional, so the declaration answers itself.
        if let Some(answer) = declared_field_hover(doc, start, end)
            .or_else(|| remote_parameter_hover(doc, start, end))
            .or_else(|| declared_parameter_hover(doc, start, end))
            .or_else(|| foreign_method_hover(doc, start, end))
            .or_else(|| type_parameter_hover(doc, start, end))
        {
            let (sl, sc) = position_of(&doc.source, start);
            let (el, ec) = position_of(&doc.source, end);
            let result = json!({
                "contents": { "kind": "markdown", "value": answer },
                "range": {
                    "start": { "line": sl, "character": sc },
                    "end": { "line": el, "character": ec }
                }
            });
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        // The key sits before `=` inside the braces of `Name { ... }`.
        if !doc.source[end..].trim_start().starts_with('=')
            || doc.source[end..].trim_start().starts_with("==")
        {
            return false;
        }

        let Some(open) = enclosing_brace(&doc.source, start) else {
            return false;
        };
        // `new Slotted<<T>> { value = ... }`: the arguments stand between
        // the name and the brace.
        let head = doc.source[..open].trim_end();
        let head = head.strip_suffix(">>").map_or(head, |h| {
            h.rfind("<<").map_or(head, |i| doc.source[..i].trim_end())
        });

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
                    .find(|l| field_key(l) == Some(word))
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

        // A data import: the path and each name open the data file, at
        // the line that defines the key when the file has it.
        if let Some(result) = uri_to_path(uri)
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .and_then(|dir| data_definition(&doc.source, offset, &dir))
        {
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

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

        // Inside a comment, `@` opens a directive, not an attribute.
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);

        if doc.source[line_start..offset].contains("--") {
            let items = st.directive_completions(uri, line, character);
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": items }));

            return true;
        }

        let trigger = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str);

        // The child lists a newline as a trigger for its `end`
        // completion. That request is the child's alone: a context list
        // answered here would open on every Enter, and the next Enter
        // would accept its first item.
        if trigger == Some("\n") {
            return false;
        }

        let Some(ctx) = context::detect(&doc.source, offset) else {
            // `(` opens an attribute's argument list; anywhere else the
            // editor asked on it for nothing, and the child would list
            // globals.
            if trigger == Some("(") {
                drop(st);
                self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": [] }));

                return true;
            }

            return false;
        };

        let mut items = st.context_items(uri, offset, &ctx);
        items.extend(st.ingot_items(uri, line, character, trigger));
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

            // A `:` opens a type far more often than it closes a
            // ternary; the `?` is what makes it the else.
            if word == ":" && !ternary_else_at(&doc.source, *start) {
                return false;
            }

            !(is_std && doc_binds(doc, word))
        });

        let result = match hit {
            Some((start, end, text)) => {
                let (sl, sc) = position_of(&doc.source, start);
                let (el, ec) = position_of(&doc.source, end);
                // A std type: the overview, then the names a reader can
                // hover on their own.
                let text = alloy::docs::type_markdown(&doc.source[start..end])
                    .unwrap_or_else(|| text.to_string());

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
                Some(spot) => Value::Array(markup::completions(&spot, &bound, &doc.source)),

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
                        let doc_path = uri_to_path(&source);
                        let mapped: Vec<Value> = match st.docs.get(&source) {
                            Some(doc) => {
                                let mut out: Vec<Value> = Vec::new();

                                let lint_config = st.lint_config();
                                let unmet = unmet_expectations(doc, &diagnostics);

                                for mut d in diagnostics {
                                    if !keep_diagnostic(&d, doc, doc_path.as_deref(), &lint_config)
                                    {
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

                                out.extend(unmet);
                                collapse_diagnostics(&mut out);

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
                            text = crate::shapes::fold(&text, &st.known_shapes_at(ctx.as_deref()));

                            if let Some(written) = declared_signature(&text, doc, line, character) {
                                text = written;
                            }

                            if let Some(named) = name_trait_method(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = name_method_receiver(&text, doc, line) {
                                text = named;
                            }

                            if let Some(named) = restore_struct_arguments(&text, doc, line) {
                                text = named;
                            }

                            if let Some(dropped) = drop_bound_intersections(&text, doc) {
                                text = dropped;
                            }

                            if let Some(named) = name_by_declaration(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = unlocal_parameter(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = name_solver_variable(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = prefer_constructed_struct(
                                &text,
                                doc,
                                line,
                                character,
                                &st.known_shapes(),
                            ) {
                                text = named;
                            }

                            if let Some(with_init) = append_initializer(&text, doc, line, character)
                            {
                                text = with_init;
                            }

                            // A component's factory returns whatever the
                            // configured `create` gives; the checker has
                            // no type for it and prints its own marker.
                            text = text.replace("): *error-type*", ")");
                            text = text.replace("*error-type*", "unknown");

                            // One grammar over a file: the Alloy fence
                            // highlights `const`, `async`, and `export`,
                            // which the Luau one drops.
                            text = text.replace("```luau", "```alloy");

                            // `type Player = Player` restates the token
                            // under the cursor and says nothing, and
                            // `string (5 bytes)` measures the key the
                            // emit wrote, not the name the source has.
                            let says_nothing = restates_itself(&text)
                                || (is_byte_count(&text) && names_a_key(doc, line, character));

                            // A std member: the type above, then what
                            // the member does and an example, which no
                            // type carries.
                            let member = std_member_hover(&text, doc, line, character);

                            if let Some(section) = &member {
                                text.push_str("\n\n");
                                text.push_str(section);
                            }

                            if says_nothing && member.is_none() {
                                *result = Value::Null;
                            } else if text != value {
                                result["contents"]["value"] = json!(text);
                            }
                        }
                    }

                    // A pulled report gets the filter the push path has. The
                    // messages wait for the mapping below: a range set here
                    // in source terms would map once more, to one byte.
                    "textDocument/diagnostic" => {
                        if let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) {
                            let lint_config = st.lint_config();
                            let doc_path = uri_to_path(uri);
                            let unmet = unmet_expectations(doc, items);
                            items.retain(|d| {
                                keep_diagnostic(d, doc, doc_path.as_deref(), &lint_config)
                            });
                            items.extend(unmet);
                            collapse_diagnostics(items);
                        }
                    }

                    // A `Color3` the desugar or an ingot wrote has no place in
                    // the author's text to put a swatch on.
                    "textDocument/documentColor" => {
                        if let Some(colors) = result.as_array_mut() {
                            colors.retain(|c| {
                                c.pointer("/range/start")
                                    .and_then(position_of_value)
                                    .is_none_or(|(l, ch)| !doc.generated_at(l, ch))
                            });
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
                                let offset = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, c)| offset_of(&doc.shadow, l, c));
                                let generated = offset
                                    .is_some_and(|o| doc.generated_offset(o.saturating_sub(1)));
                                // A parameter hint on a call the lowering
                                // wrote, `create("TextLabel")` behind a tag,
                                // lands on the tag: the byte before it is
                                // not the author's.
                                let lowered_call = h.get("kind").and_then(Value::as_u64) == Some(2)
                                    && offset.is_some_and(|o| doc.lowering_differs_before(o));

                                !error_type && !generated && !lowered_call
                            });

                            // A label the child sends in parts folds as
                            // one text, the way its edit does; the parts'
                            // locations point into the emit anyway.
                            for h in hints.iter_mut() {
                                if h.get("label").is_some_and(Value::is_array) {
                                    h["label"] = json!(hint_label(h));
                                }
                            }

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
                    let source = report
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(|shadow| st.shadows.get(shadow))
                        .cloned();
                    let doc = source.as_ref().and_then(|source| st.docs.get(source));

                    if let Some(doc) = doc
                        && let Some(items) = report.get_mut("items").and_then(Value::as_array_mut)
                    {
                        let lint_config = st.lint_config();
                        let doc_path = source.as_deref().and_then(uri_to_path);
                        items
                            .retain(|d| keep_diagnostic(d, doc, doc_path.as_deref(), &lint_config));
                    }
                }
            }

            map_from_shadow(result, ctx.as_deref(), &st);

            if method == "textDocument/diagnostic"
                && let Some(uri) = ctx.as_deref()
                && let Some(doc) = st.docs.get(uri)
                && let Some(items) = result.get_mut("items").and_then(Value::as_array_mut)
            {
                for d in items.iter_mut() {
                    friendly_message(d, doc, &st);
                }

                // Alloy's own reports travel by push alone: a push
                // overwrites the set an earlier server left on the file,
                // and a pull that repeated them would show each twice.

                // A rewrite may move two reports onto one line, the
                // `impl` an alias names among them; they collapse after
                // it, not before.
                collapse_diagnostics(items);
                snap_ranges(items, &doc.source);
            }

            // The ingots' colors join the child's `Color3` swatches; they
            // speak source positions already, so they join after the map.
            if method == "textDocument/documentColor"
                && let Some(uri) = &ctx
            {
                let mut extra = st.ingot_colors(uri);

                // A `Color3` call the child already colors gets one square.
                if let Value::Array(items) = result {
                    extra.retain(|e| !items.iter().any(|i| i.get("range") == e.get("range")));
                }

                if !extra.is_empty() {
                    match result {
                        Value::Array(items) => items.extend(extra),

                        Value::Null => *result = Value::Array(extra),

                        _ => {}
                    }
                }
            }

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
                // Before the rewrite: `__alloy_string` reads `oy_string`
                // once `__all` goes, and no list should carry it at all.
                if method == "textDocument/completion" {
                    drop_internal_items(result);
                }

                strip_std_prefix(result);
                // The checker prints a struct as its runtime table and a
                // unit enum as a union of strings; the names go back.
                crate::shapes::fold_value(result, &st.known_shapes_at(ctx.as_deref()));

                // A type hint inserts its edit on a click: the label shows
                // that text, so the two never differ.
                if method == "textDocument/inlayHint"
                    && let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u))
                    && let Some(hints) = result.as_array_mut()
                {
                    clean_hints(hints, doc);
                }

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
                        actions.extend(st.header_as_actions(uri, range));
                        actions.extend(st.lint_actions(uri, range));
                        actions.extend(st.ingot_actions(uri, range));
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

                            // Alloy's own reports travel by push alone.
                            if let Some(uri) = uri
                                && st.docs.contains_key(&uri)
                                && let Some(items) =
                                    report.get_mut("items").and_then(Value::as_array_mut)
                            {
                                collapse_diagnostics(items);
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
                        st.mark_declarations(result);

                        if let Some(doc) = st.docs.get(uri) {
                            attach_std_member_docs(result, doc, line, character);
                        }

                        let member = st
                            .docs
                            .get(uri)
                            .is_some_and(|d| member_position(d, line, character).is_some());
                        // Inside a string the child answers alone: it
                        // knows the class names `Instance.new("` and
                        // `GetService("` take, and nothing else belongs
                        // between quotes.
                        let quoted = st
                            .docs
                            .get(uri)
                            .and_then(|d| offset_of(&d.source, line, character))
                            .zip(st.docs.get(uri))
                            .is_some_and(|(at, d)| context::in_string(&d.source, at));
                        // A member list names what the value has; an
                        // auto-import is a new name, which cannot follow
                        // a `.` or a `:`.
                        let mut extra = match member || quoted {
                            true => Vec::new(),

                            false => st.auto_imports(uri, line, character),
                        };

                        if !quoted {
                            extra.extend(st.primitive_completions(uri, line, character, result));
                            extra.extend(st.std_completions(uri, line, character, result));
                            extra.extend(st.value_scope(uri, line, character, result));
                            extra.extend(st.directive_completions(uri, line, character));
                        }

                        extra.extend(st.ingot_items(uri, line, character, trigger.as_deref()));

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

                        if let Some(doc) = st.docs.get(uri) {
                            clean_completion(result, doc, line, character, st.snippets);
                        }

                        st.filter_remote_members(uri, line, character, result);
                        // After the clean: the spec `@pkg/react` is the
                        // detail, and the clean drops a detail that
                        // spells no type.
                        st.rewrite_child_auto_imports(uri, result);
                        st.keyword_first(uri, line, character, result);
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
        let (mut options, jsx, ingots) = {
            let st = self.state.lock().expect("state");
            let (o, j) = st.options_for(uri);

            (o, j, st.ingots.clone())
        };

        // A value import of a struct or an enum binds its type too.
        if let Some(path) = uri_to_path(uri) {
            options.import_types = alloy::modules::import_types_for_file(&path, &text);
            options.import_enums = alloy::modules::import_enums_for_file(&path, &text);
            options.import_privates = alloy::modules::import_privates_for_file(&path, &text);
            options.import_result_asyncs =
                alloy::modules::import_result_asyncs_for_file(&path, &text);
            options.import_trait_defaults =
                alloy::modules::import_trait_defaults_for_file(&path, &text);
        }

        let doc = Doc::new(text, version, &options, &jsx, ingots.as_deref());
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
        let (mut options, jsx) = self.state.lock().expect("state").options_for(uri);
        let mut st = self.state.lock().expect("state");
        let ingots = st.ingots.clone();

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

        if let Some(path) = uri_to_path(uri) {
            options.import_types = alloy::modules::import_types_for_file(&path, &doc.source);
            options.import_enums = alloy::modules::import_enums_for_file(&path, &doc.source);
            options.import_privates = alloy::modules::import_privates_for_file(&path, &doc.source);
            options.import_result_asyncs =
                alloy::modules::import_result_asyncs_for_file(&path, &doc.source);
            options.import_trait_defaults =
                alloy::modules::import_trait_defaults_for_file(&path, &doc.source);
        }

        doc.compile(&options, &jsx, ingots.as_deref());
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

        collapse_diagnostics(&mut diagnostics);

        if let Some(doc) = st.docs.get(uri) {
            snap_ranges(&mut diagnostics, &doc.source);
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
    /// Starts the ingots of the root's alloy.toml, replacing any that
    /// run. A problem with one is a warning in the editor; the others
    /// still load.
    fn load_ingots(&self) {
        let root = self.state.lock().expect("state").root.clone();
        let Some(root) = root else {
            return;
        };
        let config =
            Config::find_within(&root, &root).and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let Some((path, config)) = config else {
            self.state.lock().expect("state").ingots = None;

            return;
        };

        if config.ingots.is_empty() {
            self.state.lock().expect("state").ingots = None;

            return;
        }

        let base = path.parent().unwrap_or(&root);
        let ingots = alloy::ingot::Ingots::load(base, &config);

        for p in &ingots.problems {
            log::warn(&p.to_string());
            self.to_client(&json!({
                "jsonrpc": "2.0",
                "method": "window/showMessage",
                "params": { "type": 2, "message": p.to_string() },
            }));
        }

        log::info(&format!("{} ingots running", ingots.list.len()));
        self.state.lock().expect("state").ingots = Some(std::sync::Arc::new(ingots));
    }

    /// A hover an ingot answers; false when none does.
    fn ingot_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        let Some(ingots) = st.ingots.clone() else {
            return false;
        };
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };
        let Some(path) = uri_to_path(uri) else {
            return false;
        };
        let Some(hover) = ingots.hover(&path.to_string_lossy(), &doc.source, offset as u32) else {
            return false;
        };
        let result = crate::ingots::hover(doc, &hover);
        drop(st);
        self.respond(id, result);

        true
    }

    /// Whether a completion request came from a quote that closed a
    /// string: the quote is a trigger character for a require path, and
    /// the one that ends the string is the same key.
    /// Whether the cursor writes the name of a new declaration, `enum
    /// Col|`: the name is the author's to choose, so no list fits. Luau
    /// answers a binding name with nothing for the same reason; the
    /// shadow's shape differs for an Alloy declaration, so the source
    /// decides.
    fn names_a_declaration(&self, uri: &str, message: &Value) -> bool {
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

        declares_a_name_at(&doc.source, offset)
    }

    fn closes_a_string(&self, uri: &str, message: &Value) -> bool {
        let Some(trigger) = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
        else {
            return false;
        };

        if !matches!(trigger, "\"" | "'" | "`") {
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
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let before = &doc.source[line_start..offset];
        let quote = trigger.chars().next().unwrap_or('"');
        let mut open = false;
        let mut chars = before.chars();

        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
            } else if c == quote {
                open = !open;
            }
        }

        // An even count: the quote just typed closed the string.
        !open
    }

    /// The completion items an ingot offers at a position, when it has
    /// any: the list is the ingot's alone, since it owns that spot.
    fn ingot_completion(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };
        let trigger = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let st = self.state.lock().expect("state");
        let items = st.ingot_items(uri, line, character, trigger.as_deref());

        if items.is_empty() {
            return false;
        }

        drop(st);
        self.respond(id, json!(items));

        true
    }

    /// The labels an ingot offers for a color the editor picked, when
    /// the range is one the ingot colored; else the child answers.
    fn ingot_presentation(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some(range) = message.pointer("/params/range").and_then(range_of) else {
            return false;
        };
        let Some(color) = message.pointer("/params/color").cloned() else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(ingots) = st.ingots.clone() else {
            return false;
        };
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let (Some(start), Some(end)) = (
            offset_of(&doc.source, range.0.0, range.0.1),
            offset_of(&doc.source, range.1.0, range.1.1),
        ) else {
            return false;
        };
        let Some(path) = uri_to_path(uri) else {
            return false;
        };
        let labels = ingots.present(
            &path.to_string_lossy(),
            &doc.source,
            (start as u32, end as u32),
            &color,
        );
        if labels.is_empty() {
            return false;
        }

        let range_value = message
            .pointer("/params/range")
            .cloned()
            .unwrap_or(Value::Null);
        let result: Vec<Value> = labels
            .iter()
            .map(|l| json!({ "label": l, "textEdit": { "range": range_value, "newText": l } }))
            .collect();
        drop(st);
        self.respond(id, json!(result));

        true
    }

    fn open_workspace(&self) {
        self.load_ingots();
        let root = self.state.lock().expect("state").root.clone();
        let Some(root) = root else {
            return;
        };

        let (input, out) = match Config::find_within(&root, &root)
            .and_then(|p| Config::load(&p).ok().map(|c| (p, c)))
        {
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

            // The mirror's own `.luaurc`: the root's Luau configuration,
            // strict when it names no mode, plus the mount names it lacks
            // while `[project] mount_aliases` stays on. The child reads
            // this file, so `@pkg/x` resolves in a shadow the way the
            // compiler resolves it, and the user's own file stays as it
            // is. A mirrored `.config.luau` goes, so the merged file is
            // the one read.
            let mut luau = alloy::luau_config::read_dir(&root)
                .map(|(_, c)| c)
                .unwrap_or_default();

            if luau.language_mode.is_none() {
                luau.language_mode = Some("strict".to_string());
            }

            if let Some(path) = Config::find_within(&root, &root)
                && let Ok(config) = Config::load(&path)
                && config.project.mount_aliases
            {
                for (name, m) in &config.mount {
                    if !luau.aliases.iter().any(|(a, _)| a == name) {
                        luau.aliases
                            .push((name.clone(), format!("./{}", m.0.replace('\\', "/"))));
                    }
                }
            }

            st.write_mirror(
                &root.join(".luaurc"),
                &alloy::luau_config::render_luaurc(&luau),
            );
            let _ = std::fs::remove_file(st.mirror.join(".config.luau"));
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

    /// Asks the editor to report changes to data files, so a saved
    /// `.json` or `.toml` regenerates its mirror module. The extension's
    /// own watcher covers `.json`; this one adds `.toml`. An editor
    /// without dynamic registration answers with an error, which is
    /// dropped.
    fn watch_data_files(&self) {
        let id = {
            let mut st = self.state.lock().expect("state");
            let id = st.fresh_id();
            st.asked.insert(id.clone(), Asked::Watch);

            id
        };
        self.to_client(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "alloy-data-files",
                    "method": "workspace/didChangeWatchedFiles",
                    "registerOptions": {
                        "watchers": [{ "globPattern": "**/*.{json,toml}" }]
                    }
                }]
            }
        }));
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

        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let aliases = project_aliases(&dir, self.root.as_deref());

        imports::auto_import_items(&doc.source, &path, &files, &prefix, &bound, &aliases)
    }
}

impl State {
    /// The keyword wins while the typed word begins one.
    ///
    /// `end` in `if x then return end`, and in a one-line `struct T as
    /// end`, drew `EncodingService` from the child's auto-imports: the
    /// editor matched the letters and sorted the module first. So while
    /// the word begins a keyword the list drops every auto-import and
    /// every label the word does not begin, holds the keywords the word
    /// begins, and an exact keyword takes the first row.
    fn keyword_first(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return;
        };

        // A member names what a value has, and a string holds no word.
        if member_position(doc, line, character).is_some()
            || context::in_string(&doc.source, offset)
        {
            return;
        }

        let word = imports::word_before(&doc.source, offset);
        let matches = keywords::starting_with(&word);

        if matches.is_empty() {
            return;
        }

        let items = match result {
            Value::Array(v) => v,

            Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
                Some(v) => v,

                None => return,
            },

            _ => return,
        };
        items.retain(|i| {
            let label = i.get("label").and_then(Value::as_str).unwrap_or_default();

            !is_auto_import(i) && label.starts_with(word.as_str())
        });

        for keyword in &matches {
            if !items
                .iter()
                .any(|i| i.get("label").and_then(Value::as_str) == Some(*keyword))
            {
                items.push(json!({
                    "label": keyword,
                    "kind": 14,
                    "detail": "Alloy keyword",
                    "sortText": format!("0{keyword}"),
                }));
            }
        }

        if !keywords::is_keyword(&word) {
            return;
        }

        for item in items.iter_mut() {
            if item.get("label").and_then(Value::as_str) == Some(word.as_str()) {
                item["preselect"] = json!(true);
                item["filterText"] = json!(word.clone());
                item["sortText"] = json!(format!("!{word}"));
            }
        }
    }

    /// The child's module auto-imports, as Alloy imports.
    ///
    /// luau-lsp offers a module by its instance path and inserts a
    /// `require`. Alloy writes `import name from "@pkg/name"`, so the
    /// item carries the module's name, the spec the project's aliases
    /// give it, and one edit that writes the import under the last one.
    /// A module a dot folder holds, one no alias and no `[build] in`
    /// reaches, and one the file already imports are dropped.
    fn rewrite_child_auto_imports(&self, uri: &str, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let aliases = project_aliases(&dir, self.root.as_deref());
        let mounts = self.instance_mounts();
        let input = self.input_dir();
        let taken = imports::imported_specs(&doc.source);
        let source = doc.source.clone();
        let items = match result {
            Value::Array(v) => v,

            Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
                Some(v) => v,

                None => return,
            },

            _ => return,
        };

        items.retain_mut(|item| {
            if !is_module_auto_import(item) {
                return true;
            }

            let Some(instance) = item.get("detail").and_then(Value::as_str) else {
                return false;
            };
            let Some(file) = module_file_of(instance, &mounts) else {
                return false;
            };
            let Some(spec) = imports::best_spec(&dir, &file, &aliases) else {
                return false;
            };
            let under_input = input.as_ref().is_some_and(|i| file.starts_with(i));

            // A module the file reads is no offer, and a relative spec
            // outside `[build] in` names a package store the author
            // never writes.
            if taken.contains(&spec) || (spec.starts_with('.') && !under_input) {
                return false;
            }

            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let export = imports::Export {
                name: name.clone(),
                is_type: false,
                is_default: true,
                kind: 9,
            };
            item["label"] = json!(name);
            item["detail"] = json!(spec);
            item["insertText"] = json!(name);
            item["additionalTextEdits"] = json!([imports::import_edit(&source, &spec, &export)]);

            true
        });
    }

    /// The `[build] in` directory of the project, the one tree whose
    /// modules an author writes by a relative path.
    fn input_dir(&self) -> Option<PathBuf> {
        let root = self.root.as_deref()?;
        let path = Config::find_within(root, root)?;
        let config = Config::load(&path).ok()?;
        let base = path.parent().unwrap_or(Path::new("."));

        Some(config_dir(base, &config.build.input.to_string_lossy()))
    }

    /// The `[mount]` table as instance path to directory, longest
    /// instance path first, so a nested mount wins over its parent.
    fn instance_mounts(&self) -> Vec<(String, PathBuf)> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        let Some(path) = Config::find_within(root, root) else {
            return Vec::new();
        };
        let Ok(config) = Config::load(&path) else {
            return Vec::new();
        };
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut out: Vec<(String, PathBuf)> = config
            .mount
            .values()
            .filter_map(|m| {
                let instance = m.1.strip_prefix("@game/")?.replace('/', ".");

                Some((instance, config_dir(&base, &m.0)))
            })
            .collect();
        out.sort_by_key(|(i, _)| std::cmp::Reverse(i.len()));

        out
    }
}

/// Whether an item is the child's auto-import of a module: it inserts a
/// `require`, so its detail is the instance path of a module file.
fn is_module_auto_import(item: &Value) -> bool {
    if !is_auto_import(item) {
        return false;
    }

    item.get("additionalTextEdits")
        .and_then(Value::as_array)
        .is_some_and(|edits| {
            edits.iter().any(|e| {
                e.get("newText")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.contains("= require("))
            })
        })
}

/// The file an instance path names, through the `[mount]` table:
/// `ReplicatedStorage.Packages.fluid` under
/// `pkg = ["packages/roblox", "@game/ReplicatedStorage/Packages"]` is
/// `packages/roblox/fluid`.
fn module_file_of(instance: &str, mounts: &[(String, PathBuf)]) -> Option<PathBuf> {
    for (prefix, dir) in mounts {
        let Some(tail) = instance
            .strip_prefix(prefix.as_str())
            .and_then(|t| t.strip_prefix('.'))
        else {
            continue;
        };
        let mut file = dir.clone();

        for part in tail.split('.') {
            file.push(part);
        }

        return Some(file);
    }

    None
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
            // A dot directory holds tooling state, `.lest` or the test
            // modules, not sources; `.alloy` keeps the build's sourcemap,
            // and `.ember` holds the packages a `packages/` stub requires.
            if matches!(name.as_str(), "node_modules" | "target")
                || (name.starts_with('.') && !matches!(name.as_str(), ".alloy" | ".ember"))
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

/// The definition a position in a data import names: on the path, the
/// data file's first line; on an imported name, the line of that key.
/// None when the line holds no data path or the file is not there.
fn data_definition(source: &str, offset: usize, dir: &Path) -> Option<Value> {
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = source[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let at = offset - line_start;
    let reference = alloy::data::references(line).into_iter().next()?;
    let format = alloy::data::Format::of(&reference.path)?;
    let file = imports::lexical(dir, &reference.path);

    if !file.is_file() {
        return None;
    }

    let on_path = (reference.start as usize..=reference.end as usize).contains(&at);
    let mut target_line = 0;

    if !on_path {
        // A name binds to the file only in an `import` statement, and
        // only before its path; a local in `local x = import("...")`
        // is the child's to find.
        let is_statement = line.trim_start().starts_with("import ");

        if !is_statement || at >= reference.start as usize || !keywords::is_word_at(line, at) {
            return None;
        }

        let (start, end) = keywords::word_range(line, at);
        let word = &line[start..end];

        if matches!(word, "import" | "type" | "as" | "from") {
            return None;
        }

        // `key as alias`: the key is the word before `as`.
        let key = match line[..start].trim_end().strip_suffix("as") {
            Some(head) if head.ends_with(char::is_whitespace) => {
                let head = head.trim_end();
                let key_start = head
                    .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .map(|i| i + 1)
                    .unwrap_or(0);

                &head[key_start..]
            }

            _ => word,
        };

        // A name in braces names a key; the module name before `from`
        // or after `import` names the file.
        let in_braces = line[..start].contains('{') && line[end..].contains('}');

        if in_braces && let Ok(text) = std::fs::read_to_string(&file) {
            target_line = alloy::data::key_line(&text, format, key).unwrap_or(0);
        }
    }

    let position = json!({ "line": target_line, "character": 0 });

    Some(json!([{
        "uri": path_to_uri(&file),
        "range": { "start": position, "end": position },
    }]))
}

/// The module a data file builds to, `x.json` giving `x.luau`, when
/// no module of that stem sits beside it.
fn data_module_of(path: &Path) -> Option<PathBuf> {
    if alloy::data::Format::of_path(path).is_none() || alloy::data::module_beside(path).is_some() {
        return None;
    }

    Some(path.with_extension("luau"))
}

/// The data file behind a mirror module: the child answers about
/// `x.luau`, and the editor has `x.json` or `x.toml` when no real
/// `x.luau` exists.
fn data_source_of(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|e| e == "luau") && !path.exists() {
        for ext in ["json", "toml"] {
            let data = path.with_extension(ext);

            if data.is_file() {
                return data;
            }
        }
    }

    path
}

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

/// One workspace root as a directory name. Two servers run at once, one
/// per project, and neither may write where the other reads.
pub fn root_key(root: Option<&Path>) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default()
        .hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

fn mirror_dir(root: Option<&Path>) -> PathBuf {
    std::env::temp_dir().join("alloy-lsp").join(root_key(root))
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

    // The child reads a `---` comment the shadow keeps, and the hover
    // carries it behind a rule. That doc is the binding's own: adding
    // it again shows it twice.
    let doc_text = binding
        .doc
        .as_deref()
        .filter(|_| !rest.contains("\n```\n----------\n"));

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

        if let Some(doc) = doc_text {
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

    if head == binding.prefix && doc_text.is_none() {
        return None;
    }

    let mut out = format!("```alloy\n{}{tail}", binding.prefix);

    if let Some(doc) = doc_text {
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
/// Where a type name sorts in the list of a slot. `extends` and the
/// trait of an `impl` take a contract; `impl X` and the target after
/// `for` take a struct, an enum, or a class. The other names stay in
/// the list: the author may be about to declare one.
fn type_rank(prefers: context::Prefers, detail: &str) -> u8 {
    match prefers {
        context::Prefers::Any => 1,

        context::Prefers::Contract => match detail {
            "interface" | "trait" | "alloy:std trait" => 0,

            _ => 1,
        },

        context::Prefers::Concrete => match detail {
            "struct" | "enum" => 0,

            _ => 1,
        },
    }
}

/// The globals a value expression reaches for, for the list the proxy
/// builds where luau-lsp answers nothing. The full global list is the
/// child's to give; these are the names an arm or a ternary writes.
const EXPRESSION_GLOBALS: &[&str] = &[
    "print",
    "warn",
    "error",
    "assert",
    "tostring",
    "tonumber",
    "typeof",
    "type",
    "ipairs",
    "pairs",
    "next",
    "select",
    "pcall",
    "math",
    "string",
    "table",
    "os",
    "task",
    "buffer",
    "coroutine",
    "utf8",
    "game",
    "workspace",
    "script",
    "Instance",
    "Enum",
    "Vector3",
    "Vector2",
    "CFrame",
    "Color3",
    "UDim",
    "UDim2",
    "TweenInfo",
    "BrickColor",
    "Random",
    "NumberRange",
    "DateTime",
];

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
fn drop_internal_items(result: &mut Value) {
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
fn is_auto_import(item: &Value) -> bool {
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

fn strip_std_prefix(value: &mut Value) {
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

/// The list as the source can use it. A `:` names a member, so the
/// list holds members alone, each without the receiver its signature
/// carries. A detail the reader cannot write goes, and an empty
/// documentation, which opens an empty panel, goes too.
fn clean_completion(result: &mut Value, doc: &Doc, line: u32, character: u32, snippets: bool) {
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

/// Gives an item the call its signature describes, when it carries no
/// insert of its own: `earn(${1:amount})$0`, or `alive()` for a
/// signature with no argument.
fn set_call(item: &mut Value, label: &str, detail: &str, snippets: bool) {
    if item.get("insertText").is_some() || item.pointer("/textEdit/newText").is_some() {
        return;
    }

    let Some(insert) = call_snippet(label, detail) else {
        return;
    };

    match snippets {
        true => {
            item["insertText"] = json!(insert);
            item["insertTextFormat"] = json!(2);
        }

        // With no snippet support the placeholders would land as
        // literal text; an empty pair is what the editor can take.
        false => item["insertText"] = json!(format!("{label}()")),
    }
}

/// The snippet a signature calls for: one placeholder per parameter,
/// named the way the signature names it.
fn call_snippet(label: &str, detail: &str) -> Option<String> {
    let rest = match detail.strip_prefix('<') {
        Some(after) => &detail[after.find('>')? + 2..],

        None => detail,
    };
    let inner = rest.strip_prefix('(')?;
    let mut depth = 0i32;
    let mut prev = ' ';
    let mut end = None;

    for (i, c) in inner.char_indices() {
        if c == '>' && prev == '-' {
            prev = c;

            continue;
        }

        prev = c;

        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' if depth == 0 => {
                end = Some(i);

                break;
            }
            ')' | '}' | ']' | '>' => depth -= 1,
            _ => {}
        }
    }

    let params = &inner[..end?];

    if params.trim().is_empty() {
        return Some(format!("{label}()"));
    }

    let mut slots = Vec::new();
    let mut depth = 0i32;
    let mut prev = ' ';
    let mut part = String::new();

    for c in params.chars().chain(std::iter::once(',')) {
        if c == '>' && prev == '-' {
            prev = c;
            part.push(c);

            continue;
        }

        prev = c;

        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                // A vararg takes as many arguments as the caller has,
                // so it fills no slot of its own.
                if part.trim_start().starts_with("...") {
                    part.clear();

                    continue;
                }

                let name = part
                    .split(':')
                    .next()
                    .unwrap_or(&part)
                    .trim()
                    .trim_end_matches('?')
                    .to_string();
                let name = match name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    && !name.is_empty()
                {
                    true => name,

                    false => format!("v{}", slots.len() + 1),
                };
                slots.push(format!("${{{}:{name}}}", slots.len() + 1));
                part.clear();

                continue;
            }
            _ => {}
        }

        part.push(c);
    }

    match slots.is_empty() {
        true => Some(format!("{label}()")),

        false => Some(format!("{label}({})$0", slots.join(", "))),
    }
}

/// A record type without the fields the struct keeps private: the
/// detail of `to_table` prints every one, which names what the type
/// hides from a reader outside the impl.
fn hide_record(detail: &str, private: &HashSet<String>) -> String {
    let Some(open) = detail.find("{ ") else {
        return detail.to_string();
    };
    let Some(close) = detail[open..].find(" }") else {
        return detail.to_string();
    };
    let body = &detail[open + 2..open + close];
    let kept: Vec<&str> = body
        .split(", ")
        .filter(|part| {
            let name = part.split(':').next().unwrap_or(part).trim();

            !private.contains(name.trim_end_matches('?'))
        })
        .collect();

    format!(
        "{}{{ {} }}{}",
        &detail[..open],
        kept.join(", "),
        &detail[open + close + 2..]
    )
}

/// Whether the child's own mapping already puts the caret after the
/// same access. The emit copies most of them, and moving one that
/// landed right would cost the member list it already answers.
fn lands_on_member(
    doc: &Doc,
    line: u32,
    character: u32,
    base: &str,
    sep: char,
    prefix: usize,
) -> bool {
    let (sl, sc) = doc.to_shadow(line, character);
    let Some(text) = doc.shadow.lines().nth(sl as usize) else {
        return false;
    };
    let Some(at) = offset_of(text, 0, sc) else {
        return false;
    };
    let head = &text[..at.min(text.len())];
    let head = &head[..head.len() - prefix.min(head.len())];
    let receiver = base.rsplit('.').next().unwrap_or(base);

    head.strip_suffix(sep)
        .is_some_and(|h| h.ends_with(receiver))
}

/// The separator right before the word at `offset`, when one is there.
fn sep_of(source: &str, offset: usize) -> Option<char> {
    let head = &source[..offset.min(source.len())];
    let word = head.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    word.chars().next_back().filter(|c| matches!(c, '.' | ':'))
}

/// The separator a member access at the caret uses, `.` or `:`, when
/// the caret sits in a member name. `None` anywhere else.
fn member_position(doc: &Doc, line: u32, character: u32) -> Option<char> {
    let text = doc.source.lines().nth(line as usize)?;
    let head: String = text.chars().take(character as usize).collect();
    let word = head.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    let sep = word.chars().next_back()?;

    (matches!(sep, '.' | ':') && !word.ends_with("::")).then_some(sep)
}

/// A signature without the receiver a colon passes: `(Account, number)
/// -> number` reads `(number) -> number`.
fn drop_receiver(detail: &str) -> Option<String> {
    // `<U>(read T[], f: ...) -> U[]` keeps its type parameters.
    let (head, rest) = match detail.strip_prefix('<') {
        Some(after) => {
            let close = after.find('>')? + 2;

            (&detail[..close], &detail[close..])
        }

        None => ("", detail),
    };
    let inner = rest.strip_prefix('(')?;
    let mut depth = 0i32;
    let mut cut = None;
    let mut prev = ' ';

    for (k, c) in inner.char_indices() {
        // The `>` of a `->` closes nothing; reading it as a bracket
        // walks the depth below zero and cuts the wrong parameter.
        if c == '>' && prev == '-' {
            prev = c;

            continue;
        }

        prev = c;

        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' if depth == 0 => {
                cut = Some((k, k));

                break;
            }
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                cut = Some((k, k + 1));

                break;
            }
            _ => {}
        }
    }

    let (_, end) = cut?;
    let tail = inner[end..].trim_start();

    Some(format!("{head}({tail}"))
}

/// The private fields of every struct the file declares.
fn private_fields(doc: &Doc) -> HashSet<String> {
    doc.shapes
        .iter()
        .chain(doc.import_shapes.iter())
        .filter_map(|s| match s {
            alloy::declarations::Shape::Struct { fields, .. } => Some(fields),

            _ => None,
        })
        .flatten()
        .filter(|(_, private)| *private)
        .map(|(f, _)| f.clone())
        .collect()
}

/// A constructor signature without the fields the struct keeps private:
/// they have a default, so no caller writes them.
fn hide_private(detail: &str, private: &HashSet<String>) -> String {
    let Some(open) = detail.find("({ ") else {
        return detail.to_string();
    };
    let Some(close) = detail[open..].find(" }) -> ") else {
        return detail.to_string();
    };
    let body = &detail[open + 3..open + close];
    let kept: Vec<&str> = body
        .split(", ")
        .filter(|part| {
            let name = part.split(':').next().unwrap_or(part).trim();

            !private.contains(name.trim_end_matches('?'))
        })
        .collect();

    format!(
        "{}({{ {} }}{}",
        &detail[..open],
        kept.join(", "),
        &detail[open + close + 2..]
    )
}

/// The hints as the source can hold them. A parameter hint that names
/// an emit slot goes. A type hint loses `@checked`, reads by the name
/// its line gives when the print is unwritable, and inserts nothing
/// when no name is at hand.
fn clean_hints(hints: &mut Vec<Value>, doc: &Doc) {
    hints.retain(|h| !emit_slot_hint(h));

    let parameters = declared_type_parameters(&doc.source);

    for h in hints.iter_mut() {
        // The label and the edit are one text.
        if let Some(text) = h
            .pointer("/textEdits/0/newText")
            .and_then(Value::as_str)
            .map(str::to_string)
            && hint_label(h).starts_with(':')
        {
            h["label"] = json!(text);
        }

        let label = hint_label(h);

        if !label.starts_with(": ") {
            continue;
        }

        // `@checked` is an emit attribute; a written type carries none.
        let label = label
            .replace("@checked ", "")
            .replace("@native ", "")
            .replace("@checked", "");
        let annotation = label[2..].trim();
        let named = writable_type(annotation) && !undeclared_variable(annotation, &parameters);
        let position = h.get("position").and_then(position_of_value);
        // The child types a method's receiver from its body; the `impl`
        // says what it is.
        let on_self = position.is_some_and(|(l, c)| self_parameter(doc, l, c));
        let from_source = position.and_then(|(l, c)| source_type(doc, l, c));

        let label = match (named && !on_self, from_source) {
            (true, _) => label,

            (false, Some(name)) => format!(": {name}"),

            (false, None) if named => label,

            (false, None) => {
                h.as_object_mut().map(|o| o.remove("textEdits"));
                truncate_hint(h, &label);

                continue;
            }
        };

        h["label"] = json!(label);

        // A generic struct prints without its arguments: Luau names a
        // metatable type and carries none. `: Slotted` would not
        // compile, so the hint reads and inserts nothing.
        if generic_struct(doc, label[2..].trim()) {
            h.as_object_mut().map(|o| o.remove("textEdits"));

            continue;
        }

        if truncate_hint(h, &label) {
            h.as_object_mut().map(|o| o.remove("textEdits"));

            continue;
        }

        if let Some(position) = h.get("position").cloned() {
            h["textEdits"] = json!([{
                "range": { "start": position.clone(), "end": position },
                "newText": label,
            }]);
        }
    }
}

/// Whether a printed type names a struct that takes type parameters
/// and gives it none. Luau prints a struct by its metatable's name, so
/// the arguments are gone, and `Slotted` or `Pair[]` names a type the
/// source cannot write. The name may sit anywhere in the text.
fn generic_struct(doc: &Doc, text: &str) -> bool {
    let generics = |name: &str| {
        std::iter::once(&doc.source)
            .chain(doc.import_sources.iter())
            .any(|src| src.contains(&format!("struct {name}<")))
    };
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at] as char).is_alphanumeric() && bytes[at] != b'_' {
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && ((bytes[at] as char).is_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }

        let word = &text[start..at];

        if !text[at..].starts_with('<') && generics(word) {
            return true;
        }
    }

    false
}

/// A label too long for the gutter shows its head alone. True when the
/// label was cut, so what is left inserts nothing.
fn truncate_hint(h: &mut Value, label: &str) -> bool {
    if label.chars().count() <= 72 {
        return false;
    }

    let head: String = label.chars().take(69).collect();
    h["label"] = json!(format!("{}…", head.trim_end()));

    true
}

/// `_1:` and `_2:` are the payload slots of a tagged enum. The source
/// names neither, so neither belongs in the gutter.
fn emit_slot_hint(h: &Value) -> bool {
    let label = hint_label(h);
    let name = label.trim_end_matches(':');

    name.len() > 1 && name.starts_with('_') && name[1..].chars().all(|c| c.is_ascii_digit())
}

/// A type a source line can hold: no attribute, no solver clause, no
/// emit-only name, and nothing the child cut short.
fn writable_type(text: &str) -> bool {
    !text.contains('@')
        && !text.contains(" where ")
        && !text.contains('*')
        && !text.contains('…')
        && !text.contains("__")
        && !text.contains("CYCLE")
        // The child cuts a long table with `... N more ...`.
        && !text.contains(" more ...")
        // `_1` and `_2` are the payload slots of a tagged enum.
        && !text.contains("_1:")
        && !text.contains("_2:")
        // `~nil` negates a type; Alloy writes no negation. The checker's
        // own type functions, `intersect<T, ~nil>`, carry it.
        && !text.contains('~')
        && !text.contains("intersect<")
        && !text.contains("union<")
        // A type pack, `...any`, annotates no binding.
        && !text.starts_with("...")
        && !names_a_hidden_field(text)
}

/// Whether a type text names a field the emit made: `_head`, `_next`,
/// `_source`. A std type hides them, so a popup that prints them names
/// what the language does not document.
fn names_a_hidden_field(text: &str) -> bool {
    text.match_indices('_').any(|(i, _)| {
        let opens = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, '{' | ' ' | ',' | '('));
        let name: String = text[i..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        opens && text[i + name.len()..].starts_with(':')
    })
}

/// A solver variable the file never declares, `a` or `T`: no
/// annotation can name it. The whole type may be one, `T[]`, or a
/// member's may be, `{ read value: a }`.
fn undeclared_variable(text: &str, declared: &HashSet<String>) -> bool {
    let short = |name: &str| {
        name.len() <= 2
            && !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric())
            && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && !declared.contains(name)
    };

    if short(text.trim().trim_end_matches('?').trim_end_matches("[]")) {
        return true;
    }

    // Every word of the type: a short one the file never declared is a
    // solver variable wherever it stands. A word a `:` follows is a
    // member's key, and one a `<` follows takes arguments, so it names
    // a type of its own.
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at] as char).is_ascii_alphanumeric() && bytes[at] != b'_' {
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && ((bytes[at] as char).is_ascii_alphanumeric() || bytes[at] == b'_')
        {
            at += 1;
        }

        let after = text[at..].trim_start();

        if short(&text[start..at]) && !after.starts_with('<') && !after.starts_with(':') {
            return true;
        }
    }

    false
}

/// Whether the hint sits right after the `self` parameter of a method.
fn self_parameter(doc: &Doc, line: u32, character: u32) -> bool {
    let Some(text) = doc.source.lines().nth(line as usize) else {
        return false;
    };
    let before: String = text.chars().take(character as usize).collect();

    text.contains("function ") && before.trim_end().ends_with("self")
}

/// The type parameters a file declares: the names inside every
/// `Name<...>` the source writes.
fn declared_type_parameters(source: &str) -> HashSet<String> {
    let mut out = HashSet::new();

    for (i, _) in source.match_indices('<') {
        let before = source[..i].chars().next_back();

        if !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }

        let Some(end) = source[i..].find('>') else {
            continue;
        };

        for part in source[i + 1..i + end].split(',') {
            let name = part.split(':').next().unwrap_or("").trim();

            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.insert(name.to_string());
            }
        }
    }

    out
}

/// The type the source names at a position outright: the struct a
/// `new Name` or a `Name.new(` builds, and the struct a method's `self`
/// belongs to inside an `impl`.
fn source_type(doc: &Doc, line: u32, character: u32) -> Option<String> {
    let text = doc.source.lines().nth(line as usize)?;
    let declares = |name: &str| {
        doc.shapes
            .iter()
            .chain(doc.import_shapes.iter())
            .any(|s| matches!(s, alloy::declarations::Shape::Struct { name: n, .. } if n == name))
    };

    // `function get(self)` inside `impl Box`: the receiver is the struct.
    let before: String = text.chars().take(character as usize).collect();

    if before.trim_end().ends_with("self") && text.contains("function ") {
        return impl_self_type(doc, line).filter(|t| declares(t.split('<').next().unwrap_or(t)));
    }

    // `local root = new Node { ... }`, `local b = new Box<<number>> { }`.
    if let Some(i) = text.find("new ") {
        let rest = text[i + "new ".len()..].trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if declares(&name) {
            let args = rest[name.len()..]
                .strip_prefix("<<")
                .and_then(|a| a.find(">>").map(|e| a[..e].to_string()));

            return Some(match args {
                Some(a) => format!("{name}<{a}>"),

                None => name,
            });
        }
    }

    // `local p = Point.new(1, 2)`.
    let rest = text.split_once("= ")?.1.trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    // `Signal.new<<Effect>>()`: a std constructor with the arguments
    // the source wrote. Nothing else names them.
    if name.starts_with(|c: char| c.is_ascii_uppercase())
        && let Some(args) = rest[name.len()..]
            .strip_prefix(".new<<")
            .and_then(|a| a.find(">>").map(|e| a[..e].to_string()))
        && !args.is_empty()
    {
        return Some(format!("{name}<{args}>"));
    }

    // `local burn = Effect.Damage(20, Element.Fire)`: a variant with a
    // payload is a value of its enum, and the child prints the tagged
    // table it lowers to.
    if let Some(after) = rest[name.len()..].strip_prefix('.') {
        let variant: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let holds = doc.shapes.iter().chain(doc.import_shapes.iter()).any(|s| {
            matches!(s, alloy::declarations::Shape::Enum { name: n, variants }
                if *n == name && variants.iter().any(|(v, _)| *v == variant))
        });

        if holds {
            return Some(name);
        }
    }

    (declares(&name) && rest[name.len()..].starts_with(".new(")).then_some(name)
}

/// The struct a method's `self` belongs to: the nearest `impl` above
/// the line, with the type parameters the struct declares.
fn impl_self_type(doc: &Doc, line: u32) -> Option<String> {
    let head = doc
        .source
        .lines()
        .take(line as usize + 1)
        .filter_map(|l| {
            let text = l.trim_start();
            // A foreign impl is exported, so it works project wide.
            let text = text.strip_prefix("export ").unwrap_or(text);

            text.strip_prefix("impl ").map(str::to_string)
        })
        .last()?;
    // `impl Shape for Sq` names the struct after `for`.
    let named = head.split(" for ").last().unwrap_or(&head).trim();
    let name: String = named
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let generics = struct_generics(&doc.source, &name);

    (!name.is_empty()).then(|| format!("{name}{generics}"))
}

/// The type parameters of `struct Name<T, U>`, as the source writes
/// them; an empty text when the struct takes none.
fn struct_generics(source: &str, name: &str) -> String {
    let needle = format!("struct {name}<");
    let Some(i) = source.find(&needle) else {
        return String::new();
    };
    let rest = &source[i + needle.len()..];
    let Some(end) = rest.find('>') else {
        return String::new();
    };
    let params: Vec<&str> = rest[..end]
        .split(',')
        .map(|p| p.split(':').next().unwrap_or("").trim())
        .filter(|p| !p.is_empty())
        .collect();

    match params.is_empty() {
        true => String::new(),

        false => format!("<{}>", params.join(", ")),
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

/// The entries a module path can continue with: the project's aliases
/// and `@self` when nothing is typed, the children of
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
        out.push(("../".to_string(), 19, "the parent directory".to_string()));

        for (name, target) in project_aliases(dir, root) {
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

        project_aliases(dir, root)
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

        // A data file keeps its extension: the path names the file, and
        // the emit drops the extension itself.
        if let Some(format) = alloy::data::Format::of(&name) {
            if seen.insert(name.clone()) {
                out.push((name.clone(), 17, format!("{} data", format.name())));
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
        } else if let Some(b) = s
            .strip_suffix(".aly")
            .or_else(|| s.strip_suffix(".alx"))
            .or_else(|| s.strip_suffix(".json"))
            .or_else(|| s.strip_suffix(".toml"))
        {
            // A data file is a module in the mirror, as in the build.
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

/// The real path a mirror `_outside` folder holds. On Windows the
/// first segment is the drive `mirror_path` wrote, `C`, which becomes
/// `C:\`; on every other platform the path is absolute from the root.
fn outside_path(rel: &Path) -> PathBuf {
    if cfg!(windows) {
        let mut parts = rel.components();

        if let Some(std::path::Component::Normal(first)) = parts.next() {
            let drive = first.to_string_lossy().into_owned();

            if drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic()) {
                return PathBuf::from(format!("{drive}:/")).join(parts.as_path());
            }
        }
    }

    Path::new("/").join(rel)
}

/// A path a configuration file writes, as a directory.
///
/// `alloy.toml` and `.luaurc` are written by hand on every platform, so
/// a mount or an alias may read `packages\\roblox` or `~/shared`. Both
/// spellings resolve here; nothing else in the server sees them.
fn config_dir(base: &Path, text: &str) -> PathBuf {
    config_dir_from(base, text, home_dir().as_deref())
}

fn config_dir_from(base: &Path, text: &str, home: Option<&Path>) -> PathBuf {
    let text = text.replace('\\', "/");

    if text == "~" || text.starts_with("~/") {
        let rest = text.strip_prefix("~/").unwrap_or("");

        if let Some(home) = home {
            return imports::lexical(home, rest);
        }
    }

    imports::lexical(base, &text)
}

/// The home directory, for a path a configuration writes with `~`.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The aliases a module path can use from `dir`: the Luau
/// configuration above it, and the `[mount]` table of the nearest
/// `alloy.toml` while `[project] mount_aliases` stays on. A name the
/// Luau configuration declares wins over a mount of that name.
fn project_aliases(dir: &Path, root: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = luaurc_aliases(dir, root);
    let found = match root {
        Some(r) => Config::find_within(dir, r),

        None => Config::find(dir),
    };

    if let Some(path) = found
        && let Ok(config) = Config::load(&path)
        && config.project.mount_aliases
    {
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        for (name, m) in &config.mount {
            if !out.iter().any(|(a, _)| a == name) {
                out.push((name.clone(), config_dir(&base, &m.0)));
            }
        }
    }

    out.sort();
    out
}

/// The child's `require.directoryAliases` for the mount names the Luau
/// configuration lacks, while `[project] mount_aliases` stays on. The
/// child resolves `@pkg/x` in a shadow through this setting, so the
/// user's own configuration file is never written. Paths are relative
/// to the child's workspace, which mirrors the root.
fn mount_alias_settings(root: Option<&Path>) -> Value {
    let Some(root) = root else {
        return json!({});
    };
    let Some(path) = Config::find_within(root, root) else {
        return json!({});
    };
    let Ok(config) = Config::load(&path) else {
        return json!({});
    };

    if !config.project.mount_aliases || config.mount.is_empty() {
        return json!({});
    }

    let declared = luaurc_aliases(root, Some(root));
    let mut map = serde_json::Map::new();

    for (name, m) in &config.mount {
        if declared.iter().any(|(a, _)| a == name) {
            continue;
        }

        let dir = m.0.replace('\\', "/");
        let dir = dir.trim_end_matches('/');
        map.insert(format!("@{name}/"), Value::String(format!("{dir}/")));
    }

    if map.is_empty() {
        return json!({});
    }

    json!({ "require": { "directoryAliases": map } })
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
                .map(|(k, p)| (k.clone(), config_dir(&d, p)))
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
    // A Future that carries `__value` names itself in `shapes::fold`,
    // where the value type may hold braces of its own.
    while !out.contains("__value: ")
        && let Some(i) = out.find("andThen: (self: any, on_resolve: ((")
    {
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

            // The fields open on the `new` line; a brace on a later line
            // belongs to another statement.
            let line_end = rhs.find('\n').unwrap_or(rhs.len());

            // The hover is a use of this binding: no function between
            // the declaration and the hover takes the name as a parameter,
            // and no later `local` rebinds it.
            let hovered_before = offset < at;
            let rebound = !hovered_before
                && rebinds(
                    &doc.source[at + word.len()..offset.max(at + word.len())],
                    word,
                );

            if rhs.starts_with("new ")
                && !hovered_before
                && !rebound
                && let Some(open) = rhs[..line_end].find('{')
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

/// Whether a stretch of source binds `name` again: a function that
/// takes it as a parameter, or a `local` that declares it.
fn rebinds(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();

        if trimmed.starts_with("local ")
            && trimmed[6..].trim_start().starts_with(name)
            && !keywords::is_word_at(
                trimmed,
                6 + trimmed[6..].len() - trimmed[6..].trim_start().len() + name.len(),
            )
        {
            return true;
        }

        if let Some(f) = line.find("function")
            && let Some(open) = line[f..].find('(')
            && let Some(close) = line[f + open..].find(')')
        {
            let params = &line[f + open + 1..f + open + close];

            return params
                .split(',')
                .any(|p| p.trim().split(':').next().is_some_and(|n| n.trim() == name));
        }

        false
    })
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

/// Two structs of one shape print alike, so the child may name either.
/// The struct the line constructs is the one the reader means.
fn prefer_constructed_struct(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
    known: &crate::shapes::Known,
) -> Option<String> {
    let (fence, body) = value.split_once('\n')?;
    let inner = body.trim().strip_suffix("```")?.trim();
    let (head, printed) = inner.rsplit_once(": ")?;
    let named = source_type(doc, line, character)?;

    if printed == named || printed.contains(' ') {
        return None;
    }

    let is_struct = |n: &str| {
        known
            .shapes
            .iter()
            .any(|s| matches!(s, alloy::declarations::Shape::Struct { name, .. } if name == n))
    };
    let offset = offset_of(&doc.source, line, character)?;
    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    // The cursor is on the binding the line declares.
    (head.ends_with(word) && is_struct(printed) && is_struct(&named))
        .then(|| format!("{fence}\n{head}: {named}\n```"))
}

/// The hover of a `remote`: the declaration as the source wrote it,
/// with the comment above it.
fn remote_hover(source: &str, word: &str) -> Option<String> {
    let mut at = 0;

    for line in source.lines() {
        let text = line.trim();
        let head = text.strip_prefix("export ").unwrap_or(text);

        if let Some(rest) = head.strip_prefix("remote ") {
            let rest = rest.strip_prefix("function ").unwrap_or(rest);
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if name == word {
                let doc_text = alloy::declarations::doc_before(source, at)
                    .map(|d| format!("\n\n{d}"))
                    .unwrap_or_default();

                return Some(format!("```alloy\n{text}\n```{doc_text}"));
            }
        }

        at += line.len() + 1;
    }

    None
}

/// The declaration line of an exported `const`, with the comment above
/// it. A `const` cannot be reassigned, and `local` says the opposite.
fn const_hover(source: &str, word: &str) -> Option<String> {
    let mut at = 0;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text.strip_prefix("export const ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if name == word {
                // The value is the module's, not the reader's.
                let head = text.split_once(" = ").map_or(text, |(h, _)| h);
                let doc_text = alloy::declarations::doc_before(source, at)
                    .map(|d| format!("\n\n{d}"))
                    .unwrap_or_default();

                return Some(format!("```alloy\n{head}\n```{doc_text}"));
            }
        }

        at += line.len() + 1;
    }

    None
}

/// What a `remote` declaration says: whether it answers, which sides
/// fire it, and whether it carries `@ratelimit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteSpec {
    answers: bool,
    from_client: bool,
    from_server: bool,
    ratelimited: bool,
}

impl RemoteSpec {
    /// Whether a file on `side` may fire the remote. A file with no
    /// side of its own sees both surfaces.
    fn fires(&self, side: Option<alloy::directives::Side>) -> bool {
        match side {
            Some(alloy::directives::Side::Client) => self.from_client,
            Some(alloy::directives::Side::Server) => self.from_server,
            None => true,
        }
    }

    /// Whether a file on `side` may handle the remote.
    fn handles(&self, side: Option<alloy::directives::Side>) -> bool {
        match side {
            Some(alloy::directives::Side::Client) => self.from_server,
            Some(alloy::directives::Side::Server) => self.from_client,
            None => true,
        }
    }

    /// Whether the surface holds a member. The emit types every member
    /// on every remote, so the declaration and the file's side are what
    /// tell them apart.
    fn holds(&self, member: &str, side: Option<alloy::directives::Side>) -> bool {
        match member {
            "spec" | "instance" => true,
            "fire" => self.fires(side),
            "call" => self.answers && self.fires(side),
            "fire_all" | "fire_except" => self.from_server && self.fires(side),
            "on" | "once" | "wait" => self.handles(side),
            "on_ratelimited" => self.ratelimited && self.handles(side),
            _ => true,
        }
    }
}

/// The `remote` declaration a source writes for `name`, with the
/// attributes above it.
fn remote_spec(source: &str, name: &str) -> Option<RemoteSpec> {
    let lines: Vec<&str> = source.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let text = line.trim();
        let head = text.strip_prefix("export ").unwrap_or(text);
        let Some(rest) = head.strip_prefix("remote ") else {
            continue;
        };
        let answers = rest.starts_with("function ");
        let rest = rest.strip_prefix("function ").unwrap_or(rest);
        let word: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if word != name {
            continue;
        }

        let tail = text.rfind(" from ").map_or("", |at| &text[at..]);
        let mut ratelimited = false;

        // The attribute lines the declaration carries sit right above
        // it, comments aside; a blank line ends them.
        for above in lines[..i].iter().rev() {
            let t = above.trim();

            if t.starts_with('@') {
                ratelimited = ratelimited || t.starts_with("@ratelimit");

                continue;
            }

            if t.starts_with("--") {
                continue;
            }

            break;
        }

        return Some(RemoteSpec {
            answers,
            from_client: names_word(tail, "client"),
            from_server: names_word(tail, "server"),
            ratelimited,
        });
    }

    None
}

/// The hover of a name an import binds to a whole module: `import * as
/// Lib`, and the default binding of `import fluid from "@pkg/fluid"`.
///
/// The child reads the emitted `require` and prints the module's table,
/// a `__SCHEDULER_INTERFACE` field and dozens of lines with it. The
/// import line and the names the module exports say what the reader
/// asked. A `.luau` module answers the same way.
fn module_hover(
    source: &str,
    word: &str,
    from: Option<&Path>,
    aliases: &[(String, PathBuf)],
) -> Option<String> {
    let line = source
        .lines()
        .find(|l| import_binds_module(l.trim(), word))?;
    let spec = import_spec(line)?;

    // A data file imports as the table the build writes from it; its
    // type is the answer, not a list of keys.
    if spec.ends_with(".json") || spec.ends_with(".toml") {
        return None;
    }

    // A module the server cannot find is the child's to answer.
    let file = module_target(&spec, from, aliases)?;
    let names: Vec<String> = imports::exports_of_file(&file, 0)
        .into_iter()
        // A `__` name is the module's own bookkeeping, not a name the
        // reader writes.
        .filter(|e| !e.is_default && !e.name.starts_with("__"))
        .map(|e| match e.is_type {
            true => format!("`type {}`", e.name),

            false => format!("`{}`", e.name),
        })
        .collect();
    // A package exports dozens of names; the line stays readable and
    // the count says how many are left.
    const SHOWN: usize = 24;
    let more = names.len().saturating_sub(SHOWN);
    let listed = names.iter().take(SHOWN).cloned().collect::<Vec<_>>();
    let surface = match (names.is_empty(), more) {
        (true, _) => String::new(),

        (false, 0) => format!("\n\nExports: {}", listed.join(", ")),

        (false, n) => format!("\n\nExports: {}, and {n} more", listed.join(", ")),
    };

    Some(format!("```alloy\n{}\n```{surface}", line.trim()))
}

/// Whether an import line binds `word` to the whole module: `* as word`
/// or the default binding. A name in braces is one export, not the
/// module.
fn import_binds_module(head: &str, word: &str) -> bool {
    let Some(rest) = head.strip_prefix("import ") else {
        return false;
    };
    let rest = rest.trim_start();

    if let Some(after) = rest.strip_prefix("* as ") {
        return after
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            == Some(word);
    }

    if rest.starts_with('{') || rest.starts_with("type ") {
        return false;
    }

    let bound: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    bound == word
}

/// The spec of an import line, whichever quote it uses.
fn import_spec(line: &str) -> Option<String> {
    let at = line.rfind(" from ")? + " from ".len();
    let rest = line[at..].trim();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &rest[quote.len_utf8()..];
    let end = body.find(quote)?;

    Some(body[..end].to_string())
}

/// The file a spec names: an `@alias/tail` through the project's
/// aliases, anything else relative to the importing file.
fn module_target(
    spec: &str,
    from: Option<&Path>,
    aliases: &[(String, PathBuf)],
) -> Option<PathBuf> {
    let dir = from.and_then(Path::parent);
    let target = match spec.strip_prefix('@') {
        Some(rest) => {
            let (name, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let base = aliases
                .iter()
                .find(|(a, _)| a == name)
                .map(|(_, p)| p.clone())?;

            match tail.is_empty() {
                true => base,

                false => imports::lexical(&base, tail),
            }
        }

        None => imports::lexical(dir?, spec),
    };

    imports::module_file(&target)
}

/// A hover that prints one solver variable, `t3?`, names nothing. The
/// field the cursor reads names its declared type instead.
fn name_solver_variable(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let (fence, body) = value.split_once('\n')?;
    let inner = body.trim().strip_suffix("```")?.trim();
    let optional = inner.ends_with('?');
    let var = inner.trim_end_matches('?');

    if var.len() < 2 || !var.starts_with('t') || !var[1..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let offset = offset_of(&doc.source, line, character)?;
    let (start, end) = keywords::word_range(&doc.source, offset);
    let declared = declared_field_type(&doc.source, &doc.source[start..end])?;
    let declared = match optional && !declared.ends_with('?') {
        true => format!("{declared}?"),

        false => declared,
    };

    Some(format!("{fence}\n{declared}\n```"))
}

/// The type a struct body declares for a field, when one struct alone
/// declares it: `child: Node?` in `struct Node`.
fn declared_field_type(source: &str, field: &str) -> Option<String> {
    let mut found: Option<String> = None;
    let mut in_struct = false;

    for line in source.lines() {
        let text = line.trim();

        if text.starts_with("struct ") || text.starts_with("export struct ") {
            in_struct = true;

            continue;
        }

        if text == "end" {
            in_struct = false;

            continue;
        }

        if !in_struct {
            continue;
        }

        let head = text
            .trim_start_matches("private ")
            .trim_start_matches("public ")
            .trim_start_matches("read ")
            .trim_start_matches("write ");
        let Some((name, rest)) = head.split_once(':') else {
            continue;
        };

        if name.trim() != field {
            continue;
        }

        let declared = rest.split(" = ").next().unwrap_or(rest).trim().to_string();

        if declared.is_empty() {
            continue;
        }

        match &found {
            Some(other) if *other != declared => return None,

            _ => found = Some(declared),
        }
    }

    found
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
    let (decl_at, annotation) = declared_annotation(&doc.source, word, offset)?;

    // A test on the name between its declaration and the hover narrows
    // it: the child's type is the narrowed one, and it stays.
    if decl_at < offset && narrowed_between(&doc.source[decl_at..offset], word) {
        return None;
    }

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

/// The type text after the `name:` nearest before `at`: up to a `,`, a
/// `)`, an `=`, or the line's end at bracket depth zero. The result
/// carries the declaration's offset. A use never comes before its
/// declaration, so a later `name:` belongs to another scope.
fn declared_annotation(source: &str, name: &str, at: usize) -> Option<(usize, String)> {
    let mut from = 0;
    let mut found: Option<(usize, String)> = None;

    while let Some(i) = source[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let bounded = start
            .checked_sub(1)
            .is_none_or(|b| !keywords::is_word_at(source, b))
            && !keywords::is_word_at(source, end);
        let after = source[end..].trim_start();

        // `v: T` annotates; `v:m()` calls and `v :: T` casts.
        // A binding: `local v: T`, `const v: T`, or a parameter. A
        // field of a table type or a struct names another thing.
        let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
        let before = source[line_start..start].trim_end();
        let is_binding = before.ends_with("local")
            || before.ends_with("const")
            || before.ends_with('(')
            || before.ends_with(',');

        if bounded && is_binding && after.starts_with(": ") {
            let text = after[1..].trim_start();
            let mut depth = 0i32;
            let mut stop = text.len();

            for (j, c) in text.char_indices() {
                match c {
                    '(' | '{' | '[' | '<' => depth += 1,

                    ')' | '}' | ']' | '>' if depth > 0 => depth -= 1,

                    // A comma inside `Signal<Player, number>` is the type's.
                    ',' | '=' | '\n' if depth > 0 => {}

                    ')' | ',' | '=' | '\n' => {
                        stop = j;

                        break;
                    }

                    _ => {}
                }
            }

            let annotation = text[..stop].trim();

            if start > at {
                break;
            }

            if !annotation.is_empty() {
                found = Some((start, annotation.to_string()));
            }
        }

        from = end;
    }

    found
}

/// Whether a stretch of source tests `name`, so a use after it may be
/// narrowed: an `is`, a `typeof` or `type` call, an `IsA`, a nil
/// comparison, or a truthiness test.
fn narrowed_between(text: &str, name: &str) -> bool {
    let tests = [
        format!("{name} is "),
        format!("typeof({name})"),
        format!("type({name})"),
        format!("{name}:IsA("),
        format!("{name} == nil"),
        format!("{name} ~= nil"),
        format!("if {name} then"),
        format!("if not {name} then"),
        format!(" and {name} then"),
        format!("{name} and "),
        format!("{name} or "),
        format!("local {name} = "),
    ];

    for (i, _) in text.match_indices(name) {
        let bounded = i
            .checked_sub(1)
            .is_none_or(|b| !keywords::is_word_at(text, b));

        if !bounded {
            continue;
        }

        let from = text[..i].rfind(['\n', ' ', '(']).map_or(0, |k| k);

        if tests
            .iter()
            .any(|t| text[from..].starts_with(t) || text[i..].starts_with(t))
        {
            return true;
        }
    }

    false
}

/// The `--@alloy-expect-error` directives that cover a line nothing
/// reported on, each as a diagnostic on the directive. `child` holds the
/// checker's reports before the filter, in shadow lines, which the
/// source shares; the compiler's own hits come with the output.
fn unmet_expectations(doc: &Doc, child: &[Value]) -> Vec<Value> {
    let silence = alloy::directives::scan(&doc.source);

    if silence.is_empty() {
        return Vec::new();
    }

    let mut errored: HashSet<usize> = doc
        .output
        .as_ref()
        .map(|o| o.expected_hits.iter().copied().collect())
        .unwrap_or_default();

    for d in child {
        if let Some(((sl, _), _)) = d.get("range").and_then(range_of)
            && d.get("severity").and_then(Value::as_u64).unwrap_or(1) <= 2
        {
            errored.insert(sl as usize);
        }
    }

    silence
        .unmet(&errored)
        .into_iter()
        .map(|(at, _col, reason)| {
            let (s, e) = alloy::directives::span_of_line(&doc.source, at);
            let (sl, sc) = position_of(&doc.source, s);
            let (el, ec) = position_of(&doc.source, e);
            // The reason names which directive went stale, so a file
            // with several says which one to look at.
            let message = alloy::directives::unmet_message(reason.as_deref());
            let mut item = json!({
                "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                "severity": 1,
                "source": "Alloy",
                "message": alloy::docs::labeled(&message),
            });

            if let Some(code) = alloy::docs::code_for(&message)
                && let Some(url) = alloy::docs::book_url(code)
            {
                item["code"] = json!(code);
                item["codeDescription"] = json!({ "href": url });
            }

            item
        })
        .collect()
}

/// The hover of a `case` pattern's binding at `line`: the name with the
/// type the pattern gives it. `None` when the line is in no arm, or the
/// word is no binding of it.
fn case_binding_text(
    doc: &Doc,
    line: usize,
    start: usize,
    word: &str,
    known: &crate::shapes::Known,
) -> Option<String> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let mut at = line.min(lines.len().saturating_sub(1));

    // The arm the line belongs to: the nearest `case` above it, and no
    // `end` or `match` head between.
    let case_line = loop {
        let text = lines.get(at)?.trim();

        if text.starts_with("case ") {
            break at;
        }

        if text == "end" || text.ends_with(" with") {
            return None;
        }

        at = at.checked_sub(1)?;
    };
    let pattern = case_pattern(lines[case_line])?;
    let bindings = pattern_bindings(&pattern, known, || array_element(&lines, case_line));

    // `b.amount` reads a field of what `case Buff(b)` bound; the child
    // sees the payload slot and answers `any`.
    if let Some(head) = doc.source[..start].strip_suffix('.')
        && keywords::is_word_at(&doc.source, head.len().checked_sub(1)?)
    {
        let (rs, re) = keywords::word_range(&doc.source, head.len() - 1);
        let receiver = &doc.source[rs..re];
        let (_, ty, _) = bindings.iter().find(|(n, _, _)| n == receiver)?;

        return field_of_struct(doc, ty, word);
    }

    let (_, ty, owner) = bindings.into_iter().find(|(n, _, _)| n == word)?;

    Some(format!(
        "```alloy\n{word}: {ty}\n```\nA binding of {owner}."
    ))
}

/// The hover of one field of a named struct, from the declaration index.
fn field_of_struct(doc: &Doc, name: &str, field: &str) -> Option<String> {
    let line = doc
        .decls
        .iter()
        .filter(|d| d.name == name && d.hover.contains("struct "))
        .find_map(|d| {
            d.hover
                .lines()
                .find(|l| field_key(l) == Some(field))
                .map(|l| l.trim().to_string())
        })?;

    Some(format!(
        "```alloy\n{line}\n```\nA field of `struct {name}`."
    ))
}

/// The pattern of a `case` line: what stands between `case` and the
/// arm's `then`, or the guard's `and`.
fn case_pattern(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("case ")?;
    let end = [rest.find(" then"), rest.find(" and ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());

    Some(rest[..end].trim().to_string())
}

/// The names a pattern binds, each with its type and what it comes
/// from. A payload reads its type off the enum's declaration; an array
/// pattern reads the element type of what the match runs over.
fn pattern_bindings(
    pattern: &str,
    known: &crate::shapes::Known,
    element: impl Fn() -> Option<String>,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();

    if let Some(inner) = pattern.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
        let Some(elem) = element() else {
            return out;
        };

        for item in inner.split(',') {
            let item = item.trim();

            match item.strip_prefix("...") {
                Some(rest) if is_binding(rest) => {
                    out.push((
                        rest.to_string(),
                        format!("{elem}[]"),
                        "the array pattern".into(),
                    ));
                }

                _ if is_binding(item) => {
                    out.push((item.to_string(), elem.clone(), "the array pattern".into()));
                }

                _ => {}
            }
        }

        return out;
    }

    let Some(open) = pattern.find('(') else {
        return out;
    };
    let head = pattern[..open].trim();
    let variant = head.rsplit('.').next().unwrap_or(head);
    let args = pattern[open + 1..].trim_end().trim_end_matches(')');

    let found = known.shapes.iter().find_map(|s| match s {
        alloy::declarations::Shape::Enum { name, variants } => variants
            .iter()
            .find(|(v, _)| v == variant)
            .map(|(_, payload)| (name.clone(), payload.clone())),

        _ => None,
    });

    let Some((enum_name, payload)) = found else {
        return out;
    };

    for (k, item) in split_top(args).into_iter().enumerate() {
        let item = item.trim();

        if !is_binding(item) {
            continue;
        }

        let Some(ty) = payload.get(k) else {
            continue;
        };
        out.push((
            item.to_string(),
            ty.clone(),
            format!("`{enum_name}.{variant}`"),
        ));
    }

    out
}

/// Whether a pattern item is a name the arm binds, and not `_` or a
/// literal.
fn is_binding(text: &str) -> bool {
    !text.is_empty()
        && text != "_"
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The members of a pattern's argument list, split at the commas that
/// stand outside every bracket.
fn split_top(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&text[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    out.push(&text[start..]);

    out
}

/// The element type the match runs over, for an array pattern: the
/// annotation of the name the `match` head reads.
fn array_element(lines: &[&str], case_line: usize) -> Option<String> {
    let head = lines[..case_line]
        .iter()
        .rev()
        .find(|l| l.trim_end().ends_with(" with"))?;
    let at = head.find("match ")? + "match ".len();
    let name = head[at..].trim_end().trim_end_matches("with").trim();

    if !is_binding(name) {
        return None;
    }

    let needle = format!("{name}: ");

    for line in lines[..case_line].iter().rev() {
        let Some(i) = line.find(&needle) else {
            continue;
        };
        let rest = &line[i + needle.len()..];
        let end = rest.find([',', ')']).unwrap_or(rest.len());
        let ty = rest[..end].trim();

        return ty.strip_suffix("[]").map(str::to_string);
    }

    None
}

/// The std type a receiver word names, with whether the word is the
/// type itself. A value resolves through its annotation, or through
/// what it starts from.
fn std_receiver(source: &str, sigil: usize, word: &str) -> Option<(&'static str, bool)> {
    if word.is_empty() {
        return None;
    }

    if let Some(key) = alloy::docs::member_owner(word) {
        return Some((key, true));
    }

    let base = match context::declared(source, sigil, word)? {
        context::Declared::Annotation(t) => alloy::docs::type_head(&t),
        context::Declared::Init(v) => alloy::docs::value_head(&v),
    }?;

    alloy::docs::member_owner(&base).map(|key| (key, false))
}

/// The std member the byte sits on: the word after a `.` or a `:` whose
/// receiver resolves to a std type that documents it.
fn std_member_at(
    source: &str,
    offset: usize,
) -> Option<(&'static str, &'static alloy::docs::Member)> {
    let (name, sigil, receiver) = alloy::docs::member_spot(source, offset)?;
    let (key, on_type) = std_receiver(source, sigil, receiver)?;
    let m = alloy::docs::member(key, name)?;

    alloy::docs::member_fits(m.kind, on_type).then_some((key, m))
}

/// The std member a hover sits on, as the doc and the example the
/// checker's type cannot carry. The source resolves the receiver where
/// it can; otherwise the type the child printed names it.
fn std_member_hover(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;
    let hit = std_member_at(&doc.source, offset).or_else(|| {
        let (name, _, _) = alloy::docs::member_spot(&doc.source, offset)?;

        alloy::docs::MEMBERS
            .iter()
            .filter(|(key, _)| names_type(value, key))
            .find_map(|(key, _)| alloy::docs::member(key, name).map(|m| (*key, m)))
    })?;

    Some(alloy::docs::member_hover(hit.0, hit.1))
}

/// Whether a printed type names `key` as a whole word.
fn names_type(text: &str, key: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(key) {
        let start = from + i;
        let end = start + key.len();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

        if !(start > 0 && word(bytes[start - 1])) && !(end < bytes.len() && word(bytes[end])) {
            return true;
        }

        from = start + 1;
    }

    false
}

/// The child's member list, with the std's doc on the items a std type
/// declares. A completion item carries the type; the doc says what the
/// member does.
fn attach_std_member_docs(result: &mut Value, doc: &Doc, line: u32, character: u32) {
    let Some(offset) = offset_of(&doc.source, line, character) else {
        return;
    };
    let head = doc.source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    if !head.ends_with(['.', ':']) {
        return;
    }

    let sigil = head.len() - 1;
    let from = head[..sigil]
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let Some((key, on_type)) = std_receiver(&doc.source, sigil, &head[from..sigil]) else {
        return;
    };
    let items = match result.get_mut("items").and_then(Value::as_array_mut) {
        Some(items) => items,

        None => match result.as_array_mut() {
            Some(items) => items,

            None => return,
        },
    };

    for item in items {
        let Some(label) = item.get("label").and_then(Value::as_str) else {
            continue;
        };
        let Some(m) = alloy::docs::member(key, label) else {
            continue;
        };
        if !alloy::docs::member_fits(m.kind, on_type) {
            continue;
        }

        item["detail"] = json!(m.signature);
        item["documentation"] = json!({
            "kind": "markdown",
            "value": alloy::docs::member_hover(key, m),
        });
    }
}

/// Whether a hover is a type alias to itself, `type Player = Player`.
/// The child writes one for a name it has no definition for.
fn restates_itself(text: &str) -> bool {
    let Some((_, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(inner) = body.trim().strip_suffix("```") else {
        return false;
    };
    let Some(rest) = inner.trim().strip_prefix("type ") else {
        return false;
    };

    match rest.split_once(" = ") {
        Some((head, value)) => head.trim() == value.trim() && !head.contains('\n'),

        None => false,
    }
}

/// The field a struct body's line declares: the name before the colon,
/// past its visibility and its modifier. `None` when the line declares
/// no field.
fn field_key(line: &str) -> Option<&str> {
    let mut text = line.trim();

    for word in ["public ", "private ", "read ", "write "] {
        text = text.strip_prefix(word).unwrap_or(text);
    }

    let name = text.split_once(':')?.0.trim();

    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

/// A parameter of a `remote` declaration. The emit writes the name as a
/// string key of the wire table, and the child answers with its length.
fn remote_parameter_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let after = doc.source[end..].trim_start();

    if !after.starts_with(':') || after.starts_with("::") {
        return None;
    }

    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let line = doc.source[line_start..line_end].trim();
    let head = line.strip_prefix("export ").unwrap_or(line);
    let rest = head.strip_prefix("remote ")?;
    let rest = rest.strip_prefix("function ").unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    // The parameter as the line writes it, its wire attribute included.
    let from = doc.source[line_start..start].rfind(['(', ','])? + line_start + 1;
    let to = parameter_end(&doc.source, end, line_end);
    let param = doc.source[from..to].trim();

    Some(format!(
        "```alloy\n{param}\n```\nA parameter of `remote {name}`."
    ))
}

/// Whether a hover is the byte length of a string, `string (5 bytes)`.
fn is_byte_count(text: &str) -> bool {
    let Some((_, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(inner) = body.trim().strip_suffix("```") else {
        return false;
    };
    let Some(rest) = inner.trim().strip_prefix("string (") else {
        return false;
    };

    match rest
        .strip_suffix(" bytes)")
        .or_else(|| rest.strip_suffix(" byte)"))
    {
        Some(count) => !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()),

        None => false,
    }
}

/// Whether the position sits on a name outside every string literal of
/// its line. The emit turns such a name into a key, and the child then
/// answers about the key's own text.
fn names_a_key(doc: &Doc, line: u32, character: u32) -> bool {
    let Some(offset) = offset_of(&doc.source, line, character) else {
        return false;
    };

    if !keywords::is_word_at(&doc.source, offset) {
        return false;
    }

    let (start, _) = keywords::word_range(&doc.source, offset);
    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let head = &doc.source[line_start..start];

    head.matches('"').count() % 2 == 0 && head.matches('\'').count() % 2 == 0
}

/// The hover of a struct field at its declaration: the line as written,
/// under the struct it belongs to, with the comment above it. `None`
/// when the word is not a field of a struct body.
/// Whether the `:` at an offset closes a ternary: a `?` stands before
/// it on the same line, outside every string. `x: T` writes a type
/// with the same byte.
fn ternary_else_at(source: &str, at: usize) -> bool {
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut quote: Option<char> = None;

    for c in source[line_start..at].chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,

            (Some(_), _) => {}

            (None, '"' | '\'' | '`') => quote = Some(c),

            (None, '?') => return true,

            _ => {}
        }
    }

    false
}

/// The three parts of a function head after its name: the type
/// parameter list, the parameter list, and the return type. The first
/// two carry their brackets; the return is the type alone.
struct Head {
    generics: Option<(usize, usize)>,
    params: (usize, usize),
    ret: Option<(usize, usize)>,
}

/// Reads `<A, B>(p: T): R` from `at`, the byte just past a function's
/// name. `None` when no parameter list follows.
fn head_spans(text: &str, at: usize) -> Option<Head> {
    let mut i = at + text[at..].len() - text[at..].trim_start().len();
    let generics = match text[i..].starts_with('<') {
        true => {
            let len = angle_len(&text[i..])?;
            let span = (i, i + len);
            i += len;

            Some(span)
        }

        false => None,
    };
    i += text[i..].len() - text[i..].trim_start().len();

    if !text[i..].starts_with('(') {
        return None;
    }

    let len = group_len(&text[i..], '(', ')')?;
    let params = (i, i + len);
    i += len;
    let after = text[i..].trim_start();
    let skip = text[i..].len() - after.len();
    let ret = match (after.strip_prefix("->"), after.strip_prefix(':')) {
        (Some(r), _) | (_, Some(r)) => {
            let mark = after.len() - r.len();
            let body = r.trim_start();
            let start = i + skip + mark + (r.len() - body.len());
            let end = body.find('\n').map_or(text.len(), |k| start + k);

            (end > start).then_some((start, end))
        }

        _ => None,
    };

    Some(Head {
        generics,
        params,
        ret,
    })
}

/// The length of the `<...>` a text opens with. An arrow's `>` closes
/// no bracket.
fn angle_len(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut last = ' ';

    for (k, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' if last != '-' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k + 1);
                }
            }
            '\n' => return None,
            _ => {}
        }

        last = c;
    }

    None
}

/// The length of the `open ... close` group a text starts with.
fn group_len(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text.char_indices() {
        match c {
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;

                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }

    None
}

/// The names a parameter list binds, in order, and whether each one
/// carries a type. `(a: T, b)` gives `[("a", true), ("b", false)]`.
fn parameter_names(list: &str) -> Vec<(String, bool)> {
    let inner = list.trim().trim_start_matches('(').trim_end_matches(')');
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();

    for (k, c) in inner.char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&inner[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    parts.push(&inner[start..]);

    for part in parts {
        let text = part.trim();

        if text.is_empty() {
            continue;
        }

        // A wire attribute stands in front of the name.
        let text = match text.starts_with('@') {
            true => text.split_once(' ').map_or("", |(_, r)| r).trim(),

            false => text,
        };
        let name: String = text
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        out.push((name, text[..].contains(':')));
    }

    out
}

/// The head of the declaration of `name`, from the name onward, and
/// whether it is `async`. `None` when no source in reach declares the
/// name exactly once: two declarations name two things.
fn declaration_head<'a>(doc: &'a Doc, name: &str) -> Option<(&'a str, bool)> {
    let mut found: Option<(&'a str, bool)> = None;

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        for line in src.lines() {
            let text = line.trim();
            let head = text.strip_prefix("export ").unwrap_or(text);
            let head = head.strip_prefix("local ").unwrap_or(head);
            let (head, is_async) = match head.strip_prefix("async ") {
                Some(rest) => (rest, true),

                None => (head, false),
            };
            let Some(rest) = head.strip_prefix("function ") else {
                continue;
            };

            if !rest.starts_with(name) {
                continue;
            }

            let after = &rest[name.len()..];

            if !after.starts_with('<') && !after.starts_with('(') {
                continue;
            }

            if found.is_some() {
                return None;
            }

            found = Some((rest, is_async));
        }
    }

    found
}

/// The head of `function name` inside `impl Owner`, from the name
/// onward. A method name repeats across impls, so the owner picks one.
fn impl_method_head<'a>(doc: &'a Doc, owner: &str, name: &str) -> Option<&'a str> {
    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        let mut inside = false;

        for line in src.lines() {
            let text = line.trim();

            if let Some(rest) = text.strip_prefix("impl ") {
                let named = rest.split(" for ").last().unwrap_or(rest).trim();
                inside = named
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .eq(owner.chars());

                continue;
            }

            if !line.starts_with([' ', '\t']) && text != "end" {
                inside = false;
            }

            if !inside {
                continue;
            }

            let head = text.strip_prefix("private ").unwrap_or(text);
            let Some(rest) = head.strip_prefix("function ") else {
                continue;
            };

            if rest.starts_with(name)
                && matches!(rest[name.len()..].chars().next(), Some('(' | '<'))
            {
                return Some(rest);
            }
        }
    }

    None
}

/// The signature the source wrote, in place of the one the checker
/// printed. A bound leaves the type parameter list and joins every use
/// of the parameter as an intersection, and a union is reordered, so
/// the print says less than the line the reader is looking at.
fn declared_signature(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = doc.source[start..end].to_string();
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;

    if body.contains('\n') || !body.contains("function ") {
        return None;
    }

    let name_end = name_end_in(body, &word)?;
    let child = head_spans(body, name_end)?;
    let owner = printed_owner(&body[..name_end], &word);
    let (source, is_async) = match owner
        .as_deref()
        .and_then(|o| impl_method_head(doc, o, &word))
    {
        Some(head) => (head, false),

        None => declaration_head(doc, &word)?,
    };
    let src = head_spans(source, word.len())?;
    let mut out = body[..name_end].to_string();

    // The source's own parameters go in when every one of them carries
    // a type and the names line up: a print with more names is another
    // function of the same name.
    let src_params = parameter_names(&source[src.params.0..src.params.1]);
    let child_params = parameter_names(&body[child.params.0..child.params.1]);
    let same = src_params.len() == child_params.len()
        && src_params
            .iter()
            .zip(&child_params)
            .all(|(a, b)| a.0 == b.0 && a.1);
    let generics = match (same, src.generics, child.generics) {
        (true, Some((a, b)), _) => Some(&source[a..b]),

        (_, _, Some((a, b))) => Some(&body[a..b]),

        _ => None,
    };

    if let Some(g) = generics {
        out.push_str(g);
    }

    out.push_str(match same {
        true => &source[src.params.0..src.params.1],

        false => &body[child.params.0..child.params.1],
    });

    let ret = src
        .ret
        .map(|(a, b)| match is_async {
            true => format!("Future<{}>", source[a..b].trim()),

            false => source[a..b].trim().to_string(),
        })
        .or_else(|| child.ret.map(|(a, b)| body[a..b].trim().to_string()));

    if let Some(ret) = ret {
        out.push_str(": ");
        out.push_str(&ret);
    }

    (out != body).then(|| format!("{fence}\n{out}\n```{tail}"))
}

/// The byte past the last occurrence of a function's name in a printed
/// head: the one a `<` or a `(` follows.
fn name_end_in(body: &str, word: &str) -> Option<usize> {
    let mut found = None;
    let mut from = 0;

    while let Some(i) = body[from..].find(word) {
        let at = from + i;
        let end = at + word.len();
        let bounded = at == 0 || !keywords::is_word_at(body, at - 1);

        if bounded && matches!(body[end..].chars().next(), Some('<' | '(')) {
            found = Some(end);
        }

        from = end;
    }

    found
}

/// The type a printed head hangs a method off: `function Item:add`
/// gives `Item`. `None` when the head names the function alone.
fn printed_owner(head: &str, word: &str) -> Option<String> {
    let rest = head.strip_suffix(word)?;
    let rest = rest.strip_suffix(['.', ':'])?;
    let name: String = rest
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    (!name.is_empty()).then(|| name.chars().rev().collect())
}

/// Luau prints a struct's value type by the name of its metatable, so a
/// generic struct loses the arguments it was given: `Slotted<T>` reads
/// `Slotted`, which no source can write. A signature that declares the
/// same parameters names them, and so does the `impl` above a `self`.
fn restore_struct_arguments(value: &str, doc: &Doc, line: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;

    if body.contains('\n') {
        return None;
    }

    // `local self: Slotted` inside `impl Slotted<T>`. A foreign impl,
    // `impl string`, types its untyped `self` from the body, so the
    // print is `any`; the `impl` head says what it is.
    if let Some((head, printed)) = body.rsplit_once(": ")
        && head.trim_end().ends_with("self")
        && let Some(named) = impl_self_type(doc, line)
        && (matches!(printed, "any" | "any?" | "unknown" | "unknown?")
            || (named.starts_with(printed) && named.len() > printed.len()))
    {
        return Some(format!("{fence}\n{head}: {named}\n```{tail}"));
    }

    let open = body.find('(')?;
    let scope = declared_type_parameters(&body[..open]);
    let rebuilt = with_struct_arguments(&body[open..], doc, &scope);

    (rebuilt != body[open..]).then(|| format!("{fence}\n{}{rebuilt}\n```{tail}", &body[..open]))
}

/// The text with every bare generic struct name given the parameters
/// the struct declares, when the scope holds all of them.
fn with_struct_arguments(text: &str, doc: &Doc, scope: &HashSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at] as char).is_alphanumeric() && bytes[at] != b'_' {
            out.push(bytes[at] as char);
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && ((bytes[at] as char).is_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }

        let word = &text[start..at];
        out.push_str(word);

        if text[at..].starts_with('<') {
            continue;
        }

        let arguments = std::iter::once(&doc.source)
            .chain(doc.import_sources.iter())
            .find_map(|src| {
                let text = struct_generics(src, word);

                (!text.is_empty()).then_some(text)
            });

        if let Some(arguments) = arguments
            && arguments
                .trim_matches(['<', '>'])
                .split(',')
                .all(|p| scope.contains(p.trim()))
        {
            out.push_str(&arguments);
        }
    }

    out
}

/// A method's receiver, when the print names the variable the call
/// went through and types it `any`. The trait or the `impl` that
/// declares the method names the type, and a std method on a primitive
/// carries it in its own first parameter.
fn name_method_receiver(value: &str, doc: &Doc, line: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let head = body.strip_prefix("function ")?;
    let open = head.find('(')?;
    let (recv, name) = head[..open].rsplit_once([':', '.'])?;
    let plain = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');

    if !plain(recv) || !plain(name) {
        return None;
    }

    let inner = &head[open + 1..];
    let first = inner.split([',', ')']).next().unwrap_or("").trim();
    let declared = first.strip_prefix("self:").unwrap_or(first).trim();
    // A receiver that starts upper case is a type already.
    let is_type = recv.starts_with(|c: char| c.is_ascii_uppercase());
    let owner = match (is_type, declared) {
        // `function Bag:is_empty(self: any)`: the head has the type.
        (true, "any") => recv.to_string(),

        (true, _) => return None,

        // `function v:upper(string)`: the parameter carries it.
        (false, d) if !d.is_empty() && d != "any" && plain(d) => d.to_string(),

        (false, _) => trait_of_method(doc, name).or_else(|| {
            (recv == "self")
                .then(|| impl_self_type(doc, line))
                .flatten()
        })?,
    };
    let rebuilt = format!(
        "function {owner}{}(self: {owner}{}",
        &head[recv.len()..open],
        &inner[first.len()..]
    );

    (rebuilt != body).then(|| format!("{fence}\n{rebuilt}\n```{tail}"))
}

/// The trait that declares a method, when one in reach does and no
/// other does.
fn trait_of_method(doc: &Doc, method: &str) -> Option<String> {
    let mut found: Option<String> = None;

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        let mut owner: Option<String> = None;

        for line in src.lines() {
            let text = line.trim();
            let head = text.strip_prefix("export ").unwrap_or(text);

            if let Some(rest) = head.strip_prefix("trait ") {
                owner = Some(
                    rest.chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect(),
                );

                continue;
            }

            if !line.starts_with([' ', '\t']) && text != "end" {
                owner = None;
            }

            let Some(name) = owner.as_ref() else {
                continue;
            };
            let Some(rest) = text.strip_prefix("function ") else {
                continue;
            };

            if rest.starts_with(method) && rest[method.len()..].starts_with('(') {
                match &found {
                    Some(other) if other != name => return None,

                    _ => found = Some(name.clone()),
                }
            }
        }
    }

    found
}

/// A trait's required method has no body, so the emit binds it as a
/// value and the print has no name for it. The trait names it.
fn name_trait_method(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let head = body.strip_prefix("function (")?;
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let owner = trait_of_method(doc, word)?;
    let rebuilt =
        format!("function {owner}.{word}({head}").replace("(self: any", &format!("(self: {owner}"));

    Some(format!("{fence}\n{rebuilt}\n```{tail}"))
}

/// A parameter the child prints as a `local`. The two differ in what a
/// reader may do to them, and the source says which this is.
fn unlocal_parameter(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let named = body.strip_prefix("local ")?;
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    if !named.starts_with(word) || !named[word.len()..].starts_with(':') {
        return None;
    }

    // A name the file declares with a keyword is that declaration.
    if doc.bindings.iter().any(|b| b.name == *word) {
        return None;
    }

    doc.source
        .lines()
        .filter_map(|l| {
            let open = l.find('(')?;

            function_name_of(&l[..open]).map(|_| l[open..].to_string())
        })
        .any(|list| parameter_names(&list).iter().any(|(n, _)| n == word))
        .then(|| format!("{fence}\n{named}\n```{tail}"))
}

/// `local rows = checked(ids)`: the child prints a solver variable for
/// the binding. The function the line calls declares what it gives
/// back, and that is the name the reader wrote.
fn name_by_declaration(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let (head, printed) = body.rsplit_once(": ")?;

    if !holds_solver_variable(printed) {
        return None;
    }

    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    if !head.ends_with(word) {
        return None;
    }

    let text = doc.source.lines().nth(line as usize)?;
    let call = text.split_once("= ")?.1.trim_start();
    let path: String = call
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    if !call[path.len()..].starts_with('(') {
        return None;
    }

    let name = path.rsplit('.').next()?;
    let owner = path.strip_suffix(name)?.strip_suffix('.');
    let source = match owner {
        Some(owner) => impl_method_head(doc, owner, name)?,

        None => declaration_head(doc, name)?.0,
    };
    let is_async = owner.is_none() && declaration_head(doc, name).is_some_and(|(_, a)| a);
    let spans = head_spans(source, name.len())?;
    let (a, b) = spans.ret?;
    let ret = source[a..b].trim();
    let scope = spans
        .generics
        .map(|(g, h)| declared_type_parameters(&source[g..h]))
        .unwrap_or_default();

    // A return that names the function's own parameters says nothing
    // about the value the call gave back.
    if scope.iter().any(|p| mentions_word(ret, p)) {
        return None;
    }

    let ret = match is_async {
        true => format!("Future<{ret}>"),

        false => ret.to_string(),
    };

    Some(format!("{fence}\n{head}: {ret}\n```{tail}"))
}

/// Whether a printed type holds a solver variable, `t1`: a name the
/// checker made and no source can write.
fn holds_solver_variable(text: &str) -> bool {
    let bytes = text.as_bytes();

    text.match_indices('t').any(|(i, _)| {
        let before = i == 0 || !keywords::is_word_at(text, i - 1);
        let digits = text[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        let after = i + 1 + digits;

        before && digits > 0 && (after >= bytes.len() || !(bytes[after] as char).is_alphanumeric())
    })
}

/// Whether a type text names a word on its own.
fn mentions_word(text: &str, word: &str) -> bool {
    let mut from = 0;

    while let Some(i) = text[from..].find(word) {
        let at = from + i;
        let end = at + word.len();
        let before = at == 0 || !keywords::is_word_at(text, at - 1);

        if before && !keywords::is_word_at(text, end.saturating_sub(1).max(end)) {
            let next = text[end..].chars().next();

            if next.is_none_or(|c| !c.is_alphanumeric() && c != '_') {
                return true;
            }
        }

        from = end;
    }

    false
}

/// A bound leaves the type parameter list at emit and joins every use
/// of the parameter as an intersection. The reader wrote neither, so
/// `Priced & T` reads as `T` when the enclosing head bounds `T`.
fn drop_bound_intersections(value: &str, doc: &Doc) -> Option<String> {
    let mut out = value.to_string();

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        for line in src.lines() {
            let Some((_, _, params)) = declared_type_parameters_of(line) else {
                continue;
            };

            for param in params {
                let Some((name, bound)) = param.split_once(':') else {
                    continue;
                };
                let (name, bound) = (name.trim(), bound.trim());

                if name.is_empty() || bound.is_empty() {
                    continue;
                }

                out = out
                    .replace(&format!("({bound} & {name})"), name)
                    .replace(&format!("({name} & {bound})"), name)
                    .replace(&format!("{bound} & {name}"), name)
                    .replace(&format!("{name} & {bound}"), name);
            }
        }
    }

    (out != value).then_some(out)
}

/// The hover of a method an `impl` writes on a foreign type. The emit
/// hangs it off a helper table the child has no name for, so the child
/// answers `any`; the source says what it is.
fn foreign_method_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let word = &doc.source[start..end];
    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let lead = doc.source[line_start..start].trim();
    let lead = lead.strip_prefix("private ").unwrap_or(lead);

    if lead != "function" || !doc.source[end..].starts_with(['(', '<']) {
        return None;
    }

    let (line, _) = position_of(&doc.source, start);
    let owner = impl_self_type(doc, line)?;
    let base = owner.split('<').next().unwrap_or(&owner);
    let declared = |name: &str| {
        doc.shapes
            .iter()
            .chain(doc.import_shapes.iter())
            .any(|s| match s {
                alloy::declarations::Shape::Struct { name: n, .. }
                | alloy::declarations::Shape::Enum { name: n, .. } => n == name,

                _ => false,
            })
    };

    // A struct or an enum the file declares reads through the child,
    // which types its methods from the class table.
    if declared(base) {
        return None;
    }

    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let rest = doc.source[end..line_end].trim_end();
    // An untyped `self` is the type the `impl` names.
    let rest = match rest.starts_with("(self)") || rest.starts_with("(self,") {
        true => rest.replacen("(self", &format!("(self: {owner}"), 1),

        false => rest.to_string(),
    };
    let rest = rest.replacen(" -> ", ": ", 1);

    Some(format!("```alloy\nfunction {owner}.{word}{rest}\n```"))
}

/// The hover of a function parameter at its declaration: the parameter
/// as the line writes it, under the function it belongs to. A parameter
/// is not a `local`, and the child has no other word for it.
fn declared_parameter_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let after = doc.source[end..].trim_start();

    // `function trim(self)` inside `impl string`: the receiver carries
    // no type, and the child reads one off the body.
    if &doc.source[start..end] == "self"
        && doc.source[..start].trim_end().ends_with('(')
        && (after.starts_with(')') || after.starts_with(','))
    {
        let (line, _) = position_of(&doc.source, start);
        let named = impl_self_type(doc, line)?;
        let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
        let owner = function_name_of(
            doc.source[line_start..start]
                .trim_end()
                .trim_end_matches('('),
        )?;

        return Some(format!(
            "```alloy\nself: {named}\n```\nA parameter of `function {owner}`."
        ));
    }

    if !after.starts_with(':') || after.starts_with("::") {
        return None;
    }

    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let line = doc.source[line_start..line_end].trim();
    // The name stands right after the `(` or the `,` of a parameter list.
    let open = doc.source[line_start..start].rfind(['(', ','])? + line_start;
    let lead = doc.source[line_start..open].trim_end();

    if !doc.source[open + 1..start]
        .trim()
        .trim_start_matches(|c: char| c == '@' || c.is_alphanumeric() || c == '_')
        .is_empty()
    {
        return None;
    }

    let owner = function_name_of(lead)?;
    let to = parameter_end(&doc.source, end, line_end);
    let param = doc.source[open + 1..to].trim();
    let head = match line.starts_with("function ") || line.contains(" function ") {
        true => format!("function {owner}"),

        false => owner,
    };

    Some(format!("```alloy\n{param}\n```\nA parameter of `{head}`."))
}

/// Where a parameter ends: the `,` or `)` that closes it at bracket
/// depth zero. A record type, `{ a: number, b: string }`, holds commas
/// of its own.
fn parameter_end(source: &str, from: usize, line_end: usize) -> usize {
    let mut depth = 0i32;
    let mut last = ' ';

    for (i, c) in source[from..line_end].char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            // The `>` of an arrow closes no bracket.
            '}' | ']' => depth -= 1,
            '>' if last != '-' => depth -= 1,
            ')' | ',' if depth == 0 => return from + i,
            ')' => depth -= 1,
            _ => {}
        }

        last = c;
    }

    line_end
}

/// The name a function declaration head names, for the text that runs
/// up to its `(`: `local function key_for` gives `key_for`. `None` when
/// the text is no function head.
fn function_name_of(lead: &str) -> Option<String> {
    let head = lead.trim();
    let head = head.strip_prefix("export ").unwrap_or(head);
    let head = head.strip_prefix("local ").unwrap_or(head);
    let head = head.strip_prefix("async ").unwrap_or(head);
    let rest = head.strip_prefix("function ")?;
    // `function get<T>` and `function M.f` both name the function.
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == ':')
        .collect();

    (!name.is_empty()).then_some(name)
}

/// The hover of a type parameter, at its declaration and at every use
/// inside the declaration that binds it. The child has no binding for
/// one, so it answers with the constructor beside it or with nothing.
fn type_parameter_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let word = &doc.source[start..end];

    if word.len() > 2 && !word.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
        return None;
    }

    // A name the file declares is that declaration, not a parameter.
    if doc.decls.iter().any(|d| d.name == word) {
        return None;
    }

    let mut at = 0;
    let mut found = None;

    // The nearest head above the cursor binds the name; an outer head
    // of the same spelling is another parameter.
    for line in doc.source.lines() {
        let line_start = at;
        at += line.len() + 1;

        if line_start > start {
            break;
        }

        let Some((owner, kind, params)) = declared_type_parameters_of(line) else {
            continue;
        };
        let Some(spelling) = params
            .iter()
            .find(|p| p.split(':').next().map(str::trim) == Some(word))
        else {
            continue;
        };

        found = Some(format!(
            "```alloy\n{spelling}\n```\nA type parameter of `{kind} {owner}`."
        ));
    }

    found
}

/// The `<A, B: Bound>` a declaration head writes: what it declares, its
/// name, and the parameters with their bounds.
fn declared_type_parameters_of(line: &str) -> Option<(String, &'static str, Vec<String>)> {
    let text = line.trim();
    let text = text.strip_prefix("export ").unwrap_or(text);
    let text = text.strip_prefix("local ").unwrap_or(text);
    let text = text.strip_prefix("async ").unwrap_or(text);
    let kinds = [
        "struct",
        "enum",
        "trait",
        "interface",
        "function",
        "impl",
        "type",
    ];
    let kind = kinds
        .into_iter()
        .find(|k| text.strip_prefix(k).is_some_and(|r| r.starts_with(' ')))?;
    let rest = text[kind.len()..].trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let open = rest[name.len()..].strip_prefix('<')?;
    let close = open.find('>')?;
    let params: Vec<String> = open[..close]
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();

    (!name.is_empty() && !params.is_empty()).then_some((name, kind, params))
}

/// The keyword of a declaration that carries fields: `struct`,
/// `interface`, or a `type` whose body is a record. Anything else has no
/// field to name.
fn declared_field_owner(decl: &alloy::declarations::Declaration) -> Option<&'static str> {
    let line = decl.hover.lines().nth(1)?.trim_start_matches("export ");

    for keyword in ["struct ", "interface ", "type "] {
        if line.starts_with(keyword) {
            let keyword = keyword.trim_end();

            // `type Name = number` names no field.
            if keyword == "type" && !line.contains('{') {
                return None;
            }

            return Some(keyword);
        }
    }

    None
}

fn declared_field_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let after = doc.source[end..].trim_start();

    if !after.starts_with(':') || after.starts_with("::") {
        return None;
    }

    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let lead = doc.source[line_start..start].trim();

    // A wire attribute, `@u16 slot: number`, stands in front of the
    // modifiers; it is part of the field the source wrote.
    if !lead
        .split_whitespace()
        .all(|w| matches!(w, "read" | "write" | "private" | "public") || w.starts_with('@'))
    {
        return None;
    }

    // The nearest record above, still open: a `struct` or an `interface`
    // with no `end` at the margin yet, or a `type` whose braces have not
    // closed. A `type` body carries fields the same way a struct does.
    let owner = doc
        .decls
        .iter()
        .filter(|d| d.offset < start && declared_field_owner(d).is_some())
        .max_by_key(|d| d.offset)?;
    let keyword = declared_field_owner(owner)?;

    if keyword == "type" {
        // The field is inside the alias body, so a brace is still open.
        let body = &doc.source[owner.offset..start];
        let depth = body.matches('{').count() as i64 - body.matches('}').count() as i64;

        if depth <= 0 {
            return None;
        }
    } else if doc.source[owner.offset..start].lines().any(|l| l == "end") {
        return None;
    }

    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let field_line = doc.source[line_start..line_end]
        .trim()
        .trim_end_matches(',')
        .trim_end();
    let mut out = format!(
        "```alloy\n{field_line}\n```\nA field of `{keyword} {}`.",
        owner.name
    );

    if let Some(comment) = alloy::declarations::doc_before(&doc.source, line_start) {
        out.push_str("\n\n");
        out.push_str(&comment);
    }

    Some(out)
}

/// Whether `offset` sits in a name a declaring keyword introduces: the
/// word before the one at the cursor is `enum`, `struct`, `function`,
/// `local`, and the rest. The name is the author's, so no list belongs
/// there, at the first column of the name as much as mid-word.
///
/// `impl` and `class` take a type, not a new name, and an `import`
/// names nothing of its own but the alias of `* as M`.
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
    ];
    let offset = offset.min(source.len());
    let bytes = source.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = offset;

    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }

    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let statement = source[line_start..offset].trim_start();
    let import = statement
        .strip_prefix("import")
        .is_some_and(|rest| !rest.starts_with(|c: char| is_word(c as u8)));

    // Every name in an `import` comes from the module; the alias of
    // `* as M` and a default binding are the author's own.
    if import {
        let head = source[line_start..start].trim_end();

        if head
            .strip_suffix("as")
            .is_some_and(|h| h.trim_end().ends_with('*'))
        {
            return true;
        }

        let mut word_end = start;

        while word_end < bytes.len() && is_word(bytes[word_end]) {
            word_end += 1;
        }

        let after_keyword = head
            .trim_start()
            .strip_prefix("import")
            .map(str::trim)
            .unwrap_or("-");

        return after_keyword.is_empty()
            && word_end > start
            && source[word_end..].trim_start().starts_with("from");
    }

    let mut end = start;

    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }

    if end == start {
        return false;
    }

    let mut word_start = end;

    while word_start > 0 && is_word(bytes[word_start - 1]) {
        word_start -= 1;
    }

    DECLARERS.contains(&&source[word_start..end])
}

/// What a built-in attribute goes on.
fn builtin_attribute_targets(key: &str) -> &'static [&'static str] {
    match key {
        "@derive" => &["struct", "enum"],

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

/// A diagnostic points at a token the reader can see. A range that
/// lands on whitespace moves to the next token of its line.
fn snap_ranges(items: &mut [Value], source: &str) {
    let space = |u: u16| u == 0x20 || u == 0x09;

    for d in items.iter_mut() {
        let Some(((sl, sc), (el, ec))) = d.get("range").and_then(range_of) else {
            continue;
        };

        if sl != el {
            continue;
        }

        let Some(line) = source.lines().nth(sl as usize) else {
            continue;
        };
        let units: Vec<u16> = line.encode_utf16().collect();
        let start = (sc as usize).min(units.len());
        let end = (ec as usize).min(units.len());

        if units[start..end].iter().any(|u| !space(*u)) {
            continue;
        }

        let Some(from) = (start..units.len()).find(|k| !space(units[*k])) else {
            continue;
        };
        // A name ends where the name ends; anything else ends at the
        // next space.
        let name =
            |u: u16| char::from_u32(u as u32).is_some_and(|c| c.is_alphanumeric() || c == '_');
        let to = match name(units[from]) {
            true => (from..units.len())
                .find(|k| !name(units[*k]))
                .unwrap_or(units.len()),

            false => (from..units.len())
                .find(|k| space(units[*k]))
                .unwrap_or(units.len()),
        };

        d["range"] = range_value((sl, from as u32), (sl, to as u32));
    }
}

/// One report per problem: identical messages at one range collapse to
/// one, and a message the checker repeats over nested ranges keeps the
/// innermost, which is the one that points at the mistake.
fn collapse_diagnostics(items: &mut Vec<Value>) {
    let mut seen: Vec<(Value, String)> = Vec::new();

    items.retain(|d| {
        let key = (
            d.get("range").cloned().unwrap_or(Value::Null),
            d.get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );

        match seen.contains(&key) {
            true => false,

            false => {
                seen.push(key);

                true
            }
        }
    });

    let spans: Vec<(String, Span)> = items
        .iter()
        .filter_map(|d| {
            Some((
                d.get("message").and_then(Value::as_str)?.to_string(),
                d.get("range").and_then(range_of)?,
            ))
        })
        .collect();

    items.retain(|d| {
        let Some(message) = d.get("message").and_then(Value::as_str) else {
            return true;
        };
        let Some(range) = d.get("range").and_then(range_of) else {
            return true;
        };

        !spans
            .iter()
            .any(|(other, span)| other == message && *span != range && covers(range, *span))
    });

    // A nil base makes every key on it unknown. `could be nil` names
    // the problem; the key report sends the reader after a typo that is
    // not there.
    let nil_lines: Vec<u32> = items
        .iter()
        .filter(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("could be nil"))
        })
        .filter_map(|d| Some(d.get("range").and_then(range_of)?.0.0))
        .collect();

    items.retain(|d| {
        let key = d
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains(": Key '"));

        !key || d
            .get("range")
            .and_then(range_of)
            .is_none_or(|r| !nil_lines.contains(&r.0.0))
    });

    // A `.` where a `:` belongs shifts every argument, so the checker
    // reports the arity and then each mismatch that follows. The one
    // sentence that names the mistake stands alone on its line.
    let typo_lines: Vec<u32> = items
        .iter()
        .filter(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains(alloy::typecheck::DOT_FOR_COLON))
        })
        .filter_map(|d| Some(d.get("range").and_then(range_of)?.0.0))
        .collect();

    items.retain(|d| {
        d.get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains(alloy::typecheck::DOT_FOR_COLON))
            || d.get("range")
                .and_then(range_of)
                .is_none_or(|r| !typo_lines.contains(&r.0.0))
    });
}

/// A range in LSP terms: the start and the end, each a line and a
/// character.
type Span = ((u32, u32), (u32, u32));

/// Whether the range holds the other one.
fn covers(outer: Span, inner: Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// Drops a child diagnostic that reports the emit rather than the source:
/// a layout lint about hoisted statements, any warning whose range
/// touches generated text, or an unused-variable lint for a name that an
/// intrinsic such as `$nameof` consumed. Errors in generated text stay;
/// they map to the construct that produced them.
fn keep_diagnostic(
    d: &Value,
    doc: &Doc,
    doc_path: Option<&Path>,
    lint_config: &alloy::config::LintConfig,
) -> bool {
    let message = d.get("message").and_then(Value::as_str).unwrap_or_default();

    // Alloy owns the unused-name lints, in the words of what the source
    // wrote; the checker's copy says the same thing twice.
    if let Some(kind) = message.split_once(": ").map(|(k, _)| k)
        && alloy::typecheck::owned_lint(kind)
    {
        return false;
    }

    // A half-typed member access leaves the checker with no name, and
    // it reports its own stand-in. The parser already names the gap.
    if crate::shapes::names_only_the_emit(message) {
        return false;
    }

    // The enum emit writes `tag` and `_1`; a report about one of those,
    // on a line the source never wrote them on, describes the emit.
    if d.pointer("/range/start/line")
        .and_then(Value::as_u64)
        .and_then(|line| doc.source.lines().nth(line as usize))
        .is_some_and(|text| {
            crate::shapes::names_the_emit_key(message, text)
                || crate::shapes::duplicate_only_in_the_emit(message, text)
        })
    {
        return false;
    }

    // The child reads the Alloy source when the compile stopped, and
    // reads none of it: every line draws a syntax error or an unknown
    // global. The compile error alone says what is wrong.
    if doc.output.is_none() {
        return false;
    }

    // `--@alloy-nocheck`, `--@alloy-ignore`, and an ignored region
    // silence the checker too. The shadow keeps the source's lines, so
    // the line is the same. The kind before the colon is the name an
    // `--@alloy-ignore-start` may carry.
    let silence = alloy::directives::scan(&doc.source);
    // `friendly_message` renames an `Unknown require` report to
    // `UnknownModule`; a region names what the author reads.
    let kind = if message.contains("Unknown require") {
        Some("UnknownModule")
    } else {
        message.split_once(": ").map(|(k, _)| k)
    };

    if !silence.is_empty()
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && !silence.allows_named(sl as usize, kind)
    {
        return false;
    }

    if message
        .to_ascii_lowercase()
        .starts_with("samelinestatement")
    {
        return false;
    }

    // `require(script.Parent)` resolves at runtime and names no path;
    // `raw_require` already says the checker cannot follow it.
    if message.contains("Unknown require")
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && alloy::typecheck::quoted_on_line(&doc.source, sl as usize).is_none()
    {
        return false;
    }

    // Past its first error the parser invents the tree, and the emit
    // copies the text through: `trait Zap` reads to the checker as a
    // call of an unknown global, and the `end` the recovery never saw
    // is a syntax error of its own. The compiler already names the
    // parse error, and the lints are off for the same reason, so only
    // a type error away from the recovery still stands.
    if let Some(out) = &doc.output
        && !out.parsed_clean
        && (message.contains("Unknown global '")
            || message.starts_with("SyntaxError")
            || d.get("severity").and_then(Value::as_u64) != Some(1))
    {
        return false;
    }

    // A line the compiler already reports on has an unreliable emit,
    // and the checker's report there describes that emit, not the code:
    // `Unknown global 'new'` under `ReservedWord`, a syntax error under
    // a half-typed statement.
    if let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out
            .diagnostics
            .iter()
            .any(|a| alloy::directives::line_of(&doc.source, a.start as usize) == sl as usize)
    {
        return false;
    }

    // An import the build reports on reads in Alloy's words, which name
    // what the module exports; the checker's report on the same line is
    // the same problem told as a missing key.
    if let Some(path) = doc_path
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && alloy::modules::import_problems_for_file(path, None, &doc.source)
            .iter()
            .any(|p| alloy::directives::line_of(&doc.source, p.start as usize) == sl as usize)
    {
        return false;
    }

    // `private_access` already says the field is private; the checker
    // says at the same place that it does not exist.
    if let Some(field) = missing_key(message)
        && let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out.lints.iter().any(|l| {
            l.name == "private_access"
                && alloy::lint::level_in(lint_config, &silence, l.name) != alloy::lint::Level::Allow
                && alloy::directives::line_of(&doc.source, l.start as usize) == sl as usize
                && l.message.contains(&format!("`{field}`"))
        })
    {
        return false;
    }

    // The emit ends a match that has no `default` arm with a nil
    // fallthrough. The checker reports that nil; `ExhaustiveMatch`
    // already reports the arm that is missing.
    if let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out.diagnostics.iter().any(|a| {
            a.message.starts_with("this match is not exhaustive")
                && alloy::directives::line_of(&doc.source, a.start as usize) <= sl as usize
                && alloy::directives::line_of(&doc.source, a.end as usize) >= sl as usize
        })
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

    // Alloy's own lint says it on the same line, in the words of what
    // the source wrote; the checker's copy would say it twice.
    if let Some(names) = message
        .split_once(": ")
        .and_then(|(kind, _)| alloy::typecheck::paired_lint(kind))
        && let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out.lints.iter().any(|l| {
            names.contains(&l.name)
                && alloy::lint::level_in(lint_config, &silence, l.name) != alloy::lint::Level::Allow
                && alloy::directives::line_of(&doc.source, l.start as usize) == sl as usize
        })
    {
        return false;
    }

    let Some(((sl, sc), (el, ec))) = d.get("range").and_then(range_of) else {
        return true;
    };

    let Some(start) = offset_of(&doc.shadow, sl, sc) else {
        return true;
    };
    let end = offset_of(&doc.shadow, el, ec).unwrap_or(doc.shadow.len());

    !(start..end.max(start + 1)).any(|o| doc.generated_offset(o))
}

/// A child message as the editor should read it: a mirror path reads as
/// the real one, and an unresolved require is an `UnknownModule` error
/// over the whole import, naming the module the source asked for.
fn friendly_message(d: &mut Value, doc: &Doc, st: &State) {
    let here = st
        .docs
        .iter()
        .find(|(_, other)| std::ptr::eq(*other, doc))
        .map(|(uri, _)| uri.clone());
    // A type inside a message reads as a hover does: the runtime's
    // names go, and a struct's private view folds to the struct.
    let known = st.known_shapes_at(here.as_deref());

    if let Some(message) = d.get("message").and_then(Value::as_str) {
        let mut folded = json!(message);
        strip_std_prefix(&mut folded);
        let text = crate::shapes::fold(folded.as_str().unwrap_or(message), &known);
        d["message"] = json!(crate::shapes::friendly_text(&text));
    }

    alloy_wording(d, doc, &known.shapes);

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

/// The messages that describe the emit, in the source's words: `new` on
/// a type alias or on a value, `is` against a name no type has, an
/// `impl` for an alias, a method called with a dot, and the arity of a
/// method call, which the source writes without `self`.
fn alloy_wording(d: &mut Value, doc: &Doc, shapes: &[alloy::declarations::Shape]) {
    let Some(message) = d.get("message").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(((sl, sc), (_, ec))) = d.get("range").and_then(range_of) else {
        return;
    };
    let Some(line) = doc.source.lines().nth(sl as usize) else {
        return;
    };
    let (kind, body) = match message.split_once(": ") {
        Some((k, rest)) if !k.contains(' ') => (k.to_string(), rest.to_string()),

        _ => ("TypeError".to_string(), message.clone()),
    };
    let span: String = line
        .chars()
        .skip(sc as usize)
        .take(ec.saturating_sub(sc) as usize)
        .collect();

    // `new Plain { }` on a type alias, `x is Alias`, `impl T for Alias`,
    // and `new n { }` on a value each emit a name the artifact never
    // binds. The compiler writes these sentences, so the terminal and
    // the editor say one thing.
    if let Some((better, at)) =
        alloy::typecheck::rewrite_emitted_name(&message, &doc.source, sl as usize + 1)
    {
        d["message"] = json!(format!("{kind}: {better}"));

        // Every method body of an `impl` reports the same name; the
        // `impl` line is where the mistake is.
        if let Some(at) = at {
            let at = at as u32 - 1;
            d["range"] = range_value((at, 0), (at, impl_width(doc, at)));
        }

        return;
    }

    // A field a struct does not have, a name declared twice, a type
    // that is a value, and an enum variant built with the wrong
    // payload: the compiler writes these sentences and puts them on the
    // token the reader wrote.
    if let Some(better) = alloy::typecheck::resite_report(
        &body,
        shapes,
        &doc.source,
        sl as usize + 1,
        byte_column(doc, sl, sc),
    ) {
        d["message"] = json!(format!("{}: {}", better.kind, better.message));

        if let Some(code) = alloy::typecheck::section_of(better.kind) {
            d["source"] = json!("Alloy");
            d["code"] = json!(code);

            if let Some(url) = alloy::docs::book_url(code) {
                d["codeDescription"] = json!({ "href": url });
            }
        }

        if let Some((line, col)) = better.at {
            let at = line as u32 - 1;
            let start = utf16_column(doc, at, col);
            let width = word_width(doc, at, start);
            d["range"] = range_value((at, start), (at, start + width));
        }

        return;
    }

    // `Damage.on(...)` where the side has no `on`: the table is the
    // remote's surface, and the source names the remote.
    if message.contains("not found in table 'Remote'")
        && let Some(key) = quoted_after(&message, "Key '")
        && let Some(name) = span.split('.').next().filter(|n| !n.is_empty())
    {
        d["message"] = json!(format!("{kind}: remote `{name}` has no `{key}`"));

        return;
    }

    // A `{ ... }` where an Array belongs: the checker answers with the
    // nineteen methods the table lacks, and the mistake is the bracket.
    if let Some(hint) = crate::shapes::plain_table_hint(&body) {
        d["message"] = json!(format!("{kind}: {hint}"));

        return;
    }

    // A `.` where a `:` belongs, and the arity of a method call the
    // source writes without `self`. The compiler writes both sentences,
    // so the terminal and the editor say one thing.
    if let Some(better) = alloy::typecheck::rewrite_dot_call(&body, line, sc as usize + 1) {
        d["message"] = json!(format!("{kind}: {better}"));

        return;
    }

    if !message.contains("Function expects ") {
        return;
    }

    // `c.bump()` on a method the checker gave a wider arity: the
    // receiver still has to go through the colon. The sentence is the
    // compiler's, so both tools read alike.
    if let Some((receiver, tail)) = span.rsplit_once('.')
        && !span.contains(':')
        && method_owner(&doc.source, tail).is_some()
    {
        let receiver = receiver
            .rsplit(['.', ' ', '(', ','])
            .next()
            .unwrap_or(receiver);
        d["message"] = json!(format!(
            "{kind}: `{tail}` is a method; call it with `{receiver}:{tail}(...)`, not `{receiver}.{tail}(...)`"
        ));

        return;
    }

    // `xs:len(1, 2)` passes the receiver through the colon, so the
    // counts the checker gives are one more than the source shows.
    if span.contains(':')
        && !span.contains('.')
        && let Some(rewritten) = without_self(&message)
    {
        d["message"] = json!(rewritten);
    }
}

/// The text between `opener` and the next quote.
fn quoted_after<'a>(message: &'a str, opener: &str) -> Option<&'a str> {
    let at = message.find(opener)? + opener.len();

    message[at..].find('\'').map(|end| &message[at..at + end])
}

/// Whether the line holds the phrase as whole words.
fn names_word(line: &str, phrase: &str) -> bool {
    line.match_indices(phrase).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + phrase.len()..].chars().next();

        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The width of a line, in UTF-16 units.
/// The zero-based UTF-16 offset a range carries as a one-based byte
/// column, which is what the compiler's rewrites read.
fn byte_column(doc: &Doc, line: u32, offset: u32) -> usize {
    let Some(text) = doc.source.lines().nth(line as usize) else {
        return offset as usize + 1;
    };
    let mut units = 0;

    for (at, c) in text.char_indices() {
        if units >= offset {
            return at + 1;
        }

        units += c.len_utf16() as u32;
    }

    text.len() + 1
}

/// A one-based byte column of a line as the zero-based UTF-16 offset a
/// range carries.
fn utf16_column(doc: &Doc, line: u32, col: usize) -> u32 {
    doc.source
        .lines()
        .nth(line as usize)
        .and_then(|l| l.get(..col.saturating_sub(1)))
        .map(|head| head.encode_utf16().count() as u32)
        .unwrap_or(col.saturating_sub(1) as u32)
}

/// The width of the name that starts at a UTF-16 offset, in UTF-16
/// units; a range with no name under it stays one unit wide.
fn word_width(doc: &Doc, line: u32, start: u32) -> u32 {
    let width = doc
        .source
        .lines()
        .nth(line as usize)
        .map(|l| {
            l.chars()
                .skip(start as usize)
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .map(|c| c.len_utf16() as u32)
                .sum::<u32>()
        })
        .unwrap_or(0);

    width.max(1)
}

fn impl_width(doc: &Doc, line: u32) -> u32 {
    doc.source
        .lines()
        .nth(line as usize)
        .map(|l| l.trim_end().encode_utf16().count() as u32)
        .unwrap_or(1)
}

/// The struct whose `impl` writes a method, when one alone writes it.
fn method_owner(source: &str, method: &str) -> Option<String> {
    let mut owner: Option<String> = None;
    let mut found: Option<String> = None;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text.strip_prefix("impl ") {
            let named = rest.split(" for ").last().unwrap_or(rest).trim();
            owner = Some(
                named
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect(),
            );

            continue;
        }

        // An impl body is indented; a line at the margin closes it.
        if !line.starts_with([' ', '\t']) && text != "end" {
            owner = None;
        }

        let Some(head) = owner.as_ref() else {
            continue;
        };
        let Some(rest) = text.strip_prefix("function ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name == method && rest[name.len()..].starts_with("(self") {
            match &found {
                Some(other) if other != head => return None,

                _ => found = Some(head.clone()),
            }
        }
    }

    found
}

/// An arity message with the receiver taken out of both counts.
fn without_self(message: &str) -> Option<String> {
    const HEAD: &str = "Function expects ";

    let at = message.find(HEAD)?;
    let rest = &message[at + HEAD.len()..];
    let expects: u32 = rest[..rest.find(' ')?].parse().ok()?;
    let but = message.find("but ")? + "but ".len();
    let tail = message[but..]
        .strip_prefix("only ")
        .unwrap_or(&message[but..]);
    let given: u32 = match tail.starts_with("none") {
        true => 0,

        false => tail[..tail.find(' ')?].parse().ok()?,
    };
    let expects = expects.checked_sub(1)?;
    let given = given.checked_sub(1)?;
    let plural = |n: u32| if n == 1 { "argument" } else { "arguments" };
    let only = if given < expects { "only " } else { "" };
    let verb = if given == 1 { "is" } else { "are" };

    Some(format!(
        "{}{HEAD}{expects} {}, but {only}{given} {verb} specified",
        &message[..at],
        plural(expects),
    ))
}

/// The payload types of a variant signature, `Msg.Move(Player, number)`
/// giving `["Player", "number"]`, split at the commas outside brackets.
/// What a `match` scrutinee resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MatchKind {
    /// An enum in scope: its variants are the arms.
    Enum(String),
    /// A `Result<T, E>`: `Ok` and `Err`.
    Result,
    /// `T[]` or `Array<T>`: the array patterns.
    Array,
    /// A string or a number: only `default` fits.
    Literal,
    /// Nothing the proxy reads.
    Unknown,
}

/// A snippet without its placeholders, for an editor that takes none.
fn plain_snippet(insert: &str) -> String {
    let mut out = String::new();
    let mut rest = insert;

    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];

        if let Some(body) = after.strip_prefix('{') {
            let end = body.find('}').unwrap_or(body.len());
            out.push_str(body[..end].split_once(':').map_or("", |(_, name)| name));
            rest = &body[(end + 1).min(body.len())..];
        } else {
            let end = after
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after.len());
            rest = &after[end..];
        }
    }

    out.push_str(rest);

    collapse_empty_arguments(&out)
}

/// `Score($1, $2)` loses both placeholders in a plain insert; the
/// separators they stood between would read as empty arguments.
fn collapse_empty_arguments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;

    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')').map(|i| open + i) else {
            break;
        };
        let inner = &rest[open + 1..close];
        out.push_str(&rest[..=open]);

        if !inner
            .trim_matches(|c: char| c == ',' || c.is_whitespace())
            .is_empty()
        {
            out.push_str(inner);
        }

        out.push(')');
        rest = &rest[close + 1..];
    }

    out.push_str(rest);

    out
}

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

/// The key a `does not have key 'balance'` message names.
fn missing_key(message: &str) -> Option<&str> {
    let at = message.find("does not have key '")? + "does not have key '".len();

    message[at..].find('\'').map(|end| &message[at..at + end])
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

    // `@` and `$` open an attribute and a macro or intrinsic, and `(`
    // an attribute's arguments: the editor asks on them only when the
    // server lists them.
    if let Some(Value::Object(completion)) = caps.get_mut("completionProvider") {
        let list = completion
            .entry("triggerCharacters")
            .or_insert_with(|| json!([]));

        if let Some(chars) = list.as_array_mut() {
            for c in ["@", "$", "("] {
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

    // Windows: `file:///C:/x` and `file:///c%3A/x` carry a leading
    // slash before the drive. One letter then a colon is a drive; a
    // longer first segment with a colon is a file name on Unix.
    let bytes = text.as_bytes();
    let drive = bytes.len() > 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/' || bytes[3] == b'\\');
    let text = if drive { text[1..].to_string() } else { text };

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
    #[test]
    fn a_declared_annotation_keeps_its_type_arguments() {
        let src = "local damaged: Signal<Player, number> = Signal.new()\nlocal n: number = 1\nlocal function f(a: { x: number, y: number }, b: string) end\n";
        assert_eq!(
            super::declared_annotation(src, "damaged", 6).map(|(_, a)| a),
            Some("Signal<Player, number>".to_string())
        );
        assert_eq!(
            super::declared_annotation(src, "n", 60).map(|(_, a)| a),
            Some("number".to_string())
        );
        assert_eq!(
            super::declared_annotation(src, "a", 90).map(|(_, a)| a),
            Some("{ x: number, y: number }".to_string())
        );
    }

    use super::*;

    /// A state with one open document, so the declarations are there.
    fn hover_of(src: &str, line: u32, character: u32, printed: &str) -> String {
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let mut text = format!("```luau\n{printed}\n```");

        for step in [
            declared_signature as fn(&str, &Doc, u32, u32) -> Option<String>,
            name_trait_method,
            name_by_declaration,
            unlocal_parameter,
        ] {
            if let Some(next) = step(&text, doc, line, character) {
                text = next;
            }
        }

        if let Some(next) = name_method_receiver(&text, doc, line) {
            text = next;
        }

        if let Some(next) = restore_struct_arguments(&text, doc, line) {
            text = next;
        }

        if let Some(next) = drop_bound_intersections(&text, doc) {
            text = next;
        }

        text.trim_start_matches("```luau\n")
            .trim_end_matches("\n```")
            .to_string()
    }

    #[test]
    fn a_bound_reads_where_the_source_wrote_it() {
        let src = concat!(
            "export trait Priced as\n",
            "    function price(self): number\n",
            "end\n",
            "\n",
            "export function cheapest<T: Priced>(a: T, b: T): T\n",
            "    return a\n",
            "end\n",
        );

        assert_eq!(
            hover_of(
                src,
                4,
                17,
                "export function cheapest<T>(a: Priced & T, b: Priced & T): T"
            ),
            "export function cheapest<T: Priced>(a: T, b: T): T"
        );
    }

    #[test]
    fn a_union_keeps_the_order_the_source_wrote() {
        let src = concat!(
            "export function describe_any(v: string | number | boolean): string\n",
            "    return \"x\"\n",
            "end\n",
        );

        assert_eq!(
            hover_of(
                src,
                0,
                17,
                "export function describe_any(v: boolean | number | string): string"
            ),
            "export function describe_any(v: string | number | boolean): string"
        );
    }

    #[test]
    fn a_generic_struct_keeps_its_arguments() {
        let src = concat!(
            "export struct Slotted<T> as\n",
            "    value: T\n",
            "end\n",
            "\n",
            "impl Slotted<T> as\n",
            "    function get(self): T\n",
            "        return self.value\n",
            "    end\n",
            "end\n",
        );

        assert_eq!(
            hover_of(src, 5, 14, "function Slotted.get<T>(self: Slotted): T"),
            "function Slotted.get<T>(self: Slotted<T>): T"
        );
        assert_eq!(
            hover_of(src, 5, 18, "local self: Slotted"),
            "self: Slotted<T>"
        );
    }

    #[test]
    fn a_trait_method_reads_with_its_name_and_its_receiver() {
        let src = concat!(
            "export trait Describable as\n",
            "    function label(self): string\n",
            "end\n",
        );

        assert_eq!(
            hover_of(src, 1, 14, "function (self: any): string"),
            "function Describable.label(self: Describable): string"
        );
        assert_eq!(
            hover_of(src, 1, 14, "function x:label(self: any): string"),
            "function Describable:label(self: Describable): string"
        );
    }

    #[test]
    fn a_parameter_hover_keeps_a_record_type_whole() {
        let src = concat!(
            "local function Stat(props: { label: string, name: string }): number\n",
            "    return 1\n",
            "end\n",
        );
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let start = src.find("props").expect("props");
        let answer = declared_parameter_hover(doc, start, start + "props".len()).expect("hover");

        assert!(
            answer.starts_with("```alloy\nprops: { label: string, name: string }\n```"),
            "{answer}"
        );
        assert!(
            answer.ends_with("A parameter of `function Stat`."),
            "{answer}"
        );

        // The `>` of an arrow closes no bracket.
        let arrows = "local function Button(props: { on_click: () -> () })\n    return 1\nend\n";
        let (st, uri) = one_file(arrows);
        let doc = st.docs.get(uri).expect("doc");
        let start = arrows.find("props").expect("props");
        let answer = declared_parameter_hover(doc, start, start + "props".len()).expect("hover");

        assert!(
            answer.starts_with("```alloy\nprops: { on_click: () -> () }\n```"),
            "{answer}"
        );
    }

    #[test]
    fn a_foreign_impl_names_its_type() {
        let src = concat!(
            "export impl string as\n",
            "    function trim(self): string\n",
            "        return self\n",
            "    end\n",
            "end\n",
        );
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let at = src.find("trim").expect("trim");

        assert_eq!(
            foreign_method_hover(doc, at, at + "trim".len()),
            Some("```alloy\nfunction string.trim(self: string): string\n```".to_string())
        );

        // A struct's own impl reads through the child.
        let own = concat!(
            "struct Item as\n",
            "    id: number\n",
            "end\n",
            "impl Item as\n",
            "    function room(self): number\n",
            "        return self.id\n",
            "    end\n",
            "end\n",
        );
        let (st, uri) = one_file(own);
        let doc = st.docs.get(uri).expect("doc");
        let at = own.find("room").expect("room");

        assert_eq!(foreign_method_hover(doc, at, at + "room".len()), None);
    }

    #[test]
    fn a_parameter_is_not_a_local() {
        let src = "export function room(count: number): number\n    return count\nend\n";

        assert_eq!(hover_of(src, 1, 12, "local count: number"), "count: number");
        // A name the file declares with a keyword keeps its keyword.
        let bound = "local total = 1\nprint(total)\n";
        assert_eq!(
            hover_of(bound, 1, 7, "local total: number"),
            "local total: number"
        );
    }

    #[test]
    fn a_binding_reads_the_type_its_call_declares() {
        let src = concat!(
            "export function checked(n: number): Result<number, string>[]\n",
            "    return []\n",
            "end\n",
            "\n",
            "local rows = checked(1)\n",
        );

        assert_eq!(
            hover_of(src, 4, 7, "local rows: t3"),
            "local rows: Result<number, string>[]"
        );
    }

    fn one_file(src: &str) -> (State, &'static str) {
        let uri = "file:///t.aly";
        let mut st = State {
            root: Some(PathBuf::from("/")),
            mirror: PathBuf::from("/m"),
            snippets: true,
            ..State::default()
        };
        st.docs.insert(
            uri.to_string(),
            Doc::new(
                src.to_string(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            ),
        );

        (st, uri)
    }

    /// A default import and an `import * as` hover as the module: the
    /// import line and the public names, not the module's table.
    #[test]
    fn a_module_import_hovers_as_the_module() {
        let dir = std::env::temp_dir().join(format!("alloy-module-hover-{}", std::process::id()));
        let pkg = dir.join("packages");
        std::fs::create_dir_all(&pkg).expect("temp dir");
        std::fs::write(
            pkg.join("fluid.luau"),
            "local m = {}\nm.__SCHEDULER_INTERFACE = {}\nfunction m.create(x) return x end\nm.mount = 1\nreturn m\n",
        )
        .expect("module");
        let src = "import fluid from \"@pkg/fluid\"\nimport { create } from \"@pkg/fluid\"\nimport * as f2 from \"./packages/fluid\"\nprint(fluid, create, f2)\n";
        let from = dir.join("main.aly");
        let aliases = vec![("pkg".to_string(), pkg.clone())];
        let hover = module_hover(src, "fluid", Some(&from), &aliases).expect("a module hover");
        assert!(
            hover.starts_with("```alloy\nimport fluid from \"@pkg/fluid\"\n```"),
            "{hover}"
        );
        assert!(hover.contains("Exports: `create`, `mount`"), "{hover}");
        // The module's own bookkeeping is no export.
        assert!(!hover.contains("__SCHEDULER_INTERFACE"), "{hover}");

        // `import * as` answers the same way, through a relative spec.
        let namespace = module_hover(src, "f2", Some(&from), &aliases).expect("a namespace hover");
        assert!(
            namespace.contains("Exports: `create`, `mount`"),
            "{namespace}"
        );

        // A name in braces is one export, not the module.
        assert_eq!(module_hover(src, "create", Some(&from), &aliases), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A word that begins a keyword drops the child's auto-imports.
    /// `end` in a guard clause and in a one-line `impl` drew
    /// `EncodingService`, which the editor sorted first.
    #[test]
    fn the_keyword_wins_over_an_auto_import() {
        let src = "impl T as end\nlocal function f(x: number?): number\n    if x == nil then return 0 end\n    return x\nend\n";
        let (st, uri) = one_file(src);
        let child = || {
            json!([
                {
                    "label": "EncodingService",
                    "kind": 7,
                    "detail": "Auto-import",
                    "sortText": "7",
                    "additionalTextEdits": [{
                        "newText": "local EncodingService = game:GetService(\"EncodingService\")\n",
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                    }],
                },
                { "label": "endsWith", "kind": 3, "detail": "Auto-import", "sortText": "7" },
                { "label": "elseif", "kind": 14, "sortText": "0" },
                { "label": "print", "kind": 3, "sortText": "4" },
            ])
        };
        let labels = |result: &Value| -> Vec<String> {
            result
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["label"].as_str().unwrap_or("").to_string())
                .collect()
        };

        // `impl T as end`, the caret past `end`.
        let mut result = child();
        st.keyword_first(uri, 0, 13, &mut result);
        assert_eq!(labels(&result), ["end"]);
        assert_eq!(result[0]["preselect"], json!(true));
        assert_eq!(result[0]["sortText"], json!("!end"));

        // `if x == nil then return 0 end`, the caret past `end`.
        let mut result = child();
        st.keyword_first(uri, 2, 33, &mut result);
        assert_eq!(labels(&result), ["end"]);
        assert_eq!(result[0]["preselect"], json!(true));

        // Half a keyword keeps the keywords it begins, and no module.
        let mut result = child();
        st.keyword_first(uri, 2, 32, &mut result);
        let mut got = labels(&result);
        got.sort();
        assert_eq!(got, ["end", "enum"]);

        // A word that begins no keyword leaves the list alone.
        let mut result = child();
        st.keyword_first(uri, 3, 12, &mut result);
        assert_eq!(labels(&result).len(), 4);
    }

    /// A hover on a std member reads the member's own section, not the
    /// type's whole page. The receiver resolves from the source: an
    /// annotation, an initializer, or the type name itself.
    #[test]
    fn a_hover_on_a_std_member_names_the_member() {
        let src = concat!(
            "local prices: HashMap<string, number> = HashMap.new()\n",
            "local price = prices:get(\"sword\")\n",
            "local xs = [ 1, 2, 3 ]\n",
            "local n = xs:len()\n",
        );
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");

        let at_new = std_member_hover("```luau\n(...)\n```", doc, 0, 49).expect("HashMap.new");
        assert!(at_new.starts_with("**HashMap.new**"), "{at_new}");

        let at_get = std_member_hover("```luau\n(...)\n```", doc, 1, 22).expect("HashMap:get");
        assert!(at_get.starts_with("**HashMap:get**"), "{at_get}");
        assert!(
            at_get.contains("```alloy"),
            "the section carries an example"
        );

        let at_len = std_member_hover("```luau\n(...)\n```", doc, 3, 14).expect("Array:len");
        assert!(at_len.starts_with("**Array:len**"), "{at_len}");
    }

    /// With no annotation the type the child printed names the receiver.
    #[test]
    fn a_printed_type_names_the_member_the_source_cannot() {
        let src = "local n = whatever:pop()\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let printed = "```luau\n(self: Queue<string>) -> string?\n```";
        let hover = std_member_hover(printed, doc, 0, 20).expect("Queue:pop");

        assert!(hover.starts_with("**Queue:pop**"), "{hover}");
    }

    /// A hover on the type name keeps the overview and lists the names.
    #[test]
    fn a_hover_on_a_std_type_lists_its_members() {
        let text = alloy::docs::type_markdown("HashMap").expect("HashMap");

        assert!(text.contains("A map with methods"), "the overview stays");
        assert!(text.contains("Members: `new`, `from`, `get`"), "{text}");
        assert!(!text.contains("|---|"), "no table is left");
    }

    /// The child's member list gains the std's doc and signature.
    #[test]
    fn a_std_member_completion_carries_its_doc() {
        let src = "local prices: HashMap<string, number> = HashMap.new()\nprices:g\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let mut result = json!([{ "label": "get" }, { "label": "nothing" }]);
        attach_std_member_docs(&mut result, doc, 1, 8);

        assert_eq!(result[0]["detail"], json!("HashMap:get(key: K): V?"));
        assert!(
            result[0]["documentation"]["value"]
                .as_str()
                .is_some_and(|v| v.starts_with("**HashMap:get**")),
            "{result}"
        );
        assert!(
            result[1].get("detail").is_none(),
            "an unknown label is left"
        );
    }

    /// An `if` expression arm, a ternary, and a `default` get the
    /// locals, the parameters, the file's own declarations, and the std
    /// names. The child's own list wins wherever it answered.
    #[test]
    fn an_expression_position_the_child_leaves_empty_gets_the_scope() {
        let src = concat!(
            "struct Round as\n",
            "    seconds: number\n",
            "end\n",
            "\n",
            "export function pick(acc: number): string\n",
            "    local many = \"many\"\n",
            "    return if acc > 0 then \"a\" else \"b\"\n",
            "end\n",
        );
        let (st, uri) = one_file(src);
        let at = src.find("then \"a\"").unwrap() + "then ".len();
        let (line, character) = position_of(src, at);
        let items = st.value_scope(uri, line, character, &json!([]));
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in ["acc", "many", "pick", "Round", "Ok", "print", "if", "not"] {
            assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
        }

        // A field of a struct is no name the caret can write bare.
        assert!(!labels.contains(&"seconds"), "{labels:?}");
        // The child answered: its list already holds the scope.
        assert!(
            st.value_scope(uri, line, character, &json!([{ "label": "print" }]))
                .is_empty()
        );
    }

    /// A scrutinee the proxy cannot resolve keeps to the variants the
    /// file declares or imports; another file's stay out.
    #[test]
    fn an_unresolved_scrutinee_offers_only_the_names_the_file_sees() {
        let src = concat!(
            "enum Phase as\n",
            "    Lobby\n",
            "    Playing\n",
            "end\n",
            "\n",
            "export function run(input: InputObject)\n",
            "    match input.KeyCode with\n",
            "        case \n",
            "    end\n",
            "end\n",
        );
        let (mut st, uri) = one_file(src);
        st.docs.insert(
            "file:///other.aly".to_string(),
            Doc::new(
                "export enum Coin as\n    Gold\n    Silver\nend\n".to_string(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            ),
        );

        let at = src.find("case \n").unwrap() + "case ".len();
        let ctx = context::detect(src, at).expect("a case context");
        let items = st.context_items(uri, at, &ctx);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in ["Lobby", "Playing", "Ok", "Err", "Enum", "default"] {
            assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
        }

        // `Coin` is another file's, and this one imports nothing.
        for name in ["Gold", "Silver"] {
            assert!(!labels.contains(&name), "`{name}` leaked: {labels:?}");
        }
    }

    /// A statement line inside a block takes `end`, and the member
    /// column of an `impl` is the only place its member words belong.
    #[test]
    fn a_statement_line_in_a_block_takes_end() {
        let src = "export function f(n: number): number\n    local x = n\n    \nend\n";
        let (st, uri) = one_file(src);
        let at = src.find("\n    \n").unwrap() + 1 + 4;
        let (line, character) = position_of(src, at);
        let items = st.primitive_completions(uri, line, character, &json!([{ "label": "print" }]));
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"end"), "{labels:?}");
    }

    const MATCH_FILE: &str = concat!(
        "enum Msg as\n",
        "    Quit\n",
        "    Join(Player)\n",
        "end\n",
        "enum Color as Red, Green end\n",
        "type Answer = Result<number, string>\n",
        "local function handle(msg: Msg, tally: number, names: string[])\n",
        "    local parsed: Result<number, string> = Ok(1)\n",
        "    local seed = Msg.Join(p)\n",
        "    local reply: Answer = Ok(2)\n",
        "    local made = Array<Msg>()\n",
        "    match msg with\n",
        "        case \n",
        "    end\n",
        "end\n"
    );

    #[test]
    fn a_scrutinee_resolves_to_a_type() {
        let (st, uri) = one_file(MATCH_FILE);
        let at = |name: &str| st.match_kind(uri, MATCH_FILE, MATCH_FILE.len(), name);
        let msg = MatchKind::Enum("Msg".to_string());

        // The annotation of a parameter, of a local, and of a const.
        assert_eq!(at("msg"), msg);
        assert_eq!(at("parsed"), MatchKind::Result);
        assert_eq!(at("names"), MatchKind::Array);
        assert_eq!(at("tally"), MatchKind::Literal);

        // The variant a local starts at.
        assert_eq!(at("seed"), msg);

        // A hover that names an enum or a `Result`.
        assert_eq!(at("Msg"), msg);
        assert_eq!(at("reply"), MatchKind::Result);

        // Anything else keeps the full list.
        assert_eq!(at("made"), MatchKind::Unknown);
        assert_eq!(at("p"), MatchKind::Unknown);
        assert_eq!(at("year % 4, year % 100"), MatchKind::Unknown);
    }

    fn case_items(st: &State, uri: &str, src: &str) -> Vec<Value> {
        let offset = src.rfind("case ").unwrap() + "case ".len();
        let ctx = context::detect(src, offset).expect("a case list");

        st.context_items(uri, offset, &ctx)
    }

    #[test]
    fn a_case_list_holds_the_arms_of_its_own_match() {
        let (st, uri) = one_file(MATCH_FILE);
        let items = case_items(&st, uri, MATCH_FILE);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();

        // The variants of `Msg` alone, then `default`. The other enum's
        // variants, `Ok`, `Err`, and `_` stay out.
        assert_eq!(labels, ["Quit", "Join", "default"]);
        assert_eq!(items[0]["textEdit"]["newText"], "Quit");
        assert_eq!(items[1]["textEdit"]["newText"], "Join($1)");
        assert_eq!(items[1]["insertTextFormat"], 2);
        assert_eq!(items[1]["detail"], "Msg.Join(Player)");
        assert!(
            items[1]["documentation"]["value"]
                .as_str()
                .unwrap()
                .contains("A variant of `enum Msg`")
        );
    }

    #[test]
    fn a_result_a_literal_and_an_array_take_their_own_arms() {
        let result = "local r: Result<number, string> = Ok(1)\nmatch r with\n    case \nend\n";
        let (st, uri) = one_file(result);
        let items = case_items(&st, uri, result);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["Ok", "Err", "default"]);
        assert_eq!(items[0]["textEdit"]["newText"], "Ok(${1:v})");
        assert_eq!(items[1]["textEdit"]["newText"], "Err(${1:e})");

        let text = "local s: string = \"a\"\nmatch s with\n    case \nend\n";
        let (st, uri) = one_file(text);
        let items = case_items(&st, uri, text);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["label"], "default");

        let array = "local xs: string[] = {}\nmatch xs with\n    case \nend\n";
        let (st, uri) = one_file(array);
        let items = case_items(&st, uri, array);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["[ first, ...rest ]", "[ ]", "default"]);
        assert_eq!(
            items[0]["textEdit"]["newText"],
            "[ ${1:first}, ...${2:rest} ]"
        );
    }

    /// `await X.m()` moves the receiver into a call the emit wrote, so
    /// the child's own mapping lands past the member.
    #[test]
    fn an_awaited_receiver_keeps_its_member_list() {
        let src = "local function f(p: Future<number>)\n    local s = await Future.all(p)\nend\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).unwrap();
        let line = 1u32;
        let column = "    local s = await Future.".len() as u32;

        // The emit wrote `__alloy.await(__alloy.Future.all(p))`, and the
        // child's mapping no longer sits after `Future.`.
        assert!(!lands_on_member(doc, line, column, "Future", '.', 0));

        let shadow_line = doc.shadow.lines().nth(line as usize).unwrap();
        let at = context::member_column(
            doc.source.lines().nth(line as usize).unwrap(),
            shadow_line,
            "Future",
            context::Access::Plain,
            '.',
            0,
            column as usize,
        )
        .expect("a member column");
        assert!(shadow_line[..at].ends_with("Future."));

        // A plain access the emit copied keeps its own position.
        let plain = "local t = { a = 1 }\nlocal v = t.a\n";
        let (st, uri) = one_file(plain);
        let doc = st.docs.get(uri).unwrap();
        assert!(lands_on_member(
            doc,
            1,
            "local v = t.".len() as u32,
            "t",
            '.',
            0
        ));
    }

    /// A remote's surface follows the file's side and the declaration.
    #[test]
    fn a_remote_offers_the_members_its_side_reaches() {
        use alloy::directives::Side;

        let src = concat!(
            "@ratelimit(10, 1)\n",
            "export remote Chat(text: string) from client\n",
            "export remote Toast(message: string) from server\n",
            "export remote function Fetch(id: number) -> number from client\n"
        );
        let chat = remote_spec(src, "Chat").expect("Chat");
        let toast = remote_spec(src, "Toast").expect("Toast");
        let fetch = remote_spec(src, "Fetch").expect("Fetch");

        assert!(chat.ratelimited);
        assert!(!toast.ratelimited);
        assert!(fetch.answers);
        assert!(!chat.answers);

        // The client fires `Chat`; the server handles it.
        assert!(chat.holds("fire", Some(Side::Client)));
        assert!(!chat.holds("on", Some(Side::Client)));
        assert!(!chat.holds("call", Some(Side::Client)));
        assert!(chat.holds("on", Some(Side::Server)));
        assert!(chat.holds("on_ratelimited", Some(Side::Server)));

        // The server fires `Toast`, and only a server fire reaches all.
        assert!(toast.holds("fire_all", Some(Side::Server)));
        assert!(!toast.holds("fire_all", Some(Side::Client)));
        assert!(toast.holds("wait", Some(Side::Client)));
        assert!(!toast.holds("on_ratelimited", Some(Side::Server)));

        // A `remote function` answers, so the firing side may call it.
        assert!(fetch.holds("call", Some(Side::Client)));
        assert!(!fetch.holds("call", Some(Side::Server)));

        // A file with no side of its own sees both surfaces.
        assert!(chat.holds("fire", None) && chat.holds("on", None));
        assert!(!chat.holds("call", None));
    }

    /// A match lowers to one expression, so a `case` binding has no
    /// local; the pattern says what it holds.
    #[test]
    fn a_case_binding_reads_its_payload() {
        const SRC: &str = "struct Boost as\n    stat: string\n    amount: number\nend\n\nenum Effect as\n    Heal(number)\n    Buff(Boost)\nend\n\nlocal function s(e: Effect): number\n    return match e with\n        case Heal(n) then n\n        case Buff(b) then b.amount\n    end\nend\nprint(s)\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let known = st.known_shapes_at(Some(uri));
        let at = |needle: &str| SRC.find(needle).expect("needle");
        let line_of = |o: usize| position_of(SRC, o).0 as usize;
        let heal = at("case Heal(n) then n") + "case Heal(".len();
        let used = at("then n\n") + "then ".len();
        let bound = at("case Buff(b)") + "case Buff(".len();
        let field = at("b.amount") + "b.".len();

        assert_eq!(
            case_binding_text(doc, line_of(heal), heal, "n", &known),
            Some("```alloy\nn: number\n```\nA binding of `Effect.Heal`.".to_string())
        );
        assert_eq!(
            case_binding_text(doc, line_of(used), used, "n", &known),
            Some("```alloy\nn: number\n```\nA binding of `Effect.Heal`.".to_string())
        );
        assert_eq!(
            case_binding_text(doc, line_of(bound), bound, "b", &known),
            Some("```alloy\nb: Boost\n```\nA binding of `Effect.Buff`.".to_string())
        );
        assert_eq!(
            case_binding_text(doc, line_of(field), field, "amount", &known),
            Some("```alloy\namount: number\n```\nA field of `struct Boost`.".to_string())
        );
    }

    /// A record field of a `type` body hovers as the line declares it.
    /// The child sees a table key and answers with an unnamed function
    /// type, which says nothing about the field.
    #[test]
    fn a_type_body_field_reads_as_it_is_written() {
        const SRC: &str = "export type HudProps = {\n    on_swing: () -> (),\n    label: string,\n}\nprint(nil :: HudProps)\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let at = SRC.find("on_swing").expect("on_swing");

        assert_eq!(
            declared_field_hover(doc, at, at + "on_swing".len()),
            Some("```alloy\non_swing: () -> ()\n```\nA field of `type HudProps`.".to_string())
        );
        let label = SRC.find("label").expect("label");

        assert_eq!(
            declared_field_hover(doc, label, label + "label".len()),
            Some("```alloy\nlabel: string\n```\nA field of `type HudProps`.".to_string())
        );
    }

    /// A `type` that names no record has no field to answer for, and a
    /// name below the closed body belongs to nothing.
    #[test]
    fn a_field_hover_stops_at_the_end_of_the_body() {
        const SRC: &str =
            "type Id = number\ntype Props = {\n    a: number,\n}\nlocal b: number = 1\nprint(b)\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let at = SRC.rfind("b: number").expect("b");

        assert_eq!(declared_field_hover(doc, at, at + 1), None);
    }

    /// The key of a struct's raw constructor names the field, past the
    /// visibility the declaration writes.
    #[test]
    fn a_field_key_reads_past_its_visibility() {
        assert_eq!(field_key("    public read id: number"), Some("id"));
        assert_eq!(field_key("    write notes: string = \"\""), Some("notes"));
        assert_eq!(field_key("end"), None);
    }

    /// A remote's parameter reads as the line declares it; the child
    /// measures the string key the emit writes for it.
    #[test]
    fn a_remote_parameter_reads_as_it_is_written() {
        const SRC: &str = "export remote PickUp(@u32 id: number, @u8 count: number) from client\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let at = SRC.find("id:").expect("id");

        assert_eq!(
            remote_parameter_hover(doc, at, at + 2),
            Some("```alloy\n@u32 id: number\n```\nA parameter of `remote PickUp`.".to_string())
        );
        let second = SRC.find("count:").expect("count");

        assert_eq!(
            remote_parameter_hover(doc, second, second + 5),
            Some("```alloy\n@u8 count: number\n```\nA parameter of `remote PickUp`.".to_string())
        );
    }

    #[test]
    fn a_byte_count_is_the_keys_own_text() {
        assert!(is_byte_count("```alloy\nstring (5 bytes)\n```"));
        assert!(is_byte_count("```luau\nstring (1 byte)\n```"));
        assert!(!is_byte_count("```alloy\nstring\n```"));
    }

    /// A hover that restates the token under the cursor says nothing.
    #[test]
    fn a_type_alias_to_itself_is_no_hover() {
        assert!(restates_itself("```alloy\ntype Player = Player\n```"));
        assert!(restates_itself("```alloy\ntype keyof<T> = keyof<T>\n```"));
        assert!(!restates_itself(
            "```alloy\ntype Profile = { name: string }\n```"
        ));
    }

    /// A file sees its own declarations and what it imports, no more.
    #[test]
    fn a_type_list_holds_what_the_file_can_write() {
        let (st, uri) = one_file(MATCH_FILE);
        let labels: Vec<String> = st
            .type_completions(uri, &[])
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect();

        assert!(labels.contains(&"Msg".to_string()));
        assert!(labels.contains(&"Answer".to_string()));
        // The std traits a bound takes, and none of the std's own
        // numbered halves.
        assert!(labels.contains(&"Display".to_string()));
        for internal in ["Iter2", "Array3", "ResultMethods2", "Awaitable"] {
            assert!(!labels.contains(&internal.to_string()), "{internal}");
        }
    }

    /// A struct literal lists the fields of its struct, and hides the
    /// private ones outside the impl.
    #[test]
    fn a_struct_literal_lists_its_own_fields() {
        let src = concat!(
            "struct Round as\n",
            "    public phase: Phase\n",
            "    private ready: number\n",
            "end\n",
            "local r = new Round { \n"
        );
        let (st, uri) = one_file(src);
        let offset = src.rfind("{ ").unwrap() + 2;
        let ctx = context::detect(src, offset).expect("a field slot");
        let items = st.context_items(uri, offset, &ctx);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["phase"]);
        assert_eq!(items[0]["textEdit"]["newText"], "phase = ${1:phase}");
    }

    /// A signature reads its parameters, `->` and all.
    #[test]
    fn a_signature_drops_its_receiver_and_names_its_arguments() {
        assert_eq!(
            drop_receiver("({ next: (any) -> number? }, (number) -> boolean) -> boolean"),
            Some("((number) -> boolean) -> boolean".to_string())
        );
        assert_eq!(
            call_snippet("earn", "(self: Profile, amount: number) -> number"),
            Some("earn(${1:self}, ${2:amount})$0".to_string())
        );
        assert_eq!(
            call_snippet("alive", "(Profile) -> boolean"),
            Some("alive(${1:Profile})$0".to_string())
        );
        assert_eq!(
            call_snippet("history", "() -> string[]"),
            Some("history()".to_string())
        );
        // A vararg fills no slot of its own.
        assert_eq!(
            call_snippet("flush", "(...any) -> { Event }"),
            Some("flush()".to_string())
        );
        assert_eq!(plain_snippet("Score($1, $2)"), "Score()");
    }

    /// A derived table pair prints no field the struct keeps private.
    #[test]
    fn a_derived_table_hides_the_private_fields() {
        let private: HashSet<String> = ["coins", "log"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            hide_record(
                "(Profile) -> { coins: number, id: number, log: string[], name: string }",
                &private
            ),
            "(Profile) -> { id: number, name: string }"
        );
    }

    #[test]
    fn an_editor_without_snippets_takes_the_plain_text() {
        let (mut st, uri) = one_file(MATCH_FILE);
        st.snippets = false;
        let items = case_items(&st, uri, MATCH_FILE);
        assert_eq!(items[1]["textEdit"]["newText"], "Join()");
        assert!(items[1].get("insertTextFormat").is_none());
        assert_eq!(
            plain_snippet("[ ${1:first}, ...${2:rest} ]"),
            "[ first, ...rest ]"
        );
    }

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
    fn a_new_name_after_a_declaring_keyword_completes_to_nothing() {
        let src = "enum Col\nlocal x = fo\nfunction hud(a\nimport x from \"./x\"\nprint(x)\n";
        assert!(declares_a_name_at(src, 8));
        assert!(declares_a_name_at(src, 6));
        assert!(!declares_a_name_at(src, 21));
        assert!(!declares_a_name_at(src, 36));
        assert!(declares_a_name_at(src, 45));
        assert!(!declares_a_name_at(src, src.len() - 2));
    }

    #[test]
    fn a_private_view_in_a_message_reads_as_the_struct() {
        let st = State::default();
        let doc = Doc::new(
            "struct Swinger as\n    private last: number\nend\n".to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let mut d = json!({
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "message": "TypeError: Type 'Swinger & Swinger__private & { last: number, scope: Scope }' does not have key 'self'",
        });
        friendly_message(&mut d, &doc, &st);
        assert_eq!(
            d["message"],
            "TypeError: Type 'Swinger' does not have key 'self'"
        );
    }

    #[test]
    fn a_hint_in_parts_folds_as_one_label() {
        let mut result = json!([{
            "position": { "line": 8, "character": 18 },
            "kind": 1,
            "label": [{ "value": ": " }, { "value": "Swinger", "location": {} }, { "value": " & " }, { "value": "Swinger__private" }, { "value": " & { last: number, scope: Scope }" }]
        }]);
        let joined = hint_label(&result[0]);
        result[0]["label"] = json!(joined);
        crate::shapes::fold_value(&mut result, &crate::shapes::Known::default());
        assert_eq!(result[0]["label"], ": Swinger");
    }

    #[test]
    fn a_private_view_hint_folds_through_the_result_path() {
        let mut result = json!([{
            "position": { "line": 8, "character": 18 },
            "kind": 1,
            "label": ": Swinger & Swinger__private & { last: number, scope: Scope }",
            "textEdits": [{ "range": { "start": { "line": 8, "character": 18 }, "end": { "line": 8, "character": 18 } }, "newText": ": Swinger & Swinger__private & { last: number, scope: Scope }" }]
        }]);
        strip_std_prefix(&mut result);
        crate::shapes::fold_value(&mut result, &crate::shapes::Known::default());
        assert_eq!(result[0]["label"], ": Swinger");
    }

    #[test]
    fn a_doc_the_child_read_is_not_added_again() {
        let src = "--- HUD Component\nexport function Hud(props: number): number\n    return props\nend\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );

        // The shadow keeps the comment, so the child's hover carries it.
        let from_child =
            "```luau\nfunction Hud(props: number): number\n```\n----------\nHUD Component";
        let restyled = restyle_hover(from_child, &doc, 1, 17).expect("restyled");
        assert_eq!(restyled.matches("HUD Component").count(), 1);
        assert!(restyled.starts_with("```alloy\nexport function Hud("));

        // A hover without the doc gets it from the binding.
        let bare = "```luau\nfunction Hud(props: number): number\n```";
        let restyled = restyle_hover(bare, &doc, 1, 17).expect("restyled");
        assert_eq!(restyled.matches("HUD Component").count(), 1);
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

    /// A mount or an alias written by hand: `\` reads as `/`, and a
    /// leading `~` is the home directory.
    #[test]
    fn a_configured_path_reads_backslashes_and_a_tilde() {
        let base = Path::new("/w");
        let home = Path::new("/home/t");
        let at = |text: &str| config_dir_from(base, text, Some(home));
        assert_eq!(at("packages\\roblox"), PathBuf::from("/w/packages/roblox"));
        assert_eq!(at("packages/roblox"), PathBuf::from("/w/packages/roblox"));
        assert_eq!(at("../shared"), PathBuf::from("/shared"));
        assert_eq!(at("~/pkg"), PathBuf::from("/home/t/pkg"));
        assert_eq!(at("~"), PathBuf::from("/home/t"));
        assert_eq!(at("~pkg"), PathBuf::from("/w/~pkg"));
        // No home: the path stays relative to the project.
        assert_eq!(
            config_dir_from(base, "~/pkg", None),
            PathBuf::from("/w/~/pkg")
        );
    }

    /// A Windows URI: the editor writes the drive as `c%3A` and puts a
    /// slash before it. The path keeps the drive and loses the slash.
    #[test]
    fn a_windows_uri_keeps_its_drive() {
        assert_eq!(
            uri_to_path("file:///c%3A/Users/a/x.aly"),
            Some(PathBuf::from("c:/Users/a/x.aly"))
        );
        assert_eq!(
            uri_to_path("file:///C:/Users/a/x.aly"),
            Some(PathBuf::from("C:/Users/a/x.aly"))
        );
        assert_eq!(
            uri_to_path("file:///c%3A/Program%20Files/x.aly"),
            Some(PathBuf::from("c:/Program Files/x.aly"))
        );
        // A path whose second byte is a colon is a drive only when the
        // colon sits right after one letter.
        assert_eq!(
            uri_to_path("file:///ab:/x.aly"),
            Some(PathBuf::from("/ab:/x.aly"))
        );
        assert_eq!(
            path_to_uri(Path::new("c:/Users/a/x.aly")),
            "file:///c:/Users/a/x.aly"
        );
        // A path the editor wrote comes back as the same path.
        for uri in [
            "file:///c%3A/Users/a/x.aly",
            "file:///c%3A/a%20b/x.aly",
            "file:///home/a/x.aly",
        ] {
            let path = uri_to_path(uri).expect("a path");
            assert_eq!(uri_to_path(&path_to_uri(&path)), Some(path), "{uri}");
        }
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
                None,
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

    /// A project root in the temp folder, with the files each test
    /// names.
    fn alias_root(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("alloy-lsp-alias-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        for (rel, text) in files {
            let path = dir.join(rel);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the folder");
            }

            std::fs::write(path, text).expect("the file");
        }

        dir
    }

    /// The alias labels an empty import path offers, `@name/` each.
    fn alias_labels(dir: &Path, root: &Path) -> Vec<String> {
        module_entries(dir, Some(root), "", "sourcemap.json")
            .into_iter()
            .map(|(label, _, _)| label)
            .filter(|l| l.starts_with('@') && l != "@self/" && l != "@game/")
            .collect()
    }

    #[test]
    fn an_import_path_offers_the_luau_config_and_the_mounts() {
        let toml = "[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n";
        let dir = alias_root(
            "merge",
            &[
                ("alloy.toml", toml),
                (
                    ".config.luau",
                    "return { luau = { aliases = { pkg = \"Packages\" } } }\n",
                ),
                ("src/server/main.aly", ""),
            ],
        );
        let src = dir.join("src/server");

        // The Luau configuration and the table both name aliases.
        assert_eq!(
            alias_labels(&src, &dir),
            vec![
                "@pkg/".to_string(),
                "@server/".to_string(),
                "@shared/".to_string()
            ]
        );

        // The same set resolves a path, so `@shared/` lists its files.
        let shared = project_aliases(&src, Some(&dir))
            .into_iter()
            .find(|(a, _)| a == "shared")
            .map(|(_, p)| p);
        assert_eq!(shared, Some(dir.join("src/shared")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_luaurc_alias_wins_over_a_mount_of_the_same_name() {
        let toml = "[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n";
        let dir = alias_root(
            "clash",
            &[
                ("alloy.toml", toml),
                (
                    ".luaurc",
                    "{ \"aliases\": { \"shared\": \"vendor/shared\" } }\n",
                ),
                ("src/a.aly", ""),
            ],
        );
        let src = dir.join("src");
        let aliases = project_aliases(&src, Some(&dir));

        assert_eq!(
            aliases,
            vec![
                ("pkg".to_string(), dir.join("Packages")),
                ("shared".to_string(), dir.join("vendor/shared")),
            ]
        );
        assert_eq!(
            alias_labels(&src, &dir),
            vec!["@pkg/".to_string(), "@shared/".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mount_aliases_off_leaves_the_luau_config_alone() {
        let toml = "[project]\nmount_aliases = false\n\n[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\n";
        let dir = alias_root(
            "off",
            &[
                ("alloy.toml", toml),
                (".luaurc", "{ \"aliases\": { \"pkg\": \"Packages\" } }\n"),
                ("src/a.aly", ""),
            ],
        );
        let src = dir.join("src");

        assert_eq!(alias_labels(&src, &dir), vec!["@pkg/".to_string()]);
        assert!(
            project_aliases(&src, Some(&dir))
                .iter()
                .all(|(a, _)| a != "server")
        );

        let _ = std::fs::remove_dir_all(&dir);
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
    // --- the comment directives ------------------------------------------------

    /// One child diagnostic, as the checker sends it.
    fn child(line: u32, message: &str, severity: u64) -> Value {
        json!({
            "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 4 } },
            "severity": severity,
            "message": message,
        })
    }

    #[test]
    fn a_region_silences_the_checkers_reports_between_its_pair() {
        let source = concat!(
            "--@alloy-ignore-start\n",
            "local a = undefined_one\n",
            "--@alloy-ignore-end\n",
            "local b = undefined_two\n",
        );
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let config = alloy::config::LintConfig::default();

        assert!(!keep_diagnostic(
            &child(1, "TypeError: Unknown global", 1),
            doc,
            None,
            &config
        ));
        assert!(keep_diagnostic(
            &child(3, "TypeError: Unknown global", 1),
            doc,
            None,
            &config
        ));
    }

    #[test]
    fn a_named_region_silences_that_kind_alone() {
        let source = concat!(
            "--@alloy-ignore-start LocalUnused\n",
            "local a = 1\n",
            "local b = 2\n",
            "--@alloy-ignore-end\n",
        );
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let config = alloy::config::LintConfig::default();

        assert!(!keep_diagnostic(
            &child(1, "LocalUnused: Variable 'a' is never used", 2),
            doc,
            None,
            &config
        ));
        assert!(keep_diagnostic(
            &child(2, "LocalShadow: Variable 'b' shadows", 2),
            doc,
            None,
            &config
        ));
    }

    #[test]
    fn a_region_reads_the_kind_the_author_sees() {
        // The child says `Unknown require`; the editor shows
        // `UnknownModule`, and the region names that.
        let source = concat!(
            "--@alloy-ignore-start UnknownModule\n",
            "local a = require(\"./gone\")\n",
            "--@alloy-ignore-end\n",
        );
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let config = alloy::config::LintConfig::default();

        assert!(!keep_diagnostic(
            &child(1, "TypeError: Unknown require: \"./gone\"", 1),
            doc,
            None,
            &config
        ));
    }

    #[test]
    fn an_unmet_expectation_carries_its_reason() {
        // The covered line must come clean, so it holds no lint of
        // its own: an unused local would meet the expectation.
        let source = "--@alloy-expect-error a negative count is refused\nlocal a = 1\nprint(a)\n";
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let items = unmet_expectations(doc, &[]);
        assert_eq!(items.len(), 1);
        let message = items[0]["message"].as_str().unwrap_or_default();
        assert!(message.contains("a negative count is refused"), "{message}");
        // The report sits on the directive's own line.
        assert_eq!(items[0]["range"]["start"]["line"], 0);

        // A directive over a line the checker reported on says nothing.
        assert!(unmet_expectations(doc, &[child(1, "TypeError: no", 1)]).is_empty());
    }

    #[test]
    fn a_lint_directive_re_levels_one_document() {
        let source = "--@alloy-lint raw_require=deny\nlocal m = require(\"./m\")\n";
        let (st, uri) = one_file(source);
        let items = st.alloy_diagnostics(uri);
        let raw = items
            .iter()
            .find(|d| {
                d["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("raw_require"))
            })
            .expect("the lint reports");
        // Denied, so the editor shows it as an error.
        assert_eq!(raw["severity"], 1);

        let silent = "--@alloy-lint raw_require=allow\nlocal m = require(\"./m\")\n";
        let (st, uri) = one_file(silent);
        assert!(
            !st.alloy_diagnostics(uri).iter().any(|d| {
                d["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("raw_require"))
            }),
            "an allowed lint still reports"
        );
    }

    #[test]
    fn preserve_keeps_the_quick_fix_off_a_line() {
        let plain = "local n = p and p.Name\n";
        let (st, uri) = one_file(plain);
        let range = ((0, 0), (1, 0));
        assert!(
            st.lint_actions(uri, range)
                .iter()
                .any(|a| a["kind"] == "quickfix"),
            "the rewrite is offered"
        );

        let kept = "--@alloy-preserve the two names read better apart\nlocal n = p and p.Name\n";
        let (st, uri) = one_file(kept);
        let range = ((0, 0), (2, 0));
        assert!(
            st.lint_actions(uri, range).is_empty(),
            "a preserved line still offers a rewrite"
        );

        // The lint still reports, and says the line is preserved.
        let message = st
            .alloy_diagnostics(uri)
            .into_iter()
            .find_map(|d| {
                d["message"]
                    .as_str()
                    .filter(|m| m.starts_with("manual_safe_access"))
                    .map(str::to_string)
            })
            .expect("the lint reports");
        assert!(message.contains("--@alloy-preserve"), "{message}");
    }

    #[test]
    fn the_directive_list_holds_every_directive() {
        let source = "--@\n";
        let (st, uri) = one_file(source);
        let items = st.directive_completions(uri, 0, 3);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in alloy::directives::NAMES {
            assert!(labels.contains(name), "`{name}` is not offered");
        }

        // `--@` asks for Alloy's own; the Luau hot comments stay out.
        assert!(!labels.iter().any(|l| l.starts_with("--!")));
    }
}

#[cfg(test)]
mod wording_tests {
    use super::*;

    #[test]
    fn a_colon_call_drops_the_receiver() {
        assert_eq!(
            drop_receiver("(Account, number) -> number").as_deref(),
            Some("(number) -> number")
        );
        assert_eq!(
            drop_receiver("({read number}) -> number?").as_deref(),
            Some("() -> number?")
        );
        assert_eq!(
            drop_receiver("<U>(read number[], (number, number) -> U) -> U[]").as_deref(),
            Some("<U>((number, number) -> U) -> U[]")
        );
    }

    #[test]
    fn a_method_arity_leaves_out_the_receiver() {
        assert_eq!(
            without_self(
                "Argument count mismatch. Function expects 1 argument, but 3 are specified"
            )
            .as_deref(),
            Some("Argument count mismatch. Function expects 0 arguments, but 2 are specified")
        );
        assert_eq!(
            without_self(
                "Argument count mismatch. Function expects 3 arguments, but only 2 are specified"
            )
            .as_deref(),
            Some("Argument count mismatch. Function expects 2 arguments, but only 1 is specified")
        );
    }

    #[test]
    fn a_private_field_leaves_the_constructor_signature() {
        let private: HashSet<String> = ["token".to_string()].into_iter().collect();
        assert_eq!(
            hide_private("({ name: string, token: string? }) -> Cfg", &private),
            "({ name: string }) -> Cfg"
        );
    }

    #[test]
    fn the_key_a_message_says_is_missing() {
        assert_eq!(
            missing_key("TypeError: Type 'Wallet' does not have key 'balance'"),
            Some("balance")
        );
        assert_eq!(missing_key("TypeError: something else"), None);
    }

    #[test]
    fn an_emit_slot_is_no_parameter_hint() {
        assert!(emit_slot_hint(&json!({ "label": "_1:" })));
        assert!(emit_slot_hint(&json!({ "label": "_12:" })));
        assert!(!emit_slot_hint(&json!({ "label": "amount:" })));
        assert!(!emit_slot_hint(&json!({ "label": "_:" })));
    }

    #[test]
    fn a_type_the_source_cannot_write() {
        assert!(writable_type("number[]"));
        assert!(!writable_type("(@checked (string) -> string)?"));
        assert!(!writable_type("t1 where t1 = { }"));
        assert!(!writable_type("*error-type*"));
    }

    #[test]
    fn a_range_on_whitespace_moves_to_the_next_token() {
        let source = "impl Shape for Alias as\n    function area(self): number\n";
        let mut items = vec![json!({
            "range": { "start": { "line": 1, "character": 12 }, "end": { "line": 1, "character": 13 } },
            "message": "x",
        })];
        snap_ranges(&mut items, source);
        assert_eq!(range_of(&items[0]["range"]), Some(((1, 13), (1, 17))));
    }

    #[test]
    fn one_report_per_problem() {
        let one = json!({
            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 5 } },
            "message": "expected `end`",
        });
        let wide = json!({
            "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 3, "character": 9 } },
            "message": "expected `end`",
        });
        let mut items = vec![one.clone(), one.clone(), wide];
        collapse_diagnostics(&mut items);
        assert_eq!(items, vec![one]);
    }

    #[test]
    fn a_method_finds_the_impl_that_writes_it() {
        let source = "impl Counter as\n    function bump(self): number\n        return 1\n    end\n\n    function make(): Counter\n    end\nend\n";
        assert_eq!(method_owner(source, "bump").as_deref(), Some("Counter"));
        assert_eq!(method_owner(source, "make"), None);
    }
}
