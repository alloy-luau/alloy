//! `declare`, `local`, `function`, `class`, `type`, and the plain
//! expression statement.

use crate::lexer::TokKind;

use super::super::*;

impl<'a> Parser<'a> {
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
    pub(super) fn declare_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
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
    /// Reports if the token `n` ahead can begin the operand of `delete`.
    pub(super) fn delete_operand_at(&self, n: usize) -> bool {
        // `delete(x)` stays a call: plain Luau reads it that way, and a
        // file that binds `delete` as a name still parses.
        self.name_at(n)
            || matches!(
                self.kind_at(n),
                Some(TokKind::Str { .. } | TokKind::Number | TokKind::InterpStr)
            )
    }

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
                let open = self.pos;
                self.bump();
                let block = self.block()?;
                self.expect_end(open)?;

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
        let body = self.function_body(start)?;

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

        let body = self.function_body(start)?;
        Ok(Stmt::Function(Function {
            attributes,
            attrs: Vec::new(),
            exported: false,
            visibility: None,
            path,
            is_method,
            body,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(in super::super) fn function_body(
        &mut self,
        opener: usize,
    ) -> Result<FunctionBody, ParseError> {
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
        self.expect_end(opener)?;
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
            self.function_body(start)?;

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
        // `new X(...) { }`, `try f()`, and `await f()` stand alone as
        // statements: their value is dropped, the way a call's is.
        // `local n = v as number` leaves `as number` behind: the parser
        // reaches it as a statement of its own.
        if self.at("as") {
            return Err(self.err("`as` is not a cast here; use `::`"));
        }

        if matches!(self.text(), "new" | "try" | "await") && self.prefix_word_here() {
            let e = self.expr()?;

            return match &e {
                Expr::New { .. } | Expr::Try { .. } | Expr::Await { .. } => {
                    Ok(Stmt::Call(e, TokSpan::new(start, self.pos)))
                }

                _ => Err(self.err("this expression is not a statement")),
            };
        }

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
}

/// The metamethods a class can define: the classes RFC list, and `__init`
/// from the constructors RFC that followed it
const CLASS_METAMETHODS: [&str; 17] = [
    "add", "sub", "mul", "div", "mod", "pow", "tostring", "eq", "lt", "le", "iter", "len", "idiv",
    "concat", "unm", "call", "init",
];
