use super::*;

/// A request the editor sent, waiting for the child's answer.
pub(crate) struct Pending {
    pub(crate) method: String,
    /// The source URI the request was about, if an Alloy file.
    pub(crate) ctx: Option<String>,
    /// The source position of the request, for completion.
    pub(crate) position: Option<(u32, u32)>,
    /// The character that triggered a completion, when one did.
    pub(crate) trigger: Option<String>,
    /// The source range of the request, for code actions.
    pub(crate) range: Option<((u32, u32), (u32, u32))>,
}

/// An `alloy.toml` and the path it was read from.
pub(crate) type Project = std::sync::Arc<(PathBuf, Config)>;

/// A question the server asked the editor.
pub(crate) enum Asked {
    /// Apply this workspace edit if the answer is the update action.
    Rename(Value),
    /// The watcher registration; the answer says nothing to act on.
    Watch,
}

#[derive(Default)]
pub(crate) struct State {
    /// Source URI -> document.
    pub(crate) docs: HashMap<String, Doc>,
    /// Shadow URI -> source URI.
    pub(crate) shadows: HashMap<String, String>,
    /// Source URIs the editor holds open; the rest mirror the disk.
    pub(crate) editor_open: HashSet<String>,
    pub(crate) pending: HashMap<String, Pending>,
    /// The id of the `initialize` request, whose result is edited.
    pub(crate) initialize_id: Option<String>,
    /// Latest child diagnostics per source URI, already mapped.
    pub(crate) child_diagnostics: HashMap<String, Vec<Value>>,
    pub(crate) root: Option<PathBuf>,
    /// Extensions declared anywhere under the root, read at startup.
    pub(crate) extensions: Vec<alloy::extensions::Extension>,
    /// The ingots of the root's alloy.toml, started at workspace open
    /// and again when the file changes.
    pub(crate) ingots: Option<std::sync::Arc<alloy::ingot::Ingots>>,
    /// The mirror workspace the child works in.
    pub(crate) mirror: PathBuf,
    /// Plain Luau documents the editor holds open, by real URI: their
    /// text keeps the mirror copy current.
    pub(crate) plain: HashMap<String, String>,
    /// The mirror URI of the runtime module; its diagnostics stay inside.
    pub(crate) runtime_uri: Option<String>,
    /// The places the runtime was written to, one per project input.
    pub(crate) runtimes: std::cell::RefCell<std::collections::HashSet<PathBuf>>,
    /// The child's settings, answered on `workspace/configuration`.
    pub(crate) settings: Value,
    /// The proxy's own editor options, from the same settings object.
    pub(crate) editor: settings::Editor,
    /// Questions in flight, by request id.
    pub(crate) asked: HashMap<String, Asked>,
    pub(crate) next_id: u64,
    /// Whether the editor takes snippet text in a completion item.
    pub(crate) snippets: bool,
    /// Whether the editor takes a watcher registration. Without one the
    /// server polls the project's folders itself.
    pub(crate) watch_registration: bool,
    /// The `alloy.toml` a folder answers to, remembered. Every compile
    /// asks for it, and a pass over the workspace compiles every file,
    /// so a fresh read for each pair costs the pass minutes.
    pub(crate) configs: std::cell::RefCell<HashMap<PathBuf, Option<Project>>>,
    /// The project tree of a base folder, remembered for the same
    /// reason: `Tree::load` walks the whole project from disk.
    pub(crate) trees: std::cell::RefCell<HashMap<PathBuf, Arc<alloy::project::Tree>>>,
    /// The roots whose `.luaurc` the mirror already holds.
    pub(crate) luau_configs: std::cell::RefCell<HashSet<PathBuf>>,
}

impl State {
    /// Drops what the state remembers of the disk. A pass over the
    /// workspace calls it first, so a changed `alloy.toml` or a new
    /// file reaches the next compile.
    pub(crate) fn forget_disk(&self) {
        self.configs.borrow_mut().clear();
        self.trees.borrow_mut().clear();
        self.luau_configs.borrow_mut().clear();
    }

    /// The `alloy.toml` over a folder, with its path. The climb stops
    /// at the workspace root: a sibling project under the same parent
    /// must not lend its configuration.
    pub(crate) fn config_at(&self, dir: &Path) -> Option<Project> {
        if let Some(hit) = self.configs.borrow().get(dir) {
            return hit.clone();
        }

        let found = match &self.root {
            Some(root) => Config::find_within(dir, root),

            None => Config::find(dir),
        };
        let loaded = found
            .and_then(|p| Config::load(&p).ok().map(|c| (p, c)))
            .map(Arc::new);
        self.configs
            .borrow_mut()
            .insert(dir.to_path_buf(), loaded.clone());

        loaded
    }

    /// The project tree under a base folder.
    pub(crate) fn tree_at(&self, base: &Path, config: &Config) -> Arc<alloy::project::Tree> {
        if let Some(hit) = self.trees.borrow().get(base) {
            return hit.clone();
        }

        let tree = Arc::new(alloy::project::Tree::load(base, config));
        self.trees
            .borrow_mut()
            .insert(base.to_path_buf(), tree.clone());

        tree
    }

    /// The structs and enums of every open document, for the folds.
    pub(crate) fn known_shapes(&self) -> crate::shapes::Known {
        self.known_shapes_at(None)
    }

    /// The shapes of the workspace, with one document's own first: two
    /// structs of a shape print alike, and the file's own is the one
    /// its reader means.
    pub(crate) fn known_shapes_at(&self, uri: Option<&str>) -> crate::shapes::Known {
        let here = uri.and_then(|u| self.docs.get(u));
        let rest = self.docs.iter().filter(|(u, _)| Some(u.as_str()) != uri);

        crate::shapes::Known {
            shapes: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d))
                .flat_map(|d| d.shapes.iter().chain(&d.import_shapes).cloned())
                .collect(),
            interfaces: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d))
                .flat_map(|d| d.interfaces.iter().chain(&d.import_interfaces).cloned())
                .collect(),
            namespaces: here
                .into_iter()
                .chain(rest.map(|(_, d)| d))
                .flat_map(|d| d.namespaces.iter().cloned())
                .collect(),
        }
    }

    /// The `global` declarations of every open document, each with the
    /// path a message names: the file relative to `[build] in`. A
    /// script's globals belong to the module the build hoists them into.
    pub(crate) fn project_globals(&self) -> Vec<alloy::globals::Global> {
        let mut out = Vec::new();

        for (uri, doc) in &self.docs {
            let Some(rel) = self.project_rel(uri) else {
                continue;
            };

            if rel.to_string_lossy().ends_with(".d.aly") {
                continue;
            }

            let script = alloy::modules::is_script(&rel.to_string_lossy());

            for g in &doc.globals {
                let mut g = g.clone();
                g.side = g
                    .side_directive
                    .unwrap_or_else(|| self.side_of(uri, &doc.source));
                // `file` names the module a require reaches; a script
                // cannot be required, so its globals live in the module
                // the build hoists them into. `declared_in` keeps the
                // file the author wrote, which is what a message and a
                // completion detail name.
                g.declared_in = rel.clone();
                g.file = match script {
                    true => PathBuf::from(alloy::globals::hoist_name(&rel.to_string_lossy())),

                    false => rel.clone(),
                };
                out.push(g);
            }
        }

        out.sort_by(|a, b| (&a.file, a.offset).cmp(&(&b.file, b.offset)));
        out
    }

    /// What every file of the project reaches without an import: the
    /// global macros, the global attributes, and the names a `.d.aly`
    /// declares that a `global` also takes. Each document holds its
    /// own, so the set is a walk of the open documents, not a parse of
    /// every source again.
    #[allow(clippy::type_complexity)]
    pub(crate) fn project_declarations(
        &self,
        globals: &[alloy::globals::Global],
    ) -> (
        Vec<alloy::desugar::MacroSource>,
        Vec<(String, alloy::desugar::AttrDecl)>,
        Vec<(String, String)>,
    ) {
        let mut macros = Vec::new();
        let mut attributes = Vec::new();
        let mut clashes = Vec::new();

        for (uri, doc) in &self.docs {
            macros.extend(doc.macros.iter().cloned());
            attributes.extend(doc.attributes.iter().cloned());

            if doc.ambient.is_empty() {
                continue;
            }

            let Some(rel) = self.project_rel(uri) else {
                continue;
            };
            let file = rel.to_string_lossy().replace('\\', "/");

            for name in &doc.ambient {
                if globals.iter().any(|g| &g.name == name) {
                    clashes.push((name.clone(), file.clone()));
                }
            }
        }

        (macros, attributes, clashes)
    }

    /// Writes the mirror's Luau configuration for a project root once.
    /// Building the text reads the aliases from disk, and every compile
    /// asks for it, so the write waits for `forget_disk`.
    pub(crate) fn ensure_luau_config(&self, root: &Path, config: &Config) {
        if self.luau_configs.borrow().contains(root) {
            return;
        }

        self.write_mirror(&root.join(".luaurc"), &mirror_luau_text(root, Some(config)));
        self.luau_configs.borrow_mut().insert(root.to_path_buf());
    }

    /// The side a document sees: its name, then `--@alloy-side`, then
    /// the place the project's tree gives it.
    pub(crate) fn side_of(&self, uri: &str, source: &str) -> Option<alloy::directives::Side> {
        let name = uri_to_path(uri)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| uri.to_string());

        if let Some(side) = alloy::directives::effective_side(source, &name) {
            return Some(side);
        }

        let path = uri_to_path(uri)?;
        let found = self.config_at(path.parent()?)?;
        let (config_path, config) = (&found.0, &found.1);
        let base = config_path.parent()?.to_path_buf();
        let tree = self.tree_at(&base, config);
        let rel = self.project_rel(uri)?;
        let from_root = config.build.input.join(&rel);

        // `[contexts]` names the folders that hold one side's code;
        // the tree's service is the word under that.
        if let Some(side) = config.contexts.side_of(&rel, &from_root) {
            return side;
        }

        let place = alloy::project::instance_path(&tree, &from_root)?;

        alloy::directives::mount_side(&place)
    }

    /// The side of a document, or none when the file is shared. The
    /// source comes from the document the state holds.
    pub(crate) fn side_at(&self, uri: &str) -> Option<alloy::directives::Side> {
        self.side_of(uri, self.docs.get(uri).map(|d| d.source.as_str())?)
    }

    /// Whether one `global` declaration of `owner` reaches the file
    /// `uri`. A file reaches the globals of its own side and the shared
    /// ones. The compiler reads the same rule, so a name the editor
    /// offers is a name that compiles.
    pub(crate) fn global_reaches(
        &self,
        uri: &str,
        owner: &str,
        g: &alloy::globals::Global,
    ) -> bool {
        let theirs = g.side_directive.unwrap_or_else(|| self.side_at(owner));

        alloy::globals::reaches(theirs, self.side_at(uri))
    }

    /// The path of a document relative to `[build] in`, the way the
    /// build and a message name it.
    pub(crate) fn project_rel(&self, uri: &str) -> Option<PathBuf> {
        let path = uri_to_path(uri)?;
        let root = self.root.as_deref()?;
        // With no alloy.toml the workspace root is the input; a file
        // outside it keeps its own path.
        let found = path.parent().and_then(|d| self.config_at(d));
        let base = match found {
            Some(config) => config.0.parent()?.join(&config.1.build.input),

            None => root.to_path_buf(),
        };

        Some(
            normalize(&path)
                .strip_prefix(normalize(&base))
                .ok()?
                .to_path_buf(),
        )
    }

    /// The `[lint]` table of the workspace's alloy.toml, or the defaults.
    pub(crate) fn lint_config(&self) -> alloy::config::LintConfig {
        self.root
            .as_deref()
            .and_then(|r| self.config_at(r))
            .map(|c| c.1.lint.clone())
            .unwrap_or_default()
    }

    /// The emit options and markup config for a file, from the nearest
    /// `alloy.toml`: its `[alx]` table, or a `luaux.toml` beside it.
    pub(crate) fn options_for(&self, uri: &str) -> (EmitOptions, alloy::luaux::Config) {
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        // The climb stops at the workspace root: a sibling project
        // under the same parent must not lend its configuration.
        let config = self.config_at(&dir);
        let file_name = path.to_string_lossy().into_owned();
        let definitions = file_name.ends_with(".d.aly");
        let config_dir = config
            .as_ref()
            .and_then(|c| c.0.parent().map(Path::to_path_buf))
            .or_else(|| self.root.clone())
            .unwrap_or_else(|| dir.clone());
        let jsx = config
            .as_ref()
            .map(|c| c.1.markup(&config_dir))
            .unwrap_or_else(|| alloy::luaux::Config::load(&config_dir).map_err(|e| e.message))
            .unwrap_or_default();

        let options = match config {
            Some(found) => {
                let (config_path, config) = (&found.0, &found.1);
                let root = normalize(config_path.parent().unwrap_or(Path::new(".")));
                // The shadow requires the runtime by an alias, and the
                // mirror's Luau configuration names the place the runtime
                // is written. A relative path would not do: the analyzer
                // reads `../alloy` in a file the sourcemap holds as an
                // instance path, so the runtime would resolve only in a
                // project that has built one. The ship artifact still
                // writes the instance path.
                self.ensure_runtime(&normalize(&root.join(&config.build.out)));
                self.ensure_luau_config(&root, config);

                let globals = self.project_globals();
                let rel = self.project_rel(uri).unwrap_or_default();
                let script = alloy::modules::is_script(&rel.to_string_lossy());
                let (global_macros, global_attributes, ambient_clashes) =
                    self.project_declarations(&globals);

                EmitOptions {
                    wait_timeout: config.emit.wait_timeout,
                    file_name,
                    std_require: RUNTIME_ALIAS.to_string(),
                    definitions,
                    erase_type_imports: config.emit.erase_type_imports,
                    extensions: self.extensions.clone(),
                    global_macros,
                    global_attributes,
                    globals: alloy::globals::refs_for(&globals, &rel, &HashMap::new()),
                    hoist_globals: script,
                    side: self.side_of(
                        uri,
                        self.docs.get(uri).map(|d| d.source.as_str()).unwrap_or(""),
                    ),
                    ambient_clashes,
                    in_project: true,
                    ..EmitOptions::default()
                }
            }

            None => {
                self.ensure_runtime(&normalize(&dir));

                EmitOptions {
                    file_name,
                    std_require: "./alloy".to_string(),
                    definitions,
                    extensions: self.extensions.clone(),
                    ..EmitOptions::default()
                }
            }
        };

        (options, jsx)
    }
}
