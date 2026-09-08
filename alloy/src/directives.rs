//! The comment directives that silence diagnostics.
//!
//! `--@alloy-nocheck` anywhere in a file silences every diagnostic in
//! it: the compiler's, the lints, and the checker's. `--@alloy-ignore`
//! on a line of its own silences the next line that holds code; at the
//! end of a code line it silences that line. `--@alloy-expect-error`
//! silences the same way, and is itself an error when the line it
//! covers has none. All reach the checker's errors through the language
//! server, which drops a diagnostic on a silenced line before the
//! editor sees it.

use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Default, Clone)]
pub struct Directives {
    /// The whole file is silent.
    pub nocheck: bool,
    /// Silenced lines, zero-based.
    ignored: HashSet<usize>,
    /// Lines that must hold an error, each with its directive's line.
    expected: BTreeMap<usize, usize>,
}

const IGNORE: &str = "--@alloy-ignore";
const NOCHECK: &str = "--@alloy-nocheck";
pub const EXPECT: &str = "--@alloy-expect-error";

/// The message of an `--@alloy-expect-error` that covers a clean line.
pub const UNMET: &str = "the `--@alloy-expect-error` directive covers a line with no error";

/// Reads the directives of a source.
pub fn scan(src: &str) -> Directives {
    let mut out = Directives::default();
    let mut pending = false;
    let mut expecting = None;

    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with(NOCHECK) {
            out.nocheck = true;
        }

        if trimmed.starts_with(EXPECT) {
            expecting = Some(i);

            continue;
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

        if let Some(at) = expecting.take() {
            out.expected.insert(i, at);
        }

        if line.contains(EXPECT) {
            out.expected.insert(i, i);
        } else if line.contains(IGNORE) {
            out.ignored.insert(i);
        }
    }

    out
}

impl Directives {
    /// Whether a diagnostic on `line` (zero-based) shows.
    pub fn allows(&self, line: usize) -> bool {
        !self.nocheck && !self.ignored.contains(&line) && !self.expected.contains_key(&line)
    }

    /// Whether the line is one an `--@alloy-expect-error` covers.
    pub fn expects(&self, line: usize) -> bool {
        self.expected.contains_key(&line)
    }

    /// The directive lines whose covered line is not in `errored`: each
    /// is an error of its own.
    pub fn unmet(&self, errored: &HashSet<usize>) -> Vec<usize> {
        self.expected
            .iter()
            .filter(|(line, _)| !errored.contains(line))
            .map(|(_, at)| *at)
            .collect()
    }

    /// Whether any directive is present, so a caller can skip the work.
    pub fn is_empty(&self) -> bool {
        !self.nocheck && self.ignored.is_empty() && self.expected.is_empty()
    }
}

/// The byte range of the directive on `line`, for a diagnostic.
pub fn span_of_line(src: &str, line: usize) -> (usize, usize) {
    let mut start = 0;

    for (i, l) in src.split_inclusive('\n').enumerate() {
        if i == line {
            let text = l.trim_end_matches(['\n', '\r']);
            let lead = text.len() - text.trim_start().len();

            return (start + lead, start + text.len());
        }

        start += l.len();
    }

    (src.len(), src.len())
}

/// The zero-based line of a byte offset.
pub fn line_of(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expected_error_covers_the_next_line_and_reports_a_clean_one() {
        let d = scan(
            "--@alloy-expect-error\nlocal a: number = \"s\"\nlocal b = 1 --@alloy-expect-error\nlocal c = 2\n",
        );
        assert!(!d.allows(1));
        assert!(!d.allows(2));
        assert!(d.allows(3));
        assert!(d.expects(1) && d.expects(2));
        assert_eq!(d.unmet(&HashSet::from([1])), vec![2]);
        assert_eq!(d.unmet(&HashSet::from([1, 2])), Vec::<usize>::new());
        assert_eq!(span_of_line("a\n  --@alloy-expect-error\n", 1), (4, 25));
    }

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
