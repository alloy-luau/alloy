//! Flux: the correctness, suspicious, and style lints that read block
//! structure. A statement after `return`, a table that sets one key
//! twice, an `if` with two identical branches, a `return` written as an
//! `if`. The names and levels sit in `lint::LINTS`.

use alloy_syntax::lexer::TokKind;

use crate::flux_scan::{CLOSERS, IfParts, Scan};
use crate::lint::Lint;

/// Runs the structure lints on one file.
pub(crate) fn run(s: &Scan) -> Vec<Lint> {
    let mut out = Vec::new();
    s.self_assignment(&mut out);
    s.unreachable_code(&mut out);
    s.constant_condition(&mut out);
    s.duplicate_key(&mut out);
    s.misplaced_not(&mut out);
    s.identical_branches(&mut out);
    s.empty_block(&mut out);
    s.bool_comparison(&mut out);
    s.if_returns(&mut out);
    s.redundant_return(&mut out);
    s.local_then_return(&mut out);
    s.numeric_for_index(&mut out);
    s.unused_variable(&mut out);
    s.naming(&mut out);
    s.private_access(&mut out);
    out
}

/// `playerCount`: starts lowercase, has a capital, has no underscore.
fn is_camel_case(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().any(|c| c.is_ascii_uppercase())
        && !name.contains('_')
}

/// `PlayerState`: starts with a capital and has no underscore.
fn is_pascal_case(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('_')
}

impl<'s> Scan<'s> {
    /// `x = x` and `a.b = a.b`.
    fn self_assignment(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "=") || i == 0 {
                continue;
            }

            let c = self.expr_start_before(i);

            if !self.statement_start(c) || self.at(c, "local") || self.at(c, "const") {
                continue;
            }

            if self.path_end(c) != Some(i) {
                continue;
            }

            let Some(v_end) = self.same_path(c, i, i + 1) else {
                continue;
            };
            let ends = v_end >= self.toks.len()
                || CLOSERS.contains(&self.t(v_end))
                || self.at(v_end, ";")
                || self.line_of(v_end) != self.line_of(v_end - 1);

            if !ends {
                continue;
            }

            let path = self.slice(c, i);
            self.lint(
                out,
                "self_assignment",
                c,
                v_end - 1,
                format!("`{path} = {path}` changes nothing; one side is a typo"),
                None,
            );
        }
    }

    /// A statement after `return`, `break`, or `continue`.
    fn unreachable_code(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let word = self.t(i);

            if !matches!(word, "return" | "break" | "continue")
                || !self.statement_start(i)
                || matches!(self.prev(i), "." | ":")
            {
                continue;
            }

            let mut next = if word == "return" {
                self.statement_end(i)
            } else {
                i + 1
            };

            if self.at(next, ";") {
                next += 1;
            }

            if next >= self.toks.len() || CLOSERS.contains(&self.t(next)) {
                continue;
            }

            // `continue` is a name in plain Luau: `continue()` calls it.
            if word == "continue" && self.at(next, "(") {
                continue;
            }

            let stop = self.statement_end(next);
            self.lint(
                out,
                "unreachable_code",
                next,
                stop.max(next + 1) - 1,
                format!("this statement never runs: `{word}` leaves the block before it"),
                None,
            );
        }
    }

    /// `if true`, `if false`, `if nil`, `while false`.
    fn constant_condition(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let word = self.t(i);

            let (literal, closer) = match word {
                "if" | "elseif" => (self.t(i + 1), "then"),
                "while" => (self.t(i + 1), "do"),
                _ => continue,
            };

            if !matches!(literal, "true" | "false" | "nil") || !self.at(i + 2, closer) {
                continue;
            }

            if matches!(self.prev(i), "." | ":") || (word == "while" && literal == "true") {
                continue;
            }

            let message = if literal == "true" {
                format!("`{word} true` always runs its body")
            } else {
                format!("`{word} {literal}` never runs its body")
            };
            self.lint(out, "constant_condition", i, i + 2, message, None);
        }
    }

    /// A table constructor with one key set twice.
    fn duplicate_key(&self, out: &mut Vec<Lint>) {
        for open in 0..self.toks.len() {
            if !self.at(open, "{") || matches!(self.prev(open), "local" | "const" | "?.") {
                continue;
            }

            let Some(close) = self.matching(open) else {
                continue;
            };
            let mut keys: Vec<(String, usize)> = Vec::new();
            let mut j = open + 1;
            let mut element_start = true;

            while j < close {
                let text = self.t(j);

                if element_start {
                    let key = if self.is_name(j) && self.at(j + 1, "=") {
                        Some((self.t(j).to_string(), j))
                    } else if self.at(j, "[")
                        && self.at(j + 3, "=")
                        && let Some(k) = self.string_content(j + 1)
                        && self.at(j + 2, "]")
                    {
                        Some((k.to_string(), j + 1))
                    } else {
                        None
                    };

                    if let Some((k, at)) = key {
                        if let Some((_, first)) = keys.iter().find(|(n, _)| *n == k) {
                            self.lint(
                                out,
                                "duplicate_key",
                                at,
                                at,
                                format!(
                                    "`{k}` is set twice in this table; the value on line {} is lost",
                                    self.line_of(*first) + 1
                                ),
                                None,
                            );
                        } else {
                            keys.push((k, at));
                        }
                    }

                    element_start = false;
                }

                if matches!(text, "(" | "[" | "{") || text.ends_with('(') || text.ends_with('[') {
                    j = self.matching(j).unwrap_or(close);
                } else if matches!(text, "," | ";") {
                    element_start = true;
                }

                j += 1;
            }
        }
    }

    /// `not a == b` is `(not a) == b`; the test meant is `a ~= b`.
    fn misplaced_not(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "not") {
                continue;
            }

            let Some(a_end) = self.expr_end(i + 1) else {
                continue;
            };
            let op = self.t(a_end);

            if !matches!(op, "==" | "~=") {
                continue;
            }

            let Some(b_end) = self.expr_end(a_end + 1) else {
                continue;
            };
            let a = self.slice(i + 1, a_end);
            let b = self.slice(a_end + 1, b_end);
            let flipped = if op == "==" { "~=" } else { "==" };
            self.lint(
                out,
                "misplaced_not",
                i,
                b_end - 1,
                format!("`not {a} {op} {b}` compares `not {a}` to `{b}`; `{a} {flipped} {b}` is the test"),
                Some(format!("{a} {flipped} {b}")),
            );
        }
    }

    /// The token texts of `a..b`, for a comparison of two ranges.
    fn texts(&self, a: usize, b: usize) -> Vec<&'s str> {
        (a..b.min(self.toks.len())).map(|j| self.t(j)).collect()
    }

    /// An `if` whose `then` and `else` hold the same statements, and a
    /// ternary with the same value on both sides.
    fn identical_branches(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if self.at(i, "?")
                && !self.at(i + 1, "(")
                && let Some(a_end) = self.expr_end(i + 1)
                && self.at(a_end, ":")
                && let Some(b_end) = self.expr_end(a_end + 1)
                && self.texts(i + 1, a_end) == self.texts(a_end + 1, b_end)
            {
                let a = self.slice(i + 1, a_end);
                self.lint(
                    out,
                    "identical_branches",
                    i,
                    b_end - 1,
                    format!("both sides of the ternary are `{a}`; the condition decides nothing"),
                    None,
                );

                continue;
            }

            let Some(IfParts {
                then,
                elseifs,
                else_at: Some(else_at),
                end,
            }) = self.if_parts(i)
            else {
                continue;
            };

            if !elseifs.is_empty() || then + 1 == else_at {
                continue;
            }

            if self.texts(then + 1, else_at) == self.texts(else_at + 1, end) {
                self.lint(
                    out,
                    "identical_branches",
                    i,
                    then,
                    "the `then` and `else` bodies are the same; the condition decides nothing"
                        .to_string(),
                    None,
                );
            }
        }
    }

    /// An `if`, `else`, or loop body with nothing in it.
    fn empty_block(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if let Some(parts) = self.if_parts(i) {
                let after_then = parts
                    .elseifs
                    .first()
                    .map(|(e, _)| *e)
                    .or(parts.else_at)
                    .unwrap_or(parts.end);

                if after_then == parts.then + 1 && !self.comment_between(parts.then, after_then) {
                    self.lint(
                        out,
                        "empty_block",
                        i,
                        parts.then,
                        "this `if` runs nothing; fill the body or drop the branch".to_string(),
                        None,
                    );
                }

                if let Some(e) = parts.else_at
                    && e + 1 == parts.end
                    && !self.comment_between(e, parts.end)
                {
                    self.lint(
                        out,
                        "empty_block",
                        e,
                        e,
                        "an empty `else`; drop it".to_string(),
                        None,
                    );
                }

                continue;
            }

            if matches!(self.t(i), "for" | "while")
                && self.statement_start(i)
                && let Some(end) = self.st.ends[i]
                && self.at(end - 1, "do")
                && end > i + 2
                && !self.comment_between(end - 1, end)
            {
                let word = self.t(i);
                self.lint(
                    out,
                    "empty_block",
                    i,
                    end - 1,
                    format!("this `{word}` loop runs nothing"),
                    None,
                );
            }
        }
    }

    /// `x == true` and `x == false`.
    fn bool_comparison(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let op = self.t(i);

            if !matches!(op, "==" | "~=") {
                continue;
            }

            let (x, x_from, x_to, literal, from, to) = if matches!(self.t(i + 1), "true" | "false")
            {
                let c = self.expr_start_before(i);

                if c == i {
                    continue;
                }

                (self.slice(c, i), c, i, self.t(i + 1), c, i + 1)
            } else if i > 0
                && matches!(self.prev(i), "true" | "false")
                && let Some(e) = self.expr_end(i + 1)
            {
                (self.slice(i + 1, e), i + 1, e, self.prev(i), i - 1, e - 1)
            } else {
                continue;
            };

            let _ = (x_from, x_to);
            let plain = matches!((op, literal), ("==", "true") | ("~=", "false"));
            let form = if plain {
                x.to_string()
            } else {
                format!("not {x}")
            };
            self.lint(
                out,
                "bool_comparison",
                from,
                to,
                format!("`{x} {op} {literal}` is `{form}` when `{x}` is a boolean, and always false otherwise"),
                None,
            );
        }
    }

    /// Whether the tokens `a..b` yield a boolean: a comparison, `not`,
    /// or `is` at depth zero.
    fn is_boolean_expr(&self, a: usize, b: usize) -> bool {
        let mut depth = 0i32;

        for j in a..b {
            let text = self.t(j);

            if matches!(text, "(" | "[" | "{") || text.ends_with('(') || text.ends_with('[') {
                depth += 1;
            } else if matches!(text, ")" | "]" | "}") {
                depth -= 1;
            } else if depth == 0
                && matches!(
                    text,
                    "==" | "~=" | "<" | ">" | "<=" | ">=" | "not" | "is" | "true" | "false"
                )
            {
                return true;
            }
        }

        false
    }

    /// `if c then return a else return b end`: a boolean by hand, or a
    /// ternary by hand.
    fn if_returns(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let Some(IfParts {
                then,
                elseifs,
                else_at: Some(else_at),
                end,
            }) = self.if_parts(i)
            else {
                continue;
            };

            if !elseifs.is_empty()
                || !self.at(then + 1, "return")
                || !self.at(else_at + 1, "return")
            {
                continue;
            }

            let a_end = self.statement_end(then + 1);
            let b_end = self.statement_end(else_at + 1);

            if a_end != else_at || b_end != end || then + 2 == else_at || else_at + 2 == end {
                continue;
            }

            if self.expr_end(then + 2) != Some(else_at) || self.expr_end(else_at + 2) != Some(end) {
                continue;
            }

            let cond = self.slice(i + 1, then);
            let a = self.slice(then + 2, else_at);
            let b = self.slice(else_at + 2, end);
            let boolean = self.is_boolean_expr(i + 1, then);

            match (a, b) {
                ("true", "false") => {
                    let fix = boolean.then(|| format!("return {cond}"));
                    let message = if boolean {
                        format!(
                            "`if {cond} then return true else return false end` is `return {cond}`"
                        )
                    } else {
                        format!(
                            "the `if` converts `{cond}` to a boolean by hand; `return {cond} == true` says it in one statement"
                        )
                    };
                    self.lint(out, "needless_bool", i, end, message, fix);
                }

                ("false", "true") => {
                    let form = if self.expr_end(i + 1) == Some(then) {
                        format!("return not {cond}")
                    } else {
                        format!("return not ({cond})")
                    };
                    self.lint(
                        out,
                        "needless_bool",
                        i,
                        end,
                        format!("`if {cond} then return false else return true end` is `{form}`"),
                        Some(form.clone()),
                    );
                }

                _ => {
                    let rewrite = format!("return {cond} ? {a} : {b}");
                    self.lint(
                        out,
                        "manual_ternary_return",
                        i,
                        end,
                        format!("two returns that differ in the value are one: `{rewrite}`"),
                        Some(rewrite.clone()),
                    );
                }
            }
        }
    }

    /// A bare `return` before the function's `end`.
    fn redundant_return(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "function") || matches!(self.prev(i), "." | ":") {
                continue;
            }

            let Some(end) = self.st.ends[i] else {
                continue;
            };

            if end < i + 2 || !self.at(end - 1, "return") || !self.statement_start(end - 1) {
                continue;
            }

            self.lint(
                out,
                "redundant_return",
                end - 1,
                end - 1,
                "a bare `return` at the end of a function does what the `end` does; delete it"
                    .to_string(),
                Some(String::new()),
            );
        }
    }

    /// `local x = v` then `return x`.
    fn local_then_return(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "local")
                || !self.statement_start(i)
                || !self.is_name(i + 1)
                || !self.at(i + 2, "=")
            {
                continue;
            }

            let v_end = self.statement_end(i);

            if v_end == i + 3 || !self.at(v_end, "return") || self.t(v_end + 1) != self.t(i + 1) {
                continue;
            }

            let after = v_end + 2;
            let closes = after >= self.toks.len()
                || CLOSERS.contains(&self.t(after))
                || self.line_of(after) != self.line_of(after - 1);

            if !closes {
                continue;
            }

            let name = self.t(i + 1);
            let value = self.slice(i + 3, v_end).trim();
            // A call or `...` may yield several values; the local kept
            // one, so the return keeps one too.
            let value = if value.ends_with(')') || value == "..." {
                format!("({value})")
            } else {
                value.to_string()
            };
            self.lint(
                out,
                "local_then_return",
                i,
                v_end + 1,
                format!("`local {name} = ...` followed by `return {name}` is `return {value}`"),
                Some(format!("return {value}")),
            );
        }
    }

    /// `for i = 1, #t do local v = t[i]`.
    fn numeric_for_index(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !(self.at(i, "for")
                && self.statement_start(i)
                && self.is_name(i + 1)
                && self.at(i + 2, "=")
                && self.at(i + 3, "1")
                && self.at(i + 4, ",")
                && self.at(i + 5, "#"))
            {
                continue;
            }

            let Some(d) = self.path_end(i + 6) else {
                continue;
            };

            if !(self.at(d, "do")
                && self.at(d + 1, "local")
                && self.is_name(d + 2)
                && self.at(d + 3, "="))
            {
                continue;
            }

            let Some(x) = self.same_path(i + 6, d, d + 4) else {
                continue;
            };

            if !(self.at(x, "[") && self.t(x + 1) == self.t(i + 1) && self.at(x + 2, "]")) {
                continue;
            }

            let after = x + 3;
            let closes = after >= self.toks.len()
                || CLOSERS.contains(&self.t(after))
                || self.line_of(after) != self.line_of(after - 1);

            if !closes {
                continue;
            }

            let index = self.t(i + 1);
            let value = self.t(d + 2);
            let table = self.slice(i + 6, d);
            let rewrite = format!("for {index}, {value} in {table} do");
            self.lint(
                out,
                "numeric_for_index",
                i,
                x + 2,
                format!(
                    "`for {index} = 1, #{table} do local {value} = {table}[{index}]` is `{rewrite}`"
                ),
                Some(rewrite.clone()),
            );
        }
    }
}

impl<'s> Scan<'s> {
    /// The private members of each struct in the file: `(member, struct)`
    /// from `private name: T` in a `struct` and `private function name`
    /// in an `impl`.
    fn private_members(&self) -> Vec<(&'s str, &'s str)> {
        let mut out = Vec::new();

        for i in 0..self.toks.len() {
            if !self.at(i, "private") {
                continue;
            }

            let owner = self.enclosing_owner(i);
            let Some(owner) = owner else { continue };

            if self.at(i + 1, "function") && self.is_name(i + 2) {
                out.push((self.t(i + 2), owner));
            } else if self.at(i + 1, "async") && self.at(i + 2, "function") && self.is_name(i + 3) {
                out.push((self.t(i + 3), owner));
            } else {
                let mut j = i + 1;

                if matches!(self.t(j), "read" | "write") && self.is_name(j + 1) {
                    j += 1;
                }

                if self.is_name(j) && self.at(j + 1, ":") {
                    out.push((self.t(j), owner));
                }
            }
        }

        out
    }

    /// The struct a token sits in: the target of the `struct` or `impl`
    /// block that encloses it.
    fn enclosing_owner(&self, j: usize) -> Option<&'s str> {
        let mut best: Option<(usize, &'s str)> = None;

        for (i, e) in self.st.ends.iter().enumerate() {
            let Some(e) = e else { continue };

            if !(i < j && j < *e) || !matches!(self.t(i), "struct" | "impl") {
                continue;
            }

            let name = if self.at(i, "impl") && self.is_name(i + 1) && self.at(i + 2, "for") {
                self.t(i + 3)
            } else {
                self.t(i + 1)
            };

            if best.is_none_or(|(b, _)| i > b) {
                best = Some((i, name));
            }
        }

        best.map(|(_, n)| n)
    }

    /// `x.count` or `x:reset()` outside the impl of the struct that
    /// declared `count` or `reset` private.
    fn private_access(&self, out: &mut Vec<Lint>) {
        let members = self.private_members();

        if members.is_empty() {
            return;
        }

        for i in 0..self.toks.len() {
            if !matches!(self.prev(i), "." | ":" | "?." | "?:") || !self.is_name(i) {
                continue;
            }

            let name = self.t(i);
            let Some((_, owner)) = members.iter().find(|(m, _)| *m == name) else {
                continue;
            };

            if self.enclosing_owner(i) == Some(*owner) {
                continue;
            }

            self.lint(
                out,
                "private_access",
                i,
                i,
                format!("`{name}` is private to `{owner}`; only its impl reaches it"),
                None,
            );
        }
    }

    /// The names a `local` at `i` binds, with their tokens. A destructure
    /// binds through a table pattern and stays out.
    fn local_names(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut j = i + 1;

        // `local async function f`, `local const x`: the modifiers first.
        while matches!(self.t(j), "async" | "const") {
            j += 1;
        }

        if self.at(j, "function") {
            return if self.is_name(j + 1) {
                vec![j + 1]
            } else {
                out
            };
        }

        // `local Pat(x) = e` binds through a pattern, not by this name.
        if self.is_name(j) && self.at(j + 1, "(") {
            return out;
        }

        while self.is_name(j) {
            out.push(j);
            j += 1;

            // A type annotation runs to the comma or the `=` on the line.
            if self.at(j, ":") {
                let mut depth = 0i32;

                while j < self.toks.len() && self.line_of(j) == self.line_of(i) {
                    let t = self.t(j);

                    if matches!(t, "(" | "{" | "[" | "<") {
                        depth += 1;
                    } else if matches!(t, ")" | "}" | "]" | ">") {
                        depth -= 1;
                    } else if depth == 0 && matches!(t, "," | "=") {
                        break;
                    }

                    j += 1;
                }
            }

            if self.at(j, ",") {
                j += 1;
            } else {
                break;
            }
        }

        out
    }

    /// Whether the name at `n` appears again after token `from`.
    fn read_after(&self, n: usize, from: usize) -> bool {
        let name = self.t(n);

        (from..self.toks.len())
            .any(|j| j != n && self.toks[j].kind == TokKind::Ident && self.t(j) == name)
    }

    /// A local or a loop variable that nothing reads after it.
    fn unused_variable(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            let names: Vec<usize> = match self.t(i) {
                "local" | "const" if self.statement_start(i) => self.local_names(i),

                "for" if self.statement_start(i) => {
                    let mut names = Vec::new();
                    let mut j = i + 1;

                    while j < self.toks.len() && !matches!(self.t(j), "in" | "=" | "do") {
                        if self.is_name(j) {
                            names.push(j);
                        }

                        j += 1;
                    }

                    names
                }

                _ => continue,
            };

            for n in names {
                let name = self.t(n);

                if name.starts_with('_') || self.read_after(n, n + 1) {
                    continue;
                }

                self.lint(
                    out,
                    "unused_variable",
                    n,
                    n,
                    format!("`{name}` is never read; prefix it with `_` or remove it"),
                    Some(format!("_{name}")),
                );
            }
        }
    }

    /// The case of declared names.
    fn naming(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            match self.t(i) {
                "local" | "const" if self.statement_start(i) => {
                    let is_function = self.at(i + 1, "function");

                    for n in self.local_names(i) {
                        let name = self.t(n);

                        if is_camel_case(name) {
                            self.lint(
                                out,
                                "camel_case_name",
                                n,
                                n,
                                format!("`{name}` is camelCase; Alloy names are snake_case"),
                                None,
                            );
                        } else if is_function && is_pascal_case(name) {
                            self.lint(
                                out,
                                "pascal_case_function",
                                n,
                                n,
                                format!("`{name}` is a local function in PascalCase; write it snake_case"),
                                None,
                            );
                        }
                    }
                }

                "function" if !matches!(self.prev(i), "." | ":") => {
                    // The last name of the path, then the parameters. A
                    // `local function` had its name read with the `local`.
                    let is_local = matches!(self.prev(i), "local" | "async" | "const");
                    let mut j = i + 1;
                    let mut last = None;

                    while self.is_name(j) || self.at(j, ".") || self.at(j, ":") {
                        if self.is_name(j) {
                            last = Some(j);
                        }

                        j += 1;
                    }

                    if let Some(n) = last
                        && !is_local
                        && is_camel_case(self.t(n))
                    {
                        self.lint(
                            out,
                            "camel_case_name",
                            n,
                            n,
                            format!("`{}` is camelCase; Alloy names are snake_case", self.t(n)),
                            None,
                        );
                    }

                    if self.at(j, "(")
                        && let Some(close) = self.matching(j)
                    {
                        let mut at_start = true;

                        for k in j + 1..close {
                            if at_start && self.is_name(k) && is_camel_case(self.t(k)) {
                                self.lint(
                                    out,
                                    "camel_case_name",
                                    k,
                                    k,
                                    format!(
                                        "parameter `{}` is camelCase; Alloy names are snake_case",
                                        self.t(k)
                                    ),
                                    None,
                                );
                            }

                            at_start = self.at(k, ",") && self.matching_depth(j, k) == 1;
                        }
                    }
                }

                w @ ("struct" | "enum" | "trait" | "interface" | "type")
                    if self.statement_start(i) || self.prev(i) == "export" =>
                {
                    let n = i + 1;

                    if self.is_name(n) && !is_pascal_case(self.t(n)) && !self.at(n, "function") {
                        self.lint(
                            out,
                            "type_case",
                            n,
                            n,
                            format!("`{}` is a {w} name; write it PascalCase", self.t(n)),
                            None,
                        );
                    }
                }

                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::apply_fixes;

    /// The lints of a source, without `unused_variable`: the sources
    /// here bind names to show a shape, not to read them.
    fn lints(src: &str) -> Vec<crate::Lint> {
        crate::compile(src)
            .unwrap()
            .lints
            .into_iter()
            .filter(|l| l.name != "unused_variable")
            .collect()
    }

    fn fixed(src: &str) -> String {
        apply_fixes(src, &lints(src)).0
    }

    /// The lints at their default level: the pedantic ones stay out.
    fn names(src: &str) -> Vec<&'static str> {
        let config = crate::config::LintConfig::default();

        lints(src)
            .iter()
            .map(|l| l.name)
            .filter(|n| crate::lint::level_of(&config, n) != crate::lint::Level::Allow)
            .collect()
    }

    #[test]
    fn an_assignment_to_itself_fires() {
        assert_eq!(names("x = x\n"), vec!["self_assignment"]);
        assert_eq!(names("a.b = a.b\n"), vec!["self_assignment"]);
        assert_eq!(names("local x = x\n"), Vec::<&str>::new());
        assert_eq!(names("a.b = a.b.c\n"), Vec::<&str>::new());
    }

    #[test]
    fn code_after_a_jump_fires() {
        assert_eq!(
            names("local function f()\n    return 1\n    print(2)\nend\n"),
            vec!["unreachable_code"]
        );
        assert_eq!(
            names("for i = 1, 2 do\n    break\n    print(i)\nend\n"),
            vec!["unreachable_code"]
        );
        assert_eq!(
            names(
                "local function f(x)\n    if x then\n        return 1\n    end\n    return 2\nend\n"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local function f()\n    return {\n        a = 1,\n    }\nend\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_literal_condition_fires() {
        assert_eq!(
            names("if true then print(1) end\n"),
            vec!["constant_condition"]
        );
        assert_eq!(
            names("while false do print(1) end\n"),
            vec!["constant_condition"]
        );
        assert_eq!(names("while true do break end\n"), Vec::<&str>::new());
    }

    #[test]
    fn a_key_set_twice_fires() {
        assert_eq!(
            names("local t = { a = 1, b = 2, a = 3 }\n"),
            vec!["duplicate_key"]
        );
        assert_eq!(
            names("local t = { [\"a\"] = 1, [\"a\"] = 2 }\n"),
            vec!["duplicate_key"]
        );
        assert_eq!(
            names("local t = { a = 1, inner = { a = 2 } }\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_not_before_a_comparison_flips_it() {
        assert_eq!(fixed("if not a == b then end\n"), "if a ~= b then end\n");
        assert_eq!(
            fixed("if not (a == b) then end\n"),
            "if not (a == b) then end\n"
        );
    }

    #[test]
    fn identical_branches_fire() {
        assert_eq!(
            names("if c then\n    print(1)\nelse\n    print(1)\nend\n"),
            vec!["identical_branches"]
        );
        assert_eq!(names("local x = c ? 1 : 1\n"), vec!["identical_branches"]);
        assert_eq!(
            names("if c then\n    print(1)\nelse\n    print(2)\nend\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn empty_blocks_fire_unless_a_comment_explains() {
        assert_eq!(names("if c then end\n"), vec!["empty_block"]);
        assert_eq!(
            names("if c then\n    print(1)\nelse\nend\n"),
            vec!["empty_block"]
        );
        assert_eq!(names("for i = 1, 2 do end\n"), vec!["empty_block"]);
        assert_eq!(
            names("if c then\n    -- nothing to do yet\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(names("local f = function() end\n"), Vec::<&str>::new());
    }

    #[test]
    fn comparing_to_a_boolean_fires() {
        assert_eq!(
            names("if x == true then\n    print(1)\nend\n"),
            vec!["bool_comparison"]
        );
        assert_eq!(
            names("if false == x then\n    print(1)\nend\n"),
            vec!["bool_comparison"]
        );
        assert_eq!(names("if x then\n    print(1)\nend\n"), Vec::<&str>::new());
    }

    #[test]
    fn a_boolean_if_becomes_a_return() {
        assert_eq!(
            fixed(
                "local function f(a, b)\n    if a == b then\n        return true\n    else\n        return false\n    end\nend\n"
            ),
            "local function f(a, b)\n    return a == b\nend\n"
        );
        assert_eq!(
            fixed(
                "local function f(a)\n    if a then\n        return false\n    else\n        return true\n    end\nend\n"
            ),
            "local function f(a)\n    return not a\nend\n"
        );
        assert_eq!(
            names(
                "local function f(a)\n    if a then\n        return true\n    else\n        return false\n    end\nend\n"
            ),
            vec!["needless_bool"]
        );
    }

    #[test]
    fn two_returns_become_a_ternary() {
        assert_eq!(
            fixed(
                "local function f(a)\n    if a > 1 then\n        return \"big\"\n    else\n        return \"small\"\n    end\nend\n"
            ),
            "local function f(a)\n    return a > 1 ? \"big\" : \"small\"\nend\n"
        );
    }

    #[test]
    fn a_bare_return_at_the_end_goes() {
        assert_eq!(
            fixed("local function f()\n    print(1)\n    return\nend\n"),
            "local function f()\n    print(1)\n    \nend\n"
        );
        assert_eq!(
            names("local function f()\n    return 1\nend\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_local_returned_at_once_folds() {
        assert_eq!(
            fixed("local function f()\n    local x = g(1)\n    return x\nend\n"),
            "local function f()\n    return (g(1))\nend\n"
        );
        assert_eq!(
            fixed("local function f(a)\n    local x = a + 1\n    return x\nend\n"),
            "local function f(a)\n    return a + 1\nend\n"
        );
        assert_eq!(
            names("local function f()\n    local x = g(1)\n    x = x + 1\n    return x\nend\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_local_nothing_reads_fires() {
        let unused = |src: &str| -> Vec<&'static str> {
            crate::compile(src)
                .unwrap()
                .lints
                .iter()
                .map(|l| l.name)
                .filter(|n| *n == "unused_variable")
                .collect()
        };
        let src = "local count = 1\nlocal used = 2\nprint(used)\n";
        let out = crate::compile(src).unwrap();
        assert_eq!(
            apply_fixes(src, &out.lints).0,
            "local _count = 1\nlocal used = 2\nprint(used)\n"
        );
        assert_eq!(
            unused("for i, v in t do\n    print(v)\nend\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused("for _, v in t do\n    print(v)\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            unused("local function helper() end\nlocal x: number = 1\nprint(x)\n"),
            vec!["unused_variable"]
        );
        assert_eq!(unused("local { a, b } = t\nprint(a)\n"), Vec::<&str>::new());
        assert_eq!(
            unused("local async function f() end\nf()\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn the_naming_lints_read_the_case() {
        let all = |src: &str| -> Vec<&'static str> {
            lints(src)
                .iter()
                .map(|l| l.name)
                .filter(|n| n.contains("case"))
                .collect()
        };
        assert_eq!(
            all("local playerCount = 1\nprint(playerCount)\n"),
            vec!["camel_case_name"]
        );
        assert_eq!(
            all("local Players = 1\nprint(Players)\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            all("local function LoadMap() end\nLoadMap()\n"),
            vec!["pascal_case_function"]
        );
        assert_eq!(
            all("local function f(maxHealth: number) return maxHealth end\nf(1)\n"),
            vec!["camel_case_name"]
        );
        assert_eq!(
            all("struct player_state as\n    x: number\nend\n"),
            vec!["type_case"]
        );
        assert_eq!(
            all("struct PlayerState as\n    x: number\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(all("function M:Destroy() end\n"), Vec::<&str>::new());
    }

    #[test]
    fn a_private_member_read_outside_its_impl_fires() {
        let src = "struct C as\n    private count: number = 0\nend\nimpl C\n    function bump(self): number\n        self.count += 1\n        self:log()\n        return self.count\n    end\n    private function log(self)\n        print(self.count)\n    end\nend\nlocal c = new C {}\nprint(c.count)\nc:log()\nprint(c:bump())\n";
        let all = lints(src);
        let hits: Vec<&str> = all
            .iter()
            .filter(|l| l.name == "private_access")
            .map(|l| l.message.as_str())
            .collect();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits[0].contains("`count` is private to `C`"));
        assert!(hits[1].contains("`log` is private to `C`"));
    }

    #[test]
    fn a_numeric_loop_over_a_table_becomes_generic() {
        assert_eq!(
            fixed("for i = 1, #t do\n    local v = t[i]\n    print(v)\nend\n"),
            "for i, v in t do\n    print(v)\nend\n"
        );
    }
}
