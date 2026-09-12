//! Flux: the lints that read declarations and the names bound to them.
//! A private member read outside its struct's impl, a constant's value
//! changed through a method, one name given two function bodies, a
//! local nothing reads, a name in the wrong case. The names and levels
//! sit in `lint::LINTS`.

use alloy_syntax::lexer::TokKind;

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
    "clear",
    "set",
    "add",
    "sort",
    "sort_by",
    "reverse",
    "extend",
    "get_or_insert",
];

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

    /// The type a name carries in this file: a parameter or a local
    /// annotation, or the struct a `new` builds. `None` when the file
    /// does not say.
    pub(crate) fn declared_type(&self, name: &str) -> Option<&'s str> {
        for i in 0..self.toks.len() {
            if !self.is_name(i) || self.t(i) != name {
                continue;
            }

            let introduced = matches!(self.prev(i), "(" | "," | "local" | "const");

            if introduced && self.at(i + 1, ":") {
                let mut j = i + 2;

                while matches!(self.t(j), "read" | "write") {
                    j += 1;
                }

                // `print(c:ready())` reads as `(c: ready)` from the
                // tokens alone; the `(` after the name says it is a
                // method call, not an annotation.
                if self.is_name(j) && !self.at(j + 1, "(") {
                    return Some(self.t(j));
                }
            }

            if matches!(self.prev(i), "local" | "const")
                && self.at(i + 1, "=")
                && self.at(i + 2, "new")
                && self.is_name(i + 3)
            {
                return Some(self.t(i + 3));
            }
        }

        None
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

            let owner = self.t(i + 1);

            if self.enclosing_owner(i) == Some(owner) {
                continue;
            }

            // `new Name<<T>> { }` and `new Name(args) { }`: the table
            // comes after the group the head carries.
            let mut j = i + 2;

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
            // bodies never stand together.
            if self.attributed(head) {
                continue;
            }

            let start = i + 1;
            let mut j = start + 1;

            while matches!(self.t(j), "." | ":") && self.is_name(j + 1) {
                j += 2;
            }

            let path = self.slice(start, j);
            let owner = self.enclosing_owner(start).unwrap_or("");
            let key = format!("{owner}.{path}");

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

    /// Whether an attribute line stands right above the token.
    fn attributed(&self, i: usize) -> bool {
        let at = self.start(i) as usize;
        let from = self.src[..at].rfind('\n').map_or(0, |n| n + 1);
        let above = self.src[..from].trim_end();
        let above = &above[above.rfind('\n').map_or(0, |n| n + 1)..];

        above.trim_start().starts_with('@')
    }

    /// A write into the value a `const` holds: `X.field = v`, `X[k] = v`,
    /// or a call of a method that changes it. `const` freezes the
    /// binding alone, which the keyword does not say.
    pub(crate) fn const_mutation(&self, out: &mut Vec<Lint>) {
        let mut names: Vec<&'s str> = Vec::new();

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
                names.push(self.t(j));
            }
        }

        if names.is_empty() {
            return;
        }

        for i in 0..self.toks.len() {
            if !self.is_name(i) || !names.contains(&self.t(i)) || !self.statement_start(i) {
                continue;
            }

            // `X.a.b = v` and `X[k] = v`: the binding stands, the value
            // does not.
            let assigned = match self.path_end(i) {
                Some(end) if end > i + 1 && self.at(end, "=") => true,

                _ => {
                    self.at(i + 1, "[") && self.matching(i + 1).is_some_and(|c| self.at(c + 1, "="))
                }
            };

            if assigned {
                let name = self.t(i);
                self.lint(
                    out,
                    "const_mutation",
                    i,
                    i,
                    format!(
                        "`{name}` is a `const`; the binding is fixed and this writes into its value"
                    ),
                    None,
                );

                continue;
            }

            if self.at(i + 1, ":")
                && self.is_name(i + 2)
                && MUTATING_METHODS.contains(&self.t(i + 2))
            {
                let (name, method) = (self.t(i), self.t(i + 2));
                self.lint(
                    out,
                    "const_mutation",
                    i,
                    i + 2,
                    format!(
                        "`{name}` is a `const`; `{method}` changes the value the binding holds"
                    ),
                    None,
                );
            }
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
                    let names = self.local_names(i);

                    (names.clone(), self.binds_function(i, &names))
                }

                "for" if self.statement_start(i) => {
                    let mut names = Vec::new();
                    let mut j = i + 1;

                    while j < self.toks.len() && !matches!(self.t(j), "in" | "=" | "do") {
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
                "function" | "async"
                    if self.statement_start(i) && !matches!(self.prev(i), "export" | "global") =>
                {
                    let f = if self.at(i, "async") { i + 1 } else { i };

                    if !self.at(f, "function")
                        || !self.is_name(f + 1)
                        || !self.at(f + 2, "(")
                        || self
                            .inside_block(i, &["impl", "trait", "struct", "declare", "namespace"])
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

            for n in names {
                let name = self.t(n);

                if name.starts_with('_') || self.read_after(n, n + 1) {
                    continue;
                }

                let (lint, verb) = if is_function {
                    ("unused_function", "called")
                } else {
                    ("unused_variable", "read")
                };

                self.lint(
                    out,
                    lint,
                    n,
                    n,
                    format!("`{name}` is never {verb}; prefix it with `_` or remove it"),
                    Some(format!("_{name}")),
                );
            }
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
    fn inside_block(&self, j: usize, kinds: &[&str]) -> bool {
        self.st
            .ends
            .iter()
            .enumerate()
            .any(|(i, e)| e.is_some_and(|e| i < j && j < e) && kinds.contains(&self.t(i)))
    }

    /// The case of declared names.
    pub(crate) fn naming(&self, out: &mut Vec<Lint>) {
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
                    if self.statement_start(i) || matches!(self.prev(i), "export" | "global") =>
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
