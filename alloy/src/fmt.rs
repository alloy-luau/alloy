//! `alloy fmt`, Anneal: an objective formatter.
//!
//! The output depends on the tokens and the options, not on how the
//! author laid the code out, with the exceptions the options name: a
//! magic trailing comma keeps a bracket group expanded, and
//! `block_newline_gaps = "preserve"` keeps a blank line at the edge of a
//! block. Everything else is decided here: the spacing between tokens,
//! which bracket groups break and how, the quotes of a string, the
//! parentheses of a call, the leading zero of a number.
//!
//! The token stream of the output is the token stream of the input, save
//! for the quote and parenthesis rewrites the options ask for, so the
//! program is the same and a second run changes nothing.
//!
//! Statements keep their lines: a newline between two statements in the
//! source is a newline in the output, and at most one blank line stays
//! between them. Inside a bracket group the source's newlines mean
//! nothing; the group lays itself out from the width.

use alloy_syntax::lexer::{Lexed, Tok, TokKind, lex};

use crate::config::{
    BlockGaps, CallChainStyle, CallParentheses, Collapse, FmtConfig, FunctionNameSpace, IndentType,
    LeadingZero, LineEndings, QuoteStyle, RequireGrouping,
};

pub use crate::fmt_structure::{Step, Structure, structure};

/// Spaces per indentation level, for callers that only reindent.
pub const INDENT: usize = 4;

/// One lexical item: a token or a comment, in source order.
#[derive(Debug, Clone)]
struct Item {
    text: String,
    kind: ItemKind,
    /// Newlines in the source between the item before and this one.
    newlines_before: usize,
    /// Whether the source had whitespace right before this item.
    space_before: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Tok(TokKind),
    LineComment,
    LongComment,
}

impl Item {
    fn is_comment(&self) -> bool {
        matches!(self.kind, ItemKind::LineComment | ItemKind::LongComment)
    }

    fn is(&self, text: &str) -> bool {
        !self.is_comment() && self.text == text
    }

    fn is_ident(&self) -> bool {
        matches!(self.kind, ItemKind::Tok(TokKind::Ident))
    }

    fn is_string(&self) -> bool {
        matches!(
            self.kind,
            ItemKind::Tok(TokKind::Str { .. } | TokKind::InterpStr)
        )
    }

    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// A bracket group or one item.
#[derive(Debug)]
enum Node {
    Item(usize),
    Group {
        open: usize,
        close: usize,
        /// The elements between the separators, each with the separator
        /// that followed it, when one did.
        elements: Vec<(Vec<Node>, Option<usize>)>,
        /// A separator before the closer, with the closer on its own line.
        magic_comma: bool,
    },
}

/// Formats Alloy source with the default options.
pub fn format(src: &str) -> Result<String, String> {
    format_with(src, &FmtConfig::default())
}

/// Formats Alloy source. `Err` carries the lexer's message: a file that
/// does not lex stays as it is.
pub fn format_with(src: &str, options: &FmtConfig) -> Result<String, String> {
    let Lexed { toks, comments } = lex(src).map_err(|e| e.message)?;
    let items = items_of(src, &toks, &comments);

    if items.is_empty() {
        return Ok(String::new());
    }

    let mut f = Formatter {
        items,
        options,
        lines: Vec::new(),
        line: String::new(),
        line_level: 0,
        depths: Vec::new(),
        generic: Vec::new(),
        signature: Vec::new(),
    };
    f.rewrite_tokens();
    f.sort_requires();
    f.collapse_simple_statements();
    f.break_call_chains();
    f.depths = f.block_depths();
    f.generic = f.generic_brackets();
    let tree = f.tree();
    let hard = f.hard_breaks(&tree);
    f.render_nodes(&tree, &hard, 0);
    f.flush();
    let mut text = f.finish();

    if options.line_endings == LineEndings::Windows {
        text = text.replace('\n', "\r\n");
    }

    Ok(text)
}

/// Tokens and comments as one ordered list, with the whitespace facts
/// the layout needs.
fn items_of(src: &str, toks: &[Tok], comments: &[(u32, u32)]) -> Vec<Item> {
    let mut all: Vec<(usize, usize, ItemKind)> = toks
        .iter()
        .map(|t| (t.start as usize, t.end as usize, ItemKind::Tok(t.kind)))
        .collect();

    for (a, b) in comments {
        let text = &src[*a as usize..*b as usize];
        let long = text.len() > 3 && text.starts_with("--[") && text[3..].starts_with(['[', '=']);
        all.push((
            *a as usize,
            *b as usize,
            if long {
                ItemKind::LongComment
            } else {
                ItemKind::LineComment
            },
        ));
    }

    all.sort_by_key(|(a, _, _)| *a);
    let mut out = Vec::with_capacity(all.len());
    let mut prev_end = 0;

    for (a, b, kind) in all {
        let between = &src[prev_end..a];
        out.push(Item {
            text: src[a..b].to_string(),
            kind,
            newlines_before: between.matches('\n').count(),
            space_before: !between.is_empty(),
        });
        prev_end = b;
    }

    merge_operators(out)
}

/// The lexer emits `?`, `<`, and `>` one character at a time. The
/// operators built from them are one item here: `?.`, `?:`, `?[`, `?(`,
/// `??`, `??=`, and the `<<` `>>` of explicit type arguments.
fn merge_operators(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    let mut open_shl = 0usize;

    for it in items {
        let joined = match out.last() {
            Some(last) if !it.space_before && !it.is_comment() && !last.is_comment() => {
                let (a, b) = (last.text.as_str(), it.text.as_str());

                (a == "?" && matches!(b, "." | ":" | "[" | "(" | "?"))
                    || (a == "??" && b == "=")
                    || (a == "<"
                        && b == "<"
                        && out.len() >= 2
                        && out[out.len() - 2].is_ident()
                        && !last.space_before)
                    || (a == ">" && b == ">" && open_shl > 0)
            }

            _ => false,
        };

        if joined {
            let last = out.last_mut().unwrap();
            last.text.push_str(&it.text);
            last.kind = ItemKind::Tok(TokKind::Symbol);

            if last.text == "<<" {
                open_shl += 1;
            } else if last.text == ">>" {
                open_shl -= 1;
            }

            continue;
        }

        out.push(it);
    }

    out
}

struct Formatter<'s> {
    items: Vec<Item>,
    options: &'s FmtConfig,
    lines: Vec<String>,
    line: String,
    /// The indentation level of the line under construction.
    line_level: usize,
    /// The block depth of each item, brackets not counted.
    depths: Vec<usize>,
    /// The `<` and `>` of type parameters and arguments, which stay tight.
    generic: Vec<bool>,
    /// The `function` items inside a trait that have no body.
    signature: Vec<bool>,
}

/// Openers of bracket groups, as token text.
fn opens(text: &str) -> bool {
    matches!(text, "(" | "{" | "[" | "?(" | "?[" | "<<")
}

fn closes(text: &str) -> bool {
    matches!(text, ")" | "}" | "]" | ">>")
}

fn closer_of(open: &str) -> &'static str {
    match open {
        "(" | "?(" => ")",
        "[" | "?[" => "]",
        "<<" => ">>",
        _ => "}",
    }
}

impl<'s> Formatter<'s> {
    fn prev_code(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|j| !self.items[*j].is_comment())
    }

    fn next_code(&self, i: usize) -> Option<usize> {
        (i + 1..self.items.len()).find(|j| !self.items[*j].is_comment())
    }

    // --- token rewrites -----------------------------------------------------

    /// The rewrites that change a token's text: quotes, leading zeros,
    /// and the parentheses of a call with one string or table argument.
    fn rewrite_tokens(&mut self) {
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

        (a.is_ident() && !is_keyword(&a.text)) || a.is(")") || a.is("]") || a.is_string()
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

            if !(t.is_ident() && !is_keyword(&t.text)) && !t.is(".") {
                return false;
            }

            j = p;
        }

        false
    }

    /// Whether the `{` at `i` opens the name list of an `import` or an
    /// `export`: `import {`, `import type {`, `import M, {`, `export {`,
    /// `export type {`.
    fn is_import_list(&self, i: usize) -> bool {
        if !self.items[i].is("{") {
            return false;
        }

        let mut j = i;

        while let Some(p) = self.prev_code(j) {
            let t = &self.items[p];

            if t.is("import") || t.is("export") {
                return true;
            }

            if !(t.is("type") || t.is(",") || (t.is_ident() && !is_keyword(&t.text))) {
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
    fn sort_requires(&mut self) {
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
    fn collapse_simple_statements(&mut self) {
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
                    .all(|k| !self.items[k].is_comment() && !block_opener(&self.items[k].text))
                && self.items[body_start].newlines_before == 1
                && self.items[body_start].is("return")
                    | self.items[body_start].is("break")
                    | self.items[body_start].is("continue")
                    | (self.items[body_start].is_ident()
                        && !is_keyword(&self.items[body_start].text));

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
    fn break_call_chains(&mut self) {
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

    // --- block depth -------------------------------------------------------------

    /// +1 for an item that opens a block, -1 for one that closes it.
    fn block_delta(&self, i: usize) -> i32 {
        let it = &self.items[i];

        if it.is_comment() {
            return 0;
        }

        if (block_opener(&it.text) && self.starts_block(i)) || self.is_loop_head(i) {
            1
        } else if it.is("end") || it.is("until") {
            -1
        } else {
            0
        }
    }

    /// Whether the keyword at `i` opens a block: `function` always but
    /// for a signature; `if`, `do`, `match` when they start a statement.
    fn starts_block(&self, i: usize) -> bool {
        let text = self.items[i].text.as_str();
        let prev = self.prev_code(i).map(|p| self.items[p].text.as_str());

        match text {
            "function" => {
                !self.line_has_before(i, "declare")
                    && !self.line_has_before(i, "attribute")
                    && prev != Some("remote")
                    && self.signature.get(i) != Some(&true)
            }

            "if" => !prev.is_some_and(expression_context),

            "do" => !self.for_header_before(i),

            "match" => !matches!(prev, Some(".") | Some(":")),

            "struct" | "enum" | "trait" | "impl" | "interface" | "macro" => {
                self.items[i].newlines_before > 0 || i == 0 || prev == Some("export")
            }

            "class" => prev == Some("declare"),

            "with" => self.line_has_before(i, "declare"),

            _ => true,
        }
    }

    fn for_header_before(&self, i: usize) -> bool {
        let mut j = i;

        while j > 0 {
            j -= 1;
            let t = &self.items[j];

            if self.is_loop_head(j) {
                return true;
            }

            if t.newlines_before > 0 || t.is("do") || t.is("then") || t.is("end") {
                return false;
            }
        }

        false
    }

    /// A `for` or `while` that opens a loop; `impl X for Y` has none.
    fn is_loop_head(&self, i: usize) -> bool {
        let t = &self.items[i];

        t.is("while") || (t.is("for") && !self.line_has_before(i, "impl"))
    }

    fn line_has_before(&self, i: usize, word: &str) -> bool {
        let mut j = i;

        while j > 0 {
            j -= 1;

            if self.items[j].is(word) {
                return true;
            }

            if self.items[j].newlines_before > 0 {
                return false;
            }
        }

        false
    }

    /// Inside a trait, a `function` with no body: the next line of code
    /// starts another signature, an attribute, or the trait's `end`. A
    /// default with a body that starts with a comment reads as a
    /// signature; write the comment above the function instead.
    fn next_line_starts_signature(&self, i: usize) -> bool {
        let mut k = i + 1;

        while k < self.items.len()
            && (self.items[k].newlines_before == 0 || self.items[k].is_comment())
        {
            k += 1;
        }

        k >= self.items.len() || {
            let t = &self.items[k];
            t.is("function") || t.is("end") || t.is("@")
        }
    }

    /// The indentation level of each item from the blocks alone: the
    /// depth before the item, less what the item closes. Brackets do not
    /// count; the layout of a group is the renderer's. Fills `signature`
    /// on the way.
    fn block_depths(&mut self) -> Vec<usize> {
        #[derive(PartialEq, Clone, Copy)]
        enum Frame {
            Block,
            Trait,
            Match,
            Arm,
            ExprIf,
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut depths = vec![0usize; self.items.len()];
        let mut signature = vec![false; self.items.len()];
        let level = |stack: &Vec<Frame>| stack.iter().filter(|f| **f != Frame::ExprIf).count();

        for i in 0..self.items.len() {
            let it = &self.items[i];

            if it.is_comment() {
                depths[i] = level(&stack);

                continue;
            }

            let text = it.text.as_str();
            let prev = self.prev_code(i).map(|p| self.items[p].text.as_str());

            if it.newlines_before > 0 && !matches!(text, "else" | "elseif") {
                while stack.last() == Some(&Frame::ExprIf) {
                    stack.pop();
                }
            }

            match text {
                "end" => {
                    if stack.last() == Some(&Frame::Arm) {
                        stack.pop();
                    }

                    if matches!(
                        stack.last(),
                        Some(Frame::Block | Frame::Match | Frame::Trait)
                    ) {
                        stack.pop();
                    }

                    depths[i] = level(&stack);
                }

                "until" => {
                    if stack.last() == Some(&Frame::Block) {
                        stack.pop();
                    }

                    depths[i] = level(&stack);
                }

                "else" | "elseif" => {
                    let mid_line = it.newlines_before == 0;
                    let let_else = text == "else"
                        && mid_line
                        && !self.line_has_before(i, "if")
                        && (self.line_has_before(i, "local") || self.line_has_before(i, "const"));

                    if let_else {
                        depths[i] = level(&stack);
                        stack.push(Frame::Block);
                    } else if stack.last() == Some(&Frame::ExprIf)
                        || (mid_line && self.line_has_before(i, "if"))
                    {
                        depths[i] = level(&stack);
                    } else if stack.last() == Some(&Frame::Block) {
                        depths[i] = level(&stack).saturating_sub(1);
                    } else {
                        depths[i] = level(&stack);
                    }
                }

                "case" | "default" => {
                    if stack.last() == Some(&Frame::Arm) {
                        stack.pop();
                    }

                    depths[i] = level(&stack);

                    if stack.last() == Some(&Frame::Match) {
                        stack.push(Frame::Arm);
                    }
                }

                ")" | "]" | "}" | ">>" => {
                    depths[i] = level(&stack);

                    // An expression `if` ends at a closer.
                    while stack.last() == Some(&Frame::ExprIf) {
                        stack.pop();
                    }
                }

                _ => {
                    depths[i] = level(&stack);

                    if text == "if"
                        && (prev.is_some_and(expression_context)
                            || (matches!(prev, Some("then") | Some("else"))
                                && stack.last() == Some(&Frame::ExprIf)))
                    {
                        stack.push(Frame::ExprIf);
                    } else if text == "match" && self.starts_block(i) {
                        stack.push(Frame::Match);
                    } else if text == "trait" && self.starts_block(i) {
                        stack.push(Frame::Trait);
                    } else if text == "function"
                        && stack.last() == Some(&Frame::Trait)
                        && it.newlines_before > 0
                    {
                        if self.next_line_starts_signature(i) {
                            signature[i] = true;
                        } else {
                            stack.push(Frame::Block);
                        }
                    } else {
                        let opens_block = self.is_loop_head(i)
                            || ((block_opener(text) && text != "with" || text == "class")
                                && self.starts_block(i))
                            || (text == "with"
                                && self.starts_block(i)
                                && stack.last() != Some(&Frame::Match));

                        if opens_block {
                            stack.push(Frame::Block);
                        }
                    }
                }
            }
        }

        for i in 0..self.items.len() {
            if self.items[i].is_comment()
                && let Some(n) = self.next_code(i)
            {
                depths[i] = depths[n];
            }
        }

        self.signature = signature;
        depths
    }

    /// `Result<number, string>` and `<T: Display>`: a `<` right after a
    /// name, with no space in the source, opens type arguments; `a < b`
    /// compares. The matching `>` or `>>` closes it.
    fn generic_brackets(&self) -> Vec<bool> {
        let mut generic = vec![false; self.items.len()];

        for i in 0..self.items.len() {
            let it = &self.items[i];

            if !(it.is("<") || it.is("<<")) || it.space_before {
                continue;
            }

            let opens_generic = self.prev_code(i).is_some_and(|p| {
                let t = &self.items[p];

                (t.is_ident() && !is_keyword(&t.text)) || t.is(">")
            });

            if !opens_generic {
                continue;
            }

            let mut depth = 0i32;
            let mut marks = Vec::new();

            for j in i..self.items.len() {
                let t = &self.items[j];

                if (t.newlines_before > 0 && j != i)
                    || t.is("then")
                    || t.is("do")
                    || (t.is("=") && depth == 0)
                {
                    break;
                }

                if t.is("<") || t.is("<<") {
                    depth += if t.is("<<") { 2 } else { 1 };
                    marks.push(j);
                } else if t.is(">") || t.is(">>") {
                    depth -= if t.is(">>") { 2 } else { 1 };
                    marks.push(j);

                    if depth <= 0 {
                        for m in marks {
                            generic[m] = true;
                        }

                        break;
                    }
                }
            }
        }

        generic
    }

    // --- the tree ------------------------------------------------------------------

    fn tree(&self) -> Vec<Node> {
        let mut pos = 0;

        self.nodes(&mut pos, None)
    }

    /// Parses items into nodes until `until` closes them, a separator at
    /// this level, or the end. Neither the closer nor the separator is
    /// consumed.
    fn nodes(&self, pos: &mut usize, until: Option<&str>) -> Vec<Node> {
        let mut out = Vec::new();
        let mut block_depth = 0i32;

        while *pos < self.items.len() {
            let it = &self.items[*pos];

            if until.is_some() && !it.is_comment() {
                let separator = (it.is(",") || it.is(";")) && block_depth <= 0;

                if separator || Some(it.text.as_str()) == until {
                    return out;
                }
            }

            if !it.is_comment() && opens(&it.text) {
                out.push(self.group(pos));

                continue;
            }

            block_depth += self.block_delta(*pos);
            out.push(Node::Item(*pos));
            *pos += 1;
        }

        out
    }

    fn group(&self, pos: &mut usize) -> Node {
        let open = *pos;
        let closer = closer_of(&self.items[open].text);
        *pos += 1;
        let mut elements: Vec<(Vec<Node>, Option<usize>)> = Vec::new();
        let mut magic = false;

        loop {
            let element = self.nodes(pos, Some(closer));

            if *pos >= self.items.len() {
                if !element.is_empty() {
                    elements.push((element, None));
                }

                break;
            }

            let t = &self.items[*pos];

            if t.is(",") || t.is(";") {
                let sep = *pos;
                *pos += 1;
                elements.push((element, Some(sep)));

                // A trailing comma before the closer keeps the group
                // expanded when the closer already sat on its own line;
                // an import list keeps it either way, so `{ a, b, }`
                // is how a file asks for one name per line.
                if self.items.get(*pos).is_some_and(|nx| nx.is(closer)) {
                    magic = self.items[*pos].newlines_before > 0 || self.is_import_list(open);
                }

                continue;
            }

            // The closer.
            if !element.is_empty() {
                elements.push((element, None));
            }

            break;
        }

        let close = (*pos).min(self.items.len() - 1);
        *pos += 1;

        Node::Group {
            open,
            close,
            elements,
            magic_comma: magic,
        }
    }

    // --- hard breaks ------------------------------------------------------------------

    /// Which items start a new line no matter what: a newline in the
    /// source outside any bracket group, a comment on its own line, or a
    /// newline inside a block that opened inside the group, which is a
    /// callback's body.
    fn hard_breaks(&self, tree: &[Node]) -> Vec<bool> {
        let mut hard = vec![false; self.items.len()];
        self.mark_hard(tree, &mut hard, false, 0);
        hard
    }

    fn mark_hard(&self, nodes: &[Node], hard: &mut [bool], in_group: bool, mut block_depth: i32) {
        for n in nodes {
            match n {
                Node::Item(i) => {
                    let it = &self.items[*i];

                    if it.newlines_before > 0 && (!in_group || block_depth > 0 || it.is_comment()) {
                        hard[*i] = true;
                    }

                    block_depth += self.block_delta(*i);
                }

                Node::Group {
                    open,
                    close,
                    elements,
                    ..
                } => {
                    let it = &self.items[*open];

                    if it.newlines_before > 0 && (!in_group || block_depth > 0) {
                        hard[*open] = true;
                    }

                    for (el, sep) in elements {
                        self.mark_hard(el, hard, true, 0);

                        if let Some(s) = sep
                            && self.items[*s].newlines_before > 0
                            && block_depth > 0
                            && in_group
                        {
                            hard[*s] = true;
                        }
                    }

                    let c = &self.items[*close];

                    if c.newlines_before > 0 && in_group && block_depth > 0 {
                        hard[*close] = true;
                    }
                }
            }
        }
    }

    // --- rendering ---------------------------------------------------------------------

    fn render_nodes(&mut self, nodes: &[Node], hard: &[bool], extra: usize) {
        for node in nodes {
            match node {
                Node::Item(i) => self.render_item(*i, hard, extra),

                Node::Group { .. } => self.render_group(node, hard, extra),
            }
        }
    }

    fn render_item(&mut self, i: usize, hard: &[bool], extra: usize) {
        if hard[i] {
            self.newline_before(i, extra);
        } else {
            self.space_before_item(i);
        }

        let text = self.items[i].text.clone();
        self.line.push_str(&text);
    }

    fn render_group(&mut self, node: &Node, hard: &[bool], extra: usize) {
        let Node::Group {
            open,
            close,
            elements,
            magic_comma,
        } = node
        else {
            return;
        };
        let (open, close) = (*open, *close);

        if hard[open] {
            self.newline_before(open, extra);
        } else {
            self.space_before_item(open);
        }

        let opener = self.items[open].text.clone();
        let closer = self.items[close].text.clone();
        let expand = !elements.is_empty() && self.should_expand(elements, *magic_comma, hard, open);
        self.line.push_str(&opener);

        if !expand {
            for (el, sep) in elements {
                self.render_nodes(el, hard, extra);

                if let Some(s) = sep {
                    self.render_item(*s, hard, extra);
                }
            }

            self.render_item(close, hard, extra);
        } else {
            let base = self.line_level;
            let trailing =
                self.options.trailing_comma && !matches!(opener.as_str(), "(" | "?(" | "<<");

            for (k, (el, sep)) in elements.iter().enumerate() {
                self.flush();
                self.line_level = base + 1;
                self.line = self.indent(base + 1);
                self.render_nodes(el, hard, extra + 1);
                let last = k + 1 == elements.len();

                if !last || trailing {
                    let t = sep
                        .map(|s| self.items[s].text.clone())
                        .unwrap_or_else(|| ",".to_string());
                    self.line.push_str(&t);
                }
            }

            self.flush();
            self.line_level = base;
            self.line = self.indent(base);
            self.line.push_str(&closer);
        }
    }

    /// Whether a group breaks: a magic trailing comma, a comment among
    /// its elements, or a flat rendering that runs past the width.
    fn should_expand(
        &self,
        elements: &[(Vec<Node>, Option<usize>)],
        magic: bool,
        hard: &[bool],
        open: usize,
    ) -> bool {
        if magic && self.options.magic_trailing_comma {
            return true;
        }

        if self.options.expand_imports && elements.len() > 1 && self.is_import_list(open) {
            return true;
        }

        let has_comment = elements.iter().any(|(el, _)| {
            let mut block_depth = 0i32;

            el.iter().any(|n| match n {
                Node::Item(i) => {
                    block_depth += self.block_delta(*i);

                    self.items[*i].is_comment() && block_depth <= 0
                }

                Node::Group { .. } => false,
            })
        });

        if has_comment {
            return true;
        }

        let mut width = self.items[open].width();
        let mut stopped = false;

        if self.inner_space(open) {
            width += 2;
        }

        for (k, (el, sep)) in elements.iter().enumerate() {
            if k > 0 {
                width += 1;
            }

            for n in el {
                self.measure(n, hard, &mut width, &mut stopped);

                if stopped {
                    break;
                }
            }

            if stopped {
                break;
            }

            if sep.is_some() {
                width += 1;
            }
        }

        if !stopped {
            width += 1;
        }

        self.line.chars().count() + width > self.options.column_width
    }

    /// The flat width of a node, up to the first hard break inside.
    fn measure(&self, node: &Node, hard: &[bool], w: &mut usize, stopped: &mut bool) {
        match node {
            Node::Item(i) => {
                if hard[*i] {
                    *stopped = true;

                    return;
                }

                *w += self.items[*i].width() + usize::from(self.items[*i].space_before);
            }

            Node::Group {
                open,
                close,
                elements,
                ..
            } => {
                if hard[*open] {
                    *stopped = true;

                    return;
                }

                *w += self.items[*open].width();

                for (k, (el, sep)) in elements.iter().enumerate() {
                    if k > 0 {
                        *w += 1;
                    }

                    for n in el {
                        self.measure(n, hard, w, stopped);

                        if *stopped {
                            return;
                        }
                    }

                    if sep.is_some() {
                        *w += 1;
                    }
                }

                *w += self.items[*close].width();
            }
        }
    }

    /// A space inside the brackets of the group at `open`, by the options.
    fn inner_space(&self, open: usize) -> bool {
        match self.items[open].text.as_str() {
            "{" => self.options.space_inside_braces,
            "(" | "?(" => self.options.space_inside_parens,
            "[" | "?[" => {
                if self.is_index(open) {
                    self.options.space_inside_brackets
                } else {
                    self.options.space_inside_array
                }
            }
            _ => false,
        }
    }

    /// `[` that indexes, as opposed to an array literal or a type's
    /// `{ [k]: v }`.
    fn is_index(&self, i: usize) -> bool {
        if self.items[i].is("?[") {
            return true;
        }

        self.prev_code(i).is_some_and(|p| {
            let t = &self.items[p];

            (t.is_ident() && !is_keyword(&t.text))
                || t.is(")")
                || t.is("]")
                || t.is("}")
                || t.is_string()
                || t.is("{")
                || ((t.is(",") || t.is(";"))
                    && self
                        .enclosing_open(p)
                        .is_some_and(|o| self.items[o].is("{")))
        })
    }

    /// The opener of the group that holds item `i`, or none at the top.
    fn enclosing_open(&self, i: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut j = i;

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

    // --- lines ----------------------------------------------------------------------------

    fn newline_before(&mut self, i: usize, extra: usize) {
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
    fn space_before_item(&mut self, i: usize) {
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
                if is_keyword(at)
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

    fn opener_of(&self, close: usize) -> Option<usize> {
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
                )
        })
    }

    fn indent(&self, level: usize) -> String {
        match self.options.indent_type {
            IndentType::Tabs => "\t".repeat(level),
            IndentType::Spaces => " ".repeat(level * self.options.indent_width),
        }
    }

    fn flush(&mut self) {
        let line = std::mem::take(&mut self.line);
        let trimmed = line.trim_end();

        if trimmed.is_empty() {
            return;
        }

        self.lines.push(trimmed.to_string());
    }

    fn finish(mut self) -> String {
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

/// The byte offset of the `:` of a struct field line, or none.
fn field_colon(line: &str) -> Option<usize> {
    let t = line.trim_start();

    if t.starts_with("--") || t.starts_with('@') {
        return None;
    }

    let at = t.find(':')?;
    let name = t[..at].trim();
    let name = name
        .strip_prefix("read ")
        .or_else(|| name.strip_prefix("write "))
        .unwrap_or(name);

    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    Some(line.len() - t.len() + at)
}

fn synthetic(text: &str) -> Item {
    Item {
        text: text.to_string(),
        kind: ItemKind::Tok(if text == "(" {
            TokKind::LParen
        } else {
            TokKind::RParen
        }),
        newlines_before: 0,
        space_before: false,
    }
}

/// A string literal with the quotes the option asks for. The content
/// keeps its characters; under an `auto` style a string that holds the
/// other quote keeps the quotes it has.
pub(crate) fn requote(text: &str, style: QuoteStyle) -> String {
    let Some(first) = text.chars().next() else {
        return text.to_string();
    };

    if first != '"' && first != '\'' {
        return text.to_string();
    }

    let body = &text[1..text.len() - 1];
    let has_double = body.contains('"');
    let has_single = body.contains('\'');
    let want = match style {
        QuoteStyle::Preserve => return text.to_string(),
        QuoteStyle::AutoPreferDouble => {
            if has_double && !has_single {
                '\''
            } else {
                '"'
            }
        }
        QuoteStyle::AutoPreferSingle => {
            if has_single && !has_double {
                '"'
            } else {
                '\''
            }
        }
        QuoteStyle::ForceDouble => '"',
        QuoteStyle::ForceSingle => '\'',
    };

    if want == first {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 2);
    out.push(want);
    let mut chars = body.chars();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(q) if q == first => out.push(q),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else if c == want {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }

    out.push(want);
    out
}

/// Openers of a block that `end` closes, when they start a statement.
fn block_opener(text: &str) -> bool {
    matches!(
        text,
        "function"
            | "if"
            | "do"
            | "repeat"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "interface"
            | "macro"
            | "match"
            | "class"
            | "with"
    )
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "and"
            | "or"
            | "not"
            | "if"
            | "then"
            | "else"
            | "elseif"
            | "end"
            | "for"
            | "in"
            | "while"
            | "do"
            | "repeat"
            | "until"
            | "return"
            | "break"
            | "continue"
            | "local"
            | "function"
            | "nil"
            | "true"
            | "false"
            | "const"
            | "export"
            | "import"
            | "from"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "interface"
            | "match"
            | "case"
            | "default"
            | "with"
            | "new"
            | "delete"
            | "async"
            | "await"
            | "try"
            | "macro"
            | "attribute"
            | "remote"
            | "where"
            | "is"
            | "as"
            | "satisfies"
            | "declare"
            | "read"
            | "write"
            | "extends"
            | "on"
    )
}

/// Tokens after which an `if` or a `function` is an expression.
fn expression_context(prev: &str) -> bool {
    matches!(
        prev,
        "=" | "("
            | ","
            | "["
            | "{"
            | "return"
            | "and"
            | "or"
            | "not"
            | "+"
            | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "=="
            | "~="
            | "<"
            | ">"
            | "<="
            | ">="
            | "??"
            | "?"
            | ":"
            | "in"
            | "?("
            | "?["
    )
}

/// Tokens that continue the expression of the line before, when they
/// start a line.
fn continues(text: &str) -> bool {
    matches!(
        text,
        "+" | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "and"
            | "or"
            | "=="
            | "~="
            | "<"
            | ">"
            | "<="
            | ">="
            | "??"
            | "?"
            | ":"
            | "."
            | "->"
            | "=>"
            | "?."
            | "?:"
            | "?["
            | "?("
    )
}

/// Tokens after which the next line continues the expression.
fn leaves_open(text: &str) -> bool {
    matches!(
        text,
        "=" | "+"
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
            | "=="
            | "~="
            | "<="
            | ">="
            | "??"
            | "->"
            | "=>"
    )
}

fn is_closer(text: &str) -> bool {
    matches!(text, "end" | "until" | ")" | "]" | "}" | "else" | "elseif")
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format(s).unwrap()
    }

    #[test]
    fn reindents_blocks() {
        let src = "local function f(x)\nif x then\nreturn 1\nelseif x == 2 then\nreturn 2\nelse\nreturn 3\nend\nend\n";
        let want = "local function f(x)\n    if x then\n        return 1\n    elseif x == 2 then\n        return 2\n    else\n        return 3\n    end\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn spacing_is_canonical() {
        assert_eq!(fmt("local x=1+2*3\n"), "local x = 1 + 2 * 3\n");
        assert_eq!(fmt("f(a,b , c)\n"), "f(a, b, c)\n");
        assert_eq!(fmt("local t={a=1,b=2}\n"), "local t = { a = 1, b = 2 }\n");
        assert_eq!(fmt("local v = -x\n"), "local v = -x\n");
        assert_eq!(fmt("obj:method(1):other()\n"), "obj:method(1):other()\n");
        assert_eq!(
            fmt("local p = workspace->Map?.Part\n"),
            "local p = workspace->Map?.Part\n"
        );
        assert_eq!(fmt("local n: number? = nil\n"), "local n: number? = nil\n");
        assert_eq!(fmt("local s = c ? a : b\n"), "local s = c ? a : b\n");
        assert_eq!(fmt("local xs = [1,2]\n"), "local xs = [ 1, 2 ]\n");
        assert_eq!(fmt("print(t[1], #t)\n"), "print(t[1], #t)\n");
    }

    #[test]
    fn a_table_that_fits_stays_on_one_line_and_one_that_does_not_breaks() {
        assert_eq!(
            fmt("local t = {\n    a = 1,\n    b = 2 }\n"),
            "local t = { a = 1, b = 2 }\n"
        );
        let long = "local t = { alpha = 111111111111, beta = 222222222222, gamma = 333333333333, delta = 444444444444, epsilon = 5555 }\n";
        let want = "local t = {\n    alpha = 111111111111,\n    beta = 222222222222,\n    gamma = 333333333333,\n    delta = 444444444444,\n    epsilon = 5555,\n}\n";
        assert_eq!(fmt(long), want);
    }

    #[test]
    fn a_magic_trailing_comma_keeps_a_group_expanded() {
        let src = "local t = {\n    a = 1,\n    b = 2,\n}\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn a_callback_argument_indents_once() {
        assert_eq!(
            fmt("foo(function()\nbar()\nend)\n"),
            "foo(function()\n    bar()\nend)\n"
        );
    }

    #[test]
    fn quotes_follow_the_option() {
        assert_eq!(fmt("local s = 'a'\n"), "local s = \"a\"\n");
        assert_eq!(fmt("local s = 'say \"hi\"'\n"), "local s = 'say \"hi\"'\n");
        let mut o = FmtConfig::default();
        o.quote_style = QuoteStyle::ForceSingle;
        assert_eq!(
            format_with("local s = \"it's\"\n", &o).unwrap(),
            "local s = 'it\\'s'\n"
        );
    }

    #[test]
    fn call_parentheses_follow_the_option() {
        assert_eq!(
            fmt("print \"x\"\nf { a = 1 }\n"),
            "print(\"x\")\nf({ a = 1 })\n"
        );
        let mut o = FmtConfig::default();
        o.call_parentheses = CallParentheses::None;
        assert_eq!(format_with("print(\"x\")\n", &o).unwrap(), "print \"x\"\n");
    }

    #[test]
    fn a_struct_fields_form_is_not_a_call() {
        assert_eq!(
            fmt("local p = new P { x = 1 }\n"),
            "local p = new P { x = 1 }\n"
        );
    }

    #[test]
    fn leading_zeros_follow_the_option() {
        assert_eq!(fmt("local x = .5\n"), "local x = 0.5\n");
    }

    #[test]
    fn import_lists_expand_on_a_trailing_comma_or_when_asked() {
        let src = "import { world, pair, } from \"@pkg/jecs\"\nimport { x, y } from \"./m\"\nexport { x, y }\nprint(world, pair, x, y)\n";
        assert_eq!(
            format(src).unwrap(),
            "import {\n    world,\n    pair,\n} from \"@pkg/jecs\"\nimport { x, y } from \"./m\"\nexport { x, y }\nprint(world, pair, x, y)\n"
        );

        let mut o = FmtConfig::default();
        o.expand_imports = true;
        assert_eq!(
            format_with("import a, { x, y } from \"./m\"\nimport { one } from \"./o\"\nexport { x, y }\nprint(a, x, y, one)\n", &o).unwrap(),
            "import a, {\n    x,\n    y,\n} from \"./m\"\nimport { one } from \"./o\"\nexport {\n    x,\n    y,\n}\nprint(a, x, y, one)\n"
        );
    }

    #[test]
    fn imports_sort_when_asked() {
        let mut o = FmtConfig::default();
        o.sort_requires.enabled = true;
        o.sort_requires.grouping = RequireGrouping::ByKind;
        let src = "import { b } from \"./b\"\nimport { a } from \"@pkg/a\"\nprint(a, b)\n";
        assert_eq!(
            format_with(src, &o).unwrap(),
            "import { a } from \"@pkg/a\"\nimport { b } from \"./b\"\nprint(a, b)\n"
        );
    }

    #[test]
    fn blank_lines_collapse_and_the_file_ends_with_one_newline() {
        assert_eq!(
            fmt("\n\nlocal a = 1\n\n\n\nlocal b = 2\n\n\n"),
            "local a = 1\n\nlocal b = 2\n"
        );
    }

    #[test]
    fn a_blank_line_at_the_edge_of_a_block_goes() {
        assert_eq!(
            fmt("if x then\n\n    y()\n\nend\n"),
            "if x then\n    y()\nend\n"
        );
    }

    #[test]
    fn match_arms_indent_once_and_bodies_twice() {
        let src = "match m with\ncase Ok(v) then\nprint(v)\ncase Err(e) then print(e)\ndefault\nprint(0)\nend\n";
        let want = "match m with\n    case Ok(v) then\n        print(v)\n    case Err(e) then print(e)\n    default\n        print(0)\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn long_strings_and_comments_keep_their_text() {
        let src = "local s = [[\n  keep\n\tthis  \n]]\nprint(s) -- note\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn tabs_when_asked() {
        let mut o = FmtConfig::default();
        o.indent_type = IndentType::Tabs;
        assert_eq!(
            format_with("if x then\ny()\nend\n", &o).unwrap(),
            "if x then\n\ty()\nend\n"
        );
    }

    #[test]
    fn simple_statements_collapse_when_asked() {
        let mut o = FmtConfig::default();
        o.collapse_simple_statement = Collapse::Always;
        assert_eq!(
            format_with("if x then\n    return 1\nend\n", &o).unwrap(),
            "if x then return 1 end\n"
        );
    }

    #[test]
    fn call_chains_break_when_asked() {
        let mut o = FmtConfig::default();
        o.call_chains.style = CallChainStyle::Method;
        o.call_chains.min_calls = 3;
        assert_eq!(
            format_with("local v = xs:map(f):filter(g):reduce(h, 0)\n", &o).unwrap(),
            "local v = xs:map(f)\n    :filter(g)\n    :reduce(h, 0)\n"
        );
    }

    #[test]
    fn struct_fields_align_when_asked() {
        let mut o = FmtConfig::default();
        o.align_struct_fields = true;
        assert_eq!(
            format_with("struct P as\n    x: number\n    name: string\nend\n", &o).unwrap(),
            "struct P as\n    x:    number\n    name: string\nend\n"
        );
    }

    #[test]
    fn formatting_is_idempotent_on_the_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");

        if !dir.is_dir() {
            eprintln!("skipped: no examples checkout at {}", dir.display());

            return;
        }

        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aly") {
                let src = std::fs::read_to_string(&path).unwrap();
                let once = format(&src).unwrap();
                let twice = format(&once).unwrap();
                assert_eq!(once, twice, "{}", path.display());
                // The token stream holds, save for the rewrites.
                let norm = |text: &str| -> Vec<String> {
                    lex(text)
                        .unwrap()
                        .toks
                        .iter()
                        .map(|t| t.text(text).replace('\'', "\""))
                        .filter(|t| !matches!(t.as_str(), "(" | ")" | ","))
                        .collect()
                };
                assert_eq!(norm(&src), norm(&once), "{}", path.display());
            }
        }
    }
}
