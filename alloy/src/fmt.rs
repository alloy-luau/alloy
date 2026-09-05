//! `alloy fmt`: a conservative formatter.
//!
//! The formatter changes whitespace and nothing else: it reindents every
//! line from the block structure, strips trailing spaces, turns tabs into
//! four spaces, collapses runs of blank lines, and ends the file with one
//! newline. The tokens of the file stay the same, so a second run is a
//! no-op, and the output holds the same program.
//!
//! Lines inside a long string or a long comment keep their text.

use alloy_syntax::lexer::{Lexed, Tok, TokKind, lex};

/// Spaces per indentation level.
pub const INDENT: usize = 4;

/// What a frame on the block stack is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `function`, before its parameter list closed.
    FnHead,
    /// A function body.
    Fn,
    /// `if`, before `then`.
    IfHead,
    /// The body of an `if`, `elseif`, or `else`.
    IfBody,
    /// `if` in expression position: `then` and `else` change no depth,
    /// and no `end` closes it.
    ExprIf,
    /// `while` or `for`, before `do`.
    LoopHead,
    /// A block that `end` closes: a loop body, `do`, `repeat`, a
    /// declaration, an `impl`, a `trait`, a `macro`.
    Block,
    /// `match`, before `with`.
    MatchHead,
    /// The arms of a `match`.
    Match,
    /// One `case` or `default` arm.
    Arm,
    /// `(`, `{`, `[`, or the head of an interpolated string.
    Bracket,
}

/// One open block.
#[derive(Debug, Clone, Copy)]
struct Frame {
    kind: Kind,
    /// Levels the frame indents its contents by: one, or zero for a
    /// bracket that another opener on the same line carries.
    weight: usize,
    /// The index of the token that opened it.
    open_at: usize,
    /// The line the frame opened on.
    line: usize,
    /// The frame took the indent of the bracket below it; a pop on the
    /// same line gives it back.
    took_below: bool,
}

/// The block structure of a token stream.
pub struct Structure {
    /// For each token: the depth before it, the levels it closes, and
    /// the levels it opens.
    pub steps: Vec<Step>,
    /// For each token that opens a block, the index of the token that
    /// closes it.
    pub ends: Vec<Option<usize>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Step {
    pub depth_before: usize,
    pub closes: usize,
    pub opens: usize,
}

/// Tokens after which an `if` or a `function` is an expression.
fn expression_context(prev: Option<&str>) -> bool {
    match prev {
        None => false,

        Some(p) => matches!(
            p,
            "=" | "("
                | ","
                | "["
                | "{"
                | "return"
                | "and"
                | "or"
                | "not"
                | "+"
                | "-"
                | "*"
                | "/"
                | "//"
                | "%"
                | "^"
                | ".."
                | "=="
                | "~="
                | "<"
                | ">"
                | "<="
                | ">="
                | "??"
                | "?"
                | ":"
                | "in"
                | "?("
                | "?["
        ),
    }
}

/// Computes the block structure of `toks`.
pub fn structure(src: &str, toks: &[Tok]) -> Structure {
    let mut stack: Vec<Frame> = Vec::new();
    let mut steps = vec![Step::default(); toks.len()];
    let mut ends = vec![None; toks.len()];
    let mut depth = 0usize;
    let line_of = |t: &Tok| src[..t.start as usize].matches('\n').count();

    for (i, t) in toks.iter().enumerate() {
        let text = t.text(src);
        let prev = if i > 0 {
            Some(toks[i - 1].text(src))
        } else {
            None
        };
        let next = toks.get(i + 1).map(|n| n.text(src));
        let line = line_of(t);
        let member = matches!(prev, Some("." | ":" | "?." | "?:"));
        let mut closes = 0usize;
        let mut opens = 0usize;

        let top = |stack: &Vec<Frame>| stack.last().map(|f| f.kind);

        // Pops the top frame and records where it closed.
        let mut pop = |stack: &mut Vec<Frame>, closes: &mut usize, opens: &mut usize| {
            if let Some(f) = stack.pop() {
                *closes += f.weight;
                ends[f.open_at] = Some(i);

                // `f(g(1),` on one line: `g(` took the indent of `f(`,
                // and gives it back when it closes on that line.
                if f.took_below
                    && f.line == line
                    && let Some(below) = stack.last_mut()
                    && below.kind == Kind::Bracket
                    && below.weight == 0
                {
                    below.weight = 1;
                    *opens += 1;
                }
            }
        };

        // An open expression `if` ends at the first closer past it.
        let closer = matches!(text, "end" | "until" | ")" | "]" | "}")
            || matches!(t.kind, TokKind::InterpTail);

        if closer {
            while top(&stack) == Some(Kind::ExprIf) {
                pop(&mut stack, &mut closes, &mut opens);
            }
        }

        let push = |stack: &mut Vec<Frame>,
                    kind: Kind,
                    weight: usize,
                    opens: &mut usize,
                    closes: &mut usize| {
            // A bracket opened on this line, with another opener on top
            // of it, carries no indent of its own: `foo(function()` and
            // `foo({` indent their contents once.
            let mut took_below = false;

            if let Some(last) = stack.last_mut()
                && last.kind == Kind::Bracket
                && last.line == line
                && last.weight > 0
                && weight > 0
            {
                last.weight = 0;
                *closes += 1;
                took_below = true;
            }

            stack.push(Frame {
                kind,
                weight,
                open_at: i,
                line,
                took_below,
            });
            *opens += weight;
        };

        match t.kind {
            TokKind::InterpHead => push(&mut stack, Kind::Bracket, 1, &mut opens, &mut closes),

            TokKind::InterpMid => {
                if top(&stack) == Some(Kind::Bracket) {
                    pop(&mut stack, &mut closes, &mut opens);
                }

                push(&mut stack, Kind::Bracket, 1, &mut opens, &mut closes);
            }

            TokKind::InterpTail => {
                if top(&stack) == Some(Kind::Bracket) {
                    pop(&mut stack, &mut closes, &mut opens);
                }
            }

            _ if member => {}

            _ => match text {
                "function" if signature_only(src, toks, i, &stack) => {}

                "function" => {
                    // The parameter list follows the name path, if any.
                    let mut j = i + 1;

                    while toks.get(j).is_some_and(|n| {
                        matches!(n.kind, TokKind::Ident | TokKind::Dot | TokKind::Colon)
                    }) {
                        j += 1;
                    }

                    if toks.get(j).map(|n| n.text(src)) == Some("(") {
                        // The body indents once the parameter list closed.
                        push(&mut stack, Kind::FnHead, 0, &mut opens, &mut closes);
                    } else {
                        push(&mut stack, Kind::Fn, 1, &mut opens, &mut closes);
                    }
                }

                "if" => {
                    let in_expr = expression_context(prev)
                        || (matches!(prev, Some("then" | "else"))
                            && top(&stack) == Some(Kind::ExprIf));

                    if in_expr {
                        push(&mut stack, Kind::ExprIf, 0, &mut opens, &mut closes);
                    } else {
                        push(&mut stack, Kind::IfHead, 1, &mut opens, &mut closes);
                    }
                }

                "then" => {
                    if let Some(f) = stack.last_mut() {
                        if f.kind == Kind::IfHead {
                            f.kind = Kind::IfBody;
                        }
                    }
                }

                "elseif" | "else" => {
                    match stack.last_mut() {
                        Some(f) if matches!(f.kind, Kind::IfBody | Kind::IfHead) => {
                            closes += f.weight;
                            opens += f.weight;
                            f.kind = if text == "else" {
                                Kind::IfBody
                            } else {
                                Kind::IfHead
                            };
                        }

                        Some(f) if f.kind == Kind::ExprIf => {}

                        // `local P(x) = v else ... end`: the let-else block.
                        _ if text == "else" => {
                            push(&mut stack, Kind::Block, 1, &mut opens, &mut closes);
                        }

                        _ => {}
                    }
                }

                "for" if line_has_before(src, toks, i, "impl") => {}

                "while" | "for" => push(&mut stack, Kind::LoopHead, 1, &mut opens, &mut closes),

                "do" => {
                    if let Some(f) = stack.last_mut()
                        && f.kind == Kind::LoopHead
                    {
                        f.kind = Kind::Block;
                    } else {
                        push(&mut stack, Kind::Block, 1, &mut opens, &mut closes);
                    }
                }

                "repeat" => push(&mut stack, Kind::Block, 1, &mut opens, &mut closes),

                // A declaration opens at the start of a statement; the
                // same words inside `attribute ... on struct, enum` do not.
                "struct" | "enum" | "trait" | "impl" | "interface" | "macro"
                    if first_on_line(src, toks, i) || prev == Some("export") =>
                {
                    push(&mut stack, Kind::Block, 1, &mut opens, &mut closes);
                }

                "class" => {
                    let declares = prev == Some("declare")
                        || (next.is_some_and(is_name)
                            && !matches!(
                                toks.get(i + 2).map(|n| n.text(src)),
                                Some("=" | "(" | "." | ":" | "[" | ",")
                            ));

                    if declares {
                        push(&mut stack, Kind::Block, 1, &mut opens, &mut closes);
                    }
                }

                "match" => push(&mut stack, Kind::MatchHead, 1, &mut opens, &mut closes),

                "with" => {
                    if let Some(f) = stack.last_mut()
                        && f.kind == Kind::MatchHead
                    {
                        f.kind = Kind::Match;
                    } else if line_has_before(src, toks, i, "declare") {
                        push(&mut stack, Kind::Block, 1, &mut opens, &mut closes);
                    }
                }

                "case" | "default" => {
                    if matches!(top(&stack), Some(Kind::Match | Kind::Arm)) {
                        if top(&stack) == Some(Kind::Arm) {
                            pop(&mut stack, &mut closes, &mut opens);
                        }

                        push(&mut stack, Kind::Arm, 1, &mut opens, &mut closes);
                    }
                }

                "until" => {
                    if top(&stack) == Some(Kind::Block) {
                        pop(&mut stack, &mut closes, &mut opens);
                    }
                }

                "end" => {
                    if top(&stack) == Some(Kind::Arm) {
                        pop(&mut stack, &mut closes, &mut opens);
                    }

                    if matches!(
                        top(&stack),
                        Some(
                            Kind::Fn
                                | Kind::FnHead
                                | Kind::IfBody
                                | Kind::IfHead
                                | Kind::LoopHead
                                | Kind::Block
                                | Kind::Match
                                | Kind::MatchHead
                        )
                    ) {
                        pop(&mut stack, &mut closes, &mut opens);
                    }
                }

                _ if text.ends_with('(') || text.ends_with('[') || text.ends_with('{') => {
                    push(&mut stack, Kind::Bracket, 1, &mut opens, &mut closes);
                }

                ")" | "]" | "}" => {
                    if top(&stack) == Some(Kind::Bracket) {
                        pop(&mut stack, &mut closes, &mut opens);
                    }

                    // The parameter list closed: the body indents.
                    if text == ")"
                        && stack
                            .last()
                            .is_some_and(|f| f.kind == Kind::FnHead && f.weight == 0)
                    {
                        let n = stack.len();
                        let fn_line = stack[n - 1].line;

                        // `foo(function(x)`: the call's bracket yields
                        // its indent to the body, as for `foo({`.
                        if n >= 2
                            && stack[n - 2].kind == Kind::Bracket
                            && stack[n - 2].line == fn_line
                            && stack[n - 2].weight > 0
                        {
                            stack[n - 2].weight = 0;
                            stack[n - 1].took_below = true;
                            closes += 1;
                        }

                        stack[n - 1].kind = Kind::Fn;
                        stack[n - 1].weight = 1;
                        opens += 1;
                    }
                }

                _ => {}
            },
        }

        steps[i] = Step {
            depth_before: depth,
            closes,
            opens,
        };
        depth = depth.saturating_sub(closes) + opens;
    }

    Structure { steps, ends }
}

/// A `function` line with no body: a `declare function`, a method of a
/// declared class, or a trait signature. A trait signature is followed
/// by another `function`, an attribute, or the `end` of the trait.
fn signature_only(src: &str, toks: &[Tok], i: usize, stack: &[Frame]) -> bool {
    if ["declare", "remote", "attribute"]
        .iter()
        .any(|w| line_has_before(src, toks, i, w))
    {
        return true;
    }

    let Some(top) = stack.last() else {
        return false;
    };
    let opener = toks[top.open_at].text(src);

    match opener {
        "class" | "with" => true,

        "trait" => {
            let line = src[..toks[i].start as usize].matches('\n').count();
            let next_line = toks
                .iter()
                .skip(i + 1)
                .find(|t| src[..t.start as usize].matches('\n').count() > line);

            match next_line.map(|t| t.text(src)) {
                Some("function" | "end" | "@") => true,
                Some(t) => t.starts_with('@'),
                None => true,
            }
        }

        _ => false,
    }
}

/// Whether token `i` is the first on its line.
fn first_on_line(src: &str, toks: &[Tok], i: usize) -> bool {
    match i.checked_sub(1) {
        None => true,

        Some(p) => src[toks[p].end as usize..toks[i].start as usize].contains('\n'),
    }
}

fn is_name(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// Whether a token before `i` on the same line is `word`.
fn line_has_before(src: &str, toks: &[Tok], i: usize, word: &str) -> bool {
    let line_start = src[..toks[i].start as usize]
        .rfind('\n')
        .map_or(0, |p| p + 1);

    toks[..i]
        .iter()
        .rev()
        .take_while(|t| t.start as usize >= line_start)
        .any(|t| t.text(src) == word)
}

/// Tokens that continue the expression of the line before, when they
/// start a line.
fn continues(text: &str) -> bool {
    matches!(
        text,
        "+" | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "and"
            | "or"
            | "=="
            | "~="
            | "<"
            | ">"
            | "<="
            | ">="
            | "??"
            | "?"
            | ":"
            | "."
            | "->"
            | "=>"
            | "?."
            | "?:"
            | "?["
            | "?("
    )
}

/// Tokens after which the next line continues the expression. `?`,
/// `<`, and `>` are not here: a line ends with them in a type far more
/// often than in an expression.
fn leaves_open(text: &str) -> bool {
    matches!(
        text,
        "=" | "+"
            | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "and"
            | "or"
            | "not"
            | "=="
            | "~="
            | "<="
            | ">="
            | "??"
            | "->"
            | "=>"
    )
}

/// Formats Alloy source. `Err` carries the lexer's message: a file that
/// does not lex stays as it is.
pub fn format(src: &str) -> Result<String, String> {
    let Lexed { toks, comments } = lex(src).map_err(|e| e.message)?;
    let st = structure(src, &toks);
    let lines: Vec<&str> = src.split('\n').collect();

    // Lines inside a long string or a long comment keep their text.
    let mut protected = vec![false; lines.len()];
    let line_of = |offset: usize| src[..offset].matches('\n').count();

    for (a, b) in toks
        .iter()
        .map(|t| (t.start as usize, t.end as usize))
        .chain(comments.iter().map(|(a, b)| (*a as usize, *b as usize)))
    {
        let (la, lb) = (line_of(a), line_of(b.min(src.len())));

        for p in protected.iter_mut().take(lb + 1).skip(la + 1) {
            *p = true;
        }
    }

    // The indent of each line: from the first token on it, or, for a
    // comment line, from the depth at the next token.
    let mut indents = vec![None::<usize>; lines.len()];
    let mut prev_tok: Option<usize> = None;

    for (i, t) in toks.iter().enumerate() {
        let line = line_of(t.start as usize);

        if indents[line].is_none() {
            let step = st.steps[i];
            let mut level = step.depth_before.saturating_sub(step.closes);
            let text = t.text(src);
            let after_open = prev_tok.is_some_and(|p| leaves_open(toks[p].text(src)));

            if continues(text) || (after_open && !is_closer(text)) {
                level += 1;
            }

            indents[line] = Some(level);
        }

        prev_tok = Some(i);
    }

    let mut depth_after_line = vec![0usize; lines.len()];

    for (i, t) in toks.iter().enumerate() {
        let line = line_of(t.start as usize);
        let step = st.steps[i];
        depth_after_line[line] = step.depth_before.saturating_sub(step.closes) + step.opens;
    }

    let mut running = 0usize;
    let first_token_line: Vec<Option<usize>> = (0..lines.len())
        .map(|line| toks.iter().position(|t| line_of(t.start as usize) == line))
        .collect();

    for line in 0..lines.len() {
        if indents[line].is_none() {
            // A comment or blank line: the depth after the last token
            // line before it. Before a `case`, an `else`, or another
            // arm-level word, it takes that line's indent instead; before
            // an `end` it stays in the body.
            let mut d = running;
            let next = (line + 1..lines.len()).find(|&l| indents[l].is_some());

            if let Some(n) = next
                && let Some(d_next) = indents[n]
                && d_next < d
                && let Some(t) = first_token_line[n]
                && !is_closer(toks[t].text(src))
            {
                d = d_next;
            }

            indents[line] = Some(d);
        }

        if toks.iter().any(|t| line_of(t.start as usize) == line) {
            running = depth_after_line[line];
        }
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len());

    for (i, raw) in lines.iter().enumerate() {
        if protected[i] {
            out.push((*raw).to_string());

            continue;
        }

        let body = raw.trim_start_matches([' ', '\t']).trim_end();

        if body.is_empty() {
            out.push(String::new());

            continue;
        }

        let level = indents[i].unwrap_or(0);
        out.push(format!("{}{}", " ".repeat(level * INDENT), body));
    }

    // Blank lines: none at the start, none at the end, one at most in a
    // row, and one newline to close the file.
    let mut collapsed: Vec<String> = Vec::with_capacity(out.len());

    for (i, line) in out.iter().enumerate() {
        let blank = line.is_empty() && !protected[i];

        if blank && collapsed.last().is_none_or(|l| l.is_empty()) {
            continue;
        }

        collapsed.push(line.clone());
    }

    while collapsed.last().is_some_and(|l| l.is_empty()) {
        collapsed.pop();
    }

    if collapsed.is_empty() {
        return Ok(String::new());
    }

    let mut text = collapsed.join("\n");
    text.push('\n');

    Ok(text)
}

fn is_closer(text: &str) -> bool {
    matches!(text, "end" | "until" | ")" | "]" | "}" | "else" | "elseif")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format(s).unwrap()
    }

    #[test]
    fn reindents_blocks() {
        let src = "local function f(x)\nif x then\nreturn 1\nelseif x == 2 then\nreturn 2\nelse\nreturn 3\nend\nend\n";
        let want = "local function f(x)\n    if x then\n        return 1\n    elseif x == 2 then\n        return 2\n    else\n        return 3\n    end\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn match_arms_indent_once_and_bodies_twice() {
        let src = "match m with\ncase Ok(v) then\nprint(v)\ncase Err(e) then print(e)\ndefault\nprint(0)\nend\n";
        let want = "match m with\n    case Ok(v) then\n        print(v)\n    case Err(e) then print(e)\n    default\n        print(0)\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn a_callback_argument_indents_once() {
        let src = "foo(function()\nbar()\nend)\nlocal t = {\na = 1,\n}\n";
        let want = "foo(function()\n    bar()\nend)\nlocal t = {\n    a = 1,\n}\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn an_expression_if_opens_nothing() {
        let src = "local x = if a then 1 else 2\nlocal y = 3\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn continuation_lines_indent_once() {
        let src = "local x = a\n+ b\nlocal s = obj\n:method()\n:other()\n";
        let want = "local x = a\n    + b\nlocal s = obj\n    :method()\n    :other()\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn long_strings_keep_their_lines() {
        let src = "local s = [[\n  keep\n\tthis  \n]]\nprint(s)\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn blank_lines_collapse_and_the_file_ends_with_one_newline() {
        let src = "\n\nlocal a = 1\n\n\n\nlocal b = 2\n\n\n";
        assert_eq!(fmt(src), "local a = 1\n\nlocal b = 2\n");
    }

    #[test]
    fn a_struct_and_a_trait_indent_their_members() {
        let src = "struct P as\nx: number\nend\ntrait S\nfunction area(self): number\nfunction d(self)\nreturn 1\nend\nend\nstruct R as w: number end\n";
        let want = "struct P as\n    x: number\nend\ntrait S\n    function area(self): number\n    function d(self)\n        return 1\n    end\nend\nstruct R as w: number end\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn a_multi_line_parameter_list_indents_once() {
        let src = "local function f(\na: number,\nb: number\n)\nreturn a\nend\n";
        let want = "local function f(\n    a: number,\n    b: number\n)\n    return a\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn formatting_is_idempotent_on_the_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples");

        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aly") {
                let src = std::fs::read_to_string(&path).unwrap();
                let once = format(&src).unwrap();
                let twice = format(&once).unwrap();
                assert_eq!(once, twice, "{}", path.display());
            }
        }
    }
}
