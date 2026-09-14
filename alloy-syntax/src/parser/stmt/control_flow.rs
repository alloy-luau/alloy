//! `if`, `for`, and the shared condition-with-bindings grammar.

use super::super::*;

impl<'a> Parser<'a> {
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

        self.expect_end(start)?;
        Ok(Stmt::If(If {
            branches,
            else_block,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn for_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.expect("for")?;
        let first = self.binding("loop variable")?;

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
            self.expect_end(start)?;

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
            vars.push(self.binding("loop variable")?);
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
        self.expect_end(start)?;
        Ok(Stmt::GenericFor(GenericFor {
            vars,
            exprs,
            filter,
            block,
            span: TokSpan::new(start, self.pos),
        }))
    }

    // --- conditions with bindings -----------------------------------------

    /// `if local x = e`, `if not local x = e`, stacked with `;`, with an
    /// optional `where` clause; or a plain expression.
    pub(in super::super) fn cond(&mut self) -> Result<Cond, ParseError> {
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
}
