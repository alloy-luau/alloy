//! Patterns: local bindings, struct patterns, and match arm heads.

use crate::lexer::TokKind;

use super::super::*;

impl<'a> Parser<'a> {
    // --- patterns ----------------------------------------------------------

    /// Reports if the name after `local` starts a pattern local: a name
    /// or a dotted path, then `(`, or `{` on the same line.
    pub(super) fn pattern_local_follows(&self) -> bool {
        // `local f(x)` and `local P { x }` are not valid Luau, so a name
        // and `(` or `{` after `local` can only be a pattern.
        if !self.at_name() {
            return false;
        }

        let mut n = 0;

        while self.text_at(n + 1) == "." && self.name_at(n + 2) {
            n += 2;
        }

        match self.text_at(n + 1) {
            "(" => true,

            "{" => !self.newline_after(n),

            _ => false,
        }
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

                    // A dotted path compares by value. The path names one
                    // variant, so a payload list may follow it:
                    // `Kind.Big(n)` reads like the bare `Big(n)`.
                    if self.at(".") && self.name_at(1) {
                        while self.at(".") && self.name_at(1) {
                            self.pos += 2;
                        }

                        let path = TokSpan::new(name_start, self.pos);

                        // `B.Gem { n }` names a struct through a module
                        // or a namespace.
                        if self.at("{") {
                            return self.struct_pattern(Some(path), start);
                        }

                        if !self.at("(") {
                            return Ok(Pattern::Path(path));
                        }

                        let args = self.variant_args()?;

                        return Ok(Pattern::Variant {
                            name: path,
                            args,
                            span: TokSpan::new(start, self.pos),
                        });
                    }

                    let name = TokSpan::new(name_start, name_start + 1);

                    if self.at("(") {
                        let args = self.variant_args()?;

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

    /// The payload list a variant pattern carries: `(p, q)` after the
    /// variant name or after the path that names it.
    fn variant_args(&mut self) -> Result<Vec<Pattern>, ParseError> {
        self.expect("(")?;
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

        Ok(args)
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
}
