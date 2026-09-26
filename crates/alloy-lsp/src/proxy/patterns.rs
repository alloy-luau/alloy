//! Parameter patterns in the editor: `function draw({ x, y }: Point)`.
//! Completion inside the braces offers the fields the type has and the
//! pattern does not name yet, and go to definition on a bound name lands
//! on the field it reads.

use super::*;

/// A table pattern in a parameter list, read from the text so a file
/// that does not parse while someone types still answers.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Site {
    /// The `{`.
    pub open: usize,
    /// The `}`, when the pattern closes.
    pub close: Option<usize>,
    /// The type after `}:`.
    pub annotation: Option<String>,
    /// Each entry: the field it reads, and the span of the entry.
    pub entries: Vec<(String, (usize, usize))>,
}

/// The parameter pattern that holds `offset`.
pub(crate) fn site_at(src: &str, offset: usize) -> Option<Site> {
    let b = src.as_bytes();
    let offset = offset.min(src.len());

    // The innermost `{` open before the cursor, on the same header.
    let mut depth = 0i32;
    let mut i = offset;
    let mut open = None;

    while i > 0 && offset - i < 2000 {
        i -= 1;

        match b[i] {
            b'}' => depth += 1,

            b'{' if depth == 0 => {
                open = Some(i);

                break;
            }

            b'{' => depth -= 1,

            b')' if depth == 0 => return None,

            _ => {}
        }
    }

    let open = open?;
    let before = src[..open].trim_end();

    // A parameter list: `(` or `,` before the brace, and `function` or
    // `macro` before the list's `(`.
    let paren = match before.as_bytes().last()? {
        b'(' => before.len() - 1,

        b',' => {
            let mut depth = 0i32;
            let mut j = before.len() - 1;

            loop {
                if j == 0 {
                    return None;
                }

                j -= 1;

                match b[j] {
                    b')' | b'}' | b']' => depth += 1,

                    b'(' if depth == 0 => break j,

                    b'(' | b'{' | b'[' => depth -= 1,

                    _ => {}
                }
            }
        }

        _ => return None,
    };

    let mut head = src[..paren].trim_end();

    // Generics: `function f<T>(`.
    if head.ends_with('>')
        && let Some(lt) = head.rfind('<')
    {
        head = head[..lt].trim_end();
    }

    let word_start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == ':'))
        .map_or(0, |n| n + 1);
    let name = &head[word_start..];
    let lead = head[..word_start].trim_end();
    let header = matches!(name, "function" | "macro")
        || lead.ends_with("function")
        || lead.ends_with("macro");

    if !header {
        return None;
    }

    // The close, and the annotation after it.
    let mut depth = 0i32;
    let mut close = None;

    for (k, c) in src[open..].char_indices() {
        match c {
            '{' | '(' | '[' => depth += 1,

            '}' => {
                depth -= 1;

                if depth == 0 {
                    close = Some(open + k);

                    break;
                }
            }

            ')' if depth == 1 => break,

            ')' | ']' => depth -= 1,

            _ => {}
        }
    }

    let annotation = close.and_then(|c| {
        let after = src[c + 1..].trim_start().strip_prefix(':')?;
        let mut depth = 0i32;
        let end = after
            .char_indices()
            .find(|(_, ch)| match ch {
                '(' | '{' | '[' | '<' => {
                    depth += 1;

                    false
                }

                ')' | '}' | ']' | '>' if depth > 0 => {
                    depth -= 1;

                    false
                }

                ',' | ')' | '=' | '\n' => depth == 0,

                _ => false,
            })
            .map_or(after.len(), |(n, _)| n);
        let ty = after[..end].trim();

        (!ty.is_empty()).then(|| ty.to_string())
    });

    // The entries, split on the commas outside brackets.
    let inner_end = close.unwrap_or(offset);
    let mut entries = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    let push = |s: usize, e: usize, entries: &mut Vec<(String, (usize, usize))>| {
        let piece = &src[s..e];
        let field: String = piece
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if !piece.trim_start().starts_with("...") {
            entries.push((field, (s, e)));
        }
    };

    for (k, c) in src[open + 1..inner_end].char_indices() {
        let at = open + 1 + k;

        match c {
            '{' | '(' | '[' | '<' => depth += 1,

            '}' | ')' | ']' | '>' => depth -= 1,

            ',' if depth == 0 => {
                push(start, at, &mut entries);
                start = at + 1;
            }

            _ => {}
        }
    }

    push(start, inner_end, &mut entries);

    Some(Site {
        open,
        close,
        annotation,
        entries,
    })
}

impl State {
    /// The fields a parameter pattern can still name, at the start of an
    /// entry. `None` off a pattern, or where a type or a rename goes.
    pub(crate) fn pattern_completion(&self, uri: &str, offset: usize) -> Option<Vec<Value>> {
        let doc = self.docs.get(uri)?;
        let site = site_at(&doc.source, offset)?;
        let entry_start = doc.source[..offset].rfind([',', '{']).map_or(0, |n| n + 1);

        if doc.source[entry_start..offset].contains([':', '=']) {
            return None;
        }

        let ty = site.annotation.as_deref()?.trim_end_matches('?');
        let named: HashSet<&str> = site
            .entries
            .iter()
            .filter(|(_, (s, e))| !(*s <= offset && offset <= *e))
            .map(|(f, _)| f.as_str())
            .collect();

        Some(
            self.struct_fields(uri, ty, false)
                .into_iter()
                .filter(|f| !named.contains(f.name.as_str()))
                .map(|f| {
                    json!({
                        "label": f.name,
                        "kind": 5,
                        "detail": f.ty,
                        "documentation": {
                            "kind": "markdown",
                            "value": format!("A field of `{ty}`."),
                        },
                        "sortText": format!("0{}", f.name),
                    })
                })
                .collect(),
        )
    }

    /// The hover of a bound name of an annotated pattern: the local with
    /// the field's type, and the doc comment above the field.
    pub(crate) fn pattern_hover(&self, uri: &str, offset: usize) -> Option<Value> {
        let doc = self.docs.get(uri)?;
        let site = site_at(&doc.source, offset)?;
        let (field, (es, ee)) = site
            .entries
            .iter()
            .find(|(_, (s, e))| *s <= offset && offset <= *e)?;
        let ty = site.annotation.as_deref()?.trim_end_matches('?');
        let entry = doc.source[*es..*ee].trim();
        let bound = entry
            .split_once('=')
            .map_or(field.as_str(), |(_, alias)| alias.trim());
        let field_ty = self
            .struct_fields(uri, ty, true)
            .into_iter()
            .find(|f| f.name == *field)?
            .ty;
        let docs = std::iter::once(doc)
            .chain(self.docs.values())
            .find_map(|d| {
                let decl = d.decls.iter().find(|x| x.name == ty)?;
                let (s, _) = field_in(&d.source, decl.offset, field)?;

                alloy::declarations::doc_before(&d.source, s)
            })
            .map(|text| format!("\n\n{text}"))
            .unwrap_or_default();

        Some(json!({
            "contents": {
                "kind": "markdown",
                "value": format!("```alloy\nlocal {bound}: {field_ty}\n```\nField `{field}` of `{ty}`.{docs}"),
            },
        }))
    }

    /// The field a bound name of a parameter pattern reads, as an LSP
    /// location: the field in the struct, the interface, or the record
    /// type the pattern names.
    pub(crate) fn pattern_definition(&self, uri: &str, offset: usize) -> Option<Value> {
        let doc = self.docs.get(uri)?;
        let site = site_at(&doc.source, offset)?;
        let (field, _) = site
            .entries
            .iter()
            .find(|(_, (s, e))| *s <= offset && offset <= *e)?;
        let ty = site.annotation.as_deref()?.trim_end_matches('?');
        let own = std::iter::once((uri, doc));
        let others = self
            .docs
            .iter()
            .filter(|(u, _)| u.as_str() != uri)
            .map(|(u, d)| (u.as_str(), d));

        own.chain(others).find_map(|(u, d)| {
            let decl = d.decls.iter().find(|x| x.name == ty)?;
            let (s, e) = field_in(&d.source, decl.offset, field)?;
            let start = position_of(&d.source, s);
            let end = position_of(&d.source, e);

            Some(json!([{ "uri": u, "range": range_value(start, end) }]))
        })
    }
}

impl Server {
    /// Completion and definition inside a parameter pattern. `false`
    /// leaves the request to the other answers.
    pub(crate) fn pattern_answer(
        &self,
        method: &str,
        uri: &str,
        message: &Value,
        id: &Value,
    ) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(offset) = st
            .docs
            .get(uri)
            .and_then(|d| offset_of(&d.source, line, character))
        else {
            return false;
        };
        let result = match method {
            "textDocument/completion" => st.pattern_completion(uri, offset).map(Value::Array),

            "textDocument/hover" => st.pattern_hover(uri, offset),

            _ => st.pattern_definition(uri, offset),
        };
        drop(st);

        match result {
            Some(r) => {
                self.respond(id, r);

                true
            }

            None => false,
        }
    }
}

impl State {
    /// A rename of a local that a shorthand entry binds. The child edits
    /// the entry, which also names the field it reads, so the edit keeps
    /// the field and renames the local: `{ x = across }`.
    pub(crate) fn mend_pattern_rename(&self, result: &mut Value) {
        let Some(changes) = result
            .pointer_mut("/changes")
            .and_then(Value::as_object_mut)
        else {
            return;
        };

        for (uri, edits) in changes.iter_mut() {
            let Some(doc) = self.docs.get(uri) else {
                continue;
            };

            for edit in edits.as_array_mut().into_iter().flatten() {
                let Some(((sl, sc), (el, ec))) = edit.get("range").and_then(range_of) else {
                    continue;
                };
                let (Some(s), Some(e)) = (
                    offset_of(&doc.source, sl, sc),
                    offset_of(&doc.source, el, ec),
                ) else {
                    continue;
                };
                let new = edit["newText"].as_str().unwrap_or_default().to_string();

                if s < e && shorthand_entry(&doc.source, s, e) {
                    edit["newText"] = json!(format!("{} = {new}", &doc.source[s..e]));
                }
            }
        }
    }
}

/// A signature with each pattern temp dropped from its parameter list:
/// the caller passes one value, so its type stands for the pattern. A
/// `_p1` the source writes itself stays.
pub(crate) fn without_pattern_temps(text: &str, source: &str) -> Option<String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let written = |name: &str| {
        source.match_indices(name).any(|(at, _)| {
            !source[..at].ends_with(word) && !source[at + name.len()..].starts_with(word)
        })
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;

    while let Some(at) = rest.find("_p") {
        out.push_str(&rest[..at]);
        let digits = rest[at + 2..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let (name, after) = rest[at..].split_at(2 + digits);

        if digits > 0
            && (out.ends_with('(') || out.ends_with(", "))
            && let Some(ty) = after.strip_prefix(": ")
            && !written(name)
        {
            rest = ty;
            changed = true;
        } else {
            out.push_str(name);
            rest = after;
        }
    }

    out.push_str(rest);

    changed.then_some(out)
}

/// Whether `start..end` is a shorthand entry of a table pattern: `{ x }`
/// in a parameter list, a `local`, or a `for` head.
pub(crate) fn shorthand_entry(src: &str, start: usize, end: usize) -> bool {
    let before = src[..start].trim_end();
    let after = src[end..].trim_start();

    if !before.ends_with(['{', ',']) || !after.starts_with([',', '}', ':']) {
        return false;
    }

    let b = src.as_bytes();
    let mut depth = 0i32;
    let open = (0..start).rev().find(|&i| match b[i] {
        b'}' | b')' | b']' => {
            depth += 1;

            false
        }

        b'{' | b'(' | b'[' if depth == 0 => true,

        b'{' | b'(' | b'[' => {
            depth -= 1;

            false
        }

        _ => false,
    });
    let Some(open) = open.filter(|&i| b[i] == b'{') else {
        return false;
    };
    let head = src[..open].trim_end();
    let line = head[head.rfind('\n').map_or(0, |n| n + 1)..].trim_start();
    let word = |w: &str| {
        head.strip_suffix(w)
            .is_some_and(|h| !h.ends_with(|c: char| c.is_alphanumeric() || c == '_'))
    };

    word("local")
        || word("const")
        || (line.starts_with("for ") && !line.contains(" in "))
        || site_at(src, start).is_some()
}

/// The edits a rename of field `field` of `owner` makes in the parameter
/// patterns of a source that name `owner`: `{ x }` reads the field under
/// its new name and keeps its local, `{ left = x }`, and `{ x = a }`
/// becomes `{ left = a }`.
pub(crate) fn pattern_field_edits(
    src: &str,
    owner: &str,
    field: &str,
    new_name: &str,
) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();

    for (at, _) in src.match_indices('}') {
        let Some(site) = site_at(src, at) else {
            continue;
        };

        if site.close != Some(at)
            || site.annotation.as_deref().map(|t| t.trim_end_matches('?')) != Some(owner)
        {
            continue;
        }

        for (name, (s, e)) in &site.entries {
            if name != field {
                continue;
            }

            let lead = src[*s..*e].len() - src[*s..*e].trim_start().len();
            let fs = s + lead;
            let fe = fs + field.len();
            let renamed = src[fe..*e].trim_start().starts_with('=');
            let text = match renamed {
                true => new_name.to_string(),

                false => format!("{new_name} = {field}"),
            };
            out.push((fs, fe, text));
        }
    }

    out
}

/// The span of a field's name in the declaration that starts at `from`:
/// `x: number` in a struct body, or in a record type. The search stops at
/// the next statement that starts a line.
fn field_in(src: &str, from: usize, field: &str) -> Option<(usize, usize)> {
    let body_end = src[from..]
        .match_indices('\n')
        .map(|(n, _)| from + n + 1)
        .find(|at| {
            let rest = &src[*at..];

            rest.starts_with(|c: char| c.is_alphabetic()) && !rest.starts_with("end")
        })
        .unwrap_or(src.len());
    let body = &src[from..body_end];
    let bytes = body.as_bytes();

    body.match_indices(field).find_map(|(n, _)| {
        let before = n.checked_sub(1).map(|k| bytes[k]);
        let after = body[n + field.len()..].trim_start();
        let whole = before.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == b'_'))
            && !body[n + field.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_');

        (whole && after.starts_with(':')).then(|| (from + n, from + n + field.len()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_reads_the_pattern_and_its_type() {
        let src = "local function draw({ x, y = top }: Point, n: number)\nend";
        let site = site_at(src, src.find("y =").unwrap()).unwrap();

        assert_eq!(site.annotation.as_deref(), Some("Point"));
        assert_eq!(
            site.entries
                .iter()
                .map(|(f, _)| f.as_str())
                .collect::<Vec<_>>(),
            vec!["x", "y"]
        );

        // A table in a body is no pattern.
        let body = "local t = { a = 1 }";
        assert_eq!(site_at(body, body.find('a').unwrap()), None);

        // An unclosed pattern while someone types.
        let typing = "local function pick({ x, ";
        assert_eq!(site_at(typing, typing.len()).unwrap().close, None);
    }

    #[test]
    fn a_signature_drops_the_pattern_temps() {
        let src = "local function draw({ x }: Point, _p9: number)\nend";

        assert_eq!(
            without_pattern_temps(
                "function draw(_p1: Point, _p9: number, b: { _p2: string }): number",
                src
            )
            .as_deref(),
            Some("function draw(Point, _p9: number, b: { _p2: string }): number")
        );
        assert_eq!(without_pattern_temps("function f(a: number)", src), None);
    }

    #[test]
    fn a_shorthand_entry_is_found_in_every_pattern() {
        let at = |src: &str, word: &str| {
            let s = src.find(word).unwrap();

            shorthand_entry(src, s, s + word.len())
        };

        assert!(at("local function f({ x, y }: P)\nend", "y"));
        assert!(at("local { a, b: number } = t", "b"));
        assert!(at("for _, { x = ex, y } in pts do end", "y"));
        assert!(!at("for _, { x = ex, y } in pts do end", "ex"));
        assert!(!at("local t = { y }", "y"));
        assert!(!at("print { y }", "y"));
    }

    #[test]
    fn a_field_rename_reaches_the_patterns_of_its_type() {
        let src = "local function a({ x, y }: Point)\nend\nlocal function b({ x = left }: Point)\nend\nlocal function c({ x }: Size)\nend\n";
        let edits = pattern_field_edits(src, "Point", "x", "across");
        let texts: Vec<&str> = edits.iter().map(|(_, _, t)| t.as_str()).collect();

        assert_eq!(texts, vec!["across = x", "across"]);
        assert!(edits.iter().all(|(s, e, _)| &src[*s..*e] == "x"));
    }

    #[test]
    fn a_field_is_found_in_a_struct_and_a_record() {
        let src = "struct Point as\n    x: number\n    y: number\nend\ntype Size = { w: number, h: number }\n";

        assert_eq!(
            field_in(src, 0, "y"),
            Some((src.find("y:").unwrap(), src.find("y:").unwrap() + 1))
        );
        let at = src.find("type Size").unwrap();
        assert_eq!(
            field_in(src, at, "h"),
            Some((src.find("h:").unwrap(), src.find("h:").unwrap() + 1))
        );
        assert_eq!(
            field_in(src, 0, "w"),
            None,
            "the struct ends before the record"
        );
    }
}
