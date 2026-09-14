use super::*;

/// The id the proxy's own `shutdown` to the child carries. The reader
/// drops the answer: the editor already has its own.
pub(crate) const CHILD_SHUTDOWN_ID: i64 = -900_001;

pub struct Server {
    pub(crate) state: Mutex<State>,
    pub(crate) child_in: Mutex<Box<dyn Write + Send>>,
    pub(crate) client_out: Mutex<Box<dyn Write + Send>>,
    /// Held for the length of a pass over the workspace. The file poll
    /// runs on its own thread, and two passes at once would open a
    /// document twice.
    pub(crate) scan: Mutex<()>,
    /// How many editor messages the request thread has in hand. A pass
    /// over the workspace waits while one is: the pass takes the state
    /// lock for every file, and a hover wants the same lock.
    pub(crate) busy: std::sync::atomic::AtomicUsize,
    /// Set by `shutdown` and `exit`: a pass over the workspace stops
    /// at its next file, and the poll thread stops ticking.
    pub(crate) stopping: std::sync::atomic::AtomicBool,
}

impl Server {
    pub fn new(
        child_in: Box<dyn Write + Send>,
        client_out: Box<dyn Write + Send>,
        extensions: Vec<alloy::extensions::Extension>,
        api_docs: Option<PathBuf>,
    ) -> Self {
        let state = State {
            settings: settings::defaults(),
            extensions,
            api_docs,
            ..State::default()
        };

        Self {
            state: Mutex::new(state),
            child_in: Mutex::new(child_in),
            client_out: Mutex::new(client_out),
            scan: Mutex::new(()),
            busy: std::sync::atomic::AtomicUsize::new(0),
            stopping: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn to_child(&self, message: &Value) {
        // The document and its version say which shadow the child took,
        // and the child keeps no answer for a change it did not take.
        if log::level() >= log::Level::Trace {
            log::trace(&format!(
                "child <- {} {} v{} id={}",
                message
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("(response)"),
                message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                message
                    .pointer("/params/textDocument/version")
                    .unwrap_or(&Value::Null),
                message.get("id").map(id_key).unwrap_or_default()
            ));
        }

        let mut w = self.child_in.lock().expect("child stdin");

        if let Err(e) = crate::rpc::write_message(&mut *w, message) {
            log::error(&format!("write to child failed: {e}"));
        }
    }

    pub(crate) fn to_client(&self, message: &Value) {
        let mut w = self.client_out.lock().expect("client stdout");

        if let Err(e) = crate::rpc::write_message(&mut *w, message) {
            log::error(&format!("write to client failed: {e}"));
        }
    }

    pub(crate) fn respond(&self, id: &Value, result: Value) {
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Handles one message from the editor. Returns false on `exit`.
    pub fn handle_client(self: &Arc<Self>, message: Value) -> bool {
        use std::sync::atomic::Ordering;

        self.busy.fetch_add(1, Ordering::Relaxed);
        let more = self.dispatch_client(message);
        self.busy.fetch_sub(1, Ordering::Relaxed);

        more
    }

    /// Waits while the request thread holds a message, a tenth of a
    /// second at most. A pass over the workspace calls it between
    /// files, so an editor request never waits behind the whole pass.
    pub(crate) fn wait_for_requests(&self) {
        use std::sync::atomic::Ordering;

        let until = std::time::Instant::now() + std::time::Duration::from_millis(100);

        while self.busy.load(Ordering::Relaxed) > 0 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn dispatch_client(self: &Arc<Self>, mut message: Value) -> bool {
        use std::sync::atomic::Ordering;

        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        log::trace(&format!(
            "client -> {} id={}",
            method.as_deref().unwrap_or("(response)"),
            message.get("id").map(id_key).unwrap_or_default()
        ));

        match method.as_deref() {
            Some("initialize") => {
                let root = message
                    .pointer("/params/rootUri")
                    .and_then(Value::as_str)
                    .and_then(uri_to_path)
                    .or_else(|| {
                        message
                            .pointer("/params/workspaceFolders/0/uri")
                            .and_then(Value::as_str)
                            .and_then(uri_to_path)
                    });
                let mut st = self.state.lock().expect("state");
                st.mirror = mirror_dir(root.as_deref());
                let _ = std::fs::remove_dir_all(&st.mirror);
                let _ = std::fs::create_dir_all(&st.mirror);
                st.root = root;
                st.initialize_id = message.get("id").map(id_key);
                st.snippets = message
                    .pointer(
                        "/params/capabilities/textDocument/completion/completionItem/snippetSupport",
                    )
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                st.watch_registration = message
                    .pointer(
                        "/params/capabilities/workspace/didChangeWatchedFiles/dynamicRegistration",
                    )
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mirror_uri = path_to_uri(&st.mirror);
                let mirror_path = st.mirror.to_string_lossy().into_owned();

                if let Some(options) = message.pointer("/params/initializationOptions") {
                    st.editor = settings::editor(options, st.editor);
                    let over = settings::from_editor(options);
                    settings::merge(&mut st.settings, &over);
                }

                let mounts = mount_alias_settings(st.root.as_deref());
                settings::merge(&mut st.settings, &mounts);

                // The child reads its own shape from here, and asks for
                // it over `workspace/configuration`, which the proxy
                // answers: the editor need not support that request.
                let child_settings = st.settings.clone();
                drop(st);

                if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
                    params.insert("initializationOptions".to_string(), child_settings);

                    // The child's workspace is the mirror.
                    if params.contains_key("rootUri") {
                        params.insert("rootUri".to_string(), Value::String(mirror_uri.clone()));
                    }

                    if params.contains_key("rootPath") {
                        params.insert("rootPath".to_string(), Value::String(mirror_path));
                    }

                    // Every folder of the editor maps into the one mirror,
                    // so the child gets one folder. With the same URI
                    // listed twice it never finishes configuring its
                    // workspaces, and every request waits forever.
                    if let Some(folders) = params
                        .get_mut("workspaceFolders")
                        .and_then(Value::as_array_mut)
                    {
                        let name = folders
                            .first()
                            .and_then(|f| f.get("name"))
                            .cloned()
                            .unwrap_or_else(|| Value::String("workspace".to_string()));
                        folders.clear();
                        folders.push(json!({ "uri": mirror_uri.clone(), "name": name }));
                    }

                    let caps = params.entry("capabilities").or_insert_with(|| json!({}));

                    if let Some(caps) = caps.as_object_mut() {
                        let ws = caps.entry("workspace").or_insert_with(|| json!({}));

                        if let Some(ws) = ws.as_object_mut() {
                            ws.insert("configuration".to_string(), Value::Bool(true));
                        }
                    }
                }

                self.to_child(&message);
            }

            Some("initialized") => {
                self.to_child(&message);
                self.watch_project_files();

                // The mirror comes first, and on this thread: the child
                // resolves a require against the files that stand in
                // it, so the whole mirror must be there before the
                // first document reaches it. It compiles nothing.
                let files = self.open_mirror();

                // The shadows compile every file of the project. On
                // this thread they would hold the editor's first
                // `didOpen`, and every request after it, until the
                // last file, so they run on their own thread.
                let scanner = Arc::clone(self);
                std::thread::spawn(move || scanner.open_shadows(files));
            }

            Some("shutdown") => {
                // The editor gives a server two seconds to answer and
                // kills it after. The child answers only once it has
                // read its definitions, which a restart mid-index does
                // not wait for, so the proxy answers now and stops its
                // own passes; the child hears the request through its
                // own id, which the reader drops.
                self.stopping.store(true, Ordering::Relaxed);

                if let Some(id) = message.get("id") {
                    self.respond(id, Value::Null);
                }

                self.to_child(&json!({
                    "jsonrpc": "2.0",
                    "id": CHILD_SHUTDOWN_ID,
                    "method": "shutdown",
                }));
            }

            Some("exit") => {
                self.stopping.store(true, Ordering::Relaxed);
                self.to_child(&message);

                return false;
            }

            Some("workspace/didChangeConfiguration") => {
                let mut st = self.state.lock().expect("state");

                if let Some(settings) = message.pointer("/params/settings") {
                    st.editor = settings::editor(settings, st.editor);
                    let over = settings::from_editor(settings);
                    settings::merge(&mut st.settings, &over);
                }

                let mounts = mount_alias_settings(st.root.as_deref());
                settings::merge(&mut st.settings, &mounts);

                let child_settings = st.settings.clone();
                drop(st);
                self.to_child(&json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": { "settings": child_settings }
                }));
            }

            Some("textDocument/didOpen") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri) {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let version = message
                        .pointer("/params/textDocument/version")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.open_doc(&uri, text, version, true);
                } else {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.plain_changed(&uri, Some(text), &[]);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didChange") => {
                let uri = text_document_uri(&message).unwrap_or_default();
                let changes = message
                    .pointer("/params/contentChanges")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                if is_alloy_uri(&uri) {
                    let version = message
                        .pointer("/params/textDocument/version")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.change_doc(&uri, version, &changes);
                } else {
                    self.plain_changed(&uri, None, &changes);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didClose") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri) {
                    // The shadow stays open so other files still resolve
                    // it; its text goes back to the disk version.
                    let mut st = self.state.lock().expect("state");
                    st.editor_open.remove(&uri);
                    drop(st);

                    match uri_to_path(&uri).and_then(|p| std::fs::read_to_string(p).ok()) {
                        Some(text) => self.change_doc(&uri, 0, &[json!({ "text": text })]),

                        None => self.close_shadow(&uri),
                    }
                } else {
                    // Back to the disk version in the mirror.
                    let mut st = self.state.lock().expect("state");
                    st.plain.remove(&uri);

                    if let Some(path) = uri_to_path(&uri) {
                        match std::fs::read_to_string(&path) {
                            Ok(text) => st.write_mirror(&path, &text),

                            Err(_) => st.remove_mirror(&path),
                        }
                    }

                    drop(st);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didSave") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri)
                    && let Some(params) = message.get_mut("params").and_then(Value::as_object_mut)
                {
                    params.remove("text");
                }

                self.forward_plain(message);

                // The import checks read a module from disk, so the
                // save is the moment an importer's report can change.
                if is_alloy_uri(&uri)
                    && let Some(path) = uri_to_path(&uri)
                {
                    self.refresh_importers(&[path]);
                }
            }

            Some("workspace/didChangeWatchedFiles") => {
                let changes = message
                    .pointer("/params/changes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // The child re-reads a `.luau` it hears about; a data
                // file's module joins the list under the module's name.
                let mut modules: Vec<Value> = Vec::new();
                let mut data_changed: Vec<PathBuf> = Vec::new();

                for change in &changes {
                    let uri = change
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let kind = change.get("type").and_then(Value::as_i64).unwrap_or(2);

                    // The configuration decides how every file
                    // compiles, so the state forgets what it read of
                    // the disk and the next compile reads it again.
                    if uri.ends_with("/alloy.toml")
                        || uri.ends_with("/.luaurc")
                        || uri.ends_with("/luaux.toml")
                    {
                        self.state.lock().expect("state").forget_disk();
                    }

                    if uri.ends_with("/alloy.toml") {
                        self.load_ingots();
                    }

                    if !is_alloy_uri(&uri) {
                        // A plain file: the mirror copy follows the disk
                        // unless the editor holds the file open.
                        let st = self.state.lock().expect("state");

                        if let Some(path) = uri_to_path(&uri)
                            && !st.plain.contains_key(&uri)
                        {
                            match (kind, std::fs::read_to_string(&path)) {
                                (3, _) | (_, Err(_)) => st.remove_mirror(&path),

                                (_, Ok(text)) => st.write_mirror(&path, &text),
                            }

                            if let Some(module) = data_module_of(&path) {
                                modules.push(json!({ "uri": path_to_uri(&module), "type": kind }));
                                data_changed.push(path);
                            }
                        }

                        drop(st);

                        continue;
                    }

                    let open = self.state.lock().expect("state").editor_open.contains(&uri);

                    match kind {
                        3 => self.close_shadow(&uri),

                        _ if !open => {
                            if let Some(text) =
                                uri_to_path(&uri).and_then(|p| std::fs::read_to_string(p).ok())
                            {
                                self.open_doc(&uri, text, 0, false);
                            }
                        }

                        // The editor holds the file, so its own
                        // notifications carry the text. The importers
                        // still read the module from disk, which this
                        // change is what moved.
                        _ => {
                            if let Some(path) = uri_to_path(&uri) {
                                self.refresh_importers(&[path]);
                            }
                        }
                    }
                }

                if let Some(list) = message
                    .pointer_mut("/params/changes")
                    .and_then(Value::as_array_mut)
                {
                    list.extend(modules);
                }

                self.forward_plain(message);

                for path in &data_changed {
                    self.refresh_dependents(path);
                }
            }

            // The mirror is the child's one folder, whatever the editor
            // adds or removes on its side.
            Some("workspace/didChangeWorkspaceFolders") => {}

            Some("workspace/didRenameFiles") => {
                let files = message
                    .pointer("/params/files")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self.renamed(&files);
            }

            Some("textDocument/formatting") => {
                if let Some(id) = message.get("id").cloned() {
                    let uri = text_document_uri(&message).unwrap_or_default();
                    self.format_document(&uri, &id);
                }
            }

            // The outline of an Alloy file comes from the source. The
            // child reads the check artifact, where a struct is a table
            // and a namespace member is one flat name.
            Some("textDocument/documentSymbol") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && is_alloy_uri(&uri)
                {
                    let symbols = {
                        let st = self.state.lock().expect("state");

                        let path = uri_to_path(&uri).unwrap_or_else(|| PathBuf::from(&uri));

                        st.docs
                            .get(&uri)
                            .and_then(|doc| document_symbols(&doc.source, &path))
                    };

                    if let Some(symbols) = symbols {
                        self.respond(&id, json!(symbols));

                        return true;
                    }
                }

                self.forward_request(message, method.as_deref());
            }

            Some("textDocument/onTypeFormatting") => {
                if let Some(id) = message.get("id").cloned() {
                    let uri = text_document_uri(&message).unwrap_or_default();
                    self.end_after_opener(&uri, &message, &id);
                }
            }

            Some("alloy/closeTag") => {
                if let Some(id) = message.get("id").cloned() {
                    self.close_tag_name(&message, &id);
                }
            }

            // A picked color: an ingot that colored the range names it;
            // else the child's `Color3` forms.
            Some("textDocument/colorPresentation") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.ingot_presentation(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, method.as_deref());
            }

            Some(m @ ("textDocument/hover" | "textDocument/completion")) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                // An ingot's hover comes first: a class in a `.alx` string
                // is the ingot's, not the markup's. Its completion items
                // likewise, where it has any.
                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && self.ingot_hover(&uri, &message, &id)
                {
                    return true;
                }

                // A space triggers a completion for the field after the
                // comma of an object initializer; every other space
                // answers nothing, so no list opens where the author is
                // typing words.
                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && message.pointer("/params/context/triggerCharacter") == Some(&json!(" "))
                    && !self.opens_a_field_list(&uri, &message)
                {
                    self.respond(&id, json!([]));

                    return true;
                }

                // A closing quote asks for nothing: the editor sends the
                // quote as a trigger either way, and a list that pops up
                // there takes the next Enter.
                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && (self.closes_a_string(&uri, &message)
                        || self.names_a_declaration(&uri, &message))
                {
                    self.respond(&id, json!([]));

                    return true;
                }

                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && self.ingot_completion(&uri, &message, &id)
                {
                    return true;
                }

                if uri.ends_with(".alx")
                    && let Some(id) = message.get("id").cloned()
                    && self.markup_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                // The markup could not lower, so the artifact holds
                // spaces where the tag stood. There is nothing behind
                // the caret to answer with, and the position past the
                // blank would answer about someone else's code.
                if matches!(m, "textDocument/hover" | "textDocument/completion")
                    && let Some(id) = message.get("id").cloned()
                    && self.stands_in_blanked_markup(&uri, &message)
                {
                    let empty = match m {
                        "textDocument/completion" => json!([]),

                        _ => Value::Null,
                    };
                    self.respond(&id, empty);

                    return true;
                }

                // `mo?[k]` and `mo![k]`: the bracket the author wrote
                // has no position of its own, so the caret on it maps
                // to the bracket the lowering wrote, where the child
                // reads the element the index answers.
                if m == "textDocument/hover"
                    && let Some(home) = self.index_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && (self.impl_header_hover(&uri, &message, &id)
                        || self.case_binding_hover(&uri, &message, &id)
                        || self.field_hover(&uri, &message, &id)
                        || self.source_binding_hover(&uri, &message, &id)
                        || self.declaration_hover(&uri, &message, &id)
                        || self.keyword_hover(&uri, &message, &id))
                {
                    return true;
                }

                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && self.context_completion(&uri, &message, &id)
                {
                    return true;
                }

                // `bx?.`, `p!.`, `await X.`: the lowering owns the member.
                if m == "textDocument/completion"
                    && let Some(home) = self.member_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // `Ok(v)` lowers to `__alloy.Ok(v)`: the caret maps past
                // a `.` the source never wrote.
                if m == "textDocument/completion"
                    && let Some(home) = self.expression_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // A binding the desugar moved, `if local c = ...`, maps
                // to a byte the child knows nothing about. The name has
                // a home in the shadow; the child answers there.
                if m == "textDocument/hover"
                    && let Some(home) = self.hover_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                self.forward_request(message, method.as_deref());
            }

            Some("alloy/blockEnd") => {
                // The editor asks after Enter: the line before the new one.
                let uri = text_document_uri(&message).unwrap_or_default();
                let line = message
                    .pointer("/params/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let indent = self
                    .state
                    .lock()
                    .expect("state")
                    .docs
                    .get(&uri)
                    .and_then(|d| block_end::needs_end(&d.source, line));

                if let Some(id) = message.get("id") {
                    let result = match indent {
                        Some(indent) => json!({ "indent": indent }),

                        None => Value::Null,
                    };
                    self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }

            Some(m @ "textDocument/rename") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.rename_answer(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            Some(m @ "textDocument/references") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && (self.namespace_references(&uri, &message, &id)
                        || self.name_references(&uri, &message, &id))
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            Some(
                m @ ("textDocument/definition"
                | "textDocument/declaration"
                | "textDocument/typeDefinition"),
            ) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.definition_answer(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            Some(_) => self.forward_request(message, method.as_deref()),

            None => {
                // A response from the editor: to a question of ours, or to
                // the child's, whose URIs then move into the mirror.
                let key = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let asked = self.state.lock().expect("state").asked.remove(&key);

                match asked {
                    Some(Asked::Watch) => {}

                    Some(Asked::Rename(edit)) => {
                        let chosen = message
                            .pointer("/result/title")
                            .and_then(Value::as_str)
                            .unwrap_or_default();

                        if chosen == UPDATE_IMPORTS {
                            let id = self.state.lock().expect("state").fresh_id();
                            self.to_client(&json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "method": "workspace/applyEdit",
                                "params": { "label": "Update imports", "edit": edit }
                            }));
                        }
                    }

                    None => self.forward_plain(message),
                }
            }
        }

        true
    }

    /// Maps a request about an Alloy document into its shadow and
    /// forwards it, remembering what it was about.
    pub(crate) fn forward_request(&self, message: Value, method: Option<&str>) {
        self.forward_request_with(message, method, None);
    }

    /// Forwards a request whose shadow position is already known.
    pub(crate) fn forward_request_at(
        &self,
        message: Value,
        method: Option<&str>,
        shadow: (u32, u32),
    ) {
        self.forward_request_with(message, method, Some(shadow));
    }

    pub(crate) fn forward_request_with(
        &self,
        mut message: Value,
        method: Option<&str>,
        shadow: Option<(u32, u32)>,
    ) {
        let uri = text_document_uri(&message);

        // The child never sees a `.d.aly` document, so a request on one
        // gets an empty answer here instead of the child's error.
        if let Some(u) = &uri
            && !child_sees(u)
            && let Some(id) = message.get("id").cloned()
        {
            let result = match method {
                Some("textDocument/diagnostic") => json!({ "kind": "full", "items": [] }),

                Some("textDocument/semanticTokens/full") => json!({ "data": [] }),

                _ => Value::Null,
            };
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return;
        }
        let ctx = uri.filter(|u| is_alloy_uri(u));
        let position = position_of_message(&message);
        let trigger = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let range = message.pointer("/params/range").and_then(range_of);
        let diagnostics = message
            .pointer("/params/context/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let query = message
            .pointer("/params/query")
            .and_then(Value::as_str)
            .map(str::to_string);

        if let Some(id) = message.get("id") {
            let key = id_key(id);
            self.state.lock().expect("state").pending.insert(
                key,
                Pending {
                    method: method.unwrap_or_default().to_string(),
                    ctx: ctx.clone(),
                    position,
                    trigger,
                    range,
                    diagnostics,
                    query,
                },
            );
        }

        let st = self.state.lock().expect("state");

        if let Some(ctx) = &ctx
            && let Some(doc) = st.docs.get(ctx)
            && let Some(params) = message.get_mut("params")
        {
            // A markup hole that opens with a keyword lowers to the
            // keyword itself, where the child completes nothing. The
            // expression after it is the same scope, and the editor
            // still inserts where the caret is.
            if method == Some("textDocument/completion")
                && ctx.ends_with(".alx")
                && let Some((l, c)) = position
                && let Some(at) = offset_of(&doc.source, l, c)
                && let Some(moved) = markup::hole_expression_start(&doc.source, at)
            {
                let (ml, mc) = position_of(&doc.source, moved);
                params["position"] = json!({ "line": ml, "character": mc });
            }

            map_into_shadow(params, doc);

            if let Some((line, character)) = shadow {
                params["position"] = json!({ "line": line, "character": character });
            }
        }

        map_uris_into_mirror(&mut message, &st);
        drop(st);
        self.to_child(&message);
    }

    /// Handles one message from the child.
    pub fn handle_child(&self, mut message: Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);

        // The answer to the proxy's own shutdown: the editor has its
        // answer already.
        if method.is_none() && message.get("id").and_then(Value::as_i64) == Some(CHILD_SHUTDOWN_ID)
        {
            return;
        }
        log::trace(&format!(
            "child -> {} id={}",
            method.as_deref().unwrap_or("(response)"),
            message.get("id").map(id_key).unwrap_or_default()
        ));

        match method.as_deref() {
            Some("textDocument/publishDiagnostics") => {
                let uri = message
                    .pointer("/params/uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let st = self.state.lock().expect("state");

                if st.runtime_uri.as_deref() == Some(uri.as_str()) {
                    return;
                }

                let (source, is_alloy) = st.editor_uri(&uri);
                drop(st);

                match is_alloy {
                    true => {
                        let diagnostics = message
                            .pointer("/params/diagnostics")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let mut st = self.state.lock().expect("state");
                        let doc_path = uri_to_path(&source);
                        let mapped: Vec<Value> = match st.docs.get(&source) {
                            Some(doc) => {
                                let mut out: Vec<Value> = Vec::new();

                                let lint_config = st.lint_config();
                                let unmet = unmet_expectations(doc, &diagnostics);

                                for mut d in diagnostics {
                                    if !keep_diagnostic(&d, doc, doc_path.as_deref(), &lint_config)
                                    {
                                        continue;
                                    }

                                    map_from_shadow(&mut d, Some(&source), &st);
                                    friendly_message(&mut d, doc, &st);

                                    // The wording pass may leave the
                                    // report naming a member that is
                                    // there; the lint says the rest.
                                    if answers_to_the_private_lint(&d, doc, &lint_config) {
                                        continue;
                                    }

                                    // Two references in one desugar map to
                                    // one source token: report it once.
                                    if !out.iter().any(|o| {
                                        o["range"] == d["range"] && o["message"] == d["message"]
                                    }) {
                                        out.push(d);
                                    }
                                }

                                out.extend(unmet);
                                collapse_diagnostics(&mut out);

                                out
                            }

                            None => Vec::new(),
                        };
                        st.child_diagnostics.insert(source.clone(), mapped);
                        drop(st);
                        self.publish(&source);
                    }

                    false => {
                        if let Some(p) = message.pointer_mut("/params/uri") {
                            *p = Value::String(source);
                        }

                        self.to_client(&message);
                    }
                }
            }

            Some("workspace/configuration") => {
                // The child's settings are ours to answer.
                let count = message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map_or(1, Vec::len);
                let settings = self.state.lock().expect("state").settings.clone();
                let result: Vec<Value> = (0..count).map(|_| settings.clone()).collect();

                if let Some(id) = message.get("id") {
                    self.to_child(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }

            Some("client/registerCapability") => {
                // Dynamic semantic token registration would bypass the
                // static one the proxy edits; the static one stays.
                if let Some(list) = message
                    .pointer_mut("/params/registrations")
                    .and_then(Value::as_array_mut)
                {
                    list.retain(|r| {
                        !r.get("method")
                            .and_then(Value::as_str)
                            .is_some_and(|m| m.starts_with("textDocument/semanticTokens"))
                    });
                }

                self.to_client(&message);
            }

            Some(_) => {
                // A server request or notification: map any locations.
                let st = self.state.lock().expect("state");

                if let Some(params) = message.get_mut("params") {
                    map_from_shadow(params, None, &st);
                }

                drop(st);
                self.to_client(&message);
            }

            None => self.child_response(message),
        }
    }

    pub(crate) fn child_response(&self, mut message: Value) {
        let key = message.get("id").map(id_key);
        let mut st = self.state.lock().expect("state");
        let pending = key.as_ref().and_then(|k| st.pending.remove(k));
        let is_init = key.is_some() && key == st.initialize_id;

        if is_init {
            st.initialize_id = None;
            edit_capabilities(&mut message);
            st.token_types = message
                .pointer("/result/capabilities/semanticTokensProvider/legend/tokenTypes")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
        }

        let (method, ctx, position, trigger, range, reported, query) = match pending {
            Some(p) => (
                p.method,
                p.ctx,
                p.position,
                p.trigger,
                p.range,
                p.diagnostics,
                p.query,
            ),

            None => (String::new(), None, None, None, None, Vec::new(), None),
        };

        // The child answers null when it has no action; the Alloy
        // rewrites need a list to join.
        if method == "textDocument/codeAction"
            && ctx.is_some()
            && message.get("result").is_some_and(Value::is_null)
        {
            message["result"] = json!([]);
        }

        if let Some(result) = message.get_mut("result") {
            // Hints and tokens in generated text describe temps: gone
            // before the mapping moves what is left.
            if let Some(uri) = &ctx
                && let Some(doc) = st.docs.get(uri)
            {
                match method.as_str() {
                    // The child says `local` or `function`; the source may
                    // have said `const`, `export`, or `async`.
                    "textDocument/hover" => {
                        if let Some((line, character)) = position
                            && let Some(value) = result
                                .pointer("/contents/value")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        {
                            let mut text = value.clone();

                            if let Some(rewritten) = restyle_hover(&text, doc, line, character) {
                                text = rewritten;
                            }

                            if let Some(kept) = keep_annotation(&text, doc, line, character) {
                                text = kept;
                            }

                            text = fold_std_shapes(&text);
                            text = crate::shapes::fold(&text, &st.known_shapes_at(ctx.as_deref()));

                            // A module's table prints every member the
                            // solver inferred, and a function defined
                            // with no parameters carries a variadic
                            // tail there. The declaration says the list
                            // is empty.
                            if text.contains("(...any)") {
                                text = close_empty_packs(&text, &empty_parameter_names(doc));
                            }

                            if let Some(written) = declared_signature(&text, doc, line, character) {
                                text = written;
                            }

                            if let Some(named) = name_trait_method(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = name_method_receiver(&text, doc, line) {
                                text = named;
                            }

                            if let Some(named) = restore_struct_arguments(&text, doc, line) {
                                text = named;
                            }

                            if let Some(dropped) = drop_bound_intersections(&text, doc) {
                                text = dropped;
                            }

                            if let Some(named) = name_by_declaration(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = unlocal_parameter(&text, doc, line, character) {
                                text = named;
                            }

                            // Inside an `impl` the head names `self`; the
                            // checker reads it off the body and prints a
                            // different answer at every site.
                            if let Some(named) = name_self_receiver(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = name_solver_variable(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) =
                                prefer_constructed_struct(&text, doc, line, character)
                            {
                                text = named;
                            }

                            if let Some(with_init) = append_initializer(&text, doc, line, character)
                            {
                                text = with_init;
                            }

                            // The head of a method comes from several
                            // passes; the comment above its declaration
                            // goes on once they have all run.
                            if let Some(with_doc) = name_method_doc(&text, doc) {
                                text = with_doc;
                            }

                            // A component's factory returns whatever the
                            // configured `create` gives; the checker has
                            // no type for it and prints its own marker.
                            text = text.replace("): *error-type*", ")");
                            text = text.replace("*error-type*", "unknown");

                            // One grammar over a file: the Alloy fence
                            // highlights `const`, `async`, and `export`,
                            // which the Luau one drops.
                            text = text.replace("```luau", "```alloy");

                            if let Some(optional) =
                                optional_index_hover(&text, doc, line, character)
                            {
                                text = optional;
                            }

                            // `type Player = Player` restates the token
                            // under the cursor and says nothing, and
                            // `string (5 bytes)` measures the key the
                            // emit wrote, not the name the source has.
                            let says_nothing = restates_itself(&text)
                                || invents_a_type(&text, doc)
                                || lowers_a_block(&text, doc, line, character)
                                || (is_byte_count(&text) && names_a_key(doc, line, character));

                            // A std member: the type above, then what
                            // the member does and an example, which no
                            // type carries.
                            let member = std_member_hover(&text, doc, line, character);

                            if let Some(section) = &member {
                                text.push_str("\n\n");
                                text.push_str(section);
                            }

                            if says_nothing && member.is_none() {
                                *result = Value::Null;
                            } else if text != value {
                                result["contents"]["value"] = json!(text);
                            }
                        }
                    }

                    // A pulled report gets the filter the push path has. The
                    // messages wait for the mapping below: a range set here
                    // in source terms would map once more, to one byte.
                    "textDocument/diagnostic" => {
                        if let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) {
                            let lint_config = st.lint_config();
                            let doc_path = uri_to_path(uri);
                            let unmet = unmet_expectations(doc, items);
                            items.retain(|d| {
                                keep_diagnostic(d, doc, doc_path.as_deref(), &lint_config)
                            });
                            items.extend(unmet);
                            collapse_diagnostics(items);
                        }
                    }

                    // A `Color3` the desugar or an ingot wrote has no place in
                    // the author's text to put a swatch on.
                    "textDocument/documentColor" => {
                        if let Some(colors) = result.as_array_mut() {
                            colors.retain(|c| {
                                c.pointer("/range/start")
                                    .and_then(position_of_value)
                                    .is_none_or(|(l, ch)| !doc.generated_at(l, ch))
                            });
                        }
                    }

                    "textDocument/inlayHint" => {
                        if let Some(hints) = result.as_array_mut() {
                            // A hint attaches to the byte before it, so
                            // that byte decides: the `)` of a generated
                            // inner function is generated even when the
                            // copied newline follows. A hint that names an
                            // error type describes the emit, not the source.
                            hints.retain_mut(|h| {
                                let error_type = hint_label(h).contains("*error-type*");
                                let offset = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, c)| offset_of(&doc.shadow, l, c));
                                let generated = offset
                                    .is_some_and(|o| doc.generated_offset(o.saturating_sub(1)));

                                // A destructuring binding is generated
                                // text end to end, and the names the
                                // braces hold are the author's own. The
                                // hint travels with the name it types
                                // and moves onto it after the mapping,
                                // which puts every hint of the line on
                                // the line's first byte.
                                if generated
                                    && !error_type
                                    && let Some(name) = destructured_name(doc, h)
                                {
                                    h[DESTRUCTURED] = json!(name);

                                    return true;
                                }

                                !error_type && !generated
                            });

                            // A label the child sends in parts folds as
                            // one text, the way its edit does; the parts'
                            // locations point into the emit anyway.
                            for h in hints.iter_mut() {
                                if h.get("label").is_some_and(Value::is_array) {
                                    h["label"] = json!(hint_label(h));
                                }
                            }

                            // An async function's hint names the Future
                            // the caller gets, without the emit's own
                            // module name on the front.
                            for h in hints.iter_mut() {
                                let async_line = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, _)| doc.shadow.lines().nth(l as usize))
                                    .is_some_and(|line| line.contains(".future(function"));

                                if async_line {
                                    name_future_hint(h);
                                }
                            }
                        }
                    }

                    "textDocument/semanticTokens/full" => {
                        if let Some(data) = result.get("data").and_then(Value::as_array) {
                            let raw: Vec<u64> = data.iter().filter_map(Value::as_u64).collect();
                            let mapped = tokens::remap(&raw, doc, &st.token_types);
                            log::debug(&format!(
                                "semantic tokens: {} in, {} out",
                                raw.len() / 5,
                                mapped.len() / 5
                            ));
                            result["data"] = json!(mapped);
                        }

                        drop(st);
                        self.to_client(&message);

                        return;
                    }

                    _ => {}
                }
            }

            if method == "workspace/diagnostic"
                && let Some(reports) = result.get_mut("items").and_then(Value::as_array_mut)
            {
                for report in reports {
                    let source = report
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(|shadow| st.shadows.get(shadow))
                        .cloned();
                    let doc = source.as_ref().and_then(|source| st.docs.get(source));

                    if let Some(doc) = doc
                        && let Some(items) = report.get_mut("items").and_then(Value::as_array_mut)
                    {
                        let lint_config = st.lint_config();
                        let doc_path = source.as_deref().and_then(uri_to_path);
                        items
                            .retain(|d| keep_diagnostic(d, doc, doc_path.as_deref(), &lint_config));
                    }
                }
            }

            map_from_shadow(result, ctx.as_deref(), &st);

            if method == "textDocument/diagnostic"
                && let Some(uri) = ctx.as_deref()
                && let Some(doc) = st.docs.get(uri)
                && let Some(items) = result.get_mut("items").and_then(Value::as_array_mut)
            {
                for d in items.iter_mut() {
                    friendly_message(d, doc, &st);
                }

                // The wording pass may leave a report naming a member
                // that is there; the push path drops the same one.
                let lint_config = st.lint_config();
                items.retain(|d| !answers_to_the_private_lint(d, doc, &lint_config));

                // The Alloy reports join the child's the way the push
                // path lists them. A rewrite may move two reports onto
                // one line, the `impl` an alias names among them; they
                // collapse after it, not before.
                *items = st.full_diagnostics(uri, std::mem::take(items));
            }

            // The ingots' colors join the child's `Color3` swatches; they
            // speak source positions already, so they join after the map.
            if method == "textDocument/documentColor"
                && let Some(uri) = &ctx
            {
                let mut extra = st.ingot_colors(uri);

                // A `Color3` call the child already colors gets one square.
                if let Value::Array(items) = result {
                    extra.retain(|e| !items.iter().any(|i| i.get("range") == e.get("range")));
                }

                if !extra.is_empty() {
                    match result {
                        Value::Array(items) => items.extend(extra),

                        Value::Null => *result = Value::Array(extra),

                        _ => {}
                    }
                }
            }

            // The editor never sees the runtime's table: `__alloy.Future<T>`
            // reads `Future<T>`, and `__alloy_string.trim` reads `string.trim`.
            if ctx.is_some()
                && matches!(
                    method.as_str(),
                    "textDocument/hover"
                        | "textDocument/inlayHint"
                        | "textDocument/completion"
                        | "completionItem/resolve"
                        | "textDocument/signatureHelp"
                )
            {
                // Before the rewrite: `__alloy_string` reads `oy_string`
                // once `__all` goes, and no list should carry it at all.
                if method == "textDocument/completion" {
                    drop_internal_items(result);
                }

                strip_std_prefix(result);
                // The checker prints a struct as its runtime table and a
                // unit enum as a union of strings; the names go back.
                crate::shapes::fold_value(result, &st.known_shapes_at(ctx.as_deref()));

                // The same variadic tail the hover drops: a member of a
                // module reads `(...any) -> T` where the source wrote
                // no parameters at all.
                if matches!(
                    method.as_str(),
                    "textDocument/completion" | "completionItem/resolve"
                ) && let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u))
                {
                    close_item_packs(result, &empty_parameter_names(doc));
                }

                // A type hint inserts its edit on a click: the label shows
                // that text, so the two never differ.
                if method == "textDocument/inlayHint"
                    && let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u))
                    && let Some(hints) = result.as_array_mut()
                {
                    clean_hints(hints, doc);
                }

                if let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u)) {
                    strip_import_temps(result, &doc.shadow);
                }
            }

            match method.as_str() {
                // The rewrites of the lints in the range, as quick fixes,
                // and one action that applies every rewrite of the file.
                "textDocument/codeAction" => {
                    if let Some(uri) = &ctx
                        && let Some(range) = range
                        && let Some(actions) = result.as_array_mut()
                    {
                        actions.extend(st.contract_actions(uri, range));
                        actions.extend(st.header_as_actions(uri, range));
                        actions.extend(st.global_actions(uri, range));
                        actions.extend(st.compiler_actions(uri, range));
                        actions.extend(st.lint_actions(uri, range));
                        actions.extend(st.ingot_actions(uri, range));
                        actions.extend(st.import_actions(uri, &reported));
                        st.unused_import_actions(uri, range, actions);
                    }
                }

                "textDocument/signatureHelp" => {
                    if let Some(uri) = &ctx {
                        st.rewrite_variant_signatures(uri, result);

                        // The child answered nothing: a macro call is
                        // gone from the emit, and a file with an
                        // unclosed call has no compile at all, so the
                        // shadow is the Alloy source. The declaration
                        // the call names says what it takes.
                        let empty = result
                            .pointer("/signatures")
                            .and_then(Value::as_array)
                            .is_none_or(Vec::is_empty);

                        if empty
                            && let Some((line, character)) = position
                            && let Some(help) = st.declared_signature_help(uri, line, character)
                        {
                            *result = help;
                        }
                    }
                }

                // A struct field: the constructor writes it as a table
                // key the child ties to no field.
                "textDocument/rename" => {
                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                    {
                        st.mend_field_rename(uri, line, character, result);
                    }
                }

                "textDocument/references" => {
                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                    {
                        st.mend_field_references(uri, line, character, result);
                    }
                }

                // A link sits on the require the emit wrote, which maps to
                // the start of the import; it moves to the quoted path of
                // that source line, and its target leaves the mirror. One
                // shadow line can hold two requires, the runtime among
                // them, so the reader gets one link per import path.
                "textDocument/documentLink" => {
                    if let Some(uri) = &ctx
                        && let Some(doc) = st.docs.get(uri)
                        && let Some(links) = result.as_array_mut()
                    {
                        let mut seen: HashSet<(u32, u32)> = HashSet::new();

                        links.retain_mut(|link| {
                            let Some(target) = link.get("target").and_then(Value::as_str) else {
                                return false;
                            };
                            let real = uri_to_path(target).map(|p| normalize(&p));

                            // Alloy writes the runtime require; the reader
                            // wrote no import of it, so it gets no link.
                            if real
                                .as_ref()
                                .and_then(|p| st.real_path(p))
                                .is_some_and(|p| st.runtimes.borrow().contains(&normalize(&p)))
                            {
                                return false;
                            }

                            // A link out of the mirror names the file the
                            // reader edits, not its copy.
                            let (target, _) = st.editor_uri(target);

                            if uri_to_path(&target).is_none_or(|p| !p.exists()) {
                                return false;
                            }

                            let Some(((line, _), _)) = link.get("range").and_then(range_of) else {
                                return false;
                            };
                            let Some((s, e)) = quoted_span_on_line(&doc.source, line) else {
                                return false;
                            };

                            if !seen.insert((line, s)) {
                                return false;
                            }

                            link["target"] = json!(target);
                            link["range"] = range_value((line, s), (line, e));

                            true
                        });
                    }
                }

                // Every symbol the child found in a dot directory of the
                // mirror: `.ember` and `.alloy` stand there for the
                // requires and the sourcemap, and hold no source the
                // reader wrote.
                "workspace/symbol" => {
                    if let Some(symbols) = result.as_array_mut() {
                        let (mirror, root) = (st.mirror.clone(), st.root.clone());

                        symbols.retain(|symbol| {
                            let Some(uri) = symbol.pointer("/location/uri").and_then(Value::as_str)
                            else {
                                return true;
                            };

                            // An Alloy source answers from its own text
                            // below: the emit spells a namespace member
                            // `Ns_T` and writes a `__new` beside it.
                            if st.docs.contains_key(uri) {
                                return false;
                            }

                            uri_to_path(uri).is_none_or(|path| {
                                // The runtime is the compiler's own
                                // module, written into the build output;
                                // nothing in it is the reader's.
                                !in_a_dot_directory(&path, &mirror, root.as_deref())
                                    && !st.runtimes.borrow().contains(&normalize(&path))
                            })
                        });
                        // A `declare Name: T` binds no name the child can
                        // point at, so the definitions files answer here.
                        ambient_symbols(&st, query.as_deref(), symbols);
                        source_symbols(&st, query.as_deref(), symbols);
                    }
                }

                "workspace/diagnostic" => {
                    if let Some(reports) = result.get_mut("items").and_then(Value::as_array_mut) {
                        for report in reports {
                            let uri = report
                                .get("uri")
                                .and_then(Value::as_str)
                                .map(str::to_string);

                            // Alloy's own reports travel by push alone.
                            if let Some(uri) = uri
                                && st.docs.contains_key(&uri)
                                && let Some(items) =
                                    report.get_mut("items").and_then(Value::as_array_mut)
                            {
                                collapse_diagnostics(items);
                            }
                        }
                    }
                }

                "textDocument/completion" => {
                    // Enter opens no list. The `end` of an open block
                    // arrives as an on-type edit, and any other name here
                    // pops a popup the reader has to dismiss.
                    if trigger.as_deref() == Some("\n") {
                        *result = json!([]);
                    }

                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                        && trigger.as_deref() != Some("\n")
                    {
                        // A dotted value path the child could not
                        // follow answered with the scope of the file.
                        // The walk over the imports and the namespaces
                        // says what the path holds, and every pass
                        // below reads that list instead.
                        if let Some(items) = st.value_path_members(uri, line, character, result) {
                            *result = json!(items);
                        }

                        st.mark_enum_members(uri, line, character, result);
                        st.mark_declarations(uri, result);
                        st.mark_namespaces(uri, line, character, result);

                        if let Some(doc) = st.docs.get(uri) {
                            complete_std_members(doc, line, character, st.snippets, result);
                            attach_std_member_docs(result, doc, line, character);
                        }

                        let member = st
                            .docs
                            .get(uri)
                            .is_some_and(|d| member_position(d, line, character).is_some());
                        // Inside a string the child answers alone: it
                        // knows the class names `Instance.new("` and
                        // `GetService("` take, and nothing else belongs
                        // between quotes.
                        let quoted = st
                            .docs
                            .get(uri)
                            .and_then(|d| offset_of(&d.source, line, character))
                            .zip(st.docs.get(uri))
                            .is_some_and(|(at, d)| context::in_string(&d.source, at));
                        // A member list names what the value has; an
                        // auto-import is a new name, which cannot follow
                        // a `.` or a `:`.
                        let mut extra = match member || quoted {
                            true => Vec::new(),

                            false => st.auto_imports(uri, line, character),
                        };

                        if !quoted {
                            extra.extend(st.primitive_completions(uri, line, character, result));
                            extra.extend(st.std_completions(uri, line, character, result));
                            extra.extend(st.value_scope(uri, line, character, result));
                            extra.extend(st.directive_completions(uri, line, character));
                            // A trait has no table in the emit, so the
                            // child answers nothing for `self` inside a
                            // default method.
                            extra.extend(st.trait_self_members(uri, line, character));
                            // An `impl` of a struct another file declares
                            // writes its methods on the imported table,
                            // and the child types that table from the
                            // module alone.
                            extra.extend(st.impl_self_members(uri, line, character, result));
                        }

                        let (from_ingots, incomplete) =
                            st.ingot_items(uri, line, character, trigger.as_deref());
                        extra.extend(from_ingots);

                        if !extra.is_empty() {
                            match result {
                                Value::Array(items) => items.extend(extra),

                                Value::Object(obj) => {
                                    if let Some(items) =
                                        obj.get_mut("items").and_then(Value::as_array_mut)
                                    {
                                        items.extend(extra);
                                    }
                                }

                                Value::Null => *result = Value::Array(extra),

                                _ => {}
                            }
                        }

                        // An ingot's list is made from the word being
                        // typed, so the child's answer stops being the
                        // whole answer.
                        if incomplete {
                            let list = match result {
                                Value::Array(items) => Some(std::mem::take(items)),

                                Value::Object(obj) => {
                                    obj.insert("isIncomplete".into(), json!(true));

                                    None
                                }

                                _ => None,
                            };

                            if let Some(items) = list {
                                *result = json!({ "isIncomplete": true, "items": items });
                            }
                        }

                        if let Some(doc) = st.docs.get(uri) {
                            clean_completion(result, doc, line, character, st.snippets);
                        }

                        st.filter_remote_members(uri, line, character, result);
                        // A static of an `impl` sits on the same table as
                        // the methods, so the child offers it after a
                        // `self.` where no `self` can call it.
                        st.drop_impl_statics(uri, line, character, result);
                        // After the merge: the child's rows and the
                        // proxy's own read alike, so one pass marks
                        // every deprecated row and hides what the
                        // editor's settings hide.
                        st.deprecated_pass(uri, result);
                        // After the clean: the spec `@pkg/react` is the
                        // detail, and the clean drops a detail that
                        // spells no type.
                        st.rewrite_child_auto_imports(uri, result);
                        st.keyword_first(uri, line, character, result);
                    }
                }

                _ => {}
            }
        }

        drop(st);
        self.to_client(&message);
    }
}
