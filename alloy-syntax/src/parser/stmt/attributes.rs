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
                self.expect_name()?;
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

        Ok(Stmt::Attribute(AttributeDecl {
            exported,
            global: false,
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
}
