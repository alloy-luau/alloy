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
    s.naming(&mut out);
    s.private_access(&mut out);
    s.const_mutation(&mut out);
    s.duplicate_function(&mut out);
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
    fn annotation_end(&self, from: usize) -> usize {
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
                fix: Some(Fix {
                    start: from,
                    end: to,
                    replacement: String::new(),
                }),
            });
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
            false,
            &crate::lint::Thresholds::default(),
            &[],
        )
        .into_iter()
        .filter(|l| !matches!(l.name, "unused_variable" | "unused_function"))
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

        // A real statement after the jump still fires.
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
            vec!["unused_function"]
        );
        assert_eq!(unused("local { a, b } = t\nprint(a)\n"), Vec::<&str>::new());
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
        assert_eq!(
            apply_fixes(src, &out.lints).0,
            "local _f = function() end\n"
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
            false,
            &crate::lint::Thresholds::default(),
            &privates,
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
    }

    /// An `if` expression in a `case` arm has no `end`; counting one
    /// closed the `impl` early and every later member read as private.
    #[test]
    fn an_if_expression_in_an_arm_keeps_the_impl_open() {
        let src = "enum C as\n    A(number)\n    B\nend\n\nstruct R as\n    private xs: number[]\nend\n\nimpl R as\n    public function viamatch(self, c: C): number\n        return match c with\n            case A(n) then if #self.xs > 0 then n else 0\n            case B then 0\n        end\n    end\n\n    public function stmt(self, c: C)\n        match c with\n            case A(n) then self.xs:push(n)\n            case B then print(\"b\")\n        end\n    end\nend\n\nreturn R\n";
        assert_eq!(names(src), Vec::<&str>::new());
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
