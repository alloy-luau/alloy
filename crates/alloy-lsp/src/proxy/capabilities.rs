//! Capability edits: what the child answers to initialize, reworked for what the proxy can give and the child cannot.

use super::*;

pub(crate) fn map_range_value(value: &mut Value, doc: &Doc) {
    if let Some(((sl, sc), (el, ec))) = range_of(value) {
        let start = doc.to_source(sl, sc);
        // The end is exclusive: map the last byte inside the range and
        // step past it, so an end in generated text does not fall back
        // to the anchor before the start.
        let end = if (el, ec) > (sl, sc) && ec > 0 {
            let (l, c) = doc.to_source(el, ec - 1);

            (l, c + 1)
        } else {
            doc.to_source(el, ec)
        };
        let end = if end < start { start } else { end };
        let end = spelled_end(doc, (sl, sc), (el, ec), start)
            .or_else(|| macro_call_end(doc, (sl, sc), start))
            .unwrap_or(end);
        *value = range_value(start, end);
    }
}

/// The end of a macro call a range in its expansion maps to. The
/// expansion is generated text anchored at the `$`, so a report inside
/// it would underline the sigil alone; the whole call is what it reads.
fn macro_call_end(doc: &Doc, (sl, sc): (u32, u32), start: (u32, u32)) -> Option<(u32, u32)> {
    if !doc.generated_at(sl, sc) {
        return None;
    }

    let at = offset_of(&doc.source, start.0, start.1)?;
    let rest = doc.source[at..].strip_prefix('$')?;
    let name = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(rest.len());
    let open = at + 1 + name;

    if name == 0 || !doc.source[open..].starts_with('(') {
        return None;
    }

    let mut depth = 0i32;
    let mut quote: Option<char> = None;

    for (i, c) in doc.source[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,

            (Some(_), _) => {}

            (None, '"' | '\'' | '`') => quote = Some(c),

            (None, '(' | '[' | '{') => depth += 1,

            (None, ')' | ']' | '}') => {
                depth -= 1;

                if depth == 0 {
                    return Some(position_of(&doc.source, open + i + 1));
                }
            }

            _ => {}
        }
    }

    None
}

/// The end of a generated name the source spells at its anchor:
/// `function Status.describe` is generated text anchored at the
/// `describe` the author wrote, so the range keeps the name's width.
/// `None` for a range the map already reads whole.
fn spelled_end(
    doc: &Doc,
    (sl, sc): (u32, u32),
    (el, ec): (u32, u32),
    start: (u32, u32),
) -> Option<(u32, u32)> {
    if sl != el || !doc.generated_at(sl, sc) {
        return None;
    }

    let text = &doc.shadow[offset_of(&doc.shadow, sl, sc)?..offset_of(&doc.shadow, el, ec)?];

    if text.is_empty() || !text.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    let at = offset_of(&doc.source, start.0, start.1)?;

    doc.source[at..]
        .starts_with(text)
        .then(|| position_of(&doc.source, at + text.len()))
}

/// The child's capabilities, as the editor should see them: no
/// formatting of a shadow, semantic tokens whole and never by range or
/// delta, and rename follow-up for Alloy files and the Luau modules
/// they import.
pub(crate) fn edit_capabilities(message: &mut Value) {
    let Some(caps) = message
        .pointer_mut("/result/capabilities")
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    let child_on_type = caps.remove("documentOnTypeFormattingProvider");

    for key in [
        "documentFormattingProvider",
        "documentRangeFormattingProvider",
    ] {
        caps.remove(key);
    }

    // The proxy formats `.aly` itself, with `alloy fmt`.
    caps.insert("documentFormattingProvider".into(), Value::Bool(true));

    // The proxy publishes every report itself, for the file the child
    // answered and for its importers. A client that also pulls shows
    // each report twice, so the pull capability goes; a pull that
    // still arrives is answered with the same list.
    caps.remove("diagnosticProvider");

    // No range formatting. `alloy fmt` reads a whole file and rewraps to
    // `[fmt] column_width`, so it can move text across the edge of a
    // selection: an edit set cut to the range would either drop that
    // rewrap or write outside what the editor asked for. The capability
    // stays off, so no editor offers "Format Selection", and
    // `textDocument/formatting` does the work.

    // On-type formatting writes the `end` of a block after Enter: the
    // newline first, then whatever the child asked for, so a trigger of
    // its own still arrives.
    let mut more: Vec<Value> = Vec::new();

    if let Some(child) = &child_on_type {
        let first = child.get("firstTriggerCharacter").into_iter();
        let rest = child
            .get("moreTriggerCharacter")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        for c in first.chain(rest) {
            if c != "\n" && !more.contains(c) {
                more.push(c.clone());
            }
        }
    }

    caps.insert(
        "documentOnTypeFormattingProvider".into(),
        json!({ "firstTriggerCharacter": "\n", "moreTriggerCharacter": more }),
    );

    // The lints' rewrites are code actions, whatever the child offers.
    let kinds = json!({ "codeActionKinds": ["quickfix", "source.fixAll"] });
    match caps.get_mut("codeActionProvider") {
        Some(Value::Object(existing)) => {
            existing.insert("codeActionKinds".into(), kinds["codeActionKinds"].clone());
        }

        _ => {
            caps.insert("codeActionProvider".into(), kinds);
        }
    }

    if let Some(Value::Object(tokens)) = caps.get_mut("semanticTokensProvider") {
        tokens.remove("range");
        tokens.insert("full".to_string(), Value::Bool(true));
    }

    // `@` and `$` open an attribute and a macro or intrinsic, `(` an
    // attribute's arguments, `{` the field list of an object
    // initializer, and a space the side of a directive: the editor asks
    // on them only when the server lists them.
    if let Some(Value::Object(completion)) = caps.get_mut("completionProvider") {
        let list = completion
            .entry("triggerCharacters")
            .or_insert_with(|| json!([]));

        if let Some(chars) = list.as_array_mut() {
            for c in ["@", "$", "(", " ", "{"] {
                if !chars.iter().any(|v| v == c) {
                    chars.push(json!(c));
                }
            }
        }
    }

    let workspace = caps.entry("workspace").or_insert_with(|| json!({}));

    if let Some(w) = workspace.as_object_mut() {
        w.insert(
            "fileOperations".to_string(),
            json!({
                "didRename": {
                    "filters": [
                        { "pattern": { "glob": "**/*.{aly,alx,luau,lua}", "matches": "file" } },
                        { "pattern": { "glob": "**", "matches": "folder" } }
                    ]
                }
            }),
        );
    }
}
