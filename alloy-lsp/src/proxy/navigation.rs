//! Navigation: go to definition, across a source file, a data file, and a default import.

use alloy_syntax::lexer::TokKind;

use super::hover::import_spec;
use super::outline::enum_variants;
use super::*;

impl Server {
    /// The references of a namespace and of one of its members. The
    /// emit renames a member, so the child, which reads the artifact,
    /// answers with the name the reader never wrote.
    pub(crate) fn namespace_references(&self, uri: &str, message: &Value, id: &Value) -> bool {
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

        let Some(Caret { offset, start, end }) = Caret::at(&doc.source, line, character) else {
            return false;
        };
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

        let Some((line, character)) = position_of_message(message) else {
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

        let target = st.name_target(uri, offset);

        // The new name already stands where the rename writes. The edit
        // set would bind one name twice and change what the file means,
        // so the request gets the error the child answers with.
        if let Some(clash) = st.rename_clash(uri, offset, target.as_ref(), &new_name) {
            drop(st);
            self.to_client(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32803, "message": clash },
            }));

            return true;
        }

        let Some(target) = target else {
            return false;
        };
        let result = match target {
            Target::Export(file, name) => match st.export_rename(&file, &name, &new_name) {
                Some(edit) => edit,

                None => return false,
            },

            Target::Local(name) => {
                let mut edits: Vec<Value> = name_uses(&doc.source, &name)
                    .into_iter()
                    .map(|(s, e)| text_edit(&doc.source, s, e, &new_name))
                    .collect();
                edits.dedup();

                json!({ "changes": { uri: edits } })
            }

            Target::Variant { file, owner, name } => {
                match st.variant_edits(&file, &owner, &name, &new_name) {
                    Some(edit) => edit,

                    None => return false,
                }
            }

            Target::Method { trait_name, name } => {
                match st.method_edits(&trait_name, &name, &new_name) {
                    Some(edit) => edit,

                    None => return false,
                }
            }

            Target::Field { owner, name } => match st.field_edits(&owner, &name, &new_name) {
                Some(edit) => edit,

                None => return false,
            },

            Target::Nothing => json!({ "changes": {} }),
        };
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// The references of a name a module exports and of a name this
    /// file alone binds. The rename walks the workspace for the same
    /// set, so both answers read one walk and cannot disagree.
    pub(crate) fn name_references(&self, uri: &str, message: &Value, id: &Value) -> bool {
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

        let Some(target) = st.name_target(uri, offset) else {
            return false;
        };
        let out = match target {
            Target::Export(file, name) => match st.export_rename(&file, &name, &name) {
                Some(edit) => locations_of(&edit),

                None => return false,
            },

            Target::Local(name) => name_uses(&doc.source, &name)
                .into_iter()
                .map(|(s, e)| {
                    json!({
                        "uri": uri,
                        "range": range_value(
                            position_of(&doc.source, s),
                            position_of(&doc.source, e),
                        ),
                    })
                })
                .collect(),

            Target::Variant { file, owner, name } => {
                match st.variant_edits(&file, &owner, &name, &name) {
                    Some(edit) => locations_of(&edit),

                    None => return false,
                }
            }

            Target::Method { trait_name, name } => {
                match st.method_edits(&trait_name, &name, &name) {
                    Some(edit) => locations_of(&edit),

                    None => return false,
                }
            }

            Target::Field { owner, name } => match st.field_edits(&owner, &name, &name) {
                Some(edit) => locations_of(&edit),

                None => return false,
            },

            Target::Nothing => Vec::new(),
        };
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

        // `self:area()` inside a trait's own default method. Every
        // implementation writes the method again; the trait declares it
        // once, and that line is what the reader means.
        if raw_before.ends_with("self:")
            && let Some((a, b)) = trait_method_span(&doc.source, line, word)
        {
            let s = position_of(&doc.source, a);
            let e = position_of(&doc.source, b);
            let result = json!([{ "uri": uri, "range": range_value(s, e) }]);
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        // `value:method()`: the receiver is a local or a literal, and
        // the emit writes the method on the target's table, where the
        // child lands on generated text. The `impl` block that declares
        // the method is what the reader means, and a generic header or a
        // foreign target changes nothing about that.
        if raw_before.ends_with(':')
            && let Some(result) = st.impl_method_definition(uri, word)
        {
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

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

        // A `case` binding: the arm's pattern is where the name comes
        // from, and the match lowers to one expression, so the emit
        // writes no local for the child to point at.
        if key == word
            && let Some((a, b)) =
                case_binding_span(doc, line as usize, word, &st.known_shapes_at(Some(uri)))
        {
            let s = position_of(&doc.source, a);
            let e = position_of(&doc.source, b);
            let result = json!([{ "uri": uri, "range": range_value(s, e) }]);
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        // `export local Size = 42` binds the name here. Another file's
        // `Size` is a declaration of its own and says nothing about the
        // binding the caret sits on, so the child answers for this one.
        let bound_here = binds_a_value(&doc.bindings, &key);
        let found = doc
            .decls
            .iter()
            .find(|d| d.name == key)
            .map(|d| (uri.to_string(), doc, d))
            .or_else(|| {
                (!bound_here).then(|| {
                    st.docs.iter().find_map(|(u, d)| {
                        d.decls
                            .iter()
                            .find(|x| x.name == key)
                            .map(|x| (u.clone(), d, x))
                    })
                })?
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

    /// The enum that declares a variant by this name and that the file
    /// at hand reaches: the enum this file declares itself, or one whose
    /// name the file writes.
    fn variant_owner(&self, uri: &str, name: &str) -> Option<String> {
        let doc = self.docs.get(uri)?;
        let here = doc
            .shapes
            .iter()
            .find(|s| names_a_variant(s, name))
            .map(|s| s.name().to_string());

        if here.is_some() {
            return here;
        }

        self.docs
            .values()
            .flat_map(|d| d.shapes.iter())
            .find(|s| names_a_variant(s, name) && !name_uses(&doc.source, s.name()).is_empty())
            .map(|s| s.name().to_string())
    }

    /*
    The whole rename of one enum variant, as a workspace edit.

    The declaration sits in the enum body and every use is written
    `Shape.Circle`, under whatever name the reading file bound the enum
    to. The emit turns the variant into a tag, so nothing the child sees
    carries either place.
    */
    pub(crate) fn variant_edits(
        &self,
        file: &Path,
        owner: &str,
        name: &str,
        new_name: &str,
    ) -> Option<Value> {
        let module = imports::module_path(file);
        let module_uri = path_to_uri(file);
        let text = self.module_text(file)?;
        let (start, end) = enum_variants(&text)
            .into_iter()
            .find(|(o, v, _)| o == owner && v == name)
            .map(|(_, _, at)| at)?;
        let mut changes: Map<String, Value> = Map::new();
        let mut here = vec![text_edit(&text, start, end, new_name)];

        for (a, b) in member_uses(&text, std::slice::from_ref(&owner.to_string()), name) {
            here.push(text_edit(&text, a, b, new_name));
        }

        for (u, d) in &self.docs {
            if *u == module_uri {
                continue;
            }

            let reaches = |spec: &str| {
                self.resolve_spec(u, spec)
                    .map(|p| imports::module_path(&p))
                    .is_some_and(|p| p == module)
            };
            let mut holders: Vec<String> = import_entries(&d.source)
                .into_iter()
                .filter(|it| it.name == owner && reaches(&it.spec))
                .map(|it| it.bound)
                .collect();

            // `import * as M`: the enum reads `M.Shape`, so the word
            // before the variant is still the enum's own name.
            if module_bindings(&d.source)
                .iter()
                .any(|(_, spec)| reaches(spec))
            {
                holders.push(owner.to_string());
            }

            let mut edits: Vec<Value> = member_uses(&d.source, &holders, name)
                .into_iter()
                .map(|(a, b)| text_edit(&d.source, a, b, new_name))
                .collect();
            edits.sort_by_key(sort_key);
            edits.dedup();

            if !edits.is_empty() {
                changes.insert(u.clone(), json!(edits));
            }
        }

        here.sort_by_key(sort_key);
        here.dedup();
        changes.insert(module_uri, json!(here));

        Some(json!({ "changes": changes }))
    }

    /*
    What the caret names, for a rename and for a reference list.

    The name belongs to the module that declares it, so both answers
    reach the declaration, every import list that names it, every use
    under an unaliased entry, and every `M.name` under a module
    binding. An entry with an alias keeps its own name: the alias is
    this file's word, and it means nothing outside the file.
    */
    pub(crate) fn name_target(&self, uri: &str, offset: usize) -> Option<Target> {
        let doc = self.docs.get(uri)?;
        let source = &doc.source;

        if let Some(entry) = self.import_entry_at(source, offset) {
            let on_name = (entry.name_at.0..=entry.name_at.1).contains(&offset);

            if entry.alias_at.is_some() && !on_name {
                return Some(Target::Local(entry.bound));
            }

            return Some(Target::Export(self.entry_module(uri, &entry)?, entry.name));
        }

        if let Some(file) = uri_to_path(uri)
            && keywords::is_word_at(source, offset)
        {
            let (s, e) = keywords::word_range(source, offset);
            let word = source[s..e].to_string();
            // A trait's method: the trait declares it once and every
            // `impl` writes it again, so the three places are one name.
            if let Some(target) = self.method_target(doc, offset) {
                return Some(target);
            }

            let declares = export_span(source, &word) == Some((s, e));
            let exported = imports::exports_of(source, doc.is_alx)
                .iter()
                .any(|x| x.name == word);

            if declares && exported {
                return Some(Target::Export(file, word));
            }

            // `M.version` under `import * as M`: the module holds the
            // name, so the answer is the export's.
            if let Some((file, name)) = self.module_member_at(uri, source, offset) {
                return Some(Target::Export(file, name));
            }

            // A variant of an enum this file reaches: the one the
            // caret sits on in the enum body, or the one after the dot
            // of `Shape.Circle`.
            if let Some(owner) = self.variant_owner(uri, &word) {
                let declares = self.docs.iter().find_map(|(u, d)| {
                    let holds = d
                        .shapes
                        .iter()
                        .any(|s| s.name() == owner && names_a_variant(s, &word));

                    holds.then(|| uri_to_path(u)).flatten()
                });

                if let Some(file) = declares {
                    return Some(Target::Variant {
                        file,
                        owner,
                        name: word,
                    });
                }
            }

            // A field, where the struct body declares it. The emit
            // rewrites the field list, so the child finds no name to
            // rename there. A constructor key is text the child reads,
            // and the mend pass finishes that one.
            if declared_field_hover(doc, s, e).is_some()
                && let Some(owner) = self.field_owner(doc, s, e)
            {
                return Some(Target::Field { owner, name: word });
            }

            // A declaration this file keeps to itself. The emit writes
            // the header as generated text, so the child points at a
            // byte the reader never wrote: the `e` of `enum`, the `t`
            // of `trait`. No other file reaches the name, so every use
            // of it here is this declaration.
            if !exported && declares_a_local_shape(source, &word) {
                return Some(Target::Local(word));
            }

            // `import M from "./m"` and `import * as M`: the name is
            // this file's own, so the answer stops at its edges.
            if module_bindings(source)
                .iter()
                .any(|(bound, _)| *bound == word)
                || imports::bound_names(source).contains(&word)
            {
                return Some(Target::Local(word));
            }
        }

        on_import_statement(source, offset).then_some(Target::Nothing)
    }

    /// The trait method at the caret: the trait's own declaration, a
    /// method of an `impl Trait for S`, or a call on a value whose
    /// struct has such an impl. `None` for anything else, and for a
    /// method of a plain `impl S`, which no trait declares.
    fn method_target(&self, doc: &Doc, offset: usize) -> Option<Target> {
        let (s, e) = keywords::word_range(&doc.source, offset);
        let name = doc.source[s..e].to_string();
        let here = trait_method_sites(&doc.source)
            .into_iter()
            .find(|site| site.at == (s, e));

        if let Some(site) = here {
            return site
                .trait_name
                .map(|trait_name| Target::Method { trait_name, name });
        }

        // `b:hello()`: the struct the receiver holds says which trait,
        // through its own `impl` of it.
        let head = doc.source[..s].trim_end();

        if !head.ends_with(':') || head.ends_with("::") {
            return None;
        }

        let owner = receiver_type(doc, head.len() - 1);
        let mut traits: Vec<String> = Vec::new();

        for site in self
            .docs
            .values()
            .flat_map(|d| trait_method_sites(&d.source))
        {
            let Some(trait_name) = site.trait_name else {
                continue;
            };
            let mine = match (&owner, &site.target) {
                (Some(o), Some(t)) => o == t,

                // A receiver with no type of its own: one trait that
                // declares the name is still the one the call means.
                (None, _) => true,

                _ => false,
            };

            if site.name == name && mine && !traits.contains(&trait_name) {
                traits.push(trait_name);
            }
        }

        match traits.as_slice() {
            [trait_name] => Some(Target::Method {
                trait_name: trait_name.clone(),
                name,
            }),

            // Two traits of one method name say nothing about which one
            // the call reaches; the child answers instead.
            _ => None,
        }
    }

    /*
    The whole rename of one trait method, as a workspace edit.

    A trait declares the method once, every `impl Trait for S` writes it
    again, and a call reads it off a value. The emit gives a trait no
    table of its own and types the receiver as `any`, so the child finds
    the impl it stands in and nothing else.

    A call counts when the receiver holds a struct that implements the
    trait. One whose type the source does not say counts only when no
    other trait and no plain `impl` writes the name, so a call on
    another struct that shares the spelling stays as it is.
    */
    pub(crate) fn method_edits(
        &self,
        trait_name: &str,
        name: &str,
        new_name: &str,
    ) -> Option<Value> {
        let sites: Vec<(String, MethodSite)> = self
            .docs
            .iter()
            .flat_map(|(u, d)| {
                trait_method_sites(&d.source)
                    .into_iter()
                    .map(move |site| (u.clone(), site))
            })
            .collect();
        let targets: Vec<&str> = sites
            .iter()
            .filter(|(_, s)| s.name == name && s.trait_name.as_deref() == Some(trait_name))
            .filter_map(|(_, s)| s.target.as_deref())
            .collect();
        // The name belongs to this trait alone: nothing else declares it.
        let only_one = !sites
            .iter()
            .any(|(_, s)| s.name == name && s.trait_name.as_deref() != Some(trait_name));
        let mut changes: Map<String, Value> = Map::new();

        for (u, d) in &self.docs {
            let mut edits: Vec<Value> = sites
                .iter()
                .filter(|(owner, s)| {
                    owner == u && s.name == name && s.trait_name.as_deref() == Some(trait_name)
                })
                .map(|(_, s)| text_edit(&d.source, s.at.0, s.at.1, new_name))
                .collect();

            for (start, end, receiver) in method_calls(&d.source, name) {
                let holds = match receiver_type(d, receiver) {
                    Some(ty) => targets.contains(&ty.as_str()),

                    None => only_one,
                };

                if holds {
                    edits.push(text_edit(&d.source, start, end, new_name));
                }
            }

            edits.sort_by_key(sort_key);
            edits.dedup();

            if !edits.is_empty() {
                changes.insert(u.clone(), json!(edits));
            }
        }

        (!changes.is_empty()).then(|| json!({ "changes": changes }))
    }

    /// The struct a field at the caret belongs to: the receiver's type
    /// of `c.x`, the name in front of the brace of `new Shape { x = 1 }`
    /// or of `case Shape { x }`, or the struct whose body declares it.
    fn field_owner(&self, doc: &Doc, start: usize, end: usize) -> Option<String> {
        if let Some(owner) = used_field_owner(doc, start) {
            return Some(owner);
        }

        if let Some((owner, false)) = context::struct_literal_target(&doc.source, start) {
            return Some(owner);
        }

        // `x: number` in a struct body, as the field hover reads it: the
        // nearest declaration above that still carries fields.
        declared_field_hover(doc, start, end)?;

        doc.decls
            .iter()
            .filter(|d| d.offset < start && declared_field_owner(d).is_some())
            .max_by_key(|d| d.offset)
            .map(|d| d.name.clone())
    }

    /// The two places a rename of a struct field reaches and the child
    /// does not. `new Shape { x = 1 }` lowers to a table the emit hands
    /// to a constructor, where the child reads a plain record and ties
    /// the key to no field; the struct's own field list is generated
    /// text, so the child's edit of the declaration lands on the byte
    /// the header came from and rewrites whatever stands there.
    ///
    /// The pass runs beside the child's own edits and never alone: an
    /// answer with no edit names no field, and one place by itself would
    /// rename half of one.
    pub(crate) fn mend_field_rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let changes = result.pointer("/changes").and_then(Value::as_object);
        let Some(new_name) = changes
            .into_iter()
            .flat_map(Map::values)
            .filter_map(Value::as_array)
            .flatten()
            .find_map(|e| e.get("newText").and_then(Value::as_str))
            .map(str::to_string)
        else {
            return;
        };
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return;
        };
        let name = doc.source[start..end].to_string();
        let Some(owner) = self.field_owner(doc, start, end) else {
            return;
        };
        let Some(changes) = result
            .pointer_mut("/changes")
            .and_then(Value::as_object_mut)
        else {
            return;
        };

        for (u, d) in &self.docs {
            let mut mine: Vec<Value> = field_sites(d, &owner, &name)
                .into_iter()
                .map(|(s, e)| text_edit(&d.source, s, e, &new_name))
                .collect();

            let Some(list) = changes.get_mut(u).and_then(Value::as_array_mut) else {
                if !mine.is_empty() {
                    mine.sort_by_key(sort_key);
                    changes.insert(u.clone(), json!(mine));
                }

                continue;
            };
            list.retain(|e| edits_the_word(&d.source, e, &name));

            for edit in mine {
                if !list.contains(&edit) {
                    list.push(edit);
                }
            }

            list.sort_by_key(sort_key);
        }
    }

    /*
    The clash a rename would make, as the message that refuses it.

    The scan reads the places the edit set writes and no more: the file
    for a name the file binds, the module for an export, the struct for
    a field, the enum for a variant, and the trait with every impl of it
    for a method. One file is one scope here, so a name another block
    binds counts too; the walks the rename reads are file wide as well.

    A name the caret does not point at belongs to the child, which knows
    its own scopes. A local is the one exception: the child renames it,
    and the reader still loses the meaning of the line.
    */
    pub(crate) fn rename_clash(
        &self,
        uri: &str,
        offset: usize,
        target: Option<&Target>,
        new_name: &str,
    ) -> Option<String> {
        let doc = self.docs.get(uri)?;
        let says = |kind: &str, src: &str, at: usize| {
            let article = match kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
                true => "an",

                false => "a",
            };
            let line = position_of(src, at).0 + 1;

            Some(format!(
                "`{new_name}` is already {article} {kind} on line {line}"
            ))
        };

        let at_word = keywords::is_word_at(&doc.source, offset)
            .then(|| keywords::word_range(&doc.source, offset));

        // A rename onto the name the caret already carries writes
        // nothing, and the site it finds is that name's own.
        if at_word.is_some_and(|(s, e)| doc.source[s..e] == *new_name) {
            return None;
        }

        match target {
            // A name this file alone binds, and a struct with no
            // export: the file is the whole scope.
            Some(Target::Local(_)) => {
                let (kind, at) = bound_at(&doc.source, new_name)?;

                says(&kind, &doc.source, at)
            }

            Some(Target::Export(file, _)) => {
                let text = self.module_text(file)?;
                let (kind, at) = bound_at(&text, new_name)?;

                says(&kind, &text, at)
            }

            Some(Target::Field { owner, .. }) => self.docs.values().find_map(|d| {
                let (at, _) = field_declaration(&d.source, owner, new_name)?;

                says("field", &d.source, at)
            }),

            Some(Target::Variant { file, owner, .. }) => {
                let text = self.module_text(file)?;
                let (at, _) = enum_variants(&text)
                    .into_iter()
                    .find(|(o, v, _)| o == owner && v == new_name)
                    .map(|(_, _, at)| at)?;

                says("variant", &text, at)
            }

            Some(Target::Method { trait_name, .. }) => self.docs.values().find_map(|d| {
                let site = trait_method_sites(&d.source)
                    .into_iter()
                    .find(|s| s.trait_name.as_deref() == Some(trait_name) && s.name == *new_name)?;

                says("method", &d.source, site.at.0)
            }),

            Some(Target::Nothing) => None,

            // The child writes this rename. A local of the file is
            // still the reader's own name, so the clash is one the
            // proxy has to name; a method lives on its struct instead,
            // and the file says nothing about it.
            None => {
                let (s, e) = at_word?;
                let word = &doc.source[s..e];
                let head = doc.source[..s].trim_end();
                // A word after a dot is a member of another value, and a
                // method lives on its own struct. Neither reads the
                // names of the file.
                let member = (head.ends_with('.') && !head.ends_with(".."))
                    || trait_method_sites(&doc.source)
                        .iter()
                        .any(|site| site.name == word);
                let mine = bound_at(&doc.source, word).is_some_and(|(kind, _)| {
                    matches!(kind.as_str(), "local" | "const" | "function")
                });

                if member || !mine {
                    return None;
                }

                let (kind, at) = bound_at(&doc.source, new_name)?;

                says(&kind, &doc.source, at)
            }
        }
    }

    /// The whole rename of one struct field, as a workspace edit. The
    /// caret sits where the struct body declares the field, and the
    /// child answers nothing at all there, so this walk writes every
    /// place by itself.
    pub(crate) fn field_edits(&self, owner: &str, name: &str, new_name: &str) -> Option<Value> {
        let mut changes: Map<String, Value> = Map::new();

        for (u, d) in &self.docs {
            let edits: Vec<Value> = field_sites(d, owner, name)
                .into_iter()
                .map(|(s, e)| text_edit(&d.source, s, e, new_name))
                .collect();

            if !edits.is_empty() {
                changes.insert(u.clone(), json!(edits));
            }
        }

        (!changes.is_empty()).then(|| json!({ "changes": changes }))
    }

    /// Where the project declares an `impl` method of that name, with
    /// the URI of the file that wrote it. The file at hand answers
    /// first, then the rest of the workspace: a method of an imported
    /// struct is declared where the struct is.
    pub(crate) fn impl_method_definition(&self, uri: &str, name: &str) -> Option<Value> {
        let here = self.docs.get(uri).map(|d| (uri.to_string(), d));
        let rest = self
            .docs
            .iter()
            .filter(|(u, _)| u.as_str() != uri)
            .map(|(u, d)| (u.clone(), d));

        here.into_iter().chain(rest).find_map(|(u, d)| {
            let (a, b) = impl_method_span(&d.source, name)?;
            let s = position_of(&d.source, a);
            let e = position_of(&d.source, b);

            Some(json!([{ "uri": u, "range": range_value(s, e) }]))
        })
    }

    /// The file an entry's module spec names.
    fn entry_module(&self, uri: &str, entry: &ImportEntry) -> Option<PathBuf> {
        imports::module_file(&imports::module_path(&self.resolve_spec(uri, &entry.spec)?))
    }

    /// A module's text: the open document first, then the disk.
    pub(crate) fn module_text(&self, file: &Path) -> Option<String> {
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

        // A definitions file writes no import: an ambient declaration
        // stands in scope everywhere, so a type name it spells is this
        // one. Only a type reaches one, so a value skips those files.
        let is_type = declares_a_type(&text, name);
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
            // use of it in the file is this name. An ambient file names
            // the type with no entry at all.
            let plain =
                mine.iter().any(|it| it.alias_at.is_none()) || (is_type && u.ends_with(".d.aly"));

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
    /// and `import M, { a } from` both bind it.
    ///
    /// Two bindings hold the whole module instead: the `M` of
    /// `import * as M`, and the binding of a plain Luau module, which
    /// declares no default. Both land on the module's own file.
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

        // `import * as M` binds the table the module hands back, not
        // the one name its `export default` writes.
        if head.contains('*') {
            return module_location(&file);
        }

        if !is_alx && !file.extension().is_some_and(|e| e == "aly") {
            return module_location(&file);
        }

        let text = std::fs::read_to_string(&file).ok()?;
        let (a, b) = imports::default_span(&text, is_alx)?;
        let s = position_of(&text, a as usize);
        let e = position_of(&text, b as usize);

        Some(json!([{ "uri": path_to_uri(&file), "range": range_value(s, e) }]))
    }
}

/// Where one `impl` block of a source declares a method, as the byte
/// range of its name. A receiver is a local or a literal, so the target
/// is not in the text at the caret; a name two blocks declare says
/// nothing about which one the call reaches, and the child answers
/// those.
/// Where the trait whose body holds `line` declares `name`. A trait
/// stands at the margin, so its `end` is the first `end` in column
/// zero under it.
pub(crate) fn trait_method_span(src: &str, line: u32, name: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = src.lines().collect();
    let at = (line as usize).min(lines.len().saturating_sub(1));
    let head = (0..=at).rev().find(|i| {
        let text = lines[*i];
        let text = text.strip_prefix("export ").unwrap_or(text);
        let text = text.strip_prefix("global ").unwrap_or(text);

        text.starts_with("trait ")
    })?;
    let close = (head + 1..lines.len()).find(|i| lines[*i] == "end")?;

    if at <= head || at > close {
        return None;
    }

    let mut start = lines[..=head].iter().map(|l| l.len() + 1).sum::<usize>();

    for text in &lines[head + 1..close] {
        let line_start = start;
        start += text.len() + 1;
        let trimmed = text.trim_start();
        let body = trimmed
            .strip_prefix("private ")
            .or_else(|| trimmed.strip_prefix("public "))
            .unwrap_or(trimmed);
        let Some(rest) = body.strip_prefix("function ") else {
            continue;
        };

        if !rest.starts_with(name) || !rest[name.len()..].starts_with(['(', '<']) {
            continue;
        }

        let at = line_start + (text.len() - rest.len());

        return Some((at, at + name.len()));
    }

    None
}

pub(crate) fn impl_method_span(src: &str, name: &str) -> Option<(usize, usize)> {
    let mut inside = false;
    let mut at = 0;
    let mut found = None;

    for line in src.lines() {
        let start = at;
        at += line.len() + 1;
        let text = line.trim();
        let head = text.strip_prefix("export ").unwrap_or(text);

        if head.starts_with("impl ") {
            inside = true;

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

        let head = text.strip_prefix("private ").unwrap_or(text);
        let Some(rest) = head.strip_prefix("function ") else {
            continue;
        };

        if !rest.starts_with(name) || !rest[name.len()..].starts_with('(') {
            continue;
        }

        // A method takes `self`; an associated function like
        // `Vec2.new` is written `Vec2.new(` at the call and reaches
        // the declaration path instead.
        if !rest[name.len()..].starts_with("(self") {
            continue;
        }

        if found.is_some() {
            return None;
        }

        let indent = line.len() - line.trim_start().len();
        let offset = start + indent + (text.len() - head.len()) + "function ".len();
        found = Some((offset, offset + name.len()));
    }

    found
}

/// One place a method of a trait is written: the trait's own body, or
/// an `impl` block.
pub(crate) struct MethodSite {
    /// The trait the site belongs to: the trait itself, or the one an
    /// `impl Trait for S` meets. `None` for a plain `impl S`.
    pub trait_name: Option<String>,
    /// The type an `impl` targets. `None` inside a trait's own body.
    pub target: Option<String>,
    pub name: String,
    /// The byte range of the method's name.
    pub at: (usize, usize),
}

/// The leading name of a text: `Alpha<T> as` gives `Alpha`.
fn name_head(text: &str) -> String {
    text.trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Every method a source writes inside a `trait` body or an `impl`
/// block. A trait and an impl both stand at the margin and both close
/// with an `end` there, so the header above a line says which block it
/// belongs to.
pub(crate) fn trait_method_sites(src: &str) -> Vec<MethodSite> {
    let mut out = Vec::new();
    let mut head: Option<(Option<String>, Option<String>)> = None;
    let mut at = 0;

    for line in src.lines() {
        let start = at;
        at += line.len() + 1;
        let text = line.trim();
        let margin = !line.starts_with([' ', '\t']);
        let bare = text.strip_prefix("export ").unwrap_or(text);
        let bare = bare.strip_prefix("global ").unwrap_or(bare);

        if let Some(rest) = bare.strip_prefix("trait ") {
            head = Some((Some(name_head(rest)), None));

            continue;
        }

        if let Some(rest) = bare.strip_prefix("impl ") {
            head = match rest.split_once(" for ") {
                Some((t, s)) => Some((Some(name_head(t)), Some(name_head(s)))),

                None => Some((None, Some(name_head(rest)))),
            };

            continue;
        }

        // A body is indented, and the block closes with an `end` at the
        // margin. A blank line has no margin and closes nothing.
        if margin && (text == "end" || !text.is_empty()) {
            head = None;
        }

        let Some((trait_name, target)) = &head else {
            continue;
        };
        let body = text
            .strip_prefix("private ")
            .or_else(|| text.strip_prefix("public "))
            .unwrap_or(text);
        let Some(rest) = body.strip_prefix("function ") else {
            continue;
        };
        let name = name_head(rest);

        if name.is_empty() || !rest[name.len()..].starts_with(['(', '<']) {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        let offset = start + indent + (text.len() - body.len()) + "function ".len();
        let at = (offset, offset + name.len());
        out.push(MethodSite {
            trait_name: trait_name.clone(),
            target: target.clone(),
            name,
            at,
        });
    }

    out
}

/// Every `recv:name(` call of a source: the byte range of the method's
/// name, and a byte of the receiver in front of it.
fn method_calls(src: &str, name: &str) -> Vec<(usize, usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let toks = &lexed.toks;
    let mut out = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.text(src) != name || i < 2 {
            continue;
        }

        if !matches!(toks[i - 1].text(src), ":" | "?:") {
            continue;
        }

        let opens_a_call = toks.get(i + 1).is_some_and(|n| {
            matches!(
                n.kind,
                TokKind::LParen | TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpHead
            ) || n.text(src) == "{"
        });

        if opens_a_call {
            out.push((t.start as usize, t.end as usize, toks[i - 1].start as usize));
        }
    }

    out
}

/// Where a struct body declares one field, as the byte range of the
/// name. `None` when the source declares no such struct or no such
/// field.
fn field_declaration(src: &str, owner: &str, name: &str) -> Option<(usize, usize)> {
    let (at, _) = export_span(src, owner)?;
    let head = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut offset = src[head..].find('\n').map_or(src.len(), |i| head + i + 1);

    for line in src[offset..].lines() {
        if line == "end" {
            break;
        }

        if field_key(line) == Some(name)
            && let Some(col) = whole_word(line, name)
        {
            return Some((offset + col, offset + col + name.len()));
        }

        offset += line.len() + 1;
    }

    None
}

/// Where a source already binds a name: the keyword that declares it,
/// and the byte the name starts at. An import list binds a name too,
/// and its keyword sits on another line, so the entry answers as one.
fn bound_at(src: &str, name: &str) -> Option<(String, usize)> {
    if let Some((at, _)) = export_span(src, name) {
        let head = src[..at].trim_end();
        let (ks, ke) = keywords::word_range(src, head.len().checked_sub(1)?);

        return Some((src[ks..ke].to_string(), at));
    }

    import_entries(src)
        .into_iter()
        .find(|it| it.bound == name)
        .map(|it| ("import".to_string(), it.alias_at.unwrap_or(it.name_at).0))
}

/// Every place one source writes a field of a struct: the declaration
/// in the struct body, each key of a constructor, and each read off a
/// receiver of that type. A receiver whose type the source does not
/// say names no struct, so it stays as it is.
fn field_sites(doc: &Doc, owner: &str, name: &str) -> Vec<(usize, usize)> {
    let mut out = constructor_keys(&doc.source, owner, name);
    out.extend(field_declaration(&doc.source, owner, name));

    if let Ok(lexed) = alloy_syntax::lexer::lex(&doc.source) {
        out.extend(
            lexed
                .toks
                .iter()
                .filter(|t| t.text(&doc.source) == name)
                .filter(|t| used_field_owner(doc, t.start as usize).as_deref() == Some(owner))
                .map(|t| (t.start as usize, t.end as usize)),
        );
    }

    out.sort_unstable();
    out.dedup();
    out
}

/// The declaration keywords the emit rewrites: the header becomes
/// generated text, so the child answers about a byte no author wrote.
/// A `local`, a `const` and a plain `function` survive the emit, and the
/// child renames those itself.
const SHAPES: [&str; 8] = [
    "struct",
    "enum",
    "trait",
    "interface",
    "type",
    "attribute",
    "macro",
    "namespace",
];

/// Whether a source declares one of those shapes by that name. Such a
/// declaration is this file's own unless the line exports it, which the
/// caller reads for itself.
fn declares_a_local_shape(src: &str, name: &str) -> bool {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return false;
    };
    let toks = &lexed.toks;

    toks.iter().enumerate().any(|(i, t)| {
        let before = i.checked_sub(1).map(|p| toks[p].text(src));

        t.text(src) == name && before.is_some_and(|w| SHAPES.contains(&w))
    })
}

/// Whether one edit of the child's stands where the source spells the
/// name it renames. An edit that maps onto generated text lands on the
/// byte the construct came from, which says something else.
fn edits_the_word(src: &str, edit: &Value, name: &str) -> bool {
    let Some(((sl, sc), (el, ec))) = edit.get("range").and_then(range_of) else {
        return true;
    };
    let Some(start) = offset_of(src, sl, sc) else {
        return true;
    };
    let Some(end) = offset_of(src, el, ec) else {
        return true;
    };

    src.get(start..end) == Some(name)
}

/// Every `name =` key of a constructor of `owner` in a source: the
/// literal of `new Owner { ... }`, and the one a typed binding takes. A
/// nested literal reads by the brace it sits in, so an inner struct
/// keeps its own keys.
fn constructor_keys(src: &str, owner: &str, name: &str) -> Vec<(usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for (i, t) in lexed.toks.iter().enumerate() {
        if t.text(src) != name || lexed.toks.get(i + 1).map(|n| n.text(src)) != Some("=") {
            continue;
        }

        let at = context::struct_literal_target(src, t.start as usize);

        if matches!(at, Some((ref target, false)) if target == owner) {
            out.push((t.start as usize, t.end as usize));
        }
    }

    out
}

/// The module file itself, for a binding that holds the whole module.
/// A plain Luau module hands its table over on a top level `return`,
/// which is the line the reader means; a module with no such line opens
/// at its first.
fn module_location(file: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(file).ok()?;
    let at = (module_head_line(&text), 0);

    Some(json!([{ "uri": path_to_uri(file), "range": range_value(at, at) }]))
}

/// The line a whole-module binding opens at: the last top level
/// `return`, which is where a plain Luau module hands its table over.
/// A module with no such line opens at its first.
pub(crate) fn module_head_line(text: &str) -> u32 {
    text.lines()
        .enumerate()
        .filter(|(_, line)| *line == "return" || line.starts_with("return "))
        .last()
        .map_or(0, |(i, _)| i as u32)
}

/// Whether a path lies in a dot directory of the project: `.ember`
/// holds the packages a require reaches and `.alloy` the build's
/// sourcemap, so both stand in the mirror the child indexes. Neither is
/// a source the reader wrote. The auto import walk leaves every dot
/// directory out, and `workspace/symbol` agrees with it.
pub(crate) fn in_a_dot_directory(path: &Path, mirror: &Path, root: Option<&Path>) -> bool {
    let Some(rest) = path
        .strip_prefix(mirror)
        .ok()
        .or_else(|| root.and_then(|r| path.strip_prefix(r).ok()))
    else {
        return false;
    };

    rest.parent().is_some_and(|dirs| {
        dirs.components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    })
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
pub(crate) fn whole_word(line: &str, word: &str) -> Option<usize> {
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
pub(crate) fn name_uses(src: &str, name: &str) -> Vec<(usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for (i, t) in lexed.toks.iter().enumerate() {
        if t.text(src) != name {
            continue;
        }

        let before = i.checked_sub(1).map(|p| lexed.toks[p].text(src));
        let after_dot = matches!(before, Some("." | "?."));
        // `v: Vec2` names the type; `obj:method(...)` names a member of
        // the receiver. Only a call follows the method, so the token
        // after the name decides which one this is.
        let opens_a_call = lexed.toks.get(i + 1).is_some_and(|n| {
            matches!(
                n.kind,
                TokKind::LParen | TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpHead
            ) || n.text(src) == "{"
        });
        let method_call = matches!(before, Some(":" | "?:")) && opens_a_call;

        if !after_dot && !method_call {
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

/*
What a caret names, for the two answers that have to agree.

`textDocument/rename` and `textDocument/references` ask the same
question about one byte: which name is this, and where else is it
written. One walk answers both, so an edit the rename writes always has
a reference beside it.
*/
pub(crate) enum Target {
    /// A name a module exports, with the file that declares it.
    Export(PathBuf, String),
    /// A name this file alone binds: the alias of an import entry, or
    /// the binding of a whole module.
    Local(String),
    /// An enum variant, with the file that declares the enum and the
    /// enum's own name. The emit leaves a variant as a tag inside a
    /// record, so the child has no binding to point at.
    Variant {
        file: PathBuf,
        owner: String,
        name: String,
    },
    /// A field of a struct, with the struct's own name. The emit
    /// rewrites the field list and hands a constructor a plain record,
    /// so the child ties neither place to a field.
    Field { owner: String, name: String },
    /// A method a trait declares. The trait writes it once, every
    /// `impl Trait for S` writes it again, and a call reads it off a
    /// value; the emit types the receiver as `any`, so the child ties
    /// none of the three together.
    Method { trait_name: String, name: String },
    /// An `import` statement holds no other name either answer can
    /// reach: not the keywords, not the module path. The child would
    /// point at a byte the emit wrote.
    Nothing,
}

/// Whether a shape is an enum with a variant of that name.
fn names_a_variant(shape: &alloy::declarations::Shape, name: &str) -> bool {
    match shape {
        alloy::declarations::Shape::Enum { variants, .. } => {
            variants.iter().any(|(v, _)| v == name)
        }

        _ => false,
    }
}

/// Where one edit starts, so a file's edits read in source order.
fn sort_key(edit: &Value) -> (u64, u64) {
    (
        edit["range"]["start"]["line"].as_u64().unwrap_or(0),
        edit["range"]["start"]["character"].as_u64().unwrap_or(0),
    )
}

/// The locations of a workspace edit, for the reference list that
/// mirrors it.
fn locations_of(edit: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let changes = edit.pointer("/changes").and_then(Value::as_object);

    for (uri, edits) in changes.into_iter().flatten() {
        for e in edits.as_array().into_iter().flatten() {
            out.push(json!({ "uri": uri, "range": e["range"].clone() }));
        }
    }

    out
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

/// Whether a module declares a name as a type: a `struct`, an `enum`, a
/// `trait`, an `interface`, a `class`, or a `type` alias. A definitions
/// file names one of those and never a value.
fn declares_a_type(src: &str, name: &str) -> bool {
    const TYPES: [&str; 6] = ["type", "struct", "enum", "trait", "interface", "class"];

    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return false;
    };
    let toks = &lexed.toks;

    toks.iter().enumerate().any(|(i, t)| {
        let before = i.checked_sub(1).map(|p| toks[p].text(src));

        t.text(src) == name && before.is_some_and(|w| TYPES.contains(&w))
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
