//! `alloy check`: the build without the write, plus the lints.

use std::process::ExitCode;

use alloy::config::LintConfig;

use crate::cli::lint_support::{Counts, is_clean, lint_context, lint_counts, print_lints};
use crate::cli::support::{
    apply_build_options, is_source, positionals, print_diagnostics, project,
};
use crate::ui::{self, Painter};
use crate::{fail, usage};

/// The counts over every file the command line names. A second file
/// used to be dropped, so its errors never reached the report.
fn check_files(files: &[String], lint_config: &LintConfig, args: &[String]) -> Counts {
    let mut counts = Counts::default();

    for file in files {
        counts += lint_counts(file, lint_config, "check", args);
    }

    counts
}

pub(crate) fn check(args: &[String]) -> ExitCode {
    let positional = positionals(args);

    if !positional.is_empty() {
        for file in &positional {
            if !is_source(file) {
                fail(&format!("{file} is not an .aly file"));
                return usage();
            }
        }

        // The project's alloy.toml still applies to one file: its lint
        // levels, and a broken one reports here the way it does for
        // `alloy build`. `lint_context` prints that report itself.
        let Some((args, _, _, lint_config)) = lint_context(args) else {
            return ExitCode::FAILURE;
        };
        let counts = check_files(&positional, &lint_config, &args);
        let p = Painter::for_stderr();
        let clean = is_clean(&counts, &args);
        let line = p.summary(&[
            (positional.len(), "files", ui::DIM),
            (counts.errors, "errors", ui::RED),
            (counts.warnings, "warnings", ui::AMBER),
            (counts.denied, "denied", ui::RED),
        ]);
        eprintln!(
            "{} {line}",
            if clean {
                p.ok("check")
            } else {
                p.fail("check")
            }
        );

        return if clean {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let (root, mut config) = match project(args) {
        Ok(p) => p,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };
    apply_build_options(&mut config, args);

    let report = match alloy::build::check_project(&root, &config) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    let p = Painter::for_stderr();
    let input = root.join(&config.build.input);
    print_diagnostics(&input, &report.diagnostics, &report.failures);
    let (warnings, denied) = print_lints(&input, &report.lints, &config.lint, args);
    let counts = p.summary(&[
        (report.written.len(), "files", ui::DIM),
        // A compile that stopped leaves a failure, not a diagnostic; it
        // is still an error the summary counts.
        (
            report.diagnostics.len() + report.failures.len(),
            "errors",
            ui::RED,
        ),
        (warnings, "warnings", ui::AMBER),
        (denied, "denied", ui::RED),
    ]);

    if report.is_clean() && denied == 0 {
        eprintln!("{} {counts}", p.ok("check"));

        ExitCode::SUCCESS
    } else {
        eprintln!("{} {counts}", p.fail("check"));

        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `alloy check a.aly b.aly` used to compile the first file alone,
    /// so the second file's errors never reached the report.
    #[test]
    fn check_reads_every_file_the_command_line_names() {
        let dir = std::env::temp_dir().join(format!("alloy-check-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::write(
            dir.join("alloy.toml"),
            "[build]\nin = \"src\"\nout = \"build\"\n",
        )
        .expect("the file");
        std::fs::write(dir.join("src/clean.aly"), "print(1)\n").expect("the file");
        // A struct and an enum of one name: the duplicate check reports.
        std::fs::write(
            dir.join("src/bad.aly"),
            "struct Thing as\n    v: number\nend\nenum Thing as\n    A\nend\nprint(Thing)\n",
        )
        .expect("the file");

        let clean = dir.join("src/clean.aly").display().to_string();
        let bad = dir.join("src/bad.aly").display().to_string();
        let config = LintConfig::default();

        assert_eq!(
            check_files(std::slice::from_ref(&clean), &config, &[]).errors,
            0
        );
        assert_eq!(
            check_files(std::slice::from_ref(&bad), &config, &[]).errors,
            1
        );

        // The second file is read, whichever place it takes.
        assert_eq!(
            check_files(&[clean.clone(), bad.clone()], &config, &[]).errors,
            1
        );
        assert_eq!(check_files(&[bad, clean], &config, &[]).errors, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
