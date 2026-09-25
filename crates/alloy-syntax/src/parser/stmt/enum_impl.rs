//! `enum` and `impl`.

use super::super::*;

impl<'a> Parser<'a> {
    // --- enum and impl -----------------------------------------------------

    pub(super) fn enum_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        let open = self.pos;
        self.expect("enum")?;
        let name = self.expect_name()?;
        let generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };
        self.header_as(open);
        let mut variants = Vec::new();

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated enum, expected `end`"));
            }

            // A body with no `end`: the file goes on and this is not a
            // variant. `expect_end` reports the missing `end` once, the
            // variants read so far stay, and the rest of the file parses.
            if self.body_ends_early() {
                break;
            }

            let v_start = self.pos;
            let attributes = if self.at("@") {
                self.attrs()?
            } else {
                Vec::new()
            };
            let vname = self.expect_name()?;
            let mut payload = Vec::new();

            if self.eat("(") {
                while !self.at(")") {
                    // The Rust habit, `Playing(round: number)`. An
                    // enum payload is a type; the name has no place.
                    // The parse takes the type, so the rest of the
                    // enum still reads and one report covers it.
                    let named = (self.at_name() && self.text_at(1) == ":").then(|| {
                        let at = self.toks[self.pos + 1].start as usize;
                        self.bump();
                        self.bump();

                        at
                    });
                    let ty = self.type_()?;

                    if let Some(at) = named {
                        let head = self.span_text(vname).to_string();
                        let body = self.span_text(ty).to_string();
                        self.report_at(
                            at,
                            &format!(
                                "an enum payload is a type, not a name; write `{head}({body})`"
                            ),
                        );
                    }

                    payload.push(ty);

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
                attributes,
                name: vname,
                payload,
                value,
                span: TokSpan::new(v_start, self.pos),
            });

            let _ = self.eat(",") || self.eat(";");
        }

        self.expect_end(open)?;

        Ok(Stmt::Enum(EnumDecl {
            attributes: Vec::new(),
            exported,
            name,
            generics,
            variants,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// `as` closes a declaration header when the body shares its line,
    /// `enum Color as Red, Green end`. A body on the next line needs
    /// none, the way Luau's block headers read. A body on the header's
    /// line without it reports, and the parse goes on.
    pub(super) fn header_as(&mut self, head_start: usize) {
        if self.eat("as") || self.newline_before_pos() || self.at("end") {
            return;
        }

        let head = &self.src[self.toks[head_start].start as usize
            ..self.toks[self.pos.max(head_start + 1) - 1].end as usize];
        let at = self.toks[head_start].start as usize;
        self.report_at(at, &format!("`{head}` {}", super::NEEDS_AS));
    }

    pub(super) fn impl_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        self.impl_decl_with(start, Vec::new(), exported)
    }

    pub(super) fn impl_decl_with(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        let head_start = self.pos;
        self.expect("impl")?;
        let first_start = self.pos;
        self.expect_name()?;

        while self.at(".") && self.name_at(1) {
            self.pos += 2;
        }

        let first = TokSpan::new(first_start, self.pos);
        // `impl Box<T>`: the parameters the methods name.
        let mut generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };

        let (trait_name, target) = if self.eat("for") {
            let t_start = self.pos;
            self.expect_name()?;

            while self.at(".") && self.name_at(1) {
                self.pos += 2;
            }

            let target = TokSpan::new(t_start, self.pos);

            if self.at("<") {
                generics = Some(self.angle_span()?);
            }

            (Some(first), target)
        } else {
            (None, first)
        };

        self.header_as(head_start);
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
            // `@route("/x")` takes arguments, as it does on a function.
            let attrs = if self.at("@") {
                self.attrs()?
            } else {
                Vec::new()
            };
            let attributes: Vec<TokSpan> = attrs.iter().map(|a| a.span).collect();

            // `private function f`, `public async function g`.
            let visibility = if matches!(self.text(), "private" | "public")
                && matches!(self.text_at(1), "function" | "async")
            {
                Some(TokSpan::new(self.bump(), self.pos))
            } else {
                None
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
            f.visibility = visibility;
            f.attrs = attrs;
            methods.push(f);
        }

        Ok(Stmt::Impl(ImplDecl {
            attributes,
            exported,
            trait_name,
            target,
            generics,
            methods,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(in super::super) fn expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.expr()?];

        while self.eat(",") {
            out.push(self.expr()?);
        }

        Ok(out)
    }
}
