use super::*;

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
            // A `global` needs no import: it is in scope in every file
            // of a side that reaches it. The compiler reads the same
            // rule, so a name the list offers is a name that compiles.
            let globals: HashSet<String> = doc
                .decls
                .iter()
                .filter(|d| {
                    d.hover
                        .lines()
                        .nth(1)
                        .is_some_and(|l| l.starts_with("global "))
                })
                .map(|d| bound_name(&d.name))
                .filter(|bound| {
                    doc.globals
                        .iter()
                        .find(|g| g.name == *bound)
                        .is_some_and(|g| self.global_reaches(uri, u, g))
                })
                .collect();

            for d in &doc.decls {
                let bound = bound_name(&d.name);
                let reachable = own
                    || (exports.contains(&bound) && imported.contains(&bound))
                    || globals.contains(&bound);

                if reachable && seen.insert(d.name.clone()) {
                    out.push(d);
                }
            }
        }

        out
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
