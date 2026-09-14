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
const WARNS: &str = "export struct Box as\n    value: number\nend\nprint(Box)\n";

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
