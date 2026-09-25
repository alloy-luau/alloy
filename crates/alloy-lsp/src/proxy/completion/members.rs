use super::*;

impl State {
    /// `self:` inside a trait's default method. A trait has no table in
    /// the emit, so the child has no type for `self` there; the trait's
    /// own signatures are the list.
    pub(crate) fn trait_self_members(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };
        if !takes_a_self_member(&doc.source, offset) {
            return Vec::new();
        }

        let Some((name, methods)) = enclosing_trait(&doc.source, line) else {
            return Vec::new();
        };
        let snippets = self.snippets;

        methods
            .into_iter()
            .map(|(label, detail)| {
                let mut item = json!({
                    "label": label,
                    "kind": 2,
                    "detail": detail,
                    "sortText": format!("0{label}"),
                    "documentation": {
                        "kind": "markdown",
                        "value": format!("A method of `trait {name}`."),
                    },
                });
                set_call(&mut item, &label, &detail, snippets);

                item
            })
            .collect()
    }

    /// The methods of every `impl` of the struct `self` stands for.
    ///
    /// `impl Point` in a file that imports `Point` writes the methods on
    /// the table the module exports, and the checker types that table
    /// from the module alone: the child then lists the fields and no
    /// method. The source says which methods the struct has.
    pub(crate) fn impl_self_members(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &Value,
    ) -> Vec<Value> {
        let Some((target, methods)) = self.self_impl_methods(uri, line, character) else {
            return Vec::new();
        };
        let methods: Vec<(String, String)> = methods
            .into_iter()
            .filter(|(_, _, takes_self)| *takes_self)
            .map(|(label, detail, _)| (label, detail))
            .collect();
        let mut taken: Vec<String> = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i["label"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut items = Vec::new();

        for (label, detail) in methods {
            if taken.contains(&label) {
                continue;
            }

            let mut item = json!({
                "label": label,
                "kind": 2,
                "detail": detail,
                "sortText": format!("0{label}"),
                "documentation": {
                    "kind": "markdown",
                    "value": format!("A method of `impl {target}`."),
                },
            });
            set_call(&mut item, &label, &detail, self.snippets);
            taken.push(label);
            items.push(item);
        }

        items
    }

    /// The methods of every `impl` of the struct the `self.` at a
    /// position belongs to, with whether each takes a receiver. `None`
    /// when the position follows no `self.` or `self:`.
    fn self_impl_methods(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<(String, Vec<ImplMethod>)> {
        let doc = self.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;

        if !takes_a_self_member(&doc.source, offset) {
            return None;
        }

        let target = context::impl_target(&doc.source, offset)?;
        // A private method belongs to the impl alone, so it comes from
        // this file and no other.
        let mut methods = impl_methods(&doc.source, &target, true);

        // Another file's `impl` of the same struct writes on the same
        // table, and an exported one reaches every file.
        for (u, other) in &self.docs {
            if u != uri {
                methods.extend(impl_methods(&other.source, &target, false));
            }
        }

        Some((target, methods))
    }

    /// Drops the statics of an `impl` from a list after `self.`. The emit
    /// writes a method with no receiver on the same table as the rest, so
    /// the child offers it where no `self` can call it.
    pub(crate) fn drop_impl_statics(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let Some((_, methods)) = self.self_impl_methods(uri, line, character) else {
            return;
        };
        let statics: Vec<String> = methods
            .into_iter()
            .filter(|(_, _, takes_self)| !takes_self)
            .map(|(label, _, _)| label)
            .collect();

        if statics.is_empty() {
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
            !i.get("label")
                .and_then(Value::as_str)
                .is_some_and(|l| statics.iter().any(|s| s == l))
        });
    }

    /// The details of a list after `->`. The name lowers to the string
    /// of a `FindFirstChild("`, so the child details each child as
    /// `string`. The sourcemap gives the class, when it has one.
    pub(crate) fn child_name_details(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let asks_a_child = offset_of(&doc.source, line, character)
            .and_then(|at| context::child_name_start(&doc.source, at))
            .and_then(|start| child_call(doc, start))
            .is_some();

        if !asks_a_child {
            return;
        }

        let sourcemap = self
            .settings
            .pointer("/sourcemap/sourcemapFile")
            .and_then(Value::as_str)
            .unwrap_or("sourcemap.json");
        let tree = self
            .root
            .as_deref()
            .and_then(|root| std::fs::read_to_string(root.join(sourcemap)).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);
        let items = match result {
            Value::Array(v) => v,

            Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
                Some(v) => v,

                None => return,
            },

            _ => return,
        };

        child_details(&tree, items);
    }

    /// The members a dotted value path reaches, for a path the child
    /// could not follow. `import M from "./m"` on a module with an
    /// export table binds the `default` field, and the child has no
    /// type for that name: it answers with the scope of an expression
    /// instead, a list of globals under a `.`.
    ///
    /// The walk over the imports and the namespaces says what the path
    /// holds, and that list stands alone. A path the child did follow
    /// keeps the child's answer: its types come from the solver, which
    /// reads more than the source text.
    pub(crate) fn value_path_members(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &Value,
    ) -> Option<Vec<Value>> {
        let doc = self.docs.get(uri)?;

        if member_position(doc, line, character) != Some('.') {
            return None;
        }

        if !answered_the_scope(result) {
            return None;
        }

        let offset = offset_of(&doc.source, line, character)?;
        let path = namespace_before(&doc.source, offset)?;
        let segments: Vec<&str> = path.split('.').collect();
        let head = segments.first().copied()?;

        // Only a path Alloy owns. A name the source binds some other
        // way is the child's to answer, and a list built here would
        // stand in for an answer it may yet give.
        if !components::containers(&doc.source)
            .iter()
            .any(|m| m.name == head && m.detail != "table")
        {
            return None;
        }

        let load = |spec: &str| self.module_source(uri, spec);
        let found = components::members(&doc.source, &segments, &load);
        let mut items = Vec::new();

        for m in &found {
            let mut item = json!({
                "label": m.name,
                "kind": m.kind,
                "detail": format!("{} {path}.{}", m.detail, m.name),
                "sortText": m.sort_key(),
            });

            if let Some(text) = &m.signature {
                item["documentation"] = json!({
                    "kind": "markdown",
                    "value": format!("```alloy\n{text}\n```"),
                });
            }

            items.push(item);
        }

        // The walk reaching nothing is an answer too: the path names
        // no member, and the scope of the file is not what stands
        // under a `.`.
        Some(items)
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

        // `Net.Up` is the remote `Up` in a namespace or a star import:
        // the file binds the head, and the declaration names the last.
        // ponytail: the last name alone picks the declaration, so two
        // namespaces with a remote of one name read the first.
        let head = base.split('.').next().unwrap_or(&base);
        let name = base.rsplit('.').next().unwrap_or(&base);
        let here = remote_spec(&doc.source, name);
        let spec = match here {
            Some(spec) => Some(spec),

            // The declaration sits in the module the file imports it
            // from; a name no import bound is not this remote.
            None => imports::bound_names(&doc.source)
                .iter()
                .any(|b| b == head)
                .then(|| {
                    self.docs
                        .values()
                        .find_map(|d| remote_spec(&d.source, name))
                })
                .flatten(),
        };
        let Some(spec) = spec else {
            return;
        };
        let side = self.side_at(uri);

        items.retain(|i| {
            i["label"]
                .as_str()
                .is_none_or(|label| spec.holds(label, side))
        });
    }
}

/// Whether a completion answer is the scope of an expression and not a
/// member list. The child offers `nil`, `true`, and `if` where an
/// expression may start and never after a `.`, so one of those words in
/// the list says it could not type the receiver and answered with the
/// scope of the file instead.
fn answered_the_scope(result: &Value) -> bool {
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| result.as_array());

    items.is_some_and(|items| {
        items.iter().any(|i| {
            matches!(
                i.get("label").and_then(Value::as_str),
                Some("nil" | "true" | "false" | "if" | "function" | "not")
            )
        })
    })
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

/// Sets the detail of each child name to its class in the sourcemap.
/// The list names every child of one instance, so each node whose
/// children hold all the names may be that instance. A name takes a
/// class when those nodes agree on it, and else shows no detail.
pub(crate) fn child_details(tree: &Value, items: &mut [Value]) {
    fn walk<'a>(node: &'a Value, names: &[&str], out: &mut HashMap<&'a str, HashSet<&'a str>>) {
        let children = node
            .get("children")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let holds_all = names
            .iter()
            .all(|n| children.iter().any(|c| c["name"] == *n));

        for child in children {
            if holds_all
                && let Some(name) = child["name"].as_str()
                && let Some(class) = child["className"].as_str()
            {
                out.entry(name).or_default().insert(class);
            }

            walk(child, names, out);
        }
    }

    let names: Vec<String> = items
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut classes = HashMap::new();

    if !names.is_empty() {
        walk(tree, &names, &mut classes);
    }

    for item in items {
        let class = item["label"]
            .as_str()
            .and_then(|l| classes.get(l))
            .filter(|c| c.len() == 1)
            .and_then(|c| c.iter().next().map(|c| c.to_string()));
        let Some(obj) = item.as_object_mut() else {
            continue;
        };

        match class {
            Some(class) => {
                obj.insert("detail".to_string(), json!(class));
            }

            None => {
                obj.remove("detail");
            }
        }
    }
}

/// The entries a module path can continue with: the project's aliases
/// when nothing is typed, and `@self` in an `init` file, the children of
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
    // The compiler reads `@self` only in an `init` file.
    let has_self = own.is_some_and(alloy::build::is_init);

    if head.is_empty() {
        if has_self {
            out.push((
                "@self/".to_string(),
                19,
                "this file's directory".to_string(),
            ));
        }
        out.push(("../".to_string(), 19, "the parent directory".to_string()));

        for (name, target) in project_aliases(dir, root) {
            out.push((
                format!("@{name}/"),
                19,
                format!("alias: {}", target.display()),
            ));
        }

        // The Roblox services: `@game` takes a list in braces, and
        // `@game/X` names one. A path past a service is an instance
        // path, which the sourcemap answers for.
        out.push((
            "@game".to_string(),
            9,
            "the Roblox services, in braces".to_string(),
        ));
        out.push((
            "@game/".to_string(),
            19,
            match root.is_some_and(|r| r.join(sourcemap).is_file()) {
                true => format!("one Roblox service, or the DataModel from {sourcemap}"),

                false => "one Roblox service".to_string(),
            },
        ));
    }

    if let Some(rest) = head.strip_prefix("@game/") {
        // The first segment is a service, and `context_items` lists
        // those with their class summaries. From the second segment on
        // the path names an instance, so the sourcemap answers.
        if rest.is_empty() {
            return out;
        }

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
        has_self.then(|| imports::lexical(dir, rest))
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
    // The `)` that closes the list: a return type `()` has one too.
    let mut depth = 0i32;
    let Some(close) = signature[open..].char_indices().find_map(|(i, c)| {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }

        (depth == 0).then_some(open + i)
    }) else {
        return Vec::new();
    };
    let inner = &signature[open + 1..close];
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (i, c) in inner.char_indices() {
        match c {
            '(' | '{' | '<' | '[' => depth += 1,
            // The `>` of `->` closes nothing.
            '>' if inner[..i].ends_with('-') => {}
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

/// The trait whose body holds `line`, with each method's name and the
/// signature the trait writes. A trait stands at the margin, so its
/// `end` is the first `end` in column zero under it.
pub(crate) fn enclosing_trait(source: &str, line: u32) -> Option<(String, Vec<(String, String)>)> {
    let lines: Vec<&str> = source.lines().collect();
    let at = line as usize;
    let head = (0..=at.min(lines.len().saturating_sub(1)))
        .rev()
        .find(|i| {
            let text = lines[*i];

            text.starts_with("trait ")
                || text.starts_with("export trait ")
                || text.starts_with("global trait ")
        })?;
    let name: String = lines[head]
        .trim_start_matches("export ")
        .trim_start_matches("global ")
        .trim_start_matches("trait ")
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    // The caret has to sit inside the body.
    let close = (head + 1..lines.len()).find(|i| lines[*i] == "end")?;

    if at <= head || at > close {
        return None;
    }

    let mut methods = Vec::new();

    for text in &lines[head + 1..close] {
        let body = text.trim();
        let body = body.strip_prefix("private ").unwrap_or(body);
        let body = body.strip_prefix("public ").unwrap_or(body);
        let Some(rest) = body.strip_prefix("function ") else {
            continue;
        };
        let label: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if label.is_empty() || !rest[label.len()..].starts_with(['(', '<']) {
            continue;
        }

        let detail = format!("function {name}{}", &rest[label.len()..]);
        methods.push((label, detail));
    }

    (!methods.is_empty()).then_some((name, methods))
}

/// Whether the caret takes a member of `self`, after a `.` or a `:`.
fn takes_a_self_member(source: &str, offset: usize) -> bool {
    let head = source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    if !head.ends_with(['.', ':']) {
        return false;
    }

    let sigil = head.len() - 1;
    let from = head[..sigil]
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);

    &head[from..sigil] == "self"
}

/// One method of an `impl`: its label, the signature the source wrote,
/// and whether it takes a receiver.
type ImplMethod = (String, String, bool);

/// The methods every `impl` of a struct writes, each as its label, the
/// signature the source wrote, and whether it takes a receiver. A method
/// with no receiver is a static, which no `self.` reaches.
fn impl_methods(source: &str, target: &str, private_too: bool) -> Vec<ImplMethod> {
    let mut out = Vec::new();
    let mut inside = false;

    for line in source.lines() {
        let text = line.trim();
        let head = text.strip_prefix("export ").unwrap_or(text);
        let head = head.strip_prefix("global ").unwrap_or(head);

        if let Some(rest) = head.strip_prefix("impl ") {
            // `impl Shape for Sq` writes on the type after `for`.
            let named = rest.split(" for ").last().unwrap_or(rest).trim();
            inside = named
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .eq(target.chars());

            continue;
        }

        // An impl body is indented; a line at the margin closes it. A
        // blank line has no margin and closes nothing.
        if !text.is_empty() && !line.starts_with([' ', '\t']) && text != "end" {
            inside = false;
        }

        if !inside {
            continue;
        }

        let private = text.starts_with("private ");
        let body = text.strip_prefix("private ").unwrap_or(text);
        let body = body.strip_prefix("public ").unwrap_or(body);
        let Some(rest) = body.strip_prefix("function ") else {
            continue;
        };
        let label: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if label.is_empty() || !rest[label.len()..].starts_with(['(', '<']) {
            continue;
        }

        let signature = &rest[label.len()..];

        if private && !private_too {
            continue;
        }

        out.push((label, as_type(signature), signature.contains("(self")));
    }

    out
}

/// A header the source wrote, as the type of the function it declares:
/// `(self, n: number): T` reads `(self, n: number) -> T`.
fn as_type(signature: &str) -> String {
    let mut depth = 0i32;

    for (at, c) in signature.char_indices() {
        match c {
            '(' => depth += 1,

            ')' => {
                depth -= 1;

                if depth > 0 {
                    continue;
                }

                let tail = signature[at + 1..].trim_start();

                return match tail.strip_prefix(':') {
                    Some(ret) => format!("{}) -> {}", &signature[..at], ret.trim()),

                    None => signature[..=at].to_string(),
                };
            }

            _ => {}
        }
    }

    signature.to_string()
}
