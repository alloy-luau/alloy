//! `alloy build`: every source under `in`, compiled into the tree under `out`.

use std::collections::HashSet;
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

    // `luaux.toml` beside `alloy.toml` picks the UI library for `.alx`.
    let jsx_config = luaux::Config::load(root).map_err(|e| e.message);

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
        // path walks the instance tree to the runtime's mount instead.
        let depth = rel.components().count().saturating_sub(1);
        let source_rel = build.input.join(&rel);
        let std_require = emit
            .std_require
            .clone()
            .or_else(|| crate::project::std_require_for(config, &source_rel))
            .unwrap_or_else(|| {
                if depth == 0 {
                    "./alloy".to_string()
                } else {
                    format!("{}alloy", "../".repeat(depth))
                }
            });
        let options = EmitOptions {
            file_name: rel.to_string_lossy().into_owned(),
            definitions: rel.to_string_lossy().ends_with(".d.aly"),
            std_require,
            ..base_options.clone()
        };

        let compiled = if is_alx {
            let jsx = match &jsx_config {
                Ok(c) => c.clone(),

                Err(e) => {
                    report.failures.push((rel, e.clone()));

                    continue;
                }
            };

            crate::compile_alx(&source, &options, jsx).map(|a| a.output)
        } else {
            crate::compile_with(&source, &options)
        };

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

        if keep {
            let unused_lines = compiled
                .lints
                .iter()
                .filter(|l| l.name == "unused_variable")
                .map(|l| source[..l.start as usize].matches('\n').count() + 1)
                .collect();
            report.checks.push(crate::typecheck::CheckSource {
                rel: rel.clone(),
                source: source.clone(),
                check: compiled.check.clone(),
                map: compiled.map.clone(),
                unused_lines,
            });
        }

        if !write {
            report.written.push(rel_out);

            continue;
        }

        // Roblox reads no `.luaurc`: an `@alias` require in the ship
        // artifact becomes the relative instance path.
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

/// Every Alloy source under a directory, recursively.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            walk(&path, out)?;
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("aly" | "alx")
        ) {
            out.push(path);
        }
    }

    Ok(())
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
