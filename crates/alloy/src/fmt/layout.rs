//! Bracket-group breaking: the block depth of each item, the tree of
//! bracket groups, which groups must break, and the rendering of the
//! tree into lines.

use alloy_syntax::lexer::TokKind;

use super::{Formatter, ItemKind, Node, closer_of, closes, expression_context, opens};

impl<'s> Formatter<'s> {
    // --- block depth -------------------------------------------------------------

    /// +1 for an item that opens a block, -1 for one that closes it.
    fn block_delta(&self, i: usize) -> i32 {
        let it = &self.items[i];

        if it.is_comment() {
            return 0;
        }

        if (it.opens_block_here() && self.starts_block(i)) || self.is_loop_head(i) {
            1
        } else if it.is("end") || it.is("until") {
            -1
        } else {
            0
        }
    }

    /// Whether the keyword at `i` opens a block: `function` always but
    /// for a signature; `if`, `do`, `match` when they start a statement.
    pub(crate) fn starts_block(&self, i: usize) -> bool {
        let text = self.items[i].text.as_str();
        let prev = self.prev_code(i).map(|p| self.items[p].text.as_str());

        match text {
            // `attribute X on struct` closes on its own line; the `as`
            // form opens a body of `requires` clauses.
            "attribute" => self.line_has_after(i, "as"),

            // `x is function` names a type; nothing opens.
            "function" => {
                !self.line_has_before(i, "declare")
                    && !self.line_has_before(i, "attribute")
                    && prev != Some("remote")
                    && prev != Some("is")
                    && self.signature.get(i) != Some(&true)
            }

            // `): number?` ends a signature line; the `if` that opens
            // the next line is a statement, not a ternary.
            "if" => {
                !self.expr_ifs.contains(&self.items[i].start)
                    && (!prev.is_some_and(expression_context)
                        || (self.first_on_line(i)
                            && matches!(prev, Some("?") | Some("!") | Some(">") | Some(">>"))))
            }

            "do" => !self.for_header_before(i),

            // A local named match opens nothing; `match x with` does.
            "match" => !self.items[i].name_here && !matches!(prev, Some(".") | Some(":")),

            // `trait = 1` writes the word as a name, and opens nothing.
            "struct" | "enum" | "trait" | "impl" | "interface" | "macro" | "namespace" => {
                !self.items[i].name_here
                    && (self.items[i].newlines_before > 0
                        || i == 0
                        || matches!(prev, Some("export" | "global" | "public" | "private"))
                        || (prev == Some("default")
                            && self
                                .prev_code(i)
                                .and_then(|p| self.prev_code(p))
                                .is_some_and(|p| self.items[p].text == "export"))
                        || self.attributes_open_line(i))
            }

            // `class Name` opens a body, as the structure pass reads
            // it; a class used as a name, `class = 1`, opens nothing.
            "class" => {
                prev == Some("declare")
                    || self.next_code(i).is_some_and(|n| {
                        self.items[n].is_ident()
                            && !self.next_code(n).is_some_and(|m| {
                                matches!(
                                    self.items[m].text.as_str(),
                                    "=" | "(" | "." | ":" | "[" | ","
                                )
                            })
                    })
            }

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

    /// Whether the item opens its line.
    fn first_on_line(&self, i: usize) -> bool {
        i == 0 || self.items[i].newlines_before > 0
    }

    /// Reports if `word` stands later on the line item `i` sits on.
    pub(crate) fn line_has_after(&self, i: usize, word: &str) -> bool {
        for j in i + 1..self.items.len() {
            if self.items[j].newlines_before > 0 {
                return false;
            }

            if self.items[j].is(word) {
                return true;
            }
        }

        false
    }

    pub(crate) fn line_has_before(&self, i: usize, word: &str) -> bool {
        let mut j = i;

        // An item that opens its line has nothing before it there.
        while j > 0 && self.items[j].newlines_before == 0 {
            j -= 1;

            if self.items[j].is(word) {
                return true;
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
    pub(crate) fn block_depths(&mut self) -> Vec<usize> {
        #[derive(PartialEq, Clone, Copy)]
        enum Frame {
            Block,
            Trait,
            /// The body of an `attribute ... as ... end`. Its `function`
            /// and `field` words belong to a `requires` clause, so
            /// neither opens a block of its own.
            Contract,
            Match,
            Arm,
            /// An `if` expression, with the bracket depth it opened at and
            /// whether its `else` came yet.
            ExprIf(usize, bool),
            /// The body of a `declare class` or of an extern type's
            /// `with`. Its methods are signatures and open nothing.
            Class,
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut depths = vec![0usize; self.items.len()];
        let mut signature = vec![false; self.items.len()];
        let level = |stack: &Vec<Frame>| {
            stack
                .iter()
                .filter(|f| !matches!(f, Frame::ExprIf(..)))
                .count()
        };
        let in_expr_if = |stack: &Vec<Frame>| matches!(stack.last(), Some(Frame::ExprIf(..)));
        // A line that continues an `if` expression sits one level in per
        // open `if` expression, so a nested one indents under its parent.
        let expr_ifs = |stack: &Vec<Frame>| {
            stack
                .iter()
                .filter(|f| matches!(f, Frame::ExprIf(..)))
                .count()
        };
        // The open brackets before the item. A closer ends only an `if`
        // expression that opened inside its group, so the `)` of a call
        // in a branch leaves the `if` open.
        let mut brackets = 0usize;

        for i in 0..self.items.len() {
            let it = &self.items[i];

            if it.is_comment() {
                depths[i] = level(&stack);

                continue;
            }

            let text = it.text.as_str();
            let prev = self.prev_code(i).map(|p| self.items[p].text.as_str());

            // A line that opens the body of a branch continues the `if`
            // expression, so it sits one level in like the `else` does.
            let opens_expr_branch = it.newlines_before > 0
                && in_expr_if(&stack)
                && matches!(prev, Some("then") | Some("else"));

            // A `then`, `else`, or `elseif` that opens a line continues
            // the `if` expression above it; any other token ends it.
            let continues_expr_if = matches!(text, "else" | "elseif")
                || (text == "then" && in_expr_if(&stack))
                || opens_expr_branch;

            // A newline inside a group that opened in the `if` expression
            // is the group's, so it ends no `if` outside the group.
            if it.newlines_before > 0 && !continues_expr_if {
                while matches!(stack.last(), Some(Frame::ExprIf(b, _)) if *b >= brackets) {
                    stack.pop();
                }
            }

            // An `if` expression whose `else` came is whole, so the next
            // `else` or `elseif` at its depth belongs to the one around it.
            if matches!(text, "else" | "elseif") {
                while matches!(stack.last(), Some(Frame::ExprIf(b, true)) if *b == brackets) {
                    stack.pop();
                }
            }

            if opens(text) {
                brackets += 1;
            }

            // An `end` or an `until` closes a block, so an `if` expression
            // before it on its line is whole.
            if matches!(text, "end" | "until") {
                while in_expr_if(&stack) {
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
                        Some(
                            Frame::Block
                                | Frame::Match
                                | Frame::Trait
                                | Frame::Contract
                                | Frame::Class
                        )
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
                    } else if in_expr_if(&stack) || (mid_line && self.line_has_before(i, "if")) {
                        // A `then` or `else` that opens a line inside an
                        // `if` expression continues it, one level in for
                        // each open `if` expression.
                        depths[i] = level(&stack) + if mid_line { 0 } else { expr_ifs(&stack) };

                        if let Some(Frame::ExprIf(_, has_else)) = stack.last_mut() {
                            *has_else |= text == "else";
                        }
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
                    brackets = brackets.saturating_sub(1);

                    // An expression `if` ends at the closer of its group.
                    while matches!(stack.last(), Some(Frame::ExprIf(b, _)) if *b > brackets) {
                        stack.pop();
                    }
                }

                "then" if in_expr_if(&stack) && it.newlines_before > 0 => {
                    depths[i] = level(&stack) + expr_ifs(&stack);
                }

                _ => {
                    depths[i] = level(&stack)
                        + if opens_expr_branch {
                            expr_ifs(&stack)
                        } else {
                            0
                        };

                    if text == "if"
                        && ((prev.is_some_and(expression_context)
                            && !(self.first_on_line(i)
                                && matches!(prev, Some("?") | Some("!") | Some(">") | Some(">>"))))
                            || (matches!(prev, Some("then") | Some("else")) && in_expr_if(&stack))
                            || self.expr_ifs.contains(&it.start))
                    {
                        stack.push(Frame::ExprIf(brackets, false));
                    } else if text == "match" && !it.name_here && self.starts_block(i) {
                        stack.push(Frame::Match);
                    } else if text == "trait" && self.starts_block(i) {
                        stack.push(Frame::Trait);
                    } else if text == "attribute" && self.starts_block(i) {
                        stack.push(Frame::Contract);
                    } else if stack.last() == Some(&Frame::Contract) {
                        // Every word of a `requires` clause sits on one
                        // line, so nothing inside a contract body opens.
                    } else if text == "function" && stack.last() == Some(&Frame::Class) {
                        signature[i] = true;
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
                            || ((it.opens_block_here() && text != "with" || text == "class")
                                && self.starts_block(i))
                            || (text == "with"
                                && self.starts_block(i)
                                && stack.last() != Some(&Frame::Match));

                        let declared =
                            text == "with" || (text == "class" && prev == Some("declare"));

                        if opens_block && declared {
                            stack.push(Frame::Class);
                        } else if opens_block {
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
    pub(crate) fn generic_brackets(&self) -> Vec<bool> {
        let mut generic = vec![false; self.items.len()];

        for i in 0..self.items.len() {
            let it = &self.items[i];

            // `a < b` compares, so a `<` with a space before it opens no
            // type arguments. `<<` is the bracket wherever it stands.
            if !(it.is("<") || it.is("<<")) || (it.space_before && !it.is("<<")) {
                continue;
            }

            let opens_generic = self.prev_code(i).is_some_and(|p| {
                let t = &self.items[p];
                // `Signal.new` and `Result.ok` end in a word the lexer
                // also uses as a keyword. After a `.` or a `:` the word
                // is a field name, so `<<` there opens type arguments.
                let field = self.prev_code(p).is_some_and(|q| {
                    let before = &self.items[q];

                    before.is(".") || before.is(":") || before.is("?.") || before.is("?:")
                });

                (t.is_ident() && (field || !t.is_keyword_here())) || t.is(">")
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

    pub(crate) fn tree(&self) -> Vec<Node> {
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
    pub(crate) fn hard_breaks(&self, tree: &[Node]) -> Vec<bool> {
        let mut hard = vec![false; self.items.len()];
        self.mark_hard(tree, &mut hard, false, 0);

        // An interpolation hole keeps its line, so a newline in one joins.
        for (h, inside) in hard.iter_mut().zip(&self.hole) {
            *h &= !inside;
        }

        hard
    }

    fn mark_hard(&self, nodes: &[Node], hard: &mut [bool], in_group: bool, mut block_depth: i32) {
        for n in nodes {
            match n {
                Node::Item(i) => {
                    let it = &self.items[*i];

                    if self.forced[*i]
                        || (it.newlines_before > 0
                            && (!in_group || block_depth > 0 || it.is_comment()))
                    {
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

                    if self.forced[*open]
                        || (it.newlines_before > 0 && (!in_group || block_depth > 0))
                    {
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

    pub(crate) fn render_nodes(&mut self, nodes: &[Node], hard: &[bool], extra: usize) {
        for node in nodes {
            match node {
                Node::Item(i) => self.render_item(*i, hard, extra),

                Node::Group { .. } => self.render_group(node, hard, extra),
            }
        }
    }

    /// Where item `i` starts: a new line, or a space on this one. A
    /// line comment runs to the end of its line, so an item that would
    /// land after one breaks whatever the layout asked; the token would
    /// otherwise sit inside the comment and the file would lose it.
    fn open_place(&mut self, i: usize, hard: &[bool], extra: usize) {
        if hard[i] || self.after_line_comment(i) {
            self.newline_before(i, extra);
        } else {
            self.space_before_item(i);
        }
    }

    /// Whether the item before `i` is a line comment on the line under
    /// construction.
    fn after_line_comment(&self, i: usize) -> bool {
        i > 0
            && self.items[i - 1].kind == ItemKind::LineComment
            && self.at_line[i - 1] == self.lines.len()
    }

    fn render_item(&mut self, i: usize, hard: &[bool], extra: usize) {
        self.open_place(i, hard, extra);

        // The line this item lands on, for the `if` expression rule: it
        // reads the width the render came out with.
        self.at_line[i] = self.lines.len();
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

        self.open_place(open, hard, extra);

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
                // A comment the source wrote on the separator's line
                // trails the element before it, so it keeps that line
                // and the rest of this element starts a new one.
                let lead = el
                    .iter()
                    .take_while(|n| match n {
                        Node::Item(i) => {
                            let it = &self.items[*i];

                            it.is_comment() && it.newlines_before == 0
                        }

                        Node::Group { .. } => false,
                    })
                    .count();
                self.render_nodes(&el[..lead], hard, extra);

                // The element was only a trailing comment.
                if lead == el.len() {
                    continue;
                }

                self.flush();
                self.line_level = base + 1;
                self.line = self.indent(base + 1);
                // The comma follows the code: after a comment it would
                // be part of the comment, and each run would add one.
                let body = &el[lead..];
                let code = body
                    .iter()
                    .rposition(|n| !matches!(n, Node::Item(i) if self.items[*i].is_comment()))
                    .map_or(0, |p| p + 1);
                self.render_nodes(&body[..code], hard, extra + 1);
                let last = k + 1 == elements.len();

                if code > 0 && (!last || trailing) {
                    let t = sep
                        .map(|s| self.items[s].text.clone())
                        .unwrap_or_else(|| ",".to_string());
                    self.line.push_str(&t);
                }

                self.render_nodes(&body[code..], hard, extra + 1);
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
        if self.hole[open] {
            return false;
        }

        if magic && self.options.magic_trailing_comma {
            return true;
        }

        // A preserving run reflows no line: a group the author opened
        // onto its own lines stays open.
        if !self.options.recommended
            && self
                .items
                .get(open + 1)
                .is_some_and(|i| i.newlines_before > 0)
        {
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

        // A branch of an `if` expression the width forced onto its own
        // line: the group holds a break, so it cannot stay flat.
        let has_forced = elements.iter().any(|(el, _)| {
            let mut block_depth = 0i32;

            el.iter().any(|n| match n {
                Node::Item(i) => {
                    block_depth += self.block_delta(*i);

                    self.forced[*i] && block_depth <= 0
                }

                Node::Group { open, .. } => self.forced[*open],
            })
        });

        if has_forced {
            return true;
        }

        // The `if` expression around the group breaks at its keywords
        // before the group breaks, the way StyLua lays one out.
        if self.held[open] {
            return false;
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
    pub(crate) fn inner_space(&self, open: usize) -> bool {
        match self.items[open].text.as_str() {
            "{" => self.options.space_inside_braces,
            "(" | "?(" => self.options.space_inside_parens,
            // Luau's own list, `@[native]`, keeps the form Luau writes.
            "[" if self.luau_attr_list(open) => false,

            "[" | "?[" => {
                if self.is_index(open) || self.in_macro_brackets(open) {
                    self.options.space_inside_brackets
                } else {
                    self.options.space_inside_array
                }
            }
            _ => false,
        }
    }

    /// Whether the line of item `i` opens with an attribute before it:
    /// `@test namespace Suite`, `@derive(Eq) struct P`.
    fn attributes_open_line(&self, i: usize) -> bool {
        let mut j = i;

        while j > 0 && self.items[j].newlines_before == 0 {
            j -= 1;
        }

        j < i && self.items[j].is("@")
    }

    /// The `[` of Luau's attribute list, `@[native]`.
    pub(crate) fn luau_attr_list(&self, open: usize) -> bool {
        self.items[open].is("[") && self.prev_code(open).is_some_and(|p| self.items[p].is("@"))
    }

    /// Whether item `i` sits inside Luau's attribute list, where the
    /// arguments are Luau's and take no rewrite.
    pub(crate) fn in_luau_attr_list(&self, i: usize) -> bool {
        let mut at = i;

        while let Some(open) = self.enclosing_open(at) {
            if self.luau_attr_list(open) {
                return true;
            }

            at = open;
        }

        false
    }

    /// A pair of `$map[["a", 1], ["b", 2]]`: the macro's brackets and the
    /// pairs inside them are one literal, so they take one spacing rule.
    fn in_macro_brackets(&self, open: usize) -> bool {
        let Some(outer) = self.enclosing_open(open) else {
            return false;
        };

        if !self.items[outer].is("[") {
            return false;
        }

        self.prev_code(outer)
            .and_then(|name| self.prev_code(name))
            .is_some_and(|sigil| self.items[sigil].is("$"))
    }

    /// `[` that indexes, as opposed to an array literal or a type's
    /// `{ [k]: v }`.
    fn is_index(&self, i: usize) -> bool {
        if self.items[i].is("?[") {
            return true;
        }

        self.prev_code(i).is_some_and(|p| {
            let t = &self.items[p];

            (t.is_ident() && !t.is_keyword_here())
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

    // --- the `if` expression ------------------------------------------------------

    /// Breaks every `if` expression the last render left on one line past
    /// `column_width`. True when one broke, so the caller renders again.
    ///
    /// The shape is StyLua's: the condition stays after the `if`, and the
    /// first `then` and each `elseif` and `else` open a line one level in,
    /// each with its value. An `if` inside one that breaks here waits for
    /// the next pass, so it breaks only when its own line is too long.
    pub(crate) fn force_long_expr_ifs(&mut self) -> bool {
        let mut changed = false;
        // The end of the last `if` that broke in this pass.
        let mut broke_until = 0;

        for i in 0..self.items.len() {
            if i < broke_until || self.hole[i] || !self.items[i].is("if") || self.starts_block(i) {
                continue;
            }

            let (breaks, end) = self.expr_if_breaks(i);

            if breaks.is_empty() || breaks.iter().any(|b| self.forced[*b]) {
                continue;
            }

            // One line holds the whole expression, and it is too long.
            let line = self.at_line[i];

            if breaks.iter().any(|b| self.at_line[*b] != line) {
                continue;
            }

            let width = self.lines.get(line).map_or(0, |l| l.chars().count());

            if width <= self.options.column_width {
                continue;
            }

            for b in breaks {
                self.forced[b] = true;
                self.items[b].newlines_before = self.items[b].newlines_before.max(1);
            }

            broke_until = end;
            changed = true;
        }

        changed
    }

    /// The items an `if` expression breaks before, the first `then` and
    /// each `elseif` and `else`, and the index past its last item. An `if`
    /// inside it keeps its own keywords. No breaks for an expression the
    /// parser would refuse.
    fn expr_if_breaks(&self, start: usize) -> (Vec<usize>, usize) {
        let mut out = Vec::new();
        let mut depth = 0i32;
        // The `if` expressions open inside this one. Each takes the next
        // `else`, and an `elseif` before it.
        let mut inner = 0usize;
        let mut has_else = false;
        let mut i = start + 1;

        while i < self.items.len() {
            let it = &self.items[i];

            if it.is_comment() {
                i += 1;

                continue;
            }

            let text = it.text.as_str();
            let prev = self.prev_code(i).map(|p| self.items[p].text.as_str());
            // A `then`, `else`, or `elseif` that opens a line continues
            // the expression, and so does the value after one of them.
            let continues = matches!(text, "then" | "else" | "elseif")
                || matches!(prev, Some("then") | Some("else"));

            if depth == 0 && it.newlines_before > 0 && !continues {
                break;
            }

            if opens(text) {
                depth += 1;
            } else if closes(text) {
                if depth == 0 {
                    break;
                }

                depth -= 1;
            } else if depth == 0 {
                match text {
                    "if" => inner += 1,

                    // A keyword after this expression's own `else`
                    // belongs to an `if` around it.
                    "then" | "elseif" | "else" if inner == 0 && has_else => break,

                    "then" if inner == 0 && out.is_empty() => out.push(i),

                    "elseif" if inner == 0 => out.push(i),

                    "else" if inner > 0 => inner -= 1,

                    "else" => {
                        has_else = true;
                        out.push(i);
                    }

                    "," | ";" | "end" | "do" | "return" => break,

                    _ => {}
                }
            }

            i += 1;
        }

        (out, i)
    }

    /// The items of each `if` expression that has no hard break inside it
    /// yet. See `held`.
    pub(crate) fn held_items(&self, hard: &[bool]) -> Vec<bool> {
        let mut held = vec![false; self.items.len()];

        for i in 0..self.items.len() {
            if !self.items[i].is("if") || self.starts_block(i) {
                continue;
            }

            let (breaks, end) = self.expr_if_breaks(i);

            if !breaks.is_empty() && !hard[i + 1..end].contains(&true) {
                held[i..end].fill(true);
            }
        }

        held
    }

    /// Whether each item sits inside an interpolation hole. The `}` that
    /// closes a hole counts, so no line breaks before it either.
    pub(crate) fn holes(&self) -> Vec<bool> {
        let mut depth = 0usize;

        self.items
            .iter()
            .map(|it| match it.kind {
                ItemKind::Tok(TokKind::InterpHead) => {
                    depth += 1;

                    depth > 1
                }

                ItemKind::Tok(TokKind::InterpTail) => {
                    depth = depth.saturating_sub(1);

                    true
                }

                _ => depth > 0,
            })
            .collect()
    }

    // --- the `match` block ---------------------------------------------------------

    /// Breaks every `match` the last render left crowded: one whose head
    /// line runs past `column_width` with an arm still on it, or one that
    /// holds two arms on a line. True when one broke, so the caller
    /// renders again.
    ///
    /// The shape is the one a hand-broken `match` already takes: each arm
    /// one level under the `match`, its body one level deeper, and `end`
    /// back at the `match`. A `match` whose arms each own a line stays as
    /// it is, so a second run changes nothing.
    pub(crate) fn force_long_matches(&mut self) -> bool {
        let mut changed = false;

        for i in 0..self.items.len() {
            if !self.items[i].is("match") || self.items[i].name_here || !self.starts_block(i) {
                continue;
            }

            let (arms, bodies, close) = self.match_parts(i);

            if arms.is_empty() {
                continue;
            }

            let head = self.at_line[i];
            let long =
                self.lines.get(head).map_or(0, |l| l.chars().count()) > self.options.column_width;
            let on_head = arms
                .iter()
                .chain(close.iter())
                .any(|m| self.at_line[*m] == head);
            let crowded = arms
                .windows(2)
                .any(|w| self.at_line[w[0]] == self.at_line[w[1]]);

            if !crowded && !(long && on_head) {
                continue;
            }

            for b in arms.into_iter().chain(bodies).chain(close) {
                if self.items[b].newlines_before == 0 {
                    self.forced[b] = true;
                    self.items[b].newlines_before = 1;
                    changed = true;
                }
            }
        }

        changed
    }

    /// The arms of the `match` at `start`, the first item of each arm
    /// body, and the `end` that closes the block. An arm of a nested
    /// `match` belongs to that one, so only the arms one block in count.
    fn match_parts(&self, start: usize) -> (Vec<usize>, Vec<usize>, Option<usize>) {
        let mut arms = Vec::new();
        let mut bodies = Vec::new();
        let mut close = None;
        let mut depth = 1i32;
        let mut i = start + 1;

        while i < self.items.len() {
            if self.items[i].is_comment() {
                i += 1;

                continue;
            }

            depth += self.block_delta(i);

            if depth == 0 {
                close = Some(i);

                break;
            }

            let it = &self.items[i];
            let arm = depth == 1 && !it.name_here && (it.is("case") || it.is("default"));

            if arm {
                arms.push(i);

                // The body opens after the `then` of a `case`; a
                // `default` takes the arm word itself.
                let head = match it.is("default") {
                    true => Some(i),

                    false => self.arm_then(i),
                };

                if let Some(h) = head
                    && let Some(b) = self.next_code(h)
                    && !self.opens_arm(b)
                {
                    bodies.push(b);
                }
            }

            i += 1;
        }

        (arms, bodies, close)
    }

    /// The `then` that ends the head of the `case` arm at `start`, or
    /// none for an arm the parser would refuse.
    fn arm_then(&self, start: usize) -> Option<usize> {
        let mut bracket = 0i32;

        for j in start + 1..self.items.len() {
            let t = &self.items[j];

            if t.is_comment() {
                continue;
            }

            if opens(&t.text) {
                bracket += 1;
            } else if closes(&t.text) {
                bracket -= 1;
            } else if bracket <= 0 {
                if t.is("then") {
                    return Some(j);
                }

                if self.opens_arm(j) {
                    return None;
                }
            }
        }

        None
    }

    /// Whether the item at `i` ends the arm before it: the word of
    /// another arm, or the `end` of the `match`.
    fn opens_arm(&self, i: usize) -> bool {
        let it = &self.items[i];

        !it.name_here && (it.is("case") || it.is("default") || it.is("end"))
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
}
