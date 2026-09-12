//! Helpers shared by two or more of `alloy`'s commands: argument
//! parsing, project lookup, single-file compiles, and diagnostic
//! printing.

use std::path::{Path, PathBuf};

use alloy::config::{self, Config};

use crate::ui::{Level, Painter};

/// The ingots of the project a file sits in, for a one-file command.
/// A load problem prints as a warning; the compile goes on without
/// that ingot.
/// The markup config for one file: the `[alx]` table of the nearest
/// alloy.toml, or a `luaux.toml` in the working directory.
pub(crate) fn markup_near(path: &Path) -> Result<alloy::luaux::Config, String> {
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

pub(crate) fn load_ingots_near(path: &Path) -> Option<alloy::ingot::Ingots> {
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

/// Reads a `--flag value` pair out of the arguments.
pub(crate) fn option<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The arguments that are not options. `--out`, `--config`,
/// `--wait-timeout`, and `--explain` take a value.
pub(crate) fn positionals(args: &[String]) -> Vec<String> {
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

pub(crate) fn is_source(path: &str) -> bool {
    path.ends_with(".aly") || path.ends_with(".alx")
}

/// The project root and its config: `--config`, else the nearest
/// `alloy.toml`, else the defaults in the working directory.
pub(crate) fn project(args: &[String]) -> Result<(PathBuf, Config), String> {
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

            for line in config.deprecations() {
                eprintln!("{}", Painter::for_stderr().warn(&line));
            }

            Ok((root, config))
        }

        None => Ok((cwd, Config::default())),
    }
}

pub(crate) fn apply_build_options(config: &mut Config, args: &[String]) {
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

/// One failure line. A compile that stopped names its position, so the
/// line reads `path:line:col: message` as every diagnostic does; a
/// failure with no position keeps the plain `path: message`.
/// A compile that stopped, as every other diagnostic reads: the
/// position, the section code, and the kind. A failure with no position,
/// such as a bad `[alx]` table, keeps the plain form.
pub(crate) fn print_failure(p: &Painter, path: &str, message: &str) {
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

pub(crate) fn print_diagnostics(input: &Path, report: &alloy::build::Report) {
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

/// Compiles one file the way `alloy build <file>` does.
pub(crate) fn compile_file(path: &str, args: &[String]) -> Option<(String, alloy::Output)> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,

        Err(err) => {
            crate::fail(&format!("cannot read {path}: {err}"));
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
        import_attributes: alloy::modules::import_attributes_for_file(Path::new(path), &source),
        plain_modules: alloy::modules::plain_modules_for_file(Path::new(path), &source),
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
            crate::fail(&format!("{path}: {err}"));
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

pub(crate) fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let upto = &text[..offset.min(text.len())];
    let line = upto.matches('\n').count() + 1;
    let col = upto.rfind('\n').map_or(offset, |i| offset - i - 1) + 1;

    (line, col)
}
