//! `alloy` command line entry point.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alloy::config::{self, Config, LintConfig};
use alloy::lint::{self, Lint};

mod art;
mod doc_cmd;
mod help;
mod highlight;
mod ingot_cmd;
mod jsonc;
mod self_cmd;
mod self_code;
mod ui;

use ui::{Level, Painter};

/// The version of this binary, for `alloy self update`.
pub fn alloy_version() -> &'static str {
    alloy::VERSION
}

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

        Some("ingot") if wants_help(&args) => command_help(help::INGOT_TEXT),

        Some("ingot") => ingot_cmd::run(&args[1..]),

        Some(other) => {
            fail(&format!("unknown command `{other}`"));
            usage()
        }
    }
}

/// The ingots of the project a file sits in, for a one-file command.
/// A load problem prints as a warning; the compile goes on without
/// that ingot.
/// The markup config for one file: the `[alx]` table of the nearest
/// alloy.toml, or a `luaux.toml` in the working directory.
fn markup_near(path: &Path) -> Result<alloy::luaux::Config, String> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };

    match alloy::config::Config::find(&dir) {
        Some(config_path) => {
            let config = alloy::config::Config::load(&config_path).map_err(|e| e.to_string())?;
            let root = config_path.parent().unwrap_or(Path::new("."));

            config.markup(root)
        }

        None => alloy::luaux::Config::load(Path::new(".")).map_err(|e| e.message),
    }
}

fn load_ingots_near(path: &Path) -> Option<alloy::ingot::Ingots> {
    // The search walks up from the file's directory; a relative path
    // has no parents to walk, so it is made absolute first.
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    let config_path = alloy::config::Config::find(&dir)?;
    let config = alloy::config::Config::load(&config_path).ok()?;

    if config.ingots.is_empty() {
        return None;
    }

    let root = config_path.parent().unwrap_or(Path::new("."));
    let ingots = alloy::ingot::Ingots::load(root, &config);

    for p in &ingots.problems {
        eprintln!("{}", Painter::for_stderr().warn(&p.to_string()));
    }

    Some(ingots)
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
            // Everything after `--` goes to another tool.
            "--" => break,

            "--out" | "--config" | "--wait-timeout" | "--explain" | "--filter" => i += 2,

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
        (report.copied.len(), "copied", ui::DIM),
        (report.data.len(), "data", ui::DIM),
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

/// One failure line. A compile that stopped names its position, so the
/// line reads `path:line:col: message` as every diagnostic does; a
/// failure with no position keeps the plain `path: message`.
/// A compile that stopped, as every other diagnostic reads: the
/// position, the section code, and the kind. A failure with no position,
/// such as a bad `[alx]` table, keeps the plain form.
fn print_failure(p: &Painter, path: &str, message: &str) {
    let located = message
        .split_once(':')
        .and_then(|(line, rest)| rest.split_once(':').map(|(col, text)| (line, col, text)))
        .and_then(|(line, col, text)| Some((line.parse().ok()?, col.parse().ok()?, text.trim())));

    match located {
        Some((line, col, text)) => eprintln!(
            "{}",
            p.diagnostic(
                path,
                line,
                col,
                Level::Error,
                alloy::docs::code_for(text),
                &alloy::docs::labeled(text)
            )
        ),

        None => eprintln!("{}", p.fail(&failure_line(path, message))),
    }
}

fn failure_line(path: &str, message: &str) -> String {
    let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    let positioned = message
        .split_once(':')
        .and_then(|(line, rest)| rest.split_once(':').map(|(col, _)| (line, col)))
        .is_some_and(|(line, col)| digits(line) && digits(col));

    if positioned {
        format!("{path}:{message}")
    } else {
        format!("{path}: {message}")
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
                &alloy::docs::labeled(&d.message)
            )
        );
    }

    for (rel, message) in &report.failures {
        print_failure(&p, &input.join(rel).display().to_string(), message);
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

    // The ingots of the nearest project add their lints under their names.
    if let Some(config_path) = Config::find(Path::new("."))
        && let Ok(config) = Config::load(&config_path)
        && !config.ingots.is_empty()
    {
        let root = config_path.parent().unwrap_or(Path::new("."));
        let ingots = alloy::ingot::Ingots::load(root, &config);

        for problem in &ingots.problems {
            eprintln!("{}", Painter::for_stderr().warn(&problem.to_string()));
        }

        for ingot in &ingots.list {
            if ingot.manifest.lints.is_empty() {
                continue;
            }

            println!();
            println!(
                "{}  {}",
                p.bold(&ingot.name),
                p.paint(ui::DIM, &format!("ingot: {}", ingot.manifest.description))
            );

            for l in lint::external().iter().filter(|l| l.group == ingot.name) {
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
        }
    }

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

/// `alloy flux`: the compile, the type check of the check artifact
/// through luau-lsp, and every lint at its `[lint]` level, in one run.
/// `--fix` applies the rewrites; `-W`, `-A`, and `-D` set a level for
/// this run; `--explain <lint>` prints its page.
/// A path on the command line as a path relative to `[build] in`, or
/// `None` when it names a file outside the project's sources.
fn relative_to_input(file: &str, root: &Path, config: &Config) -> Option<PathBuf> {
    let input = root.join(&config.build.input);
    let full = std::fs::canonicalize(file).ok()?;
    let input = std::fs::canonicalize(&input).ok()?;

    full.strip_prefix(&input).ok().map(Path::to_path_buf)
}

fn flux_cmd(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "--list") {
        return list_lints();
    }

    if let Some(name) = option(args, "--explain") {
        return doc_cmd::run(&[name.to_string()]);
    }

    if args.iter().any(|a| a == "--watch") {
        let roots = match project(args) {
            Ok((root, config)) => {
                vec![root.join(&config.build.input), root.join(config::FILE_NAME)]
            }

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
                let p = Painter::for_stderr();
                eprintln!(
                    "{}",
                    p.note(&format!(
                        "{file} is outside {}; the type check did not run",
                        root.join(&config.build.input).display()
                    ))
                );

                return lint_one(file, &lint_config, Some("flux"), args);
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
    print_diagnostics(&input, &report);

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
    let (rewrites, remaining) = if fix {
        apply_lint_fixes(&input, &report.lints, &lint_config)
    } else {
        (0, report.lints.clone())
    };
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
    if args.iter().any(|a| a == "--watch" || a == "-W") {
        let roots = match project(args) {
            Ok((root, config)) => {
                vec![root.join(&config.build.input), root.join(config::FILE_NAME)]
            }

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

/// Lints or checks one file outside a project. `summary` names the
/// command that prints a closing line.
fn lint_one(
    path: &str,
    lint_config: &LintConfig,
    summary: Option<&str>,
    args: &[String],
) -> ExitCode {
    let Some((source, mut out)) = compile_file(path, args) else {
        // The compile stopped, so there is one error and no lint. The
        // run still ends with the summary every other run prints.
        if let Some(command) = summary {
            let p = Painter::for_stderr();
            let counts = p.summary(&[
                (1, "errors", ui::RED),
                (0, "warnings", ui::AMBER),
                (0, "denied", ui::RED),
            ]);
            eprintln!("{} {counts}", p.fail(command));
        }

        return ExitCode::FAILURE;
    };

    // The project build reports these too; a single file names the
    // module it could not find, and the name it could not import.
    let silence = alloy::directives::scan(&source);

    for problem in alloy::modules::import_problems_for_file(Path::new(path), None, &source) {
        if silence.allows_named(
            alloy::directives::line_of(&source, problem.start as usize),
            Some(problem.kind),
        ) {
            out.diagnostics.push(alloy::Diagnostic {
                start: problem.start,
                end: problem.end,
                message: problem.message,
            });
        }
    }

    out.diagnostics.sort_by_key(|d| d.start);

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
                &alloy::docs::labeled(&d.message)
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
    offer_fixes(
        Path::new(""),
        &lints,
        lint_config,
        fix,
        summary.unwrap_or("lint"),
    );
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

/// The directives of each file under a root, read once per path. The
/// lint levels and `--@alloy-preserve` are per file, so every reporter
/// needs them beside the `[lint]` table.
#[derive(Default)]
struct FileDirectives {
    seen: std::collections::HashMap<PathBuf, alloy::directives::Directives>,
}

impl FileDirectives {
    fn of(&mut self, path: &Path) -> &alloy::directives::Directives {
        self.seen.entry(path.to_path_buf()).or_insert_with(|| {
            alloy::directives::scan(&std::fs::read_to_string(path).unwrap_or_default())
        })
    }
}

/// Whether `alloy flux --fix` may rewrite a lint: the level is not
/// `allow`, the lint has a rewrite, and no `--@alloy-preserve` covers
/// the line the rewrite starts on.
fn is_fixable(path: &Path, l: &Lint, config: &LintConfig, directives: &mut FileDirectives) -> bool {
    let Some(fix) = &l.fix else { return false };
    let d = directives.of(path);

    if lint::level_in(config, d, l.name) == lint::Level::Allow {
        return false;
    }

    let source = std::fs::read_to_string(path).unwrap_or_default();

    !directives
        .of(path)
        .preserves(alloy::directives::line_of(&source, fix.start as usize))
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
    let mut directives = alloy::directives::Directives::default();

    for (rel, l) in lints {
        let path = input.join(rel);

        if last_path.as_ref() != Some(&path) {
            source = std::fs::read_to_string(&path).unwrap_or_default();
            directives = alloy::directives::scan(&source);
            last_path = Some(path.clone());
        }

        // A `--@alloy-lint` in the file wins over the `[lint]` table.
        let level = lint::level_in(&config, &directives, l.name);

        if level == lint::Level::Allow {
            continue;
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
            let at = alloy::directives::line_of(&source, fix.start as usize);

            if directives.preserves(at) {
                eprintln!(
                    "{}",
                    p.note(
                        "`--@alloy-preserve` keeps this line, so `--fix` writes no rewrite here"
                    )
                );
            } else {
                eprintln!("{}", p.note(&rewrite_note(&source, fix)));
            }
        }
    }

    (warnings, denied)
}

/// The most times `--fix` re-reads one file. Each pass applies every
/// rewrite that does not overlap another, so a chain needs one pass per
/// level; the cap stops a rewrite that undoes itself.
const FIX_PASSES: usize = 8;

/// The lints of one file's text, for the `--fix` loop. A file that no
/// longer compiles has none, and the run reports the rewrites it made.
fn lints_of(path: &Path, source: &str) -> Vec<Lint> {
    let name = path.to_string_lossy().into_owned();
    let options = alloy::EmitOptions {
        file_name: name.clone(),
        definitions: name.ends_with(".d.aly"),
        import_types: alloy::modules::import_types_for_file(path, source),
        import_enums: alloy::modules::import_enums_for_file(path, source),
        import_privates: alloy::modules::import_privates_for_file(path, source),
        import_result_asyncs: alloy::modules::import_result_asyncs_for_file(path, source),
        import_trait_defaults: alloy::modules::import_trait_defaults_for_file(path, source),
        ..alloy::EmitOptions::default()
    };
    let jsx = markup_near(path).ok();
    let ingots = load_ingots_near(path);

    alloy::compile_file(&name, source, &options, jsx.as_ref(), ingots.as_ref())
        .map(|o| o.lints)
        .unwrap_or_default()
}

/// The `note: rewrite:` line of a lint: the line as it will read.
///
/// A rewrite inside one line prints that line rewritten, so a sub-range
/// edit such as `p?` reads as the whole statement. One that empties a
/// line says so. A rewrite over several lines prints its text on one.
fn rewrite_note(source: &str, fix: &alloy::lint::Fix) -> String {
    let (start, end) = (fix.start as usize, fix.end as usize);
    let line_start = source[..start.min(source.len())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let line_end = source[start.min(source.len())..]
        .find('\n')
        .map_or(source.len(), |i| start + i);

    // A rewrite that deletes a line reaches one past its end, over the
    // newline; that still describes the one line.
    if end <= line_end + 1 {
        let rebuilt = format!(
            "{}{}{}",
            &source[line_start..start],
            fix.replacement,
            &source[end.min(line_end)..line_end]
        );

        if rebuilt.trim().is_empty() {
            return "rewrite: delete this line".to_string();
        }

        return format!("rewrite: {}", rebuilt.trim());
    }

    let one_line = fix
        .replacement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    format!("rewrite: {one_line}")
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
    let mut directives = FileDirectives::default();
    let mut remaining: Vec<(PathBuf, Lint)> = lints
        .iter()
        .filter(|(rel, l)| !is_fixable(&input.join(rel), l, config, &mut directives))
        .cloned()
        .collect();

    for rel in paths {
        let path = input.join(rel);
        let live: Vec<Lint> = lints
            .iter()
            .filter(|(r, l)| r == rel && is_fixable(&path, l, config, &mut directives))
            .map(|(_, l)| l.clone())
            .collect();

        if live.is_empty() {
            continue;
        }

        let Ok(mut source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut live = live;
        let mut written = 0;
        let mut failed = false;

        // One rewrite can expose the next: collapsing an `if` chain
        // leaves another collapsible pair. The fixer runs again over
        // what it wrote, so one `--fix` reaches the fixed point.
        for _ in 0..FIX_PASSES {
            let (text, n) = lint::apply_fixes(&source, &live);

            if n == 0 || text == source {
                break;
            }

            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!(
                    "{}",
                    p.fail(&format!("{}: cannot write: {e}", path.display()))
                );
                failed = true;

                break;
            }

            written += n;
            source = text;
            live = lints_of(&path, &source)
                .into_iter()
                .filter(|l| is_fixable(&path, l, config, &mut directives))
                .collect();

            if live.is_empty() {
                break;
            }
        }

        if failed {
            remaining.extend(live.into_iter().map(|l| (rel.clone(), l)));

            continue;
        }

        if written > 0 {
            rewrites += written;
            eprintln!(
                "{}",
                p.wrote(&format!("{}: {written} rewrites", path.display()))
            );
        }
    }

    remaining.sort_by_key(|(rel, l)| (rel.clone(), l.start));
    (rewrites, remaining)
}

/// Says how many rewrites `--fix` would apply, when it was not given.
fn offer_fixes(
    input: &Path,
    lints: &[(PathBuf, Lint)],
    config: &LintConfig,
    fixed: bool,
    command: &str,
) {
    if fixed {
        return;
    }

    let mut directives = FileDirectives::default();
    let n = lints
        .iter()
        .filter(|(rel, l)| is_fixable(&input.join(rel), l, config, &mut directives))
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

        let result = if name.ends_with(".alx") {
            alloy::fmt_alx::format_alx_file(&source, &config.fmt)
        } else {
            alloy::fmt::format_file(&source, &config.fmt)
        };
        let formatted = match result {
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
        import_types: alloy::modules::import_types_for_file(Path::new(path), &source),
        import_enums: alloy::modules::import_enums_for_file(Path::new(path), &source),
        import_privates: alloy::modules::import_privates_for_file(Path::new(path), &source),
        import_result_asyncs: alloy::modules::import_result_asyncs_for_file(
            Path::new(path),
            &source,
        ),
        import_trait_defaults: alloy::modules::import_trait_defaults_for_file(
            Path::new(path),
            &source,
        ),
        ..alloy::EmitOptions::default()
    };

    // The nearest alloy.toml's `[alx]` picks the UI library, else a
    // `luaux.toml` in the working directory; the same file names the
    // ingots.
    let jsx = match markup_near(Path::new(path)) {
        Ok(c) => Some(c),

        Err(err) if path.ends_with(".alx") => {
            fail(&format!("{path}: {err}"));
            return None;
        }

        Err(_) => None,
    };
    let ingots = load_ingots_near(Path::new(path));
    let out = alloy::compile_file(path, &source, &options, jsx.as_ref(), ingots.as_ref());

    match out {
        Ok(out) => Some((source, out)),

        Err(err) => {
            // A compile that stopped reads as every other diagnostic:
            // the position, the section code, and the kind.
            let (line, col) = line_col(&source, err.offset.min(source.len()));
            let p = Painter::for_stderr();
            eprintln!(
                "{}",
                p.diagnostic(
                    path,
                    line,
                    col,
                    Level::Error,
                    alloy::docs::code_for(&err.message),
                    &alloy::docs::labeled(&err.message)
                )
            );
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
                &alloy::docs::labeled(&d.message)
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
