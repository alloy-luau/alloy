//! Bracket-group breaking: the block depth of each item, the tree of
//! bracket groups, which groups must break, and the rendering of the
//! tree into lines.

use super::{
    Formatter, Node, block_opener, closer_of, closes, expression_context, is_keyword, opens,
};

impl<'s> Formatter<'s> {
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
    pub(crate) fn starts_block(&self, i: usize) -> bool {
        let text = self.items[i].text.as_str();
        let prev = self.prev_code(i).map(|p| self.items[p].text.as_str());

        match text {
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
                !prev.is_some_and(expression_context)
                    || (self.first_on_line(i)
                        && matches!(prev, Some("?") | Some("!") | Some(">") | Some(">>")))
            }

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

    /// Whether the item opens its line.
    fn first_on_line(&self, i: usize) -> bool {
        i == 0 || self.items[i].newlines_before > 0
    }

    pub(crate) fn line_has_before(&self, i: usize, word: &str) -> bool {
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
    pub(crate) fn block_depths(&mut self) -> Vec<usize> {
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

            // A `then`, `else`, or `elseif` that opens a line continues
            // the `if` expression above it; any other token ends it.
            let continues_expr_if = matches!(text, "else" | "elseif")
                || (text == "then" && stack.last() == Some(&Frame::ExprIf));

            if it.newlines_before > 0 && !continues_expr_if {
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
                        // A `then` or `else` that opens a line inside an
                        // `if` expression continues it, one level in.
                        depths[i] = level(&stack) + usize::from(!mid_line);
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

                "then" if stack.last() == Some(&Frame::ExprIf) && it.newlines_before > 0 => {
                    depths[i] = level(&stack) + 1;
                }

                _ => {
                    depths[i] = level(&stack);

                    if text == "if"
                        && ((prev.is_some_and(expression_context)
                            && !(self.first_on_line(i)
                                && matches!(prev, Some("?") | Some("!") | Some(">") | Some(">>"))))
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

                (t.is_ident() && (field || !is_keyword(&t.text))) || t.is(">")
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

    pub(crate) fn render_nodes(&mut self, nodes: &[Node], hard: &[bool], extra: usize) {
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
    pub(crate) fn inner_space(&self, open: usize) -> bool {
        match self.items[open].text.as_str() {
            "{" => self.options.space_inside_braces,
            "(" | "?(" => self.options.space_inside_parens,
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
}
