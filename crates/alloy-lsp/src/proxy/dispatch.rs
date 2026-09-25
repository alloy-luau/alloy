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
    /// When each file was last edited. The pass that compiles the
    /// files which import it reads the time, so a burst of keystrokes
    /// costs one pass. See `schedule_import_refresh`.
    pub(crate) edited: Mutex<HashMap<PathBuf, std::time::Instant>>,
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
            edited: Mutex::new(HashMap::new()),
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
                st.mirror = mirror_dir(root.as_deref(), mirror_above(root.as_deref()));
                let _ = std::fs::remove_dir_all(mirror_base(&st.mirror));
                purge_stale_mirrors(&st.mirror);
                let _ = std::fs::create_dir_all(&st.mirror);
                claim_mirror(&st.mirror);
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
                alloy_syntax::parser::spawn_deep(move || scanner.open_shadows(files));
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

                // The mirror is this session's alone; a root opened
                // once and never again left its copy behind.
                let mirror = self.state.lock().expect("state").mirror.clone();
                let _ = std::fs::remove_dir_all(mirror_base(&mirror));

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

                    // The indexes of an importer read this file's
                    // text, so an edit here reaches the importer only
                    // when it compiles again.
                    if let Some(path) = uri_to_path(&uri) {
                        self.schedule_import_refresh(path);
                    }
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
                // The project's structs come from disk too.
                if is_alloy_uri(&uri)
                    && let Some(path) = uri_to_path(&uri)
                {
                    self.state
                        .lock()
                        .expect("state")
                        .project_shapes
                        .borrow_mut()
                        .take();
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
                let mut config_changed = false;

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
                    let config_file = uri.ends_with("/alloy.toml") || uri.ends_with("/.config.aly");

                    if config_file || uri.ends_with("/.luaurc") || uri.ends_with("/luaux.toml") {
                        self.state.lock().expect("state").forget_disk();
                        config_changed = true;
                    }

                    if config_file {
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

                // A file the editor shows compiled with the old config,
                // and no edit comes to compile it again. The reports on
                // the config files follow the disk too.
                if config_changed {
                    self.publish_alias_problems();
                    let open: Vec<String> = self
                        .state
                        .lock()
                        .expect("state")
                        .editor_open
                        .iter()
                        .cloned()
                        .collect();

                    for uri in open {
                        self.resend_doc(&uri);
                        self.publish(&uri);
                    }
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

            // Folding reads the source's own blocks. The child folds the
            // shadow, and its ranges start past the end of a line.
            Some("textDocument/foldingRange") => {
                let uri = text_document_uri(&message).unwrap_or_default();
                let ranges = {
                    let st = self.state.lock().expect("state");

                    st.docs.get(&uri).map(|d| folding_ranges(&d.source))
                };

                match (message.get("id").cloned(), ranges) {
                    (Some(id), Some(ranges)) => self.respond(&id, json!(ranges)),

                    _ => self.forward_request(message, method.as_deref()),
                }
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

                // A `.config.aly` table answers from the schema of
                // `alloy.toml`, before any rule about spaces or quotes.
                if crate::config_aly::is_config(&uri)
                    && let Some(id) = message.get("id").cloned()
                    && self.config_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                // Inside a parameter pattern the fields of its type
                // complete, before the rules about spaces and names that
                // are being declared, and a bound name hovers as its field.
                if let Some(id) = message.get("id").cloned()
                    && self.pattern_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                // A space triggers a completion for the field after the
                // comma of an object initializer, and for the next name
                // of an import list; every other space answers nothing,
                // so no list opens where the author is typing words.
                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && message.pointer("/params/context/triggerCharacter") == Some(&json!(" "))
                    && !self.opens_a_list(&uri, &message)
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

                // A key of a table literal: the child reads it as a
                // string and answers its byte count. At the binding it
                // prints the whole record, and the response takes the
                // key's entry out of that.
                if m == "textDocument/hover"
                    && let Some(home) = self.literal_key_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // `script.Parent->sys`: the name lowers to the string of
                // a `FindFirstChild("`, where the child lists the
                // children the sourcemap gives, and nothing else.
                if m == "textDocument/completion"
                    && let Some(home) = self.child_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

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
                    && (self.prop_answer(m, &uri, &message, &id)
                        || self.rename_answer(&uri, &message, &id))
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            Some(m @ "textDocument/references") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && (self.prop_answer(m, &uri, &message, &id)
                        || self.namespace_references(&uri, &message, &id)
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

                // A bound name of a parameter pattern reads a field.
                if m == "textDocument/definition"
                    && let Some(id) = message.get("id").cloned()
                    && self.pattern_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                if let Some(id) = message.get("id").cloned()
                    && (self.prop_answer(m, &uri, &message, &id)
                        || self.definition_answer(&uri, &message, &id))
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            // `$dbg(Point.new(1, 2))`: a call inside an intrinsic's
            // argument answers from the code copy of the argument. The
            // intrinsic's own list, with no call open in it, keeps the
            // plain route, where the declaration answers.
            Some(m @ "textDocument/signatureHelp") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                // `remote test(` declares; the child sees a call there.
                if self.in_declared_params(&uri, &message) {
                    self.respond(&message["id"], Value::Null);

                    return true;
                }

                if let Some(home) = self.signature_home(&uri, &message) {
                    self.forward_request_at(message, Some(m), home);

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
        // A rename carries its new name the same way, for the answer
        // the proxy builds when the child has none.
        let query = message
            .pointer("/params/query")
            .or_else(|| message.pointer("/params/newName"))
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

                // The merged `.d.aly` files, which the child could not
                // load: each report goes to the file its line came from,
                // through that file's map when it is open. Every file
                // gets its list, so a fixed one clears.
                let read = uri_to_path(&uri);
                let sources: Vec<String> = st
                    .definition_sources
                    .iter()
                    .filter(|(r, _)| Some(r) == read.as_ref())
                    .map(|(_, s)| path_to_uri(&s.source))
                    .collect();

                if !sources.is_empty() {
                    let mut lists: Vec<(String, Vec<Value>, bool)> = sources
                        .iter()
                        .map(|s| (s.clone(), Vec::new(), st.docs.contains_key(s)))
                        .collect();
                    let diagnostics = message
                        .pointer("/params/diagnostics")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();

                    for mut d in diagnostics {
                        let line = d.pointer("/range/start/line").and_then(Value::as_u64);
                        let Some((source, first)) =
                            line.and_then(|l| st.declared_at(&uri, l as usize))
                        else {
                            continue;
                        };

                        if let Some(range) = d.get_mut("range") {
                            shift_lines(range, first);
                        }

                        if let Some((_, list, open)) = lists.iter_mut().find(|l| l.0 == source) {
                            if *open {
                                map_from_shadow(&mut d, Some(&source), &st);
                            }

                            list.push(d);
                        }
                    }

                    drop(st);

                    for (source, list, open) in lists {
                        self.state
                            .lock()
                            .expect("state")
                            .child_diagnostics
                            .insert(source.clone(), list.clone());

                        match open {
                            true => self.publish(&source),

                            false => self.to_client(&json!({
                                "jsonrpc": "2.0",
                                "method": "textDocument/publishDiagnostics",
                                "params": { "uri": source, "diagnostics": list },
                            })),
                        }
                    }

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

                // "Failed to read definitions file" names the merged copy,
                // which the reader never wrote, so it names the `.d.aly`
                // files in it instead.
                if let Some(Value::String(text)) = message.pointer_mut("/params/message")
                    && let Some((read, _)) = st
                        .definition_sources
                        .iter()
                        .find(|(read, _)| text.contains(&*read.to_string_lossy()))
                {
                    let names: Vec<String> = st
                        .definition_sources
                        .iter()
                        .filter(|(r, _)| r == read)
                        .map(|(_, s)| s.source.display().to_string())
                        .collect();
                    *text = text.replace(&*read.to_string_lossy(), &names.join(", "));
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
            let legend = |key: &str| -> Vec<String> {
                message
                    .pointer(&format!(
                        "/result/capabilities/semanticTokensProvider/legend/{key}"
                    ))
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            st.token_types = legend("tokenTypes");
            st.token_modifiers = legend("tokenModifiers");
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

                            // Before any pass reads the `where` clause of
                            // a solver variable as a type of its own.
                            if let Some(named) = name_solver_local(&st, &text, doc, line, character)
                                .or_else(|| {
                                    let reach: Vec<_> = st
                                        .imported_docs(uri)
                                        .into_iter()
                                        .flat_map(|d| d.import_decls.iter())
                                        .collect();

                                    name_solver_struct(&text, doc, &reach)
                                })
                            {
                                text = named;
                            }

                            if let Some(rewritten) = restyle_hover(&text, doc, line, character) {
                                text = rewritten;
                            }

                            if let Some(plain) =
                                super::patterns::without_pattern_temps(&text, &doc.source)
                            {
                                text = plain;
                            }

                            if let Some(kept) = keep_annotation(&text, doc, line, character) {
                                text = kept;
                            }

                            text = fold_std_shapes(&text);
                            text = alloy::shapes::fold(&text, &st.known_shapes_at(ctx.as_deref()));
                            text = colon_without_self(&text);

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

                            if let Some(path) = uri_to_path(uri)
                                && let Some(types) =
                                    type_only_module_hover(&text, doc, &path, line, character)
                            {
                                text = types;
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

                            // A child name asked at the name that holds
                            // the lookup: the child hover, with the type
                            // the sourcemap gives.
                            if let Some((child, range)) =
                                child_lookup_hover(&value, doc, line, character)
                            {
                                text = child;
                                result["range"] = range;
                            }

                            // A key of a table literal asked at its
                            // binding: the entry the key names is the
                            // hover, and a record with no such entry
                            // says nothing about the key.
                            let key_path = literal_key_path(doc, line, character);
                            let key_entry = key_path
                                .as_deref()
                                .and_then(|path| record_entry(&text, path));
                            let key_missing = key_path.is_some() && key_entry.is_none();

                            if let Some(entry) = key_entry
                                && let Some(Caret { start, end, .. }) =
                                    Caret::at(&doc.source, line, character)
                            {
                                let (sl, sc) = position_of(&doc.source, start);
                                let (el, ec) = position_of(&doc.source, end);
                                text = entry;
                                result["range"] = json!({
                                    "start": { "line": sl, "character": sc },
                                    "end": { "line": el, "character": ec }
                                });
                            }

                            // `type Player = Player` restates the token
                            // under the cursor and says nothing, and
                            // `string (5 bytes)` measures the key the
                            // emit wrote, not the name the source has.
                            let says_nothing = key_missing
                                || restates_itself(&text)
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

                                if error_type || generated {
                                    return false;
                                }

                                // The desugar writes a `local` of its
                                // own for a name the author gave: the
                                // alias of `match e as n with`, the
                                // binding of `if local n = e`, a name
                                // inside `local { a, b } = t`. The byte
                                // after the name is generated, so the
                                // map would send the hint to the head
                                // of the statement, which spells a
                                // keyword. The hint carries the source
                                // position, and the fold puts it there.
                                if let Some((line, character)) = name_end(doc, h) {
                                    h[NAME_END] = json!({
                                        "line": line,
                                        "character": character,
                                    });
                                }

                                true
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
                            let mapped =
                                tokens::remap(&raw, doc, &st.token_types, &st.token_modifiers);
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

            // Before the map: the check reads the child's ranges in the
            // shadow, where generated text still shows as generated.
            if method == "textDocument/codeAction"
                && let Some(uri) = &ctx
                && let Some(actions) = result.as_array_mut()
            {
                actions.retain(|a| st.keeps_child_action(a, uri, range));
            }

            // A refactor the list kept can still edit generated text
            // around the selection. It then applies nothing, and says so.
            if method == "codeAction/resolve"
                && let Some(action) = result.as_object_mut()
                && action
                    .get("edit")
                    .is_some_and(|e| !st.writes_source_only(e))
            {
                action.remove("edit");
                self.to_client(&json!({
                    "jsonrpc": "2.0",
                    "method": "window/showMessage",
                    "params": {
                        "type": 2,
                        "message": "Alloy: this refactor changes code that the compiler generates, so it does not apply here.",
                    },
                }));
            }

            // A follow-up command reads the shadow's edits, so it maps
            // before the edits do.
            match method.as_str() {
                "textDocument/codeAction" => result
                    .as_array_mut()
                    .into_iter()
                    .flatten()
                    .for_each(|a| map_follow_up(a, &st)),

                "codeAction/resolve" => map_follow_up(result, &st),

                _ => {}
            }

            map_from_shadow(result, ctx.as_deref(), &st);

            // An extract that breaks the parse applies nothing, and its
            // rename has no name to rename.
            if method == "codeAction/resolve"
                && !st.extract_parses(result)
                && let Some(action) = result.as_object_mut()
            {
                action.remove("edit");
                action.remove("command");
                self.to_client(&json!({
                    "jsonrpc": "2.0",
                    "method": "window/showMessage",
                    "params": {
                        "type": 2,
                        "message": "Alloy: this extract would leave code that does not parse, so it does not apply here.",
                    },
                }));
            }

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

            if method == "textDocument/signatureHelp" {
                parameter_labels_as_text(result);
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
                alloy::shapes::fold_value(result, &st.known_shapes_at(ctx.as_deref()));

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
                    // A return type reads `-> T` when the editor asks
                    // for the arrow; `local x: T` has one spelling and
                    // keeps the colon.
                    arrow_returns(hints, doc, st.editor.arrow_return_hints);

                    if st.settings.pointer("/inlayHints/variableTypes") != Some(&json!(false))
                        && let Some(path) = ctx.as_deref().and_then(uri_to_path)
                    {
                        type_only_module_hints(hints, doc, &path);
                    }
                }

                if let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u)) {
                    strip_import_temps(result, &doc.shadow);
                }
            }

            // A site in generated text maps to its anchor, which spells
            // another word: gone from a references list and a rename. The
            // field mends read the child's whole answer first: a stray
            // site on a struct's `end` names the struct.
            let child = result.clone();

            if matches!(
                method.as_str(),
                "textDocument/references" | "textDocument/rename"
            ) && let Some(uri) = &ctx
                && let Some((line, character)) = position
            {
                // A rename the child could not answer still takes the
                // field's own walk.
                if method == "textDocument/rename"
                    && result.is_null()
                    && let Some(new_name) = &query
                {
                    *result = json!({ "changes": { uri.clone(): [] } });
                    result["changes"][uri.as_str()] = json!([{
                        "range": range_value((line, character), (line, character)),
                        "newText": new_name,
                    }]);
                }

                st.drop_stray_sites(uri, line, character, result);
            }

            match method.as_str() {
                // The child answered nothing, as for `try f(x)`, whose
                // desugar moved the name into generated text; or it
                // answered a site in generated text, the forward `local`
                // of a hoisted function. The declaration the name reads
                // is what the reader means. A file with no such
                // declaration keeps the child's answer: the anchor of a
                // destructuring binding is its own `local` line.
                "textDocument/definition" | "textDocument/declaration" => {
                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                    {
                        st.mend_field_definition(uri, line, character, result);
                        let child = result.clone();
                        st.drop_stray_sites(uri, line, character, result);

                        if result.as_array().is_none_or(Vec::is_empty) {
                            *result = st
                                .declared_definition(uri, line, character)
                                .unwrap_or(child);
                        }
                    }
                }

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
                        drop_child_prefix_fixes(actions);
                    }
                }

                "textDocument/signatureHelp" => {
                    if let Some(uri) = &ctx {
                        st.rewrite_variant_signatures(uri, result);

                        if let Some((line, character)) = position
                            && let Some(doc) = st.docs.get(uri)
                        {
                            restyle_signatures(result, doc, line, character);
                        }

                        let mended = position.is_some_and(|(line, character)| {
                            st.mend_constructor_signature(uri, line, character, result)
                        });

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
                            && !mended
                            && let Some((line, character)) = position
                            && let Some(help) = st.declared_signature_help(uri, line, character)
                        {
                            *result = help;
                        }

                        parameter_labels_as_offsets(result);
                    }
                }

                // A shorthand pattern entry keeps its field. A struct
                // field: the constructor writes it as a table key the
                // child ties to no field.
                "textDocument/rename" => {
                    st.mend_pattern_rename(result);

                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                    {
                        st.mend_field_rename(uri, line, character, &child, result);
                        st.mend_export_list(uri, line, character, result);
                        st.mend_prop_attributes(uri, line, character, result);
                    }
                }

                "textDocument/references" => {
                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                    {
                        st.mend_field_references(uri, line, character, &child, result);
                        st.mend_export_list(uri, line, character, result);
                        st.mend_prop_attributes(uri, line, character, result);
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

                            if !seen.insert(s) {
                                return false;
                            }

                            link["target"] = json!(target);
                            link["range"] = range_value(s, e);

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
                            complete_std_module(doc, line, character, result);
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
                            .is_some_and(|(at, d)| {
                                // A child name was asked inside the string
                                // its lookup lowers to.
                                context::in_string(&d.source, at)
                                    || context::child_name_start(&d.source, at)
                                        .and_then(|start| child_call(d, start))
                                        .is_some()
                            });
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
                            extra.extend(st.functions_below(uri, line, character, result));
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

                        let (from_ingots, incomplete) = st
                            .ingot_items(uri, line, character, trigger.as_deref())
                            .map_or((Vec::new(), false), |(items, c)| (items, c.incomplete));
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
                            let reach: Vec<_> = st
                                .imported_docs(uri)
                                .into_iter()
                                .flat_map(|d| d.import_shapes.iter())
                                .collect();

                            clean_completion(result, doc, &reach, line, character, st.snippets);
                        }

                        st.child_name_details(uri, line, character, result);

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

/// The parameters of every signature, each as the text of its label.
/// The child gives UTF-16 offsets into its own label, and the passes
/// that shorten the label would leave them pointing past its end.
fn parameter_labels_as_text(result: &mut Value) {
    for sig in result
        .get_mut("signatures")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let label: Vec<u16> = sig["label"].as_str().unwrap_or("").encode_utf16().collect();

        for p in sig
            .get_mut("parameters")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let span = p["label"]
                .as_array()
                .and_then(|a| Some((a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize)));

            if let Some((s, e)) = span
                && let Some(part) = label.get(s..e)
            {
                p["label"] = json!(String::from_utf16_lossy(part));
            }
        }
    }
}

/// The parameters back as UTF-16 offsets into the final label, so the
/// editor marks the active one exactly. Each one is found in order
/// among the entries of the label's list, not in its text: `self:
/// Signal<number, string>` holds the words of the entries after it. A
/// text the label no longer holds stays text.
fn parameter_labels_as_offsets(result: &mut Value) {
    for sig in result
        .get_mut("signatures")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let label = sig["label"].as_str().unwrap_or("").to_string();
        let entries = label_entries(&label);
        let mut next = 0;
        let units = |bytes: usize| label[..bytes].encode_utf16().count();

        for p in sig
            .get_mut("parameters")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let Some(text) = p["label"].as_str().filter(|t| !t.is_empty()) else {
                continue;
            };
            let rest = entries.get(next..).unwrap_or_default();
            // The entry that is the parameter, else the first that
            // starts with it or holds it.
            let found = rest
                .iter()
                .position(|&(s, e)| &label[s..e] == text)
                .or_else(|| {
                    rest.iter()
                        .position(|&(s, e)| label[s..e].starts_with(text))
                })
                .or_else(|| rest.iter().position(|&(s, e)| label[s..e].contains(text)));

            if let Some(k) = found {
                let (s, e) = rest[k];
                let at = s + label[s..e].find(text).unwrap_or_default();
                next += k + 1;
                p["label"] = json!([units(at), units(at + text.len())]);
            }
        }
    }
}

/// The byte range of each entry of a label's parameter list, split at
/// the commas outside every bracket. The `>` of a `->` closes nothing.
fn label_entries(label: &str) -> Vec<(usize, usize)> {
    let Some(open) = label.find('(') else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    let mut prev = '(';
    let mut push = |from: usize, to: usize| {
        let text = &label[from..to];
        let from = from + (text.len() - text.trim_start().len());

        entries.push((from, from + text.trim().len()));
    };

    for (i, c) in label[open + 1..].char_indices() {
        let at = open + 1 + i;

        match c {
            '(' | '[' | '{' | '<' => depth += 1,

            '>' if prev == '-' => {}

            ')' if depth == 0 => {
                push(start, at);

                break;
            }

            ')' | ']' | '}' | '>' => depth -= 1,

            ',' if depth == 0 => {
                push(start, at);
                start = at + 1;
            }

            _ => {}
        }

        prev = c;
    }

    entries
}

/// The child's "Prefix 'x' with '_'" where the `unused_variable` lint
/// offers "Rewrite as `_x`" already: two entries for one change.
fn drop_child_prefix_fixes(actions: &mut Vec<Value>) {
    let title = |a: &Value| {
        a.get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let ours: Vec<String> = actions.iter().map(title).collect();

    actions.retain(|a| {
        let t = title(a);
        let Some(name) = t
            .strip_prefix("Prefix '")
            .and_then(|r| r.strip_suffix("' with '_' to silence"))
        else {
            return true;
        };
        let rewrite = format!("Rewrite as `_{name}`");

        !ours.iter().any(|o| o.starts_with(&rewrite))
    });
}

/// `function Item:cost(self: Item): number` names `self` twice: the `:`
/// already takes it, as a call with `:` passes it. The signature drops
/// the parameter, the way the declaration `function Item:cost()` reads.
fn colon_without_self(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some(rest) = line.strip_prefix("function ") else {
                return line.to_string();
            };
            let Some(open) = rest.find('(') else {
                return line.to_string();
            };

            if !rest[..open].contains(':') {
                return line.to_string();
            }

            let args = &rest[open + 1..];
            let after_self = args
                .strip_prefix("self")
                .filter(|a| a.starts_with([':', ',', ')']));
            let Some(after) = after_self else {
                return line.to_string();
            };
            // Past the type of `self`: the next `,` or `)` at depth 0.
            let mut depth = 0i32;
            let mut cut = None;

            for (i, c) in after.char_indices() {
                match c {
                    '(' | '{' | '<' | '[' => depth += 1,

                    ')' | '}' | '>' | ']' if depth > 0 => depth -= 1,

                    ',' | ')' if depth == 0 => {
                        cut = Some((i, c));
                        break;
                    }

                    _ => {}
                }
            }

            match cut {
                Some((i, ',')) => {
                    format!("function {}({}", &rest[..open], after[i + 1..].trim_start())
                }

                Some((i, _)) => format!("function {}({}", &rest[..open], &after[i..]),

                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if text.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod signature_tests {
    use serde_json::json;

    /// The folds shorten the label; the parameters follow it.
    #[test]
    fn parameter_offsets_follow_a_shortened_label() {
        let long = "function area(s: {tag: \"Circle\"} | {tag: \"Dot\"}, n: number): number";
        let mut help = json!({ "signatures": [{
            "label": long,
            "parameters": [{ "label": [14, 47] }, { "label": [49, 58] }],
        }] });
        super::parameter_labels_as_text(&mut help);

        assert_eq!(
            help["signatures"][0]["parameters"][0]["label"],
            "s: {tag: \"Circle\"} | {tag: \"Dot\"}"
        );

        help["signatures"][0]["label"] = json!("function area(s: Shape, n: number): number");
        help["signatures"][0]["parameters"][0]["label"] = json!("s: Shape");
        super::parameter_labels_as_offsets(&mut help);

        assert_eq!(
            help["signatures"][0]["parameters"][0]["label"],
            json!([14, 22])
        );
        assert_eq!(
            help["signatures"][0]["parameters"][1]["label"],
            json!([24, 33])
        );
    }

    /// The type of `self` holds the words of the parameters after it;
    /// each parameter marks its own entry of the list.
    #[test]
    fn a_parameter_marks_its_own_entry_past_the_self_type() {
        let label = "function Signal:Fire(self: Signal<number, string>, number, string): ()";
        let mut help = json!({ "signatures": [{
            "label": label,
            "parameters": [{ "label": "number" }, { "label": "string" }],
        }] });
        super::parameter_labels_as_offsets(&mut help);

        let marked = |i: usize| {
            let span = &help["signatures"][0]["parameters"][i]["label"];
            let (s, e) = (span[0].as_u64().unwrap(), span[1].as_u64().unwrap());

            (s, &label[s as usize..e as usize])
        };

        assert_eq!(marked(0), (51, "number"));
        assert_eq!(marked(1), (59, "string"));

        // A function type in the list: its `->` closes no bracket.
        let label = "function f(g: (number) -> Map<string, number>, n: number): ()";
        let mut help = json!({ "signatures": [{
            "label": label,
            "parameters": [{ "label": "g: (number) -> Map<string, number>" }, { "label": "n: number" }],
        }] });
        super::parameter_labels_as_offsets(&mut help);

        let n = label.find("n: number").unwrap();
        assert_eq!(
            help["signatures"][0]["parameters"][1]["label"],
            json!([n, n + "n: number".len()])
        );
    }
}

#[cfg(test)]
mod colon_tests {
    #[test]
    fn a_colon_signature_names_self_once() {
        let clean = super::colon_without_self;

        assert_eq!(
            clean("```alloy\nfunction Item:cost(self: Item): number\n```"),
            "```alloy\nfunction Item:cost(): number\n```"
        );
        assert_eq!(
            clean("function Item:scale(self: Item, by: number): number"),
            "function Item:scale(by: number): number"
        );
        assert_eq!(
            clean("function Box<T>:map(self: Box<T>, f: (T) -> T): Box<T>"),
            "function Box<T>:map(f: (T) -> T): Box<T>"
        );
        // A `.` function takes `self` as an argument, and keeps it.
        assert_eq!(
            clean("function Item.cost(self: Item): number"),
            "function Item.cost(self: Item): number"
        );
    }
}
