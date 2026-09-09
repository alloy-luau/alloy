//! `alloy check`: the build without the write, plus the lints.

use std::process::ExitCode;

use alloy::config::LintConfig;

use crate::cli::lint_support::{lint_one, print_lints};
use crate::cli::support::{
    apply_build_options, is_source, positionals, print_diagnostics, project,
};
use crate::ui::{self, Painter};
use crate::{fail, usage};

pub(crate) fn check(args: &[String]) -> ExitCode {
    let positional = positionals(args);

    if let Some(file) = positional.first() {
        if !is_source(file) {
            fail(&format!("{file} is not an .aly file"));
            return usage();
        }

        return lint_one(file, &LintConfig::default(), Some("check"), args);
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
    print_diagnostics(&input, &report);
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
