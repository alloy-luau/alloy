//! Expressions: precedence climbing and the suffixed chain.

use crate::lexer::TokKind;

use super::*;

impl<'a> Parser<'a> {
    // --- expressions -------------------------------------------------------

    pub(super) fn expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let cond = self.sub_expr(0)?;

        // `c ? a : b`. A `?` that fuses with the next token is safe access,
        // handled in the suffix loop; a lone `?` here is the ternary.
        if self.at("?") && !self.question_fuses() {
            self.bump();
            self.no_method_call += 1;
            let then_value = self.expr();
            self.no_method_call -= 1;
            let then_value = then_value?;
            self.expect(":")?;
            let else_value = self.expr()?;

            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then_value: Box::new(then_value),
                else_value: Box::new(else_value),
                span: TokSpan::new(start, self.pos),
            });
        }

        Ok(cond)
    }

    /// Runs a parse with method calls allowed again, inside brackets where
    /// a `:` can no longer be a ternary's else marker. Kept as an `#[inline]`
    /// closure-free pair so a nested parenthesis costs no extra frame.
    #[inline(always)]
    fn bracketed<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        let saved = self.no_method_call;
        self.no_method_call = 0;
        let r = f(self);
        self.no_method_call = saved;

        r
    }

    /// A backtick string with holes: head, then expression and mid or
    /// tail, until the tail.
    pub(super) fn interp_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.bump();
        let mut parts = Vec::new();

        loop {
            let part = self.bracketed(|p| p.expr())?;
            parts.push(part);

            match self.kind_at(0) {
                Some(TokKind::InterpMid) => {
                    self.bump();
                }

                Some(TokKind::InterpTail) => {
                    self.bump();

                    break;
                }

                _ => return Err(self.err("expected `}` to close the interpolation hole")),
            }
        }

        Ok(Expr::Interp {
            parts,
            span: TokSpan::new(start, self.pos),
        })
    }

    /// Reports if the `?` at the cursor fuses with the token after it.
    /// `x:name`: the colon touches the receiver and the name.
    fn colon_fuses(&self) -> bool {
        self.pos > 0
            && self.toks[self.pos - 1].end == self.toks[self.pos].start
            && self.adjacent(0)
            && self.name_at(1)
    }

    fn question_fuses(&self) -> bool {
        self.adjacent(0) && matches!(self.text_at(1), "." | ":" | "[" | "(" | "?")
    }

    pub(super) fn sub_expr(&mut self, limit: u8) -> Result<Expr, ParseError> {
        self.enter()?;
        let start = self.pos;
        // `bnot` is a word operator: it stays a name unless an operand
        // follows on the same line.
        let bnot = self.text() == "bnot"
            && !self.newline_after(0)
            && matches!(
                self.kind_at(1),
                Some(TokKind::Ident | TokKind::Number | TokKind::LParen)
            );
        let mut left = if is_unary_op(self.text()) || bnot {
            let op = self.bump();
            let operand = self.sub_expr(UNARY_PRIORITY)?;

            Expr::Unary {
                op: TokSpan::new(op, op + 1),
                operand: Box::new(operand),
                span: TokSpan::new(start, self.pos),
            }
        } else {
            self.simple_expr()?
        };

        while let Some((width, (left_prec, right_prec))) = self.binop_at() {
            if left_prec <= limit {
                break;
            }

            let op = self.pos;
            self.pos += width;
            let rhs = self.sub_expr(right_prec)?;

            left = Expr::Binary {
                op: TokSpan::new(op, op + width),
                lhs: Box::new(left),
                rhs: Box::new(rhs),
                span: TokSpan::new(start, self.pos),
            };
        }

        self.leave();

        Ok(left)
    }

    pub(super) fn simple_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let mut e = match self.text() {
            "nil" => {
                self.bump();

                Expr::Nil(TokSpan::new(start, self.pos))
            }

            "true" => {
                self.bump();

                Expr::True(TokSpan::new(start, self.pos))
            }

            "false" => {
                self.bump();

                Expr::False(TokSpan::new(start, self.pos))
            }

            "..." => {
                self.bump();

                Expr::Vararg(TokSpan::new(start, self.pos))
            }

            "function" => {
                self.bump();
                let body = self.function_body()?;
                Expr::Function {
                    attributes: Vec::new(),
                    body: Box::new(body),
                    span: TokSpan::new(start, self.pos),
                }
            }

            "@" => {
                let attributes = self.attributes()?;
                self.expect("function")?;
                let body = self.function_body()?;
                Expr::Function {
                    attributes,
                    body: Box::new(body),
                    span: TokSpan::new(start, self.pos),
                }
            }

            "{" => self.table_expr()?,

            "[" => self.array_expr()?,

            "if" => self.if_else_expr()?,

            "async" if self.text_at(1) == "function" && !self.newline_after(0) => {
                let is_async = Some(TokSpan::new(self.bump(), self.pos));
                self.bump();
                let mut body = self.function_body()?;
                body.is_async = is_async;
                Expr::Function {
                    attributes: Vec::new(),
                    body: Box::new(body),
                    span: TokSpan::new(start, self.pos),
                }
            }

            "async" if self.text_at(1) == "do" && !self.newline_after(0) => {
                self.bump();
                self.bump();
                let block = self.block()?;
                self.expect("end")?;
                Expr::AsyncBlock {
                    block,
                    span: TokSpan::new(start, self.pos),
                }
            }

            "try" if self.text_at(1) == "do" && !self.newline_after(0) => {
                self.bump();
                self.bump();
                let block = self.block()?;
                self.expect("end")?;
                Expr::TryBlock {
                    block,
                    span: TokSpan::new(start, self.pos),
                }
            }

            "await" | "try" if self.prefix_word_here() => {
                let is_await = self.at("await");
                self.bump();
                let operand = Box::new(self.sub_expr(UNARY_PRIORITY)?);
                let span = TokSpan::new(start, self.pos);

                if is_await {
                    Expr::Await { operand, span }
                } else {
                    Expr::Try { operand, span }
                }
            }

            "new" if self.prefix_word_here() && self.name_at(1) => self.new_expr()?,

            "match" if self.match_follows() => self.match_expr()?,

            "$" => self.macro_call()?,

            _ => match self.kind_at(0) {
                Some(TokKind::Number) => {
                    self.bump();

                    Expr::Number(TokSpan::new(start, self.pos))
                }

                // A string literal takes a method: `"abc":upper()`.
                Some(TokKind::Str { .. }) if self.text_at(1) == ":" => self.suffixed_expr()?,

                Some(TokKind::InterpStr) if self.text_at(1) == ":" => self.suffixed_expr()?,

                Some(TokKind::Str { .. }) => {
                    self.bump();

                    Expr::String(TokSpan::new(start, self.pos))
                }

                Some(TokKind::InterpStr) => {
                    self.bump();

                    Expr::InterpString(TokSpan::new(start, self.pos))
                }

                // A string with holes may take a method too; the suffix
                // parser sees the whole string as one primary.
                Some(TokKind::InterpHead) => self.suffixed_expr()?,

                _ => self.suffixed_expr()?,
            },
        };

        // `expr :: T`, `expr is T`, and `expr satisfies T` bind more tightly
        // than any binary operator.
        loop {
            if self.at("::") {
                self.bump();
                let ty = self.type_()?;
                e = Expr::TypeAssert {
                    expr: Box::new(e),
                    ty,
                    span: TokSpan::new(start, self.pos),
                };
            } else if self.at("is") && self.infix_word_here() && !self.ternary_colon_next() {
                self.bump();
                let negated = self.eat("not");
                let name = self.dotted_name()?;
                e = Expr::Is {
                    expr: Box::new(e),
                    negated,
                    name,
                    span: TokSpan::new(start, self.pos),
                };
            } else if self.at("satisfies") && self.infix_word_here() {
                self.bump();
                let ty = self.type_()?;
                e = Expr::Satisfies {
                    expr: Box::new(e),
                    ty,
                    span: TokSpan::new(start, self.pos),
                };
            } else {
                break;
            }
        }

        Ok(e)
    }

    /// Inside a ternary's then-branch, a `:` after `is` would be the else.
    fn ternary_colon_next(&self) -> bool {
        self.no_method_call > 0 && self.text_at(1) == ":"
    }

    /// `Name`, `Name.Name`, `Enum.KeyCode`: the type name after `is`.
    fn dotted_name(&mut self) -> Result<TokSpan, ParseError> {
        let start = self.pos;

        if !self.at_name() && !matches!(self.text(), "nil" | "function") {
            return Err(self.err(&format!("expected a type name, found {}", self.found())));
        }

        self.bump();

        while self.at(".") && self.name_at(1) {
            self.pos += 2;
        }

        Ok(TokSpan::new(start, self.pos))
    }

    /// `[ a, b, c ]`.
    fn array_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.expect("[")?;
        let mut items = Vec::new();

        while !self.at("]") {
            if self.at_end() {
                return Err(self.err("unterminated array literal"));
            }

            items.push(self.bracketed(|p| p.expr())?);

            if !self.eat(",") && !self.eat(";") {
                break;
            }
        }

        self.expect("]")?;

        Ok(Expr::Array {
            items,
            span: TokSpan::new(start, self.pos),
        })
    }

    /// `new Name<<T>>(args) { init }`, `new Name(args)`, `new Name { init }`.
    fn new_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.bump();
        let name_start = self.pos;
        self.expect_name()?;

        while self.at(".") && self.name_at(1) {
            self.pos += 2;
        }

        let name = Expr::Name(TokSpan::new(name_start, self.pos));

        let type_args = if self.at("<") && self.text_at(1) == "<" {
            Some(self.angle_span()?)
        } else {
            None
        };

        let args = if self.at("(") {
            Some(self.call_args()?)
        } else {
            None
        };

        let init = if self.at("{") {
            Some(Box::new(self.table_expr()?))
        } else {
            None
        };

        if args.is_none() && init.is_none() {
            return Err(self.err("`new` needs `(args)` or `{ fields }` after the name"));
        }

        Ok(Expr::New {
            name: Box::new(name),
            type_args,
            args,
            init,
            span: TokSpan::new(start, self.pos),
        })
    }

    /// `$name(args)`, `$M.name(args)`.
    fn macro_call(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.expect("$")?;

        if !self.adjacent(0) && self.pos > 0 {
            // `$ name` with a space is not a macro.
        }

        if !self.at_name() {
            return Err(self.err("`$` must be followed by a macro name"));
        }

        let name_start = self.pos;
        self.bump();

        while self.at(".") && self.name_at(1) {
            self.pos += 2;
        }

        let name = TokSpan::new(name_start, self.pos);
        self.expect("(")?;
        let args = if self.at(")") {
            Vec::new()
        } else {
            self.expr_list()?
        };
        self.expect(")")?;

        Ok(Expr::Macro {
            name,
            args,
            span: TokSpan::new(start, self.pos),
        })
    }

    pub(super) fn primary_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;

        // A string literal as a receiver: `"abc":upper()`.
        if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
            self.bump();

            return Ok(Expr::String(TokSpan::new(start, self.pos)));
        }

        if matches!(self.kind_at(0), Some(TokKind::InterpStr)) {
            self.bump();

            return Ok(Expr::InterpString(TokSpan::new(start, self.pos)));
        }

        if matches!(self.kind_at(0), Some(TokKind::InterpHead)) {
            return self.interp_expr();
        }

        if self.at("(") {
            self.bump();
            let inner = self.bracketed(|p| p.expr())?;
            self.expect(")")?;

            return Ok(Expr::Paren {
                inner: Box::new(inner),
                span: TokSpan::new(start, self.pos),
            });
        }

        let name = self.expect_name()?;
        let tok = self.toks[name.start as usize];
        let word = &self.src[tok.start as usize..tok.end as usize];

        // `import(...)` and `import<<T>>(...)` are the expression form of
        // `import`; the emit turns the call into `require`.
        if !(word == "import" && (self.at("(") || self.at("<"))) {
            self.reject_reserved(name);
        }

        Ok(Expr::Name(name))
    }

    pub(super) fn suffixed_expr(&mut self) -> Result<Expr, ParseError> {
        self.enter()?;

        let start = self.pos;
        let mut e = self.primary_expr()?;

        loop {
            match self.text() {
                "." => {
                    self.bump();
                    let field = self.expect_name()?;
                    e = Expr::Index {
                        object: Box::new(e),
                        key: IndexKey::Field(field),
                        optional: false,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                "[" => {
                    self.bump();
                    let key = self.bracketed(|p| p.expr())?;
                    self.expect("]")?;
                    e = Expr::Index {
                        object: Box::new(e),
                        key: IndexKey::Computed(Box::new(key)),
                        optional: false,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                // `obj->Name` finds a child; `obj=>Name` waits for one.
                "->" | "=>" => {
                    let wait = self.at("=>");
                    self.bump();
                    let name = self.child_name()?;
                    e = Expr::Child {
                        object: Box::new(e),
                        name,
                        wait,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                // Inside a ternary's then-branch, a `:` with space before
                // it is the else marker; one that touches its receiver and
                // a name, `x:upper()`, is the method call it looks like.
                ":" if self.no_method_call > 0 && !self.colon_fuses() => break,

                ":" => {
                    self.bump();

                    let method = self.expect_name()?;

                    // A method call also takes type arguments: `obj:m<<T>>()`.
                    let type_args = if self.at("<") && self.text_at(1) == "<" {
                        Some(self.angle_span()?)
                    } else {
                        None
                    };

                    // `obj:name` with nothing callable after it is a bound
                    // method reference. A `(` on the next line stays the
                    // ambiguity Luau reports, so it is not a reference.
                    let callable = self.at("(")
                        || self.at("{")
                        || matches!(self.kind_at(0), Some(TokKind::Str { .. }));

                    if !callable && type_args.is_none() {
                        e = Expr::MethodRef {
                            object: Box::new(e),
                            name: method,
                            span: TokSpan::new(start, self.pos),
                        };

                        continue;
                    }

                    let args = self.call_args()?;

                    e = Expr::Call {
                        func: Box::new(e),
                        method: Some(method),
                        type_args,
                        optional: false,
                        args,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                /*
                The safe-access family: `?` fused with the token that touches
                it. `??` belongs to the binary operator loop above this one,
                so two `?` in a row end the chain here. A `?` that touches
                nothing it can fuse with is an error, since Luau has no `?`
                in an expression.
                */
                "?" if self.adjacent(0) && self.text_at(1) != "?" => match self.text_at(1) {
                    "." => {
                        self.pos += 2;
                        let field = self.expect_name()?;
                        e = Expr::Index {
                            object: Box::new(e),
                            key: IndexKey::Field(field),
                            optional: true,
                            span: TokSpan::new(start, self.pos),
                        };
                    }

                    "[" => {
                        self.pos += 2;
                        let key = self.expr()?;
                        self.expect("]")?;
                        e = Expr::Index {
                            object: Box::new(e),
                            key: IndexKey::Computed(Box::new(key)),
                            optional: true,
                            span: TokSpan::new(start, self.pos),
                        };
                    }

                    ":" => {
                        self.pos += 2;
                        let method = self.expect_name()?;
                        let type_args = if self.at("<") && self.text_at(1) == "<" {
                            Some(self.angle_span()?)
                        } else {
                            None
                        };
                        let args = self.call_args()?;
                        e = Expr::Call {
                            func: Box::new(e),
                            method: Some(method),
                            type_args,
                            optional: true,
                            args,
                            span: TokSpan::new(start, self.pos),
                        };
                    }

                    "(" => {
                        self.pos += 1;
                        let args = self.call_args()?;
                        e = Expr::Call {
                            func: Box::new(e),
                            method: None,
                            type_args: None,
                            optional: true,
                            args,
                            span: TokSpan::new(start, self.pos),
                        };
                    }

                    _ => {
                        return Err(self.err(
                            "`?` must be followed by `.`, `:`, `[`, or `(`, or doubled as `??`",
                        ));
                    }
                },

                // `expr!` asserts non-nil. `!=` is the one typo worth naming.
                "!" => {
                    if self.text_at(1) == "=" && self.adjacent(0) {
                        return Err(self.err("`!=` is not an operator; write `~=`"));
                    }

                    self.bump();
                    e = Expr::NonNil {
                        operand: Box::new(e),
                        span: TokSpan::new(start, self.pos),
                    };
                }

                /*
                This is an explicit type instantiation, `f<<T>>()`, which
                Luau calls a turbofish. It is the only place where two `<`
                sit next to each other. No expression starts with `<`, and
                Luau has no shift operator. So there is no other reading to
                separate this from.

                The argument is a type or a type pack. So `<<number>>`,
                `<<(number, string)>>`, `<<()>>` and `<<...number>>` are all
                legal. `angle_span` already counts bracket depth. That is the
                reason the nested pair needs no special case.

                A turbofish always comes before a call. So `call_args` is
                required, not optional, and the parser reports a missing one.
                */
                "<" if self.text_at(1) == "<" => {
                    let type_args = Some(self.angle_span()?);

                    let args = self.call_args()?;

                    e = Expr::Call {
                        func: Box::new(e),
                        method: None,
                        type_args,
                        optional: false,
                        args,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                "(" | "{" => {
                    /*
                    A `(` on a line of its own after a complete expression is
                    the oldest ambiguity in Lua: it reads as a call of the line
                    above, and it reads equally well as a new statement that
                    opens with a parenthesis. Luau refuses to guess and asks
                    for a semicolon, so larvae refuses too.

                    Matching Luau matters more than being permissive here.
                    Accepting it would mean `larvae check` passes a file the
                    real compiler rejects, and `larvae fmt` would join the two
                    lines into the reading larvae picked, which is a choice the
                    author never made.
                    */
                    if self.text() == "(" && self.newline_before_pos() {
                        return Err(self.err(
                            "ambiguous syntax: this looks like the argument list of a call \
                             on the line above, and also like the start of a new statement, \
                             separate them with a `;`",
                        ));
                    }

                    let args = self.call_args()?;
                    e = Expr::Call {
                        func: Box::new(e),
                        method: None,
                        type_args: None,
                        optional: false,
                        args,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                _ => {
                    if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
                        let args = self.call_args()?;
                        e = Expr::Call {
                            func: Box::new(e),
                            method: None,
                            type_args: None,
                            optional: false,
                            args,
                            span: TokSpan::new(start, self.pos),
                        };
                    } else {
                        break;
                    }
                }
            }
        }

        self.leave();

        Ok(e)
    }

    /// The name after `->` or `=>`: a Name, a string, or `[expr]`.
    fn child_name(&mut self) -> Result<ChildName, ParseError> {
        if self.at_name() {
            let i = self.bump();

            return Ok(ChildName::Name(TokSpan::new(i, i + 1)));
        }

        if matches!(
            self.kind_at(0),
            Some(TokKind::Str { .. } | TokKind::InterpStr)
        ) {
            let i = self.bump();

            return Ok(ChildName::Str(TokSpan::new(i, i + 1)));
        }

        // A string with holes is an expression, like `[expr]`.
        if matches!(self.kind_at(0), Some(TokKind::InterpHead)) {
            let e = self.interp_expr()?;

            return Ok(ChildName::Computed(Box::new(e)));
        }

        if self.at("[") {
            self.bump();
            let key = self.expr()?;
            self.expect("]")?;

            return Ok(ChildName::Computed(Box::new(key)));
        }

        Err(self.err(&format!(
            "expected a child name, a string, or `[expr]`, found {}",
            self.found()
        )))
    }

    pub(super) fn call_args(&mut self) -> Result<CallArgs, ParseError> {
        if self.at("(") {
            self.bump();
            let args = if self.at(")") {
                Vec::new()
            } else {
                self.bracketed(|p| p.expr_list())?
            };

            self.expect(")")?;

            return Ok(CallArgs::Paren(args));
        }

        if self.at("{") {
            let table = self.table_expr()?;

            return Ok(CallArgs::Table(Box::new(table)));
        }

        if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
            let i = self.bump();

            return Ok(CallArgs::Str(TokSpan::new(i, i + 1)));
        }

        Err(self.err(&format!("expected call arguments, found {}", self.found())))
    }

    pub(super) fn table_expr(&mut self) -> Result<Expr, ParseError> {
        self.bracketed(|p| p.table_expr_inner())
    }

    fn table_expr_inner(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.expect("{")?;
        let mut fields = Vec::new();

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated table"));
            }

            if self.at("...") && self.spread_follows() {
                self.bump();
                let value = self.sub_expr(UNARY_PRIORITY)?;
                fields.push(TableField::Spread(value));
            } else if self.at("[") {
                self.bump();
                let key = self.expr()?;
                self.expect("]")?;
                self.expect("=")?;
                let value = self.expr()?;
                fields.push(TableField::Computed { key, value });
            } else if self.at_name() && self.text_at(1) == "=" {
                let name = self.expect_name()?;
                self.bump();
                let value = self.expr()?;
                fields.push(TableField::Named { name, value });
            } else {
                fields.push(TableField::Positional(self.expr()?));
            }

            if !self.eat(",") && !self.eat(";") {
                break;
            }
        }

        self.expect("}")?;
        Ok(Expr::Table {
            fields,
            span: TokSpan::new(start, self.pos),
        })
    }

    /// After `...` in a table: a spread when an expression follows on the
    /// same line, the vararg when `,`, `}`, `;`, or a binary operator does.
    fn spread_follows(&self) -> bool {
        if self.newline_after(0) {
            return false;
        }

        let next = self.text_at(1);

        !(next == ","
            || next == "}"
            || next == ";"
            || binop_priority(next).is_some()
            || next == "?")
            && self.kind_at(1).is_some()
    }

    pub(super) fn if_else_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.expect("if")?;

        let mut branches = Vec::new();
        let cond = self.cond()?;

        self.expect("then")?;
        branches.push((cond, self.expr()?));

        while self.at("elseif") {
            self.bump();
            let cond = self.cond()?;
            self.expect("then")?;
            branches.push((cond, self.expr()?));
        }

        self.expect("else")?;
        let else_value = self.expr()?;
        Ok(Expr::IfElse {
            branches,
            else_value: Box::new(else_value),
            span: TokSpan::new(start, self.pos),
        })
    }
}
