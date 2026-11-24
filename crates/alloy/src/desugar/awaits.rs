//! Where an `await` may stand.
//!
//! `await` lowers to `alloy.await`, which calls `coroutine.yield` on the
//! running thread. The yield is safe only where the runtime owns that
//! thread: the body of an `async function`, an `async do` block, the
//! body of an `after` block, which `task.delay` runs on a thread of its
//! own, and the top level of a Roblox Script, which is a thread of its
//! own too. A function literal handed to a spawner runs on a thread of
//! its own as well: `task.spawn`, `task.defer`, `task.delay`, and the
//! `Connect` and `Once` of a signal, and the `on` and `once` of a
//! remote. Roblox and the std `Signal` both start each handler on a new
//! thread, and a remote's handler runs from a Roblox signal or invoke.
//!
//! Anywhere else the yield lands in the caller. At a module's top level
//! it lands in `require`. In a plain callback it lands wherever the
//! caller runs it, and a caller that is C code, such as `table.sort`,
//! kills the thread with "attempt to yield across metamethod/C-call
//! boundary". That crash does not reproduce off Roblox: with no `task`
//! library the std runs a Future to completion at once, so `await` never
//! yields and a standalone test suite passes.

use alloy_syntax::ast::{
    Block, CallArgs, ClassMember, DefaultExport, Expr, FunctionBody, IndexKey, Stmt, TokSpan,
};

use super::{Child, Desugar, expr_children, stmt_children};

/// The place an `await` sits in, which decides whether its yield is
/// safe.
enum Spot {
    /// A thread the runtime owns: an `async function` body, an
    /// `async do` block, an `after` block, a spawned function.
    Async,
    /// The top level of a `.server` or `.client` file, which Roblox
    /// runs as a Script on a thread of its own.
    Script,
    /// The top level of a module, which runs inside `require`.
    Module,
    /// The body of a `function*`. Its thread belongs to the reader of
    /// the generator, which resumes it for the next value.
    Generator,
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

            Spot::Generator => "`await` in a generator stops the body the way `yield` does, and the reader takes it as the end; `await` the value before the call, or in the loop that reads the generator".to_string(),

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

            Stmt::TypeAlias(t) => {
                if let Some(body) = &t.function {
                    self.type_function_body(body);
                }
            }

            Stmt::Local(l) if l.using => {
                if !self.options.fflags.on(&crate::fflags::USING_SYNTAX) {
                    let report = format!(
                        "`using` is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                        crate::fflags::USING_SYNTAX.name
                    );
                    self.diagnose(l.keyword, &report);
                } else if self.function_depth == 0 {
                    self.diagnose(
                        l.keyword,
                        "`using` drops its value when its function ends, and no function is open here; put it in a function",
                    );
                }

                for c in stmt_children(s) {
                    self.awaits_in_child(c, spot);
                }
            }

            Stmt::Defer { block, span } => {
                let word = TokSpan::new(span.start as usize, span.start as usize + 1);

                if !self.options.fflags.on(&crate::fflags::DEFER_SYNTAX) {
                    let report = format!(
                        "`defer` is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                        crate::fflags::DEFER_SYNTAX.name
                    );
                    self.diagnose(word, &report);
                } else if self.function_depth == 0 {
                    self.diagnose(
                        word,
                        "`defer` runs when its function ends, and no function is open here; put it in a function",
                    );
                }

                self.awaits_in_block(block, spot);
            }

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

    /// The type checker runs the body of a `type function`, and the
    /// emit copies it as written. A form the emit lowers elsewhere
    /// would reach Luau as it stands, so each one reports here. The
    /// body takes no lowering: most forms need the runtime or a name of
    /// the module, and a type function sees neither.
    fn type_function_body(&mut self, body: &FunctionBody) {
        const REPORT: &str = "a type function body is Luau: the type checker runs it and the compiler copies it as written; write this in Luau, without Alloy syntax";
        let start = self.byte_start(body.span);
        let end = self.byte_end(body.span);
        let word = |at: usize| TokSpan::new(at, at + 1);

        let at = if super::function_needs_rewrite(body) {
            Some(body.span.start as usize)
        } else if let Some(s) = super::first_alloy_stmt(&body.block) {
            Some(s.span().start as usize)
        } else {
            // Alloy syntax in a type, `T[]` or `~T`: the emit writes a
            // name of the module there, which the body cannot read.
            self.earliest_edit_start(start, end)
                .and_then(|byte| self.toks.iter().position(|t| t.start == byte))
        };

        if let Some(at) = at {
            self.diagnose(word(at), REPORT);
        }
    }

    /// A function body, under its own spot. A parameter default is read
    /// inside the body, so it takes the body's spot too.
    fn awaits_in_body(&mut self, body: &FunctionBody, name: Option<String>) {
        let spot = if body.is_async.is_some() {
            Spot::Async
        } else if body.generator.is_some() {
            Spot::Generator
        } else {
            Spot::Plain(name)
        };

        self.awaits_in_body_at(body, &spot);
    }

    /// A function body under a spot the caller gives: a spawner runs
    /// the literal it takes on a thread of its own.
    fn awaits_in_body_at(&mut self, body: &FunctionBody, spot: &Spot) {
        // Every function body passes here once, the literal a spawner
        // takes too. The reports of a `function*` and of a lambda sit
        // here and not in the emit, which may run twice.
        if let Some(star) = body.generator {
            if !self.options.fflags.on(&crate::fflags::GENERATOR_SYNTAX) {
                let report = format!(
                    "`function*` is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                    crate::fflags::GENERATOR_SYNTAX.name
                );
                self.diagnose(star, &report);
            } else if let Some(a) = body.is_async {
                self.diagnose(
                    a,
                    "a generator cannot be `async`; its reader resumes it, and a Future resumes on its own",
                );
            }
        }

        if body.lambda && !self.options.fflags.on(&crate::fflags::LAMBDA_SYNTAX) {
            let report = format!(
                "`|x| value` is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                crate::fflags::LAMBDA_SYNTAX.name
            );
            let bar = TokSpan::new(body.span.start as usize, body.span.start as usize + 1);
            self.diagnose(bar, &report);
        }

        for p in &body.params {
            if let Some(d) = &p.default {
                self.awaits_in_expr(d, spot);
            }
        }

        self.function_depth += 1;
        self.awaits_in_block(&body.block, spot);
        self.function_depth -= 1;
    }

    /// Whether a call starts each function it is handed on a thread of
    /// its own.
    fn spawns_threads(&self, func: &Expr, method: Option<TokSpan>) -> bool {
        match (method, func) {
            // The std `Signal` writes `connect` and `once` beside the
            // Roblox names, as the same functions.
            (Some(m), _) => matches!(self.text_of(m), "Connect" | "Once" | "connect" | "once"),

            // `Ping.on(handler)`: the runtime calls a remote's handler
            // from `OnServerEvent:Connect` or `OnServerInvoke`, and
            // Roblox runs each of those on a thread of its own.
            (
                None,
                Expr::Index {
                    object,
                    key: IndexKey::Field(verb),
                    ..
                },
            ) if matches!(self.text_of(*verb), "on" | "once") => {
                self.remote_named(object).is_some()
            }

            (None, _) => matches!(
                self.dotted_name(func).as_deref(),
                Some("task.spawn" | "task.defer" | "task.delay")
            ),
        }
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

            Expr::Bail { span, .. } if !self.options.fflags.on(&crate::fflags::BAIL_SYNTAX) => {
                let word = TokSpan::new(span.start as usize, span.start as usize + 1);
                let report = format!(
                    "`?? {}` is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                    self.text_of(word),
                    crate::fflags::BAIL_SYNTAX.name
                );
                self.diagnose(word, &report);
            }

            Expr::Call {
                func,
                method,
                args: CallArgs::Paren(args),
                ..
            } if self.spawns_threads(func, *method) => {
                self.awaits_in_expr(func, spot);

                for a in args {
                    match a {
                        Expr::Function { body, .. } => self.awaits_in_body_at(body, &Spot::Async),

                        _ => self.awaits_in_expr(a, spot),
                    }
                }

                return;
            }

            Expr::Await { span, operand } => {
                // The keyword alone, not the operand: the word is what
                // moves, and the operand can run for lines.
                let word = TokSpan::new(span.start as usize, span.start as usize + 1);

                // `await delayed()` of a plain function: the value is no
                // Future, and the runtime raises at the `await`.
                if let Some(name) = self.plain_callee(operand) {
                    let report = format!("`{name}` is not async; `await` takes a Future");
                    self.diagnose(word, &report);
                } else if !spot.allows_a_yield() {
                    let report = spot.report();
                    self.diagnose(word, &report);
                }
            }

            _ => {}
        }

        for c in expr_children(e) {
            self.awaits_in_child(c, spot);
        }
    }

    /// The plain function of this file the operand calls, when its
    /// declared return type is one this file knows is no Future.
    fn plain_callee(&self, operand: &Expr) -> Option<String> {
        let Expr::Call {
            func, method: None, ..
        } = operand
        else {
            return None;
        };
        let Expr::Name(n) = &**func else {
            return None;
        };
        let name = self.text_of(*n);
        let ty = self.fn_ret_types.get(name)?.trim();

        (self.plain_fns.contains(name) && self.known_type(ty)).then(|| name.to_string())
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
