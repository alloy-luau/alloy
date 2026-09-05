//! `alloy` command line entry point.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alloy::config::{self, Config, LintConfig};
use alloy::lint::{self, Lint};

mod art;
mod doc_cmd;
mod help;
mod highlight;
mod self_cmd;
mod ui;

use ui::{Level, Painter};

/// A wrong invocation points at the help screen and fails.
fn usage() -> ExitCode {
    let p = Painter::for_stderr();
    eprintln!(
        "{}",
        p.note("usage: alloy <command> [options]; `alloy --help` lists the commands")
    );
    ExitCode::FAILURE
}

/// `✗ message` on stderr.
fn fail(message: &str) {
    eprintln!("{}", Painter::for_stderr().fail(message));
}

/// `alloy <command> --help` prints that command's options.
fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

fn command_help(text: &str) -> ExitCode {
    print!("{}", help::render_plain(text, ui::want_color()));
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("alloy {}", alloy::VERSION);
            ExitCode::SUCCESS
        }

        Some("--help" | "-h" | "help") | None => {
            print!("{}", help::render(ui::want_color()));
            ExitCode::SUCCESS
        }

        Some("build") if wants_help(&args) => command_help(help::BUILD_TEXT),

        Some("build") => build(&args[1..]),

        Some("check") if wants_help(&args) => command_help(help::CHECK_TEXT),

        Some("check") => check(&args[1..]),

        Some("lint") if wants_help(&args) => command_help(help::LINT_TEXT),

        Some("lint") => lint_cmd(&args[1..]),

        Some("flux") if wants_help(&args) => command_help(help::FLUX_TEXT),

        Some("flux") => flux_cmd(&args[1..]),

        Some("test") if wants_help(&args) => command_help(help::TEST_TEXT),

        Some("test") => test_cmd(&args[1..]),

        Some("fmt") if wants_help(&args) => command_help(help::FMT_TEXT),

        Some("fmt") => fmt_cmd(&args[1..]),

        Some("doc") if wants_help(&args) => command_help(help::DOC_TEXT),

        Some("doc") => doc_cmd::run(&args[1..]),

        Some("init") => init(),

        Some("self") => self_cmd::run(&args[1..]),

        Some(other) => {
            fail(&format!("unknown command `{other}`"));
            usage()
        }
    }
}

/// Writes `alloy.toml`, and the Luau configuration when the folder has
/// none: strict mode and the `@alloy` alias, as `.luaurc` and as
/// `.config.luau`, so either reader finds it.
fn init() -> ExitCode {
    let path = Path::new(config::FILE_NAME);

    let p = Painter::for_stdout();

    if path.exists() {
        fail(&format!("{} already exists", path.display()));
        return ExitCode::FAILURE;
    }

    if let Err(e) = std::fs::write(path, config::TEMPLATE) {
        fail(&format!("cannot write {}: {e}", path.display()));
        return ExitCode::FAILURE;
    }

    println!("{}", p.wrote(&path.display().to_string()));

    if alloy::luau_config::has_config(Path::new(".")) {
        return ExitCode::SUCCESS;
    }

    for (name, text) in [
        (".luaurc", config::LUAURC_TEMPLATE),
        (".config.luau", config::CONFIG_LUAU_TEMPLATE),
    ] {
        match std::fs::write(name, text) {
            Ok(()) => println!("{}", p.wrote(name)),

            Err(e) => {
                fail(&format!("cannot write {name}: {e}"));
                return ExitCode::FAILURE;
            }
        }
    }

    println!(
        "{}",
        p.ok("ready; put sources under src and run `alloy build`")
    );

    ExitCode::SUCCESS
}

/// Reads a `--flag value` pair out of the arguments.
fn option<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The arguments that are not options. `--out`, `--config`,
/// `--wait-timeout`, and `--explain` take a value.
fn positionals(args: &[String]) -> Vec<String> {
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--out" | "--config" | "--wait-timeout" | "--explain" => i += 2,

            "-W" => i += 1,

            a if a.starts_with("--") => i += 1,

            a => {
                positional.push(a.to_string());
                i += 1;
            }
        }
    }

    positional
}

fn is_source(path: &str) -> bool {
    path.ends_with(".aly") || path.ends_with(".alx")
}

/// The project root and its config: `--config`, else the nearest
/// `alloy.toml`, else the defaults in the working directory.
fn project(args: &[String]) -> Result<(PathBuf, Config), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let found = match option(args, "--config") {
        Some(path) => Some(PathBuf::from(path)),

        None => Config::find(&cwd),
    };

    match found {
        Some(path) => {
            let root = path.parent().map(Path::to_path_buf).unwrap_or(cwd.clone());
            let root = if root.as_os_str().is_empty() {
                cwd
            } else {
                root
            };
            let config = Config::load(&path).map_err(|e| e.to_string())?;

            Ok((root, config))
        }

        None => Ok((cwd, Config::default())),
    }
}

fn apply_build_options(config: &mut Config, args: &[String]) {
    if let Some(out) = option(args, "--out") {
        config.build.out = PathBuf::from(out);
    }

    if args.iter().any(|a| a == "--check") {
        config.build.artifact = config::Artifact::Check;
    }

    if let Some(t) = option(args, "--wait-timeout").and_then(|t| t.parse().ok()) {
        config.emit.wait_timeout = Some(t);
    }
}

fn build(args: &[String]) -> ExitCode {
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
                Ok((root, config)) => {
                    vec![root.join(&config.build.input), root.join(config::FILE_NAME)]
                }

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
fn watch_loop(roots: &[PathBuf], build: impl Fn() -> ExitCode) -> ExitCode {
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

fn print_diagnostics(input: &Path, report: &alloy::build::Report) {
    let p = Painter::for_stderr();

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
                &d.message
            )
        );
    }

    for (rel, message) in &report.failures {
        eprintln!(
            "{}",
            p.fail(&format!("{}: {message}", input.join(rel).display()))
        );
    }
}

/// `alloy check`: the build without the write, plus the lints.
fn check(args: &[String]) -> ExitCode {
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
        (report.diagnostics.len(), "errors", ui::RED),
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

/// `-W name`, `-A name`, `-D name`, and their long forms, taken out of
/// the arguments: the level each sets, and the arguments that remain.
fn split_level_flags(args: &[String]) -> (Vec<(lint::Level, String)>, Vec<String>) {
    let mut flags = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let level = match args[i].as_str() {
            "-W" | "--warn" => Some(lint::Level::Warn),
            "-A" | "--allow" => Some(lint::Level::Allow),
            "-D" | "--deny" => Some(lint::Level::Deny),
            _ => None,
        };

        match (level, args.get(i + 1)) {
            (Some(level), Some(name)) if !name.starts_with('-') => {
                flags.push((level, name.clone()));
                i += 2;
            }

            _ => {
                rest.push(args[i].clone());
                i += 1;
            }
        }
    }

    (flags, rest)
}

/// The `[lint]` table with `--strict` and the level flags applied. A
/// flag beats the table: its name leaves the other lists.
fn lint_config_for(
    config: &Config,
    flags: &[(lint::Level, String)],
    args: &[String],
) -> LintConfig {
    let mut lint_config = config.lint.clone();

    if args.iter().any(|a| a == "--strict") {
        lint_config.strict = true;
    }

    for (level, name) in flags {
        for list in [
            &mut lint_config.allow,
            &mut lint_config.warn,
            &mut lint_config.deny,
        ] {
            list.retain(|n| n != name);
        }

        match level {
            lint::Level::Allow => lint_config.allow.push(name.clone()),
            lint::Level::Warn => lint_config.warn.push(name.clone()),
            lint::Level::Deny => lint_config.deny.push(name.clone()),
        }
    }

    for name in lint::unknown_names(&lint_config) {
        eprintln!(
            "{}",
            Painter::for_stderr().warn(&format!(
                "`{name}` is neither a lint nor a group; `alloy flux --list` has them"
            ))
        );
    }

    lint_config
}

/// `--list`: every lint with its group and default level.
fn list_lints() -> ExitCode {
    let p = Painter::for_stdout();

    for group in lint::Group::ALL {
        println!(
            "{}  {}",
            p.bold(group.name()),
            p.paint(ui::DIM, group.summary())
        );

        for l in lint::LINTS.iter().filter(|l| l.group == *group) {
            let (level, rgb) = match l.default {
                lint::Level::Allow => ("allow", ui::DIM),
                lint::Level::Warn => ("warn", ui::AMBER),
                lint::Level::Deny => ("deny", ui::RED),
            };
            println!(
                "  {:<24} {}  {}",
                l.name,
                p.paint(rgb, &format!("{level:<6}")),
                l.summary
            );
        }

        println!();
    }

    println!(
        "{}  {}",
        p.bold(lint::LUAU_GROUP),
        p.paint(
            ui::DIM,
            "the type checker's own lints, LocalUnused and the rest, under `alloy flux`"
        )
    );

    ExitCode::SUCCESS
}

/// `alloy lint`: every lint at `warn` or `deny` under the `[lint]`
/// table. `--strict` turns the pedantic lints on for this run, and
/// `--deny-warnings` fails the run on any hit.
fn lint_cmd(args: &[String]) -> ExitCode {
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
    let (rewrites, remaining) = if fix {
        apply_lint_fixes(&input, &report.lints, &lint_config)
    } else {
        (0, report.lints.clone())
    };
    let (warnings, denied) = print_lints(&input, &remaining, &lint_config, args);
    offer_fixes(&report.lints, &lint_config, fix, "lint");
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

/// `alloy flux`: the compile, the type check of the check artifact
/// through luau-lsp, and every lint at its `[lint]` level, in one run.
/// `--fix` applies the rewrites; `-W`, `-A`, and `-D` set a level for
/// this run; `--explain <lint>` prints its page.
fn flux_cmd(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--list") {
        return list_lints();
    }

    if let Some(name) = option(args, "--explain") {
        return doc_cmd::run(&[name.to_string()]);
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

        return lint_one(file, &lint_config, Some("flux"), args);
    }

    let report = match alloy::build::flux_project(&root, &config) {
        Ok(r) => r,

        Err(e) => {
            fail(&e.to_string());
            return ExitCode::FAILURE;
        }
    };

    let p = Painter::for_stderr();
    let input = root.join(&config.build.input);
    print_diagnostics(&input, &report);

    // The type check: errors count as errors, the checker's lints take
    // their level from the `luau` group.
    let mut type_errors = 0;
    let mut type_warnings = 0;
    let mut type_denied = 0;
    let typecheck = config.flux.typecheck && !args.iter().any(|a| a == "--no-typecheck");

    if typecheck {
        match alloy::typecheck::analyze(&root, &config, &report.checks) {
            Ok(analysis) => {
                for note in &analysis.notes {
                    eprintln!("{}", p.note(note));
                }

                for d in &analysis.diagnostics {
                    let path = input.join(&d.rel).display().to_string();
                    let level = if d.is_error() {
                        type_errors += 1;

                        Level::Error
                    } else {
                        match lint::level_of(&lint_config, &d.kind) {
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
                            Some("luau"),
                            &format!("{}: {}", d.kind, d.message)
                        )
                    );
                }
            }

            Err(e) => eprintln!("{}", p.warn(&format!("type check skipped: {e}"))),
        }
    }

    let fix = args.iter().any(|a| a == "--fix");
    let (rewrites, remaining) = if fix {
        apply_lint_fixes(&input, &report.lints, &lint_config)
    } else {
        (0, report.lints.clone())
    };
    let (warnings, denied) = print_lints(&input, &remaining, &lint_config, args);
    offer_fixes(&report.lints, &lint_config, fix, "flux");
    let deny_warnings = args.iter().any(|a| a == "--deny-warnings");
    let errors = report.diagnostics.len() + type_errors;
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

/// `alloy test`: builds the project, then writes one lest spec per
/// source with a `@test` under `[test] out`. `--check` writes nothing
/// and fails when a spec would change; `--run` runs lest afterwards.
fn test_cmd(args: &[String]) -> ExitCode {
    let positional = positionals(args);
    let check_only = args.iter().any(|a| a == "--check");
    let run = args.iter().any(|a| a == "--run");
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

        return match alloy::testbuild::spec(&config, &root, &rel, &source) {
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
                            &d.message
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
                &d.message
            )
        );
    }

    for (rel, message) in &report.failures {
        eprintln!(
            "{}",
            p.fail(&format!("{}: {message}", input.join(rel).display()))
        );
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
        .arg(&config.test.suite)
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

/// Lints or checks one file outside a project. `summary` names the
/// command that prints a closing line.
fn lint_one(
    path: &str,
    lint_config: &LintConfig,
    summary: Option<&str>,
    args: &[String],
) -> ExitCode {
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
                &d.message
            )
        );
    }

    let lints: Vec<(PathBuf, Lint)> = out
        .lints
        .iter()
        .map(|l| (PathBuf::from(path), l.clone()))
        .collect();
    let fix = args.iter().any(|a| a == "--fix");
    let (_, remaining) = if fix {
        apply_lint_fixes(Path::new(""), &lints, lint_config)
    } else {
        (0, lints.clone())
    };
    let (warnings, denied) = print_lints(Path::new(""), &remaining, lint_config, args);
    offer_fixes(&lints, lint_config, fix, summary.unwrap_or("lint"));
    let deny_warnings = args.iter().any(|a| a == "--deny-warnings");

    let clean = out.diagnostics.is_empty() && denied == 0 && !(deny_warnings && warnings > 0);

    if let Some(command) = summary {
        let counts = p.summary(&[
            (out.diagnostics.len(), "errors", ui::RED),
            (warnings, "warnings", ui::AMBER),
            (denied, "denied", ui::RED),
        ]);
        eprintln!(
            "{} {counts}",
            if clean {
                p.ok(command)
            } else {
                p.fail(command)
            }
        );
    }

    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Prints the lints at `warn` and `deny`; returns how many of each.
fn print_lints(
    input: &Path,
    lints: &[(PathBuf, Lint)],
    config: &LintConfig,
    args: &[String],
) -> (usize, usize) {
    let p = Painter::for_stderr();
    let mut config = config.clone();

    if args.iter().any(|a| a == "--strict") {
        config.strict = true;
    }

    let mut warnings = 0;
    let mut denied = 0;
    let mut last_path: Option<PathBuf> = None;
    let mut source = String::new();

    for (rel, l) in lints {
        let level = lint::level_of(&config, l.name);

        if level == lint::Level::Allow {
            continue;
        }

        let path = input.join(rel);

        if last_path.as_ref() != Some(&path) {
            source = std::fs::read_to_string(&path).unwrap_or_default();
            last_path = Some(path.clone());
        }

        let (line, col) = line_col(&source, l.start as usize);
        let shown = match level {
            lint::Level::Deny => {
                denied += 1;

                Level::Error
            }

            _ => {
                warnings += 1;

                Level::Warning
            }
        };
        eprintln!(
            "{}",
            p.diagnostic(
                &path.display().to_string(),
                line,
                col,
                shown,
                Some(alloy::docs::LINT_CODE),
                &format!(
                    "{}: {} {}",
                    l.name,
                    l.message,
                    p.paint(ui::DIM, &format!("[{}]", lint::group_name(l.name)))
                )
            )
        );

        if let Some(fix) = &l.fix {
            let one_line = fix
                .replacement
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!("{}", p.note(&format!("rewrite: {one_line}")));
        }
    }

    (warnings, denied)
}

/// `--fix`: applies the rewrites of the lints at `warn` or `deny`, one
/// file at a time. Returns how many rewrites landed and the lints that
/// had none, which the caller prints.
fn apply_lint_fixes(
    input: &Path,
    lints: &[(PathBuf, Lint)],
    config: &LintConfig,
) -> (usize, Vec<(PathBuf, Lint)>) {
    let p = Painter::for_stderr();
    let mut paths: Vec<&PathBuf> = lints.iter().map(|(rel, _)| rel).collect();
    paths.sort();
    paths.dedup();
    let mut rewrites = 0;
    let mut remaining: Vec<(PathBuf, Lint)> = lints
        .iter()
        .filter(|(_, l)| l.fix.is_none())
        .cloned()
        .collect();

    for rel in paths {
        let live: Vec<Lint> = lints
            .iter()
            .filter(|(r, l)| {
                r == rel && l.fix.is_some() && lint::level_of(config, l.name) != lint::Level::Allow
            })
            .map(|(_, l)| l.clone())
            .collect();

        if live.is_empty() {
            continue;
        }

        let path = input.join(rel);
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (text, n) = lint::apply_fixes(&source, &live);

        if n == 0 || text == source {
            continue;
        }

        match std::fs::write(&path, text) {
            Ok(()) => {
                rewrites += n;
                eprintln!("{}", p.wrote(&format!("{}: {n} rewrites", path.display())));
            }

            Err(e) => {
                eprintln!(
                    "{}",
                    p.fail(&format!("{}: cannot write: {e}", path.display()))
                );
                remaining.extend(live.into_iter().map(|l| (rel.clone(), l)));
            }
        }
    }

    remaining.sort_by_key(|(rel, l)| (rel.clone(), l.start));
    (rewrites, remaining)
}

/// Says how many rewrites `--fix` would apply, when it was not given.
fn offer_fixes(lints: &[(PathBuf, Lint)], config: &LintConfig, fixed: bool, command: &str) {
    if fixed {
        return;
    }

    let n = lints
        .iter()
        .filter(|(_, l)| l.fix.is_some() && lint::level_of(config, l.name) != lint::Level::Allow)
        .count();

    if n > 0 {
        let p = Painter::for_stderr();
        eprintln!(
            "{}",
            p.note(&format!(
                "`alloy {command} --fix` applies {n} of these rewrites"
            ))
        );
    }
}

/// `alloy fmt`: formats the project sources, or the paths given.
/// `--check` writes nothing and fails when a file would change.
fn fmt_cmd(args: &[String]) -> ExitCode {
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

    if positional.is_empty() {
        match alloy::build::sources(&root.join(&config.build.input)) {
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
                match alloy::build::sources(&path) {
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

        let result = if name.ends_with(".alx") {
            alloy::fmt_alx::format_alx(&source, &config.fmt)
        } else {
            alloy::fmt::format_with(&source, &config.fmt)
        };
        let formatted = match result {
            Ok(f) => f,

            Err(e) => {
                eprintln!("{}", p.fail(&format!("{name}: {e}")));
                failed += 1;

                continue;
            }
        };

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

/// Compiles one file the way `alloy build <file>` does.
fn compile_file(path: &str, args: &[String]) -> Option<(String, alloy::Output)> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,

        Err(err) => {
            fail(&format!("cannot read {path}: {err}"));
            return None;
        }
    };

    let options = alloy::EmitOptions {
        wait_timeout: option(args, "--wait-timeout").and_then(|t| t.parse().ok()),
        file_name: path.to_string(),
        definitions: path.ends_with(".d.aly"),
        ..alloy::EmitOptions::default()
    };

    let out = if path.ends_with(".alx") {
        // `luaux.toml` in the working directory picks the UI library.
        let jsx = match alloy::luaux::Config::load(Path::new(".")) {
            Ok(c) => c,

            Err(err) => {
                fail(&format!("{path}: {}", err.message));
                return None;
            }
        };

        alloy::compile_alx(&source, &options, jsx).map(|a| a.output)
    } else {
        alloy::compile_with(&source, &options)
    };

    match out {
        Ok(out) => Some((source, out)),

        Err(err) => {
            fail(&format!("{path}: {err}"));
            None
        }
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
                &d.message
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

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let upto = &text[..offset.min(text.len())];
    let line = upto.matches('\n').count() + 1;
    let col = upto.rfind('\n').map_or(offset, |i| offset - i - 1) + 1;

    (line, col)
}
