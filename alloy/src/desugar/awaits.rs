//! Where an `await` may stand.
//!
//! `await` lowers to `alloy.await`, which calls `coroutine.yield` on the
//! running thread. The yield is safe only where the runtime owns that
//! thread: the body of an `async function`, an `async do` block, the
//! body of an `after` block, which `task.delay` runs on a thread of its
//! own, and the top level of a Roblox Script, which is a thread of its
//! own too.
//!
//! Anywhere else the yield lands in the caller. At a module's top level
//! it lands in `require`. In a plain callback it lands wherever the
//! caller runs it, and a caller that is C code, such as `table.sort`,
//! kills the thread with "attempt to yield across metamethod/C-call
//! boundary". That crash does not reproduce off Roblox: with no `task`
//! library the std runs a Future to completion at once, so `await` never
//! yields and a standalone test suite passes.

use alloy_syntax::ast::{Block, ClassMember, DefaultExport, Expr, FunctionBody, Stmt, TokSpan};

use super::{Child, Desugar, expr_children, stmt_children};

/// The place an `await` sits in, which decides whether its yield is
/// safe.
enum Spot {
    /// A thread the runtime owns: an `async function` body, an
    /// `async do` block, an `after` block.
    Async,
    /// The top level of a `.server` or `.client` file, which Roblox
    /// runs as a Script on a thread of its own.
    Script,
    /// The top level of a module, which runs inside `require`.
    Module,
    /// A plain function body, under the name it was declared with. A
    /// function boundary resets the context: an ordinary `function` in
    /// an `async function` is a plain one.
    Plain(Option<String>),
}

impl Spot {
    fn allows_a_yield(&self) -> bool {
        matches!(self, Spot::Async | Spot::Script)
    }

    /// The report for an `await` here. The two spots take two fixes, so
    /// they take two sentences.
    fn report(&self) -> String {
        match self {
            Spot::Plain(Some(name)) => format!(
                "`await` needs an async context; mark `{name}` as `async`, or wrap the body in `async do ... end`"
            ),

            Spot::Plain(None) => "`await` needs an async context; mark the function `async`, or wrap the body in `async do ... end`".to_string(),

            _ => "`await` at a module's top level yields inside `require`; wrap it in `async do ... end`".to_string(),
        }
    }
}

impl Desugar<'_> {
    /// Reports every `await` that sits where a yield is not safe.
    pub(crate) fn check_await_spots(&mut self, block: &Block) {
        let top = if crate::modules::is_script(&self.options.file_name) {
            Spot::Script
        } else {
            Spot::Module
        };

        self.awaits_in_block(block, &top);
    }

    fn awaits_in_block(&mut self, block: &Block, spot: &Spot) {
        for stmt in &block.stmts {
            self.awaits_in_stmt(stmt, spot);
        }
    }

    fn awaits_in_stmt(&mut self, s: &Stmt, spot: &Spot) {
        match s {
            Stmt::Function(f) => {
                let name = self.function_name(f);
                self.awaits_in_body(&f.body, name);
            }

            Stmt::LocalFunction(f) => {
                let name = Some(self.text_of(f.name).to_string());
                self.awaits_in_body(&f.body, name);
            }

            Stmt::Impl(i) => {
                for m in &i.methods {
                    let name = self.function_name(m);
                    self.awaits_in_body(&m.body, name);
                }
            }

            Stmt::Class(c) => {
                for m in &c.members {
                    if let ClassMember::Method(f) = m {
                        let name = self.function_name(f);
                        self.awaits_in_body(&f.body, name);
                    }
                }
            }

            Stmt::Trait(t) => {
                for m in &t.methods {
                    if let Some(body) = &m.body {
                        let name = Some(self.text_of(m.name).to_string());
                        self.awaits_in_body(body, name);
                    }
                }
            }

            // A macro body runs where the call is written, not here. The
            // expansion compiles on its own and reads its own spot.
            Stmt::Macro(_) => {}

            // A namespace renders its members one at a time, so the
            // generic walk never reaches them.
            Stmt::Namespace(ns) => {
                for m in &ns.members {
                    self.awaits_in_stmt(&m.stmt, spot);
                }
            }

            Stmt::ExportDefault {
                value: DefaultExport::Decl(inner),
                ..
            } => self.awaits_in_stmt(inner, spot),

            // `after n do ... end` is `task.delay(n, function() ... end)`.
            // The block and the `where` filter run on the timer's own
            // thread; the delay is read where the statement stands.
            Stmt::After(a) => {
                self.awaits_in_expr(&a.delay, spot);

                if let Some(f) = &a.filter {
                    self.awaits_in_expr(f, &Spot::Async);
                }

                self.awaits_in_block(&a.block, &Spot::Async);
            }

            _ => {
                for c in stmt_children(s) {
                    self.awaits_in_child(c, spot);
                }
            }
        }
    }

    fn awaits_in_child(&mut self, c: Child<'_>, spot: &Spot) {
        match c {
            Child::Expr(e) => self.awaits_in_expr(e, spot),

            Child::Block(b) => self.awaits_in_block(b, spot),

            Child::Function(f) => self.awaits_in_body(f, None),
        }
    }

    /// A function body, under its own spot. A parameter default is read
    /// inside the body, so it takes the body's spot too.
    fn awaits_in_body(&mut self, body: &FunctionBody, name: Option<String>) {
        let spot = if body.is_async.is_some() {
            Spot::Async
        } else {
            Spot::Plain(name)
        };

        for p in &body.params {
            if let Some(d) = &p.default {
                self.awaits_in_expr(d, &spot);
            }
        }

        self.awaits_in_block(&body.block, &spot);
    }

    fn awaits_in_expr(&mut self, e: &Expr, spot: &Spot) {
        match e {
            Expr::Function { body, .. } => {
                self.awaits_in_body(body, None);

                return;
            }

            Expr::AsyncBlock { block, .. } => {
                self.awaits_in_block(block, &Spot::Async);

                return;
            }

            Expr::Await { span, .. } if !spot.allows_a_yield() => {
                // The keyword alone, not the operand: the word is what
                // moves, and the operand can run for lines.
                let word = TokSpan::new(span.start as usize, span.start as usize + 1);
                let report = spot.report();
                self.diagnose(word, &report);
            }

            _ => {}
        }

        for c in expr_children(e) {
            self.awaits_in_child(c, spot);
        }
    }

    /// The name a `function` declaration writes, `V.new` and `V:len`
    /// included, as the source spells it.
    fn function_name(&self, f: &alloy_syntax::ast::Function) -> Option<String> {
        let first = *f.path.first()?;
        let last = *f.path.last()?;

        Some(
            self.text_of(TokSpan::new(first.start as usize, last.end as usize))
                .to_string(),
        )
    }
}
