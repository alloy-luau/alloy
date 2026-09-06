//! The ingots in the editor: hover, completion, and code actions from
//! the project's extensions, shaped into LSP values. The host side of
//! the protocol lives in the compiler crate; this file only converts
//! byte spans to positions and joins the answers into the responses.

use serde_json::{Value, json};

use crate::doc::{Doc, position_of};

fn range_of_span(source: &str, span: &Value) -> Option<Value> {
    let a = span.as_array()?;
    let start = a.first()?.as_u64()? as usize;
    let end = a.get(1)?.as_u64()? as usize;
    let (sl, sc) = position_of(source, start.min(source.len()));
    let (el, ec) = position_of(source, end.min(source.len()));

    Some(json!({
        "start": { "line": sl, "character": sc },
        "end": { "line": el, "character": ec },
    }))
}

/// A hover reply as the LSP hover.
pub fn hover(doc: &Doc, hover: &Value) -> Value {
    let mut out = json!({
        "contents": { "kind": "markdown", "value": hover["contents"].as_str().unwrap_or("") },
    });

    if let Some(range) = range_of_span(&doc.source, &hover["span"]) {
        out["range"] = range;
    }

    out
}

/// The LSP number of an item kind word.
fn kind_code(word: &str) -> u64 {
    const KINDS: &[&str] = &[
        "text",
        "method",
        "function",
        "constructor",
        "field",
        "variable",
        "class",
        "interface",
        "module",
        "property",
        "unit",
        "value",
        "enum",
        "keyword",
        "snippet",
        "color",
        "file",
        "reference",
        "folder",
        "enummember",
        "constant",
        "struct",
        "event",
        "operator",
        "typeparameter",
    ];

    KINDS
        .iter()
        .position(|k| *k == word)
        .map(|i| i as u64 + 1)
        .unwrap_or(1)
}

/// Completion items as the LSP shapes them. An item with a span becomes
/// a text edit over it; one without inserts at the cursor.
pub fn completion_items(doc: &Doc, items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .filter_map(|i| {
            let label = i["label"].as_str()?;
            let mut item = json!({ "label": label });

            if let Some(k) = i["kind"].as_str() {
                item["kind"] = json!(kind_code(k));
            }

            if let Some(d) = i["detail"].as_str() {
                item["detail"] = json!(d);
            }

            if let Some(d) = i["documentation"].as_str() {
                // A color item's documentation is its hex value, which the
                // editor draws as a swatch when it is a plain string.
                if i["kind"].as_str() == Some("color") && d.starts_with('#') {
                    item["documentation"] = json!(d);
                } else {
                    item["documentation"] = json!({ "kind": "markdown", "value": d });
                }
            }

            let insert = i["insert"].as_str().unwrap_or(label);
            let snippet = i["snippet"].as_bool() == Some(true);

            if snippet {
                item["insertTextFormat"] = json!(2);
            }

            match range_of_span(&doc.source, &i["span"]) {
                Some(range) => {
                    item["textEdit"] = json!({ "range": range, "newText": insert });
                }

                None if insert != label || snippet => {
                    item["insertText"] = json!(insert);
                }

                None => {}
            }

            Some(item)
        })
        .collect()
}

/// Colors as LSP color information over the source.
pub fn colors(doc: &Doc, colors: &[Value]) -> Vec<Value> {
    colors
        .iter()
        .filter_map(|c| {
            let range = range_of_span(&doc.source, &c["span"])?;

            Some(json!({
                "range": range,
                "color": {
                    "red": c["red"].as_f64().unwrap_or(0.0),
                    "green": c["green"].as_f64().unwrap_or(0.0),
                    "blue": c["blue"].as_f64().unwrap_or(0.0),
                    "alpha": c["alpha"].as_f64().unwrap_or(1.0),
                },
            }))
        })
        .collect()
}

/// Code actions as LSP actions with a workspace edit over the source.
pub fn actions(doc: &Doc, uri: &str, actions: &[Value]) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| {
            let title = a["title"].as_str()?;
            let edits: Vec<Value> = a["edits"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    let arr = e.as_array()?;
                    let range = range_of_span(
                        &doc.source,
                        &json!([arr.first()?.as_u64()?, arr.get(1)?.as_u64()?]),
                    )?;

                    Some(json!({ "range": range, "newText": arr.get(2)?.as_str()? }))
                })
                .collect();
            let mut action = json!({
                "title": title,
                "kind": a["kind"].as_str().unwrap_or("quickfix"),
                "edit": { "changes": { uri: edits } },
            });

            if a["kind"].is_null() {
                action["kind"] = json!("quickfix");
            }

            Some(action)
        })
        .collect()
}

/// The diagnostics of the document that touch a span, for the ingot
/// that offers a fix: the lints and the compiler's messages.
pub fn diagnostics_in(doc: &Doc, span: (u32, u32)) -> Vec<Value> {
    let Some(out) = &doc.output else {
        return Vec::new();
    };
    let touches = |s: u32, e: u32| s <= span.1 && e >= span.0;
    let mut list = Vec::new();

    for l in out.lints.iter().filter(|l| touches(l.start, l.end)) {
        list.push(json!({ "span": [l.start, l.end], "message": l.message, "lint": l.name }));
    }

    for d in out.diagnostics.iter().filter(|d| touches(d.start, d.end)) {
        list.push(json!({ "span": [d.start, d.end], "message": d.message }));
    }

    list
}
