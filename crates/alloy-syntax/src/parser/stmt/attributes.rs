//! Statement attributes (`@name`), and the `attribute` declaration.

use super::super::*;

impl<'a> Parser<'a> {
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

            let name = self.attribute_name()?;
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

    /// The name of an attribute. `@M.icon` reads the module's
    /// attribute through a path, which the grammar does not have; the
    /// report at the dot names the import that does.
    fn attribute_name(&mut self) -> Result<TokSpan, ParseError> {
        let name = self.expect_name()?;

        if self.at(".") && self.adjacent_prev() && self.name_at(1) {
            let bare = self.text_at(1);

            return Err(self.err(&format!(
                "an attribute is used by its bare name; import it with `import {{ {bare} }} from ...`"
            )));
        }

        Ok(name)
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

    pub(in super::super) fn attributes(&mut self) -> Result<Vec<TokSpan>, ParseError> {
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
                self.attribute_name()?;
            }

            out.push(TokSpan::new(start, self.pos));
        }

        Ok(out)
    }

    pub(super) fn attribute_decl(
        &mut self,
        start: usize,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
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

        // `as ... end` states the contract. A declaration with none keeps
        // the short form, which is what every attribute wrote before.
        let requires = match self.at("as") && !self.newline_before_pos() && self.eat("as") {
            true => self.require_clauses(start)?,

            false => Vec::new(),
        };

        Ok(Stmt::Attribute(AttributeDecl {
            exported,
            name,
            params,
            targets,
            requires,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /*
    The body of an `attribute ... as ... end`: one `requires` clause per
    line, up to the `end`.

    `requires` and `each` are keywords here and nowhere else, so a file
    that binds either name keeps it. The clause head is what the rest of
    the toolchain reads too; the rule lives in [`crate::contextual`].
    */
    fn require_clauses(&mut self, opener: usize) -> Result<Vec<RequireClause>, ParseError> {
        let mut out = Vec::new();

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated attribute, expected `end`"));
            }

            let at = self.pos;

            match self.require_clause() {
                Ok(clause) => out.push(clause),

                /*
                A clause is one line. An unfinished one reports once and
                takes the rest of its line; the `end` of the body stays,
                so the statement closes here. Unwinding would send the
                recovery back to `attribute`, which reads the header
                again as an `impl` and reports every line of the body.
                */
                Err(e) if self.lenient => {
                    self.report_at(e.offset, &e.message);
                    self.pos = self.pos.max(at + 1);

                    while !self.at_end() && !self.newline_before_pos() {
                        self.bump();
                    }
                }

                Err(e) => return Err(e),
            }
        }

        self.expect_end(opener)?;

        Ok(out)
    }

    fn require_clause(&mut self) -> Result<RequireClause, ParseError> {
        let start = self.pos;
        self.expect("requires")?;
        let visibility = match matches!(self.text(), "public" | "private") {
            true => {
                let i = self.bump();

                Some(TokSpan::new(i, i + 1))
            }

            false => None,
        };

        if !matches!(self.text(), "function" | "field") {
            return Err(self.clause_err("a `requires` clause asks for a `function` or a `field`"));
        }

        let kind_at = self.bump();
        let kind = TokSpan::new(kind_at, kind_at + 1);
        let is_function = self.span_text(kind) == "function";
        let member = match self.at("each") {
            true => {
                self.bump();

                RequireMember::Each(self.clause_name()?)
            }

            false => RequireMember::Name(self.clause_name()?),
        };

        /*
        A function's shape is its parameter list, with the return type
        when the clause writes one. A field's shape is its type.

        Both are optional: `requires function Start` asks for the member
        alone, whatever signature it carries.
        */
        let shape = if is_function {
            match self.at("(") {
                true => {
                    let sig_start = self.pos;
                    self.param_list()?;

                    if self.eat(":") || self.eat("->") {
                        self.type_ret()?;
                    }

                    Some(TokSpan::new(sig_start, self.pos))
                }

                false => None,
            }
        } else if self.eat(":") {
            Some(self.type_()?)
        } else {
            None
        };

        Ok(RequireClause {
            visibility,
            kind,
            member,
            shape,
            span: TokSpan::new(start, self.pos),
        })
    }

    /// The name of the member a clause asks for.
    fn clause_name(&mut self) -> Result<TokSpan, ParseError> {
        if self.clause_ends() {
            return Err(self.clause_err("expected a name"));
        }

        self.expect_name()
    }

    /// Reports if the clause stops at the cursor. A clause is one line,
    /// so the first token of the next line is past it.
    fn clause_ends(&self) -> bool {
        self.at_end() || self.newline_before_pos()
    }

    /// The report for a clause that stops before it is whole. It sits
    /// where the clause stops, so the editor marks the clause and not
    /// the line under it.
    fn clause_err(&self, wanted: &str) -> ParseError {
        if !self.clause_ends() {
            return self.err(&format!("{wanted}, found {}", self.found()));
        }

        let offset = self
            .pos
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(self.src.len(), |t| t.end as usize);

        ParseError {
            offset,
            message: format!("{wanted}, found end of line"),
        }
    }

    fn target_word(&mut self) -> Result<TokSpan, ParseError> {
        if crate::ATTRIBUTE_TARGETS.contains(&self.text()) {
            let i = self.bump();

            return Ok(TokSpan::new(i, i + 1));
        }

        Err(self.err(&format!(
            "expected an attribute target, found {}",
            self.found()
        )))
    }
}
