use super::*;
use crate::names::EXPRESSION_GLOBALS;

impl State {
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

    /// The file's own top-level functions declared below the caret. A
    /// call above the declaration is a forward call, and the emit
    /// binds the function as a `local` the child scopes from its line
    /// down, so the child's list holds the name only after that line.
    pub(crate) fn functions_below(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &Value,
    ) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };

        // A member of a value is no function of the file.
        if member_position(doc, line, character).is_some() {
            return Vec::new();
        }

        let listed: HashSet<&str> = result
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| result.as_array())
            .into_iter()
            .flatten()
            .filter_map(|i| i["label"].as_str())
            .collect();
        let mut items = Vec::new();

        for text in doc.source.lines().skip(line as usize + 1) {
            // A top-level header starts at the margin; a method of an
            // `impl` and a member of a namespace sit inside a body.
            if text.starts_with([' ', '\t']) {
                continue;
            }

            let mut head = text;

            for keyword in ["export ", "local ", "async "] {
                head = head.strip_prefix(keyword).unwrap_or(head);
            }

            let Some(rest) = head.strip_prefix("function ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if name.is_empty()
                || !rest[name.len()..].starts_with(['(', '<'])
                || listed.contains(name.as_str())
            {
                continue;
            }

            let detail = declared_head(doc, &name).unwrap_or_else(|| format!("function {name}"));
            let mut item = json!({ "label": name, "kind": 3, "detail": detail });
            let doc_text = doc
                .bindings
                .iter()
                .find(|b| b.name == name)
                .and_then(|b| b.doc.clone());

            if let Some(text) = doc_text {
                item["documentation"] = json!({ "kind": "markdown", "value": text });
            }

            items.push(item);
        }

        items
    }

    /// The macros an import list binds under another name, each with the
    /// file's own word for it. A macro's declaration carries its sigil and
    /// the name the module wrote, `$logit`, so `import { logit as log }`
    /// reaches it under no name the scope walk knows.
    pub(crate) fn aliased_macros(
        &self,
        uri: &str,
    ) -> Vec<(String, &alloy::declarations::Declaration)> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let mut out = Vec::new();

        for entry in import_entries(&doc.source) {
            if entry.bound == entry.name {
                continue;
            }

            let key = format!("${}", entry.name);
            // Two modules may export one macro name, so the module the
            // entry's spec resolves to answers first.
            let target = self
                .resolve_spec(uri, &entry.spec)
                .map(|p| imports::module_path(&p));
            let named = target.and_then(|t| {
                self.docs
                    .iter()
                    .find(|(u, _)| uri_to_path(u).is_some_and(|p| imports::module_path(&p) == t))
                    .map(|(_, d)| d)
            });
            let found = named
                .into_iter()
                .flat_map(|d| d.decls.iter())
                .chain(doc.import_decls.iter())
                .chain(self.docs.values().flat_map(|d| d.decls.iter()))
                .find(|d| d.name == key);

            if let Some(decl) = found {
                out.push((entry.bound.clone(), decl));
            }
        }

        out
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
            // The export list of the file names what a hover does not:
            // an attribute's hover opens with `@name(...)`, and a name
            // an `export { }` list sends out has no `export` in front.
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
                .chain(doc.exports.iter().map(|e| e.name.clone()))
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
}
