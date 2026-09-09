//! Lint listing, single-file linting, and the `--fix` machinery, shared
//! by `check`, `lint`, and `flux`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alloy::config::{Config, LintConfig};
use alloy::lint::{self, Lint};

use crate::cli::support::{compile_file, line_col, load_ingots_near, markup_near};
use crate::ui::{self, Level, Painter};

/// `-W name`, `-A name`, `-D name`, and their long forms, taken out of
/// the arguments: the level each sets, and the arguments that remain.
pub(crate) fn split_level_flags(args: &[String]) -> (Vec<(lint::Level, String)>, Vec<String>) {
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
pub(crate) fn lint_config_for(
    config: &Config,
    flags: &[(lint::Level, String)],
    args: &[String],
) -> LintConfig {
    let mut lint_config = config.lint.clone();

    if args.iter().any(|a| a == "--strict") {
        lint_config.strict = true;
    }

    for (level, name) in flags {
        // The deprecated lists still read, so a flag has to leave them
        // or the file would beat the command line.
        for list in [
            &mut lint_config.allow,
            &mut lint_config.warn,
            &mut lint_config.deny,
        ] {
            list.retain(|n| n != name);
        }

        lint_config.rules.insert(name.clone(), *level);
    }

    for name in lint::unknown_names(&lint_config) {
        eprintln!(
            "{}",
            Painter::for_stderr().warn(&format!(
                "`{name}` is neither a lint nor a group; `alloy lint --list` has them"
            ))
        );
    }

    lint_config
}

/// One `--list` line: the name and its level as `[lint.rules]` takes
/// them, then the summary. The padding is counted before the paint, so
/// the colour codes never move the column.
fn list_line(p: &Painter, name: &str, level: lint::Level, summary: &str) {
    let rgb = match level {
        lint::Level::Allow => ui::DIM,
        lint::Level::Warn => ui::AMBER,
        lint::Level::Deny => ui::RED,
    };
    let rule = format!("\"{}\"", level.name());
    let width = name.chars().count() + 3 + rule.chars().count();
    let pad = " ".repeat(38usize.saturating_sub(width));

    println!("  {name} = {}{pad}  {summary}", p.paint(rgb, &rule));
}

/// `--list`: every lint with its group and the level a project with no
/// `[lint.rules]` gives it, written the way that table takes it.
pub(crate) fn list_lints() -> ExitCode {
    let p = Painter::for_stdout();
    let defaults = LintConfig::default();

    println!(
        "{}  {}",
        p.bold("[lint.rules]"),
        p.paint(
            ui::DIM,
            "a lint name or a group name, at allow, warn, or deny; a name beats its group"
        )
    );
    println!();

    for group in lint::Group::ALL {
        println!(
            "{}  {}",
            p.bold(group.name()),
            p.paint(ui::DIM, group.summary())
        );

        for l in lint::LINTS.iter().filter(|l| l.group == *group) {
            list_line(&p, l.name, lint::level_of(&defaults, l.name), l.summary);
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
    println!();
    println!(
        "{}  {}",
        p.bold("alx"),
        p.paint(ui::DIM, "the markup lints of `.alx` files")
    );

    for l in lint::ALX_LINTS {
        list_line(
            &p,
            &format!("{}{}", lint::ALX_PREFIX, l.name),
            l.default,
            l.summary,
        );
    }

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
                list_line(&p, l.name, l.default, &l.summary);
            }
        }
    }

    ExitCode::SUCCESS
}

/// Lints or checks one file outside a project. `summary` names the
/// command that prints a closing line.
pub(crate) fn lint_one(
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
pub(crate) fn print_lints(
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
        plain_modules: alloy::modules::plain_modules_for_file(path, source),
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

/// `--fix`: writes the `as` an `impl` or a `trait` header is missing.
///
/// The header is a syntax error, and a file the parser reported on has
/// no lints, so this rewrite rides on the diagnostic instead. It runs
/// before the lint fixes, which then see a file that parses clean.
pub(crate) fn apply_header_as_fixes(
    input: &Path,
    diagnostics: &[(PathBuf, alloy::Diagnostic)],
) -> usize {
    let mut paths: Vec<&PathBuf> = diagnostics
        .iter()
        .filter(|(_, d)| d.message.ends_with(alloy::fmt::NEEDS_AS))
        .map(|(rel, _)| rel)
        .collect();
    paths.sort();
    paths.dedup();
    let mut rewrites = 0;

    for rel in paths {
        let path = input.join(rel);
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let fixes = alloy::fmt::header_as_fixes(&source);

        if fixes.is_empty() {
            continue;
        }

        let mut text = source;

        for f in fixes.iter().rev() {
            text.insert_str(f.start as usize, &f.replacement);
        }

        if fs::write(&path, &text).is_ok() {
            rewrites += fixes.len();
        }
    }

    rewrites
}

/// `--fix`: applies the rewrites of the lints at `warn` or `deny`, one
/// file at a time. Returns how many rewrites landed and the lints that
/// had none, which the caller prints.
pub(crate) fn apply_lint_fixes(
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
pub(crate) fn offer_fixes(
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
