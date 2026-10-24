//! `alloy check`: the build without the write, plus the lints.

use std::process::ExitCode;

use crate::cli::lint_support::{
    apply_header_as_fixes, apply_lint_fixes, apply_std_import_fixes, lint_config_for, lint_context,
    lint_files, offer_fixes, print_lints, split_level_flags,
};
use crate::cli::support::{
    apply_build_options, is_source, positionals, print_diagnostics, project,
};
use crate::ui::{self, Painter};
use crate::{fail, usage};

pub(crate) fn check(args: &[String]) -> ExitCode {
    // `-W style` and `--deny name` set a level for this run. Their
    // value is no file, so they leave the arguments first.
    let (flags, rest) = split_level_flags(args);
    let positional = positionals(&rest);

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

    let args = &rest[..];
    let (root, mut config) = match project(args) {
        Ok(p) => p,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };
    apply_build_options(&mut config, args);

    let mut report = match alloy::build::check_project(&root, &config) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    let p = Painter::for_stderr();
    let input = root.join(&config.build.input);
    // `--fix` rewrites every file of the project, as `alloy lint --fix`
    // does: the source rewrites first, then a fresh check, then the
    // lint fixes. Every path is under the input folder.
    let fix = args.iter().any(|a| a == "--fix");
    let header_rewrites = match fix {
        true => {
            apply_header_as_fixes(&input, &report.diagnostics)
                + apply_std_import_fixes(&input, &report.diagnostics)
        }

        false => 0,
    };

    if header_rewrites > 0 {
        report = match alloy::build::check_project(&root, &config) {
            Ok(r) => r,

            Err(e) => {
                fail(&e.to_string());
                return ExitCode::FAILURE;
            }
        };
    }

    print_diagnostics(&input, &report.diagnostics, &report.failures);
    let lint_config = lint_config_for(&config, &flags, args);
    let (rewrites, remaining) = if fix {
        apply_lint_fixes(&input, &report.lints, &lint_config)
    } else {
        (0, report.lints.clone())
    };
    let (warnings, denied) = print_lints(&input, &remaining, &lint_config, args);
    offer_fixes(&input, &report.lints, &lint_config, fix, "check");
    let mut parts = vec![(report.written.len(), "files", ui::DIM)];

    // The count of rewrites shows only when `--fix` asks for them.
    if fix {
        parts.push((rewrites + header_rewrites, "fixed", ui::GREEN));
    }

    parts.extend([
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
    let counts = p.summary(&parts);

    let deny_warnings = args.iter().any(|a| a == "--deny-warnings");

    if report.is_clean() && denied == 0 && !(deny_warnings && warnings > 0) {
        eprintln!("{} {counts}", p.ok("check"));

        ExitCode::SUCCESS
    } else {
        eprintln!("{} {counts}", p.fail("check"));

        ExitCode::FAILURE
    }
}
