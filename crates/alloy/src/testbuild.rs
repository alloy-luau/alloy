//! `alloy test`: one lest spec per source with a `@test`.
//!
//! A test lives beside the code it tests, so a source holds both. The
//! spec takes the `@test` functions and every top-level statement they
//! reach: the imports they use, the locals and functions they call, the
//! structs and impls those need. Nothing else of the module goes with
//! them, so the module's side effects stay out of the test VM. The
//! slice keeps the source's lines, so a failure points at the real one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Chunk, Expr, ImportKind, Stmt};
use alloy_syntax::lexer::{Tok, TokKind};

use crate::build::{module_base, relative_require};
use crate::config::Config;
use crate::{Diagnostic, EmitOptions};

/// What one test build did.
#[derive(Debug, Default)]
pub struct Report {
    /// The specs written, relative to the root.
    pub written: Vec<PathBuf>,
    /// The specs that would change, under `--check`.
    pub stale: Vec<PathBuf>,
    /// Stale specs removed.
    pub removed: Vec<PathBuf>,
    /// The tests found, per source.
    pub tests: usize,
    pub diagnostics: Vec<(PathBuf, Diagnostic)>,
    pub failures: Vec<(PathBuf, String)>,
    pub notes: Vec<String>,
    /// What the run set up, such as the `@lest` alias.
    pub ok: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty() && self.failures.is_empty() && self.stale.is_empty()
    }
}

/// The folder under `[test] out` that holds the modules the specs
/// require: every source compiled with file paths, the runtime, the
/// data modules, and the plain files. The build output cannot serve:
/// under a mount its requires are instance paths.
pub const MODULES: &str = ".modules";

/// The modules folder, relative to the root.
pub fn modules_dir(config: &Config) -> PathBuf {
    config.test.out.join(MODULES)
}

/// The path of the spec for a source, relative to the test folder:
/// `a/b.aly` becomes `a/b.spec.luau`.
pub fn spec_for(rel: &Path) -> Option<PathBuf> {
    let name = rel.file_name()?.to_str()?;
    let stem = name.strip_suffix(".aly")?;

    if stem.ends_with(".d") {
        return None;
    }

    Some(rel.with_file_name(format!("{stem}.spec.luau")))
}

/// What one top-level statement declares, and what it attaches to.
struct Decl {
    /// Names the statement binds at the top level.
    declares: Vec<String>,
    /// A name the statement extends: `impl X`, `function X.f`, `X.k = v`.
    /// The statement goes with the declaration of that name.
    attaches: Option<String>,
    /// A statement that declares nothing and runs for effect: a loop, a
    /// block, a call, an index assignment. It goes with the declarations
    /// it names, since a `for` that fills a table is what the table is.
    effect: bool,
    /// Every identifier inside the statement.
    refs: HashSet<String>,
    /// Whether the statement is a `@test` function.
    is_test: bool,
    /// The byte range.
    start: usize,
    end: usize,
}

fn name_of(src: &str, toks: &[Tok], span: alloy_syntax::ast::TokSpan) -> String {
    toks[span.start as usize].text(src).to_string()
}

/// The first name of an expression, for an assignment target.
fn head_name(src: &str, toks: &[Tok], e: &Expr) -> Option<(String, bool)> {
    let span = e.span();
    let first = toks[span.start as usize];

    if first.kind != TokKind::Ident {
        return None;
    }

    let plain = span.end == span.start + 1;

    Some((first.text(src).to_string(), plain))
}

/// Whether a namespace holds a `@test`, at any depth, or carries one
/// itself. Such a namespace is a test of the file: the emit puts each
/// member on the table, so the spec calls the test by its path.
fn namespace_has_test(src: &str, toks: &[Tok], ns: &alloy_syntax::ast::NamespaceDecl) -> bool {
    let tested = |attrs: &[alloy_syntax::ast::Attr]| {
        attrs.iter().any(|a| {
            a.name
                .is_some_and(|n| toks[n.start as usize].text(src) == "test")
        })
    };

    // `@test namespace Suite as ... end`: every public function of the
    // group is a test, so the whole group joins the spec.
    if tested(&ns.attributes) {
        return true;
    }

    ns.members.iter().any(|m| match &m.stmt {
        Stmt::Function(f) => tested(&f.attrs),
        Stmt::LocalFunction(f) => tested(&f.attrs),
        Stmt::Namespace(inner) => namespace_has_test(src, toks, inner),

        _ => false,
    })
}

fn describe(src: &str, toks: &[Tok], stmt: &Stmt) -> Decl {
    let span = stmt.span();
    let start = toks[span.start as usize].start as usize;
    let end = toks[(span.end as usize)
        .saturating_sub(1)
        .max(span.start as usize)]
    .end as usize;
    let mut declares = Vec::new();
    let mut attaches = None;
    let mut is_test = false;
    let has_test = |attrs: &[alloy_syntax::ast::Attr]| {
        attrs.iter().any(|a| {
            a.name
                .is_some_and(|n| toks[n.start as usize].text(src) == "test")
        })
    };

    match stmt {
        Stmt::Local(l) => {
            for b in &l.names {
                match &b.destructure {
                    Some(_) => {
                        for j in b.name.start..b.name.end {
                            let t = toks[j as usize];

                            if t.kind == TokKind::Ident {
                                declares.push(t.text(src).to_string());
                            }
                        }
                    }

                    None => declares.push(name_of(src, toks, b.name)),
                }
            }
        }

        Stmt::PatternLocal(p) => {
            for j in p.span.start..p.span.end {
                let t = toks[j as usize];

                if t.kind == TokKind::Ident && toks[j as usize + 1].text(src) != "(" {
                    declares.push(t.text(src).to_string());
                }

                if t.text(src) == "=" {
                    break;
                }
            }
        }

        Stmt::LocalFunction(f) => {
            declares.push(name_of(src, toks, f.name));
            is_test = has_test(&f.attrs);
        }

        Stmt::Function(f) => {
            is_test = has_test(&f.attrs);

            match f.path.as_slice() {
                [only] => declares.push(name_of(src, toks, *only)),

                [head, ..] => attaches = Some(name_of(src, toks, *head)),

                [] => {}
            }
        }

        Stmt::Assign(a) => {
            if let [target] = a.targets.as_slice()
                && let Some((name, plain)) = head_name(src, toks, target)
            {
                if plain {
                    declares.push(name);
                } else {
                    attaches = Some(name);
                }
            }
        }

        Stmt::Import(i) => match &i.kind {
            ImportKind::Default(n) => declares.push(name_of(src, toks, *n)),

            ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                declares.push(name_of(src, toks, *n));

                for s in specs {
                    declares.push(name_of(src, toks, s.alias.unwrap_or(s.name)));
                }
            }

            ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                for s in specs {
                    declares.push(name_of(src, toks, s.alias.unwrap_or(s.name)));
                }
            }
        },

        Stmt::Namespace(ns) => {
            declares.push(name_of(src, toks, ns.name));
            is_test = namespace_has_test(src, toks, ns);
        }

        Stmt::Struct(s) => declares.push(name_of(src, toks, s.name)),
        Stmt::Enum(e) => declares.push(name_of(src, toks, e.name)),
        Stmt::Trait(t) => declares.push(name_of(src, toks, t.name)),
        Stmt::Interface(i) => declares.push(name_of(src, toks, i.name)),
        Stmt::TypeAlias(t) => declares.push(name_of(src, toks, t.name)),
        Stmt::Remote(r) => declares.push(name_of(src, toks, r.name)),
        Stmt::Attribute(a) => declares.push(name_of(src, toks, a.name)),
        Stmt::Macro(m) => declares.push(name_of(src, toks, m.name)),
        Stmt::Class(c) => declares.push(name_of(src, toks, c.name)),
        // An impl on a foreign type, `impl string`, attaches to a name no
        // test declares; its method names are what a test reaches for.
        Stmt::Impl(i) => {
            attaches = Some(name_of(src, toks, i.target));

            for m in &i.methods {
                if let Some(last) = m.path.last() {
                    declares.push(name_of(src, toks, *last));
                }
            }
        }

        _ => {}
    }

    let effect = declares.is_empty()
        && attaches.is_none()
        && !is_test
        && matches!(
            stmt,
            Stmt::NumericFor(_)
                | Stmt::GenericFor(_)
                | Stmt::While(_)
                | Stmt::Repeat(_)
                | Stmt::Do(_)
                | Stmt::If(_)
                | Stmt::Call(..)
                | Stmt::Assign(_)
                | Stmt::Match(_)
        );

    let refs = (span.start..span.end)
        .map(|j| toks[j as usize])
        .filter(|t| t.kind == TokKind::Ident)
        .map(|t| t.text(src).to_string())
        .collect();

    Decl {
        declares,
        attaches,
        effect,
        refs,
        is_test,
        start,
        end,
    }
}

/// The source cut down to its tests and what they reach, with every
/// other top-level statement blanked so the lines stay. `None` when the
/// file has no `@test`.
pub fn slice(src: &str, toks: &[Tok], chunk: &Chunk) -> Option<String> {
    let decls: Vec<Decl> = chunk
        .block
        .stmts
        .iter()
        .map(|s| describe(src, toks, s))
        .collect();

    if !decls.iter().any(|d| d.is_test) {
        return None;
    }

    let mut selected: Vec<bool> = decls.iter().map(|d| d.is_test).collect();
    let mut needed: HashSet<String> = HashSet::new();

    for (d, s) in decls.iter().zip(&selected) {
        if *s {
            needed.extend(d.refs.iter().cloned());
        }
    }

    // A statement joins when it declares a needed name, or attaches to
    // a name a selected statement declares. Its own references join
    // the needed set, until nothing changes.
    loop {
        let declared_by_selected: HashSet<&String> = decls
            .iter()
            .zip(&selected)
            .filter(|(_, s)| **s)
            .flat_map(|(d, _)| d.declares.iter())
            .collect();
        let mut changed = false;

        for (k, d) in decls.iter().enumerate() {
            if selected[k] {
                continue;
            }

            let declares_needed = d.declares.iter().any(|n| needed.contains(n));
            let attaches_selected = d
                .attaches
                .as_ref()
                .is_some_and(|n| declared_by_selected.contains(n));
            let effect_on_selected =
                d.effect && d.refs.iter().any(|n| declared_by_selected.contains(n));

            if declares_needed || attaches_selected || effect_on_selected {
                selected[k] = true;
                needed.extend(d.refs.iter().cloned());
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    let mut out = src.as_bytes().to_vec();

    for (d, s) in decls.iter().zip(&selected) {
        if *s {
            continue;
        }

        for b in out.iter_mut().take(d.end).skip(d.start) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }

    let text = String::from_utf8(out).expect("blanking keeps UTF-8");
    let trimmed: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut joined = trimmed.join("\n");

    if text.ends_with('\n') {
        joined.push('\n');
    }

    Some(joined)
}

/// The spec's require target for a path the source requires, relative
/// to the source: the built output when the target is an Alloy file,
/// the file itself otherwise. `None` for a path that is not relative.
fn target_for(
    config: &Config,
    tree: &crate::project::Tree,
    root: &Path,
    source_rel: &Path,
    path: &str,
) -> Option<PathBuf> {
    let base = source_rel.parent().unwrap_or(Path::new(""));
    let joined = if let Some(rest) = path.strip_prefix("./") {
        base.join(rest)
    } else if path.starts_with("../") {
        base.join(path)
    } else {
        let rest = path.strip_prefix('@')?;
        let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let (_, dir) = tree.aliases.iter().find(|(a, _)| a == alias)?;

        dir.join(tail)
    };
    let normal = normalize(&joined);

    // Anything under `in` has a module under the modules folder: a
    // compiled source, a data file, or a plain file copied over.
    if let Ok(under) = normal.strip_prefix(&config.build.input) {
        let input = root.join(&config.build.input);
        let modules = modules_dir(config);

        for ext in ["aly", "alx"] {
            if input.join(under).with_extension(ext).is_file() {
                return crate::build::output_for(&under.with_extension(ext))
                    .map(|o| modules.join(o).with_extension(""));
            }
        }

        // `init.aly` names its directory.
        for ext in ["aly", "alx"] {
            if input.join(under).join(format!("init.{ext}")).is_file() {
                return Some(modules.join(under));
            }
        }

        for ext in ["json", "toml", "luau", "lua"] {
            if input.join(under).with_extension(ext).is_file() {
                return Some(modules.join(under));
            }
        }
    }

    Some(normal)
}

/// The engine names the doubles stand in for.
const SHIM_NAMES: &[&str] = &[
    "typeof",
    "Vector3",
    "Vector2",
    "CFrame",
    "Color3",
    "UDim",
    "UDim2",
    "NumberRange",
    "Enum",
    "Instance",
    "task",
    "game",
    "workspace",
    "warn",
];

/// The doubles as locals at the head of a chunk, so a module that names
/// `Vector3` or `game` at load has them. The VM gives each chunk its own
/// globals, so a global the shim set would stay in the shim. The line
/// joins the first line, so every later line keeps its number.
fn with_shim(text: &str, shim_require: &str) -> String {
    let values: Vec<String> = SHIM_NAMES.iter().map(|n| format!("__shim.{n}")).collect();

    format!(
        "local __shim = require({}) local {} = {} {text}",
        luau_string(shim_require),
        SHIM_NAMES.join(", "),
        values.join(", ")
    )
}

/// Writes the modules folder: every source compiled with file-path
/// requires, the runtime, every data file as a module, and the plain
/// files. The folder is rebuilt from nothing on each run.
fn write_modules(
    root: &Path,
    config: &Config,
    ingots: &crate::ingot::Ingots,
    extensions: &[crate::extensions::Extension],
) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut failures = Vec::new();
    let input = root.join(&config.build.input);
    let modules = modules_dir(config);
    let dir = root.join(&modules);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("alloy.luau"), crate::RUNTIME)?;
    std::fs::write(dir.join("shim.luau"), crate::SHIM)?;
    let exclude = crate::build::globs(&config.build.exclude)?;
    let written = crate::build::written_dirs(root, config);
    let jsx = config.markup(root).ok();
    let runtime = modules.join("alloy");
    let tree = crate::project::Tree::load(root, config);
    let aliases = crate::modules::aliases(root, &tree);

    for path in crate::build::sources(&input, &written)? {
        let rel = path.strip_prefix(&input).unwrap_or(&path).to_path_buf();

        if exclude.is_match(&rel) {
            continue;
        }

        let Some(rel_out) = crate::build::output_for(&rel) else {
            continue;
        };
        let module_rel = modules.join(&rel_out);
        let source = std::fs::read_to_string(&path)?;
        let source_rel = config.build.input.join(&rel);
        let options = EmitOptions {
            file_name: source_rel.to_string_lossy().into_owned(),
            definitions: rel.to_string_lossy().ends_with(".d.aly"),
            std_require: relative_require(&source_rel, &runtime),
            wait_timeout: config.emit.wait_timeout,
            extensions: extensions.to_vec(),
            ..EmitOptions::default().imports(&source, &path, &aliases)
        };
        let compiled = crate::compile_file(
            &source_rel.to_string_lossy(),
            &source,
            &options,
            jsx.as_ref(),
            Some(ingots),
        );

        match compiled {
            Ok(out) => {
                let mut text =
                    rewrite_requires(config, &tree, root, &source_rel, &module_rel, &out.ship);

                if config.test.shim {
                    text = with_shim(
                        &text,
                        &relative_require(&module_base(&module_rel), &modules.join("shim")),
                    );
                }

                let target = dir.join(&rel_out);

                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                std::fs::write(target, text)?;
            }

            Err(e) => failures.push((rel, e.to_string())),
        }
    }

    let mut plain = Vec::new();
    crate::build::walk_plain(&input, &written, &mut plain)?;

    for path in plain {
        let rel = path.strip_prefix(&input).unwrap_or(&path);
        let target = dir.join(rel);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::copy(&path, target)?;
    }

    let mut data = Vec::new();
    crate::build::walk_data(&input, &written, &mut data)?;

    for path in data {
        let rel = path.strip_prefix(&input).unwrap_or(&path);

        if crate::data::module_beside(&path).is_some() {
            continue;
        }

        if let Some(format) = crate::data::Format::of_path(&path)
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            match crate::data::convert(&text, format) {
                Ok(luau) => {
                    let target = dir.join(rel).with_extension("luau");

                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }

                    std::fs::write(target, luau)?;
                }

                Err(e) => failures.push((rel.to_path_buf(), e.to_string())),
            }
        }
    }

    Ok(failures)
}

/// A path with its `.` and `..` components folded.
fn normalize(path: &Path) -> PathBuf {
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

/// Rewrites every relative or aliased `require` of an emitted text to
/// the path from the spec to the target. The text keeps its line count.
/// A spec named `init.luau` requires from its folder, as Luau reads it.
fn rewrite_requires(
    config: &Config,
    tree: &crate::project::Tree,
    root: &Path,
    source_rel: &Path,
    spec_rel: &Path,
    text: &str,
) -> String {
    crate::project::map_requires(text, |path| {
        target_for(config, tree, root, source_rel, path)
            .map(|target| relative_require(&module_base(spec_rel), &target))
    })
}

/// The lest spec of one source: the sliced module, then the `describe`
/// that registers each test. `None` when the source has no `@test`.
pub fn spec(
    config: &Config,
    root: &Path,
    source_rel: &Path,
    source: &str,
    ingots: Option<&crate::ingot::Ingots>,
    extensions: &[crate::extensions::Extension],
) -> Result<Option<(String, Vec<Diagnostic>, usize)>, crate::CompileError> {
    let parsed = alloy_syntax::parse_lenient(source, Default::default()).map_err(|e| {
        crate::CompileError {
            offset: e.offset,
            message: e.message,
        }
    })?;
    // A source the lenient parse could not read is no spec: the tree is
    // the recovery's, not the author's, and the slice would register a
    // test the file never wrote. The diagnostics report and the caller
    // writes nothing.
    if !parsed.diagnostics.is_empty() {
        let diagnostics = parsed
            .diagnostics
            .iter()
            .map(|e| Diagnostic {
                start: e.offset as u32,
                end: e.offset as u32,
                message: e.message.clone(),
            })
            .collect();

        return Ok(Some((String::new(), diagnostics, 0)));
    }

    let Some(sliced) = slice(source, &parsed.lexed.toks, &parsed.chunk) else {
        return Ok(None);
    };
    let spec_rel = config.test.out.join(
        spec_for(
            source_rel
                .strip_prefix(&config.build.input)
                .unwrap_or(source_rel),
        )
        .unwrap_or_default(),
    );
    // The require is written from the source's place, as every other
    // require in the text, and the rewrite below moves them all.
    let runtime = modules_dir(config).join("alloy");
    let tree = crate::project::Tree::load(root, config);
    let aliases = crate::modules::aliases(root, &tree);
    let options = EmitOptions {
        file_name: source_rel.to_string_lossy().into_owned(),
        std_require: relative_require(source_rel, &runtime),
        tests: true,
        wait_timeout: config.emit.wait_timeout,
        extensions: extensions.to_vec(),
        ..EmitOptions::default().imports(source, &root.join(source_rel), &aliases)
    };
    let out = crate::compile_file(
        &source_rel.to_string_lossy(),
        &sliced,
        &options,
        None,
        ingots,
    )?;
    let mut text = rewrite_requires(config, &tree, root, source_rel, &spec_rel, &out.ship);

    if config.test.shim {
        text = with_shim(
            &text,
            &relative_require(&spec_rel, &modules_dir(config).join("shim")),
        );
    }

    let name = source_rel
        .strip_prefix(&config.build.input)
        .unwrap_or(source_rel)
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");

    if !text.ends_with('\n') {
        text.push('\n');
    }

    text.push_str("\nlocal __lest = require(\"@lest\")\n");
    // `@cfg(test)` holds while the spec runs.
    text.push_str("__alloy.set_testing(true)\n");
    text.push_str(&format!(
        "__lest.describe({}, function()\n",
        luau_string(&name)
    ));

    for (test, is_async) in &out.tests {
        if *is_async {
            text.push_str(&format!(
                "    __lest.it({}, function()\n        __alloy.await({test}())\n    end)\n",
                luau_string(test)
            ));
        } else {
            text.push_str(&format!("    __lest.it({}, {test})\n", luau_string(test)));
        }
    }

    text.push_str("end)\n");

    Ok(Some((text, out.diagnostics, out.tests.len())))
}

fn luau_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The `lest.toml` `alloy test` writes when the root has none.
pub fn lest_toml(config: &Config) -> String {
    let out = config.test.out.to_string_lossy().replace('\\', "/");

    format!(
        "# Written by `alloy test`. `lest` runs the suite; `alloy test` rewrites the specs.\n\n[suites.{}]\ninclude = [\"{out}/**/*.spec.luau\"]\n\n[settings]\nbackend = \"native\"\ntimeout_ms = 5000\n",
        config.test.suite
    )
}

/// Writes the specs of a project under `[test] out`. With `write`
/// false, nothing changes and `stale` lists the specs that would.
pub fn run(root: &Path, config: &Config, write: bool) -> std::io::Result<Report> {
    let mut report = Report::default();
    let input = root.join(&config.build.input);
    let out_dir = root.join(&config.test.out);
    let mut expected: HashSet<PathBuf> = HashSet::new();
    let exclude = crate::build::globs(&config.build.exclude)?;
    let written = crate::build::written_dirs(root, config);
    let ingots = crate::ingot::Ingots::load(root, config);

    for p in &ingots.problems {
        report
            .failures
            .push((PathBuf::from(crate::config::FILE_NAME), p.to_string()));
    }

    // Extensions are project wide, as in the build: a call by an
    // extension name routes through the dispatcher in every spec.
    let mut extensions = Vec::new();

    for path in crate::build::sources(&input, &written)? {
        if path.extension().is_some_and(|e| e == "aly")
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            extensions.extend(crate::extensions::collect(&source));
        }
    }

    if write {
        for (rel, message) in write_modules(root, config, &ingots, &extensions)? {
            report.failures.push((rel, message));
        }
    }

    for path in crate::build::sources(&input, &written)? {
        let rel = path.strip_prefix(&input).unwrap_or(&path).to_path_buf();

        if exclude.is_match(&rel) {
            continue;
        }

        let Some(spec_rel) = spec_for(&rel) else {
            continue;
        };
        let source = std::fs::read_to_string(&path)?;
        let source_rel = config.build.input.join(&rel);

        let built = match spec(
            config,
            root,
            &source_rel,
            &source,
            Some(&ingots),
            &extensions,
        ) {
            Ok(Some(b)) => b,

            Ok(None) => continue,

            Err(e) => {
                report.failures.push((rel, e.to_string()));

                continue;
            }
        };
        let (text, diagnostics, count) = built;
        report.tests += count;

        for d in diagnostics {
            report.diagnostics.push((rel.clone(), d));
        }

        let target = out_dir.join(&spec_rel);
        expected.insert(target.clone());

        // `spec` gives no text for a source that does not parse. The
        // diagnostics above count it; the spec it wrote before stays, so
        // a syntax error deletes nothing.
        if text.is_empty() {
            continue;
        }
        let shown = config.test.out.join(&spec_rel);
        let current = std::fs::read_to_string(&target).ok();

        if !write {
            if current.as_deref() != Some(text.as_str()) {
                report.stale.push(shown);
            }

            continue;
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if current.as_deref() != Some(text.as_str()) {
            std::fs::write(&target, &text)?;
        }

        report.written.push(shown);
    }

    // A spec whose source lost its tests goes. The removal is a change,
    // so `--check` counts the orphan as stale and fails on it.
    if out_dir.is_dir() {
        let mut all = Vec::new();
        walk(&out_dir, &mut all)?;

        for file in all {
            if !file.to_string_lossy().ends_with(".spec.luau") || expected.contains(&file) {
                continue;
            }

            let shown = file.strip_prefix(root).unwrap_or(&file).to_path_buf();

            if !write {
                report.stale.push(shown);

                continue;
            }

            std::fs::remove_file(&file)?;
            report.removed.push(shown);
        }
    }

    if !write {
        return Ok(report);
    }

    if config.test.lest && !report.written.is_empty() {
        let toml = root.join("lest.toml");

        if !toml.is_file() {
            std::fs::write(&toml, lest_toml(config))?;
            report
                .notes
                .push(format!("wrote lest.toml: suite `{}`", config.test.suite));
        }

        lest_alias(root, &mut report)?;
    }

    Ok(report)
}

/// The alias lest resolves to the framework it writes under
/// `.lest/core`.
const LEST_ALIAS: (&str, &str) = ("lest", ".lest/core");

/// Adds the `@lest` alias to the Luau configuration of the root, the
/// file lest and the build read. A file the editor cannot change gets
/// a note that says why.
fn lest_alias(root: &Path, report: &mut Report) -> std::io::Result<()> {
    // `.config.luau` wins over `.luaurc`, as in Luau.
    let Some(path) = [".config.luau", ".luaurc"]
        .iter()
        .map(|name| root.join(name))
        .find(|p| p.is_file())
    else {
        let c = crate::luau_config::LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: vec![(LEST_ALIAS.0.to_string(), LEST_ALIAS.1.to_string())],
        };
        std::fs::write(root.join(".luaurc"), crate::luau_config::render_luaurc(&c))?;
        report
            .notes
            .push("wrote .luaurc: strict mode and the @lest alias".to_string());

        return Ok(());
    };
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let text = std::fs::read_to_string(&path)?;
    let edited = if name == ".config.luau" {
        crate::luau_config::add_alias_config_luau(&text, LEST_ALIAS)
    } else {
        crate::luau_config::add_alias_luaurc(&text, LEST_ALIAS)
    };

    match edited {
        Ok(Some(text)) => {
            std::fs::write(&path, text)?;
            report
                .ok
                .push(format!("added {} to the aliases of {name}", LEST_ALIAS.0));
        }

        // The file carries the alias already.
        Ok(None) => {}

        Err(e) => report.notes.push(format!(
            "add `{} = \"{}\"` to the aliases of {name}: {e}",
            LEST_ALIAS.0, LEST_ALIAS.1
        )),
    }

    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sliced(src: &str) -> Option<String> {
        let parsed = alloy_syntax::parse_lenient(src, Default::default())
            .ok()
            .unwrap();

        slice(src, &parsed.lexed.toks, &parsed.chunk)
    }

    #[test]
    fn a_file_without_tests_has_no_spec() {
        assert_eq!(sliced("local x = 1\nprint(x)\n"), None);
    }

    #[test]
    fn the_slice_keeps_what_the_tests_reach_and_the_lines() {
        let src = "import { helper } from \"./util\"\nimport { other } from \"./other\"\nlocal Players = game:GetService(\"Players\")\n\nlocal function used(): number\n    return helper(1)\nend\n\nlocal function unused(): number\n    return other(2)\nend\n\nPlayers.PlayerAdded:Connect(function() end)\n\n@test\nfunction it_works()\n    $assert_eq(used(), 1)\nend\n\nreturn { used = used }\n";
        let out = sliced(src).unwrap();
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(out.contains("import { helper } from \"./util\""));
        assert!(!out.contains("other"));
        assert!(!out.contains("Players"));
        assert!(out.contains("local function used()"));
        assert!(!out.contains("unused"));
        assert!(!out.contains("return { used"));
        assert!(out.contains("function it_works()"));
    }

    #[test]
    fn an_impl_follows_its_struct() {
        let src = "struct V as\n    x: number\nend\n\nimpl V as\n    function len(self): number\n        return self.x\n    end\nend\n\nstruct W as\n    y: number\nend\n\n@test\nfunction v_len()\n    $assert_eq((new V { x = 2 }):len(), 2)\nend\n";
        let out = sliced(src).unwrap();
        assert!(out.contains("impl V"));
        assert!(!out.contains("struct W"));
    }

    /// A spec whose source lost its last `@test` is stale under
    /// `--check` and goes under `alloy test`.
    #[test]
    fn an_orphan_spec_is_stale_under_check() {
        let dir = std::env::temp_dir().join(format!("alloy-orphan-spec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::create_dir_all(dir.join("tests")).expect("the folder");
        std::fs::write(dir.join("src/main.aly"), "local x = 1\n").expect("the file");
        std::fs::write(dir.join("tests/main.spec.luau"), "return {}\n").expect("the file");

        let config = Config::default();
        let report = run(&dir, &config, false).expect("the check");

        assert_eq!(report.stale, [PathBuf::from("tests/main.spec.luau")]);
        assert!(!report.is_clean());

        let report = run(&dir, &config, true).expect("the write");

        assert_eq!(report.removed, [PathBuf::from("tests/main.spec.luau")]);
        assert!(!dir.join("tests/main.spec.luau").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `alloy test` adds the `@lest` alias to the Luau configuration
    /// the project has. A note that names the file is the last resort,
    /// not the first.
    #[test]
    fn the_lest_alias_lands_in_the_luau_configuration() {
        let dir = std::env::temp_dir().join(format!("alloy-lest-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder");

        // `.config.luau`, the file `alloy init` writes.
        let path = dir.join(".config.luau");
        std::fs::write(&path, crate::config::CONFIG_LUAU_TEMPLATE).expect("the file");

        let mut report = Report::default();
        lest_alias(&dir, &mut report).expect("the edit");
        let text = std::fs::read_to_string(&path).expect("the file");
        let back = crate::luau_config::parse_config_luau(&text).expect("the chunk");

        assert_eq!(
            back.aliases,
            vec![
                ("alloy".to_string(), "./build/alloy".to_string()),
                ("lest".to_string(), ".lest/core".to_string()),
            ],
            "{text}"
        );
        assert_eq!(report.ok, ["added lest to the aliases of .config.luau"]);
        assert!(report.notes.is_empty(), "{:?}", report.notes);

        // A second run changes nothing.
        let mut again = Report::default();
        lest_alias(&dir, &mut again).expect("the edit");
        assert_eq!(std::fs::read_to_string(&path).expect("the file"), text);
        assert!(again.ok.is_empty() && again.notes.is_empty());

        // A `.luaurc` keeps its comments and its own aliases.
        std::fs::remove_file(&path).expect("the file");
        let rc = dir.join(".luaurc");
        std::fs::write(
            &rc,
            "{\n  // ours\n  \"aliases\": { \"pkg\": \"Packages\" }\n}\n",
        )
        .expect("the file");

        let mut report = Report::default();
        lest_alias(&dir, &mut report).expect("the edit");
        let text = std::fs::read_to_string(&rc).expect("the file");

        assert!(text.contains("// ours"), "{text}");
        assert!(text.contains("\"pkg\""), "{text}");
        assert!(text.contains("\".lest/core\""), "{text}");
        assert_eq!(report.ok, ["added lest to the aliases of .luaurc"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requires_point_from_the_spec_at_the_build() {
        assert_eq!(
            relative_require(Path::new("tests/a/b.spec.luau"), Path::new("build/a/c")),
            "../../build/a/c"
        );
        assert_eq!(
            relative_require(Path::new("tests/b.spec.luau"), Path::new("build/alloy")),
            "../build/alloy"
        );
        assert_eq!(
            spec_for(Path::new("a/b.aly")),
            Some(PathBuf::from("a/b.spec.luau"))
        );
        assert_eq!(spec_for(Path::new("g.d.aly")), None);
    }

    #[test]
    fn the_spec_registers_each_test_and_awaits_the_async_ones() {
        let src = "local function f(): number\n    return 1\nend\n\n@test\nfunction plain()\n    $assert_eq(f(), 1)\nend\n\n@test\nasync function later()\n    local v = await Future.delay(0)\n    $assert(v == nil)\nend\n";
        let config = Config::default();
        let (text, diagnostics, count) = spec(
            &config,
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 2);
        assert!(
            text.contains("local __alloy = require(\"./.modules/alloy\")"),
            "{text}"
        );
        assert!(text.contains("__lest.describe(\"m\", function()"), "{text}");
        assert!(text.contains("__lest.it(\"plain\", plain)"), "{text}");
        assert!(text.contains("__alloy.await(later())"), "{text}");
        assert!(!text.contains("__alloy.test("), "{text}");
    }

    /// A namespace member renders under its own name and the table
    /// carries it, so the spec calls the test by its path.
    #[test]
    fn a_test_inside_a_namespace_registers_by_its_path() {
        let src = "namespace Suite as\n    @test\n    function ns_case()\n        $assert(1 == 1)\n    end\nend\n";
        let (text, _, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(count, 1);
        assert!(
            text.contains("__lest.it(\"Suite.ns_case\", Suite.ns_case)"),
            "{text}"
        );
        assert!(text.contains("Suite.ns_case = Suite_ns_case"), "{text}");
    }

    /// `@test` on the group makes every public function of it a test.
    /// A private member and a member that binds no function stay out,
    /// a member with its own `@test` registers once, and the flag
    /// reaches a public nested namespace.
    #[test]
    fn a_test_namespace_registers_each_public_function() {
        let src = "@test
namespace Suite as
    const LIMIT = 4

    private function helper(n: number): number
        return n * 2
    end

    function plain()
        $assert_eq(helper(2), LIMIT)
    end

    @test
    function marked()
        $assert(true)
    end

    namespace Inner as
        function deep()
            $assert(true)
        end
    end

    private namespace Hidden as
        function skipped()
            $assert(true)
        end
    end
end
";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 3);
        assert!(
            text.contains("__lest.it(\"Suite.plain\", Suite.plain)"),
            "{text}"
        );
        assert!(
            text.contains("__lest.it(\"Suite.marked\", Suite.marked)"),
            "{text}"
        );
        assert!(
            text.contains("__lest.it(\"Suite.Inner.deep\", Suite.Inner.deep)"),
            "{text}"
        );
        // A private member, a member that binds no function, and a
        // private nested namespace register nothing.
        assert!(!text.contains("__lest.it(\"Suite.helper"), "{text}");
        assert!(!text.contains("__lest.it(\"Suite.LIMIT"), "{text}");
        assert!(!text.contains("__lest.it(\"Suite.Hidden"), "{text}");
    }

    /// `$assert` lowers to the Luau `assert`, so the body needs nothing
    /// of the runtime. The spec still calls `__alloy.set_testing`.
    #[test]
    fn a_spec_requires_the_runtime_it_calls() {
        let src = "@test
local function only_assert()
    $assert(1 == 1)
end
";
        let (text, _, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/m.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(count, 1);
        assert!(
            text.contains("local __alloy = require(\"./.modules/alloy\")"),
            "{text}"
        );
    }

    /// A source that does not parse gives the recovery's tree, not the
    /// author's, and the slice read a `@test` out of it. The spec built
    /// and the run passed. The parse diagnostics now come back with no
    /// text, so the caller reports them and writes nothing.
    #[test]
    fn a_source_that_does_not_parse_writes_no_spec() {
        let src =
            "namespace A {\n    @test\n    function name()\n        $assert(1 == 1)\n    end\n}\n";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/broken.aly"),
            src,
            None,
            &[],
        )
        .unwrap()
        .unwrap();

        assert!(text.is_empty(), "{text}");
        assert_eq!(count, 0);
        assert!(!diagnostics.is_empty());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("a namespace body is `as ... end`")),
            "{:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // The same file with braces turned into `as ... end` parses, so
        // the spec builds.
        let good = "namespace A as\n    @test\n    function name()\n        $assert(1 == 1)\n    end\nend\n";
        let (text, diagnostics, count) = spec(
            &Config::default(),
            Path::new("/none"),
            Path::new("src/good.aly"),
            good,
            None,
            &[],
        )
        .unwrap()
        .unwrap();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(count, 1);
        assert!(text.contains("__lest.describe(\"good\""), "{text}");
    }
}
