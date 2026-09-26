//! Diagnostics: the child's reports, reworded and filtered, and the ones alloy compiles and lints itself.

use std::collections::BTreeMap;

use super::completion::strip_std_prefix;
use super::documents::project_aliases;
use super::hover::{byte_column, impl_width, method_owner, utf16_column, without_self, word_width};
use super::*;

/// The head of the lexer's report about one character it cannot read.
/// The reader sees `unexpected character 'é'`, and the character is one
/// byte range of the source.
const BAD_CHARACTER: &str = "unexpected character ";

impl State {
    /// The compiler's diagnostics of one document as LSP diagnostics.
    /// Another open source that builds the same module as this one:
    /// `reg.aly` beside `reg.alx`. One name cannot mean two files, in
    /// the output or in a `require`.
    pub(crate) fn twin_module(&self, uri: &str) -> Option<String> {
        let path = uri_to_path(uri)?;
        let module = imports::module_path(&path);

        self.docs
            .keys()
            .filter(|u| u.as_str() != uri && is_alloy_uri(u))
            .find(|u| {
                uri_to_path(u).is_some_and(|p| p != path && imports::module_path(&p) == module)
            })
            .cloned()
    }

    pub(crate) fn alloy_diagnostics(&self, uri: &str) -> Vec<Value> {
        let mut diagnostics = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return diagnostics;
        };

        // Two sources whose names differ in the extension alone build
        // one module. The build overwrote one with the other, and here
        // the shadow of one takes the other's place, so every answer
        // about this file is about someone else's code.
        if let Some(twin) = self.twin_module(uri) {
            let name = |u: &str| u.rsplit('/').next().unwrap_or(u).to_string();
            let module = imports::module_path(Path::new(&name(uri)));
            // The first line carries it: the collision is the file's
            // name, which no range inside the text points at.
            let width = doc
                .source
                .lines()
                .next()
                .map(|l| l.trim_end().encode_utf16().count() as u32)
                .unwrap_or(0);
            diagnostics.push(json!({
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": width },
                },
                "severity": 1,
                "source": "Alloy",
                "message": format!(
                    "{} and {} both build {}.luau; rename one",
                    name(uri),
                    name(&twin),
                    module.display()
                ),
            }));
        }

        // A markup config that does not load: the build skips the file,
        // and the report sits on the key of the table, in alloy.toml.
        // The default backend stands in meanwhile, so its own stop
        // names a factory the project never chose.
        if self.markup_problem(uri).is_some() && doc.error.is_some() {
            return diagnostics;
        }

        // A compile that stopped leaves no output. Its one error is all
        // the file can say; the child reads Alloy source and reports
        // every line of it, so those reports are dropped.
        if let Some(e) = &doc.error {
            let at = e.offset.min(doc.source.len());
            let (line, character) = position_of(&doc.source, at);
            // The lexer points at one character, and the rest of the
            // line is code the reader wrote on purpose. The range is
            // that character alone, in the UTF-16 units it takes.
            let one_character = e
                .message
                .starts_with(BAD_CHARACTER)
                .then(|| doc.source[at..].chars().next())
                .flatten()
                .map(|c| character + c.len_utf16() as u32);
            let end = one_character.unwrap_or_else(|| {
                doc.source
                    .lines()
                    .nth(line as usize)
                    .map(|l| l.trim_end().encode_utf16().count() as u32)
                    .unwrap_or(character + 1)
                    .max(character + 1)
            });
            let mut item = json!({
                "range": {
                    "start": { "line": line, "character": character },
                    "end": { "line": line, "character": end },
                },
                "severity": 1,
                "source": "Alloy",
                "message": alloy::docs::labeled(&e.message),
            });

            // The stop of a compile links to its book section the way
            // a report of a finished compile does.
            if let Some(code) = alloy::docs::code_for(&e.message)
                && let Some(url) = alloy::docs::book_url(code)
            {
                item["code"] = json!(code);
                item["codeDescription"] = json!({ "href": url });
            }

            diagnostics.push(item);

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

                let code = alloy::docs::kind_section(problem.kind).map_or("3.2", |s| s.number);

                if let Some(url) = alloy::docs::book_url(code) {
                    item["code"] = json!(code);
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
    /*
    "Write the members `@provider` requires": one action per attribute
    whose contract the declaration under it does not meet.

    The compiler says which members are missing and where they go, so
    the action writes them in the order the clauses read and the result
    compiles.
    */
    pub(crate) fn contract_actions(
        &self,
        uri: &str,
        range: ((u32, u32), (u32, u32)),
    ) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let gaps = doc
            .output
            .as_ref()
            .map(|o| o.contract_gaps.as_slice())
            .unwrap_or_default();

        if gaps.is_empty() {
            return actions;
        }

        let ((from_line, _), (to_line, _)) = range;
        // One action per attribute and insertion point: a contract that
        // asks for a field and a method writes each where it belongs.
        let mut seen: Vec<(&str, u32)> = Vec::new();
        let step = self.fmt_config(uri).indent_width;

        for gap in gaps {
            if seen.contains(&(gap.attr.as_str(), gap.insert_at)) {
                continue;
            }

            let (line, _) = position_of(&doc.source, gap.start as usize);

            if line < from_line || line > to_line {
                continue;
            }

            seen.push((&gap.attr, gap.insert_at));
            let mine: Vec<&alloy::desugar::ContractGap> = gaps
                .iter()
                .filter(|g| g.attr == gap.attr && g.insert_at == gap.insert_at)
                .collect();
            let text: String = mine.iter().map(|g| member_text(g, step)).collect();
            let (el, ec) = position_of(&doc.source, gap.insert_at as usize);
            let at = json!({ "line": el, "character": ec.saturating_sub(gap.indent) });
            let names: Vec<String> = mine.iter().map(|g| g.member.clone()).collect();
            actions.push(json!({
                "title": format!(
                    "Write the member{} `@{}` requires: {}",
                    if names.len() == 1 { "" } else { "s" },
                    gap.attr,
                    names.join(", ")
                ),
                "kind": "quickfix",
                "isPreferred": true,
                "edit": { "changes": { uri: [{
                    "range": { "start": at, "end": at },
                    "newText": text,
                }] } },
            }));
        }

        actions
    }

    /*
    The quick fixes for three of the compiler's own reports, and for
    the checker's report on a remote in the compiler's words.

    Each one says what the file is missing, so the edit writes it: the
    `new` a construction wants, the arms a `match` does not cover, the
    variant or the field a misspelling meant, and the verb a remote's
    member meant. The child reads the emit, where none of them is left
    to see.
    */
    pub(crate) fn compiler_actions(
        &self,
        uri: &str,
        range: ((u32, u32), (u32, u32)),
    ) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let ((from_line, _), (to_line, _)) = range;

        for d in doc
            .output
            .as_ref()
            .map(|o| o.diagnostics.as_slice())
            .unwrap_or_default()
        {
            let start = d.start as usize;
            let end = d.end.max(d.start) as usize;
            let (sl, sc) = position_of(&doc.source, start);
            let (el, ec) = position_of(&doc.source, end);

            if el < from_line || sl > to_line {
                continue;
            }

            let at = json!({
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec },
            });
            // The fields form with no `new`: the word goes in front of
            // the name the report points at.
            let fix = if d.message.starts_with("construct `") {
                Some((
                    "Add `new`".to_string(),
                    json!([{
                        "range": { "start": { "line": sl, "character": sc }, "end": { "line": sl, "character": sc } },
                        "newText": "new ",
                    }]),
                ))
            } else if let Some((wrote, simple)) = double_negation_fix(&d.message)
                && doc.source.get(start..end) == Some(wrote)
            {
                // `~~number` reads `number`: the report spans the type.
                Some((
                    format!("Write `{simple}`"),
                    json!([{ "range": at, "newText": simple }]),
                ))
            } else if d.message.starts_with("`!=` is not an operator")
                && doc.source.get(start..start + 2) == Some("!=")
            {
                // The report spans the `!`; the edit takes the `=` too.
                let (el, ec) = position_of(&doc.source, start + 2);

                Some((
                    "Write `~=`".to_string(),
                    json!([{
                        "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                        "newText": "~=",
                    }]),
                ))
            } else if d.message.starts_with("`as` is not a cast here")
                && doc.source.get(start..start + 2) == Some("as")
            {
                // The report sits on `as`; the type after it stands.
                let (el, ec) = position_of(&doc.source, start + 2);

                Some((
                    "Write `::`".to_string(),
                    json!([{
                        "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                        "newText": "::",
                    }]),
                ))
            } else if d.message.starts_with("Alloy has no `let`")
                && doc.source.get(start..start + 3) == Some("let")
            {
                // The report sits on `let`; the rest of the line stands.
                // When the report drops a `mut`, the edit takes it too.
                let after = &doc.source[start + 3..];
                let cut = match after.trim_start().strip_prefix("mut") {
                    Some(tail)
                        if tail.starts_with(char::is_whitespace)
                            && !d.message.contains("write `local mut") =>
                    {
                        after.len() - tail.len()
                    }

                    _ => 0,
                };
                let (el, ec) = position_of(&doc.source, start + 3 + cut);

                Some((
                    "Write `local`".to_string(),
                    json!([{
                        "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                        "newText": "local",
                    }]),
                ))
            } else if d.message.starts_with("Alloy has no `mut`")
                && doc.source.get(start..start + 3) == Some("mut")
            {
                // `local mut x`: the word and the space after it go.
                let after = &doc.source[start + 3..];
                let gap = after.len() - after.trim_start().len();
                let (el, ec) = position_of(&doc.source, start + 3 + gap);

                Some((
                    "Remove `mut`".to_string(),
                    json!([{
                        "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                        "newText": "",
                    }]),
                ))
            } else if d.message.starts_with("type arguments at a call take")
                && let Some(edits) = angle_call_edits(&doc.source, start)
            {
                Some(("Write `<<...>>`".to_string(), edits))
            } else if let Some(found) = self.missing_arm_fix(doc, &d.message, (start, end)) {
                Some(found)
            } else if let Some(name) = alloy::std_names::missing_name(&d.message) {
                // A std name with no import: the line the report names.
                let spec = alloy::std_names::spec_of(name).unwrap_or_default();
                let fixes = alloy::std_names::import_fixes(&doc.source, &[name]);

                Some((
                    format!("Import `{name}` from \"{spec}\""),
                    json!(super::completion::fix_edits(&doc.source, &fixes)),
                ))
            } else {
                nearest_variant_fix(&d.message).map(|name| {
                    (
                        format!("Rename to `{name}`"),
                        json!([{ "range": at, "newText": name }]),
                    )
                })
            };
            let Some((title, edits)) = fix else {
                continue;
            };

            actions.push(json!({
                "title": title,
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": at,
                    "severity": 1,
                    "source": "Alloy",
                    "message": alloy::docs::labeled(&d.message),
                }],
                "edit": { "changes": { uri: edits } },
            }));
        }

        // Two std names or more with no import: one action writes every
        // import, the way `alloy flux --fix` does.
        let missing: Vec<&str> = doc
            .output
            .as_ref()
            .map(|o| o.diagnostics.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|d| alloy::std_names::missing_name(&d.message))
            .collect();

        if missing.len() > 1
            && actions.iter().any(|a| {
                a["title"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("Import `") && t.contains("@alloy/std"))
            })
        {
            let fixes = alloy::std_names::import_fixes(&doc.source, &missing);
            actions.push(json!({
                "title": "Import every std name this file uses",
                "kind": "quickfix",
                "edit": { "changes": { uri: super::completion::fix_edits(&doc.source, &fixes) } },
            }));
        }

        // `Damage.fier(1, 2)`: the wording pass names the verb the
        // member meant. The report spans the whole call head, so the
        // edit finds the member on the line.
        for d in self.child_diagnostics.get(uri).into_iter().flatten() {
            let Some(message) = d.get("message").and_then(Value::as_str) else {
                continue;
            };
            let Some(((sl, sc), (el, _))) = d.get("range").and_then(range_of) else {
                continue;
            };

            if el < from_line || sl > to_line {
                continue;
            }

            let Some(text) = doc.source.lines().nth(sl as usize) else {
                continue;
            };
            let from = byte_column(doc, sl, sc) - 1;

            // `bag.add(3)` on a method: the `.` before the name becomes `:`.
            if let Some(wrote) = dot_call_fix(message)
                && let Some(i) = text.get(from..).and_then(|rest| rest.find(&wrote))
                && let Some(dot) = wrote.rfind('.')
            {
                let at = utf16_column(doc, sl, from + i + dot + 1);

                actions.push(json!({
                    "title": format!("Write `{}:{}`", &wrote[..dot], &wrote[dot + 1..]),
                    "kind": "quickfix",
                    "isPreferred": true,
                    "diagnostics": [d],
                    "edit": { "changes": { uri: [{
                        "range": {
                            "start": { "line": sl, "character": at },
                            "end": { "line": sl, "character": at + 1 },
                        },
                        "newText": ":",
                    }] } },
                }));

                continue;
            }

            let Some((wrote, name)) = remote_verb_fix(message) else {
                continue;
            };
            // A field report starts on the name, after its `.` or `:`.
            let head = from.saturating_sub(1);
            let Some(i) = text.get(head..).and_then(|rest| {
                rest.find(&format!(".{wrote}"))
                    .or_else(|| rest.find(&format!(":{wrote}")))
            }) else {
                continue;
            };
            let start = utf16_column(doc, sl, head + i + 2);
            let end = start + wrote.encode_utf16().count() as u32;

            actions.push(json!({
                "title": format!("Rename to `{name}`"),
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [d],
                "edit": { "changes": { uri: [{
                    "range": {
                        "start": { "line": sl, "character": start },
                        "end": { "line": sl, "character": end },
                    },
                    "newText": name,
                }] } },
            }));
        }

        actions
    }

    /*
    The arms a `match` has no case for, as one insert before its `end`.

    The report names the enum and every variant the arms leave out, and
    the range it points at is the whole statement, so its last three
    bytes are the `end` the arms go above. A variant with a payload
    takes one, written `_`. An arm of a match that gives a value needs
    a value, so its body there is `$todo()`, which fits any type.
    */
    fn missing_arm_fix(
        &self,
        doc: &Doc,
        message: &str,
        span: (usize, usize),
    ) -> Option<(String, Value)> {
        let (head, tail) = message.split_once(" has no arm for ")?;

        if !head.starts_with("this match is not exhaustive") {
            return None;
        }

        let owner = quoted_names(head).pop()?;
        let missing = quoted_names(tail.split(';').next().unwrap_or(tail));

        if missing.is_empty() {
            return None;
        }

        let (start, end) = span;
        // The `end` of the statement, on a line of its own: an arm
        // written in front of anything else would run into it.
        let close = end
            .checked_sub(3)
            .filter(|at| doc.source.get(*at..end) == Some("end"))?;
        let line_start = doc.source[..close].rfind('\n').map_or(0, |i| i + 1);
        let closing = &doc.source[line_start..close];

        if !closing.trim().is_empty() {
            return None;
        }

        // The arms of the match, where it already has one; a match with
        // none indents its arms one step under the `end`.
        let arm = doc.source[start..close]
            .lines()
            .find(|l| l.trim_start().starts_with("case "))
            .map(|l| l[..l.len() - l.trim_start().len()].to_string())
            .unwrap_or_else(|| format!("{closing}    "));
        let payloads = self.variant_payloads(&owner);
        // The report starts on `match`. After `return`, an `=`, an open
        // bracket, or a comma, the match gives a value.
        let before = doc.source[..start].trim_end();
        let body = match ["return", "=", "(", "[", "{", ","]
            .iter()
            .any(|w| before.ends_with(w))
        {
            true => "$todo()",

            false => "",
        };
        let mut text = String::new();

        for name in &missing {
            let takes = payloads
                .iter()
                .find(|(v, _)| v == name)
                .map(|(_, n)| *n)
                .unwrap_or(0);
            let holes = match takes {
                0 => String::new(),

                n => format!("({})", vec!["_"; n].join(", ")),
            };
            text.push_str(&format!(
                "{arm}case {owner}.{name}{holes} then\n{arm}    {body}\n"
            ));
        }

        let title = match missing.len() {
            1 => "Add the missing arm".to_string(),

            _ => "Add the missing arms".to_string(),
        };
        let (line, _) = position_of(&doc.source, line_start);

        Some((
            title,
            json!([{
                "range": {
                    "start": { "line": line, "character": 0 },
                    "end": { "line": line, "character": 0 },
                },
                "newText": text,
            }]),
        ))
    }

    /// Each variant of an enum the workspace declares, with the number
    /// of payload values it takes. The report names the enum the way
    /// the source writes it, so a namespace path reads by its last
    /// word.
    fn variant_payloads(&self, owner: &str) -> Vec<(String, usize)> {
        let bare = owner.rsplit('.').next().unwrap_or(owner);

        self.docs
            .values()
            .flat_map(|d| d.shapes.iter())
            .find_map(|s| match s {
                alloy::declarations::Shape::Enum { name, variants, .. } if name == bare => Some(
                    variants
                        .iter()
                        .map(|(v, payload)| (v.clone(), payload.len()))
                        .collect(),
                ),

                _ => None,
            })
            .unwrap_or_default()
    }

    /// The quick fix of the `global` report: the word becomes `export`.
    /// `global` left the language, and the declaration it sits on is an
    /// `export` with one word changed.
    pub(crate) fn global_actions(&self, uri: &str, range: ((u32, u32), (u32, u32))) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let ((from_line, _), (to_line, _)) = range;

        for d in doc
            .output
            .as_ref()
            .map(|o| o.diagnostics.as_slice())
            .unwrap_or_default()
        {
            if !d.message.starts_with("`global` is removed") {
                continue;
            }

            let (line, at) = position_of(&doc.source, d.start as usize);

            if line < from_line || line > to_line {
                continue;
            }

            let (el, ec) = position_of(&doc.source, d.end as usize);
            let where_it_is = json!({
                "start": { "line": line, "character": at },
                "end": { "line": el, "character": ec },
            });
            actions.push(json!({
                "title": "replace `global` with `export`",
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": where_it_is,
                    "severity": 1,
                    "source": "Alloy",
                    "message": alloy::docs::labeled(&d.message),
                }],
                "edit": { "changes": { uri: [{
                    "range": where_it_is,
                    "newText": "export",
                }] } },
            }));
        }

        actions
    }

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
            // The parser's report names the header. A rewrite with no
            // report on its line is the scan's mistake, not a fix.
            let Some(head) = doc
                .output
                .as_ref()
                .map(|o| o.diagnostics.as_slice())
                .unwrap_or_default()
                .iter()
                .find(|d| {
                    d.message.ends_with(alloy::fmt::NEEDS_AS)
                        && position_of(&doc.source, d.start as usize).0 == line
                })
            else {
                continue;
            };
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

            let (sl, sc) = position_of(&doc.source, head.start as usize);
            actions.push(json!({
                "title": "Write `as` after the header",
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": {
                        "start": { "line": sl, "character": sc },
                        "end": { "line": line, "character": at },
                    },
                    "severity": 1,
                    "source": "Alloy",
                    "message": alloy::docs::labeled(&head.message),
                }],
                "edit": { "changes": { uri: [edit] } },
            }));
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
        let mut all_fixes: Vec<(&str, &alloy::lint::Fix)> = Vec::new();
        let edits_of = |fix: &alloy::lint::Fix| -> Vec<Value> {
            fix.edits()
                .map(|e| {
                    let (sl, sc) = position_of(&doc.source, e.start as usize);
                    let (el, ec) = position_of(&doc.source, e.end as usize);

                    json!({
                        "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                        "newText": e.replacement,
                    })
                })
                .collect()
        };

        for l in &out.lints {
            // The `<<...>>` form turns two comparisons into a call, so
            // the lint carries no rewrite for `--fix`. The editor offers
            // it here, as it does for the compiler's error.
            if l.name == "single_angle_call"
                && alloy::lint::level_in(&lint_config, &directives, l.name)
                    != alloy::lint::Level::Allow
                && let Some(edits) = angle_call_edits(&doc.source, l.start as usize)
            {
                let (ll, lc) = position_of(&doc.source, l.start as usize);
                let (le, lec) = position_of(&doc.source, l.end as usize);

                if le >= from_line && ll <= to_line {
                    actions.push(json!({
                        "title": "Write `<<...>>`",
                        "kind": "quickfix",
                        "isPreferred": true,
                        "diagnostics": [{
                            "range": { "start": { "line": ll, "character": lc }, "end": { "line": le, "character": lec } },
                            "severity": 2,
                            "source": "Alloy",
                            "code": alloy::docs::LINT_CODE,
                            "message": format!("{}: {}", l.name, l.message),
                        }],
                        "edit": { "changes": { uri: edits } },
                    }));
                }
            }

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

            all_fixes.push((l.name, fix));

            let (ll, lc) = position_of(&doc.source, l.start as usize);
            let (le, lec) = position_of(&doc.source, l.end.max(l.start) as usize);

            if le < from_line || ll > to_line {
                continue;
            }

            // A rewrite that breaks the parse is a lint's mistake, and
            // `--fix` refuses it too.
            if !alloy::lint::sound(&doc.source, vec![fix]).1.is_empty() {
                continue;
            }

            // A fix with no replacement deletes, so the title names
            // what goes away: `Rewrite as ``` says nothing.
            let (verb, text) = match fix.replacement.trim().is_empty() {
                true => (
                    "Remove",
                    doc.source
                        .get(fix.start as usize..fix.end as usize)
                        .unwrap_or_default(),
                ),

                false => ("Rewrite as", fix.replacement.as_str()),
            };
            let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let shown = if one_line.chars().count() > 40 {
                format!("{}…", one_line.chars().take(40).collect::<String>())
            } else {
                one_line
            };
            actions.push(json!({
                "title": format!("{verb} `{shown}` ({})", l.name),
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": { "start": { "line": ll, "character": lc }, "end": { "line": le, "character": lec } },
                    "severity": 2,
                    "source": "Alloy",
                    "code": alloy::docs::LINT_CODE,
                    "message": format!("{}: {}\n`alloy flux --fix` rewrites it.", l.name, l.message),
                }],
                "edit": { "changes": { uri: edits_of(fix) } },
            }));
        }

        if all_fixes.len() > 1 {
            // `prefer_const` goes first, as in `alloy fmt`: a local it
            // makes a `const` takes the const style, so the renames come
            // from the text with the `const`s in it.
            let consts: Vec<alloy::lint::Fix> = all_fixes
                .iter()
                .filter(|(name, _)| *name == "prefer_const")
                .map(|(_, f)| (*f).clone())
                .collect();
            let renames = match consts.is_empty() || uri.ends_with(".alx") {
                true => Vec::new(),

                false => {
                    alloy::naming::lints_after_consts(&doc.source, &consts, &lint_config.naming)
                }
            };
            let renamed: Vec<&alloy::lint::Fix> = renames
                .iter()
                .filter(|l| {
                    let line = alloy::directives::line_of(&doc.source, l.start as usize);

                    alloy::lint::level_in(&lint_config, &directives, l.name)
                        != alloy::lint::Level::Allow
                        && directives.allows_lint(line, l.name)
                        && !directives.preserves(line)
                })
                .filter_map(|l| l.fix.as_ref())
                .collect();
            let keep_names = renamed.is_empty();
            let fixes = all_fixes
                .iter()
                .filter(|(name, _)| keep_names || *name != alloy::naming::LINT)
                .map(|(_, f)| *f)
                .chain(renamed);
            // Two rewrites that overlap keep the first, as `--fix` does,
            // and a rename lands with every edit it makes.
            let chosen = alloy::lint::compatible(&doc.source, fixes);
            let (chosen, _) = alloy::lint::sound(&doc.source, chosen);
            let kept: Vec<Value> = chosen.iter().flat_map(|f| edits_of(f)).collect();

            actions.push(json!({
                "title": format!("Apply every Alloy rewrite in this file ({})", chosen.len()),
                "kind": "source.fixAll",
                "edit": { "changes": { uri: kept } },
            }));
        }

        actions
    }
}

impl State {
    /// Whether the editor already holds this set for the file, and
    /// remembers it when it does not. The editor keeps the set it has
    /// until the next one, so the same set again is no news: a pass
    /// over the workspace opens every file, the editor opens it again,
    /// and the child answers each of them.
    pub(crate) fn already_published(&mut self, uri: &str, diagnostics: &[Value]) -> bool {
        if self
            .published
            .get(uri)
            .is_some_and(|held| held == diagnostics)
        {
            return true;
        }

        self.published.insert(uri.to_string(), diagnostics.to_vec());

        false
    }
}

impl State {
    /// The list one file shows: the Alloy reports, then the child's
    /// mapped ones, collapsed and snapped. The push and the pull path
    /// both read it, so a pull-mode client sees every lint and compile
    /// error a push-mode client sees.
    pub(crate) fn full_diagnostics(&self, uri: &str, child: Vec<Value>) -> Vec<Value> {
        let mut diagnostics = self.alloy_diagnostics(uri);
        diagnostics.extend(child);

        // A config exports tables the schema documents, so the lint for
        // an export with no comment says nothing there.
        if crate::config_aly::is_config(uri) {
            diagnostics.retain(|d| {
                !d.get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|m| m.starts_with("missing_doc"))
            });
            diagnostics.extend(self.config_diagnostics(uri));
        }

        collapse_diagnostics(&mut diagnostics);

        if let Some(doc) = self.docs.get(uri) {
            snap_ranges(&mut diagnostics, &doc.source);
        }

        diagnostics
    }
}

impl Server {
    /// Publishes the Alloy diagnostics and the mapped child diagnostics
    /// of one source document.
    pub(crate) fn publish(&self, uri: &str) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());

        if !st.docs.contains_key(uri) {
            return;
        }

        let child = st.child_diagnostics.get(uri).cloned().unwrap_or_default();
        let diagnostics = st.full_diagnostics(uri, child);

        if st.already_published(uri, &diagnostics) {
            return;
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

/// The line and the span of the key that declares `alias` in a
/// configuration file: `(line, start, end)`. The three files spell a
/// key three ways, so the search takes the first line whose first word
/// is the name, in quotes or bare, with a `=` or a `:` after it.
pub(crate) fn alias_key_line(text: &str, alias: &str) -> Option<(u32, u32, u32)> {
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let quoted = trimmed.starts_with(&format!("\"{alias}\""));
        let bare = trimmed.starts_with(alias)
            && trimmed[alias.len()..].trim_start().starts_with(['=', ':'])
            && !quoted;

        if !quoted && !bare {
            continue;
        }

        let start = (line.len() - trimmed.len()) as u32;
        let width = if quoted {
            alias.len() as u32 + 2
        } else {
            alias.len() as u32
        };

        return Some((n as u32, start, start + width));
    }

    None
}

impl Server {
    /// The alias problems of the project, as diagnostics on the file
    /// that declares each one. The editor holds no document for
    /// `alloy.toml` or a Luau configuration, so the report goes to the
    /// file's own URI. Every pass republishes all three files, empty
    /// where nothing is wrong, so a renamed alias clears its report.
    pub(crate) fn publish_alias_problems(&self) {
        let root = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .root
            .clone();
        let Some(root) = root else {
            return;
        };
        let Some((path, config)) =
            Config::find_within(&root, &root).and_then(|p| Config::load(&p).ok().map(|c| (p, c)))
        else {
            return;
        };
        let base = path.parent().unwrap_or(&root).to_path_buf();
        let mut by_file: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
        by_file.insert(base.join(alloy::config::FILE_NAME), Vec::new());

        for name in alloy::luau_config::FILE_NAMES {
            let file = base.join(name);

            if file.is_file() {
                by_file.insert(file, Vec::new());
            }
        }

        // A markup table that does not load stops every `.alx` file of
        // the project. The mistake is in the table, so the report sits
        // on the key it names.
        let luaux = base.join("luaux.toml");

        if luaux.is_file() {
            by_file.insert(luaux, Vec::new());
        }

        if let Err(problem) = config.markup(&base) {
            let (file, at) = config.markup_problem_at(&base, &problem);
            let (line, col) = at.unwrap_or((0, 0));
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let written = text.lines().nth(line).unwrap_or_default().trim_end();
            let utf16 = |s: &str| s.encode_utf16().count() as u32;
            let message = format!("markup: {problem}");
            by_file.entry(file).or_default().push(json!({
                "range": {
                    "start": { "line": line, "character": utf16(&written[..col.min(written.len())]) },
                    "end": { "line": line, "character": utf16(written) },
                },
                "severity": 1,
                "source": "Alloy",
                "code": alloy::docs::code_for(&message),
                "message": alloy::docs::labeled(&message),
            }));
        }

        for problem in alloy::modules::alias_problems(&base, &config) {
            let text = std::fs::read_to_string(&problem.file).unwrap_or_default();
            let (line, start, end) = alias_key_line(&text, &problem.alias).unwrap_or((0, 0, 0));
            by_file
                .entry(problem.file.clone())
                .or_default()
                .push(json!({
                    "range": {
                        "start": { "line": line, "character": start },
                        "end": { "line": line, "character": end },
                    },
                    "severity": 1,
                    "source": "alloy",
                    "code": problem.code,
                    "message": problem.message,
                }));
        }

        for (file, diagnostics) in by_file {
            self.to_client(&json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": { "uri": path_to_uri(&file), "diagnostics": diagnostics },
            }));
        }
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

/// The first quoted string on a line, quotes included, as a start and
/// an end position. An import over several lines holds its path on its
/// last line, and the emit writes its `require` on the line of `import`,
/// so that line answers with the path of the statement.
pub(crate) fn quoted_span_on_line(source: &str, line: u32) -> Option<((u32, u32), (u32, u32))> {
    if let Some(s) = alloy_syntax::scan::import_statements(source)
        .into_iter()
        .find(|s| s.line == line as usize && source[s.start..s.end].contains('\n'))
    {
        let quote = source[..s.end].chars().next_back()?;
        let open = source[..s.end - 1].rfind(quote)?;

        return Some((position_of(source, open), position_of(source, s.end)));
    }

    let text = source.lines().nth(line as usize)?;
    let open = text.find(['"', '\''])?;
    let quote = text.as_bytes()[open] as char;
    let close = text[open + 1..].find(quote)? + open + 1;
    let col = |byte: usize| text[..byte].encode_utf16().count() as u32;

    Some(((line, col(open)), (line, col(close + 1))))
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

    // A name the module does not export draws an `ImportError` and an
    // `unused_import` on the same span, or on its alias: `Nope as N`
    // reports `Nope` and `N`. The name is wrong, and the second report
    // says nothing more.
    let missing: Vec<Span> = items
        .iter()
        .filter(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.starts_with("ImportError"))
        })
        .filter_map(|d| d.get("range").and_then(range_of))
        .collect();

    items.retain(|d| {
        !d.get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.starts_with("unused_import"))
            || d.get("range").and_then(range_of).is_none_or(|r| {
                !missing
                    .iter()
                    .any(|m| *m == r || (m.1.0 == r.0.0 && m.1.1 + " as ".len() as u32 == r.0.1))
            })
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

    // The checker gave up on the line: what else it says there comes
    // from a solve it did not finish.
    let limit_lines: Vec<u32> = items
        .iter()
        .filter(|d| {
            d.get("message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains(alloy::typecheck::SOLVER_LIMIT))
        })
        .filter_map(|d| Some(d.get("range").and_then(range_of)?.0.0))
        .collect();

    items.retain(|d| {
        let message = d.get("message").and_then(Value::as_str).unwrap_or_default();

        message.contains(alloy::typecheck::SOLVER_LIMIT)
            || !message.starts_with("TypeError")
            || d.get("range")
                .and_then(range_of)
                .is_none_or(|r| !limit_lines.contains(&r.0.0))
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
    if alloy::shapes::names_only_the_emit(message) {
        return false;
    }

    // The enum emit writes `tag` and `_1`; a report about one of those,
    // on a line the source never wrote them on, describes the emit.
    if d.pointer("/range/start/line")
        .and_then(Value::as_u64)
        .and_then(|line| doc.source.lines().nth(line as usize))
        .is_some_and(|text| {
            alloy::shapes::names_the_emit_key(message, text)
                || alloy::shapes::duplicate_only_in_the_emit(message, text)
        })
    {
        return false;
    }

    // The child reads the Alloy source when the compile stopped, and
    // reads none of it: every line draws a syntax error or an unknown
    // global. The compile error alone says what is wrong. A repaired
    // artifact reads, but it holds a placeholder the author never
    // wrote, so its reports name nothing to fix either.
    if doc.output.is_none() || doc.repair.is_some() {
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

    if answers_to_the_private_lint(d, doc, lint_config) {
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

    // A deprecated component reads by its tag, `<OldRow />`: the
    // lowering writes the call, and the report stands for the tag.
    if message.starts_with("DeprecatedApi") {
        return true;
    }

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
    let (end_line, end) = quoted_span_on_line(&doc.source, line as u32)
        .map(|(_, e)| e)
        .unwrap_or_else(|| (line as u32, text.encode_utf16().count() as u32));
    d["range"] = json!({
        "start": { "line": line, "character": start },
        "end": { "line": end_line, "character": end },
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
    let mut known = st.known_shapes_at(here.as_deref());

    if let Some(message) = d.get("message").and_then(Value::as_str) {
        let mut folded = json!(message);
        strip_std_prefix(&mut folded);
        let text = alloy::shapes::fold(folded.as_str().unwrap_or(message), &known);
        d["message"] = json!(alloy::shapes::friendly_text(&text));
    }

    // A private method sits outside the struct's public table, so the
    // checker reads a call of one from another file as a member the
    // struct has not got. The rewrite names a private member off the
    // shape, the way it names a private field, so the methods the
    // project's impls keep to themselves join the shapes here. `alloy
    // flux` feeds its own rewrite the same list.
    let privates = st.project_impls();

    for shape in &mut known.shapes {
        let alloy::declarations::Shape::Struct { name, fields, .. } = shape else {
            continue;
        };
        let Some((_, names)) = privates.privates.iter().find(|(t, _)| t == name) else {
            continue;
        };

        for n in names {
            if !fields.iter().any(|(f, _)| f == n) {
                fields.push((n.clone(), true));
            }
        }
    }

    alloy_wording(d, doc, here.as_deref(), &known.shapes, &raw);

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
            // A plain `.luau` module has no export table, so its report
            // asks for a `return` alone.
            let luau = here
                .as_deref()
                .and_then(|uri| st.resolve_spec(uri, &spec))
                .and_then(|p| imports::module_file(&imports::module_path(&p)))
                .and_then(|f| f.extension().map(|e| e == "luau" || e == "lua"))
                .unwrap_or(false);
            d["message"] = json!(format!(
                "UnknownModule: {}",
                alloy::typecheck::no_module_return_message(&spec, luau)
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
///
/// `raw` is the report as the child wrote it, before the fold: the
/// remote rewrite lists the members the fold cuts to `Remote`.
pub(crate) fn alloy_wording(
    d: &mut Value,
    doc: &Doc,
    here: Option<&str>,
    shapes: &[alloy::declarations::Shape],
    raw: &str,
) {
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
    // A name an import the file writes could bring in: the fix is one
    // word in that list, and the terminal says the same.
    // `new Nope { }` names a struct, not a global. The compiler writes
    // the sentence and moves the report onto the name.
    let unknown_struct = here.and_then(uri_to_path).and_then(|path| {
        alloy::typecheck::unknown_struct_report(&message, &path, &doc.source, sl as usize + 1)
    });

    if unknown_struct.is_none()
        && let Some(path) = here.and_then(uri_to_path)
        && let Some(better) = alloy::modules::missing_import_message(&message, &path, &doc.source)
    {
        d["message"] = json!(format!("{kind}: {better}"));

        return;
    }

    if unknown_struct.is_none()
        && let Some((better, at)) =
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
    if let Some(better) = unknown_struct.or_else(|| {
        alloy::typecheck::resite_report(
            &body,
            shapes,
            &doc.source,
            sl as usize + 1,
            byte_column(doc, sl, sc),
        )
    }) {
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
    // remote's surface, and the source names the remote. The compiler
    // writes the sentence off the source line; the span the report
    // covers may start at a macro's `$`, which read as the name.
    // The raw report still lists the members of the surface, so the
    // "did you mean" reads as `alloy flux` prints it.
    if message.contains("not found in table 'Remote'") {
        let better = alloy::typecheck::friendly_type_message(
            raw,
            &alloy::shapes::Known::default(),
            Some(line),
            sc as usize + 1,
        );
        d["message"] = json!(format!("{kind}: {better}"));

        return;
    }

    // A `{ ... }` where an Array belongs: the checker answers with the
    // nineteen methods the table lacks, and the mistake is the bracket.
    if let Some(hint) = alloy::shapes::plain_table_hint(&body) {
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

/// Whether the word at `start` names a parameter of a function the
/// source declares: `function f(a, b: T)` at `a` or `b`.
fn names_a_parameter(src: &str, start: usize) -> bool {
    let declared = super::completion::open_paren_word(src, start)
        .is_some_and(|(word, _, _)| super::completion::declares_params(src, word));

    declared && src[..start].trim_end().ends_with(['(', ','])
}

/// The names a message writes in backticks.
fn quoted_names(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The variant or the field a misspelling meant: the one name of the
/// list the report prints that stands within two edits of the word the
/// file wrote. Two names that near say nothing about which one, so
/// neither answers.
fn nearest_variant_fix(message: &str) -> Option<String> {
    let (head, tail) = message
        .split_once("; its variants are ")
        .or_else(|| message.split_once("; its fields are "))?;
    let wrote = quoted_names(
        head.split(" has no variant ")
            .nth(1)
            .or_else(|| head.split(" has no field ").nth(1))?,
    )
    .pop()?;
    let near: Vec<String> = quoted_names(tail)
        .into_iter()
        .filter(|v| edit_distance(v, &wrote) <= 2)
        .collect();

    match near.as_slice() {
        [one] => Some(one.clone()),

        _ => None,
    }
}

/// The member a remote typo or a struct field typo meant, with the
/// member the file wrote: the compiler's sentence names both.
fn remote_verb_fix(message: &str) -> Option<(String, String)> {
    let (head, tail) = message.split_once("; did you mean ")?;
    let remote = head.contains("remote `") && head.contains("` has no `");
    let member = head.contains("` has no field `") || head.contains("` has no method `");

    if !remote && !member {
        return None;
    }

    let wrote = quoted_names(head).pop()?;
    let name = quoted_names(tail).pop()?;

    Some((wrote, name))
}

/// The call a report says needs `:`, as the source wrote it: `bag.add`
/// from "`add` is a method; call it with `bag:add(...)`, not
/// `bag.add(...)`".
fn dot_call_fix(message: &str) -> Option<String> {
    if !message.contains(alloy::typecheck::DOT_FOR_COLON) {
        return None;
    }

    let wrote = quoted_names(message).pop()?;

    wrote.strip_suffix("(...)").map(str::to_string)
}

/// The edit distance of two names, for a "did you mean".
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }

    row[b.len()]
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

/// True for a report that names a member the struct does have, on a
/// line `private_access` already lints. The declaring file keeps a
/// private member out of the struct's public type, so the checker reads
/// it as missing. `alloy doc private` promises a type error there, so
/// the report that names the member as private stands beside the lint;
/// these two would send the reader after a member that is there.
/// `alloy flux` drops the same two, after the same rewrite, so the
/// caller tests the message the reader sees.
pub(crate) fn answers_to_the_private_lint(
    d: &Value,
    doc: &Doc,
    lint_config: &alloy::config::LintConfig,
) -> bool {
    let message = d.get("message").and_then(Value::as_str).unwrap_or_default();

    if !message.contains("has no method") && !message.contains("not found in table") {
        return false;
    }

    let Some(out) = &doc.output else {
        return false;
    };
    let Some(((sl, _), _)) = d.get("range").and_then(range_of) else {
        return false;
    };
    let silence = alloy::directives::scan(&doc.source);

    out.lints.iter().any(|l| {
        l.name == "private_access"
            && alloy::lint::level_in(lint_config, &silence, l.name) != alloy::lint::Level::Allow
            && alloy::directives::line_of(&doc.source, l.start as usize) == sl as usize
    })
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

/*
One missing member as the source writes it, indented to the body of the
declaration it goes in.

A function takes its parameter list and an `end`; a field takes its type.
The visibility goes in front when the clause asked for one, and the
member reads as the plain member it is when the clause took either.

`step` is `[fmt] indent_width`, so the text the fix writes is the text
the formatter keeps.
*/
fn member_text(gap: &alloy::desugar::ContractGap, step: usize) -> String {
    let pad = " ".repeat(gap.indent as usize + step);
    let visibility = match gap.visibility.is_empty() {
        true => String::new(),

        false => format!("{} ", gap.visibility),
    };

    if gap.kind == "field" {
        let ty = match gap.shape.is_empty() {
            true => "unknown".to_string(),

            false => gap.shape.clone(),
        };

        return format!("{pad}{visibility}{}: {ty}\n", gap.member);
    }

    let params = match gap.shape.is_empty() {
        true => "(self)".to_string(),

        false => gap.shape.clone(),
    };
    // A stub whose clause declares a return type must return, or the
    // checker reports that not all codepaths do. `error` returns
    // `never`, which every return type takes: `boolean`, a tuple, a
    // type parameter. A value of the right type would be a lie the
    // author has to find later.
    let body = match declares_a_return(&params) {
        true => format!("{pad}{}error(\"todo\")\n", " ".repeat(step)),

        false => String::new(),
    };

    format!(
        "{pad}{visibility}function {}{params}\n{body}{pad}end\n",
        gap.member
    )
}

/// Whether a clause's shape declares a return type: `(self): boolean`
/// does, `(self, dt: number)` does not. The `:` after the closing
/// parenthesis of the parameter list is the one that counts.
fn declares_a_return(shape: &str) -> bool {
    let mut depth = 0i32;

    for (i, c) in shape.char_indices() {
        match c {
            '(' => depth += 1,

            ')' => {
                depth -= 1;

                if depth == 0 {
                    return shape[i + 1..].trim_start().starts_with(':');
                }
            }

            _ => {}
        }
    }

    false
}

/// One `import` statement of a source: the whole statement with its
/// newline, and every name it binds with the bytes to cut to drop that
/// one name. A name outside the `{ ... }` list, a `* as M` or a default
/// binding, cuts back to the list; with no list the statement goes
/// whole.
struct ImportLine {
    span: (usize, usize),
    names: Vec<Bound>,
    /// The bytes from the head name to the `}`, which drop the whole
    /// list and leave `import * as M`. `None` when the statement binds
    /// no head or no list.
    list_cut: Option<(usize, usize)>,
}

/// One name an `import` statement binds: the byte range of the name and
/// the bytes to cut to drop that one name.
struct Bound {
    name: (usize, usize),
    cut: Option<(usize, usize)>,
    /// Whether the name stands outside the `{ ... }` list: the `M` of
    /// `* as M`, or a default binding.
    head: bool,
}

/// The `import` statements of a source. A statement may run over several
/// lines, so the walk reads from the `import` to the `from` of the same
/// statement.
fn import_lines(src: &str) -> Vec<ImportLine> {
    // A comment in a list holds no name, no comma and no `from`. The
    // walk reads a copy with each comment blanked; the copy keeps every
    // offset and every line break.
    let mut blank = src.as_bytes().to_vec();

    for (a, b) in alloy_syntax::lexer::lex(src).map_or(Vec::new(), |l| l.comments) {
        for byte in &mut blank[a as usize..b as usize] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }

    let blank = String::from_utf8(blank).unwrap_or_else(|_| src.to_string());
    let src = blank.as_str();
    let mut out = Vec::new();
    let mut at = 0;

    for line in src.split_inclusive('\n') {
        let here = at;
        at += line.len();

        // The keyword may stand alone on its line, with the names under
        // it, so any space after it opens a statement.
        let lead = line.len() - line.trim_start().len();
        let Some(rest) = line[lead..].strip_prefix("import") else {
            continue;
        };

        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            continue;
        }

        let word = here + lead;

        let Some(from) = src[here..].find(" from ").map(|i| here + i) else {
            continue;
        };
        let end = src[from..].find('\n').map_or(src.len(), |i| from + i + 1);
        let mut names: Vec<Bound> = Vec::new();
        let list = src[here..from]
            .find('{')
            .map(|i| here + i)
            .and_then(|open| {
                src[open..from]
                    .find('}')
                    .map(|i| open + i)
                    .map(|close| (open, close))
            });
        let mut list_cut = None;

        if let Some(name) = head_name(src, word, from) {
            // `import * as M, { a }`: the head half runs from the `*` or
            // the default name to the `{`, and cutting it leaves
            // `import { a }`. The list itself goes back to the head
            // name, which leaves `import * as M`.
            let after = &src[word + "import".len()..from];
            let head_at = word + "import".len() + (after.len() - after.trim_start().len());
            let cut = list.map(|(open, _)| (head_at, open));
            list_cut = list.map(|(_, close)| (name.1, close + 1));
            names.push(Bound {
                name,
                cut,
                head: true,
            });
        }

        if let Some((open, close)) = list {
            names.extend(list_cuts(src, open, close));
        }

        if !names.is_empty() {
            out.push(ImportLine {
                span: (here, end),
                names,
                list_cut,
            });
        }
    }

    out
}

/// The name a statement's head binds outside its list: the `M` of
/// `* as M`, or a default binding. `None` when the head opens the list.
fn head_name(src: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let head = src.get(start..end)?;
    let after = head.strip_prefix("import")?;

    if !after.starts_with(char::is_whitespace) {
        return None;
    }

    let lead = "import".len() + (after.len() - after.trim_start().len());
    let rest = after.trim_start();

    if rest.starts_with('{') || rest.starts_with("type") {
        return None;
    }

    let at = match rest.strip_prefix('*') {
        Some(star) => {
            let gap = star.len() - star.trim_start().len();
            let named = star.trim_start().strip_prefix("as ")?;

            lead + 1 + gap + 3 + (named.len() - named.trim_start().len())
        }

        None => lead,
    };
    let word: String = src[start + at..end]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    (!word.is_empty()).then(|| (start + at, start + at + word.len()))
}

/// Each entry of an `import { ... }` list with the bytes to cut to drop
/// it: the entry and the comma after it, or the comma before the last
/// one. `type T as U` binds `U`, so the name is the entry's last word.
fn list_cuts(src: &str, open: usize, close: usize) -> Vec<Bound> {
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut start = open + 1;

    for (i, c) in src[open + 1..close].char_indices() {
        if c == ',' {
            parts.push((start, open + 1 + i));
            start = open + 2 + i;
        }
    }

    parts.push((start, close));
    let parts: Vec<(usize, usize)> = parts
        .into_iter()
        .filter_map(|(s, e)| {
            let text = src.get(s..e)?;
            let lead = text.len() - text.trim_start().len();

            (!text.trim().is_empty()).then(|| (s + lead, s + text.trim_end().len()))
        })
        .collect();
    let mut out = Vec::new();

    for (k, &(s, e)) in parts.iter().enumerate() {
        let entry = &src[s..e];
        let word = entry.split_whitespace().next_back().unwrap_or(entry);
        let name_at = s + (entry.len() - word.len());
        // A list over several lines: an entry on a line of its own goes
        // with that line, so the comment of a neighbour stays.
        let line_start = src[..s].rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[e..].find('\n').map_or(src.len(), |i| e + i + 1);
        let after = src[e..line_end].trim_start();
        let alone = src[line_start..s].trim().is_empty()
            && after.strip_prefix(',').unwrap_or(after).trim().is_empty();
        let cut = match (parts.get(k + 1), k) {
            _ if alone => (line_start, line_end),

            (Some(&(next, _)), _) => (s, next),

            (None, 0) => (s, e),

            (None, _) => (parts[k - 1].1, e),
        };

        out.push(Bound {
            name: (
                name_at + (word.len() - word.trim_start_matches('@').len()),
                e,
            ),
            cut: Some(cut),
            head: false,
        });
    }

    out
}

impl State {
    /// The removals for the unused names of a file's imports, and the
    /// child's own removal dropped when it takes a name the file still
    /// uses. The `unused_import` lint carries no fix, and luau-lsp reads
    /// the emit, where one import is one `require`: its `Remove all
    /// unused code` cuts the whole statement even when two of three
    /// names are alive.
    pub(crate) fn unused_import_actions(
        &self,
        uri: &str,
        range: ((u32, u32), (u32, u32)),
        actions: &mut Vec<Value>,
    ) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(out) = &doc.output else {
            return;
        };

        if alloy::lint::level_in(
            &self.lint_config(),
            &alloy::directives::scan(&doc.source),
            "unused_import",
        ) == alloy::lint::Level::Allow
        {
            return;
        }

        let unused: Vec<(usize, usize)> = out
            .lints
            .iter()
            .filter(|l| l.name == "unused_import")
            .map(|l| (l.start as usize, l.end.max(l.start) as usize))
            .collect();

        if unused.is_empty() {
            return;
        }

        let ((from_line, _), (to_line, _)) = range;
        let mut mine: Vec<Value> = Vec::new();
        let mut covered: Vec<(u32, u32)> = Vec::new();

        for line in import_lines(&doc.source) {
            let dead = |b: &Bound| unused.iter().any(|&(s, e)| (s, e) == b.name);
            let dead_names: Vec<&Bound> = line.names.iter().filter(|b| dead(b)).collect();

            if dead_names.is_empty() {
                continue;
            }

            let (sl, _) = position_of(&doc.source, line.span.0);
            let (el, _) = position_of(&doc.source, line.span.1.saturating_sub(1));
            let list_dead = line.names.iter().filter(|b| !b.head).count() > 0
                && line.names.iter().all(|b| b.head || dead(b));
            let head_dead = line.names.iter().any(|b| b.head && dead(b));
            // Every name gone takes the statement. A dead list under a
            // live head leaves the head alone; else each dead entry goes
            // with the comma that joins it to its neighbour.
            let cuts: Vec<(usize, usize)> = if dead_names.len() == line.names.len() {
                vec![line.span]
            } else if list_dead && !head_dead {
                line.list_cut.into_iter().collect()
            } else {
                dead_names.iter().filter_map(|b| b.cut).collect()
            };

            if cuts.is_empty() || el < from_line || sl > to_line {
                continue;
            }

            covered.push((sl, el));
            let edits: Vec<Value> = cuts
                .iter()
                .map(|&(s, e)| {
                    let (a, b) = position_of(&doc.source, s);
                    let (c, d) = position_of(&doc.source, e);

                    json!({
                        "range": { "start": { "line": a, "character": b }, "end": { "line": c, "character": d } },
                        "newText": "",
                    })
                })
                .collect();
            let (nl, nc) = position_of(&doc.source, dead_names[0].name.0);
            let (ne, nec) = position_of(&doc.source, dead_names[0].name.1);
            let title = match dead_names.len() {
                1 => "Remove unused import".to_string(),

                n => format!("Remove {n} unused imports"),
            };
            let message = out
                .lints
                .iter()
                .find(|l| l.start as usize == dead_names[0].name.0)
                .map(|l| l.message.clone())
                .unwrap_or_default();
            mine.push(json!({
                "title": title,
                "kind": "quickfix",
                "isPreferred": true,
                "diagnostics": [{
                    "range": { "start": { "line": nl, "character": nc }, "end": { "line": ne, "character": nec } },
                    "severity": 2,
                    "source": "Alloy",
                    "code": alloy::docs::LINT_CODE,
                    "message": format!("unused_import: {message}"),
                }],
                "edit": { "changes": { uri: edits } },
            }));
        }

        // The child reads the emit and cuts the whole `require` line,
        // which takes the names the file still uses. Ours is the edit
        // for that statement, so the child's goes. A statement may run
        // over several lines, and a cut that touches any of them is the
        // child's cut of that statement.
        actions.retain(|a| {
            let Some(edits) = a
                .get("edit")
                .and_then(|e| e.get("changes"))
                .and_then(|c| c.get(uri))
                .and_then(Value::as_array)
            else {
                return true;
            };

            !edits.iter().any(|e| {
                let line = |key: &str| e.pointer(key).and_then(Value::as_u64).map(|l| l as u32);
                let Some(first) = line("/range/start/line") else {
                    return false;
                };
                let last = line("/range/end/line").unwrap_or(first);

                e.get("newText").and_then(Value::as_str) == Some("")
                    && covered.iter().any(|&(sl, el)| first <= el && last >= sl)
            })
        });
        actions.extend(mine);
    }

    /// Whether an action of the child can stay in an Alloy file's list,
    /// while its ranges still name the shadow. The child computes it on
    /// the lowered Luau, so an edit over generated text writes that Luau
    /// into the source. An edit that changes nothing is noise.
    pub(crate) fn keeps_child_action(
        &self,
        action: &Value,
        uri: &str,
        range: Option<((u32, u32), (u32, u32))>,
    ) -> bool {
        // The child's extracts read the lowered Luau. "Extract to
        // function" passes a module name as an untyped parameter and
        // drops the types. "Extract to local variable" on a statement
        // word or a declared name takes the whole enclosing function.
        match action.pointer("/data/type").and_then(Value::as_str) {
            Some("extractFunction") => return false,

            Some("extractVariable") if !self.extracts_at(uri, range) => return false,

            // The child inlines a `local` or a `const` that holds a
            // value. A parameter, an import, or a function has none, and
            // its resolve carries no edit.
            Some("inlineVariable") if !self.inlines(uri, action, range.map(|r| r.1.0)) => {
                return false;
            }

            _ => {}
        }

        if let Some(edit) = action.get("edit") {
            return edit_count(edit) > 0 && self.writes_source_only(edit);
        }

        // A refactor sends its edit on resolve. A selection over text
        // the lowering wrote gives an edit over that text.
        let refactor = action
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k.starts_with("refactor"));
        // A position the lowering replaced maps to the next copied byte,
        // which maps back somewhere else.
        let clean = || {
            let (start, end) = range?;
            let doc = self.docs.get(uri)?;
            let (s, e) = (doc.to_shadow(start.0, start.1), doc.to_shadow(end.0, end.1));

            Some(
                doc.to_source(s.0, s.1) == start
                    && doc.to_source(e.0, e.1) == end
                    && doc.copies_source(s, e),
            )
        };

        !refactor || clean() == Some(true)
    }

    /// Whether the start of a source range sits in an expression, where
    /// an extract has something to take. A declared name and a word
    /// that only a statement writes are not in one. An `if` or a `then`
    /// is a statement's on a line that `if`, `elseif` or `else` opens.
    fn extracts_at(&self, uri: &str, range: Option<((u32, u32), (u32, u32))>) -> bool {
        let Some(((line, character), _)) = range else {
            return false;
        };
        let Some(doc) = self.docs.get(uri) else {
            return false;
        };
        let Some(at) = offset_of(&doc.source, line, character) else {
            return false;
        };

        // A `.config.aly` is one `export default` table, and the child
        // writes the new `local` between `export default` and `const`.
        if crate::config_aly::is_config(uri) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, at);
        let word = &doc.source[start..end];
        let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
        let branch = matches!(word, "if" | "then" | "elseif" | "else")
            && matches!(
                doc.source[line_start..].split_whitespace().next(),
                Some("if" | "elseif" | "else")
            );

        // A type has no value to extract, and neither has a parameter.
        // A caret on the space before a word reads the head up to it.
        let rest = &doc.source[start..];
        let word_at = start + rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let head = &doc.source[line_start..word_at];
        // A key, `{ alpha = 1 }`, and a method name, `cp:advance`, name
        // no value: the child writes `local extracted = alpha`. The
        // braces of `new S { }` and a `?.` or `!.` chain lower to
        // generated text, and there the child's edit comes back empty.
        let before = doc.source[..start].trim_end();
        let after = &doc.source[end..];
        let key = before.ends_with(['{', ',', ';'])
            && after.trim_start().starts_with('=')
            && !after.trim_start().starts_with("==");
        let method = doc.source[..start].ends_with(':') && !doc.source[..start].ends_with("::");
        let chain = doc.source[line_start..start]
            .rsplit(|c: char| !(c.is_alphanumeric() || "_.:?![]".contains(c)))
            .next()
            .unwrap_or("");
        let nil_safe = ["?.", "!.", "?[", "!["]
            .iter()
            .any(|s| chain.contains(s) || after.starts_with(s));
        // A caret on the `{` of `new S {` sits in the braces too.
        let past = doc.source[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8());

        // The target of an assignment, `t.n = 5` or `t.n += 1`, is no
        // value to read: the child writes `extracted = 5`.
        let target = alloy_syntax::lexer::lex(&doc.source).is_ok_and(|lexed| {
            lexed
                .toks
                .iter()
                .position(|t| t.start as usize == start && t.end as usize == end)
                .is_some_and(|i| assignment_target(&lexed.toks, &doc.source, i))
        });

        !branch
            && !key
            && !target
            && !method
            && !nil_safe
            && !super::completion::in_constructor_braces(&doc.source, past)
            && !declares_a_name_at(&doc.source, at)
            && !crate::context::takes_a_type(head)
            && !names_a_parameter(&doc.source, start)
            && !matches!(
                word,
                "local"
                    | "const"
                    | "return"
                    | "do"
                    | "end"
                    | "while"
                    | "for"
                    | "in"
                    | "repeat"
                    | "until"
                    | "break"
                    | "continue"
                    | "export"
            )
    }

    /// Whether the name an "Inline variable" action names is a `local`
    /// or a `const` of the file, which the child can inline.
    ///
    /// The child deletes the line that declares the name and writes its
    /// value at each use. A line the lowering rewrote, `local cp = new S
    /// { }`, gives an edit over generated text, and resolve drops it.
    fn inlines(&self, uri: &str, action: &Value, line: Option<u32>) -> bool {
        let name = action
            .get("title")
            .and_then(Value::as_str)
            .and_then(|t| t.strip_prefix("Inline variable '"))
            .and_then(|t| t.strip_suffix('\''));
        let Some((name, doc)) = name.zip(self.docs.get(uri)) else {
            return true;
        };
        let bound = doc.bindings.iter().any(|b| {
            let words: Vec<&str> = b.prefix.split_whitespace().collect();

            b.name == name
                && words.iter().any(|w| matches!(*w, "local" | "const"))
                && !words.contains(&"function")
        });
        let declares = |text: &str| {
            let text = text.trim_start();
            let text = text.strip_prefix("export ").unwrap_or(text);
            let text = text.strip_prefix("global ").unwrap_or(text);

            ["local ", "const "].iter().any(|k| {
                text.strip_prefix(k).is_some_and(|rest| {
                    rest.strip_prefix(name).is_some_and(|tail| {
                        !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                })
            })
        };
        let Some((at, text)) = doc
            .source
            .lines()
            .enumerate()
            .take(line.map_or(usize::MAX, |l| l as usize + 1))
            .filter(|(_, text)| declares(text))
            .last()
        else {
            return bound;
        };
        let from = doc
            .source
            .split_inclusive('\n')
            .take(at)
            .map(str::len)
            .sum();
        let at = at as u32;
        let width = text.encode_utf16().count() as u32;

        bound
            && doc.copies_source(doc.to_shadow(at, 0), doc.to_shadow(at, width))
            && inline_keeps_behaviour(&doc.source, from, name)
    }

    /// Puts parentheses around a value a resolved "Inline variable"
    /// writes where the use goes on with `.`, `:`, `[` or `(`. The child
    /// writes `{ stage = 2 }.stage` and `"hi":upper()`, and neither
    /// parses. A name, a call, or an index needs none.
    pub(crate) fn wrap_inlined(&self, action: &mut Value) {
        if action.pointer("/data/type").and_then(Value::as_str) != Some("inlineVariable") {
            return;
        }

        let Some(changes) = action
            .pointer_mut("/edit/changes")
            .and_then(Value::as_object_mut)
        else {
            return;
        };

        for (uri, edits) in changes.iter_mut() {
            let Some(doc) = self.docs.get(uri) else {
                continue;
            };

            for e in edits.as_array_mut().into_iter().flatten() {
                let end = e
                    .get("range")
                    .and_then(range_of)
                    .and_then(|(_, (l, c))| offset_of(&doc.source, l, c));
                let text = e.get("newText").and_then(Value::as_str).unwrap_or("");
                let goes_on = end.is_some_and(|end| {
                    let rest = &doc.source[end..];

                    rest.starts_with(['.', ':', '[', '(']) && !rest.starts_with("..")
                });
                // `x.y = 1` parses only where `x` is a prefix expression.
                let prefix = alloy_syntax::parse_one(&format!("{text}.y = 1")).is_ok();

                if goes_on && !text.is_empty() && !prefix {
                    e["newText"] = json!(format!("({text})"));
                }
            }
        }
    }

    /// Whether a resolved "Extract to local variable" or "Inline
    /// variable" leaves each source it edits parsing. The child reads
    /// the lowered Luau and can write `local extracted = local function`.
    /// The check is the one `--fix` gives a lint's rewrite. Other
    /// actions pass.
    pub(crate) fn extract_parses(&self, action: &Value) -> bool {
        if !matches!(
            action.pointer("/data/type").and_then(Value::as_str),
            Some("extractVariable" | "inlineVariable")
        ) {
            return true;
        }

        let mut changes = action
            .pointer("/edit/changes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten();

        changes.all(|(uri, edits)| {
            let Some(doc) = self.docs.get(uri) else {
                return true;
            };
            let src = doc.source.as_str();
            let fixes: Option<Vec<alloy::lint::Fix>> = edits
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    let ((sl, sc), (el, ec)) = e.get("range").and_then(range_of)?;
                    let start = offset_of(src, sl, sc)? as u32;
                    let end = offset_of(src, el, ec)? as u32;

                    Some(alloy::lint::Fix::new(
                        src,
                        start,
                        end,
                        e.get("newText").and_then(Value::as_str)?,
                    ))
                })
                .collect();
            let Some((first, rest)) = fixes.as_deref().and_then(<[_]>::split_first) else {
                return false;
            };
            let fix = alloy::lint::Fix {
                more: rest.to_vec(),
                ..first.clone()
            };

            alloy::lint::sound(src, vec![&fix]).1.is_empty()
        })
    }

    /// Whether a workspace edit of the child, in shadow terms, rewrites
    /// only text the author wrote in every Alloy file it touches.
    pub(crate) fn writes_source_only(&self, edit: &Value) -> bool {
        let changes = edit
            .get("changes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(uri, edits)| (uri.as_str(), edits));
        let documents = edit
            .get("documentChanges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| Some((c.pointer("/textDocument/uri")?.as_str()?, c.get("edits")?)));

        changes.chain(documents).all(|(uri, edits)| {
            let (real, is_alloy) = self.editor_uri(uri);

            if !is_alloy {
                return true;
            }

            let Some(doc) = self.docs.get(&real) else {
                return false;
            };

            edits.as_array().into_iter().flatten().all(|e| {
                e.get("range")
                    .and_then(range_of)
                    .is_some_and(|(start, end)| doc.copies_source(start, end))
            })
        })
    }
}

/// How many text edits a workspace edit holds.
fn edit_count(edit: &Value) -> usize {
    let lists = edit
        .get("changes")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
        .chain(
            edit.get("documentChanges")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("edits")),
        );

    lists.filter_map(Value::as_array).map(Vec::len).sum()
}

/// Whether the name at token `i` sits in the targets of an assignment
/// or of a compound one: `t` and `n` in `t.n = 5`, `a` in `a, b = 1, 2`.
/// Each `.name` and `[index]` after the name belongs to its target. A
/// name inside the index, `i` in `t[i] = 5`, is a value.
fn assignment_target(toks: &[alloy_syntax::lexer::Tok], src: &str, i: usize) -> bool {
    use alloy_syntax::lexer::TokKind;

    const ASSIGN: [&str; 9] = ["=", "+=", "-=", "*=", "/=", "//=", "%=", "^=", "..="];
    let text = |j: usize| toks.get(j).map_or("", |t| t.text(src));
    let ident = |j: usize| toks.get(j).is_some_and(|t| t.kind == TokKind::Ident);
    let target_end = |mut j: usize| {
        loop {
            if toks.get(j).is_some_and(|t| t.kind == TokKind::Dot) && ident(j + 1) {
                j += 2;
            } else if text(j) == "[" {
                let mut depth = 0;

                while j < toks.len() {
                    match text(j) {
                        "[" => depth += 1,

                        "]" => depth -= 1,

                        _ => {}
                    }

                    j += 1;

                    if depth == 0 {
                        break;
                    }
                }
            } else {
                return j;
            }
        }
    };
    let mut j = target_end(i + 1);

    while text(j) == "," && ident(j + 1) {
        j = target_end(j + 2);
    }

    ASSIGN.contains(&text(j))
}

/*
Whether "Inline variable" keeps what the code does, for the declaration
of `name` on the line that starts at byte `from`.

The child writes the value at each use and deletes the declaration. A
value with a call then runs at the use: once per item inside a `for`
body, or after a later call that it ran before. So a literal and a
plain name inline anywhere. Other values inline only when they read
names, fields, and operators, and when one use at most reads them,
outside any loop or function that starts after the declaration.

The value also reads its names and fields at the use. An assignment to
one of them before the use, `a = 10` or `t.n = 5`, changes what the use
reads, so the action goes.
*/
fn inline_keeps_behaviour(src: &str, from: usize, name: &str) -> bool {
    use alloy_syntax::lexer::TokKind;

    const OPERATORS: [&str; 15] = [
        "+", "-", "*", "/", "//", "%", "^", "..", "==", "~=", "<", ">", "<=", ">=", "#",
    ];
    // The words a value may hold. The first three carry it to the next line.
    const WORDS: [&str; 10] = [
        "and", "or", "not", "nil", "true", "false", "if", "then", "else", "elseif",
    ];

    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return false;
    };
    let toks = &lexed.toks;
    let text = |i: usize| toks[i].text(src);
    let mut at = 0;
    let mut row = 0;
    let lines: Vec<usize> = toks
        .iter()
        .map(|t| {
            row += src[at..t.start as usize].matches('\n').count();
            at = t.start as usize;

            row
        })
        .collect();
    // A `(`, a string, or a `{` after one of these makes a call.
    let ends_a_value = |i: usize| {
        (toks[i].kind == TokKind::Ident && !WORDS.contains(&text(i)))
            || matches!(text(i), ")" | "]")
    };
    let Some(bound) = (0..toks.len()).find(|&i| toks[i].start as usize >= from && text(i) == name)
    else {
        return false;
    };
    let mut depth = 0;
    let Some(eq) = (bound + 1..toks.len()).find(|&i| {
        match text(i) {
            "(" | "{" | "[" => depth += 1,

            ")" | "}" | "]" => depth -= 1,

            _ => {}
        }

        depth == 0 && text(i) == "="
    }) else {
        return false;
    };

    // The value runs to the end of its line, or further while a bracket
    // is open or an operator carries it on.
    let carries = |i: usize| OPERATORS.contains(&text(i)) || WORDS[..3].contains(&text(i));
    let mut end = eq + 1;
    let mut depth = 0;

    while end < toks.len() {
        if depth == 0
            && end > eq + 1
            && lines[end] > lines[end - 1]
            && !carries(end - 1)
            && !carries(end)
            && !matches!(text(end), "then" | "else" | "elseif")
        {
            break;
        }

        match text(end) {
            "(" | "{" | "[" => depth += 1,

            ")" | "}" | "]" => depth -= 1,

            _ => {}
        }

        end += 1;
    }

    let value = eq + 1..end;
    // A literal or a name reads the same at any use, loop or not.
    let single = value.len() == 1;

    if single
        && !matches!(
            toks[value.start].kind,
            TokKind::Number | TokKind::Str { .. } | TokKind::InterpStr | TokKind::Ident
        )
    {
        return false;
    }

    let member = |i: usize| i > 0 && matches!(toks[i - 1].kind, TokKind::Dot | TokKind::Colon);
    let reads = |fields: bool| -> HashSet<&str> {
        value
            .clone()
            .filter(|&i| toks[i].kind == TokKind::Ident && member(i) == fields)
            .map(text)
            .filter(|t| !WORDS.contains(t))
            .collect()
    };
    let (names, fields) = (reads(false), reads(true));

    let pure = single
        || value.clone().all(|i| {
            let call = i > value.start && ends_a_value(i - 1);

            match toks[i].kind {
                TokKind::Ident => !matches!(
                    text(i),
                    "function" | "await" | "new" | "match" | "do" | "end" | "try"
                ),

                TokKind::Number | TokKind::InterpMid | TokKind::InterpTail | TokKind::RParen => {
                    true
                }

                TokKind::Str { .. }
                | TokKind::InterpStr
                | TokKind::InterpHead
                | TokKind::LParen => !call,

                TokKind::Dot => toks.get(i + 1).is_some_and(|t| t.kind == TokKind::Ident),

                TokKind::Colon => false,

                // A table constructor and its `[key]`. A `[` after a value
                // is an index, which can run `__index`.
                TokKind::Symbol => match text(i) {
                    "{" => !call,

                    "[" => i > value.start && matches!(text(i - 1), "{" | "," | ";"),

                    "}" | "]" | "," | ";" | "=" => true,

                    t => OPERATORS.contains(&t),
                },
            }
        });

    if !pure {
        return false;
    }

    // The rest of the block. Each open block says whether it runs its
    // body again: a loop and a function do. An `if` value closes with
    // its `else`.
    #[derive(PartialEq)]
    enum Open {
        Block,
        Again,
        IfValue,
    }

    // Whether the name or the field at `i` is the target of an
    // assignment, of a compound one, or of a new `local`. A key of a
    // table constructor is none.
    let assigned = |i: usize, in_table: bool| {
        let before = if i > 0 { text(i - 1) } else { "" };

        if before == "{" || (in_table && matches!(before, "," | ";")) {
            return false;
        }

        (matches!(before, "local" | "function") && !member(i)) || assignment_target(toks, src, i)
    };

    let mut open: Vec<Open> = Vec::new();
    let mut brackets: Vec<&str> = Vec::new();
    let mut loop_head = false;
    let mut uses = 0;
    let mut last_use = None;
    let mut first_write = None;
    let mut use_again = false;
    let mut write_again = false;

    for i in end..toks.len() {
        match text(i) {
            t @ ("(" | "[" | "{") => brackets.push(t),

            ")" | "]" | "}" => {
                brackets.pop();
            }

            _ => {}
        }

        if toks[i].kind != TokKind::Ident {
            continue;
        }

        let before = if i > 0 { text(i - 1) } else { "" };
        let read = match member(i) {
            true => fields.contains(text(i)),

            false => names.contains(text(i)),
        };

        if read && assigned(i, brackets.last() == Some(&"{")) {
            first_write.get_or_insert(i);
            write_again |= open.contains(&Open::Again);
        }

        match text(i) {
            "function" | "repeat" => open.push(Open::Again),

            "for" | "while" => {
                open.push(Open::Again);
                loop_head = true;
            }

            "do" if loop_head => loop_head = false,

            "do" | "struct" | "enum" | "interface" | "impl" | "trait" | "macro" | "namespace" => {
                open.push(Open::Block);
            }

            "match" if alloy_syntax::contextual::keyword_at_byte(src, toks[i].start as usize) => {
                open.push(Open::Block);
            }

            "if" => {
                let value = OPERATORS.contains(&before)
                    || matches!(
                        before,
                        "=" | "(" | "," | "[" | "{" | "return" | "and" | "or" | "not"
                    )
                    || (matches!(before, "then" | "else") && open.last() == Some(&Open::IfValue));

                open.push(if value { Open::IfValue } else { Open::Block });
            }

            "else" if open.last() == Some(&Open::IfValue) => {
                open.pop();
            }

            "end" | "until" => {
                if open.pop().is_none() {
                    break;
                }
            }

            // The branch or the arm that declares the name ends here.
            "else" | "elseif" if open.is_empty() => break,

            "case" | "default" if open.is_empty() && lines[i] > lines[i - 1] => break,

            word if word == name && !matches!(before, "." | ":") => {
                let again = open.contains(&Open::Again);

                if again && !single {
                    return false;
                }

                uses += 1;
                last_use = Some(i);
                use_again |= again;
            }

            _ => {}
        }
    }

    // A write in a loop reaches a use in a loop on the next turn.
    let moved = first_write.zip(last_use).is_some_and(|(w, u)| w < u);

    !moved && !(write_again && use_again) && (single || uses <= 1)
}

/// The edits that double the `<` at `at` and the `>` that closes it:
/// `id<number>(5)` becomes `id<<number>>(5)`.
fn angle_call_edits(source: &str, at: usize) -> Option<Value> {
    let close = angle_close(source, at)?;
    let (sl, sc) = position_of(source, at);
    let (cl, cc) = position_of(source, close);

    Some(json!([
        {
            "range": { "start": { "line": sl, "character": sc }, "end": { "line": sl, "character": sc } },
            "newText": "<",
        },
        {
            "range": { "start": { "line": cl, "character": cc }, "end": { "line": cl, "character": cc } },
            "newText": ">",
        },
    ]))
}

/// The offset of the `>` that closes the `<` at `at`. The walk reads
/// tokens, so the `>` of a `->` inside a function type does not count.
fn angle_close(source: &str, at: usize) -> Option<usize> {
    let toks = alloy_syntax::lexer::lex(source).ok()?.toks;
    let first = toks.iter().position(|t| t.start as usize == at)?;
    let mut depth = 0;

    for t in &toks[first..] {
        depth += match t.text(source) {
            "<" => 1,

            ">" => -1,

            _ => 0,
        };

        if depth == 0 {
            return Some(t.start as usize);
        }
    }

    None
}

/// The written type and its simple form, from the report of a double
/// negation: "`~~number` negates twice; write `number`".
fn double_negation_fix(message: &str) -> Option<(&str, &str)> {
    let rest = message.strip_prefix('`')?;
    let (wrote, rest) = rest.split_once('`')?;
    let simple = rest
        .strip_prefix(" negates twice; write `")?
        .strip_suffix('`')?;

    Some((wrote, simple))
}
