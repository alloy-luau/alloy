//! Token-text rewrites and statement-level passes: the quotes of a
//! string, the leading zero of a number, the parentheses of a call,
//! the `as` of an `impl` or `trait` header, sorted imports, collapsed
//! simple statements, and broken call chains.

use alloy_syntax::lexer::TokKind;

use crate::config::{CallChainStyle, CallParentheses, Collapse, LeadingZero, RequireGrouping};

use super::{Formatter, Item, ItemKind, closes, expression_context, opens, requote, synthetic};

impl<'s> Formatter<'s> {
    // --- token rewrites -----------------------------------------------------

    /// The rewrites that change a token's text: quotes, leading zeros,
    /// and the parentheses of a call with one string or table argument.
    pub(crate) fn rewrite_tokens(&mut self) {
        let quote = self.options.quote_style;
        let zero = self.options.leading_zero;

        for it in &mut self.items {
            match it.kind {
                ItemKind::Tok(TokKind::Str { .. }) => it.text = requote(&it.text, quote),

                ItemKind::Tok(TokKind::Number) => {
                    let t = &it.text;
                    it.text = match zero {
                        LeadingZero::Add if t.starts_with('.') && t.len() > 1 => format!("0{t}"),
                        LeadingZero::Strip if t.starts_with("0.") && t.len() > 2 => {
                            t[1..].to_string()
                        }
                        _ => t.clone(),
                    };
                }

                _ => {}
            }
        }

        self.call_parentheses();
        self.header_as();
    }

    /// `impl T end` and `trait T end` write `impl T as end` and
    /// `trait T as end`. The header then closes the way a `struct`, an
    /// `enum`, and an `interface` header closes. The parser reads both,
    /// so a file written before `as` still builds.
    fn header_as(&mut self) {
        let mut i = 0;

        while i < self.items.len() {
            if !(self.items[i].is("impl") || self.items[i].is("trait")) || !self.starts_block(i) {
                i += 1;

                continue;
            }

            match self.header_end(i) {
                Some(at) if !self.items[at].is("as") => {
                    self.items.insert(
                        at,
                        Item {
                            text: "as".to_string(),
                            kind: ItemKind::Tok(TokKind::Ident),
                            newlines_before: 0,
                            space_before: true,
                            name_here: false,
                        },
                    );
                    i = at + 1;
                }

                _ => i += 1,
            }
        }
    }

    /// The item after an `impl` or `trait` header: the header holds
    /// names, `.`, `for`, and a `<...>` group, and nothing else.
    fn header_end(&self, open: usize) -> Option<usize> {
        let mut j = open + 1;
        let mut angle = 0usize;

        while j < self.items.len() {
            let it = &self.items[j];

            if it.is_comment() {
                break;
            }

            if angle > 0 {
                if it.is("<") {
                    angle += 1;
                } else if it.is(">") {
                    angle -= 1;
                }

                j += 1;

                continue;
            }

            if it.is("<") {
                angle += 1;
                j += 1;
            } else if it.is(".") || it.is("for") || (it.is_ident() && !it.is_keyword_here()) {
                j += 1;
            } else {
                break;
            }
        }

        (angle == 0 && j > open + 1 && j < self.items.len()).then_some(j)
    }

    /// Whether item `i` can be the callee of a call written without
    /// parentheses: a name, a closer, or a string, and not a keyword.
    fn callee_before(&self, i: usize) -> bool {
        let Some(p) = self.prev_code(i) else {
            return false;
        };
        let a = &self.items[p];

        if self.items[i].newlines_before > 0 {
            return false;
        }

        (a.is_ident() && !a.is_keyword_here()) || a.is(")") || a.is("]") || a.is_string()
    }

    /// `f "x"` and `f { }` take or lose their parentheses by the option.
    fn call_parentheses(&mut self) {
        let mode = self.options.call_parentheses;

        if mode == CallParentheses::Input {
            return;
        }

        let mut i = 0;

        while i < self.items.len() {
            if self.items[i].is_comment() || !self.callee_before(i) {
                i += 1;

                continue;
            }

            let cur = &self.items[i];
            let bare_string = cur.is_string();
            let bare_table = cur.is("{") && !self.is_type_or_struct_context(i);

            if (bare_string
                && matches!(
                    mode,
                    CallParentheses::Always | CallParentheses::NoSingleTable
                ))
                || (bare_table
                    && matches!(
                        mode,
                        CallParentheses::Always | CallParentheses::NoSingleString
                    ))
            {
                let end = if bare_string {
                    i + 1
                } else {
                    self.matching(i) + 1
                };
                self.items.insert(end, synthetic(")"));
                self.items.insert(i, synthetic("("));
                self.items[i + 1].space_before = false;
                i = end + 2;

                continue;
            }

            if cur.is("(") {
                let close = self.matching(i);
                let one_string = close == i + 2 && self.items[i + 1].is_string();
                let one_table = self.items[i + 1].is("{") && self.matching(i + 1) + 1 == close;

                if (one_string
                    && matches!(
                        mode,
                        CallParentheses::NoSingleString | CallParentheses::None
                    ))
                    || (one_table
                        && matches!(mode, CallParentheses::NoSingleTable | CallParentheses::None))
                {
                    self.items.remove(close);
                    self.items.remove(i);
                    self.items[i].space_before = true;
                    i += 1;

                    continue;
                }
            }

            i += 1;
        }
    }

    /// A `{` after a name that is a fields form, a type, or a cast is not
    /// a call: `new P { }`, `x: { a: number }`, `satisfies { }`.
    fn is_type_or_struct_context(&self, i: usize) -> bool {
        if self.line_has_before(i, "case") {
            return true;
        }

        let mut j = i;

        // `new Instance("Part") { }`: step over the argument list.
        if let Some(p) = self.prev_code(j)
            && self.items[p].is(")")
            && let Some(o) = self.opener_of(p)
        {
            j = o;
        }

        while let Some(p) = self.prev_code(j) {
            let t = &self.items[p];

            if t.is("new")
                || t.is(":")
                || t.is("::")
                || t.is("satisfies")
                || t.is("as")
                || t.is("extends")
            {
                return true;
            }

            if t.is("type")
                && self
                    .prev_code(p)
                    .is_some_and(|q| self.items[q].is("import") || self.items[q].is("export"))
            {
                return true;
            }

            if !(t.is_ident() && !t.is_keyword_here()) && !t.is(".") {
                return false;
            }

            j = p;
        }

        false
    }

    /// Whether the `{` at `i` opens the name list of an `import` or an
    /// `export`: `import {`, `import type {`, `import M, {`, `export {`,
    /// `export type {`.
    pub(crate) fn is_import_list(&self, i: usize) -> bool {
        if !self.items[i].is("{") {
            return false;
        }

        let mut j = i;

        while let Some(p) = self.prev_code(j) {
            let t = &self.items[p];

            if t.is("import") || t.is("export") {
                return true;
            }

            if !(t.is("type") || t.is(",") || (t.is_ident() && !t.is_keyword_here())) {
                return false;
            }

            j = p;
        }

        false
    }

    /// The index of the closer of the opener at `i`.
    fn matching(&self, i: usize) -> usize {
        let mut depth = 0i32;

        for (j, it) in self.items.iter().enumerate().skip(i) {
            if it.is_comment() {
                continue;
            }

            if opens(&it.text) {
                depth += 1;
            } else if closes(&it.text) {
                depth -= 1;

                if depth == 0 {
                    return j;
                }
            }
        }

        self.items.len() - 1
    }

    // --- statement-level passes ----------------------------------------------

    /// Sorts the run of `import` statements at the top of the file by
    /// path; by kind when asked: aliases, then absolute, then relative.
    pub(crate) fn sort_requires(&mut self) {
        if !self.options.sort_requires.enabled {
            return;
        }

        let mut stmts: Vec<(usize, usize, String)> = Vec::new();
        let mut i = 0;

        while i < self.items.len() {
            let it = &self.items[i];

            if it.is_comment() && stmts.is_empty() {
                i += 1;

                continue;
            }

            if !it.is("import") {
                break;
            }

            let start = i;
            let mut j = i + 1;

            while j < self.items.len() && self.items[j].newlines_before == 0 {
                j += 1;
            }

            let path = (start..j)
                .rev()
                .find(|k| self.items[*k].is_string())
                .map(|k| self.items[k].text.trim_matches(['"', '\'']).to_string())
                .unwrap_or_default();
            stmts.push((start, j, path));
            i = j;
        }

        if stmts.len() < 2 {
            return;
        }

        let kind = |p: &str| -> u8 {
            if p.starts_with('@') {
                0
            } else if p.starts_with('.') {
                2
            } else {
                1
            }
        };
        let mut order: Vec<usize> = (0..stmts.len()).collect();
        let grouping = self.options.sort_requires.grouping;
        order.sort_by(|a, b| {
            let (pa, pb) = (&stmts[*a].2, &stmts[*b].2);

            match grouping {
                RequireGrouping::ByKind => kind(pa).cmp(&kind(pb)).then_with(|| pa.cmp(pb)),
                RequireGrouping::Flat => pa.cmp(pb),
            }
        });

        if order.iter().enumerate().all(|(i, o)| i == *o) {
            return;
        }

        let first = stmts[0].0;
        let last = stmts[stmts.len() - 1].1;
        let head_newlines = self.items[first].newlines_before;
        let mut rebuilt: Vec<Item> = Vec::new();

        for (n, o) in order.iter().enumerate() {
            let (a, b, _) = stmts[*o];
            let mut chunk: Vec<Item> = self.items[a..b].to_vec();
            chunk[0].newlines_before = if n == 0 { head_newlines } else { 1 };
            rebuilt.extend(chunk);
        }

        self.items.splice(first..last, rebuilt);
    }

    /// `collapse_simple_statement`: `if c then\n    return x\nend` and a
    /// function with one statement join onto one line when they fit.
    pub(crate) fn collapse_simple_statements(&mut self) {
        let mode = self.options.collapse_simple_statement;

        if mode == Collapse::Never {
            return;
        }

        let n = self.items.len();
        let mut i = 0;

        while i < n {
            let it = &self.items[i];
            let conditional = it.is("if")
                && !self
                    .prev_code(i)
                    .is_some_and(|p| expression_context(&self.items[p].text));
            let function = it.is("function");
            let wanted = (conditional
                && matches!(mode, Collapse::ConditionalOnly | Collapse::Always))
                || (function && matches!(mode, Collapse::FunctionOnly | Collapse::Always));

            if !wanted {
                i += 1;

                continue;
            }

            // The header runs to `then` or to the `)` of the parameters.
            let header_end = (i..n).find(|k| {
                let t = &self.items[*k];
                (conditional && t.is("then"))
                    || (function
                        && t.is(")")
                        && self.items[*k + 1..]
                            .first()
                            .is_some_and(|nx| nx.newlines_before > 0))
            });
            let Some(h) = header_end else {
                i += 1;

                continue;
            };
            // Exactly one statement, on one line, then `end` on its own.
            let body_start = h + 1;
            let mut body_end = body_start;

            while body_end < n
                && (body_end == body_start || self.items[body_end].newlines_before == 0)
            {
                body_end += 1;
            }

            let is_end = body_end < n
                && self.items[body_end].is("end")
                && self.items[body_end].newlines_before > 0;
            let simple = is_end
                && body_start < body_end
                && (body_start..body_end)
                    .all(|k| !self.items[k].is_comment() && !self.items[k].opens_block_here())
                && self.items[body_start].newlines_before == 1
                && self.items[body_start].is("return")
                    | self.items[body_start].is("break")
                    | self.items[body_start].is("continue")
                    | (self.items[body_start].is_ident()
                        && !self.items[body_start].is_keyword_here());

            if simple {
                let width: usize = (i..=body_end).map(|k| self.items[k].width() + 1).sum();

                if width < self.options.column_width {
                    self.items[body_start].newlines_before = 0;
                    self.items[body_end].newlines_before = 0;
                }
            }

            i += 1;
        }
    }

    /// `call_chains`: a chain of method calls breaks before each call
    /// past the first, or before every call, once it holds `min_calls`.
    pub(crate) fn break_call_chains(&mut self) {
        let style = self.options.call_chains.style;

        if style == CallChainStyle::Preserve {
            return;
        }

        let min = self.options.call_chains.min_calls;
        let n = self.items.len();
        let mut i = 0;

        while i < n {
            // A chain: `:name(` links following one receiver on one line.
            if !self.items[i].is(":") || self.items[i].newlines_before > 0 {
                i += 1;

                continue;
            }

            let mut links = vec![i];
            let mut j = i;

            while let Some(name) = self.next_code(j)
                && let Some(nx) = self.next_code(name)
            {
                let t = &self.items[nx];

                if t.is("(") || t.is("?(") {
                    let close = self.matching(nx);

                    if let Some(after) = self.next_code(close)
                        && (self.items[after].is(":") || self.items[after].is("?:"))
                        && self.items[after].newlines_before == 0
                    {
                        links.push(after);
                        j = after;

                        continue;
                    }

                    j = close;
                }

                break;
            }

            if links.len() >= min.max(1) && min > 0 {
                let from = if style == CallChainStyle::Method {
                    1
                } else {
                    0
                };

                for l in links.iter().skip(from) {
                    self.items[*l].newlines_before = 1;
                }
            }

            i = j.max(i) + 1;
        }
    }
}
