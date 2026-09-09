//! Documents: open, change, and close; the shadow and mirror a document keeps in the child's workspace; the workspace scan and its watcher.

use super::capabilities::map_range_value;
use super::navigation::{data_module_of, data_source_of};
use super::*;

impl State {
    pub(crate) fn fresh_id(&mut self) -> String {
        self.next_id += 1;

        format!("alloy:{}", self.next_id)
    }

    /// The mirror path of a real path: the same place under the mirror,
    /// with an Alloy extension swapped to Luau. A path outside the root
    /// goes under `_outside`.
    pub(crate) fn mirror_path(&self, real: &Path) -> PathBuf {
        let real = normalize(real);
        let root = self.root.as_deref().map(normalize);
        let rel = match root.as_deref().and_then(|r| real.strip_prefix(r).ok()) {
            Some(rel) => rel.to_path_buf(),

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

        self.mirror.join(rel.with_file_name(swapped))
    }

    /// The real path of a mirror path, for a plain file. An Alloy file
    /// resolves through `shadows` first, since its extension changed.
    pub(crate) fn real_path(&self, mirror: &Path) -> Option<PathBuf> {
        let rel = mirror.strip_prefix(&self.mirror).ok()?;

        if let Ok(outside) = rel.strip_prefix("_outside") {
            return Some(outside_path(outside));
        }

        Some(self.root.as_deref()?.join(rel))
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
        let st = self.state.lock().expect("state");
        map_uris_into_mirror(&mut message, &st);
        drop(st);
        self.to_child(&message);
    }

    /// A plain Luau document changed in the editor: the mirror copy
    /// follows the text.
    pub(crate) fn plain_changed(&self, uri: &str, whole: Option<String>, changes: &[Value]) {
        let mut st = self.state.lock().expect("state");
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
            let st = self.state.lock().expect("state");

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

    /// The module a script's globals move into, written into the
    /// mirror beside the script. The script's shadow requires it, so
    /// the child needs the file to be there.
    pub(crate) fn write_hoisted(&self, uri: &str, source: &str) {
        let Some(path) = uri_to_path(uri) else {
            return;
        };

        if !alloy::modules::is_script(&path.to_string_lossy()) {
            return;
        }

        let (options, jsx) = {
            let st = self.state.lock().expect("state");
            let (mut o, j) = st.options_for(uri);
            let rel = st.project_rel(uri).unwrap_or_default();
            let module_rel = PathBuf::from(alloy::globals::hoist_name(&rel.to_string_lossy()));
            o.globals =
                alloy::globals::refs_for(&st.project_globals(), &module_rel, &HashMap::new());
            o.hoist_globals = false;

            (o, j)
        };
        let name = alloy::globals::hoist_name(
            &path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let module_path = path.with_file_name(name);
        let module_src = alloy::globals::hoisted_module(&alloy::globals::index_text(&path, source));
        let mut options = options;
        options.file_name = module_path.to_string_lossy().into_owned();
        let text = match alloy::compile_file(
            &options.file_name,
            &module_src,
            &options,
            Some(&jsx),
            None,
        ) {
            Ok(out) => out.check,

            Err(_) => return,
        };
        self.state
            .lock()
            .expect("state")
            .write_mirror(&module_path, &text);
    }

    /// Opens or replaces a document and its shadow.
    pub(crate) fn open_doc(&self, uri: &str, text: String, version: i64, by_editor: bool) {
        let (mut options, jsx, ingots, had_globals) = {
            let st = self.state.lock().expect("state");
            let (o, j) = st.options_for(uri);
            let had = st.docs.get(uri).map(|d| global_names(&d.globals));

            (o, j, st.ingots.clone(), had.unwrap_or_default())
        };

        // A value import of a struct or an enum binds its type too.
        if let Some(path) = uri_to_path(uri) {
            options.import_types = alloy::modules::import_types_for_file(&path, &text);
            options.import_enums = alloy::modules::import_enums_for_file(&path, &text);
            options.import_privates = alloy::modules::import_privates_for_file(&path, &text);
            options.import_result_asyncs =
                alloy::modules::import_result_asyncs_for_file(&path, &text);
            options.import_trait_defaults =
                alloy::modules::import_trait_defaults_for_file(&path, &text);
            options.plain_modules = alloy::modules::plain_modules_for_file(&path, &text);
        }

        let doc = Doc::new(text, version, &options, &jsx, ingots.as_deref());
        let source = doc.source.clone();
        let fresh_globals = global_names(&doc.globals);
        let (shadow, existed) = {
            let mut st = self.state.lock().expect("state");
            let shadow = st.child_uri(uri);

            if by_editor {
                st.editor_open.insert(uri.to_string());
            }

            if let Some(path) = uri_to_path(uri) {
                st.write_mirror(&path, &doc.shadow);
            }

            let existed = st.docs.contains_key(uri);
            st.shadows.insert(shadow.clone(), uri.to_string());
            st.docs.insert(uri.to_string(), doc);

            (shadow, existed)
        };

        let st = self.state.lock().expect("state");
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

        self.write_hoisted(uri, &source);

        if had_globals != fresh_globals {
            self.refresh_globals(uri);
        }

        self.publish(uri);
    }

    /// Sends every other document to the child again. A global the
    /// workspace gained, lost, or renamed changes what each file binds
    /// on its first line, so every other shadow is stale.
    pub(crate) fn refresh_globals(&self, except: &str) {
        let uris: Vec<String> = {
            let st = self.state.lock().expect("state");

            st.docs.keys().filter(|u| *u != except).cloned().collect()
        };

        for uri in uris {
            let (options, jsx, ingots) = {
                let st = self.state.lock().expect("state");
                let (o, j) = st.options_for(&uri);

                (o, j, st.ingots.clone())
            };
            let message = {
                let mut st = self.state.lock().expect("state");
                let shadow = st.child_uri(&uri);
                let Some(doc) = st.docs.get_mut(&uri) else {
                    continue;
                };
                doc.compile(&options, &jsx, ingots.as_deref());
                let text = doc.shadow.clone();
                let version = doc.version;

                if let Some(path) = uri_to_path(&uri) {
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

            if child_sees(&uri) {
                self.to_child(&message);
            }
        }
    }

    pub(crate) fn change_doc(&self, uri: &str, version: i64, changes: &[Value]) {
        let (mut options, jsx) = self.state.lock().expect("state").options_for(uri);
        let mut st = self.state.lock().expect("state");
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
            options.import_types = alloy::modules::import_types_for_file(&path, &doc.source);
            options.import_enums = alloy::modules::import_enums_for_file(&path, &doc.source);
            options.import_privates = alloy::modules::import_privates_for_file(&path, &doc.source);
            options.import_result_asyncs =
                alloy::modules::import_result_asyncs_for_file(&path, &doc.source);
            options.import_trait_defaults =
                alloy::modules::import_trait_defaults_for_file(&path, &doc.source);
            options.plain_modules = alloy::modules::plain_modules_for_file(&path, &doc.source);
        }

        let had_globals = global_names(&doc.globals);
        doc.compile(&options, &jsx, ingots.as_deref());
        let fresh_globals = global_names(&doc.globals);
        let source = doc.source.clone();
        let shadow_text = doc.shadow.clone();
        let shadow = st.child_uri(uri);

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

        self.write_hoisted(uri, &source);

        if had_globals != fresh_globals {
            self.refresh_globals(uri);
        }

        self.publish(uri);
    }

    pub(crate) fn close_shadow(&self, uri: &str) {
        let mut st = self.state.lock().expect("state");
        let existed = st.docs.remove(uri).is_some();
        let shadow = st.child_uri(uri);
        st.shadows.remove(&shadow);
        st.child_diagnostics.remove(uri);

        if let Some(path) = uri_to_path(uri) {
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
    }

    pub(crate) fn open_workspace(&self) {
        self.load_ingots();
        let root = self.state.lock().expect("state").root.clone();
        let Some(root) = root else {
            return;
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

        files.sort();
        files.dedup();

        // Plain files copy into the mirror, so requires to them resolve.
        // The root's Luau configuration sets strict mode when it sets no
        // mode, and a root with no configuration gets one: strict is
        // the default.
        {
            let st = self.state.lock().expect("state");

            for path in plain {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let text = strict_config(&path, &root, text);
                    let text = if path.file_name().is_some_and(|n| n == "sourcemap.json") {
                        mirrored_sourcemap(&text, &input, out.as_deref(), &root)
                    } else {
                        text
                    };
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
            if let Some(config) = &config {
                let tree = alloy::project::Tree::load(&root, config);

                if !tree.mounts.is_empty()
                    && let Ok(map) = alloy::project::sourcemap(&tree, &root)
                {
                    let text = serde_json::to_string_pretty(&map).unwrap_or_default() + "\n";
                    st.write_mirror(
                        &root.join("sourcemap.json"),
                        &mirrored_sourcemap(&text, &input, out.as_deref(), &root),
                    );
                }
            }
        }

        for path in files {
            let uri = path_to_uri(&path);
            let already = self.state.lock().expect("state").docs.contains_key(&uri);

            if already {
                continue;
            }

            if let Ok(text) = std::fs::read_to_string(&path) {
                self.open_doc(&uri, text, 0, false);
            }
        }

        let runtime = {
            let mut st = self.state.lock().expect("state");
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
        log::info("workspace shadows opened");
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
            let roots = self.poll_roots();

            if roots.is_empty() {
                continue;
            }

            let now = tree_stamp(&roots);

            // The first pass rescans too: a package install between the
            // startup pass and this one would otherwise be the baseline and
            // never read. A rescan that finds nothing new costs one walk.
            if stamp != Some(now) {
                stamp = Some(now);
                self.rescan_workspace();
            }
        }
    }

    /// The folders a file poll watches: `[build] in`, every folder the
    /// mount table and the Luau configuration name, and the workspace
    /// root's own files. The output folder stays out: the build writes
    /// there, and a poll of it would answer its own writes.
    pub(crate) fn poll_roots(&self) -> Vec<PathBuf> {
        let root = self.state.lock().expect("state").root.clone();
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
        let root = self.state.lock().expect("state").root.clone();
        let Some(root) = root else {
            return;
        };
        let out = Config::find_within(&root, &root)
            .and_then(|p| Config::load(&p).ok().map(|c| (p, c)))
            .map(|(p, c)| p.parent().unwrap_or(&root).join(&c.build.out));
        let mut files = Vec::new();
        let mut plain = Vec::new();
        walk(&root, out.as_deref(), &mut files, &mut plain);

        // What the mirror does not already hold, letter for letter.
        let changed: Vec<PathBuf> = {
            let st = self.state.lock().expect("state");

            plain
                .into_iter()
                .filter(|path| {
                    let target = st.mirror_path(path);

                    std::fs::read_to_string(&target).ok() != std::fs::read_to_string(path).ok()
                })
                .collect()
        };
        let fresh: Vec<PathBuf> = {
            let st = self.state.lock().expect("state");

            files
                .into_iter()
                .filter(|path| !st.docs.contains_key(&path_to_uri(path)))
                .collect()
        };

        if changed.is_empty() && fresh.is_empty() {
            return;
        }

        log::info(&format!(
            "rescan: {} plain files, {} shadows",
            changed.len(),
            fresh.len()
        ));
        self.open_workspace();

        // The child caches a module it has read; it re-reads one it
        // hears about.
        let notice: Vec<Value> = {
            let st = self.state.lock().expect("state");
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
        self.refresh_importers(&touched);
    }

    /// Resends every document whose imports name one of these files, so
    /// the child reads the module again and the document types against
    /// it. A new module alone leaves the import as it was typed.
    pub(crate) fn refresh_importers(&self, changed: &[PathBuf]) {
        let changed: Vec<PathBuf> = changed.iter().map(|p| normalize(p)).collect();
        let messages: Vec<(String, Value)> = {
            let st = self.state.lock().expect("state");

            st.docs
                .iter()
                .filter_map(|(uri, doc)| {
                    let path = uri_to_path(uri)?;

                    if !child_sees(uri) {
                        return None;
                    }

                    let names = alloy::modules::import_targets_for_file(&path, &doc.source)
                        .iter()
                        .any(|t| changed.contains(&normalize(t)));

                    if !names {
                        return None;
                    }

                    Some((
                        uri.clone(),
                        json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/didChange",
                            "params": {
                                "textDocument": { "uri": st.child_uri(uri), "version": doc.version },
                                "contentChanges": [{ "text": doc.shadow }]
                            }
                        }),
                    ))
                })
                .collect()
        };

        for (uri, message) in messages {
            self.to_child(&message);
            self.publish(&uri);
        }
    }

    /// Asks the editor to report changes to every file the project
    /// reads: a source, a plain Luau module, a data file, and the Luau
    /// configuration. A package install writes a whole tree at once, and
    /// the child types an import of a module it never read as `any`.
    /// An editor that takes no dynamic registration is polled instead.
    pub(crate) fn watch_project_files(&self) {
        if !self.state.lock().expect("state").watch_registration {
            return;
        }

        let id = {
            let mut st = self.state.lock().expect("state");
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
            let st = self.state.lock().expect("state");
            st.docs
                .iter()
                .filter_map(|(uri, doc)| {
                    uri_to_path(uri).map(|p| (uri.clone(), p, doc.source.clone()))
                })
                .collect()
        };
        let changes = imports::rename_edits(&docs, &renames);

        // Shadows move: the old one closes, the new one opens from disk
        // or from the text we hold.
        for (uri, path, source) in &docs {
            let moved = imports_map(path, &renames);

            if moved == *path {
                continue;
            }

            let text = std::fs::read_to_string(&moved).unwrap_or_else(|_| source.clone());
            let open = self.state.lock().expect("state").editor_open.contains(uri);
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
            let mut st = self.state.lock().expect("state");
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

/// One workspace root as a directory name. Two servers run at once, one
/// per project, and neither may write where the other reads.
pub fn root_key(root: Option<&Path>) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_default()
        .hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

pub(crate) fn mirror_dir(root: Option<&Path>) -> PathBuf {
    std::env::temp_dir().join("alloy-lsp").join(root_key(root))
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

/// A sourcemap on its way into the mirror: every `.aly` and `.alx`
/// path becomes the mirror's `.luau`, and the runtime under the output
/// root becomes the mirror's copy under the input root.
pub(crate) fn mirrored_sourcemap(
    text: &str,
    input: &Path,
    out: Option<&Path>,
    root: &Path,
) -> String {
    let Ok(mut json) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let runtime_out = out.map(|o| rel(&o.join("alloy.luau")));
    let runtime_in = rel(&input.join("alloy.luau"));

    pub(crate) fn walk(v: &mut Value, f: &dyn Fn(&str) -> String) {
        match v {
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, f)),

            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    if k == "filePaths" {
                        if let Value::Array(paths) = v {
                            for p in paths.iter_mut() {
                                if let Value::String(s) = p {
                                    *s = f(s);
                                }
                            }
                        }
                    } else {
                        walk(v, f);
                    }
                }
            }

            _ => {}
        }
    }

    walk(&mut json, &|s: &str| {
        if runtime_out.as_deref() == Some(s) {
            return runtime_in.clone();
        }

        if let Some(b) = s.strip_suffix(".d.aly") {
            format!("{b}.d.luau")
        } else if let Some(b) = s
            .strip_suffix(".aly")
            .or_else(|| s.strip_suffix(".alx"))
            .or_else(|| s.strip_suffix(".json"))
            .or_else(|| s.strip_suffix(".toml"))
        {
            // A data file is a module in the mirror, as in the build.
            format!("{b}.luau")
        } else {
            s.to_string()
        }
    });

    serde_json::to_string(&json).unwrap_or_else(|_| text.to_string())
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
/// The names a document declares as global, in order, for the compare
/// that says whether the workspace's set changed.
fn global_names(globals: &[alloy::globals::Global]) -> Vec<String> {
    globals.iter().map(|g| g.name.clone()).collect()
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
