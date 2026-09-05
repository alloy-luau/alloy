//! Type syntax. The parser consumes it for its extent and never interprets it.

use crate::lexer::TokKind;

use super::*;

impl<'a> Parser<'a> {
    // --- types, extent only ------------------------------------------------

    /// Consumes a balanced `<...>` group. Generic parameter lists use this.
    pub(super) fn angle_span(&mut self) -> Result<TokSpan, ParseError> {
        let start = self.pos;
        self.expect("<")?;
        let mut depth = 1usize;

        while depth > 0 {
            if self.at_end() {
                return Err(self.err("unterminated generic parameter list"));
            }

            match self.text() {
                "<" => depth += 1,

                ">" => depth -= 1,

                ">=" if depth == 1 => {
                    return Err(self.err("write `> =` here, `>=` reads as one operator"));
                }

                _ => {}
            }

            self.bump();
        }

        Ok(TokSpan::new(start, self.pos))
    }

    pub(super) fn type_(&mut self) -> Result<TokSpan, ParseError> {
        self.enter()?;

        let start = self.pos;
        let r = self.type_body();

        self.leave();
        r?;

        Ok(TokSpan::new(start, self.pos))
    }

    pub(super) fn type_body(&mut self) -> Result<(), ParseError> {
        // Luau allows a leading `|` or `&`.
        if self.at("|") || self.at("&") {
            self.bump();
        }

        self.type_suffixed()?;

        while self.at("|") || self.at("&") {
            self.bump();
            self.type_suffixed()?;
        }

        Ok(())
    }

    /// Parses a return type: a single type or a parenthesized pack.
    pub(super) fn type_ret(&mut self) -> Result<TokSpan, ParseError> {
        self.type_()
    }

    pub(super) fn type_suffixed(&mut self) -> Result<(), ParseError> {
        // `read T[]` and `write T[]` in a type position.
        let modifier =
            if matches!(self.text(), "read" | "write") && self.name_at(1) && !self.newline_after(0)
            {
                let i = self.bump();

                Some(TokSpan::new(i, i + 1))
            } else {
                None
            };

        let operand_start = self.pos;
        self.type_primary()?;

        loop {
            if self.at("?") {
                self.bump();
            } else if self.at("[") && self.text_at(1) == "]" {
                // `T[]` is Alloy's array type; emit rewrites it.
                let operand = TokSpan::new(operand_start, self.pos);
                let b = self.pos;
                self.pos += 2;
                self.type_edits.push(TypeEdit::ArraySuffix {
                    modifier,
                    operand,
                    brackets: TokSpan::new(b, b + 2),
                });
            } else if self.at("->") {
                self.bump();

                /*
                The return type is a whole type, so `() -> | a | b` parses.

                Luau allows a leading `|` or `&` there, and it allows a union
                without one. The parser records the extent of a type and never
                its structure, so it does not matter here whether `a -> b | c`
                groups as `a -> (b | c)` or `(a -> b) | c`. The span covers the
                same tokens either way.
                */
                self.type_body()?;
            } else {
                break;
            }
        }

        Ok(())
    }

    pub(super) fn type_primary(&mut self) -> Result<(), ParseError> {
        self.enter()?;
        let r = self.type_primary_inner();
        self.leave();

        r
    }

    pub(super) fn type_primary_inner(&mut self) -> Result<(), ParseError> {
        match self.text() {
            "nil" | "true" | "false" => {
                self.bump();

                Ok(())
            }

            "typeof" if self.text_at(1) == "(" => {
                self.bump();
                self.bump();
                self.expr()?;
                self.expect(")")?;

                Ok(())
            }

            "..." => {
                // The variadic element of a type pack, and it can be a union:
                // `...("critical" | "weak")` and `..."hit" | "miss"`.
                self.bump();

                self.type_body()
            }

            // This is a generic function type: `<T>(T) -> T`.
            "<" => {
                self.angle_span()?;

                self.type_primary_inner()
            }

            "(" => {
                // This is a parenthesized type or the parameters of a function type.
                self.bump();

                if !self.at(")") {
                    loop {
                        if self.at("...") {
                            self.bump();

                            if !self.at(")") && !self.at(",") {
                                self.type_body()?;
                            }
                        } else {
                            // The parameter can have a name.
                            if self.at_name() && self.text_at(1) == ":" {
                                self.bump();
                                self.bump();
                            }

                            self.type_body()?;
                        }

                        if !self.eat(",") {
                            break;
                        }
                    }
                }

                self.expect(")")?;

                Ok(())
            }

            "{" => self.type_table(),

            _ => match self.kind_at(0) {
                Some(TokKind::Str { .. }) => {
                    // This is a singleton string type.
                    self.bump();

                    Ok(())
                }

                Some(TokKind::Ident) if !is_reserved(self.text()) => {
                    let is_ambient = matches!(
                        self.text(),
                        "Future"
                            | "Result"
                            | "Array"
                            | "HashMap"
                            | "Set"
                            | "Signal"
                            | "SignalConnection"
                            | "Signalish"
                            | "Partial"
                            | "Readonly"
                            | "Sink"
                            | "Queue"
                            | "Heap"
                            | "Scope"
                            | "Iter"
                    ) && self.text_at(1) != ".";
                    let i = self.bump();

                    if is_ambient {
                        self.type_edits
                            .push(TypeEdit::AmbientName(TokSpan::new(i, i + 1)));
                    }

                    if self.at(".") {
                        self.bump();
                        self.expect_name()?;
                    }

                    if self.at("<") {
                        self.angle_span()?;
                    }

                    // This is a generic type pack: `T...`.
                    if self.at("...") {
                        self.bump();
                    }

                    Ok(())
                }

                _ => Err(self.err(&format!("expected a type, found {}", self.found()))),
            },
        }
    }

    pub(super) fn type_table(&mut self) -> Result<(), ParseError> {
        let table_start = self.pos;
        self.expect("{")?;

        // `{ [K in keyof T]: V }`: a mapped type, recorded for emit.
        if self.at("[") && self.name_at(1) && self.text_at(2) == "in" && self.text_at(3) == "keyof"
        {
            self.bump();
            let key = self.expect_name()?;
            self.bump(); // in
            self.bump(); // keyof
            let source = self.expect_name()?;
            self.expect("]")?;
            self.expect(":")?;
            let modifier = if matches!(self.text(), "read" | "write") {
                let i = self.bump();

                Some(TokSpan::new(i, i + 1))
            } else {
                None
            };
            // The value: `T[K]` with an optional `?`.
            self.expect_name()?;
            self.expect("[")?;
            self.expect_name()?;
            self.expect("]")?;
            let optional = self.eat("?");
            let _ = self.eat(",") || self.eat(";");
            self.expect("}")?;
            self.type_edits.push(TypeEdit::Mapped {
                table: TokSpan::new(table_start, self.pos),
                key,
                source,
                modifier,
                optional,
            });

            return Ok(());
        }

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated table type"));
            }

            /*
            The `read` and `write` access modifiers come before a name or
            an indexer: `{ read x: T }` and `{ read [string]: T }` alike.
            A field NAMED read stays a field, because the modifier reading
            needs something to modify after it.
            */
            if matches!(self.text(), "read" | "write")
                && (matches!(self.kind_at(1), Some(TokKind::Ident)) || self.text_at(1) == "[")
            {
                self.bump();
            }

            if self.at("[") {
                self.bump();
                self.type_body()?;
                self.expect("]")?;
                self.expect(":")?;
                self.type_body()?;
            } else {
                if self.at_name() && self.text_at(1) == ":" {
                    self.bump();
                    self.bump();
                    self.type_body()?;
                } else {
                    // This is an array style element.
                    self.type_body()?;
                }
            }

            if !self.eat(",") && !self.eat(";") {
                break;
            }
        }

        self.expect("}")?;

        Ok(())
    }
}
