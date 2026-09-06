//! The token scanner the Flux lints share: one file's tokens, its
//! block structure, and the small questions every lint asks of them.

use alloy_syntax::lexer::{Tok, TokKind};

use crate::fmt_structure::Structure;
use crate::lint::{Fix, Lint};

/// One file's tokens with the helpers every Flux lint reads them
/// through.
pub(crate) struct Scan<'s> {
    pub(crate) src: &'s str,
    pub(crate) toks: &'s [Tok],
    pub(crate) st: &'s Structure,
}

pub(crate) const KEYWORDS: &[&str] = &[
    "and", "or", "not", "if", "then", "else", "elseif", "end", "for", "in", "while", "do",
    "repeat", "until", "return", "break", "continue", "local", "function", "nil", "true", "false",
    "private", "public",
];

/// Tokens that end the expression before them when walked backwards:
/// a condition runs back to one of these.
pub(crate) const COND_BEFORE: &[&str] = &[
    "=", "(", ",", "[", "{", "return", "then", "else", "elseif", "local", "until", "while", "if",
    "in", "?", ":", "do", "end", ";",
];

/// Tokens after which an `if` is an expression.
pub(crate) const EXPR_IF_BEFORE: &[&str] = &[
    "=", "(", ",", "[", "{", "return", "and", "or", "not", "+", "-", "*", "/", "//", "%", "^",
    "..", "==", "~=", "<", ">", "<=", ">=", "??", "?", ":", "in", "?(", "?[",
];

/// Tokens that close the block a statement sits in.
pub(crate) const CLOSERS: &[&str] = &["end", "else", "elseif", "until", "case", "default"];

/// The parts of one `if` statement: the token after each branch's
/// keyword, and the `end`.
pub(crate) struct IfParts {
    /// The `then` of the `if`.
    pub then: usize,
    /// Each `elseif` with its `then`.
    pub elseifs: Vec<(usize, usize)>,
    /// The `else`, if any.
    pub else_at: Option<usize>,
    pub end: usize,
}

impl<'s> Scan<'s> {
    pub(crate) fn new(src: &'s str, toks: &'s [Tok], st: &'s Structure) -> Self {
        Self { src, toks, st }
    }

    pub(crate) fn t(&self, i: usize) -> &'s str {
        self.toks.get(i).map(|t| t.text(self.src)).unwrap_or("")
    }

    pub(crate) fn at(&self, i: usize, text: &str) -> bool {
        self.t(i) == text
    }

    pub(crate) fn is_name(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == TokKind::Ident) && !KEYWORDS.contains(&self.t(i))
    }

    pub(crate) fn prev(&self, i: usize) -> &'s str {
        if i == 0 { "" } else { self.t(i - 1) }
    }

    pub(crate) fn start(&self, i: usize) -> u32 {
        self.toks[i].start
    }

    pub(crate) fn end(&self, i: usize) -> u32 {
        self.toks[i.min(self.toks.len() - 1)].end
    }

    /// The source text of the tokens `a..b`.
    pub(crate) fn slice(&self, a: usize, b: usize) -> &'s str {
        if a >= b || a >= self.toks.len() {
            return "";
        }

        &self.src[self.start(a) as usize..self.end(b - 1) as usize]
    }

    pub(crate) fn line_of(&self, i: usize) -> usize {
        self.st.lines[i.min(self.toks.len() - 1)]
    }

    /// Whether `i` starts a statement: nothing before it on the line, or
    /// a token that ends one.
    pub(crate) fn statement_start(&self, i: usize) -> bool {
        i == 0
            || self.line_of(i - 1) != self.line_of(i)
            || matches!(
                self.prev(i),
                "then" | "do" | "else" | "end" | ";" | "repeat"
            )
    }

    /// A name and its `.name` members: `a.b.c`. The end is exclusive.
    pub(crate) fn path_end(&self, i: usize) -> Option<usize> {
        if !self.is_name(i) {
            return None;
        }

        let mut j = i + 1;

        while self.at(j, ".") && self.is_name(j + 1) {
            j += 2;
        }

        Some(j)
    }

    /// Whether the tokens from `b` spell the same path as `a..a_end`;
    /// the end of the second path when they do.
    pub(crate) fn same_path(&self, a: usize, a_end: usize, b: usize) -> Option<usize> {
        let n = a_end - a;

        for k in 0..n {
            if self.t(a + k) != self.t(b + k) || self.toks.get(b + k).is_none() {
                return None;
            }
        }

        Some(b + n)
    }

    pub(crate) fn matching(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;

        for i in open..self.toks.len() {
            let text = self.t(i);

            if matches!(text, "(" | "[" | "{") || text.ends_with('(') || text.ends_with('[') {
                depth += 1;
            } else if matches!(text, ")" | "]" | "}") {
                depth -= 1;

                if depth == 0 {
                    return Some(i);
                }
            }
        }

        None
    }

    /// The exclusive end of a simple expression at `i`: a literal, a
    /// name with members, calls and indexes, or a bracket group; with
    /// one prefix `-`, `#`, or `not`.
    pub(crate) fn expr_end(&self, i: usize) -> Option<usize> {
        let mut j = i;

        if matches!(self.t(j), "-" | "#" | "not") {
            j += 1;
        }

        let t = self.toks.get(j)?;
        let text = t.text(self.src);

        match t.kind {
            TokKind::Str { .. } | TokKind::InterpStr | TokKind::Number => j += 1,

            TokKind::Ident if matches!(text, "true" | "false" | "nil") => j += 1,

            TokKind::Ident if self.is_name(j) => j += 1,

            TokKind::InterpHead => {
                while j < self.toks.len() && self.toks[j].kind != TokKind::InterpTail {
                    j += 1;
                }

                j += 1;
            }

            _ if matches!(text, "(" | "{" | "[") => j = self.matching(j)? + 1,

            _ => return None,
        }

        loop {
            let text = self.t(j);

            let same_line = self.line_of(j) == self.line_of(j - 1);
            let group = matches!(text, "(" | "[") || (text == "{" && self.is_name(j - 1));

            if matches!(text, "." | ":") && self.is_name(j + 1) {
                j += 2;
            } else if group && same_line {
                j = self.matching(j)? + 1;
            } else {
                return Some(j);
            }
        }
    }

    /// The content of a plain string literal at `i`, without its quotes.
    pub(crate) fn string_content(&self, i: usize) -> Option<&'s str> {
        let text = self.t(i);

        if text.len() >= 2 && (text.starts_with('"') || text.starts_with('\'')) {
            Some(&text[1..text.len() - 1])
        } else {
            None
        }
    }

    pub(crate) fn lint(
        &self,
        out: &mut Vec<Lint>,
        name: &'static str,
        a: usize,
        b: usize,
        message: String,
        fix: Option<String>,
    ) {
        out.push(Lint {
            name,
            start: self.start(a),
            end: self.end(b),
            message,
            fix: fix.map(|replacement| Fix {
                start: self.start(a),
                end: self.end(b),
                replacement,
            }),
        });
    }

    /// The source between the previous token and this one: whitespace
    /// and comments.
    pub(crate) fn gap_before(&self, i: usize) -> &'s str {
        let from = if i == 0 { 0 } else { self.end(i - 1) as usize };
        let to = self
            .toks
            .get(i)
            .map_or(self.src.len(), |t| t.start as usize);

        &self.src[from..to]
    }

    /// Whether a comment sits between token `a` and token `b`.
    pub(crate) fn comment_between(&self, a: usize, b: usize) -> bool {
        if a >= b {
            return false;
        }

        let from = self.end(a) as usize;
        let to = self
            .toks
            .get(b)
            .map_or(self.src.len(), |t| t.start as usize);

        from < to && self.src[from..to].contains("--")
    }

    /// Every comment in the file: its byte range and text. The lexer
    /// skips comments, so they sit in the gaps between tokens.
    pub(crate) fn comments(&self) -> Vec<(u32, u32, &'s str)> {
        let mut out = Vec::new();
        let mut from = 0usize;
        let starts: Vec<(usize, usize)> = self
            .toks
            .iter()
            .map(|t| (t.start as usize, t.end as usize))
            .chain(std::iter::once((self.src.len(), self.src.len())))
            .collect();

        for (start, end) in starts {
            let gap = &self.src[from.min(start)..start];
            let mut offset = from.min(start);
            let mut rest = gap;

            while let Some(i) = rest.find("--") {
                let at = offset + i;
                let text = &rest[i..];
                let len = if text.starts_with("--[[") || text.starts_with("--[=") {
                    text.find("]]").map_or(text.len(), |e| e + 2)
                } else {
                    text.find('\n').unwrap_or(text.len())
                };
                out.push((at as u32, (at + len) as u32, &text[..len]));
                rest = &text[len..];
                offset = at + len;
            }

            from = end;
        }

        out
    }

    /// Whether `if` at `i` is a statement, not an `if` expression. After
    /// `then` or `else` it is the branch of an `if` expression only when
    /// one opens earlier on the same line.
    pub(crate) fn is_statement_if(&self, i: usize) -> bool {
        if !self.at(i, "if") || !self.statement_start(i) {
            return false;
        }

        let mut k = i;

        loop {
            let prev = self.prev(k);

            if !matches!(prev, "then" | "else") {
                return !EXPR_IF_BEFORE.contains(&prev);
            }

            // Back to the `if` this `then` or `else` belongs to, on this line.
            let mut j = k - 1;

            while j > 0 && self.line_of(j - 1) == self.line_of(k) && !self.at(j, "if") {
                j -= 1;
            }

            if !self.at(j, "if") || self.line_of(j) != self.line_of(k) {
                return true;
            }

            k = j;
        }
    }

    /// The branches of the `if` statement at `i`. `None` when the
    /// structure lost it, or an `if` expression sits in one of its
    /// conditions.
    pub(crate) fn if_parts(&self, i: usize) -> Option<IfParts> {
        if !self.is_statement_if(i) {
            return None;
        }

        let end = self.st.ends[i]?;
        let mut then = None;
        let mut elseifs: Vec<(usize, usize)> = Vec::new();
        let mut else_at = None;
        let mut pending_elseif: Option<usize> = None;
        let mut j = i + 1;

        while j < end {
            let text = self.t(j);

            // A nested block: skip to its closer.
            if j != i
                && matches!(
                    text,
                    "function" | "if" | "for" | "while" | "repeat" | "do" | "match"
                )
                && !matches!(self.prev(j), "." | ":")
            {
                if text == "if" && !self.is_statement_if(j) {
                    return None;
                }

                match self.st.ends[j] {
                    Some(e) if e > j => {
                        j = e + 1;

                        continue;
                    }

                    _ if text == "function" => {}

                    _ => return None,
                }
            }

            match text {
                "then" => {
                    if then.is_none() {
                        then = Some(j);
                    } else if let Some(e) = pending_elseif.take() {
                        elseifs.push((e, j));
                    }
                }

                "elseif" => pending_elseif = Some(j),

                "else" => else_at = Some(j),

                _ => {}
            }

            j += 1;
        }

        Some(IfParts {
            then: then?,
            elseifs,
            else_at,
            end,
        })
    }

    /// The exclusive end of the statement that starts at `i`: the next
    /// token on a later line that does not continue the expression, a
    /// `;`, or a closer. A block opener inside it skips to its `end`.
    pub(crate) fn statement_end(&self, i: usize) -> usize {
        let mut j = i + 1;
        let mut depth = 0i32;

        while j < self.toks.len() {
            let text = self.t(j);

            if depth == 0 {
                if CLOSERS.contains(&text) || text == ";" {
                    return j;
                }

                if self.line_of(j) != self.line_of(j - 1) {
                    let prev = self.prev(j);
                    let continues = matches!(
                        prev,
                        "," | "("
                            | "["
                            | "{"
                            | "="
                            | ".."
                            | "+"
                            | "-"
                            | "*"
                            | "/"
                            | "//"
                            | "%"
                            | "^"
                            | "and"
                            | "or"
                            | "not"
                            | "=="
                            | "~="
                            | "<"
                            | ">"
                            | "<="
                            | ">="
                            | "?"
                            | ":"
                            | "??"
                            | "."
                    ) || matches!(text, "." | ":" | "?." | "?:");

                    if !continues {
                        return j;
                    }
                }

                let opener = matches!(
                    text,
                    "function" | "for" | "while" | "repeat" | "do" | "match"
                ) || (text == "if" && self.is_statement_if(j));

                if opener
                    && !matches!(self.prev(j), "." | ":")
                    && let Some(e) = self.st.ends[j]
                    && e > j
                {
                    j = e + 1;

                    continue;
                }
            }

            if matches!(text, "(" | "[" | "{") || text.ends_with('(') || text.ends_with('[') {
                depth += 1;
            } else if matches!(text, ")" | "]" | "}") {
                depth -= 1;
            }

            j += 1;
        }

        self.toks.len()
    }

    /// The start of the expression that ends right before `k`, walked
    /// back on one line to a token that cannot be inside it.
    pub(crate) fn expr_start_before(&self, k: usize) -> usize {
        let mut c = k;

        while c > 0
            && !COND_BEFORE.contains(&self.prev(c))
            && self.line_of(c - 1) == self.line_of(c)
        {
            c -= 1;
        }

        c
    }

    /// For each token, how many block openers enclose it: `function`,
    /// `if`, loops, `match`, `do`. A function's own body starts at one.
    pub(crate) fn nesting(&self) -> Vec<usize> {
        let mut nest = vec![0usize; self.toks.len()];

        for (i, e) in self.st.ends.iter().enumerate() {
            let Some(e) = e else { continue };

            if !matches!(
                self.t(i),
                "function" | "if" | "for" | "while" | "repeat" | "do" | "match"
            ) || matches!(self.prev(i), "." | ":")
                || (self.at(i, "if") && !self.is_statement_if(i))
            {
                continue;
            }

            for n in nest.iter_mut().take(*e).skip(i + 1) {
                *n += 1;
            }
        }

        nest
    }

    /// Whether a loop encloses token `j`.
    pub(crate) fn in_loop(&self, j: usize) -> bool {
        self.st.ends.iter().enumerate().any(|(i, e)| {
            matches!(self.t(i), "for" | "while" | "repeat") && e.is_some_and(|e| i < j && j < e)
        })
    }

    /// The function names the file declares, as `function X:name`,
    /// `function name`, or `name = function`.
    pub(crate) fn declared_functions(&self) -> Vec<&'s str> {
        let mut out = Vec::new();

        for i in 0..self.toks.len() {
            if self.at(i, "function") {
                let mut j = i + 1;
                let mut last = None;

                while self.is_name(j) || self.at(j, ".") || self.at(j, ":") {
                    if self.is_name(j) {
                        last = Some(self.t(j));
                    }

                    j += 1;
                }

                if let Some(n) = last {
                    out.push(n);
                }

                if i >= 2 && self.at(i - 1, "=") && self.is_name(i - 2) {
                    out.push(self.t(i - 2));
                }
            }
        }

        out
    }
}
