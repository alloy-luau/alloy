//! Syntax colors for code on the terminal: the same rules and the same
//! palette as the website's highlighter, so a snippet in `alloy doc`
//! reads like the one in the book.

use crate::ui::{self, RESET};

/// The language of a fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Alloy,
    Luau,
    Toml,
    Json,
    Shell,
    Text,
}

impl Mode {
    /// The mode a fence tag names.
    pub fn of(tag: &str) -> Self {
        match tag.trim() {
            "alloy" | "aly" | "alx" => Mode::Alloy,
            "luau" | "lua" => Mode::Luau,
            "toml" => Mode::Toml,
            "json" => Mode::Json,
            "sh" | "bash" | "shell" => Mode::Shell,
            _ => Mode::Text,
        }
    }
}

const KEYWORDS: &[&str] = &[
    "local",
    "const",
    "function",
    "end",
    "if",
    "then",
    "else",
    "elseif",
    "for",
    "in",
    "while",
    "do",
    "repeat",
    "until",
    "return",
    "break",
    "continue",
    "and",
    "or",
    "not",
    "nil",
    "true",
    "false",
    "struct",
    "enum",
    "trait",
    "impl",
    "interface",
    "extends",
    "as",
    "match",
    "case",
    "default",
    "with",
    "async",
    "await",
    "try",
    "new",
    "delete",
    "import",
    "export",
    "from",
    "type",
    "remote",
    "macro",
    "attribute",
    "on",
    "where",
    "is",
    "read",
    "write",
    "client",
    "server",
    "self",
    "declare",
];

/// The palette, as the website's `pre.code` classes.
pub const KEYWORD: (u8, u8, u8) = (0xC9, 0xB5, 0xFF);
pub const STRING: (u8, u8, u8) = (0x9F, 0xE2, 0xC0);
pub const NUMBER: (u8, u8, u8) = (0xFF, 0xD0, 0x8A);
pub const COMMENT: (u8, u8, u8) = (0x8B, 0x83, 0xA8);
pub const SIGIL: (u8, u8, u8) = (0x5C, 0xC6, 0xEE);
pub const OPERATOR: (u8, u8, u8) = (0xF0, 0xA5, 0x8F);
pub const PUNCT: (u8, u8, u8) = (0x8A, 0x83, 0xA6);
pub const CALL: (u8, u8, u8) = (0x9F, 0xCA, 0xFF);
pub const TYPE: (u8, u8, u8) = (0xD9, 0xC8, 0xFF);
pub const PLAIN: (u8, u8, u8) = (0xE9, 0xE4, 0xF7);

/// Paints `code`; the plain text when `color` is off.
pub fn paint(code: &str, mode: Mode, color: bool) -> String {
    if !color {
        return code.to_string();
    }

    code.lines()
        .map(|l| paint_line(l, mode))
        .collect::<Vec<_>>()
        .join("\n")
}

fn span(rgb: (u8, u8, u8), text: &str) -> String {
    format!("{}{text}{RESET}", ui::fg(rgb))
}

fn paint_line(line: &str, mode: Mode) -> String {
    if mode == Mode::Shell {
        return paint_shell(line);
    }

    let mut out = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let n = chars.len();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    while i < n {
        let c = chars[i];
        let rest: String = chars[i..].iter().collect();

        // Comments.
        if mode == Mode::Toml && c == '#'
            || (matches!(mode, Mode::Alloy | Mode::Luau | Mode::Text) && rest.starts_with("--"))
        {
            out.push_str(&span(COMMENT, &rest));

            break;
        }

        // Strings.
        if c == '"' || c == '\'' || c == '`' {
            let close = chars[i + 1..]
                .iter()
                .position(|&d| d == c)
                .map(|p| i + 1 + p);
            let end = close.map_or(n, |p| p + 1);
            let s: String = chars[i..end].iter().collect();
            out.push_str(&span(STRING, &s));
            i = end;

            continue;
        }

        // Numbers.
        if c.is_ascii_digit() {
            let mut j = i;

            while j < n && (chars[j].is_ascii_digit() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }

            let s: String = chars[i..j].iter().collect();
            out.push_str(&span(NUMBER, &s));
            i = j;

            continue;
        }

        // Sigils: `$dbg`, `@derive`.
        if (c == '$' || c == '@')
            && i + 1 < n
            && (chars[i + 1].is_alphabetic() || chars[i + 1] == '_')
        {
            let mut j = i + 1;

            while j < n && is_word(chars[j]) {
                j += 1;
            }

            let s: String = chars[i..j].iter().collect();
            out.push_str(&span(SIGIL, &s));
            i = j;

            continue;
        }

        // TOML tables.
        if mode == Mode::Toml && c == '[' {
            let end = chars[i..]
                .iter()
                .position(|&d| d == ']')
                .map_or(n, |p| i + p + 1);
            let s: String = chars[i..end].iter().collect();
            out.push_str(&span(KEYWORD, &s));
            i = end;

            continue;
        }

        // Words: keywords, calls, types, or plain.
        if c.is_alphabetic() || c == '_' {
            let mut j = i;

            while j < n && is_word(chars[j]) {
                j += 1;
            }

            let w: String = chars[i..j].iter().collect();
            let prev = if i > 0 { chars[i - 1] } else { ' ' };
            let after: String = chars[j..].iter().collect();
            let keyword = matches!(mode, Mode::Alloy | Mode::Luau)
                && KEYWORDS.contains(&w.as_str())
                && prev != '.'
                && prev != ':';
            let called = !keyword && after.trim_start().starts_with('(');
            let typed = prev == ':' && w.chars().next().is_some_and(|f| f.is_uppercase());

            if keyword {
                out.push_str(&span(KEYWORD, &w));
            } else if called {
                out.push_str(&span(CALL, &w));
            } else if typed {
                out.push_str(&span(TYPE, &w));
            } else {
                out.push_str(&span(PLAIN, &w));
            }

            i = j;

            continue;
        }

        // Alloy's operators, warm; plain punctuation, dim.
        const OPS: &[&str] = &[
            "??=", "...", "?.", "?:", "?[", "?(", "??", "->", "=>", "::", "..", "==", "~=", "<=",
            ">=", "+", "-", "*", "/", "%", "^", "#", "=", "<", ">", "!", "?", "|", "&",
        ];

        if let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) {
            out.push_str(&span(OPERATOR, op));
            i += op.chars().count();

            continue;
        }

        if "(){}[],;:.".contains(c) {
            out.push_str(&span(PUNCT, &c.to_string()));
            i += 1;

            continue;
        }

        out.push(c);
        i += 1;
    }

    out
}

/// A shell line: the command, its subcommand, flags, paths, a comment.
fn paint_shell(line: &str) -> String {
    let (cmd, comment) = match line.find('#') {
        Some(at) => (&line[..at], Some(&line[at..])),
        None => (line, None),
    };
    let mut out = String::new();
    let mut n = 0;
    let mut rest = cmd;

    while !rest.is_empty() {
        let lead = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..lead]);
        rest = &rest[lead..];

        if rest.is_empty() {
            break;
        }

        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let w = &rest[..end];
        n += 1;
        let rgb = if n == 1 {
            CALL
        } else if w.starts_with('-') {
            OPERATOR
        } else if w.contains('/') || w.contains('.') || w.contains('=') {
            STRING
        } else if n == 2 {
            KEYWORD
        } else {
            PLAIN
        };
        out.push_str(&span(rgb, w));
        rest = &rest[end..];
    }

    if let Some(c) = comment {
        out.push_str(&span(COMMENT, c));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_when_color_is_off() {
        assert_eq!(paint("local x = 1", Mode::Alloy, false), "local x = 1");
    }

    #[test]
    fn keywords_strings_and_numbers_take_their_colors() {
        let out = paint("local s = \"hi\" -- c", Mode::Alloy, true);
        assert!(out.contains(&format!("{}local{RESET}", ui::fg(KEYWORD))));
        assert!(out.contains(&format!("{}\"hi\"{RESET}", ui::fg(STRING))));
        assert!(out.contains(&format!("{}-- c{RESET}", ui::fg(COMMENT))));
        let n = paint("x = 42", Mode::Alloy, true);
        assert!(n.contains(&format!("{}42{RESET}", ui::fg(NUMBER))));
    }

    #[test]
    fn a_member_named_like_a_keyword_is_plain() {
        let out = paint("t.end", Mode::Alloy, true);
        assert!(!out.contains(&format!("{}end{RESET}", ui::fg(KEYWORD))));
    }
}
