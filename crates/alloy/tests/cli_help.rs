//! `alloy <command> --help` prints the usage and runs nothing. The
//! subcommands of `self` and `ingot` are the ones that acted: `alloy
//! self uninstall --help` removed the install.

use std::path::Path;
use std::process::Command;

const ALLOY: &str = env!("CARGO_BIN_EXE_alloy");

/// A directory that holds an install: the two binaries `self
/// uninstall` removes.
fn fake_install(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-help-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the folder");

    for binary in ["alloy", "alloy-lsp"] {
        std::fs::write(dir.join(binary), "binary\n").expect("the file");
    }

    dir
}

fn installed(dir: &Path) -> bool {
    dir.join("alloy").is_file() && dir.join("alloy-lsp").is_file()
}

/// Every `self` and `ingot` subcommand answers `--help` and `-h` with
/// its usage, and touches nothing.
#[test]
fn help_of_a_subcommand_runs_nothing() {
    let dir = fake_install("sub");

    for (command, subs, usage) in [
        (
            "self",
            ["install", "update", "uninstall", "code", "schema"].as_slice(),
            "Usage: alloy self",
        ),
        (
            "ingot",
            ["new", "info", "install", "update", "run"].as_slice(),
            "Usage: alloy ingot",
        ),
    ] {
        for sub in subs {
            for flag in ["--help", "-h"] {
                let mut run = Command::new(ALLOY);
                run.args([command, sub, flag]).current_dir(&dir);

                // `--dir` is an option of `self` alone.
                if command == "self" {
                    run.arg("--dir").arg(&dir);
                }

                let out = run.output().expect("the run");
                let text = String::from_utf8_lossy(&out.stdout);

                assert!(out.status.success(), "alloy {command} {sub} {flag} failed");
                assert!(
                    text.contains(usage),
                    "alloy {command} {sub} {flag} printed {text}"
                );
                assert!(
                    installed(&dir),
                    "alloy {command} {sub} {flag} removed a file"
                );
            }
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A flag `self` does not take stops the run, so a typo never removes
/// an install.
#[test]
fn an_unknown_flag_stops_uninstall() {
    let dir = fake_install("flag");
    let out = Command::new(ALLOY)
        .args(["self", "uninstall", "--forse", "--dir"])
        .arg(&dir)
        .output()
        .expect("the run");

    assert!(!out.status.success());
    assert!(installed(&dir), "the install is gone");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("`--forse` is not an option"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
