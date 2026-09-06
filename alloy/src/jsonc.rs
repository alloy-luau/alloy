//! A small editor for the JSONC settings files of an editor.
//!
//! VS Code and Zed keep comments and trailing commas in `settings.json`,
//! and a parse and reprint would lose them. This editor changes one
//! value at a path and copies every other byte. It knows enough JSON to
//! step over strings, comments, and nested brackets, and no more.

/// One change to make.
#[derive(Debug, Clone)]
pub enum Edit {
    /// Set the value at `path`; a missing object on the way is created.
    Set { path: Vec<String>, value: String },
    /// Append `value` to the array at `path`, unless the array's text
    /// already holds `marker`.
    Push {
        path: Vec<String>,
        value: String,
        marker: String,
    },
}

/// The look of the file: its indent unit and line ending.
struct Style {
    indent: String,
    newline: String,
}

/// One member of an object: the key, decoded, and the value's byte span.
struct Member {
    key: String,
    start: usize,
    end: usize,
}

/// Applies every edit in turn. An empty text is an empty object. The
/// `default_indent` serves a file that has no indented line yet.
pub fn apply(text: &str, edits: &[Edit], default_indent: &str) -> Result<String, String> {
    let mut text = if text.trim().is_empty() {
        "{\n}\n".to_string()
    } else {
        text.to_string()
    };

    let style = Style {
        indent: detect_indent(&text).unwrap_or_else(|| default_indent.to_string()),
        newline: if text.contains("\r\n") { "\r\n" } else { "\n" }.to_string(),
    };

    for edit in edits {
        let open = root_object(&text)?;

        text = match edit {
            Edit::Set { path, value } => {
                set_in_object(&text, open, path, &Leaf::Set(value), 0, &style)
            }

            Edit::Push {
                path,
                value,
                marker,
            } => set_in_object(&text, open, path, &Leaf::Push(value, marker), 0, &style),
        };
    }

    Ok(text)
}

enum Leaf<'a> {
    Set(&'a str),
    Push(&'a str, &'a str),
}

/// The `{` of the root object.
fn root_object(text: &str) -> Result<usize, String> {
    let bytes = text.as_bytes();
    let mut i = 0;

    if text.starts_with('\u{feff}') {
        i = 3;
    }

    i = skip_trivia(bytes, i);

    if bytes.get(i) == Some(&b'{') {
        Ok(i)
    } else {
        Err("the file is not a JSON object".to_string())
    }
}

/// The leading whitespace of the first indented line, when one exists.
fn detect_indent(text: &str) -> Option<String> {
    text.lines()
        .filter(|l| l.starts_with(' ') || l.starts_with('\t'))
        .map(|l| l[..l.len() - l.trim_start().len()].to_string())
        .find(|ws| !ws.is_empty())
}

fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }

        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;

            while i < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }

            i = (i + 2).min(b.len());
        } else {
            return i;
        }
    }
}

/// From the opening quote to past the closing one.
fn skip_string(b: &[u8], mut i: usize) -> usize {
    i += 1;

    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }

    b.len()
}

/// From the first byte of a value to past its last.
fn skip_value(b: &[u8], mut i: usize) -> usize {
    match b.get(i) {
        Some(b'"') => skip_string(b, i),

        Some(b'{' | b'[') => {
            let mut depth = 0usize;

            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = skip_string(b, i);
                        continue;
                    }

                    b'/' if b[i..].starts_with(b"//") || b[i..].starts_with(b"/*") => {
                        i = skip_trivia(b, i);
                        continue;
                    }

                    b'{' | b'[' => depth += 1,

                    b'}' | b']' => {
                        depth -= 1;

                        if depth == 0 {
                            return i + 1;
                        }
                    }

                    _ => {}
                }

                i += 1;
            }

            b.len()
        }

        _ => {
            while i < b.len() && !b[i].is_ascii_whitespace() && !b",}]/".contains(&b[i]) {
                i += 1;
            }

            i
        }
    }
}

/// The members of the object that opens at `open`, and the index of its
/// closing brace.
fn members(text: &str, open: usize) -> (Vec<Member>, usize) {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = open + 1;

    loop {
        i = skip_trivia(b, i);

        match b.get(i) {
            None => return (out, text.len()),
            Some(b'}') => return (out, i),
            Some(b',') => {
                i += 1;
                continue;
            }
            Some(b'"') => {}
            // A bare word or a stray byte: step past it rather than loop.
            Some(_) => {
                i += 1;
                continue;
            }
        }

        let key_end = skip_string(b, i);
        let key = serde_json::from_str::<String>(&text[i..key_end]).unwrap_or_default();
        i = skip_trivia(b, key_end);

        if b.get(i) != Some(&b':') {
            continue;
        }

        i = skip_trivia(b, i + 1);
        let start = i;
        let end = skip_value(b, i);
        out.push(Member { key, start, end });
        i = end;
    }
}

fn indent(style: &Style, depth: usize) -> String {
    style.indent.repeat(depth)
}

/// The text of the tables still to create under an existing one, down to
/// the leaf. `depth` is the depth of the object that holds `path[0]`.
fn render_path(path: &[String], leaf: &Leaf, depth: usize, style: &Style) -> String {
    if path.is_empty() {
        return match leaf {
            Leaf::Set(value) => value.to_string(),

            Leaf::Push(value, _) => format!(
                "[{nl}{inner}{value}{nl}{outer}]",
                nl = style.newline,
                inner = indent(style, depth + 1),
                outer = indent(style, depth),
            ),
        };
    }

    format!(
        "{{{nl}{inner}{key}: {rest}{nl}{outer}}}",
        nl = style.newline,
        inner = indent(style, depth + 1),
        key = serde_json::to_string(&path[0]).unwrap_or_default(),
        rest = render_path(&path[1..], leaf, depth + 1, style),
        outer = indent(style, depth),
    )
}

/// Sets the leaf under the object that opens at `open`, whose members
/// sit at `depth + 1`.
fn set_in_object(
    text: &str,
    open: usize,
    path: &[String],
    leaf: &Leaf,
    depth: usize,
    style: &Style,
) -> String {
    let (found, close) = members(text, open);
    let member = found.iter().find(|m| m.key == path[0]);

    let Some(m) = member else {
        let value = render_path(&path[1..], leaf, depth + 1, style);
        return insert_member(
            text,
            open,
            close,
            !found.is_empty(),
            &path[0],
            &value,
            depth,
            style,
        );
    };

    let current = &text[m.start..m.end];

    if path.len() > 1 {
        if current.starts_with('{') {
            return set_in_object(text, m.start, &path[1..], leaf, depth + 1, style);
        }

        let value = render_path(&path[1..], leaf, depth + 1, style);
        return replace(text, m.start, m.end, &value);
    }

    match leaf {
        Leaf::Set(value) => replace(text, m.start, m.end, value),

        Leaf::Push(value, marker) => {
            if !current.starts_with('[') {
                let value = render_path(&[], leaf, depth + 1, style);
                return replace(text, m.start, m.end, &value);
            }

            if current.contains(marker) {
                return text.to_string();
            }

            push_element(text, m.start, m.end, value, depth + 1, style)
        }
    }
}

fn replace(text: &str, start: usize, end: usize, value: &str) -> String {
    format!("{}{value}{}", &text[..start], &text[end..])
}

/// Whether the bracket at `open` is followed by a line break.
fn breaks_after(text: &str, open: usize) -> bool {
    text[open + 1..]
        .trim_start_matches([' ', '\t'])
        .starts_with(['\n', '\r'])
}

/// Adds `"key": value` to the object at `open`. A new member goes first,
/// right after the brace, so the rest of the object stays as it was.
#[allow(clippy::too_many_arguments)]
fn insert_member(
    text: &str,
    open: usize,
    close: usize,
    has_members: bool,
    key: &str,
    value: &str,
    depth: usize,
    style: &Style,
) -> String {
    let key = serde_json::to_string(key).unwrap_or_default();
    let nl = &style.newline;
    let inner = indent(style, depth + 1);

    if breaks_after(text, open) {
        let comma = if has_members { "," } else { "" };
        let member = format!("{nl}{inner}{key}: {value}{comma}");

        return replace(text, open + 1, open + 1, &member);
    }

    if has_members {
        return replace(text, open + 1, open + 1, &format!("{key}: {value}, "));
    }

    let object = format!("{{{nl}{inner}{key}: {value}{nl}{}}}", indent(style, depth));

    replace(text, open, close + 1, &object)
}

/// Adds `value` as the first element of the array at `start..end`.
fn push_element(
    text: &str,
    start: usize,
    end: usize,
    value: &str,
    depth: usize,
    style: &Style,
) -> String {
    let inner = &text[start + 1..end - 1];
    let empty = skip_trivia(inner.as_bytes(), 0) == inner.len();
    let nl = &style.newline;

    if empty {
        let array = format!(
            "[{nl}{}{value}{nl}{}]",
            indent(style, depth + 1),
            indent(style, depth)
        );

        return replace(text, start, end, &array);
    }

    if breaks_after(text, start) {
        let element = format!("{nl}{}{value},", indent(style, depth + 1));

        return replace(text, start + 1, start + 1, &element);
    }

    replace(text, start + 1, start + 1, &format!("{value}, "))
}

/// A path from dotted-free parts, for the callers' tables.
pub fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(parts: &[&str], value: &str) -> Edit {
        Edit::Set {
            path: path(parts),
            value: value.to_string(),
        }
    }

    fn run(text: &str, edits: &[Edit]) -> String {
        apply(text, edits, "    ").unwrap()
    }

    /// The result, with comments and trailing commas removed, as JSON.
    fn parse(text: &str) -> serde_json::Value {
        let mut out = String::new();
        let b = text.as_bytes();
        let mut i = 0;

        while i < b.len() {
            match b[i] {
                b'"' => {
                    let end = skip_string(b, i);
                    out.push_str(&text[i..end]);
                    i = end;
                }

                b'/' if b[i..].starts_with(b"//") || b[i..].starts_with(b"/*") => {
                    i = skip_trivia(b, i);
                }

                b',' => {
                    let next = skip_trivia(b, i + 1);

                    if !matches!(b.get(next), Some(b'}' | b']')) {
                        out.push(',');
                    }

                    i += 1;
                }

                c => {
                    out.push(c as char);
                    i += 1;
                }
            }
        }

        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    #[test]
    fn an_empty_file_becomes_an_object() {
        let out = run(
            "",
            &[set(&["files.associations", "alloy.toml"], "\"toml\"")],
        );
        assert_eq!(
            out,
            "{\n    \"files.associations\": {\n        \"alloy.toml\": \"toml\"\n    }\n}\n"
        );
    }

    #[test]
    fn a_line_comment_and_a_trailing_comma_survive() {
        let text = "{\n  // the theme\n  \"workbench.colorTheme\": \"Dark\", // trailing\n  \"editor.tabSize\": 2,\n}\n";
        let out = run(
            text,
            &[set(&["files.associations", "alloy.toml"], "\"toml\"")],
        );
        assert!(out.contains("// the theme"));
        assert!(out.contains("\"Dark\", // trailing"));
        assert!(out.ends_with("\"editor.tabSize\": 2,\n}\n"));
        assert!(out.starts_with(
            "{\n  \"files.associations\": {\n    \"alloy.toml\": \"toml\"\n  },\n  // the theme"
        ));
        assert_eq!(parse(&out)["files.associations"]["alloy.toml"], "toml");
    }

    #[test]
    fn a_block_comment_inside_a_value_is_stepped_over() {
        let text = "{\n    \"files.associations\": {\n        /* keep } this */ \"*.x\": \"xml\"\n    }\n}\n";
        let out = run(
            text,
            &[set(&["files.associations", "alloy.toml"], "\"toml\"")],
        );
        assert!(out.contains("/* keep } this */ \"*.x\": \"xml\""));
        let v = parse(&out);
        assert_eq!(v["files.associations"]["alloy.toml"], "toml");
        assert_eq!(v["files.associations"]["*.x"], "xml");
    }

    #[test]
    fn crlf_stays_crlf() {
        let text = "{\r\n  \"a\": 1\r\n}\r\n";
        let out = run(text, &[set(&["b", "c"], "true")]);
        assert_eq!(
            out,
            "{\r\n  \"b\": {\r\n    \"c\": true\r\n  },\r\n  \"a\": 1\r\n}\r\n"
        );
    }

    #[test]
    fn an_existing_key_keeps_its_other_entries_and_is_overwritten() {
        let text = "{\n    \"files.associations\": {\n        \"*.aly\": \"alloy-luau\",\n        \"alloy.toml\": \"alloy-toml\"\n    }\n}\n";
        let out = run(
            text,
            &[set(&["files.associations", "alloy.toml"], "\"toml\"")],
        );
        assert_eq!(
            out,
            "{\n    \"files.associations\": {\n        \"*.aly\": \"alloy-luau\",\n        \"alloy.toml\": \"toml\"\n    }\n}\n"
        );
    }

    #[test]
    fn a_deep_path_is_created_under_an_existing_table() {
        let text = "{\n  \"lsp\": {\n    \"rust-analyzer\": {}\n  }\n}\n";
        let out = run(
            text,
            &[set(
                &["lsp", "taplo", "settings", "schema", "associations", "^x$"],
                "\"file:///s.json\"",
            )],
        );
        let v = parse(&out);
        assert_eq!(
            v["lsp"]["taplo"]["settings"]["schema"]["associations"]["^x$"],
            "file:///s.json"
        );
        assert!(v["lsp"]["rust-analyzer"].is_object());
        assert!(out.contains("\n  \"lsp\": {\n    \"taplo\": {\n      \"settings\": {\n        \"schema\": {\n          \"associations\": {\n            \"^x$\": \"file:///s.json\"\n          }\n        }\n      }\n    },\n    \"rust-analyzer\": {}"));
    }

    #[test]
    fn an_inline_object_gains_a_member_inline() {
        let out = run("{\"a\": {\"b\": 1}}", &[set(&["a", "c"], "2")]);
        assert_eq!(out, "{\"a\": {\"c\": 2, \"b\": 1}}");
    }

    #[test]
    fn an_empty_inline_object_expands() {
        let out = run("{\n  \"a\": {}\n}\n", &[set(&["a", "c"], "2")]);
        assert_eq!(out, "{\n  \"a\": {\n    \"c\": 2\n  }\n}\n");
    }

    #[test]
    fn a_non_object_on_the_path_is_replaced() {
        let out = run("{\n  \"a\": null\n}\n", &[set(&["a", "c"], "2")]);
        assert_eq!(out, "{\n  \"a\": {\n    \"c\": 2\n  }\n}\n");
    }

    #[test]
    fn push_adds_once() {
        let push = Edit::Push {
            path: path(&["lsp", "tombi", "settings", "tombi", "schemas"]),
            value: "{ \"path\": \"file:///s.json\" }".to_string(),
            marker: "file:///s.json".to_string(),
        };
        let once = run("{}", std::slice::from_ref(&push));
        let v = parse(&once);
        assert_eq!(
            v["lsp"]["tombi"]["settings"]["tombi"]["schemas"][0]["path"],
            "file:///s.json"
        );

        let twice = run(&once, std::slice::from_ref(&push));
        assert_eq!(once, twice);

        let other = "{\n  \"lsp\": {\n    \"tombi\": {\n      \"settings\": {\n        \"tombi\": {\n          \"schemas\": [\n            { \"path\": \"a.json\" },\n          ]\n        }\n      }\n    }\n  }\n}\n";
        let out = run(other, &[push]);
        let v = parse(&out);
        assert_eq!(
            v["lsp"]["tombi"]["settings"]["tombi"]["schemas"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(out.contains("{ \"path\": \"a.json\" },\n"));
    }

    #[test]
    fn a_file_that_is_not_an_object_is_an_error() {
        assert!(apply("[1, 2]", &[set(&["a"], "1")], "  ").is_err());
    }

    #[test]
    fn the_indent_comes_from_the_file() {
        let out = run("{\n\t\"a\": 1\n}\n", &[set(&["b", "c"], "1")]);
        assert_eq!(out, "{\n\t\"b\": {\n\t\t\"c\": 1\n\t},\n\t\"a\": 1\n}\n");
    }
}
