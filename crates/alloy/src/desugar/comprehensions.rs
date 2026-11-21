//! Comprehensions and chained comparisons.
//!
//! `[ value for x in xs if cond ]` builds an Array with one loop. The
//! loop runs inside a function the expression calls at once, so the
//! parts of the statement around it keep the order the source gives
//! them. The loop header means what it means in a `for` loop: one name
//! over a table is the key, and one name over an Iter or a generator is
//! the item. The emit reads no type, so it cannot give one name a
//! meaning of its own. The feature sits behind
//! `AlloyExperimentalComprehensionSyntax`.
//!
//! `a < b <= c` is `a < b and b <= c`, and each operand runs one time.
//! Luau reads the same text as `(a < b) < c`, which compares a boolean
//! and fails, so no working program changes. The feature sits behind
//! `AlloyExperimentalChainedComparisonSyntax`; with the flag off the
//! text goes to Luau as it is.

use std::collections::HashSet;

use alloy_syntax::ast::{Comprehension, Expr, Stmt, TokSpan};

use super::{Child, Desugar, expr_children, expr_needs_desugar, stmt_children};

/// The operators that chain. `==` and `~=` do not: `a == b == c` is a
/// comparison of two booleans in Luau, and it runs.
const ORDER: [&str; 4] = ["<", "<=", ">", ">="];

/// Whether an operand can be written two times with no second
/// evaluation.
fn is_plain(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Name(_) | Expr::Number(_) | Expr::String(_) | Expr::True(_) | Expr::False(_)
    )
}

impl<'s> Desugar<'s> {
    fn is_order(&self, op: TokSpan) -> bool {
        ORDER.contains(&self.text_of(op))
    }

    /// The operands of a chained comparison, the first one first, and
    /// the operator after each operand but the last. `None` for any
    /// other expression, and for every expression with the flag off.
    pub(crate) fn chain_of<'e>(&self, e: &'e Expr) -> Option<(Vec<&'e Expr>, Vec<TokSpan>)> {
        if !self.chained {
            return None;
        }

        let Expr::Binary { op, lhs, .. } = e else {
            return None;
        };

        if !self.is_order(*op)
            || !matches!(lhs.as_ref(), Expr::Binary { op, .. } if self.is_order(*op))
        {
            return None;
        }

        let mut operands = Vec::new();
        let mut ops = Vec::new();
        let mut at = e;

        while let Expr::Binary { op, lhs, rhs, .. } = at
            && self.is_order(*op)
        {
            operands.push(rhs.as_ref());
            ops.push(*op);
            at = lhs;
        }

        operands.push(at);
        operands.reverse();
        ops.reverse();

        Some((operands, ops))
    }

    /// Whether a statement holds a chained comparison.
    pub(crate) fn holds_chain(&self, s: &Stmt) -> bool {
        self.chained && stmt_children(s).iter().any(|c| self.chain_in(c))
    }

    fn chain_in(&self, c: &Child<'_>) -> bool {
        match c {
            Child::Expr(e) => {
                self.chain_of(e).is_some() || expr_children(e).iter().any(|c| self.chain_in(c))
            }

            Child::Block(b) => b.stmts.iter().any(|s| self.holds_chain(s)),

            Child::Function(f) => f.block.stmts.iter().any(|s| self.holds_chain(s)),
        }
    }

    /// Writes a chained comparison as its pairs joined with `and`.
    pub(crate) fn comparison_chain(&mut self, e: &Expr, operands: &[&Expr], ops: &[TokSpan]) {
        let anchor = self.byte_start(e.span());
        let end = self.byte_end(e.span());
        let up = |d: &Self, op: TokSpan| d.text_of(op).starts_with('<');

        if let Some(op) = ops.iter().find(|op| up(self, **op) != up(self, ops[0])) {
            self.diagnose(
                *op,
                "a chained comparison goes one way: `<` and `<=`, or `>` and `>=`",
            );
        }

        let middle = &operands[1..operands.len() - 1];

        // Each middle operand is written two times, so it must cost
        // nothing to read again, and no operand may ask for a hoist.
        if middle.iter().all(|o| is_plain(o)) && !operands.iter().any(|o| expr_needs_desugar(o)) {
            self.generate(anchor, "(");
            self.expr(operands[0]);

            for (i, pair) in operands.windows(2).enumerate() {
                self.copy(
                    self.byte_end(pair[0].span()),
                    self.byte_start(pair[1].span()),
                );
                self.expr(pair[1]);

                if i + 1 < ops.len() {
                    let again = self.render_to_string(pair[1]);
                    self.generate(self.byte_end(pair[1].span()), &format!(" and {again}"));
                }
            }

            self.generate(end, ")");

            return;
        }

        // A closure reads no `...` of the function around it.
        let (open, close) = if super::any_part(e, &|x| matches!(x, Expr::Vararg(_))) {
            ("(function(...) ", " end)(...)")
        } else {
            ("(function() ", " end)()")
        };
        self.generate(anchor, open);

        for (i, operand) in operands.iter().enumerate() {
            let (hoists, side) = self.pipe_stage(operand, None);
            let at = self.byte_start(operand.span());
            self.write_hoists(hoists, true);

            match i {
                0 => self.generate(at, "local __l = "),

                1 => self.generate(at, "local __r = "),

                _ => self.generate(at, "__l = __r  __r = "),
            }

            self.r.append(side);
            self.generate(self.byte_end(operand.span()), "  ");

            if i > 0 {
                let op = self.text_of(ops[i - 1]);
                let text = match i + 1 == operands.len() {
                    true => format!("return __l {op} __r"),

                    false => format!("if not (__l {op} __r) then return false end  "),
                };
                self.generate(self.byte_end(operand.span()), &text);
            }

            // The text up to the next operand holds the newlines.
            if let Some(next) = operands.get(i + 1) {
                self.blank_lines(self.byte_end(operand.span()), self.byte_start(next.span()));
            }
        }

        self.generate(end, close);
    }

    /// Writes a comprehension as a loop inside a function the
    /// expression calls at once.
    pub(crate) fn comprehension(&mut self, e: &Expr, c: &Comprehension) {
        let Comprehension {
            value,
            vars,
            exprs,
            numeric,
            filter,
            span,
        } = c;
        let anchor = self.byte_start(*span);
        let end = self.byte_end(*span);
        let for_tok = TokSpan::new(value.span().end as usize, value.span().end as usize + 1);
        let fflags = &self.options.fflags;

        if !fflags.on(&crate::fflags::COMPREHENSION_SYNTAX) {
            self.diagnose(
                for_tok,
                &format!(
                    "a comprehension is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                    crate::fflags::COMPREHENSION_SYNTAX.name
                ),
            );
            let nil = self.any_cast("nil");
            self.generate(anchor, &nil);

            return;
        }

        if let Some(v) = vars.iter().find(|v| v.destructure.is_some()) {
            self.diagnose(
                v.name,
                "a comprehension takes plain loop names; destructure the item in a `for` loop",
            );
            let nil = self.any_cast("nil");
            self.generate(anchor, &nil);

            return;
        }

        // With the analysis flag off the checker reads the items as `any`.
        let typed = fflags.on(&crate::fflags::COMPREHENSION_ANALYSIS);
        let untyped = self.options.check && !typed;
        self.comp_next += 1;
        let list = format!("__list{}", self.comp_next);
        // A closure reads no `...` of the function around it.
        let (open, close) = if super::any_part(e, &|x| matches!(x, Expr::Vararg(_))) {
            ("(function(...) ", ")(...)")
        } else {
            ("(function() ", ")()")
        };
        let ty = if untyped { ": { any }" } else { "" };
        let std = self.std();
        // `Array.from` takes the list outside the function. Inside, the
        // checker has not yet settled the type of the list, and the
        // call would read its items as `unknown`.
        let from = match untyped {
            true => format!("({std}.Array.from("),

            false => format!("{std}.Array.from("),
        };
        self.generate(anchor, &format!("{from}{open}local {list}{ty} = {{}} "));

        // The header: `for`, the names, and the iterator expressions.
        let for_start = self.byte_start(for_tok);
        let first_expr = self.byte_start(exprs[0].span());
        let last_expr = exprs
            .last()
            .map(|x| self.byte_end(x.span()))
            .unwrap_or(first_expr);
        self.blank_lines(anchor, self.byte_start(value.span()));
        self.blank_lines(self.byte_end(value.span()), for_start);
        self.copy(for_start, first_expr);
        let children: Vec<Child<'_>> = exprs.iter().map(Child::Expr).collect();
        let header = u32::from(!*numeric);
        self.for_header += header;
        self.stitch_between(first_expr, last_expr, &children);
        self.for_header -= header;
        self.generate(last_expr, " do ");

        // The filter and the value read the names of the loop.
        self.scopes.push(HashSet::new());

        for v in vars {
            self.declare_binding(v);
        }

        let mut cursor = last_expr;

        if let Some(c) = filter {
            let at = self.byte_start(c.span());
            self.blank_lines(cursor, at);
            let (hoists, side) = self.pipe_stage(c, None);
            self.write_hoists(hoists, true);
            self.generate(at, "if ");
            self.r.append(side);
            cursor = self.byte_end(c.span());
            self.generate(cursor, " then ");
        }

        let (hoists, side) = self.pipe_stage(value, None);
        self.write_hoists(hoists, true);
        self.generate(cursor, &format!("{list}[#{list} + 1] = "));
        self.r.append(side);
        self.scopes.pop();
        self.blank_lines(cursor, end);

        let guard = if filter.is_some() { " end" } else { "" };
        let cast = if untyped { " :: any)" } else { "" };
        self.generate(end, &format!("{guard} end return {list} end{close}){cast}"));
    }
}
