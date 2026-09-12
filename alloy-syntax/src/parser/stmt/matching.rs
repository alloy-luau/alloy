//! The `match` statement and the `match` expression.

use super::super::*;

impl<'a> Parser<'a> {
    // --- match -------------------------------------------------------------

    /// `match` is a keyword when an expression that is not a call shape
    /// follows on the same line. The rule lives in [`crate::contextual`],
    /// so the formatter and the server read it too.
    pub(in super::super) fn match_follows(&self) -> bool {
        crate::contextual::match_follows(self.src, self.toks, self.pos)
    }

    pub(super) fn match_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump();
        let scrutinees = self.expr_list()?;
        self.expect("with")?;
        let mut arms = Vec::new();
        let mut default = None;

        loop {
            if self.at("end") {
                self.bump();

                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated match, expected `end`"));
            }

            if self.at("case") {
                let arm_start = self.bump();
                let (patterns, guard) = self.arm_head()?;
                self.expect("then")?;
                let block = self.match_block()?;
                arms.push(MatchArm {
                    patterns,
                    guard,
                    block,
                    span: TokSpan::new(arm_start, self.pos),
                });

                continue;
            }

            if self.at("default") {
                self.bump();
                default = Some(self.match_block()?);

                continue;
            }

            return Err(self.err(&format!(
                "expected `case`, `default`, or `end`, found {}",
                self.found()
            )));
        }

        Ok(Stmt::Match(MatchStmt {
            scrutinees,
            arms,
            default,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// The patterns of an arm, one per scrutinee, and the `and` guard.
    fn arm_head(&mut self) -> Result<(Vec<Pattern>, Option<Expr>), ParseError> {
        let mut patterns = vec![self.pattern()?];

        while self.eat(",") {
            patterns.push(self.pattern()?);
        }

        let guard = if self.eat("and") {
            Some(self.expr()?)
        } else {
            None
        };

        Ok((patterns, guard))
    }

    /// A block that ends at the next `case`, `default`, or `end`.
    fn match_block(&mut self) -> Result<Block, ParseError> {
        self.in_match_arm += 1;
        let r = self.block();
        self.in_match_arm -= 1;

        r
    }

    /// The expression form: each arm is one expression.
    pub(in super::super) fn match_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.bump();
        let scrutinees = self.expr_list()?;
        self.expect("with")?;
        let mut arms = Vec::new();
        let mut default = None;

        loop {
            if self.at("end") {
                self.bump();

                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated match, expected `end`"));
            }

            if self.at("case") {
                let arm_start = self.bump();
                let (patterns, guard) = self.arm_head()?;
                self.expect("then")?;
                let value = self.expr()?;
                arms.push(MatchExprArm {
                    patterns,
                    guard,
                    value,
                    span: TokSpan::new(arm_start, self.pos),
                });

                continue;
            }

            if self.at("default") {
                self.bump();
                default = Some(Box::new(self.expr()?));

                continue;
            }

            return Err(self.err(&format!(
                "expected `case`, `default`, or `end`, found {}",
                self.found()
            )));
        }

        Ok(Expr::Match(Box::new(MatchExpr {
            scrutinees,
            arms,
            default,
            span: TokSpan::new(start, self.pos),
        })))
    }

    /// Whether a declaration follows `export default`. Anything else
    /// is an expression: `export default function() end` is a value.
    pub(super) fn default_decl_follows(&self) -> bool {
        match self.text() {
            "function" => self.name_at(1),

            "async" => self.text_at(1) == "function" && self.name_at(2),

            "struct" | "enum" | "trait" | "interface" | "class" | "remote" | "macro" | "local"
            | "const" | "type" => self.name_at(1),

            _ => false,
        }
    }
}
