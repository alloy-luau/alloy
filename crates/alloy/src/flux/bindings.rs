//! Flux: the lints that read declarations and the names bound to them.
//! A private member read outside its struct's impl, a constant's value
//! changed through a method, one name given two function bodies, a
//! local nothing reads. The names and levels sit in `lint::LINTS`, and
//! the case of names is `crate::naming`.

use alloy_syntax::lexer::TokKind;

use super::Callable;
use super::scan::Scan;
use crate::lint::Lint;

/// The std methods that change the value they are called on. `const`
/// freezes the binding, so a call of one of these on a constant is the
/// write the keyword did not stop.
const MUTATING_METHODS: &[&str] = &[
    "push",
    "pop",
    "insert",
    "remove",
    "swap_remove",
    "clear",
    "set",
    "add",
    "sort",
    "sort_by",
    "reverse",
    "extend",
    "get_or_insert",
];

/// A write into the value a name holds, as `value_write` reads it.
enum ValueWrite<'s> {
    Assign,
    Method(&'s str),
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
                self.last_segment(i + 3)
            } else {
                self.last_segment(i + 1)
            };

            if best.is_none_or(|(b, _)| i > b) {
                best = Some((i, name));
            }
        }

        best.map(|(_, n)| n)
    }

    /// The block path a token sits in: the names of the `struct`,
    /// `impl` and `namespace` blocks that enclose it, outermost first.
    /// Two functions in different blocks share no scope, so they may
    /// share a name.
    fn enclosing_path(&self, j: usize) -> String {
        let mut parts: Vec<(usize, &'s str)> = Vec::new();

        for (i, e) in self.st.ends.iter().enumerate() {
            let Some(e) = e else { continue };

            if !(i < j && j < *e) || !matches!(self.t(i), "struct" | "impl" | "namespace") {
                continue;
            }

            let name = if self.at(i, "impl") && self.is_name(i + 1) && self.at(i + 2, "for") {
                self.last_segment(i + 3)
            } else {
                self.last_segment(i + 1)
            };

            parts.push((i, name));
        }

        parts.sort_unstable();
        parts.iter().map(|(_, n)| *n).collect::<Vec<_>>().join(".")
    }

    /// The last name of a dotted path that starts at `i`: `Zoo.Lion` is
    /// `Lion`. An `impl` may target a namespace member, and the struct
    /// that declares a private member is the member itself.
    fn last_segment(&self, i: usize) -> &'s str {
        match self.path_end(i) {
            Some(end) => self.t(end - 1),

            None => self.t(i),
        }
    }

    /// The type a name carries in this file: a parameter or a local
    /// annotation, or the struct a `new` builds. `None` when the file
    /// does not say.
    pub(crate) fn declared_type(&self, name: &str) -> Option<&'s str> {
        (0..self.toks.len())
            .filter(|&i| self.is_name(i) && self.t(i) == name)
            .find_map(|i| self.decl_type(i))
    }

    /// The type the name at `at` carries: the declaration it reads says,
    /// or the first declaration of the name in the file when no local
    /// or parameter in scope binds it.
    pub(crate) fn type_at(&self, at: usize) -> Option<&'s str> {
        match self.binding_at(at) {
            Some(d) => self.decl_type(d),

            None => self.declared_type(self.t(at)),
        }
    }

    /// The type the declaration at `i` gives its name: an annotation on
    /// a parameter or a local, or the struct a `new` builds.
    fn decl_type(&self, i: usize) -> Option<&'s str> {
        let introduced = matches!(self.prev(i), "(" | "," | "local" | "const");

        if introduced && self.at(i + 1, ":") {
            let mut j = i + 2;

            while matches!(self.t(j), "read" | "write") {
                j += 1;
            }

            // `print(c:ready())` reads as `(c: ready)` from the tokens
            // alone; the `(` after the name says it is a method call,
            // not an annotation.
            if self.is_name(j) && !self.at(j + 1, "(") {
                return Some(self.last_segment(j));
            }
        }

        (matches!(self.prev(i), "local" | "const")
            && self.at(i + 1, "=")
            && self.at(i + 2, "new")
            && self.is_name(i + 3))
        .then(|| self.last_segment(i + 3))
    }

    /// Each function a `@deprecated` marks: its `function` token and
    /// the note.
    fn deprecations(&self) -> Vec<(usize, String)> {
        let mut out = Vec::new();

        for i in 0..self.toks.len() {
            if !(self.at(i, "@") && self.at(i + 1, "deprecated")) {
                continue;
            }

            let (note, mut j) = match self.at(i + 2, "(").then(|| self.matching(i + 2)) {
                Some(Some(close)) => (self.deprecation_note(i + 3, close), close + 1),

                _ => (String::new(), i + 2),
            };

            // Other attributes and the modifiers of the method.
            loop {
                if self.at(j, "@") && self.is_name(j + 1) {
                    j += 2;

                    if self.at(j, "(") {
                        let Some(close) = self.matching(j) else { break };
                        j = close + 1;
                    }
                } else if matches!(self.t(j), "private" | "public" | "async" | "export") {
                    j += 1;
                } else {
                    break;
                }
            }

            if self.at(j, "function") {
                out.push((j, note));
            }
        }

        out
    }

    /// Every function a call can name, keyed the way the call spells
    /// it: `heal`, `Box.new`, `Box:value`, or `t.f` for a function in
    /// the table a local holds. A method that takes `self` answers to
    /// `Box.value` as well. A function inside a `trait` has no body a
    /// call reaches, so it is left out.
    pub(crate) fn callables(&self) -> Vec<(String, Callable)> {
        let notes = self.deprecations();
        let mut out: Vec<(String, Callable)> = Vec::new();
        let mut add = |key: String, c: Callable| match out.iter_mut().find(|(k, _)| *k == key) {
            // Two declarations of one name make the count a range.
            Some((_, seen)) => {
                seen.params = None;
                seen.exported |= c.exported;
                seen.deprecated = seen.deprecated.take().or(c.deprecated);
            }

            None => out.push((key, c)),
        };

        for f in 0..self.toks.len() {
            if !self.at(f, "function") || matches!(self.prev(f), "." | ":" | "type") {
                continue;
            }

            let mut open = f + 1;

            while self.is_name(open) || self.at(open, ".") || self.at(open, ":") {
                open += 1;
            }

            let name_end = open;

            // The generic parameters of `function pick<T>(...)`.
            if self.at(open, "<") {
                let mut depth = 0i32;

                while open < self.toks.len() {
                    depth += match self.t(open) {
                        "<" => 1,

                        ">" => -1,

                        _ => 0,
                    };
                    open += 1;

                    if depth == 0 {
                        break;
                    }
                }
            }

            if !self.at(open, "(") {
                continue;
            }

            let params = self.param_count(open);
            let exported = self.prev(f) == "export"
                || (self.prev(f) == "async" && f >= 2 && self.at(f - 2, "export"));
            let make = |params: Option<usize>, exported: bool| Callable {
                params,
                deprecated: notes
                    .iter()
                    .find(|(at, _)| *at == f)
                    .map(|(_, n)| n.clone()),
                exported,
            };
            let written = self.slice(f + 1, name_end);
            let block = self.enclosing_block(f, &["function", "struct", "impl", "trait"]);

            // `function one()` in `namespace Ns` is `Ns.one` outside it.
            // A `local function` there is no member.
            if name_end == f + 2
                && self.prev(f) != "local"
                && let Some((path, exported)) = self.namespace_path(f)
            {
                let exported = exported && self.prev(f) != "private";
                add(format!("{path}.{written}"), make(params, exported));
            }

            if written.contains(':') {
                // `function Box:value(n)` takes `self` without writing it.
                let params = params.map(|p| p + 1);
                add(written.replacen(':', ".", 1), make(params, true));
                add(written.to_string(), make(params, true));
            } else if name_end == f + 2
                && block.is_some_and(|b| matches!(self.t(b), "struct" | "impl"))
            {
                let Some(owner) = self.enclosing_owner(f) else {
                    continue;
                };
                let name = self.t(f + 1);

                if self.at(open + 1, "self") {
                    add(format!("{owner}:{name}"), make(params, true));
                }

                add(format!("{owner}.{name}"), make(params, true));
            } else if block.is_some_and(|b| self.at(b, "trait")) {
                continue;
            } else if name_end > f + 1 {
                add(written.to_string(), make(params, exported));
            } else if f >= 2 && self.at(f - 1, "=") && self.is_name(f - 2) {
                // `local heal = function` and `{ heal = function }`.
                let name = self.t(f - 2);

                if matches!(self.prev(f - 2), "local" | "const") {
                    add(name.to_string(), make(params, false));
                } else if matches!(self.prev(f - 2), "{" | ",")
                    && let Some(table) = self.table_local(f - 2)
                {
                    add(format!("{table}.{name}"), make(params, false));
                }
            }
        }

        // A write to `t.f` after the table puts another function there.
        for i in 0..self.toks.len() {
            if let Some(end) = self.path_end(i)
                && end > i + 1
                && self.at(end, "=")
                && self.statement_start(i)
                && let Some((_, c)) = out.iter_mut().find(|(k, _)| k == self.slice(i, end))
            {
                c.params = None;
            }
        }

        out
    }

    /// The namespaces that hold the member at `j`, as `Outer.Inner`,
    /// and whether the outermost one is exported. `None` when a
    /// function, a struct, an impl or a trait holds it first.
    fn namespace_path(&self, j: usize) -> Option<(String, bool)> {
        const HOLDERS: &[&str] = &["function", "struct", "impl", "trait", "namespace"];
        let holder = |at: usize| {
            self.enclosing_block(at, HOLDERS)
                .filter(|&b| self.at(b, "namespace"))
        };
        let mut ns = holder(j)?;
        let mut names = vec![self.t(ns + 1)];

        while let Some(outer) = holder(ns) {
            names.push(self.t(outer + 1));
            ns = outer;
        }

        names.reverse();

        Some((names.join("."), self.prev(ns) == "export"))
    }

    /// The innermost block of one of `kinds` that encloses token `j`.
    fn enclosing_block(&self, j: usize, kinds: &[&str]) -> Option<usize> {
        (0..j)
            .rev()
            .find(|&i| kinds.contains(&self.t(i)) && self.st.ends[i].is_some_and(|e| j < e))
    }

    /// The local a table constructor goes into, `t` in `local t = {`,
    /// for the field name at `k` inside it.
    fn table_local(&self, k: usize) -> Option<&'s str> {
        let mut depth = 0i32;

        for i in (0..k).rev() {
            match self.t(i) {
                ")" | "]" | "}" => depth += 1,

                "(" | "[" => depth -= 1,

                "{" if depth == 0 => {
                    return (i >= 3
                        && self.at(i - 1, "=")
                        && self.is_name(i - 2)
                        && matches!(self.t(i - 3), "local" | "const"))
                    .then(|| self.t(i - 2));
                }

                "{" => depth -= 1,

                _ => {}
            }
        }

        None
    }

    /// The parameters of the list that opens at `open`, `self` counted.
    /// `None` when a vararg or a default makes the count a range. A
    /// destructured parameter counts as the one argument it takes.
    fn param_count(&self, open: usize) -> Option<usize> {
        let close = self.matching(open)?;
        let mut depth = 0i32;
        let mut slots = usize::from(close > open + 1);

        for k in open + 1..close {
            let t = self.t(k);

            if t.ends_with('(') || t.ends_with('[') || t.ends_with('{') || t == "<" {
                depth += 1;
            } else if matches!(t, ")" | "]" | "}" | ">") {
                depth -= 1;
            } else if depth == 0 && t == "," {
                slots += 1;
            } else if depth == 0 && (t == "..." || t == "=") {
                return None;
            }
        }

        Some(slots)
    }

    /// The `)` of the call whose `(` is at `open`, and the arguments the
    /// call passes. A string holds no bracket, and an interpolated
    /// string opens at its head and closes at its tail, so a comma in a
    /// hole stays inside it.
    fn call_args(&self, open: usize) -> Option<(usize, usize)> {
        let mut depth = 0i32;
        let mut commas = 0;
        // The depth inside the type arguments of a call, `f<<K, V>>()`:
        // their commas split no argument. Two `<` side by side open them
        // and nothing else, since Luau has no shift operator.
        let mut angle = 0i32;

        for k in open..self.toks.len() {
            let text = self.t(k);

            if angle > 0 || (text == "<" && self.at(k + 1, "<")) {
                match text {
                    "<" => angle += 1,

                    ">" => angle -= 1,

                    _ => {}
                }

                continue;
            }

            depth += match self.toks[k].kind {
                TokKind::InterpHead => 1,

                TokKind::InterpTail => -1,

                TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpMid => 0,

                _ if text.ends_with('(') || text.ends_with('[') || text.ends_with('{') => 1,

                _ if matches!(text, ")" | "]" | "}") => -1,

                _ => 0,
            };

            if depth == 0 {
                return Some((k, if k == open + 1 { 0 } else { commas + 1 }));
            }

            if depth == 1 && text == "," {
                commas += 1;
            }
        }

        None
    }

    /// A call with more arguments than the function takes. Luau's
    /// solver reports too few and misses too many, and the extra values
    /// are dropped in silence. The count is exact for a function the
    /// file or an imported module declares once with a fixed list: a
    /// plain name, `M.f`, a static, a method on a value the file types,
    /// and a function in a local table.
    pub(crate) fn argument_count(&self, out: &mut Vec<Lint>) {
        let own = self.callables();
        let find = |key: &str| {
            own.iter()
                .chain(self.callables)
                .find(|(k, _)| k == key)
                .and_then(|(_, c)| c.params)
        };

        for i in 0..self.toks.len() {
            if !self.is_name(i)
                || matches!(
                    self.prev(i),
                    "." | ":" | "?." | "?:" | "function" | "local" | "const"
                )
            {
                continue;
            }

            let Some(end) = self.path_end(i) else {
                continue;
            };
            let (key, open, colon) = if self.at(end, "(") {
                // A parameter of that name is some other value.
                if end > i + 1
                    && self
                        .binding_at(i)
                        .is_some_and(|d| !(self.at(d + 1, "=") && self.at(d + 2, "{")))
                {
                    continue;
                }

                (self.slice(i, end).to_string(), end, false)
            } else if end == i + 1
                && self.at(end, ":")
                && self.is_name(end + 1)
                && self.at(end + 2, "(")
            {
                let ty = match self.t(i) {
                    "self" => self.enclosing_owner(i),

                    _ => self.type_at(i),
                };
                let Some(ty) = ty else { continue };

                (format!("{ty}:{}", self.t(end + 1)), end + 2, true)
            } else {
                continue;
            };
            let Some(takes) = find(&key) else { continue };
            let Some((close, given)) = self.call_args(open) else {
                continue;
            };
            // A `:` call passes the value as `self`, which no one wrote.
            let takes = if colon {
                takes.saturating_sub(1)
            } else {
                takes
            };

            if given <= takes {
                continue;
            }

            let word = |n: usize| if n == 1 { "argument" } else { "arguments" };
            self.lint(
                out,
                "argument_count",
                i,
                close,
                format!(
                    "`{}` takes {takes} {}; this call passes {given}",
                    self.slice(i, open),
                    word(takes)
                ),
                None,
            );
        }
    }

    /// `b:value()` where `value` is a method its impl marks
    /// `@deprecated`, and the file types `b` as the struct: an
    /// annotation, or the struct a `new` builds. The impl may sit in
    /// this file or in a module the file imports. Luau reports
    /// `Box.value(b)`, but its lint does not follow a method call
    /// through the metatable.
    pub(crate) fn deprecated_call(&self, out: &mut Vec<Lint>) {
        let own = self.callables();
        let marked: Vec<(&str, &str)> = own
            .iter()
            .chain(self.callables.iter())
            .filter(|(k, _)| k.contains(':'))
            .filter_map(|(k, c)| Some((k.as_str(), c.deprecated.as_deref()?)))
            .collect();

        if marked.is_empty() {
            return;
        }

        for i in 0..self.toks.len() {
            if !self.is_name(i)
                || matches!(self.prev(i), "." | ":" | "?." | "?:")
                || !self.at(i + 1, ":")
                || !self.is_name(i + 2)
                || !self.at(i + 3, "(")
            {
                continue;
            }

            let Some(ty) = self.type_at(i) else { continue };
            let key = format!("{ty}:{}", self.t(i + 2));
            let Some((_, note)) = marked.iter().find(|(k, _)| *k == key) else {
                continue;
            };

            self.lint(
                out,
                "deprecated_call",
                i + 2,
                i + 2,
                format!("`{key}` is deprecated{note}"),
                None,
            );
        }
    }

    /// The note of `@deprecated(...)` between `from` and `close`: a
    /// string, or a table of `reason` and `use`.
    fn deprecation_note(&self, from: usize, close: usize) -> String {
        if close == from + 1
            && let Some(text) = self.string_content(from)
        {
            return format!("; {text}");
        }

        let key = |k: &str| {
            (from..close)
                .find(|&t| self.at(t, k) && self.at(t + 1, "="))
                .and_then(|t| self.string_content(t + 2))
        };

        match (key("reason"), key("use")) {
            (Some(r), Some(u)) => format!("; {r}; use `{u}`"),

            (Some(r), None) => format!("; {r}"),

            (None, Some(u)) => format!("; use `{u}`"),

            (None, None) => String::new(),
        }
    }

    /// `x.count` or `x:reset()` outside the impl of the struct that
    /// declared `count` or `reset` private.
    pub(crate) fn private_access(&self, out: &mut Vec<Lint>) {
        let mut members = self.private_members();

        // A struct another module declares reaches this file through an
        // import; its private fields come with the shape, not the tokens.
        for (owner, fields) in self.privates {
            for field in fields {
                if !members.iter().any(|(m, o)| *m == field && *o == owner) {
                    members.push((field.as_str(), owner.as_str()));
                }
            }
        }

        if members.is_empty() {
            return;
        }

        for i in 0..self.toks.len() {
            if !matches!(self.prev(i), "." | ":" | "?." | "?:") || !self.is_name(i) {
                continue;
            }

            let name = self.t(i);

            if !members.iter().any(|(m, _)| *m == name) {
                continue;
            }

            // The receiver decides which struct the member belongs to.
            // Without it, a field named `coins` on an unrelated record
            // reads as the private `coins` of a struct nearby.
            let base = i
                .checked_sub(2)
                .filter(|b| self.is_name(*b))
                .map(|b| self.t(b))
                .filter(|n| *n != "self");
            let owner = match base.and_then(|n| self.declared_type(n)) {
                Some(ty) => members.iter().find(|(m, o)| *m == name && *o == ty),

                None => members.iter().find(|(m, _)| *m == name),
            };
            let Some((_, owner)) = owner else {
                continue;
            };

            // `Lifecycle.Start` reads a variant of the enum; it is not
            // the private `Start` of a struct elsewhere in the file. A
            // receiver the file gives no type and spells with a capital
            // is a type, a namespace, or a module, so the member is
            // that one's, unless the receiver is the owner itself.
            if base.is_some_and(|n| {
                n != *owner
                    && self.declared_type(n).is_none()
                    && n.starts_with(|c: char| c.is_ascii_uppercase())
            }) {
                continue;
            }

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

        self.private_constructor_keys(&members, out);
    }

    /// `new Vault { code = 7 }` outside the impl of `Vault`, where
    /// `code` is private and carries a default. A private field without
    /// a default has to be set at construction, so that one stays.
    fn private_constructor_keys(&self, members: &[(&'s str, &'s str)], out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "new") || !self.is_name(i + 1) {
                continue;
            }

            // `new Zoo.Box { }`: the struct is the member the path
            // names, which is how `private_members` keys it.
            let after = self.path_end(i + 1).unwrap_or(i + 2);
            let owner = self.t(after - 1);

            if self.enclosing_owner(i) == Some(owner) {
                continue;
            }

            // `new Name<<T>> { }` and `new Name(args) { }`: the table
            // comes after the group the head carries.
            let mut j = after;

            while matches!(self.t(j), "(" | "<") {
                match self.matching(j) {
                    Some(close) => j = close + 1,

                    None => break,
                }
            }

            if !self.at(j, "{") {
                continue;
            }

            let Some(close) = self.matching(j) else {
                continue;
            };
            let mut k = j + 1;

            while k < close {
                if self.is_name(k)
                    && self.at(k + 1, "=")
                    && members.iter().any(|(m, o)| *m == self.t(k) && *o == owner)
                    && self.field_has_default(self.t(k), owner)
                {
                    let name = self.t(k);
                    self.lint(
                        out,
                        "private_access",
                        k,
                        k,
                        format!("`{name}` is private to `{owner}`; only its impl sets it"),
                        None,
                    );
                }

                k += 1;
            }
        }
    }

    /// A field of `owner` declared with a default value: an `=` on the
    /// line the field name opens.
    fn field_has_default(&self, field: &str, owner: &str) -> bool {
        for i in 0..self.toks.len() {
            if !self.is_name(i) || self.t(i) != field || !self.at(i + 1, ":") {
                continue;
            }

            if self.enclosing_owner(i) != Some(owner) {
                continue;
            }

            let line = self.line_of(i);
            let mut j = i + 2;

            while j < self.toks.len() && self.line_of(j) == line {
                if self.at(j, "=") {
                    return true;
                }

                j += 1;
            }
        }

        false
    }

    /// One name given a `function` body twice in one scope. The second
    /// body replaces the first, so the first never runs. The checker
    /// reports it as `DuplicateFunction`, in the emit's names; this one
    /// names the path the source wrote.
    pub(crate) fn duplicate_function(&self, out: &mut Vec<Lint>) {
        let mut seen: Vec<(String, usize)> = Vec::new();

        for i in 0..self.toks.len() {
            if !self.at(i, "function") {
                continue;
            }

            let mut head = i;

            while head > 0
                && matches!(
                    self.t(head - 1),
                    "local" | "export" | "global" | "async" | "private" | "public"
                )
            {
                head -= 1;
            }

            if !self.statement_start(head) || !self.is_name(i + 1) {
                continue;
            }

            // A `@cfg` pair declares one name per build, so the two
            // bodies never stand together. Any other attribute, such as
            // `@test`, leaves both bodies in the build.
            if self.cfg_gated(head) {
                continue;
            }

            // A trait's method is a contract each impl writes, not a body
            // of the file: two traits may name one method.
            if self.inside_block(i, &["trait"]) {
                continue;
            }

            let start = i + 1;
            let mut j = start + 1;

            while matches!(self.t(j), "." | ":") && self.is_name(j + 1) {
                j += 2;
            }

            let path = self.slice(start, j);
            let key = format!("{}.{path}", self.enclosing_path(start));

            match seen.iter().find(|(k, _)| *k == key) {
                Some((_, first)) => {
                    let line = self.line_of(*first) + 1;
                    self.lint(
                        out,
                        "duplicate_function",
                        start,
                        j - 1,
                        format!(
                            "`{path}` already has a body, on line {line}; this one replaces it"
                        ),
                        None,
                    );
                }

                None => seen.push((key, start)),
            }
        }
    }

    /// Whether a `@cfg` line stands right above the token.
    fn cfg_gated(&self, i: usize) -> bool {
        let at = self.start(i) as usize;
        let from = self.src[..at].rfind('\n').map_or(0, |n| n + 1);
        let above = self.src[..from].trim_end();
        let above = &above[above.rfind('\n').map_or(0, |n| n + 1)..];
        above.trim_start().starts_with("@cfg")
    }

    /// A write into the value a `const` holds: `X.field = v`, `X[k] = v`,
    /// or a call of a method that changes it. `const` freezes the
    /// binding alone, which the keyword does not say.
    pub(crate) fn const_mutation(&self, out: &mut Vec<Lint>) {
        // Each const name with the token that declares it. A write reaches
        // the const only inside its block and past any nearer binding of
        // the name, such as a `local` in another function.
        let mut names: Vec<(&'s str, usize)> = Vec::new();

        for i in 0..self.toks.len() {
            if !self.at(i, "const")
                || !self.statement_start(if self.at(i.wrapping_sub(1), "local") {
                    i - 1
                } else {
                    i
                })
            {
                continue;
            }

            for j in self.local_names(i) {
                names.push((self.t(j), j));
            }
        }

        if names.is_empty() {
            return;
        }

        for i in 0..self.toks.len() {
            if !self.is_name(i)
                || !names
                    .iter()
                    .any(|&(n, at)| n == self.t(i) && self.reads_binding(at, i))
            {
                continue;
            }

            let name = self.t(i);

            match self.value_write(i) {
                Some(ValueWrite::Assign) => self.lint(
                    out,
                    "const_mutation",
                    i,
                    i,
                    format!(
                        "`{name}` is a `const`; the binding is fixed and this writes into its value"
                    ),
                    None,
                ),

                Some(ValueWrite::Method(method)) => self.lint(
                    out,
                    "const_mutation",
                    i,
                    i + 2,
                    format!(
                        "`{name}` is a `const`; `{method}` changes the value the binding holds"
                    ),
                    None,
                ),

                None => {}
            }
        }
    }

    /// Whether the name at `j` reads the binding that the name at `n`
    /// declares: `j` is in the block of `n`, and no nearer `local`,
    /// `const` or parameter of the name holds it.
    fn reads_binding(&self, n: usize, j: usize) -> bool {
        n < j && j < self.scope_end(n) && self.binding_at(j).is_none_or(|d| d <= n)
    }

    /// How the statement at the name `i` writes into the value the name
    /// holds: `X.a.b = v`, `X[k] = v`, and `X.n += 1` assign into it,
    /// `X:push(v)` calls a method that changes it. The binding itself
    /// stands.
    fn value_write(&self, i: usize) -> Option<ValueWrite<'s>> {
        // A method call changes the value wherever it stands, so
        // `local r = bag:add(s)` writes into `bag` as `bag:add(s)` does.
        if !matches!(self.prev(i), "." | ":" | "?." | "?:")
            && self.at(i + 1, ":")
            && MUTATING_METHODS.contains(&self.t(i + 2))
            && self.is_member(i + 2)
        {
            return Some(ValueWrite::Method(self.t(i + 2)));
        }

        if !self.statement_start(i) {
            return None;
        }

        let assigned = match self.path_end(i) {
            Some(end) if end > i + 1 && self.assigns_at(end) => true,

            _ => {
                self.at(i + 1, "[") && self.matching(i + 1).is_some_and(|c| self.assigns_at(c + 1))
            }
        };

        assigned.then_some(ValueWrite::Assign)
    }

    /// Whether token `k` assigns: `=`, a compound operator such as `+=`
    /// or `..=`, or `??=`, which lexes as `?`, `?`, and `=`.
    fn assigns_at(&self, k: usize) -> bool {
        matches!(
            self.t(k),
            "=" | "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "^=" | "..="
        ) || (self.at(k, "?") && self.at(k + 1, "?") && self.at(k + 2, "="))
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
        // So does `local Pt { x } = e`: `Pt` names the struct, and the
        // braces hold the names. Either one can take a dotted path.
        let mut head = j;

        while self.is_name(head) && self.at(head + 1, ".") && self.is_name(head + 2) {
            head += 2;
        }

        if self.is_name(head) && self.at(head + 1, "(") {
            return out;
        }

        if self.is_name(head) && self.at(head + 1, "{") {
            return self.pattern_names(head + 1).0;
        }

        // `local { a, b = c } = t` and `local [ x, ...rest ] = t` bind
        // the names the pattern holds, not one name of their own.
        if matches!(self.t(j), "{" | "[") {
            return self.pattern_names(j).0;
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

    /// The names a destructuring pattern binds, from the `{` or `[` at
    /// `open` to the token that closes it.
    ///
    /// `{ a, b = c }` binds `a` and `c`: the name on the right of `=`
    /// is the local, the one on the left is the field it reads. `[ x,
    /// ...rest ]` binds `x` and `rest`. A nested pattern hands its own
    /// names up the same way.
    ///
    /// `{ a: T }` binds `a`, and `T` is a type. The second value is the
    /// token that closes the pattern.
    fn pattern_names(&self, open: usize) -> (Vec<usize>, usize) {
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut j = open;

        while j < self.toks.len() {
            match self.t(j) {
                "{" | "[" => depth += 1,

                "}" | "]" => {
                    depth -= 1;

                    if depth == 0 {
                        break;
                    }
                }

                ":" => {
                    j = self.annotation_end(j + 1);

                    continue;
                }

                // A name before `(`, `{` or `.`, or after `.`, names a
                // variant or a struct in a nested pattern: `Some(v)`,
                // `Pt { x }`, `Kind.Big`.
                _ if self.is_name(j)
                    && !matches!(self.t(j + 1), "=" | "(" | "{" | ".")
                    && self.prev(j) != "." =>
                {
                    out.push(j)
                }

                _ => {}
            }

            j += 1;
        }

        (out, j)
    }

    /// Whether the name at `n`, in the binding that starts at `from`, is
    /// a shorthand entry of a table pattern, `{ a, b }`. The name is the
    /// field it reads, so a rename has to keep the field: `b = _b`.
    fn shorthand_entry(&self, from: usize, n: usize) -> bool {
        if !matches!(self.prev(n), "{" | ",") {
            return false;
        }

        let mut depth = 0;

        for j in (from..n).rev() {
            match self.t(j) {
                "}" | "]" | ")" => depth += 1,

                "{" | "[" | "(" if depth == 0 => return self.at(j, "{"),

                "{" | "[" | "(" => depth -= 1,

                _ => {}
            }
        }

        false
    }

    /// Whether the name at `n` appears again after token `from`.
    ///
    /// A member after `.` or `:` belongs to the value on its left, so
    /// `t.origin` is not a read of a local named `origin`.
    fn read_after(&self, n: usize, from: usize) -> bool {
        let name = self.t(n);

        (from..self.toks.len()).any(|j| {
            j != n && self.toks[j].kind == TokKind::Ident && self.t(j) == name && !self.is_member(j)
        })
    }

    /// Whether the name at `j` is a member of the value before it:
    /// `t.name`, or `t:name(...)`. A name after `:` that no argument
    /// follows is a type annotation, which does read the name.
    fn is_member(&self, j: usize) -> bool {
        match self.prev(j) {
            "." | "?." => true,

            ":" | "?:" => {
                let arg = self.t(j + 1);

                arg == "(" || arg == "{" || arg.starts_with(['"', '\'', '`'])
            }

            _ => false,
        }
    }

    /// A local or a loop variable that nothing reads after it, as
    /// `unused_variable`; a function that nothing calls, declared or
    /// bound to a local, as `unused_function`.
    pub(crate) fn unused_variable(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            // A namespace is one name. Its members read as `Math.PI`
            // from outside, which is not the token this scan looks for,
            // so the namespace's own name is the one that must be read.
            if self.inside_block(i, &["namespace"]) {
                continue;
            }

            let (names, is_function): (Vec<usize>, bool) = match self.t(i) {
                "local" | "const" if self.statement_start(i) => {
                    // The module table reads an export, and a `global`
                    // reads from anywhere, so neither name is unused.
                    // `export const X`, `export local x`,
                    // `export default const X` and
                    // `export const { a, b } = t` all stay out.
                    if self.sends_out(i) {
                        continue;
                    }

                    let names = self.local_names(i);
                    let is_function = self.binds_function(i, &names);

                    // An attribute hands the function to the runtime or
                    // the compiler, so `@test local function f` is a
                    // test the spec calls, as `@test function f` is.
                    if is_function && self.has_attribute(i) {
                        continue;
                    }

                    (names, is_function)
                }

                "for" if self.statement_start(i) => {
                    let mut names = Vec::new();
                    let mut j = i + 1;

                    while j < self.toks.len() && !matches!(self.t(j), "in" | "=" | "do") {
                        // `for a: T in` binds `a`. The `T` after the
                        // colon reads a type, so it is not a binding.
                        if self.at(j, ":") {
                            j = self.annotation_end(j + 1);
                            continue;
                        }

                        // `for _, { a = q } in` binds `q`, and `a` is
                        // the field it reads.
                        if matches!(self.t(j), "{" | "[") {
                            let (bound, close) = self.pattern_names(j);
                            names.extend(bound);
                            j = close + 1;
                            continue;
                        }

                        if self.is_name(j) {
                            names.push(j);
                        }

                        j += 1;
                    }

                    (names, false)
                }

                // `function f` and `async function f` at statement level:
                // a plain name, not exported, not a method of an `impl`
                // or a `trait`. A global reads from anywhere in the file.
                "function" | "async" if self.statement_start(i) && !self.sends_out(i) => {
                    let f = if self.at(i, "async") { i + 1 } else { i };

                    if !self.at(f, "function")
                        || !self.is_name(f + 1)
                        || !self.at(f + 2, "(")
                        || self.inside_block(
                            i,
                            // A `class` body has no lowering yet and the
                            // render blanks it, so its members are not
                            // functions of the file.
                            &["impl", "trait", "struct", "declare", "namespace", "class"],
                        )
                        || self.has_attribute(i)
                    {
                        continue;
                    }

                    let n = f + 1;
                    let name = self.t(n);

                    if name.starts_with('_') || self.read_after(n, 0) {
                        continue;
                    }

                    self.lint(
                        out,
                        "unused_function",
                        n,
                        n,
                        format!("`{name}` is never called; prefix it with `_` or remove it"),
                        Some(format!("_{name}")),
                    );

                    continue;
                }

                _ => continue,
            };

            // A top-level `local function` that a body above it calls:
            // the emit declares the name on the first line, so the
            // read counts, as it does for `function f`.
            let from_start = is_function && !self.inside_any_block(i);
            // A body above a top-level `local` or `const` reads a global
            // of that name, since a local is not hoisted. The checker
            // reports that read and says to move the declaration up, so
            // the name counts as read.
            let top_value = !is_function && self.t(i) != "for" && !self.inside_any_block(i);

            for n in names {
                let name = self.t(n);
                let from = if from_start { 0 } else { n + 1 };
                let read_above = top_value
                    && (0..i).any(|j| {
                        self.toks[j].kind == TokKind::Ident
                            && self.t(j) == name
                            && !self.is_member(j)
                            && self.inside_block(j, &["function"])
                    });

                if name.starts_with('_') || self.read_after(n, from) || read_above {
                    continue;
                }

                let (lint, verb) = if is_function {
                    ("unused_function", "called")
                } else {
                    ("unused_variable", "read")
                };
                let fix = match self.shorthand_entry(i, n) {
                    true => format!("{name} = _{name}"),

                    false => format!("_{name}"),
                };

                self.lint(
                    out,
                    lint,
                    n,
                    n,
                    format!("`{name}` is never {verb}; prefix it with `_` or remove it"),
                    Some(fix),
                );
            }
        }
    }

    /// `local x = v` that nothing assigns again reads as `const x = v`:
    /// the word says the binding holds one value, and a later write
    /// becomes a compile error. The scan is file-wide, as the one for
    /// reads is, so a write to any local of the name keeps it quiet. A
    /// later `local` of the name holds the writes in its own block.
    pub(crate) fn prefer_const(&self, out: &mut Vec<Lint>) {
        for i in 0..self.toks.len() {
            if !self.at(i, "local")
                || !self.statement_start(i)
                || matches!(self.t(i + 1), "function" | "async" | "const")
            {
                continue;
            }

            let names = self.local_names(i);
            let Some(&last) = names.last() else {
                continue;
            };
            // `local x` with no value takes one later, by an assignment.
            let after = match self.t(i + 1) {
                "{" | "[" => self.pattern_names(i + 1).1 + 1,

                _ if self.at(last + 1, ":") => self.annotation_end(last + 2),

                _ => last + 1,
            };

            // A value written into stays `local`: `const_mutation` reads
            // `const` as deep, and the two would disagree.
            if !self.at(after, "=")
                || names.iter().any(|&n| {
                    self.written_after(n)
                        || (n + 1..self.toks.len()).any(|j| {
                            self.t(j) == self.t(n)
                                && !self.is_member(j)
                                && self.value_write(j).is_some()
                                && self.reads_binding(n, j)
                        })
                })
            {
                continue;
            }

            let listed: Vec<String> = names.iter().map(|&n| format!("`{}`", self.t(n))).collect();
            let message = match listed.len() {
                1 => format!("{} is never assigned again; declare it `const`", listed[0]),

                _ => format!(
                    "{} are never assigned again; declare them `const`",
                    listed.join(", ")
                ),
            };
            self.lint(
                out,
                "prefer_const",
                i,
                i,
                message,
                Some("const".to_string()),
            );
        }
    }

    /// Whether a statement after the name at `n` assigns a name of its
    /// text: `x = 1`, `x += 1`, or a target of `a, x = f()`.
    pub(super) fn written_after(&self, n: usize) -> bool {
        let name = self.t(n);

        (n + 1..self.toks.len()).any(|j| {
            if self.toks[j].kind != TokKind::Ident || self.t(j) != name || self.is_member(j) {
                return false;
            }

            // The rest of a list of targets, then the operator.
            let mut k = j + 1;

            while self.at(k, ",") && self.is_name(k + 1) {
                k += 2;
            }

            if !self.assigns_at(k) || !self.reads_binding(n, j) {
                return false;
            }

            // The first target opens the statement; `local x =` again
            // declares a new local instead. A name right after the end of
            // an expression opens one too: `function() n += 1 end` holds
            // the write on the line of the `function`.
            let mut first = j;

            while first >= 2 && self.at(first - 1, ",") && self.is_name(first - 2) {
                first -= 2;
            }

            let after_expression = first > 0
                && (matches!(self.prev(first), ")" | "]" | "}")
                    || matches!(
                        self.toks[first - 1].kind,
                        TokKind::Str { .. }
                            | TokKind::InterpStr
                            | TokKind::InterpTail
                            | TokKind::Number
                    )
                    || self.is_name(first - 1));

            (self.statement_start(first) || after_expression)
                && !matches!(self.prev(first), "local" | "const")
        })
    }

    /// Whether the declaration at `i` sends its name out of the file:
    /// `export`, `global`, or `export default` in front of it.
    fn sends_out(&self, i: usize) -> bool {
        match self.prev(i) {
            "export" | "global" => true,

            "default" => i >= 2 && self.t(i - 2) == "export",

            _ => false,
        }
    }

    /// Whether the `local` or `const` at `i` binds a function: `local
    /// function f`, `local async function f`, or one name with a
    /// function, plain or async, as its value.
    fn binds_function(&self, i: usize, names: &[usize]) -> bool {
        let mut j = i + 1;

        while matches!(self.t(j), "async" | "const") {
            j += 1;
        }

        if self.at(j, "function") {
            return true;
        }

        let [n] = names else {
            return false;
        };
        let eq = n + 1;

        self.at(eq, "=")
            && (self.at(eq + 1, "function")
                || (self.at(eq + 1, "async") && self.at(eq + 2, "function")))
    }

    /// Whether an attribute, `@test` or `@name(...)`, stands right
    /// before token `i`: the runtime or the compiler calls such a
    /// function, not the file.
    fn has_attribute(&self, i: usize) -> bool {
        let src = self.src.as_bytes();
        let mut k = self.start(i) as usize;

        {
            while k > 0 && (src[k - 1] as char).is_ascii_whitespace() {
                k -= 1;
            }

            if k == 0 {
                return false;
            }

            // Luau's list, `@[native]`: the `]` closes an `@[`.
            if src[k - 1] == b']' {
                let mut depth = 0i32;

                while k > 0 {
                    k -= 1;

                    match src[k] {
                        b']' => depth += 1,
                        b'[' => {
                            depth -= 1;

                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }

                return depth == 0 && k > 0 && src[k - 1] == b'@';
            }

            // `@name(...)`: step over the arguments.
            if src[k - 1] == b')' {
                let mut depth = 0i32;

                while k > 0 {
                    k -= 1;

                    match src[k] {
                        b')' => depth += 1,
                        b'(' => {
                            depth -= 1;

                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }

                if depth != 0 {
                    return false;
                }
            }

            let end = k;

            while k > 0 && ((src[k - 1] as char).is_ascii_alphanumeric() || src[k - 1] == b'_') {
                k -= 1;
            }

            if k == end {
                return false;
            }

            k > 0 && src[k - 1] == b'@'
        }
    }

    /// Whether token `j` sits inside a block one of `kinds` opens.
    fn inside_any_block(&self, j: usize) -> bool {
        self.st
            .ends
            .iter()
            .enumerate()
            .any(|(i, e)| e.is_some_and(|e| i < j && j < e))
    }

    fn inside_block(&self, j: usize, kinds: &[&str]) -> bool {
        self.st
            .ends
            .iter()
            .enumerate()
            .any(|(i, e)| e.is_some_and(|e| i < j && j < e) && kinds.contains(&self.t(i)))
    }
}
