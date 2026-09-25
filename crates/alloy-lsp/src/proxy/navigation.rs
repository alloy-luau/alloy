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
        let module = uri_to_path(&home).map(|p| imports::module_path(&p));
        let mut out: Vec<Value> = Vec::new();

        for (u, d) in &st.docs {
            // A namespace the module keeps to itself is named in that
            // file alone; the same spelling elsewhere is another one.
            if !owner.exported && *u != home {
                continue;
            }

            let Some(module) = &module else {
                continue;
            };

            // The group itself, under every word this file writes it
            // under: `M.Ns`, the alias of `import { Ns as A }`, and the
            // plain `Ns`.
            if is_group {
                for (s, e) in st.group_uses(u, &d.source, module, &owner.path) {
                    out.push(json!({
                        "uri": u,
                        "range": range_value(position_of(&d.source, s), position_of(&d.source, e)),
                    }));
                }

                continue;
            }

            // A member reads bare inside its namespace and by the path
            // outside; a name of the same spelling anywhere else is not
            // this one.
            for (s, e) in name_uses(&d.source, &word) {
                if *u != home || s < owner.start || s > owner.end {
                    continue;
                }

                out.push(json!({
                    "uri": u,
                    "range": range_value(position_of(&d.source, s), position_of(&d.source, e)),
                }));
            }

            // `Math.PI` outside the namespace: the member after the
            // dot, under every word this file puts the group under.
            for (a, b) in member_uses(
                &d.source,
                &st.namespace_heads(u, &d.source, module, &owner.path),
                &word,
            ) {
                out.push(json!({
                    "uri": u,
                    "range": range_value(position_of(&d.source, a), position_of(&d.source, b)),
                }));
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

    /// A caret on an attribute of a component's tag: the prop's field in
    /// the component's props type answers. Definition lands there, and
    /// rename and references ask the child there, as though the caret
    /// sat on the field. The mend adds the tags back to that answer.
    pub(crate) fn prop_answer(&self, method: &str, uri: &str, message: &Value, id: &Value) -> bool {
        if !uri.ends_with(".alx") {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let field = {
            let st = self.state.lock().expect("state");
            let spot = st.docs.get(uri).and_then(|doc| {
                markup::hover_spot(&doc.source, offset_of(&doc.source, line, character)?)
            });

            match spot {
                Some(markup::Spot::Attribute { class, name }) => {
                    st.prop_declaration(uri, &class, &name)
                }

                _ => None,
            }
        };
        let Some((home, range)) = field else {
            return false;
        };

        if method == "textDocument/definition" {
            self.respond(id, json!([{ "uri": home, "range": range }]));

            return true;
        }

        let mut moved = message.clone();
        moved["params"]["textDocument"]["uri"] = json!(home);
        moved["params"]["position"] = range["start"].clone();
        self.forward_request(moved, Some(method));

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

                // The child sees the import as generated text and edits
                // this file alone, which leaves the import on the old
                // name. No edit is better than half of one.
                None if file.extension().is_some_and(|e| e == "aly" || e == "alx") => {
                    drop(st);
                    self.to_client(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32803,
                            "message": format!(
                                "Cannot rename `{name}`: no module that `{}` reaches declares it",
                                file.display()
                            ),
                        },
                    }));

                    return true;
                }

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

            Target::Binding { name, start, end } => {
                let mut edits: Vec<Value> = uses_in_range(&doc.source, &name, start, end)
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

            Target::Method {
                trait_name,
                name,
                home,
            } => match st.method_edits(&trait_name, home.as_ref(), &name, &new_name) {
                Some(edit) => edit,

                None => return false,
            },

            // The child reads every value of the struct, typed by the
            // checker, so it answers from the field list the artifact
            // writes; the mend adds the declaration and the keys.
            Target::Field { owner, name } => match shadow_field(doc, &owner, &name) {
                Some(at) => {
                    drop(st);
                    self.forward_request_at(message.clone(), Some("textDocument/rename"), at);

                    return true;
                }

                None => match st.field_edits(uri, &owner, &name, &new_name) {
                    Some(edit) => edit,

                    None => return false,
                },
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

            Target::Binding { name, start, end } => uses_in_range(&doc.source, &name, start, end)
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

            Target::Method {
                trait_name,
                name,
                home,
            } => match st.method_edits(&trait_name, home.as_ref(), &name, &name) {
                Some(edit) => locations_of(&edit),

                None => return false,
            },

            Target::Field { owner, name } => match shadow_field(doc, &owner, &name) {
                Some(at) => {
                    drop(st);
                    self.forward_request_at(message.clone(), Some("textDocument/references"), at);

                    return true;
                }

                None => match st.field_edits(uri, &owner, &name, &name) {
                    Some(edit) => locations_of(&edit),

                    None => return false,
                },
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

        if !keywords::is_word_caret(&doc.source, offset) {
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

        // `Light.Active` and `B.Status.Active`: the path names the enum,
        // and another module may declare one by the same name.
        if let Some((file, owner)) = st.variant_home(uri, offset)
            && let Some(text) = st.module_text(&file)
            && let Some((_, _, (a, b))) = enum_variants(&text)
                .into_iter()
                .find(|(o, v, _)| *o == owner && *v == doc.source[word_start..word_end])
        {
            let range = range_value(position_of(&text, a), position_of(&text, b));
            let result = json!([{ "uri": path_to_uri(&file), "range": range }]);
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

        // `import * as M from "./m"`, and the `M` of `M.Ns.T`: the emit
        // writes the `local` for the module in generated text, so the
        // import line is the place the name comes from.
        if let Some(result) =
            module_binding_definition(&doc.source, uri, &doc.source[word_start..word_end])
        {
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

        if raw_before.trim_end().ends_with('.')
            && let Some(result) = st.field_definition(uri, offset)
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

        // A let-else binding: the emit writes its declaration after
        // the `end` of the else block, on another line, so the child
        // points past that `end`. The pattern is where the name comes
        // from.
        if key == word
            && let Some(((a, b), _)) = let_else_binding(doc, line as usize, word)
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
        // `Phase.Lobby` under `import { Phase }`: the module the import
        // reads declares it. Another file may keep a `Phase` of its own.
        let home = st
            .import_home(uri, key.split('.').next().unwrap_or(&key))
            .map(|file| path_to_uri(&file));
        let found = doc
            .decls
            .iter()
            .find(|d| d.name == key)
            .map(|d| (uri.to_string(), doc, d))
            .or_else(|| {
                (!bound_here).then(|| {
                    let holds = |d: &Doc| d.decls.iter().any(|x| x.name == key);
                    let (u, d) = home
                        .as_ref()
                        .and_then(|h| st.docs.get_key_value(h))
                        .filter(|(_, d)| holds(d))
                        .or_else(|| st.docs.iter().find(|(_, d)| holds(d)))?;

                    d.decls
                        .iter()
                        .find(|x| x.name == key)
                        .map(|x| (u.clone(), d, x))
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
        if !keywords::is_word_caret(source, offset) {
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
            .map(|(_, spec)| spec);

        let Some(spec) = spec else {
            // `A.T` under `import { Ns as A }`, and `M.Ns.T` under a
            // module binding: the holder names the group, and the
            // module that declares the group declares the member.
            return self.namespace_member_at(uri, source, hs, holder, &name);
        };
        let file = imports::module_file(&imports::module_path(&self.resolve_spec(uri, &spec)?))?;

        Some((file, name))
    }

    /// The module and the member name a `Ns.T` reads: the group under
    /// the word an import list binds it to, or under the `M.Ns` of a
    /// module binding. `None` when the holder names no namespace that
    /// declares the member.
    fn namespace_member_at(
        &self,
        uri: &str,
        source: &str,
        holder_at: usize,
        holder: &str,
        name: &str,
    ) -> Option<(PathBuf, String)> {
        let by_entry = self
            .import_entry_at(source, holder_at)
            .filter(|it| it.bound == holder)
            .and_then(|it| Some((self.entry_module(uri, &it)?, it.name)));
        let (file, group) = match by_entry {
            Some(found) => found,

            // `M.Ns.T`: the word in front of the group holds the module.
            None => {
                let by_module = || {
                    let before = source[..holder_at].trim_end().strip_suffix('.')?;
                    let at = before.len().checked_sub(1)?;

                    if !keywords::is_word_at(source, at) {
                        return None;
                    }

                    let (ms, me) = keywords::word_range(source, at);
                    let spec = module_bindings(source)
                        .into_iter()
                        .find(|(bound, _)| *bound == source[ms..me])
                        .map(|(_, spec)| spec)?;

                    imports::module_file(&imports::module_path(&self.resolve_spec(uri, &spec)?))
                };
                // `Outer.Inner.T`: the word in front is a group of this
                // file, so the member is this file's own. The holds
                // test below says whether the group has it.
                let file = by_module().or_else(|| uri_to_path(uri))?;

                (file, holder.to_string())
            }
        };
        let text = self.module_text(&file)?;
        let holds = alloy::declarations::namespace_ranges(&text)
            .into_iter()
            .any(|n| {
                n.path.rsplit('.').next() == Some(group.as_str())
                    && n.members.iter().any(|(m, _)| m == name)
            });

        holds.then(|| (file, name.to_string()))
    }

    /// The words a file writes in front of a member of one namespace.
    ///
    /// The module's own file writes the group's last word, and so does
    /// a reader under `import * as M`, which spells `M.Ns.T`. An import
    /// list binds the group to a name of its own, so
    /// `import { Ns as A }` writes `A.T`. A group inside another keeps
    /// its own word whatever the reader bound the outer one to.
    pub(crate) fn namespace_heads(
        &self,
        uri: &str,
        source: &str,
        module: &Path,
        path: &str,
    ) -> Vec<String> {
        let root = path.split('.').next().unwrap_or(path);
        let last = path.rsplit('.').next().unwrap_or(path).to_string();
        let reaches = |spec: &str| {
            self.resolve_spec(uri, spec)
                .map(|p| imports::module_path(&p))
                .is_some_and(|p| p == module)
        };
        let bound: Vec<String> = import_entries(source)
            .into_iter()
            .filter(|it| it.name == root && reaches(&it.spec))
            .map(|it| it.bound)
            .collect();
        let holds_module = module_bindings(source)
            .iter()
            .any(|(_, spec)| reaches(spec));
        let home = uri_to_path(uri).is_some_and(|p| imports::module_path(&p) == module);
        let mut heads = Vec::new();

        if home || holds_module || (path.contains('.') && !bound.is_empty()) {
            heads.push(last);
        }

        // An alias stands for the group the list names, so it is the
        // word in front of the member. A group inside another is
        // reached through its own name and never through the alias.
        if !path.contains('.') {
            for name in bound {
                if !heads.contains(&name) {
                    heads.push(name);
                }
            }
        }

        heads
    }

    /// Every place one file writes a namespace group itself: the word
    /// the file binds the group to, alone or after the word that holds
    /// what it came from. `M.Ns` under a module binding, `A` under
    /// `import { Ns as A }`, and `Ns` in the module's own file.
    ///
    /// The head list says which words stand for the group. A head after
    /// a dot is no name of its own, so the walk over plain names misses
    /// it and the member walk reads it.
    pub(crate) fn group_uses(
        &self,
        uri: &str,
        source: &str,
        module: &Path,
        path: &str,
    ) -> Vec<(usize, usize)> {
        let reaches = |spec: &str| {
            self.resolve_spec(uri, spec)
                .map(|p| imports::module_path(&p))
                .is_some_and(|p| p == module)
        };
        let mut holders: Vec<String> = module_bindings(source)
            .into_iter()
            .filter(|(_, spec)| reaches(spec))
            .map(|(bound, _)| bound)
            .collect();

        // A group inside another reads under its parent's word.
        if let Some(parent) = path.rsplit('.').nth(1) {
            holders.push(parent.to_string());
        }

        let mut out = Vec::new();

        for head in self.namespace_heads(uri, source, module, path) {
            out.extend(name_uses(source, &head));
            out.extend(member_uses(source, &holders, &head));
        }

        // The name an import list writes, whatever it binds it to.
        let root = path.split('.').next().unwrap_or(path);

        if !path.contains('.') {
            out.extend(
                import_entries(source)
                    .into_iter()
                    .filter(|it| it.name == root && reaches(&it.spec))
                    .map(|it| it.name_at),
            );
        }

        out.sort();
        out.dedup();
        out
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

        if !keywords::is_word_caret(source, offset) {
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
        // A bare name in a pattern binds unless it names a unit variant,
        // so `case Nil` is a use of `Nil` alone.
        let unit = self
            .docs
            .values()
            .flat_map(|d| d.shapes.iter().chain(&d.import_shapes))
            .any(|s| match s {
                alloy::declarations::Shape::Enum {
                    name: n, variants, ..
                } => n == owner && variants.iter().any(|(v, p)| v == name && p.is_empty()),

                _ => false,
            });

        for (a, b) in variant_uses(&text, std::slice::from_ref(&owner.to_string()), name)
            .into_iter()
            .chain(pattern_uses(&text, name, unit))
        {
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

            // `import * as M`: the enum reads `M.Shape`.
            holders.extend(
                module_bindings(&d.source)
                    .into_iter()
                    .filter(|(_, spec)| reaches(spec))
                    .map(|(bound, _)| format!("{bound}.{owner}")),
            );

            // A `case Some(v)` names the variant bare; the file reaches
            // the enum when it binds the enum's name.
            let patterns = match holders.is_empty() {
                true => Vec::new(),

                false => pattern_uses(&d.source, name, unit),
            };
            let mut edits: Vec<Value> = variant_uses(&d.source, &holders, name)
                .into_iter()
                .chain(patterns)
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

        // The caret's own binding decides. A parameter, a `local`, and a
        // `for` variable each have a scope, and the child knows where
        // that scope ends; the walks below read the file by spelling, so
        // they would edit every same-named name in the file. A name this
        // file exports at the caret is the module's, and those walks
        // answer for it.
        if keywords::is_word_caret(source, offset) {
            let (s, e) = keywords::word_range(source, offset);
            let word = &source[s..e];
            let declares = export_span(source, word) == Some((s, e))
                && imports::exports_of(source, doc.is_alx)
                    .iter()
                    .any(|x| x.name == *word);

            // A `case` binding is the proxy's own. The match lowers to
            // one expression, so the child's edits land on generated
            // text: the `case` keyword and the `end` of the declaration
            // the arm reads. The arm holds every use of the name.
            let line = position_of(source, offset).0 as usize;

            if !declares && let Some((start, end)) = case_arm_of_binding(doc, line, word) {
                return Some(Target::Binding {
                    name: word.to_string(),
                    start,
                    end,
                });
            }

            // A let-else binding is the proxy's own too: the emit
            // writes its declaration after the `end` of the else block,
            // on another line, so the child ties the name to nothing.
            if !declares && let Some((_, (start, end))) = let_else_binding(doc, line, word) {
                return Some(Target::Binding {
                    name: word.to_string(),
                    start,
                    end,
                });
            }

            // A component's tags are generated calls the child ties to
            // nothing, so its declaration and its uses answer below.
            if !declares
                && !(doc.is_alx && markup::names_a_tag(source, word))
                && matches!(
                    context::binding_in_scope(source, offset, word).map(|l| l.kind),
                    Some(context::LocalKind::Parameter | context::LocalKind::Variable)
                )
            {
                return None;
            }
        }

        if let Some(entry) = self.import_entry_at(source, offset) {
            let on_name = (entry.name_at.0..=entry.name_at.1).contains(&offset);

            if entry.alias_at.is_some() && !on_name {
                return Some(Target::Local(entry.bound));
            }

            return Some(Target::Export(self.entry_module(uri, &entry)?, entry.name));
        }

        // `export { X as Y } from "./m"`: `X` is the name of `./m`, and
        // `Y` is the name of this barrel. Without an alias the barrel's
        // name is the one `./m` declares, and the rename walks from there.
        let inside = |(s, e): (usize, usize)| (s..=e).contains(&offset);

        if let Some(entry) = reexport_entries(source)
            .into_iter()
            .find(|e| inside(e.name_at) || e.alias_at.is_some_and(inside))
        {
            return match entry.alias_at.is_some() && inside(entry.name_at) {
                true => Some(Target::Export(self.entry_module(uri, &entry)?, entry.name)),

                false => Some(Target::Export(uri_to_path(uri)?, entry.bound)),
            };
        }

        if let Some(file) = uri_to_path(uri)
            && keywords::is_word_caret(source, offset)
        {
            let (s, e) = keywords::word_range(source, offset);
            let word = source[s..e].to_string();

            // A variant after the dot of `Shape.Circle`, `Light.Active`,
            // or `B.Status.Active`: the path names the enum, and the
            // module walks below would read the path as a module member.
            if let Some((file, owner)) = self.variant_home(uri, offset) {
                return Some(Target::Variant {
                    file,
                    owner,
                    name: word,
                });
            }

            // A trait's method: the trait declares it once and every
            // `impl` writes it again, so the three places are one name.
            if let Some(target) = self.method_target(uri, doc, offset) {
                return Some(target);
            }

            let declares = export_span(source, &word) == Some((s, e));
            let exported = imports::exports_of(source, doc.is_alx)
                .iter()
                .any(|x| x.name == word);

            if declares && exported {
                return Some(Target::Export(file, word));
            }

            // `<Header />`: the tag names a component function, and
            // the lowering writes the call as generated text the child
            // ties to nothing. The function's own walk answers, here or
            // in the module an import reads it from.
            if doc.is_alx
                && (matches!(
                    markup::hover_spot(source, offset),
                    Some(markup::Spot::Tag { .. })
                ) || markup::names_a_tag(source, &word))
            {
                if let Some(entry) = import_entries(source)
                    .into_iter()
                    .find(|it| it.bound == word && it.alias_at.is_none())
                    && let Some(module) = self.entry_module(uri, &entry)
                {
                    return Some(Target::Export(module, entry.name));
                }

                if export_span(source, &word).is_some() {
                    return Some(match exported {
                        true => Target::Export(file, word),

                        false => Target::Local(word),
                    });
                }
            }

            // A member of a namespace this file declares. Every reader
            // writes it under the group, so the module's walk answers
            // for it whether or not the group is exported.
            if declares
                && alloy::declarations::namespace_ranges(source)
                    .iter()
                    .any(|n| {
                        (n.start..=n.end).contains(&s) && n.members.iter().any(|(m, _)| *m == word)
                    })
            {
                return Some(Target::Export(file, word));
            }

            // `M.version` under `import * as M`: the module holds the
            // name, so the answer is the export's.
            if let Some((file, name)) = self.module_member_at(uri, source, offset) {
                return Some(Target::Export(file, name));
            }

            // A variant of an enum this file reaches: the one the
            // caret sits on in the enum body, or one a pattern writes
            // bare.
            if let Some(owner) = self.variant_owner(uri, &word) {
                let holds = |d: &Doc| {
                    d.shapes
                        .iter()
                        .any(|s| s.name() == owner && names_a_variant(s, &word))
                };
                // The enum this file declares, else the one its import
                // reads: another module may keep an enum of the same
                // name to itself.
                let declares = holds(doc)
                    .then(|| file.clone())
                    .or_else(|| self.import_home(uri, &owner))
                    .or_else(|| {
                        self.docs
                            .iter()
                            .find_map(|(u, d)| holds(d).then(|| uri_to_path(u)).flatten())
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
    fn method_target(&self, uri: &str, doc: &Doc, offset: usize) -> Option<Target> {
        let (s, e) = keywords::word_range(&doc.source, offset);
        let name = doc.source[s..e].to_string();
        let here = trait_method_sites(&doc.source)
            .into_iter()
            .find(|site| site.at == (s, e));

        // A plain `impl S` declares the method on `S` itself, so the
        // struct stands where a trait would.
        if let Some(site) = here {
            let home = self.site_home(uri, doc, &site);

            return site
                .trait_name
                .or(site.target)
                .map(|trait_name| Target::Method {
                    trait_name,
                    name,
                    home,
                });
        }

        // `b:hello()`: the struct the receiver holds says which trait,
        // through its own `impl` of it. `s.speak()` reads the same way
        // where a call follows; `p.x` alone is a field.
        let head = doc.source[..s].trim_end();
        let colon = head.ends_with(':') && !head.ends_with("::");
        let dot = head.ends_with('.')
            && !head.ends_with("..")
            && doc.source[e..]
                .trim_start()
                .starts_with(['(', '{', '"', '\'', '`']);

        if !colon && !dot {
            return None;
        }

        // `Counter.bump(c)`: the receiver is the struct itself.
        let owner = receiver_type(self, doc, head.len() - 1).or_else(|| {
            dot.then(|| self.impl_target_at(doc, head.len() - 1))
                .flatten()
        });
        let mut traits: Vec<(String, Option<TraitHome>)> = Vec::new();

        for (u, d) in &self.docs {
            for site in trait_method_sites(&d.source) {
                let Some(trait_name) = site.trait_name.clone().or(site.target.clone()) else {
                    continue;
                };

                if site.name != name {
                    continue;
                }

                let home = self.site_home(u, d, &site);
                let mine = match (&owner, &site.target) {
                    // The receiver is the trait itself, or a type
                    // parameter the trait bounds: `s: Speaker`,
                    // `<T: Speaker>`.
                    (Some(o), _) if *o == trait_name || bound_by(&doc.source, o, &trait_name) => {
                        true
                    }

                    (Some(o), Some(t)) => o == t,

                    // The trait's own body: an `impl Trait for S` with no
                    // method of its own still hands `S` the default.
                    (Some(o), None) => self.implements(o, &trait_name, home.as_ref()),

                    // A receiver with no type of its own: one trait that
                    // declares the name is still the one the call means.
                    (None, _) => true,
                };
                let pair = (trait_name, home);

                if mine && !traits.contains(&pair) {
                    traits.push(pair);
                }
            }
        }

        match traits.as_slice() {
            [(trait_name, home)] => Some(Target::Method {
                trait_name: trait_name.clone(),
                name,
                home: home.clone(),
            }),

            // Two traits of one method name say nothing about which one
            // the call reaches; the child answers instead.
            _ => None,
        }
    }

    /// The impl target the word before `at` names: the `Counter` of
    /// `Counter.bump(c)`. `None` for a word no `impl` block targets.
    fn impl_target_at(&self, doc: &Doc, at: usize) -> Option<String> {
        let head = doc.source[..at].trim_end();

        if head.is_empty() || !keywords::is_word_at(&doc.source, head.len() - 1) {
            return None;
        }

        let (s, e) = keywords::word_range(&doc.source, head.len() - 1);
        let word = &doc.source[s..e];

        self.docs
            .values()
            .flat_map(|d| &d.impl_blocks)
            .any(|b| b.target == word)
            .then(|| word.to_string())
    }

    /// Whether any file writes `impl Trait for S`, with or without a
    /// method in the block.
    fn implements(&self, owner: &str, trait_name: &str, home: Option<&TraitHome>) -> bool {
        self.impl_targets(trait_name, home)
            .iter()
            .any(|t| t == owner)
    }

    /// Every type an `impl Trait for S` of the trait targets, with or
    /// without a method in the block.
    fn impl_targets(&self, trait_name: &str, home: Option<&TraitHome>) -> Vec<String> {
        self.docs
            .iter()
            .flat_map(|(u, d)| d.impl_blocks.iter().map(move |b| (u, d, b)))
            .filter(|(u, d, b)| {
                b.trait_name
                    .as_deref()
                    .is_some_and(|t| self.is_trait(u, d, &path_head(t), b.start, trait_name, home))
            })
            .map(|(_, _, b)| b.target.clone())
            .collect()
    }

    /// The trait a method site belongs to, where it lives. `None` for a
    /// plain `impl S`, and for a trait no scope of the file reaches.
    fn site_home(&self, uri: &str, doc: &Doc, site: &MethodSite) -> Option<TraitHome> {
        self.trait_home(uri, doc, site.trait_path.as_deref()?, site.header)
    }

    /// Whether the trait `written` names at `at` is the one `home`
    /// points at. Where either side has no home, the names decide.
    fn is_trait(
        &self,
        uri: &str,
        doc: &Doc,
        written: &str,
        at: usize,
        trait_name: &str,
        home: Option<&TraitHome>,
    ) -> bool {
        match (home, self.trait_home(uri, doc, written, at)) {
            (Some(h), Some(here)) => *h == here,

            _ => last_name(written) == trait_name,
        }
    }

    /*
    Where the trait a file writes as `written` at byte `at` lives.

    The file's own trait answers first, read from the innermost
    namespace around `at` outward: `trait Mover` inside `namespace
    Motion` is `Motion.Mover`. Else an import binds the first name of
    the path, and a barrel passes it on to the module that declares it.
    Two files that each declare a `Mover` hold two traits.
    */
    fn trait_home(&self, uri: &str, doc: &Doc, written: &str, at: usize) -> Option<TraitHome> {
        let mut scopes: Vec<&str> = doc
            .namespace_ranges
            .iter()
            .filter(|n| n.start <= at && at < n.end)
            .map(|n| n.path.as_str())
            .collect();
        scopes.sort_by_key(|p| std::cmp::Reverse(p.len()));
        let own = scopes
            .iter()
            .map(|s| format!("{s}.{written}"))
            .chain([written.to_string()])
            .find(|path| doc.decls.iter().any(|d| d.name == *path));

        if let Some(path) = own {
            return Some((imports::module_path(&uri_to_path(uri)?), path));
        }

        let (head, rest) = match written.split_once('.') {
            Some((h, r)) => (h, Some(r)),

            None => (written, None),
        };
        let (spec, name, rest) = match import_entries(&doc.source)
            .into_iter()
            .find(|e| e.bound == head)
        {
            Some(e) => (e.spec, e.name, rest),

            // `import * as Lib`: `Lib.Mover` names the module's export.
            None => {
                let (_, spec) = module_bindings(&doc.source)
                    .into_iter()
                    .find(|(alias, _)| alias == head)?;
                let rest = rest?;
                let (name, deeper) = match rest.split_once('.') {
                    Some((n, d)) => (n, Some(d)),

                    None => (rest, None),
                };

                (spec, name.to_string(), deeper)
            }
        };
        let (module, own) = self.declaring_module(uri, &spec, &name)?;

        Some(match rest {
            Some(rest) => (module, format!("{own}.{rest}")),

            None => (module, own),
        })
    }

    /// The module that declares what `spec` sends out as `name`, read
    /// from the file at `uri`, and the name it declares it under. A
    /// barrel's `export { T } from` passes the walk on.
    fn declaring_module(&self, uri: &str, spec: &str, name: &str) -> Option<TraitHome> {
        let (mut uri, mut spec, mut name) = (uri.to_string(), spec.to_string(), name.to_string());

        for _ in 0..4 {
            let Some((text, file)) = self.module_source(&uri, &spec) else {
                break;
            };
            let Some(entry) = reexport_entries(&text)
                .into_iter()
                .find(|e| e.bound == name)
            else {
                break;
            };

            (uri, spec, name) = (path_to_uri(&file), entry.spec, entry.name);
        }

        Some((imports::module_path(&self.resolve_spec(&uri, &spec)?), name))
    }

    /*
    The whole rename of one method, as a workspace edit.

    The name is the trait's, or the struct's for a method of a plain
    `impl S`; the child ties none of the sites of either together.

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
        home: Option<&TraitHome>,
        name: &str,
        new_name: &str,
    ) -> Option<Value> {
        // The trait of each site is settled once, here: two files may
        // each declare a trait of this name.
        let sites: Vec<(String, MethodSite, bool)> = self
            .docs
            .iter()
            .flat_map(|(u, d)| {
                trait_method_sites(&d.source)
                    .into_iter()
                    .filter(|s| s.name == name)
                    .map(move |site| {
                        let mine = match &site.trait_path {
                            Some(t) => self.is_trait(u, d, t, site.header, trait_name, home),

                            None => site.target.as_deref() == Some(trait_name),
                        };

                        (u.clone(), site, mine)
                    })
            })
            .collect();
        // Every struct the trait reaches, an empty `impl Trait for S`
        // among them; the struct itself for a plain `impl S`.
        let targets: Vec<String> = self
            .impl_targets(trait_name, home)
            .into_iter()
            .chain([trait_name.to_string()])
            .collect();
        // The name belongs to this trait alone: nothing else declares it.
        let only_one = sites.iter().all(|(_, _, mine)| *mine);
        // The modules that declare a target. An importer names one by
        // its module path, the way the export rename finds importers.
        let declared: Vec<PathBuf> = self
            .docs
            .iter()
            .filter(|(_, d)| d.decls.iter().any(|dc| targets.contains(&dc.name)))
            .filter_map(|(u, _)| uri_to_path(u).map(|p| imports::module_path(&p)))
            .collect();
        let mut changes: Map<String, Value> = Map::new();

        for (u, d) in &self.docs {
            let mut edits: Vec<Value> = sites
                .iter()
                .filter(|(owner, _, mine)| owner == u && *mine)
                .map(|(_, s, _)| text_edit(&d.source, s.at.0, s.at.1, new_name))
                .collect();
            // `import { Gadget as Gizmo }`: this file writes the target
            // under its alias, so a receiver of that type holds it too.
            let aliases: Vec<String> = import_entries(&d.source)
                .into_iter()
                .filter(|it| {
                    targets.contains(&it.name)
                        && self
                            .resolve_spec(u, &it.spec)
                            .is_some_and(|p| declared.contains(&imports::module_path(&p)))
                })
                .map(|it| it.bound)
                .collect();

            for (start, end, receiver, dotted) in method_calls(&d.source, name) {
                let holds = match receiver_type(self, d, receiver) {
                    Some(ty) => {
                        targets.contains(&ty)
                            || aliases.contains(&ty)
                            || bound_by(&d.source, &ty, trait_name)
                    }

                    // `Ns.f()` reads a namespace the file never binds;
                    // only a `:` call with no type still means the
                    // trait. `Counter.bump(c)` names the struct itself.
                    None => {
                        (only_one && !dotted)
                            || (dotted
                                && self.impl_target_at(d, receiver).as_deref() == Some(trait_name))
                    }
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
        if let Some(owner) = used_field_owner(self, doc, start) {
            return Some(owner);
        }

        if let Some((owner, false)) = context::struct_literal_target(&doc.source, start) {
            // The walks below read the word the file writes in front of
            // the body, so a namespace member answers by its own name.
            return Some(owner.rsplit('.').next().unwrap_or(&owner).to_string());
        }

        // `x: number` in a struct body, as the field hover reads it: the
        // nearest declaration above that still carries fields.
        declared_field_hover(doc, start, end)?;

        // The name the source writes at the header, not the one the
        // declaration is keyed by: a namespace member answers to its
        // path and to the name the emit gives it, and the walks below
        // read the file for the word `struct` stands in front of.
        doc.decls
            .iter()
            .filter(|d| d.offset < start && declared_field_owner(d).is_some())
            .max_by_key(|d| d.offset)
            .map(|d| {
                let (s, e) = keywords::word_range(&doc.source, d.offset);

                doc.source[s..e].to_string()
            })
    }

    /*
    The struct a field at the caret belongs to, and the file that
    declares it.

    The file's own index answers for a receiver it can type: an
    annotation, a `new`, a declared return. A receiver the checker
    typed alone, `stock:get("x")` or a loop variable, still has an
    answer: the child's sites for the field. One of them lands on the
    struct's field list, which the emit writes as generated text on the
    struct's `end` line, and another may sit on a receiver the index
    can type.
    */
    fn field_target(
        &self,
        uri: &str,
        start: usize,
        end: usize,
        child: &Value,
    ) -> Option<(String, Option<PathBuf>)> {
        let doc = self.docs.get(uri)?;
        let name = &doc.source[start..end];
        // An owner counts when a struct of the name declares the field:
        // the index reads `stock:get("x")` as a value of `stock`.
        let declares = |owner: &str, home: &Option<PathBuf>| match home {
            Some(file) => self
                .module_text(file)
                .is_some_and(|text| field_declaration(&text, owner, name).is_some()),

            None => self
                .docs
                .values()
                .any(|d| field_declaration(&d.source, owner, name).is_some()),
        };

        if let Some(owner) = self.field_owner(doc, start, end) {
            let home = self.struct_home(uri, &owner);

            if declares(&owner, &home) {
                return Some((owner, home));
            }
        }

        if !doc.source[..start].trim_end().ends_with('.') {
            return None;
        }

        site_list(child).into_iter().find_map(|(u, (l, c))| {
            let path = uri_to_path(&u)?;
            let text = self.module_text(&path)?;

            if let Some((owner, _)) = struct_field_at_line(&text, l, name) {
                return Some((owner, Some(path)));
            }

            let d = self.docs.get(&u)?;
            let at = offset_of(&d.source, l, c)?;
            let (s, e) = keywords::word_range(&d.source, at);
            let owner = self.field_owner(d, s, e)?;
            let home = self.struct_home(&u, &owner);

            declares(&owner, &home).then_some((owner, home))
        })
    }

    /// A field read whose receiver the checker typed: the child points
    /// at the struct's `end` line, where the emit writes the field list.
    /// The field's own line in the struct body is what the reader means.
    pub(crate) fn mend_field_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return;
        };

        if !doc.source[..start].trim_end().ends_with('.') {
            return;
        }

        let name = &doc.source[start..end];
        let Some(list) = result.as_array_mut() else {
            return;
        };

        for loc in list.iter_mut() {
            let uri_key = if loc.get("targetUri").is_some() {
                "targetUri"
            } else {
                "uri"
            };
            let range_key = if loc.get("targetRange").is_some() {
                "targetRange"
            } else {
                "range"
            };
            let Some(path) = loc
                .get(uri_key)
                .and_then(Value::as_str)
                .and_then(uri_to_path)
            else {
                continue;
            };
            let Some(((l, _), _)) = loc.get(range_key).and_then(range_of) else {
                continue;
            };
            let Some(text) = self.module_text(&path) else {
                continue;
            };

            if let Some((_, (a, b))) = struct_field_at_line(&text, l, name) {
                let range = range_value(position_of(&text, a), position_of(&text, b));
                loc[range_key] = range.clone();

                if loc.get("targetSelectionRange").is_some() {
                    loc["targetSelectionRange"] = range;
                }
            }
        }
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
        child: &Value,
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
        let Some((owner, home)) = self.field_target(uri, start, end, child) else {
            return;
        };
        let Some(changes) = result
            .pointer_mut("/changes")
            .and_then(Value::as_object_mut)
        else {
            return;
        };

        for (u, d) in &self.docs {
            let mut mine: Vec<Value> = field_sites(self, u, home.as_deref(), &owner, &name)
                .into_iter()
                .map(|(s, e)| text_edit(&d.source, s, e, &new_name))
                .collect();

            // A parameter pattern of the type reads the field too.
            if self.struct_home(u, &owner) == home {
                mine.extend(
                    super::patterns::pattern_field_edits(&d.source, &owner, &name, &new_name)
                        .into_iter()
                        .map(|(s, e, text)| text_edit(&d.source, s, e, &text)),
                );
            }

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

    /// Drops every site the child answered that does not spell the name
    /// under the caret. A function called above its declaration gets a
    /// forward `local` on line 1 as generated text, and the map sends
    /// that site to its anchor: the first byte of the file, which is a
    /// word of its own. A definition, a references list, and a rename
    /// each carry it, and each loses it here.
    pub(crate) fn drop_stray_sites(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return;
        };
        let name = &doc.source[start..end];
        let spells = |u: &str, site: &mut Value| {
            let Some(d) = self.docs.get(u) else {
                return true;
            };

            if edits_the_word(&d.source, site, name) {
                return true;
            }

            // `match k with` on literal cases lowers to `if k == 1` on
            // the line of each arm, so the child's site lands there.
            let Some((s, e)) = site
                .get("range")
                .and_then(range_of)
                .and_then(|((l, c), _)| offset_of(&d.source, l, c))
                .and_then(|at| scrutinee_of_arm(&d.source, at, name))
            else {
                return false;
            };
            site["range"] = range_value(position_of(&d.source, s), position_of(&d.source, e));

            true
        };

        // `cfg?.a` and `v ??= 3` lower to two reads of the name that
        // both map back to the one the author wrote. The editor refuses
        // a rename whose edits overlap, so each site stays once.
        let mut seen: Vec<Value> = Vec::new();
        let mut first = |site: &Value| {
            let fresh = !seen.contains(site);
            seen.push(site.clone());

            fresh
        };

        if let Some(list) = result.as_array_mut() {
            list.retain_mut(|loc| {
                let u = loc
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();

                spells(&u, loc) && first(loc)
            });
        }

        if let Some(changes) = result
            .pointer_mut("/changes")
            .and_then(Value::as_object_mut)
        {
            for (u, edits) in changes.iter_mut() {
                if let Some(list) = edits.as_array_mut() {
                    list.retain_mut(|e| spells(u, e) && first(&json!([u, e])));
                }
            }
        }
    }

    /// The same mend for a references answer: the child's list gets
    /// every site the proxy knows, and loses a location that points at
    /// no word of that name, which is where the generated struct
    /// header sent it.
    pub(crate) fn mend_field_references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        child: &Value,
        result: &mut Value,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return;
        };
        let name = doc.source[start..end].to_string();
        let Some((owner, home)) = self.field_target(uri, start, end, child) else {
            return;
        };
        let Some(list) = result.as_array_mut() else {
            return;
        };

        list.retain(|loc| {
            let u = loc.get("uri").and_then(Value::as_str).unwrap_or_default();

            self.docs
                .get(u)
                .is_none_or(|d| edits_the_word(&d.source, loc, &name))
        });

        for (u, d) in &self.docs {
            for (s, e) in field_sites(self, u, home.as_deref(), &owner, &name) {
                let loc = json!({
                    "uri": u,
                    "range": range_value(position_of(&d.source, s), position_of(&d.source, e)),
                });

                if !list.contains(&loc) {
                    list.push(loc);
                }
            }
        }
    }

    /// The binding a local `export { inner as bump }` list names. The
    /// emit writes the list as generated text, so the child's rename
    /// and references of `inner` leave the list out. The child's answer
    /// holds the module's own declaration when the caret's name is the
    /// one the list reads.
    pub(crate) fn mend_export_list(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return;
        };
        let name = &doc.source[start..end];
        let entries: Vec<ImportEntry> = export_list_entries(&doc.source)
            .into_iter()
            .filter(|e| e.name == name)
            .collect();

        if entries.is_empty() {
            return;
        }

        let Some((s, e)) = export_span(&doc.source, name) else {
            return;
        };
        let declared = range_value(position_of(&doc.source, s), position_of(&doc.source, e));
        let site = |(s, e): (usize, usize)| {
            range_value(position_of(&doc.source, s), position_of(&doc.source, e))
        };

        if let Some(list) = result.as_array_mut() {
            let at_declaration = list
                .iter()
                .any(|l| l["uri"] == uri && l["range"] == declared);

            for entry in entries.iter().filter(|_| at_declaration) {
                let loc = json!({ "uri": uri, "range": site(entry.name_at) });

                if !list.contains(&loc) {
                    list.push(loc);
                }
            }
        }

        if let Some(edits) = result
            .pointer_mut("/changes")
            .and_then(|c| c.get_mut(uri))
            .and_then(Value::as_array_mut)
            && let Some(new_text) = edits
                .iter()
                .find(|e| e["range"] == declared)
                .map(|e| e["newText"].clone())
        {
            for entry in &entries {
                let edit = json!({ "range": site(entry.name_at), "newText": new_text });

                if !edits.contains(&edit) {
                    edits.push(edit);
                }
            }
        }
    }

    /// The file that declares the component a tag of `uri` names, with
    /// its text: this file, or the module an import brings it from.
    fn component_home(&self, uri: &str, tag: &str) -> Option<(String, String)> {
        let doc = self.docs.get(uri)?;
        let last = tag.rsplit('.').next()?;

        if doc.source.contains(&format!("function {last}(")) {
            return Some((uri.to_string(), doc.source.clone()));
        }

        let load = |spec: &str| self.module_source(uri, spec);
        let spec = markup::component_module(&doc.source, tag, &load)?;
        let file = imports::module_file(&imports::module_path(&self.resolve_spec(uri, &spec)?))?;

        Some((path_to_uri(&file), self.module_text(&file)?))
    }

    /// Where the props type of a component declares `prop`: the record
    /// in the parameter list, or the `type`, `struct`, or `interface` it
    /// names in the same file. A URI and a source range.
    pub(crate) fn prop_declaration(
        &self,
        uri: &str,
        tag: &str,
        prop: &str,
    ) -> Option<(String, Value)> {
        let (home, text) = self.component_home(uri, tag)?;
        let head = format!("function {}(", tag.rsplit('.').next()?);
        let open = text.find(&head)? + head.len();
        let close = open + text[open..].find(')')?;
        let declared = text[open..close].split_once(':')?.1.trim();
        let from = match declared.starts_with('{') {
            true => open,

            false => {
                let name: String = declared
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();

                export_span(&text, &name)?.1
            }
        };
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let at = from
            + text[from..]
                .match_indices(prop)
                .map(|(i, _)| i)
                .find(|&i| {
                    let (s, e) = (from + i, from + i + prop.len());
                    let after = text[e..].trim_start();

                    !text[..s].ends_with(word)
                        && !text[e..].starts_with(word)
                        && after.starts_with(':')
                        && !after.starts_with("::")
                })?;

        Some((
            home,
            range_value(position_of(&text, at), position_of(&text, at + prop.len())),
        ))
    }

    /// The attributes that set a prop on a component's tags. The markup
    /// lowers each to a key of a generated table, so the child's rename
    /// and references of the prop's field leave the tags out. A tag
    /// joins when its component's field is one of the child's sites.
    pub(crate) fn mend_prop_attributes(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        result: &mut Value,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return;
        };
        let name = doc.source[start..end].to_string();
        let mut sites: Vec<(String, Value)> = Vec::new();
        let mut new_text = None;

        if let Some(list) = result.as_array() {
            for loc in list {
                sites.push((
                    loc["uri"].as_str().unwrap_or_default().to_string(),
                    loc["range"].clone(),
                ));
            }
        }

        if let Some(changes) = result.get("changes").and_then(Value::as_object) {
            for (u, edits) in changes {
                for e in edits.as_array().into_iter().flatten() {
                    sites.push((u.clone(), e["range"].clone()));
                    new_text = Some(e["newText"].clone());
                }
            }
        }

        // Only a field's rename reaches a tag, and a field key reads
        // `name:`. Any other answer skips the walk over the markup.
        let names_a_field = sites.iter().any(|(u, range)| {
            self.docs.get(u).is_some_and(|d| {
                range_of(range)
                    .and_then(|(_, (l, c))| offset_of(&d.source, l, c))
                    .is_some_and(|at| d.source[at..].trim_start().starts_with(':'))
            })
        });

        if !names_a_field {
            return;
        }

        let mut extra: Vec<(String, Value)> = Vec::new();

        for (u, d) in self.docs.iter().filter(|(_, d)| d.is_alx) {
            for (tag, (s, e)) in markup::attribute_sites(&d.source, &name) {
                if self
                    .prop_declaration(u, &tag, &name)
                    .is_some_and(|field| sites.contains(&field))
                {
                    let range = range_value(position_of(&d.source, s), position_of(&d.source, e));
                    extra.push((u.clone(), range));
                }
            }
        }

        for (u, range) in extra {
            if sites.contains(&(u.clone(), range.clone())) {
                continue;
            }

            if let Some(list) = result.as_array_mut() {
                list.push(json!({ "uri": u, "range": range }));
            } else if let (Some(changes), Some(text)) = (
                result.get_mut("changes").and_then(Value::as_object_mut),
                &new_text,
            ) {
                let edit = json!({ "range": range, "newText": text });

                match changes.get_mut(&u).and_then(Value::as_array_mut) {
                    Some(list) => list.push(edit),

                    None => {
                        changes.insert(u, json!([edit]));
                    }
                }
            }
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

        let at_word = keywords::is_word_caret(&doc.source, offset)
            .then(|| keywords::word_range(&doc.source, offset));

        // A rename onto the name the caret already carries writes
        // nothing, and the site it finds is that name's own.
        if at_word.is_some_and(|(s, e)| doc.source[s..e] == *new_name) {
            return None;
        }

        match target {
            // A name this file alone binds, and a struct with no
            // export: the file is the whole scope.
            Some(Target::Local(_) | Target::Binding { .. }) => {
                let (kind, at) = bound_at(&doc.source, new_name)?;

                says(&kind, &doc.source, at)
            }

            Some(Target::Export(file, _)) => {
                let text = self.module_text(file)?;
                let (kind, at) = bound_at(&text, new_name)?;

                says(&kind, &text, at)
            }

            Some(Target::Field { owner, .. }) => {
                let (kind, src, at) = self.struct_member_site(owner, new_name)?;

                says(kind, src, at)
            }

            Some(Target::Variant { file, owner, .. }) => {
                let text = self.module_text(file)?;
                let (at, _) = enum_variants(&text)
                    .into_iter()
                    .find(|(o, v, _)| o == owner && v == new_name)
                    .map(|(_, _, at)| at)?;

                says("variant", &text, at)
            }

            Some(Target::Method { trait_name, .. }) => self
                .docs
                .values()
                .find_map(|d| {
                    let site = trait_method_sites(&d.source).into_iter().find(|s| {
                        s.trait_name.as_deref() == Some(trait_name) && s.name == *new_name
                    })?;

                    says("method", &d.source, site.at.0)
                })
                .or_else(|| {
                    // Every struct that meets the trait writes the
                    // method on its own table, beside its fields.
                    let targets: Vec<String> = self
                        .docs
                        .values()
                        .flat_map(|d| trait_method_sites(&d.source))
                        .filter(|s| s.trait_name.as_deref() == Some(trait_name))
                        .filter_map(|s| s.target)
                        .chain([trait_name.clone()])
                        .collect();

                    targets.iter().find_map(|owner| {
                        let (kind, src, at) = self.struct_member_site(owner, new_name)?;

                        says(kind, src, at)
                    })
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

                // A method of a plain `impl S` sits on the table of `S`,
                // beside its fields, so a field of the new name is a
                // clash. The child reads the artifact, where the field
                // list is generated text, and sees none of it.
                if let Some(owner) = trait_method_sites(&doc.source)
                    .into_iter()
                    .find(|site| site.at == (s, e))
                    .and_then(|site| site.target)
                    && let Some((kind, src, at)) = self.struct_member_site(&owner, new_name)
                {
                    return says(kind, src, at);
                }

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

    /// A name that already stands on a struct: a field of its body, or a
    /// method one of its `impl` blocks writes. The emit puts both on one
    /// table, so either kind blocks the other.
    fn struct_member_site(&self, owner: &str, name: &str) -> Option<(&'static str, &str, usize)> {
        self.docs.values().find_map(|d| {
            if let Some((at, _)) = field_declaration(&d.source, owner, name) {
                return Some(("field", d.source.as_str(), at));
            }

            let site = trait_method_sites(&d.source)
                .into_iter()
                .find(|s| s.target.as_deref() == Some(owner) && s.name == name)?;

            Some(("method", d.source.as_str(), site.at.0))
        })
    }

    /// The whole rename of one struct field, as a workspace edit. The
    /// caret sits where the struct body declares the field, and the
    /// child answers nothing at all there, so this walk writes every
    /// place by itself.
    pub(crate) fn field_edits(
        &self,
        uri: &str,
        owner: &str,
        name: &str,
        new_name: &str,
    ) -> Option<Value> {
        let mut changes: Map<String, Value> = Map::new();
        let home = self.struct_home(uri, owner);

        for (u, d) in &self.docs {
            // A parameter pattern of the struct reads the field too.
            let patterns = match self.struct_home(u, owner) == home {
                true => super::patterns::pattern_field_edits(&d.source, owner, name, new_name),

                false => Vec::new(),
            };
            let edits: Vec<Value> = field_sites(self, u, home.as_deref(), owner, name)
                .into_iter()
                .map(|(s, e)| text_edit(&d.source, s, e, new_name))
                .chain(
                    patterns
                        .into_iter()
                        .map(|(s, e, text)| text_edit(&d.source, s, e, &text)),
                )
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

    /// Where the project declares the name under the caret, for a use
    /// the emit moved into generated text: the call after `try` or
    /// `await`, the body of a `match` arm, the argument of a macro.
    /// The child sees no token there and answers nothing. This file
    /// answers first; another file answers for a name it exports.
    pub(crate) fn declared_definition(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<Value> {
        let doc = self.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;

        if !keywords::is_word_caret(&doc.source, offset) {
            return None;
        }

        let (s, e) = keywords::word_range(&doc.source, offset);
        let name = &doc.source[s..e];
        let here = self.docs.get(uri).map(|d| (uri.to_string(), d));
        let rest = self
            .docs
            .iter()
            .filter(|(u, d)| {
                u.as_str() != uri
                    && imports::exports_of(&d.source, d.is_alx)
                        .iter()
                        .any(|x| x.name == name)
            })
            .map(|(u, d)| (u.clone(), d));

        here.into_iter().chain(rest).find_map(|(u, d)| {
            let (a, b) = export_span(&d.source, name)
                .or_else(|| (u == uri).then(|| parameter_span(&d.source, offset, name))?)?;
            let s = position_of(&d.source, a);
            let e = position_of(&d.source, b);

            Some(json!([{ "uri": u, "range": range_value(s, e) }]))
        })
    }

    /// `p.b`, where `p` holds a struct: the line of the body that
    /// declares `b`. The emit writes the field list in generated text,
    /// so the child lands on the struct's `end`, and the fallback by
    /// name finds a `b` that another file exports.
    pub(crate) fn field_definition(&self, uri: &str, offset: usize) -> Option<Value> {
        let doc = self.docs.get(uri)?;
        let (start, end) = keywords::word_range(&doc.source, offset);
        let name = &doc.source[start..end];
        let owner = used_field_owner(self, doc, start)?;
        let file = self.struct_home(uri, &owner)?;
        let text = self.module_text(&file)?;
        let (a, b) = field_declaration(&text, &owner, name)?;
        let range = range_value(position_of(&text, a), position_of(&text, b));

        Some(json!([{ "uri": path_to_uri(&file), "range": range }]))
    }

    /*
    The enum a dotted variant at the caret names: the file that declares
    it and the enum's name there.

    The path in front of the variant says which enum: `Status.Active`
    under the file's own enum or its import, `Light.Active` under
    `import { Status as Light }`, and `B.Status.Active` under `import *
    as B`. Two modules may each declare a `Status`, so the name alone
    does not.
    */
    pub(crate) fn variant_home(&self, uri: &str, offset: usize) -> Option<(PathBuf, String)> {
        let doc = self.docs.get(uri)?;
        let lexed = alloy_syntax::lexer::lex(&doc.source).ok()?;
        let toks = &lexed.toks;
        // The caret at the head of the word also touches the `.` before.
        let at = toks.iter().position(|t| {
            t.kind == TokKind::Ident && (t.start as usize..=t.end as usize).contains(&offset)
        })?;
        let variant = toks[at].text(&doc.source);
        let path = path_before(&doc.source, toks, at)?;
        let (file, owner) = match path.split_once('.') {
            None if doc.shapes.iter().any(|s| s.name() == path) => (uri_to_path(uri)?, path),

            None => {
                let entry = import_entries(&doc.source)
                    .into_iter()
                    .find(|e| e.bound == path)?;

                (self.entry_module(uri, &entry)?, entry.name)
            }

            Some((module, owner)) => {
                let spec = module_bindings(&doc.source)
                    .into_iter()
                    .find(|(bound, _)| bound == module)
                    .map(|(_, spec)| spec)?;

                (self.spec_module(uri, &spec)?, owner.to_string())
            }
        };
        let text = self.module_text(&file)?;

        enum_variants(&text)
            .iter()
            .any(|(o, v, _)| *o == owner && v == variant)
            .then_some((file, owner))
    }

    /// The module a file's import list reads a name from: `./types`
    /// for `Phase` under `import { Phase } from "./types"`.
    pub(crate) fn import_home(&self, uri: &str, name: &str) -> Option<PathBuf> {
        let doc = self.docs.get(uri)?;

        import_entries(&doc.source)
            .into_iter()
            .find(|it| it.bound == name || it.name == name)
            .and_then(|it| self.entry_module(uri, &it))
    }

    /// The file an entry's module spec names. A module the editor holds
    /// open answers before the disk has it.
    fn entry_module(&self, uri: &str, entry: &ImportEntry) -> Option<PathBuf> {
        self.spec_module(uri, &entry.spec)
    }

    /// The file a module spec names, on disk or open in the editor.
    fn spec_module(&self, uri: &str, spec: &str) -> Option<PathBuf> {
        let target = imports::module_path(&self.resolve_spec(uri, spec)?);

        imports::module_file(&target).or_else(|| {
            ["aly", "alx"]
                .iter()
                .map(|ext| PathBuf::from(format!("{}.{ext}", target.display())))
                .find(|p| self.docs.contains_key(&path_to_uri(p)))
        })
    }

    /*
    The file that declares the struct a file means by `owner`.

    Two modules may each declare a `Door`, so a field rename keys by the
    declaring file and not by the name. A file means its own type of the
    name first, then the one its import list names. A file that names no
    such type may still read a field through a value, `make_item()`, and
    then the struct comes from a module it imports. A project with one
    struct of the name answers that one.
    */
    pub(crate) fn struct_home(&self, uri: &str, owner: &str) -> Option<PathBuf> {
        let doc = self.docs.get(uri)?;
        let named = |n: &str| n == owner || n.rsplit('.').next() == Some(owner);

        if doc.decls.iter().any(|d| named(&d.name)) {
            return uri_to_path(uri);
        }

        if let Some(home) = self.import_home(uri, owner) {
            return Some(home);
        }

        let declares = |file: &Path| {
            self.module_text(file)
                .is_some_and(|text| declares_a_type(&text, owner))
        };
        let imported = import_entries(&doc.source)
            .into_iter()
            .filter_map(|e| self.entry_module(uri, &e))
            .chain(
                module_bindings(&doc.source)
                    .into_iter()
                    .filter_map(|(_, spec)| self.spec_module(uri, &spec)),
            )
            .find(|file| declares(file));

        if imported.is_some() {
            return imported;
        }

        let mut declaring = self.docs.iter().filter(|(_, d)| {
            d.decls
                .iter()
                .any(|x| named(&x.name) && x.hover.contains("struct "))
        });

        match (declaring.next(), declaring.next()) {
            (Some((u, _)), None) => uri_to_path(u),

            _ => None,
        }
    }

    /// A module's text: the open document first, then the disk.
    pub(crate) fn module_text(&self, file: &Path) -> Option<String> {
        let uri = path_to_uri(file);

        match self.docs.get(&uri) {
            Some(doc) => Some(doc.source.clone()),

            None => std::fs::read_to_string(file).ok(),
        }
    }

    /// The edits a rename makes in the files that import a module: each
    /// import entry, each use an entry without an alias binds, `M.name`
    /// under a module binding, and a re-export, whose importers follow.
    #[allow(clippy::too_many_arguments)]
    fn importer_edits(
        &self,
        module_uri: &str,
        module: &Path,
        name: &str,
        new_name: &str,
        group: Option<&str>,
        is_type: bool,
        changes: &mut Map<String, Value>,
        seen: &mut Vec<PathBuf>,
    ) {
        let mut barrels: Vec<(String, PathBuf)> = Vec::new();

        for (u, d) in &self.docs {
            if *u == module_uri {
                continue;
            }

            let reaches = |spec: &str| {
                self.resolve_spec(u, spec)
                    .map(|p| imports::module_path(&p))
                    .is_some_and(|p| p == *module)
            };
            let mine: Vec<ImportEntry> = import_entries(&d.source)
                .into_iter()
                .filter(|it| it.name == name && reaches(&it.spec))
                .collect();
            let mut holders: Vec<String> = module_bindings(&d.source)
                .into_iter()
                .filter(|(_, spec)| reaches(spec))
                .map(|(bound, _)| bound)
                .collect();

            if let Some(group) = group {
                holders = self.namespace_heads(u, &d.source, module, group);
            }

            let mut edits: Vec<Value> = Vec::new();

            // An entry with no alias binds the name itself, so every
            // use of it in the file is this name. An ambient file names
            // the type with no entry at all.
            let plain =
                mine.iter().any(|it| it.alias_at.is_none()) || (is_type && u.ends_with(".d.aly"));

            for it in &mine {
                edits.push(text_edit(&d.source, it.name_at.0, it.name_at.1, new_name));
            }

            // `export { X } from "./m"` sends the name on. Without an
            // alias the barrel now exports the new name, so its own
            // importers follow.
            for it in reexport_entries(&d.source)
                .iter()
                .filter(|it| it.name == name && reaches(&it.spec))
            {
                edits.push(text_edit(&d.source, it.name_at.0, it.name_at.1, new_name));

                if it.alias_at.is_none()
                    && let Some(path) = uri_to_path(u)
                {
                    barrels.push((u.clone(), imports::module_path(&path)));
                }
            }

            if plain {
                for (s, e) in name_uses(&d.source, name) {
                    edits.push(text_edit(&d.source, s, e, new_name));
                }

                // `export { X }` after the import sends the name on too.
                if export_list_entries(&d.source)
                    .iter()
                    .any(|it| it.name == name && it.alias_at.is_none())
                    && let Some(path) = uri_to_path(u)
                {
                    barrels.push((u.clone(), imports::module_path(&path)));
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
                match changes.get_mut(u).and_then(Value::as_array_mut) {
                    Some(held) => held.extend(edits),

                    None => {
                        changes.insert(u.clone(), json!(edits));
                    }
                }
            }
        }

        for (barrel_uri, barrel) in barrels {
            if !seen.contains(&barrel) {
                seen.push(barrel.clone());
                self.importer_edits(
                    &barrel_uri,
                    &barrel,
                    name,
                    new_name,
                    None,
                    is_type,
                    changes,
                    seen,
                );
            }
        }
    }

    /// The whole rename of one name a module exports, as a workspace
    /// edit: the declaration and every use in the module, then each
    /// file that imports it. An entry with an alias keeps the alias and
    /// only its own name changes; an entry without one changes with
    /// every use under it, and `M.name` under a module binding too.
    pub(crate) fn export_rename(&self, file: &Path, name: &str, new_name: &str) -> Option<Value> {
        // An import of a barrel names the barrel, which declares
        // nothing. The walk starts where the name is declared, and it
        // comes back to the barrel and its importers from there.
        let file = self.export_home(file, name)?;
        let file = file.as_path();
        let module = imports::module_path(file);
        let module_uri = path_to_uri(file);
        let text = self.module_text(file)?;

        // A definitions file writes no import: an ambient declaration
        // stands in scope everywhere, so a type name it spells is this
        // one. Only a type reaches one, so a value skips those files.
        let is_type = declares_a_type(&text, name);
        // A member of a namespace: every reader writes it under the
        // group, so the walk reads the words each file puts in front
        // of it.
        let group = alloy::declarations::namespace_ranges(&text)
            .into_iter()
            .find(|n| n.members.iter().any(|(m, _)| m == name))
            .map(|n| n.path);
        let mut changes: Map<String, Value> = Map::new();
        let mut here: Vec<Value> = name_uses(&text, name)
            .into_iter()
            .map(|(s, e)| text_edit(&text, s, e, new_name))
            .collect();

        if let Some(group) = &group {
            let heads = self.namespace_heads(&module_uri, &text, &module, group);

            for (s, e) in member_uses(&text, &heads, name) {
                here.push(text_edit(&text, s, e, new_name));
            }
        }

        let mut seen = vec![module.clone()];
        self.importer_edits(
            &module_uri,
            &module,
            name,
            new_name,
            group.as_deref(),
            is_type,
            &mut changes,
            &mut seen,
        );

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

    /// The module that declares a name `file` exports. A barrel sends
    /// the name on with `export { X } from "./m"`, or with an import
    /// and `export { X }`, and the walk follows it. A barrel that sends
    /// the name out under an alias declares the alias itself.
    fn export_home(&self, file: &Path, name: &str) -> Option<PathBuf> {
        let mut file = file.to_path_buf();

        for _ in 0..8 {
            let text = self.module_text(&file)?;

            if export_span(&text, name).is_some() {
                return Some(file);
            }

            let passed = export_list_entries(&text)
                .iter()
                .any(|e| e.bound == name && e.alias_at.is_none());
            let entry = reexport_entries(&text)
                .into_iter()
                .chain(import_entries(&text).into_iter().filter(|_| passed))
                .find(|e| e.bound == name && e.alias_at.is_none())?;

            file = self.spec_module(&path_to_uri(&file), &entry.spec)?;
        }

        None
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
        // A list may run over several lines, so the statement and not
        // the caret's line holds the path.
        let statement = alloy_syntax::scan::import_statements(source)
            .into_iter()
            .find(|s| s.text.starts_with("import ") && (s.start..=s.end).contains(&offset))?;
        let line = statement.text.as_str();
        let (start, end) = keywords::word_range(source, offset);
        let word = &source[start..end];
        let at = start.checked_sub(statement.start)?;
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

        let file = imports::module_file(&imports::module_path(
            &self.resolve_spec(uri, &statement.spec)?,
        ))?;
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

/// Where a trait lives: the module path that declares it, and its path
/// there, `Motion.Mover`.
pub(crate) type TraitHome = (PathBuf, String);

/// One place a method of a trait is written: the trait's own body, or
/// an `impl` block.
pub(crate) struct MethodSite {
    /// The trait the site belongs to: the trait itself, or the one an
    /// `impl Trait for S` meets. `None` for a plain `impl S`. A path
    /// gives its last name, `Mover` for `impl Motion.Mover for S`.
    pub trait_name: Option<String>,
    /// The trait as the header writes it: `Motion.Mover`.
    pub trait_path: Option<String>,
    /// The type an `impl` targets. `None` inside a trait's own body.
    pub target: Option<String>,
    pub name: String,
    /// The byte range of the method's name.
    pub at: (usize, usize),
    /// The byte offset of the block's header. The namespaces around it
    /// say which trait the header names.
    pub header: usize,
}

/// The leading name of a text: `Alpha<T> as` gives `Alpha`.
fn name_head(text: &str) -> String {
    text.trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// The leading path of a text: `Motion.Mover<T> for` gives
/// `Motion.Mover`.
fn path_head(text: &str) -> String {
    let path: String = text
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    path.trim_end_matches('.').to_string()
}

/// The last name of a path: `Mover` for `Motion.Mover`.
fn last_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Every method a source writes inside a `trait` body or an `impl`
/// block. A trait and an impl both stand at the margin and both close
/// with an `end` there, so the header above a line says which block it
/// belongs to.
pub(crate) fn trait_method_sites(src: &str) -> Vec<MethodSite> {
    let mut out = Vec::new();
    let mut head: Option<(Option<String>, Option<String>, usize)> = None;
    let mut at = 0;

    for line in src.lines() {
        let start = at;
        at += line.len() + 1;
        let text = line.trim();
        let margin = !line.starts_with([' ', '\t']);
        let bare = text.strip_prefix("export ").unwrap_or(text);
        let bare = bare.strip_prefix("global ").unwrap_or(bare);

        if let Some(rest) = bare.strip_prefix("trait ") {
            head = Some((Some(name_head(rest)), None, start));

            continue;
        }

        if let Some(rest) = bare.strip_prefix("impl ") {
            head = match rest.split_once(" for ") {
                Some((t, s)) => Some((Some(path_head(t)), Some(name_head(s)), start)),

                None => Some((None, Some(name_head(rest)), start)),
            };

            continue;
        }

        // A body is indented, and the block closes with an `end` at the
        // margin. A blank line has no margin and closes nothing.
        if margin && (text == "end" || !text.is_empty()) {
            head = None;
        }

        let Some((trait_path, target, header)) = &head else {
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
            trait_name: trait_path.as_deref().map(|p| last_name(p).to_string()),
            trait_path: trait_path.clone(),
            target: target.clone(),
            name,
            at,
            header: *header,
        });
    }

    out
}

/// Whether a source bounds the type parameter `ty` by `trait_name`:
/// `<T: Trait>` or `<T: A & B>` anywhere in the file. A trait in a
/// namespace bounds by its path, `<T: Abilities.Ability>`, and the site
/// scan names it `Ability`, so the last segment of a path counts.
///
/// ponytail: a file-wide scan; two functions that bound one parameter
/// name by different traits read as both. Walk the enclosing header
/// if that shows up.
fn bound_by(src: &str, ty: &str, trait_name: &str) -> bool {
    src.match_indices('<').any(|(i, _)| {
        let Some(end) = src[i..].find('>') else {
            return false;
        };

        src[i + 1..i + end].split(',').any(|part| {
            let Some((name, bounds)) = part.split_once(':') else {
                return false;
            };

            name.trim() == ty
                && bounds
                    .split(['&', '+'])
                    .map(str::trim)
                    .any(|b| b == trait_name || b.rsplit('.').next() == Some(trait_name))
        })
    })
}

/// Every `recv:name(` and `recv.name(` call of a source: the byte range
/// of the method's name, a byte of the receiver in front of it, and
/// whether a `.` joins them.
fn method_calls(src: &str, name: &str) -> Vec<(usize, usize, usize, bool)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let toks = &lexed.toks;
    let mut out = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.text(src) != name || i < 2 {
            continue;
        }

        let sep = toks[i - 1].text(src);

        if !matches!(sep, ":" | "?:" | "." | "?.") {
            continue;
        }

        let opens_a_call = toks.get(i + 1).is_some_and(|n| {
            matches!(
                n.kind,
                TokKind::LParen | TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpHead
            ) || n.text(src) == "{"
        });

        if opens_a_call {
            out.push((
                t.start as usize,
                t.end as usize,
                toks[i - 1].start as usize,
                sep.ends_with('.'),
            ));
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
fn field_sites(
    st: &State,
    uri: &str,
    home: Option<&Path>,
    owner: &str,
    name: &str,
) -> Vec<(usize, usize)> {
    let Some(doc) = st.docs.get(uri) else {
        return Vec::new();
    };

    // Another module may declare a struct of the same name, and a file
    // with a type of its own under the name holds another field. The
    // file reaches the struct the rename names, or it holds no site.
    if st.struct_home(uri, owner).as_deref() != home {
        return Vec::new();
    }

    let mut out = constructor_keys(&doc.source, owner, name);
    out.extend(field_declaration(&doc.source, owner, name));

    if let Ok(lexed) = alloy_syntax::lexer::lex(&doc.source) {
        out.extend(
            lexed
                .toks
                .iter()
                .filter(|t| t.text(&doc.source) == name)
                .filter(|t| used_field_owner(st, doc, t.start as usize).as_deref() == Some(owner))
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

/// The scrutinee `name` of the `match` whose arm holds byte `at`: the
/// `k` of `match k with` when `at` sits on a `case` or `default` line
/// under it. The head is the nearest line above that is indented less.
fn scrutinee_of_arm(src: &str, at: usize, name: &str) -> Option<(usize, usize)> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let arm = src[line_start..].lines().next()?;

    if !(arm.trim_start().starts_with("case ") || arm.trim_start().starts_with("default")) {
        return None;
    }

    let mut end = line_start.checked_sub(1)?;
    let head_start = loop {
        let start = src[..end].rfind('\n').map_or(0, |i| i + 1);
        let line = &src[start..end];

        if !line.trim().is_empty() && indent(line) < indent(arm) {
            break start;
        }

        end = start.checked_sub(1)?;
    };
    let head = &src[head_start..end];
    let at = head.rfind("match ")? + "match ".len();

    (head[at..].trim_end().strip_suffix(" with")?.trim() == name).then(|| {
        let s = head_start + at + (head[at..].len() - head[at..].trim_start().len());

        (s, s + name.len())
    })
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

        if matches!(at, Some((ref target, false)) if target.rsplit('.').next() == Some(owner)) {
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
    // A name list may run over several lines, and the path sits on the
    // last one; an import statement reads whole.
    let statement = alloy_syntax::scan::import_statements(source)
        .into_iter()
        .find(|s| (s.start..=s.end).contains(&offset));
    let (line_start, line) = match &statement {
        Some(s) => (s.start, s.text.as_str()),

        None => {
            let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line_end = source[offset..]
                .find('\n')
                .map(|i| offset + i)
                .unwrap_or(source.len());

            (line_start, &source[line_start..line_end])
        }
    };
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

        if !is_statement || at >= reference.start as usize || !keywords::is_word_caret(line, at) {
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
    import_binding(source, uri, word, |text| {
        imports::service_bindings(text)
            .iter()
            .any(|(local, _)| local == word)
    })
}

/// Where the first import statement that `binds` accepts writes `word`.
/// A list may run over several lines, and the statement's text keeps
/// the offsets of the source.
fn import_binding(
    source: &str,
    uri: &str,
    word: &str,
    binds: impl Fn(&str) -> bool,
) -> Option<Value> {
    alloy_syntax::scan::import_statements(source)
        .into_iter()
        .filter(|s| s.text.starts_with("import ") && binds(&s.text))
        .find_map(|s| {
            let at = s.start + whole_word(&s.text, word)?;
            let range = range_value(
                position_of(source, at),
                position_of(source, at + word.len()),
            );

            Some(json!([{ "uri": uri, "range": range }]))
        })
}

/// Where `import * as M` binds `M`, when the word is such a binding.
/// The emit writes the module's `local` itself, so the child points at
/// generated text and the reader means the import line.
pub(crate) fn module_binding_definition(source: &str, uri: &str, word: &str) -> Option<Value> {
    if !module_bindings(source)
        .iter()
        .any(|(bound, _)| bound == word)
    {
        return None;
    }

    import_binding(source, uri, word, |_| true)
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
/// The uses of a name inside one byte range: the arm of a `case`
/// binding, which is the whole scope of what the pattern binds.
pub(crate) fn uses_in_range(
    src: &str,
    name: &str,
    start: usize,
    end: usize,
) -> Vec<(usize, usize)> {
    name_uses(src, name)
        .into_iter()
        .filter(|(s, _)| *s >= start && *s < end)
        .collect()
}

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
#[derive(Debug)]
pub(crate) enum Target {
    /// A name a module exports, with the file that declares it.
    Export(PathBuf, String),
    /// A name this file alone binds: the alias of an import entry, or
    /// the binding of a whole module.
    Local(String),
    /// A name a `case` pattern binds, with the byte range of the arm.
    /// The match lowers to one expression, so the child would edit the
    /// generated text of it. The arm is the whole scope of the name.
    Binding {
        name: String,
        start: usize,
        end: usize,
    },
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
    /// none of the three together. A method of a plain `impl S` is
    /// the struct's: `trait_name` holds `S`, and the child finds no
    /// site of it through a `:` call. `home` is where the trait lives,
    /// so two traits that share a name stay apart.
    Method {
        trait_name: String,
        name: String,
        home: Option<TraitHome>,
    },
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
    list_entries(src, &["import "], false)
}

/// Every name an `export { ... } from` list of the file sends on. The
/// file binds none of them; a rename edits the list.
pub(crate) fn reexport_entries(src: &str) -> Vec<ImportEntry> {
    list_entries(src, &["export {", "export type {"], false)
}

/// Every name an `export { ... }` list with no `from` sends out. The
/// entry's name is the file's own binding, and its bound name is the
/// one importers write: `export { inner as bump }` exports `bump`.
pub(crate) fn export_list_entries(src: &str) -> Vec<ImportEntry> {
    list_entries(src, &["export {", "export type {"], true)
}

/// The entries of the lists whose statements start with one of `heads`:
/// the lists that name a module with `from`, or with `local`, the ones
/// that do not.
fn list_entries(src: &str, heads: &[&str], local: bool) -> Vec<ImportEntry> {
    let mut out = Vec::new();
    let mut line_start = 0;

    for line in src.split_inclusive('\n') {
        let here = line_start;
        line_start += line.len();

        if !heads.iter().any(|h| line.trim_start().starts_with(h)) {
            continue;
        }

        // A `from` belongs to the list when it follows the `}`; one
        // further down belongs to another statement.
        let Some(open) = line.find('{').map(|i| here + i) else {
            continue;
        };
        let Some(close) = src[open..].find('}').map(|i| open + i) else {
            continue;
        };
        let tail_end = src[close..].find('\n').map_or(src.len(), |i| close + i);
        let spec = match (import_spec(&src[close..tail_end]), local) {
            (Some(spec), false) => spec,

            (None, true) => String::new(),

            _ => continue,
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

/// Where the function around the caret declares `name` as a parameter,
/// as the byte range of the name: the nearest `function` header above
/// the caret whose list holds it.
// ponytail: a closed sibling function above the caret with the same
// parameter name answers first; walk the `end`s if that shows up.
fn parameter_span(src: &str, offset: usize, name: &str) -> Option<(usize, usize)> {
    let lexed = alloy_syntax::lexer::lex(src).ok()?;
    let toks = &lexed.toks;
    let heads = toks
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, t)| (t.start as usize) < offset && t.text(src) == "function");

    for (i, _) in heads {
        let list = toks[i + 1..]
            .iter()
            .take_while(|t| t.text(src) != ")" && (t.start as usize) < offset);

        for (k, t) in list.enumerate() {
            let before = toks[i + k].text(src);

            if t.text(src) == name && matches!(before, "(" | ",") {
                return Some((t.start as usize, t.end as usize));
            }
        }
    }

    None
}

/// Where a module declares a name it exports, as the byte range of the
/// name. `export default` has an answer of its own, in
/// `default_import_definition`. The alias of `export { inner as bump }`
/// declares `bump`: no other place in the module spells it.
pub(crate) fn export_span(src: &str, name: &str) -> Option<(usize, usize)> {
    let lexed = alloy_syntax::lexer::lex(src).ok()?;
    let toks = &lexed.toks;

    toks.iter()
        .enumerate()
        .find_map(|(i, t)| {
            let before = toks.get(i.wrapping_sub(1))?.text(src);

            (t.text(src) == name && DECLARES.contains(&before))
                .then_some((t.start as usize, t.end as usize))
        })
        .or_else(|| {
            export_list_entries(src)
                .into_iter()
                .chain(reexport_entries(src))
                .find(|e| e.bound == name)
                .and_then(|e| e.alias_at)
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

/// The names an import statement binds to a whole module, each with
/// the spec of its statement: `import * as M from "./m"` and nothing
/// else. A member of such a module is written `M.name`, which a rename
/// of `name` has to follow.
///
/// A default binding is not one of these. `import M from "./m"` on a
/// module with an export table binds the `default` field, so `M.name`
/// there is a field of that value and not the module's export.
pub(crate) fn module_bindings(src: &str) -> Vec<(String, String)> {
    alloy_syntax::scan::import_statements(src)
        .into_iter()
        .filter_map(|s| Some((star_alias(&s.text)?.to_string(), s.spec)))
        .collect()
}

/// The name `import * as M` binds, from the statement's text. The list
/// of `import * as M, { a }` is not part of it.
pub(crate) fn star_alias(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("import ")?.trim_start();
    let name = rest.strip_prefix('*')?.trim_start().strip_prefix("as ")?;
    let name = name.trim_start();
    let end = name
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(name.len());

    (end > 0).then(|| &name[..end])
}

/// Every `case` pattern that names a variant, as the byte range of the
/// name: `case Some(v)`, one nested in another's payload, and one inside
/// a struct pattern. A bare name binds unless it names a unit variant,
/// so a bare `Nil` counts when `unit` says so. `Opt.Some(v)` is a
/// member use, and `member_uses` reads it.
fn pattern_uses(src: &str, name: &str, unit: bool) -> Vec<(usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let toks = &lexed.toks;
    let mut out = Vec::new();
    let mut i = 0;

    while i < toks.len() {
        if toks[i].text(src) != "case" {
            i += 1;

            continue;
        }

        let mut depth = 0i32;
        i += 1;

        while i < toks.len() {
            let text = toks[i].text(src);

            match text {
                "(" | "{" | "[" => depth += 1,
                ")" | "}" | "]" => depth -= 1,
                "then" | "case" | "default" | "end" if depth <= 0 => break,
                _ => {}
            }

            let prev = i.checked_sub(1).map(|k| toks[k].text(src));
            let next = toks.get(i + 1).map(|t| t.text(src));
            // A key of a struct pattern stands before `=`; a member
            // after `.` is the holder's.
            let own = prev != Some(".") && next != Some("=");

            if text == name && own && (next == Some("(") || unit) {
                out.push((toks[i].start as usize, toks[i].end as usize));
            }

            i += 1;
        }
    }

    out
}

/// Every `Holder.name` in a source, as the byte range of `name`.
/// Every site of a references list or a rename answer, as its URI and
/// its start.
fn site_list(answer: &Value) -> Vec<(String, (u32, u32))> {
    let start = |v: &Value| v.get("range").and_then(range_of).map(|(s, _)| s);
    let mut out: Vec<(String, (u32, u32))> = Vec::new();

    if let Some(list) = answer.as_array() {
        for loc in list {
            if let (Some(u), Some(at)) = (loc.get("uri").and_then(Value::as_str), start(loc)) {
                out.push((u.to_string(), at));
            }
        }
    }

    if let Some(changes) = answer.get("changes").and_then(Value::as_object) {
        for (u, edits) in changes {
            for e in edits.as_array().into_iter().flatten() {
                if let Some(at) = start(e) {
                    out.push((u.clone(), at));
                }
            }
        }
    }

    out
}

/// The struct whose body holds `line`, from its header to its `end`,
/// and the span of `field` in it. The emit writes a struct's field list
/// as generated text on its `end` line, so a site the child gives for a
/// field lands there.
pub(crate) fn struct_field_at_line(
    text: &str,
    line: u32,
    field: &str,
) -> Option<(String, (usize, usize))> {
    let lines: Vec<&str> = text.lines().collect();
    let line = (line as usize).min(lines.len().checked_sub(1)?);

    for at in (0..=line).rev() {
        let head = lines[at].trim_start();
        let head = head.strip_prefix("export ").unwrap_or(head);

        if let Some(rest) = head.strip_prefix("struct ") {
            let owner: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let close = (at + 1..lines.len()).find(|k| lines[*k].trim() == "end")?;

            return (close >= line)
                .then(|| field_declaration(text, &owner, field).map(|span| (owner, span)))
                .flatten();
        }

        // The `end` of another block above the line: the line sits
        // outside every struct.
        if at < line && lines[at].trim() == "end" {
            return None;
        }
    }

    None
}

/// Where the check artifact writes `field` in the type of `owner`: the
/// `type Owner = typeof(setmetatable({} :: { ... }, Owner))` the emit
/// puts on the struct's `end` line. The child types the field there,
/// so its references and its rename start from that position.
pub(crate) fn shadow_field(doc: &Doc, owner: &str, field: &str) -> Option<(u32, u32)> {
    let shadow = &doc.shadow;
    let head = format!("type {owner}");

    shadow.match_indices(&head).find_map(|(i, _)| {
        let rest = &shadow[i + head.len()..];
        let rest = match rest.strip_prefix('<') {
            Some(generics) => &generics[generics.find('>')? + 1..],

            None => rest,
        };
        let body = rest.strip_prefix(" = typeof(setmetatable({} :: {")?;
        let body_at = shadow.len() - body.len();
        let close = super::hover::group_len(&shadow[body_at - 1..], '{', '}')?;
        let inside = &shadow[body_at..body_at - 1 + close];
        let key = format!("{field}:");
        let at = inside.match_indices(&key).find_map(|(k, _)| {
            let before = inside[..k].chars().next_back();

            before
                .is_none_or(|c| matches!(c, ' ' | ',' | '{'))
                .then_some(k)
        })?;

        Some(position_of(shadow, body_at + at))
    })
}

/// The dotted path that stands in front of token `at`, the `.` before
/// it included: `B.Status` for the `Active` of `B.Status.Active`.
fn path_before(src: &str, toks: &[alloy_syntax::lexer::Tok], at: usize) -> Option<String> {
    let mut k = at.checked_sub(2)?;

    if toks[at - 1].text(src) != "." || toks[k].kind != TokKind::Ident {
        return None;
    }

    let mut path = toks[k].text(src).to_string();

    while k >= 2 && toks[k - 1].text(src) == "." && toks[k - 2].kind == TokKind::Ident {
        k -= 2;
        path = format!("{}.{path}", toks[k].text(src));
    }

    Some(path)
}

/// Each `Status.Active` a source writes, where the whole path in front
/// of the variant is one of the holders: `Status` under an import,
/// `B.Status` under `import * as B`. `X.Status.Active` names the enum of
/// another module, so a holder matches the path whole.
fn variant_uses(src: &str, holders: &[String], name: &str) -> Vec<(usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let toks = &lexed.toks;

    toks.iter()
        .enumerate()
        .filter(|(i, t)| {
            t.text(src) == name && path_before(src, toks, *i).is_some_and(|p| holders.contains(&p))
        })
        .map(|(_, t)| (t.start as usize, t.end as usize))
        .collect()
}

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
