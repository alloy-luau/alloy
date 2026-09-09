//! On-type formatting: the end an open block writes, and the closing tag after an opener.

use super::*;

impl Server {
    /// `textDocument/formatting`: `alloy fmt` over the open document, as
    /// one edit that replaces the whole text. An `.alx` file, a file
    /// that does not lex, and one already formatted get no edits.
    pub(crate) fn format_document(&self, uri: &str, id: &Value) {
        let source = {
            let st = self.state.lock().expect("state");

            st.docs.get(uri).map(|d| d.source.clone())
        };
        let Some(source) = source else {
            self.respond(id, Value::Null);

            return;
        };

        if uri.ends_with(".alx") {
            self.respond(id, json!([]));

            return;
        }

        let ingots = self.state.lock().expect("state").ingots.clone();
        let formatted = alloy::fmt::format(&source).map(|f| match (&ingots, uri_to_path(uri)) {
            (Some(ingots), Some(path)) => ingots.format(&path.to_string_lossy(), &f).0,

            _ => f,
        });

        match formatted {
            Ok(formatted) if formatted != source => {
                let (el, ec) = position_of(&source, source.len());
                self.respond(
                    id,
                    json!([{
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": el, "character": ec } },
                        "newText": formatted,
                    }]),
                );
            }

            _ => self.respond(id, json!([])),
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
        let st = self.state.lock().expect("state");
        let at = message
            .pointer("/params/position")
            .and_then(position_of_value);
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
        let st = self.state.lock().expect("state");
        let ch = message
            .pointer("/params/ch")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let at = message
            .pointer("/params/position")
            .and_then(position_of_value);
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
pub(crate) fn append_initializer(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let mut from = 0;

    while let Some(i) = doc.source[from..].find(word) {
        let at = from + i;
        let line_start = doc.source[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
        let head = doc.source[line_start..at].trim();
        let after = &doc.source[at + word.len()..];
        let is_decl = matches!(head, "local" | "const" | "export local" | "export const")
            && !keywords::is_word_at(&doc.source, at + word.len());

        let eq = after.find('=');
        let between = eq.map(|e| after[..e].trim_start()).unwrap_or("x");

        if is_decl
            && (between.is_empty() || between.starts_with(':'))
            && let Some(eq) = eq
        {
            let rhs = after[eq + 1..].trim_start();

            // The fields open on the `new` line; a brace on a later line
            // belongs to another statement.
            let line_end = rhs.find('\n').unwrap_or(rhs.len());

            // The hover is a use of this binding: no function between
            // the declaration and the hover takes the name as a parameter,
            // and no later `local` rebinds it.
            let hovered_before = offset < at;
            let rebound = !hovered_before
                && rebinds(
                    &doc.source[at + word.len()..offset.max(at + word.len())],
                    word,
                );

            if rhs.starts_with("new ")
                && !hovered_before
                && !rebound
                && let Some(open) = rhs[..line_end].find('{')
                && let Some(close) = matching_brace(rhs, open)
            {
                let block = rhs[open..=close].trim();

                return Some(format!(
                    "{value}\n\nInitialized with\n```alloy\n{block}\n```"
                ));
            }

            return None;
        }

        from = at + word.len();
    }

    None
}

/// Whether a stretch of source binds `name` again: a function that
/// takes it as a parameter, or a `local` that declares it.
pub(crate) fn rebinds(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();

        if trimmed.starts_with("local ")
            && trimmed[6..].trim_start().starts_with(name)
            && !keywords::is_word_at(
                trimmed,
                6 + trimmed[6..].len() - trimmed[6..].trim_start().len() + name.len(),
            )
        {
            return true;
        }

        if let Some(f) = line.find("function")
            && let Some(open) = line[f..].find('(')
            && let Some(close) = line[f + open..].find(')')
        {
            let params = &line[f + open + 1..f + open + close];

            return params
                .split(',')
                .any(|p| p.trim().split(':').next().is_some_and(|n| n.trim() == name));
        }

        false
    })
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
