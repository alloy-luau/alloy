//! The block structure of a token stream: what opens a block, what
//! closes it, and how deep each token sits. The formatter indents from
//! it and the linter finds a function's body with it.

use alloy_syntax::lexer::{Tok, TokKind};

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
    /// For each token, the zero-based line it starts on.
    pub lines: Vec<usize>,
}

/// The zero-based line each token starts on, in one pass over the
/// source. A scan from the start per token would be quadratic.
pub fn token_lines(src: &str, toks: &[Tok]) -> Vec<usize> {
    let mut lines = Vec::with_capacity(toks.len());
    let mut line = 0usize;
    let mut at = 0usize;

    for t in toks {
        let start = t.start as usize;

        if start > at {
            line += src[at..start].matches('\n').count();
            at = start;
        }

        lines.push(line);
    }

    lines
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
    let lines = token_lines(src, toks);

    for (i, t) in toks.iter().enumerate() {
        let text = t.text(src);
        let prev = if i > 0 {
            Some(toks[i - 1].text(src))
        } else {
            None
        };
        let next = toks.get(i + 1).map(|n| n.text(src));
        let line = lines[i];
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
                "function" if signature_only(src, toks, i, &stack, &lines) => {}

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

    Structure { steps, ends, lines }
}

/// A `function` line with no body: a `declare function`, a method of a
/// declared class, or a trait signature. A trait signature is followed
/// by another `function`, an attribute, or the `end` of the trait.
fn signature_only(src: &str, toks: &[Tok], i: usize, stack: &[Frame], lines: &[usize]) -> bool {
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
            let line = lines[i];
            let next_line = (i + 1..toks.len())
                .find(|&j| lines[j] > line)
                .map(|j| &toks[j]);

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
