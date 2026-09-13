use super::*;

impl State {
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

        let answer = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array());
        let answered = answer.is_some_and(|items| !items.is_empty());
        // An auto-import offers a name the file does not hold yet, so it
        // names no binding the caret can write. A package module called
        // `Signal` would otherwise hide the std `Signal`, and the child's
        // row for it drops later, which left the name in no list at all.
        // The clean pass drops the auto-import once the std name is here.
        let labels: Vec<&str> = answer
            .map(|items| {
                items
                    .iter()
                    .filter(|i| !is_auto_import(i))
                    .filter_map(|i| i["label"].as_str())
                    .collect()
            })
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
        if !answered {
            return Vec::new();
        }

        let mut items: Vec<Value> = Vec::new();
        items.extend(
            alloy::desugar::AMBIENT
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
                }),
        );

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

    /// The type names for an annotation: every struct, interface, enum,
    /// trait, and type alias of the workspace, the std types, and the
    /// primitives.
    pub(crate) fn type_completions(&self, uri: &str, labels: &[&str]) -> Vec<Value> {
        let mut items = Vec::new();
        let mut seen: HashSet<String> = labels.iter().map(|l| l.to_string()).collect();
        let folded = self.folded_names();
        let mut push = |name: &str, kind: u64, detail: &str, doc_text: Option<String>| {
            if !is_internal_name(name) && !folded.contains(name) && seen.insert(name.to_string()) {
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
                Some(7)
            } else if head.contains("interface ") || head.contains("trait ") {
                Some(8)
            } else if head.contains("enum ") {
                Some(13)
            } else if head.contains("type ") {
                Some(7)
            } else {
                None
            };

            // The kind first, then the name: `struct Profile` says what
            // the slot takes, where `Profile` alone says nothing.
            if let Some(kind) = kind
                && let Some(detail) = declaration_detail(&d.hover)
            {
                push(&d.name, kind, &detail, Some(d.hover.clone()));
            }
        }

        // The type parameters the file declares: `<T: Keyed>` puts `T`
        // in every type slot of that head and its body.
        if let Some(doc) = self.docs.get(uri) {
            for name in declared_type_parameters(&doc.source) {
                push(&name, 25, "type parameter", None);
            }
        }

        // The list the parser marks as ambient in a type slot, so the
        // two stay in step.
        for name in alloy::desugar::AMBIENT_TYPES {
            push(
                name,
                7,
                "alloy:std",
                alloy::docs::type_markdown(name)
                    .or_else(|| keywords::doc(name).map(str::to_string)),
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
            .filter(|name| !is_flat_enum(name))
        {
            push(name, 7, "roblox", None);
        }

        // Luau's type functions. `typeof` reads an expression in
        // parentheses; every other one takes type arguments in angle
        // brackets, `keyof<T>`.
        for (name, what) in TYPE_FUNCTIONS {
            if !seen.insert(name.to_string()) {
                continue;
            }

            let (open, close) = match name {
                "typeof" => ('(', ')'),

                _ => ('<', '>'),
            };
            let mut item = json!({
                "label": name,
                "kind": 3,
                "detail": "Luau type function",
                "documentation": { "kind": "markdown", "value": what },
            });

            // The accept writes the brackets and leaves the caret
            // between them: the argument is what the author writes next.
            match self.snippets {
                true => {
                    item["insertText"] = json!(format!("{name}{open}$1{close}"));
                    item["insertTextFormat"] = json!(2);
                }

                false => item["insertText"] = json!(format!("{name}{open}{close}")),
            }

            items.push(item);
        }

        items
    }
}

/// The type functions Luau's solver holds, with what each one reads.
/// `union` and `intersect` are not among them: the checker reports
/// `Unknown type 'union'`.
pub(crate) const TYPE_FUNCTIONS: [(&str, &str); 7] = [
    ("typeof", "The type of an expression."),
    ("keyof", "The keys of a table type, as a union of strings."),
    ("rawkeyof", "The keys of a table type, metatable aside."),
    ("index", "The type one key of a table type holds."),
    ("rawget", "The type one key holds, metatable aside."),
    ("setmetatable", "A table type with a metatable on it."),
    ("getmetatable", "The metatable of a type."),
];

/// Whether the name is the definitions file's own spelling of a Roblox
/// enum, `EnumUserInputType`. luau-lsp keeps those out of the type
/// scope, so the flat name never compiles; `Enum.UserInputType` is the
/// only spelling a type slot takes. `Enum`, `EnumItem`, and `Enums` are
/// datatypes of their own and stay.
pub(crate) fn is_flat_enum(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("Enum") else {
        return false;
    };
    // The definitions file writes the table of one enum's items as
    // `EnumUserInputType_INTERNAL`, which is no type either.
    let rest = rest.strip_suffix("_INTERNAL").unwrap_or(rest);

    !rest.is_empty() && rest != "Item" && rest != "s"
}

/// The Roblox enum names, as `Enum.UserInputType` spells them. The
/// definitions file names each one `EnumUserInputType`, so the list
/// comes from the datatypes with that prefix off.
pub(crate) fn roblox_enum_names() -> Vec<&'static str> {
    alloy::roblox_classes::DATATYPES
        .iter()
        .copied()
        .filter(|name| is_flat_enum(name) && !name.ends_with("_INTERNAL"))
        .filter_map(|name| name.strip_prefix("Enum"))
        .collect()
}

/// The std type a member position reads, with whether the receiver is
/// the type itself. `HashMap.` names the type; `prices.` names a value.
pub(crate) fn std_member_receiver(
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<(&'static str, bool)> {
    let offset = offset_of(&doc.source, line, character)?;
    let head = doc.source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    if !head.ends_with(['.', ':']) {
        return None;
    }

    let sigil = head.len() - 1;
    let from = head[..sigil]
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);

    std_receiver(&doc.source, sigil, &head[from..sigil])
}

/// The member list of a std type. A dangling `.` stops the parse, so
/// the artifact falls back to the Alloy source, where the child knows
/// no `HashMap` and answers nothing. The std table holds the names
/// either way, and it drops what the sigil cannot call: a static takes
/// no receiver, a method takes one.
pub(crate) fn complete_std_members(
    doc: &Doc,
    line: u32,
    character: u32,
    snippets: bool,
    result: &mut Value,
) {
    let Some((key, on_type)) = std_member_receiver(doc, line, character) else {
        return;
    };

    if result.is_null() {
        *result = json!([]);
    }

    let items = match result.get_mut("items").and_then(Value::as_array_mut) {
        Some(items) => items,

        None => match result.as_array_mut() {
            Some(items) => items,

            None => return,
        },
    };

    items.retain(|i| {
        let Some(label) = i.get("label").and_then(Value::as_str) else {
            return true;
        };

        match alloy::docs::member(key, label) {
            Some(m) => alloy::docs::member_fits(m.kind, on_type),

            // A name the std table does not document is the child's to
            // answer for.
            None => true,
        }
    });

    let held: HashSet<&str> = items
        .iter()
        .filter_map(|i| i.get("label").and_then(Value::as_str))
        .collect();
    let mut extra = Vec::new();

    for m in alloy::docs::members(key) {
        if held.contains(m.name) || !alloy::docs::member_fits(m.kind, on_type) {
            continue;
        }

        let kind = match m.kind {
            alloy::docs::MemberKind::Method => 2,
            alloy::docs::MemberKind::Static => 3,
            alloy::docs::MemberKind::Field => 5,
            alloy::docs::MemberKind::Constant => 21,
        };
        let mut item = json!({
            "label": m.name,
            "kind": kind,
            "detail": m.signature,
            "documentation": {
                "kind": "markdown",
                "value": alloy::docs::member_hover(key, m),
            },
        });
        // The signature reads `HashMap.new<K, V>(): HashMap<K, V>`; the
        // call the list inserts starts after the name.
        let sigil = match m.kind {
            alloy::docs::MemberKind::Method => ':',

            _ => '.',
        };

        if let Some(rest) = m.signature.strip_prefix(&format!("{key}{sigil}{}", m.name)) {
            set_call(&mut item, m.name, rest, snippets);
        }

        extra.push(item);
    }

    items.extend(extra);
}
