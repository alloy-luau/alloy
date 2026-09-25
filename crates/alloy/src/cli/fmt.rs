//! `alloy fmt`: formats the project sources, or the paths given.
//! `--check` writes nothing and fails when a file would change.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::support::{positionals, project};
use crate::fail;
use crate::ui::{self, Painter};

pub(crate) fn fmt_cmd(args: &[String]) -> ExitCode {
    let check_only = args.iter().any(|a| a == "--check");
    let positional = positionals(args);
    let mut files: Vec<PathBuf> = Vec::new();
    let (root, config) = match project(args) {
        Ok(p) => p,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    let written = alloy::build::written_dirs(&root, &config);

    if positional.is_empty() {
        match alloy::build::sources(&root.join(&config.build.input), &written) {
            Ok(list) => files.extend(list),

            Err(e) => {
                fail(&e.to_string());
                return ExitCode::FAILURE;
            }
        }
    } else {
        for p in &positional {
            let path = PathBuf::from(p);

            if path.is_dir() {
                match alloy::build::sources(&path, &written) {
                    Ok(list) => files.extend(list),

                    Err(e) => {
                        fail(&e.to_string());
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                files.push(path);
            }
        }
    }

    let p = Painter::for_stderr();
    let mut changed = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let ingots = alloy::ingot::Ingots::load(&root, &config);

    for problem in &ingots.problems {
        eprintln!("{}", p.warn(&problem.to_string()));
    }

    for path in &files {
        let name = path.to_string_lossy();

        if config.fmt.exclude.iter().any(|g| glob_matches(g, &name)) {
            skipped += 1;

            continue;
        }

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,

            Err(e) => {
                eprintln!("{}", p.fail(&format!("{name}: cannot read: {e}")));
                failed += 1;

                continue;
            }
        };

        // Under `[fmt] recommended = false` the indent is the file's
        // own, so the options are settled per file. The renames of
        // `fix_naming` read the styles and the level from `[lint]`.
        let mut options = config.fmt.for_source(&source);
        options.lint = config.lint.clone();
        let formatted = match alloy::fmt::format_named(&name, &source, &options) {
            Ok(f) => f,

            Err(e) if e.starts_with(alloy::fmt::UNPARSED) => {
                eprintln!("{}", p.warn(&format!("{name}: skipped, it {e}")));
                skipped += 1;

                continue;
            }

            Err(e) => {
                eprintln!("{}", p.fail(&format!("{name}: {e}")));
                failed += 1;

                continue;
            }
        };

        // An ingot's formatter runs over Anneal's layout.
        let (formatted, problems) = ingots.format(&name, &formatted);

        for problem in problems {
            eprintln!("{}", p.warn(&format!("{name}: {problem}")));
        }

        if formatted == source {
            continue;
        }

        changed += 1;

        if check_only {
            eprintln!("{}", p.warn(&format!("{name} would change")));
        } else if let Err(e) = std::fs::write(path, formatted) {
            eprintln!("{}", p.fail(&format!("{name}: cannot write: {e}")));
            failed += 1;
        } else {
            eprintln!("{}", p.wrote(&format!("{name} formatted")));
        }
    }

    let clean = failed == 0 && !(check_only && changed > 0);
    let what = if check_only {
        "would change"
    } else {
        "formatted"
    };
    let tint = if check_only { ui::AMBER } else { ui::GREEN };
    let counts = p.summary(&[
        (files.len(), "files", ui::DIM),
        (changed, what, tint),
        (skipped, "skipped", ui::DIM),
        (failed, "failed", ui::RED),
    ]);
    eprintln!(
        "{} {counts}",
        if clean { p.ok("fmt") } else { p.fail("fmt") }
    );

    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `[fmt] exclude`: a pattern matches a path when its pieces around
/// each `*` appear in order, the first at the start and the last at the
/// end, unless the pattern begins or ends with `*`.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let pieces: Vec<&str> = pattern.split('*').collect();

    if pieces.len() == 1 {
        return path == pattern || path.ends_with(&format!("/{pattern}"));
    }

    let mut at = 0;

    for (k, piece) in pieces.iter().enumerate() {
        if piece.is_empty() {
            continue;
        }

        let Some(found) = path[at..].find(piece) else {
            return false;
        };

        if k == 0 && found != 0 {
            return false;
        }

        at += found + piece.len();
    }

    pieces.last().is_some_and(|last| last.is_empty()) || at == path.len()
}
