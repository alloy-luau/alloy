//! `alloy lint`: every lint at `warn` or `deny` under the `[lint]`
//! table. `--strict` turns the pedantic lints on for this run, and
//! `--deny-warnings` fails the run on any hit.

use std::process::ExitCode;

use crate::cli::lint_support::{
    apply_header_as_fixes, apply_lint_fixes, lint_config_for, lint_one, list_lints, offer_fixes,
    print_lints, split_level_flags,
};
use crate::cli::support::{is_source, positionals, print_diagnostics, project};
use crate::ui::{self, Painter};
use crate::{fail, usage};

pub(crate) fn lint_cmd(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--list") {
        return list_lints();
    }

    let (flags, args) = split_level_flags(args);
    let args = &args[..];
    let positional = positionals(args);
    let (root, config) = match project(args) {
        Ok(p) => p,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };
    let lint_config = lint_config_for(&config, &flags, args);

    if let Some(file) = positional.first() {
        if !is_source(file) {
            fail(&format!("{file} is not an .aly file"));
            return usage();
        }

        return lint_one(file, &lint_config, None, args);
    }

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
    offer_fixes(&input, &report.lints, &lint_config, fix, "lint");
    let deny_warnings = args.iter().any(|a| a == "--deny-warnings");
    let counts = p.summary(&[
        (report.written.len(), "files", ui::DIM),
        (rewrites, "fixed", ui::GREEN),
        (warnings, "warnings", ui::AMBER),
        (denied, "denied", ui::RED),
    ]);

    if report.is_clean() && denied == 0 && !(deny_warnings && warnings > 0) {
        eprintln!("{} {counts}", p.ok("lint"));

        ExitCode::SUCCESS
    } else {
        eprintln!("{} {counts}", p.fail("lint"));

        ExitCode::FAILURE
    }
}
