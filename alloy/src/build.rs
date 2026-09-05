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

    run_with(root, &config, true)
}

/// The build of a whole config: the mounts write the project files and
/// route the requires.
pub fn run_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, true)
}

/// `check` for a whole config.
pub fn check_project(root: &Path, config: &Config) -> std::io::Result<Report> {
    run_with(root, config, false)
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

    run_with(root, &config, false)
}

/// The Alloy sources under `input`, sorted.
pub fn sources(input: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut list = Vec::new();
    walk(input, &mut list)?;
    list.sort();

    Ok(list)
}

fn run_with(root: &Path, config: &Config, write: bool) -> std::io::Result<Report> {
    let build = &config.build;
    let emit = &config.emit;
    let base_options = EmitOptions {
        wait_timeout: emit.wait_timeout,
        std_require: emit
            .std_require
            .clone()
            .unwrap_or_else(|| "@alloy".to_string()),
        ..EmitOptions::default()
    };
    let input = root.join(&build.input);
    let out = root.join(&build.out);
    let exclude = globs(&build.exclude)?;
    let mut report = Report::default();
    let mut expected: HashSet<PathBuf> = HashSet::new();

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

fn globs(patterns: &[String]) -> std::io::Result<GlobSet> {
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
