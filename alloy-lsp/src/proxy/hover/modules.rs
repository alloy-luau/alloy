use super::*;

impl Server {
    /// The hover of a name the source declares and the child sees only
    /// as a table: a `remote`, and the namespace an `import * as` binds.
    pub(crate) fn source_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
        let quoted = doc.source[line_start..start].matches('"').count() % 2 == 1
            || doc.source[line_start..start].matches('\'').count() % 2 == 1;
        // The line the caret sits on, for a word inside a path string:
        // one file can name the same service on two lines.
        let line_end = doc.source[start..]
            .find('\n')
            .map_or(doc.source.len(), |i| start + i);
        let spec_line = quoted.then(|| &doc.source[line_start..line_end]);
        // A project global is a bare name. `Analytics.count` names a
        // member, and a global that shares the word says nothing about
        // it.
        let after_separator = follows_a_separator(&doc.source, start);
        // A project global is declared in another file and needs no
        // import, so the file that wrote it is the one to read.
        let from_global = || {
            if after_separator {
                return None;
            }

            let owner = global_owner(&st, uri, &word)?;

            // A `remote` has no type the child can print, so the
            // declaration answers. A `global local` or a `global const`
            // is a value the child types off the binding the first line
            // writes, and that type is the inferred one; the answer
            // comes back through `restyle_global_hover`, which puts the
            // declaring keywords in front of it.
            remote_hover(&owner.source, &word)
        };
        let answer = remote_hover(&doc.source, &word)
            .or_else(imported)
            .or_else(from_global)
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

/// The declaration a project global wrote, for a hover the child left
/// unanswered. The type is the one the source wrote, since nothing
/// here infers one.
pub(crate) fn global_declaration_hover(
    doc: &Doc,
    st: &State,
    uri: &str,
    line: u32,
    character: u32,
) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);

    if follows_a_separator(&doc.source, start) {
        return None;
    }

    let word = &doc.source[start..end];
    let owner = global_owner(st, uri, word)?;

    const_hover(&owner.source, word)
}

/// The open document that declares `word` as a project global this
/// file reaches. A global needs no import, so the file that wrote it
/// is the one to read.
pub(crate) fn global_owner<'a>(st: &'a State, uri: &str, word: &str) -> Option<&'a Doc> {
    st.docs
        .iter()
        .find(|(u, d)| {
            u.as_str() != uri
                && d.globals
                    .iter()
                    .any(|g| g.name == word && st.global_reaches(uri, u, g))
        })
        .map(|(_, d)| d)
}

/// Whether a word starts right after a `.` or a `:`, which makes it a
/// member of what stands before it and no name of its own.
pub(crate) fn follows_a_separator(source: &str, start: usize) -> bool {
    matches!(source[..start].chars().next_back(), Some('.' | ':'))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `Analytics.count` names a member. A project global that shares
    /// the word said `global local count` about it, which is another
    /// file's declaration and nothing to do with the module.
    #[test]
    fn a_member_is_no_project_global() {
        let src = "import Analytics from \"./a\"\nprint(Analytics.count(), count)\n";
        let at = |word: &str| follows_a_separator(src, src.find(word).expect("the word"));
        assert!(at("count("));
        assert!(!at("count)"));
        assert!(!at("Analytics."));
    }
}
