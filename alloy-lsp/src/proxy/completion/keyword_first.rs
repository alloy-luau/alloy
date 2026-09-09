use super::*;

impl State {
    /// The keyword wins while the typed word begins one.
    ///
    /// `end` in `if x then return end`, and in a one-line `struct T as
    /// end`, drew `EncodingService` from the child's auto-imports: the
    /// editor matched the letters and sorted the module first. So while
    /// the word begins a keyword the list drops every auto-import and
    /// every label the word does not begin, holds the keywords the word
    /// begins, and an exact keyword takes the first row.
    pub(crate) fn keyword_first(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return;
        };

        // A member names what a value has, and a string holds no word.
        if member_position(doc, line, character).is_some()
            || context::in_string(&doc.source, offset)
        {
            return;
        }

        let word = imports::word_before(&doc.source, offset);
        let matches = keywords::starting_with(&word);

        if matches.is_empty() {
            return;
        }

        let items = match result {
            Value::Array(v) => v,

            Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
                Some(v) => v,

                None => return,
            },

            _ => return,
        };
        items.retain(|i| {
            let label = i.get("label").and_then(Value::as_str).unwrap_or_default();

            !is_auto_import(i) && label.starts_with(word.as_str())
        });

        for keyword in &matches {
            if !items
                .iter()
                .any(|i| i.get("label").and_then(Value::as_str) == Some(*keyword))
            {
                items.push(json!({
                    "label": keyword,
                    "kind": 14,
                    "detail": "Alloy keyword",
                    "sortText": format!("0{keyword}"),
                }));
            }
        }

        if !keywords::is_keyword(&word) {
            return;
        }

        for item in items.iter_mut() {
            if item.get("label").and_then(Value::as_str) == Some(word.as_str()) {
                item["preselect"] = json!(true);
                item["filterText"] = json!(word.clone());
                item["sortText"] = json!(format!("!{word}"));
            }
        }

        // The word is the whole keyword and the list holds it alone. An
        // item the reader has already typed asks for an accept and
        // writes nothing, so the empty list closes the popup instead.
        if items.len() == 1 {
            items.clear();
        }
    }
}
