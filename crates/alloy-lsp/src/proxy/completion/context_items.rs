use super::*;

impl State {
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
            Context::Attribute {
                sigil,
                target,
                bare,
                ..
            } => {
                // Only the attributes that go on what the position
                // names. With nothing under the caret to carry one, the
                // list holds the attributes that go anywhere: every
                // other one names a target the reader has not written.
                // `attribute X on function` covers a method too, so a
                // method position takes what a function takes.
                let fits = |targets: &[&str]| match target {
                    Some("method") => targets.contains(&"method") || targets.contains(&"function"),

                    Some(t) => targets.contains(t),

                    None if *bare => targets.is_empty(),

                    None => true,
                };

                for key in keywords::keys_with_prefix("@") {
                    let ok = match (target, *bare) {
                        (None, true) => OPEN_ATTRIBUTES.contains(&key),

                        // The compiler reports `@test` on a method:
                        // the runner calls a test by name, and a method
                        // takes a receiver.
                        (Some("method"), _) if key == "@test" => false,

                        _ => fits(builtin_attribute_targets(key)),
                    };

                    if ok {
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
                        let mut item = word(&d.name, 7, Some(d.hover.clone()), *sigil);

                        if let Some(detail) = declaration_detail(&d.hover) {
                            item["detail"] = json!(detail);
                        }

                        items.push(item);
                    }
                }
            }

            /*
            An entry of an attribute argument. The declared type of the
            parameter says what fits: a union of string literals offers
            its members, and an enum offers its variants.

            An argument is a literal the compiler reads, so nothing from
            the scope belongs here. A parameter with no type to read
            offers nothing, which is what the built-in attributes had.
            */
            Context::AttributeArg {
                prefix,
                attr,
                param,
                quote,
            } => {
                let from = offset - prefix.len();
                let key = format!("@{attr}");
                // The declaration is this file's, another open file's, or
                // one an import reaches before the workspace pass has
                // opened the module it lives in.
                let params = self
                    .decls_in_scope(uri)
                    .into_iter()
                    .find(|d| d.name == key)
                    .or_else(|| doc.import_decls.iter().find(|d| d.name == key))
                    .map(|d| alloy::declarations::attribute_params(&d.hover))
                    .unwrap_or_default();
                let ty = match param {
                    Some(name) => params.iter().find(|(p, _)| p == name).map(|(_, t)| t),

                    None => params.first().map(|(_, t)| t),
                };

                if let Some(ty) = ty {
                    let element = element_type(ty);

                    for (label, insert, detail) in
                        self.literal_items(uri, &element, *quote, attr.as_str())
                    {
                        let mut item = word(&label, 21, None, from);
                        item["textEdit"]["newText"] = json!(insert);
                        item["detail"] = json!(detail);
                        item["sortText"] = json!("0");
                        items.push(item);
                    }
                }

                // `@validate(` takes the validator of the remote under
                // it, and a parameter of a function type takes a
                // function of that shape: one snippet, the way the
                // child offers a handler to `Connect(`.
                let function = match attr.as_str() {
                    "validate" => remote_below(&doc.source, offset).map(|(name, params)| {
                        (
                            validator_params(&params),
                            format!("the validator for {name}"),
                            keywords::doc("@validate").and_then(doc_sentence),
                        )
                    }),

                    _ => ty
                        .filter(|t| t.contains("->"))
                        .map(|t| (payload_types(t), format!("takes `{t}` for `@{attr}`"), None)),
                };

                if let Some((params, detail, doc_text)) = function {
                    let names: Vec<&str> = params
                        .iter()
                        .map(|p| p.split_once(':').map_or(p.as_str(), |(n, _)| n).trim())
                        .collect();
                    let label = format!("function({}) ... end", names.join(", "));
                    let insert = format!("function({})\n\t$0\nend", params.join(", "));
                    items.push(snippet(&label, &insert, 15, &detail, doc_text, from));
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
                        let mut item = word(&d.name, 3, Some(d.hover.clone()), *sigil);

                        if let Some(detail) = declaration_detail(&d.hover) {
                            item["detail"] = json!(detail);
                        }

                        items.push(item);
                    }
                }

                // `import { logit as log }`: the alias is the only name
                // this file can write for the macro.
                for (bound, d) in self.aliased_macros(uri) {
                    let label = format!("${bound}");

                    if !seen.insert(label.clone()) {
                        continue;
                    }

                    let mut item = word(&label, 3, Some(d.hover.clone()), *sigil);

                    if let Some(detail) = declaration_detail(&d.hover) {
                        item["detail"] = json!(detail);
                    }

                    items.push(item);
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
                sigil,
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

                // `import { X } from "@game"`: the names in braces are
                // the Roblox services. The old spelling still opens the
                // same list.
                let every_service = matches!(
                    spec.as_deref().and_then(alloy::game_import::game_path),
                    Some(alloy::game_import::GamePath::Every)
                );

                if every_service && !*type_only && !*sigil {
                    for name in alloy::roblox_services::SERVICES {
                        let mut item = word(
                            name,
                            9,
                            Some(alloy::game_import::service_summary(name)),
                            from,
                        );
                        item["detail"] = json!(format!("game:GetService(\"{name}\")"));
                        items.push(item);
                    }

                    return items;
                }

                let data_format = spec.as_deref().and_then(alloy::data::Format::of);

                // A data file exports no type, and `@` already says the
                // entry names an attribute.
                if !*type_only && !*sigil && data_format.is_none() {
                    items.push(word(
                        "type",
                        14,
                        Some("A type-only name in a value import.".to_string()),
                        from,
                    ));
                }

                // What one export reads as in this list, or `None`
                // for an export the list does not take. The default is
                // not a name in braces; a bare `import X from` reads
                // it. An `@` already written asks for the module's
                // attributes alone, and an attribute is a value, so a
                // type-only list holds none.
                let listed = |e: &imports::Export| -> Option<String> {
                    let dropped = e.is_default
                        || (*type_only && !e.is_type)
                        || (*sigil && !e.is_attribute)
                        || (*type_only && e.is_attribute);

                    match dropped {
                        true => None,

                        false => Some(match e.is_type && !*type_only {
                            true => format!("type {}", e.name),

                            false => e.written(),
                        }),
                    }
                };

                // `import { |` with no `from` yet: every name the
                // project exports, each under the module it comes
                // from. The accept writes the `from` clause, so the
                // reader picks the name and the line finishes itself.
                if spec.is_none() {
                    let (start, end, close) = from_clause_at(&doc.source, offset);

                    for (spec, e) in self.project_exports(uri, prefix) {
                        let Some(label) = listed(&e) else {
                            continue;
                        };
                        let clause = format!("{close} from \"{spec}\"");
                        let mut item = word(&label, e.kind, None, from);
                        item["detail"] = json!(format!("from \"{spec}\""));

                        // One edit writes the name and the clause where
                        // the clause starts at the caret. Two edits at
                        // one offset have no order, so the text they
                        // leave would be the client's guess.
                        if start == offset {
                            item["textEdit"] = json!({
                                "range": range_value(
                                    position_of(&doc.source, from),
                                    position_of(&doc.source, end),
                                ),
                                "newText": format!("{label}{clause}"),
                            });
                        } else {
                            item["additionalTextEdits"] = json!([{
                                "range": range_value(
                                    position_of(&doc.source, start),
                                    position_of(&doc.source, end),
                                ),
                                "newText": clause,
                            }]);
                        }

                        items.push(item);
                    }

                    return items;
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
                            && !*sigil
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

                    let exports = self.exports_of_target(resolved.clone());
                    let decls = self.decls_of_target(resolved);

                    for e in &exports {
                        let Some(label) = listed(e) else {
                            continue;
                        };
                        // The module's own declaration says what the
                        // name is, so the list reads `struct Profile`.
                        let found = decls.iter().find(|d| d.name == e.name);
                        let mut item = word(&label, e.kind, found.map(|d| d.hover.clone()), from);

                        if let Some(detail) = found.and_then(|d| declaration_detail(&d.hover)) {
                            item["detail"] = json!(detail);
                        }

                        items.push(item);
                    }
                }
            }

            // A finished statement wants no list: after the closing quote
            // of an import path, Enter is a newline.
            Context::Nothing => {}

            Context::TypeSlot { prefix, prefers } => {
                let from = offset - prefix.len();

                // `local p: Math.|`: the types of that namespace, and
                // nothing else. Luau has no such type path, so the
                // child answers nothing here.
                if let Some(path) = namespace_before(&doc.source, from) {
                    for d in self.namespace_types(uri, &path) {
                        let head = d.hover.lines().nth(1).unwrap_or("");
                        let kind_word = ["struct", "enum", "trait", "interface", "type"]
                            .into_iter()
                            .find(|w| head.contains(&format!("{w} ")))
                            .unwrap_or("type");
                        let label = d.name[path.len() + 1..].to_string();
                        let mut item = word(&label, 7, Some(d.hover.clone()), from);
                        item["detail"] = json!(format!("{kind_word} {path}.{label}"));
                        items.push(item);
                    }

                    // `store: Scribe.|`: an import binds `Scribe`, so
                    // no declaration of this file carries the path. The
                    // walk follows the import into the module and lists
                    // the types it exports, and no value among them.
                    let load = |spec: &str| self.module_source(uri, spec);
                    let returns_one = self.plain_modules(uri);
                    let plain = |spec: &str| returns_one.iter().any(|s| s == spec);
                    let segments: Vec<&str> = path.split('.').collect();

                    for m in components::types(&doc.source, &segments, &load, &plain) {
                        if items.iter().any(|i| i["label"] == json!(m.name)) {
                            continue;
                        }

                        let hover = m.signature.as_ref().map(|s| format!("```alloy\n{s}\n```"));
                        let mut item = word(&m.name, m.kind, hover, from);
                        item["detail"] = json!(format!("{} {path}.{}", m.detail, m.name));

                        // `Shapes.Deep` is a step, not a type, so the
                        // accept writes the `.` the way a bare slot
                        // does for the head of the path.
                        if m.detail == "namespace" {
                            item["textEdit"]["newText"] = json!(format!("{}.", m.name));
                            item["command"] = json!({
                                "title": "Suggest",
                                "command": "editor.action.triggerSuggest",
                            });
                        }

                        items.push(item);
                    }

                    // `local b: Enum.|`: the engine's enums. That path
                    // is the only spelling a type slot takes for one,
                    // and the definitions file holds no namespace the
                    // walk above could follow.
                    if path == "Enum" && items.is_empty() {
                        for name in roblox_enum_names() {
                            let mut item = word(name, 13, None, from);
                            item["detail"] = json!(format!("enum Enum.{name}"));
                            items.push(item);
                        }
                    }

                    return items;
                }

                for mut item in self.type_completions(uri, &[]) {
                    let label = item["label"].as_str().unwrap_or("").to_string();
                    let kind = item["kind"].as_u64().unwrap_or(7);
                    let doc_text = item["documentation"]["value"].as_str().map(str::to_string);
                    let detail = item["detail"].clone();
                    let rank = type_rank(*prefers, detail.as_str().unwrap_or(""));
                    // A type function inserts its brackets, so the text
                    // the accept writes rides along with the label.
                    let insert = item["insertText"].as_str().map(str::to_string);
                    let format = item["insertTextFormat"].clone();
                    item = word(&label, kind, doc_text, from);
                    item["detail"] = detail;
                    item["sortText"] = json!(format!("{rank}{label}"));

                    if let Some(insert) = insert {
                        item["textEdit"]["newText"] = json!(insert);

                        if !format.is_null() {
                            item["insertTextFormat"] = format;
                        }
                    }

                    items.push(item);
                }

                // `stor: Scri|`: the module `Scribe` is no type itself,
                // and `Scribe.Store` is one. The slot offers the name
                // as the head of that path, beside the plain types and
                // ranked with them, so the list reads by name.
                let load = |spec: &str| self.module_source(uri, spec);
                let returns_one = self.plain_modules(uri);
                let plain = |spec: &str| returns_one.iter().any(|s| s == spec);

                for m in components::type_prefixes(&doc.source, &load, &plain) {
                    if items.iter().any(|i| i["label"] == json!(m.name)) {
                        continue;
                    }

                    let hover = m.signature.as_ref().map(|s| format!("```alloy\n{s}\n```"));
                    let rank = type_rank(*prefers, &m.detail);
                    let mut item = word(&m.name, m.kind, hover, from);
                    item["detail"] = json!(m.detail);
                    item["sortText"] = json!(format!("{rank}{}", m.name));
                    // The accept writes the `.` too. The name alone
                    // leaves half a type in the slot, and the `.` puts
                    // the caret where the dotted list answers, so one
                    // accept and one keystroke reach `Scribe.Store`.
                    item["textEdit"]["newText"] = json!(format!("{}.", m.name));
                    item["command"] = json!({
                        "title": "Suggest",
                        "command": "editor.action.triggerSuggest",
                    });
                    items.push(item);
                }
            }

            Context::NewTarget { prefix } => {
                let from = offset - prefix.len();

                for d in self.decls_in_scope(uri) {
                    let head = d.hover.lines().nth(1).unwrap_or("");

                    if head.contains("struct ") || head.contains("class ") {
                        let mut item = word(&d.name, 7, Some(d.hover.clone()), from);

                        if let Some(detail) = declaration_detail(&d.hover) {
                            item["detail"] = json!(detail);
                        }

                        items.push(item);
                    }
                }

                for name in [
                    "HashMap", "Set", "Queue", "Heap", "Scope", "Signal", "Symbol", "Array",
                ] {
                    let mut item = word(name, 7, keywords::doc(name).map(str::to_string), from);
                    item["detail"] = json!("alloy:std");
                    items.push(item);
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

                    // A struct matches as one pattern over its fields:
                    // `Vec2 { x, y }`, where a name alone binds the
                    // field. An interface declares no case of its own:
                    // the value is one of the structs whose fields
                    // cover it, so each of those is an arm. Nothing
                    // else of the scope is one.
                    MatchKind::Struct(name) | MatchKind::Interface(name) => {
                        let inside = context::impl_target(&doc.source, offset);
                        let names = match kind {
                            MatchKind::Interface(_) => self.structs_satisfying(uri, name),

                            _ => vec![name.clone()],
                        };

                        for name in names {
                            let fields =
                                self.struct_fields(uri, &name, inside.as_deref() == Some(&*name));
                            let slots: Vec<String> = fields
                                .iter()
                                .enumerate()
                                .map(|(i, f)| format!("${{{}:{}}}", i + 1, f.name))
                                .collect();
                            let insert = match slots.is_empty() {
                                true => format!("{name} {{ }}"),

                                false => format!("{name} {{ {} }}", slots.join(", ")),
                            };
                            let listed: Vec<&str> =
                                fields.iter().map(|f| f.name.as_str()).collect();
                            let detail = match listed.is_empty() {
                                true => format!("{name} {{ }}"),

                                false => format!("{name} {{ {} }}", listed.join(", ")),
                            };
                            items.push(snippet(
                                &format!("{name} {{ }}"),
                                &insert,
                                20,
                                &detail,
                                Some(format!("A pattern over the fields of `struct {name}`.")),
                                from,
                            ));
                        }

                        items.push(word(
                            "_",
                            14,
                            Some("Matches anything without binding it.".to_string()),
                            from,
                        ));
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

                // A contract on this declaration says which fields are
                // missing. Those rank first.
                for gap in contract_gaps_at(doc, offset, "field") {
                    let ty = match gap.shape.is_empty() {
                        true => "unknown".to_string(),

                        false => gap.shape.clone(),
                    };
                    let insert = match gap.visibility.is_empty() {
                        true => format!("{}: {ty}", gap.member),

                        false => format!("{} {}: {ty}", gap.visibility, gap.member),
                    };
                    let mut item = word(
                        &gap.member,
                        5,
                        Some(format!("`@{}` requires this field.", gap.attr)),
                        from,
                    );
                    item["textEdit"]["newText"] = json!(insert);
                    item["detail"] = json!(gap_detail(gap));
                    // The editor matches the word the author typed
                    // against this, so `pri` and `state` both keep the
                    // row while the label stays the member's name.
                    item["filterText"] = json!(insert);
                    item["sortText"] = json!("0");
                    items.push(item);
                }

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

                // A contract on this declaration says what is missing.
                // Those members rank first: the author is here to write
                // one of them.
                for gap in contract_gaps_at(doc, offset, "function") {
                    let insert = match gap.visibility.is_empty() {
                        true => format!("function {}{}", gap.member, gap_params(gap)),

                        false => format!(
                            "{} function {}{}",
                            gap.visibility,
                            gap.member,
                            gap_params(gap)
                        ),
                    };
                    let mut item = snippet(
                        &gap.member,
                        &format!("{insert}\n\t$0\nend"),
                        2,
                        &gap_detail(gap),
                        Some(format!("`@{}` requires this member.", gap.attr)),
                        from,
                    );
                    // The editor matches the word the author typed
                    // against this, so `pri`, `function` and `Start`
                    // all keep the row.
                    item["filterText"] = json!(insert);
                    item["sortText"] = json!("0");
                    items.push(item);
                }

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

            /*
            A line of an attribute contract. The context already holds
            exactly the words that fit, so the list is that and nothing
            else: the whole scope here named no member of a contract,
            and the child sees an emit that holds no attribute at all.
            */
            Context::ContractClause { prefix, words } => {
                let from = offset - prefix.len();

                for w in words {
                    let what = match w.as_str() {
                        "requires" => "What the thing the attribute sits on must carry.",

                        "public" => "The member must be public.",

                        "private" => "The member must be private.",

                        "function" => "The member is a method.",

                        "field" => "The member is a field.",

                        "each" => "One clause per entry of a list parameter.",

                        "end" => "Closes the body.",

                        // A parameter of this attribute, which is what
                        // `each` reads.
                        _ => "A list parameter; `each` writes one clause per entry.",
                    };
                    items.push(word(w, 14, Some(what.to_string()), from));
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

                // The same detail and the same text a member list gives
                // after `part.`: the type the property writes, and the
                // engine's own description of it.
                for name in alloy::luaux::roblox::properties(class) {
                    // The detail is the type alone, as after `part.`.
                    let detail = match alloy::roblox_props::property_type(class, name) {
                        Some(ty) => readable_type(ty),

                        None => format!("property of {class}"),
                    };
                    let mut item = snippet(
                        name,
                        &format!("{name} = ${{1:{name}}}"),
                        5,
                        &detail,
                        self.roblox_doc(class, name),
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
                        self.roblox_doc(class, name),
                        from,
                    );
                    item["sortText"] = json!(format!("1{name}"));
                    items.push(item);
                }
            }

            // `profile["|`: the keys the receiver's type names, each
            // written with its quotes. An open quote is part of what
            // the item replaces, so the key never doubles it.
            Context::IndexKey {
                prefix,
                receiver,
                quote,
            } => {
                let from = offset - prefix.len() - quote.map_or(0, char::len_utf8);
                let q = quote.unwrap_or('"');

                for field in self.index_keys(uri, &doc.source, offset, receiver) {
                    let label = format!("{q}{}{q}", field.name);
                    let mut item = word(&label, 21, Some(format!("A key of `{receiver}`.")), from);
                    item["detail"] = json!(field.ty);
                    item["sortText"] = json!(format!("0{}", field.name));
                    items.push(item);
                }
            }

            // `destroy part |`: the timer form of the statement.
            Context::DestroyAfter { prefix } => {
                items.push(word(
                    "after",
                    14,
                    Some("`destroy x after n` waits `n` seconds, then destroys `x`.".to_string()),
                    offset - prefix.len(),
                ));
            }

            // `match e |`: the head opens the arms with `with`, and
            // names the value with `as` first.
            Context::MatchWith { prefix, aliased } => {
                let from = offset - prefix.len();

                if !aliased {
                    items.push(word(
                        "as",
                        14,
                        Some("Names the value for every arm: `match e as name with`.".to_string()),
                        from,
                    ));
                }

                items.push(word(
                    "with",
                    14,
                    Some("Opens the arms of the match.".to_string()),
                    from,
                ));
            }

            Context::AfterDo { prefix, filtered } => {
                let from = offset - prefix.len();
                items.push(word(
                    "do",
                    14,
                    Some("Opens the block the timer runs.".to_string()),
                    from,
                ));

                if !filtered {
                    items.push(word(
                        "where",
                        14,
                        Some("A condition on the block, read when the timer fires.".to_string()),
                        from,
                    ));
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
                        "Opens the body: the fields of a struct, the variants of an enum, the methods of an `impl` or a `trait`. An attribute opens its `requires` clauses with it."
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

            Context::RemoteFunction { prefix } => {
                items.push(word(
                    "function",
                    14,
                    Some(
                        "A remote that returns a value: `remote function Name(x: number): string from server`."
                            .to_string(),
                    ),
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

                for target in alloy_syntax::ATTRIBUTE_TARGETS {
                    let doc = attribute_target_doc(target).map(str::to_string);

                    items.push(word(target, 21, doc, from));
                }
            }

            Context::ImportSpec { text, start } => {
                // `"@game/X"` names one service, so the segment right
                // after the alias is the service list. A second segment
                // makes the path an instance path, which the module
                // entries walk the sourcemap for. `"game:X"` is the old
                // spelling of the same list.
                let service_head = text
                    .strip_prefix("@game/")
                    .filter(|rest| !rest.contains('/'))
                    .map(|_| "@game/")
                    .or_else(|| text.starts_with("game:").then_some("game:"));

                if let Some(head) = service_head {
                    let from = start + head.len();

                    for name in alloy::roblox_services::SERVICES {
                        let mut item = word(
                            name,
                            9,
                            Some(alloy::game_import::service_summary(name)),
                            from,
                        );
                        item["detail"] = json!(format!("game:GetService(\"{name}\")"));
                        items.push(item);
                    }

                    return items;
                }

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

    /// The specs this file imports whose module returns one value: a
    /// `.luau` or `.lua` file, a data file, and an Alloy module that
    /// ends in `return <expr>`. `import M from` binds `require(...)`
    /// whole for one of those, so `M.T` reads a type the module
    /// exports; every other module binds the `default` field instead.
    pub(crate) fn plain_modules(&self, uri: &str) -> Vec<String> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };

        alloy::modules::plain_modules_for_file(&path, &doc.source)
    }

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

    /// The declarations of the module a path resolves to, for the
    /// detail an import list shows beside each name.
    pub(crate) fn decls_of_target(
        &self,
        resolved: Option<PathBuf>,
    ) -> Vec<&alloy::declarations::Declaration> {
        let Some(resolved) = resolved else {
            return Vec::new();
        };
        let target = imports::module_path(&resolved);
        let mut out = Vec::new();

        for (u, d) in &self.docs {
            let Some(p) = uri_to_path(u) else { continue };

            if imports::module_path(&p) == target {
                out.extend(d.decls.iter());
            }
        }

        out
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

        if head.contains("struct ") {
            return MatchKind::Struct(name.to_string());
        }

        if head.contains("interface ") {
            return MatchKind::Interface(name.to_string());
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

        // `new Vec2 { ... }`: the constructor names the type.
        if let Some(rest) = t.strip_prefix("new ") {
            let named: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();

            return self.kind_of_name(uri, &named);
        }

        if head.is_empty() || !t[head.len()..].starts_with('.') {
            return MatchKind::Unknown;
        }

        match self.kind_of_name(uri, &head) {
            MatchKind::Enum(name) => MatchKind::Enum(name),

            _ => MatchKind::Unknown,
        }
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
                            l.contains("enum ")
                                && (*u == uri
                                    || l.starts_with("export ")
                                    || l.starts_with("global "))
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

    /// The fields a struct or a record type declares, read from the
    /// declaration's hover. A private field stays out unless the caret
    /// sits in the impl of that same type.
    /// The string keys the type of a receiver names: the fields of a
    /// record or of a struct. An array, a `HashMap`, and a
    /// `{ [string]: T }` name none, so the list stays empty and the
    /// child answers with the scope, where the key is an expression.
    pub(crate) fn index_keys(
        &self,
        uri: &str,
        source: &str,
        offset: usize,
        receiver: &str,
    ) -> Vec<context::Field> {
        let mut parts = receiver.split('.');
        let head = parts.next().unwrap_or(receiver);
        let Some(mut ty) = self.value_type(source, offset, head) else {
            return Vec::new();
        };

        for field in parts {
            match self.field_type(uri, &ty, field) {
                Some(t) => ty = t,

                None => return Vec::new(),
            }
        }

        let ty = ty.trim().trim_end_matches('?').trim();

        match ty.starts_with('{') {
            true => context::record_entries(ty),

            false => {
                let name = ty.split('<').next().unwrap_or(ty).trim();
                let inside = context::impl_target(source, offset).as_deref() == Some(name);

                self.struct_fields(uri, name, inside)
            }
        }
    }

    /// Every struct in reach whose fields cover the ones an interface
    /// declares: the names, and the types where both sides write one.
    /// A value of the interface holds one of these at run time, so each
    /// of them is a `case` of a match on it.
    pub(crate) fn structs_satisfying(&self, uri: &str, interface: &str) -> Vec<String> {
        let wanted = self.struct_fields(uri, interface, false);

        if wanted.is_empty() {
            return Vec::new();
        }

        let mut out: Vec<String> = Vec::new();

        for d in self.decls_in_scope(uri) {
            if !d
                .hover
                .lines()
                .nth(1)
                .is_some_and(|head| head.contains("struct "))
            {
                continue;
            }

            let fields = context::record_entries(&d.hover);
            let covers = wanted.iter().all(|w| {
                fields.iter().any(|f| {
                    f.name == w.name
                        && (w.ty.is_empty() || f.ty.is_empty() || f.ty == w.ty)
                        && !f.private
                })
            });

            if covers && !out.contains(&d.name) {
                out.push(d.name.clone());
            }
        }

        out
    }

    /// The fields of a struct, by the name a literal writes in front of
    /// its table. A namespace member is keyed by its path, and a `*`
    /// import or a module binding puts its own name in front of that,
    /// so each leading step drops until one key matches.
    pub(crate) fn struct_fields(&self, uri: &str, name: &str, inside: bool) -> Vec<context::Field> {
        let decls = self.decls_in_scope(uri);
        // `import * as M` binds the whole module, so no name of it is
        // in scope on its own. What the file imports carries it.
        let imported = self
            .docs
            .get(uri)
            .map(|d| d.import_decls.as_slice())
            .unwrap_or_default();

        // `import { Ns as A }` then `new A.T { |`: the module declares
        // the member under its own group name, so the alias reads as
        // that name first.
        let declared = self
            .docs
            .get(uri)
            .into_iter()
            .flat_map(|d| import_entries(&d.source))
            .filter(|it| it.bound != it.name)
            .find_map(|it| {
                let rest = name.strip_prefix(it.bound.as_str())?.strip_prefix('.')?;

                Some(format!("{}.{rest}", it.name))
            });

        declared
            .as_deref()
            .into_iter()
            .chain([name])
            .flat_map(|n| {
                std::iter::successors(Some(n), |path| path.split_once('.').map(|(_, rest)| rest))
            })
            .find_map(|path| {
                decls
                    .iter()
                    .copied()
                    .chain(imported)
                    .find(|d| d.name == path)
            })
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
}

impl Server {
    /// side, or an import, where the child would list globals.
    /// Whether the caret takes a field name of an object initializer.
    /// The space after a comma continues the field list, so the trigger
    /// answers there and nowhere else a space lands.
    pub(crate) fn opens_a_field_list(&self, uri: &str, message: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        context::detect(&doc.source, offset).is_some_and(|c| fills_an_initializer(&c))
    }

    pub(crate) fn context_completion(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
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

        let Some(ctx) = context::detect(&doc.source, offset) else {
            // `(` opens an attribute's argument list and `{` the field
            // list of an object initializer; anywhere else the editor
            // asked on one for nothing, and the child would list
            // globals. A string in a call the emit rewrote,
            // `$todo("why")`, maps to no byte of its own, and the child
            // would list the scope of the line it lands on.
            if matches!(trigger, Some("(" | "{")) || string_left_behind(doc, line, character) {
                drop(st);
                self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": [] }));

                return true;
            }

            return false;
        };

        // The child lists a newline as a trigger for its `end`
        // completion. That request is the child's alone: a context list
        // answered here would open on every Enter, and the next Enter
        // would accept its first item. The blank line an object
        // initializer holds is the exception: the field list is what
        // Enter asks for there.
        if trigger == Some("\n") && !fills_an_initializer(&ctx) {
            return false;
        }

        // `{` opens the field list of an object initializer. Every
        // other `{`, a table literal, a type, or a markup hole, takes
        // no list, so nothing pops up where the author writes a value.
        if trigger == Some("{") && !fills_an_initializer(&ctx) {
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": [] }));

            return true;
        }

        let mut items = st.context_items(uri, offset, &ctx);

        // A receiver whose type names no key takes any expression
        // there, so the scope the child lists is the right answer.
        if items.is_empty() && matches!(ctx, context::Context::IndexKey { .. }) {
            return false;
        }

        let (extra, incomplete) = st.ingot_items(uri, line, character, trigger);
        items.extend(extra);
        let mut result = match incomplete {
            true => json!({ "isIncomplete": true, "items": items }),

            false => json!(items),
        };
        // The list never reaches the merge path, so it marks and hides
        // its own deprecated rows.
        st.deprecated_pass(uri, &mut result);
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }
}

/// Whether a context takes a field name of an object initializer:
/// `new Instance("Part") { |`, `new Stats { |`, and the slot after each
/// comma or newline inside the braces.
pub(crate) fn fills_an_initializer(ctx: &context::Context) -> bool {
    matches!(
        ctx,
        context::Context::StructField { .. } | context::Context::InstanceField { .. }
    )
}

/// Removes the runtime's table from a type text, in every string of the
/// value: `__alloy.Future<T>` becomes `Future<T>`, and the primitive
/// helper `__alloy_string.trim` becomes `string.trim`.
/// Where a type name sorts in the list of a slot. `extends` and the
/// trait of an `impl` take a contract; `impl X` and the target after
/// `for` take a struct, an enum, or a class. The other names stay in
/// the list: the author may be about to declare one.
pub(crate) fn type_rank(prefers: context::Prefers, detail: &str) -> u8 {
    // The detail names the kind first, `struct Profile`, so the rank
    // reads the word alone. `alloy:std trait` is the std's own line.
    let word = match detail {
        "alloy:std trait" => "trait",

        _ => {
            let rest = detail.strip_prefix("global ").unwrap_or(detail);

            rest.split_whitespace().next().unwrap_or(rest)
        }
    };

    match prefers {
        context::Prefers::Any => 1,

        context::Prefers::Contract => match word {
            "interface" | "trait" => 0,

            _ => 1,
        },

        context::Prefers::Concrete => match word {
            "struct" | "enum" => 0,

            _ => 1,
        },
    }
}

/// The payload types of a variant signature, `Msg.Move(Player, number)`
/// giving `["Player", "number"]`, split at the commas outside brackets.
/// What a `match` scrutinee resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchKind {
    /// An enum in scope: its variants are the arms.
    Enum(String),
    /// A struct in scope: one pattern over its fields.
    Struct(String),
    /// An interface in scope: one pattern for every struct that
    /// satisfies it.
    Interface(String),
    /// A `Result<T, E>`: `Ok` and `Err`.
    Result,
    /// `T[]` or `Array<T>`: the array patterns.
    Array,
    /// A string or a number: only `default` fits.
    Literal,
    /// Nothing the proxy reads.
    Unknown,
}

/// A Roblox type as the source writes it: the dump spells an enum
/// `EnumFont`, and the reader writes `Enum.Font`.
pub(crate) fn readable_type(name: &str) -> String {
    match name.strip_prefix("Enum") {
        Some(rest) if rest.starts_with(char::is_uppercase) => format!("Enum.{rest}"),

        _ => name.to_string(),
    }
}

/// The element of a list type: `Lifecycle[]` and `Array<Lifecycle>` both
/// give `Lifecycle`. Any other type is its own element, so a parameter
/// that takes one value completes the same way.
fn element_type(ty: &str) -> String {
    let ty = ty.trim();
    let inner = ty
        .strip_suffix("[]")
        .or_else(|| ty.strip_prefix("Array<").and_then(|r| r.strip_suffix('>')))
        .unwrap_or(ty)
        .trim();
    let inner = inner
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or(inner);

    inner.trim().to_string()
}

impl State {
    /*
    The literals a type admits, as completion items: the label, the text
    to insert, and the detail that says where the list came from.

    A union of string literals gives one item per member. A name that an
    enum in scope carries gives one item per variant, written the way a
    source writes it, `Lifecycle.Init`. Any other type gives nothing: the
    argument is a literal, and this offers only the ones the type names.
    */
    fn literal_items(
        &self,
        uri: &str,
        element: &str,
        quote: Option<char>,
        attr: &str,
    ) -> Vec<(String, String, String)> {
        let detail = format!("takes `{element}` for `@{attr}`");

        if element.contains('"') || element.contains('\'') {
            return element
                .split('|')
                .filter_map(|part| {
                    let text = part.trim().trim_matches(['"', '\'']);

                    (!text.is_empty() && !part.trim().starts_with(|c: char| c.is_alphanumeric()))
                        .then(|| {
                            let insert = match quote {
                                // The editor's word starts after the
                                // quote, so the insert carries no quote
                                // of its own.
                                Some(_) => text.to_string(),

                                None => format!("\"{text}\""),
                            };

                            (text.to_string(), insert, detail.clone())
                        })
                })
                .collect();
        }

        // A quote is open, so an enum path does not fit there.
        if quote.is_some() {
            return Vec::new();
        }

        for shape in &self.known_shapes_at(Some(uri)).shapes {
            let alloy::declarations::Shape::Enum { name, variants, .. } = shape else {
                continue;
            };

            if name != element {
                continue;
            }

            return variants
                .iter()
                .map(|(v, _)| {
                    let path = format!("{name}.{v}");

                    (path.clone(), path, detail.clone())
                })
                .collect();
        }

        Vec::new()
    }
}

/*
The members of one kind a contract asks for in the body the offset sits
in. `kind` is `function` for a method column and `field` for a field
column: a `requires field` clause goes on the struct, and the same
declaration can carry gaps of both kinds.

A half-written member stops the compile, so the list is empty there. The
items carry a `filterText` of the whole clause instead, which keeps the
row while the editor filters by the word the author types.
*/
fn contract_gaps_at<'a>(
    doc: &'a Doc,
    offset: usize,
    kind: &str,
) -> Vec<&'a alloy::desugar::ContractGap> {
    doc.output
        .as_ref()
        .map(|o| o.contract_gaps.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|g| g.kind == kind && holds_the_gap(&doc.source, offset, g))
        .collect()
}

/// Whether the caret sits in the body a gap goes at the end of.
///
/// A gap carries the `end` it belongs in front of. Every line from the
/// caret to that `end` belongs to the body, so each one sits deeper
/// than the `end` does. A line back at that column closes another
/// block, which puts the caret outside.
fn holds_the_gap(src: &str, offset: usize, gap: &alloy::desugar::ContractGap) -> bool {
    let insert_at = gap.insert_at as usize;

    if offset > insert_at || insert_at > src.len() {
        return false;
    }

    src[offset..insert_at].lines().skip(1).all(|line| {
        let text = line.trim_start();

        text.is_empty() || line.len() - text.len() > gap.indent as usize
    })
}

/// The detail of a missing member: the clause that asks for it, and the
/// attribute the clause belongs to.
fn gap_detail(gap: &alloy::desugar::ContractGap) -> String {
    let mut clause = "requires".to_string();

    if !gap.visibility.is_empty() {
        clause.push(' ');
        clause.push_str(&gap.visibility);
    }

    clause.push(' ');
    clause.push_str(&gap.kind);
    clause.push(' ');
    clause.push_str(&gap.member);

    match gap.kind.as_str() {
        "field" if !gap.shape.is_empty() => clause.push_str(&format!(": {}", gap.shape)),

        "field" => {}

        _ => clause.push_str(&gap_params(gap)),
    }

    format!("{clause} from `@{}`", gap.attr)
}

/// The parameter list a missing function takes: the one the clause wrote,
/// or `(self)` when it wrote none.
fn gap_params(gap: &alloy::desugar::ContractGap) -> String {
    match gap.shape.is_empty() {
        true => "(self)".to_string(),

        false => gap.shape.clone(),
    }
}

/// The first sentence past the fence of a documentation entry.
fn doc_sentence(text: &str) -> Option<String> {
    text.rsplit("```")
        .next()?
        .trim()
        .lines()
        .next()
        .map(str::to_string)
}

/// Where the accept of a name in an `import { }` list writes the
/// `from` clause: the range it replaces, and what goes in front of
/// the `from` word, which closes the list when the line does not.
fn from_clause_at(src: &str, offset: usize) -> (usize, usize, &'static str) {
    let rest = src[offset..].split('\n').next().unwrap_or("");
    let line_end = offset + rest.len();

    match rest.find('}') {
        // The editor closed the brace as the reader opened it: the
        // name and the clause take the space up to it.
        Some(i) if rest[..i].trim().is_empty() => (offset, offset + i + 1, " }"),

        // The caret sits in the middle of the list, so the clause goes
        // after the brace that closes it.
        Some(i) => (offset + i + 1, offset + i + 1, ""),

        None => (line_end, line_end, " }"),
    }
}

/// The one-line documentation of an attribute target.
///
/// The words come from [`alloy_syntax::ATTRIBUTE_TARGETS`], so a new
/// target needs a line here. The test below holds the two lists together.
fn attribute_target_doc(target: &str) -> Option<&'static str> {
    let doc = match target {
        "function" => "A function or method.",
        "struct" => "A struct declaration.",
        "enum" => "An enum declaration.",
        "variant" => "One variant of an enum.",
        "field" => "A field of a struct.",
        "param" => "A parameter, on a function or a remote.",
        "remote" => "A remote declaration.",
        "interface" => "An interface declaration.",
        "type" => "A type alias.",
        "local" => "A local or const binding.",
        "namespace" => "A namespace declaration.",
        "impl" => "An `impl` block.",
        "trait" => "A trait declaration.",

        _ => return None,
    };

    Some(doc)
}

#[cfg(test)]
mod tests {
    use super::attribute_target_doc;

    /// The parser accepts the words of `ATTRIBUTE_TARGETS`, and the
    /// completion offers them. A word with no line here would reach the
    /// editor bare, so the two lists must stay together.
    #[test]
    fn every_attribute_target_carries_a_doc() {
        for target in alloy_syntax::ATTRIBUTE_TARGETS {
            assert!(
                attribute_target_doc(target).is_some(),
                "`{target}` has no documentation line"
            );
        }
    }
}
