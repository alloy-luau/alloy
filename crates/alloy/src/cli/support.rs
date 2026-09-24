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

/// The options one command takes, or `None` when the name is no command
/// of ours. The lists are what each command reads, so a flag added to a
/// command belongs here too.
fn flags_of(command: &str) -> Option<Vec<&'static str>> {
    // The lints read the same flags wherever they run, and
    // `apply_build_options` reads the same ones for every command that
    // compiles.
    const LINTS: &[&str] = &[
        "--fix",
        "--strict",
        "--deny-warnings",
        "--warn",
        "--allow",
        "--deny",
        "--config",
    ];
    const BUILD: &[&str] = &["--out", "--check", "--wait-timeout", "--config"];

    let flags = match command {
        "build" => [BUILD, &["--watch", "--map"]].concat(),
        "check" => [BUILD, LINTS].concat(),
        "lint" => [LINTS, &["--list"]].concat(),
        "flux" => [LINTS, &["--list", "--watch", "--explain", "--no-typecheck"]].concat(),
        "test" => [BUILD, &["--run", "--coverage", "--filter", "--watch"]].concat(),
        "fmt" => vec!["--check", "--config"],
        "doc" => vec!["--json"],
        "init" => vec!["--interactive", "--yes", "--non-interactive"],
        "self" => vec!["--dir", "--version", "--dry-run"],
        "ingot" => vec!["--lint", "--output", "--format", "--hover", "--complete"],

        _ => return None,
    };

    Some(flags)
}

/// The message for the first argument that starts with `--` and is no
/// option of the command. An option no command takes does nothing, and a
/// run that reads as a working one is worse than a failure.
///
/// Everything after `--` belongs to another tool, so the walk stops
/// there, and `--help` reaches every command.
pub(crate) fn unknown_flag(command: &str, args: &[String]) -> Option<String> {
    let flags = flags_of(command)?;

    args.iter()
        .take_while(|a| *a != "--")
        .find(|a| a.starts_with("--") && *a != "--help" && !flags.contains(&a.as_str()))
        .map(|a| {
            format!(
                "`{a}` is not an option of `alloy {command}`; alloy {command} --help lists them"
            )
        })
}

pub(crate) fn is_source(path: &str) -> bool {
    path.ends_with(".aly") || path.ends_with(".alx")
}

/// Where the search for `alloy.toml` starts: the folder of the first
/// path on the command line, else the working directory. A file names
/// its own project, so `alloy flux /elsewhere/src/x.aly` reads the
/// `alloy.toml` above that file and not the one above the cwd.
fn search_dir(args: &[String], cwd: &Path) -> PathBuf {
    let Some(first) = positionals(args).into_iter().next() else {
        return cwd.to_path_buf();
    };
    let path = std::path::absolute(&first).unwrap_or_else(|_| PathBuf::from(&first));

    if path.is_dir() {
        path
    } else if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.to_path_buf())
    } else {
        cwd.to_path_buf()
    }
}

/// The project root and its config: `--config`, else the nearest
/// `alloy.toml` above the named path, else the one above the working
/// directory, else the defaults in the working directory.
pub(crate) fn project(args: &[String]) -> Result<(PathBuf, Config), String> {
    let (root, config) = find_project(args)?;

    for line in config
        .deprecations()
        .into_iter()
        .chain(config.unknown_rules())
    {
        eprintln!("{}", Painter::for_stderr().warn(&line));
    }

    Ok((root, config))
}

/// [`project`] without the warnings, for a caller that reads the config
/// again while a watch runs.
pub(crate) fn find_project(args: &[String]) -> Result<(PathBuf, Config), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let dir = search_dir(args, &cwd);

    let found = match option(args, "--config") {
        Some(path) => Some(PathBuf::from(path)),

        None => Config::find(&dir).or_else(|| Config::find(&cwd)),
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

            // A source folder that is not there compiles nothing, and
            // a clean report over no files hides the typo.
            if !root.join(&config.build.input).is_dir() {
                return Err(format!(
                    "`[build] in` names `{}`, which does not exist under {}",
                    config.build.input.display(),
                    root.display()
                ));
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

/// Prints every diagnostic and every failure of a build, each against
/// the source it names under `input`.
pub(crate) fn print_diagnostics(
    input: &Path,
    diagnostics: &[(PathBuf, alloy::Diagnostic)],
    failures: &[(PathBuf, String)],
) {
    let p = Painter::for_stderr();

    for (rel, d) in diagnostics {
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

    for (rel, message) in failures {
        print_failure(&p, &input.join(rel).display().to_string(), message);
    }
}

/// The data files one source imports, read and parsed the way the
/// project build reads them, so `alloy check <file>` reports the
/// document a project build would refuse. A document that does not
/// parse is a diagnostic on its import literal.
///
/// A missing file is `unknown_module`, which the import scan already
/// reports, and an `@alias/x.json` spec needs the project's mounts, so
/// both pass here.
fn data_problems(path: &Path, source: &str) -> Vec<alloy::Diagnostic> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();

    for r in alloy::data::references(source) {
        let Some(format) = alloy::data::Format::of(&r.path) else {
            continue;
        };

        if !(r.path.starts_with("./") || r.path.starts_with("../")) {
            continue;
        }

        let file = dir.join(r.path.trim_start_matches("./"));
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };

        if let Err(e) = alloy::data::convert(&text, format) {
            out.push(alloy::Diagnostic {
                start: r.start,
                end: r.end,
                message: format!(
                    "data file {} does not parse as {}: {e}",
                    file.display().to_string().replace('\\', "/"),
                    format.name()
                ),
            });
        }
    }

    out
}

/// Compiles one file the way `alloy build <file>` does. `deps` says
/// what to do with an import into another project: `Some(true)` builds
/// that project and writes its output, `Some(false)` compiles it
/// without the write, and `None` leaves such an import alone, as
/// `alloy lint` does.
pub(crate) fn compile_file(
    path: &str,
    args: &[String],
    deps: Option<bool>,
) -> Option<(String, alloy::Output)> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,

        Err(err) => {
            crate::fail(&format!("cannot read {path}: {err}"));
            return None;
        }
    };

    // The nearest alloy.toml's `[emit]` applies to one file the way it
    // applies to the project build.
    let emit = Path::new(path)
        .parent()
        .and_then(alloy::config::Config::find)
        .and_then(|c| alloy::config::Config::load(&c).ok())
        .map(|c| c.emit)
        .unwrap_or_default();
    let options = alloy::EmitOptions {
        wait_timeout: option(args, "--wait-timeout")
            .and_then(|t| t.parse().ok())
            .or(emit.wait_timeout),
        erase_type_imports: emit.erase_type_imports,
        file_name: path.to_string(),
        definitions: path.ends_with(".d.aly"),
        ..alloy::EmitOptions::default().imports_for_file(Path::new(path), &source)
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
        Ok(mut out) => {
            out.diagnostics
                .extend(data_problems(Path::new(path), &source));

            // An import into another project builds that project first,
            // and the require names its output, as in a project build.
            if let Some(write) = deps {
                let name = Path::new(Path::new(path).file_name().unwrap_or_default());
                let out_file = option(args, "--out").map(|dir| {
                    Path::new(dir).join(alloy::build::output_for(name).unwrap_or_default())
                });
                let outside = alloy::build::file_outside(
                    Path::new(path),
                    out_file.as_deref(),
                    &out.imports,
                    &out.data_refs,
                    write,
                );
                let silence = alloy::directives::scan(&source);

                for p in outside.problems {
                    if silence.allows_named(
                        alloy::directives::line_of(&source, p.start as usize),
                        Some(p.kind),
                    ) {
                        out.diagnostics.push(alloy::Diagnostic {
                            start: p.start,
                            end: p.end,
                            message: p.message,
                        });
                    }
                }

                if !outside.rewrites.is_empty() {
                    let map = |text: &str| {
                        alloy::project::map_requires(text, |p| {
                            outside
                                .rewrites
                                .iter()
                                .find(|(spec, _)| spec == p)
                                .map(|(_, to)| to.clone())
                        })
                    };
                    out.ship = map(&out.ship);
                    out.check = map(&out.check);
                }
            }

            out.diagnostics.sort_by_key(|d| d.start);

            Some((source, out))
        }

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

pub(crate) use alloy::directives::line_col;

#[cfg(test)]
mod tests {
    use super::*;

    /// The search for `alloy.toml` starts at the named file, so a
    /// command that names a file in another project reads that
    /// project's config and not the one above the working directory.
    #[test]
    fn the_config_search_starts_at_the_named_path() {
        let dir = std::env::temp_dir().join(format!("alloy-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::write(dir.join("src/a.aly"), "print(1)\n").expect("the file");

        let cwd = Path::new("/cwd");
        let file = dir.join("src/a.aly").display().to_string();

        assert_eq!(
            search_dir(std::slice::from_ref(&file), cwd),
            dir.join("src")
        );
        assert_eq!(
            search_dir(&[dir.join("src").display().to_string()], cwd),
            dir.join("src")
        );
        assert_eq!(search_dir(&[], cwd), cwd);
        assert_eq!(search_dir(&["no-such-file.aly".to_string()], cwd), cwd);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A data import of one file is read, not only of a project: a
    /// document that does not parse is a diagnostic on its literal.
    #[test]
    fn a_bad_data_file_reports_for_one_file() {
        let dir = std::env::temp_dir().join(format!("alloy-data-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("the folder");
        std::fs::write(dir.join("src/bad.json"), "{ \"a\": 1, }\n").expect("the file");
        std::fs::write(dir.join("src/good.toml"), "a = 1\n").expect("the file");

        let source =
            "import bad from \"./bad.json\"\nimport good from \"./good.toml\"\nprint(bad, good)\n";
        let problems = data_problems(&dir.join("src/use.aly"), source);

        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0]
                .message
                .contains("bad.json does not parse as JSON: "),
            "{}",
            problems[0].message
        );
        assert_eq!(
            &source[problems[0].start as usize..problems[0].end as usize],
            "\"./bad.json\""
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An option a command does not take is a failure, not a silent run.
    /// A flag it does take, an argument after `--`, and `--help` pass.
    #[test]
    fn an_option_no_command_takes_reports() {
        let args =
            |list: &[&str]| -> Vec<String> { list.iter().map(|a| (*a).to_string()).collect() };

        assert_eq!(
            unknown_flag("check", &args(&["--json", "src/x.aly"])),
            Some(
                "`--json` is not an option of `alloy check`; alloy check --help lists them"
                    .to_string()
            )
        );
        assert!(unknown_flag("fmt", &args(&["--stdin"])).is_some());
        assert!(unknown_flag("check", &args(&["--fix", "--strict"])).is_none());
        assert!(unknown_flag("fmt", &args(&["--check", "src"])).is_none());
        assert!(unknown_flag("test", &args(&["--run", "--", "--nocolor"])).is_none());
        assert!(unknown_flag("check", &args(&["--help"])).is_none());
        // A name no command carries reports itself, so the flags are not
        // read for it.
        assert!(unknown_flag("--version", &args(&["--nope"])).is_none());
    }
}
