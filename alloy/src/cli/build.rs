//! `alloy build`: compiles the project, or one file, to Luau.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alloy::config::{self, Config};

use crate::cli::support::{
    apply_build_options, compile_file, is_source, line_col, option, positionals, print_diagnostics,
    project,
};
use crate::ui::{self, Level, Painter};
use crate::{fail, usage};

pub(crate) fn build(args: &[String]) -> ExitCode {
    let positional = positionals(args);
    let watch = args.iter().any(|a| a == "--watch" || a == "-W");

    match positional.first() {
        Some(file) if is_source(file) => {
            if watch {
                watch_loop(&[PathBuf::from(file)], || build_one(file, args))
            } else {
                build_one(file, args)
            }
        }

        Some(other) => {
            fail(&format!("{other} is not an .aly file"));
            usage()
        }

        None if watch => {
            let roots = match project(args) {
                Ok((root, config)) => watch_roots(&root, &config),

                Err(e) => {
                    fail(&e);
                    return ExitCode::FAILURE;
                }
            };

            watch_loop(&roots, || build_project(args))
        }

        None => build_project(args),
    }
}

/// What watch mode polls: the sources, `alloy.toml`, the project file,
/// and every folder the tree mounts. A change to the tree outside the
/// sources still moves the sourcemap and the build project, so the
/// build must run again.
pub(crate) fn watch_roots(root: &Path, config: &Config) -> Vec<PathBuf> {
    let mut roots = vec![root.join(&config.build.input), root.join(config::FILE_NAME)];
    let tree = alloy::project::Tree::load(root, config);

    if let Some(project) = &tree.project {
        roots.push(root.join(&project.file));
    }

    let out = root.join(&config.build.out);

    for m in &tree.mounts {
        let path = root.join(&m.disk);

        // A mount under `[build] out` is what this build writes; polling
        // it would make the build wake itself.
        if !path.starts_with(&out) && !roots.iter().any(|r| path.starts_with(r)) {
            roots.push(path);
        }
    }

    roots
}

/// The newest change under the roots: the count of files and the latest
/// modification time, which together move on any write, add, or delete.
fn tree_stamp(roots: &[PathBuf]) -> (usize, Option<std::time::SystemTime>) {
    fn walk(dir: &Path, count: &mut usize, newest: &mut Option<std::time::SystemTime>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();

            if name == ".git" || name == "node_modules" || name == "target" {
                continue;
            }

            if path.is_dir() {
                walk(&path, count, newest);
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
        if root.is_dir() {
            walk(root, &mut count, &mut newest);
        } else if let Ok(meta) = std::fs::metadata(root)
            && let Ok(m) = meta.modified()
        {
            count += 1;

            if newest.is_none_or(|n| m > n) {
                newest = Some(m);
            }
        }
    }

    (count, newest)
}

/// Runs `build` now and again after every change under the roots,
/// polled four times a second, until ctrl-c.
pub(crate) fn watch_loop(roots: &[PathBuf], build: impl Fn() -> ExitCode) -> ExitCode {
    let p = Painter::for_stderr();
    let shown: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
    let mut stamp = tree_stamp(roots);
    build();
    eprintln!(
        "{}",
        p.note(&format!("watching {} (ctrl-c to stop)", shown.join(", ")))
    );

    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let now = tree_stamp(roots);

        if now != stamp {
            // A save often lands as two writes; the second one settles.
            std::thread::sleep(std::time::Duration::from_millis(60));
            stamp = tree_stamp(roots);
            eprintln!();
            build();
        }
    }
}

fn build_project(args: &[String]) -> ExitCode {
    let (root, mut config) = match project(args) {
        Ok(p) => p,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };
    apply_build_options(&mut config, args);

    let report = match alloy::build::run_project(&root, &config) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    let p = Painter::for_stderr();
    let input = root.join(&config.build.input);
    print_diagnostics(&input, &report);

    for note in &report.notes {
        eprintln!("{}", p.note(note));
    }

    for file in &report.project_files {
        if file.file_name().is_some_and(|n| n == ".gitignore") {
            continue;
        }

        eprintln!("{}", p.wrote(&file.to_string_lossy()));
    }

    let counts = p.summary(&[
        (report.written.len(), "written", ui::GREEN),
        (report.copied.len(), "copied", ui::DIM),
        (report.data.len(), "data", ui::DIM),
        (report.skipped.len(), "skipped", ui::DIM),
        (report.removed.len(), "removed", ui::AMBER),
        (report.diagnostics.len(), "diagnostics", ui::RED),
    ]);
    let out = p.paint(
        ui::DIM,
        &format!(
            "{} {}",
            if p.color { "→" } else { "->" },
            root.join(&config.build.out).display()
        ),
    );

    if report.is_clean() {
        eprintln!("{} {counts}  {out}", p.ok("build"));

        ExitCode::SUCCESS
    } else {
        eprintln!("{} {counts}  {out}", p.fail("build"));

        ExitCode::FAILURE
    }
}

fn build_one(path: &str, args: &[String]) -> ExitCode {
    let want_check = args.iter().any(|a| a == "--check");
    let want_map = args.iter().any(|a| a == "--map");

    let Some((source, out)) = compile_file(path, args) else {
        return ExitCode::FAILURE;
    };

    let p = Painter::for_stderr();

    for d in &out.diagnostics {
        let (line, col) = line_col(&source, d.start as usize);
        eprintln!(
            "{}",
            p.diagnostic(
                path,
                line,
                col,
                Level::Error,
                alloy::docs::code_for(&d.message),
                &alloy::docs::labeled(&d.message)
            )
        );
    }

    if want_map {
        for (i, chunk) in out.map.chunks().iter().enumerate() {
            eprintln!("{i}: {chunk:?}");
        }
    }

    let text = if want_check { &out.check } else { &out.ship };

    match option(args, "--out") {
        Some(dir) => {
            let rel = Path::new(path)
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default();
            let Some(target) = alloy::build::output_for(&rel) else {
                return usage();
            };
            let target = Path::new(dir).join(target);

            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            if let Err(e) = std::fs::write(&target, text) {
                fail(&format!("cannot write {}: {e}", target.display()));
                return ExitCode::FAILURE;
            }

            eprintln!("{}", p.wrote(&target.display().to_string()));
        }

        None => print!("{text}"),
    }

    if out.diagnostics.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Watch mode follows the tree, not only the sources.
    #[test]
    fn watch_roots_cover_the_tree() {
        let dir = std::env::temp_dir().join(format!("alloy-build-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder");

        std::fs::write(
            dir.join("alloy.toml"),
            "[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n",
        )
        .expect("the file");
        let config = Config::load(&dir.join("alloy.toml")).expect("the config");
        let roots = watch_roots(&dir, &config);

        assert!(roots.contains(&dir.join("src")));
        assert!(roots.contains(&dir.join(config::FILE_NAME)));
        // A folder under `[build] in` is covered by the sources root.
        assert!(!roots.contains(&dir.join("src/shared")));
        assert!(roots.contains(&dir.join("Packages")));

        // A project file joins the roots too.
        std::fs::write(dir.join("alloy.toml"), "[build]\nin = \"src\"\n").expect("the file");
        std::fs::write(
            dir.join("default.project.json"),
            "{ \"name\": \"n\", \"tree\": { \"$className\": \"DataModel\", \"ReplicatedStorage\": { \"$className\": \"ReplicatedStorage\", \"P\": { \"$path\": \"Packages\" } } } }",
        )
        .expect("the file");
        let config = Config::load(&dir.join("alloy.toml")).expect("the config");
        let roots = watch_roots(&dir, &config);
        assert!(roots.contains(&dir.join("default.project.json")));
        assert!(roots.contains(&dir.join("Packages")));

        // The runtime the build writes is never polled: the build would
        // wake itself.
        std::fs::write(
            dir.join("default.project.json"),
            "{ \"name\": \"n\", \"tree\": { \"$className\": \"DataModel\", \"ReplicatedStorage\": { \"$className\": \"ReplicatedStorage\", \"Alloy\": { \"$path\": \"build/alloy.luau\" } } } }",
        )
        .expect("the file");
        let roots = watch_roots(&dir, &config);
        assert!(!roots.contains(&dir.join("build/alloy.luau")));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
