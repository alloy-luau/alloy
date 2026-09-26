//! Documents: open, change, and close; the shadow and mirror a document keeps in the child's workspace; the workspace scan and its watcher.

use super::capabilities::map_range_value;
use super::navigation::{data_module_of, data_source_of};
use super::*;

/// How long an edit waits before the files that import it compile
/// again. A reader types faster than this, so one pass covers a burst
/// of keystrokes. See `Server::schedule_import_refresh`.
const IMPORT_REFRESH_WAIT: std::time::Duration = std::time::Duration::from_millis(400);

impl State {
    pub(crate) fn fresh_id(&mut self) -> String {
        self.next_id += 1;

        format!("alloy:{}", self.next_id)
    }

    /// The mirror path of a real path: the same place under the mirror,
    /// with an Alloy extension swapped to Luau. A path a few folders
    /// above the root keeps its place above the mirror, so a require
    /// into another project resolves there; one further out goes under
    /// `_outside`.
    pub(crate) fn mirror_path(&self, real: &Path) -> PathBuf {
        let real = normalize(real);
        let root = self.root.as_deref().map(normalize);
        // A climb that leaves the mirror's own directory is no place
        // for a file: a mirror set by hand has no folders above it.
        let climbed = root
            .as_deref()
            .and_then(|r| climb(r, &real, above_of(&self.mirror)))
            .filter(|rel| normalize(&self.mirror.join(rel)).starts_with(mirror_base(&self.mirror)));
        let rel = match climbed {
            Some(rel) => rel,

            None => {
                let mut p = PathBuf::from("_outside");

                for c in real.components() {
                    match c {
                        // Windows: `C:` becomes one folder name, so the
                        // drive survives the trip through the mirror
                        // and `real_path` writes it back.
                        std::path::Component::Prefix(prefix) => {
                            let text = prefix.as_os_str().to_string_lossy();
                            let text = text.trim_end_matches([':', '/', '\\']);

                            if !text.is_empty() {
                                p.push(text);
                            }
                        }

                        std::path::Component::Normal(n) => p.push(n),

                        _ => {}
                    }
                }

                p
            }
        };
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let swapped = if let Some(b) = name.strip_suffix(".d.aly") {
            format!("{b}.d.luau")
        } else if let Some(b) = name
            .strip_suffix(".aly")
            .or_else(|| name.strip_suffix(".alx"))
        {
            format!("{b}.luau")
        } else {
            name
        };

        normalize(&self.mirror.join(rel.with_file_name(swapped)))
    }

    /// The real path of a mirror path, for a plain file. An Alloy file
    /// resolves through `shadows` first, since its extension changed.
    pub(crate) fn real_path(&self, mirror: &Path) -> Option<PathBuf> {
        if let Ok(rel) = mirror.strip_prefix(&self.mirror)
            && let Ok(outside) = rel.strip_prefix("_outside")
        {
            return Some(outside_path(outside));
        }

        if !mirror.starts_with(mirror_base(&self.mirror)) {
            return None;
        }

        let rel = climb(&self.mirror, mirror, above_of(&self.mirror))?;

        Some(normalize(&self.root.as_deref()?.join(rel)))
    }

    /// The URI the child sees for a real URI.
    pub(crate) fn child_uri(&self, real: &str) -> String {
        match uri_to_path(real) {
            Some(path) => path_to_uri(&self.mirror_path(&path)),

            None => real.to_string(),
        }
    }

    /// The URI the editor sees for a child URI, and whether it names an
    /// Alloy document.
    pub(crate) fn editor_uri(&self, child: &str) -> (String, bool) {
        if let Some(source) = self.shadows.get(child) {
            return (source.clone(), true);
        }

        let real = uri_to_path(child)
            .and_then(|p| self.real_path(&p))
            .map(|p| path_to_uri(&data_source_of(p)))
            .unwrap_or_else(|| child.to_string());

        (real, false)
    }

    /// A real path as a message shows it: relative to the root when it
    /// is under it, else as it is.
    pub(crate) fn friendly_path(&self, path: &Path) -> String {
        let path = normalize(path);
        let shown = match self.root.as_deref().map(normalize) {
            Some(root) => path
                .strip_prefix(&root)
                .map(Path::to_path_buf)
                .unwrap_or(path),

            None => path,
        };

        shown.to_string_lossy().replace('\\', "/")
    }

    /// Writes the runtime into the mirror under `dir` once, so the
    /// `require` of a file there resolves for the child.
    pub(crate) fn ensure_runtime(&self, dir: &Path) {
        let real = dir.join("alloy.luau");

        if self.runtimes.borrow_mut().insert(real.clone()) {
            self.write_mirror(&real, alloy::RUNTIME);
        }
    }

    /// Writes a mirror file, creating its directories. A data file also
    /// writes the module the build makes of it, `x.json` as `x.luau`.
    pub(crate) fn write_mirror(&self, real: &Path, text: &str) {
        let target = self.mirror_path(real);

        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if std::fs::read_to_string(&target).ok().as_deref() != Some(text) {
            let _ = std::fs::write(&target, text);
        }

        if let Some(format) = alloy::data::Format::of_path(real)
            && !alloy::data::is_project_file(real)
        {
            self.mirror_data(real, text, format);
        }
    }

    /// The module of a data file, into the mirror. A module of the same
    /// stem beside it wins, as it does in the build. A document that
    /// does not parse keeps the last good module: a half-typed edit
    /// would drop every type at once.
    pub(crate) fn mirror_data(&self, real: &Path, text: &str, format: alloy::data::Format) {
        if alloy::data::module_beside(real).is_some() {
            return;
        }

        if let Ok(luau) = alloy::data::convert(text, format) {
            self.write_mirror(&real.with_extension("luau"), &luau);
        }
    }

    pub(crate) fn remove_mirror(&self, real: &Path) {
        let _ = std::fs::remove_file(self.mirror_path(real));

        if alloy::data::Format::of_path(real).is_some()
            && alloy::data::module_beside(real).is_none()
        {
            let _ = std::fs::remove_file(self.mirror_path(&real.with_extension("luau")));
        }
    }
}

impl Server {
    /// Forwards a message whose URIs name real files: each becomes its
    /// mirror URI.
    pub(crate) fn forward_plain(&self, mut message: Value) {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        map_uris_into_mirror(&mut message, &st);
        drop(st);
        self.to_child(&message);
    }

    /// A plain Luau document changed in the editor: the mirror copy
    /// follows the text.
    pub(crate) fn plain_changed(&self, uri: &str, whole: Option<String>, changes: &[Value]) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut text = whole
            .or_else(|| st.plain.get(uri).cloned())
            .unwrap_or_default();

        for change in changes {
            let piece = change
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let range = change.get("range").and_then(range_of);
            crate::doc::apply_change(&mut text, range, piece);
        }

        let module = uri_to_path(uri).and_then(|path| {
            st.write_mirror(&path, &text);

            data_module_of(&path)
        });

        st.plain.insert(uri.to_string(), text);
        drop(st);

        // The child re-reads the module once it hears the module changed.
        if let Some(module) = module {
            self.forward_plain(json!({
                "jsonrpc": "2.0",
                "method": "workspace/didChangeWatchedFiles",
                "params": { "changes": [{ "uri": path_to_uri(&module), "type": 2 }] }
            }));

            if let Some(path) = uri_to_path(uri) {
                self.refresh_dependents(&path);
            }
        }
    }

    /// Resends every open document that names a data file, so the child
    /// checks it against the module it just re-read. A dirty dependency
    /// alone leaves the next completion on the document empty.
    pub(crate) fn refresh_dependents(&self, data: &Path) {
        let data = normalize(data);
        let messages: Vec<Value> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            st.docs
                .iter()
                .filter_map(|(uri, doc)| {
                    let path = uri_to_path(uri)?;
                    let dir = path.parent()?;
                    let names = alloy::data::references(&doc.source)
                        .iter()
                        .any(|r| normalize(&imports::lexical(dir, &r.path)) == data);

                    if !names || !child_sees(uri) {
                        return None;
                    }

                    Some(json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didChange",
                        "params": {
                            "textDocument": { "uri": st.child_uri(uri), "version": doc.version },
                            "contentChanges": [{ "text": doc.shadow }]
                        }
                    }))
                })
                .collect()
        };

        for message in messages {
            self.to_child(&message);
        }
    }

    /// The compile options of one document: the project's, and the
    /// import maps the file's own text asks for. A value import of a
    /// struct or an enum binds its type too, and the maps say which.
    #[allow(clippy::type_complexity)]
    pub(crate) fn compile_options(
        &self,
        uri: &str,
        text: &str,
    ) -> (
        EmitOptions,
        alloy::luaux::Config,
        Option<std::sync::Arc<alloy::ingot::Ingots>>,
    ) {
        let (mut options, jsx, ingots) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let (o, j) = st.options_for(uri);

            (o, j, st.ingots.clone())
        };

        if let Some(path) = uri_to_path(uri) {
            options = options.imports_for_file(&path, text);
        }

        (options, jsx, ingots)
    }

    /// Opens or replaces a document and its shadow.
    pub(crate) fn open_doc(&self, uri: &str, text: String, version: i64, by_editor: bool) {
        let had_exports = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            match st.docs.get(uri) {
                Some(d) => export_surface(d),

                None => Default::default(),
            }
        };
        let (options, jsx, ingots) = self.compile_options(uri, &text);
        let doc = Doc::new(text, version, &options, &jsx, ingots.as_deref());
        let fresh_exports = export_surface(&doc);
        let (shadow, existed) = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let shadow = st.child_uri(uri);

            // The editor's buffer wins over the disk. The workspace
            // pass reads a file the editor opened while the pass ran,
            // and its text is one version behind: the child takes no
            // change that goes backwards, so it would keep the text it
            // has and never hear the next edit.
            if !by_editor && st.editor_open.contains(uri) {
                return;
            }

            if by_editor {
                st.editor_open.insert(uri.to_string());
            }

            if let Some(path) = uri_to_path(uri) {
                // The reader's text stands in front of the disk: a
                // file that imports this one reads what the editor
                // holds, not the last save.
                alloy::modules::set_open_source(&path, Some(&doc.source));
                st.write_mirror(&path, &doc.shadow);
            }

            let existed = st.docs.contains_key(uri);
            st.shadows.insert(shadow.clone(), uri.to_string());
            st.docs.insert(uri.to_string(), doc);

            (shadow, existed)
        };

        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let doc = &st.docs[uri];

        let message = if existed {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": shadow, "version": doc.version },
                    "contentChanges": [{ "text": doc.shadow }]
                }
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": shadow,
                        "languageId": "luau",
                        "version": doc.version,
                        "text": doc.shadow
                    }
                }
            })
        };

        drop(st);

        if child_sees(uri) {
            self.to_child(&message);
        }

        // A pass over the workspace opens every file; one refresh at
        // the end of it costs one recompile each, not one per file.
        if self.scan.try_lock().is_ok()
            && had_exports != fresh_exports
            && let Some(path) = uri_to_path(uri)
        {
            self.refresh_importers(&[path]);
        }

        self.publish(uri);
    }

    /// Compiles one document again and gives the child the fresh
    /// shadow. The compile runs outside the state lock: it is the slow
    /// part, and a hover waits for the same lock.
    ///
    /// The child keeps what it read of a module until a document that
    /// requires it changes, so a stale shadow sent again teaches it
    /// nothing. The compile is what makes the text new.
    pub(crate) fn resend_doc(&self, uri: &str) {
        let Some((source, version)) = ({
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            st.docs.get(uri).map(|d| (d.source.clone(), d.version))
        }) else {
            return;
        };
        let (options, jsx, ingots) = self.compile_options(uri, &source);
        let fresh = Doc::new(source.clone(), version, &options, &jsx, ingots.as_deref());
        let message = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let shadow = st.child_uri(uri);

            // The editor typed while this compile ran: its own
            // recompile is the one to keep.
            if st
                .docs
                .get(uri)
                .is_some_and(|d| d.source != source || d.version != version)
            {
                return;
            }

            let text = fresh.shadow.clone();
            st.docs.insert(uri.to_string(), fresh);

            if let Some(path) = uri_to_path(uri) {
                st.write_mirror(&path, &text);
            }

            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": shadow, "version": version },
                    "contentChanges": [{ "text": text }]
                }
            })
        };

        if child_sees(uri) {
            self.to_child(&message);
        }
    }

    pub(crate) fn change_doc(&self, uri: &str, version: i64, changes: &[Value]) {
        let (mut options, jsx) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .options_for(uri);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ingots = st.ingots.clone();

        let Some(doc) = st.docs.get_mut(uri) else {
            drop(st);

            if let Some(text) = changes
                .last()
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
            {
                self.open_doc(uri, text.to_string(), version, true);
            }

            return;
        };

        let had_impls = impl_surface(&doc.source);

        for change in changes {
            let text = change
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let range = change.get("range").and_then(range_of);
            doc.apply_change(range, text);
        }

        doc.version = version;

        if let Some(path) = uri_to_path(uri) {
            alloy::modules::set_open_source(&path, Some(&doc.source));
            options = options.imports_for_file(&path, &doc.source);
        }

        let had_exports = export_surface(doc);
        doc.compile(&options, &jsx, ingots.as_deref());
        let fresh_exports = export_surface(doc);
        let fresh_impls = impl_surface(&doc.source);
        let source = doc.source.clone();
        let shadow_text = doc.shadow.clone();
        let shadow = st.child_uri(uri);

        // The project's `impl` index holds this file's blocks, so the
        // next compile of any file reads them again.
        if had_impls != fresh_impls {
            st.project.borrow_mut().take();
        }

        if let Some(path) = uri_to_path(uri) {
            st.write_mirror(&path, &shadow_text);
        }

        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": shadow, "version": version },
                "contentChanges": [{ "text": shadow_text }]
            }
        });
        drop(st);

        if child_sees(uri) {
            self.to_child(&message);
        }

        if had_exports != fresh_exports
            && let Some(path) = uri_to_path(uri)
        {
            self.refresh_importers(&[path]);
        }

        // An `impl` here declares methods on a struct another file
        // owns. That file's check artifact carries them, so its shadow
        // follows the edit.
        if had_impls != fresh_impls
            && let Some(path) = uri_to_path(uri)
        {
            self.refresh_imported(&path, &source);
        }

        self.publish(uri);
    }

    /// Resends every open document this one imports.
    pub(crate) fn refresh_imported(&self, path: &Path, source: &str) {
        for target in alloy::modules::import_targets_for_file(path, source) {
            let uri = path_to_uri(&normalize(&target));

            if !self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .docs
                .contains_key(&uri)
            {
                continue;
            }

            self.wait_for_requests();
            self.resend_doc(&uri);
            self.publish(&uri);
        }
    }

    pub(crate) fn close_shadow(&self, uri: &str) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let existed = st.docs.remove(uri).is_some();
        let shadow = st.child_uri(uri);
        st.shadows.remove(&shadow);
        st.child_diagnostics.remove(uri);
        st.published.remove(uri);
        // The document is gone, so no editor holds it. A name left in
        // the set stands for the whole session, and a file written at
        // that path again would get no shadow: the pass reads the set
        // and leaves a file the editor holds to the editor.
        st.editor_open.remove(uri);

        if let Some(path) = uri_to_path(uri) {
            // The file is gone, and so is the text the editor held.
            alloy::modules::set_open_source(&path, None);
            st.remove_mirror(&path);
        }

        drop(st);

        if !existed {
            return;
        }

        self.to_child(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": shadow } }
        }));
        self.to_client(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": [] }
        }));

        // The disk no longer holds the file. The state forgets what it
        // read, which drops the project's `impl` index with it, and
        // every open file that imports the file compiles again: the
        // import now names no module, and the editor sends no edit.
        if let Some(path) = uri_to_path(uri).filter(|p| !p.exists()) {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .forget_disk();
            self.refresh_importers(&[path]);
        }
    }

    /// The whole pass: the mirror the child reads, then a shadow for
    /// every source. The file poll runs it again when the project
    /// changes on disk.
    pub(crate) fn open_workspace(&self) {
        let files = self.open_mirror();
        self.open_shadows(files);
    }

    /// The mirror the child reads: its Luau configuration, a copy of
    /// every plain file, the project's sourcemap, and the runtime. The
    /// pass compiles nothing, so it takes milliseconds, and the child
    /// must hold all of it before it sees the first document: it
    /// resolves a require against the files that are there. Returns
    /// the Alloy sources the walk found, for the shadow pass.
    pub(crate) fn open_mirror(&self) -> Vec<PathBuf> {
        let started = std::time::Instant::now();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget_disk();
        self.load_ingots();
        self.publish_alias_problems();
        let root = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .root
            .clone();
        let Some(root) = root else {
            return Vec::new();
        };

        // One pass at a time: the file poll and the editor both start
        // one, and a document opened twice reaches the child twice.
        let _pass = self.scan.lock().unwrap_or_else(|e| e.into_inner());

        let config =
            Config::find_within(&root, &root).and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let (input, out) = match &config {
            Some((p, c)) => {
                let base = p.parent().unwrap_or(&root).to_path_buf();

                (base.join(&c.build.input), Some(base.join(&c.build.out)))
            }

            None => (root.clone(), None),
        };
        let config = config.map(|(_, c)| c);

        let mut files = Vec::new();
        let mut plain = Vec::new();
        walk(&root, out.as_deref(), &mut files, &mut plain);

        // An input outside the root, `in = "../examples"`, gets its
        // shadows too, or a require between its files finds nothing.
        let input = normalize(&input);

        if !input.starts_with(normalize(&root)) && input.is_dir() {
            walk(&input, out.as_deref(), &mut files, &mut plain);
        }

        // A project an import leads into gets its shadows too: the
        // importer's require points into it, and the child resolves a
        // require against the files that are there.
        walk_dependencies(&root, config.as_ref(), &mut files, &mut plain);
        // So does an alias folder outside the root, `pkg = "../Packages"`.
        // The compiler resolves through it, so the mirror holds it too,
        // or the child reports a require the build accepts. A target
        // above the root is skipped: walking it walks the root again.
        let root_n = normalize(&root);

        for (_, target) in project_aliases(&root, Some(&root)) {
            let target = normalize(&target);

            if !target.starts_with(&root_n) && !root_n.starts_with(&target) && target.is_dir() {
                walk(&target, out.as_deref(), &mut files, &mut plain);
            }
        }
        files.sort();
        files.dedup();
        log::took(&format!("scan: walked {} sources", files.len()), started);
        let mirrored = std::time::Instant::now();

        // Plain files copy into the mirror, so requires to them resolve.
        // The root's Luau configuration sets strict mode when it sets no
        // mode, and a root with no configuration gets one: strict is
        // the default.
        {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            for path in plain {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let text =
                        mirror_text(&path, &root, config.as_ref(), &input, out.as_deref(), text);
                    st.write_mirror(&path, &text);

                    // `alloy build` writes `sourcemap.json` at the root.
                    // A root that still holds the `.alloy/sourcemap.json`
                    // an older build wrote uses that one.
                    if path == root.join(".alloy/sourcemap.json")
                        && !root.join("sourcemap.json").is_file()
                    {
                        st.write_mirror(&root.join("sourcemap.json"), &text);
                    }
                }
            }

            // The mirror's own `.luaurc`. A mirrored `.config.luau` goes,
            // so the merged file is the one read.
            st.write_mirror(
                &root.join(".luaurc"),
                &mirror_luau_text(&root, config.as_ref()),
            );
            let _ = std::fs::remove_file(st.mirror.join(".config.luau"));

            // The tree writes the mirror's sourcemap, as `alloy build`
            // writes the project's. A file added since the last build is
            // in this one, so `@game/` completes and types without one.
            if let Some(config) = &config
                && tree_sourcemap(&root, Some(config))
                && let Some(text) = alloy::project::luau_sourcemap(&root, config, None)
            {
                st.write_mirror(
                    &root.join("sourcemap.json"),
                    &mirrored_sourcemap(&text, &input, out.as_deref(), &root),
                );
            }
        }

        let runtime = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // The runtime goes where the build puts it, the place the
            // mirror's `alloy` alias names.
            let real = normalize(
                &out.clone()
                    .unwrap_or_else(|| root.clone())
                    .join("alloy.luau"),
            );
            st.runtimes.borrow_mut().insert(real.clone());
            st.write_mirror(&real, alloy::RUNTIME);
            let uri = st.child_uri(&path_to_uri(&real));
            st.runtime_uri = Some(uri.clone());

            uri
        };
        self.to_child(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": runtime,
                    "languageId": "luau",
                    "version": 0,
                    "text": alloy::RUNTIME
                }
            }
        }));
        log::took("scan: mirrored the plain files", mirrored);
        log::took("workspace mirror written", started);

        files
    }

    /// A shadow for every source of the project. The pass compiles each
    /// file, so it runs off the request thread.
    pub(crate) fn open_shadows(&self, files: Vec<PathBuf>) {
        let shadows = std::time::Instant::now();

        // One pass at a time, the way `open_mirror` holds it.
        let _pass = self.scan.lock().unwrap_or_else(|e| e.into_inner());
        let held_before = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor_open
            .clone();

        for path in dependencies_first(files) {
            if self.stopping.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            self.wait_for_requests();
            let uri = path_to_uri(&path);
            let already = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .docs
                .contains_key(&uri);

            if already {
                continue;
            }

            if let Ok(text) = std::fs::read_to_string(&path) {
                self.open_doc(&uri, text, 0, false);
            }
        }

        // The child checked a file the editor opened during the pass
        // against a mirror that still lacked some shadows. The mirror
        // is whole now, so the child checks it again.
        let opened: Vec<String> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            st.editor_open.difference(&held_before).cloned().collect()
        };

        for uri in opened {
            self.wait_for_requests();
            self.resend_doc(&uri);
            self.publish(&uri);
        }

        log::took("workspace shadows opened", shadows);
    }

    /// Polls the project's folders and re-reads what changed. Not every
    /// editor sends `workspace/didChangeWatchedFiles`, and a package
    /// install writes its files outside the editor either way, so the poll
    /// is the floor under the watcher. `ALLOY_LSP_POLL_SECS` sets the
    /// interval; the default is 15 seconds.
    pub fn poll_files(&self) {
        let secs = std::env::var("ALLOY_LSP_POLL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(15);
        let mut stamp: Option<(usize, Option<std::time::SystemTime>)> = None;

        loop {
            std::thread::sleep(std::time::Duration::from_secs(secs));

            if self.stopping.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            let roots = self.poll_roots();

            if roots.is_empty() {
                continue;
            }

            let now = tree_stamp(&roots);
            // A pass is already running: it reads the same tree, so
            // this tick has nothing to add.
            let idle = self.scan.try_lock().is_ok();
            log::trace(&format!(
                "poll: {} files, changed {}, idle {idle}",
                now.0,
                stamp != Some(now)
            ));

            // The first pass rescans too: a package install between the
            // startup pass and this one would otherwise be the baseline and
            // never read. A rescan that finds nothing new costs one walk.
            if stamp != Some(now) && idle {
                stamp = Some(now);
                self.rescan_workspace();
            }
        }
    }

    /// The folders a file poll watches: `[build] in`, every folder the
    /// mount table and the Luau configuration name, the `[build] in` of
    /// every project the root's sources import into, and the workspace
    /// root's own files. The output folder stays out: the build writes
    /// there, and a poll of it would answer its own writes. Each tick
    /// reads the list again, so an import added since follows.
    pub(crate) fn poll_roots(&self) -> Vec<PathBuf> {
        let root = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .root
            .clone();
        let Some(root) = root else {
            return Vec::new();
        };
        let mut roots = vec![root.clone()];

        if let Some(path) = Config::find_within(&root, &root)
            && let Ok(config) = Config::load(&path)
        {
            let base = path.parent().unwrap_or(&root).to_path_buf();
            roots.push(base.join(&config.build.input));

            for m in config.mount.values() {
                roots.push(base.join(&m.0));
            }

            roots.extend(alloy::build::dependency_inputs(&base, &config));
        }

        for (_, target) in project_aliases(&root, Some(&root)) {
            roots.push(target);
        }

        roots.sort();
        roots.dedup();
        // A folder inside another is already walked by it.
        let all = roots.clone();
        roots.retain(|r| !all.iter().any(|o| o != r && r.starts_with(o)));
        roots
    }

    /// Re-reads the project from disk. A package install writes a whole
    /// tree at once: the new files reach the mirror, the child hears
    /// that each plain module changed, and every document that imports
    /// one is sent again, or its import keeps the type it had.
    pub(crate) fn rescan_workspace(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget_disk();
        let root = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .root
            .clone();
        let Some(root) = root else {
            return;
        };
        let found =
            Config::find_within(&root, &root).and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let (input, out) = match &found {
            Some((p, c)) => {
                let base = p.parent().unwrap_or(&root).to_path_buf();

                (
                    normalize(&base.join(&c.build.input)),
                    Some(base.join(&c.build.out)),
                )
            }

            None => (normalize(&root), None),
        };
        let config = found.map(|(_, c)| c);
        let mut files = Vec::new();
        let mut plain = Vec::new();
        walk(&root, out.as_deref(), &mut files, &mut plain);
        walk_dependencies(&root, config.as_ref(), &mut files, &mut plain);

        // A source the editor does not hold open, saved on disk since
        // its shadow: a dependency edited in another window. The shadow
        // follows the disk, the way a watched change makes it.
        let stale: Vec<(String, String)> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            files
                .iter()
                .filter_map(|path| {
                    let uri = path_to_uri(path);
                    let doc = st.docs.get(&uri)?;

                    if st.editor_open.contains(&uri) {
                        return None;
                    }

                    let text = std::fs::read_to_string(path).ok()?;

                    (text != doc.source).then_some((uri, text))
                })
                .collect()
        };

        // What the mirror does not already hold, letter for letter. The
        // compare reads the text the pass writes, not the file itself:
        // the pass rewrites some files on the way in, and a compare
        // against the file reads those as changed on every tick.
        let changed: Vec<PathBuf> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let from_tree = tree_sourcemap(&root, config.as_ref());

            plain
                .into_iter()
                .filter(|path| {
                    if path.parent() == Some(root.as_path()) {
                        match path.file_name().and_then(|n| n.to_str()) {
                            // The mirror holds no `.config.luau`: its
                            // `.luaurc` carries the merged
                            // configuration, so that file is the one
                            // an edit here has to move.
                            Some(".config.luau") => {
                                return std::fs::read_to_string(
                                    st.mirror_path(&root.join(".luaurc")),
                                )
                                .ok()
                                .as_deref()
                                    != Some(mirror_luau_text(&root, config.as_ref()).as_str());
                            }

                            // The tree writes the mirror's sourcemap,
                            // so the file the last build left says
                            // nothing about it.
                            Some("sourcemap.json") if from_tree => return false,

                            _ => {}
                        }
                    }

                    let Ok(text) = std::fs::read_to_string(path) else {
                        return true;
                    };
                    let want =
                        mirror_text(path, &root, config.as_ref(), &input, out.as_deref(), text);

                    std::fs::read_to_string(st.mirror_path(path))
                        .ok()
                        .as_deref()
                        != Some(want.as_str())
                })
                .collect()
        };
        let fresh: Vec<PathBuf> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            files
                .into_iter()
                .filter(|path| !st.docs.contains_key(&path_to_uri(path)))
                .collect()
        };

        if changed.is_empty() && fresh.is_empty() && stale.is_empty() {
            return;
        }

        log::info(&format!(
            "rescan: {} plain files, {} shadows, {} saved sources",
            changed.len(),
            fresh.len(),
            stale.len()
        ));
        self.open_workspace();

        for (uri, text) in &stale {
            self.open_doc(uri, text.clone(), 0, false);
        }

        // The child caches a module it has read; it re-reads one it
        // hears about.
        let notice: Vec<Value> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut list: Vec<Value> = Vec::new();

            for path in &changed {
                list.push(json!({ "uri": path_to_uri(path), "type": 2 }));

                if let Some(module) = data_module_of(path) {
                    list.push(json!({ "uri": path_to_uri(&module), "type": 2 }));
                }
            }

            drop(st);
            list
        };

        if !notice.is_empty() {
            self.forward_plain(json!({
                "jsonrpc": "2.0",
                "method": "workspace/didChangeWatchedFiles",
                "params": { "changes": notice }
            }));
        }

        let mut touched = changed;
        touched.extend(fresh);
        touched.extend(stale.iter().filter_map(|(uri, _)| uri_to_path(uri)));
        self.refresh_importers(&touched);
    }

    /// Resends every document whose imports name one of these files, so
    /// the child reads the module again and the document types against
    /// it. A new module alone leaves the import as it was typed.
    ///
    /// The match reads the module path a spec names, not the file the
    /// spec finds on disk. A deleted file finds nothing, and its
    /// importers are the documents that have to hear about it.
    pub(crate) fn refresh_importers(&self, changed: &[PathBuf]) {
        let changed: Vec<PathBuf> = changed
            .iter()
            .map(|p| imports::module_path(&normalize(p)))
            .collect();
        let importers: Vec<String> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            st.docs
                .iter()
                .filter(|(uri, doc)| {
                    child_sees(uri)
                        && imports::imported_specs(&doc.source).iter().any(|spec| {
                            st.resolve_spec(uri, spec).is_some_and(|target| {
                                changed.contains(&imports::module_path(&normalize(&target)))
                            })
                        })
                })
                .map(|(uri, _)| uri.clone())
                .collect()
        };

        for uri in importers {
            self.wait_for_requests();
            self.resend_doc(&uri);
            self.publish(&uri);
        }
    }

    /*
    Compiles the files that import this one again, once the edits stop.

    Every index an importer holds reads the text of the module it
    imports, so an edit reaches the importer only when the importer
    compiles again. One pass per keystroke would be one compile per
    importing file, so the pass waits for a quiet moment. A save runs
    the same pass at once.
    */
    pub(crate) fn schedule_import_refresh(self: &Arc<Self>, path: PathBuf) {
        let alone = {
            let mut edited = self.edited.lock().unwrap_or_else(|e| e.into_inner());
            let alone = edited.is_empty();
            edited.insert(path, std::time::Instant::now());

            alone
        };

        // One thread waits for every edited file. A rename across the
        // workspace edits a hundred files in one burst; a thread each
        // is a hundred threads and a hundred passes over the documents.
        if !alone {
            return;
        }

        let server = Arc::clone(self);

        alloy_syntax::parser::spawn_deep(move || {
            loop {
                std::thread::sleep(IMPORT_REFRESH_WAIT);

                // The files nobody has typed in since the last tick.
                // They leave the map here, so a file edited again
                // during the pass waits for the next one.
                let quiet: Vec<PathBuf> = {
                    let mut edited = server.edited.lock().unwrap_or_else(|e| e.into_inner());
                    let ready: Vec<PathBuf> = edited
                        .iter()
                        .filter(|(_, last)| last.elapsed() >= IMPORT_REFRESH_WAIT)
                        .map(|(p, _)| p.clone())
                        .collect();

                    for path in &ready {
                        edited.remove(path);
                    }

                    ready
                };

                if server.stopping.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }

                if !quiet.is_empty() {
                    server.refresh_importers(&quiet);
                }

                // The map is read again under its own lock: an edit
                // that arrives now finds it empty and starts a thread.
                if server
                    .edited
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty()
                {
                    return;
                }
            }
        });
    }

    /// Asks the editor to report changes to every file the project
    /// reads: a source, a plain Luau module, a data file, and the Luau
    /// configuration. A package install writes a whole tree at once, and
    /// the child types an import of a module it never read as `any`.
    /// An editor that takes no dynamic registration is polled instead.
    pub(crate) fn watch_project_files(&self) {
        if !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watch_registration
        {
            return;
        }

        let id = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let id = st.fresh_id();
            st.asked.insert(id.clone(), Asked::Watch);

            id
        };
        self.to_client(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "alloy-project-files",
                    "method": "workspace/didChangeWatchedFiles",
                    "registerOptions": {
                        "watchers": [
                            { "globPattern": "**/*.{aly,alx,luau,lua,json,toml,luaurc}" },
                            { "globPattern": "**/.luaurc" },
                            { "globPattern": "**/.config.luau" }
                        ]
                    }
                }]
            }
        }));
    }

    /// Files moved: the shadows follow at once, and the imports that
    /// named the old paths follow after the editor's answer.
    pub(crate) fn renamed(&self, files: &[Value]) {
        let mut renames = Vec::new();

        for f in files {
            let old = f
                .get("oldUri")
                .and_then(Value::as_str)
                .and_then(uri_to_path);
            let new = f
                .get("newUri")
                .and_then(Value::as_str)
                .and_then(uri_to_path);

            if let (Some(old), Some(new)) = (old, new) {
                renames.push(Rename { old, new });
            }
        }

        if renames.is_empty() {
            return;
        }

        // The edits come from the texts as they were before the move.
        let docs: Vec<(String, PathBuf, String)> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.docs
                .iter()
                .filter_map(|(uri, doc)| {
                    uri_to_path(uri).map(|p| (uri.clone(), p, doc.source.clone()))
                })
                .collect()
        };
        let root = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .root
            .clone();
        let changes = imports::rename_edits(&docs, &renames, &|dir| {
            project_aliases(dir, root.as_deref())
        });

        // Shadows move: the old one closes, the new one opens from disk
        // or from the text we hold.
        for (uri, path, source) in &docs {
            let moved = imports_map(path, &renames);

            if moved == *path {
                continue;
            }

            let text = std::fs::read_to_string(&moved).unwrap_or_else(|_| source.clone());
            let open = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .editor_open
                .contains(uri);
            self.close_shadow(uri);
            let new_uri = path_to_uri(&moved);
            self.open_doc(&new_uri, text, 0, open);
        }

        if changes.is_empty() {
            return;
        }

        let count: usize = changes.values().map(Vec::len).sum();
        let edit = json!({ "changes": changes });
        let id = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let id = st.fresh_id();
            st.asked.insert(id.clone(), Asked::Rename(edit));

            id
        };
        self.to_client(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "window/showMessageRequest",
            "params": {
                "type": 3,
                "message": format!("Update {count} import path(s) for the moved file(s)?"),
                "actions": [{ "title": UPDATE_IMPORTS }, { "title": "Leave" }]
            }
        }));
    }
}

/// The answer that applies the rename edit.
pub(crate) const UPDATE_IMPORTS: &str = "Update imports";

pub(crate) fn imports_map(path: &Path, renames: &[Rename]) -> PathBuf {
    for r in renames {
        if path == r.old {
            return r.new.clone();
        }

        if let Ok(rest) = path.strip_prefix(&r.old) {
            return r.new.join(rest);
        }
    }

    path.to_path_buf()
}

impl State {
    /// The `[build] in` directory of the project, the one tree whose
    /// modules an author writes by a relative path.
    pub(crate) fn input_dir(&self) -> Option<PathBuf> {
        let root = self.root.as_deref()?;
        let path = Config::find_within(root, root)?;
        let config = Config::load(&path).ok()?;
        let base = path.parent().unwrap_or(Path::new("."));

        Some(config_dir(base, &config.build.input.to_string_lossy()))
    }

    /// The `[mount]` table as instance path to directory, longest
    /// instance path first, so a nested mount wins over its parent.
    pub(crate) fn instance_mounts(&self) -> Vec<(String, PathBuf)> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        let Some(path) = Config::find_within(root, root) else {
            return Vec::new();
        };
        let Ok(config) = Config::load(&path) else {
            return Vec::new();
        };
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut out: Vec<(String, PathBuf)> = config
            .mount
            .values()
            .filter_map(|m| {
                let instance = m.1.strip_prefix("@game/")?.replace('/', ".");

                Some((instance, config_dir(&base, &m.0)))
            })
            .collect();
        out.sort_by_key(|(i, _)| std::cmp::Reverse(i.len()));

        out
    }
}

/// Alloy files into `out`; the plain files a require or the child's
/// configuration can reach into `plain`.
/// The newest change under the roots: how many files there are and the
/// latest modification time. Both move on any write, add, or delete.
pub(crate) fn tree_stamp(roots: &[PathBuf]) -> (usize, Option<std::time::SystemTime>) {
    fn walk_stamp(dir: &Path, count: &mut usize, newest: &mut Option<std::time::SystemTime>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            if path.is_dir() {
                if matches!(name.as_str(), "node_modules" | "target")
                    || (name.starts_with('.') && !matches!(name.as_str(), ".alloy" | ".ember"))
                {
                    continue;
                }

                walk_stamp(&path, count, newest);
            } else if let Ok(meta) = entry.metadata()
                && let Ok(m) = meta.modified()
            {
                *count += 1;

                if newest.is_none_or(|n| m > n) {
                    *newest = Some(m);
                }
            }
        }
    }

    let mut count = 0;
    let mut newest = None;

    for root in roots {
        walk_stamp(root, &mut count, &mut newest);
    }

    (count, newest)
}

pub(crate) fn walk(
    dir: &Path,
    skip: Option<&Path>,
    out: &mut Vec<PathBuf>,
    plain: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        if path.is_dir() {
            // A dot directory holds tooling state, `.lest` or the test
            // modules, not sources; `.alloy` keeps the build's sourcemap,
            // and `.ember` holds the packages a `packages/` stub requires.
            if matches!(name.as_str(), "node_modules" | "target")
                || (name.starts_with('.') && !matches!(name.as_str(), ".alloy" | ".ember"))
                || Some(path.as_path()) == skip
            {
                continue;
            }

            walk(&path, skip, out, plain);
        } else if name.ends_with(".aly") || name.ends_with(".alx") {
            out.push(path);
        } else if [".luau", ".lua", ".json", ".toml", ".luaurc"]
            .iter()
            .any(|ext| name.ends_with(ext))
            || name == ".luaurc"
        {
            plain.push(path);
        }
    }
}

/// A path with its `.` and `..` components folded, so `crates/../examples`
/// and `examples` name one place.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }

            other => out.push(other),
        }
    }

    out
}

/*
The sources in the order the shadow pass opens them: each file after the
files it imports, in path order otherwise.

The child checks a shadow when it opens, and it resolves each require
against the mirror as it stands then. A require to a shadow the pass has
not written yet fails, and the child records no dependency for it. The
later open of that shadow then does not check the importer again, so
the importer keeps the failed types until its own text changes. In path
order, `server/Plot` opens before the `shared/index` it imports.
*/
pub(crate) fn dependencies_first(files: Vec<PathBuf>) -> Vec<PathBuf> {
    let known: HashSet<PathBuf> = files.iter().map(|p| normalize(p)).collect();
    let imports_of = |path: &Path| -> Vec<PathBuf> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };

        alloy::modules::import_targets_for_file(path, &text)
            .into_iter()
            .map(|t| normalize(&t))
            .filter(|t| known.contains(t))
            .collect()
    };
    let mut out = Vec::with_capacity(files.len());
    let mut seen: HashSet<PathBuf> = HashSet::new();

    // A depth-first walk with its own stack: an import chain can be
    // longer than the thread's stack allows for recursion.
    for file in files {
        let file = normalize(&file);

        if !seen.insert(file.clone()) {
            continue;
        }

        let mut stack = vec![(file.clone(), imports_of(&file))];

        while let Some((path, pending)) = stack.last_mut() {
            match pending.pop() {
                Some(next) => {
                    if seen.insert(next.clone()) {
                        let deps = imports_of(&next);
                        stack.push((next, deps));
                    }
                }

                None => {
                    out.push(path.clone());
                    stack.pop();
                }
            }
        }
    }

    out
}

/// Walks the `[build] in` of every project the root's sources import
/// into, each with its own output folder left out.
fn walk_dependencies(
    root: &Path,
    config: Option<&Config>,
    files: &mut Vec<PathBuf>,
    plain: &mut Vec<PathBuf>,
) {
    for dep in config
        .map(|c| alloy::build::dependency_inputs(root, c))
        .unwrap_or_default()
    {
        let dep_out = Config::find(&dep).and_then(|p| {
            let c = Config::load(&p).ok()?;

            Some(p.parent()?.join(&c.build.out))
        });
        walk(&dep, dep_out.as_deref(), files, plain);
    }
}

/// One workspace root as a directory name. Two servers run at once, one
/// per project, and neither may write where the other reads.
pub(crate) fn root_key(root: Option<&Path>) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default()
        .hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

/// The fewest folders above the root the mirror keeps, and the most: a
/// dependency at `../../shared` stays inside the mirror's own directory,
/// and one further out than `ABOVE_MAX` falls under `_outside`, where
/// no require reaches it.
const ABOVE: usize = 4;
const ABOVE_MAX: usize = 8;

/// How many folders above the root the mirror keeps: `ABOVE`, or as
/// many as the deepest project the root's sources import into needs,
/// up to `ABOVE_MAX`. The child takes the mirror as its root at
/// initialize, so the count is fixed there.
pub(crate) fn mirror_above(root: Option<&Path>) -> usize {
    let Some(root) = root else {
        return ABOVE;
    };
    let deepest = Config::find_within(root, root)
        .and_then(|p| Config::load(&p).ok().map(|c| (p, c)))
        .and_then(|(p, c)| {
            let base = normalize(p.parent().unwrap_or(root));

            alloy::build::dependency_inputs(&base, &c)
                .iter()
                .filter_map(|dep| ups(&base, &normalize(dep)))
                .max()
        })
        .unwrap_or(0);

    deepest.clamp(ABOVE, ABOVE_MAX)
}

/// The folder that holds the mirror of every root. `ALLOY_LSP_MIRRORS`
/// moves it, so a test run keeps its mirrors in a folder it removes.
fn mirror_parent() -> PathBuf {
    std::env::var_os("ALLOY_LSP_MIRRORS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("alloy-lsp"))
}

/// The folder beside the mirrors that holds each root's definitions.
const DEFINITIONS: &str = "definitions";

/// The folder of one root's patched definitions. It sits under the
/// mirrors' folder, so `ALLOY_LSP_MIRRORS` moves it too. It is no part
/// of the mirror: `initialize` empties the mirror, and the child reads
/// the definitions after that.
pub fn definitions_dir(root: Option<&Path>) -> PathBuf {
    mirror_parent().join(DEFINITIONS).join(root_key(root))
}

/// The definitions folder of the root a mirror belongs to.
pub(crate) fn definitions_of(mirror: &Path) -> PathBuf {
    let base = mirror_base(mirror);
    let parent = base.parent().unwrap_or(base);

    parent
        .join(DEFINITIONS)
        .join(base.file_name().unwrap_or_default())
}

pub(crate) fn mirror_dir(root: Option<&Path>, above: usize) -> PathBuf {
    let mut dir = mirror_parent().join(root_key(root));

    for _ in 0..above {
        dir.push("up");
    }

    dir.join("root")
}

/// The file in the base of a mirror that names the server that uses it.
const OWNER: &str = "server.pid";

/// Writes the pid of this server into its mirror. The purge of another
/// server then keeps the mirror while this server runs.
pub(crate) fn claim_mirror(mirror: &Path) {
    let _ = std::fs::write(
        mirror_base(mirror).join(OWNER),
        std::process::id().to_string(),
    );
}

/// Removes the mirrors of other roots that no server touched for a day
/// and no live server owns. A server that was killed leaves its mirror
/// behind, and a test or a probe that opens many roots leaves one each.
/// Only a folder named like a root key goes, so a parent set by hand
/// loses nothing else.
///
/// The definitions of a root go by the same rule, and the owner of the
/// mirror of that root owns them. They grew to 7 GB in 9199 folders.
pub(crate) fn purge_stale_mirrors(mirror: &Path) {
    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let own = mirror_base(mirror).to_path_buf();
    let Some(parent) = own.parent() else {
        return;
    };
    let keyed = |n: &std::ffi::OsStr| {
        n.to_str()
            .is_some_and(|n| n.len() == 16 && n.chars().all(|c| c.is_ascii_hexdigit()))
    };
    let mut folders = vec![parent.to_path_buf(), parent.join(DEFINITIONS)];
    // The folder an older server wrote the definitions to. Its keys are
    // the keys of the default mirrors, so only a default parent reads it.
    let legacy = std::env::temp_dir().join("alloy-lsp-definitions");

    if parent == std::env::temp_dir().join("alloy-lsp") {
        folders.push(legacy.clone());
    }

    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();

            if Some(name.as_os_str()) == own.file_name() || !keyed(&name) {
                continue;
            }

            if age(&path).is_some_and(|a| a > DAY) && !owner_alive(&parent.join(&name)) {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }

    // `remove_dir` removes an empty folder only.
    let _ = std::fs::remove_dir(&legacy);
}

/// How long ago a file or a folder last changed.
fn age(path: &Path) -> Option<std::time::Duration> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;

    std::time::SystemTime::now().duration_since(modified).ok()
}

/// Whether the server in the pid file of a mirror still runs. Linux
/// lists each live process under `/proc`. Other systems give std no
/// such check, so there a pid file younger than a week counts as live.
fn owner_alive(base: &Path) -> bool {
    const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let file = base.join(OWNER);

    if cfg!(target_os = "linux") {
        return std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .is_some_and(|pid| Path::new("/proc").join(pid.to_string()).exists());
    }

    age(&file).is_some_and(|a| a < WEEK)
}

/// How many folders above its root a mirror keeps: the `up` folders
/// in its path. A mirror set by hand has none.
fn above_of(mirror: &Path) -> usize {
    mirror
        .ancestors()
        .skip(1)
        .take_while(|a| a.file_name().is_some_and(|n| n == "up"))
        .count()
}

/// The directory that holds the whole mirror, the folders above the
/// root included.
pub(crate) fn mirror_base(mirror: &Path) -> &Path {
    mirror
        .ancestors()
        .nth(above_of(mirror) + 1)
        .unwrap_or(mirror)
}

/// How many folders `base` climbs to the one it shares with `path`;
/// `None` when they share nothing, as on another drive.
fn ups(base: &Path, path: &Path) -> Option<usize> {
    let base: Vec<_> = base.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = base.iter().zip(&path).take_while(|(a, b)| a == b).count();

    (common > 0).then(|| base.len() - common)
}

/// `path` relative to the folder `base`, with at most `above` leading
/// `..`; `None` further out, or on another drive.
fn climb(base: &Path, path: &Path, above: usize) -> Option<PathBuf> {
    let up = ups(base, path).filter(|n| *n <= above)?;
    let base: Vec<_> = base.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = base.len() - up;
    let mut out = PathBuf::new();

    for _ in 0..up {
        out.push("..");
    }

    for c in &path[common..] {
        out.push(c);
    }

    Some(out)
}

/// Moves every URI in a message from the workspace into the mirror.
pub(crate) fn map_uris_into_mirror(value: &mut Value, st: &State) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                match (key.as_str(), v) {
                    ("uri" | "targetUri" | "oldUri" | "newUri", Value::String(uri)) => {
                        *uri = st.child_uri(uri);
                    }

                    (_, other) => map_uris_into_mirror(other, st),
                }
            }
        }

        Value::Array(items) => {
            for item in items {
                map_uris_into_mirror(item, st);
            }
        }

        _ => {}
    }
}

pub(crate) fn map_into_shadow(params: &mut Value, doc: &Doc) {
    match params {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "position" => {
                        if let Some((l, c)) = position_of_value(value) {
                            let (l, c) = doc.to_shadow(l, c);
                            *value = json!({ "line": l, "character": c });
                        }
                    }

                    "range" => {
                        if let Some(((sl, sc), (el, ec))) = range_of(value) {
                            let (sl, sc) = doc.to_shadow(sl, sc);
                            let (el, ec) = doc.to_shadow(el, ec);
                            *value = range_value((sl, sc), (el, ec));
                        }
                    }

                    _ => map_into_shadow(value, doc),
                }
            }
        }

        Value::Array(items) => {
            for item in items {
                map_into_shadow(item, doc);
            }
        }

        _ => {}
    }
}

/// Maps URIs and ranges in a result or notification back to sources.
/// `ctx` is the source URI ranges belong to until a `uri` key says
/// otherwise; a URI that is not a shadow clears it.
pub(crate) fn map_from_shadow(value: &mut Value, ctx: Option<&str>, st: &State) {
    match value {
        Value::Object(map) => {
            // A place in the merged definitions file: the `.d.aly` it
            // came from, on that file's own lines.
            for (key, ranges) in [
                ("uri", &["range"][..]),
                ("targetUri", &["targetRange", "targetSelectionRange"][..]),
            ] {
                let line = ranges
                    .iter()
                    .find_map(|r| map.get(*r)?.pointer("/start/line")?.as_u64());
                let place = map
                    .get(key)
                    .and_then(Value::as_str)
                    .zip(line)
                    .and_then(|(uri, line)| st.declared_at(uri, line as usize));

                if let Some((source, first)) = place {
                    map.insert(key.to_string(), json!(source));

                    for r in ranges {
                        if let Some(range) = map.get_mut(*r) {
                            shift_lines(range, first);
                        }
                    }
                }
            }

            let mut here: Option<String> = ctx.map(str::to_string);

            if let Some(Value::String(uri)) = map.get_mut("uri") {
                let (real, is_alloy) = st.editor_uri(uri);
                *uri = real.clone();
                here = is_alloy.then_some(real);
            }

            let mut target: Option<String> = None;

            if let Some(Value::String(uri)) = map.get_mut("targetUri") {
                let (real, is_alloy) = st.editor_uri(uri);
                *uri = real.clone();
                target = is_alloy.then_some(real);
            }

            // A workspace edit keys changes by URI.
            if let Some(Value::Object(changes)) = map.get_mut("changes") {
                let mut rebuilt = Map::new();

                for (uri, mut edits) in std::mem::take(changes) {
                    let (real, is_alloy) = st.editor_uri(&uri);
                    map_from_shadow(&mut edits, is_alloy.then_some(real.as_str()), st);
                    rebuilt.insert(real, edits);
                }

                *changes = rebuilt;
            }

            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "changes" | "uri" | "targetUri" => {}

                    "targetRange" | "targetSelectionRange" => {
                        if let Some(doc) = target.as_deref().and_then(|u| st.docs.get(u)) {
                            map_range_value(value, doc);
                        }
                    }

                    "range" | "selectionRange" | "originSelectionRange" | "insert" | "replace" => {
                        if let Some(doc) = here.as_deref().and_then(|u| st.docs.get(u)) {
                            map_range_value(value, doc);
                        }
                    }

                    "position" => {
                        if let Some(doc) = here.as_deref().and_then(|u| st.docs.get(u))
                            && let Some((l, c)) = position_of_value(value)
                        {
                            let (l, c) = doc.to_source(l, c);
                            *value = json!({ "line": l, "character": c });
                        }
                    }

                    _ => map_from_shadow(value, here.as_deref(), st),
                }
            }
        }

        Value::Array(items) => {
            for item in items {
                map_from_shadow(item, ctx, st);
            }
        }

        _ => {}
    }
}

/*
A child refactor can send a command that follows its edit: "Extract to
local variable" asks for `luau-lsp.rename` on the new name. The
arguments name the shadow, at a place in the text after the edit. The
place sits in text an edit inserts, and that text lands in the source
as it is, so the place moves with the edit. A command with a place in
older text goes: its arguments would name a file of the mirror.
*/
pub(crate) fn map_follow_up(action: &mut Value, st: &State) {
    let Some(args) = action
        .pointer("/command/arguments")
        .and_then(Value::as_array)
    else {
        return;
    };
    let Some((i, shadow)) = args.iter().enumerate().find_map(|(i, a)| {
        a.as_str()
            .filter(|u| st.shadows.contains_key(*u))
            .map(|u| (i, u.to_string()))
    }) else {
        return;
    };
    let moved = st.shadows.get(&shadow).and_then(|source| {
        let doc = st.docs.get(source)?;
        let (line, character) = args.get(i + 1).and_then(position_of_value)?;
        let edit = action.get("edit")?;
        let mut edits: Vec<&Value> = edit
            .get("changes")
            .and_then(|c| c.get(&shadow))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();

        for change in edit
            .get("documentChanges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if change.pointer("/textDocument/uri").and_then(Value::as_str) == Some(&shadow) {
                edits.extend(
                    change
                        .get("edits")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                );
            }
        }

        let at = inserted_place(&edits, doc, (line, character))?;

        Some((source.clone(), at))
    });

    match (moved, action.pointer_mut("/command/arguments")) {
        (Some((source, (l, c))), Some(Value::Array(args))) => {
            args[i] = json!(source);
            args[i + 1] = json!({ "line": l, "character": c });

            // luau-lsp's extension owns `luau-lsp.rename`, and it
            // registers the command only once it starts on a Luau file.
            // The Alloy extension registers its own copy.
            if let Some(command) = action.pointer_mut("/command/command")
                && command == "luau-lsp.rename"
            {
                *command = json!("alloy-luau.rename");
            }
        }

        _ => {
            if let Some(action) = action.as_object_mut() {
                action.remove("command");
            }
        }
    }
}

/// Where a place after the edits lands in the source, when it sits in
/// text an edit inserts. The edits go in order, and each one moves the
/// lines below it by the lines it adds, on each side. `None` when the
/// place is in text that was there before.
fn inserted_place(
    edits: &[&Value],
    doc: &Doc,
    (line, character): (u32, u32),
) -> Option<(u32, u32)> {
    let mut edits: Vec<_> = edits
        .iter()
        .filter_map(|e| Some((range_of(e.get("range")?)?, e.get("newText")?.as_str()?)))
        .collect();
    edits.sort_by_key(|((start, _), _)| *start);
    let (mut shadow_shift, mut source_shift) = (0i64, 0i64);

    for ((start, end), text) in edits {
        let mut mapped = range_value(start, end);
        map_range_value(&mut mapped, doc);
        let (to, to_end) = range_of(&mapped)?;
        let lines: Vec<&str> = text.split('\n').collect();
        let k = i64::from(line) - (i64::from(start.0) + shadow_shift);

        if let Some(piece) = usize::try_from(k).ok().and_then(|k| lines.get(k)) {
            let from = if k == 0 { start.1 } else { 0 };
            let width = piece.encode_utf16().count() as u32;

            if (from..=from + width).contains(&character) {
                let l = i64::from(to.0) + source_shift + k;
                let c = if k == 0 {
                    to.1 + character - start.1
                } else {
                    character
                };

                return Some((u32::try_from(l).ok()?, c));
            }
        }

        let added = lines.len() as i64 - 1;
        shadow_shift += added - i64::from(end.0 - start.0);
        source_shift += added - i64::from(to_end.0 - to.0);
    }

    None
}

/// Moves both ends of a range up by `by` lines.
pub(crate) fn shift_lines(range: &mut Value, by: usize) {
    for end in ["start", "end"] {
        if let Some(line) = range.pointer_mut(&format!("/{end}/line"))
            && let Some(n) = line.as_u64()
        {
            *line = json!(n.saturating_sub(by as u64));
        }
    }
}

/// A sourcemap on its way into the mirror: every `.aly` and `.alx`
/// path becomes the mirror's `.luau`, and the runtime under the output
/// root becomes the mirror's copy under the input root.
pub(crate) fn mirrored_sourcemap(
    text: &str,
    input: &Path,
    out: Option<&Path>,
    root: &Path,
) -> String {
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let runtime_out = out.map(|o| rel(&o.join("alloy.luau")));
    let runtime_in = rel(&input.join("alloy.luau"));

    alloy::project::map_sourcemap(text, &|s| {
        if runtime_out.as_deref() == Some(s) {
            return runtime_in.clone();
        }

        alloy::project::luau_script_path(s)
    })
}

/// The text the workspace pass writes into the mirror for a plain
/// file: the mirror's own Luau configuration for the root's `.luaurc`,
/// a sourcemap whose paths name the shadows, and any other file as it
/// is, with `strict` added where the root's configuration names no
/// mode.
///
/// The rescan compares the mirror against this, not against the file
/// on disk. A compare against the file reads every one of these as
/// changed on every tick, and the pass then runs on every tick: it
/// tells the child that its configuration and its sourcemap moved, and
/// the child drops the types it had.
pub(crate) fn mirror_text(
    path: &Path,
    root: &Path,
    config: Option<&Config>,
    input: &Path,
    out: Option<&Path>,
    text: String,
) -> String {
    if path.parent() == Some(root) && path.file_name().is_some_and(|n| n == ".luaurc") {
        return mirror_luau_text(root, config);
    }

    let text = strict_config(path, root, text);

    match path.file_name().is_some_and(|n| n == "sourcemap.json") {
        true => mirrored_sourcemap(&text, input, out, root),

        false => text,
    }
}

/// Whether the pass writes the mirror's sourcemap from the tree. The
/// mirror's copy then follows the tree, and the file the last build
/// left at the root says nothing about it.
pub(crate) fn tree_sourcemap(root: &Path, config: Option<&Config>) -> bool {
    config.is_some_and(|c| !alloy::project::Tree::load(root, c).mounts.is_empty())
}

/// A Luau configuration file on its way into the mirror. One at the
/// workspace root that sets no language mode gets `strict`; any other
/// file copies as it is.
pub(crate) fn strict_config(path: &Path, root: &Path, text: String) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if path.parent() != Some(root) {
        return text;
    }

    match name {
        ".luaurc" => {
            let Ok(mut json) = serde_json::from_str::<Value>(&text) else {
                return text;
            };

            if let Some(map) = json.as_object_mut()
                && !map.contains_key("languageMode")
            {
                map.insert("languageMode".into(), Value::String("strict".into()));

                return serde_json::to_string_pretty(&json).unwrap_or(text);
            }

            text
        }

        ".config.luau" => {
            let parsed = alloy::luau_config::parse_config_luau(&text);

            if parsed.is_some_and(|c| c.language_mode.is_none())
                && let Some(i) = text.find("luau")
                && let Some(brace) = text[i..].find('{')
            {
                let at = i + brace + 1;

                return format!("{} languagemode = \"strict\",{}", &text[..at], &text[at..]);
            }

            text
        }

        _ => text,
    }
}

/// `to` as a require path relative to the folder `from`, both
/// absolute: `..` for each folder of `from` past the common part, then
/// the rest of `to`, and `./` when nothing climbs.
pub(crate) fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out: Vec<String> = vec!["..".to_string(); from.len() - common];

    if out.is_empty() {
        out.push(".".to_string());
    }

    out.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );

    out.join("/")
}

/// The real path a mirror `_outside` folder holds. On Windows the
/// first segment is the drive `mirror_path` wrote, `C`, which becomes
/// `C:\`; on every other platform the path is absolute from the root.
pub(crate) fn outside_path(rel: &Path) -> PathBuf {
    if cfg!(windows) {
        let mut parts = rel.components();

        if let Some(std::path::Component::Normal(first)) = parts.next() {
            let drive = first.to_string_lossy().into_owned();

            if drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic()) {
                return PathBuf::from(format!("{drive}:/")).join(parts.as_path());
            }
        }
    }

    Path::new("/").join(rel)
}

/// A path a configuration file writes, as a directory.
///
/// `alloy.toml` and `.luaurc` are written by hand on every platform, so
/// a mount or an alias may read `packages\\roblox` or `~/shared`. Both
/// spellings resolve here; nothing else in the server sees them.
pub(crate) fn config_dir(base: &Path, text: &str) -> PathBuf {
    config_dir_from(base, text, home_dir().as_deref())
}

pub(crate) fn config_dir_from(base: &Path, text: &str, home: Option<&Path>) -> PathBuf {
    let text = text.replace('\\', "/");

    if text == "~" || text.starts_with("~/") {
        let rest = text.strip_prefix("~/").unwrap_or("");

        if let Some(home) = home {
            return imports::lexical(home, rest);
        }
    }

    imports::lexical(base, &text)
}

/// The home directory, for a path a configuration writes with `~`.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The alias the shadow requires the runtime by. The mirror's Luau
/// configuration points it at the file the mirror holds, so a shadow
/// resolves the runtime on disk, with no sourcemap and no build.
/// What one document lends the files that import it: each name it
/// sends out and whether that name is the default. A change here moves
/// the reports of every importer, and the import checks read the module
/// from disk, so an importer's report stands until something asks for
/// it again.
/// What a document adds to the project's `impl` index: the methods its
/// `impl` blocks put on a struct or an enum another file declares, and
/// which of them it keeps private. The index is stale when this
/// changes. A source with no `impl` word adds nothing, and the test
/// costs a scan, not a parse.
pub(crate) fn impl_surface(
    source: &str,
) -> (
    Vec<alloy::extensions::Extension>,
    Vec<(String, Vec<String>)>,
) {
    if !source.contains("impl") {
        return Default::default();
    }

    let impls = alloy::extensions::project_impls(&[source.to_string()]);

    (impls.methods, impls.privates)
}

pub(crate) fn export_surface(doc: &Doc) -> Vec<(String, bool)> {
    doc.exports
        .iter()
        .map(|e| (e.name.clone(), e.is_default))
        .collect()
}

pub(crate) const RUNTIME_ALIAS: &str = "@alloy";

/// The Luau configuration the mirror gets for a project root: the
/// root's own file, strict when it names no mode, the `[mount]` names
/// it lacks while `[project] mount_aliases` stays on, and `alloy` at
/// the place the mirror keeps the runtime. The child reads this file,
/// so a shadow resolves `@pkg/x` and `@alloy` the way the compiler
/// resolves them. The user's own file is never written.
pub(crate) fn mirror_luau_text(root: &Path, config: Option<&Config>) -> String {
    let mut luau = alloy::luau_config::read_dir(root)
        .map(|(_, c)| c)
        .unwrap_or_default();

    if luau.language_mode.is_none() {
        luau.language_mode = Some("strict".to_string());
    }

    if let Some(config) = config
        && config.project.mount_aliases
    {
        for (name, m) in &config.mount {
            if !luau.aliases.iter().any(|(a, _)| a == name) {
                luau.aliases
                    .push((name.clone(), format!("./{}", m.0.replace('\\', "/"))));
            }
        }
    }

    // The mirror holds no output tree, so an `alloy` alias the user
    // wrote points at nothing here. The runtime is written under
    // `[build] out`, and this alias names it there.
    let target = match config {
        Some(c) => format!(
            "./{}/alloy",
            c.build.out.to_string_lossy().replace('\\', "/")
        ),

        None => "./alloy".to_string(),
    };

    match luau
        .aliases
        .iter_mut()
        .find(|(a, _)| a == RUNTIME_ALIAS.trim_start_matches('@'))
    {
        Some((_, t)) => *t = target,

        None => luau
            .aliases
            .push((RUNTIME_ALIAS.trim_start_matches('@').to_string(), target)),
    }

    alloy::luau_config::render_luaurc(&luau)
}

/// The aliases a module path can use from `dir`: the Luau
/// configuration above it, and the `[mount]` table of the nearest
/// `alloy.toml` while `[project] mount_aliases` stays on. A name the
/// Luau configuration declares wins over a mount of that name.
pub(crate) fn project_aliases(dir: &Path, root: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = luaurc_aliases(dir, root);
    let found = match root {
        Some(r) => Config::find_within(dir, r),

        None => Config::find(dir),
    };

    if let Some(path) = found
        && let Ok(config) = Config::load(&path)
        && config.project.mount_aliases
    {
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        for (name, m) in &config.mount {
            if !out.iter().any(|(a, _)| a == name) {
                out.push((name.clone(), config_dir(&base, &m.0)));
            }
        }
    }

    out.sort();
    out
}

/// The child's `require.directoryAliases` for the mount names the Luau
/// configuration lacks, while `[project] mount_aliases` stays on. The
/// child resolves `@pkg/x` in a shadow through this setting, so the
/// user's own configuration file is never written. Paths are relative
/// to the child's workspace, which mirrors the root.
pub(crate) fn mount_alias_settings(root: Option<&Path>) -> Value {
    let Some(root) = root else {
        return json!({});
    };
    let Some(path) = Config::find_within(root, root) else {
        return json!({});
    };
    let Ok(config) = Config::load(&path) else {
        return json!({});
    };

    if !config.project.mount_aliases || config.mount.is_empty() {
        return json!({});
    }

    let declared = luaurc_aliases(root, Some(root));
    let mut map = serde_json::Map::new();

    for (name, m) in &config.mount {
        if declared.iter().any(|(a, _)| a == name) {
            continue;
        }

        let dir = m.0.replace('\\', "/");
        let dir = dir.trim_end_matches('/');
        map.insert(format!("@{name}/"), Value::String(format!("{dir}/")));
    }

    if map.is_empty() {
        return json!({});
    }

    json!({ "require": { "directoryAliases": map } })
}

/// The `aliases` of the nearest `.luaurc` or `.config.luau` above
/// `dir`, up to the root, each resolved against the directory that
/// declares it.
pub(crate) fn luaurc_aliases(dir: &Path, root: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut cur = Some(dir.to_path_buf());

    while let Some(d) = cur {
        if let Some((_, config)) = alloy::luau_config::read_dir(&d) {
            let mut out: Vec<(String, PathBuf)> = config
                .aliases
                .iter()
                .map(|(k, p)| (k.clone(), config_dir(&d, p)))
                .collect();
            out.sort();

            return out;
        }

        if root.is_some_and(|r| r == d) {
            break;
        }

        cur = d.parent().map(Path::to_path_buf);
    }

    Vec::new()
}
