//! `struct`, `interface`, `trait`, `remote`, and `macro`.

use super::super::*;

impl<'a> Parser<'a> {
    // --- struct, trait, interface, remote, attribute, macro ----------------

    /*
    A Luau reserved word where a field name goes: the word, then `:`.

    `end: number` closed the body, so the type and every line after it
    reported against a declaration that had already ended. The word is a
    field the language cannot spell, and the body reads on to the next
    one.
    */
    fn reserved_field(&self) -> bool {
        self.at_reserved() && self.text_at(1) == ":"
    }

    /// The fields of a struct or interface, up to `end`.
    fn fields(&mut self) -> Result<Vec<Field>, ParseError> {
        let mut fields = Vec::new();

        while !self.at("end") || self.reserved_field() {
            if self.at_end() {
                return Err(self.err("unterminated declaration, expected `end`"));
            }

            // A body with no `end`: the file goes on and this is not a
            // field. The caller reports the missing `end` once, and the
            // statements after the body still parse. A reserved word
            // with a type behind it is a field, not the file going on.
            if self.body_ends_early() && !self.reserved_field() {
                break;
            }

            let f_start = self.pos;
            let attributes = if self.at("@") {
                self.attrs()?
            } else {
                Vec::new()
            };
            let visibility = if matches!(self.text(), "private" | "public") {
                let i = self.bump();

                Some(TokSpan::new(i, i + 1))
            } else {
                None
            };
            let modifier = if matches!(self.text(), "read" | "write") && self.name_at(1) {
                let i = self.bump();

                Some(TokSpan::new(i, i + 1))
            } else {
                None
            };
            // A field still being typed, a modifier alone or a name with
            // no type, gets one diagnostic that names what is missing.
            // The rest of the line is skipped and the declaration goes
            // on, so the file around it still parses.
            match self.field_rest() {
                Ok((name, ty, default)) => {
                    fields.push(Field {
                        attributes,
                        visibility,
                        modifier,
                        name,
                        ty,
                        default,
                        span: TokSpan::new(f_start, self.pos),
                    });

                    let _ = self.eat(",") || self.eat(";");
                }

                Err((offset, message)) => {
                    self.report_at(offset, &message);

                    // Past the rest of the line, and past at least one
                    // token, so a broken field that starts a line cannot
                    // hold the loop in place.
                    if self.pos == f_start && !self.at_end() {
                        self.bump();
                    }

                    while !self.at_end() && !self.at("end") && !self.newline_before_pos() {
                        self.bump();
                    }
                }
            }
        }

        Ok(fields)
    }

    /// `name: T = default` of a field, after its attributes and
    /// modifiers. An error carries the offset to report at and what the
    /// field lacks.
    fn field_rest(&mut self) -> Result<(TokSpan, TokSpan, Option<Expr>), (usize, String)> {
        let prev = self
            .pos
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map(|t| {
                (
                    t.start as usize,
                    &self.src[t.start as usize..t.end as usize],
                )
            });
        let here = self.toks.get(self.pos).map(|t| t.start as usize);

        if !self.name_at(0) {
            // A word the language keeps, with a type behind it: the
            // word is the mistake, not the token the parse stopped at.
            // Without the type it is the `end` of the body, and a
            // modifier alone keeps its own report.
            if self.reserved_field() {
                let tok = self.toks[self.pos];
                let word = &self.src[tok.start as usize..tok.end as usize];

                return Err((
                    tok.start as usize,
                    format!("`{word}` is a reserved word and cannot name a field"),
                ));
            }

            return Err(match prev {
                Some((at, word @ ("private" | "public" | "read" | "write"))) => (
                    at,
                    format!("`{word}` needs a field name after it: `{word} name: T`"),
                ),

                _ => (
                    here.unwrap_or(self.src.len()),
                    format!("expected a field name, found {}", self.found()),
                ),
            });
        }

        let name_at = self.toks[self.pos].start as usize;
        let name = TokSpan::new(self.pos, self.pos + 1);
        let word = self.text().to_string();
        self.bump();

        if !self.eat(":") {
            return Err((name_at, format!("field `{word}` needs a type: `{word}: T`")));
        }

        let ty = self.type_().map_err(|e| {
            (
                e.offset,
                format!("field `{word}` needs a type after `:`; {}", e.message),
            )
        })?;
        let default = if self.eat("=") {
            Some(self.expr().map_err(|e| (e.offset, e.message))?)
        } else {
            None
        };

        Ok((name, ty, default))
    }

    pub(super) fn struct_decl(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        let open = self.pos;
        self.expect("struct")?;
        let name = self.expect_name()?;
        let generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };
        self.expect("as")?;
        let fields = self.fields()?;
        self.expect_end(open)?;

        Ok(Stmt::Struct(StructDecl {
            attributes,
            exported,
            name,
            generics,
            fields,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn interface_decl(
        &mut self,
        start: usize,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        self.interface_decl_with(start, Vec::new(), exported)
    }

    pub(super) fn interface_decl_with(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        let open = self.pos;
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
        self.expect_end(open)?;

        // An interface is a shape other code sees whole: a field of it
        // has no visibility.
        for v in fields.iter().filter_map(|f| f.visibility) {
            self.report_at(
                self.toks[v.start as usize].start as usize,
                "an interface field has no visibility; `private` and `public` belong to a struct",
            );
        }

        Ok(Stmt::Interface(InterfaceDecl {
            attributes,
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
    pub(super) fn trait_decl(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        let open = self.pos;
        let head_start = self.pos;
        self.expect("trait")?;
        let name = self.expect_name()?;

        // A trait is not generic: it is one shape, and a bound names it,
        // `<T: Trait>`. Reading the list keeps the rest of the file.
        if self.at("<") {
            let at = self.toks[self.pos].start as usize;
            self.report_at(
                at,
                "a trait takes no type parameters; put `<T>` on the method",
            );
            self.angle_span()?;
        }

        self.header_as(head_start);
        let mut methods = Vec::new();
        // The column of the first signature. A trait body holds
        // `function` members, so the opener's column alone cannot say
        // where the body stops: a body with no indent starts at the same
        // column as the `trait`. A `function` left of the first signature
        // is the file going on.
        let mut member_column = None;

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated trait, expected `end`"));
            }

            // A body with no `end`: `expect_end` reports it once, the
            // signatures read so far stay, and the rest of the file parses.
            let dedents = member_column.is_some_and(|c| self.column_at(self.pos) < c);

            if self.body_ends_early() && (dedents || !self.at("function")) {
                break;
            }

            let m_start = self.pos;
            member_column.get_or_insert_with(|| self.column_at(m_start));
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

            // A signature followed by a statement at or left of its own
            // column is a trait with no `end`, not a default body: a
            // default body sits one level in from its `function`.
            let has_body = !self.at_end()
                && !matches!(self.text(), "function" | "end" | "@")
                && !(self.body_ends_early() && self.column_at(self.pos) <= self.column_at(m_start));
            let body = if has_body {
                let b_start = self.pos;
                let block = self.block()?;
                self.expect_end(m_start)?;

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

        self.expect_end(open)?;

        Ok(Stmt::Trait(TraitDecl {
            attributes,
            exported,
            name,
            methods,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// `(a: T = 1, @u8 b: number, ...)` with attributes on parameters.
    pub(super) fn param_list(&mut self) -> Result<Vec<Param>, ParseError> {
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

    pub(super) fn remote_decl(
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

    /// `macro name(params) {stat} [exp] end`.
    pub(super) fn macro_decl(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        let open = self.pos;
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
        self.expect_end(open)?;

        Ok(Stmt::Macro(MacroDecl {
            exported,
            name,
            params,
            body,
            tail,
            span: TokSpan::new(start, self.pos),
        }))
    }
}
