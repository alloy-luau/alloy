//! `alloy build`: every source under `in`, compiled into the tree under `out`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::config::{Artifact, Build, Config, Emit};
use crate::{Diagnostic, EmitOptions, Lint};

/// What one build did.
#[derive(Debug, Default)]
pub struct Report {
    pub written: Vec<PathBuf>,
    /// The outputs the build left alone: the file already held the
    /// bytes the compile produced, so nothing was written.
    pub up_to_date: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Diagnostics per source, with the source path.
    pub diagnostics: Vec<(PathBuf, Diagnostic)>,
    /// Every lint that fired, by source path relative to `in`.
    pub lints: Vec<(PathBuf, Lint)>,
    pub failures: Vec<(PathBuf, String)>,
    /// The project files written for the tree, relative to the root.
    pub project_files: Vec<PathBuf>,
    pub notes: Vec<String>,
    /// The check artifacts, kept for the type check of `alloy flux`.
    pub checks: Vec<crate::typecheck::CheckSource>,
    /// Plain `.luau` and `.lua` files copied from `in` to `out`.
    pub copied: Vec<PathBuf>,
    /// The `.json` and `.toml` files a source names, written to `out`
    /// as `.luau` modules; the paths are relative to `out`.
    pub data: Vec<PathBuf>,
    /// The check artifacts of every project an import leads into, by
    /// absolute output path, for the checker's mirror.
    pub dep_artifacts: Vec<(PathBuf, String)>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty() && self.failures.is_empty()
    }
}

/// The output path for a source path, relative to the roots.
///
/// `x.aly` becomes `x.luau`, `x.d.aly` becomes `x.d.luau`, and `x.alx`
/// becomes `x.luau`, each under the same subdirectory.
pub fn output_for(rel: &Path) -> Option<PathBuf> {
    let name = rel.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".aly")
        .or_else(|| name.strip_suffix(".alx"))?;

    Some(rel.with_file_name(format!("{stem}.luau")))
}

/// The relative require path from the directory of `from` to `to`,
/// both relative to one root: `./x` for a sibling, `../` per level up.
pub(crate) fn relative_require(from: &Path, to: &Path) -> String {
    let from_dir: Vec<_> = from
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();
    let to_parts: Vec<_> = to.components().collect();
    let common = from_dir
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_dir.len() - common;
    let mut out = if ups == 0 {
        ".".to_string()
    } else {
        vec![".."; ups].join("/")
    };

    for c in &to_parts[common..] {
        out.push('/');
        out.push_str(&c.as_os_str().to_string_lossy());
    }

    out
}

/// The manifest of the outputs the last build wrote, one path from the
/// root per line.
const OUTPUTS: &str = ".alloy/outputs.txt";

/// Whether a file is the `init` of its folder: `init.luau`, and also a
/// script like `init.server.luau`. Rojo makes the folder that module or
/// script, and the other files of the folder its children.
pub fn is_init(path: &Path) -> bool {
    path.file_stem()
        .is_some_and(|s| s == "init" || s == "init.server" || s == "init.client")
}

/// The path a module requires its siblings from: the file itself, and
/// its folder for an `init.luau`. Luau reads `x/init.luau` as the
/// module `x`, so its `./y` names a file beside `x`, not one inside it.
pub(crate) fn module_base(path: &Path) -> PathBuf {
    match is_init(path) {
        true => path.parent().unwrap_or(Path::new("")).to_path_buf(),

        false => path.to_path_buf(),
    }
}

/// Runs a build from the project root.
pub fn run(root: &Path, build: &Build, emit: &Emit) -> std::io::Result<Report> {
    let config = Config {
        build: build.clone(),
        emit: emit.clone(),
        ..Config::default()
    };

    run_with(root, &config, true, false, &mut Deps::default())
}

/// The build of a whole config: the tree writes the project files and
/// routes the requires.
pub fn run_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, true, false, &mut Deps::default())
}

/// `check` for a whole config.
pub fn check_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, false, false, &mut Deps::default())
}

/// `flux` for a whole config: the check, with the artifacts kept for
/// the analyzer.
pub fn flux_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, false, true, &mut Deps::default())
}

/// The build without the write: every source compiles and the report
/// carries the diagnostics and the lints, and the output tree does not
/// change. `written` lists the files that would be written.
pub fn check(root: &Path, build: &Build, emit: &Emit) -> std::io::Result<Report> {
    let config = Config {
        build: build.clone(),
        emit: emit.clone(),
        ..Config::default()
    };

    run_with(root, &config, false, false, &mut Deps::default())
}

/// The structs the sources declare, with each field's type and width,
/// for the wire layout of a remote, and the enums with their variants.
/// Each source also gives its imports, so a layout reads a type name
/// the way the file that writes it does. `base` is the project's `in`
/// folder, which each module is relative to. A source that does not
/// parse contributes nothing; its own compile reports the error.
pub fn struct_shapes(
    sources: &[PathBuf],
    base: &Path,
    aliases: &[(String, PathBuf)],
) -> (Vec<crate::StructShape>, Vec<crate::WireScope>) {
    let mut shapes = Vec::new();
    let mut scopes = Vec::new();
    let base = crate::modules::normalize(base);
    let module_of = |path: &Path| {
        let path = crate::modules::normalize(path);

        path.strip_prefix(&base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/")
    };

    for path in sources {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        // The key holds all the result reads: the text, the base, the
        // aliases, and the file each import names now.
        let key = {
            use std::hash::{Hash, Hasher};

            let mut h = std::collections::hash_map::DefaultHasher::new();
            (&src, &base, aliases).hash(&mut h);

            for spec in crate::modules::import_specs(&src) {
                crate::modules::resolve(&spec, path, aliases).hash(&mut h);
            }

            h.finish()
        };
        let held = shape_cache().lock().ok().and_then(|c| {
            c.get(path)
                .filter(|(k, _)| *k == key)
                .map(|(_, own)| own.clone())
        });
        let own = match held {
            Some(own) => own,

            None => {
                let own = file_shapes(path, &src, &module_of(path), &module_of, aliases);

                if let Ok(mut c) = shape_cache().lock() {
                    c.insert(path.clone(), (key, own.clone()));
                }

                own
            }
        };

        if let Some((own, scope)) = own {
            shapes.extend(own);
            scopes.push(scope);
        }
    }

    (shapes, scopes)
}

/// The shapes one source declares and its scope, `None` for a source
/// that does not parse.
type FileShapes = Option<(Vec<crate::StructShape>, crate::WireScope)>;

/*
The shapes of each source, by path, with the key they came from. The
editor reads the chain of a file's imports on each edit, and most of
those modules did not change: 31 modules took about 4 ms a read, and a
held one costs the read of its text and the lookup of its imports.
*/
fn shape_cache() -> &'static std::sync::Mutex<HashMap<PathBuf, (u64, FileShapes)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, (u64, FileShapes)>>> =
        std::sync::OnceLock::new();

    CACHE.get_or_init(Default::default)
}

/// The shapes one source declares, and the imports that say what a
/// name in it means. `module` is the source's own module.
fn file_shapes(
    path: &Path,
    src: &str,
    module: &str,
    module_of: &dyn Fn(&Path) -> String,
    aliases: &[(String, PathBuf)],
) -> FileShapes {
    let mut shapes = Vec::new();
    let parsed = alloy_syntax::parse_one(src).ok()?;
    // A barrel's `export { Inner } from "./inner"` binds the name
    // for an importer the way an import does.
    let passed = crate::modules::reexports(src)
        .into_iter()
        .filter_map(|(name, exported, spec)| {
            let target = crate::modules::resolve(&spec, path, aliases)?;

            Some((exported, module_of(&target), name))
        });
    let scope = crate::WireScope {
        module: module.to_string(),
        names: crate::modules::named_specs(src, path, aliases)
            .into_iter()
            .map(|(target, name, local)| (local, module_of(&target), name))
            .chain(passed)
            .collect(),
        stars: crate::modules::star_locals(src, path, aliases)
            .into_iter()
            .map(|(target, local)| (local, module_of(&target)))
            .collect(),
    };
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, &parsed.lexed.toks).to_string();
    // A field type may name an alias of this file, which an
    // importer cannot see, so it reads as its value.
    let mut type_aliases = std::collections::HashMap::new();

    for stmt in &parsed.chunk.block.stmts {
        if let alloy_syntax::ast::Stmt::TypeAlias(t) = stmt.under_default()
            && let Some((_, value)) = text(t.span).split_once('=')
        {
            type_aliases.insert(text(t.name), value.trim().to_string());
        }
    }

    let resolve = |ty: String| -> String {
        let base = ty.trim_end_matches('?').trim();
        let optional = &ty[base.len()..];

        if let Some(value) = type_aliases.get(base) {
            match optional.is_empty() {
                true => value.clone(),

                false => format!("({value}){optional}"),
            }
        } else {
            ty
        }
    };

    let derives_of = |attributes: &[alloy_syntax::ast::Attr]| -> Vec<String> {
        attributes
            .iter()
            .filter(|a| a.name.map(&text).as_deref() == Some("derive"))
            .flat_map(|a| a.args.iter().map(|x| text(x.span())))
            // `serde.Serialize` through a star import of the std.
            .map(|d: String| match d.rsplit_once('.') {
                Some((_, n)) if crate::std_names::is_std_name(n) => n.to_string(),

                _ => d,
            })
            .collect()
    };

    let mut scoped = Vec::new();
    let top: Vec<&alloy_syntax::ast::Stmt> = parsed.chunk.block.stmts.iter().collect();
    scoped_stmts(&top, &text, &[], &mut scoped);

    for (stmt, around) in scoped {
        // A member names a sibling by its own name, and the shape keeps
        // the path, `Combat.Pos`, that the layout reads in this module.
        let field_type = |ty: String| {
            resolve(crate::desugar::qualify_names(&ty, &|w| {
                let (path, _) = around
                    .iter()
                    .rev()
                    .find(|(_, names)| names.iter().any(|n| n == w))?;

                Some(format!("{path}.{w}"))
            }))
        };
        // A namespace member renders under one flat name, `Combat_Hit`.
        let prefix: String = around
            .last()
            .map_or(String::new(), |(p, _)| format!("{}_", p.replace('.', "_")));

        if let alloy_syntax::ast::Stmt::Enum(e) = stmt {
            let variants = e.variants.iter().map(|v| {
                let types = v
                    .payload
                    .iter()
                    .map(|t| field_type(text(*t).trim().to_string()))
                    .collect();

                (text(v.name), types)
            });
            shapes.push(crate::StructShape {
                name: format!("{prefix}{}", text(e.name)),
                module: module.to_string(),
                variants: variants.collect(),
                derives: derives_of(&e.attributes),
                ..Default::default()
            });
        }

        let alloy_syntax::ast::Stmt::Struct(st) = stmt else {
            continue;
        };
        // A `@skip` field stays off the wire, as it stays out of
        // the derived table.
        let fields = st
            .fields
            .iter()
            .filter(|f| {
                !f.attributes
                    .iter()
                    .any(|a| a.name.map(&text).as_deref() == Some("skip"))
            })
            .map(|f| crate::WireField {
                name: text(f.name),
                ty: field_type(text(f.ty).trim().to_string()),
                width: f.attributes.iter().find_map(|a| {
                    let n = text(a.name?);

                    crate::desugar::WIRE_WIDTHS
                        .contains(&n.as_str())
                        .then_some(n)
                }),
            })
            .collect();
        let derives = derives_of(&st.attributes);
        shapes.push(crate::StructShape {
            name: format!("{prefix}{}", text(st.name)),
            fields,
            derives,
            module: module.to_string(),
            variants: Vec::new(),
        });
    }

    Some((shapes, scope))
}

/// A namespace around a statement: its path, `Combat.Deep`, and the
/// types it declares.
type Around = (String, Vec<String>);

/// Each statement of a block, and of every namespace in it, with the
/// namespaces around it, innermost last.
fn scoped_stmts<'a>(
    stmts: &[&'a alloy_syntax::ast::Stmt],
    text: &dyn Fn(alloy_syntax::ast::TokSpan) -> String,
    around: &[Around],
    out: &mut Vec<(&'a alloy_syntax::ast::Stmt, Vec<Around>)>,
) {
    use alloy_syntax::ast::Stmt;

    for stmt in stmts {
        let stmt = stmt.under_default();

        if let Stmt::Namespace(ns) = stmt {
            let name = text(ns.name);
            let path = match around.last() {
                Some((p, _)) => format!("{p}.{name}"),

                None => name,
            };
            let members: Vec<&Stmt> = ns.members.iter().map(|m| m.stmt.under_default()).collect();
            let types = members
                .iter()
                .filter_map(|m| match m {
                    Stmt::Struct(s) => Some(text(s.name)),

                    Stmt::Enum(e) => Some(text(e.name)),

                    Stmt::Namespace(n) => Some(text(n.name)),

                    _ => None,
                })
                .collect();
            let mut inner = around.to_vec();
            inner.push((path, types));
            scoped_stmts(&members, text, &inner, out);
        }

        out.push((stmt, around.to_vec()));
    }
}

/// The Alloy sources under `input`, sorted.
pub fn sources(input: &Path, written: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
    let mut list = Vec::new();
    walk(input, written, &mut list)?;
    list.sort();

    Ok(list)
}

/// One project's run, on the dependency stack: an import that leads
/// back to a project on the stack is a cycle.
fn run_with(
    root: &Path,
    config: &Config,
    write: bool,
    keep: bool,
    deps: &mut Deps,
) -> std::io::Result<Report> {
    deps.stack.push(absolute(root));
    let report = run_inner(root, config, write, keep, deps);
    deps.stack.pop();

    report
}

fn run_inner(
    root: &Path,
    config: &Config,
    write: bool,
    keep: bool,
    deps: &mut Deps,
) -> std::io::Result<Report> {
    let build = &config.build;
    let emit = &config.emit;
    let base_options = EmitOptions {
        wait_timeout: emit.wait_timeout,
        std_require: emit
            .std_require
            .clone()
            .unwrap_or_else(|| "@alloy".to_string()),
        erase_type_imports: emit.erase_type_imports,
        thresholds: config.flux.thresholds(),
        naming: config.lint.naming.clone(),
        test_runner: config.test.lest,
        std_globals: config.std.globals.clone(),
        new_solver: config.flux.new_solver,
        ..EmitOptions::default()
    };
    let input = root.join(&build.input);
    let out = root.join(&build.out);
    let exclude = globs(&build.exclude)?;
    // The tree is read once: the `[mount]` table, else the project file
    // at the root. It routes the requires and writes the project files.
    let tree = crate::project::Tree::load(root, config);
    let mut report = Report::default();
    let mut expected: HashSet<PathBuf> = HashSet::new();
    // The outputs of the sources that failed this run. Each keeps the
    // output a run before wrote, so the game runs the last good build.
    let mut held: HashSet<PathBuf> = HashSet::new();
    let mut imports: Vec<(PathBuf, Vec<crate::ImportRef>)> = Vec::new();

    // A project whose `out` (or spec folder) sits under `in` would read
    // its own output back as a source on the next run.
    let written = written_dirs(root, config);

    if input.is_dir() && normalize_path(&out).starts_with(normalize_path(&input)) {
        report
            .notes
            .push("[build] out sits inside in; its files are skipped".to_string());
    }

    let mut sources = Vec::new();
    walk(&input, &written, &mut sources)?;
    sources.sort();
    let mut plain = Vec::new();
    walk_plain(&input, &written, &mut plain)?;

    // The structs of every source, so a remote in one file packs a
    // struct another file declares.
    let module_aliases = crate::modules::aliases(root, &tree);
    let (shapes, wire_scopes) = struct_shapes(&sources, &input, &module_aliases);
    let base_options = EmitOptions {
        shapes,
        wire_scopes,
        ..base_options
    };

    // A data file builds beside the modules; one that would build to
    // the same `.luau` as a source or a plain file is a diagnostic.
    // An alias in a data path resolves through the root's Luau
    // configuration, to a folder under `in`.
    let aliases: Vec<(String, PathBuf)> = tree
        .aliases
        .iter()
        .map(|(alias, path)| (alias.clone(), normalize_path(&root.join(path))))
        .collect();

    let mut data_files = DataFiles {
        input: normalize_path(&input),
        input_rel: build.input.clone(),
        out: out.clone(),
        owners: HashMap::new(),
        done: HashMap::new(),
        aliases,
    };

    let shown = |p: &Path| build.input.join(p).to_string_lossy().replace('\\', "/");

    for path in sources.iter().chain(&plain) {
        let rel = path.strip_prefix(&input).unwrap_or(path).to_path_buf();
        let out_rel = output_for(&rel).unwrap_or_else(|| rel.clone());
        data_files.owners.entry(out_rel).or_insert(rel);
    }

    // Two sources whose names differ in the extension alone build one
    // module: the second write overwrites the first, and
    // `require("./reg")` could not say which one it meant either. The
    // report names both, the way it does for a data file.
    // A plain `.luau` or `.lua` copies under its own name, and a source
    // of its stem writes the same module.
    let mut builds: HashMap<PathBuf, PathBuf> = HashMap::new();

    for path in sources.iter().chain(&plain) {
        let rel = path.strip_prefix(&input).unwrap_or(path).to_path_buf();

        if exclude.is_match(&rel) {
            continue;
        }

        let out_rel = output_for(&rel).unwrap_or_else(|| rel.with_extension("luau"));

        match builds.get(&out_rel) {
            Some(owner) => report.diagnostics.push((
                rel.clone(),
                Diagnostic {
                    start: 0,
                    end: 0,
                    message: format!(
                        "{} and {} both build {}; rename one",
                        shown(&rel),
                        shown(owner),
                        shown(&out_rel)
                    ),
                },
            )),

            None => {
                builds.insert(out_rel, rel);
            }
        }
    }

    // A plain `.luau` or `.lua` beside the sources goes to the output as
    // it is, so a `require("./other")` from emitted code finds it there.
    if write {
        for path in &plain {
            let rel = path.strip_prefix(&input).unwrap_or(path).to_path_buf();

            if exclude.is_match(&rel) {
                continue;
            }

            let target = out.join(&rel);
            expected.insert(target.clone());
            let text = std::fs::read(path)?;

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }

            if std::fs::read(&target).ok().as_deref() != Some(text.as_slice()) {
                std::fs::write(&target, &text)?;
            }

            report.copied.push(rel);
        }
    }

    // Extensions are project wide: a call by an extension name routes
    // through the dispatcher in every file, so the set comes first.
    let mut base_options = base_options;
    let mut alloy_sources: Vec<String> = Vec::new();

    for path in &sources {
        if path.extension().is_some_and(|e| e == "aly")
            && let Ok(source) = std::fs::read_to_string(path)
        {
            base_options
                .extensions
                .extend(crate::extensions::collect(&source));
            alloy_sources.push(source);
        }
    }

    // An `impl` on a struct another file declares attaches at run time
    // through the require. The declaring file's check artifact declares
    // the methods, so the type follows, and the privacy lint reads which
    // of them the impl keeps to itself.
    let project = crate::extensions::project_impls(&alloy_sources);
    base_options.foreign_impls = project.methods;
    base_options.foreign_privates = project.privates;

    // A `.d.aly` declares a name with no module behind it, so a check
    // that asks whether a name exists reads the list.
    let mut ambient_names: Vec<String> = Vec::new();
    // The file each name came from. A second declaration of one name
    // wins silently, and the reader then meets the wrong type at a
    // call, so the build reports the clash at the second declaration.
    let mut ambient_from: HashMap<String, PathBuf> = HashMap::new();

    for path in &sources {
        let rel = path.strip_prefix(&input).unwrap_or(path).to_path_buf();

        if exclude.is_match(&rel) || !rel.to_string_lossy().ends_with(".d.aly") {
            continue;
        }

        if let Ok(text) = std::fs::read_to_string(path) {
            for d in crate::declarations::summaries(&text, true) {
                if let Some(first) = ambient_from.get(&d.name) {
                    report.diagnostics.push((
                        rel.clone(),
                        Diagnostic {
                            start: d.offset as u32,
                            end: (d.offset + d.name.len()) as u32,
                            message: format!(
                                "`{}` is already declared in {}; a declare name is global, so one name declares once",
                                d.name,
                                first.display()
                            ),
                        },
                    ));

                    continue;
                }

                ambient_from.insert(d.name.clone(), rel.clone());
                ambient_names.push(d.name);
            }
        }
    }

    // `[alx]` in alloy.toml, or a `luaux.toml` beside it, picks the UI
    // library for `.alx`.
    let jsx_config = config.markup(root);
    // A table that does not load is one mistake, in the file that holds
    // it, however many `.alx` files it stops.
    let mut markup_reported = false;

    // An alias the project declares wrongly is a failure of the
    // project, not of one file. The path is absolute, so the report
    // names the file the alias came from.
    for problem in crate::modules::alias_problems(root, config) {
        report.failures.push((problem.file, problem.message));
    }

    // The ingots start once per build and see every file.
    let ingots = crate::ingot::Ingots::load(root, config);

    // The failure names the project's own alloy.toml. A relative path
    // would print under `[build] in`, where no alloy.toml sits.
    for p in &ingots.problems {
        report
            .failures
            .push((Config::file_of(&ingots.root), p.to_string()));
    }

    for path in sources {
        let rel = path.strip_prefix(&input).unwrap_or(&path).to_path_buf();

        if exclude.is_match(&rel) {
            report.skipped.push(rel);

            continue;
        }

        let Some(rel_out) = output_for(&rel) else {
            continue;
        };
        // The diagnostics this file adds start here; one is enough to
        // keep its output unwritten.
        let errors_before = report.diagnostics.len();

        let target = out.join(&rel_out);
        let is_alx = rel.extension().and_then(|e| e.to_str()) == Some("alx");
        let source = std::fs::read_to_string(&path)?;

        // The runtime sits at the output root; a file requires it by a
        // relative path unless the project names one. Under a mount the
        // ship names the runtime's `@game/...` path instead.
        let source_rel = build.input.join(&rel);
        let by_file = relative_require(
            &module_base(&build.out.join(&rel_out)),
            &build.out.join("alloy"),
        );
        let ship_by_tree = crate::project::std_require_for(&tree, &source_rel);
        let (std_require, ship_std_require) = match &emit.std_require {
            Some(s) => (s.clone(), None),

            // `flux` gives luau-lsp the sourcemap, and luau-lsp reads a
            // relative path in a file the sourcemap holds as a place in
            // the tree. The check artifact then names the runtime by the
            // alias the flux mirror declares, as the language server
            // does. A project an import leads into sits outside the
            // mirror's configuration and keeps the file path.
            None if keep && deps.stack.len() == 1 => {
                ("@alloy".to_string(), Some(ship_by_tree.unwrap_or(by_file)))
            }

            None => (by_file, ship_by_tree),
        };
        // A project an import leads into sits outside the sourcemap, so
        // its check artifact keeps the file path, as for the runtime.
        let mount_requires = match deps.stack.len() {
            1 => crate::project::mount_requires(&tree, &source_rel, &source),

            _ => Vec::new(),
        };
        let options = EmitOptions {
            file_name: rel.to_string_lossy().into_owned(),
            module_rel: build.out.join(&rel_out).to_string_lossy().into_owned(),
            mount_requires,
            definitions: rel.to_string_lossy().ends_with(".d.aly"),
            std_require,
            ship_std_require,
            ambient_names: ambient_names.clone(),
            ..base_options
                .clone()
                .imports(&source, &path, &module_aliases)
        };

        // `--@alloy-lint alx.<name>=<level>` sets a markup lint for
        // one file, the way it sets any other lint.
        let per_file;
        let jsx = match (&jsx_config, is_alx) {
            (Ok(c), true) => {
                match crate::directives::scan(&source)
                    .level_override("alx.static_conditional_child")
                {
                    Some(level) => {
                        per_file = {
                            let mut own = c.clone();
                            own.static_conditional_child = crate::config::markup_level(level);

                            own
                        };

                        Some(&per_file)
                    }

                    None => Some(c),
                }
            }

            (Ok(c), false) => Some(c),

            (Err(_), false) => None,

            (Err(e), true) => {
                held.insert(target.clone());
                report.skipped.push(rel);

                if !markup_reported {
                    markup_reported = true;
                    let (file, at) = config.markup_problem_at(root, e);
                    // `markup:` gives the report the MarkupError kind.
                    let message = match at {
                        Some((line, col)) => format!("{}:{}: markup: {e}", line + 1, col + 1),

                        None => format!("markup: {e}"),
                    };
                    report.failures.push((file, message));
                }

                continue;
            }
        };
        let compiled = crate::compile_file(
            &path.to_string_lossy(),
            &source,
            &options,
            jsx,
            Some(&ingots),
        );

        let mut compiled = match compiled {
            Ok(c) => c,

            Err(e) => {
                held.insert(target.clone());
                report.skipped.push(rel.clone());
                report.failures.push((rel, e.located(&source)));

                continue;
            }
        };

        // An import that leaves `in` names another project: it builds
        // first, and the require here names its output.
        let outside = deps.outside(
            &Site {
                from: &absolute(&path),
                out_file: &absolute(&target),
                input: &absolute(&input),
                root,
            },
            &compiled.imports,
            &compiled.data_refs,
            write,
            keep,
        );

        if !outside.rewrites.is_empty() {
            let map = |text: &str| {
                crate::project::map_requires(text, |p| {
                    outside
                        .rewrites
                        .iter()
                        .find(|(spec, _)| spec == p)
                        .map(|(_, to)| to.clone())
                })
            };
            compiled.ship = map(&compiled.ship);
            compiled.check = map(&compiled.check);
        }

        for d in &compiled.diagnostics {
            report.diagnostics.push((rel.clone(), d.clone()));
        }

        for l in &compiled.lints {
            report.lints.push((rel.clone(), l.clone()));
        }

        imports.push((rel.clone(), compiled.imports.clone()));

        // A module that names no file, a name the module does not
        // export, and a name imported twice: each fails at runtime, so
        // each is an error of the build, not of the type check alone.
        let silence = crate::directives::scan(&source);

        // The type check reads these lines: a module the checker cannot
        // resolve either would say the same thing a second time.
        let mut import_lines: Vec<usize> = Vec::new();
        // The lines an `--@alloy-expect-error` covers that reported, so
        // a directive left over is stale. The compile names its own;
        // every problem below counts too, silenced or not.
        let mut expect_hits = compiled.expected_hits.clone();

        // An import into another project reports once: the report that
        // names the project and its first error, over the module scan's.
        let taken: Vec<u32> = outside.problems.iter().map(|p| p.start).collect();
        let scanned = crate::modules::import_problems(&source, &source_rel, &path, &module_aliases)
            .into_iter()
            .filter(|p| !taken.contains(&p.start));

        for problem in outside.problems.into_iter().chain(scanned) {
            let at = crate::directives::line_of(&source, problem.start as usize);
            expect_hits.push(at);

            // The problem's kind is the name an `--@alloy-ignore-start`
            // may carry, so a region for `UnknownModule` silences that
            // alone.
            if !silence.allows_named(at, Some(problem.kind)) {
                continue;
            }

            import_lines.push(source[..problem.start as usize].matches('\n').count() + 1);
            report.diagnostics.push((
                rel.clone(),
                Diagnostic {
                    start: problem.start,
                    end: problem.end,
                    message: problem.message,
                },
            ));
        }

        // Every data file the source names becomes a module in the
        // output; a problem with one is a diagnostic on the literal.
        let mut data_diagnostics = Vec::new();

        for r in &compiled.data_refs {
            // A data file of another project went with the imports.
            if outside.data.contains(&r.path) {
                continue;
            }

            match data_files.module(&r.path, &rel, &source_rel, write) {
                Ok(out_rel) => {
                    expected.insert(out.join(&out_rel));

                    if !report.data.contains(&out_rel) {
                        report.data.push(out_rel);
                    }
                }

                Err(message) => data_diagnostics.push(Diagnostic {
                    start: r.start,
                    end: r.end,
                    message,
                }),
            }
        }

        if keep {
            let lint_lines = compiled
                .lints
                .iter()
                .map(|l| (source[..l.start as usize].matches('\n').count() + 1, l.name))
                .collect();
            let line = |at: u32| source[..at as usize].matches('\n').count() + 1;
            let error_lines = compiled
                .diagnostics
                .iter()
                .chain(&data_diagnostics)
                .filter(|d| !crate::alx::is_attribute_check(&d.message))
                // A match with no arm for a variant ends in a nil
                // fallthrough, and the checker reports that nil at the
                // last arm. The report on the match already says it, so
                // it covers every line of the match, as in the editor.
                .flat_map(
                    |d| match d.message.starts_with("this match is not exhaustive") {
                        true => line(d.start)..=line(d.end),

                        false => line(d.start)..=line(d.start),
                    },
                )
                .chain(import_lines)
                .collect();
            report.checks.push(crate::typecheck::CheckSource {
                rel: rel.clone(),
                source: source.clone(),
                check: compiled.check.clone(),
                map: compiled.map.clone(),
                lint_lines,
                error_lines,
                parsed_clean: compiled.parsed_clean,
                expected_hits: compiled.expected_hits.clone(),
            });
        }

        for d in data_diagnostics {
            expect_hits.push(crate::directives::line_of(&source, d.start as usize));
            report.diagnostics.push((rel.clone(), d));
        }

        // A stale `--@alloy-expect-error`. `flux` keeps the artifacts
        // and reports what is left over after the type check, which may
        // report on the line itself; a run without the checker has the
        // whole answer here.
        if !keep {
            report.diagnostics.extend(
                silence
                    .unmet_diagnostics(&source, &expect_hits)
                    .into_iter()
                    .map(|d| (rel.clone(), d)),
            );
        }

        // A `.d.aly` feeds the type check of the editor and of `flux`
        // and runs nowhere, so the output tree takes nothing from it.
        if options.definitions {
            continue;
        }

        if !write {
            report.written.push(rel_out);

            continue;
        }

        // Past its first error the parser invents the tree and the emit
        // copies the text through, so the output would hold Alloy. A
        // compile error past the parse ships the construct it reported.
        // The file produces nothing: `clean` then takes the stale output
        // a run before this one left.
        if !compiled.parsed_clean || report.diagnostics.len() > errors_before {
            held.insert(target.clone());
            report.skipped.push(rel);

            continue;
        }

        expected.insert(target.clone());

        // Roblox reads no `.luaurc`: an `@alias` require in the ship
        // artifact becomes the `@game/...` instance path.
        let ship = crate::project::rewrite_requires(&tree, &source_rel, &compiled.ship);
        let text = match build.artifact {
            Artifact::Ship => &ship,

            Artifact::Check => &compiled.check,
        };

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Skip the write when nothing changed, so rojo does not resync.
        // The count follows the bytes: a file the build leaves alone is
        // up to date, not written.
        match std::fs::read_to_string(&target).ok().as_deref() == Some(text.as_str()) {
            true => report.up_to_date.push(rel_out),

            false => {
                std::fs::write(&target, text)?;
                report.written.push(rel_out);
            }
        }
    }

    report.lints.extend(circular_imports(&imports));
    report.dep_artifacts = deps.artifacts.clone();

    // The run that started the build reports every dependency it wrote.
    if deps.stack.len() == 1 {
        report.notes.append(&mut deps.notes);
    }
    report
        .diagnostics
        .sort_by(|a, b| (&a.0, a.1.start).cmp(&(&b.0, b.1.start)));
    report
        .lints
        .sort_by(|a, b| (&a.0, a.1.start).cmp(&(&b.0, b.1.start)));

    if !write {
        return Ok(report);
    }

    // The runtime rides along with the output. It goes first: the tree
    // mounts it, so the sourcemap below names its file.
    let runtime = out.join("alloy.luau");
    expected.insert(runtime.clone());

    if std::fs::read_to_string(&runtime).ok().as_deref() != Some(crate::RUNTIME) {
        std::fs::create_dir_all(&out)?;
        std::fs::write(&runtime, crate::RUNTIME)?;
    }

    // The tree describes the Rojo project of the output and the
    // sourcemap.
    for (rel, text) in crate::project::files(&tree, config, root)? {
        let path = root.join(&rel);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&path, &text)?;
        }

        report.project_files.push(rel);
    }

    // The schema of this project, ingots and all; a `#:schema` line at
    // the top of alloy.toml points the editor at it.
    let schema_rel = PathBuf::from(".alloy/alloy.schema.json");
    let schema_path = root.join(&schema_rel);
    let schema_text = serde_json::to_string_pretty(&crate::schema::project(
        &ingots.list.iter().map(|i| &i.manifest).collect::<Vec<_>>(),
    ))
    .unwrap_or_default()
        + "\n";

    if let Some(parent) = schema_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // A rebuild with the same ingots leaves the file and says nothing.
    if std::fs::read_to_string(&schema_path).ok().as_deref() != Some(schema_text.as_str()) {
        std::fs::write(&schema_path, &schema_text)?;
        report.project_files.push(schema_rel);
    }

    if build.clean && out.is_dir() {
        let mut outputs = Vec::new();
        walk_all(&out, &mut outputs)?;

        for file in outputs {
            let is_luau = file.extension().and_then(|e| e.to_str()) == Some("luau");

            if is_luau && !expected.contains(&file) {
                std::fs::remove_file(&file)?;
                report
                    .removed
                    .push(file.strip_prefix(&out).unwrap_or(&file).to_path_buf());
            }
        }
    }

    // `clean` off keeps what no source makes, so a renamed script left
    // its old output, and Rojo ran both. The manifest lists what the
    // build wrote, and an entry no source makes now goes. A file the
    // build never wrote stays.
    let manifest = root.join(OUTPUTS);
    let before = std::fs::read_to_string(&manifest).unwrap_or_default();

    for line in before.lines() {
        let path = root.join(line);

        if expected.contains(&path) || held.contains(&path) || !path.starts_with(&out) {
            continue;
        }

        if path.is_file() {
            std::fs::remove_file(&path)?;
            report
                .removed
                .push(path.strip_prefix(&out).unwrap_or(&path).to_path_buf());
        }

        // A folder the file leaves empty goes too, else Rojo keeps an
        // empty instance of it.
        let mut dir = path.parent();

        while let Some(d) = dir
            && d != out
            && d.starts_with(&out)
            && std::fs::remove_dir(d).is_ok()
        {
            dir = d.parent();
        }
    }

    // A held output enters the manifest only when the build wrote it.
    let mut now: Vec<String> = expected
        .iter()
        .chain(
            held.iter()
                .filter(|p| before.lines().any(|l| root.join(l) == **p)),
        )
        .filter(|p| p.is_file())
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    now.sort();
    let text: String = now.iter().map(|l| format!("{l}\n")).collect();

    if text != before {
        std::fs::create_dir_all(root.join(".alloy"))?;
        std::fs::write(&manifest, text)?;
    }

    // A mount with no source yet, `src/client` before the first client
    // file, still has a place in the build project.
    for dir in tree.out_dirs(root) {
        std::fs::create_dir_all(root.join(dir))?;
    }

    Ok(report)
}

/// The projects one run builds for the imports that leave `[build] in`.
/// Each builds once, by its own `alloy.toml`, and the importer requires
/// its output.
#[derive(Default)]
pub struct Deps {
    /// By project root: where its sources and its output sit, or why
    /// it does not build.
    done: HashMap<PathBuf, Result<Dep, String>>,
    /// The roots on the way to the project that builds now. A root met
    /// twice is a cycle.
    stack: Vec<PathBuf>,
    /// The check artifacts of every project built, by absolute output
    /// path, for the checker's mirror.
    artifacts: Vec<(PathBuf, String)>,
    /// One line per project a build wrote, for the summary: the
    /// counts of its own `out`, which the project's counts leave out.
    notes: Vec<String>,
}

/// One project a build depends on: its `[build] in` and `out`,
/// absolute.
#[derive(Clone)]
struct Dep {
    input: PathBuf,
    out: PathBuf,
}

/// The file an import sits in, with what its project knows.
pub struct Site<'a> {
    /// The source, absolute.
    pub from: &'a Path,
    /// The output the build writes for the source, absolute; the
    /// rewritten require is relative to its folder.
    pub out_file: &'a Path,
    /// `[build] in` of the project, absolute.
    pub input: &'a Path,
    /// The project root, for the paths a message shows.
    pub root: &'a Path,
}

/// What the imports of one source that leave `in` came to.
#[derive(Default)]
pub struct Outside {
    /// The require path each spec becomes in the output.
    pub rewrites: Vec<(String, String)>,
    /// The specs of data files another project holds; the build of this
    /// project leaves them to that one.
    pub data: Vec<String>,
    pub problems: Vec<crate::modules::ImportProblem>,
}

impl Deps {
    /// The imports of one source that leave `input`: a file of another
    /// project builds that project first, and a file no project holds
    /// reports.
    pub fn outside(
        &mut self,
        site: &Site,
        imports: &[crate::ImportRef],
        data_refs: &[crate::ImportRef],
        write: bool,
        keep: bool,
    ) -> Outside {
        let mut out = Outside::default();
        let base = site.from.parent().unwrap_or(Path::new(""));

        for r in imports.iter().chain(data_refs) {
            let spec = &r.path;
            // The emit writes a data require without its extension.
            let key = crate::data::strip_spec(spec);

            if !(spec.starts_with("./") || spec.starts_with("../"))
                || out.rewrites.iter().any(|(s, _)| s == key)
            {
                continue;
            }

            let format = crate::data::Format::of(spec);
            let target = match format {
                Some(_) => Some(normalize_path(&base.join(spec))).filter(|p| p.is_file()),

                None => crate::modules::resolve(spec, site.from, &[]),
            };

            // A missing file is `import_problems`' report, and a plain
            // `.luau` outside `in` stays the developer's own require.
            let Some(target) = target else {
                continue;
            };
            let alloy = target.extension().is_some_and(|e| e == "aly" || e == "alx");

            if target.starts_with(site.input) || !(alloy || format.is_some()) {
                continue;
            }

            let problem = |message: String| crate::modules::ImportProblem {
                start: r.start,
                end: r.end,
                kind: "UnknownModule",
                message,
            };
            let Some(toml) = target.parent().and_then(Config::find) else {
                out.problems.push(problem(format!(
                    "\"{spec}\" is outside this project and no alloy.toml holds it; give it a project (`alloy init` there) or move it under [build] in"
                )));

                continue;
            };
            let dep_root = toml.parent().unwrap_or(Path::new("/"));
            let shown_root = relative(site.root, dep_root);
            let dep = match self.project(dep_root, write, keep) {
                Ok(dep) => dep,

                Err(e) => {
                    out.problems.push(problem(format!(
                        "\"{spec}\" is in the project at {shown_root}, which does not build: {e}"
                    )));

                    continue;
                }
            };
            let Ok(rel) = target.strip_prefix(&dep.input) else {
                out.problems.push(problem(format!(
                    "\"{spec}\" is in the project at {shown_root} but outside its [build] in; move it under {}",
                    relative(site.root, &dep.input)
                )));

                continue;
            };
            let out_rel = match format {
                Some(_) => rel.with_extension("luau"),

                None => output_for(rel).unwrap_or_else(|| rel.to_path_buf()),
            };
            let dep_out = dep.out.join(out_rel);

            // The dependency writes a data file only when a source of
            // its own names it, so the module is written from here.
            if let Some(format) = format {
                out.data.push(spec.clone());

                match self.data_module(&target, &dep_out, format, write) {
                    Ok(()) => {}

                    Err(e) => {
                        out.problems.push(problem(format!("data file {spec} {e}")));

                        continue;
                    }
                }
            }

            let from_dir = site.out_file.parent().unwrap_or(Path::new("/"));
            out.rewrites.push((
                key.to_string(),
                relative(from_dir, &dep_out.with_extension("")),
            ));
        }

        out
    }

    /// The project at `root`, built once.
    fn project(&mut self, root: &Path, write: bool, keep: bool) -> Result<Dep, String> {
        let root = absolute(root);

        if self.stack.contains(&root) {
            // The two projects, shown from the one the run started in.
            let top = self.stack.first().cloned().unwrap_or_default();
            let name = |p: &Path| match p == top {
                true => "this project".to_string(),

                false => relative(&top, p),
            };
            let from = self.stack.last().cloned().unwrap_or_default();

            return Err(format!(
                "{} and {} import each other; move the shared part into a third project",
                name(&root),
                name(&from)
            ));
        }

        if let Some(done) = self.done.get(&root) {
            return done.clone();
        }

        let result = self.build(&root, write, keep);
        self.done.insert(root, result.clone());

        result
    }

    fn build(&mut self, root: &Path, write: bool, keep: bool) -> Result<Dep, String> {
        let config = Config::load(&Config::file_of(root))
            .map_err(|e| format!("its configuration does not load: {e}"))?;
        // The importer's root, for the path a message shows.
        let importer = self.stack.last().cloned().unwrap_or_default();
        let report = run_with(root, &config, write, keep, self).map_err(|e| e.to_string())?;
        let input = absolute(&root.join(&config.build.input));
        let out = absolute(&root.join(&config.build.out));
        let shown = |rel: &Path| relative(&importer, &input.join(rel));

        // The first error is the one the importer's line names.
        if let Some((rel, d)) = report.diagnostics.first() {
            let source = std::fs::read_to_string(input.join(rel)).unwrap_or_default();
            let (line, col) = crate::directives::line_col(&source, d.start as usize);

            return Err(format!("{}:{line}:{col}: {}", shown(rel), d.message));
        }

        if let Some((rel, message)) = report.failures.first() {
            return Err(format!("{}: {message}", shown(rel)));
        }

        if write {
            let top = self.stack.first().cloned().unwrap_or_default();
            self.notes.push(format!(
                "dependency {}: {} written, {} up to date",
                relative(&top, root),
                report.written.len(),
                report.up_to_date.len()
            ));
        }

        for c in report.checks {
            if let Some(o) = output_for(&c.rel) {
                self.artifacts.push((out.join(o), c.check));
            }
        }

        self.artifacts
            .push((out.join("alloy.luau"), crate::RUNTIME.to_string()));

        Ok(Dep { input, out })
    }

    /// The module of a data file another project holds, written under
    /// that project's `out`.
    fn data_module(
        &mut self,
        path: &Path,
        target: &Path,
        format: crate::data::Format,
        write: bool,
    ) -> Result<(), String> {
        if self.artifacts.iter().any(|(p, _)| p == target) {
            return Ok(());
        }

        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let luau = crate::data::convert(&text, format)
            .map_err(|e| format!("does not parse as {}: {e}", format.name()))?;

        if write {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }

            if std::fs::read_to_string(target).ok().as_deref() != Some(luau.as_str()) {
                std::fs::write(target, &luau).map_err(|e| e.to_string())?;
            }
        }

        self.artifacts.push((target.to_path_buf(), luau));

        Ok(())
    }
}

/// The imports of one file that leave its project, for `alloy build
/// <file>` and `alloy check <file>`: the same rules as a project build,
/// with `out_file` where the output goes, else where the project build
/// would write it. A file under no `alloy.toml` has no project to leave.
pub fn file_outside(
    path: &Path,
    out_file: Option<&Path>,
    imports: &[crate::ImportRef],
    data_refs: &[crate::ImportRef],
    write: bool,
) -> Outside {
    let from = absolute(path);
    let Some((root, config)) = from
        .parent()
        .and_then(Config::find)
        .and_then(|toml| Some((toml.parent()?.to_path_buf(), Config::load(&toml).ok()?)))
    else {
        return Outside::default();
    };
    let input = absolute(&root.join(&config.build.input));
    let out_file = match out_file {
        Some(o) => absolute(o),

        None => match from.strip_prefix(&input) {
            Ok(rel) => absolute(&root.join(&config.build.out))
                .join(output_for(rel).unwrap_or_else(|| rel.to_path_buf())),

            Err(_) => from.with_extension("luau"),
        },
    };

    // The file's project starts the stack, as a project build does.
    let mut deps = Deps::default();
    deps.stack.push(absolute(&root));

    deps.outside(
        &Site {
            from: &from,
            out_file: &out_file,
            input: &input,
            root: &root,
        },
        imports,
        data_refs,
        write,
        false,
    )
}

/// The `[build] in` of every project a source of this one imports
/// into, absolute, for watch mode.
pub fn dependency_inputs(root: &Path, config: &Config) -> Vec<PathBuf> {
    let input = absolute(&root.join(&config.build.input));
    let mut out: Vec<PathBuf> = Vec::new();

    for path in sources(&input, &written_dirs(root, config)).unwrap_or_default() {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };

        for target in crate::modules::import_targets_for_file(&path, &source) {
            if target.starts_with(&input) {
                continue;
            }

            let dep = target
                .parent()
                .and_then(Config::find)
                .and_then(|toml| Some((toml.parent()?.to_path_buf(), Config::load(&toml).ok()?)))
                .map(|(dep_root, c)| absolute(&dep_root.join(&c.build.input)));

            if let Some(dep) = dep
                && !out.contains(&dep)
            {
                out.push(dep);
            }
        }
    }

    out
}

/// A path made absolute against the working directory, with `.` and
/// `..` folded.
fn absolute(path: &Path) -> PathBuf {
    normalize_path(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// `to` as a path relative to the folder `from`, with `/` between the
/// parts, the way a require spells it: `./x` beside, `../x` above.
fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];

    if parts.is_empty() {
        parts.push(".".to_string());
    }

    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );

    parts.join("/")
}

/// The data files of one build: each converts once, and every source
/// that names it gets the same answer.
struct DataFiles {
    /// `[build] in`, absolute and as written, for reads and messages.
    input: PathBuf,
    input_rel: PathBuf,
    out: PathBuf,
    /// The output path each source, plain file, and data file claims,
    /// by the file that claims it, relative to `in`.
    owners: HashMap<PathBuf, PathBuf>,
    /// The outcome per data file, relative to `in`: the output path, or
    /// the diagnostic.
    done: HashMap<PathBuf, Result<PathBuf, String>>,
    /// Alias to the folder it names, absolute, from the Luau
    /// configuration of the root.
    aliases: Vec<(String, PathBuf)>,
}

/// A path with `.` and `..` folded, no file system access.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                out.pop();
            }

            other => out.push(other),
        }
    }

    out
}

impl DataFiles {
    /// The output module for a data spec named from a source, or the
    /// message for the import.
    fn module(
        &mut self,
        spec: &str,
        from: &Path,
        source_rel: &Path,
        write: bool,
    ) -> Result<PathBuf, String> {
        let Some(format) = crate::data::Format::of(spec) else {
            return Err(format!("data file \"{spec}\" is neither .json nor .toml"));
        };

        // `@alias/x.json` resolves through `.config.luau` or `.luaurc`;
        // the folder must sit under `in`, since the build writes there.
        let rel = if let Some(rest) = spec.strip_prefix('@') {
            let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let Some((_, dir)) = self.aliases.iter().find(|(a, _)| a == alias) else {
                return Err(format!(
                    "data file \"{spec}\" names no alias {alias} in alloy.toml's [mount] table, .config.luau, or .luaurc"
                ));
            };
            let abs = normalize_path(&dir.join(tail));

            match abs.strip_prefix(&self.input) {
                Ok(rel) => rel.to_path_buf(),

                Err(_) => {
                    return Err(format!(
                        "data file \"{spec}\" lies outside [build] in; move it under the source root"
                    ));
                }
            }
        } else {
            if !(spec.starts_with("./") || spec.starts_with("../")) {
                return Err(format!(
                    "data file \"{spec}\" needs a relative path, `./` or `../`, or an alias, `@shared/`"
                ));
            }

            let Some(rel) = data_path(from, spec) else {
                return Err(format!(
                    "data file \"{spec}\" lies outside [build] in; move it under the source root"
                ));
            };

            rel
        };

        if let Some(done) = self.done.get(&rel) {
            return done.clone();
        }

        let result = self.convert(&rel, spec, format, source_rel, write);
        self.done.insert(rel, result.clone());

        result
    }

    fn convert(
        &mut self,
        rel: &Path,
        spec: &str,
        format: crate::data::Format,
        source_rel: &Path,
        write: bool,
    ) -> Result<PathBuf, String> {
        let shown = |p: &Path| self.input_rel.join(p).to_string_lossy().replace('\\', "/");
        let path = self.input.join(rel);

        if !path.is_file() {
            return Err(crate::typecheck::unknown_module_message(
                spec, source_rel, None,
            ));
        }

        let out_rel = rel.with_extension("luau");

        if let Some(owner) = self.owners.get(&out_rel) {
            return Err(format!(
                "data file {} and {} both build {}; rename one",
                shown(rel),
                shown(owner),
                shown(&out_rel)
            ));
        }

        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("data file {}: {e}", shown(rel)))?;
        let luau = crate::data::convert(&text, format).map_err(|e| {
            format!(
                "data file {} does not parse as {}: {e}",
                shown(rel),
                format.name()
            )
        })?;
        self.owners.insert(out_rel.clone(), rel.to_path_buf());

        if write {
            let target = self.out.join(&out_rel);

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }

            if std::fs::read_to_string(&target).ok().as_deref() != Some(luau.as_str()) {
                std::fs::write(&target, &luau).map_err(|e| e.to_string())?;
            }
        }

        Ok(out_rel)
    }
}

/// The path under `in` a relative data spec names from a source, or
/// none when it climbs out of `in`.
fn data_path(from: &Path, spec: &str) -> Option<PathBuf> {
    let base = from.parent().unwrap_or(Path::new(""));
    let mut joined = PathBuf::new();

    for c in base.join(spec).components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                if !joined.pop() {
                    return None;
                }
            }

            other => joined.push(other),
        }
    }

    Some(joined)
}

/// The source a relative import of `from` names, among the sources:
/// `./x` beside it, `../x` above, with `.aly`, `.alx`, or `init.aly`.
fn resolve_import(from: &Path, path: &str, sources: &[PathBuf]) -> Option<PathBuf> {
    if !(path.starts_with("./") || path.starts_with("../")) {
        return None;
    }

    // A data or Luau file is no source. `with_extension` below would
    // read `./data.json` as `./data.aly`, the module beside it.
    if crate::data::Format::of(path).is_some() || path.ends_with(".luau") || path.ends_with(".lua")
    {
        return None;
    }

    let base = from.parent().unwrap_or(Path::new(""));
    let mut joined = PathBuf::new();

    for c in base.join(path).components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                joined.pop();
            }

            other => joined.push(other),
        }
    }

    for ext in ["aly", "alx"] {
        let candidate = joined.with_extension(ext);

        if sources.contains(&candidate) {
            return Some(candidate);
        }

        let init = joined.join(format!("init.{ext}"));

        if sources.contains(&init) {
            return Some(init);
        }
    }

    None
}

/// `circular_import`: an import that leads back to the file it sits in.
/// Each file on the cycle reports the import that starts it.
fn circular_imports(imports: &[(PathBuf, Vec<crate::ImportRef>)]) -> Vec<(PathBuf, Lint)> {
    let sources: Vec<PathBuf> = imports.iter().map(|(p, _)| p.clone()).collect();
    let mut edges: Vec<(usize, &crate::ImportRef, usize)> = Vec::new();

    for (i, (from, list)) in imports.iter().enumerate() {
        for im in list {
            if let Some(to) = resolve_import(from, &im.path, &sources)
                && let Some(j) = sources.iter().position(|s| *s == to)
            {
                edges.push((i, im, j));
            }
        }
    }

    // Whether `to` reaches `from` along the edges.
    let reaches = |start: usize, goal: usize| {
        let mut seen = vec![false; sources.len()];
        let mut stack = vec![start];

        while let Some(n) = stack.pop() {
            if n == goal {
                return true;
            }

            if std::mem::replace(&mut seen[n], true) {
                continue;
            }

            stack.extend(edges.iter().filter(|(a, _, _)| *a == n).map(|(_, _, b)| *b));
        }

        false
    };

    edges
        .iter()
        .filter(|(from, _, to)| reaches(*to, *from))
        .map(|(from, im, to)| {
            (
                sources[*from].clone(),
                Lint {
                    name: "circular_import",
                    start: im.start,
                    end: im.end,
                    message: format!(
                        "`{}` imports `{}`, which imports it back; move the shared part into a third module",
                        sources[*from].display(),
                        sources[*to].display()
                    ),
                    fix: None,
                },
            )
        })
        .collect()
}

pub fn globs(patterns: &[String]) -> std::io::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();

    for p in patterns {
        let g = Glob::new(p).map_err(|e| std::io::Error::other(format!("exclude {p:?}: {e}")))?;
        b.add(g);
    }

    b.build().map_err(std::io::Error::other)
}

/// The directories the build writes, as normalized paths: the output
/// folder and the spec folder. A source walk skips them. Without this a
/// project whose `out` sits under `in` reads its own output back and
/// nests one level deeper on every run.
pub fn written_dirs(root: &Path, config: &Config) -> Vec<PathBuf> {
    vec![
        normalize_path(&root.join(&config.build.out)),
        normalize_path(&root.join(&config.test.out)),
    ]
}

/// A directory the source walks leave alone: a dot directory, a package
/// store, a build tree, a directory the build writes, or a nested
/// project with an `alloy.toml` of its own, which builds on its own.
fn skipped_dir(path: &Path, top: bool, written: &[PathBuf]) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    (name.starts_with('.') && name != ".ember")
        || matches!(name.as_str(), "node_modules" | "target")
        || (!top && written.contains(&normalize_path(path)))
        || (!top && Config::file_in(path).is_some())
}

/// Every plain Luau file under a directory, recursively: what a require
/// from emitted code may name beside the sources.
pub fn walk_plain(dir: &Path, written: &[PathBuf], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    fn go(
        dir: &Path,
        top: bool,
        written: &[PathBuf],
        out: &mut Vec<PathBuf>,
    ) -> std::io::Result<()> {
        if !dir.is_dir() || skipped_dir(dir, top, written) {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_dir() {
                go(&path, false, written, out)?;
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("luau" | "lua")
            ) {
                out.push(path);
            }
        }

        Ok(())
    }

    go(dir, true, written, out)?;
    out.sort();

    Ok(())
}

/// Every `.json` and `.toml` file under a directory, recursively: what
/// a data import may name.
pub fn walk_data(dir: &Path, written: &[PathBuf], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    fn go(
        dir: &Path,
        top: bool,
        written: &[PathBuf],
        out: &mut Vec<PathBuf>,
    ) -> std::io::Result<()> {
        if !dir.is_dir() || skipped_dir(dir, top, written) {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_dir() {
                go(&path, false, written, out)?;
            } else if crate::data::Format::of_path(&path).is_some()
                && !crate::data::is_project_file(&path)
            {
                out.push(path);
            }
        }

        Ok(())
    }

    go(dir, true, written, out)?;
    out.sort();

    Ok(())
}

/// The definitions a project loads: every `.d.aly` under `[build] in`
/// that `exclude` keeps, then each `[flux] definitions` entry. The
/// editor and `flux` read this one list, so both see the same globals.
pub fn definition_files(root: &Path, config: &Config) -> Vec<PathBuf> {
    let input = root.join(&config.build.input);
    let exclude = globs(&config.build.exclude).unwrap_or_default();
    let mut sources = Vec::new();
    let _ = walk(&input, &written_dirs(root, config), &mut sources);
    sources.sort();

    let mut out: Vec<PathBuf> = sources
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".d.aly"))
        .filter(|p| !exclude.is_match(p.strip_prefix(&input).unwrap_or(p)))
        .collect();

    for d in &config.flux.definitions {
        let path = normalize_path(&root.join(d));

        if !out.iter().any(|p| normalize_path(p) == path) {
            out.push(path);
        }
    }

    out
}

/// Every Alloy source under a directory, recursively.
fn walk(dir: &Path, written: &[PathBuf], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    fn go(
        dir: &Path,
        top: bool,
        written: &[PathBuf],
        out: &mut Vec<PathBuf>,
    ) -> std::io::Result<()> {
        if !dir.is_dir() || skipped_dir(dir, top, written) {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_dir() {
                go(&path, false, written, out)?;
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("aly" | "alx")
            ) {
                out.push(path);
            }
        }

        Ok(())
    }

    go(dir, true, written, out)
}

fn walk_all(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            walk_all(&path, out)?;
        } else {
            out.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cycle_of_imports_is_a_lint_on_each_file() {
        let im = |path: &str| crate::ImportRef {
            start: 0,
            end: 1,
            path: path.to_string(),
        };
        let imports = vec![
            (PathBuf::from("a.aly"), vec![im("./b")]),
            (PathBuf::from("b.aly"), vec![im("./c")]),
            (PathBuf::from("c.aly"), vec![im("./a"), im("./d")]),
            (PathBuf::from("d.aly"), vec![]),
        ];
        let lints = circular_imports(&imports);
        let files: Vec<String> = lints.iter().map(|(p, _)| p.display().to_string()).collect();
        assert_eq!(files, vec!["a.aly", "b.aly", "c.aly"]);
        assert!(lints[0].1.message.contains("`a.aly` imports `b.aly`"));

        // `data.aly` beside `data.json`: an import of the data file is
        // no import of the module, so there is no cycle to report.
        let data = vec![(
            PathBuf::from("data.aly"),
            vec![im("./data.json"), im("./data.luau")],
        )];
        assert!(circular_imports(&data).is_empty());
    }

    #[test]
    fn a_walk_skips_the_output_folder_under_the_input() {
        let dir = std::env::temp_dir().join(format!("alloy-out-in-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("main.aly"), "").unwrap();
        std::fs::write(dir.join("build/main.luau"), "").unwrap();
        std::fs::write(dir.join("tests/main.spec.luau"), "").unwrap();
        let written = vec![dir.join("build"), dir.join("tests")];
        let mut plain = Vec::new();
        walk_plain(&dir, &written, &mut plain).unwrap();
        assert!(plain.is_empty(), "{plain:?}");
        let mut list = Vec::new();
        walk(&dir, &written, &mut list).unwrap();
        assert_eq!(list, vec![dir.join("main.aly")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_data_path_stays_under_the_input() {
        assert_eq!(
            data_path(Path::new("a/main.aly"), "./data.json"),
            Some(PathBuf::from("a/data.json"))
        );
        assert_eq!(
            data_path(Path::new("a/b/main.aly"), "../cfg.toml"),
            Some(PathBuf::from("a/cfg.toml"))
        );
        assert_eq!(data_path(Path::new("main.aly"), "../cfg.toml"), None);
    }

    #[test]
    fn output_names_follow_the_source() {
        assert_eq!(
            output_for(Path::new("a/b.aly")),
            Some(PathBuf::from("a/b.luau"))
        );
        assert_eq!(
            output_for(Path::new("t.d.aly")),
            Some(PathBuf::from("t.d.luau"))
        );
        assert_eq!(
            output_for(Path::new("ui.alx")),
            Some(PathBuf::from("ui.luau"))
        );
        assert_eq!(output_for(Path::new("notes.md")), None);
    }
}
