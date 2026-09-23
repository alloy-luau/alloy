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
        let failure = match loads.get(uri) {
            Some((source, failure)) if *source == doc.source => failure.clone(),

            _ => {
                let failure = alloy::config_aly::evaluate_source(&doc.source, &path)
                    .map_err(|e| alloy::config::ConfigError::Script(path.clone(), e))
                    .and_then(|table| alloy::config::Config::from_table(table, &path))
                    .err()
                    .map(|e| e.to_string());
                loads.insert(uri.to_string(), (doc.source.clone(), failure.clone()));

                failure
            }
        };

        if let Some(message) = failure {
            // The report sits on the statement that gives the config.
            let start = doc
                .source
                .lines()
                .scan(0usize, |at, line| {
                    let here = *at;
                    *at += line.len() + 1;

                    Some((here, line))
                })
                .find(|(_, line)| line.starts_with("export") || line.starts_with("return"))
                .map_or(0, |(at, _)| at);
            let end = doc.source[start..]
                .find('\n')
                .map_or(doc.source.len(), |n| start + n);
            let shown = message.replace(&format!("{}: ", path.display()), "");
            out.push(diagnostic(
                start,
                end,
                &format!("the config does not load: {shown}"),
            ));
        }

        out
    }
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
        let st = self.state.lock().expect("state");
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
                let items = config_aly::completions(&schema, &site);
                let space =
                    message.pointer("/params/context/triggerCharacter") == Some(&json!(" "));

                // A key slot answers even with nothing left to add, so no
                // global of the file lands where a key goes. A space
                // opens only a list of values.
                match (&site.slot, items.is_empty()) {
                    (Slot::Key { .. }, _) if space => None,

                    (Slot::Key { .. }, _) | (_, false) => {
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
