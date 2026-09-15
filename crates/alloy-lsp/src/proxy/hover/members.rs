use super::*;

/// The std type a receiver word names, with whether the word is the
/// type itself. A value resolves through its annotation, or through
/// what it starts from.
pub(crate) fn std_receiver(source: &str, sigil: usize, word: &str) -> Option<(&'static str, bool)> {
    if word.is_empty() {
        return None;
    }

    if let Some(key) = alloy::docs::member_owner(word) {
        return Some((key, true));
    }

    let base = match context::declared(source, sigil, word)? {
        context::Declared::Annotation(t) => alloy::docs::type_head(&t),
        context::Declared::Init(v) => alloy::docs::value_head(&v),
    }?;

    alloy::docs::member_owner(&base).map(|key| (key, false))
}

/// The std member the byte sits on: the word after a `.` or a `:` whose
/// receiver resolves to a std type that documents it.
pub(crate) fn std_member_at(
    source: &str,
    offset: usize,
) -> Option<(&'static str, &'static alloy::docs::Member)> {
    let (name, sigil, receiver) = alloy::docs::member_spot(source, offset)?;
    let (key, on_type) = std_receiver(source, sigil, receiver)?;
    let m = alloy::docs::member(key, name)?;

    alloy::docs::member_fits(m.kind, on_type).then_some((key, m))
}

/// The std member a hover sits on, as the doc and the example the
/// checker's type cannot carry. The source resolves the receiver where
/// it can; otherwise the type the child printed names it.
pub(crate) fn std_member_hover(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;
    let hit = std_member_at(&doc.source, offset).or_else(|| {
        let (name, _, _) = alloy::docs::member_spot(&doc.source, offset)?;

        alloy::docs::MEMBERS
            .iter()
            .filter(|(key, _)| names_type(value, key))
            .find_map(|(key, _)| alloy::docs::member(key, name).map(|m| (*key, m)))
    })?;

    Some(alloy::docs::member_hover(hit.0, hit.1))
}

/// Whether a printed type names `key` as a whole word.
pub(crate) fn names_type(text: &str, key: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(key) {
        let start = from + i;
        let end = start + key.len();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

        if !(start > 0 && word(bytes[start - 1])) && !(end < bytes.len() && word(bytes[end])) {
            return true;
        }

        from = start + 1;
    }

    false
}

/// The child's member list, with the std's doc on the items a std type
/// declares. A completion item carries the type; the doc says what the
/// member does.
pub(crate) fn attach_std_member_docs(result: &mut Value, doc: &Doc, line: u32, character: u32) {
    let Some(offset) = offset_of(&doc.source, line, character) else {
        return;
    };
    let head = doc.source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    if !head.ends_with(['.', ':']) {
        return;
    }

    let sigil = head.len() - 1;
    let from = head[..sigil]
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let Some((key, on_type)) = std_receiver(&doc.source, sigil, &head[from..sigil]) else {
        return;
    };
    let items = match result.get_mut("items").and_then(Value::as_array_mut) {
        Some(items) => items,

        None => match result.as_array_mut() {
            Some(items) => items,

            None => return,
        },
    };

    for item in items {
        let Some(label) = item.get("label").and_then(Value::as_str) else {
            continue;
        };
        let Some(m) = alloy::docs::member(key, label) else {
            continue;
        };
        if !alloy::docs::member_fits(m.kind, on_type) {
            continue;
        }

        item["detail"] = json!(m.signature);
        item["documentation"] = json!({
            "kind": "markdown",
            "value": alloy::docs::member_hover(key, m),
        });
    }
}
