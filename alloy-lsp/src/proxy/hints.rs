//! Inlay hints: the folds that clean and filter what the child offers.

use super::hover::source_type;
use super::*;

/// The text of a hint label, whether a string or parts.
pub(crate) fn hint_label(hint: &Value) -> String {
    match hint.get("label") {
        Some(Value::String(s)) => s.clone(),

        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("value").and_then(Value::as_str))
            .collect(),

        _ => String::new(),
    }
}

/// The hints as the source can hold them. A parameter hint that names
/// an emit slot goes. A type hint loses `@checked`, reads by the name
/// its line gives when the print is unwritable, and inserts nothing
/// when no name is at hand.
pub(crate) fn clean_hints(hints: &mut Vec<Value>, doc: &Doc) {
    hints.retain(|h| !emit_slot_hint(h));

    let parameters = declared_type_parameters(&doc.source);

    for h in hints.iter_mut() {
        // The label and the edit are one text.
        if let Some(text) = h
            .pointer("/textEdits/0/newText")
            .and_then(Value::as_str)
            .map(str::to_string)
            && hint_label(h).starts_with(':')
        {
            h["label"] = json!(text);
        }

        let label = hint_label(h);

        if !label.starts_with(": ") {
            continue;
        }

        // `@checked` is an emit attribute; a written type carries none.
        let label = label
            .replace("@checked ", "")
            .replace("@native ", "")
            .replace("@checked", "");
        let annotation = label[2..].trim();
        let named = writable_type(annotation) && !undeclared_variable(annotation, &parameters);
        let position = h.get("position").and_then(position_of_value);
        // The child types a method's receiver from its body; the `impl`
        // says what it is.
        let on_self = position.is_some_and(|(l, c)| self_parameter(doc, l, c));
        let from_source = position.and_then(|(l, c)| source_type(doc, l, c));

        let label = match (named && !on_self, from_source) {
            (true, _) => label,

            (false, Some(name)) => format!(": {name}"),

            (false, None) if named => label,

            (false, None) => {
                h.as_object_mut().map(|o| o.remove("textEdits"));
                truncate_hint(h, &label);

                continue;
            }
        };

        h["label"] = json!(label);

        // A generic struct prints without its arguments: Luau names a
        // metatable type and carries none. `: Slotted` would not
        // compile, so the hint reads and inserts nothing.
        if generic_struct(doc, label[2..].trim()) {
            h.as_object_mut().map(|o| o.remove("textEdits"));

            continue;
        }

        if truncate_hint(h, &label) {
            h.as_object_mut().map(|o| o.remove("textEdits"));

            continue;
        }

        if let Some(position) = h.get("position").cloned() {
            h["textEdits"] = json!([{
                "range": { "start": position.clone(), "end": position },
                "newText": label,
            }]);
        }
    }
}

/// Whether a printed type names a struct that takes type parameters
/// and gives it none. Luau prints a struct by its metatable's name, so
/// the arguments are gone, and `Slotted` or `Pair[]` names a type the
/// source cannot write. The name may sit anywhere in the text.
pub(crate) fn generic_struct(doc: &Doc, text: &str) -> bool {
    let generics = |name: &str| {
        std::iter::once(&doc.source)
            .chain(doc.import_sources.iter())
            .any(|src| src.contains(&format!("struct {name}<")))
    };
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at] as char).is_alphanumeric() && bytes[at] != b'_' {
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && ((bytes[at] as char).is_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }

        let word = &text[start..at];

        if !text[at..].starts_with('<') && generics(word) {
            return true;
        }
    }

    false
}

/// A label too long for the gutter shows its head alone. True when the
/// label was cut, so what is left inserts nothing.
pub(crate) fn truncate_hint(h: &mut Value, label: &str) -> bool {
    if label.chars().count() <= 72 {
        return false;
    }

    let head: String = label.chars().take(69).collect();
    h["label"] = json!(format!("{}…", head.trim_end()));

    true
}

/// `_1:` and `_2:` are the payload slots of a tagged enum. The source
/// names neither, so neither belongs in the gutter.
pub(crate) fn emit_slot_hint(h: &Value) -> bool {
    let label = hint_label(h);
    let name = label.trim_end_matches(':');

    name.len() > 1 && name.starts_with('_') && name[1..].chars().all(|c| c.is_ascii_digit())
}

/// A type a source line can hold: no attribute, no solver clause, no
/// emit-only name, and nothing the child cut short.
pub(crate) fn writable_type(text: &str) -> bool {
    !text.contains('@')
        && !text.contains(" where ")
        && !text.contains('*')
        && !text.contains('…')
        && !text.contains("__")
        && !text.contains("CYCLE")
        // The child cuts a long table with `... N more ...`.
        && !text.contains(" more ...")
        // `_1` and `_2` are the payload slots of a tagged enum.
        && !text.contains("_1:")
        && !text.contains("_2:")
        // `~nil` negates a type; Alloy writes no negation. The checker's
        // own type functions, `intersect<T, ~nil>`, carry it.
        && !text.contains('~')
        && !text.contains("intersect<")
        && !text.contains("union<")
        // A type pack, `...any`, annotates no binding.
        && !text.starts_with("...")
        && !names_a_hidden_field(text)
}

/// Whether a type text names a field the emit made: `_head`, `_next`,
/// `_source`. A std type hides them, so a popup that prints them names
/// what the language does not document.
pub(crate) fn names_a_hidden_field(text: &str) -> bool {
    text.match_indices('_').any(|(i, _)| {
        let opens = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, '{' | ' ' | ',' | '('));
        let name: String = text[i..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        opens && text[i + name.len()..].starts_with(':')
    })
}

/// A solver variable the file never declares, `a` or `T`: no
/// annotation can name it. The whole type may be one, `T[]`, or a
/// member's may be, `{ read value: a }`.
pub(crate) fn undeclared_variable(text: &str, declared: &HashSet<String>) -> bool {
    let short = |name: &str| {
        name.len() <= 2
            && !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric())
            && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && !declared.contains(name)
    };

    if short(text.trim().trim_end_matches('?').trim_end_matches("[]")) {
        return true;
    }

    // Every word of the type: a short one the file never declared is a
    // solver variable wherever it stands. A word a `:` follows is a
    // member's key, and one a `<` follows takes arguments, so it names
    // a type of its own.
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at] as char).is_ascii_alphanumeric() && bytes[at] != b'_' {
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && ((bytes[at] as char).is_ascii_alphanumeric() || bytes[at] == b'_')
        {
            at += 1;
        }

        let after = text[at..].trim_start();

        if short(&text[start..at]) && !after.starts_with('<') && !after.starts_with(':') {
            return true;
        }
    }

    false
}

/// Whether the hint sits right after the `self` parameter of a method.
pub(crate) fn self_parameter(doc: &Doc, line: u32, character: u32) -> bool {
    let Some(text) = doc.source.lines().nth(line as usize) else {
        return false;
    };
    let before: String = text.chars().take(character as usize).collect();

    text.contains("function ") && before.trim_end().ends_with("self")
}

/// A return type hint of `: Future<T>` becomes `: T`, in the label and
/// in the edit that inserts it, since `async function f(): T` is what
/// the source accepts.
pub(crate) fn unwrap_future_hint(hint: &mut Value) {
    pub(crate) fn unwrap(text: &str) -> Option<String> {
        let rest = text.strip_prefix(": ")?;
        let inner = rest
            .strip_prefix("__alloy.Future<")
            .or_else(|| rest.strip_prefix("Future<"))?
            .strip_suffix('>')?;

        Some(format!(": {inner}"))
    }

    if let Some(new) = unwrap(&hint_label(hint)) {
        hint["label"] = json!(new);
    }

    if let Some(edits) = hint.get_mut("textEdits").and_then(Value::as_array_mut) {
        for edit in edits {
            if let Some(text) = edit.get("newText").and_then(Value::as_str)
                && let Some(new) = unwrap(text)
            {
                edit["newText"] = json!(new);
            }
        }
    }
}
