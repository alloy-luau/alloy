//! `alloy test`: builds the project, then writes one lest spec per
//! source with a `@test` under `[test] out`. `--check` writes nothing
//! and fails when a spec would change; `--run` runs lest afterwards.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::build::{watch_loop, watch_roots};
use crate::cli::support::{
    line_col, option, positionals, print_diagnostics, print_failure, project,
};
use crate::fail;
use crate::ui::{self, Level, Painter};
use crate::usage;

/// The lest binary, on the PATH or under `~/.ember/bin`.
fn find_lest() -> Option<PathBuf> {
    let name = if cfg!(windows) { "lest.exe" } else { "lest" };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".ember/bin"));
    }

    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

pub(crate) fn test_cmd(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--watch" || a == "-W") {
        let roots = match project(args) {
            Ok((root, config)) => watch_roots(&root, &config),

            Err(e) => {
                fail(&e);
                return ExitCode::FAILURE;
            }
        };

        return watch_loop(&roots, || test_once(args));
    }

    test_once(args)
}

/// The arguments for lest: `--coverage` and `--filter <text>` by their
/// names here, and everything after `--` as given.
fn lest_args(args: &[String], suite: &str) -> Vec<String> {
    let mut out = vec![suite.to_string()];

    if args.iter().any(|a| a == "--coverage") {
        out.push("--coverage".to_string());
    }

    if let Some(text) = option(args, "--filter") {
        out.push("--filter".to_string());
        out.push(text.to_string());
    }

    if let Some(i) = args.iter().position(|a| a == "--") {
        out.extend(args[i + 1..].iter().cloned());
    }

    out
}

/// One run of `alloy test`.
fn test_once(args: &[String]) -> ExitCode {
    let positional = positionals(args);
    let check_only = args.iter().any(|a| a == "--check");
    let run = args.iter().any(|a| a == "--run")
        || args.iter().any(|a| a == "--coverage")
        || option(args, "--filter").is_some();
    let (root, mut config) = match project(args) {
        Ok(p) => p,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    if let Some(out) = option(args, "--out") {
        config.test.out = PathBuf::from(out);
    }

    let p = Painter::for_stderr();

    // One file: its spec to stdout.
    if let Some(file) = positional.first() {
        if !file.ends_with(".aly") {
            fail(&format!("{file} is not an .aly file"));
            return usage();
        }

        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,

            Err(e) => {
                fail(&format!("{file}: {e}"));
                return ExitCode::FAILURE;
            }
        };
        let rel = Path::new(file)
            .strip_prefix(&root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from(file));

        let ingots = alloy::ingot::Ingots::load(&root, &config);

        return match alloy::testbuild::spec(&config, &root, &rel, &source, Some(&ingots), &[]) {
            Ok(Some((text, diagnostics, _))) => {
                for d in &diagnostics {
                    let (line, col) = line_col(&source, d.start as usize);
                    eprintln!(
                        "{}",
                        p.diagnostic(
                            file,
                            line,
                            col,
                            Level::Error,
                            alloy::docs::code_for(&d.message),
                            &alloy::docs::labeled(&d.message)
                        )
                    );
                }

                print!("{text}");

                if diagnostics.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }

            Ok(None) => {
                eprintln!("{}", p.note(&format!("{file} has no @test")));

                ExitCode::SUCCESS
            }

            Err(e) => {
                fail(&format!("{file}: {e}"));

                ExitCode::FAILURE
            }
        };
    }

    // The specs require the build output, so the build comes first.
    if !check_only {
        let build = match alloy::build::run_project(&root, &config) {
            Ok(r) => r,

            Err(e) => {
                fail(&e.to_string());
                return ExitCode::FAILURE;
            }
        };
        let input = root.join(&config.build.input);
        print_diagnostics(&input, &build);

        if !build.is_clean() {
            eprintln!("{}", p.fail("test: the build has errors; no spec written"));

            return ExitCode::FAILURE;
        }
    }

    let report = match alloy::testbuild::run(&root, &config, !check_only) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };
    let input = root.join(&config.build.input);

    for (rel, d) in &report.diagnostics {
        let path = input.join(rel);
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let (line, col) = line_col(&source, d.start as usize);
        eprintln!(
            "{}",
            p.diagnostic(
                &path.display().to_string(),
                line,
                col,
                Level::Error,
                alloy::docs::code_for(&d.message),
                &alloy::docs::labeled(&d.message)
            )
        );
    }

    for (rel, message) in &report.failures {
        print_failure(&p, &input.join(rel).display().to_string(), message);
    }

    for note in &report.notes {
        eprintln!("{}", p.note(note));
    }

    for file in &report.written {
        eprintln!("{}", p.wrote(&root.join(file).display().to_string()));
    }

    for file in &report.stale {
        eprintln!(
            "{}",
            p.warn(&format!("{} would change", root.join(file).display()))
        );
    }

    for file in &report.removed {
        eprintln!(
            "{}",
            p.note(&format!("removed {}", root.join(file).display()))
        );
    }

    let counts = p.summary(&[
        (report.tests, "tests", ui::DIM),
        (report.written.len(), "specs", ui::GREEN),
        (report.stale.len(), "stale", ui::AMBER),
        (report.removed.len(), "removed", ui::AMBER),
        (
            report.diagnostics.len() + report.failures.len(),
            "errors",
            ui::RED,
        ),
    ]);
    let out = p.paint(
        ui::DIM,
        &format!(
            "{} {}",
            if p.color { "→" } else { "->" },
            root.join(&config.test.out).display()
        ),
    );

    if !report.is_clean() {
        eprintln!("{} {counts}  {out}", p.fail("test"));

        return ExitCode::FAILURE;
    }

    eprintln!("{} {counts}  {out}", p.ok("test"));

    if !run {
        return ExitCode::SUCCESS;
    }

    let Some(lest) = find_lest() else {
        fail("lest is not on the PATH; see https://github.com/lest-luau/lest");

        return ExitCode::FAILURE;
    };

    match std::process::Command::new(lest)
        .current_dir(&root)
        .args(lest_args(args, &config.test.suite))
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,

        Ok(_) => ExitCode::FAILURE,

        Err(e) => {
            fail(&format!("cannot run lest: {e}"));

            ExitCode::FAILURE
        }
    }
}
