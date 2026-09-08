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
    pub skipped: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Diagnostics per source, with the source path.
    pub diagnostics: Vec<(PathBuf, Diagnostic)>,
    /// Every lint that fired, by source path relative to `in`.
    pub lints: Vec<(PathBuf, Lint)>,
    pub failures: Vec<(PathBuf, String)>,
    /// The project files written for the mounts, relative to the root,
    /// and what the alias sync did.
    pub project_files: Vec<PathBuf>,
    pub notes: Vec<String>,
    /// The check artifacts, kept for the type check of `alloy flux`.
    pub checks: Vec<crate::typecheck::CheckSource>,
    /// Plain `.luau` and `.lua` files copied from `in` to `out`.
    pub copied: Vec<PathBuf>,
    /// The `.json` and `.toml` files a source names, written to `out`
    /// as `.luau` modules; the paths are relative to `out`.
    pub data: Vec<PathBuf>,
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

/// Runs a build from the project root.
pub fn run(root: &Path, build: &Build, emit: &Emit) -> std::io::Result<Report> {
    let config = Config {
        build: build.clone(),
        emit: emit.clone(),
        ..Config::default()
    };

    run_with(root, &config, true, false)
}

/// The build of a whole config: the mounts write the project files and
/// route the requires.
pub fn run_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, true, false)
}

/// `check` for a whole config.
pub fn check_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, false, false)
}

/// `flux` for a whole config: the check, with the artifacts kept for
/// the analyzer.
pub fn flux_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, false, true)
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

    run_with(root, &config, false, false)
}

/// The Alloy sources under `input`, sorted.
pub fn sources(input: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut list = Vec::new();
    walk(input, &mut list)?;
    list.sort();

    Ok(list)
}

fn run_with(root: &Path, config: &Config, write: bool, keep: bool) -> std::io::Result<Report> {
    let build = &config.build;
    let emit = &config.emit;
    let base_options = EmitOptions {
        wait_timeout: emit.wait_timeout,
        std_require: emit
            .std_require
            .clone()
            .unwrap_or_else(|| "@alloy".to_string()),
        thresholds: config.flux.thresholds(),
        ..EmitOptions::default()
    };
    let input = root.join(&build.input);
    let out = root.join(&build.out);
    let exclude = globs(&build.exclude)?;
    let mut report = Report::default();
    let mut expected: HashSet<PathBuf> = HashSet::new();
    let mut imports: Vec<(PathBuf, Vec<crate::ImportRef>)> = Vec::new();

    let mut sources = Vec::new();
    walk(&input, &mut sources)?;
    sources.sort();
    let mut plain = Vec::new();
    walk_plain(&input, &mut plain)?;

    // A data file builds beside the modules; one that would build to
    // the same `.luau` as a source or a plain file is a diagnostic.
    // An alias in a data path resolves through the mount table and the
    // root's Luau configuration, to a folder under `in`.
    let mut aliases: Vec<(String, PathBuf)> = config
        .mount
        .iter()
        .map(|(alias, m)| (alias.clone(), normalize_path(&root.join(&m.0))))
        .collect();

    if let Some((_, luau)) = crate::luau_config::read_dir(root) {
        for (alias, target) in luau.aliases {
            if !aliases.iter().any(|(a, _)| *a == alias) {
                aliases.push((alias, normalize_path(&root.join(target))));
            }
        }
    }

    let mut data_files = DataFiles {
        input: normalize_path(&input),
        input_rel: build.input.clone(),
        out: out.clone(),
        owners: HashMap::new(),
        done: HashMap::new(),
        aliases,
    };

    for path in sources.iter().chain(&plain) {
        let rel = path.strip_prefix(&input).unwrap_or(path).to_path_buf();
        let out_rel = output_for(&rel).unwrap_or_else(|| rel.clone());
        data_files.owners.entry(out_rel).or_insert(rel);
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

    for path in &sources {
        if path.extension().is_some_and(|e| e == "aly")
            && let Ok(source) = std::fs::read_to_string(path)
        {
            base_options
                .extensions
                .extend(crate::extensions::collect(&source));
        }
    }

    // `[alx]` in alloy.toml, or a `luaux.toml` beside it, picks the UI
    // library for `.alx`.
    let jsx_config = config.markup(root);
    let module_aliases = crate::modules::aliases(root, config);

    // The ingots start once per build and see every file.
    let ingots = crate::ingot::Ingots::load(root, config);

    for p in &ingots.problems {
        report
            .failures
            .push((PathBuf::from(crate::config::FILE_NAME), p.to_string()));
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

        let target = out.join(&rel_out);
        expected.insert(target.clone());

        let is_alx = rel.extension().and_then(|e| e.to_str()) == Some("alx");
        let source = std::fs::read_to_string(&path)?;

        // The runtime sits at the output root; a file requires it by a
        // relative path unless the project names one. Under a mount the
        // ship names the runtime's `@game/...` path instead.
        let depth = rel.components().count().saturating_sub(1);
        let source_rel = build.input.join(&rel);
        let by_file = if depth == 0 {
            "./alloy".to_string()
        } else {
            format!("{}alloy", "../".repeat(depth))
        };
        let (std_require, ship_std_require) = match &emit.std_require {
            Some(s) => (s.clone(), None),

            None => (
                by_file,
                crate::project::std_require_for(config, &source_rel),
            ),
        };
        let options = EmitOptions {
            file_name: rel.to_string_lossy().into_owned(),
            definitions: rel.to_string_lossy().ends_with(".d.aly"),
            std_require,
            ship_std_require,
            import_types: crate::modules::import_types(&source, &path, &module_aliases),
            import_result_asyncs: crate::modules::import_result_asyncs(
                &source,
                &path,
                &module_aliases,
            ),
            import_trait_defaults: crate::modules::import_trait_defaults(
                &source,
                &path,
                &module_aliases,
            ),
            ..base_options.clone()
        };

        let jsx = match (&jsx_config, is_alx) {
            (Ok(c), _) => Some(c),

            (Err(_), false) => None,

            (Err(e), true) => {
                report.failures.push((rel, e.clone()));

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

        let compiled = match compiled {
            Ok(c) => c,

            Err(e) => {
                report.failures.push((rel, e.to_string()));

                continue;
            }
        };

        for d in &compiled.diagnostics {
            report.diagnostics.push((rel.clone(), d.clone()));
        }

        for l in &compiled.lints {
            report.lints.push((rel.clone(), l.clone()));
        }

        imports.push((rel.clone(), compiled.imports.clone()));

        // Every data file the source names becomes a module in the
        // output; a problem with one is a diagnostic on the literal.
        let mut data_diagnostics = Vec::new();

        for r in &compiled.data_refs {
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
            let unused_lines = compiled
                .lints
                .iter()
                .filter(|l| l.name == "unused_variable" || l.name == "unused_function")
                .map(|l| source[..l.start as usize].matches('\n').count() + 1)
                .collect();
            let error_lines = compiled
                .diagnostics
                .iter()
                .chain(&data_diagnostics)
                .map(|d| source[..d.start as usize].matches('\n').count() + 1)
                .collect();
            report.checks.push(crate::typecheck::CheckSource {
                rel: rel.clone(),
                source: source.clone(),
                check: compiled.check.clone(),
                map: compiled.map.clone(),
                unused_lines,
                error_lines,
                expected_hits: compiled.expected_hits.clone(),
            });
        }

        for d in data_diagnostics {
            report.diagnostics.push((rel.clone(), d));
        }

        if !write {
            report.written.push(rel_out);

            continue;
        }

        // Roblox reads no `.luaurc`: an `@alias` require in the ship
        // artifact becomes the `@game/...` instance path.
        let ship = crate::project::rewrite_requires(config, &source_rel, &compiled.ship);
        let text = match build.artifact {
            Artifact::Ship => &ship,

            Artifact::Check => &compiled.check,
        };

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Skip the write when nothing changed, so rojo does not resync.
        if std::fs::read_to_string(&target).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&target, text)?;
        }

        report.written.push(rel_out);
    }

    report.lints.extend(circular_imports(&imports));
    report
        .lints
        .sort_by(|a, b| (&a.0, a.1.start).cmp(&(&b.0, b.1.start)));

    if !write {
        return Ok(report);
    }

    // The mounts describe the Rojo projects and the sourcemap.
    for (rel, text) in crate::project::files(config, root)? {
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

    if std::fs::read_to_string(&schema_path).ok().as_deref() != Some(schema_text.as_str()) {
        std::fs::write(&schema_path, &schema_text)?;
    }

    report.project_files.push(schema_rel);
    report.notes = crate::project::sync_aliases(config, root)?;

    // The runtime rides along with the output.
    let runtime = out.join("alloy.luau");
    expected.insert(runtime.clone());

    if std::fs::read_to_string(&runtime).ok().as_deref() != Some(crate::RUNTIME) {
        std::fs::create_dir_all(&out)?;
        std::fs::write(&runtime, crate::RUNTIME)?;
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

    Ok(report)
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
    /// Alias to the folder it names, absolute: the mounts, then `.luaurc`.
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

        // `@alias/x.json` resolves through the mounts or `.luaurc`; the
        // folder must sit under `in`, since the build writes there.
        let rel = if let Some(rest) = spec.strip_prefix('@') {
            let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let Some((_, dir)) = self.aliases.iter().find(|(a, _)| a == alias) else {
                return Err(format!(
                    "data file \"{spec}\" names no alias @{alias} in the [mount] table or .luaurc"
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
            return Err(crate::typecheck::unknown_module_message(spec, source_rel));
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

/// A directory the source walks leave alone: a dot directory, a package
/// store, a build tree, or a nested project with an `alloy.toml` of its
/// own, which builds on its own.
fn skipped_dir(path: &Path, top: bool) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    (name.starts_with('.') && name != ".ember")
        || matches!(name.as_str(), "node_modules" | "target")
        || (!top && path.join(crate::config::FILE_NAME).is_file())
}

/// Every plain Luau file under a directory, recursively: what a require
/// from emitted code may name beside the sources.
pub fn walk_plain(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    fn go(dir: &Path, top: bool, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if !dir.is_dir() || skipped_dir(dir, top) {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_dir() {
                go(&path, false, out)?;
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("luau" | "lua")
            ) {
                out.push(path);
            }
        }

        Ok(())
    }

    go(dir, true, out)?;
    out.sort();

    Ok(())
}

/// Every `.json` and `.toml` file under a directory, recursively: what
/// a data import may name.
pub fn walk_data(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    fn go(dir: &Path, top: bool, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if !dir.is_dir() || skipped_dir(dir, top) {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_dir() {
                go(&path, false, out)?;
            } else if crate::data::Format::of_path(&path).is_some()
                && !crate::data::is_project_file(&path)
            {
                out.push(path);
            }
        }

        Ok(())
    }

    go(dir, true, out)?;
    out.sort();

    Ok(())
}

/// Every Alloy source under a directory, recursively.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    fn go(dir: &Path, top: bool, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if !dir.is_dir() || skipped_dir(dir, top) {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();

            if path.is_dir() {
                go(&path, false, out)?;
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("aly" | "alx")
            ) {
                out.push(path);
            }
        }

        Ok(())
    }

    go(dir, true, out)
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
