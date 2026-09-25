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
    /// The diagnostics the editor sent with a code action request. An
    /// import quick fix reads the name the report could not resolve.
    pub(crate) diagnostics: Vec<Value>,
    /// The text a workspace symbol request searches for.
    pub(crate) query: Option<String>,
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
    /// The set last published per source URI. A pass over the
    /// workspace opens every file, the editor opens it again, and the
    /// child answers each time: the same set again teaches the editor
    /// nothing, so it stays here and goes no further.
    pub(crate) published: HashMap<String, Vec<Value>>,
    pub(crate) root: Option<PathBuf>,
    /// Extensions declared anywhere under the root, read at startup.
    pub(crate) extensions: Vec<alloy::extensions::Extension>,
    /// The `impl` blocks the project writes on a struct or an enum
    /// another file declares, as `alloy build` reads them. The walk
    /// costs one parse per source, so the answer is remembered.
    pub(crate) project: std::cell::RefCell<Option<Arc<alloy::extensions::ProjectImpls>>>,
    /// The structs of the whole project with their wire widths and
    /// derives, as `alloy build` feeds every file: a remote packs a
    /// struct another file declares, and a derive reaches its importer.
    pub(crate) project_shapes: std::cell::RefCell<Option<Arc<Vec<alloy::StructShape>>>>,
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
    /// The semantic token types the child's legend names, in its own
    /// order. The proxy draws tokens of its own for the Alloy
    /// constructs, and it paints with the child's numbers.
    pub(crate) token_types: Vec<String>,
    /// The semantic token modifiers the child's legend names.
    pub(crate) token_modifiers: Vec<String>,
    /// Whether the editor takes snippet text in a completion item.
    pub(crate) snippets: bool,
    /// Whether the editor takes a watcher registration. Without one the
    /// server polls the project's folders itself.
    pub(crate) watch_registration: bool,
    /// The `alloy.toml` a folder answers to, remembered. Every compile
    /// asks for it, and a pass over the workspace compiles every file,
    /// so a fresh read for each pair costs the pass minutes.
    pub(crate) configs: std::cell::RefCell<HashMap<PathBuf, Option<Project>>>,
    /// The roots whose `.luaurc` the mirror already holds.
    pub(crate) luau_configs: std::cell::RefCell<HashSet<PathBuf>>,
    /// The last load of each open `.config.aly`: the source it ran, and
    /// the lint names it gives that name no lint, or its failure. A
    /// publish runs often, and the load runs the file.
    pub(crate) config_loads: std::cell::RefCell<HashMap<String, (String, ConfigLoad)>>,
    /// The Roblox API docs the child was started with, `--docs`. The
    /// proxy answers the object initializer itself, so it reads the
    /// same text the child shows after a `.`.
    pub(crate) api_docs: Option<PathBuf>,
    /// The definitions files the child reads. A datatype member such as
    /// `CFrame:inverse` is marked `@deprecated` there and not in the docs.
    pub(crate) definitions: Vec<PathBuf>,
    /// The merged definitions file the child reads, once per `.d.aly`
    /// in it, with the line that file starts on. The child reports on
    /// the file it read, and the report belongs on the `.d.aly`.
    pub(crate) definition_sources: Vec<(PathBuf, alloy::declarations::Segment)>,
    /// The child runs Luau's old solver, which types some checks of the
    /// check artifact in another form. See `EmitOptions::new_solver`.
    pub(crate) old_solver: bool,
    /// The `@roblox/globaltype/Class.Member` entries of that file, read
    /// once on the first list that needs one. The file is 7 MB, so a
    /// read at startup would cost every session that never opens a
    /// class body.
    pub(crate) roblox_docs: std::cell::RefCell<Option<Arc<HashMap<String, String>>>>,
    /// The member names those entries mark deprecated, read off the
    /// same index the first time a list has to hide one.
    pub(crate) roblox_deprecated: std::cell::RefCell<Option<Arc<HashSet<String>>>>,
}

/// What one load of a `.config.aly` gives: the lint names that name no
/// lint, or the reason it did not load.
pub(crate) type ConfigLoad = Result<Vec<String>, String>;

impl State {
    /// The `.d.aly` a zero-based line of a merged definitions file came
    /// from, as a URI, and the line it starts on there.
    pub(crate) fn declared_at(&self, uri: &str, line: usize) -> Option<(String, usize)> {
        let path = uri_to_path(uri)?;
        let segments: Vec<alloy::declarations::Segment> = self
            .definition_sources
            .iter()
            .filter(|(read, _)| *read == path)
            .map(|(_, s)| s.clone())
            .collect();

        alloy::declarations::segment_at(&segments, line)
            .map(|(s, _)| (path_to_uri(&s.source), s.first_line))
    }

    /// Drops what the state remembers of the disk. A pass over the
    /// workspace calls it first, so a changed `alloy.toml` or a new
    /// file reaches the next compile.
    pub(crate) fn forget_disk(&self) {
        self.configs.borrow_mut().clear();
        self.luau_configs.borrow_mut().clear();
        self.project.borrow_mut().take();
        self.project_shapes.borrow_mut().take();
    }

    /// The project's `impl` index, the one `alloy build` feeds every
    /// file: an `impl` on a struct another file declares reaches the
    /// declaring file's check artifact, so every file reads one shape.
    /// A document the editor holds open answers for its own file, the
    /// rest come from the disk.
    pub(crate) fn project_impls(&self) -> Arc<alloy::extensions::ProjectImpls> {
        if let Some(held) = self.project.borrow().as_ref() {
            return Arc::clone(held);
        }

        let sources: Vec<String> = self
            .project_sources()
            .into_iter()
            .filter_map(|path| match self.docs.get(&path_to_uri(&path)) {
                Some(doc) => Some(doc.source.clone()),

                None => std::fs::read_to_string(&path).ok(),
            })
            .collect();

        let held = Arc::new(alloy::extensions::project_impls(&sources));
        *self.project.borrow_mut() = Some(Arc::clone(&held));

        held
    }

    /// The project's structs, the list `alloy build` feeds every file.
    /// The compiler reads each one from disk, so a save is what changes
    /// the answer.
    pub(crate) fn project_shapes(&self) -> Arc<Vec<alloy::StructShape>> {
        if let Some(held) = self.project_shapes.borrow().as_ref() {
            return Arc::clone(held);
        }

        // The editor runs no wire, so the root serves as the base of
        // each shape's module; the keys only have to agree here.
        let base = self.root.clone().unwrap_or_default();
        let held = Arc::new(alloy::build::struct_shapes(&self.project_sources(), &base, &[]).0);
        *self.project_shapes.borrow_mut() = Some(Arc::clone(&held));

        held
    }

    /// The `.aly` sources under the workspace root.
    fn project_sources(&self) -> Vec<PathBuf> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        // The output folder holds the build's own Luau, never a source,
        // and a walk of it would cost the whole tree.
        let out = self
            .config_at(root)
            .map(|c| c.0.parent().unwrap_or(root).join(&c.1.build.out));
        let mut files = Vec::new();
        let mut plain = Vec::new();
        super::documents::walk(root, out.as_deref(), &mut files, &mut plain);
        files.retain(|path| path.to_string_lossy().ends_with(".aly"));

        files
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

    /// The shapes one document reaches, its own first: two structs of
    /// a shape print alike, and a fold names one the file declares or
    /// imports. A struct of a file the document does not import is no
    /// answer for it. With no document, the whole workspace.
    pub(crate) fn known_shapes_at(&self, uri: Option<&str>) -> alloy::shapes::Known {
        let here = uri.and_then(|u| self.docs.get(u));
        let rest = self.docs.iter().filter(|(u, _)| Some(u.as_str()) != uri);

        alloy::shapes::Known {
            shapes: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d).filter(|_| here.is_none()))
                .flat_map(|d| d.shapes.iter().chain(&d.import_shapes).cloned())
                .collect(),
            interfaces: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d))
                .flat_map(|d| d.interfaces.iter().chain(&d.import_interfaces).cloned())
                .collect(),
            namespaces: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d))
                .flat_map(|d| d.namespaces.iter().cloned())
                .collect(),
            tables: here
                .into_iter()
                .chain(rest.map(|(_, d)| d))
                .flat_map(|d| d.tables.iter().cloned())
                .collect(),
        }
    }

    /// The side a document sees: `ui.client.aly` is the client's and
    /// `main.server.aly` the server's. Any other name takes the side of
    /// its place in the game, as the compile does.
    pub(crate) fn side_at(&self, uri: &str) -> Option<alloy::directives::Side> {
        let path = uri_to_path(uri);
        let name = path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| uri.to_string());

        alloy::directives::file_side(&name).or_else(|| {
            let path = path?;
            let project = self.config_at(path.parent()?)?;
            let root = project.0.parent()?;
            let tree = alloy::project::Tree::load(root, &project.1);

            alloy::project::place_side(&tree, path.strip_prefix(root).ok()?)
        })
    }

    /// Writes the mirror's Luau configuration for a project root once.
    /// Building the text reads the aliases from disk, and every compile
    /// asks for it, so the write waits for `forget_disk`.
    pub(crate) fn ensure_luau_config(&self, root: &Path, config: &Config) {
        if self.luau_configs.borrow().contains(root) {
            return;
        }

        let text = mirror_luau_text(root, Some(config));
        self.write_mirror(&root.join(".luaurc"), &text);

        // `in = "../examples"`: the sources sit beside the root, not
        // under it, so no configuration above them sets the mode, and
        // the child checks them non-strict. They take the root's, with
        // each alias rebased from the input folder.
        let input = alloy::modules::normalize(&root.join(&config.build.input));

        if !input.starts_with(root)
            && let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text)
        {
            if let Some(aliases) = value
                .get_mut("aliases")
                .and_then(serde_json::Value::as_object_mut)
            {
                for (_, path) in aliases.iter_mut() {
                    if let Some(rel) = path.as_str().filter(|p| !Path::new(p).is_absolute()) {
                        let target = alloy::modules::normalize(&root.join(rel));
                        *path = serde_json::json!(crate::imports::relative_spec(&input, &target));
                    }
                }
            }

            let text = serde_json::to_string_pretty(&value).unwrap_or_default() + "\n";
            self.write_mirror(&input.join(".luaurc"), &text);
        }

        self.luau_configs.borrow_mut().insert(root.to_path_buf());
    }

    /// The text the Roblox API docs hold for a member of a class,
    /// walking what the class extends the way a property lookup does.
    /// `None` without a `--docs` file, which is how the child runs
    /// when the editor found none.
    pub(crate) fn roblox_doc(&self, class: &str, member: &str) -> Option<String> {
        let docs = self.roblox_docs()?;
        let mut name = Some(class);

        while let Some(c) = name {
            if let Some(text) = docs.get(&format!("@roblox/globaltype/{c}.{member}")) {
                return Some(text.clone());
            }

            name = alloy::luaux::roblox::superclass(c);
        }

        None
    }

    /// The member names the API docs mark deprecated: the engine opens
    /// the text of a deprecated member with the note. `brickColor` and
    /// `BasePart.brickColor` both stand in the set, so a row that names
    /// the class and one that names the member alone both find it.
    pub(crate) fn roblox_deprecated_names(&self) -> Arc<HashSet<String>> {
        if let Some(held) = self.roblox_deprecated.borrow().as_ref() {
            return Arc::clone(held);
        }

        let mut names = HashSet::new();

        if let Some(docs) = self.roblox_docs() {
            for (key, text) in docs.iter() {
                let Some(entry) = key.strip_prefix("@roblox/globaltype/") else {
                    continue;
                };

                // `Class.Method/param/0` documents an argument, not a
                // member the list writes.
                if entry.contains('/') || !crate::proxy::completion::says_deprecated(text) {
                    continue;
                }

                if let Some((_, member)) = entry.rsplit_once('.') {
                    names.insert(member.to_string());
                }

                names.insert(entry.to_string());
            }
        }

        // `@deprecated` on its own line marks the member on the next.
        for path in &self.definitions {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let mut owner = "";
            let mut marked = false;

            for line in text.lines().map(str::trim) {
                if let Some(rest) = line
                    .strip_prefix("declare extern type ")
                    .or_else(|| line.strip_prefix("declare class "))
                {
                    owner = rest.split_whitespace().next().unwrap_or("");
                } else if line == "@deprecated" {
                    marked = true;

                    continue;
                } else if marked {
                    let member = line
                        .strip_prefix("function ")
                        .unwrap_or(line)
                        .split(['(', ':'])
                        .next()
                        .unwrap_or("")
                        .trim();

                    if !member.is_empty() {
                        names.insert(member.to_string());
                        names.insert(format!("{owner}.{member}"));
                    }
                }

                marked = false;
            }
        }

        let held = Arc::new(names);
        *self.roblox_deprecated.borrow_mut() = Some(Arc::clone(&held));

        held
    }

    /// The docs file, read once. A file that will not parse reads as an
    /// empty index, so the read runs once and not on every list.
    fn roblox_docs(&self) -> Option<Arc<HashMap<String, String>>> {
        if let Some(held) = self.roblox_docs.borrow().as_ref() {
            return Some(Arc::clone(held));
        }

        let mut index = HashMap::new();

        if let Some(path) = &self.api_docs {
            match std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str::<HashMap<String, Value>>(&t).ok())
            {
                Some(entries) => {
                    for (key, entry) in entries {
                        if !key.starts_with("@roblox/globaltype/") {
                            continue;
                        }

                        // The text the way luau-lsp writes it: the
                        // description, the link to the reference page,
                        // and the code sample, so the editor's side
                        // panel reads the same for a property the proxy
                        // lists and one the child lists.
                        let text = entry
                            .get("documentation")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                            .map(plain_docs);
                        let link = entry
                            .get("learn_more_link")
                            .and_then(Value::as_str)
                            .filter(|l| !l.is_empty())
                            .map(|l| format!("[Learn More]({l})"));
                        let sample = entry
                            .get("code_sample")
                            .and_then(Value::as_str)
                            .filter(|c| !c.trim().is_empty())
                            .map(|c| format!("```luau\n{}\n```", c.trim_end()));
                        let parts: Vec<String> =
                            [text, link, sample].into_iter().flatten().collect();

                        if !parts.is_empty() {
                            index.insert(key, parts.join("\n\n"));
                        }
                    }
                }

                None => log::warn(&format!("cannot read the API docs at {}", path.display())),
            }
        }

        let held = Arc::new(index);
        *self.roblox_docs.borrow_mut() = Some(Arc::clone(&held));

        Some(held)
    }

    /// The `[lint]` table of the workspace's alloy.toml, or the defaults.
    pub(crate) fn lint_config(&self) -> alloy::config::LintConfig {
        self.root
            .as_deref()
            .and_then(|r| self.config_at(r))
            .map(|c| c.1.lint.clone())
            .unwrap_or_default()
    }

    /// The `[fmt]` table for a file, from the nearest `alloy.toml`.
    ///
    /// The climb starts at the file, not the workspace root, so a
    /// project inside a multi-root workspace keeps its own layout. The
    /// editor formats through this, the way `alloy fmt` does, so
    /// format on save and the command agree. The renames of
    /// `fix_naming` read the styles and the level from `[lint]`.
    pub(crate) fn fmt_config(&self, uri: &str) -> alloy::config::FmtConfig {
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();

        self.config_at(&dir)
            .map(|c| alloy::config::FmtConfig {
                lint: c.1.lint.clone(),
                ..c.1.fmt.clone()
            })
            .unwrap_or_default()
    }

    /// The configuration a file answers to, and the folder its paths
    /// read from.
    fn project_for(&self, path: &Path) -> (Option<Project>, PathBuf) {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        // The climb stops at the workspace root: a sibling project
        // under the same parent must not lend its configuration. A file
        // under the root's `in`, `../examples`, answers to the root, as
        // it does for `alloy build`.
        let config = self.config_at(&dir).or_else(|| {
            let root = self.root.as_deref()?;
            let found = self.config_at(root)?;
            let input = normalize(&root.join(&found.1.build.input));

            path.starts_with(&input).then_some(found)
        });
        let config_dir = config
            .as_ref()
            .and_then(|c| c.0.parent().map(Path::to_path_buf))
            .or_else(|| self.root.clone())
            .unwrap_or(dir);

        (config, config_dir)
    }

    /// The markup config of a file: the `[alx]` table, or a
    /// `luaux.toml` beside the project.
    fn markup_for(
        config: Option<&Project>,
        config_dir: &Path,
    ) -> Result<alloy::luaux::Config, String> {
        match config {
            Some(c) => c.1.markup(config_dir),

            None => alloy::luaux::Config::load(config_dir).map_err(|e| e.message),
        }
    }

    /// Why the markup config of a `.alx` file does not load. The build
    /// skips the file then; the editor compiles it with the default
    /// backend, whose own report would hide the real one.
    pub(crate) fn markup_problem(&self, uri: &str) -> Option<String> {
        if !uri.ends_with(".alx") {
            return None;
        }

        let path = uri_to_path(uri)?;
        let (config, config_dir) = self.project_for(&path);

        Self::markup_for(config.as_ref(), &config_dir).err()
    }

    /// The emit options and markup config for a file, from the nearest
    /// `alloy.toml`: its `[alx]` table, or a `luaux.toml` beside it.
    pub(crate) fn options_for(&self, uri: &str) -> (EmitOptions, alloy::luaux::Config) {
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let (config, config_dir) = self.project_for(&path);
        let file_name = path.to_string_lossy().into_owned();
        let definitions = file_name.ends_with(".d.aly");
        let jsx = Self::markup_for(config.as_ref(), &config_dir).unwrap_or_default();

        let mut options = match config {
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

                EmitOptions {
                    wait_timeout: config.emit.wait_timeout,
                    file_name,
                    std_require: RUNTIME_ALIAS.to_string(),
                    definitions,
                    erase_type_imports: config.emit.erase_type_imports,
                    test_runner: config.test.lest,
                    extensions: self.extensions.clone(),
                    std_globals: config.std.globals.clone(),
                    naming: config.lint.naming.clone(),
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

        // Luau reads `x/init.luau` as the module `x`, so a relative
        // require in it names a file beside `x`. The shadow of an
        // `init.aly` sits at the same place in the mirror, so the mirror
        // path is the space its requires are written in. Without it
        // `init.aly` requires a file one folder too high, and every name
        // it imports reads as `unknown`.
        options.module_rel = self.mirror_path(&path).to_string_lossy().into_owned();

        // An `impl` on a struct another file declares attaches at run
        // time through the require, and the declaring file's artifact
        // declares the methods, so the type follows. `alloy build`
        // feeds every file the same index; the editor now does too.
        let project = self.project_impls();
        options.foreign_impls = project.methods.clone();
        options.foreign_privates = project.privates.clone();
        options.shapes = self.project_shapes().to_vec();
        options.new_solver = !self.old_solver;

        (options, jsx)
    }
}

/// The API docs write HTML: `<code>Instance</code>` for a name, and a
/// `<br/>` for a break. Markdown carries the same two.
fn plain_docs(text: &str) -> String {
    let text = text
        .replace("<code>", "`")
        .replace("</code>", "`")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n");
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;

    for c in text.chars() {
        match c {
            '<' => in_tag = true,

            '>' => in_tag = false,

            _ if !in_tag => out.push(c),

            _ => {}
        }
    }

    out.trim().to_string()
}
