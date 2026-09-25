//! `alloy lint`: every lint at `warn` or `deny` under the `[lint]`
//! table. `--strict` turns the pedantic lints on for this run, and
//! `--deny-warnings` fails the run on any hit.

use std::process::ExitCode;

use crate::cli::lint_support::{
    apply_header_as_fixes, apply_lint_fixes, apply_std_import_fixes, lint_context, lint_files,
    list_lints, offer_fixes, print_lints,
};
use crate::cli::support::{is_source, positionals, print_diagnostics};
use crate::ui::{self, Painter};
use crate::{fail, usage};

pub(crate) fn lint_cmd(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--list") {
        return list_lints();
    }

    let Some((args, root, config, lint_config)) = lint_context(args) else {
        return ExitCode::FAILURE;
    };
    let args = &args[..];
    let positional = positionals(args);

    if !positional.is_empty() {
        for file in &positional {
            if !is_source(file) {
                fail(&format!("{file} is not an .aly file"));
                return usage();
            }
        }

        return lint_files("lint", &positional, &lint_config, args);
    }

    let mut report = match alloy::build::check_project(&root, &config) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    let p = Painter::for_stderr();
    let input = root.join(&config.build.input);
    let fix = args.iter().any(|a| a == "--fix");
    // The source rewrites run before the report, as in `alloy flux`.
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
