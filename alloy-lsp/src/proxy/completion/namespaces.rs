//! Completion inside and around a `namespace`.
//!
//! The emit gives each member a name of its own, `Math_Vec2`, so the
//! child offers that name. Inside the namespace the reader wrote
//! `Vec2`, and outside the name is not theirs at all: the list takes
//! the first spelling and drops the second.

use super::*;

impl State {
    /// The child's list, read the way the source writes it: a member
    /// of the namespace the caret sits in loses its prefix, and a
    /// member of any other namespace leaves the list.
    pub(crate) fn mark_namespaces(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };

        if doc.namespace_ranges.is_empty() {
            return;
        }

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return;
        };
        let items = match result {
            Value::Array(items) => items,

            Value::Object(obj) => match obj.get_mut("items").and_then(Value::as_array_mut) {
                Some(items) => items,

                None => return,
            },

            _ => return,
        };
        // `Math.` in front of the caret: the child already answers off
        // the table, and the docs come from the declaration.
        let dotted = namespace_before(&doc.source, offset);

        if let Some(path) = &dotted {
            let decls: Vec<&alloy::declarations::Declaration> = doc
                .decls
                .iter()
                .filter(|d| d.name.starts_with(&format!("{path}.")))
                .collect();

            for item in items.iter_mut() {
                let Some(label) = item["label"].as_str().map(str::to_string) else {
                    continue;
                };
                let full = format!("{path}.{label}");

                if let Some(d) = decls.iter().find(|d| d.name == full) {
                    item["documentation"] = json!({ "kind": "markdown", "value": d.hover });
                }
            }

            return;
        }

        // The namespaces the caret sits inside, innermost last.
        let inside: Vec<&alloy::declarations::NamespaceSpan> = doc
            .namespace_ranges
            .iter()
            .filter(|n| offset >= n.start && offset <= n.end)
            .collect();
        // Every rendered member name, with the label the reader wrote
        // when the caret can see it bare.
        let mut shown: Vec<(String, String)> = Vec::new();
        let mut hidden: Vec<String> = Vec::new();

        for ns in &doc.namespace_ranges {
            let prefix = ns.path.replace('.', "_");

            for (member, _) in &ns.members {
                let rendered = format!("{prefix}_{member}");

                match inside.iter().any(|n| n.path == ns.path) {
                    true => shown.push((rendered, member.clone())),

                    false => hidden.push(rendered),
                }
            }
        }

        items.retain(|item| {
            item["label"]
                .as_str()
                .is_none_or(|label| !hidden.iter().any(|h| h == label))
        });

        for item in items.iter_mut() {
            let Some(label) = item["label"].as_str().map(str::to_string) else {
                continue;
            };
            let Some((_, member)) = shown.iter().find(|(r, _)| *r == label) else {
                continue;
            };
            item["label"] = json!(member);

            if item.get("textEdit").is_some() {
                item["textEdit"]["newText"] = json!(member);
            } else {
                item["insertText"] = json!(member);
            }

            let full = doc
                .namespace_ranges
                .iter()
                .filter(|n| inside.iter().any(|i| i.path == n.path))
                .map(|n| format!("{}.{member}", n.path))
                .find(|name| doc.decls.iter().any(|d| d.name == *name));

            if let Some(d) = full.and_then(|name| doc.decls.iter().find(|d| d.name == name)) {
                item["documentation"] = json!({ "kind": "markdown", "value": d.hover });
            }
        }
    }
}

impl State {
    /// The public types of one namespace, as its declarations.
    pub(crate) fn namespace_types(
        &self,
        uri: &str,
        path: &str,
    ) -> Vec<&alloy::declarations::Declaration> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let inside = self.docs.get(uri).and_then(|d| {
            d.namespace_ranges
                .iter()
                .find(|n| n.path == path)
                .map(|n| n.members.clone())
        });
        let public = |member: &str| match &inside {
            Some(members) => !members
                .iter()
                .any(|(name, private)| name == member && *private),

            None => true,
        };
        let head = format!("{path}.");
        let mut out = Vec::new();

        for d in doc.decls.iter().chain(
            self.docs
                .iter()
                .filter(|(u, _)| u.as_str() != uri)
                .flat_map(|(_, d)| d.decls.iter()),
        ) {
            let Some(member) = d.name.strip_prefix(&head) else {
                continue;
            };

            if member.contains('.') || !public(member) {
                continue;
            }

            let line = d.hover.lines().nth(1).unwrap_or("");
            let is_type = ["struct ", "enum ", "trait ", "interface ", "type "]
                .iter()
                .any(|w| line.contains(w));

            if is_type
                && !out
                    .iter()
                    .any(|x: &&alloy::declarations::Declaration| x.name == d.name)
            {
                out.push(d);
            }
        }

        out
    }
}

/// The namespace path a `.` in front of the caret names, when one
/// does: `Math.|` and `Outer.Inner.|`.
pub(crate) fn namespace_before(source: &str, offset: usize) -> Option<String> {
    let head = source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    let before = head.strip_suffix('.')?;
    let path: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    (!path.is_empty()).then_some(path)
}
