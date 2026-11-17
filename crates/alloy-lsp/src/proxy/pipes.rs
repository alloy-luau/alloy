//! The pipe operator in the editor: what a hover and a report call the
//! stage local, and where the hint of a stage stands.
//!
//! The emit names each stage `__pipeN`. The author wrote `_`, so every
//! text the editor shows spells it that way.

use super::*;

/// `text` with each stage local spelled `_`: `__pipe12` reads `_`.
pub(crate) fn show_topic(text: &str) -> String {
    const NAME: &str = "__pipe";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(at) = rest.find(NAME) {
        let after = &rest[at + NAME.len()..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        // A longer name that only opens with these letters is not a
        // stage local.
        let whole = digits > 0
            && !rest[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
            && !after[digits..].starts_with(|c: char| c.is_alphanumeric() || c == '_');

        out.push_str(&rest[..at]);

        match whole {
            true => out.push('_'),

            false => out.push_str(&rest[at..at + NAME.len() + digits]),
        }

        rest = &after[digits..];
    }

    out.push_str(rest);

    out
}

/// Where the type hint of a stage local stands in the source: right
/// after the stage, in front of the `|>` that passes its value on. The
/// child puts the hint after the name, which the source does not write.
pub(crate) fn stage_hint_position(doc: &Doc, hint: &Value) -> Option<(u32, u32)> {
    let (line, character) = hint.get("position").and_then(position_of_value)?;
    let at = offset_of(&doc.shadow, line, character)?;
    let head = &doc.shadow[..at];
    let name_start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    let name = &head[name_start..];

    if !head[..name_start].ends_with("local ") {
        return None;
    }

    let out = doc.mapping()?;
    let (op, _) = out.pipe_ops.iter().find(|(_, n)| n == name)?;
    let stage_end = doc.source.get(..*op as usize)?.trim_end().len();
    // A comment after the stage stays behind the hint.
    let line_start = doc.source[..stage_end].rfind('\n').map_or(0, |i| i + 1);
    let stage_end = match doc.source[line_start..stage_end].find("--") {
        Some(i) => line_start + doc.source[line_start..line_start + i].trim_end().len(),

        None => stage_end,
    };

    Some(position_of(&doc.source, stage_end))
}

/// Whether the caret sits in the body of a pipe: a `|>` stands before
/// it on its line.
///
/// ponytail: a body that runs past its first line, such as a function
/// literal, is not seen. Read the pipe off the parse when that matters.
fn in_pipe_body(src: &str, offset: usize) -> bool {
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);

    context::code_of(&src[line_start..offset]).contains("|>")
}

impl State {
    /// Adds `_` to a completion list in the body of a pipe. The name is
    /// no local of the shadow, so the child never lists it.
    pub(crate) fn topic_item(&self, uri: &str, line: u32, character: u32, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return;
        };
        let word = imports::word_before(&doc.source, offset);
        let head = &doc.source[..offset - word.len()];

        // After a `.` or a `:` the list holds members, and a word that
        // does not open with `_` filters the name out anyway.
        if !in_pipe_body(&doc.source, offset)
            || head.ends_with(['.', ':'])
            || !"_".starts_with(word.as_str()) && !word.is_empty()
        {
            return;
        }

        let Some(items) = (match result {
            Value::Array(items) => Some(items),

            _ => result.get_mut("items").and_then(Value::as_array_mut),
        }) else {
            return;
        };

        if items.iter().any(|i| i["label"] == "_") {
            return;
        }

        items.push(json!({
            "label": "_",
            "kind": 6,
            "detail": "the piped value",
            "sortText": "0_",
            "documentation": {
                "kind": "markdown",
                "value": "The value on the left of this `|>`.",
            },
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::show_topic;

    #[test]
    fn a_stage_local_reads_as_the_topic() {
        assert_eq!(show_topic("local __pipe1: number"), "local _: number");
        assert_eq!(
            show_topic("Type '__pipe12' and __pipe3."),
            "Type '_' and _."
        );
        assert_eq!(
            show_topic("my__pipe1 __pipes __pipe"),
            "my__pipe1 __pipes __pipe"
        );
    }
}
