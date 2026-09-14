//! Statements, blocks, and the declaration forms.

mod attributes;
mod control_flow;
mod declarations;
mod enum_impl;
mod imports;
mod matching;
mod namespaces;
mod patterns;
mod structs;

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
            let terminator = if self.at("return") {
                Some("return")
            } else if self.at("break") {
                Some("break")
            } else if self.at("continue") && self.continue_is_keyword() {
                Some("continue")
            } else {
                None
            };
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

            if terminator.is_some() {
                // A `;` belongs to the statement that ends the block.
                if self.at(";") {
                    let i = self.bump();
                    stmts.push(Stmt::Empty(TokSpan::new(i, i + 1)));
                }

                if self.at_end() || self.at_block_end() {
                    break;
                }

                // A statement under a `return`, a `break`, or a
                // `continue` never runs. The block keeps it so the file
                // still parses; `unreachable_code` names it and the emit
                // drops it, since Luau rejects it.
                //
                // A statement that starts left of the terminator is the
                // other case: the block is missing its `end`. Closing
                // here reports that against the keyword that opened it.
                if self.column_at(self.pos) < self.column_at(stmt_start) {
                    break;
                }
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

    /// Reports if a declaration follows the `const` at the cursor. The
    /// rule lives in [`crate::contextual`], so the formatter and the
    /// server read it too.
    pub(in super::super) fn const_decl_follows(&self) -> bool {
        crate::contextual::const_decl_follows(self.src, self.toks, self.pos)
    }

    /// Reports if a delay follows the `after` at the cursor, so the word
    /// opens the statement instead of naming a local. The rule lives in
    /// [`crate::contextual`], so the formatter and the server read it too.
    pub(in super::super) fn after_delay_follows(&self) -> bool {
        crate::contextual::after_delay_follows(self.src, self.toks, self.pos)
    }

    /*
    Reports if the cursor stands outside a body whose `end` is missing.

    A body with no `end` would otherwise read the rest of the file as its
    own members. A keyword that opens a statement is the file going on, at
    any column: a member of an enum, a struct, or an interface never starts
    with one, so an indented `local` after the last member is a statement,
    not a member. The caller breaks out and reports the missing `end` once,
    through `expect_end`. A trait holds `function` members, so its reader
    adds the column test that tells a member from the file going on.
    */
    fn body_ends_early(&self) -> bool {
        self.opens_statement()
    }

    /// Reports if the token at the cursor is a keyword that begins a statement.
    fn opens_statement(&self) -> bool {
        matches!(
            self.text(),
            "local" | "if" | "while" | "for" | "repeat" | "do" | "function" | "return" | "break"
        ) || (self.at("continue") && self.continue_is_keyword())
            || (self.at("const") && self.const_decl_follows())
            || (self.at("async") && matches!(self.text_at(1), "function" | "do"))
            || (self.at("delete") && self.name_at(1))
            || (self.at("destroy") && self.name_at(1))
            || (self.at("after") && self.after_delay_follows())
            || (self.at("import") && self.import_follows())
            || (self.at("enum") && self.name_at(1) && self.text_at(2) == "as")
            || (self.at("impl") && self.name_at(1))
            || (self.at("match") && self.match_follows())
            || (matches!(
                self.text(),
                "struct" | "trait" | "interface" | "remote" | "attribute" | "macro"
            ) && self.name_at(1))
            || (self.at("namespace") && self.namespace_follows())
            || (self.at("export")
                && matches!(self.text_at(1), "local" | "const" | "function" | "type"))
            || (self.at("global") && self.global_follows())
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
                self.expect_end(start)?;
                Ok(Stmt::While(While {
                    cond,
                    block,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "do" => {
                self.bump();
                let block = self.block()?;
                self.expect_end(start)?;
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

            // `async do ... end` alone: the block starts on a thread of
            // its own and the Future it gives back is dropped, the way a
            // call's value is. `local f = async do ... end` keeps it.
            "async" if self.text_at(1) == "do" && !self.newline_after(0) => {
                let e = self.expr()?;

                Ok(Stmt::Call(e, TokSpan::new(start, self.pos)))
            }

            // `$name(args)` as a statement: an intrinsic or macro call.
            "$" => {
                let call = self.simple_expr()?;

                Ok(Stmt::Call(call, TokSpan::new(start, self.pos)))
            }

            // `delete` is reserved, so anything an expression can start
            // with follows it: a string or a table gets the same message
            // a number does.
            "delete" if !self.newline_after(0) && self.delete_operand_at(1) => {
                self.bump();
                let expr = self.suffixed_expr()?;

                Ok(Stmt::Delete {
                    expr,
                    span: TokSpan::new(start, self.pos),
                })
            }

            // `destroy x` and `destroy x after n`. The operand rule is
            // `delete`'s, so a string or a number gets the same message.
            "destroy" if !self.newline_after(0) && self.delete_operand_at(1) => {
                self.bump();
                let expr = self.suffixed_expr()?;
                let delay = match self.at("after") && self.infix_word_here() {
                    true => {
                        self.bump();

                        Some(self.expr()?)
                    }

                    false => None,
                };

                Ok(Stmt::Destroy {
                    expr,
                    delay,
                    span: TokSpan::new(start, self.pos),
                })
            }

            // `after n do ... end`, with `where` between the two. The
            // delay guard keeps `after = 1` and `after(x)` as the local
            // a file may name after.
            "after" if self.after_delay_follows() => {
                self.bump();
                let delay = self.expr()?;
                let filter = match self.at("where") && self.infix_word_here() {
                    true => {
                        self.bump();

                        Some(self.expr()?)
                    }

                    false => None,
                };
                self.expect("do")?;
                let block = self.block()?;
                self.expect_end(start)?;

                Ok(Stmt::After(After {
                    delay,
                    filter,
                    block,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "local" => self.local_stmt(start),

            // `const` declares only where a declaration follows. Luau's own
            // `const LIMIT = 5` keeps its reading, and `local const = 1`
            // then `const = const + 1` keeps the name.
            "const" if self.const_decl_follows() => self.local_stmt(start),

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

                // A definitions file decorates declarations the same way.
                if self.options.definitions && self.at("declare") {
                    return self.declare_stmt(start);
                }

                self.attributed_stmt(start, attrs)
            }

            /*
            `struct S { n: number }` writes another language's body. Alloy
            opens a body with `as`, and this arm comes before the readers
            of the same keywords, so each one reports its own form. The
            brace group goes with the report, so its members draw no
            second one.
            */
            "struct" | "enum" | "trait" | "interface" | "namespace" | "impl"
                if self.name_at(1) && self.text_at(2) == "{" =>
            {
                let word = self.text();
                let article = match word {
                    "enum" | "interface" | "impl" => "an",

                    _ => "a",
                };
                let message = format!(
                    "{article} {word} body is `as ... end`: `{word} {} as`",
                    self.text_at(1)
                );
                self.pos += 2;
                let at = self.toks[self.pos].start as usize;
                self.report_at(at, &message);
                self.skip_braces();

                Ok(Stmt::Error(TokSpan::new(start, self.pos)))
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

            "namespace" if self.namespace_follows() => {
                self.namespace_decl(start, Vec::new(), false)
            }

            /*
            `global` is contextual, like `export`. It opens a declaration
            only when a declaration follows, so a variable named global
            keeps parsing as an expression.
            */
            "global" if self.global_follows() => self.global_stmt(start),

            // `export global local x = 1` reports the removal too.
            "export" if self.text_at(1) == "global" => {
                self.bump();
                self.global_stmt(start)
            }

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

                // A declaration parses as itself, from its own keyword,
                // so it renders as a plain declaration and the module
                // exports the name it binds.
                if self.default_decl_follows() {
                    let decl = self.stmt()?;

                    return Ok(Stmt::ExportDefault {
                        value: DefaultExport::Decl(Box::new(decl)),
                        span: TokSpan::new(start, self.pos),
                    });
                }

                let value = self.expr()?;

                Ok(Stmt::ExportDefault {
                    value: DefaultExport::Value(value),
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

            "export" if self.text_at(1) == "namespace" && self.name_at(2) => {
                self.bump();
                self.namespace_decl(start, Vec::new(), true)
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

            /*
            `// text` is another language's comment. Luau opens one with
            `--`, and the lexer reads `//` as floor division, so the
            report names the form instead of the token.
            */
            "//" => Err(self.err("a comment starts with `--`")),

            /*
            `declare namespace`, `declare enum`, `declare struct`: Luau's
            definition syntax has no form for an Alloy declaration. The
            keyword that follows is the mistake, and recovery reads the
            declaration itself, so the report stands alone.
            */
            "declare" if self.options.definitions && not_declared(self.text_at(1)).is_some() => {
                let noun = not_declared(self.text_at(1)).unwrap_or_default();
                self.bump();
                let at = self.toks[self.pos].start as usize;
                self.report_at(
                    at,
                    &format!(
                        "`declare` takes a function, a name with a type, an extern type, or a class; {noun} is not declared"
                    ),
                );
                self.skip_declaration(start)?;

                Ok(Stmt::Error(TokSpan::new(start, self.pos)))
            }

            _ => self.expr_stmt(start),
        }
    }

    /*
    Moves past a declaration the parser does not read, to the `end` that
    closes it.

    Statement recovery reads one token and stops at the next line, which
    leaves the body to report again, once per member. A body sits in from
    the keyword that opened it, so the first `end` at or left of that
    column is the one that closes the declaration, and a keyword that
    opens a statement there is the file going on. `expect_end` then
    reports a missing `end` once, the way every body reader does.
    */
    fn skip_declaration(&mut self, opener: usize) -> Result<(), ParseError> {
        let column = self.column_at(opener);

        while !self.at_end() {
            if self.column_at(self.pos) <= column && (self.at("end") || self.body_ends_early()) {
                break;
            }

            self.bump();
        }

        self.expect_end(opener)?;

        Ok(())
    }

    /// Moves past the brace group at the cursor, balanced.
    fn skip_braces(&mut self) {
        let mut depth = 0usize;

        while !self.at_end() {
            match self.text() {
                "{" => depth += 1,

                "}" => depth -= 1,

                _ => {}
            }

            self.bump();

            if depth == 0 {
                break;
            }
        }
    }
}

/// The noun for a declaration word `declare` does not take. Luau's
/// definition syntax holds a function, a name with a type, an extern
/// type, and a class, and nothing else.
fn not_declared(word: &str) -> Option<&'static str> {
    Some(match word {
        "namespace" => "a namespace",

        "enum" => "an enum",

        "struct" => "a struct",

        "trait" => "a trait",

        "interface" => "an interface",

        _ => return None,
    })
}

impl<'a> Parser<'a> {
    /// A declaration under `@attr` lines, from the `@` at `start`. The
    /// namespace member loop reads the attributes itself, so the two
    /// callers share one dispatch.
    pub(super) fn attributed_stmt(
        &mut self,
        start: usize,
        attrs: Vec<Attr>,
    ) -> Result<Stmt, ParseError> {
        let attributes: Vec<TokSpan> = attrs.iter().map(|a| a.span).collect();

        // `@attr global function f()` reads the way
        // `@attr export function f()` does. `global` is removed, so the
        // word reports and the declaration exports.
        let was_global = self.at("global") && self.global_follows();

        if was_global {
            let at = self.bump();
            self.removed_global(at);
        }

        let exported = self.eat("export") || was_global;

        match self.text() {
            "struct" if self.name_at(1) => {
                return self.struct_decl(start, attrs, exported);
            }

            "enum" if self.name_at(1) => {
                let mut stmt = self.enum_decl(start, exported)?;

                if let Stmt::Enum(e) = &mut stmt {
                    e.attributes = attrs;
                }

                return Ok(stmt);
            }

            "trait" if self.name_at(1) => {
                return self.trait_decl(start, attrs, exported);
            }

            "remote" if self.name_at(1) || self.text_at(1) == "function" => {
                return self.remote_decl(start, attrs, exported);
            }

            "impl" if self.name_at(1) => {
                return self.impl_decl_with(start, attrs, exported);
            }

            "interface" if self.name_at(1) => {
                return self.interface_decl_with(start, attrs, exported);
            }

            "namespace" if self.namespace_follows() => {
                return self.namespace_decl(start, attrs, exported);
            }

            // `attribute X on type` declares one, so a type alias takes
            // attributes the way every other declaration does. The
            // modifier is already read, so the flags go on by hand.
            "type" if self.name_at(1) || self.text_at(1) == "function" => {
                let mut stmt = self.type_alias(start)?;

                if let Stmt::TypeAlias(t) = &mut stmt {
                    t.attributes = attrs;
                    t.exported = exported;
                }

                return Ok(stmt);
            }

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

    /// Whether a declaration follows `global`, so the word is the
    /// modifier and not a name.
    pub(super) fn global_follows(&self) -> bool {
        match self.text_at(1) {
            "local" | "const" | "function" | "type" | "struct" | "enum" | "trait" | "interface"
            | "remote" | "impl" | "class" | "macro" | "attribute" | "namespace" | "export" => true,

            "async" => self.text_at(2) == "function",

            "open" => self.text_at(2) == "class",

            _ => false,
        }
    }

    /// `global <declaration>`: the declaration parses from `global`, so
    /// its span covers the word and the emit replaces the whole thing.
    /// `global` left the language, so the word reports and the
    /// declaration parses as an `export`.
    fn global_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        // `type` reads its own keyword, the way `export type` does.
        if self.text_at(1) == "type" {
            return self.type_alias(start);
        }

        let at = self.bump();
        self.removed_global(at);
        self.eat("export");

        let stmt = match self.text() {
            "struct" => self.struct_decl(start, Vec::new(), true)?,

            "enum" => self.enum_decl(start, true)?,

            "trait" => self.trait_decl(start, Vec::new(), true)?,

            "interface" => self.interface_decl(start, true)?,

            "remote" => self.remote_decl(start, Vec::new(), true)?,

            "impl" => self.impl_decl(start, true)?,

            "class" | "open" => self.class_stmt(start, true)?,

            "macro" => self.macro_decl(start, true)?,

            "namespace" => self.namespace_decl(start, Vec::new(), true)?,

            "attribute" => self.attribute_decl(start, true)?,

            "async" => {
                let is_async = Some(TokSpan::new(self.bump(), self.pos));
                let mut stmt = self.function_stmt(start, Vec::new())?;

                if let Stmt::Function(f) = &mut stmt {
                    f.body.is_async = is_async;
                }

                mark_exported(stmt)
            }

            "function" => mark_exported(self.function_stmt(start, Vec::new())?),

            _ => mark_exported(self.local_stmt(start)?),
        };

        Ok(stmt)
    }

    /// Records the `global` keyword at `at`, for the report the
    /// compiler writes. See `Chunk::global_keywords`.
    pub(super) fn removed_global(&mut self, at: usize) {
        self.global_keywords.push(TokSpan::new(at, at + 1));
    }
}

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
