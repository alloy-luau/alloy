//! An import into another project: that project builds by its own
//! `alloy.toml`, and the importer requires its output.

use std::fs;
use std::path::{Path, PathBuf};

use alloy::config::Config;

const TOML: &str = "[build]\nin = \"src\"\nout = \"build\"\n";
const UTIL: &str = "export function double(n: number): number\n    return n * 2\nend\n";

/// A folder of sibling projects, each with `src/` and the same
/// `alloy.toml`.
fn workspace(name: &str, projects: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-across-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);

    for p in projects {
        fs::create_dir_all(dir.join(p).join("src")).unwrap();
        fs::write(dir.join(p).join("alloy.toml"), TOML).unwrap();
    }

    dir
}

fn build(root: &Path) -> alloy::build::Report {
    let config = Config::load(&root.join("alloy.toml")).unwrap();

    alloy::build::run_project(root, &config).unwrap()
}

fn messages(report: &alloy::build::Report) -> Vec<String> {
    report
        .diagnostics
        .iter()
        .map(|(rel, d)| format!("{}:{} {}", rel.display(), d.start, d.message))
        .collect()
}

#[test]
fn a_build_requires_the_output_of_the_other_project() {
    let dir = workspace("two", &["main", "shared"]);
    fs::write(dir.join("shared/src/util.aly"), UTIL).unwrap();
    fs::write(
        dir.join("main/src/main.aly"),
        "import { double } from \"../../shared/src/util\"\n\nprint(double(21))\n",
    )
    .unwrap();

    let report = build(&dir.join("main"));

    assert!(report.is_clean(), "{:?}", messages(&report));
    assert_eq!(
        fs::read_to_string(dir.join("main/build/main.luau")).unwrap(),
        "local _m1 = require(\"../../shared/build/util\") local double = _m1.double\n\nprint(double(21))\n"
    );
    // The dependency built under its own `out`, runtime and all. The
    // project's counts leave those files out, so a note carries them.
    assert!(dir.join("shared/build/util.luau").is_file());
    assert!(dir.join("shared/build/alloy.luau").is_file());
    assert!(!dir.join("main/build/util.luau").exists());
    assert_eq!(
        report.notes,
        vec!["dependency ../shared: 1 written, 0 up to date".to_string()]
    );

    let again = build(&dir.join("main"));

    assert_eq!(
        again.notes,
        vec!["dependency ../shared: 0 written, 1 up to date".to_string()]
    );

    // `check` follows the same route and writes nothing.
    let _ = fs::remove_dir_all(dir.join("shared/build"));
    let config = Config::load(&dir.join("main/alloy.toml")).unwrap();
    let report = alloy::build::check_project(&dir.join("main"), &config).unwrap();

    assert!(report.is_clean(), "{:?}", messages(&report));
    assert!(!dir.join("shared/build").exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_chain_of_three_builds_from_the_end() {
    let dir = workspace("chain", &["a", "b", "c"]);
    fs::write(
        dir.join("a/src/a.aly"),
        "import { b } from \"../../b/src/b\"\nprint(b(1))\n",
    )
    .unwrap();
    fs::write(
        dir.join("b/src/b.aly"),
        "import { c } from \"../../c/src/c\"\nexport function b(n: number): number\n    return c(n) + 1\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("c/src/c.aly"),
        "export function c(n: number): number\n    return n * 10\nend\n",
    )
    .unwrap();

    let report = build(&dir.join("a"));

    assert!(report.is_clean(), "{:?}", messages(&report));
    assert!(
        fs::read_to_string(dir.join("a/build/a.luau"))
            .unwrap()
            .contains("require(\"../../b/build/b\")")
    );
    assert!(
        fs::read_to_string(dir.join("b/build/b.luau"))
            .unwrap()
            .contains("require(\"../../c/build/c\")")
    );
    assert!(dir.join("c/build/c.luau").is_file());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cycle_of_projects_is_one_report_naming_both() {
    let dir = workspace("cycle", &["a", "b"]);
    fs::write(
        dir.join("a/src/a.aly"),
        "import { b } from \"../../b/src/b\"\nexport local a = b\n",
    )
    .unwrap();
    fs::write(
        dir.join("b/src/b.aly"),
        "import { a } from \"../../a/src/a\"\nexport local b = a\n",
    )
    .unwrap();

    let report = build(&dir.join("a"));
    let messages = messages(&report);

    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].starts_with(
            "a.aly:18 \"../../b/src/b\" is in the project at ../b, which does not build:"
        ),
        "{}",
        messages[0]
    );
    assert!(
        messages[0].ends_with(
            "this project and ../b import each other; move the shared part into a third project"
        ),
        "{}",
        messages[0]
    );
    assert!(!dir.join("a/build/a.luau").exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_under_no_project_reports_at_the_import() {
    let dir = workspace("loose", &["main"]);
    fs::create_dir_all(dir.join("loose")).unwrap();
    fs::write(dir.join("loose/util.aly"), UTIL).unwrap();
    fs::write(
        dir.join("main/src/main.aly"),
        "import { double } from \"../../loose/util\"\nprint(double(2))\n",
    )
    .unwrap();

    let report = build(&dir.join("main"));

    assert_eq!(
        messages(&report),
        vec![
            "main.aly:23 \"../../loose/util\" is outside this project and no alloy.toml holds it; give it a project (`alloy init` there) or move it under [build] in".to_string()
        ]
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_dependency_that_fails_names_its_first_error() {
    let dir = workspace("bad", &["main", "shared"]);
    fs::write(dir.join("shared/src/util.aly"), "local x = y ??\n").unwrap();
    fs::write(
        dir.join("main/src/main.aly"),
        "import { double } from \"../../shared/src/util\"\nprint(double(2))\n",
    )
    .unwrap();

    let report = build(&dir.join("main"));
    let messages = messages(&report);

    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].starts_with(
            "main.aly:23 \"../../shared/src/util\" is in the project at ../shared, which does not build: ../shared/src/util.aly:"
        ),
        "{}",
        messages[0]
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_data_file_of_the_other_project_builds_under_its_out() {
    let dir = workspace("data", &["main", "shared"]);
    fs::write(dir.join("shared/src/cfg.json"), "{ \"answer\": 42 }\n").unwrap();
    fs::write(
        dir.join("main/src/main.aly"),
        "import cfg from \"../../shared/src/cfg.json\"\nprint(cfg.answer)\n",
    )
    .unwrap();

    let report = build(&dir.join("main"));

    assert!(report.is_clean(), "{:?}", messages(&report));
    assert!(
        fs::read_to_string(dir.join("main/build/main.luau"))
            .unwrap()
            .contains("require(\"../../shared/build/cfg\")")
    );
    assert!(dir.join("shared/build/cfg.luau").is_file());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn flux_sees_the_other_project_and_types_the_import() {
    let dir = workspace("flux", &["main", "shared"]);
    fs::write(dir.join("shared/src/util.aly"), UTIL).unwrap();
    fs::write(
        dir.join("main/src/main.aly"),
        "import { double } from \"../../shared/src/util\"\n\nprint(double(\"x\"))\n",
    )
    .unwrap();

    let root = dir.join("main");
    let config = Config::load(&root.join("alloy.toml")).unwrap();
    let report = alloy::build::flux_project(&root, &config).unwrap();

    assert!(report.is_clean(), "{:?}", messages(&report));
    // The check artifact requires the dependency's output, and the
    // dependency's artifact rides along for the checker's mirror.
    assert!(
        report.checks[0]
            .check
            .contains("require(\"../../shared/build/util\")"),
        "{}",
        report.checks[0].check
    );
    assert!(
        report
            .dep_artifacts
            .iter()
            .any(|(p, text)| p.ends_with("shared/build/util.luau") && text.contains("double")),
        "{:?}",
        report
            .dep_artifacts
            .iter()
            .map(|(p, _)| p)
            .collect::<Vec<_>>()
    );
    assert!(!dir.join("shared/build").exists(), "flux writes nothing");

    // Only a machine without a working luau-lsp skips: an analyzer that
    // runs and says nothing is the bug, not a reason to pass.
    if alloy::typecheck::find_luau_lsp(&config.flux).is_none() {
        eprintln!("skipped: luau-lsp is not installed");

        return;
    }

    let analysis = alloy::typecheck::analyze(&root, &config, &report.checks, &report.dep_artifacts)
        .expect("the type check runs");
    let errors: Vec<String> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{}:{} {}", d.line, d.col, d.message))
        .collect();

    assert_eq!(
        errors,
        vec!["3:16 Expected this to be 'number', but got 'string'".to_string()]
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn one_file_follows_the_same_route() {
    let dir = workspace("one", &["main", "shared"]);
    fs::write(dir.join("shared/src/util.aly"), UTIL).unwrap();
    let path = dir.join("main/src/main.aly");
    let source = "import { double } from \"../../shared/src/util\"\nprint(double(21))\n";
    fs::write(&path, source).unwrap();

    let options = alloy::EmitOptions::default().imports_for_file(&path, source);
    let out = alloy::compile_file(&path.to_string_lossy(), source, &options, None, None).unwrap();
    let outside = alloy::build::file_outside(&path, None, &out.imports, &out.data_refs, true);

    assert!(outside.problems.is_empty(), "{:?}", outside.problems);
    assert_eq!(
        outside.rewrites,
        vec![(
            "../../shared/src/util".to_string(),
            "../../shared/build/util".to_string()
        )]
    );
    assert!(dir.join("shared/build/util.luau").is_file());

    // `--out` moves the output, and the require follows from there.
    let outside = alloy::build::file_outside(
        &path,
        Some(&dir.join("main/dist/main.luau")),
        &out.imports,
        &out.data_refs,
        false,
    );

    assert_eq!(outside.rewrites[0].1, "../../shared/build/util");

    let _ = fs::remove_dir_all(&dir);
}
