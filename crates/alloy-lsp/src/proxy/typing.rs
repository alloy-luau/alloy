//! On-type formatting: the end an open block writes, and the closing tag after an opener.

use super::*;

impl Server {
    /// `textDocument/formatting`: `alloy fmt` over the open document, as
    /// one edit that replaces the whole text. A file already formatted
    /// gets no edits, and one the parser cannot read keeps its text and
    /// says why.
    pub(crate) fn format_document(&self, uri: &str, id: &Value) {
        let open = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            st.docs.get(uri).map(|d| (d.source.clone(), d.bom))
        };
        let Some((source, bom)) = open else {
            self.respond(id, Value::Null);

            return;
        };

        // The project's own `[fmt]` table, so format on save lays a
        // file out the way `alloy fmt` does. Reading none of it made
        // the editor ignore `indent_width` and the rest.
        let (ingots, options) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

            (st.ingots.clone(), st.fmt_config(uri))
        };
        let formatted =
            format_source(uri, &source, &options).map(|f| match (&ingots, uri_to_path(uri)) {
                (Some(ingots), Some(path)) => ingots.format(&path.to_string_lossy(), &f).0,

                _ => f,
            });

        match formatted {
            Ok(formatted) if formatted != source => {
                let (el, ec) = position_of(&source, source.len());
                // The document dropped a leading byte order mark on the
                // way in, and this edit writes the whole text: the mark
                // goes back where the editor had it.
                let text = match bom {
                    true => format!("{MARK}{formatted}"),

                    false => formatted,
                };
                self.respond(
                    id,
                    json!([{
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": el, "character": ec } },
                        "newText": text,
                    }]),
                );
            }

            Ok(_) => self.respond(id, json!([])),

            // The formatter failed, and an empty edit list looks like a
            // file already laid out. The reader hears the reason
            // instead, the way `alloy fmt` prints it: a file the parser
            // cannot read is skipped, anything else failed.
            Err(reason) => {
                let kind = match reason.starts_with(alloy::fmt::UNPARSED) {
                    true => 2,

                    false => 1,
                };
                self.to_client(&json!({
                    "jsonrpc": "2.0",
                    "method": "window/showMessage",
                    "params": { "type": kind, "message": format!("alloy fmt: {reason}") },
                }));
                self.respond(id, json!([]));
            }
        }
    }

    /// `alloy/closeTag`: the element name the `>` before the position
    /// opens, as `{ "name": "Frame" }`, or null. Markup lives in `.alx`
    /// alone, so no other file answers.
    ///
    /// The editor writes the closing tag itself, as a snippet whose
    /// `$0` holds the caret between the two tags. A text edit carries no
    /// caret, so the insert belongs to the client.
    pub(crate) fn close_tag_name(&self, message: &Value, id: &Value) {
        let uri = message
            .pointer("/params/uri")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| text_document_uri(message))
            .unwrap_or_default();
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let at = position_of_message(message);
        let name = match st.editor.auto_close_tags && uri.ends_with(".alx") {
            true => at
                .zip(st.docs.get(&uri))
                .and_then(|((line, character), doc)| {
                    let offset = offset_of(&doc.source, line, character)?;

                    markup::close_tag(&doc.source, offset)
                }),

            false => None,
        };
        drop(st);

        match name {
            Some(name) => self.respond(id, json!({ "name": name })),

            None => self.respond(id, Value::Null),
        }
    }

    /// `textDocument/onTypeFormatting` with a newline: Enter on a line
    /// that opens a block writes the block's `end` one line below.
    ///
    /// The editor sends the request after its own auto-indent, so the
    /// caret already sits on an indented line of its own. The edit
    /// inserts at the caret, which leaves the caret where it is and puts
    /// the `end` under the opener.
    pub(crate) fn end_after_opener(&self, uri: &str, message: &Value, id: &Value) {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ch = message
            .pointer("/params/ch")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let at = position_of_message(message);
        let edit = match (st.editor.auto_end, ch, at) {
            (true, "\n", Some((line, character))) => st.end_edit(uri, line, character),

            _ => None,
        };
        drop(st);
        self.respond(id, edit.unwrap_or_else(|| json!([])));
    }
}

impl State {
    /// The `end` an open block still wants, as the edit a newline
    /// on-type request answers with.
    ///
    /// The editor already made the indented line under the opener and
    /// left the caret on it. The edit adds the `end` a line below, at
    /// the opener's own indentation. It writes nothing when the caret
    /// line holds text, when the block is closed, or when an `end`
    /// already stands under the opener.
    ///
    /// The child sees the shadow, where `struct`, `trait`, and `match`
    /// are already Luau, so the answer comes from the Alloy source here.
    pub(crate) fn end_edit(&self, uri: &str, line: u32, character: u32) -> Option<Value> {
        // The opener is the line the reader left with Enter.
        let opener = line.checked_sub(1)?;
        let doc = self.docs.get(uri)?;
        let indent = block_end::needs_end(&doc.source, opener)?;
        let offset = offset_of(&doc.source, line, character)?;
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line_end = doc.source[offset..]
            .find('\n')
            .map_or(doc.source.len(), |i| offset + i);
        let blank = |text: &str| text.chars().all(|c| c == ' ' || c == '\t');

        // Enter left the caret on a line of its own indentation. With
        // text on either side the line is the reader's, not ours, and
        // an insert at the caret would cut it in two.
        if !blank(&doc.source[line_start..offset]) || !blank(&doc.source[offset..line_end]) {
            return None;
        }

        if end_follows(&doc.source, offset, &indent) {
            return None;
        }

        Some(json!([{
            "range": range_value((line, character), (line, character)),
            "newText": format!("\n{indent}end"),
        }]))
    }
}

/// The layout of one open document, by the formatter `alloy fmt` reads
/// for its name.
pub(crate) fn format_source(
    uri: &str,
    source: &str,
    options: &alloy::config::FmtConfig,
) -> Result<String, String> {
    alloy::fmt::format_named(uri, source, options)
}

/// Whether an `end` already stands under the opener: the first line
/// with text after `offset` is `end` at `indent`.
pub(crate) fn end_follows(src: &str, offset: usize, indent: &str) -> bool {
    let mut lines = src[offset..].lines();
    lines.next();
    let Some(line) = lines.find(|l| !l.trim().is_empty()) else {
        return false;
    };
    let text = line.trim_start();
    let own = &line[..line.len() - text.len()];
    let word = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default();

    own == indent && word == "end"
}

/// The fields of `local x = new T(...) { ... }`, under the hover of `x`.
///
/// The declaration is the one in scope at the hover. A search by name
/// found the first `bag` of the file, so one function showed the value
/// of a `bag` in another.
pub(crate) fn append_initializer(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let Caret { offset, start, end } = Caret::at(&doc.source, line, character)?;
    let word = &doc.source[start..end];
    let at = alloy::flux::binding_of(&doc.source, start)?;
    let line_start = doc.source[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let head = doc.source[line_start..at].trim();
    let after = &doc.source[at + word.len()..];

    if !matches!(head, "local" | "const" | "export local" | "export const") {
        return None;
    }

    // The `=` of `local q: T` with no value is the next statement's:
    // `q = new T {}` on a later line sets the value, and the
    // declaration holds none.
    let eq = after.find('=').filter(|e| !after[..*e].contains('\n'))?;
    let between = after[..eq].trim_start();

    if !(between.is_empty() || between.starts_with(':')) {
        return None;
    }

    let rhs = after[eq + 1..].trim_start();

    // The fields open on the `new` line; a brace on a later line
    // belongs to another statement.
    let line_end = rhs.find('\n').unwrap_or(rhs.len());

    // At a use, a `local` that takes a new value later may hold that
    // one, so only the declaration shows the first value. A `const`
    // keeps its value.
    let at_use = offset > at + word.len();
    let reassigned = at_use && !head.ends_with("const") && alloy::flux::reassigned(&doc.source, at);

    if !rhs.starts_with("new ") || reassigned {
        return None;
    }

    let open = rhs[..line_end].find('{')?;
    let close = matching_brace(rhs, open)?;

    // The value joins the declaration line, the way Rust shows a
    // `const`: `local hits: Counter = new Counter { name = "hits" }`. A
    // long one keeps its head alone.
    let before = &doc.source[line_start..at];
    let indent = &before[..before.len() - before.trim_start().len()];
    let lines: Vec<&str> = rhs[..=close]
        .lines()
        .map(|l| l.strip_prefix(indent).unwrap_or(l))
        .collect();
    let init = match lines.len() > 6 {
        true => format!("{} ... }}", rhs[..=open].trim_end()),

        false => lines.join("\n"),
    };

    Some(join_initializer(value, word, &init))
}

/// The hover with ` = init` after the declaration line of its code
/// block, or a code block of its own when the hover names no `local`.
fn join_initializer(value: &str, word: &str, init: &str) -> String {
    let decl = value.find("```").and_then(|fence| {
        let start = fence + value[fence..].find('\n')? + 1;
        let end = start + value[start..].find('\n')?;
        let line = value[start..end].trim_start();
        let declares = ["local ", "const ", "export local ", "export const "]
            .iter()
            .any(|k| {
                line.strip_prefix(k)
                    .is_some_and(|rest| rest.starts_with(word))
            });

        declares.then_some(end)
    });

    match decl {
        Some(end) => format!("{} = {init}{}", &value[..end], &value[end..]),

        None => format!("{value}\n\n```alloy\nlocal {word} = {init}\n```"),
    }
}

/// The index of the `}` that closes the `{` at `open`.
pub(crate) fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,

            '}' => {
                depth -= 1;

                if depth == 0 {
                    return Some(open + i);
                }
            }

            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_markup_file_formats_the_way_the_command_does() {
        let src = "local function Row()\n        local x=1\n    return (\n        <Frame>{x}</Frame>\n    )\nend\n";
        let options = alloy::config::FmtConfig::default();
        let formatted = format_source("file:///ui.alx", src, &options).expect("the markup formats");
        assert_ne!(formatted, src, "an over-indented file wants an edit");
        assert_eq!(
            formatted,
            alloy::fmt::alx::format_alx_file(src, &options).expect("the markup formats")
        );
        // The plain pass cannot read markup, so the two differ.
        assert!(format_source("file:///ui.aly", src, &options).is_err());
    }
}
