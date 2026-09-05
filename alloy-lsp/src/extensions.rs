//! Extension methods on foreign types reach the analyzer through the
//! definitions. At startup the server reads every `.aly` under the root,
//! collects the `impl` blocks on Instance classes and datatypes, and
//! writes a patched copy of each definitions file that mentions the
//! target. The injection itself lives in `alloy::extensions`, which
//! `alloy flux` shares.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use alloy::extensions::Extension;

use crate::log;

/// Every extension declared in the given files.
pub fn collect(files: &[PathBuf]) -> Vec<Extension> {
    let mut out = Vec::new();

    for path in files {
        if let Ok(source) = std::fs::read_to_string(path) {
            out.extend(alloy::extensions::collect(&source));
        }
    }

    out
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("alloy-lsp")
}

/// A definitions file with the extensions injected, written to the
/// cache directory. The original path comes back when nothing applies.
pub fn apply(
    path: &Path,
    exts: &[Extension],
    done: &mut HashSet<usize>,
) -> Result<PathBuf, String> {
    let before = done.len();
    let target = alloy::extensions::apply(path, exts, done, &cache_dir())?;

    if done.len() > before {
        log::info(&format!(
            "{} extension methods injected into {}",
            done.len() - before,
            path.display()
        ));
    }

    Ok(target)
}

/// A definitions file that declares one helper table per primitive with
/// extensions. None when no primitive has an extension.
pub fn primitives_file(
    exts: &[Extension],
    done: &mut HashSet<usize>,
) -> Result<Option<PathBuf>, String> {
    let target = alloy::extensions::primitives_file(exts, done, &cache_dir())?;

    if let Some(t) = &target {
        log::info(&format!(
            "primitive extension helpers written to {}",
            t.display()
        ));
    }

    Ok(target)
}
