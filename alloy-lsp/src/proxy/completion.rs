//! Completion: the context items, the std and keyword lists, member lists, auto-imports, and the value scope a position sees.

use super::documents::{normalize, project_aliases};
use super::hints::writable_type;
use super::hover::{builtin_attribute_targets, declared_attribute_targets, remote_spec};
use super::navigation::module_file_of;
use super::*;

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
    pub(crate) fn mark_declarations(&self, result: &mut Value) {
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
    pub(crate) fn std_completions(
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
    pub(crate) fn value_scope(
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
    pub(crate) fn type_completions(&self, uri: &str, labels: &[&str]) -> Vec<Value> {
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
    pub(crate) fn enum_variants(
        &self,
        uri: &str,
        name: &str,
    ) -> Vec<&alloy::declarations::Declaration> {
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
    pub(crate) fn match_kind(
        &self,
        uri: &str,
        source: &str,
        offset: usize,
        scrutinee: &str,
    ) -> MatchKind {
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
    pub(crate) fn kind_of_arms(&self, uri: &str, source: &str, offset: usize) -> MatchKind {
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
    pub(crate) fn kind_of_type(&self, uri: &str, text: &str) -> MatchKind {
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
    pub(crate) fn kind_of_name(&self, uri: &str, name: &str) -> MatchKind {
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
    pub(crate) fn kind_of_value(&self, uri: &str, text: &str) -> MatchKind {
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
    pub(crate) fn decls_in_scope(&self, uri: &str) -> Vec<&alloy::declarations::Declaration> {
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
    pub(crate) fn struct_fields(&self, uri: &str, name: &str, inside: bool) -> Vec<context::Field> {
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
    pub(crate) fn value_type(&self, source: &str, offset: usize, name: &str) -> Option<String> {
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
    pub(crate) fn field_type(&self, uri: &str, owner: &str, field: &str) -> Option<String> {
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
    pub(crate) fn filter_remote_members(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
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
    pub(crate) fn context_items(
        &self,
        uri: &str,
        offset: usize,
        ctx: &context::Context,
    ) -> Vec<Value> {
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

            Context::ImportHead {
                prefix,
                type_only,
                spec,
            } => {
                let from = offset - prefix.len();

                // `import | from "./m"`: the module's default binds
                // here, under any name. Its own name reads best.
                if !*type_only
                    && let Some(spec) = spec
                    && let Some(default) = self
                        .exports_of_target(self.resolve_spec(uri, spec))
                        .into_iter()
                        .find(|e| e.is_default)
                {
                    items.push(word(
                        &default.name,
                        default.kind,
                        Some(format!("The default export of `{spec}`.")),
                        from,
                    ));
                }

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

                    let exports = self.exports_of_target(resolved);

                    for e in &exports {
                        if *type_only && !e.is_type {
                            continue;
                        }

                        // The default is not a name in braces; a bare
                        // `import X from` reads it.
                        if e.is_default {
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

                for (label, kind, detail) in
                    module_entries(dir, self.root.as_deref(), head, sourcemap, Some(&path))
                {
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
}

impl Server {
    /// Answers a completion inside an attribute, a macro call, a remote's
    /// side, or an import, where the child would list globals.
    pub(crate) fn context_completion(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        let (extra, incomplete) = st.ingot_items(uri, line, character, trigger);
        items.extend(extra);
        drop(st);
        let result = match incomplete {
            true => json!({ "isIncomplete": true, "items": items }),

            false => json!(items),
        };
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }
}

impl State {
    /// The file a spec names from the file at `uri`: `@alias/x` goes
    /// through the project's aliases, and a relative spec is path
    /// arithmetic.
    pub(crate) fn resolve_spec(&self, uri: &str, spec: &str) -> Option<PathBuf> {
        let path = uri_to_path(uri)?;
        let dir = path.parent()?;

        match spec.strip_prefix('@') {
            Some(rest) => {
                let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));

                project_aliases(dir, self.root.as_deref())
                    .into_iter()
                    .find(|(a, _)| a == alias)
                    .map(|(_, base)| imports::lexical(&base, tail))
            }

            None => Some(imports::lexical(dir, spec)),
        }
    }

    /// What the module at a resolved path exports. An open document
    /// answers first; else the file on disk, which a plain Luau module
    /// in a package is.
    pub(crate) fn exports_of_target(&self, resolved: Option<PathBuf>) -> Vec<imports::Export> {
        let mut exports: Vec<imports::Export> = Vec::new();
        let Some(resolved) = resolved else {
            return exports;
        };
        let target = imports::module_path(&resolved);

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

        exports
    }

    /// Auto-import items for a completion at a source position.
    pub(crate) fn auto_imports(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
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

    /// The keyword wins while the typed word begins one.
    ///
    /// `end` in `if x then return end`, and in a one-line `struct T as
    /// end`, drew `EncodingService` from the child's auto-imports: the
    /// editor matched the letters and sorted the module first. So while
    /// the word begins a keyword the list drops every auto-import and
    /// every label the word does not begin, holds the keywords the word
    /// begins, and an exact keyword takes the first row.
    pub(crate) fn keyword_first(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
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

        // The word is the whole keyword and the list holds it alone. An
        // item the reader has already typed asks for an accept and
        // writes nothing, so the empty list closes the popup instead.
        if items.len() == 1 {
            items.clear();
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
    pub(crate) fn rewrite_child_auto_imports(&self, uri: &str, result: &mut Value) {
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
            // An Alloy module binds whole under `* as`; a bare name
            // would read its `export default`. A plain Luau module
            // returns one value, which is what a bare name takes.
            let is_alloy = file.extension().is_some_and(|e| e == "aly" || e == "alx");
            let edit = match is_alloy {
                true => imports::namespace_import_edit(&source, &spec, &name),

                false => imports::import_edit(
                    &source,
                    &spec,
                    &imports::Export {
                        name: name.clone(),
                        is_type: false,
                        is_default: true,
                        kind: 9,
                    },
                ),
            };
            item["label"] = json!(name);
            item["detail"] = json!(spec);
            item["insertText"] = json!(name);
            item["additionalTextEdits"] = json!([edit]);

            true
        });
    }
}

/// Whether an item is the child's auto-import of a module: it inserts a
/// `require`, so its detail is the instance path of a module file.
pub(crate) fn is_module_auto_import(item: &Value) -> bool {
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

/// Removes the runtime's table from a type text, in every string of the
/// value: `__alloy.Future<T>` becomes `Future<T>`, and the primitive
/// helper `__alloy_string.trim` becomes `string.trim`.
/// Where a type name sorts in the list of a slot. `extends` and the
/// trait of an `impl` take a contract; `impl X` and the target after
/// `for` take a struct, an enum, or a class. The other names stay in
/// the list: the author may be about to declare one.
pub(crate) fn type_rank(prefers: context::Prefers, detail: &str) -> u8 {
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
pub(crate) fn set_call(item: &mut Value, label: &str, detail: &str, snippets: bool) {
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
pub(crate) fn call_snippet(label: &str, detail: &str) -> Option<String> {
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
pub(crate) fn hide_record(detail: &str, private: &HashSet<String>) -> String {
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
pub(crate) fn lands_on_member(
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
pub(crate) fn sep_of(source: &str, offset: usize) -> Option<char> {
    let head = &source[..offset.min(source.len())];
    let word = head.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    word.chars().next_back().filter(|c| matches!(c, '.' | ':'))
}

/// The separator a member access at the caret uses, `.` or `:`, when
/// the caret sits in a member name. `None` anywhere else.
pub(crate) fn member_position(doc: &Doc, line: u32, character: u32) -> Option<char> {
    let text = doc.source.lines().nth(line as usize)?;
    let head: String = text.chars().take(character as usize).collect();
    let word = head.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    let sep = word.chars().next_back()?;

    (matches!(sep, '.' | ':') && !word.ends_with("::")).then_some(sep)
}

/// A signature without the receiver a colon passes: `(Account, number)
/// -> number` reads `(number) -> number`.
pub(crate) fn drop_receiver(detail: &str) -> Option<String> {
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
pub(crate) fn private_fields(doc: &Doc) -> HashSet<String> {
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
pub(crate) fn hide_private(detail: &str, private: &HashSet<String>) -> String {
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

/// The entries a module path can continue with: the project's aliases
/// and `@self` when nothing is typed, the children of
/// the sourcemap under `@game/`, and otherwise the directories and the
/// modules of the resolved directory. Each is `(label, kind, detail)`.
pub(crate) fn module_entries(
    dir: &Path,
    root: Option<&Path>,
    head: &str,
    sourcemap: &str,
    own: Option<&Path>,
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

        // A module never imports itself.
        if own.is_some_and(|own| normalize(&path) == normalize(own)) {
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

        // A `.server` or `.client` file is a script, not a module: it
        // returns nothing, and Roblox runs it on its own.
        if let Some(stem) = stem
            && stem != "init"
            && !alloy::modules::is_script(&name)
            && seen.insert(stem.to_string())
        {
            out.push((stem.to_string(), 9, name.clone()));
        }
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));

    out
}

/// The payload types of a variant signature, `Msg.Move(Player, number)`
/// giving `["Player", "number"]`, split at the commas outside brackets.
/// What a `match` scrutinee resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchKind {
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
pub(crate) fn plain_snippet(insert: &str) -> String {
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
pub(crate) fn collapse_empty_arguments(text: &str) -> String {
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

pub(crate) fn payload_types(signature: &str) -> Vec<String> {
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
