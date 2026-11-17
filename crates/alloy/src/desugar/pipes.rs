//! The pipe operator: `value |> body`, where `_` in `body` reads `value`.
//!
//! Each stage but the last becomes a local on the line the stage starts
//! on, and each `_` takes the name of the local of the stage before it.
//! A pipe that is the one value of a `local`, a `const`, a `return`, or
//! a plain assignment writes those locals in front of the statement. A
//! pipe in any other place writes them inside a function it calls at
//! once, so the parts of the statement keep the order the source gives
//! them.
//!
//! The feature sits behind `AlloyExperimentalPipeOperator`. The RFC is
//! `syntax-pipe-operator` in alloy-luau/rfcs.

use alloy_syntax::ast::{Expr, IndexKey, Stmt, TokSpan};

use super::{Desugar, Hoist};
use crate::render::Renderer;

/// The piped value of one `|>` while its body renders.
pub(crate) struct Topic {
    /// The local that holds the value.
    name: String,
    /// The scope depth at the pipe. A `_` that a deeper scope declares
    /// is that local, not the piped value.
    depth: usize,
    /// How many times the body read `_`.
    reads: u32,
}

/// The stages of a pipe, the first value first, and the `|>` after each
/// stage but the last. `a |> b |> c` joins to the left, so the walk goes
/// down the `value` side.
fn stages(e: &Expr) -> (Vec<&Expr>, Vec<TokSpan>) {
    let mut list = Vec::new();
    let mut ops = Vec::new();
    let mut at = e;

    while let Expr::Pipe {
        value, op, body, ..
    } = at
    {
        list.push(body.as_ref());
        ops.push(*op);
        at = value;
    }

    list.push(at);
    list.reverse();
    ops.reverse();

    (list, ops)
}

/// Whether an assignment can read its target after the value with no
/// change in what the program does: a name, or one index of a name by a
/// field, a name, or a literal.
fn plain_target(e: &Expr) -> bool {
    match e {
        Expr::Name(_) => true,

        Expr::Index {
            object,
            key,
            optional: false,
            ..
        } => {
            matches!(object.as_ref(), Expr::Name(_))
                && match key {
                    IndexKey::Field(_) => true,

                    IndexKey::Computed(k) => {
                        matches!(
                            k.as_ref(),
                            Expr::Name(_) | Expr::Number(_) | Expr::String(_)
                        )
                    }
                }
        }

        _ => false,
    }
}

impl<'s> Desugar<'s> {
    fn pipes_on(&self) -> bool {
        self.options.fflags.on(&crate::fflags::PIPE_OPERATOR)
    }

    /// The stage local a `_` reads, when `name` is the piped value here.
    pub(crate) fn topic_read(&mut self, name: &str) -> Option<String> {
        if name != "_" {
            return None;
        }

        let depth = self.pipe_topics.last()?.depth;

        if self.is_local_since(depth, "_") {
            return None;
        }

        let topic = self.pipe_topics.last_mut()?;
        topic.reads += 1;

        Some(topic.name.clone())
    }

    /// Reports an assignment to `_` inside the body of a pipe.
    pub(crate) fn check_topic_write(&mut self, stmt: &Stmt) {
        let Stmt::Assign(a) = stmt else {
            return;
        };
        let Some(depth) = self.pipe_topics.last().map(|t| t.depth) else {
            return;
        };

        for target in &a.targets {
            if let Expr::Name(span) = target
                && self.text_of(*span) == "_"
                && !self.is_local_since(depth, "_")
            {
                self.diagnose(*span, "`_` is the piped value and cannot be assigned");
            }
        }
    }

    /// Renders one stage aside, with `topic` as its `_`, and gives back
    /// the hoists it asked for. The hoists go in front of the stage and
    /// not in front of the statement: they may read the stage before.
    fn pipe_stage(
        &mut self,
        stage: &Expr,
        topic: Option<String>,
    ) -> (Vec<Hoist<'s>>, Renderer<'s>) {
        let saved_hoists = std::mem::take(&mut self.hoists);
        let saved_flags = (self.lazy, self.effects, self.reads, self.in_place);
        (self.lazy, self.effects, self.reads, self.in_place) = (false, false, false, false);
        let bound = topic.is_some();

        if let Some(name) = topic {
            let depth = self.scope_depth();
            self.pipe_topics.push(Topic {
                name,
                depth,
                reads: 0,
            });
        }

        let side = self.render_to_side(stage);

        if bound
            && let Some(topic) = self.pipe_topics.pop()
            && topic.reads == 0
        {
            self.diagnose(stage.span(), "the body of `|>` never reads `_`");
        }

        let hoists = std::mem::replace(&mut self.hoists, saved_hoists);
        (self.lazy, self.effects, self.reads, self.in_place) = saved_flags;

        (hoists, side)
    }

    /// Writes each stage of `e` but the last as a local, on the line the
    /// stage starts on, and gives back the last stage, rendered.
    fn pipe_locals(&mut self, e: &Expr) -> (Vec<Hoist<'s>>, Renderer<'s>, u32) {
        let (list, ops) = stages(e);
        let mut topic = None;
        let last = list.len() - 1;

        for (i, stage) in list.iter().enumerate() {
            let (hoists, side) = self.pipe_stage(stage, topic.take());
            let start = self.byte_start(stage.span());

            if i == last {
                return (hoists, side, start);
            }

            self.pipe_next += 1;
            let name = format!("__pipe{}", self.pipe_next);
            self.write_hoists(hoists, true);
            self.generate(start, &format!("local {name} = "));
            self.r.append(side);
            self.r.end_stmt();
            // The text up to the `|>` holds the newline, and a comment
            // the author put after the stage.
            let op = self.byte_start(ops[i]);
            self.copy(self.byte_end(stage.span()), op);
            self.generate(op, " ");
            self.pipe_ops.push((op, name.clone()));
            topic = Some(name);
        }

        unreachable!("a pipe has a last stage")
    }

    /// A pipe in an expression. The stage locals go inside a function
    /// the expression calls at once.
    pub(crate) fn pipe_expr(&mut self, e: &Expr, op: TokSpan) {
        let anchor = self.byte_start(e.span());

        if !self.pipes_on() {
            self.diagnose(
                op,
                &format!(
                    "`|>` is not final; set `{} = true` under `[fflags]` in alloy.toml to use it",
                    crate::fflags::PIPE_OPERATOR.name
                ),
            );
            let nil = self.any_cast("nil");
            self.generate(anchor, &nil);

            return;
        }

        // A closure reads no `...` of the function around it.
        let (open, close) = if super::any_part(e, &|x| matches!(x, Expr::Vararg(_))) {
            ("(function(...) ", " end)(...)")
        } else {
            ("(function() ", " end)()")
        };
        self.generate(anchor, open);
        let (hoists, side, start) = self.pipe_locals(e);
        self.write_hoists(hoists, true);
        self.generate(start, "return ");
        self.r.append(side);
        self.generate(self.byte_end(e.span()), close);
    }

    /// The pipe a statement can hoist, with the byte its value starts
    /// at: the one value of a plain `local`, `const`, or `return`, or of
    /// an assignment to one plain target.
    pub(crate) fn hoisted_pipe<'e>(&self, stmt: &'e Stmt) -> Option<(u32, &'e Expr)> {
        if !self.pipes_on() {
            return None;
        }

        let value = match stmt {
            Stmt::Local(l)
                if !l.exported
                    && l.attrs.is_empty()
                    && l.names.iter().all(|b| b.destructure.is_none()) =>
            {
                l.values.as_slice()
            }

            // The check artifact casts a `return` inside a for-in
            // header, so that one keeps the function form.
            Stmt::Return(r) if !r.value_only && self.for_header == 0 => r.values.as_slice(),

            Stmt::Assign(a)
                if self.text_of(a.op) == "="
                    && matches!(a.targets.as_slice(), [t] if plain_target(t)) =>
            {
                a.values.as_slice()
            }

            _ => return None,
        };

        match value {
            [e @ Expr::Pipe { .. }] => Some((self.byte_start(e.span()), e)),

            _ => None,
        }
    }

    /// A statement whose one value is a pipe. The stage locals go in
    /// front of it, and the head of the statement, `local label =`,
    /// moves to the line of the last stage.
    pub(crate) fn pipe_stmt(&mut self, stmt: &Stmt, head_end: u32, value: &Expr) {
        let span = stmt.span();
        let head_start = self.byte_start(span);
        // The head renders aside with its source chunks, so the name it
        // declares still maps to where the author wrote it.
        let head = self.render_side(|d| match stmt {
            Stmt::Assign(a) => {
                let target = &a.targets[0];
                d.expr(target);
                d.copy(d.byte_end(target.span()), head_end);
            }

            _ => d.copy(head_start, head_end),
        });
        let (hoists, side, _) = self.pipe_locals(value);
        self.write_hoists(hoists, true);
        self.r.append(head);
        self.r.append(side);
        self.copy(self.byte_end(value.span()), self.byte_end(span));
    }
}

#[cfg(test)]
mod tests {
    fn compiled(src: &str) -> crate::Output {
        let options = crate::EmitOptions {
            fflags: toml::from_str("AlloyExperimentalPipeOperator = true").unwrap(),
            ..crate::EmitOptions::default()
        };

        crate::compile_with(src, &options).unwrap()
    }

    fn messages(out: &crate::Output) -> Vec<&str> {
        out.diagnostics.iter().map(|d| d.message.as_str()).collect()
    }

    /// The one value of a `local`, a `return`, and a plain assignment
    /// hoists: each stage is a local on its own line, and the head of
    /// the statement moves to the last stage.
    #[test]
    fn a_statement_hoists_its_pipe_one_stage_to_a_line() {
        let src = "local h = { hp = 5 }\nlocal label = h.hp\n    |> math.floor(_) -- whole\n    |> tostring(_)\nh.hp = 1 |> _ + 1\nlocal function f()\n    return 2 |> _ * 2\nend\nprint(label, f())\n";
        let out = compiled(src);
        assert_eq!(messages(&out), [] as [&str; 0]);

        let lines: Vec<&str> = out.ship.lines().map(str::trim).collect();
        let at = lines
            .iter()
            .position(|l| *l == "local __pipe1 = h.hp")
            .expect(&out.ship);
        assert_eq!(
            lines[at + 1],
            "local __pipe2 = math.floor(__pipe1) -- whole"
        );
        assert_eq!(lines[at + 2], "local label = tostring(__pipe2)");
        assert_eq!(lines[at + 3], "local __pipe3 = 1  h.hp = __pipe3 + 1");
        assert_eq!(lines[at + 5], "local __pipe4 = 2  return __pipe4 * 2");
        assert_eq!(
            out.ship.lines().count(),
            src.lines().count(),
            "{}",
            out.ship
        );
    }

    /// Any other place keeps the order of the statement: the stages go
    /// inside a function the expression calls at once. A body that
    /// reads `...` passes it in.
    #[test]
    fn a_pipe_in_an_expression_runs_in_place() {
        let src = "local t = {}\nfunction t.get() return t end\nprint(1 |> _ + 1)\nt.get().x = 2 |> _ * 2\nt.x += 3 |> _\nlocal function f(...)\n    print(4 |> select(_, ...))\nend\n";
        let out = compiled(src);
        assert_eq!(messages(&out), [] as [&str; 0]);

        for want in [
            "print((function() local __pipe1 = 1  return __pipe1 + 1 end)())",
            "t.get().x = (function() local __pipe2 = 2  return __pipe2 * 2 end)()",
            "t.x += (function() local __pipe3 = 3  return __pipe3 end)()",
            "print((function(...) local __pipe4 = 4  return select(__pipe4, ...) end)(...))",
        ] {
            assert!(out.ship.contains(want), "{want}\n{}", out.ship);
        }
    }

    /// `_` is the piped value in the whole body, a function literal
    /// included. A scope that declares its own `_` reads that one, and a
    /// pipe inside a body binds its own.
    #[test]
    fn the_topic_follows_the_scopes_of_the_body() {
        let src = "local a = 1 |> (function() return _ end)()\nlocal b = 2 |> (function(_) return _ end)(_)\nlocal c = 3 |> (function() for _, v in {} do end return _ end)()\nlocal d = 4 |> (5 |> _ + 1) + _\nprint(a, b, c, d)\n";
        let out = compiled(src);
        assert_eq!(messages(&out), [] as [&str; 0]);

        for want in [
            "local a = (function() return __pipe1 end)()",
            "local b = (function(_) return _ end)(__pipe2)",
            "local c = (function() for _, v in {} do end return __pipe3 end)()",
            "local __pipe5 = 5  return __pipe5 + 1 end)()) + __pipe4",
        ] {
            assert!(out.ship.contains(want), "{want}\n{}", out.ship);
        }
    }

    /// `|>` binds more loosely than every other operator, and the last
    /// branch of an `if` expression ends at it.
    #[test]
    fn a_pipe_takes_the_whole_expression_on_its_left() {
        let src = "local n = 2\nlocal a = n or 1 |> _ + 1\nlocal b = if n > 1 then 1 else 2 |> _ + 10\nlocal c = n |> _.x ?? 0\nprint(a, b, c)\n";
        let out = compiled(src);
        assert_eq!(messages(&out), [] as [&str; 0]);

        for want in [
            "local __pipe1 = n or 1  local a = __pipe1 + 1",
            "local __pipe2 = if n > 1 then 1 else 2  local b = __pipe2 + 10",
            "local __pipe3 = n  ",
        ] {
            assert!(out.ship.contains(want), "{want}\n{}", out.ship);
        }
    }

    /// The reports of a wrong form, each on the token that caused it.
    #[test]
    fn a_wrong_pipe_reports() {
        let out = compiled(
            "local function f(x) return x end\nlocal a = 1 |> f\nlocal b = 2 |> function() _ = 3 return _ end\nprint(a, b)\n",
        );
        assert_eq!(
            messages(&out),
            [
                "the body of `|>` never reads `_`",
                "`_` is the piped value and cannot be assigned",
            ]
        );

        let out = compiled("local a = 1 |>\n");
        assert_eq!(messages(&out), ["expected an expression after `|>`"]);
    }

    /// With the flag off the operator reports and names the flag.
    #[test]
    fn a_pipe_needs_its_flag() {
        let out = crate::compile("local a = 1 |> _ + 1\nprint(a)\n").unwrap();
        assert_eq!(
            messages(&out),
            [
                "`|>` is not final; set `AlloyExperimentalPipeOperator = true` under `[fflags]` in alloy.toml to use it"
            ]
        );
    }
}
