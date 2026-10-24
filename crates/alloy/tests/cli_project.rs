//! The project commands over a temporary project: the exit codes the
//! flags and the `[build]` table decide.

use std::path::{Path, PathBuf};
use std::process::Command;

const ALLOY: &str = env!("CARGO_BIN_EXE_alloy");

/// A project root with one `alloy.toml` and one source file.
fn project(name: &str, toml: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-project-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("the folder");
    std::fs::write(dir.join("alloy.toml"), toml).expect("the toml");
    std::fs::write(dir.join("src/main.aly"), source).expect("the source");

    dir
}

/// The exit code and the stderr of `alloy <args>` run at `root`.
fn run(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(ALLOY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("alloy runs");

    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const TOML: &str = "[build]\nin = \"src\"\nout = \"build\"\n";

/// An exported struct with no comment above it: one warning, no error.
const WARNS: &str = "export struct Box\n    value: number\nend\nprint(Box)\n";

/// `-W`, `-A`, and `-D` set a lint's level for one run, in their long
/// forms too. `alloy check` read the name after the flag as a file and
/// printed the usage note, though `lint` and `flux` took the flags.
#[test]
fn a_level_flag_reaches_the_lint_in_every_lint_command() {
    let root = project("level-flags", TOML, WARNS);

    for command in ["check", "lint", "flux"] {
        let extra: &[&str] = if command == "flux" {
            &["--no-typecheck"]
        } else {
            &[]
        };

        for (flag, code, summary) in [
            ("-D", 1, "1 denied"),
            ("--deny", 1, "1 denied"),
            ("-A", 0, "0 warnings"),
            ("--allow", 0, "0 warnings"),
            ("-W", 0, "1 warnings"),
        ] {
            let args = [&[command, flag, "missing_doc"][..], extra].concat();
            let (status, stderr) = run(&root, &args);
            assert!(!stderr.contains("usage:"), "{args:?}: {stderr}");
            assert_eq!(status, code, "{args:?}: {stderr}");
            assert!(stderr.contains(summary), "{args:?}: {stderr}");
        }
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// `alloy check --fix` with no file rewrites every source of the project,
/// as `alloy lint --fix` does, and never a package in a `.ember` folder.
/// It used to rewrite only the files named on the command line.
#[test]
fn check_fix_rewrites_the_whole_project() {
    let root = project("check-fix", TOML, "local a = 1\nprint(a)\n");
    std::fs::write(root.join("src/two.aly"), "local b = 2\nprint(b)\n").expect("the source");
    let package = root.join("src/packages/.ember/acme_lib");
    std::fs::create_dir_all(&package).expect("the package folder");
    std::fs::write(package.join("init.aly"), "local c = 3\nprint(c)\n").expect("the package");

    let (code, err) = run(&root, &["check", "--fix"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("2 fixed"), "{err}");

    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).expect("the file");
    assert!(read("src/main.aly").starts_with("const a = 1"));
    assert!(read("src/two.aly").starts_with("const b = 2"));
    assert!(read("src/packages/.ember/acme_lib/init.aly").starts_with("local c = 3"));

    // A second run finds nothing left to fix in the project's own files.
    let (_, err) = run(&root, &["check", "--fix"]);
    assert!(err.contains("0 fixed"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deny_warnings_fails_the_project_check() {
    let root = project("deny", TOML, WARNS);

    let (code, err) = run(&root, &["check"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("1 warnings"), "{err}");

    let (code, err) = run(&root, &["check", "--deny-warnings"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("1 warnings"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_input_folder_reports_from_every_command() {
    let root = project(
        "input",
        "[build]\nin = \"nosuchdir\"\nout = \"build\"\n",
        "print(1)\n",
    );

    for command in ["check", "flux", "build", "lint", "fmt"] {
        let (code, err) = run(&root, &[command]);
        assert_eq!(code, 1, "{command}: {err}");
        assert!(
            err.contains("`[build] in` names `nosuchdir`, which does not exist under"),
            "{command}: {err}"
        );
        assert_eq!(err.matches("nosuchdir").count(), 1, "{command}: {err}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// A file that reports an error is not written, to `--out` or to the
/// stdout an unnamed `--out` writes: half-lowered text used to land
/// there beside the error and the failing exit code.
#[test]
fn a_file_with_an_error_writes_no_luau_to_stdout() {
    let root = project(
        "stdout",
        TOML,
        "local total = 0\nprint(total)\nlocal u = t[ [1][1] ]\n",
    );
    let out = Command::new(ALLOY)
        .args(["build", "src/main.aly"])
        .current_dir(&root)
        .output()
        .expect("alloy runs");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("SyntaxError"), "{err}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // A file that compiles still writes its Luau there.
    std::fs::write(root.join("src/main.aly"), "print(1)\n").expect("the source");
    let out = Command::new(ALLOY)
        .args(["build", "src/main.aly"])
        .current_dir(&root)
        .output()
        .expect("alloy runs");

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "print(1) return nil\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// An analyzer that runs and fails is an error of its own: it reports
/// no diagnostic, and a clean report would say the types were checked.
#[cfg(unix)]
#[test]
fn a_luau_lsp_that_cannot_run_fails_the_type_check() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("alloy-fake-lsp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the folder");
    let fake = dir.join("luau-lsp");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho \"Failed to find tool 'luau-lsp'\" >&2\nexit 1\n",
    )
    .expect("the script");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("the mode");

    let root = project(
        "brokenlsp",
        &format!(
            "{TOML}\n[flux]\nroblox_types = false\nluau_lsp = \"{}\"\n",
            fake.display()
        ),
        "print(1)\n",
    );
    let (code, err) = run(&root, &["flux"]);

    assert_eq!(code, 1, "{err}");
    assert!(err.contains("type check failed"), "{err}");
    assert!(err.contains("Failed to find tool 'luau-lsp'"), "{err}");
    assert!(err.contains(&fake.display().to_string()), "{err}");
    assert!(err.contains("`[flux] luau_lsp`"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that does not lex fails the build, and the summary counts it
/// as `check` does.
#[test]
fn a_file_that_does_not_lex_counts_in_the_build_summary() {
    let root = project("lex", TOML, "local a = 0o17\nprint(a)\n");

    let (code, err) = run(&root, &["build"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("malformed number `0o17`"), "{err}");
    assert!(err.contains("1 diagnostics"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
}
