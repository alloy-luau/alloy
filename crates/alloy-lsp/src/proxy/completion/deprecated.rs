//! What a completion list marks deprecated, and what it leaves out.
//!
//! The child marks its own rows: luau-lsp reads `@deprecated` in the
//! definitions and sends `deprecated: true`. The proxy assembles lists
//! of its own, so those rows carry the mark too, as the `tags` the
//! editor strikes through. Two editor settings then drop the marked
//! rows: `hideRobloxDeprecated` for the engine's own members, and
//! `hideAllDeprecated` for what the source marks as well.

use super::*;

/// `CompletionItemTag.Deprecated`.
const DEPRECATED: u64 = 1;

impl State {
    /// Marks every deprecated row of a completion answer, then drops
    /// the rows the editor's settings hide. The list holds the child's
    /// rows and the proxy's own by now, so both read alike.
    pub(crate) fn deprecated_pass(&self, uri: &str, result: &mut Value) {
        // The file's own marks, and those of the modules it imports:
        // `import { oldFn } from "./dep"` reads the attribute in `dep`.
        let own = match self.docs.get(uri) {
            Some(doc) => std::iter::once(doc.source.as_str())
                .chain(doc.import_sources.iter().map(String::as_str))
                .flat_map(deprecated_names)
                .collect(),

            None => HashSet::new(),
        };
        let Some(items) = items_of(result) else {
            return;
        };

        for item in items.iter_mut() {
            if is_deprecated(item, &own) {
                mark(item);
            }
        }

        let hide_all = self.editor.hide_all_deprecated;

        if !hide_all && !self.editor.hide_roblox_deprecated {
            return;
        }

        // A name the source declares is the author's own, whatever the
        // engine calls it: `impl BasePart as function destroy(self)`
        // stays in the list where `destroy` is a deprecated member.
        let declared = self.declared_names(uri);
        let engine = self.roblox_deprecated_names();

        items.retain(|item| {
            if !marked(item) {
                return true;
            }

            let label = label_of(item);

            if declared.contains(label) && !own.contains(label) {
                return true;
            }

            match hide_all {
                true => false,

                false => !engine.contains(label) && !alloy::luaux::roblox::is_deprecated(label),
            }
        });
    }

    /// The names the workspace declares itself: the extension methods
    /// on foreign types, and the declarations of the file.
    fn declared_names(&self, uri: &str) -> HashSet<String> {
        let mut out: HashSet<String> = self
            .extensions
            .iter()
            .map(|e| e.name.clone())
            .collect::<HashSet<String>>();

        if let Some(doc) = self.docs.get(uri) {
            for d in &doc.decls {
                // A namespace member is indexed as `Old.f` and as
                // `Old_f`; the list writes the member alone.
                if let Some((_, member)) = d.name.rsplit_once('.') {
                    out.insert(member.to_string());
                }

                out.insert(d.name.clone());
            }
        }

        out
    }
}

/// The rows of a completion answer, whether it came as a list or as a
/// list with the incomplete flag.
fn items_of(result: &mut Value) -> Option<&mut Vec<Value>> {
    match result {
        Value::Array(items) => Some(items),

        Value::Object(o) => o.get_mut("items").and_then(Value::as_array_mut),

        _ => None,
    }
}

fn label_of(item: &Value) -> &str {
    item.get("label")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The documentation text of a row, however the sender wrote it.
fn documentation(item: &Value) -> &str {
    item.get("documentation")
        .and_then(Value::as_str)
        .or_else(|| item.pointer("/documentation/value").and_then(Value::as_str))
        .unwrap_or_default()
}

/// Whether a row already carries the tag.
fn marked(item: &Value) -> bool {
    item.get("tags")
        .and_then(Value::as_array)
        .is_some_and(|tags| tags.iter().any(|t| t.as_u64() == Some(DEPRECATED)))
}

/// Writes the tag the editor strikes the row through with. The older
/// `deprecated` flag the child sends stays where it is: a client that
/// reads one reads the row the same way.
fn mark(item: &mut Value) {
    match item.get_mut("tags").and_then(Value::as_array_mut) {
        Some(tags) => tags.push(json!(DEPRECATED)),

        None => item["tags"] = json!([DEPRECATED]),
    }
}

/// Whether a row is deprecated: the child said so, the documentation
/// opens with the note, or the source marks the name.
fn is_deprecated(item: &Value, own: &HashSet<String>) -> bool {
    if marked(item) || item.get("deprecated").and_then(Value::as_bool) == Some(true) {
        return true;
    }

    says_deprecated(documentation(item)) || own.contains(label_of(item))
}

/// Whether a documentation text opens with the deprecation note. The
/// engine writes `<strong>Deprecated:</strong>`, which the proxy shows
/// as `Deprecated:`; a namespace hover writes `**Deprecated.**`.
pub(crate) fn says_deprecated(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line
            .trim_start()
            .trim_start_matches("<strong>")
            .trim_start_matches("**");

        line.starts_with("Deprecated:") || line.starts_with("Deprecated.")
    })
}

/// The names a source marks `@deprecated`. The attribute goes on a
/// function or a namespace, so the declaration under it names the row
/// the list has to mark.
pub(crate) fn deprecated_names(src: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut pending = false;

    for line in src.lines() {
        let text = line.trim();

        if text.is_empty() || text.starts_with("--") {
            continue;
        }

        if let Some(rest) = text.strip_prefix("@deprecated") {
            // `@deprecated("use Geometry")` carries a message, and the
            // declaration may follow on the same line.
            let rest = match rest.starts_with('(') {
                true => rest.split_once(')').map(|(_, r)| r).unwrap_or(""),

                false => rest,
            };

            match declared_name(rest.trim()) {
                Some(name) => {
                    out.insert(name);
                }

                None => pending = true,
            }

            continue;
        }

        // Another attribute of the same stack.
        if text.starts_with('@') {
            continue;
        }

        if pending {
            if let Some(name) = declared_name(text) {
                out.insert(name);
            }

            pending = false;
        }
    }

    out
}

/// The name a declaration line declares: the word after `function` or
/// `namespace`, without the path an impl method writes in front of it.
fn declared_name(line: &str) -> Option<String> {
    let mut words = line.split_whitespace().peekable();

    while let Some(word) = words.peek() {
        if matches!(
            *word,
            "export" | "global" | "local" | "public" | "private" | "async" | "open"
        ) {
            words.next();

            continue;
        }

        break;
    }

    let word = words.next()?;

    if !matches!(word, "function" | "namespace") {
        return None;
    }

    let rest = words.next()?;
    let path: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == ':')
        .collect();
    let name = path.rsplit(['.', ':']).next().unwrap_or_default();

    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_at_the_head_of_the_docs_says_deprecated() {
        assert!(says_deprecated("<strong>Deprecated:</strong> use `Color`"));
        assert!(says_deprecated("Deprecated: use `Color`"));
        assert!(says_deprecated(
            "```alloy\nnamespace Old as\nend\n```\n\n**Deprecated.** use `Geometry`"
        ));
        assert!(!says_deprecated("Returns the deprecated flag of a part."));
        assert!(!says_deprecated(""));
    }

    #[test]
    fn the_attribute_names_the_declaration_under_it() {
        let src = concat!(
            "@deprecated(\"use fresh\")\n",
            "local function old(): number\n",
            "    return 1\n",
            "end\n",
            "\n",
            "local function fresh() end\n",
            "\n",
            "@deprecated\n",
            "@native\n",
            "export function past() end\n",
            "\n",
            "@deprecated(\"use Geometry\")\n",
            "namespace Old as\n",
            "    @deprecated\n",
            "    public function f() end\n",
            "end\n",
            "\n",
            "impl Box as\n",
            "    @deprecated\n",
            "    public function Box.width(self) end\n",
            "end\n",
        );
        let mut names: Vec<String> = deprecated_names(src).into_iter().collect();
        names.sort();

        assert_eq!(names, ["Old", "f", "old", "past", "width"]);
    }
}
