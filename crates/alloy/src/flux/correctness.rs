//! Flux: the correctness, suspicious, and style lints that read
//! control flow. A statement after `return`, a table that sets one
//! key twice, an `if` with two identical branches, a `return` written
//! as an `if`. `bindings` holds the lints that read declarations and
//! names instead; the names and levels of both sit in `lint::LINTS`.

use super::scan::{CLOSERS, IfParts, Scan};
use crate::lint::{Fix, Lint};

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
    s.needless_guard(&mut out);
    s.if_returns(&mut out);
    s.redundant_return(&mut out);
    s.local_then_return(&mut out);
    s.numeric_for_index(&mut out);
    s.unused_variable(&mut out);
    s.private_access(&mut out);
    s.deprecated_call(&mut out);
    s.argument_count(&mut out);
    s.const_mutation(&mut out);
    s.duplicate_function(&mut out);
    s.prefer_const(&mut out);
    out
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
                            // On one line the reader sees both values, so
                            // naming the line adds nothing.
                            let lost = if self.line_of(*first) == self.line_of(at) {
                                String::new()
                            } else {
                                format!("; the value on line {} is lost", self.line_of(*first) + 1)
                            };
                            self.lint(
                                out,
                                "duplicate_key",
                                at,
                                at,
                                format!("`{k}` is set twice in this table{lost}"),
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

            // `c == true` on a `boolean?` is the one shape where the
            // comparison is the right thing to write: it has to tell
            // `false` from `nil`.
            if self.declared_optional_boolean(x) {
                continue;
            }

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
        // One name, declared `boolean`: a parameter or a local says so.
        if b == a + 1 && self.declared_boolean(self.t(a)) {
            return true;
        }

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

    /// Reports if a name carries a `: boolean?` annotation anywhere in
    /// the file. Three states need the comparison.
    fn declared_optional_boolean(&self, name: &str) -> bool {
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return false;
        }

        (0..self.toks.len().saturating_sub(3)).any(|j| {
            self.t(j) == name
                && self.t(j + 1) == ":"
                && self.t(j + 2) == "boolean"
                && self.t(j + 3) == "?"
        })
    }

    /// Reports if a name carries a `: boolean` annotation anywhere in the
    /// file, as a parameter or as a local.
    fn declared_boolean(&self, name: &str) -> bool {
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return false;
        }

        (0..self.toks.len().saturating_sub(2))
            .any(|j| self.t(j) == name && self.t(j + 1) == ":" && self.t(j + 2) == "boolean")
    }

    /// The exclusive end of the type annotation that starts at `from`:
    /// the `,`, `=`, `)` or `;` that closes it at depth zero.
    pub(super) fn annotation_end(&self, from: usize) -> usize {
        let mut depth = 0i32;
        let mut j = from;

        while j < self.toks.len() {
            match self.t(j) {
                "(" | "[" | "{" | "<" | "<<" => depth += 1,

                ")" | "]" | "}" | ">" | ">>" if depth == 0 => break,

                ")" | "]" | "}" | ">" | ">>" => depth -= 1,

                "," | "=" | ";" if depth == 0 => break,

                _ => {}
            }

            j += 1;
        }

        j
    }

    /// Every name the file annotates, with whether its type ends in
    /// `?`. Reads `local`, `const`, and the parameters of a function.
    /// A field of a record type binds nothing, so it stays out.
    fn annotated_bindings(&self) -> Vec<(&'s str, bool)> {
        let mut out = Vec::new();

        for i in 0..self.toks.len() {
            let params = self.at(i, "function");
            let mut j = match (matches!(self.t(i), "local" | "const"), params) {
                (true, _) => i + 1,

                // `function name<T>(a: A, b: B)`: the head runs to the
                // `(` over the path and the generics alone, so a call
                // never reads as a signature.
                (_, true) => {
                    let mut k = i + 1;

                    while matches!(self.t(k), "." | ":" | "<" | ">" | ",")
                        || self.is_name(k)
                        || self.at(k, "async")
                    {
                        k += 1;
                    }

                    match self.at(k, "(") {
                        true => k + 1,

                        false => continue,
                    }
                }

                _ => continue,
            };

            let stop = match params {
                true => self.matching(j - 1).unwrap_or(self.toks.len()),

                false => self.toks.len(),
            };

            while j < stop && self.is_name(j) {
                match self.at(j + 1, ":") {
                    true => {
                        let end = self.annotation_end(j + 2);
                        out.push((self.t(j), end > j + 2 && self.at(end - 1, "?")));
                        j = end;
                    }

                    false => j += 1,
                }

                if !self.at(j, ",") {
                    break;
                }

                j += 1;
            }
        }

        out
    }

    /// Whether every annotation the file gives `name` has no `?`.
    /// False when the file annotates it nowhere: a type nobody wrote is
    /// no ground for a diagnostic.
    fn never_optional(&self, name: &str, bindings: &[(&'s str, bool)]) -> bool {
        let mut seen = false;

        for (bound, optional) in bindings {
            if *bound != name {
                continue;
            }

            if *optional {
                return false;
            }

            seen = true;
        }

        seen
    }

    /// `p!` and `p?[k]` where `p` carries a type with no `?`. The
    /// assert can never throw and the guard can never stop the chain,
    /// so both operators only cost a reader a second look.
    fn needless_guard(&self, out: &mut Vec<Lint>) {
        let bindings = self.annotated_bindings();

        if bindings.is_empty() {
            return;
        }

        for i in 1..self.toks.len() {
            let guard = self.t(i);

            if !matches!(guard, "!" | "?") || !self.is_name(i - 1) {
                continue;
            }

            // `a.b!` asserts a field, whose type the annotations of the
            // file do not name. Only a bound name reads here.
            if matches!(self.prev(i - 1), "." | ":" | "?." | "?:") {
                continue;
            }

            // `?` alone is the ternary; the bracket after it makes the
            // safe index.
            if guard == "?" && !self.at(i + 1, "[") {
                continue;
            }

            let name = self.t(i - 1);

            if !self.never_optional(name, &bindings) {
                continue;
            }

            let (lint, message) = match guard {
                "!" => (
                    "needless_assert",
                    format!(
                        "`{name}` is never nil, so the `!` asserts what already holds; remove it"
                    ),
                ),

                _ => (
                    "optional_access",
                    format!(
                        "`{name}` is never nil, so `?[` guards what already holds; index it with `[`"
                    ),
                ),
            };
            self.lint(out, lint, i, i, message, Some(String::new()));
        }
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

                // `identical_branches` covers two returns of one value;
                // a ternary of the same value on both sides is no fix.
                (x, y) if x == y => {}

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

            // The rewrite deletes the line, not the token: an empty
            // replacement over `return` alone leaves the indentation
            // behind, and `fmt --check` then reports the file.
            let (from, to) = self.whole_line(end - 1);
            out.push(Lint {
                name: "redundant_return",
                start: self.start(end - 1),
                end: self.end(end - 1),
                message:
                    "a bare `return` at the end of a function does what the `end` does; delete it"
                        .to_string(),
                fix: Some(Fix::new(self.src, from, to, "")),
            });
        }
    }

    /// `local x = v` then `return x`, and the same with `const`.
    /// `prefer_const` writes `const` over the same `local`, and `--fix`
    /// takes that rewrite first, so a `const` has to fire too: the
    /// second pass then applies this one.
    fn local_then_return(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !(self.at(i, "local") || self.at(i, "const"))
                || !self.statement_start(i)
                || self.cond_binding(i)
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

            let word = self.t(i);
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
                format!("`{word} {name} = ...` followed by `return {name}` is `return {value}`"),
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
            // A body that never reads the index gets `_`; naming it
            // would leave an `unused_variable` behind the rewrite.
            let body = self.st.ends[i].unwrap_or(self.toks.len());
            let read = (x + 3..body).any(|k| self.is_name(k) && self.t(k) == index);
            let bound = if read { index } else { "_" };
            let rewrite = format!("for {bound}, {value} in {table} do");
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

#[cfg(test)]
mod tests {
    use super::super::helpers::{fixed_by, names_of};
    use crate::lint::apply_fixes;

    /// The lints of one source, straight from the lint pass.
    ///
    /// These lints read the token stream, so they fire on a source the
    /// parser rejects too, and `compile` drops the lints of such a file.
    fn lints(src: &str) -> Vec<crate::Lint> {
        let Ok(parsed) =
            alloy_syntax::parse_lenient(src, alloy_syntax::parser::ParseOptions::default())
        else {
            panic!("the source does not lex");
        };

        crate::lint::run(
            src,
            &parsed.lexed.toks,
            &parsed.chunk,
            false,
            &crate::lint::Thresholds::default(),
            &[],
            &[],
        )
        .into_iter()
        .filter(|l| {
            !matches!(
                l.name,
                "unused_variable" | "unused_function" | "redundant_as" | "prefer_const"
            )
        })
        .collect()
    }

    fn fixed(src: &str) -> String {
        fixed_by(src, &lints(src))
    }

    fn names(src: &str) -> Vec<&'static str> {
        names_of(&lints(src))
    }

    /// `!` and `?[` on a name the file types with no `?`: the check
    /// never fires, so both operators go. An optional keeps them.
    #[test]
    fn a_guard_on_a_name_that_is_never_nil_fires() {
        assert_eq!(
            names("local m: { [string]: number } = {}\nprint(m![\"k\"])\n"),
            vec!["needless_assert"]
        );
        assert_eq!(
            names("local m: { [string]: number } = {}\nprint(m?[\"k\"])\n"),
            vec!["optional_access"]
        );
        assert_eq!(
            names("local function f(p: Part)\n    print(p!.Name)\nend\n"),
            vec!["needless_assert"]
        );
        assert_eq!(
            fixed("local m: { [string]: number } = {}\nprint(m![\"k\"])\n"),
            "local m: { [string]: number } = {}\nprint(m[\"k\"])\n"
        );

        // An optional needs both, and a field carries no annotation the
        // file can read.
        assert_eq!(
            names("local m: { [string]: number }? = nil\nprint(m![\"k\"])\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local function f(p: Part?)\n    print(p!.Name)\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local function f(p: Part)\n    print(p.Parent!.Name)\nend\n"),
            Vec::<&str>::new()
        );
        // A name nothing annotates says nothing either way.
        assert_eq!(names("print(m![\"k\"])\n"), Vec::<&str>::new());
        // The ternary is no safe index.
        assert_eq!(
            names("local n: number = 1\nlocal s = n > 0 ? \"up\" : \"down\"\nprint(s)\n"),
            Vec::<&str>::new()
        );
        // A record field of a type is no binding: `name` there says
        // nothing about the local the assert reads.
        assert_eq!(
            names("type P = { a: number, name: string }\nprint(name!.x)\n"),
            Vec::<&str>::new()
        );
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

    /// A `return` whose value spans several lines is one statement, so
    /// no line of it is code after the jump. The broken form of a long
    /// `if` expression is the shape `alloy fmt` writes.
    #[test]
    fn a_return_over_several_lines_stays_clean() {
        let broken = "local function f(n: number): string\n    return if n == 1 then\n        \"one\"\n        elseif n == 2 then\n        \"two\"\n        else\n        \"other\"\nend\n";
        assert_eq!(names(broken), Vec::<&str>::new());

        let table_and_call = "local function g(a: number)\n    return {\n        value = a,\n        name = tostring(\n            a\n        ),\n    }\nend\n";
        assert_eq!(names(table_and_call), Vec::<&str>::new());

        // A line that opens with an operator, an access or a bracket goes
        // on with the value above it.
        for rest in [
            "a\n        + b\n        + c",
            "a\n        and b",
            "a\n        .. b",
            "a\n        == b",
            "a\n        ?? b",
            "t\n        .x",
            "t\n        [1]",
        ] {
            let src = format!(
                "local function k(a: any, b: any, c: any, t: any)\n    return {rest}\nend\n"
            );
            assert_eq!(names(&src), Vec::<&str>::new(), "{src}");
        }

        // A real statement after the jump still fires.
        assert_eq!(
            names("local function j(a: number)\n    return a\n    print(a)\nend\n"),
            vec!["unreachable_code"]
        );
        assert_eq!(
            names(
                "local function h(n: number): string\n    return if n == 1 then\n        \"one\"\n        else\n        \"other\"\n    print(n)\nend\n"
            ),
            vec!["unreachable_code"]
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
            "local function f()\n    print(1)\nend\n"
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
                .filter(|n| n.starts_with("unused_"))
                .collect()
        };
        let src = "local count = 1\nlocal used = 2\nprint(used)\n";
        let out = crate::compile(src).unwrap();
        let kept: Vec<_> = out
            .lints
            .into_iter()
            .filter(|l| l.name != "prefer_const")
            .collect();
        assert_eq!(
            apply_fixes(src, &kept).0,
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
        // A function above a top-level `const` reads it. The read is a
        // global, which the checker reports; the const is not unused.
        assert_eq!(
            unused(
                "local function f(): number\n    return LIMIT\nend\nconst LIMIT = 1\nprint(f())\n"
            ),
            Vec::<&str>::new()
        );
        // A read at the top level above it is no use of the const.
        assert_eq!(
            unused("print(LIMIT)\nconst LIMIT = 1\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused("local function helper() end\nlocal x: number = 1\nprint(x)\n"),
            vec!["unused_function"]
        );
        // A destructuring pattern binds its names one by one, so each
        // unread name is its own report.
        assert_eq!(
            unused("local { a, b } = t\nprint(a)\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused("local [first, second] = arr\nprint(first)\n"),
            vec!["unused_variable"]
        );
        // `{ b = c }` binds `c`; `b` is the field it reads.
        assert_eq!(
            unused("local { a = kept, b = gone } = t\nprint(kept)\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused("local [head, ...rest] = arr\nprint(head)\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused("local { a, b } = t\nprint(a)\nprint(b)\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            unused("local async function f() end\nf()\n"),
            Vec::<&str>::new()
        );
        // A field of another value spelled the same counted as a read.
        assert_eq!(
            unused("local t = { origin = 1 }\nlocal origin = 5\nprint(t.origin)\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused(
                "local t = { origin = function() end }\nlocal origin = 5\nt:origin()\nprint(t)\n"
            ),
            vec!["unused_variable"]
        );
        // A type annotation still reads the name it writes.
        assert_eq!(
            unused("local t = { a = 1 }\nlocal x: typeof(t) = t\nprint(x)\n"),
            Vec::<&str>::new()
        );
        // Every `local` on one physical line is its own statement.
        assert_eq!(
            unused("local used_x = 1  local unused_y = 2\nprint(used_x)\n"),
            vec!["unused_variable"]
        );
        assert_eq!(
            unused("local a = 1  local b = 2  local c = 3\nprint(b)\n"),
            vec!["unused_variable", "unused_variable"]
        );
        // The module table reads an export, so no exported binding is
        // unused. A plain `local` beside one keeps its report.
        assert_eq!(unused("export const SCHEMA = 1\n"), Vec::<&str>::new());
        assert_eq!(unused("export local count = 3\n"), Vec::<&str>::new());
        assert_eq!(unused("export const { a, b } = t\n"), Vec::<&str>::new());
        assert_eq!(unused("global const LIMIT = 9\n"), Vec::<&str>::new());
        assert_eq!(
            unused("export const SCHEMA = 1\nlocal unused = 1\n"),
            vec!["unused_variable"]
        );
        // The fix no longer offers `_SCHEMA` for an export.
        let src = "export const SCHEMA = 1\nlocal unused = 1\n";
        let out = crate::compile(src).unwrap();
        let kept: Vec<_> = out
            .lints
            .into_iter()
            .filter(|l| l.name != "prefer_const")
            .collect();
        assert_eq!(
            apply_fixes(src, &kept).0,
            "export const SCHEMA = 1\nlocal _unused = 1\n"
        );
    }

    #[test]
    fn a_function_nothing_calls_fires() {
        let unused = |src: &str| -> Vec<&'static str> {
            crate::compile(src)
                .unwrap()
                .lints
                .iter()
                .map(|l| l.name)
                .filter(|n| n.starts_with("unused_"))
                .collect()
        };
        assert_eq!(unused("function f() end\n"), vec!["unused_function"]);
        assert_eq!(unused("async function f() end\n"), vec!["unused_function"]);
        assert_eq!(
            unused("local f = function() end\n"),
            vec!["unused_function"]
        );
        assert_eq!(
            unused("const g = async function() end\n"),
            vec!["unused_function"]
        );
        assert_eq!(
            unused("local f = function() end\nf()\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            unused("print(1)\nfunction f() end\nf()\n"),
            Vec::<&str>::new()
        );
        assert_eq!(unused("export function f() end\n"), Vec::<&str>::new());
        assert_eq!(unused("@test\nfunction f() end\n"), Vec::<&str>::new());
        // An attribute exempts the `local function` form too: the spec
        // calls the test, not the file.
        assert_eq!(
            unused("@test\nlocal function f() end\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            unused("@test\nlocal f = function() end\n"),
            Vec::<&str>::new()
        );
        // A `local` with no function keeps its report.
        assert_eq!(unused("@test\nlocal x = 1\n"), vec!["unused_variable"]);
        assert_eq!(
            unused("@ratelimit(1, 2)\nasync function f() end\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            unused("struct S as\n    x: number\nend\nimpl S as\n    function m(self) end\nend\n"),
            Vec::<&str>::new()
        );
        let src = "local f = function() end\n";
        let out = crate::compile(src).unwrap();
        let kept: Vec<_> = out
            .lints
            .into_iter()
            .filter(|l| l.name != "prefer_const")
            .collect();
        assert_eq!(apply_fixes(src, &kept).0, "local _f = function() end\n");
    }

    #[test]
    fn a_private_member_read_outside_its_impl_fires() {
        let src = "struct C as\n    private count: number = 0\nend\nimpl C as\n    function bump(self): number\n        self.count += 1\n        self:log()\n        return self.count\n    end\n    private function log(self)\n        print(self.count)\n    end\nend\nlocal c = new C {}\nprint(c.count)\nc:log()\nprint(c:bump())\n";
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
    fn a_private_field_of_an_imported_struct_fires() {
        // The lint reads tokens, so a struct another module declares
        // reaches it through the shape the import carries.
        let src = "import { Item } from \"./lib\"\nlocal it: Item = make(1)\nlocal leak = it.secret\nprint(leak)\n";
        let privates = vec![("Item".to_string(), vec!["secret".to_string()])];
        let Ok(parsed) =
            alloy_syntax::parse_lenient(src, alloy_syntax::parser::ParseOptions::default())
        else {
            panic!("the source does not lex");
        };
        let hits: Vec<String> = crate::lint::run(
            src,
            &parsed.lexed.toks,
            &parsed.chunk,
            false,
            &crate::lint::Thresholds::default(),
            &privates,
            &[],
        )
        .into_iter()
        .filter(|l| l.name == "private_access")
        .map(|l| l.message)
        .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(
            hits[0].contains("`secret` is private to `Item`"),
            "{hits:?}"
        );
    }

    /// An optional boolean has three states, so `c == true` is the
    /// right thing to write.
    #[test]
    fn bool_comparison_stands_down_on_an_optional() {
        assert_eq!(
            names("local function tri(c: boolean?): boolean\n    return c == true\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local function two(c: boolean): boolean\n    return c == true\nend\n"),
            vec!["bool_comparison"]
        );
    }

    /// The receiver decides which struct a member belongs to: a field
    /// named `coins` on an unrelated record is not the private `coins`
    /// of a struct nearby.
    #[test]
    fn private_access_reads_the_receiver() {
        let src = "struct Profile as\n    private coins: number\nend\n\nimpl Profile as\n    public function earn(self, n: number)\n        self.coins += n\n    end\nend\n\ntype Raw = { coins: number }\n\nlocal function load(raw: Raw, p: Profile)\n    p:earn(raw.coins)\nend\n\nreturn load\n";
        assert_eq!(names(src), Vec::<&str>::new());

        // A receiver the file does not type still fires.
        let bare = "struct Profile as\n    private coins: number\nend\n\nlocal p = make()\nprint(p.coins)\n";
        assert_eq!(names(bare), vec!["private_access"]);
    }

    /// `impl Zoo.Lion` is the struct's own impl: the owner of a private
    /// member is the member the path names, not the namespace. The lint
    /// read the first name after `impl` and fired inside the impl.
    #[test]
    fn private_access_reads_a_namespace_path_as_its_last_name() {
        let src = "namespace Zoo as\n    struct Lion as\n        read name: string\n        private roar_power: number = 10\n    end\nend\n\nimpl Zoo.Lion as\n    function roar(self): number\n        return self.roar_power\n    end\nend\nprint(Zoo)\n";
        assert_eq!(names(src), Vec::<&str>::new());

        // From outside the impl it still fires, and it names the struct.
        let outside =
            format!("{src}local l = new Zoo.Lion {{ name = \"Leo\" }}\nprint(l.roar_power)\n");
        let hits: Vec<String> = lints(&outside)
            .into_iter()
            .filter(|l| l.name == "private_access")
            .map(|l| l.message)
            .collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(
            hits[0].contains("`roar_power` is private to `Lion`"),
            "{hits:?}"
        );
    }

    /// A private field with a default need not be set, so a `new`
    /// outside the impl that names it reaches past the visibility.
    #[test]
    fn a_constructor_key_reads_the_visibility() {
        let src = "struct Vault as\n    owner: string\n    private code: number = 0\nend\n\nlocal v = new Vault { owner = \"ana\", code = 7 }\nprint(v)\n";
        assert_eq!(names(src), vec!["private_access"]);

        // A private field with no default has to be set at construction.
        let required = "struct Vault as\n    owner: string\n    private code: number\nend\n\nlocal v = new Vault { owner = \"ana\", code = 7 }\nprint(v)\n";
        assert_eq!(names(required), Vec::<&str>::new());

        // Inside the impl the field is the struct's own.
        let inside = "struct Vault as\n    owner: string\n    private code: number = 0\nend\n\nimpl Vault as\n    function new(owner: string): Vault\n        return new Vault { owner = owner, code = 1 }\n    end\nend\nprint(Vault)\n";
        assert_eq!(names(inside), Vec::<&str>::new());
    }

    /// `const` freezes the binding, not the value. The pedantic lint
    /// says so on a field write, an index write, and a mutating call.
    #[test]
    fn a_write_into_a_const_value_fires() {
        let pedantic = |src: &str| -> Vec<&'static str> {
            lints(src)
                .iter()
                .map(|l| l.name)
                .filter(|n| *n == "const_mutation")
                .collect()
        };
        assert_eq!(
            pedantic("const LIMITS = { hp = 100 }\nLIMITS.hp = 1\nprint(LIMITS)\n"),
            vec!["const_mutation"]
        );
        assert_eq!(
            pedantic("const NAMES = [ \"ana\" ]\nNAMES:push(\"bo\")\nprint(NAMES)\n"),
            vec!["const_mutation"]
        );
        assert_eq!(
            pedantic("const T = { a = 1 }\nT[\"a\"] = 2\nprint(T)\n"),
            vec!["const_mutation"]
        );
        // A read of a const is no write, and a plain local is not a const.
        assert_eq!(
            pedantic("const T = { a = 1 }\nprint(T.a)\nlocal u = { a = 1 }\nu.a = 2\nprint(u)\n"),
            Vec::<&str>::new()
        );
    }

    /// Luau reports `Box.value(b)` on a deprecated method and misses
    /// `b:value()`. The lint takes the method call when the file types
    /// the receiver, and a receiver of no known type stays quiet.
    #[test]
    fn a_method_call_of_a_deprecated_method_fires() {
        let src = "struct Box as\n    n: number\nend\nimpl Box as\n    @deprecated(\"use get\")\n    function value(self): number\n        return self.n\n    end\n    function get(self): number\n        return self.n\n    end\nend\nlocal b = new Box { n = 1 }\nprint(b:value(), b:get())\nlocal function show(x: Box, y)\n    print(x:value(), y:value())\nend\nshow(b, b)\n";
        let got: Vec<(usize, String)> = lints(src)
            .into_iter()
            .filter(|l| l.name == "deprecated_call")
            .map(|l| (src[..l.start as usize].matches('\n').count() + 1, l.message))
            .collect();
        assert_eq!(
            got,
            [
                (14, "`Box:value` is deprecated; use get".to_string()),
                (16, "`Box:value` is deprecated; use get".to_string()),
            ]
        );
    }

    /// A compound write into a field is a write into the value: the
    /// const draws `const_mutation`, and the local stays `local`. A
    /// `local` of the name in an inner block holds its own writes.
    /// `prefer_const` and `local_then_return` both rewrite one `local`.
    /// `--fix` took `const` first, and `local_then_return` then read
    /// nothing, so flux offered two rewrites and applied one.
    #[test]
    fn a_const_then_return_fires_so_both_rewrites_land() {
        let src = "local function total(): number\n    local x = math.random()\n    return x\nend\nprint(total())\n";
        let fixable = |text: &str| -> Vec<crate::lint::Lint> {
            crate::compile(text)
                .unwrap()
                .lints
                .into_iter()
                .filter(|l| matches!(l.name, "prefer_const" | "local_then_return"))
                .collect()
        };
        let offered = fixable(src).len();
        let mut text = src.to_string();
        let mut applied = 0;

        // The passes of `--fix`: each applies what does not overlap.
        for _ in 0..4 {
            let (next, n) = crate::lint::apply_fixes(&text, &fixable(&text));

            if n == 0 {
                break;
            }

            applied += n;
            text = next;
        }

        assert_eq!((offered, applied), (2, 2));
        assert!(text.contains("    return (math.random())\n"), "{text}");
    }

    #[test]
    fn a_compound_field_write_writes_into_the_value() {
        // The lines one lint fires on.
        let lines = |src: &str, lint: &str| -> Vec<usize> {
            crate::compile(src)
                .unwrap()
                .lints
                .into_iter()
                .filter(|l| l.name == lint)
                .map(|l| src[..l.start as usize].matches('\n').count() + 1)
                .collect()
        };
        assert_eq!(
            lines(
                "const W = { n = 0, s = \"\" }\nW.n += 1\nW[\"s\"] ..= \"x\"\nprint(W)\n",
                "const_mutation"
            ),
            [2, 3]
        );
        assert_eq!(
            lines("local u = { n = 0 }\nu.n += 1\nprint(u)\n", "prefer_const"),
            Vec::<usize>::new()
        );
        // The inner `z` takes the write, so the outer one is a const.
        assert_eq!(
            lines(
                "local z = 1\ndo\n    local z = 2\n    z = 3\n    print(z)\nend\nprint(z)\n",
                "prefer_const"
            ),
            [1]
        );
        // A `local` in one branch does not reach the other: the write
        // in the `else` keeps the outer `q` a local, and the inner `q`,
        // which nothing writes, takes `const`.
        assert_eq!(
            lines(
                "local q = 1\nif q then\n    local q = 2\n    print(q)\nelse\n    q = 3\nend\nprint(q)\n",
                "prefer_const"
            ),
            [3]
        );
    }

    /// A method that changes the value writes into it in any expression.
    /// `bag:add(s)` kept `bag` a `local`, but `local r = bag:add(s)` drew
    /// `prefer_const`, and `const_mutation` saw only the statement.
    #[test]
    fn a_method_call_writes_into_the_value_in_any_expression() {
        let lines = |src: &str, lint: &str| -> Vec<usize> {
            crate::compile(src)
                .unwrap()
                .lints
                .into_iter()
                .filter(|l| l.name == lint)
                .map(|l| src[..l.start as usize].matches('\n').count() + 1)
                .collect()
        };
        let src = "local a = {}\na:push(1)\nlocal b = {}\nlocal r = b:push(2)\nlocal c = {}\nlocal n = c:len()\nprint(a, b, r, c, n, t.b:push(3))\n";
        assert_eq!(lines(src, "prefer_const"), [4, 5, 6]);
        assert_eq!(
            lines(
                "const B = {}\nlocal r = B:push(1)\nB:push(2)\nprint(r, B:len())\n",
                "const_mutation"
            ),
            [2, 3]
        );
    }

    /// A `const` reaches only its own block. `const_mutation` read any
    /// later name of its text, so a write through a `local` or a
    /// parameter of that name in another function fired.
    #[test]
    fn a_const_does_not_reach_a_name_in_another_function() {
        let lines = |src: &str, lint: &str| -> Vec<usize> {
            crate::compile(src)
                .unwrap()
                .lints
                .into_iter()
                .filter(|l| l.name == lint)
                .map(|l| src[..l.start as usize].matches('\n').count() + 1)
                .collect()
        };
        let src = "local function read(): number\n    const held = { x = 1 }\n    held.x = 2\n    return held.x\nend\nlocal function write(): ()\n    local held = { x = 1 }\n    held.x = 3\n    print(held)\nend\nlocal function take(held: { x: number }): ()\n    held.x = 4\nend\nprint(read, write, take)\n";
        assert_eq!(lines(src, "const_mutation"), [3]);
        // The write in `write` keeps that `held` a `local`, and reaches no other.
        assert_eq!(lines(src, "prefer_const"), Vec::<usize>::new());
        // A top-level const reaches into a function that has no binding of the name.
        assert_eq!(
            lines(
                "const T = { n = 0 }\nlocal function bump(): ()\n    T.n += 1\nend\nprint(bump)\n",
                "const_mutation"
            ),
            [3]
        );
    }

    /// One name given a body twice: the second replaces the first, and
    /// the checker's `DuplicateFunction` gives way to this one.
    #[test]
    fn a_second_body_for_one_name_fires() {
        assert_eq!(
            names(
                "local function twice()\n    return 1\nend\n\nlocal function twice()\n    return 2\nend\nprint(twice())\n"
            ),
            vec!["duplicate_function"]
        );
        // Two impls may write one method name; the owner tells them apart.
        let two = "struct A as\n    x: number\nend\nstruct B as\n    x: number\nend\nimpl A as\n    function get(self): number\n        return self.x\n    end\nend\nimpl B as\n    function get(self): number\n        return self.x\n    end\nend\nprint(A, B)\n";
        assert_eq!(names(two), Vec::<&str>::new());
        // `@test` keeps both bodies in the build, so the pair still fires.
        // Only a `@cfg` pair stands apart.
        assert_eq!(
            names(
                "@test\nfunction name()\n    return 1\nend\n\nfunction name()\n    return 2\nend\n"
            ),
            vec!["duplicate_function"]
        );
        assert_eq!(
            names(
                "@cfg(debug)\nfunction name()\n    return 1\nend\n\n@cfg(not debug)\nfunction name()\n    return 2\nend\n"
            ),
            Vec::<&str>::new()
        );
        // A namespace holds its own scope, so the two names never meet.
        assert_eq!(
            names(
                "namespace A as\n    function name()\n        return 1\n    end\nend\n\nfunction name()\n    return 2\nend\n"
            ),
            Vec::<&str>::new()
        );
    }

    /// An `if` expression in a `case` arm has no `end`; counting one
    /// closed the `impl` early and every later member read as private.
    #[test]
    fn an_if_expression_in_an_arm_keeps_the_impl_open() {
        let src = "enum C as\n    A(number)\n    B\nend\n\nstruct R as\n    private xs: number[]\nend\n\nimpl R as\n    public function viamatch(self, c: C): number\n        return match c with\n            case A(n) then if #self.xs > 0 then n else 0\n            case B then 0\n        end\n    end\n\n    public function stmt(self, c: C)\n        match c with\n            case A(n) then self.xs:push(n)\n            case B then print(\"b\")\n        end\n    end\nend\n\nreturn R\n";
        assert_eq!(names(src), Vec::<&str>::new());
    }

    /// The binding of a condition is not a statement. `if local r = f()`
    /// then `return r` read as one, and `--fix` wrote `if return f() then`.
    #[test]
    fn a_condition_binding_takes_no_return_rewrite() {
        for head in [
            "if local r = f() then",
            "if const r = t[1] then",
            "while local r = f() do",
            "if not local q = f() then\n        return 0\n    elseif local r = f() then",
            "if local a = f(); local r = g(a) then",
        ] {
            let src = format!("local function use()\n    {head}\n        return r\n    end\nend\n");
            assert!(!names(&src).contains(&"local_then_return"), "{src}");
        }
        // A plain `local` after a condition still takes it.
        assert_eq!(
            fixed("if ok then\n    local r = f()\n    return r\nend\n"),
            "if ok then\n    return (f())\nend\n"
        );

        // Two statements on one line: `{}` ends the first, and the name
        // after it opens the second. The rewrite once took both as the
        // value, `return ({} table.insert(out, 1))`.
        for line in [
            "const out = {} table.insert(out, 1)",
            "local out = f() print(out)",
            "local out = 1 if ok then out = 2 end",
            "local out = \"s\" print(out)",
        ] {
            let src = format!("local function g()\n    {line}\n    return out\nend\n");
            assert!(!names(&src).contains(&"local_then_return"), "{src}");
        }

        // A word operator goes on with the value: it is one statement.
        assert_eq!(
            fixed("local function g()\n    local out = a and b\n    return out\nend\n"),
            "local function g()\n    return a and b\nend\n"
        );
    }

    /// A rewrite that breaks the parse does not land, and one beside it
    /// that keeps the parse still does.
    #[test]
    fn a_fix_that_breaks_the_parse_is_refused() {
        let src = "local a = 1\nif a then\n    print(a)\nend\n";
        let fix = |from: &str, to: &str| {
            let at = src.find(from).unwrap() as u32;

            crate::Lint {
                name: "test",
                start: at,
                end: at,
                message: String::new(),
                fix: Some(crate::lint::Fix::new(src, at, at + from.len() as u32, to)),
            }
        };
        let (text, n) = apply_fixes(
            src,
            &[
                fix("if a then", "if return a then"),
                fix("local a", "const a"),
            ],
        );
        assert_eq!(
            (text.as_str(), n),
            ("const a = 1\nif a then\n    print(a)\nend\n", 1)
        );
    }

    #[test]
    fn a_numeric_loop_over_a_table_becomes_generic() {
        assert_eq!(
            fixed("for i = 1, #t do\n    local v = t[i]\n    print(v)\nend\n"),
            "for _, v in t do\n    print(v)\nend\n"
        );
        // A body that reads the index keeps its name.
        assert_eq!(
            fixed("for i = 1, #t do\n    local v = t[i]\n    print(i, v)\nend\n"),
            "for i, v in t do\n    print(i, v)\nend\n"
        );
    }
}
