//! Flux: the lints that name a habit and show the Alloy form of it.
//!
//! Each lint here reads the token stream for one shape of Luau that
//! Alloy has a word for: `a and a.b` for `a?.b`, `if f then f() end`
//! for `f?()`, `typeof(x) == "T"` for `x is T`. When the rewrite keeps
//! the program the same, the lint carries it as a `Fix` and
//! `alloy lint --fix` applies it. When it would not, the message shows
//! the form and leaves the change to the author.
//!
//! The names and levels sit in `lint::LINTS` with the other lints.

use alloy_syntax::lexer::{Tok, TokKind};

use crate::desugar::PRIMITIVES;
use crate::lint::{Fix, Lint};
use crate::roblox_classes::DATATYPES;

/// Runs the Flux lints on one file.
pub fn run(src: &str, toks: &[Tok]) -> Vec<Lint> {
    let s = Scan { src, toks };
    let mut out = Vec::new();
    s.manual_safe_access(&mut out);
    s.manual_coalesce(&mut out);
    s.and_or_ternary(&mut out);
    s.manual_child_lookup(&mut out);
    s.nil_check_call(&mut out);
    s.manual_type_test(&mut out);
    s.legacy_iterator(&mut out);
    s.manual_floor_div(&mut out);
    s.manual_push(&mut out);
    s.concat_interpolation(&mut out);
    s.raw_pcall(&mut out);
    s.raw_require(&mut out);
    s.manual_class(&mut out);
    s.explicit_any(&mut out);
    out
}

struct Scan<'s> {
    src: &'s str,
    toks: &'s [Tok],
}

const KEYWORDS: &[&str] = &[
    "and", "or", "not", "if", "then", "else", "elseif", "end", "for", "in", "while", "do",
    "repeat", "until", "return", "break", "continue", "local", "function", "nil", "true", "false",
];

/// Tokens after which the next token starts an expression.
const EXPR_BEFORE: &[&str] = &[
    "=", "(", ",", "[", "{", "return", "and", "or", "not", "+", "-", "*", "/", "//", "%", "^",
    "..", "==", "~=", "<", ">", "<=", ">=", "?", ":", "in", "then", "else", "local", "until",
    "while", "if", "elseif",
];

impl<'s> Scan<'s> {
    fn t(&self, i: usize) -> &'s str {
        self.toks.get(i).map(|t| t.text(self.src)).unwrap_or("")
    }

    fn at(&self, i: usize, text: &str) -> bool {
        self.t(i) == text
    }

    fn is_name(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == TokKind::Ident) && !KEYWORDS.contains(&self.t(i))
    }

    fn prev(&self, i: usize) -> &'s str {
        if i == 0 { "" } else { self.t(i - 1) }
    }

    fn start(&self, i: usize) -> u32 {
        self.toks[i].start
    }

    fn end(&self, i: usize) -> u32 {
        self.toks[i.min(self.toks.len() - 1)].end
    }

    /// The source text of the tokens `a..b`.
    fn slice(&self, a: usize, b: usize) -> &'s str {
        if a >= b || a >= self.toks.len() {
            return "";
        }

        &self.src[self.start(a) as usize..self.end(b - 1) as usize]
    }

    fn line_of(&self, i: usize) -> usize {
        self.src[..self.start(i.min(self.toks.len() - 1)) as usize]
            .matches('\n')
            .count()
    }

    /// Whether `i` starts a statement: nothing before it on the line, or
    /// a token that ends one.
    fn statement_start(&self, i: usize) -> bool {
        i == 0
            || self.line_of(i - 1) != self.line_of(i)
            || matches!(
                self.prev(i),
                "then" | "do" | "else" | "end" | ";" | "repeat"
            )
    }

    /// A name and its `.name` members: `a.b.c`. The end is exclusive.
    fn path_end(&self, i: usize) -> Option<usize> {
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
    fn same_path(&self, a: usize, a_end: usize, b: usize) -> Option<usize> {
        let n = a_end - a;

        for k in 0..n {
            if self.t(a + k) != self.t(b + k) || self.toks.get(b + k).is_none() {
                return None;
            }
        }

        Some(b + n)
    }

    fn matching(&self, open: usize) -> Option<usize> {
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
    fn expr_end(&self, i: usize) -> Option<usize> {
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
    fn string_content(&self, i: usize) -> Option<&'s str> {
        let text = self.t(i);

        if text.len() >= 2 && (text.starts_with('"') || text.starts_with('\'')) {
            Some(&text[1..text.len() - 1])
        } else {
            None
        }
    }

    fn lint(
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

    // --- the lints -----------------------------------------------------------------------

    /// `a and a.b` is `a?.b`.
    fn manual_safe_access(&self, out: &mut Vec<Lint>) {
        let mut i = 0;

        while i < self.toks.len() {
            let Some(p_end) = self.path_end(i) else {
                i += 1;

                continue;
            };

            if matches!(self.prev(i), "." | ":" | "function" | "local") || !self.at(p_end, "and") {
                i += 1;

                continue;
            }

            let Some(q_end) = self.same_path(i, p_end, p_end + 1) else {
                i += 1;

                continue;
            };

            if !(matches!(self.t(q_end), "." | ":") && self.is_name(q_end + 1)) {
                i += 1;

                continue;
            }

            let path = self.slice(i, p_end);
            let member = self.t(q_end + 1);
            let op = self.t(q_end);
            let chain_end = self.expr_end(p_end + 1).unwrap_or(q_end + 2);
            let followed_by_or = self.at(chain_end, "or");
            let fix = if followed_by_or {
                None
            } else {
                Some(format!("{path}?"))
            };
            let message = if followed_by_or {
                format!(
                    "`{path} and {path}{op}{member} or x` is `{path}?{op}{member} ?? x` when `{member}` is never false"
                )
            } else {
                format!("`{path} and {path}{op}{member}` is `{path}?{op}{member}`")
            };
            self.lint(out, "manual_safe_access", i, q_end - 1, message, fix);
            i = q_end + 1;
        }
    }

    /// `if x == nil then x = v end` is `x ??= v`.
    fn manual_coalesce(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "if") || !self.statement_start(i) {
                continue;
            }

            let Some(p_end) = self.path_end(i + 1) else {
                continue;
            };

            if !(self.at(p_end, "==") && self.at(p_end + 1, "nil") && self.at(p_end + 2, "then")) {
                continue;
            }

            let Some(q_end) = self.same_path(i + 1, p_end, p_end + 3) else {
                continue;
            };

            if !self.at(q_end, "=") {
                continue;
            }

            // The value runs to `end`, with no block in between.
            let mut e = q_end + 1;
            let mut depth = 0i32;
            let mut simple = true;

            while e < self.toks.len() {
                let text = self.t(e);

                if matches!(text, "(" | "[" | "{") {
                    depth += 1;
                } else if matches!(text, ")" | "]" | "}") {
                    depth -= 1;
                } else if depth == 0 && text == "end" {
                    break;
                } else if matches!(
                    text,
                    "function" | "if" | "do" | "while" | "for" | "repeat" | "match"
                ) {
                    simple = false;

                    break;
                }

                e += 1;
            }

            if !simple || e >= self.toks.len() || e == q_end + 1 {
                continue;
            }

            let path = self.slice(i + 1, p_end);
            let value = self.slice(q_end + 1, e).trim();
            self.lint(
                out,
                "manual_coalesce",
                i,
                e,
                format!("`if {path} == nil then {path} = ... end` is `{path} ??= ...`"),
                Some(format!("{path} ??= {value}")),
            );
        }
    }

    /// `c and a or b` is `c ? a : b`.
    fn and_or_ternary(&self, out: &mut Vec<Lint>) {
        for k in 0..self.toks.len() {
            if !self.at(k, "and") {
                continue;
            }

            let Some(a_end) = self.expr_end(k + 1) else {
                continue;
            };

            if !self.at(a_end, "or") {
                continue;
            }

            // The condition runs back to the start of the expression.
            let mut c = k;

            while c > 0
                && !EXPR_BEFORE.contains(&self.prev(c))
                && self.line_of(c - 1) == self.line_of(c)
            {
                c -= 1;
            }

            if c == k || matches!(self.prev(c), "?" | ":") {
                continue;
            }

            // `a and a.b or c` belongs to manual_safe_access.
            if let Some(p_end) = self.path_end(c)
                && p_end == k
                && self.same_path(c, p_end, k + 1).is_some()
            {
                continue;
            }

            let Some(b_end) = self.expr_end(a_end + 1) else {
                continue;
            };
            let boundary = b_end >= self.toks.len()
                || matches!(
                    self.t(b_end),
                    ")" | "," | "]" | "}" | "end" | "then" | "else" | "do"
                )
                || self.line_of(b_end) != self.line_of(b_end - 1);

            if !boundary {
                continue;
            }

            let cond = self.slice(c, k);
            let yes = self.slice(k + 1, a_end);
            let no = self.slice(a_end + 1, b_end);
            let truthy = self.toks[k + 1].kind != TokKind::Ident && a_end == k + 2
                || matches!(self.t(k + 1), "true" | "{" | "[")
                || self.toks[k + 1].kind == TokKind::InterpHead;
            let rewrite = format!("{cond} ? {yes} : {no}");
            let message = if truthy {
                format!("`{cond} and {yes} or {no}` is `{rewrite}`")
            } else {
                format!(
                    "`and ... or` yields `{no}` when `{yes}` is false or nil; `{rewrite}` is the ternary"
                )
            };
            self.lint(
                out,
                "and_or_ternary",
                c,
                b_end - 1,
                message,
                truthy.then_some(rewrite),
            );
        }
    }

    /// `:FindFirstChild("X")` is `->X`; `:WaitForChild("X")` is `=>X`.
    fn manual_child_lookup(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, ":") {
                continue;
            }

            let arrow = match self.t(i + 1) {
                "FindFirstChild" => "->",
                "WaitForChild" => "=>",
                _ => continue,
            };

            if !(self.at(i + 2, "(") && self.at(i + 4, ")")) {
                continue;
            }

            let Some(name) = self.string_content(i + 3) else {
                continue;
            };
            let valid = name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');

            if !valid {
                continue;
            }

            let call = self.t(i + 1);
            self.lint(
                out,
                "manual_child_lookup",
                i,
                i + 4,
                format!("`:{call}(\"{name}\")` is `{arrow}{name}`"),
                Some(format!("{arrow}{name}")),
            );
        }
    }

    /// `if f then f(x) end` is `f?(x)`.
    fn nil_check_call(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "if") || !self.statement_start(i) {
                continue;
            }

            let Some(p_end) = self.path_end(i + 1) else {
                continue;
            };

            if !self.at(p_end, "then") {
                continue;
            }

            let Some(q_end) = self.same_path(i + 1, p_end, p_end + 1) else {
                continue;
            };

            if !self.at(q_end, "(") {
                continue;
            }

            let Some(close) = self.matching(q_end) else {
                continue;
            };

            if !self.at(close + 1, "end") {
                continue;
            }

            let path = self.slice(i + 1, p_end);
            let args = self.slice(q_end + 1, close);
            self.lint(
                out,
                "nil_check_call",
                i,
                close + 1,
                format!("`if {path} then {path}(...) end` is `{path}?(...)`"),
                Some(format!("{path}?({args})")),
            );
        }
    }

    /// `typeof(x) == "T"` is `x is T`.
    fn manual_type_test(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let call = self.t(i);

            if !matches!(call, "type" | "typeof")
                || matches!(self.prev(i), "." | ":")
                || !self.at(i + 1, "(")
            {
                continue;
            }

            let Some(close) = self.matching(i + 1) else {
                continue;
            };

            if self.path_end(i + 2) != Some(close) {
                continue;
            }

            let op = self.t(close + 1);

            if !matches!(op, "==" | "~=") {
                continue;
            }

            let Some(name) = self.string_content(close + 2) else {
                continue;
            };
            let known = name == "nil"
                || PRIMITIVES.contains(&name)
                || (call == "typeof" && (name == "Instance" || DATATYPES.contains(&name)));

            if !known {
                continue;
            }

            let x = self.slice(i + 2, close);
            let test = if op == "==" { "is" } else { "is not" };
            self.lint(
                out,
                "manual_type_test",
                i,
                close + 2,
                format!("`{call}({x}) {op} \"{name}\"` is `{x} {test} {name}`"),
                Some(format!("{x} {test} {name}")),
            );
        }
    }

    /// `for k, v in pairs(t) do` is `for k, v in t do`.
    fn legacy_iterator(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let call = self.t(i);

            if !matches!(call, "pairs" | "ipairs") || self.prev(i) != "in" || !self.at(i + 1, "(") {
                continue;
            }

            let Some(close) = self.matching(i + 1) else {
                continue;
            };

            if !self.at(close + 1, "do") {
                continue;
            }

            let inner = self.slice(i + 2, close).trim();
            self.lint(
                out,
                "legacy_iterator",
                i,
                close,
                format!(
                    "`in {call}({inner})` is `in {inner}`; Luau iterates a table without a wrapper"
                ),
                Some(inner.to_string()),
            );
        }
    }

    /// `math.floor(a / b)` is `a // b`.
    fn manual_floor_div(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !(self.at(i, "math")
                && self.at(i + 1, ".")
                && self.at(i + 2, "floor")
                && self.at(i + 3, "("))
            {
                continue;
            }

            let Some(close) = self.matching(i + 3) else {
                continue;
            };
            let mut depth = 0i32;
            let mut slash = None;
            let mut plain = true;

            for j in i + 4..close {
                let text = self.t(j);

                if matches!(text, "(" | "[" | "{") {
                    depth += 1;
                } else if matches!(text, ")" | "]" | "}") {
                    depth -= 1;
                } else if depth == 0 {
                    if text == "/" && slash.is_none() {
                        slash = Some(j);
                    } else if matches!(text, "/" | "*" | "%" | "//") && slash.is_some()
                        || matches!(
                            text,
                            "+" | ".."
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
                                | ","
                        )
                        || (text == "-" && j != i + 4)
                    {
                        plain = false;
                    }
                }
            }

            let (Some(slash), true) = (slash, plain) else {
                continue;
            };
            let a = self.slice(i + 4, slash).trim();
            let b = self.slice(slash + 1, close).trim();
            let tight = matches!(self.prev(i), "*" | "/" | "//" | "%" | "^" | "#" | "-")
                || matches!(self.t(close + 1), "*" | "/" | "//" | "%" | "^");
            let rewrite = if tight {
                format!("({a} // {b})")
            } else {
                format!("{a} // {b}")
            };
            self.lint(
                out,
                "manual_floor_div",
                i,
                close,
                format!("`math.floor({a} / {b})` is `{a} // {b}`"),
                Some(rewrite),
            );
        }
    }

    /// `table.insert(xs, v)` on an Array is `xs:push(v)`.
    fn manual_push(&self, out: &mut Vec<Lint>) {
        // The names the file declares with an Array type.
        let mut arrays: Vec<&str> = Vec::new();

        for i in 0..self.toks.len() {
            if !self.is_name(i) {
                continue;
            }

            if self.at(i + 1, ":") {
                let mut j = i + 2;
                let mut depth = 0i32;

                while j < self.toks.len() {
                    let text = self.t(j);

                    if matches!(text, "(" | "[" | "{" | "<") {
                        depth += 1;
                    } else if matches!(text, ")" | "]" | "}" | ">") {
                        depth -= 1;
                    }

                    if depth < 0
                        || (depth == 0 && matches!(text, "=" | ","))
                        || self.line_of(j) != self.line_of(i)
                    {
                        break;
                    }

                    j += 1;
                }

                let array = (self.at(j - 1, "]") && self.at(j - 2, "["))
                    || (self.at(i + 2, "Array") && self.at(i + 3, "<"));

                if array {
                    arrays.push(self.t(i));
                }
            } else if self.prev(i) == "local" && self.at(i + 1, "=") && self.at(i + 2, "[") {
                arrays.push(self.t(i));
            }
        }

        if arrays.is_empty() {
            return;
        }

        for i in 0..self.toks.len() {
            if !(self.at(i, "table") && self.at(i + 1, ".") && self.at(i + 3, "(")) {
                continue;
            }

            let call = self.t(i + 2);
            let Some(close) = self.matching(i + 3) else {
                continue;
            };
            let target = self.t(i + 4);

            if !arrays.contains(&target) {
                continue;
            }

            let commas = (i + 4..close)
                .filter(|j| self.at(*j, ",") && self.matching_depth(i + 3, *j) == 1)
                .count();

            match (call, commas) {
                ("insert", 1) => {
                    let comma = (i + 4..close).find(|j| self.at(*j, ",")).unwrap_or(close);
                    let value = self.slice(comma + 1, close).trim();
                    self.lint(
                        out,
                        "manual_push",
                        i,
                        close,
                        format!("`{target}` is an Array; `table.insert({target}, v)` is `{target}:push(v)`"),
                        Some(format!("{target}:push({value})")),
                    );
                }

                ("remove", 0) if self.at(i + 5, ")") => {
                    self.lint(
                        out,
                        "manual_push",
                        i,
                        close,
                        format!(
                            "`{target}` is an Array; `table.remove({target})` is `{target}:pop()`"
                        ),
                        Some(format!("{target}:pop()")),
                    );
                }

                _ => {}
            }
        }
    }

    /// The bracket depth of `j` relative to the opener at `open`.
    fn matching_depth(&self, open: usize, j: usize) -> i32 {
        let mut depth = 0i32;

        for k in open..j {
            let text = self.t(k);

            if matches!(text, "(" | "[" | "{") {
                depth += 1;
            } else if matches!(text, ")" | "]" | "}") {
                depth -= 1;
            }
        }

        depth
    }

    /// `"a" .. x .. "b"` is `` `a{x}b` ``.
    fn concat_interpolation(&self, out: &mut Vec<Lint>) {
        let mut i = 0;

        while i < self.toks.len() {
            let Some(first_end) = self.expr_end(i) else {
                i += 1;

                continue;
            };

            if !self.at(first_end, "..")
                || matches!(self.prev(i), "." | ":" | "..")
                || self.line_of(first_end) != self.line_of(i)
            {
                i += 1;

                continue;
            }

            let mut parts = vec![(i, first_end)];
            let mut e = first_end;
            let mut ok = true;

            while self.at(e, "..") {
                let Some(next) = self.expr_end(e + 1) else {
                    ok = false;

                    break;
                };

                if self.line_of(next - 1) != self.line_of(i) {
                    ok = false;

                    break;
                }

                parts.push((e + 1, next));
                e = next;
            }

            if !ok || parts.len() < 2 {
                i = e.max(i + 1);

                continue;
            }

            let mut text = String::from("`");
            let mut literals = 0;
            let mut names = 0;

            for (a, b) in &parts {
                if let Some(content) = self.string_content(*a).filter(|_| b - a == 1) {
                    if content.contains(['`', '{', '}', '\\']) {
                        ok = false;

                        break;
                    }

                    literals += 1;
                    text.push_str(content);
                } else {
                    let raw = if self.at(*a, "tostring")
                        && self.at(a + 1, "(")
                        && self.matching(a + 1) == Some(b - 1)
                    {
                        self.slice(a + 2, b - 1)
                    } else {
                        self.slice(*a, *b)
                    };

                    if raw.contains(['`', '{', '}']) {
                        ok = false;

                        break;
                    }

                    names += 1;
                    text.push('{');
                    text.push_str(raw.trim());
                    text.push('}');
                }
            }

            text.push('`');

            if ok && literals > 0 && names > 0 {
                self.lint(
                    out,
                    "concat_interpolation",
                    i,
                    e - 1,
                    format!("a `..` chain with a literal reads as one string: {text}"),
                    Some(text),
                );
            }

            i = e.max(i + 1);
        }
    }

    /// `pcall(f)` yields a flag and a value; `Result.pcall(f)` a Result.
    fn raw_pcall(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let call = self.t(i);

            if !matches!(call, "pcall" | "xpcall")
                || matches!(self.prev(i), "." | ":" | "function" | "local")
                || !self.at(i + 1, "(")
            {
                continue;
            }

            self.lint(
                out,
                "raw_pcall",
                i,
                i,
                format!("`{call}` yields a flag and a value; `Result.pcall(f, ...)` yields a Result, and `try` unwraps it"),
                None,
            );
        }
    }

    /// `local X = require("./x")` is `import X from "./x"`.
    fn raw_require(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "require")
                || matches!(self.prev(i), "." | ":" | "function" | "local")
                || !self.at(i + 1, "(")
            {
                continue;
            }

            let Some(close) = self.matching(i + 1) else {
                continue;
            };
            let path = self.string_content(i + 2).filter(|_| close == i + 3);
            let binding = i >= 3
                && self.at(i - 1, "=")
                && self.is_name(i - 2)
                && self.at(i - 3, "local")
                && self.statement_start(i - 3);
            let statement_ends =
                close + 1 >= self.toks.len() || self.line_of(close + 1) != self.line_of(close);

            match path {
                Some(p) if binding && statement_ends => {
                    let name = self.t(i - 2);
                    self.lint(
                        out,
                        "raw_require",
                        i - 3,
                        close,
                        format!("`local {name} = require(\"{p}\")` is `import {name} from \"{p}\"`, which the checker follows"),
                        Some(format!("import {name} from \"{p}\"")),
                    );
                }

                Some(p) => self.lint(
                    out,
                    "raw_require",
                    i,
                    close,
                    format!("`import {{ name }} from \"{p}\"` binds what the file uses, with its types; `require` binds the whole module"),
                    None,
                ),

                None => self.lint(
                    out,
                    "raw_require",
                    i,
                    close,
                    "`require` of an instance path resolves at runtime; `import ... from \"./path\"` resolves at build time and the checker follows it".to_string(),
                    None,
                ),
            }
        }
    }

    /// `X.__index = X` is the class idiom; `struct X` writes it.
    fn manual_class(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !(self.is_name(i)
                && self.at(i + 1, ".")
                && self.at(i + 2, "__index")
                && self.at(i + 3, "=")
                && self.t(i + 4) == self.t(i))
            {
                continue;
            }

            let name = self.t(i);
            self.lint(
                out,
                "manual_class",
                i,
                i + 4,
                format!("`{name}.__index = {name}` is the class idiom by hand; `struct {name} as ... end` and `impl {name}` write it with types"),
                None,
            );
        }
    }

    /// `: any` turns the checker off; `unknown` with `is` keeps it on.
    fn explicit_any(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if self.t(i) == "any" && matches!(self.prev(i), ":" | "::") && !self.at(i + 1, "_cast")
            {
                self.lint(
                    out,
                    "explicit_any",
                    i,
                    i,
                    "`any` turns the checker off for this value; `unknown` keeps it on, and `is` narrows it".to_string(),
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::apply_fixes;

    fn lints(src: &str) -> Vec<crate::Lint> {
        crate::compile(src).unwrap().lints
    }

    fn fixed(src: &str) -> String {
        apply_fixes(src, &lints(src)).0
    }

    fn names(src: &str) -> Vec<&'static str> {
        lints(src).iter().map(|l| l.name).collect()
    }

    #[test]
    fn a_guarded_index_becomes_safe_access() {
        assert_eq!(fixed("local n = p and p.Name\n"), "local n = p?.Name\n");
        assert_eq!(
            fixed("local n = a.b and a.b:c(1)\n"),
            "local n = a.b?:c(1)\n"
        );
        assert_eq!(
            names("local n = p and p.Name or \"x\"\n"),
            vec!["manual_safe_access"]
        );
        assert_eq!(
            fixed("local n = p and p.Name or \"x\"\n"),
            "local n = p and p.Name or \"x\"\n"
        );
    }

    #[test]
    fn a_nil_check_assignment_becomes_coalesce_assign() {
        assert_eq!(fixed("if t.x == nil then t.x = 1 end\n"), "t.x ??= 1\n");
        assert_eq!(
            fixed("if x == nil then\n    x = f(1)\nend\n"),
            "x ??= f(1)\n"
        );
    }

    #[test]
    fn and_or_becomes_a_ternary_when_the_middle_is_truthy() {
        assert_eq!(
            fixed("local s = ok and \"yes\" or \"no\"\n"),
            "local s = ok ? \"yes\" : \"no\"\n"
        );
        assert_eq!(
            names("local s = ok and value or fallback\n"),
            vec!["and_or_ternary"]
        );
        assert_eq!(
            fixed("local s = ok and value or fallback\n"),
            "local s = ok and value or fallback\n"
        );
    }

    #[test]
    fn child_lookups_become_arrows() {
        assert_eq!(
            fixed("local m = workspace:FindFirstChild(\"Map\")\n"),
            "local m = workspace->Map\n"
        );
        assert_eq!(
            fixed("local m = workspace:WaitForChild(\"Map\")\n"),
            "local m = workspace=>Map\n"
        );
        assert_eq!(
            names("local m = workspace:FindFirstChild(\"Map\", true)\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_guarded_call_becomes_an_optional_call() {
        assert_eq!(fixed("if cb then cb(1, 2) end\n"), "cb?(1, 2)\n");
        assert_eq!(
            fixed("if self.on_done then\n    self.on_done()\nend\n"),
            "self.on_done?()\n"
        );
    }

    #[test]
    fn type_tests_become_is() {
        assert_eq!(
            fixed("if typeof(x) == \"Instance\" then end\n"),
            "if x is Instance then end\n"
        );
        assert_eq!(
            fixed("if type(x) ~= \"string\" then end\n"),
            "if x is not string then end\n"
        );
        assert_eq!(
            fixed("if typeof(x) == \"Vector3\" then end\n"),
            "if x is Vector3 then end\n"
        );
    }

    #[test]
    fn pairs_and_ipairs_go() {
        assert_eq!(
            fixed("for k, v in pairs(t) do end\n"),
            "for k, v in t do end\n"
        );
        assert_eq!(
            fixed("for i, v in ipairs(t.list) do end\n"),
            "for i, v in t.list do end\n"
        );
    }

    #[test]
    fn floor_of_a_division_is_floor_division() {
        assert_eq!(fixed("local q = math.floor(a / b)\n"), "local q = a // b\n");
        assert_eq!(
            fixed("local q = 2 * math.floor(a / b)\n"),
            "local q = 2 * (a // b)\n"
        );
        assert_eq!(
            names("local q = math.floor(a / b + 1)\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn inserts_on_an_array_become_push() {
        assert_eq!(
            fixed("local xs: number[] = []\ntable.insert(xs, 1)\n"),
            "local xs: number[] = []\nxs:push(1)\n"
        );
        assert_eq!(
            fixed("local t = {}\ntable.insert(t, 1)\n"),
            "local t = {}\ntable.insert(t, 1)\n"
        );
    }

    #[test]
    fn a_concat_chain_becomes_an_interpolated_string() {
        assert_eq!(
            fixed("print(\"Hello \" .. name .. \"!\")\n"),
            "print(`Hello {name}!`)\n"
        );
        assert_eq!(
            fixed("print(\"n = \" .. tostring(n))\n"),
            "print(`n = {n}`)\n"
        );
        assert_eq!(names("print(a .. b)\n"), Vec::<&str>::new());
    }

    #[test]
    fn the_message_only_lints_fire() {
        assert_eq!(names("local ok, e = pcall(f)\n"), vec!["raw_pcall"]);
        assert_eq!(names("local M = {}\nM.__index = M\n"), vec!["manual_class"]);
        assert_eq!(
            fixed("local X = require(\"./x\")\n"),
            "import X from \"./x\"\n"
        );
        assert_eq!(names("local x: any = 1\n"), vec!["explicit_any"]);
    }

    #[test]
    fn the_legacy_globals_take_their_replacements() {
        assert_eq!(
            fixed("wait(1)\nlocal a = unpack(t)\n"),
            "task.wait(1)\nlocal a = table.unpack(t)\n"
        );
    }
}
