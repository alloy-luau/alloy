//! Whether a line opens a block that has no `end` yet. The editor asks
//! after Enter and inserts the `end` a line below the cursor.

/// The indentation of the block opener on `line` when the file lacks its
/// `end`, from a walk over the lexer's tokens: strings and comments never
/// count. None when the line opens nothing or the file is balanced.
pub fn needs_end(src: &str, line: u32) -> Option<String> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return None;
    };

    let toks = &lexed.toks;
    let text = |i: usize| &src[toks[i].start as usize..toks[i].end as usize];
    let line_of = |offset: usize| src[..offset].matches('\n').count() as u32;
    let mut stack: Vec<(u32, usize)> = Vec::new();

    for (i, tok) in toks.iter().enumerate() {
        let word = text(i);
        let before = i.checked_sub(1).map(text).unwrap_or("");
        let _ = tok;

        match word {
            "function" | "do" | "repeat" | "struct" | "enum" | "interface" | "trait" | "impl"
            | "macro" | "match" => {
                stack.push((line_of(toks[i].start as usize), toks[i].start as usize));
            }

            // An `if` expression closes with `else`, not `end`: it sits
            // after an operator or an opening bracket.
            "if" if !matches!(
                before,
                "=" | "("
                    | ","
                    | "["
                    | "{"
                    | "return"
                    | "and"
                    | "or"
                    | "not"
                    | ".."
                    | "+"
                    | "-"
                    | "*"
                    | "/"
                    | "%"
                    | "^"
                    | "=="
                    | "~="
                    | "<"
                    | ">"
                    | "<="
                    | ">="
                    | "??"
            ) =>
            {
                stack.push((line_of(toks[i].start as usize), toks[i].start as usize));
            }

            "end" | "until" => {
                stack.pop();
            }

            _ => {}
        }
    }

    let (opener_line, offset) = *stack.last()?;

    if opener_line != line {
        return None;
    }

    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent: String = src[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    Some(indent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_block_on_the_line_wants_an_end() {
        assert_eq!(needs_end("local function f()\n", 0).as_deref(), Some(""));
        assert_eq!(needs_end("    if x then\n", 0).as_deref(), Some("    "));
        assert_eq!(needs_end("for i = 1, 2 do\n\n", 0).as_deref(), Some(""));
        assert_eq!(needs_end("struct V as\n", 0).as_deref(), Some(""));
        assert_eq!(needs_end("macro twice(x)\n", 0).as_deref(), Some(""));
    }

    #[test]
    fn a_balanced_file_wants_nothing() {
        assert_eq!(needs_end("if x then\nend\n", 0), None);
        assert_eq!(needs_end("enum Color as Red, Green end\n", 0), None);
        assert_eq!(needs_end("local x = if a then 1 else 2\n", 0), None);
        assert_eq!(needs_end("local s = \"if then\"\n", 0), None);
        assert_eq!(needs_end("-- function f()\n", 0), None);
    }

    #[test]
    fn only_the_opener_line_answers() {
        assert_eq!(needs_end("function f()\n    local x = 1\n", 1), None);
        assert_eq!(
            needs_end("function f()\n    local x = 1\n", 0).as_deref(),
            Some("")
        );
    }
}
