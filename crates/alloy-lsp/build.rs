//! Stamps the git commit into `alloy-lsp --version`. Every build of one
//! release prints the same version, so only the commit tells an old
//! installed server from a new one. A build outside git has no commit.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;

    (out.status.success() && !text.trim().is_empty()).then(|| text.trim().to_string())
}

fn main() {
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    }

    if let Some(commit) = git(&["rev-parse", "--short=9", "HEAD"]) {
        println!("cargo:rustc-env=ALLOY_LSP_COMMIT={commit}");
    }
}
