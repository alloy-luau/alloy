//! `alloy flux`: the compile, the type check of the check artifact
//! through luau-lsp, and every lint at its `[lint]` level, in one run.
//! `--fix` applies the rewrites; `-W`, `-A`, and `-D` set a level for
//! this run; `--explain <lint>` prints its page.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alloy::config::Config;
use alloy::lint;

use crate::cli::build::{watch_loop, watch_roots};
use crate::cli::lint_support::{
    apply_header_as_fixes, apply_lint_fixes, lint_context, lint_one, list_lints, offer_fixes,
    print_lints,
};
use crate::cli::support::{is_source, option, positionals, print_diagnostics, project};
use crate::ui::{self, Level, Painter};
use crate::{fail, usage};

/// A path on the command line as a path relative to `[build] in`, or
/// `None` when it names a file outside the project's sources.
fn relative_to_input(file: &str, root: &Path, config: &Config) -> Option<PathBuf> {
    let input = root.join(&config.build.input);
    let full = std::fs::canonicalize(file).ok()?;
    let input = std::fs::canonicalize(&input).ok()?;

    full.strip_prefix(&input).ok().map(Path::to_path_buf)
}

/// What `alloy flux <file>` reports for a file outside `[build] in`.
/// Flux runs the Luau checker, and the checker needs the project the
/// file belongs to, so the run fails instead of passing on the lints
/// alone.
fn outside_input_message(file: &str, input: &Path) -> String {
    let input = input.display();

    format!(
        "{file} is outside {input}; flux did not type check it. \
         Run flux from the project that holds the file, or name a file under {input}"
    )
}

pub(crate) fn flux_cmd(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--list") {
        return list_lints();
    }

    if let Some(name) = option(args, "--explain") {
        return crate::cli::doc::run(&[name.to_string()]);
    }

    if args.iter().any(|a| a == "--watch") {
        let roots = match project(args) {
            Ok((root, config)) => watch_roots(&root, &config),

            Err(e) => {
                fail(&e);
                return ExitCode::FAILURE;
            }
        };

        return watch_loop(&roots, || flux_once(args));
    }

    flux_once(args)
}

/// One run of `alloy flux`.
fn flux_once(args: &[String]) -> ExitCode {
    let Some((args, root, config, lint_config)) = lint_context(args) else {
        return ExitCode::FAILURE;
    };
    let args = &args[..];
    let positional = positionals(args);

    // One file named on the command line: the whole project still
    // compiles, since the type check needs every module the file
    // imports, and the report is then cut down to that file.
    let mut only = None;

    if let Some(file) = positional.first() {
        if !is_source(file) {
            fail(&format!("{file} is not an .aly file"));
            return usage();
        }

        match relative_to_input(file, &root, &config) {
            Some(rel) => only = Some(rel),

            None => {
                // The lints still say what they can about the file, so
                // they print first; the error then carries the run.
                let _ = lint_one(file, &lint_config, None, args);
                fail(&outside_input_message(
                    file,
                    &root.join(&config.build.input),
                ));

                return ExitCode::FAILURE;
            }
        }
    }

    let mut report = match alloy::build::flux_project(&root, &config) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    if let Some(rel) = &only {
        report.diagnostics.retain(|(r, _)| r == rel);
        report.failures.retain(|(r, _)| r == rel);
        report.lints.retain(|(r, _)| r == rel);
        // `written` holds the emitted `.luau` paths, so the source name
        // never matches one; the run covered this one file.
        let output = alloy::build::output_for(rel);
        report
            .written
            .retain(|w| output.as_deref().is_some_and(|o| w == o));
    }

    let report = report;
    let p = Painter::for_stderr();
    let input = root.join(&config.build.input);
    print_diagnostics(&input, &report.diagnostics, &report.failures);

    // The type check: errors count as errors, the checker's lints take
    // their level from the `luau` group.
    let mut type_errors = 0;
    let mut type_warnings = 0;
    let mut type_denied = 0;
    let typecheck = config.flux.typecheck && !args.iter().any(|a| a == "--no-typecheck");

    if typecheck {
        // The checker's lints take their level from the file's own
        // `--@alloy-lint` before the `[lint]` table, so each source is
        // scanned once here.
        let per_file: Vec<(PathBuf, alloy::directives::Directives)> = report
            .checks
            .iter()
            .map(|c| (c.rel.clone(), alloy::directives::scan(&c.source)))
            .collect();

        match alloy::typecheck::analyze(&root, &config, &report.checks) {
            Ok(analysis) => {
                for note in &analysis.notes {
                    eprintln!("{}", p.note(note));
                }

                for d in &analysis.diagnostics {
                    if only.as_ref().is_some_and(|rel| &d.rel != rel) {
                        continue;
                    }

                    let path = input.join(&d.rel).display().to_string();
                    let empty = alloy::directives::Directives::default();
                    let file_directives = per_file
                        .iter()
                        .find(|(rel, _)| *rel == d.rel)
                        .map_or(&empty, |(_, d)| d);
                    let level = if d.is_error() {
                        type_errors += 1;

                        Level::Error
                    } else {
                        match lint::level_in(&lint_config, file_directives, &d.kind) {
                            lint::Level::Allow => continue,

                            lint::Level::Deny => {
                                type_denied += 1;

                                Level::Error
                            }

                            lint::Level::Warn => {
                                type_warnings += 1;

                                Level::Warning
                            }
                        }
                    };
                    eprintln!(
                        "{}",
                        p.diagnostic(
                            &path,
                            d.line,
                            d.col,
                            level,
                            Some(d.code().unwrap_or("luau")),
                            &format!("{}: {}", d.kind, d.message)
                        )
                    );
                }
            }

            Err(e) => eprintln!("{}", p.warn(&format!("type check skipped: {e}"))),
        }
    }

    let fix = args.iter().any(|a| a == "--fix");
    let header_rewrites = if fix {
        apply_header_as_fixes(&input, &report.diagnostics)
    } else {
        0
    };
    let (rewrites, remaining) = if fix {
        apply_lint_fixes(&input, &report.lints, &lint_config)
    } else {
        (0, report.lints.clone())
    };
    let rewrites = rewrites + header_rewrites;
    let (warnings, denied) = print_lints(&input, &remaining, &lint_config, args);
    offer_fixes(&input, &report.lints, &lint_config, fix, "flux");
    let deny_warnings = args.iter().any(|a| a == "--deny-warnings");
    let errors = report.diagnostics.len() + report.failures.len() + type_errors;
    let warnings = warnings + type_warnings;
    let denied = denied + type_denied;
    let counts = p.summary(&[
        (report.written.len(), "files", ui::DIM),
        (errors, "errors", ui::RED),
        (warnings, "warnings", ui::AMBER),
        (denied, "denied", ui::RED),
        (rewrites, "fixed", ui::GREEN),
    ]);

    if report.is_clean() && type_errors == 0 && denied == 0 && !(deny_warnings && warnings > 0) {
        eprintln!("{} {counts}", p.ok("flux"));

        ExitCode::SUCCESS
    } else {
        eprintln!("{} {counts}", p.fail("flux"));

        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file outside `[build] in` has no path relative to it, and the
    /// message names the folder the file has to sit under.
    #[test]
    fn a_file_outside_the_input_has_no_relative_path() {
        let dir = std::env::temp_dir().join(format!("alloy-flux-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::write(dir.join("alloy.toml"), "[build]\nin = \"src\"\n").expect("the file");
        std::fs::write(dir.join("src/inside.aly"), "print(1)\n").expect("the file");
        std::fs::write(dir.join("outside.aly"), "print(1)\n").expect("the file");

        let config = Config::load(&dir.join("alloy.toml")).expect("the config");
        let inside = dir.join("src/inside.aly");
        let outside = dir.join("outside.aly");

        assert_eq!(
            relative_to_input(&inside.to_string_lossy(), &dir, &config),
            Some(PathBuf::from("inside.aly"))
        );
        assert_eq!(
            relative_to_input(&outside.to_string_lossy(), &dir, &config),
            None
        );

        let message = outside_input_message("outside.aly", &dir.join("src"));

        assert!(message.contains("flux did not type check it"), "{message}");
        assert!(
            message.contains(&dir.join("src").display().to_string()),
            "{message}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
