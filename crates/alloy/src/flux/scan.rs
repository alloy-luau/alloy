//! The token scanner the Flux lints share: one file's tokens, its
//! block structure, and the small questions every lint asks of them.

use alloy_syntax::contextual::binop_priority;
use alloy_syntax::lexer::{Tok, TokKind};

use crate::fmt::structure::Structure;
use crate::lint::{Fix, Lint};

/// One file's tokens with the helpers every Flux lint reads them
/// through.
pub(crate) struct Scan<'s> {
    pub(crate) src: &'s str,
    pub(crate) toks: &'s [Tok],
    pub(crate) st: &'s Structure,
    /// Per struct an imported module declares, its private field names.
    /// The file's own privates come from its tokens instead.
    pub(crate) privates: &'s [(String, Vec<String>)],
    /// The functions the imported modules declare, keyed the way this
    /// file calls them. See `crate::modules::import_callables`.
    pub(crate) callables: &'s [(String, super::Callable)],
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

/// The binary operators that join two expressions into one.
pub(crate) const BINARY_OPS: &[&str] = &[
    "+", "-", "*", "/", "//", "%", "^", "..", "==", "~=", "<", ">", "<=", ">=", "and", "or", "??",
];

/// Tokens that close the block a statement sits in.
pub(crate) const CLOSERS: &[&str] = &["end", "else", "elseif", "until", "case", "default"];

/// The words that go on with the expression or the statement in front
/// of them: a word operator, a clause word, or a word that closes a
/// block. Any other name or keyword after a complete expression opens
/// the next statement.
const GOES_ON: &[&str] = &[
    "and",
    "or",
    "not",
    "in",
    "is",
    "satisfies",
    "bor",
    "bxor",
    "band",
    "shl",
    "shr",
    "then",
    "do",
    "else",
    "elseif",
    "until",
    "end",
    "with",
    "where",
    "as",
];

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
        Self {
            src,
            toks,
            st,
            privates: &[],
            callables: &[],
        }
    }

    /// The same scan, with the private fields of the imported structs.
    pub(crate) fn with_privates(mut self, privates: &'s [(String, Vec<String>)]) -> Self {
        self.privates = privates;
        self
    }

    /// The same scan, with the functions of the imported modules.
    pub(crate) fn with_callables(mut self, callables: &'s [(String, super::Callable)]) -> Self {
        self.callables = callables;
        self
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

    /// Whether `i` starts a statement: nothing before it on the line, a
    /// token that ends one, or a keyword that opens one.
    pub(crate) fn statement_start(&self, i: usize) -> bool {
        i == 0
            || self.line_of(i - 1) != self.line_of(i)
            || matches!(
                self.prev(i),
                "then" | "do" | "else" | "end" | ";" | "repeat"
            )
            // `local a = 1  local b = 2` is two statements on one line.
            // No expression holds a `local` or a `const`, so each one
            // opens a declaration wherever it stands. The binding of a
            // condition is a declaration too, but not a statement: see
            // `cond_binding`.
            || matches!(self.t(i), "local" | "const")
    }

    /// Whether the `local` or `const` at `i` binds in a condition, as in
    /// `if local x = f() then` or `while const v = g() do`. A rewrite of
    /// a whole statement must skip it: the `then` or `do` follows it.
    pub(crate) fn cond_binding(&self, i: usize) -> bool {
        match self.prev(i) {
            "if" | "elseif" | "while" | "not" => true,

            // `if local a = f(); local b = g(a) then` stacks two.
            ";" => (0..i - 1)
                .rev()
                .find(|&k| matches!(self.t(k), "local" | "const"))
                .is_some_and(|k| self.cond_binding(k)),

            _ => false,
        }
    }

    /// The token that ends the block a declaration at `d` stands in: the
    /// first later token that closes its level, such as the `end` of a
    /// `do` or the `else` of an `if`. At the top level, the token count.
    pub(crate) fn scope_end(&self, d: usize) -> usize {
        let level = self.st.steps[d].depth_before;

        (d + 1..self.toks.len())
            .find(|&k| {
                let step = self.st.steps[k];

                step.depth_before.saturating_sub(step.closes) < level
            })
            .unwrap_or(self.toks.len())
    }

    /// The declaration that the name at `at` reads: the last `local`,
    /// `const`, or parameter of the name before it whose block still
    /// holds it. `None` for a name that no such declaration binds.
    pub(crate) fn binding_at(&self, at: usize) -> Option<usize> {
        let name = self.t(at);

        (0..at).rev().find(|&d| {
            self.is_name(d)
                && self.t(d) == name
                && match self.prev(d) {
                    "local" | "const" => at < self.scope_end(d),

                    "(" | "," => self.param_scope(d).is_some_and(|end| at < end),

                    _ => false,
                }
        })
    }

    /// The `end` of the function whose parameter list holds the name at
    /// `d`. `None` when the name is not a parameter.
    fn param_scope(&self, d: usize) -> Option<usize> {
        let f = (0..d).rev().find(|&f| self.at(f, "function"))?;
        let open = (f + 1..d).find(|&j| self.at(j, "("))?;

        if self.matching(open)? < d {
            return None;
        }

        self.st.ends[f]
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

    /// The exclusive end of a whole expression at `i`: the simple
    /// expression and every binary operator that continues it. A value
    /// that ends here is one statement, so a rewrite that replaces the
    /// statement cannot swallow the one after it.
    pub(crate) fn value_end(&self, i: usize) -> Option<usize> {
        let mut j = self.expr_end(i)?;

        while BINARY_OPS.contains(&self.t(j)) {
            j = self.expr_end(j + 1)?;
        }

        Some(j)
    }

    /// Whether the token at `j` goes on with the right operand of an
    /// `and` before it: an operator that binds tighter, such as `>`, `+`,
    /// `..`, `??` or `!=`, or a type suffix, such as `::` or `is`.
    pub(crate) fn binds_tighter_than_and(&self, j: usize) -> bool {
        let t = self.t(j);

        // `and` binds at 2 and `or` at 1: see `binop_priority`.
        binop_priority(t).is_some_and(|(left, _)| left > 2)
            || matches!((t, self.t(j + 1)), ("?", "?") | ("!", "="))
            || matches!(t, "::" | "is" | "satisfies" | "as")
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
            fix: fix.map(|replacement| Fix::new(self.src, self.start(a), self.end(b), replacement)),
        });
    }

    /// The column a token starts at, counted from zero.
    pub(crate) fn indent_of(&self, i: usize) -> usize {
        let at = self.start(i) as usize;

        at - self.src[..at].rfind('\n').map_or(0, |n| n + 1)
    }

    /// The byte range of the whole line a token sits on, the newline
    /// included, when nothing else shares that line. A rewrite that
    /// deletes a statement takes the line with it; one that shares a
    /// line takes the token alone.
    pub(crate) fn whole_line(&self, i: usize) -> (u32, u32) {
        let (start, end) = (self.start(i) as usize, self.end(i) as usize);
        let from = self.src[..start].rfind('\n').map_or(0, |at| at + 1);
        let to = self.src[end..]
            .find('\n')
            .map_or(self.src.len(), |at| end + at + 1);

        if self.src[from..start].trim().is_empty() && self.src[end..to].trim().is_empty() {
            (from as u32, to as u32)
        } else {
            (start as u32, end as u32)
        }
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
                // A `?` or `!` that ends the line above is a type suffix
                // or an assert, never a ternary waiting for its `if`.
                let first_on_line = k == 0 || self.line_of(k - 1) != self.line_of(k);

                return !EXPR_IF_BEFORE.contains(&prev)
                    || (first_on_line && matches!(prev, "?" | "!" | ">" | ">>"));
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

    /// Whether the token at `j` opens a statement on the line of the
    /// token before it: a name or a keyword right after a complete
    /// expression. No expression goes on from `{}` to a name, so
    /// `const out = {} table.insert(out, 1)` is two statements. A word
    /// that may be a name, such as `new` or `match`, completes nothing.
    pub(crate) fn begins_after_expr(&self, j: usize) -> bool {
        use alloy_syntax::contextual::{is_contextual, is_luau_reserved};

        if j == 0 || self.toks[j].kind != TokKind::Ident || GOES_ON.contains(&self.t(j)) {
            return false;
        }

        let prev = self.t(j - 1);

        match self.toks[j - 1].kind {
            TokKind::Number | TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpTail => {
                true
            }

            TokKind::Ident => {
                matches!(prev, "end" | "true" | "false" | "nil")
                    || !(is_luau_reserved(prev) || is_contextual(prev) || GOES_ON.contains(&prev))
            }

            _ => matches!(prev, ")" | "]" | "}" | "..."),
        }
    }

    /// The exclusive end of the statement that starts at `i`: the next
    /// token on a later line that does not continue the expression, a
    /// name or a keyword after a complete expression, a `;`, or a
    /// closer. A block opener inside it skips to its `end`, and an `if`
    /// expression runs to the end of its `else` branch.
    pub(crate) fn statement_end(&self, i: usize) -> usize {
        let mut j = i + 1;
        let mut depth = 0i32;
        // The `if` expressions still waiting for an `else`, and whether
        // the `else` branch of one is open. A long `if` expression sits
        // on several lines, so the tokens say where it ends; the lines
        // do not.
        let mut open_ifs = 0usize;
        let mut else_branch = false;

        while j < self.toks.len() {
            let text = self.t(j);

            if depth == 0 {
                if self.begins_after_expr(j) {
                    return j;
                }

                if text == "if" && !self.is_statement_if(j) {
                    open_ifs += 1;
                } else if open_ifs > 0 && matches!(text, "else" | "elseif") {
                    // The `else` or `elseif` of the expression, not of a
                    // block: it ends no statement, and the branch below
                    // it continues this one.
                    if text == "else" {
                        open_ifs -= 1;
                        else_branch = true;
                    }

                    j += 1;

                    continue;
                }

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
                    ) || matches!(text, "." | ":" | "?." | "?:")
                        || ((open_ifs > 0 || else_branch) && matches!(prev, "then" | "else"));

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

    /// Whether a namespace body encloses token `j`.
    pub(crate) fn in_namespace(&self, j: usize) -> bool {
        self.st
            .ends
            .iter()
            .enumerate()
            .any(|(i, e)| self.at(i, "namespace") && e.is_some_and(|e| i < j && j < e))
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
