use super::*;

impl Server {
    pub(crate) fn declaration_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];

        // A sigil names a macro or an attribute of this project. After a
        // dot the name is the receiver's: `Msg.Join` finds the variant
        // through the enum's name; any other field is not ours.
        let before = doc.source[..start].trim_end();
        let key = if doc.source[..start].ends_with('$') || before.ends_with("macro") {
            format!("${word}")
        } else if doc.source[..start].ends_with('@') || before.ends_with("attribute") {
            format!("@{word}")
        } else if let Some(head) = before.strip_suffix('.') {
            let at = head.len().saturating_sub(1);

            if head.is_empty() || !keywords::is_word_at(&doc.source, at) {
                return false;
            }

            let (hs, he) = keywords::word_range(&doc.source, at);

            format!("{}.{word}", &doc.source[hs..he])
        } else if doc.source[..start].ends_with(':') {
            // `obj:method`, not the `x: T` of an annotation.
            return false;
        } else {
            word.to_string()
        };

        // An attribute or a macro is keyed by its sigil; a bare name that
        // an import bound finds it that way.
        let sigils = [format!("@{key}"), format!("${key}")];
        let lookup = |name: &str| {
            doc.decls.iter().find(|d| d.name == name).or_else(|| {
                st.docs
                    .values()
                    .flat_map(|d| d.decls.iter())
                    .find(|d| d.name == name)
            })
        };
        let found = lookup(&key).or_else(|| sigils.iter().find_map(|k| lookup(k)));

        let Some(decl) = found else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": decl.hover },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// A `case` pattern's binding hovers as the payload it names. A
    /// match lowers to one expression, so the binding has no local of
    /// its own and the child answers the arm's result instead.
    pub(crate) fn case_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = doc.source[start..end].to_string();
        let known = st.known_shapes_at(Some(uri));

        let Some(answer) = case_binding_text(doc, line as usize, start, &word, &known) else {
            return false;
        };
        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": answer },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }
}

/// The hover of a `case` pattern's binding at `line`: the name with the
/// type the pattern gives it. `None` when the line is in no arm, or the
/// word is no binding of it.
pub(crate) fn case_binding_text(
    doc: &Doc,
    line: usize,
    start: usize,
    word: &str,
    known: &crate::shapes::Known,
) -> Option<String> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let mut at = line.min(lines.len().saturating_sub(1));

    // The arm the line belongs to: the nearest `case` above it, and no
    // `end` or `match` head between.
    let case_line = loop {
        let text = lines.get(at)?.trim();

        if text.starts_with("case ") {
            break at;
        }

        if text == "end" || text.ends_with(" with") {
            return None;
        }

        at = at.checked_sub(1)?;
    };
    let pattern = case_pattern(lines[case_line])?;
    let bindings = pattern_bindings(&pattern, known, || array_element(&lines, case_line));

    // `b.amount` reads a field of what `case Buff(b)` bound; the child
    // sees the payload slot and answers `any`.
    if let Some(head) = doc.source[..start].strip_suffix('.')
        && keywords::is_word_at(&doc.source, head.len().checked_sub(1)?)
    {
        let (rs, re) = keywords::word_range(&doc.source, head.len() - 1);
        let receiver = &doc.source[rs..re];
        let (_, ty, _) = bindings.iter().find(|(n, _, _)| n == receiver)?;

        return field_of_struct(doc, ty, word);
    }

    let (_, ty, owner) = bindings.into_iter().find(|(n, _, _)| n == word)?;

    Some(format!(
        "```alloy\n{word}: {ty}\n```\nA binding of {owner}."
    ))
}

/// The hover of one field of a named struct, from the declaration index.
pub(crate) fn field_of_struct(doc: &Doc, name: &str, field: &str) -> Option<String> {
    let line = doc
        .decls
        .iter()
        .filter(|d| d.name == name && d.hover.contains("struct "))
        .find_map(|d| {
            d.hover
                .lines()
                .find(|l| field_key(l) == Some(field))
                .map(|l| l.trim().to_string())
        })?;

    Some(format!(
        "```alloy\n{line}\n```\nA field of `struct {name}`."
    ))
}

/// The pattern of a `case` line: what stands between `case` and the
/// arm's `then`, or the guard's `and`.
pub(crate) fn case_pattern(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("case ")?;
    let end = [rest.find(" then"), rest.find(" and ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());

    Some(rest[..end].trim().to_string())
}

/// The names a pattern binds, each with its type and what it comes
/// from. A payload reads its type off the enum's declaration; an array
/// pattern reads the element type of what the match runs over.
pub(crate) fn pattern_bindings(
    pattern: &str,
    known: &crate::shapes::Known,
    element: impl Fn() -> Option<String>,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();

    if let Some(inner) = pattern.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
        let Some(elem) = element() else {
            return out;
        };

        for item in inner.split(',') {
            let item = item.trim();

            match item.strip_prefix("...") {
                Some(rest) if is_binding(rest) => {
                    out.push((
                        rest.to_string(),
                        format!("{elem}[]"),
                        "the array pattern".into(),
                    ));
                }

                _ if is_binding(item) => {
                    out.push((item.to_string(), elem.clone(), "the array pattern".into()));
                }

                _ => {}
            }
        }

        return out;
    }

    let Some(open) = pattern.find('(') else {
        return out;
    };
    let head = pattern[..open].trim();
    let variant = head.rsplit('.').next().unwrap_or(head);
    let args = pattern[open + 1..].trim_end().trim_end_matches(')');

    let found = known.shapes.iter().find_map(|s| match s {
        alloy::declarations::Shape::Enum { name, variants } => variants
            .iter()
            .find(|(v, _)| v == variant)
            .map(|(_, payload)| (name.clone(), payload.clone())),

        _ => None,
    });

    let Some((enum_name, payload)) = found else {
        return out;
    };

    for (k, item) in split_top(args).into_iter().enumerate() {
        let item = item.trim();

        if !is_binding(item) {
            continue;
        }

        let Some(ty) = payload.get(k) else {
            continue;
        };
        out.push((
            item.to_string(),
            ty.clone(),
            format!("`{enum_name}.{variant}`"),
        ));
    }

    out
}

/// Whether a pattern item is a name the arm binds, and not `_` or a
/// literal.
pub(crate) fn is_binding(text: &str) -> bool {
    !text.is_empty()
        && text != "_"
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The members of a pattern's argument list, split at the commas that
/// stand outside every bracket.
pub(crate) fn split_top(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&text[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    out.push(&text[start..]);

    out
}

/// The element type the match runs over, for an array pattern: the
/// annotation of the name the `match` head reads.
pub(crate) fn array_element(lines: &[&str], case_line: usize) -> Option<String> {
    let head = lines[..case_line]
        .iter()
        .rev()
        .find(|l| l.trim_end().ends_with(" with"))?;
    let at = head.find("match ")? + "match ".len();
    let name = head[at..].trim_end().trim_end_matches("with").trim();

    if !is_binding(name) {
        return None;
    }

    let needle = format!("{name}: ");

    for line in lines[..case_line].iter().rev() {
        let Some(i) = line.find(&needle) else {
            continue;
        };
        let rest = &line[i + needle.len()..];
        let end = rest.find([',', ')']).unwrap_or(rest.len());
        let ty = rest[..end].trim();

        return ty.strip_suffix("[]").map(str::to_string);
    }

    None
}
