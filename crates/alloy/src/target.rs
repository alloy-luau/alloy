//! The name a release gives the machine it built for.
//!
//! Rust builds for a target triple, `x86_64-unknown-linux-gnu`. A
//! release names its files for a reader instead: `linux-x64`. The
//! `unknown` of a triple is its vendor field, which Linux leaves
//! empty, and a reader on a downloads page has no use for it.
//!
//! The toolchain and the ingot fetcher both read this, so an ingot
//! author names an asset the way `alloy self update` looks for one.

/// The release name for an arch and an OS, as `std::env::consts`
/// spells them. The pair is an argument so a test can ask for a
/// machine it is not running on.
pub fn label_for(arch: &str, os: &str) -> Option<&'static str> {
    Some(match (arch, os) {
        ("x86_64", "linux") => "linux-x64",
        ("aarch64", "linux") => "linux-arm64",
        ("x86_64", "macos") => "macos-x64",
        ("aarch64", "macos") => "macos-arm64",
        ("x86_64", "windows") => "windows-x64",
        _ => return None,
    })
}

/// The release name for the machine this binary runs on.
pub fn label() -> Option<&'static str> {
    label_for(std::env::consts::ARCH, std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_the_release_builds_has_a_label() {
        assert_eq!(label_for("x86_64", "linux"), Some("linux-x64"));
        assert_eq!(label_for("aarch64", "linux"), Some("linux-arm64"));
        assert_eq!(label_for("x86_64", "macos"), Some("macos-x64"));
        assert_eq!(label_for("aarch64", "macos"), Some("macos-arm64"));
        assert_eq!(label_for("x86_64", "windows"), Some("windows-x64"));
    }

    #[test]
    fn a_machine_the_release_skips_has_no_label() {
        assert_eq!(label_for("riscv64", "linux"), None);
        assert_eq!(label_for("aarch64", "windows"), None);
    }

    #[test]
    fn a_label_carries_no_vendor_field() {
        for l in [
            "linux-x64",
            "linux-arm64",
            "macos-x64",
            "macos-arm64",
            "windows-x64",
        ] {
            assert!(!l.contains("unknown"), "{l} still reads as a triple");
            assert_eq!(l.split('-').count(), 2, "{l} is not <os>-<arch>");
        }
    }
}
