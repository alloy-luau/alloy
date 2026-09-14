//! Low-level group and member scanning shared by more than one fold in
//! `shapes`: balanced brackets, a type's length, and a table's members.

/// The parts of `text` at each `sep` of depth zero; one part when the
/// text holds no such separator. The parts keep their own spacing, so a
/// caller that wants them bare trims them itself.
///
/// `angles` counts `<` and `>` as a bracket pair, where an arrow's `>`
/// closes nothing. An intersection of types inside a generic reads as
/// one list, so its splitter leaves the angles alone.
pub(crate) fn split_at_depth<'t>(text: &'t str, sep: &str, angles: bool) -> Vec<&'t str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'<' if angles => depth += 1,
            b'>' if angles && (i == 0 || bytes[i - 1] != b'-') => depth -= 1,
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => depth -= 1,
            _ if depth == 0 && text[i..].starts_with(sep) => {
                out.push(&text[from..i]);
                i += sep.len();
                from = i;

                continue;
            }
            _ => {}
        }

        i += 1;
    }

    out.push(&text[from..]);

    out
}

/// The members of a union at depth zero of `text`; one member when the
/// text holds no such union.
pub(crate) fn split_union(text: &str) -> Vec<&str> {
    split_at_depth(text, " | ", true)
}

/// The length of the group `open ... close` that starts the text.
pub(crate) fn group_len(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = text.as_bytes();

    for i in 0..bytes.len() {
        if bytes[i] == open as u8 {
            depth += 1;
        } else if bytes[i] == close as u8 && !(close == '>' && i > 0 && bytes[i - 1] == b'-') {
            depth -= 1;

            if depth == 0 {
                return Some(i + 1);
            }
        }
    }

    None
}

/// The length of a type that starts at the text's start: a balanced
/// bracket group, or a run up to the next separator.
pub(crate) fn balanced_len(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;

    for (k, c) in text.char_indices() {
        if in_string {
            if c == '"' {
                in_string = false;
            }

            continue;
        }

        // `->` carries a `>`, so angle brackets do not count.
        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k + 1);
                }
            }
            '\n' | ';' if depth == 0 => return Some(k),
            // A diagnostic quotes a type; the quote that closes it is
            // no part of the type.
            '\'' | '`' if depth == 0 => return Some(k),
            _ => {}
        }
    }

    (depth == 0).then_some(text.len())
}

/// The length of a type at the start of the text, up to a `,` or a
/// closing bracket at depth zero. Angle brackets count, so
/// `Result<number, any>` stays whole; the `>` of an arrow closes none.
pub(crate) fn type_len(text: &str) -> usize {
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut in_string = false;
    let mut prev = ' ';

    for (k, c) in text.char_indices() {
        if in_string {
            if c == '"' {
                in_string = false;
            }

            prev = c;

            continue;
        }

        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' if depth == 0 => return k,
            '}' | ')' | ']' => depth -= 1,
            '<' if prev.is_ascii_alphanumeric() || prev == '_' => angle += 1,
            '>' if prev != '-' && angle > 0 => angle -= 1,
            ',' if depth == 0 && angle == 0 => return k,
            _ => {}
        }

        prev = c;
    }

    text.len()
}

/// Whether a character stands in the text outside every bracket.
pub(crate) fn outside_angles(text: &str, want: char) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 0i32;

    for (k, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if k > 0 && bytes[k - 1] == b'-' => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            _ if c == want && depth == 0 => return true,
            _ => {}
        }
    }

    false
}

/// The opener that matches the closing bracket at `close`.
pub(crate) fn enclosing_open(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text[..=close].char_indices().rev() {
        match c {
            '}' | ')' | ']' => depth += 1,
            '{' | '(' | '[' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }

    None
}

/// The `{` that opens the table a byte sits in.
pub(crate) fn enclosing_brace(text: &str, at: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text[..at].char_indices().rev() {
        match c {
            '}' => depth += 1,
            '{' if depth == 0 => return Some(k),
            '{' => depth -= 1,
            _ => {}
        }
    }

    None
}

/// The length of one union member at the start of `text`.
pub(crate) fn member_len(text: &str) -> Option<usize> {
    if text.starts_with('{') {
        return balanced_len(text);
    }

    if let Some(rest) = text.strip_prefix('"') {
        return rest.find('"').map(|i| i + 2);
    }

    let name: usize = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .map(char::len_utf8)
        .sum();

    if name == 0 {
        return None;
    }

    if text[name..].starts_with('<') {
        let mut depth = 0i32;

        for (k, c) in text[name..].char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;

                    if depth == 0 {
                        return Some(name + k + 1);
                    }
                }
                _ => {}
            }
        }

        return None;
    }

    Some(name)
}

/// The `(` that opens the group a `)` at `close` ends.
pub(crate) fn group_start(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text[..=close].char_indices().rev() {
        match c {
            ')' | '}' | ']' => depth += 1,
            '(' | '{' | '[' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }

    None
}

/// The type text before ` where `: from the start of its line, past a
/// `local x: ` or `x: ` head, so a replacement keeps the label.
pub(crate) fn head_of(text: &str, where_at: usize) -> (usize, &str) {
    // A head printed over several lines ends in a bracket before the
    // `where`; the type starts on the line that opens it. A union walks
    // group by group, past every `|` between them.
    let mut group_start = where_at;

    loop {
        let before = text[..group_start].trim_end();

        match before.chars().next_back() {
            Some('}' | ')' | ']') => match enclosing_open(text, before.len() - 1) {
                Some(open) => group_start = open,

                None => break,
            },

            Some('|' | '&' | '?') => group_start = before.len() - 1,

            _ => break,
        }
    }

    let line_start = text[..group_start].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let line = &text[line_start..group_start];
    // The type starts after the last `: ` outside brackets on the line,
    // or at the line start when the line is the type alone.
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in line.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ':' if depth == 0 && line[k + 1..].starts_with(' ') => start = k + 2,
            // A diagnostic quotes the type on one line with its
            // sentence: `Expected this to be '{ ... } where ...'`.
            '\'' | '`' if depth == 0 => start = k + 1,
            _ => {}
        }
    }

    (line_start + start, &text[line_start + start..where_at])
}

/// The length of `pattern` at the start of `text`, when the two agree
/// up to whitespace: a run of it on either side matches any run, or
/// none, on the other.
pub(crate) fn match_loose(text: &str, pattern: &str) -> Option<usize> {
    let t: Vec<(usize, char)> = text.char_indices().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut i, mut j) = (0, 0);

    while j < p.len() {
        if p[j].is_whitespace() {
            while j < p.len() && p[j].is_whitespace() {
                j += 1;
            }

            while i < t.len() && t[i].1.is_whitespace() {
                i += 1;
            }

            continue;
        }

        while i < t.len() && t[i].1.is_whitespace() {
            i += 1;
        }

        if i >= t.len() || t[i].1 != p[j] {
            return None;
        }

        i += 1;
        j += 1;
    }

    Some(t.get(i).map_or(text.len(), |(k, _)| *k))
}

/// The members of a table body split at the commas of depth one, with
/// their modifiers. A type argument list holds its own commas, so
/// `[number]: Result<number, any>` stays one member.
pub(crate) fn member_parts(body: &str) -> Vec<&str> {
    let inner = body.trim();
    let inner = inner
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(inner);
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();

    while start < inner.len() {
        let len = type_len(&inner[start..]);

        if inner[start + len..].starts_with(',') {
            parts.push(&inner[start..start + len]);
            start += len + 1;
        } else {
            break;
        }
    }

    parts.push(&inner[start..]);

    parts
}

/// The colon after a member's key, not one inside a function type.
pub(crate) fn find_key_colon(part: &str) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in part.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ':' if depth == 0 => return Some(k),
            _ => {}
        }
    }

    None
}

pub(crate) fn members(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();

    for part in member_parts(body) {
        let part = part.trim();

        if part.is_empty() {
            continue;
        }

        let Some(colon) = find_key_colon(part) else {
            continue;
        };
        let key = part[..colon].trim();
        let key = key
            .strip_prefix("read ")
            .or_else(|| key.strip_prefix("write "))
            .unwrap_or(key);
        out.push((key.to_string(), part[colon + 1..].trim().to_string()));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_argument_list_keeps_its_comma() {
        assert_eq!(
            member_parts("{ [number]: Result<number, any>, len: (self: t1) -> number }"),
            vec![
                " [number]: Result<number, any>",
                " len: (self: t1) -> number "
            ]
        );
    }
}
