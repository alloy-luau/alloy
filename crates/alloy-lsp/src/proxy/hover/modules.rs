use super::*;

impl Server {
    /// The hover of a name the source declares and the child sees only
    /// as a table: a `remote`, and the namespace an `import * as` binds.
    pub(crate) fn source_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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

        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return false;
        };
        let word = doc.source[start..end].to_string();
        let path = uri_to_path(uri);
        // A `remote` or an exported `const` the file imported is
        // declared somewhere else; the child reads the emitted local
        // and calls it a `local`.
        let imported = || {
            doc.import_sources.iter().find_map(|text| {
                remote_hover(text, &word)
                    .or_else(|| const_hover(text, &word))
                    .or_else(|| function_hover(text, &word))
            })
        };
        let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
        let quoted = doc.source[line_start..start].matches('"').count() % 2 == 1
            || doc.source[line_start..start].matches('\'').count() % 2 == 1;
        // The line the caret sits on, for a word inside a path string:
        // one file can name the same service on two lines.
        let line_end = doc.source[start..]
            .find('\n')
            .map_or(doc.source.len(), |i| start + i);
        let spec_line = quoted.then(|| &doc.source[line_start..line_end]);
        let shadowed = shadows_an_import(&doc.source, &word, start);
        let inner = |answer: Option<String>| answer.filter(|_| !shadowed);
        let answer = inner(remote_hover(&doc.source, &word))
            .or_else(|| inner(imported()))
            .or_else(|| {
                (!quoted)
                    .then(|| inner(std_import_hover(&doc.source, &word, start)))
                    .flatten()
            })
            .or_else(|| service_hover(&doc.source, &word, spec_line))
            .or_else(|| {
                let dir = path
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new("."))
                    .to_path_buf();
                let aliases = project_aliases(&dir, st.root.as_deref());

                module_hover(&doc.source, &word, path.as_deref(), &aliases, quoted)
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
}

/// Whether a nearer binding than an import holds
/// the word at `start`: a `local`, a parameter, a `for` variable, or a
/// `case` binding. The child types that one, and the outer declaration
/// says nothing about it.
///
/// The caret's own line counts on its own: `locals_in_scope` leaves a
/// `local x = |` out, since there the caret sits in the value and not in
/// the name. The scope reads from the end of that line, so a parameter
/// answers where its own list writes it.
pub(crate) fn shadows_an_import(source: &str, word: &str, start: usize) -> bool {
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |i| start + i);
    let line = &source[line_start..line_end];

    // An import line binds nothing of its own here: `bindings` reads
    // the declaring keywords, and `import` is not one.
    alloy::declarations::bindings(line)
        .iter()
        .any(|b| b.name == word)
        || crate::context::locals_in_scope(source, line_end)
            .iter()
            .any(|l| l.name == word)
}

/// The hover of a `remote`: the declaration as the source wrote it,
/// with the comment above it.
pub(crate) fn remote_hover(source: &str, word: &str) -> Option<String> {
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
pub(crate) fn const_hover(source: &str, word: &str) -> Option<String> {
    let mut at = 0;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text
            .strip_prefix("export const ")
            .or_else(|| text.strip_prefix("global const "))
            .or_else(|| text.strip_prefix("global local "))
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if name == word {
                // The value is the module's, not the reader's.
                let head = text.split_once(" = ").map_or(text, |(h, _)| h);
                // A declaration with no annotation still has a type.
                // Without it the reader sees `global local hp` and has
                // to open the other file to learn what it holds.
                let head = match head.contains(':') {
                    true => head.to_string(),

                    false => match alloy::declarations::binding_type(source, word) {
                        Some(ty) => format!("{head}: {ty}"),

                        None => head.to_string(),
                    },
                };
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

/// The declaration of an exported function, with the comment above
/// it. The emit binds the name here as a local, and the child types
/// that local as `unknown`, so the module's own declaration answers.
pub(crate) fn function_hover(source: &str, word: &str) -> Option<String> {
    let (head, offset) = alloy::declarations::export_head(source, word)?;
    let doc_text = alloy::declarations::doc_before(source, offset)
        .map(|d| format!("\n\n{d}"))
        .unwrap_or_default();

    Some(format!("```alloy\n{head}\n```{doc_text}"))
}

/// What a `remote` declaration says: whether it answers, which sides
/// fire it, and whether it carries `@ratelimit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteSpec {
    pub(crate) answers: bool,
    pub(crate) from_client: bool,
    pub(crate) from_server: bool,
    pub(crate) ratelimited: bool,
}

/// The `remote` declaration a source writes for `name`, with the
/// attributes above it.
pub(crate) fn remote_spec(source: &str, name: &str) -> Option<RemoteSpec> {
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
pub(crate) fn module_hover(
    source: &str,
    word: &str,
    from: Option<&Path>,
    aliases: &[(String, PathBuf)],
    in_spec: bool,
) -> Option<String> {
    // On the binding the child's answer stands: the module's table, as
    // it prints. On the path the answer is the file the path names.
    if !in_spec {
        return None;
    }

    let line = source.lines().find(|l| {
        let l = l.trim();

        l.starts_with("import ") && import_spec(l).is_some_and(|spec| spec_names(&spec, word))
    })?;
    let spec = import_spec(line)?;

    // A module the server cannot find is the child's to answer.
    module_target(&spec, from, aliases)?;

    // No link: the editor's document links already offer to follow the
    // path, on the same characters.
    Some(format!("```alloy\n{}\n```", line.trim()))
}

/// The hover of a name a std import binds. The emit reads the std from
/// the runtime, so the child prints the whole runtime table or an alias
/// of its own. A star import's alias hovers as its module: the import
/// line and what the module exports, the list `std.` completes. An
/// alias of a std name, `HashMap as Map`, hovers as that name. A std
/// type with no doc entry, `SignalConnection`, hovers as the runtime
/// declares it.
pub(crate) fn std_import_hover(source: &str, word: &str, start: usize) -> Option<String> {
    // `x.serde` is a member, not the alias.
    if source[..start].ends_with(['.', ':']) {
        return None;
    }

    if let Some(names) = crate::proxy::completion::std_module_names(source, word) {
        let line = source.lines().map(str::trim).find(|l| {
            l.starts_with("import ")
                && l.contains(&format!(" as {word} "))
                && import_spec(l).is_some_and(|s| alloy::std_names::module_of_spec(&s).is_some())
        })?;
        let module = alloy::std_names::module_of_spec(&import_spec(line)?)?.to_string();
        let attributes = alloy::std_names::ATTRIBUTES
            .iter()
            .filter(|(m, _)| module.is_empty() || *m == module)
            .flat_map(|(_, names)| names.iter().map(|n| format!("`@{n}`")));
        let exports: Vec<String> = names
            .iter()
            .map(|n| format!("`{n}`"))
            .chain(attributes)
            .collect();

        return Some(format!(
            "```alloy\n{line}\n```\nExports: {}",
            exports.join(", ")
        ));
    }

    let entry = super::super::navigation::import_entries(source)
        .into_iter()
        .find(|e| {
            e.bound == word
                && alloy::std_names::module_of_spec(&e.spec).is_some()
                && alloy::std_names::is_std_name(&e.name)
        })?;
    let documented = alloy::docs::type_markdown(&entry.name)
        .or_else(|| keywords::doc(&entry.name).map(str::to_string));

    match (entry.alias_at, documented) {
        (Some(_), Some(text)) => Some(format!(
            "The std `{}`, imported as `{word}`.\n\n{text}",
            entry.name
        )),

        (Some(_), None) => Some(format!(
            "The std `{}`, imported as `{word}`.\n\n{}",
            entry.name,
            runtime_type(&entry.name)?
        )),

        // A documented name keeps the hover the keyword pass gives it.
        (None, Some(_)) => None,

        (None, None) => runtime_type(&entry.name),
    }
}

/// A std type as the runtime declares it, its comments aside.
fn runtime_type(name: &str) -> Option<String> {
    let head = format!("export type {name}");
    let mut out: Vec<String> = Vec::new();
    let mut depth = 0i32;

    for line in alloy::RUNTIME
        .lines()
        .skip_while(|l| !(l.starts_with(&head) && l[head.len()..].starts_with([' ', '<', '='])))
    {
        let code = line.split("--").next().unwrap_or_default().trim_end();

        if code.trim().is_empty() {
            continue;
        }

        depth += code.matches(['{', '(']).count() as i32;
        depth -= code.matches(['}', ')']).count() as i32;
        out.push(code.replace('\t', "    "));

        // A union goes on past a `}` that ends in `|`.
        if depth <= 0 && !code.ends_with(['|', '=', ',']) {
            break;
        }
    }

    let text = out.join("\n");

    (!text.is_empty()).then(|| format!("```alloy\n{}\n```", text.trim_start_matches("export ")))
}

/// Whether an import path holds `word` as one of its segments, the
/// alias included: `@pkg/fluid` names `pkg` and `fluid`.
pub(crate) fn spec_names(spec: &str, word: &str) -> bool {
    spec.trim_start_matches('@')
        .split('/')
        .any(|segment| segment == word || segment.trim_end_matches(".luau") == word)
}

/// The spec of an import line, whichever quote it uses.
pub(crate) fn import_spec(line: &str) -> Option<String> {
    let at = line.rfind(" from ")? + " from ".len();
    let rest = line[at..].trim();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &rest[quote.len_utf8()..];
    let end = body.find(quote)?;

    Some(body[..end].to_string())
}

/// The file a spec names: an `@alias/tail` through the project's
/// aliases, anything else relative to the importing file.
pub(crate) fn module_target(
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

impl RemoteSpec {
    /// Whether a file on `side` may fire the remote. A file with no
    /// side of its own sees both surfaces.
    pub(crate) fn fires(&self, side: Option<alloy::directives::Side>) -> bool {
        match side {
            Some(alloy::directives::Side::Client) => self.from_client,
            Some(alloy::directives::Side::Server) => self.from_server,
            None => true,
        }
    }

    /// Whether a file on `side` may handle the remote.
    pub(crate) fn handles(&self, side: Option<alloy::directives::Side>) -> bool {
        match side {
            Some(alloy::directives::Side::Client) => self.from_server,
            Some(alloy::directives::Side::Server) => self.from_client,
            None => true,
        }
    }

    /// Whether the surface holds a member. The emit types every member
    /// on every remote, so the declaration and the file's side are what
    /// tell them apart.
    pub(crate) fn holds(&self, member: &str, side: Option<alloy::directives::Side>) -> bool {
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

/// The hover of a Roblox service an import binds, on the binding and on
/// the path. The child reads the emitted `game:GetService` and prints
/// the whole class table; the import line and one line about the
/// service say what the reader asked.
///
/// `spec_line` is the line the caret sits on when it sits inside a path
/// string. `None` means the word is a binding, which any line of the
/// file may have bound.
pub(crate) fn service_hover(source: &str, word: &str, spec_line: Option<&str>) -> Option<String> {
    let lines: Vec<&str> = match spec_line {
        Some(line) => vec![line],

        None => source.lines().collect(),
    };

    for line in lines {
        let text = line.trim();

        if !text.starts_with("import ") {
            continue;
        }

        let bound = imports::service_bindings(text);

        if bound.is_empty() {
            continue;
        }

        // Inside the path, `game` names every service the line binds
        // and `game:Players` names the one it spells out. Outside it,
        // the word is the local the line binds.
        let hit: Vec<&str> = match spec_line.is_some() {
            true => match word == "game" || bound.iter().any(|(_, s)| s == word) {
                true => bound.iter().map(|(_, s)| s.as_str()).collect(),

                false => Vec::new(),
            },

            false => bound
                .iter()
                .filter(|(local, _)| local == word)
                .map(|(_, s)| s.as_str())
                .collect(),
        };

        if hit.is_empty() {
            continue;
        }

        // On the binding, the reader wants the type the name carries,
        // the way every other binding hovers. The import line above is
        // the line they are already looking at. Inside the path, one
        // word can name several services, so the line stands.
        let head = match (spec_line.is_none(), hit.as_slice()) {
            (true, [service]) => format!("local {word}: {service}"),

            _ => text.to_string(),
        };
        let mut out = format!("```alloy\n{head}\n```");

        for service in hit {
            out.push('\n');
            out.push_str(&alloy::game_import::service_summary(service));
        }

        return Some(out);
    }

    None
}
