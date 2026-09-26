//! The `.config.aly` answers: completion and hover from the schema of
//! `alloy.toml`, and the problems the schema and the load find.

use super::*;
use crate::config_aly::{self, Slot};

impl State {
    /// The schema of the project's configuration, the ingots' tables
    /// with it. `alloy.toml` validates against the same one.
    pub(crate) fn config_schema(&self) -> Value {
        let manifests: Vec<&alloy::ingot::Manifest> = self
            .ingots
            .as_ref()
            .map(|i| i.list.iter().map(|x| &x.manifest).collect())
            .unwrap_or_default();

        alloy::schema::project(&manifests)
    }

    /// The problems of a config document: keys and literal values the
    /// schema does not take, and then what the load reports. The load
    /// runs the file, so its answer is kept for the source it read.
    pub(crate) fn config_diagnostics(&self, uri: &str) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri).filter(|_| config_aly::is_config(uri)) else {
            return Vec::new();
        };
        let diagnostic = |start: usize, end: usize, message: &str| {
            let (sl, sc) = position_of(&doc.source, start);
            let (el, ec) = position_of(&doc.source, end);

            json!({
                "range": {
                    "start": { "line": sl, "character": sc },
                    "end": { "line": el, "character": ec },
                },
                "severity": 1,
                "source": "Alloy",
                "message": message,
            })
        };
        let schema = self.config_schema();
        let mut out: Vec<Value> = config_aly::check(&schema, &doc.source)
            .into_iter()
            .map(|p| diagnostic(p.start, p.end, &p.message))
            .collect();

        // The file still completes and checks, so the one sign that no
        // build reads it goes on its first line.
        if let Some(message) = uri_to_path(uri)
            .and_then(|p| p.parent().and_then(alloy::config::Config::ignored_script))
        {
            let end = doc.source.find('\n').unwrap_or(doc.source.len());
            let mut d = diagnostic(0, end, &message);
            d["severity"] = json!(2);
            out.push(d);
        }

        // A file that does not compile says so through the compiler, and
        // one the check faults would load with the same complaint.
        let compiles = doc
            .output
            .as_ref()
            .is_some_and(|o| o.diagnostics.is_empty());

        if !out.is_empty() || !compiles {
            return out;
        }

        let Some(path) = uri_to_path(uri) else {
            return out;
        };
        let mut loads = self.config_loads.borrow_mut();
        let loaded = match loads.get(uri) {
            Some((source, loaded)) if *source == doc.source => loaded.clone(),

            _ => {
                let loaded = alloy::config_aly::evaluate_source(&doc.source, &path)
                    .map_err(|e| alloy::config::ConfigError::Script(path.clone(), e))
                    .and_then(|table| alloy::config::Config::from_table(table, &path))
                    .map(|config| {
                        // `<ingot>/<lint>` names a lint the ingot
                        // registers after the load, as the CLI reads it.
                        alloy::lint::unknown_names(&config.lint)
                            .into_iter()
                            .filter(|name| !name.contains('/'))
                            .collect()
                    })
                    .map_err(|e| e.to_string());
                loads.insert(uri.to_string(), (doc.source.clone(), loaded.clone()));

                loaded
            }
        };
        let line_span = |start: usize| {
            let end = doc.source[start..]
                .find('\n')
                .map_or(doc.source.len(), |n| start + n);

            (start, end)
        };

        match loaded {
            // The schema takes any string as a lint name, so the load
            // says which names are none. Each sits on its key.
            Ok(unknown) => {
                for name in unknown {
                    if let Some(at) = key_offset(&doc.source, &name, 0) {
                        let mut d = diagnostic(
                            at,
                            at + name.len(),
                            &alloy::config::unknown_rule_message(&name),
                        );
                        d["severity"] = json!(2);
                        out.push(d);
                    }
                }
            }

            // A run that failed names the line: `<path>:<line>: <message>`.
            // The report sits there, else on the statement that gives the
            // config. A load that refuses several values gives a line each.
            Err(messages) => {
                for message in messages.lines() {
                    let prefix = format!("{}:", path.display());
                    let at_line = message
                        .strip_prefix(&prefix)
                        .and_then(|rest| rest.split_once(": "))
                        .and_then(|(n, text)| Some((n.parse::<usize>().ok()?, text)));
                    let (line_at, shown) = match at_line {
                        Some((n, text)) => (
                            offset_of(&doc.source, n.saturating_sub(1) as u32, 0),
                            text.to_string(),
                        ),

                        None => (None, message.replace(&format!("{}: ", path.display()), "")),
                    };
                    // A value the load refuses, `Sgnal` in a list of std
                    // names, sits where the source writes it: from the
                    // line of its key down, or anywhere with no line.
                    let named =
                        shown
                            .split('`')
                            .nth(1)
                            .filter(|n| !n.is_empty())
                            .and_then(|name| {
                                let at = key_offset(&doc.source, name, line_at.unwrap_or(0))?;

                                Some((at, at + name.len()))
                            });
                    let span = named.or_else(|| line_at.map(line_span));
                    let (start, end) = span.unwrap_or_else(|| {
                        line_span(
                            doc.source
                                .lines()
                                .scan(0usize, |at, line| {
                                    let here = *at;
                                    *at += line.len() + 1;

                                    Some((here, line))
                                })
                                .find(|(_, line)| {
                                    line.starts_with("export") || line.starts_with("return")
                                })
                                .map_or(0, |(at, _)| at),
                        )
                    });
                    out.push(diagnostic(
                        start,
                        end,
                        &format!("the config does not load: {shown}"),
                    ));
                }
            }
        }

        out
    }
}

/// The byte offset of the lint name `name` in a config source, at or
/// after the byte `from`: a key, `name =`, or a string, `["name"] =` or
/// an item of `deny = { ... }`.
fn key_offset(src: &str, name: &str, from: usize) -> Option<usize> {
    let toks = alloy_syntax::lexer::lex(src).ok()?.toks;
    let text = |i: usize| {
        toks.get(i)
            .map_or("", |t| &src[t.start as usize..t.end as usize])
    };
    let first = toks.partition_point(|t| (t.start as usize) < from);

    (first..toks.len()).find_map(|i| {
        let start = toks[i].start as usize;
        let t = text(i);

        if t == name && text(i + 1) == "=" {
            return Some(start);
        }

        let quoted = t.len() == name.len() + 2
            && t.starts_with(['"', '\''])
            && t.get(1..=name.len()) == Some(name);

        quoted.then_some(start + 1)
    })
}

impl Server {
    /// Completion and hover in a config table, from the schema. `false`
    /// leaves the request to the other answers: the caret sits in no
    /// config table, or in a value the schema lists nothing for.
    pub(crate) fn config_answer(
        &self,
        method: &str,
        uri: &str,
        message: &Value,
        id: &Value,
    ) -> bool {
        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };
        let schema = st.config_schema();
        let answer = match method {
            "textDocument/hover" => config_aly::hover(&schema, &doc.source, offset)
                .map(|text| json!({ "contents": { "kind": "markdown", "value": text } })),

            _ => config_aly::site_at(&doc.source, offset).and_then(|site| {
                // The snippets write the quotes and indent `[fmt]` asks for.
                let fmt = st.fmt_config(uri).for_source(&doc.source);
                let items = config_aly::completions(&schema, &site, &fmt);
                let space =
                    message.pointer("/params/context/triggerCharacter") == Some(&json!(" "));

                // A key slot answers even with nothing left to add, so no
                // global of the file lands where a key goes, and so does
                // the inside of a string. A space opens only a list of
                // values.
                match (&site.slot, items.is_empty()) {
                    (Slot::Key { .. }, _) if space => None,

                    (Slot::Key { .. } | Slot::Value { quoted: true, .. }, _) | (_, false) => {
                        Some(json!({ "isIncomplete": false, "items": items }))
                    }

                    _ => None,
                }
            }),
        };
        drop(st);

        match answer {
            Some(result) => {
                self.respond(id, result);

                true
            }

            None => false,
        }
    }
}
