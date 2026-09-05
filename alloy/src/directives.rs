//! The two comment directives that silence diagnostics.
//!
//! `--@alloy-nocheck` anywhere in a file silences every diagnostic in
//! it: the compiler's, the lints, and the checker's. `--@alloy-ignore`
//! on a line of its own silences the next line that holds code; at the
//! end of a code line it silences that line. Both reach the checker's
//! errors through the language server, which drops a diagnostic on a
//! silenced line before the editor sees it.

use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct Directives {
    /// The whole file is silent.
    pub nocheck: bool,
    /// Silenced lines, zero-based.
    ignored: HashSet<usize>,
}

const IGNORE: &str = "--@alloy-ignore";
const NOCHECK: &str = "--@alloy-nocheck";

/// Reads the directives of a source.
pub fn scan(src: &str) -> Directives {
    let mut out = Directives::default();
    let mut pending = false;

    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with(NOCHECK) {
            out.nocheck = true;
        }

        if trimmed.starts_with(IGNORE) {
            // A directive line: the next line with code is silent.
            pending = true;

            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if pending {
            out.ignored.insert(i);
            pending = false;
        }

        if line.contains(IGNORE) {
            out.ignored.insert(i);
        }
    }

    out
}

impl Directives {
    /// Whether a diagnostic on `line` (zero-based) shows.
    pub fn allows(&self, line: usize) -> bool {
        !self.nocheck && !self.ignored.contains(&line)
    }

    /// Whether any directive is present, so a caller can skip the work.
    pub fn is_empty(&self) -> bool {
        !self.nocheck && self.ignored.is_empty()
    }
}

/// The zero-based line of a byte offset.
pub fn line_of(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ignore_line_silences_the_next_code_line() {
        let d = scan("local a = 1\n--@alloy-ignore\n\n-- note\nlocal b = 2\nlocal c = 3\n");
        assert!(d.allows(0));
        assert!(!d.allows(4));
        assert!(d.allows(5));
    }

    #[test]
    fn a_trailing_ignore_silences_its_own_line() {
        let d = scan("local a = 1 --@alloy-ignore\nlocal b = 2\n");
        assert!(!d.allows(0));
        assert!(d.allows(1));
    }

    #[test]
    fn nocheck_silences_the_file() {
        let d = scan("--@alloy-nocheck\nlocal a = 1\n");
        assert!(d.nocheck);
        assert!(!d.allows(1));
    }

    #[test]
    fn the_compiler_drops_silenced_diagnostics() {
        let src = "enum D as\n    A\n    B\nend\nlocal d: D = D.A\n--@alloy-ignore\nmatch d with\n    case A then print(1)\nend\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        let loud = crate::compile(&src.replace("--@alloy-ignore\n", "")).unwrap();
        assert_eq!(loud.diagnostics.len(), 1);
    }
}
