//! The comment directives that steer the diagnostics of one file.
//!
//! `--@alloy-nocheck` anywhere in a file silences every diagnostic in
//! it: the compiler's, the lints, and the checker's. `--@alloy-ignore`
//! on a line of its own silences the next line that holds code; at the
//! end of a code line it silences that line. `--@alloy-expect-error`
//! silences the same way, and is itself an error when the line it
//! covers has none. `--@alloy-ignore-start` and `--@alloy-ignore-end`
//! silence a region. `--@alloy-lint` sets a lint's level for the file.
//! `--@alloy-file-side` says which side of a remote the file sees;
//! `--@alloy-side` says it for the one global under it.
//! `--@alloy-preserve` keeps `alloy flux --fix` off a line. All reach
//! the checker's errors through the language server, which drops a
//! diagnostic on a silenced line before the editor sees it.

use std::collections::{BTreeMap, HashSet};

use crate::lint::Level;

/// Which side of a remote a file sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Client,
    Server,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Client => "client",
            Side::Server => "server",
        }
    }
}

/// One `--@alloy-expect-error`: the line it sits on and the reason the
/// author wrote after it.
#[derive(Debug, Clone)]
struct Expect {
    at: usize,
    /// The one-based column the directive starts at. A trailing one
    /// sits after the code, and the report points at it.
    col: usize,
    reason: Option<String>,
}

/// One `--@alloy-ignore-start` and its end, as zero-based lines. The
/// pair lines are outside the region, so a directive error on either
/// still reports. `name` limits the region to one lint or kind.
#[derive(Debug, Clone)]
struct Region {
    start: usize,
    end: usize,
    name: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Directives {
    /// The whole file is silent.
    pub nocheck: bool,
    /// Silenced lines, zero-based.
    ignored: HashSet<usize>,
    /// Lines that must hold an error, each with the directives that
    /// cover it. Two stacked directives both report.
    expected: BTreeMap<usize, Vec<Expect>>,
    /// Every `--@alloy-` comment that names no directive: the line and
    /// the word the author wrote.
    pub unknown: Vec<(usize, String)>,
    /// The levels `--@alloy-lint` sets, in the order they were written.
    /// A later one wins, so the last word in the file decides.
    levels: Vec<(String, Level)>,
    /// The `--@alloy-ignore-start` regions, closed and unclosed.
    regions: Vec<Region>,
    /// The side `--@alloy-file-side` declares for the whole file, and
    /// the line it sits on. `None` inside the pair means shared.
    pub file_side: Option<(usize, Option<Side>)>,
    /// Every `--@alloy-side`, by the line it sits on. Each one belongs
    /// to the declaration under it, not to the file.
    pub decl_sides: Vec<(usize, Option<Side>)>,
    /// Lines `--@alloy-preserve` keeps `--fix` off.
    preserved: HashSet<usize>,
    /// A directive the scan could read but not accept: the line and
    /// the message. Each is a `DirectiveError`.
    pub errors: Vec<(usize, String)>,
    /// The `--@alloy-expect-error` lines that carry no reason. The
    /// `missing_reason` lint reports each.
    pub missing_reason: Vec<usize>,
}

const IGNORE: &str = "--@alloy-ignore";
const IGNORE_START: &str = "--@alloy-ignore-start";
const IGNORE_END: &str = "--@alloy-ignore-end";
const NOCHECK: &str = "--@alloy-nocheck";
pub const EXPECT: &str = "--@alloy-expect-error";
const LINT: &str = "--@alloy-lint";
const SIDE: &str = "--@alloy-side";
const FILE_SIDE: &str = "--@alloy-file-side";
const PRESERVE: &str = "--@alloy-preserve";

/// Every directive name, in the order the docs list them.
pub const NAMES: &[&str] = &[
    IGNORE,
    IGNORE_START,
    IGNORE_END,
    EXPECT,
    NOCHECK,
    LINT,
    SIDE,
    FILE_SIDE,
    PRESERVE,
];

/// The words a side directive takes.
pub const SIDE_WORDS: &[(&str, &str)] = &[
    (
        "client",
        "The file, or the global under the directive, is the client's.",
    ),
    (
        "server",
        "The file, or the global under the directive, is the server's.",
    ),
    (
        "shared",
        "The file, or the global under the directive, runs on either side.",
    ),
];

/// The prefix every directive shares.
const PREFIX: &str = "--@alloy-";

/// The name of the lint that a bare `--@alloy-expect-error` draws.
pub const MISSING_REASON: &str = "missing_reason";

/// The message for a `--@alloy-` comment that names no directive.
pub fn unknown_message(word: &str) -> String {
    let names: Vec<String> = NAMES.iter().map(|n| format!("`{n}`")).collect();

    format!(
        "`{word}` is no directive; the directives are {}",
        names.join(", ")
    )
}

/// The message of an `--@alloy-expect-error` that covers a clean line.
pub const UNMET: &str = "the `--@alloy-expect-error` directive covers a line with no error";

/// The same message with the reason the author wrote, so a stale
/// directive is easy to place among several.
pub fn unmet_message(reason: Option<&str>) -> String {
    match reason {
        Some(r) if !r.is_empty() => format!("{UNMET}: {r}"),
        _ => UNMET.to_string(),
    }
}

/// Reads the directives of a source.
///
/// `--@alloy-lint`, `--@alloy-file-side`, `--@alloy-side`,
/// `--@alloy-ignore-start`, and
/// `--@alloy-ignore-end` sit on a line of their own. `--@alloy-ignore`,
/// `--@alloy-expect-error`, and `--@alloy-preserve` sit on their own
/// line or at the end of a line with code.
pub fn scan(src: &str) -> Directives {
    let mut out = Directives::default();
    let mut pending = false;
    let mut pending_preserve = false;
    let mut expecting: Vec<Expect> = Vec::new();
    // The open `--@alloy-ignore-start` directives: line and name. The
    // last one closes first, so a nested pair works.
    let mut open: Vec<(usize, Option<String>)> = Vec::new();

    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim();

        if let Some(word) = unknown_directive(line) {
            out.unknown.push((i, word));
        }

        if trimmed.starts_with(NOCHECK) {
            out.nocheck = true;
        }

        if let Some(rest) = leading(trimmed, LINT) {
            out.read_levels(i, rest);

            continue;
        }

        if let Some(rest) = leading(trimmed, FILE_SIDE) {
            out.read_file_side(i, rest);

            continue;
        }

        if let Some(rest) = leading(trimmed, SIDE) {
            out.read_side(i, rest);

            continue;
        }

        if let Some(rest) = leading(trimmed, IGNORE_START) {
            let name = name_argument(rest);

            // The region's filter is a lint name, and a name no lint
            // carries silences nothing.
            if let Some(n) = &name
                && !crate::lint::is_known_name(n)
            {
                out.errors.push((
                    i,
                    format!(
                        "the `{IGNORE_START}` directive names `{n}`, which is neither a lint nor a group; `alloy lint --list` has them"
                    ),
                ));
            }

            open.push((i, name));

            continue;
        }

        if let Some(rest) = leading(trimmed, IGNORE_END) {
            let name = name_argument(rest);

            if let Some(n) = &name
                && !crate::lint::is_known_name(n)
            {
                out.errors.push((
                    i,
                    format!(
                        "the `{IGNORE_END}` directive names `{n}`, which is neither a lint nor a group; `alloy lint --list` has them"
                    ),
                ));
            }

            close_region(&mut out, &mut open, i, name);

            continue;
        }

        if leading(trimmed, PRESERVE).is_some() {
            pending_preserve = true;

            continue;
        }

        if let Some(rest) = leading(trimmed, EXPECT) {
            let reason = reason_of(rest);

            if reason.is_none() {
                out.missing_reason.push(i);
            }

            expecting.push(Expect {
                at: i,
                col: line.find(EXPECT).map_or(0, |at| at + 1),
                reason,
            });

            continue;
        }

        if leading(trimmed, IGNORE).is_some() {
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

        if pending_preserve {
            out.preserved.insert(i);
            pending_preserve = false;
        }

        if !expecting.is_empty() {
            out.expected
                .entry(i)
                .or_default()
                .append(&mut std::mem::take(&mut expecting));
        }

        if trailing(line, PRESERVE).is_some() {
            out.preserved.insert(i);
        }

        if let Some(rest) = trailing(line, EXPECT) {
            let reason = reason_of(rest);

            if reason.is_none() {
                out.missing_reason.push(i);
            }

            out.expected.entry(i).or_default().push(Expect {
                at: i,
                col: line.find(EXPECT).map_or(0, |at| at + 1),
                reason,
            });
        } else if trailing(line, IGNORE).is_some() {
            out.ignored.insert(i);
        }
    }

    // A start with no end silences to the end of the file, which is
    // what the author asked for, and reports on its own line.
    for (at, name) in open {
        out.errors.push((
            at,
            format!("this `{IGNORE_START}` directive has no `{IGNORE_END}` after it"),
        ));
        out.regions.push(Region {
            start: at,
            end: usize::MAX,
            name,
        });
    }

    out.regions.sort_by_key(|r| (r.start, r.end));

    out
}

/// Closes the innermost open region an `--@alloy-ignore-end` matches:
/// the one with the same name, or the last one when the end names
/// nothing. An end that closes nothing is an error of its own.
fn close_region(
    out: &mut Directives,
    open: &mut Vec<(usize, Option<String>)>,
    at: usize,
    name: Option<String>,
) {
    let found = match &name {
        None => open.pop(),

        Some(n) => open
            .iter()
            .rposition(|(_, o)| o.as_deref() == Some(n.as_str()))
            .map(|i| open.remove(i)),
    };

    match found {
        Some((start, region_name)) => out.regions.push(Region {
            start,
            end: at,
            name: region_name,
        }),

        None => {
            let what = match &name {
                Some(n) => format!("`{IGNORE_END} {n}`"),
                None => format!("`{IGNORE_END}`"),
            };
            out.errors.push((
                at,
                format!("the {what} directive closes no `{IGNORE_START}`"),
            ));
        }
    }
}

/// The text after a directive that opens the line. The character after
/// the name must end it, so `--@alloy-ignore-start` never answers for
/// `--@alloy-ignore`.
fn leading<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?;

    ends_the_name(rest).then(|| rest.trim())
}

/// The text after a directive the line holds anywhere, for the forms
/// an author may write at the end of a line with code.
fn trailing<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.match_indices(name)
        .find(|(at, _)| ends_the_name(&line[at + name.len()..]))
        .map(|(at, _)| line[at + name.len()..].trim())
}

/// Whether the text after a directive name starts a new word, so the
/// name is whole and not the head of a longer one.
fn ends_the_name(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '-' && c != '_')
}

/// The reason an author wrote after a directive, or `None` when the
/// rest of the line is empty or is punctuation alone.
fn reason_of(rest: &str) -> Option<String> {
    let text = rest.trim_start_matches([':', '-', ' ', '\t']).trim();

    (!text.is_empty()).then(|| text.to_string())
}

/// The single name after `--@alloy-ignore-start`, or `None`.
fn name_argument(rest: &str) -> Option<String> {
    let word = rest.split_whitespace().next()?;

    (!word.is_empty()).then(|| word.to_string())
}

impl Directives {
    /// Reads `--@alloy-lint name=level, other=level`. An unknown name
    /// or level is an error on the directive's line.
    fn read_levels(&mut self, at: usize, rest: &str) {
        if rest.is_empty() {
            self.errors.push((
                at,
                format!("the `{LINT}` directive takes `<lint>=<allow|warn|deny>`, one or more, separated by commas"),
            ));

            return;
        }

        for part in rest.split(',') {
            let part = part.trim();

            if part.is_empty() {
                continue;
            }

            let Some((name, level)) = part.split_once('=') else {
                self.errors.push((
                    at,
                    format!("the `{LINT}` directive says `{part}`, which has no level; write `{part}=warn`"),
                ));

                continue;
            };
            let name = name.trim();
            // A word after the level is the author's note: `warn -- why`.
            let level = level.split_whitespace().next().unwrap_or("");

            if !crate::lint::is_known_name(name) {
                self.errors.push((
                    at,
                    format!(
                        "the `{LINT}` directive names `{name}`, which is neither a lint nor a group; `alloy lint --list` has them"
                    ),
                ));

                continue;
            }

            let Some(level) = Level::from_name(level) else {
                self.errors.push((
                    at,
                    format!("the `{LINT}` directive says `{level}`, which is no level; the levels are `allow`, `warn`, and `deny`"),
                ));

                continue;
            };

            self.levels.push((name.to_string(), level));
        }
    }

    /// The side word of a directive: `client`, `server`, or `shared`,
    /// where shared is no side at all.
    fn read_side_word(&mut self, at: usize, name: &str, rest: &str) -> Option<Option<Side>> {
        match rest.split_whitespace().next() {
            Some("client") => Some(Some(Side::Client)),
            Some("server") => Some(Some(Side::Server)),
            Some("shared") => Some(None),

            other => {
                let what = other.unwrap_or("");
                self.errors.push((
                    at,
                    format!(
                        "the `{name}` directive says `{what}`; the sides are `client`, `server`, and `shared`"
                    ),
                ));

                None
            }
        }
    }

    /// Reads `--@alloy-side client`: the side of the global under it.
    fn read_side(&mut self, at: usize, rest: &str) {
        if let Some(side) = self.read_side_word(at, SIDE, rest) {
            self.decl_sides.push((at, side));
        }
    }

    /// Reads `--@alloy-file-side client`: the side of the whole file.
    fn read_file_side(&mut self, at: usize, rest: &str) {
        let Some(side) = self.read_side_word(at, FILE_SIDE, rest) else {
            return;
        };

        match self.file_side {
            Some((_, first)) if first != side => self.errors.push((
                at,
                format!(
                    "this file already has a `{FILE_SIDE}` directive, which says `{}`",
                    first.map(Side::name).unwrap_or("shared")
                ),
            )),

            Some(_) => {}

            None => self.file_side = Some((at, side)),
        }
    }

    /// The side `--@alloy-side` gives the declaration that starts on
    /// `line`: the directive right above it, past blank and comment
    /// lines. `None` when no directive covers the declaration.
    pub fn side_above(&self, src: &str, line: usize) -> Option<Option<Side>> {
        let lines: Vec<&str> = src.lines().collect();
        let mut at = line;

        while at > 0 {
            at -= 1;
            let text = lines.get(at).map(|l| l.trim()).unwrap_or("");

            if let Some((_, side)) = self.decl_sides.iter().find(|(l, _)| *l == at) {
                return Some(*side);
            }

            if text.is_empty() || text.starts_with("--") || text.starts_with('@') {
                continue;
            }

            return None;
        }

        None
    }

    /// Whether a diagnostic on `line` (zero-based) shows. A diagnostic
    /// with a lint name or a checker kind goes through `allows_named`,
    /// which reads the regions that name one.
    pub fn allows(&self, line: usize) -> bool {
        self.allows_named(line, None)
    }

    /// Whether a diagnostic on `line` shows, given the lint name or the
    /// checker kind it carries.
    pub fn allows_named(&self, line: usize, name: Option<&str>) -> bool {
        !self.nocheck
            && !self.ignored.contains(&line)
            && !self.expected.contains_key(&line)
            && !self.in_region(line, name)
    }

    /// Whether a lint on `line` shows. `missing_reason` is about the
    /// directive line itself, so an expectation there must not hide the
    /// lint that says the expectation needs a reason.
    pub fn allows_lint(&self, line: usize, name: &str) -> bool {
        if name == MISSING_REASON {
            return !self.nocheck
                && !self.ignored.contains(&line)
                && !self.in_region(line, Some(name));
        }

        self.allows_named(line, Some(name))
    }

    /// Whether a region covers `line`. A region that names a lint or a
    /// kind covers only a diagnostic of that name.
    fn in_region(&self, line: usize, name: Option<&str>) -> bool {
        self.regions.iter().any(|r| {
            line > r.start
                && line < r.end
                && match (&r.name, name) {
                    (None, _) => true,
                    (Some(want), Some(got)) => want == got,
                    (Some(_), None) => false,
                }
        })
    }

    /// Whether `alloy flux --fix` must leave `line` as it is.
    pub fn preserves(&self, line: usize) -> bool {
        self.preserved.contains(&line)
    }

    /// The level this file sets for a lint, a group, or the `luau`
    /// group, over what `alloy.toml` says. The lint's own name beats
    /// its group's, as in `[lint]`, and the last of two wins.
    pub fn level_override(&self, name: &str) -> Option<Level> {
        let group = crate::lint::group_name(name);
        let last = |key: &str| {
            self.levels
                .iter()
                .rev()
                .find(|(n, _)| n == key)
                .map(|(_, l)| *l)
        };

        last(name).or_else(|| last(group))
    }

    /// Whether the file sets any level, so a caller can skip the work.
    pub fn has_levels(&self) -> bool {
        !self.levels.is_empty()
    }

    /// Whether the line is one an `--@alloy-expect-error` covers.
    pub fn expects(&self, line: usize) -> bool {
        self.expected.contains_key(&line)
    }

    /// The directives whose covered line is not in `errored`: each is
    /// an error of its own, with the reason the author wrote.
    pub fn unmet(&self, errored: &HashSet<usize>) -> Vec<(usize, usize, Option<String>)> {
        self.expected
            .iter()
            .filter(|(line, _)| !errored.contains(line))
            .flat_map(|(_, at)| at.iter().map(|e| (e.at, e.col, e.reason.clone())))
            .collect()
    }

    /// Every directive the scan could not accept, as a line and a
    /// message: an unknown name, and the errors the readers found.
    pub fn problems(&self) -> Vec<(usize, String)> {
        let mut out: Vec<(usize, String)> = self
            .unknown
            .iter()
            .map(|(at, word)| (*at, unknown_message(word)))
            .chain(self.errors.iter().cloned())
            .collect();
        out.sort_by_key(|(at, _)| *at);

        out
    }

    /// The error a `--@alloy-side` draws when the file name already
    /// names the other side. The scan has no file name, so the caller
    /// asks for this once it knows one.
    pub fn side_problem(&self, file_name: &str) -> Option<(usize, String)> {
        let (at, side) = self.file_side?;
        let named = file_side(file_name)?;

        (Some(named) != side).then(|| {
            (
                at,
                format!(
                    "the `{FILE_SIDE} {}` directive contradicts the file name, which says `{}`",
                    side.map(Side::name).unwrap_or("shared"),
                    named.name()
                ),
            )
        })
    }

    /// Whether any directive is present, so a caller can skip the work.
    pub fn is_empty(&self) -> bool {
        !self.nocheck
            && self.ignored.is_empty()
            && self.expected.is_empty()
            && self.unknown.is_empty()
            && self.levels.is_empty()
            && self.regions.is_empty()
            && self.preserved.is_empty()
            && self.file_side.is_none()
            && self.decl_sides.is_empty()
            && self.errors.is_empty()
    }
}

/// The side a file name declares: `ui.client.aly` is the client, and
/// `main.server.aly` is the server. Any other name is shared.
pub fn file_side(file: &str) -> Option<Side> {
    let stem = file
        .strip_suffix(".aly")
        .or_else(|| file.strip_suffix(".alx"))
        .unwrap_or(file);

    if stem.ends_with(".client") {
        Some(Side::Client)
    } else if stem.ends_with(".server") {
        Some(Side::Server)
    } else {
        None
    }
}

/// The side a DataModel place puts a file on. `ServerScriptService`
/// and `ServerStorage` do not replicate, so a file there is the
/// server's. `StarterPlayer`, `StarterGui`, and `StarterPack` are
/// copied into each player, so a file there is the client's. Every
/// other service replicates to both, `ReplicatedFirst` included, so a
/// file there is shared.
pub fn mount_side(place: &[String]) -> Option<Side> {
    match place.first().map(String::as_str) {
        Some("ServerScriptService" | "ServerStorage") => Some(Side::Server),

        Some("StarterPlayer" | "StarterGui" | "StarterPack") => Some(Side::Client),

        _ => None,
    }
}

/// The side a file sees, from the strongest word to the weakest: the
/// name suffix, `--@alloy-side`, then the place the tree gives the
/// file. A file no rule reaches is shared, and runs on either side.
///
/// One function answers this for every reader: the globals of a
/// project and the surface of a `remote` both take the same side.
pub fn side_of(
    src: &str,
    file_name: &str,
    context: Option<Option<Side>>,
    place: Option<&[String]>,
) -> Option<Side> {
    if let Some(side) = file_side(file_name) {
        return Some(side);
    }

    if let Some((_, side)) = scan(src).file_side {
        return side;
    }

    if let Some(side) = context {
        return side;
    }

    place.and_then(mount_side)
}

/// The side a file sees with no tree to ask: the name, else the
/// `--@alloy-side` directive.
pub fn effective_side(src: &str, file_name: &str) -> Option<Side> {
    side_of(src, file_name, None, None)
}

/// The word of a `--@alloy-` comment that names no directive, or `None`
/// when the line carries a known one or none at all.
fn unknown_directive(line: &str) -> Option<String> {
    let at = line.find(PREFIX)?;
    let rest = &line[at..];
    let word: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '@')
        .collect();

    (!NAMES.contains(&word.as_str())).then_some(word)
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
        // The trailing directive reports at its own column, not at 1.
        assert_eq!(d.unmet(&HashSet::from([1])), vec![(2, 13, None)]);
        assert!(d.unmet(&HashSet::from([1, 2])).is_empty());
        assert_eq!(span_of_line("a\n  --@alloy-expect-error\n", 1), (4, 25));
    }

    #[test]
    fn two_stacked_expect_directives_both_report() {
        let d = scan("--@alloy-expect-error\n--@alloy-expect-error\nlocal c = 2\n");
        assert_eq!(d.unmet(&HashSet::new()), vec![(0, 1, None), (1, 1, None)]);
        assert!(d.unmet(&HashSet::from([2])).is_empty());
    }

    #[test]
    fn a_directive_no_one_declared_is_named() {
        let d = scan("--@alloy-bogus-directive\nlocal a = 1 --@alloy-ignore\n");
        assert_eq!(d.unknown, vec![(0, "--@alloy-bogus-directive".to_string())]);
        assert!(scan("--@alloy-nocheck\n").unknown.is_empty());

        for name in NAMES {
            assert!(
                scan(&format!("{name} client\n")).unknown.is_empty(),
                "{name} reads as unknown"
            );
        }
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

    // --- 1. the reason of an expectation ---------------------------------------

    #[test]
    fn an_expectation_keeps_the_reason_and_a_bare_one_draws_the_lint() {
        let d = scan("--@alloy-expect-error the solver misreads this\nlocal a = 1\n");
        assert_eq!(
            d.unmet(&HashSet::new()),
            vec![(0, 1, Some("the solver misreads this".to_string()))]
        );
        assert!(d.missing_reason.is_empty());
        assert_eq!(
            unmet_message(Some("the solver misreads this")),
            format!("{UNMET}: the solver misreads this")
        );
        assert_eq!(unmet_message(None), UNMET);

        let bare = scan("--@alloy-expect-error\nlocal a = 1\n");
        assert_eq!(bare.missing_reason, vec![0]);

        // A trailing one keeps its reason too, and reports without one.
        let tail = scan("local a = 1 --@alloy-expect-error: private on purpose\n");
        assert_eq!(
            tail.unmet(&HashSet::new()),
            vec![(0, 13, Some("private on purpose".to_string()))]
        );
        assert!(tail.missing_reason.is_empty());
        assert_eq!(
            scan("local a = 1 --@alloy-expect-error\n").missing_reason,
            [0]
        );
    }

    #[test]
    fn an_ignore_takes_a_reason_and_never_draws_the_lint() {
        let d = scan("--@alloy-ignore the new solver gets this wrong\nlocal a = 1\n");
        assert!(!d.allows(1));
        assert!(d.missing_reason.is_empty());
        assert!(
            scan("--@alloy-ignore\nlocal a = 1\n")
                .missing_reason
                .is_empty()
        );
    }

    #[test]
    fn a_bare_expectation_is_still_visible_under_its_own_line() {
        // The lint sits on the directive's own line, which the
        // expectation covers when the directive trails code.
        let d = scan("local a = 1 --@alloy-expect-error\n");
        assert!(!d.allows(0));
        assert!(d.allows_lint(0, MISSING_REASON));
        assert!(!d.allows_lint(0, "raw_require"));
    }

    // --- 2. the lint level of one file -----------------------------------------

    #[test]
    fn a_lint_directive_sets_a_level_for_the_file() {
        let d = scan("--@alloy-lint raw_require=allow\nlocal a = 1\n");
        assert_eq!(d.level_override("raw_require"), Some(Level::Allow));
        assert_eq!(d.level_override("optional_access"), None);
        assert!(d.errors.is_empty());

        // Several on one line, and one per line.
        let many =
            scan("--@alloy-lint raw_require=deny, explicit_any=warn\n--@alloy-lint style=allow\n");
        assert_eq!(many.level_override("raw_require"), Some(Level::Deny));
        assert_eq!(many.level_override("explicit_any"), Some(Level::Warn));
        // A group name covers every lint in it that no directive
        // names on its own.
        assert_eq!(many.level_override("manual_floor_div"), Some(Level::Allow));
        assert_eq!(many.level_override("raw_require"), Some(Level::Deny));
        assert!(many.errors.is_empty());

        // The last word in the file wins.
        let twice = scan("--@alloy-lint raw_require=deny\n--@alloy-lint raw_require=allow\n");
        assert_eq!(twice.level_override("raw_require"), Some(Level::Allow));
    }

    /// A markup lint carries the `alx.` prefix `[lint.rules]` gives it.
    #[test]
    fn a_lint_directive_takes_a_markup_name() {
        let d = scan("--@alloy-lint alx.static_conditional_child=allow\nlocal a = 1\n");
        assert_eq!(
            d.level_override("alx.static_conditional_child"),
            Some(Level::Allow)
        );
        assert!(d.errors.is_empty());

        let unknown = scan("--@alloy-lint alx.no_such_lint=allow\n");
        assert_eq!(unknown.errors.len(), 1);
        assert!(unknown.errors[0].1.contains("`alx.no_such_lint`"));
    }

    #[test]
    fn an_unknown_lint_or_level_is_a_directive_error() {
        let bad_name = scan("--@alloy-lint no_such_lint=warn\n");
        assert_eq!(bad_name.errors.len(), 1);
        assert!(bad_name.errors[0].1.contains("`no_such_lint`"));
        assert!(bad_name.errors[0].1.contains("directive"));
        assert_eq!(bad_name.errors[0].0, 0);

        let bad_level = scan("--@alloy-lint raw_require=loud\n");
        assert_eq!(bad_level.errors.len(), 1);
        assert!(bad_level.errors[0].1.contains("`loud`"));

        let no_level = scan("--@alloy-lint raw_require\n");
        assert_eq!(no_level.errors.len(), 1);
        assert!(no_level.errors[0].1.contains("has no level"));

        assert_eq!(scan("--@alloy-lint\n").errors.len(), 1);
    }

    // --- 3. the ignored region -------------------------------------------------

    #[test]
    fn a_region_silences_the_lines_between_its_pair() {
        let d = scan(
            "local a = 1\n--@alloy-ignore-start\nlocal b = 2\nlocal c = 3\n--@alloy-ignore-end\nlocal e = 4\n",
        );
        assert!(d.allows(0));
        assert!(!d.allows(2));
        assert!(!d.allows(3));
        assert!(d.allows(5));
        // The pair lines are outside, so an error on one still reports.
        assert!(d.allows(1) && d.allows(4));
        assert!(d.errors.is_empty());
    }

    #[test]
    fn a_named_region_silences_that_name_alone() {
        let d = scan("--@alloy-ignore-start raw_require\nlocal b = 2\n--@alloy-ignore-end\n");
        assert!(!d.allows_named(1, Some("raw_require")));
        assert!(d.allows_named(1, Some("explicit_any")));
        // A compiler diagnostic carries no name, so it stays.
        assert!(d.allows(1));
    }

    #[test]
    fn regions_nest_and_the_inner_one_closes_first() {
        let d = scan(
            "--@alloy-ignore-start\nlocal a = 1\n--@alloy-ignore-start raw_require\nlocal b = 2\n--@alloy-ignore-end raw_require\nlocal c = 3\n--@alloy-ignore-end\nlocal e = 4\n",
        );
        assert!(d.errors.is_empty());
        assert!(!d.allows(1));
        assert!(!d.allows(3));
        assert!(!d.allows(5));
        assert!(d.allows(7));
    }

    #[test]
    fn an_unclosed_start_and_a_lone_end_are_directive_errors() {
        let open = scan("--@alloy-ignore-start\nlocal a = 1\n");
        assert_eq!(open.errors.len(), 1);
        assert_eq!(open.errors[0].0, 0);
        assert!(open.errors[0].1.contains("has no"));
        // The region still runs to the end of the file.
        assert!(!open.allows(1));
        // The error's own line reports.
        assert!(open.allows(0));

        let lone = scan("local a = 1\n--@alloy-ignore-end\n");
        assert_eq!(lone.errors.len(), 1);
        assert_eq!(lone.errors[0].0, 1);
        assert!(lone.errors[0].1.contains("closes no"));
    }

    // --- 4. the side of a file -------------------------------------------------

    #[test]
    fn a_file_side_directive_names_the_side() {
        let d = scan("--@alloy-file-side client\nlocal a = 1\n");
        assert_eq!(d.file_side, Some((0, Some(Side::Client))));
        assert!(d.errors.is_empty());
        assert_eq!(
            effective_side("--@alloy-file-side server\n", "shared.aly"),
            Some(Side::Server)
        );
        // `shared` is a side word too: it says the file runs on either.
        assert_eq!(
            effective_side("--@alloy-file-side shared\n", "shared.aly"),
            None
        );
        // With no directive the file name decides.
        assert_eq!(
            effective_side("local a = 1\n", "ui.client.aly"),
            Some(Side::Client)
        );
        assert_eq!(effective_side("local a = 1\n", "shared.aly"), None);
    }

    /// `--@alloy-side` names the side of the global under it, and only
    /// that; the file's own side comes from `--@alloy-file-side`.
    #[test]
    fn a_side_directive_names_the_declaration_under_it() {
        let src = "--@alloy-side client\nglobal const A = 1\n\nglobal const B = 2\n";
        let d = scan(src);
        assert_eq!(d.decl_sides, vec![(0, Some(Side::Client))]);
        assert_eq!(d.side_above(src, 1), Some(Some(Side::Client)));
        assert_eq!(d.side_above(src, 3), None);
        // It says nothing about the file.
        assert_eq!(effective_side(src, "shared.aly"), None);
    }

    #[test]
    fn a_side_that_contradicts_the_file_name_is_an_error() {
        let d = scan("--@alloy-file-side client\n");
        assert!(d.side_problem("main.server.aly").is_some());
        assert!(d.side_problem("ui.client.aly").is_none());
        assert!(d.side_problem("shared.aly").is_none());

        let bad = scan("--@alloy-file-side middle\n");
        assert_eq!(bad.errors.len(), 1);
        assert!(bad.errors[0].1.contains("`client`, `server`, and `shared`"));

        let twice = scan("--@alloy-file-side client\n--@alloy-file-side server\n");
        assert_eq!(twice.errors.len(), 1);
        assert_eq!(twice.file_side, Some((0, Some(Side::Client))));
    }

    /// The DataModel place a file lands at names its side when nothing
    /// else does.
    #[test]
    fn the_mount_names_the_side_of_a_file_with_no_suffix() {
        let server = ["ServerScriptService".to_string(), "Game".to_string()];
        let client = [
            "StarterPlayer".to_string(),
            "StarterPlayerScripts".to_string(),
        ];
        let shared = ["ReplicatedStorage".to_string(), "Shared".to_string()];
        let first = ["ReplicatedFirst".to_string()];
        assert_eq!(mount_side(&server), Some(Side::Server));
        assert_eq!(mount_side(&client), Some(Side::Client));
        assert_eq!(mount_side(&shared), None);
        assert_eq!(mount_side(&first), None);
        assert_eq!(mount_side(&["StarterGui".to_string()]), Some(Side::Client));
        assert_eq!(mount_side(&["StarterPack".to_string()]), Some(Side::Client));
        assert_eq!(
            mount_side(&["ServerStorage".to_string()]),
            Some(Side::Server)
        );
        assert_eq!(mount_side(&["Workspace".to_string()]), None);

        // The suffix beats the place, and the file-wide directive sits
        // between them.
        assert_eq!(
            side_of("local a = 1\n", "ui.client.aly", None, Some(&server)),
            Some(Side::Client)
        );
        assert_eq!(
            side_of("--@alloy-file-side client\n", "x.aly", None, Some(&server)),
            Some(Side::Client)
        );
        assert_eq!(
            side_of("local a = 1\n", "x.aly", None, Some(&server)),
            Some(Side::Server)
        );
        // `[contexts]` sits under the directive and over the place.
        assert_eq!(
            side_of(
                "local a = 1\n",
                "x.aly",
                Some(Some(Side::Client)),
                Some(&server)
            ),
            Some(Side::Client)
        );
        assert_eq!(side_of("local a = 1\n", "x.aly", None, Some(&shared)), None);
    }

    // --- 5. the preserved line -------------------------------------------------

    #[test]
    fn preserve_keeps_a_line_off_the_rewrite() {
        let above = scan("--@alloy-preserve\nlocal a = b and b.c\nlocal d = e and e.f\n");
        assert!(above.preserves(1));
        assert!(!above.preserves(2));
        // It silences nothing.
        assert!(above.allows(1));

        let tail = scan("local a = b and b.c --@alloy-preserve\n");
        assert!(tail.preserves(0));
        assert!(tail.allows(0));
    }

    #[test]
    fn a_file_with_no_directive_is_empty() {
        assert!(scan("local a = 1\n").is_empty());
        assert!(!scan("--@alloy-preserve\nlocal a = 1\n").is_empty());
        assert!(!scan("--@alloy-lint raw_require=allow\n").is_empty());
        assert!(!scan("--@alloy-side client\n").is_empty());
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
