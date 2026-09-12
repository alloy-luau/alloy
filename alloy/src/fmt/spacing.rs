//! Token-spacing and line-layout decisions: where a line breaks, how
//! many blank lines stay, which two tokens want a space between them,
//! and the final indent and join into text.

use alloy_syntax::lexer::TokKind;

use crate::config::{BlockGaps, FunctionNameSpace, IndentType};

use super::{Formatter, ItemKind, closes, continues, field_colon, is_closer, leaves_open, opens};

impl<'s> Formatter<'s> {
    // --- lines ----------------------------------------------------------------------------

    pub(crate) fn newline_before(&mut self, i: usize, extra: usize) {
        self.flush();
        let blank = self.items[i].newlines_before >= 2 && !self.lines.is_empty();

        if blank && self.blank_allowed(i) {
            self.lines.push(String::new());
        }

        let mut level = self.depths[i] + extra;

        if self.continues_at(i) {
            level += 1;
        }

        self.line_level = level;
        self.line = self.indent(level);
    }

    /// `block_newline_gaps = "never"` drops a blank line right after a
    /// block opener or right before its closer.
    fn blank_allowed(&self, i: usize) -> bool {
        if self.options.block_newline_gaps == BlockGaps::Preserve {
            return true;
        }

        let it = &self.items[i];

        if it.is("end") || it.is("until") || it.is("else") || it.is("elseif") || closes(&it.text) {
            return false;
        }

        self.prev_code(i).is_none_or(|p| {
            let t = self.items[p].text.as_str();

            !(matches!(t, "then" | "do" | "else" | "repeat" | "as" | "with")
                || opens(t)
                || (t == ")" && self.function_header_ends(p)))
        })
    }

    /// Whether the `)` at `p` closes a function's parameter list, with
    /// or without a return type after it.
    fn function_header_ends(&self, p: usize) -> bool {
        let mut j = p;
        let mut depth = 0;

        while j > 0 {
            j -= 1;
            let t = &self.items[j];

            if t.is(")") {
                depth += 1;
            } else if t.is("(") {
                if depth == 0 {
                    return (j.saturating_sub(4)..j).any(|k| self.items[k].is("function"));
                }

                depth -= 1;
            }
        }

        false
    }

    fn continues_at(&self, i: usize) -> bool {
        let it = &self.items[i];

        if it.is_comment() {
            return false;
        }

        if continues(&it.text) {
            return true;
        }

        self.prev_code(i)
            .is_some_and(|p| leaves_open(&self.items[p].text) && !is_closer(&it.text))
    }

    /// The canonical space before item `i` on the current line.
    pub(crate) fn space_before_item(&mut self, i: usize) {
        if self.line.trim().is_empty() {
            return;
        }

        let Some(p) = (0..i).next_back() else {
            return;
        };

        if self.wants_space(p, i) {
            self.line.push(' ');
        }
    }

    /// The spacing rule between two adjacent items on one line.
    fn wants_space(&self, ai: usize, bi: usize) -> bool {
        let a = &self.items[ai];
        let b = &self.items[bi];
        let at = a.text.as_str();
        let bt = b.text.as_str();

        if a.is_comment() || b.is_comment() {
            return true;
        }

        // Openers: nothing after them, unless the option pads the inside.
        if opens(at) {
            return self.inner_space(ai) && !closes(bt);
        }

        // Closers: nothing before them, unless the option pads the inside.
        if closes(bt) {
            return self.opener_of(bi).is_some_and(|o| self.inner_space(o));
        }

        // Separators and member access.
        if matches!(bt, "," | ";" | "." | "?." | "?:" | "?(" | "?[") {
            return false;
        }

        if matches!(at, "." | "?." | "?:" | "#" | "$" | "@") {
            return false;
        }

        // The postfix assert binds to what follows: `x!.y`, `f!(1)`.
        if bt == "!"
            || (at == "!" && matches!(bt, "." | "(" | "[" | ":" | "?." | "?:" | "?(" | "?["))
        {
            return false;
        }

        // A spread or a rest binding: `...rest`.
        if at == "..." && b.is_ident() {
            return false;
        }

        // Type arguments: `Result<number, string>`, `show_all<T>(items)`.
        if self.generic.get(ai) == Some(&true) {
            return !matches!(at, "<" | "<<")
                && !matches!(bt, "(" | "?" | "," | "." | "?." | ">" | ">>")
                && !closes(bt);
        }

        if self.generic.get(bi) == Some(&true) {
            return false;
        }

        // A method colon is tight; an annotation colon breathes after.
        // `a:b()` and `a: b` lex the same, so the source decides.
        if bt == ":" {
            return b.space_before;
        }

        if at == ":" {
            return b.space_before;
        }

        // The ternary `?` and the optional type `T?`: the source decides.
        if bt == "?" || at == "?" {
            return b.space_before;
        }

        // Chain arrows are tight; a type arrow breathes. The source decides.
        if matches!(bt, "->" | "=>") || matches!(at, "->" | "=>") {
            return b.space_before;
        }

        // A call or an index: `f(`, `t[`.
        if bt == "(" || bt == "[" {
            if a.is_ident() {
                if a.is_keyword_here()
                    && !matches!(at, "self" | "nil" | "true" | "false")
                    && !self.name_position(ai)
                {
                    return !(at == "function"
                        && bt == "("
                        && self.options.space_after_function_names != FunctionNameSpace::Always)
                        && !(at == "function" && bt == "(");
                }

                if bt == "(" && self.function_name_before(ai) {
                    return matches!(
                        self.options.space_after_function_names,
                        FunctionNameSpace::Always | FunctionNameSpace::Definitions
                    );
                }

                return false;
            }

            if at == ")" || at == "]" || at == "}" || a.is_string() {
                return false;
            }
        }

        if at == "function" && bt == "(" {
            return self.options.space_after_function_names == FunctionNameSpace::Always;
        }

        // Unary minus.
        if at == "-" && self.is_unary(ai) {
            return false;
        }

        // Interpolated strings hold their own spacing.
        if matches!(
            a.kind,
            ItemKind::Tok(TokKind::InterpHead | TokKind::InterpMid)
        ) || matches!(
            b.kind,
            ItemKind::Tok(TokKind::InterpMid | TokKind::InterpTail)
        ) {
            return false;
        }

        // Everything else: one space.
        true
    }

    /// A word after `function`, `.`, or `:` is a name, keyword or not.
    fn name_position(&self, i: usize) -> bool {
        self.prev_code(i).is_some_and(|p| {
            let t = &self.items[p];

            t.is("function")
                || t.is(".")
                || t.is(":")
                || t.is("?.")
                || t.is("?:")
                || t.is("local")
                || t.is("const")
        })
    }

    fn function_name_before(&self, ai: usize) -> bool {
        let mut j = ai;

        while let Some(p) = self.prev_code(j) {
            let t = &self.items[p];

            if t.is("function") {
                return true;
            }

            if !(t.is_ident() || t.is(".") || t.is(":")) {
                return false;
            }

            j = p;
        }

        false
    }

    pub(crate) fn opener_of(&self, close: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut j = close;

        while j > 0 {
            j -= 1;
            let t = &self.items[j];

            if t.is_comment() {
                continue;
            }

            if closes(&t.text) {
                depth += 1;
            } else if opens(&t.text) {
                if depth == 0 {
                    return Some(j);
                }

                depth -= 1;
            }
        }

        None
    }

    fn is_unary(&self, i: usize) -> bool {
        self.prev_code(i).is_none_or(|p| {
            let t = self.items[p].text.as_str();

            opens(t)
                || matches!(
                    t,
                    "," | "="
                        | "=="
                        | "~="
                        | "<"
                        | ">"
                        | "<="
                        | ">="
                        | "+"
                        | "-"
                        | "*"
                        | "/"
                        | "//"
                        | "%"
                        | "^"
                        | ".."
                        | "and"
                        | "or"
                        | "not"
                        | "return"
                        | "then"
                        | "else"
                        | "do"
                        | "in"
                        | "??"
                        | "?"
                        | ":"
                        | "if"
                        | "elseif"
                        | "while"
                        | "until"
                        | ";"
                        | "case"
                        | "default"
                )
        })
    }

    pub(crate) fn indent(&self, level: usize) -> String {
        match self.options.indent_type {
            IndentType::Tabs => "\t".repeat(level),
            IndentType::Spaces => " ".repeat(level * self.options.indent_width),
        }
    }

    pub(crate) fn flush(&mut self) {
        let line = std::mem::take(&mut self.line);
        let trimmed = line.trim_end();

        if trimmed.is_empty() {
            return;
        }

        self.lines.push(trimmed.to_string());
    }

    pub(crate) fn finish(mut self) -> String {
        if self.options.align_struct_fields {
            self.align_struct_fields();
        }

        while self.lines.last().is_some_and(|l| l.is_empty()) {
            self.lines.pop();
        }

        if self.lines.is_empty() {
            return String::new();
        }

        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }

    /// `align_struct_fields`: the `:` of the fields of one struct line up.
    fn align_struct_fields(&mut self) {
        let mut i = 0;

        while i < self.lines.len() {
            let head = self.lines[i].trim_start();

            if !(head.starts_with("struct ") || head.starts_with("export struct "))
                || head.ends_with(" end")
            {
                i += 1;

                continue;
            }

            let indent = self.lines[i].len() - head.len();
            let mut j = i + 1;
            let mut fields: Vec<usize> = Vec::new();

            while j < self.lines.len() {
                let l = &self.lines[j];
                let lead = l.len() - l.trim_start().len();

                if lead <= indent && !l.trim().is_empty() {
                    break;
                }

                if field_colon(l).is_some() {
                    fields.push(j);
                }

                j += 1;
            }

            let widest = fields
                .iter()
                .filter_map(|k| field_colon(&self.lines[*k]))
                .max()
                .unwrap_or(0);

            for k in fields {
                let l = self.lines[k].clone();

                if let Some(at) = field_colon(&l) {
                    let pad = " ".repeat(widest - at);
                    self.lines[k] = format!("{}{}{}", &l[..=at], pad, &l[at + 1..]);
                }
            }

            i = j.max(i + 1);
        }
    }
}
