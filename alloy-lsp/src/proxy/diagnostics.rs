//! Diagnostics: the child's reports, reworded and filtered, and the ones alloy compiles and lints itself.

use super::completion::strip_std_prefix;
use super::documents::project_aliases;
use super::hover::{byte_column, impl_width, method_owner, utf16_column, without_self, word_width};
use super::*;

impl State {
    /// The compiler's diagnostics of one document as LSP diagnostics.
    pub(crate) fn alloy_diagnostics(&self, uri: &str) -> Vec<Value> {
        let mut diagnostics = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return diagnostics;
        };

        // A compile that stopped leaves no output. Its one error is all
        // the file can say; the child reads Alloy source and reports
        // every line of it, so those reports are dropped.
        if let Some(e) = &doc.error {
            let (line, character) = position_of(&doc.source, e.offset.min(doc.source.len()));
            let end = doc
                .source
                .lines()
                .nth(line as usize)
                .map(|l| l.trim_end().encode_utf16().count() as u32)
                .unwrap_or(character + 1)
                .max(character + 1);
            diagnostics.push(json!({
                "range": {
                    "start": { "line": line, "character": character },
                    "end": { "line": line, "character": end },
                },
                "severity": 1,
                "source": "Alloy",
                "message": alloy::docs::labeled(&e.message),
            }));

            return diagnostics;
        }

        if let Some(out) = &doc.output {
            for d in &out.diagnostics {
                let (sl, sc) = position_of(&doc.source, d.start as usize);
                let (el, ec) = position_of(&doc.source, d.end.max(d.start) as usize);
                let (el, ec) = if (el, ec) == (sl, sc) {
                    (sl, sc + 1)
                } else {
                    (el, ec)
                };
                // `Alloy(4.2)`: the book section as the code, and the
                // number links to the section.
                let mut item = json!({
                    "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                    "severity": 1,
                    "source": "Alloy",
                    "message": alloy::docs::labeled(&d.message),
                });

                if let Some(code) = alloy::docs::code_for(&d.message)
                    && let Some(url) = alloy::docs::book_url(code)
                {
                    item["code"] = json!(code);
                    item["codeDescription"] = json!({ "href": url });
                }

                diagnostics.push(item);
            }

            // Lints at their `[lint]` level: a warning, or an error for
            // a denied one. The table comes from the project's alloy.toml.
            let lint_config = self.lint_config();
            let directives = alloy::directives::scan(&doc.source);

            for l in &out.lints {
                // A `--@alloy-lint` in the file wins over the table.
                let level = alloy::lint::level_in(&lint_config, &directives, l.name);

                if level == alloy::lint::Level::Allow {
                    continue;
                }

                let (sl, sc) = position_of(&doc.source, l.start as usize);
                let (el, ec) = position_of(&doc.source, l.end.max(l.start) as usize);
                let (el, ec) = if (el, ec) == (sl, sc) {
                    (sl, sc + 1)
                } else {
                    (el, ec)
                };
                let severity = if level == alloy::lint::Level::Deny {
                    1
                } else {
                    2
                };
                diagnostics.push(json!({
                    "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                    "severity": severity,
                    "source": "Alloy",
                    "code": alloy::docs::LINT_CODE,
                    "codeDescription": { "href": alloy::docs::book_url(alloy::docs::LINT_CODE).unwrap_or_default() },
                    "message": match &l.fix {
                        Some(f) if directives.preserves(alloy::directives::line_of(&doc.source, f.start as usize)) => {
                            format!("{}: {}\n`--@alloy-preserve` keeps this line, so `--fix` writes no rewrite here.", l.name, l.message)
                        }

                        Some(_) => format!("{}: {}\n`alloy flux --fix` rewrites it.", l.name, l.message),

                        None => format!("{}: {}", l.name, l.message),
                    },
                }));
            }
        }

        // A module that names no file, a name the module does not
        // export, and a name imported twice. The build reports these,
        // and the checker has no words for the last two, so the editor
        // reads them here.
        if doc.error.is_none()
            && let Some(path) = uri_to_path(uri)
        {
            let silence = alloy::directives::scan(&doc.source);

            let rel = PathBuf::from(self.friendly_path(&path));

            for problem in alloy::modules::import_problems_for_file(&path, Some(&rel), &doc.source)
            {
                if !silence.allows_named(
                    alloy::directives::line_of(&doc.source, problem.start as usize),
                    Some(problem.kind),
                ) {
                    continue;
                }

                let (sl, sc) = position_of(&doc.source, problem.start as usize);
                let (el, ec) = position_of(&doc.source, problem.end.max(problem.start) as usize);
                let mut item = json!({
                    "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                    "severity": 1,
                    "source": "Alloy",
                    "message": format!("{}: {}", problem.kind, problem.message),
                });

                if let Some(url) = alloy::docs::book_url("3.2") {
                    item["code"] = json!("3.2");
                    item["codeDescription"] = json!({ "href": url });
                }

                diagnostics.push(item);
            }
        }

        diagnostics
    }

    /// The quick fixes for an `impl` or a `trait` header without `as`:
    /// one per header in the range, and one that writes every header of
    /// the file. A file the parser reported on carries no lint, so this
    /// rewrite rides on the diagnostic, the way `alloy flux --fix` does.
    pub(crate) fn header_as_actions(
        &self,
        uri: &str,
        range: ((u32, u32), (u32, u32)),
    ) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let fixes = alloy::fmt::header_as_fixes(&doc.source);

        if fixes.is_empty() {
            return actions;
        }

        let ((from_line, _), (to_line, _)) = range;
        let mut all: Vec<Value> = Vec::new();

        for f in &fixes {
            let (line, at) = position_of(&doc.source, f.start as usize);
            let edit = json!({
                "range": {
                    "start": { "line": line, "character": at },
                    "end": { "line": line, "character": at },
                },
                "newText": f.replacement,
            });
            all.push(edit.clone());

            if line < from_line || line > to_line {
                continue;
            }

            let head = doc
                .output
                .as_ref()
                .map(|o| o.diagnostics.as_slice())
                .unwrap_or_default()
                .iter()
                .find(|d| {
                    d.message.ends_with(alloy::fmt::NEEDS_AS)
                        && position_of(&doc.source, d.start as usize).0 == line
                });
            let mut action = json!({
                "title": "Write `as` after the header",
                "kind": "quickfix",
                "isPreferred": true,
                "edit": { "changes": { uri: [edit] } },
            });

            if let Some(d) = head {
                let (sl, sc) = position_of(&doc.source, d.start as usize);
                action["diagnostics"] = json!([{
                    "range": {
                        "start": { "line": sl, "character": sc },
                        "end": { "line": line, "character": at },
                    },
                    "severity": 1,
                    "source": "Alloy",
                    "message": alloy::docs::labeled(&d.message),
                }]);
            }

            actions.push(action);
        }

        if all.len() > 1 {
            actions.push(json!({
                "title": format!("Write `as` after every header in this file ({})", all.len()),
                "kind": "source.fixAll",
                "edit": { "changes": { uri: all } },
            }));
        }

        actions
    }

    /// The code actions of the lints: a quick fix per rewrite whose lint
    /// touches the range, each tied to its diagnostic so the editor's
    /// light bulb finds it, and `source.fixAll` for the whole file.
    pub(crate) fn lint_actions(&self, uri: &str, range: ((u32, u32), (u32, u32))) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let Some(out) = &doc.output else {
            return actions;
        };
        let lint_config = self.lint_config();
        let directives = alloy::directives::scan(&doc.source);
        let ((from_line, _), (to_line, _)) = range;
        let mut all_edits: Vec<Value> = Vec::new();

        for l in &out.lints {
            let Some(fix) = &l.fix else { continue };

            if alloy::lint::level_in(&lint_config, &directives, l.name) == alloy::lint::Level::Allow
            {
                continue;
            }

            // `--@alloy-preserve` keeps the line as the author wrote it,
            // in the editor as under `alloy flux --fix`.
            if directives.preserves(alloy::directives::line_of(&doc.source, fix.start as usize)) {
                continue;
            }

            let (sl, sc) = position_of(&doc.source, fix.start as usize);
            let (el, ec) = position_of(&doc.source, fix.end as usize);
            let edit = json!({
                "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                "newText": fix.replacement,
            });
            all_edits.push(edit.clone());

            let (ll, lc) = position_of(&doc.source, l.start as usize);
            let (le, lec) = position_of(&doc.source, l.end.max(l.start) as usize);

            if le < from_line || ll > to_line {
                continue;
            }

            let one_line: String = fix
                .replacement
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let shown = if one_line.chars().count() > 40 {
                format!("{}…", one_line.chars().take(40).collect::<String>())
            } else {
                one_line
            };
            actions.push(json!({
                "title": format!("Rewrite as `{shown}` ({})", l.name),
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": { "start": { "line": ll, "character": lc }, "end": { "line": le, "character": lec } },
                    "severity": 2,
                    "source": "Alloy",
                    "code": alloy::docs::LINT_CODE,
                    "message": format!("{}: {}\n`alloy flux --fix` rewrites it.", l.name, l.message),
                }],
                "edit": { "changes": { uri: [edit] } },
            }));
        }

        if all_edits.len() > 1 {
            // Two rewrites that overlap keep the first, as `--fix` does.
            let mut kept: Vec<Value> = Vec::new();
            let mut last_end: Option<(u64, u64)> = None;

            for e in &all_edits {
                let start = (
                    e["range"]["start"]["line"].as_u64().unwrap_or(0),
                    e["range"]["start"]["character"].as_u64().unwrap_or(0),
                );
                let end = (
                    e["range"]["end"]["line"].as_u64().unwrap_or(0),
                    e["range"]["end"]["character"].as_u64().unwrap_or(0),
                );

                if last_end.is_none_or(|l| l <= start) {
                    kept.push(e.clone());
                    last_end = Some(end);
                }
            }

            actions.push(json!({
                "title": format!("Apply every Alloy rewrite in this file ({})", kept.len()),
                "kind": "source.fixAll",
                "edit": { "changes": { uri: kept } },
            }));
        }

        actions
    }
}

impl Server {
    /// Publishes the Alloy diagnostics and the mapped child diagnostics
    /// of one source document.
    pub(crate) fn publish(&self, uri: &str) {
        let st = self.state.lock().expect("state");

        if !st.docs.contains_key(uri) {
            return;
        }

        let mut diagnostics: Vec<Value> = st.alloy_diagnostics(uri);

        if let Some(mapped) = st.child_diagnostics.get(uri) {
            diagnostics.extend(mapped.iter().cloned());
        }

        collapse_diagnostics(&mut diagnostics);

        if let Some(doc) = st.docs.get(uri) {
            snap_ranges(&mut diagnostics, &doc.source);
        }

        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics }
        });
        drop(st);
        self.to_client(&message);
    }
}

/// The `--@alloy-expect-error` directives that cover a line nothing
/// reported on, each as a diagnostic on the directive. `child` holds the
/// checker's reports before the filter, in shadow lines, which the
/// source shares; the compiler's own hits come with the output.
pub(crate) fn unmet_expectations(doc: &Doc, child: &[Value]) -> Vec<Value> {
    let silence = alloy::directives::scan(&doc.source);

    if silence.is_empty() {
        return Vec::new();
    }

    let mut errored: HashSet<usize> = doc
        .output
        .as_ref()
        .map(|o| o.expected_hits.iter().copied().collect())
        .unwrap_or_default();

    for d in child {
        if let Some(((sl, _), _)) = d.get("range").and_then(range_of)
            && d.get("severity").and_then(Value::as_u64).unwrap_or(1) <= 2
        {
            errored.insert(sl as usize);
        }
    }

    silence
        .unmet(&errored)
        .into_iter()
        .map(|(at, _col, reason)| {
            let (s, e) = alloy::directives::span_of_line(&doc.source, at);
            let (sl, sc) = position_of(&doc.source, s);
            let (el, ec) = position_of(&doc.source, e);
            // The reason names which directive went stale, so a file
            // with several says which one to look at.
            let message = alloy::directives::unmet_message(reason.as_deref());
            let mut item = json!({
                "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                "severity": 1,
                "source": "Alloy",
                "message": alloy::docs::labeled(&message),
            });

            if let Some(code) = alloy::docs::code_for(&message)
                && let Some(url) = alloy::docs::book_url(code)
            {
                item["code"] = json!(code);
                item["codeDescription"] = json!({ "href": url });
            }

            item
        })
        .collect()
}

/// The columns of the first quoted string on a line, quotes included.
pub(crate) fn quoted_span_on_line(source: &str, line: u32) -> Option<(u32, u32)> {
    let text = source.lines().nth(line as usize)?;
    let open = text.find(['"', '\''])?;
    let quote = text.as_bytes()[open] as char;
    let close = text[open + 1..].find(quote)? + open + 1;
    let col = |byte: usize| text[..byte].encode_utf16().count() as u32;

    Some((col(open), col(close + 1)))
}

/// A diagnostic points at a token the reader can see. A range that
/// lands on whitespace moves to the next token of its line.
pub(crate) fn snap_ranges(items: &mut [Value], source: &str) {
    let space = |u: u16| u == 0x20 || u == 0x09;

    for d in items.iter_mut() {
        let Some(((sl, sc), (el, ec))) = d.get("range").and_then(range_of) else {
            continue;
        };

        if sl != el {
            continue;
        }

        let Some(line) = source.lines().nth(sl as usize) else {
            continue;
        };
        let units: Vec<u16> = line.encode_utf16().collect();
        let start = (sc as usize).min(units.len());
        let end = (ec as usize).min(units.len());

        if units[start..end].iter().any(|u| !space(*u)) {
            continue;
        }

        let Some(from) = (start..units.len()).find(|k| !space(units[*k])) else {
            continue;
        };
        // A name ends where the name ends; anything else ends at the
        // next space.
        let name =
            |u: u16| char::from_u32(u as u32).is_some_and(|c| c.is_alphanumeric() || c == '_');
        let to = match name(units[from]) {
            true => (from..units.len())
                .find(|k| !name(units[*k]))
                .unwrap_or(units.len()),

            false => (from..units.len())
                .find(|k| space(units[*k]))
                .unwrap_or(units.len()),
        };

        d["range"] = range_value((sl, from as u32), (sl, to as u32));
    }
}

type Span = ((u32, u32), (u32, u32));

/// One report per problem: identical messages at one range collapse to
/// one, and a message the checker repeats over nested ranges keeps the
/// innermost, which is the one that points at the mistake.
pub(crate) fn collapse_diagnostics(items: &mut Vec<Value>) {
    let mut seen: Vec<(Value, String)> = Vec::new();

    items.retain(|d| {
        let key = (
            d.get("range").cloned().unwrap_or(Value::Null),
            d.get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );

        match seen.contains(&key) {
            true => false,

            false => {
                seen.push(key);

                true
            }
        }
    });

    let spans: Vec<(String, Span)> = items
        .iter()
        .filter_map(|d| {
            Some((
                d.get("message").and_then(Value::as_str)?.to_string(),
                d.get("range").and_then(range_of)?,
            ))
        })
        .collect();

    items.retain(|d| {
        let Some(message) = d.get("message").and_then(Value::as_str) else {
            return true;
        };
        let Some(range) = d.get("range").and_then(range_of) else {
            return true;
        };

        !spans
            .iter()
            .any(|(other, span)| other == message && *span != range && covers(range, *span))
    });

    // A nil base makes every key on it unknown. `could be nil` names
    // the problem; the key report sends the reader after a typo that is
    // not there.
    let nil_lines: Vec<u32> = items
        .iter()
        .filter(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("could be nil"))
        })
        .filter_map(|d| Some(d.get("range").and_then(range_of)?.0.0))
        .collect();

    items.retain(|d| {
        let key = d
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains(": Key '"));

        !key || d
            .get("range")
            .and_then(range_of)
            .is_none_or(|r| !nil_lines.contains(&r.0.0))
    });

    // A `.` where a `:` belongs shifts every argument, so the checker
    // reports the arity and then each mismatch that follows. The one
    // sentence that names the mistake stands alone on its line.
    let typo_lines: Vec<u32> = items
        .iter()
        .filter(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains(alloy::typecheck::DOT_FOR_COLON))
        })
        .filter_map(|d| Some(d.get("range").and_then(range_of)?.0.0))
        .collect();

    items.retain(|d| {
        d.get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains(alloy::typecheck::DOT_FOR_COLON))
            || d.get("range")
                .and_then(range_of)
                .is_none_or(|r| !typo_lines.contains(&r.0.0))
    });
}

/// Whether the range holds the other one.
pub(crate) fn covers(outer: Span, inner: Span) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// Whether a shadow position sits in the runtime require the emit
/// writes at the head of a file, `local __alloy = require(...)`.
pub(crate) fn in_runtime_require(shadow: &str, line: u32, character: u32) -> bool {
    let Some(text) = shadow.lines().nth(line as usize) else {
        return false;
    };
    let Some((start, end)) = alloy::typecheck::runtime_require_span(text) else {
        return false;
    };
    let column = |at: usize| text[..at].encode_utf16().count() as u32;

    (column(start)..=column(end)).contains(&character)
}

/// Drops a child diagnostic that reports the emit rather than the source:
/// a layout lint about hoisted statements, any warning whose range
/// touches generated text, or an unused-variable lint for a name that an
/// intrinsic such as `$nameof` consumed. Errors in generated text stay;
/// they map to the construct that produced them.
pub(crate) fn keep_diagnostic(
    d: &Value,
    doc: &Doc,
    doc_path: Option<&Path>,
    lint_config: &alloy::config::LintConfig,
) -> bool {
    let message = d.get("message").and_then(Value::as_str).unwrap_or_default();

    // Alloy owns the unused-name lints, in the words of what the source
    // wrote; the checker's copy says the same thing twice.
    if let Some(kind) = message.split_once(": ").map(|(k, _)| k)
        && alloy::typecheck::owned_lint(kind)
    {
        return false;
    }

    // A half-typed member access leaves the checker with no name, and
    // it reports its own stand-in. The parser already names the gap.
    if crate::shapes::names_only_the_emit(message) {
        return false;
    }

    // The enum emit writes `tag` and `_1`; a report about one of those,
    // on a line the source never wrote them on, describes the emit.
    if d.pointer("/range/start/line")
        .and_then(Value::as_u64)
        .and_then(|line| doc.source.lines().nth(line as usize))
        .is_some_and(|text| {
            crate::shapes::names_the_emit_key(message, text)
                || crate::shapes::duplicate_only_in_the_emit(message, text)
        })
    {
        return false;
    }

    // The child reads the Alloy source when the compile stopped, and
    // reads none of it: every line draws a syntax error or an unknown
    // global. The compile error alone says what is wrong.
    if doc.output.is_none() {
        return false;
    }

    // `--@alloy-nocheck`, `--@alloy-ignore`, and an ignored region
    // silence the checker too. The shadow keeps the source's lines, so
    // the line is the same. The kind before the colon is the name an
    // `--@alloy-ignore-start` may carry.
    let silence = alloy::directives::scan(&doc.source);
    // `friendly_message` renames an `Unknown require` report to
    // `UnknownModule`; a region names what the author reads.
    let kind = if message.contains("Unknown require") {
        Some("UnknownModule")
    } else {
        message.split_once(": ").map(|(k, _)| k)
    };

    if !silence.is_empty()
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && !silence.allows_named(sl as usize, kind)
    {
        return false;
    }

    if message
        .to_ascii_lowercase()
        .starts_with("samelinestatement")
    {
        return false;
    }

    // `require(script.Parent)` resolves at runtime and names no path;
    // `raw_require` already says the checker cannot follow it.
    if message.contains("Unknown require")
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && alloy::typecheck::quoted_on_line(&doc.source, sl as usize).is_none()
    {
        return false;
    }

    // Alloy writes the runtime require at the head of the shadow; the
    // reader wrote no import of it, so a report about it names nothing
    // to fix. The range is still in shadow terms here.
    if message.contains("Unknown require")
        && let Some(((sl, sc), _)) = d.get("range").and_then(range_of)
        && in_runtime_require(&doc.shadow, sl, sc)
    {
        return false;
    }

    // Past its first error the parser invents the tree, and the emit
    // copies the text through: `trait Zap` reads to the checker as a
    // call of an unknown global, and the `end` the recovery never saw
    // is a syntax error of its own. The compiler already names the
    // parse error, and the lints are off for the same reason, so only
    // a type error away from the recovery still stands.
    if let Some(out) = &doc.output
        && !out.parsed_clean
        && (message.contains("Unknown global '")
            || message.starts_with("SyntaxError")
            || d.get("severity").and_then(Value::as_u64) != Some(1))
    {
        return false;
    }

    // A line the compiler already reports on has an unreliable emit,
    // and the checker's report there describes that emit, not the code:
    // `Unknown global 'new'` under `ReservedWord`, a syntax error under
    // a half-typed statement.
    if let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out
            .diagnostics
            .iter()
            .any(|a| alloy::directives::line_of(&doc.source, a.start as usize) == sl as usize)
    {
        return false;
    }

    // An import the build reports on reads in Alloy's words, which name
    // what the module exports; the checker's report on the same line is
    // the same problem told as a missing key.
    if let Some(path) = doc_path
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && alloy::modules::import_problems_for_file(path, None, &doc.source)
            .iter()
            .any(|p| alloy::directives::line_of(&doc.source, p.start as usize) == sl as usize)
    {
        return false;
    }

    // `private_access` already says the field is private; the checker
    // says at the same place that it does not exist.
    if let Some(field) = missing_key(message)
        && let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out.lints.iter().any(|l| {
            l.name == "private_access"
                && alloy::lint::level_in(lint_config, &silence, l.name) != alloy::lint::Level::Allow
                && alloy::directives::line_of(&doc.source, l.start as usize) == sl as usize
                && l.message.contains(&format!("`{field}`"))
        })
    {
        return false;
    }

    // The emit ends a match that has no `default` arm with a nil
    // fallthrough. The checker reports that nil; `ExhaustiveMatch`
    // already reports the arm that is missing.
    if let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out.diagnostics.iter().any(|a| {
            a.message.starts_with("this match is not exhaustive")
                && alloy::directives::line_of(&doc.source, a.start as usize) <= sl as usize
                && alloy::directives::line_of(&doc.source, a.end as usize) >= sl as usize
        })
    {
        return false;
    }

    let is_error = d.get("severity").and_then(Value::as_u64).unwrap_or(1) == 1;

    if is_error {
        return true;
    }

    if let Some(name) = unused_name(message)
        && consumed_by_intrinsic(&doc.source, name)
    {
        return false;
    }

    // Alloy's own lint says it on the same line, in the words of what
    // the source wrote; the checker's copy would say it twice.
    if let Some(names) = message
        .split_once(": ")
        .and_then(|(kind, _)| alloy::typecheck::paired_lint(kind))
        && let Some(out) = &doc.output
        && let Some(((sl, _), _)) = d.get("range").and_then(range_of)
        && out.lints.iter().any(|l| {
            names.contains(&l.name)
                && alloy::lint::level_in(lint_config, &silence, l.name) != alloy::lint::Level::Allow
                && alloy::directives::line_of(&doc.source, l.start as usize) == sl as usize
        })
    {
        return false;
    }

    let Some(((sl, sc), (el, ec))) = d.get("range").and_then(range_of) else {
        return true;
    };

    let Some(start) = offset_of(&doc.shadow, sl, sc) else {
        return true;
    };
    let end = offset_of(&doc.shadow, el, ec).unwrap_or(doc.shadow.len());

    !(start..end.max(start + 1)).any(|o| doc.generated_offset(o))
}

/// Puts a report over the whole import statement, from its first word
/// to the end of its quoted path.
pub(crate) fn statement_range(d: &mut Value, doc: &Doc, line: usize) {
    let Some(text) = doc.source.lines().nth(line) else {
        return;
    };
    let start = text.len() - text.trim_start().len();
    let start = text[..start].encode_utf16().count() as u32;
    let end = quoted_span_on_line(&doc.source, line as u32)
        .map(|(_, e)| e)
        .unwrap_or_else(|| text.encode_utf16().count() as u32);
    d["range"] = json!({
        "start": { "line": line, "character": start },
        "end": { "line": line, "character": end },
    });
}

/// A child message as the editor should read it: a mirror path reads as
/// the real one, and an unresolved require is an `UnknownModule` error
/// over the whole import, naming the module the source asked for.
pub(crate) fn friendly_message(d: &mut Value, doc: &Doc, st: &State) {
    // The child's own words, before the folding below rewrites them: an
    // `Unknown require` names the file it looked for, and that path
    // says which require of the line the report is about.
    let raw = d
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let here = st
        .docs
        .iter()
        .find(|(_, other)| std::ptr::eq(*other, doc))
        .map(|(uri, _)| uri.clone());
    // A type inside a message reads as a hover does: the runtime's
    // names go, and a struct's private view folds to the struct.
    let known = st.known_shapes_at(here.as_deref());

    if let Some(message) = d.get("message").and_then(Value::as_str) {
        let mut folded = json!(message);
        strip_std_prefix(&mut folded);
        let text = crate::shapes::fold(folded.as_str().unwrap_or(message), &known);
        d["message"] = json!(crate::shapes::friendly_text(&text));
    }

    alloy_wording(d, doc, &known.shapes);

    let Some(message) = d.get("message").and_then(Value::as_str) else {
        return;
    };

    // The child writes `TypeError: Cannot require module <path>:
    // Module does not return exactly 1 value.`
    if message.contains(alloy::typecheck::NO_MODULE_RETURN) {
        let line = d
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;

        if let Some(spec) = alloy::typecheck::required_spec(&raw, &doc.source, line) {
            d["message"] = json!(format!(
                "UnknownModule: {}",
                alloy::typecheck::no_module_return_message(&spec)
            ));
            d["severity"] = json!(1);
            d["source"] = json!("Alloy");
            d["code"] = json!("3.2");
            statement_range(d, doc, line);

            if let Some(url) = alloy::docs::book_url("3.2") {
                d["codeDescription"] = json!({ "href": url });
            }
        }

        return;
    }

    // The child writes `TypeError: Unknown require: <path>`.
    if message.contains("Unknown require") {
        let line = d
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let spec = alloy::typecheck::required_spec(&raw, &doc.source, line).unwrap_or_default();
        let doc_path = st
            .docs
            .iter()
            .find(|(_, other)| std::ptr::eq(*other, doc))
            .and_then(|(uri, _)| uri_to_path(uri));
        let source_rel = doc_path
            .as_deref()
            .map(|p| st.friendly_path(p))
            .unwrap_or_default();
        let named = doc_path
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| project_aliases(dir, st.root.as_deref()))
            .and_then(|aliases| alloy::modules::alias_target(&spec, &aliases, st.root.as_deref()));
        d["message"] = json!(format!(
            "UnknownModule: {}",
            alloy::typecheck::unknown_module_message(
                &spec,
                Path::new(&source_rel),
                named.as_deref()
            )
        ));
        d["severity"] = json!(1);
        d["source"] = json!("Alloy");
        d["code"] = json!("3.2");

        statement_range(d, doc, line);

        if let Some(url) = alloy::docs::book_url("3.2") {
            d["codeDescription"] = json!({ "href": url });
        }

        return;
    }

    let mirror = st.mirror.to_string_lossy().into_owned();

    if message.contains(&mirror) {
        let outside = format!("{mirror}/_outside");
        let root = st
            .root
            .as_deref()
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rewritten = message.replace(&outside, "").replace(&mirror, &root);
        d["message"] = json!(rewritten);
    }
}

/// The messages that describe the emit, in the source's words: `new` on
/// a type alias or on a value, `is` against a name no type has, an
/// `impl` for an alias, a method called with a dot, and the arity of a
/// method call, which the source writes without `self`.
pub(crate) fn alloy_wording(d: &mut Value, doc: &Doc, shapes: &[alloy::declarations::Shape]) {
    let Some(message) = d.get("message").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(((sl, sc), (_, ec))) = d.get("range").and_then(range_of) else {
        return;
    };
    let Some(line) = doc.source.lines().nth(sl as usize) else {
        return;
    };
    let (kind, body) = match message.split_once(": ") {
        Some((k, rest)) if !k.contains(' ') => (k.to_string(), rest.to_string()),

        _ => ("TypeError".to_string(), message.clone()),
    };
    let span: String = line
        .chars()
        .skip(sc as usize)
        .take(ec.saturating_sub(sc) as usize)
        .collect();

    // `new Plain { }` on a type alias, `x is Alias`, `impl T for Alias`,
    // and `new n { }` on a value each emit a name the artifact never
    // binds. The compiler writes these sentences, so the terminal and
    // the editor say one thing.
    if let Some((better, at)) =
        alloy::typecheck::rewrite_emitted_name(&message, &doc.source, sl as usize + 1)
    {
        d["message"] = json!(format!("{kind}: {better}"));

        // Every method body of an `impl` reports the same name; the
        // `impl` line is where the mistake is.
        if let Some(at) = at {
            let at = at as u32 - 1;
            d["range"] = range_value((at, 0), (at, impl_width(doc, at)));
        }

        return;
    }

    // A field a struct does not have, a name declared twice, a type
    // that is a value, and an enum variant built with the wrong
    // payload: the compiler writes these sentences and puts them on the
    // token the reader wrote.
    if let Some(better) = alloy::typecheck::resite_report(
        &body,
        shapes,
        &doc.source,
        sl as usize + 1,
        byte_column(doc, sl, sc),
    ) {
        d["message"] = json!(format!("{}: {}", better.kind, better.message));

        if let Some(code) = alloy::typecheck::section_of(better.kind) {
            d["source"] = json!("Alloy");
            d["code"] = json!(code);

            if let Some(url) = alloy::docs::book_url(code) {
                d["codeDescription"] = json!({ "href": url });
            }
        }

        if let Some((line, col)) = better.at {
            let at = line as u32 - 1;
            let start = utf16_column(doc, at, col);
            let width = word_width(doc, at, start);
            d["range"] = range_value((at, start), (at, start + width));
        }

        return;
    }

    // `Damage.on(...)` where the side has no `on`: the table is the
    // remote's surface, and the source names the remote.
    if message.contains("not found in table 'Remote'")
        && let Some(key) = quoted_after(&message, "Key '")
        && let Some(name) = span.split('.').next().filter(|n| !n.is_empty())
    {
        d["message"] = json!(format!("{kind}: remote `{name}` has no `{key}`"));

        return;
    }

    // A `{ ... }` where an Array belongs: the checker answers with the
    // nineteen methods the table lacks, and the mistake is the bracket.
    if let Some(hint) = crate::shapes::plain_table_hint(&body) {
        d["message"] = json!(format!("{kind}: {hint}"));

        return;
    }

    // A `.` where a `:` belongs, and the arity of a method call the
    // source writes without `self`. The compiler writes both sentences,
    // so the terminal and the editor say one thing.
    if let Some(better) = alloy::typecheck::rewrite_dot_call(&body, line, sc as usize + 1) {
        d["message"] = json!(format!("{kind}: {better}"));

        return;
    }

    if !message.contains("Function expects ") {
        return;
    }

    // `c.bump()` on a method the checker gave a wider arity: the
    // receiver still has to go through the colon. The sentence is the
    // compiler's, so both tools read alike.
    if let Some((receiver, tail)) = span.rsplit_once('.')
        && !span.contains(':')
        && method_owner(&doc.source, tail).is_some()
    {
        let receiver = receiver
            .rsplit(['.', ' ', '(', ','])
            .next()
            .unwrap_or(receiver);
        d["message"] = json!(format!(
            "{kind}: `{tail}` is a method; call it with `{receiver}:{tail}(...)`, not `{receiver}.{tail}(...)`"
        ));

        return;
    }

    // `xs:len(1, 2)` passes the receiver through the colon, so the
    // counts the checker gives are one more than the source shows.
    if span.contains(':')
        && !span.contains('.')
        && let Some(rewritten) = without_self(&message)
    {
        d["message"] = json!(rewritten);
    }
}

/// The text between `opener` and the next quote.
pub(crate) fn quoted_after<'a>(message: &'a str, opener: &str) -> Option<&'a str> {
    let at = message.find(opener)? + opener.len();

    message[at..].find('\'').map(|end| &message[at..at + end])
}

/// Whether the line holds the phrase as whole words.
pub(crate) fn names_word(line: &str, phrase: &str) -> bool {
    line.match_indices(phrase).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + phrase.len()..].chars().next();

        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The key a `does not have key 'balance'` message names.
pub(crate) fn missing_key(message: &str) -> Option<&str> {
    let at = message.find("does not have key '")? + "does not have key '".len();

    message[at..].find('\'').map(|end| &message[at..at + end])
}

/// The variable of a `LocalUnused` or `FunctionUnused` lint.
pub(crate) fn unused_name(message: &str) -> Option<&str> {
    let rest = message
        .strip_prefix("LocalUnused: Variable '")
        .or_else(|| message.strip_prefix("FunctionUnused: Function '"))?;

    rest.split('\'').next()
}

/// True when `$nameof(` or `$stringify(` names the variable in its
/// argument. The emit turns that argument into a string, so the child
/// sees no use, while the source plainly has one.
pub(crate) fn consumed_by_intrinsic(source: &str, name: &str) -> bool {
    for sigil in ["$nameof(", "$stringify("] {
        let mut from = 0;

        while let Some(i) = source[from..].find(sigil) {
            let start = from + i + sigil.len();
            let argument = source[start..]
                .split_once(')')
                .map(|(a, _)| a)
                .unwrap_or(&source[start..]);

            if crate::keywords::find_word(argument, name).is_some() {
                return true;
            }

            from = start;
        }
    }

    false
}
