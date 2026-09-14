//! `alloy check`: the build without the write, plus the lints.

use std::process::ExitCode;

use crate::cli::lint_support::{lint_context, lint_files, print_lints};
use crate::cli::support::{
    apply_build_options, is_source, positionals, print_diagnostics, project,
};
use crate::ui::{self, Painter};
use crate::{fail, usage};

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

        return lint_files("check", &positional, &lint_config, &args);
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
