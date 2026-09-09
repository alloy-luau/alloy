//! Navigation: go to definition, across a source file, a data file, and a default import.

use super::hover::import_spec;
use super::*;

impl Server {
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
    for (prefix, dir) in mounts {
        let Some(tail) = instance
            .strip_prefix(prefix.as_str())
            .and_then(|t| t.strip_prefix('.'))
        else {
            continue;
        };
        let mut file = dir.clone();

        for part in tail.split('.') {
            file.push(part);
        }

        return Some(file);
    }

    None
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
