//! The `match` statement and the `match` expression.

use super::super::*;

const CASE_AFTER_DEFAULT: &str = "a `case` arm cannot follow `default`; move `default` last";
const TWO_DEFAULTS: &str =
    "a `default` arm cannot follow `default`; a `match` takes one `default` arm";
const DEFAULT_ALONE: &str = "a `match` needs a `case` arm before `default`";
const ALIAS_NEEDS_NAME: &str = "expected a name after `as`; write `match e as name with`";
const ARM_GIVES_NO_VALUE: &str =
    "this arm gives no value: end it with the value, or leave with `return`";
const RUST_ARM: &str = "an arm reads `case Ok(v) then ...`; `=>` after a pattern is Rust's arm";
const STMT_ARM_TAKES_STATEMENT: &str = "a statement arm takes a statement; write `local x = match ... with` to read the arms as values";
const BARE_CASE: &str = "expected a pattern after `case`";

/// The patterns of an arm, one per scrutinee, and its guard.
type ArmHead = (Vec<Pattern>, Option<Expr>);

/// Whether an expression stands alone as a statement: a call, the three
/// words that wrap one, a macro call, `$assert(x)`, and a `match`, which
/// in statement position is the statement form.
fn stands_alone(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Call { .. }
            | Expr::New { .. }
            | Expr::Try { .. }
            | Expr::Await { .. }
            | Expr::Macro { .. }
            | Expr::Match(_)
    )
}

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
        let reports = self.diagnostics.len();
        self.bump();
        let (scrutinees, aliases) = self.match_head()?;
        self.expect("with")?;
        let mut arms = Vec::new();
        let mut default = None;
        // A `case` counts though its arm is dropped: a head the lenient
        // parse could not read is not a missing arm.
        let mut cased = false;

        loop {
            if self.at("end") {
                self.bump();

                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated match, expected `end`"));
            }

            if self.at("case") {
                cased = true;

                if default.is_some() {
                    self.bad_arm(CASE_AFTER_DEFAULT)?;
                }

                let arm_start = self.bump();

                if self.bare_case(arm_start)? {
                    continue;
                }

                let Some((patterns, guard)) = self.arm_head_or_skip(arm_start)? else {
                    continue;
                };
                self.check_alias_binds(&aliases, &patterns)?;
                // A broken head, one the author is still typing, keeps no
                // arm: the emit reads an arm up to its `then`.
                if !self.arm_then()? {
                    continue;
                }

                let block = self.arm_block()?;
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
                } else if !cased {
                    self.bad_arm(DEFAULT_ALONE)?;
                }

                self.bump();
                default = Some(self.arm_block()?);

                continue;
            }

            if self.rust_arm_ahead() {
                return Err(self.err(RUST_ARM));
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
            recovered: self.diagnostics.len() > reports,
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
            self.match_head += 1;
            let scrutinee = self.expr();
            self.match_head -= 1;
            scrutinees.push(scrutinee?);
            aliases.push(self.match_alias()?);

            if !self.eat(",") {
                break;
            }
        }

        self.check_alias_names(&aliases)?;

        Ok((scrutinees, aliases))
    }

    /// The `as name` after one scrutinee, if the head wrote one.
    ///
    /// A lenient parse of a head that still closes reports the missing
    /// name, drops the alias, and reads on: the arms and the `end` of
    /// the match then parse, so one mistake is one report. A head no
    /// `with` closes is the error node of its line instead.
    fn match_alias(&mut self) -> Result<Option<TokSpan>, ParseError> {
        if !self.at("as") {
            return Ok(None);
        }

        self.bump();

        // `with` opens the arms, so it is the word the head ends on and
        // never the alias, however free a name it is elsewhere.
        if self.at_name() && !self.at("with") {
            let at = self.bump();

            return Ok(Some(TokSpan::new(at, at + 1)));
        }

        if !self.lenient
            || !crate::contextual::with_closes_scrutinees(self.src, self.toks, self.pos - 1)
        {
            return Err(self.err(ALIAS_NEEDS_NAME));
        }

        self.report(ALIAS_NEEDS_NAME);

        while !self.at_end() && !self.at("with") && !self.at(",") {
            self.bump();
        }

        Ok(None)
    }

    /// Reports a name two values of one head share. The second would
    /// shadow the first, and both stand on one line.
    fn check_alias_names(&mut self, aliases: &[Option<TokSpan>]) -> Result<(), ParseError> {
        let mut seen: Vec<&str> = Vec::new();

        for a in aliases.iter().flatten() {
            let name = self.span_text(*a);

            if seen.contains(&name) {
                self.alias_error(
                    *a,
                    format!(
                        "the alias `{name}` is already the alias of another value of this match; give each value its own name"
                    ),
                )?;

                continue;
            }

            seen.push(name);
        }

        Ok(())
    }

    /// Reports a mistake about an alias, on the name it is about. A
    /// lenient parse reads on, so the rest of the file still reports.
    fn alias_error(&mut self, at: TokSpan, message: String) -> Result<(), ParseError> {
        let offset = self.toks[at.start as usize].start as usize;

        if self.lenient {
            self.report_at(offset, &message);

            return Ok(());
        }

        Err(ParseError { offset, message })
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

            self.alias_error(
                b,
                format!("`{name}` is the alias of the match; a pattern cannot bind it again"),
            )?;
        }

        Ok(())
    }

    /// The patterns of an arm, one per scrutinee, and the guard. `where`
    /// is the guard's word, as on a `for` filter; `and` is the old one,
    /// and a lint rewrites it.
    fn arm_head(&mut self) -> Result<ArmHead, ParseError> {
        let mut patterns = vec![self.pattern()?];

        while self.eat(",") {
            patterns.push(self.pattern()?);
        }

        let guard = if self.eat("where") || self.eat("and") {
            Some(self.expr()?)
        } else {
            None
        };

        Ok((patterns, guard))
    }

    /// The patterns and the guard of an arm. A lenient parse that meets
    /// a mistake in them reports it once and moves to the next arm, so
    /// the arms around it and the match's `end` report nothing.
    fn arm_head_or_skip(&mut self, arm_start: usize) -> Result<Option<ArmHead>, ParseError> {
        let e = match self.arm_head() {
            Ok(head) => return Ok(Some(head)),

            Err(e) if !self.lenient => return Err(e),

            Err(e) => e,
        };

        // The read of a head the author is still typing runs on into
        // the next arm, so the skip starts over at the head.
        self.pos = arm_start + 1;
        let e = self.unclosed_head().unwrap_or(e);
        self.report_at(e.offset, &e.message);
        self.skip_to_next_arm();

        Ok(None)
    }

    /// The report for an arm head that leaves a bracket open, on the
    /// last one: `case Hit.Block(` above the next arm. The head ends at
    /// its `then`, or at a line that opens with an arm or the `end`.
    fn unclosed_head(&self) -> Option<ParseError> {
        let text = |i: usize| self.toks[i].text(self.src);
        let mut stack = Vec::new();
        // A guard can hold an `if` expression, whose `then` is its own.
        let mut ifs = 0usize;

        for i in self.pos..self.toks.len() {
            let line_start = crate::contextual::newline_after(self.src, self.toks, i - 1);

            match text(i) {
                "case" | "default" | "end" if line_start => break,

                "if" => ifs += 1,

                "then" if ifs > 0 => ifs -= 1,

                "then" => break,

                "(" | "{" | "[" => stack.push(i),

                ")" | "}" | "]" => {
                    stack.pop();
                }

                _ => {}
            }
        }

        let last = *stack.last()?;
        let closers: String = stack
            .iter()
            .rev()
            .map(|&i| match text(i) {
                "{" => '}',

                "[" => ']',

                _ => ')',
            })
            .collect();

        Some(ParseError {
            offset: self.toks[last].start as usize,
            message: format!(
                "this arm opens `{}` and never closes it; write `{closers}` before `then`",
                text(last)
            ),
        })
    }

    /// Whether the `case` at `at` has no pattern, as while the author
    /// types it: the next token ends the arm. A lenient parse reports it
    /// on the `case` and reads on, so the other arms and the `end` of the
    /// match report nothing.
    fn bare_case(&mut self, at: usize) -> Result<bool, ParseError> {
        if !self.arm_ends() {
            return Ok(false);
        }

        let offset = self.toks[at].start as usize;

        if !self.lenient {
            return Err(ParseError {
                offset,
                message: BARE_CASE.to_string(),
            });
        }

        self.report_at(offset, BARE_CASE);

        Ok(true)
    }

    /// The `then` after an arm's patterns. `=>` there is Rust's arm.
    fn arm_then(&mut self) -> Result<bool, ParseError> {
        if self.at("=>") {
            return Err(self.err(RUST_ARM));
        }

        // A half-typed guard, `case n wh`, is the editor's everyday text.
        // A lenient parse reports it once and moves to the next arm, so
        // the arms around it and the match's `end` report nothing. Two
        // spellings from other languages get the Alloy form.
        let message = match self.text() {
            "then" => None,

            "if" => Some(
                "a guard reads `where`, the word a `for` filter takes: `case n where n > 5 then`"
                    .to_string(),
            ),

            "|" => Some("alternatives join with `or`: `case A or B then`".to_string()),

            _ => Some(format!(
                "expected `then` after the arm's patterns, found {}",
                self.found()
            )),
        };

        if let Some(message) = &message
            && !self.lenient
            && !self.at("then")
            && matches!(self.text(), "if" | "|")
        {
            return Err(self.err(message));
        }

        if self.lenient
            && let Some(message) = message
        {
            self.report(&message);

            // A guard's `if` opens no block that an `end` closes.
            if self.at("if") {
                self.bump();
            }

            self.skip_to_next_arm();

            return Ok(false);
        }

        self.expect("then").map(|_| true)
    }

    /// The value of an expression arm, read as a value position: a line
    /// that opens with a string, `{`, or `[` ends it. An arm that holds
    /// one expression is that expression; one that runs statements first
    /// is a block whose last line is the value.
    fn arm_value(&mut self) -> Result<Expr, ParseError> {
        if self.arm_is_one_expression() {
            self.value_lines += 1;
            let value = self.expr();
            self.value_lines -= 1;

            return value;
        }

        let start = self.pos;
        self.value_block = true;
        self.in_match_arm += 1;
        let block = self.block();
        self.in_match_arm -= 1;
        let block = block?;

        // The arm gives a value or leaves; a last line that does neither
        // would leave the binding nil.
        if !matches!(
            block.stmts.last(),
            Some(Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_))
        ) {
            self.bad_arm(ARM_GIVES_NO_VALUE)?;
        }

        Ok(Expr::Block {
            block,
            span: TokSpan::new(start, self.pos),
        })
    }

    /// Whether the arm at the cursor is one expression up to the next
    /// arm. The read runs ahead and rewinds, as `arm_is_value` does.
    fn arm_is_one_expression(&mut self) -> bool {
        // A statement word opens a block arm; `continue` would otherwise
        // read as a name.
        if matches!(
            self.text(),
            "return" | "break" | "local" | "const" | "for" | "while" | "repeat" | "do"
        ) || (self.at("continue") && self.continue_is_keyword())
        {
            return false;
        }

        let save = self.pos;
        let reports = self.diagnostics.len();
        let edits = self.type_edits.len();
        let names = self.type_names.len();
        self.value_lines += 1;
        let one = self.expr().is_ok()
            && (self.at_end() || matches!(self.text(), "case" | "default" | "end"));
        self.value_lines -= 1;

        self.pos = save;
        self.diagnostics.truncate(reports);
        self.type_edits.truncate(edits);
        self.type_names.truncate(names);

        one
    }

    /// Moves past a broken arm head to the next `case`, `default`, or the
    /// match's `end`, over the blocks nested in the arm.
    fn skip_to_next_arm(&mut self) {
        let mut depth = 0usize;

        while !self.at_end() {
            match self.text() {
                "case" | "default" if depth == 0 => return,

                "end" if depth == 0 => return,

                "end" => depth -= 1,

                "if" | "function" | "do" => depth += 1,

                _ => {}
            }

            self.bump();
        }
    }

    /// Whether the line at the cursor holds a `=>`: `Ok(v) => f(v)`.
    fn rust_arm_ahead(&self) -> bool {
        (0..)
            .take_while(|&n| n == 0 || !self.newline_after(n - 1))
            .take_while(|&n| self.pos + n < self.toks.len())
            .any(|n| self.text_at(n) == "=>")
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

    /// Whether the cursor stands where an arm ends: the next `case`,
    /// the `default`, or the `end` of the match.
    fn arm_ends(&self) -> bool {
        self.at_end() || matches!(self.text(), "case" | "default" | "end")
    }

    /// Whether the whole arm body is one value, `case "a" then 5`. The
    /// block reader takes the value for a broken statement and asks for
    /// a name, so the arm reports the expectation itself.
    ///
    /// The read runs ahead and rewinds, so it costs one extra parse of
    /// the first expression of every statement arm.
    fn arm_is_value(&mut self) -> bool {
        // `continue` is a name to the expression reader, and a
        // statement here.
        if self.at("continue") {
            return false;
        }

        let save = self.pos;
        let reports = self.diagnostics.len();
        let edits = self.type_edits.len();
        let names = self.type_names.len();
        let value = matches!(self.expr(), Ok(e) if !stands_alone(&e)) && self.arm_ends();

        self.pos = save;
        self.diagnostics.truncate(reports);
        self.type_edits.truncate(edits);
        self.type_names.truncate(names);

        value
    }

    /// The block of a statement arm. A value where the statement goes
    /// reports once, and the read moves past the value so the rest of
    /// the match still parses.
    fn arm_block(&mut self) -> Result<Block, ParseError> {
        if !self.arm_is_value() {
            return self.match_block();
        }

        let start = self.pos;
        self.bad_arm(STMT_ARM_TAKES_STATEMENT)?;
        let _ = self.expr();
        let span = TokSpan::new(start, self.pos);

        Ok(Block {
            stmts: vec![Stmt::Error(span)],
            span,
        })
    }

    /// The expression form: each arm is one expression, or statements
    /// that end in the value ([`Self::arm_value`]).
    pub(in super::super) fn match_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let reports = self.diagnostics.len();
        self.bump();
        let (scrutinees, aliases) = self.match_head()?;
        self.expect("with")?;
        let mut arms = Vec::new();
        let mut default = None;
        // A `case` counts though its arm is dropped: a head the lenient
        // parse could not read is not a missing arm.
        let mut cased = false;

        loop {
            if self.at("end") {
                self.bump();

                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated match, expected `end`"));
            }

            if self.at("case") {
                cased = true;

                if default.is_some() {
                    self.bad_arm(CASE_AFTER_DEFAULT)?;
                }

                let arm_start = self.bump();

                if self.bare_case(arm_start)? {
                    continue;
                }

                let Some((patterns, guard)) = self.arm_head_or_skip(arm_start)? else {
                    continue;
                };
                self.check_alias_binds(&aliases, &patterns)?;
                // A broken head, one the author is still typing, keeps no
                // arm: the emit reads an arm up to its `then`.
                if !self.arm_then()? {
                    continue;
                }

                let value = self.arm_value()?;
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
                } else if !cased {
                    self.bad_arm(DEFAULT_ALONE)?;
                }

                self.bump();
                default = Some(Box::new(self.arm_value()?));

                continue;
            }

            if self.rust_arm_ahead() {
                return Err(self.err(RUST_ARM));
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
            recovered: self.diagnostics.len() > reports,
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
