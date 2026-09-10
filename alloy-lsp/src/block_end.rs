//! Whether a line opens a block that has no `end` yet. The editor asks
//! after Enter and inserts the `end` a line below the cursor.

/// The indentation of the block opener on `line` when the file lacks its
/// `end`, from a walk over the lexer's tokens: strings and comments never
/// count. None when the line opens nothing or the file is balanced.
///
/// The walk stops at the end of the opener line, so a body that sits
/// inside a `namespace`, an `impl`, or any other block answers too. Over
/// the whole file the count alone cannot say which opener the last `end`
/// belongs to: in
///
/// ```text
/// namespace N as
///     function f()
/// end
/// ```
///
/// the `end` closes the namespace, and a count gives it to `f`. The
/// indentation is what tells the two apart, so `closed_below` reads it.
pub fn needs_end(src: &str, line: u32) -> Option<String> {
    // A file with every `end` in place wants none: the reader pressed
    // Enter inside a block that is already whole.
    if open_blocks(src, src.len()).is_empty() {
        return None;
    }

    let stack = open_blocks(src, end_of_line(src, line)?);
    let (opener_line, offset) = *stack.last()?;

    if opener_line != line {
        return None;
    }

    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent: String = src[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    match closed_below(src, offset, &indent) {
        true => None,

        false => Some(indent),
    }
}

/// The byte offset of the line break that ends `line`, or the end of
/// the source for the last line. None when the file has no such line.
fn end_of_line(src: &str, line: u32) -> Option<usize> {
    let mut at = 0;

    for _ in 0..line {
        at += src[at..].find('\n')? + 1;
    }

    if at > src.len() {
        return None;
    }

    Some(src[at..].find('\n').map_or(src.len(), |i| at + i))
}

/// How far an indentation reaches, with a tab as four columns. A file
/// writes one of the two, so the width compares the lines of a body
/// whichever it is.
fn width(indent: &str) -> usize {
    indent.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// Whether the block that opens at `offset` already has its closing
/// word. The scan takes the first line under the opener that is back at
/// the opener's own column or further left: the body sits deeper, so
/// that line either closes the block or belongs to what holds it.
fn closed_below(src: &str, offset: usize, indent: &str) -> bool {
    let own = width(indent);
    let after = match src[offset..].find('\n') {
        Some(i) => &src[offset + i + 1..],

        None => return false,
    };

    for line in after.lines() {
        let text = line.trim_start();

        if text.is_empty() {
            continue;
        }

        if width(&line[..line.len() - text.len()]) > own {
            continue;
        }

        let word = text
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or_default();

        // `else` and `elseif` carry an `if` on to its own `end`, and
        // `until` closes a `repeat`.
        return width(&line[..line.len() - text.len()]) == own
            && matches!(word, "end" | "else" | "elseif" | "until");
    }

    false
}

/// Whether a block is still open at a byte offset, so `end` is a word
/// the line can take.
pub fn open_before(src: &str, offset: usize) -> bool {
    !open_blocks(src, offset).is_empty()
}

/// The openers with no `end` yet, each as its line and byte offset, over
/// the tokens that start before `until`.
///
/// A word alone does not open a block: `attribute r() on function`,
/// `remote function Buy(id: string) from server`, `declare function
/// print(s: string)`, and `type Handler = function` all write the word
/// `function` and take no body. The walk reads the head of the
/// statement, which is what the parser reads to tell the forms apart.
fn open_blocks(src: &str, until: usize) -> Vec<(u32, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };

    let toks = &lexed.toks;
    let text = |i: usize| &src[toks[i].start as usize..toks[i].end as usize];
    let mut stack: Vec<(u32, usize)> = Vec::new();
    // The statement the token belongs to, as the word it starts with
    // and where that word sits. A statement ends at a line break that
    // no bracket holds open, so a head wrapped over lines keeps.
    let mut head = "";
    let mut head_at = usize::MAX;
    let mut line = 0u32;
    let mut read = 0usize;
    let mut depth = 0i32;

    for i in 0..toks.len() {
        let at = toks[i].start as usize;

        if at >= until {
            break;
        }

        let breaks = src[read..at].matches('\n').count() as u32;
        line += breaks;
        read = at;

        if i == 0 || (breaks > 0 && depth <= 0) {
            head_at = i;
            head = text(i);

            // `export` and `global` carry the declaration behind
            // them; the word after is the one that says what the
            // statement is.
            if matches!(head, "export" | "global") && i + 1 < toks.len() {
                head_at = i + 1;
                head = text(i + 1);
            }
        }

        let word = text(i);
        let before = i.checked_sub(1).map(text).unwrap_or("");
        let after = toks.get(i + 1).map(|_| text(i + 1)).unwrap_or("");

        match word {
            "(" | "[" | "{" => depth += 1,

            ")" | "]" | "}" => depth -= 1,

            // A `function` in a type slot names a shape, not a body.
            // `declare function`, `remote function`, and the target of
            // an `attribute` declare a signature and stop there. A
            // `type` alias takes a body only as `type function f(t)`.
            "function"
                if !matches!(before, ":" | "->" | "|" | "&")
                    && !matches!(head, "remote" | "attribute")
                    && !(head == "declare" && head_at + 1 == i)
                    && !(head == "type" && head_at + 1 != i) =>
            {
                stack.push((line, at));
            }

            // `attribute r() on struct` names the target of the
            // declaration; the words open no block there.
            "struct" | "enum" | "interface" | "trait" | "impl" | "macro" | "match"
            | "namespace"
                if head != "attribute" =>
            {
                stack.push((line, at));
            }

            // `class Name`, `open class Name`, and `declare class Name`
            // all take members up to an `end`. `local class = 1` and
            // `t.class` write the same word as a name.
            "class"
                if !matches!(before, "." | ":" | "->" | "|" | "&")
                    && after.starts_with(|c: char| c.is_alphabetic() || c == '_')
                    && after != "end" =>
            {
                stack.push((line, at));
            }

            // `declare extern type Name with ... end`: the members
            // close with an `end`, the way a class does.
            "extern" if head == "declare" => {
                stack.push((line, at));
            }

            "do" | "repeat" => {
                stack.push((line, at));
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
                stack.push((line, at));
            }

            "end" | "until" => {
                stack.pop();
            }

            _ => {}
        }
    }

    stack
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declaration that takes no body writes a word a block also
    /// writes. The head of the statement tells the two apart.
    #[test]
    fn a_bodyless_declaration_wants_no_end() {
        for src in [
            "attribute ratelimit(n: number) on function\n",
            "attribute tag(name: string) on struct\n",
            "attribute mark() on enum\n",
            "attribute mark() on interface\n",
            "export attribute tag(n: string) on struct\n",
            "remote function Buy(id: string) from server\n",
            "remote Hit(target: Player) from client\n",
            "export remote function Buy(id: string) from server\n",
            "declare function print(s: string)\n",
            "declare Players: Instance\n",
            "type F = (x: number) -> ()\n",
            "type Handler = function\n",
            "local f: (a: number) -> number = g\n",
            "local f: function = g\n",
            "import { a } from './b'\n",
            "export { a, b }\n",
            "if x then return end\n",
        ] {
            assert_eq!(needs_end(src, 0), None, "{src:?}");
        }

        // A head wrapped over lines is still the head of its statement.
        let wrapped = "attribute r(\n    n: number\n) on function\n";

        assert_eq!(needs_end(wrapped, 2), None, "{wrapped:?}");

        let remote = "remote function Buy(\n    id: string\n) from server\n";

        assert_eq!(needs_end(remote, 2), None, "{remote:?}");
    }

    #[test]
    fn an_open_block_on_the_line_wants_an_end() {
        for src in [
            "function f()\n",
            "local function f()\n",
            "export function f()\n",
            "type function pick(t)\n",
            "if x then\n",
            "for i = 1, 2 do\n\n",
            "while ok do\n",
            "do\n",
            "repeat\n",
            "struct V as\n",
            "impl V as\n",
            "trait Priced as\n",
            "enum Phase as\n",
            "interface Named as\n",
            "match x with\n",
            "macro twice(x)\n",
            "class Player\n",
            "open class Player\n",
            "declare class Player\n",
            "declare extern type Player with\n",
        ] {
            assert_eq!(needs_end(src, 0).as_deref(), Some(""), "{src:?}");
        }

        assert_eq!(needs_end("    if x then\n", 0).as_deref(), Some("    "));
        assert_eq!(
            needs_end("function f()\n    local g = function()\n", 1).as_deref(),
            Some("    ")
        );
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

    /// A body inside a block that already has its own `end` answers
    /// too. The count gives the last `end` to the inner opener; the
    /// column says it belongs to the namespace, the impl, or the trait.
    #[test]
    fn an_opener_inside_a_closed_block_wants_its_own_end() {
        for (src, line, want) in [
            ("namespace N as\n    function f()\nend\n", 1, "    "),
            ("impl V as\n    function V.new()\nend\n", 1, "    "),
            ("trait Show as\n    function show(self)\nend\n", 1, "    "),
            (
                "namespace A as\n    namespace B as\n        function f()\n    end\nend\n",
                2,
                "        ",
            ),
            (
                "struct V as\n    n: number\nend\n\nimpl V as\n    function V.scale(self)\nend\n",
                5,
                "    ",
            ),
            ("namespace N as\n\tfunction f()\nend\n", 1, "\t"),
            (
                "namespace N as\n    function f()\n        if x then\n    end\nend\n",
                2,
                "        ",
            ),
        ] {
            assert_eq!(needs_end(src, line).as_deref(), Some(want), "{src:?}");
        }
    }

    /// The `end` of the opener is there already: the next line back at
    /// the opener's column closes it, whatever stands between.
    #[test]
    fn an_opener_that_already_closes_wants_nothing() {
        for (src, line) in [
            ("namespace N as\n    function f()\n    end\nend\n", 1),
            (
                "namespace N as\n    function f()\n        local x = 1\n    end\n",
                1,
            ),
            // The `if` carries on to its own `end` through the `else`.
            ("namespace N as\n    if x then\n    else\n    end\n", 1),
            ("namespace N as\n    repeat\n    until x\n", 1),
        ] {
            assert_eq!(needs_end(src, line), None, "{src:?}");
        }
    }
}
