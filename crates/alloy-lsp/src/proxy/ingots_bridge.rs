//! The ingots: their completion items, colors, actions, and hovers, glued to the proxy's documents.

use super::hover::declares_a_name_at;
use super::*;

impl State {
    /// The completion items the ingots offer at a position, as LSP
    /// items, with what the replies ask of the host's own list.
    pub(crate) fn ingot_items(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        trigger: Option<&str>,
    ) -> Option<(Vec<Value>, alloy::ingot::Completed)> {
        let ingots = self.ingots.as_ref()?;
        let doc = self.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;
        let path = uri_to_path(uri)?;
        let completed =
            ingots.complete(&path.to_string_lossy(), &doc.source, offset as u32, trigger);

        Some((
            crate::ingots::completion_items(doc, &completed.items),
            completed,
        ))
    }

    /// The props the ingots read on a markup tag of a document, as the
    /// markup completion takes them.
    pub(crate) fn ingot_props(&self, uri: &str) -> Vec<markup::IngotProp> {
        let (Some(ingots), Some(path)) = (&self.ingots, uri_to_path(uri)) else {
            return Vec::new();
        };

        ingots
            .props(&path.to_string_lossy())
            .into_iter()
            .map(|(name, doc, ingot, insert)| markup::IngotProp {
                name: name.to_string(),
                doc: doc.to_string(),
                ingot: ingot.to_string(),
                insert: insert.to_string(),
            })
            .collect()
    }

    /// The colors the ingots find in a document, as LSP color
    /// information.
    pub(crate) fn ingot_colors(&self, uri: &str) -> Vec<Value> {
        let Some(ingots) = &self.ingots else {
            return Vec::new();
        };
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let colors = ingots.colors(&path.to_string_lossy(), &doc.source);

        crate::ingots::colors(doc, &colors)
    }

    /// The code actions the ingots offer for a range.
    pub(crate) fn ingot_actions(&self, uri: &str, range: ((u32, u32), (u32, u32))) -> Vec<Value> {
        let Some(ingots) = &self.ingots else {
            return Vec::new();
        };
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let (Some(start), Some(end)) = (
            offset_of(&doc.source, range.0.0, range.0.1),
            offset_of(&doc.source, range.1.0, range.1.1),
        ) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let span = (start as u32, end as u32);
        let diagnostics = crate::ingots::diagnostics_in(doc, span);
        let actions = ingots.actions(&path.to_string_lossy(), &doc.source, span, &diagnostics);

        crate::ingots::actions(doc, uri, &actions)
    }
}

impl Server {
    /// Opens a shadow for every Alloy file under the root, and one for the
    /// runtime, so requires between them resolve.
    /// Starts the ingots of the root's alloy.toml, replacing any that
    /// run. A problem with one is a warning in the editor; the others
    /// still load.
    pub(crate) fn load_ingots(&self) {
        let root = self.state.lock().expect("state").root.clone();
        let Some(root) = root else {
            return;
        };
        let config =
            Config::find_within(&root, &root).and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let Some((path, config)) = config else {
            self.state.lock().expect("state").ingots = None;

            return;
        };

        if config.ingots.is_empty() {
            self.state.lock().expect("state").ingots = None;

            return;
        }

        let base = path.parent().unwrap_or(&root);
        let ingots = alloy::ingot::Ingots::load(base, &config);

        for p in &ingots.problems {
            log::warn(&p.to_string());
            self.to_client(&json!({
                "jsonrpc": "2.0",
                "method": "window/showMessage",
                "params": { "type": 2, "message": p.to_string() },
            }));
        }

        log::info(&format!("{} ingots running", ingots.list.len()));
        self.state.lock().expect("state").ingots = Some(std::sync::Arc::new(ingots));
    }

    /// A hover an ingot answers; false when none does.
    pub(crate) fn ingot_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(ingots) = st.ingots.clone() else {
            return false;
        };
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };
        let Some(path) = uri_to_path(uri) else {
            return false;
        };
        let Some(hover) = ingots.hover(&path.to_string_lossy(), &doc.source, offset as u32) else {
            return false;
        };
        let result = crate::ingots::hover(doc, &hover);
        drop(st);
        self.respond(id, result);

        true
    }

    /// Whether a completion request came from a quote that closed a
    /// string: the quote is a trigger character for a require path, and
    /// the one that ends the string is the same key.
    /// Whether the cursor writes the name of a new declaration, `enum
    /// Col|`: the name is the author's to choose, so no list fits. Luau
    /// answers a binding name with nothing for the same reason; the
    /// shadow's shape differs for an Alloy declaration, so the source
    /// decides.
    pub(crate) fn names_a_declaration(&self, uri: &str, message: &Value) -> bool {
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

        declares_a_name_at(&doc.source, offset)
    }

    pub(crate) fn closes_a_string(&self, uri: &str, message: &Value) -> bool {
        let Some(trigger) = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
        else {
            return false;
        };

        if !matches!(trigger, "\"" | "'" | "`") {
            return false;
        }

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
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let before = &doc.source[line_start..offset];
        let quote = trigger.chars().next().unwrap_or('"');
        let mut open = false;
        let mut chars = before.chars();

        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
            } else if c == quote {
                open = !open;
            }
        }

        // An even count: the quote just typed closed the string.
        !open
    }

    /// The completion items an ingot offers at a position, when it has
    /// any: the list is the ingot's alone, since it owns that spot.
    pub(crate) fn ingot_completion(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let trigger = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let st = self.state.lock().expect("state");
        let Some((mut items, completed)) = st.ingot_items(uri, line, character, trigger.as_deref())
        else {
            return false;
        };

        if items.is_empty() {
            return false;
        }

        // The ingot asked for the markup list too: an HTML tag slot also
        // takes the components, and its attributes the properties of the
        // class the tag becomes.
        if completed.merge
            && let Some(offset) = st
                .docs
                .get(uri)
                .and_then(|d| offset_of(&d.source, line, character))
            && let Some(host) = st.markup_completion(uri, offset, completed.class.as_deref())
        {
            items = merged(items, host, completed.hide_roblox);
        }

        drop(st);
        let result = match completed.incomplete {
            true => json!({ "isIncomplete": true, "items": items }),

            false => json!(items),
        };
        self.respond(id, result);

        true
    }

    /// The labels an ingot offers for a color the editor picked, when
    /// the range is one the ingot colored; else the child answers.
    pub(crate) fn ingot_presentation(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some(range) = message.pointer("/params/range").and_then(range_of) else {
            return false;
        };
        let Some(color) = message.pointer("/params/color").cloned() else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(ingots) = st.ingots.clone() else {
            return false;
        };
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let (Some(start), Some(end)) = (
            offset_of(&doc.source, range.0.0, range.0.1),
            offset_of(&doc.source, range.1.0, range.1.1),
        ) else {
            return false;
        };
        let Some(path) = uri_to_path(uri) else {
            return false;
        };
        let labels = ingots.present(
            &path.to_string_lossy(),
            &doc.source,
            (start as u32, end as u32),
            &color,
        );
        if labels.is_empty() {
            return false;
        }

        let range_value = message
            .pointer("/params/range")
            .cloned()
            .unwrap_or(Value::Null);
        let result: Vec<Value> = labels
            .iter()
            .map(|l| json!({ "label": l, "textEdit": { "range": range_value, "newText": l } }))
            .collect();
        drop(st);
        self.respond(id, json!(result));

        true
    }
}

/// The ingot's items, then the host's markup items it does not already
/// hold. `hide_roblox` leaves out the Roblox classes and their properties
/// and events.
fn merged(mut items: Vec<Value>, host: Vec<Value>, hide_roblox: bool) -> Vec<Value> {
    let roblox = |item: &Value| {
        let detail = item["detail"].as_str().unwrap_or_default();

        detail == "Roblox class"
            || detail.starts_with("property of ")
            || detail.starts_with("event of ")
    };
    let taken: HashSet<String> = items
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect();

    items.extend(host.into_iter().filter(|item| {
        !(hide_roblox && roblox(item)) && !item["label"].as_str().is_some_and(|l| taken.contains(l))
    }));

    items
}

#[cfg(test)]
mod merge_tests {
    use serde_json::json;

    #[test]
    fn the_host_list_follows_and_can_hide_roblox() {
        let items = vec![json!({ "label": "div" }), json!({ "label": "key" })];
        let host = vec![
            json!({ "label": "Frame", "detail": "Roblox class" }),
            json!({ "label": "Card", "detail": "component" }),
            json!({ "label": "Size", "detail": "property of Frame" }),
            json!({ "label": "Activated", "detail": "event of TextButton" }),
            json!({ "label": "key", "detail": "prop of the markup" }),
        ];
        let labels = |v: Vec<serde_json::Value>| {
            v.iter()
                .map(|i| i["label"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            labels(super::merged(items.clone(), host.clone(), false)),
            ["div", "key", "Frame", "Card", "Size", "Activated"]
        );
        assert_eq!(
            labels(super::merged(items, host, true)),
            ["div", "key", "Card"]
        );
    }
}
