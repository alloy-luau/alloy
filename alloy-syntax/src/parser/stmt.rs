//! Statements, blocks, and the declaration forms.

use crate::lexer::TokKind;

use super::*;

impl<'a> Parser<'a> {
    // --- statements --------------------------------------------------------

    pub(super) fn at_block_end(&self) -> bool {
        matches!(self.text(), "end" | "else" | "elseif" | "until")
            || (self.in_match_arm > 0 && matches!(self.text(), "case" | "default"))
    }

    pub(super) fn block(&mut self) -> Result<Block, ParseError> {
        self.enter()?;

        let start = self.pos;
        let mut stmts = Vec::new();

        while !self.at_end() && !self.at_block_end() {
            let is_return = self.at("return");
            let stmt_start = self.pos;

            let stmt = match self.stmt() {
                Ok(s) => s,

                Err(e) if self.lenient && self.diagnostics.len() < MAX_DIAGNOSTICS => {
                    self.diagnostics.push(e);
                    self.pos = stmt_start;
                    self.skip_to_recovery_point();
                    stmts.push(Stmt::Error(TokSpan::new(stmt_start, self.pos)));

                    continue;
                }

                Err(e) if self.lenient => {
                    // Past the cap: one node takes the rest of the file.
                    let _ = e;
                    self.pos = self.toks.len();
                    stmts.push(Stmt::Error(TokSpan::new(stmt_start, self.pos)));

                    break;
                }

                Err(e) => return Err(e),
            };

            stmts.push(stmt);

            if is_return {
                // A return ends its block. Only a `;` can follow.
                if self.at(";") {
                    let i = self.bump();
                    stmts.push(Stmt::Empty(TokSpan::new(i, i + 1)));
                }

                break;
            }
        }

        self.leave();
        Ok(Block {
            stmts,
            span: TokSpan::new(start, self.pos),
        })
    }

    /*
    Moves past a failed statement to the next place one can begin.

    The first token always goes, whatever it is: that is the advance
    guarantee. After it the skip stops at a block-end keyword, at a keyword
    that opens a statement, or at the first token on a new line. A `(` on a
    new line does not count, because Luau itself reads that as ambiguous.
    Brackets are not balanced on purpose; a lost `}` would otherwise swallow
    the rest of the file into one error node.
    */
    fn skip_to_recovery_point(&mut self) {
        debug_assert!(!self.at_end(), "recovery starts on a token");
        self.bump();

        while !self.at_end() {
            if self.at_block_end() || self.opens_statement() {
                break;
            }

            if self.newline_before_pos() && !self.at("(") {
                break;
            }

            self.bump();
        }
    }

    /// Reports if the token at the cursor is a keyword that begins a statement.
    fn opens_statement(&self) -> bool {
        matches!(
            self.text(),
            "local" | "if" | "while" | "for" | "repeat" | "do" | "function" | "return" | "break"
        ) || (self.at("continue") && self.continue_is_keyword())
            || (self.at("const") && self.name_at(1))
            || (self.at("async") && self.text_at(1) == "function")
            || (self.at("delete") && self.name_at(1))
            || (self.at("import") && self.import_follows())
            || (self.at("enum") && self.name_at(1) && self.text_at(2) == "as")
            || (self.at("impl") && self.name_at(1))
            || (self.at("match") && self.match_follows())
            || (matches!(
                self.text(),
                "struct" | "trait" | "interface" | "remote" | "attribute" | "macro"
            ) && self.name_at(1))
            || (self.at("export")
                && matches!(self.text_at(1), "local" | "const" | "function" | "type"))
            || (self.at("type") && self.type_is_alias())
    }

    pub(super) fn stmt(&mut self) -> Result<Stmt, ParseError> {
        self.enter()?;
        let r = self.stmt_inner();
        self.leave();

        r
    }

    pub(super) fn stmt_inner(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;

        match self.text() {
            ";" => {
                self.bump();

                Ok(Stmt::Empty(TokSpan::new(start, self.pos)))
            }

            "if" => self.if_stmt(start),

            "while" => {
                self.bump();
                let cond = self.cond()?;
                self.expect("do")?;
                let block = self.block()?;
                self.expect("end")?;
                Ok(Stmt::While(While {
                    cond,
                    block,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "do" => {
                self.bump();
                let block = self.block()?;
                self.expect("end")?;
                Ok(Stmt::Do(DoBlock {
                    block,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "for" => self.for_stmt(start),

            "repeat" => {
                self.bump();
                let block = self.block()?;
                self.expect("until")?;
                let cond = self.expr()?;
                Ok(Stmt::Repeat(Repeat {
                    block,
                    cond,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "function" => self.function_stmt(start, Vec::new()),

            "async" if self.text_at(1) == "function" && !self.newline_after(0) => {
                let is_async = Some(TokSpan::new(self.bump(), self.pos));
                let mut stmt = self.function_stmt(start, Vec::new())?;

                if let Stmt::Function(f) = &mut stmt {
                    f.body.is_async = is_async;
                }

                Ok(stmt)
            }

            // `$name(args)` as a statement: an intrinsic or macro call.
            "$" => {
                let call = self.simple_expr()?;

                Ok(Stmt::Call(call, TokSpan::new(start, self.pos)))
            }

            "delete" if self.prefix_word_here() && self.name_at(1) => {
                self.bump();
                let expr = self.suffixed_expr()?;

                Ok(Stmt::Delete {
                    expr,
                    span: TokSpan::new(start, self.pos),
                })
            }

            "local" | "const" => self.local_stmt(start),

            "return" => {
                self.bump();
                let values = if self.at_end() || self.at_block_end() || self.at(";") {
                    Vec::new()
                } else {
                    self.expr_list()?
                };

                Ok(Stmt::Return(Return {
                    values,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "break" => {
                self.bump();

                Ok(Stmt::Break(TokSpan::new(start, self.pos)))
            }

            "continue" if self.continue_is_keyword() => {
                self.bump();

                Ok(Stmt::Continue(TokSpan::new(start, self.pos)))
            }

            "@" => {
                let attrs = self.attrs()?;
                let attributes: Vec<TokSpan> = attrs.iter().map(|a| a.span).collect();

                // A definitions file decorates declarations the same way.
                if self.options.definitions && self.at("declare") {
                    return self.declare_stmt(start);
                }

                let exported = self.at("export");

                if exported {
                    self.bump();
                }

                // Declarations that take attributes.
                match self.text() {
                    "struct" if self.name_at(1) => return self.struct_decl(start, attrs, exported),

                    "enum" if self.name_at(1) => {
                        let mut stmt = self.enum_decl(start, exported)?;

                        if let Stmt::Enum(e) = &mut stmt {
                            e.attributes = attrs;
                        }

                        return Ok(stmt);
                    }

                    "trait" if self.name_at(1) => return self.trait_decl(start, attrs, exported),

                    "remote" if self.name_at(1) || self.text_at(1) == "function" => {
                        return self.remote_decl(start, attrs, exported);
                    }

                    "impl" if self.name_at(1) => return self.impl_decl(start, exported),

                    _ => {}
                }

                let is_async = if self.at("async") && self.text_at(1) == "function" {
                    Some(TokSpan::new(self.bump(), self.pos))
                } else {
                    None
                };

                let mut stmt = if self.at("local") || self.at("const") {
                    let is_const = self.at("const");
                    self.bump();

                    let is_async = if self.at("async") && self.text_at(1) == "function" {
                        Some(TokSpan::new(self.bump(), self.pos))
                    } else {
                        is_async
                    };

                    if self.at("function") {
                        let mut s = self.local_function(start, attributes, is_const)?;

                        if let Stmt::LocalFunction(f) = &mut s {
                            f.body.is_async = is_async;
                            f.attrs = attrs;
                        }

                        s
                    } else {
                        // `@attr local x = 1`: attributes on a local.
                        self.pos -= 1;
                        let mut s = self.local_stmt(start)?;

                        if let Stmt::Local(l) = &mut s {
                            l.attrs = attrs;
                        }

                        s
                    }
                } else {
                    let mut s = self.function_stmt(start, attributes)?;

                    if let Stmt::Function(f) = &mut s {
                        f.body.is_async = is_async;
                        f.attrs = attrs;
                    }

                    s
                };

                if exported {
                    stmt = mark_exported(stmt);
                }

                Ok(stmt)
            }

            "struct"
                if self.name_at(1) && self.text_at(2) == "as"
                    || (self.at("struct") && self.name_at(1) && self.text_at(2) == "<") =>
            {
                self.struct_decl(start, Vec::new(), false)
            }

            "trait" if self.name_at(1) && !self.newline_after(0) => {
                self.trait_decl(start, Vec::new(), false)
            }

            "interface" if self.name_at(1) && !self.newline_after(0) => {
                self.interface_decl(start, false)
            }

            "remote"
                if (self.name_at(1) || self.text_at(1) == "function") && !self.newline_after(0) =>
            {
                self.remote_decl(start, Vec::new(), false)
            }

            "attribute" if self.name_at(1) && !self.newline_after(0) => {
                self.attribute_decl(start, false)
            }

            "macro" if self.name_at(1) && self.text_at(2) == "(" => self.macro_decl(start, false),

            /*
            `export` is contextual, like `type`. It opens a declaration only
            when a declaration follows, so a variable named export keeps
            parsing as an expression.
            */
            "export" if self.text_at(1) == "type" && self.text_at(2) != "{" => {
                self.type_alias(start)
            }

            "export" if self.text_at(1) == "{" => self.export_list(start, false),

            "export" if self.text_at(1) == "type" && self.text_at(2) == "{" => {
                self.bump();
                self.export_list(start, true)
            }

            "export" if self.text_at(1) == "default" && !self.newline_after(1) => {
                self.pos += 2;
                let value = self.expr()?;

                Ok(Stmt::ExportDefault {
                    value,
                    span: TokSpan::new(start, self.pos),
                })
            }

            "export" if self.text_at(1) == "enum" && self.name_at(2) => {
                self.bump();
                self.enum_decl(start, true)
            }

            "export" if self.text_at(1) == "struct" && self.name_at(2) => {
                self.bump();
                self.struct_decl(start, Vec::new(), true)
            }

            "export" if self.text_at(1) == "trait" && self.name_at(2) => {
                self.bump();
                self.trait_decl(start, Vec::new(), true)
            }

            "export" if self.text_at(1) == "interface" && self.name_at(2) => {
                self.bump();
                self.interface_decl(start, true)
            }

            "export" if self.text_at(1) == "remote" => {
                self.bump();
                self.remote_decl(start, Vec::new(), true)
            }

            "export" if self.text_at(1) == "attribute" && self.name_at(2) => {
                self.bump();
                self.attribute_decl(start, true)
            }

            "export" if self.text_at(1) == "macro" && self.name_at(2) => {
                self.bump();
                self.macro_decl(start, true)
            }

            "export" if self.text_at(1) == "impl" && self.name_at(2) => {
                self.bump();
                self.impl_decl(start, true)
            }

            "import" if self.import_follows() => self.import_stmt(start),

            "enum" if self.name_at(1) && self.text_at(2) == "as" => self.enum_decl(start, false),

            "impl" if self.name_at(1) && !self.newline_after(0) => self.impl_decl(start, false),

            "match" if self.match_follows() => self.match_stmt(start),

            "export"
                if matches!(self.text_at(1), "local" | "const" | "function" | "class")
                    || (self.text_at(1) == "open" && self.text_at(2) == "class")
                    || (self.text_at(1) == "async" && self.text_at(2) == "function") =>
            {
                self.bump();

                if self.at("class") || self.at("open") {
                    return self.class_stmt(start, true);
                }

                if self.at("async") {
                    let is_async = Some(TokSpan::new(self.bump(), self.pos));
                    let mut stmt = self.function_stmt(start, Vec::new())?;

                    if let Stmt::Function(f) = &mut stmt {
                        f.body.is_async = is_async;
                    }

                    return Ok(mark_exported(stmt));
                }

                if self.at("function") {
                    return Ok(mark_exported(self.function_stmt(start, Vec::new())?));
                }

                Ok(mark_exported(self.local_stmt(start)?))
            }

            // `class` and `open` are contextual too: a declaration only before a name.
            "class" if self.name_at(1) => self.class_stmt(start, false),

            "open" if self.text_at(1) == "class" && self.name_at(2) => {
                self.class_stmt(start, false)
            }

            "type" if self.type_is_alias() => self.type_alias(start),

            /*

            `declare` is the statement of a definitions file, and it stays

            contextual: `declare = 1` and `declare(x)` are a name in code, and

            only the three declaration forms take the keyword reading.

            */
            "declare"
                if self.options.definitions
                    && (matches!(self.text_at(1), "function" | "class" | "extern")
                        || self.text_at(2) == ":") =>
            {
                self.declare_stmt(start)
            }

            _ => self.expr_stmt(start),
        }
    }

    /*
    One `declare` statement, in its three forms:

    `declare function name<T>(a: T, ...: any): R`
    `declare name: T`
    `declare class Name extends Base ... end`

    A class body holds properties (`name: T`, `["a b"]: T`), methods
    (`function name(self): R`), and indexers (`[T]: U`), each with an
    optional `read` or `write` in front. The tree keeps only the span; a
    declaration is meta code that larvae validates and never rewrites.
    */
    fn declare_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump(); // declare

        match self.text() {
            "function" => {
                self.bump();
                self.expect_name()?;
                self.declare_signature()?;
            }

            /*
            The new solver's spelling: `declare extern type Name with ...
            end`. The members are the members of a class declaration, so
            both forms share the loop below.
            */
            "extern" => {
                self.bump();
                self.expect("type")?;
                self.expect_name()?;

                if self.at("extends") {
                    self.bump();
                    self.expect_name()?;
                }

                self.expect("with")?;
                self.declare_members()?;
                self.expect("end")?;

                return Ok(Stmt::Declare(Declare {
                    span: TokSpan::new(start, self.pos),
                }));
            }

            "class" => {
                self.bump();
                self.expect_name()?;

                if self.at("extends") {
                    self.bump();
                    self.expect_name()?;
                }

                self.declare_members()?;
                self.expect("end")?;

                return Ok(Stmt::Declare(Declare {
                    span: TokSpan::new(start, self.pos),
                }));
            }

            _ => {
                self.expect_name()?;
                self.expect(":")?;
                self.type_()?;
            }
        }

        Ok(Stmt::Declare(Declare {
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// The members of a class or extern type declaration, up to its `end`
    fn declare_members(&mut self) -> Result<(), ParseError> {
        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("this declaration never ends"));
            }

            // A member takes attributes, ex: `@deprecated` above a method.
            if self.at("@") {
                self.attributes()?;
            }

            // The modifier changes nothing about the shape that follows.
            if self.at("read") || self.at("write") {
                self.bump();
            }

            if self.at("function") {
                self.bump();
                self.expect_name()?;
                self.declare_signature()?;
            } else if self.at("[") {
                self.bump();

                // A quoted name is a property; a type is an indexer.
                if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
                    self.bump();
                } else {
                    self.type_()?;
                }

                self.expect("]")?;
                self.expect(":")?;
                self.type_()?;
            } else {
                self.expect_name()?;
                self.expect(":")?;
                self.type_()?;
            }
        }

        Ok(())
    }

    /// The parameter list and return type of a declared function, no body
    fn declare_signature(&mut self) -> Result<(), ParseError> {
        if self.at("<") {
            self.angle_span()?;
        }

        self.expect("(")?;

        while !self.at(")") {
            if self.at_end() {
                return Err(self.err("this parameter list never closes"));
            }

            if self.at("...") {
                self.bump();

                if self.at(":") {
                    self.bump();
                    self.type_()?;
                }

                break;
            }

            self.expect_name()?;

            if self.at(":") {
                self.bump();
                self.type_()?;
            }

            if self.at(",") {
                self.bump();
            } else {
                break;
            }
        }

        self.expect(")")?;

        if self.at(":") {
            self.bump();
            self.type_ret()?;
        }

        Ok(())
    }

    /// `continue` is contextual. It is the keyword only when no token that
    /// would continue an expression follows it.
    pub(super) fn continue_is_keyword(&self) -> bool {
        !matches!(
            self.text_at(1),
            "=" | "," | "." | "(" | "[" | ":" | "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "..="
        )
    }

    /// `type` is also contextual: `type X =`, `type X<`, `type function f`.
    pub(super) fn type_is_alias(&self) -> bool {
        if self.text_at(1) == "function" {
            return true;
        }

        matches!(self.kind_at(1), Some(TokKind::Ident))
            && !is_reserved(self.text_at(1))
            && matches!(self.text_at(2), "=" | "<")
    }

    /// Attributes with parsed arguments: `@name`, `@name(args)`, `@[...]`.
    pub(super) fn attrs(&mut self) -> Result<Vec<Attr>, ParseError> {
        let mut out = Vec::new();

        while self.at("@") {
            let start = self.bump();

            if self.at("[") {
                let mut depth = 0usize;

                loop {
                    if self.at_end() {
                        return Err(self.err("this attribute never closes"));
                    }

                    if self.at("[") {
                        depth += 1;
                    } else if self.at("]") {
                        depth -= 1;

                        if depth == 0 {
                            self.bump();

                            break;
                        }
                    }

                    self.bump();
                }

                out.push(Attr {
                    name: None,
                    args: Vec::new(),
                    span: TokSpan::new(start, self.pos),
                });

                continue;
            }

            let name = self.expect_name()?;
            let mut args = Vec::new();

            // Arguments only when `(` touches the name: `@derive(Eq)`.
            if self.at("(") && self.adjacent_prev() {
                self.bump();

                if !self.at(")") {
                    args = self.expr_list()?;
                }

                self.expect(")")?;
            }

            out.push(Attr {
                name: Some(name),
                args,
                span: TokSpan::new(start, self.pos),
            });
        }

        Ok(out)
    }

    /// Reports if the token at the cursor touches the one before it.
    fn adjacent_prev(&self) -> bool {
        match (
            self.pos.checked_sub(1).and_then(|i| self.toks.get(i)),
            self.toks.get(self.pos),
        ) {
            (Some(a), Some(b)) => a.end == b.start,

            _ => false,
        }
    }

    pub(super) fn attributes(&mut self) -> Result<Vec<TokSpan>, ParseError> {
        let mut out = Vec::new();

        while self.at("@") {
            let start = self.bump();

            /*
            The bracket form of a definitions file, ex:
            `@[deprecated { use = "task.spawn" }]`. The group skips whole
            and balanced; its content is metadata larvae reads past.
            */
            if self.at("[") {
                let mut depth = 0usize;

                loop {
                    if self.at_end() {
                        return Err(self.err("this attribute never closes"));
                    }

                    if self.at("[") {
                        depth += 1;
                    } else if self.at("]") {
                        depth -= 1;

                        if depth == 0 {
                            self.bump();

                            break;
                        }
                    }

                    self.bump();
                }
            } else {
                self.expect_name()?;
            }

            out.push(TokSpan::new(start, self.pos));
        }

        Ok(out)
    }

    pub(super) fn if_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.expect("if")?;

        let mut branches = Vec::new();
        let cond = self.cond()?;

        self.expect("then")?;
        branches.push((cond, self.block()?));

        while self.at("elseif") {
            self.bump();
            let cond = self.cond()?;
            self.expect("then")?;
            branches.push((cond, self.block()?));
        }

        let else_block = if self.eat("else") {
            Some(self.block()?)
        } else {
            None
        };

        self.expect("end")?;
        Ok(Stmt::If(If {
            branches,
            else_block,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn for_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.expect("for")?;
        let first = self.binding()?;

        if self.eat("=") {
            let from = self.expr()?;
            self.expect(",")?;
            let limit = self.expr()?;
            let step = if self.eat(",") {
                Some(self.expr()?)
            } else {
                None
            };

            self.expect("do")?;
            let block = self.block()?;
            self.expect("end")?;

            return Ok(Stmt::NumericFor(NumericFor {
                var: first,
                start: from,
                limit,
                step,
                block,
                span: TokSpan::new(start, self.pos),
            }));
        }

        let mut vars = vec![first];

        while self.eat(",") {
            vars.push(self.binding()?);
        }

        self.expect("in")?;
        let exprs = self.expr_list()?;
        let filter = if self.at("where") && self.infix_word_here() {
            self.bump();

            Some(self.expr()?)
        } else {
            None
        };
        self.expect("do")?;
        let block = self.block()?;
        self.expect("end")?;
        Ok(Stmt::GenericFor(GenericFor {
            vars,
            exprs,
            filter,
            block,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn local_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let is_const = self.at("const");
        // `start` can sit on `export`; the keyword is the token here.
        let keyword_at = self.bump();

        if self.at("function") {
            return self.local_function(start, Vec::new(), is_const);
        }

        if self.at("async") && self.text_at(1) == "function" {
            let is_async = Some(TokSpan::new(self.bump(), self.pos));
            let mut stmt = self.local_function(start, Vec::new(), is_const)?;

            if let Stmt::LocalFunction(f) = &mut stmt {
                f.body.is_async = is_async;
            }

            return Ok(stmt);
        }

        if self.at("@") {
            let attributes = self.attributes()?;

            return self.local_function(start, attributes, is_const);
        }

        // `local Ok(v) = e [else ... end]`: a pattern binding.
        if self.at_name() && self.text_at(1) == "(" && self.pattern_local_follows() {
            let keyword = TokSpan::new(keyword_at, keyword_at + 1);
            let pattern = self.pattern()?;
            self.expect("=")?;
            let value = self.expr()?;
            let else_block = if self.at("else") {
                self.bump();
                let block = self.block()?;
                self.expect("end")?;

                Some(block)
            } else {
                None
            };

            return Ok(Stmt::PatternLocal(PatternLocal {
                keyword,
                pattern,
                value,
                else_block,
                span: TokSpan::new(start, self.pos),
            }));
        }

        let keyword = TokSpan::new(keyword_at, keyword_at + 1);
        let mut names = vec![self.binding()?];

        while self.eat(",") {
            names.push(self.binding()?);
        }

        let values = if self.eat("=") {
            self.expr_list()?
        } else {
            Vec::new()
        };

        Ok(Stmt::Local(Local {
            attrs: Vec::new(),
            keyword,
            exported: false,
            is_const,
            names,
            values,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn local_function(
        &mut self,
        start: usize,
        attributes: Vec<TokSpan>,
        is_const: bool,
    ) -> Result<Stmt, ParseError> {
        self.expect("function")?;

        let name = self.expect_name()?;
        self.reject_reserved(name);
        let body = self.function_body()?;

        Ok(Stmt::LocalFunction(LocalFunction {
            attributes,
            attrs: Vec::new(),
            exported: false,
            is_const,
            name,
            body,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn function_stmt(
        &mut self,
        start: usize,
        attributes: Vec<TokSpan>,
    ) -> Result<Stmt, ParseError> {
        self.expect("function")?;

        let mut path = vec![self.expect_name()?];
        let mut is_method = false;

        // A plain `function new()` binds a global; `Vec2.new` is a field,
        // and so is a method in an `impl` or `trait` body.
        if !self.at(".") && !self.at(":") && self.method_context == 0 {
            self.reject_reserved(path[0]);
        }

        loop {
            if self.eat(".") {
                path.push(self.expect_name()?);
            } else if self.at(":") {
                self.bump();
                path.push(self.expect_name()?);
                is_method = true;
                break;
            } else {
                break;
            }
        }

        let body = self.function_body()?;
        Ok(Stmt::Function(Function {
            attributes,
            attrs: Vec::new(),
            exported: false,
            path,
            is_method,
            body,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn function_body(&mut self) -> Result<FunctionBody, ParseError> {
        let start = self.pos;
        let generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };

        self.expect("(")?;
        let mut params = Vec::new();

        if !self.at(")") {
            loop {
                if self.at("@") {
                    self.attrs()?;
                }

                if self.at("...") {
                    let i = self.bump();
                    let ty = if self.eat(":") {
                        Some(self.type_()?)
                    } else {
                        None
                    };

                    params.push(Param {
                        name: TokSpan::new(i, i + 1),
                        is_vararg: true,
                        ty,
                        default: None,
                        destructure: None,
                    });

                    break;
                }

                let b = self.binding()?;
                let default = if self.eat("=") {
                    Some(self.expr()?)
                } else {
                    None
                };

                params.push(Param {
                    name: b.name,
                    is_vararg: false,
                    ty: b.ty,
                    default,
                    destructure: b.destructure,
                });

                if !self.eat(",") {
                    break;
                }
            }
        }

        self.expect(")")?;
        let (ret_type, ret_arrow) = if self.eat(":") {
            (Some(self.type_ret()?), None)
        } else if self.at("->") {
            let arrow = self.bump();

            (Some(self.type_ret()?), Some(TokSpan::new(arrow, arrow + 1)))
        } else {
            (None, None)
        };

        let block = self.block()?;
        self.expect("end")?;
        // A `:` inside the list is a bound; `<T: Shape>` has no Luau form.
        let has_bounds = generics
            .is_some_and(|g| (g.start..g.end).any(|i| self.toks[i as usize].text(self.src) == ":"));
        Ok(FunctionBody {
            is_async: None,
            generics,
            has_bounds,
            params,
            ret_type,
            ret_arrow,
            block,
            span: TokSpan::new(start, self.pos),
        })
    }

    /*
    `[export] class Name ... end`, per the classes RFC.

    The body holds two member forms. A field is `[public] name [: type]`,
    and a method is an ordinary function with exactly one name. A method
    whose name starts with `__` must be one of the metamethods the RFC
    lists, and everything else with that prefix is a syntax error there.
    Inheritance is deferred in the RFC, so no clause follows the name.
    */
    pub(super) fn class_stmt(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        let open = self.eat("open");
        self.expect("class")?;
        let name = self.expect_name()?;

        // `extends Base`, from the inheritance RFC; an open class allows it.
        let extends = match self.eat("extends") {
            true => Some(self.expect_name()?),

            false => None,
        };

        let mut members = Vec::new();

        loop {
            if self.eat("end") {
                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated class, expected `end`"));
            }

            if self.eat(";") {
                continue;
            }

            if self.at("function") || self.at("@") {
                let m_start = self.pos;
                let attributes = match self.at("@") {
                    true => self.attributes()?,

                    false => Vec::new(),
                };

                // The name sits after `function`; the checks read it there.
                let method_name = self.text_at(1);

                if let Some(bare) = method_name.strip_prefix("__")
                    && !CLASS_METAMETHODS.contains(&bare)
                {
                    return Err(
                        self.err(&format!("__{bare} is not a metamethod a class can define"))
                    );
                }

                if matches!(self.text_at(2), "." | ":") {
                    return Err(self.err("a class method takes one name, without `.` or `:`"));
                }

                self.method_context += 1;
                let parsed = self.function_stmt(m_start, attributes);
                self.method_context -= 1;

                let Stmt::Function(f) = parsed? else {
                    unreachable!("function_stmt parses a function");
                };

                members.push(ClassMember::Method(f));

                continue;
            }

            let m_start = self.pos;
            let public = self.eat("public");
            let field = self.expect_name()?;
            let ty = match self.eat(":") {
                true => Some(self.type_()?),

                false => None,
            };

            members.push(ClassMember::Field {
                public,
                name: field,
                ty,
                span: TokSpan::new(m_start, self.pos),
            });
        }

        Ok(Stmt::Class(Class {
            exported,
            open,
            name,
            extends,
            members,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn binding(&mut self) -> Result<Binding, ParseError> {
        let start = self.pos;

        let destructure = if self.at("{") {
            self.bump();
            let mut fields = Vec::new();

            while !self.at("}") {
                if self.at_end() {
                    return Err(self.err("unterminated destructuring pattern"));
                }

                let field = self.expect_name()?;
                let rename = if self.eat("=") {
                    Some(self.expect_name()?)
                } else {
                    None
                };
                fields.push(FieldBinding { field, rename });

                if !self.eat(",") {
                    break;
                }
            }

            self.expect("}")?;

            Some(Destructure::Table(fields))
        } else if self.at("[") {
            self.bump();
            let mut items = Vec::new();
            let mut rest = None;

            while !self.at("]") {
                if self.at_end() {
                    return Err(self.err("unterminated destructuring pattern"));
                }

                if self.eat("...") {
                    rest = Some(self.expect_name()?);

                    break;
                }

                items.push(self.expect_name()?);

                if !self.eat(",") {
                    break;
                }
            }

            self.expect("]")?;

            Some(Destructure::Array { items, rest })
        } else {
            None
        };

        let name = match destructure {
            Some(_) => TokSpan::new(start, self.pos),

            None => {
                let n = self.expect_name()?;
                self.reject_reserved(n);

                n
            }
        };

        let ty = if self.eat(":") {
            Some(self.type_()?)
        } else {
            None
        };

        Ok(Binding {
            name,
            ty,
            destructure,
        })
    }

    pub(super) fn type_alias(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let exported = self.eat("export");
        self.expect("type")?;

        if self.at("function") {
            // `type function f() ... end` is a user-defined type function.
            self.bump();
            let name = self.expect_name()?;
            self.function_body()?;

            return Ok(Stmt::TypeAlias(TypeAlias {
                exported,
                name,
                span: TokSpan::new(start, self.pos),
            }));
        }

        let name = self.expect_name()?;

        if self.at("<") {
            self.angle_span()?;
        }

        self.expect("=")?;
        self.type_()?;
        Ok(Stmt::TypeAlias(TypeAlias {
            exported,
            name,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn expr_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let first = self.suffixed_expr()?;
        // This is an assignment, in the plain or the compound form.
        if self.at("=") || self.at(",") || self.compound_op_at().is_some() {
            let mut targets = vec![first];

            while self.eat(",") {
                targets.push(self.suffixed_expr()?);
            }

            let (op_idx, width) = if let Some(width) = self.compound_op_at() {
                let i = self.pos;
                self.pos += width;

                (i, width)
            } else {
                (self.expect("=")?, 1)
            };

            let values = self.expr_list()?;

            return Ok(Stmt::Assign(Assign {
                targets,
                op: TokSpan::new(op_idx, op_idx + width),
                values,
                span: TokSpan::new(start, self.pos),
            }));
        }

        match &first {
            Expr::Call { .. } => Ok(Stmt::Call(first, TokSpan::new(start, self.pos))),

            _ => Err(self.err("this expression is not a statement")),
        }
    }

    // --- conditions with bindings -----------------------------------------

    /// `if local x = e`, `if not local x = e`, stacked with `;`, with an
    /// optional `where` clause; or a plain expression.
    pub(super) fn cond(&mut self) -> Result<Cond, ParseError> {
        let start = self.pos;
        let negated = self.at("not") && matches!(self.text_at(1), "local" | "const");

        if !negated && !matches!(self.text(), "local" | "const") {
            return Ok(Cond::Expr(self.expr()?));
        }

        if negated {
            self.bump();
        }

        let mut bindings = Vec::new();

        loop {
            let is_const = self.at("const");

            if !matches!(self.text(), "local" | "const") {
                return Err(self.err(&format!(
                    "expected `local` or `const` in the condition, found {}",
                    self.found()
                )));
            }

            self.bump();
            let pattern = self.pattern()?;
            let ty = if self.eat(":") {
                Some(self.type_()?)
            } else {
                None
            };
            self.expect("=")?;
            let value = self.expr()?;
            bindings.push(CondBinding {
                is_const,
                pattern,
                ty,
                value,
            });

            if self.at(";") && matches!(self.text_at(1), "local" | "const") {
                self.bump();

                continue;
            }

            break;
        }

        let filter = if self.at("where") && self.infix_word_here() {
            self.bump();

            Some(self.expr()?)
        } else {
            None
        };

        Ok(Cond::Local {
            negated,
            bindings,
            filter,
            span: TokSpan::new(start, self.pos),
        })
    }

    // --- patterns ----------------------------------------------------------

    /// Reports if `local Name(` starts a pattern local, not a call.
    fn pattern_local_follows(&self) -> bool {
        // `local f(x)` is not valid Luau, so a name and `(` after `local`
        // can only be a pattern.
        true
    }

    pub(super) fn pattern(&mut self) -> Result<Pattern, ParseError> {
        let start = self.pos;
        let mut left = self.pattern_primary()?;

        while self.at("or") {
            self.bump();
            let right = self.pattern_primary()?;
            left = Pattern::Or(
                Box::new(left),
                Box::new(right),
                TokSpan::new(start, self.pos),
            );
        }

        Ok(left)
    }

    fn pattern_primary(&mut self) -> Result<Pattern, ParseError> {
        let start = self.pos;

        match self.text() {
            "_" => {
                self.bump();

                Ok(Pattern::Wildcard(TokSpan::new(start, self.pos)))
            }

            "nil" | "true" | "false" => {
                let e = self.simple_expr()?;

                Ok(Pattern::Literal(Box::new(e)))
            }

            "-" => {
                let e = self.sub_expr(UNARY_PRIORITY)?;

                Ok(Pattern::Literal(Box::new(e)))
            }

            "{" => self.struct_pattern(None, start),

            "[" => {
                self.bump();
                let mut items = Vec::new();
                let mut rest = None;

                while !self.at("]") {
                    if self.at_end() {
                        return Err(self.err("unterminated array pattern"));
                    }

                    if self.eat("...") {
                        rest = Some(self.expect_name()?);

                        break;
                    }

                    items.push(self.pattern()?);

                    if !self.eat(",") {
                        break;
                    }
                }

                self.expect("]")?;

                Ok(Pattern::Array {
                    items,
                    rest,
                    span: TokSpan::new(start, self.pos),
                })
            }

            _ => match self.kind_at(0) {
                Some(TokKind::Number)
                | Some(TokKind::Str { .. })
                | Some(TokKind::InterpStr | TokKind::InterpHead) => {
                    let e = self.simple_expr()?;

                    Ok(Pattern::Literal(Box::new(e)))
                }

                Some(TokKind::Ident) if self.at_name() => {
                    let name_start = self.bump();

                    // A dotted path compares by value.
                    if self.at(".") && self.name_at(1) {
                        while self.at(".") && self.name_at(1) {
                            self.pos += 2;
                        }

                        return Ok(Pattern::Path(TokSpan::new(name_start, self.pos)));
                    }

                    let name = TokSpan::new(name_start, name_start + 1);

                    if self.at("(") {
                        self.bump();
                        let mut args = Vec::new();

                        while !self.at(")") {
                            if self.at_end() {
                                return Err(self.err("unterminated variant pattern"));
                            }

                            args.push(self.pattern()?);

                            if !self.eat(",") {
                                break;
                            }
                        }

                        self.expect(")")?;

                        return Ok(Pattern::Variant {
                            name,
                            args,
                            span: TokSpan::new(start, self.pos),
                        });
                    }

                    if self.at("{") {
                        return self.struct_pattern(Some(name), start);
                    }

                    Ok(Pattern::Bind(name))
                }

                _ => Err(self.err(&format!("expected a pattern, found {}", self.found()))),
            },
        }
    }

    fn struct_pattern(
        &mut self,
        name: Option<TokSpan>,
        start: usize,
    ) -> Result<Pattern, ParseError> {
        self.expect("{")?;
        let mut fields = Vec::new();

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated struct pattern"));
            }

            let field = self.expect_name()?;
            let pattern = if self.eat("=") {
                Some(self.pattern()?)
            } else {
                None
            };
            fields.push(FieldPattern { field, pattern });

            if !self.eat(",") {
                break;
            }
        }

        self.expect("}")?;

        Ok(Pattern::Struct {
            name,
            fields,
            span: TokSpan::new(start, self.pos),
        })
    }

    // --- match -------------------------------------------------------------

    /// `match` is a keyword when an expression that is not a call shape
    /// follows on the same line. `match(s, p)` and `match "x"` stay calls.
    pub(super) fn match_follows(&self) -> bool {
        if self.newline_after(0) {
            return false;
        }

        match self.kind_at(1) {
            Some(TokKind::LParen)
            | Some(TokKind::Str { .. })
            | Some(TokKind::InterpStr | TokKind::InterpHead) => false,

            Some(TokKind::Ident) => {
                !is_reserved(self.text_at(1))
                    || matches!(self.text_at(1), "nil" | "true" | "false" | "not")
            }

            Some(TokKind::Number) => true,

            _ => matches!(self.text_at(1), "-" | "#" | "{" | "["),
        }
    }

    fn match_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump();
        let scrutinees = self.expr_list()?;
        self.expect("with")?;
        let mut arms = Vec::new();
        let mut default = None;

        loop {
            if self.at("end") {
                self.bump();

                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated match, expected `end`"));
            }

            if self.at("case") {
                let arm_start = self.bump();
                let (patterns, guard) = self.arm_head()?;
                self.expect("then")?;
                let block = self.match_block()?;
                arms.push(MatchArm {
                    patterns,
                    guard,
                    block,
                    span: TokSpan::new(arm_start, self.pos),
                });

                continue;
            }

            if self.at("default") {
                self.bump();
                default = Some(self.match_block()?);

                continue;
            }

            return Err(self.err(&format!(
                "expected `case`, `default`, or `end`, found {}",
                self.found()
            )));
        }

        Ok(Stmt::Match(MatchStmt {
            scrutinees,
            arms,
            default,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// The patterns of an arm, one per scrutinee, and the `and` guard.
    fn arm_head(&mut self) -> Result<(Vec<Pattern>, Option<Expr>), ParseError> {
        let mut patterns = vec![self.pattern()?];

        while self.eat(",") {
            patterns.push(self.pattern()?);
        }

        let guard = if self.eat("and") {
            Some(self.expr()?)
        } else {
            None
        };

        Ok((patterns, guard))
    }

    /// A block that ends at the next `case`, `default`, or `end`.
    fn match_block(&mut self) -> Result<Block, ParseError> {
        self.in_match_arm += 1;
        let r = self.block();
        self.in_match_arm -= 1;

        r
    }

    /// The expression form: each arm is one expression.
    pub(super) fn match_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.bump();
        let scrutinees = self.expr_list()?;
        self.expect("with")?;
        let mut arms = Vec::new();
        let mut default = None;

        loop {
            if self.at("end") {
                self.bump();

                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated match, expected `end`"));
            }

            if self.at("case") {
                let arm_start = self.bump();
                let (patterns, guard) = self.arm_head()?;
                self.expect("then")?;
                let value = self.expr()?;
                arms.push(MatchExprArm {
                    patterns,
                    guard,
                    value,
                    span: TokSpan::new(arm_start, self.pos),
                });

                continue;
            }

            if self.at("default") {
                self.bump();
                default = Some(Box::new(self.expr()?));

                continue;
            }

            return Err(self.err(&format!(
                "expected `case`, `default`, or `end`, found {}",
                self.found()
            )));
        }

        Ok(Expr::Match(Box::new(MatchExpr {
            scrutinees,
            arms,
            default,
            span: TokSpan::new(start, self.pos),
        })))
    }

    // --- modules -----------------------------------------------------------

    /// `import` is a keyword before `*`, `{`, `type {`, or `Name from`.
    fn import_follows(&self) -> bool {
        match self.text_at(1) {
            "*" | "{" => true,

            "type" => self.text_at(2) == "{",

            _ => self.name_at(1) && self.text_at(2) == "from",
        }
    }

    fn import_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump();

        let kind = if self.eat("*") {
            self.expect("as")?;

            ImportKind::Namespace(self.expect_name()?)
        } else if self.at("type") && self.text_at(1) == "{" {
            self.bump();
            let mut specs = self.import_specs()?;

            for s in &mut specs {
                s.is_type = true;
            }

            ImportKind::TypeOnly(specs)
        } else if self.at("{") {
            ImportKind::Named(self.import_specs()?)
        } else {
            ImportKind::Namespace(self.expect_name()?)
        };

        self.expect("from")?;
        let path = self.string_token()?;

        Ok(Stmt::Import(Import {
            kind,
            path,
            span: TokSpan::new(start, self.pos),
        }))
    }

    fn import_specs(&mut self) -> Result<Vec<ImportSpec>, ParseError> {
        self.expect("{")?;
        let mut specs = Vec::new();

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated import list"));
            }

            let is_type = self.at("type") && self.name_at(1);

            if is_type {
                self.bump();
            }

            let name = self.expect_name()?;
            let alias = if self.eat("as") {
                Some(self.expect_name()?)
            } else {
                None
            };
            specs.push(ImportSpec {
                name,
                alias,
                is_type,
            });

            if !self.eat(",") {
                break;
            }
        }

        self.expect("}")?;

        Ok(specs)
    }

    fn string_token(&mut self) -> Result<TokSpan, ParseError> {
        if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
            let i = self.bump();

            return Ok(TokSpan::new(i, i + 1));
        }

        Err(self.err(&format!(
            "expected a module path string, found {}",
            self.found()
        )))
    }

    fn export_list(&mut self, start: usize, type_only: bool) -> Result<Stmt, ParseError> {
        self.bump();
        let specs = self.import_specs()?;
        let from = if self.eat("from") {
            Some(self.string_token()?)
        } else {
            None
        };

        Ok(Stmt::ExportList(ExportList {
            specs,
            from,
            type_only,
            span: TokSpan::new(start, self.pos),
        }))
    }

    // --- struct, trait, interface, remote, attribute, macro ----------------

    /// The fields of a struct or interface, up to `end`.
    fn fields(&mut self) -> Result<Vec<Field>, ParseError> {
        let mut fields = Vec::new();

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated declaration, expected `end`"));
            }

            let f_start = self.pos;
            let attributes = if self.at("@") {
                self.attrs()?
            } else {
                Vec::new()
            };
            let modifier = if matches!(self.text(), "read" | "write") && self.name_at(1) {
                let i = self.bump();

                Some(TokSpan::new(i, i + 1))
            } else {
                None
            };
            let name = self.expect_name()?;
            self.expect(":")?;
            let ty = self.type_()?;
            let default = if self.eat("=") {
                Some(self.expr()?)
            } else {
                None
            };
            fields.push(Field {
                attributes,
                modifier,
                name,
                ty,
                default,
                span: TokSpan::new(f_start, self.pos),
            });

            let _ = self.eat(",") || self.eat(";");
        }

        Ok(fields)
    }

    fn struct_decl(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        self.expect("struct")?;
        let name = self.expect_name()?;
        let generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };
        self.expect("as")?;
        let fields = self.fields()?;
        self.expect("end")?;

        Ok(Stmt::Struct(StructDecl {
            attributes,
            exported,
            name,
            generics,
            fields,
            span: TokSpan::new(start, self.pos),
        }))
    }

    fn interface_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        self.expect("interface")?;
        let name = self.expect_name()?;
        let generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };
        let mut extends = Vec::new();

        if self.eat("extends") {
            extends.push(self.expect_name()?);

            while self.eat(",") {
                extends.push(self.expect_name()?);
            }
        }

        self.expect("as")?;
        let fields = self.fields()?;
        self.expect("end")?;

        Ok(Stmt::Interface(InterfaceDecl {
            exported,
            name,
            generics,
            extends,
            fields,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /*
    A trait holds signatures. A signature followed by a statement is a
    default method: a body exists iff the next token is not `function`,
    `end`, `@`, or the end of the file. An empty default is `do end`.
    */
    fn trait_decl(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        self.expect("trait")?;
        let name = self.expect_name()?;
        let mut methods = Vec::new();

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated trait, expected `end`"));
            }

            let m_start = self.pos;
            self.expect("function")?;
            let mname = self.expect_name()?;
            let sig_start = self.pos;
            let params = self.param_list()?;
            let mut ret = None;

            if self.eat(":") || self.eat("->") {
                ret = Some(self.type_ret()?);
            }

            let _ = ret;
            let signature = TokSpan::new(sig_start, self.pos);

            let has_body = !self.at_end() && !matches!(self.text(), "function" | "end" | "@");
            let body = if has_body {
                let b_start = self.pos;
                let block = self.block()?;
                self.expect("end")?;

                Some(FunctionBody {
                    is_async: None,
                    generics: None,
                    has_bounds: false,
                    params: Vec::new(),
                    ret_type: None,
                    ret_arrow: None,
                    block,
                    span: TokSpan::new(b_start, self.pos),
                })
            } else {
                None
            };

            methods.push(TraitMethod {
                name: mname,
                signature,
                params,
                body,
                span: TokSpan::new(m_start, self.pos),
            });
        }

        self.expect("end")?;

        Ok(Stmt::Trait(TraitDecl {
            attributes,
            exported,
            name,
            methods,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// `(a: T = 1, @u8 b: number, ...)` with attributes on parameters.
    fn param_list(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect("(")?;
        let mut params = Vec::new();

        if !self.at(")") {
            loop {
                if self.at("@") {
                    self.attrs()?;
                }

                if self.at("...") {
                    let i = self.bump();
                    let ty = if self.eat(":") {
                        Some(self.type_()?)
                    } else {
                        None
                    };
                    params.push(Param {
                        name: TokSpan::new(i, i + 1),
                        is_vararg: true,
                        ty,
                        default: None,
                        destructure: None,
                    });

                    break;
                }

                let b = self.binding()?;
                let default = if self.eat("=") {
                    Some(self.expr()?)
                } else {
                    None
                };
                params.push(Param {
                    name: b.name,
                    is_vararg: false,
                    ty: b.ty,
                    default,
                    destructure: b.destructure,
                });

                if !self.eat(",") {
                    break;
                }
            }
        }

        self.expect(")")?;

        Ok(params)
    }

    fn remote_decl(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        self.expect("remote")?;
        let is_function = self.eat("function");
        let name = self.expect_name()?;
        let params = self.param_list()?;
        let ret_type = if self.eat("->") || self.eat(":") {
            Some(self.type_ret()?)
        } else {
            None
        };
        self.expect("from")?;
        let mut from_client = false;
        let mut from_server = false;

        loop {
            match self.text() {
                "client" => from_client = true,

                "server" => from_server = true,

                _ => return Err(self.err("expected `client` or `server` after `from`")),
            }

            self.bump();

            if !self.eat("or") {
                break;
            }
        }

        Ok(Stmt::Remote(RemoteDecl {
            attributes,
            exported,
            is_function,
            name,
            params,
            ret_type,
            from_client,
            from_server,
            span: TokSpan::new(start, self.pos),
        }))
    }

    fn attribute_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        self.expect("attribute")?;
        let name = self.expect_name()?;
        let params = if self.at("(") {
            self.param_list()?
        } else {
            Vec::new()
        };
        self.expect("on")?;
        let mut targets = vec![self.target_word()?];

        while self.eat(",") {
            targets.push(self.target_word()?);
        }

        Ok(Stmt::Attribute(AttributeDecl {
            exported,
            name,
            params,
            targets,
            span: TokSpan::new(start, self.pos),
        }))
    }

    fn target_word(&mut self) -> Result<TokSpan, ParseError> {
        if matches!(
            self.text(),
            "function"
                | "struct"
                | "enum"
                | "variant"
                | "field"
                | "param"
                | "remote"
                | "interface"
                | "type"
                | "local"
        ) {
            let i = self.bump();

            return Ok(TokSpan::new(i, i + 1));
        }

        Err(self.err(&format!(
            "expected an attribute target, found {}",
            self.found()
        )))
    }

    /// `macro name(params) {stat} [exp] end`.
    fn macro_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        self.expect("macro")?;
        let name = self.expect_name()?;
        let params = self.param_list()?;
        let b_start = self.pos;
        let mut stmts = Vec::new();
        let mut tail = None;

        while !self.at_end() && !self.at("end") {
            // A trailing expression: try a statement first; when the
            // statement parser refuses, the rest is the tail.
            let save = self.pos;

            match self.stmt() {
                Ok(s) => stmts.push(s),

                Err(e) => {
                    self.pos = save;

                    let t = self.expr().map_err(|_| e)?;

                    if !self.at("end") {
                        return Err(self.err("a macro's trailing expression must be last"));
                    }

                    tail = Some(t);
                }
            }
        }

        let body = Block {
            stmts,
            span: TokSpan::new(
                b_start,
                tail.as_ref()
                    .map(|t| t.span().start as usize)
                    .unwrap_or(self.pos),
            ),
        };
        self.expect("end")?;

        Ok(Stmt::Macro(MacroDecl {
            exported,
            name,
            params,
            body,
            tail,
            span: TokSpan::new(start, self.pos),
        }))
    }

    // --- enum and impl -----------------------------------------------------

    fn enum_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        self.expect("enum")?;
        let name = self.expect_name()?;
        self.expect("as")?;
        let mut variants = Vec::new();

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated enum, expected `end`"));
            }

            let v_start = self.pos;

            if self.at("@") {
                self.attrs()?;
            }

            let vname = self.expect_name()?;
            let mut payload = Vec::new();

            if self.eat("(") {
                while !self.at(")") {
                    payload.push(self.type_()?);

                    if !self.eat(",") {
                        break;
                    }
                }

                self.expect(")")?;
            }

            let value = if self.eat("=") {
                Some(self.expr()?)
            } else {
                None
            };

            variants.push(Variant {
                name: vname,
                payload,
                value,
                span: TokSpan::new(v_start, self.pos),
            });

            let _ = self.eat(",") || self.eat(";");
        }

        self.expect("end")?;

        Ok(Stmt::Enum(EnumDecl {
            attributes: Vec::new(),
            exported,
            name,
            variants,
            span: TokSpan::new(start, self.pos),
        }))
    }

    fn impl_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        self.expect("impl")?;
        let first_start = self.pos;
        self.expect_name()?;

        while self.at(".") && self.name_at(1) {
            self.pos += 2;
        }

        let first = TokSpan::new(first_start, self.pos);

        let (trait_name, target) = if self.eat("for") {
            let t_start = self.pos;
            self.expect_name()?;

            while self.at(".") && self.name_at(1) {
                self.pos += 2;
            }

            (Some(first), TokSpan::new(t_start, self.pos))
        } else {
            (None, first)
        };

        let mut methods = Vec::new();

        loop {
            if self.eat("end") {
                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated impl, expected `end`"));
            }

            if self.eat(";") {
                continue;
            }

            let m_start = self.pos;
            let attributes = if self.at("@") {
                self.attributes()?
            } else {
                Vec::new()
            };

            let is_async = if self.at("async") && self.text_at(1) == "function" {
                Some(TokSpan::new(self.bump(), self.pos))
            } else {
                None
            };

            self.method_context += 1;
            let parsed = self.function_stmt(m_start, attributes);
            self.method_context -= 1;

            let Stmt::Function(mut f) = parsed? else {
                unreachable!("function_stmt parses a function");
            };
            f.body.is_async = is_async;
            methods.push(f);
        }

        Ok(Stmt::Impl(ImplDecl {
            exported,
            trait_name,
            target,
            methods,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.expr()?];

        while self.eat(",") {
            out.push(self.expr()?);
        }

        Ok(out)
    }
}

/// The metamethods a class can define: the classes RFC list, and `__init`
/// from the constructors RFC that followed it
const CLASS_METAMETHODS: [&str; 17] = [
    "add", "sub", "mul", "div", "mod", "pow", "tostring", "eq", "lt", "le", "iter", "len", "idiv",
    "concat", "unm", "call", "init",
];

/// The statement with its export flag set; `export` reads the same three ways
fn mark_exported(stmt: Stmt) -> Stmt {
    match stmt {
        Stmt::Local(mut n) => {
            n.exported = true;

            Stmt::Local(n)
        }

        Stmt::Function(mut n) => {
            n.exported = true;

            Stmt::Function(n)
        }

        Stmt::LocalFunction(mut n) => {
            n.exported = true;

            Stmt::LocalFunction(n)
        }

        other => other,
    }
}
