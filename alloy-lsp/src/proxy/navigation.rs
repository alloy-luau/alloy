//! Navigation: go to definition, across a source file, a data file, and a default import.

use super::hover::import_spec;
use super::*;

impl Server {
    /// The references of a project global: the declaration, and every
    /// file that names it. A global reaches every file with no import,
    /// so the child, which reads one file's requires, cannot find them.
    pub(crate) fn global_references(&self, uri: &str, message: &Value, id: &Value) -> bool {
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

        // The global this file reaches by that name. A global of the
        // other side is another name; nothing here refers to it.
        let Some(side) = st.docs.iter().find_map(|(u, d)| {
            d.globals
                .iter()
                .find(|g| g.name == word && st.global_reaches(uri, u, g))
                .map(|g| g.side_directive.unwrap_or_else(|| st.side_at(u)))
        }) else {
            return false;
        };

        let mut out: Vec<Value> = Vec::new();

        for (u, d) in &st.docs {
            // A file that cannot reach the global writes another name.
            if !alloy::globals::reaches(side, st.side_at(u)) {
                continue;
            }

            for (s, e) in name_uses(&d.source, &word) {
                out.push(json!({
                    "uri": u,
                    "range": range_value(position_of(&d.source, s), position_of(&d.source, e)),
                }));
            }
        }

        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": out }));

        true
    }

    /// The references of a namespace and of one of its members. The
    /// emit renames a member, so the child, which reads the artifact,
    /// answers with the name the reader never wrote.
    pub(crate) fn namespace_references(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        // The namespace this name belongs to: the group itself, or the
        // one that declares a member by this name.
        let owner = st.docs.iter().find_map(|(u, d)| {
            let hit = d.namespace_ranges.iter().find(|n| {
                n.path.rsplit('.').next() == Some(word.as_str())
                    || (offset >= n.start
                        && offset <= n.end
                        && n.members.iter().any(|(m, _)| *m == word))
                    || (doc.source[..start].ends_with('.')
                        && n.members.iter().any(|(m, _)| *m == word))
            });

            hit.map(|n| (u.clone(), n.clone()))
        });
        let Some((home, owner)) = owner else {
            return false;
        };
        let is_group = owner.path.rsplit('.').next() == Some(word.as_str());
        let mut out: Vec<Value> = Vec::new();

        for (u, d) in &st.docs {
            // A namespace the module keeps to itself is named in that
            // file alone; the same spelling elsewhere is another one.
            if !owner.exported && *u != home {
                continue;
            }

            for (s, e) in name_uses(&d.source, &word) {
                // A member reads bare inside its namespace and by the
                // path outside; a name of the same spelling anywhere
                // else is not this one.
                if !is_group {
                    let inside = *u == home && s >= owner.start && s <= owner.end;

                    if !inside {
                        continue;
                    }
                }

                out.push(json!({
                    "uri": u,
                    "range": range_value(position_of(&d.source, s), position_of(&d.source, e)),
                }));
            }

            if is_group {
                continue;
            }

            // `Math.PI` outside the namespace: the member after the dot.
            let path = format!("{}.{word}", owner.path);
            let mut from = 0;

            while let Some(i) = d.source[from..].find(&path) {
                let at = from + i;
                let member = at + owner.path.len() + 1;
                out.push(json!({
                    "uri": u,
                    "range": range_value(
                        position_of(&d.source, member),
                        position_of(&d.source, member + word.len()),
                    ),
                }));
                from = at + path.len();
            }
        }

        out.sort_by_key(|v| {
            (
                v["uri"].as_str().unwrap_or("").to_string(),
                v["range"]["start"]["line"].as_u64().unwrap_or(0),
                v["range"]["start"]["character"].as_u64().unwrap_or(0),
            )
        });
        out.dedup();
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": out }));

        true
    }

    /// Go to definition for a name Alloy declares: a struct, an enum or a
    /// variant, a trait, an interface, a type alias, a macro, or an
    /// attribute, in this file first and then any file of the workspace.
    /// The child answers for everything the emit keeps as written.
    pub(crate) fn definition_answer(&self, uri: &str, message: &Value, id: &Value) -> bool {
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

        let (word_start, word_end) = keywords::word_range(&doc.source, offset);

        // A Roblox service: the emitted `local` is generated text, so
        // the child lands at the start of the import line. The name the
        // line binds is where the reader means to go.
        if let Some(result) =
            service_definition(&doc.source, uri, &doc.source[word_start..word_end])
        {
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        // A project global: the name reaches this file with no import,
        // and the emit binds it on the first line, so the child would
        // land there. The declaration is what the reader means.
        let word = &doc.source[word_start..word_end];

        if !doc.globals.iter().any(|g| g.name == word)
            && let Some((target_uri, target)) = st.docs.iter().find_map(|(u, d)| {
                d.globals
                    .iter()
                    .find(|g| g.name == word && st.global_reaches(uri, u, g))
                    .map(|g| (u.clone(), g))
            })
        {
            let target_doc = &st.docs[&target_uri];
            let s = position_of(&target_doc.source, target.offset as usize);
            let e = position_of(&target_doc.source, target.offset as usize + word.len());
            let result = json!([{ "uri": target_uri, "range": range_value(s, e) }]);
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        // `import M from "./m"`: the binding names the module's
        // `export default`, wherever that sits.
        if let Some(result) = st.default_import_definition(uri, &doc.source, offset) {
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
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
}

impl State {
    /// The definition a default import's binding names: the
    /// `export default` of the module the line reads. `import M from`
    /// and `import M, { a } from` both bind it. A plain Luau module has
    /// no such declaration, and the path link already opens the file.
    pub(crate) fn default_import_definition(
        &self,
        uri: &str,
        source: &str,
        offset: usize,
    ) -> Option<Value> {
        let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line_end = source[offset..]
            .find('\n')
            .map_or(source.len(), |i| offset + i);
        let line = &source[line_start..line_end];

        if !line.trim_start().starts_with("import ") {
            return None;
        }

        let (start, end) = keywords::word_range(source, offset);
        let word = &source[start..end];
        let at = start - line_start;
        let head = &line[..at];

        // The binding sits between `import` and the braces or `from`;
        // a name in braces is a named import, which the child answers.
        if matches!(word, "import" | "type" | "as" | "from")
            || head.contains('{')
            || head.contains(" from ")
            || !head.trim_start().starts_with("import")
        {
            return None;
        }

        let spec = import_spec(line)?;
        let file = imports::module_file(&imports::module_path(&self.resolve_spec(uri, &spec)?))?;
        let is_alx = file.extension().is_some_and(|e| e == "alx");

        if !is_alx && !file.extension().is_some_and(|e| e == "aly") {
            return None;
        }

        let text = std::fs::read_to_string(&file).ok()?;
        let (a, b) = imports::default_span(&text, is_alx)?;
        let s = position_of(&text, a as usize);
        let e = position_of(&text, b as usize);

        Some(json!([{ "uri": path_to_uri(&file), "range": range_value(s, e) }]))
    }
}

/// The file an instance path names, through the `[mount]` table:
/// `ReplicatedStorage.Packages.fluid` under
/// `pkg = ["packages/roblox", "@game/ReplicatedStorage/Packages"]` is
/// `packages/roblox/fluid`.
pub(crate) fn module_file_of(instance: &str, mounts: &[(String, PathBuf)]) -> Option<PathBuf> {
    let segments = instance_segments(instance);

    for (prefix, dir) in mounts {
        let head = instance_segments(prefix);

        if segments.len() <= head.len() || !segments.starts_with(&head) {
            continue;
        }

        let mut file = dir.clone();

        for part in &segments[head.len()..] {
            file.push(part);
        }

        return Some(file);
    }

    None
}

/// The parts of an instance path. A part that is no identifier arrives
/// in brackets, so `Packages[".ember"].jecs` is `Packages`, `.ember`,
/// `jecs`. A package store folder reaches the path that way.
pub(crate) fn instance_segments(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;

    while !rest.is_empty() {
        rest = rest.trim_start_matches('.');

        let Some(after) = rest.strip_prefix('[') else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());

            if end == 0 {
                break;
            }

            out.push(rest[..end].to_string());
            rest = &rest[end..];

            continue;
        };

        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let body = &after[quote.len_utf8()..];
        let Some(end) = body.find(quote) else {
            break;
        };

        out.push(body[..end].to_string());
        rest = body[end + quote.len_utf8()..]
            .strip_prefix(']')
            .unwrap_or("");
    }

    out
}

/// The definition a position in a data import names: on the path, the
/// data file's first line; on an imported name, the line of that key.
/// None when the line holds no data path or the file is not there.
pub(crate) fn data_definition(source: &str, offset: usize, dir: &Path) -> Option<Value> {
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
pub(crate) fn data_module_of(path: &Path) -> Option<PathBuf> {
    if alloy::data::Format::of_path(path).is_none() || alloy::data::module_beside(path).is_some() {
        return None;
    }

    Some(path.with_extension("luau"))
}

/// The data file behind a mirror module: the child answers about
/// `x.luau`, and the editor has `x.json` or `x.toml` when no real
/// `x.luau` exists.
pub(crate) fn data_source_of(path: PathBuf) -> PathBuf {
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

/// The definition a Roblox service binding names: the name its import
/// line binds. The emit writes the `local` as generated text, which
/// carries no column of its own.
pub(crate) fn service_definition(source: &str, uri: &str, word: &str) -> Option<Value> {
    let mut at = 0usize;

    for line in source.lines() {
        if imports::service_bindings(line)
            .iter()
            .any(|(local, _)| local == word)
            && let Some(col) = whole_word(line, word)
        {
            let s = position_of(source, at + col);
            let e = position_of(source, at + col + word.len());

            return Some(json!([{ "uri": uri, "range": range_value(s, e) }]));
        }

        at += line.len() + 1;
    }

    None
}

/// Where a word sits in a line on its own, not inside a longer name:
/// `Run` in `{ RunService as Run }` is the second match, not the first.
fn whole_word(line: &str, word: &str) -> Option<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    line.match_indices(word)
        .find(|(at, _)| {
            let before = line[..*at].chars().next_back();
            let after = line[at + word.len()..].chars().next();

            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .map(|(at, _)| at)
}

/// Every use of a name in a source, as byte ranges. The lexer leaves
/// comments and strings out, and a name after a `.` or a `:` is a
/// field of something else, not this one.
fn name_uses(src: &str, name: &str) -> Vec<(usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for (i, t) in lexed.toks.iter().enumerate() {
        if t.text(src) != name {
            continue;
        }

        let after_dot = i
            .checked_sub(1)
            .is_some_and(|p| matches!(lexed.toks[p].text(src), "." | ":" | "?." | "?:"));

        if !after_dot {
            out.push((t.start as usize, t.end as usize));
        }
    }

    out
}
