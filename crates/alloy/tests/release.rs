//! The release pipeline's two guards, checked here so a bad version
//! shape fails at `cargo test` and not at the tag.
//!
//! The workflow compares the tag minus `v` against every crate; the
//! script decides which versions it accepts. A pre-release like
//! `0.1.0-rc` has to pass both.

use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let path = workspace().join(rel);

    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The first `version = "..."` of a manifest, which is the package's
/// own; the same line `grep -m1 '^version'` takes in the workflow.
fn package_version(dir: &str) -> String {
    read(&format!("{dir}/Cargo.toml"))
        .lines()
        .find(|l| l.starts_with("version"))
        .and_then(|l| l.split('"').nth(1).map(str::to_string))
        .unwrap_or_else(|| panic!("{dir}/Cargo.toml has no version line"))
}

/// The directories the workflow's guard walks.
fn guarded_crates() -> Vec<String> {
    let yaml = read(".github/workflows/release.yml");
    let line = yaml
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("for crate in"))
        .expect("the guard loop is in release.yml");

    line.trim_start_matches("for crate in")
        .trim_end_matches("; do")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_tag_guard_passes_for_this_version() {
    // What the workflow computes from the tag.
    let tag = format!("v{}", alloy::VERSION);
    let version = tag.trim_start_matches('v');

    for name in guarded_crates() {
        let dir = format!("crates/{name}");
        assert_eq!(
            package_version(&dir),
            version,
            "{dir}/Cargo.toml disagrees with the tag {tag}"
        );
    }
}

#[test]
fn the_guard_walks_every_member() {
    let root = read("Cargo.toml");
    let members: Vec<String> = root
        .lines()
        .find(|l| l.starts_with("members"))
        .expect("the workspace lists its members")
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    let guarded = guarded_crates();

    for member in members {
        // The members sit under `crates/`; the guard names them bare.
        let name = member.trim_start_matches("crates/").to_string();
        assert!(
            guarded.contains(&name),
            "the release guard skips {member}; a mismatched version would reach crates.io"
        );
    }
}

#[test]
fn the_workspace_version_matches_the_crates() {
    let root = read("Cargo.toml");
    let version = root
        .lines()
        .find(|l| l.starts_with("version"))
        .and_then(|l| l.split('"').nth(1))
        .expect("the workspace names a version");

    assert_eq!(version, alloy::VERSION);
}

/// Every internal dependency names the workspace version. A path
/// dependency that carries a stale version publishes a crate whose
/// requirement does not match what the release put on crates.io.
#[test]
fn every_path_dependency_names_this_version() {
    for name in guarded_crates() {
        let manifest = read(&format!("crates/{name}/Cargo.toml"));

        for line in manifest.lines() {
            // An internal dependency names a path under `crates/`.
            if !line.contains("path = \"../") || !line.contains("version = ") {
                continue;
            }

            let version = line
                .split("version = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("crates/{name}/Cargo.toml: {line}"));

            assert_eq!(
                version,
                alloy::VERSION,
                "crates/{name}/Cargo.toml names {version}, not {}: {line}",
                alloy::VERSION
            );
        }
    }
}

/// The release script bumps every one of those versions. A dependency
/// the script does not name keeps the old number through a release.
#[test]
fn the_release_script_bumps_every_path_dependency() {
    let script = read("scripts/release.sh");

    for name in guarded_crates() {
        let manifest = read(&format!("crates/{name}/Cargo.toml"));

        for line in manifest.lines() {
            if !line.contains("path = \"../") || !line.contains("version = ") {
                continue;
            }

            // The dependency reads as `dep = {` or as `package = "dep"`.
            let key = line
                .split("package = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    line.split('=')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string()
                });

            assert!(
                script.contains(&key),
                "scripts/release.sh never names {key}, so crates/{name}/Cargo.toml keeps its old version"
            );
        }
    }
}

/// The pattern `scripts/release.sh` holds its argument to.
fn release_pattern() -> String {
    let script = read("scripts/release.sh");
    let line = script
        .lines()
        .find(|l| l.contains("=~ ^[0-9]"))
        .expect("release.sh holds the version to a pattern");
    let start = line.find("=~ ").expect("the test operator") + 3;
    let rest = &line[start..];
    let end = rest.find(" ]]").unwrap_or(rest.len());

    rest[..end].trim().to_string()
}

/// Runs the script's own pattern under bash. Without bash the check
/// cannot run, and the test says nothing.
fn accepted(version: &str) -> Option<bool> {
    let script = format!(
        "if [[ \"$1\" =~ {} ]]; then exit 0; else exit 1; fi",
        release_pattern()
    );
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .arg("release-test")
        .arg(version)
        .status()
        .ok()?;

    Some(status.success())
}

#[test]
fn the_release_script_takes_a_release_candidate() {
    let Some(rc) = accepted("0.1.0-rc") else {
        eprintln!("no bash; the release.sh pattern is not checked");

        return;
    };

    assert!(rc, "release.sh must take 0.1.0-rc");
    assert_eq!(accepted("1.2.3"), Some(true));
    assert_eq!(accepted("1.2.3-beta.1"), Some(true));
    assert_eq!(accepted(alloy::VERSION), Some(true));
    assert_eq!(accepted("rc"), Some(false));
    assert_eq!(accepted("0.1"), Some(false));
    assert_eq!(accepted("v0.1.0-rc"), Some(false));
}
