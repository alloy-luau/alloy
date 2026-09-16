//! The `match` statement and the `match` expression.

use super::super::*;

const CASE_AFTER_DEFAULT: &str = "a `case` arm cannot follow `default`; move `default` last";
const TWO_DEFAULTS: &str =
    "a `default` arm cannot follow `default`; a `match` takes one `default` arm";
const DEFAULT_ALONE: &str = "a `match` needs a `case` arm before `default`";
const ALIAS_NEEDS_NAME: &str = "expected a name after `as`; write `match e as name with`";

/// Every name a pattern binds, in order. A field with no pattern binds
/// itself, and `...rest` binds the tail.
fn bind_spans(p: &Pattern, out: &mut Vec<TokSpan>) {
    match p {
        Pattern::Bind(n) => out.push(*n),

        Pattern::Variant { args, .. } => {
            for a in args {
                bind_spans(a, out);
            }
        }

        Pattern::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(inner) => bind_spans(inner, out),

                    None => out.push(f.field),
                }
            }
        }

        Pattern::Array { items, rest, .. } => {
            for i in items {
                bind_spans(i, out);
            }

            if let Some(r) = rest {
                out.push(*r);
            }
        }

        Pattern::Or(a, b, _) => {
            bind_spans(a, out);
            bind_spans(b, out);
        }

        Pattern::Wildcard(_) | Pattern::Literal(_) | Pattern::Path(_) => {}
    }
}

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
        let (scrutinees, aliases) = self.match_head()?;
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
                if default.is_some() {
                    self.bad_arm(CASE_AFTER_DEFAULT)?;
                }

                let arm_start = self.bump();
                let (patterns, guard) = self.arm_head()?;
                self.check_alias_binds(&aliases, &patterns)?;
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
                if default.is_some() {
                    self.bad_arm(TWO_DEFAULTS)?;
                } else if arms.is_empty() {
                    self.bad_arm(DEFAULT_ALONE)?;
                }

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
            aliases,
            arms,
            default,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// The scrutinees of a match head, each with its optional `as`
    /// alias. `as` opens no expression in Alloy, so the name after it
    /// belongs to the head.
    fn match_head(&mut self) -> Result<(Vec<Expr>, Vec<Option<TokSpan>>), ParseError> {
        let mut scrutinees = Vec::new();
        let mut aliases = Vec::new();

        loop {
            scrutinees.push(self.expr()?);
            aliases.push(self.match_alias()?);

            if !self.eat(",") {
                break;
            }
        }

        Ok((scrutinees, aliases))
    }

    /// The `as name` after one scrutinee, if the head wrote one.
    fn match_alias(&mut self) -> Result<Option<TokSpan>, ParseError> {
        if !self.at("as") {
            return Ok(None);
        }

        self.bump();

        // `with` opens the arms, so it is the word the head ends on and
        // never the alias, however free a name it is elsewhere.
        if !self.at_name() || self.at("with") {
            return Err(self.err(ALIAS_NEEDS_NAME));
        }

        let at = self.bump();

        Ok(Some(TokSpan::new(at, at + 1)))
    }

    /// Reports a pattern that binds a name the head already aliased.
    /// Both names would read one value on one line.
    fn check_alias_binds(
        &mut self,
        aliases: &[Option<TokSpan>],
        patterns: &[Pattern],
    ) -> Result<(), ParseError> {
        if aliases.iter().all(Option::is_none) {
            return Ok(());
        }

        let names: Vec<&str> = aliases
            .iter()
            .flatten()
            .map(|a| self.span_text(*a))
            .collect();
        let mut binds = Vec::new();

        for p in patterns {
            bind_spans(p, &mut binds);
        }

        for b in binds {
            let name = self.span_text(b);

            if !names.contains(&name) {
                continue;
            }

            let message =
                format!("`{name}` is the alias of the match; a pattern cannot bind it again");
            let offset = self.toks[b.start as usize].start as usize;

            if self.lenient {
                self.report_at(offset, &message);

                continue;
            }

            return Err(ParseError { offset, message });
        }

        Ok(())
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

    /// Reports an arm list the emit cannot read: `default` ends the list
    /// and comes once, after at least one `case`. The emit reads the arms
    /// in order, so any other shape leaks its source into the output. A
    /// lenient parse reports it and reads the arm, which keeps the rest
    /// of the file.
    fn bad_arm(&mut self, message: &'static str) -> Result<(), ParseError> {
        if self.lenient {
            self.report(message);

            return Ok(());
        }

        Err(self.err(message))
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
        let (scrutinees, aliases) = self.match_head()?;
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
                if default.is_some() {
                    self.bad_arm(CASE_AFTER_DEFAULT)?;
                }

                let arm_start = self.bump();
                let (patterns, guard) = self.arm_head()?;
                self.check_alias_binds(&aliases, &patterns)?;
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
                if default.is_some() {
                    self.bad_arm(TWO_DEFAULTS)?;
                } else if arms.is_empty() {
                    self.bad_arm(DEFAULT_ALONE)?;
                }

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
            aliases,
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
