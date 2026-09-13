//! The comment directives, end to end through `compile_with`: the
//! diagnostics they silence, the ones they raise, and the lints they
//! re-level. Every case is what `alloy check` and `alloy flux` print.

use alloy::directives;
use alloy::lint::Level;

fn compile(src: &str) -> alloy::Output {
    alloy::compile_with(src, &alloy::EmitOptions::default()).unwrap()
}

fn compile_as(src: &str, file_name: &str) -> alloy::Output {
    let options = alloy::EmitOptions {
        file_name: file_name.to_string(),
        ..alloy::EmitOptions::default()
    };

    alloy::compile_with(src, &options).unwrap()
}

fn messages(src: &str) -> Vec<String> {
    compile(src)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

fn lint_names(src: &str) -> Vec<&'static str> {
    compile(src).lints.into_iter().map(|l| l.name).collect()
}

/// A source with one error the compiler always reports: a `match` that
/// leaves a variant out.
const NOT_EXHAUSTIVE: &str = concat!(
    "enum D as\n",
    "    A\n",
    "    B\n",
    "end\n",
    "local d: D = D.A\n",
    "match d with\n",
    "    case A then print(1)\n",
    "end\n",
);

// --- 1. the reason of an expectation ---------------------------------------

#[test]
fn a_bare_expectation_draws_the_missing_reason_lint() {
    let src = "--@alloy-expect-error\nlocal a = 1\n";
    assert_eq!(lint_names(src), vec![directives::MISSING_REASON]);

    let out = compile(src);
    assert!(
        out.lints[0].message.contains("write the reason after it"),
        "{}",
        out.lints[0].message
    );
    assert!(out.lints[0].fix.is_none());
    // The lint sits on the directive, not on the line it covers.
    assert_eq!(directives::line_of(src, out.lints[0].start as usize), 0);
}

#[test]
fn an_expectation_with_a_reason_draws_no_lint() {
    assert!(lint_names("--@alloy-expect-error the solver widens this\nlocal a = 1\n").is_empty());
    // An ignore never draws it, with or without a reason.
    assert!(lint_names("--@alloy-ignore\nlocal a = 1\n").is_empty());
    assert!(lint_names("--@alloy-ignore the solver widens this\nlocal a = 1\n").is_empty());
}

#[test]
fn a_trailing_bare_expectation_still_draws_the_lint() {
    // The expectation silences its own line; the lint about the
    // directive must survive that.
    let names = lint_names("local a = 1 --@alloy-expect-error\n");
    assert_eq!(names, vec![directives::MISSING_REASON]);
}

#[test]
fn the_unmet_message_quotes_the_reason() {
    let d = directives::scan("--@alloy-expect-error a negative count is refused\nlocal a = 1\n");
    let unmet = d.unmet(&std::collections::HashSet::new());
    assert_eq!(unmet.len(), 1);
    assert_eq!(
        directives::unmet_message(unmet[0].2.as_deref()),
        format!("{}: a negative count is refused", directives::UNMET)
    );
}

/// A stale `--@alloy-expect-error` is an error of its own, on the
/// directive. A run with no type check reports it from the compile;
/// `alloy flux` keeps the artifacts and reports what is left over after
/// the checker, so the two never both report.
#[test]
fn a_stale_expectation_reports_once_per_run() {
    let dir = std::env::temp_dir().join(format!("alloy-stale-expect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    // The covered line has to come clean, so it draws no lint either.
    let src = "local function twice(n: number): number\n    --@alloy-expect-error nothing is wrong here\n    return n * 2\nend\n\nreturn twice\n";
    std::fs::write(dir.join("src/stale.aly"), src).unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::check_project(&dir, &config).unwrap();
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|(_, d)| d.message.clone())
        .collect();

    assert_eq!(
        messages,
        vec![format!("{}: nothing is wrong here", directives::UNMET)]
    );
    assert_eq!(
        directives::line_of(src, report.diagnostics[0].1.start as usize),
        1,
        "the report sits on the directive"
    );

    let flux = alloy::build::flux_project(&dir, &config).unwrap();

    assert!(flux.diagnostics.is_empty(), "{:?}", flux.diagnostics);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_expectation_over_a_real_error_silences_it_and_counts_as_a_hit() {
    let src = NOT_EXHAUSTIVE.replace(
        "match d with",
        "--@alloy-expect-error a variant is missing\nmatch d with",
    );
    let out = compile(&src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.expected_hits, vec![6]);
}

// --- 2. the lint level of one file ------------------------------------------

#[test]
fn a_lint_directive_re_levels_one_file() {
    let src = "--@alloy-lint raw_require=deny\nlocal m = require(\"./m\")\n";
    let out = compile(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    let d = directives::scan(src);
    let config = alloy::config::LintConfig::default();
    assert_eq!(alloy::lint::level_of(&config, "raw_require"), Level::Warn);
    assert_eq!(
        alloy::lint::level_in(&config, &d, "raw_require"),
        Level::Deny
    );

    // The lint still fires; only its level changed.
    assert!(out.lints.iter().any(|l| l.name == "raw_require"));
}

#[test]
fn a_lint_directive_beats_the_project_table() {
    let src = "--@alloy-lint raw_require=allow\nlocal m = require(\"./m\")\n";
    let d = directives::scan(src);
    let config = alloy::config::LintConfig {
        deny: vec!["raw_require".to_string()],
        ..alloy::config::LintConfig::default()
    };
    assert_eq!(alloy::lint::level_of(&config, "raw_require"), Level::Deny);
    assert_eq!(
        alloy::lint::level_in(&config, &d, "raw_require"),
        Level::Allow
    );
}

#[test]
fn an_unknown_lint_name_or_level_is_a_directive_error() {
    let names = messages("--@alloy-lint no_such_lint=warn\nlocal a = 1\n");
    assert_eq!(names.len(), 1);
    assert!(names[0].contains("`no_such_lint`"), "{}", names[0]);
    // Every directive error carries the book section of the directives.
    assert_eq!(alloy::docs::code_for(&names[0]), Some("4.4"));

    let levels = messages("--@alloy-lint raw_require=loud\nlocal a = 1\n");
    assert_eq!(levels.len(), 1);
    assert!(levels[0].contains("`loud`"), "{}", levels[0]);
}

// --- 3. the ignored region ---------------------------------------------------

#[test]
fn a_region_silences_the_diagnostics_between_its_pair() {
    let src = NOT_EXHAUSTIVE.replace("match d with", "--@alloy-ignore-start\nmatch d with")
        + "--@alloy-ignore-end\n";
    let out = compile(&src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    // Without the pair the same file reports.
    assert_eq!(compile(NOT_EXHAUSTIVE).diagnostics.len(), 1);
}

#[test]
fn a_named_region_silences_that_lint_alone() {
    let src = concat!(
        "--@alloy-ignore-start raw_require\n",
        "local m = require(\"./m\")\n",
        "local n = a and a.b\n",
        "--@alloy-ignore-end\n",
    );
    let names = lint_names(src);
    assert!(!names.contains(&"raw_require"), "{names:?}");
    assert!(names.contains(&"manual_safe_access"), "{names:?}");
}

#[test]
fn a_region_may_name_a_diagnostic_kind() {
    // `UnknownModule` is the kind of an import the build cannot
    // resolve, so a region may name it the way it names a lint.
    let d = directives::scan(
        "--@alloy-ignore-start UnknownModule\nimport X from \"./gone\"\n--@alloy-ignore-end\n",
    );
    assert!(!d.allows_named(1, Some("UnknownModule")));
    assert!(d.allows_named(1, Some("TypeError")));
}

#[test]
fn an_unclosed_region_reports_and_still_silences() {
    let src = "--@alloy-ignore-start\n".to_string() + NOT_EXHAUSTIVE;
    let out = compile(&src);
    let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("has no `--@alloy-ignore-end`"));
    // The error sits on the directive's own line.
    assert_eq!(
        directives::line_of(&src, out.diagnostics[0].start as usize),
        0
    );
}

#[test]
fn an_end_that_closes_nothing_reports() {
    let messages = messages("local a = 1\n--@alloy-ignore-end\n");
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("closes no `--@alloy-ignore-start`"));
}

// --- 4. the side of a file ---------------------------------------------------

const REMOTE: &str = "remote Buy(item: string) from client\n";

/// The file name says which half of a remote the file sees.
#[test]
fn the_file_name_shapes_the_remote() {
    let by_name = compile_as(REMOTE, "shop.server.aly");

    // The server sees `on`, and never the client's `fire`.
    assert!(by_name.check.contains("on: "), "{}", by_name.check);
    assert!(!by_name.check.contains("fire: "), "{}", by_name.check);

    // A shared file sees both halves.
    let shared = compile_as(REMOTE, "shop.aly");
    assert!(shared.check.contains("fire: "), "{}", shared.check);
    assert!(shared.check.contains("on: "), "{}", shared.check);
}

/// The side directives left with `global`; the words name no directive.
#[test]
fn the_side_directives_are_gone() {
    let file_side = messages("--@alloy-file-side client\nlocal a = 1\n");
    assert_eq!(file_side.len(), 1, "{file_side:?}");
    assert!(file_side[0].contains("is no directive"), "{file_side:?}");

    let decl_side = messages("--@alloy-side client\nlocal a = 1\n");
    assert_eq!(decl_side.len(), 1, "{decl_side:?}");
    assert!(decl_side[0].contains("is no directive"), "{decl_side:?}");
}

// --- 5. the preserved line ---------------------------------------------------

#[test]
fn preserve_keeps_the_lint_and_its_rewrite_apart() {
    let src = "--@alloy-preserve the two names read better apart\nlocal n = p and p.Name\n";
    let out = compile(src);
    let d = directives::scan(src);
    let lint = out
        .lints
        .iter()
        .find(|l| l.name == "manual_safe_access")
        .expect("the lint fires");

    // The lint still reports, and its rewrite still exists; the guard
    // is at the point that applies it.
    assert!(lint.fix.is_some());
    assert!(d.preserves(directives::line_of(
        src,
        lint.fix.as_ref().unwrap().start as usize
    )));

    // Without the directive the same line is open to `--fix`.
    let plain = "local n = p and p.Name\n";
    assert!(!directives::scan(plain).preserves(0));
    let safe: Vec<alloy::Lint> = compile(plain)
        .lints
        .into_iter()
        .filter(|l| l.name == "manual_safe_access")
        .collect();
    assert_eq!(
        alloy::lint::apply_fixes(plain, &safe).0,
        "local n = p?.Name\n"
    );
}

#[test]
fn preserve_silences_nothing() {
    let src = "--@alloy-preserve\n".to_string() + NOT_EXHAUSTIVE;
    assert_eq!(compile(&src).diagnostics.len(), 1);
}

// --- the directives together -------------------------------------------------

#[test]
fn a_name_no_directive_has_still_reports_and_lists_them_all() {
    let messages = messages("--@alloy-quiet\nlocal a = 1\n");
    assert_eq!(messages.len(), 1);

    for name in directives::NAMES {
        assert!(messages[0].contains(name), "`{name}` is not in the list");
    }
}

#[test]
fn nocheck_silences_every_other_directive() {
    let src = concat!(
        "--@alloy-nocheck\n",
        "--@alloy-expect-error\n",
        "local a = 1\n",
        "--@alloy-ignore-end\n",
    );
    let out = compile(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.lints.is_empty(), "{:?}", out.lints);
}
