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

                // `import { X } from "game"`: the names in braces are
                // the Roblox services.
                if spec.as_deref() == Some("game") && !*type_only {
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

                    return items;
                }

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
                    ("local", "A local or const binding."),
                    ("namespace", "A namespace declaration."),
                ] {
                    items.push(word(target, 21, Some(doc_text.to_string()), from));
                }
            }

            Context::ImportSpec { text, start } => {
                // `"game:X"` names one service, so the segment after the
                // colon is the service list.
                if text.starts_with("game:") {
                    let from = start + "game:".len();

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
                    // is an alias the project has to declare. `game` is
                    // neither: it names the services.
                    if head.is_empty()
                        && !label.starts_with(['@', '.'])
                        && !label.starts_with("game")
                    {
                        item["textEdit"]["newText"] = json!(format!("./{label}"));
                    }

                    items.push(item);
                }
            }
        }

        items
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
}

impl Server {
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
