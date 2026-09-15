//! Low-level text scanning shared by more than one concern in
//! `context`: word boundaries, comment stripping, type-text spans, and
//! block-opener and block-closer counts.

pub(crate) fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The code of a line, with a `--` comment cut off. A `--` inside a
/// string is text, not a comment.
pub(crate) fn code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];

        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }

            None => {
                if matches!(c, b'"' | b'\'' | b'`') {
                    quote = Some(c);
                } else if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
                    return &line[..i];
                }
            }
        }

        i += 1;
    }

    line
}

/// The last whole word of a text, empty when it ends in punctuation.
pub(crate) fn last_word(text: &str) -> &str {
    let end = text.trim_end_matches(is_word);

    &text[end.len()..]
}

/// The type an annotation names, up to the `,`, `)`, or `=` that ends
/// it at the top level.
pub(crate) fn type_text(rest: &str) -> String {
    let mut depth = 0i32;
    let mut end = rest.len();
    let mut prev = ' ';

    for (i, c) in rest.char_indices() {
        // `->` carries a `>` that closes nothing.
        if c == '>' && prev == '-' {
            prev = c;

            continue;
        }

        prev = c;

        match c {
            '<' | '(' | '[' | '{' => depth += 1,

            '>' | ')' | ']' | '}' => {
                if depth == 0 {
                    end = i;

                    break;
                }

                depth -= 1;
            }

            ',' | '=' if depth == 0 => {
                end = i;

                break;
            }

            _ => {}
        }
    }

    rest[..end].trim().to_string()
}

pub(crate) fn block_openers(text: &str) -> i32 {
    text.split(|c: char| !is_word(c))
        .filter(|w| {
            matches!(
                *w,
                "function" | "if" | "for" | "while" | "do" | "match" | "repeat"
            )
        })
        .count() as i32
        - text
            .split(|c: char| !is_word(c))
            .filter(|w| matches!(*w, "do"))
            .count() as i32
            * i32::from(text.contains("while ") || text.contains("for "))
}

pub fn block_closers(text: &str) -> i32 {
    text.split(|c: char| !is_word(c))
        .filter(|w| matches!(*w, "end" | "until"))
        .count() as i32
}
