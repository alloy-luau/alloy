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

/// The relative require path from the directory of `from` to `to`,
/// both relative to one root: `./x` for a sibling, `../` per level up.
fn relative_require(from: &Path, to: &Path) -> String {
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
    let jsx = config.markup(root).ok();
    let runtime = modules.join("alloy");
    let tree = crate::project::Tree::load(root, config);
    let aliases = crate::modules::aliases(root, &tree);

    for path in crate::build::sources(&input)? {
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
            import_types: crate::modules::import_types(&source, &path, &aliases),
            import_enums: crate::modules::import_enums(&source, &path, &aliases),
            import_privates: crate::modules::import_privates(&source, &path, &aliases),
            import_attributes: crate::modules::import_attributes(&source, &path, &aliases),
            plain_modules: crate::modules::plain_modules(&source, &path, &aliases),
            import_result_asyncs: crate::modules::import_result_asyncs(&source, &path, &aliases),
            import_trait_defaults: crate::modules::import_trait_defaults(&source, &path, &aliases),
            ..EmitOptions::default()
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
                    text = with_shim(&text, &relative_require(&module_rel, &modules.join("shim")));
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
    crate::build::walk_plain(&input, &mut plain)?;

    for path in plain {
        let rel = path.strip_prefix(&input).unwrap_or(&path);
        let target = dir.join(rel);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::copy(&path, target)?;
    }

    let mut data = Vec::new();
    crate::build::walk_data(&input, &mut data)?;

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
fn rewrite_requires(
    config: &Config,
    tree: &crate::project::Tree,
    root: &Path,
    source_rel: &Path,
    spec_rel: &Path,
    text: &str,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(i) = rest.find("require(") {
        let after = &rest[i + "require(".len()..];
        let quote = after.chars().next();

        if !matches!(quote, Some('"' | '\'')) {
            out.push_str(&rest[..i + "require(".len()]);
            rest = after;

            continue;
        }

        let q = quote.unwrap_or('"');
        let body = &after[1..];
        let Some(end) = body.find(q) else {
            out.push_str(&rest[..i + "require(".len()]);
            rest = after;

            continue;
        };
        let path = &body[..end];
        let replaced = target_for(config, tree, root, source_rel, path)
            .map(|target| relative_require(spec_rel, &target));

        out.push_str(&rest[..i + "require(".len()]);
        out.push(q);
        out.push_str(replaced.as_deref().unwrap_or(path));
        out.push(q);
        rest = &body[end + 1..];
    }

    out.push_str(rest);
    out
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
        import_types: crate::modules::import_types(source, &root.join(source_rel), &aliases),
        import_enums: crate::modules::import_enums(source, &root.join(source_rel), &aliases),
        import_privates: crate::modules::import_privates(source, &root.join(source_rel), &aliases),
        import_attributes: crate::modules::import_attributes(
            source,
            &root.join(source_rel),
            &aliases,
        ),
        plain_modules: crate::modules::plain_modules(source, &root.join(source_rel), &aliases),
        import_result_asyncs: crate::modules::import_result_asyncs(
            source,
            &root.join(source_rel),
            &aliases,
        ),
        import_trait_defaults: crate::modules::import_trait_defaults(
            source,
            &root.join(source_rel),
            &aliases,
        ),
        ..EmitOptions::default()
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
    let ingots = crate::ingot::Ingots::load(root, config);

    for p in &ingots.problems {
        report
            .failures
            .push((PathBuf::from(crate::config::FILE_NAME), p.to_string()));
    }

    // Extensions are project wide, as in the build: a call by an
    // extension name routes through the dispatcher in every spec.
    let mut extensions = Vec::new();

    for path in crate::build::sources(&input)? {
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

    for path in crate::build::sources(&input)? {
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

    if !write {
        return Ok(report);
    }

    // A spec whose source lost its tests goes.
    if out_dir.is_dir() {
        let mut all = Vec::new();
        walk(&out_dir, &mut all)?;

        for file in all {
            if file.to_string_lossy().ends_with(".spec.luau") && !expected.contains(&file) {
                std::fs::remove_file(&file)?;
                report
                    .removed
                    .push(file.strip_prefix(root).unwrap_or(&file).to_path_buf());
            }
        }
    }

    if config.test.lest && !report.written.is_empty() {
        let toml = root.join("lest.toml");

        if !toml.is_file() {
            std::fs::write(&toml, lest_toml(config))?;
            report
                .notes
                .push(format!("wrote lest.toml: suite `{}`", config.test.suite));
        }

        report.notes.extend(lest_alias(root)?);
    }

    Ok(report)
}

/// Adds the `@lest` alias to the root's `.luaurc`, which lest resolves
/// to the framework it writes under `.lest/core`.
fn lest_alias(root: &Path) -> std::io::Result<Vec<String>> {
    let rc = root.join(".luaurc");
    let mut notes = Vec::new();

    if !rc.is_file() {
        if crate::luau_config::has_config(root) {
            notes.push("add `lest = \".lest/core\"` to the aliases of .config.luau".to_string());

            return Ok(notes);
        }

        let c = crate::luau_config::LuauConfig {
            language_mode: Some("strict".to_string()),
            aliases: vec![("lest".to_string(), ".lest/core".to_string())],
        };
        std::fs::write(&rc, crate::luau_config::render_luaurc(&c))?;
        notes.push("wrote .luaurc: strict mode and the @lest alias".to_string());

        return Ok(notes);
    }

    let text = std::fs::read_to_string(&rc)?;
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(notes);
    };
    let Some(map) = json.as_object_mut() else {
        return Ok(notes);
    };
    let aliases = map
        .entry("aliases")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));

    if let Some(aliases) = aliases.as_object_mut()
        && !aliases.contains_key("lest")
    {
        aliases.insert(
            "lest".to_string(),
            serde_json::Value::String(".lest/core".to_string()),
        );
        let mut text = serde_json::to_string_pretty(&json).unwrap_or(text);
        text.push('\n');
        std::fs::write(&rc, text)?;
        notes.push("added @lest to .luaurc".to_string());
    }

    Ok(notes)
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
}
