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

    /// Renames a name an import list binds. The emit writes the binding
    /// as generated text, so the child's edits map back to the first
    /// byte of the import line: one-character ranges, in the wrong
    /// file as often as the right one.
    ///
    /// The name belongs to the module that declares it, so the rename
    /// reaches the declaration, every import list that names it, every
    /// use under an unaliased entry, and every `M.name` under a module
    /// binding. An entry with an alias keeps its own name: the alias is
    /// this file's word, and renaming it touches this file alone.
    pub(crate) fn rename_answer(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };
        // An attribute is written `@tag`, so a reader may type the
        // sigil into the box. The edit writes the name alone.
        let new_name = message
            .pointer("/params/newName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .trim_start_matches('@')
            .to_string();

        if new_name.is_empty() {
            return false;
        }

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        let Some(entry) = st.import_entry_at(&doc.source, offset) else {
            // The caret on the name a module exports. The child renamed
            // the value beside it there, one character wide, and left
            // every importer as it was.
            if let Some(file) = uri_to_path(uri)
                && keywords::is_word_at(&doc.source, offset)
            {
                let (s, e) = keywords::word_range(&doc.source, offset);
                let word = doc.source[s..e].to_string();
                let declares = export_span(&doc.source, &word) == Some((s, e));
                let exported = imports::exports_of(&doc.source, uri.ends_with(".alx"))
                    .iter()
                    .any(|x| x.name == word);

                if declares
                    && exported
                    && let Some(target) = st.export_rename(&file, &word, &new_name)
                {
                    drop(st);
                    self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": target }));

                    return true;
                }

                // `M.version` under `import * as M`: the module holds
                // the name, so the rename is the export's.
                if let Some((file, name)) = st.module_member_at(uri, &doc.source, offset)
                    && let Some(target) = st.export_rename(&file, &name, &new_name)
                {
                    drop(st);
                    self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": target }));

                    return true;
                }

                // `import M from "./m"` and `import * as M`: the name is
                // this file's own, so the rename stops at its edges.
                if module_bindings(&doc.source)
                    .iter()
                    .any(|(bound, _)| *bound == word)
                    || imports::bound_names(&doc.source).contains(&word)
                {
                    let edits: Vec<Value> = name_uses(&doc.source, &word)
                        .into_iter()
                        .map(|(s, e)| text_edit(&doc.source, s, e, &new_name))
                        .collect();
                    let result = json!({ "changes": { uri: edits } });
                    drop(st);
                    self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

                    return true;
                }
            }

            // An `import` statement holds no other name a rename can
            // reach: not the keywords, not the module path. The child
            // would edit a byte the emit wrote, in another file as
            // often as this one.
            if on_import_statement(&doc.source, offset) {
                drop(st);
                self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": { "changes": {} } }));

                return true;
            }

            return false;
        };
        let on_alias = entry
            .alias_at
            .is_some_and(|(s, e)| (s..=e).contains(&offset));

        // The alias is this file's own word. Nothing outside the file
        // knows it, so nothing outside the file changes.
        if on_alias
            || (entry.alias_at.is_some() && !(entry.name_at.0..=entry.name_at.1).contains(&offset))
        {
            let Some((s, e)) = entry.alias_at else {
                return false;
            };
            let mut edits = vec![text_edit(&doc.source, s, e, &new_name)];

            for (s, e) in name_uses(&doc.source, &entry.bound) {
                if (s, e) != (entry.name_at.0, entry.name_at.1) {
                    edits.push(text_edit(&doc.source, s, e, &new_name));
                }
            }

            edits.dedup();
            let result = json!({ "changes": { uri: edits } });
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        let Some(file) = st.entry_module(uri, &entry) else {
            return false;
        };
        let Some(target) = st.export_rename(&file, &entry.name, &new_name) else {
            return false;
        };
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": target }));

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

        // `import { version } from "./m"`, and every use of `version`
        // under it: the module declares the name, and the emit binds it
        // in generated text the child cannot point at.
        if let Some(result) = st.import_name_definition(uri, &doc.source, offset) {
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
    /// The definition a name in an import list points at: where the
    /// module the line reads declares it. The emit binds the name in
    /// generated text, so the child lands on the first byte of the
    /// import line and the editor shows a one-character range there.
    ///
    /// A use of the name reads the same way: the module is where the
    /// name comes from, and the import list is one step on the way.
    pub(crate) fn import_name_definition(
        &self,
        uri: &str,
        source: &str,
        offset: usize,
    ) -> Option<Value> {
        let (file, name) = match self.import_entry_at(source, offset) {
            Some(entry) => (self.entry_module(uri, &entry)?, entry.name),

            // `M.version` under `import * as M`: the module is the
            // holder, and the field is the name it exports.
            None => {
                let (s, e) = self.module_member_at(uri, source, offset)?;

                (s, e)
            }
        };
        let text = self.module_text(&file)?;
        let (a, b) = export_span(&text, &name)?;
        let s = position_of(&text, a);
        let e = position_of(&text, b);

        Some(json!([{ "uri": path_to_uri(&file), "range": range_value(s, e) }]))
    }

    /// The module and the export name a `M.name` under
    /// `import * as M from "..."` reads.
    pub(crate) fn module_member_at(
        &self,
        uri: &str,
        source: &str,
        offset: usize,
    ) -> Option<(PathBuf, String)> {
        if !keywords::is_word_at(source, offset) {
            return None;
        }

        let (s, e) = keywords::word_range(source, offset);
        let name = source[s..e].to_string();
        let head = source[..s].trim_end().strip_suffix('.')?;
        let at = head.len().saturating_sub(1);

        if head.is_empty() || !keywords::is_word_at(source, at) {
            return None;
        }

        let (hs, he) = keywords::word_range(source, at);
        let holder = &source[hs..he];
        let spec = module_bindings(source)
            .into_iter()
            .find(|(bound, _)| bound == holder)
            .map(|(_, spec)| spec)?;
        let file = imports::module_file(&imports::module_path(&self.resolve_spec(uri, &spec)?))?;

        Some((file, name))
    }

    /// The import entry a byte offset belongs to: one whose own name or
    /// alias holds it, else the entry that binds the word there.
    pub(crate) fn import_entry_at(&self, source: &str, offset: usize) -> Option<ImportEntry> {
        let entries = import_entries(source);
        let inside = |(s, e): (usize, usize)| (s..=e).contains(&offset);

        if let Some(found) = entries
            .iter()
            .find(|e| inside(e.name_at) || e.alias_at.is_some_and(inside))
        {
            return Some(found.clone());
        }

        if !keywords::is_word_at(source, offset) {
            return None;
        }

        let (s, e) = keywords::word_range(source, offset);
        let word = &source[s..e];

        entries.iter().find(|it| it.bound == word).cloned()
    }

    /// The file an entry's module spec names.
    fn entry_module(&self, uri: &str, entry: &ImportEntry) -> Option<PathBuf> {
        imports::module_file(&imports::module_path(&self.resolve_spec(uri, &entry.spec)?))
    }

    /// A module's text: the open document first, then the disk.
    fn module_text(&self, file: &Path) -> Option<String> {
        let uri = path_to_uri(file);

        match self.docs.get(&uri) {
            Some(doc) => Some(doc.source.clone()),

            None => std::fs::read_to_string(file).ok(),
        }
    }

    /// The whole rename of one name a module exports, as a workspace
    /// edit: the declaration and every use in the module, then each
    /// file that imports it. An entry with an alias keeps the alias and
    /// only its own name changes; an entry without one changes with
    /// every use under it, and `M.name` under a module binding too.
    pub(crate) fn export_rename(&self, file: &Path, name: &str, new_name: &str) -> Option<Value> {
        let module = imports::module_path(file);
        let module_uri = path_to_uri(file);
        let text = self.module_text(file)?;

        export_span(&text, name)?;

        let mut changes: Map<String, Value> = Map::new();
        let mut here: Vec<Value> = name_uses(&text, name)
            .into_iter()
            .map(|(s, e)| text_edit(&text, s, e, new_name))
            .collect();

        for (u, d) in &self.docs {
            if *u == module_uri {
                continue;
            }

            let reaches = |spec: &str| {
                self.resolve_spec(u, spec)
                    .map(|p| imports::module_path(&p))
                    .is_some_and(|p| p == module)
            };
            let mine: Vec<ImportEntry> = import_entries(&d.source)
                .into_iter()
                .filter(|it| it.name == name && reaches(&it.spec))
                .collect();
            let holders: Vec<String> = module_bindings(&d.source)
                .into_iter()
                .filter(|(_, spec)| reaches(spec))
                .map(|(bound, _)| bound)
                .collect();
            let mut edits: Vec<Value> = Vec::new();

            // An entry with no alias binds the name itself, so every
            // use of it in the file is this name.
            let plain = mine.iter().any(|it| it.alias_at.is_none());

            for it in &mine {
                edits.push(text_edit(&d.source, it.name_at.0, it.name_at.1, new_name));
            }

            if plain {
                for (s, e) in name_uses(&d.source, name) {
                    edits.push(text_edit(&d.source, s, e, new_name));
                }
            }

            for (s, e) in member_uses(&d.source, &holders, name) {
                edits.push(text_edit(&d.source, s, e, new_name));
            }

            edits.sort_by_key(|e| {
                (
                    e["range"]["start"]["line"].as_u64().unwrap_or(0),
                    e["range"]["start"]["character"].as_u64().unwrap_or(0),
                )
            });
            edits.dedup();

            if !edits.is_empty() {
                changes.insert(u.clone(), json!(edits));
            }
        }

        // A `M.name` read inside the module itself, through its own
        // import of another file, is someone else's name; the walk over
        // this file's own uses has it right already.
        here.sort_by_key(|e| {
            (
                e["range"]["start"]["line"].as_u64().unwrap_or(0),
                e["range"]["start"]["character"].as_u64().unwrap_or(0),
            )
        });
        here.dedup();

        if !here.is_empty() {
            changes.insert(module_uri, json!(here));
        }

        (!changes.is_empty()).then(|| json!({ "changes": changes }))
    }

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

/// Whether a byte offset sits inside an `import` statement. A name list
/// may run over several lines, so the statement reaches to the end of
/// the line its `from` sits on.
fn on_import_statement(src: &str, offset: usize) -> bool {
    let mut line_start = 0;

    for line in src.split_inclusive('\n') {
        let here = line_start;
        line_start += line.len();

        if !line.trim_start().starts_with("import ") {
            continue;
        }

        let end = match src[here..].find(" from ") {
            Some(i) => src[here + i..]
                .find('\n')
                .map_or(src.len(), |j| here + i + j),

            None => line_start,
        };

        if (here..=end).contains(&offset) {
            return true;
        }
    }

    false
}

/// One text edit, from a byte range of a source.
fn text_edit(src: &str, start: usize, end: usize, new_text: &str) -> Value {
    json!({
        "range": range_value(position_of(src, start), position_of(src, end)),
        "newText": new_text,
    })
}

/// One entry of an `import { ... }` list, with where its parts sit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportEntry {
    /// The name the module exports, without the `@` of an attribute.
    pub name: String,
    /// The name this file writes: the alias when the entry has one.
    pub bound: String,
    /// The byte range of the export name inside the entry.
    pub name_at: (usize, usize),
    /// The byte range of the alias, when the entry has one.
    pub alias_at: Option<(usize, usize)>,
    /// The module path the line reads, as the source spells it.
    pub spec: String,
}

/// Every name an `import { ... }` list of the file binds, with byte
/// ranges. The emit writes the binding as generated text, so the child
/// answers about a byte no author wrote; these ranges are the author's.
///
/// The list may run over several lines, so the walk reads from the
/// `import` to the `from` of the same statement and not to the end of
/// the line.
pub(crate) fn import_entries(src: &str) -> Vec<ImportEntry> {
    let mut out = Vec::new();
    let mut line_start = 0;

    for line in src.split_inclusive('\n') {
        let here = line_start;
        line_start += line.len();

        if !line.trim_start().starts_with("import ") {
            continue;
        }

        // The `from` closes the head of the statement. A `{` past it
        // belongs to someone else's code.
        let Some(from) = src[here..].find(" from ").map(|i| here + i) else {
            continue;
        };
        let head = &src[here..from];
        let Some(open) = head.find('{').map(|i| here + i) else {
            continue;
        };
        let Some(close) = src[open..from].find('}').map(|i| open + i) else {
            continue;
        };
        let tail_end = src[from..].find('\n').map_or(src.len(), |i| from + i);
        let Some(spec) = import_spec(&src[from..tail_end]) else {
            continue;
        };

        for entry in split_entries(&src[open + 1..close]) {
            let at = open + 1 + entry.0;
            let words = words_of(entry.1);
            // `type T`, `T as U`, `type T as U`, `@tag`, `@tag as t`.
            let (name, alias) = match words.as_slice() {
                [name, (_, "as"), alias] => (*name, Some(*alias)),
                [(_, "type"), name, (_, "as"), alias] => (*name, Some(*alias)),
                [(_, "type"), name] => (*name, None),
                [name] => (*name, None),
                _ => continue,
            };

            fn bare(text: &str) -> &str {
                text.trim_start_matches('@')
            }

            let sigil = name.1.len() - bare(name.1).len();

            out.push(ImportEntry {
                name: bare(name.1).to_string(),
                bound: bare(alias.map_or(name.1, |a| a.1)).to_string(),
                name_at: (at + name.0 + sigil, at + name.0 + name.1.len()),
                alias_at: alias.map(|a| {
                    let start = at + a.0 + (a.1.len() - bare(a.1).len());

                    (start, at + a.0 + a.1.len())
                }),
                spec: spec.clone(),
            });
        }
    }

    out
}

/// The entries of one list, each with its byte offset inside the text
/// between the braces.
fn split_entries(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;

    for part in text.split(',') {
        out.push((at, part));
        at += part.len() + 1;
    }

    out
}

/// The words of one entry, each with its byte offset inside the entry.
fn words_of(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;

    while at < text.len() {
        let rest = &text[at..];
        let skip = rest.len() - rest.trim_start().len();
        let start = at + skip;
        let word = text[start..].split_whitespace().next().unwrap_or("");

        if word.is_empty() {
            break;
        }

        out.push((start, word));
        at = start + word.len();
    }

    out
}

/// The keywords that stand right in front of the name a declaration
/// binds. `export` and `global` say where the name goes, not what it
/// is, so they sit further left.
const DECLARES: [&str; 13] = [
    "const",
    "local",
    "function",
    "type",
    "struct",
    "enum",
    "trait",
    "interface",
    "class",
    "attribute",
    "macro",
    "remote",
    "namespace",
];

/// Where a module declares a name it exports, as the byte range of the
/// name. `export default` has an answer of its own, in
/// `default_import_definition`.
pub(crate) fn export_span(src: &str, name: &str) -> Option<(usize, usize)> {
    let lexed = alloy_syntax::lexer::lex(src).ok()?;
    let toks = &lexed.toks;

    toks.iter().enumerate().find_map(|(i, t)| {
        let before = toks.get(i.wrapping_sub(1))?.text(src);

        (t.text(src) == name && DECLARES.contains(&before))
            .then_some((t.start as usize, t.end as usize))
    })
}

/// The names an import line binds to a whole module, each with the spec
/// of its line: `import * as M from "./m"` and nothing else. A member
/// of such a module is written `M.name`, which a rename of `name` has
/// to follow.
///
/// A default binding is not one of these. `import M from "./m"` on a
/// module with an export table binds the `default` field, so `M.name`
/// there is a field of that value and not the module's export.
pub(crate) fn module_bindings(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();

    for line in src.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("import ") else {
            continue;
        };
        let Some(after_star) = rest.trim_start().strip_prefix('*') else {
            continue;
        };
        let Some(name) = after_star.trim_start().strip_prefix("as ") else {
            continue;
        };
        let Some(spec) = import_spec(line) else {
            continue;
        };

        out.push((
            name.split_whitespace().next().unwrap_or("").to_string(),
            spec,
        ));
    }

    out.retain(|(name, _)| !name.is_empty());
    out
}

/// Every `Holder.name` in a source, as the byte range of `name`.
fn member_uses(src: &str, holders: &[String], name: &str) -> Vec<(usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let toks = &lexed.toks;
    let mut out = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.text(src) != name || i < 2 {
            continue;
        }

        if toks[i - 1].text(src) == "." && holders.iter().any(|h| h == toks[i - 2].text(src)) {
            out.push((t.start as usize, t.end as usize));
        }
    }

    out
}
