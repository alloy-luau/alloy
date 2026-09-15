//! Expression lowering, including postfix chains and `new`/table
//! construction.

use alloy_syntax::ast::{Block, CallArgs, ChildName, Cond, Expr, IndexKey, TableField, TokSpan};

use crate::roblox_classes::{DATATYPES, INSTANCE_CLASSES};

use super::types::pack_type_args;
use super::*;

pub(crate) const WORD_OPS: &[&str] = &["band", "bor", "bxor", "shl", "shr", "bnot", "in"];

pub(crate) enum WordOp {
    Bit,
    In,
}

pub(crate) struct ChainParts {
    pub(crate) guard: Option<String>,
    pub(crate) inner: String,
}

/// Reports if a chain holds any link that is not plain Luau, or a base
/// that Luau cannot index directly.
/// A chain with a written turbofish on one of its calls. The list has
/// to lower, and the chain rewrite is what lowers it.
pub(crate) fn chain_has_type_args(e: &Expr) -> bool {
    let (_, links) = flatten(e);

    links.iter().any(|l| {
        matches!(
            l,
            Link::Plain(Step::Call {
                type_args: Some(_),
                ..
            }) | Link::Optional(Step::Call {
                type_args: Some(_),
                ..
            })
        )
    })
}

/// A module path the analyzer can follow: a string, or a chain of names
/// and fields such as `script.Parent.Module`.
pub(crate) fn is_static_module(e: &Expr) -> bool {
    if matches!(e, Expr::String(_)) {
        return true;
    }

    let (base, links) = flatten(e);

    matches!(base, Expr::Name(_))
        && links
            .iter()
            .all(|l| matches!(l, Link::Plain(Step::Field(_))))
}

/// A source slice with every run of whitespace as one space, so it fits
/// on the line the generated text sits on.
pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl<'s> Desugar<'s> {
    pub(crate) fn expr(&mut self, e: &Expr) {
        let anchor = self.byte_start(e.span());

        match e {
            Expr::Name(span) => {
                let name = self.text_of(*span);

                if let Some(path) = self.renamed(name) {
                    self.generate(anchor, &path);
                } else if AMBIENT.contains(&name) && !self.is_local(name) {
                    let std = self.std();
                    self.generate(anchor, &format!("{std}.{name}"));
                } else {
                    self.copy_span(*span);
                }
            }

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "bnot" => {
                let inner = self.render_to_string(operand);
                self.generate(anchor, &format!("bit32.bnot({inner})"));
            }

            Expr::Binary { op, lhs, rhs, span } if self.is_coalesce(*op) => {
                self.coalesce(*span, lhs, rhs);
            }

            Expr::Binary { op, lhs, rhs, .. } if self.word_binop(*op).is_some() => {
                let (kind, name) = self.word_binop(*op).unwrap();
                let l = self.render_to_string(lhs);
                let r = self.render_to_string(rhs);
                let text = match kind {
                    WordOp::Bit => format!("bit32.{name}({l}, {r})"),

                    WordOp::In => {
                        let std = self.std();

                        format!("{std}.contains({r}, {l})")
                    }
                };
                self.generate(anchor, &text);
            }

            // `Signal.new<<A, B>>()`: the std types the statics with a
            // type pack, which takes one parenthesized argument.
            Expr::Call {
                func,
                method: None,
                type_args: Some(t),
                args,
                ..
            } if self.is_signal_new(func) => {
                let std = self.std();
                let text = self.text_of(*t).to_string();
                let targs = pack_type_args(&self.lower_type_args(&text));
                let a = self.args_text(args);
                self.generate(anchor, &format!("{std}.Signal.new{targs}{a}"));
            }

            // An element read out of a bounded `T[]`. The cast is the
            // whole expression, so `xs[i]:size()` still calls through it.
            Expr::Index {
                object,
                key: IndexKey::Computed(k),
                optional: false,
                ..
            } if self.element_bound(object).is_some() => {
                let ty = self.element_bound(object).expect("matched above");
                let obj = self.render_to_string(object);
                let key = self.render_to_string(k);
                self.generate(anchor, &format!("({obj}[{key}] :: {ty})"));
            }

            Expr::Index { .. } | Expr::Call { .. } | Expr::Child { .. } | Expr::NonNil { .. }
                if chain_has_alloy(e)
                    || chain_has_type_args(e)
                    || self.chain_has_ext(e)
                    || self.is_struct_call(e)
                    || self.is_import_call(e)
                    || self.expected_generic.is_some() =>
            {
                let text = self.chain_expr(e);
                self.generate_chain(anchor, &text, e);
            }

            Expr::Ternary {
                cond,
                then_value,
                else_value,
                ..
            } => {
                let c = self.render_to_string(cond);
                let a = self.render_to_string(then_value);
                let b = self.render_to_string(else_value);
                self.generate(anchor, &format!("(if {c} then {a} else {b})"));
            }

            Expr::Is {
                expr,
                negated,
                name,
                ..
            } => {
                let text = self.is_test(expr, *name, *negated);
                self.generate(anchor, &text);
            }

            Expr::Satisfies { expr, ty, .. } => {
                let inner = self.render_to_string(expr);
                // `number[]` and a namespace type are Alloy's spellings,
                // so the type goes through the same copy a declaration
                // does.
                let ty = self.copy_type_to_string(*ty).trim().to_string();
                // `::` allows a cast in either direction, so it lets a
                // literal through that the type does not cover. A call
                // checks the argument against the parameter, which is
                // what `satisfies` means. The ship artifact keeps the
                // cast: it runs, and the check is the checker's.
                let text = match self.options.check {
                    true => format!("((function(value: {ty}): {ty} return value end)({inner}))"),

                    false => format!("({inner} :: {ty})"),
                };
                self.generate(anchor, &text);
            }

            Expr::Array { items, span } => {
                // An empty literal has no element type; the check artifact
                // lets the annotation on the left decide it.
                let cast = self.options.check && items.is_empty();
                let std = self.std();
                let open_text = if cast {
                    format!("({std}.Array.from({{")
                } else {
                    format!("{std}.Array.from({{")
                };
                self.generate(anchor, &open_text);
                let open = self.toks[span.start as usize].end;
                let close = self.toks[span.end as usize - 1].start;
                let children: Vec<Child<'_>> = items.iter().map(Child::Expr).collect();
                self.stitch_between(open, close, &children);
                self.generate(close, if cast { "}) :: any)" } else { "})" });
            }

            Expr::Table { fields, span }
                if fields.iter().any(|f| matches!(f, TableField::Spread(_))) =>
            {
                self.spread_table(*span, fields);
            }

            Expr::MethodRef { object, name, .. } => {
                let obj = self.reusable(object);
                let method = self.text_of(*name).to_string();
                let std = self.std();
                let bound = self.any_cast(&format!("{std}.bind({obj}, {obj}.{method})"));
                self.generate(anchor, &bound);
            }

            Expr::New {
                name,
                type_args,
                args,
                init,
                span,
            } => {
                // A fields table copies as it is, since it may span
                // lines and generated text never holds a newline.
                if self.fields_form(name, args.as_ref(), init.as_deref())
                    && let Some(table) = init.as_deref()
                {
                    self.check_new(name, args.as_ref(), Some(table), *span);
                    // The struct's declared name, not the name the
                    // source wrote: a namespace member renders under
                    // `Zoo_Lion`, and that is the table that carries
                    // the raw constructor. Reading `Zoo.Lion` instead
                    // found the `new` a user impl writes.
                    let n = self
                        .constructed_struct(name)
                        .map_or_else(|| self.render_to_string(name), |(_, n)| n);
                    // Inside the struct's own impl the instance carries the
                    // full view, so `self.count` in `new` type checks.
                    let full_view = self.impl_target.as_deref() == Some(n.as_str())
                        && self.has_private_view(&n);
                    let open = if full_view {
                        format!("(({}(", self.raw_ctor(&n))
                    } else {
                        format!("{}(", self.raw_ctor(&n))
                    };
                    self.generate(anchor, &open);
                    self.expr(table);
                    let close = if full_view {
                        format!(") :: any) :: {n}__all)")
                    } else {
                        ")".to_string()
                    };
                    self.generate(self.byte_end(table.span()), &close);
                } else if self.self_construct(name, args.as_ref(), init.as_deref()) {
                    self.check_new(name, args.as_ref(), init.as_deref(), *span);
                    let n = self.render_to_string(name);
                    let text = self.empty_construct(&n);
                    self.generate(anchor, &text);
                } else {
                    let head =
                        self.new_head(name, *type_args, args.as_ref(), init.as_deref(), *span);
                    self.generate(anchor, &head);

                    if let Some(table) = init.as_deref() {
                        self.expr(table);
                        self.generate(self.byte_end(table.span()), ")");
                    }
                }
            }

            // The operand keeps its chunks, so the editor maps its names.
            Expr::Await { operand, .. } => {
                let std = self.std();
                self.generate(anchor, &format!("{std}.await("));
                self.expr(operand);
                self.generate(anchor, ")");
            }

            Expr::Try { operand, span } => {
                let text = self.try_expr(operand, *span);
                self.generate(anchor, &text);
            }

            Expr::AsyncBlock { block, span } | Expr::TryBlock { block, span } => {
                let is_try = matches!(e, Expr::TryBlock { .. });
                let helper = if is_try { "try_block" } else { "future" };
                let std = self.std();
                // A `try do` block hands its closure the `fail` a `try`
                // inside it calls. The name carries the nesting, so an
                // inner block shadows no outer one.
                let target = is_try.then(|| TryTarget {
                    fail: self.fail_name(),
                    exact: self.block_fail_exact(block),
                });
                let params = match &target {
                    Some(t) => t.fail.clone(),

                    None => String::new(),
                };
                // `local f: Future<T> = async do ... end` names the
                // payload, and the closure carries it: the checker
                // otherwise infers the body's result, and an open
                // result lands on `unknown`. The hint is for this
                // block alone, so a nested one takes none.
                let payload = match self.expected_payload.take() {
                    Some(t) if !is_try => format!(": {}", self.lower_type(&t)),

                    _ => String::new(),
                };
                self.generate(
                    anchor,
                    &format!("{std}.{helper}(function({params}){payload}"),
                );
                // The two keywords are replaced; the block and `end` copy.
                let after_keywords = self.toks[span.start as usize + 1].end;
                let end_tok = self.toks[span.end as usize - 1];
                let body_start = self.block_start_or(block, end_tok.start);
                self.copy(after_keywords, body_start);
                self.try_targets.push(target);
                self.block(block);
                self.try_targets.pop();
                let after_block = self.block_end_or(block, body_start);
                self.copy(after_block, end_tok.start);
                self.copy(end_tok.start, end_tok.end);
                self.generate(end_tok.end, ")");
            }

            Expr::Macro { name, args, span } => {
                let mname = self.text_of(*name).to_string();

                if let Some(m) = self.macros.get(&mname).cloned() {
                    let text = self.expand_macro(&m, &mname, args, *span);
                    self.generate(anchor, &text);
                } else {
                    let text = self.intrinsic(*name, args, *span);
                    self.generate(anchor, &text);
                }
            }

            Expr::Match(m) => self.match_expr(m),

            Expr::IfElse {
                branches,
                else_value,
                span,
            } if branches
                .iter()
                .any(|(c, _)| matches!(c, Cond::Local { .. })) =>
            {
                self.if_expr_with_locals(*span, branches, else_value);
            }

            Expr::Function { body, .. } if function_needs_rewrite(body) => {
                self.function_with_header(e.span(), body);
            }

            _ => {
                let children = expr_children(e);

                if children.is_empty() {
                    self.copy_span(e.span());
                } else {
                    self.stitch(e.span(), &children, |d, child| match child {
                        Child::Expr(e) => d.expr(e),

                        Child::Block(b) => d.block(b),

                        Child::Function(b) => d.function_block(b),
                    });
                }
            }
        }
    }

    pub(crate) fn block_start_or(&self, block: &Block, default: u32) -> u32 {
        if block.span.is_empty() {
            default
        } else {
            self.byte_start(block.span)
        }
    }

    pub(crate) fn block_end_or(&self, block: &Block, default: u32) -> u32 {
        if block.span.is_empty() {
            default
        } else {
            self.byte_end(block.span)
        }
    }

    /// `Signal.new` on the std `Signal`, not on a local of that name.
    pub(crate) fn is_signal_new(&self, func: &Expr) -> bool {
        let Expr::Index {
            object,
            key: IndexKey::Field(f),
            ..
        } = func
        else {
            return false;
        };

        matches!(object.as_ref(), Expr::Name(n)
            if self.text_of(*n) == "Signal" && !self.is_local("Signal"))
            && self.text_of(*f) == "new"
    }

    pub(crate) fn is_coalesce(&self, op: TokSpan) -> bool {
        op.end - op.start == 2 && self.text_of(op) == "??"
    }

    pub(crate) fn is_coalesce_assign(&self, op: TokSpan) -> bool {
        op.end - op.start == 3 && self.text_of(op) == "??="
    }

    pub(crate) fn word_binop(&self, op: TokSpan) -> Option<(WordOp, &'static str)> {
        if op.end - op.start != 1 {
            return None;
        }

        match self.text_of(op) {
            "band" => Some((WordOp::Bit, "band")),

            "bor" => Some((WordOp::Bit, "bor")),

            "bxor" => Some((WordOp::Bit, "bxor")),

            "shl" => Some((WordOp::Bit, "lshift")),

            "shr" => Some((WordOp::Bit, "rshift")),

            "in" => Some((WordOp::In, "contains")),

            _ => None,
        }
    }

    /*
    `a ?? b` renders as `(if A == nil then B else A)`.

    A simple left side reads twice in place. Any other left side hoists into
    a temp so it evaluates once. The right side renders inline either way:
    it evaluates only when the left is nil, which is the point.
    */
    pub(crate) fn coalesce(&mut self, span: TokSpan, lhs: &Expr, rhs: &Expr) {
        let anchor = self.byte_start(span);
        let left = self.reusable(lhs);
        let right = self.render_to_string(rhs);
        self.generate(
            anchor,
            &format!("(if {left} == nil then {right} else {left})"),
        );
    }

    /*
    `x is T` by the name on the right. A primitive tests `type`, a Roblox
    datatype tests `typeof`, an Instance class tests `IsA`, an enum tests
    the `EnumType`, and any other name is an Alloy struct's metatable.
    */
    pub(crate) fn is_test(&mut self, expr: &Expr, name: TokSpan, negated: bool) -> String {
        let x = self.reusable(expr);
        let n = self.text_of(name).to_string();

        if n == "nil" {
            return if negated {
                format!("({x} ~= nil)")
            } else {
                format!("({x} == nil)")
            };
        }

        let test = if PRIMITIVES.contains(&n.as_str()) {
            format!("type({x}) == \"{n}\"")
        } else if let Some(item) = n.strip_prefix("Enum.") {
            format!("typeof({x}) == \"EnumItem\" and {x}.EnumType == Enum.{item}")
        } else if INSTANCE_CLASSES.contains(&n.as_str()) {
            if n == "Instance" {
                // The root class: `typeof` alone answers, and an `IsA`
                // on a value typed `any` trips the solver.
                format!("typeof({x}) == \"Instance\"")
            } else {
                format!("typeof({x}) == \"Instance\" and {x}:IsA(\"{n}\")")
            }
        } else if DATATYPES.contains(&n.as_str()) {
            format!("typeof({x}) == \"{n}\"")
        } else if self.enums.contains_key(&n) {
            format!("{n}.is({x})")
        } else {
            format!("getmetatable({}) == {n}", self.any_cast(&x))
        };

        if negated {
            format!("(not ({test}))")
        } else {
            format!("({test})")
        }
    }

    /// The type of a `try` operand when the file declares it and it is
    /// no Result. A type the file cannot name gives `None`: the checker
    /// reads those, and a guess here would be a false report.
    fn non_result_type(&self, operand: &Expr) -> Option<String> {
        let Expr::Call {
            func, method: None, ..
        } = operand
        else {
            return None;
        };
        let Expr::Name(n) = &**func else {
            return None;
        };
        let ty = self.fn_ret_types.get(self.text_of(*n))?.trim();

        if ty.contains("Result") || self.result_aliases.contains(ty) {
            return None;
        }

        self.known_type(ty).then(|| ty.to_string())
    }

    /// Whether a written type is one this file can name for certain. A
    /// generic parameter, an alias, or an imported name types elsewhere.
    pub(crate) fn known_type(&self, ty: &str) -> bool {
        PRIMITIVES.contains(&ty)
            || self.structs.contains(ty)
            || self.enum_decls.contains_key(ty)
            || ty == "nil"
            || ty == "()"
    }

    /// The dotted name of a callee, `Future.resolve` for an `Index`
    /// chain of plain fields. Anything else gives `None`.
    pub(crate) fn dotted_name(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(n) => Some(self.text_of(*n).to_string()),

            Expr::Index {
                object,
                key: IndexKey::Field(f),
                optional: false,
                ..
            } => Some(format!(
                "{}.{}",
                self.dotted_name(object)?,
                self.text_of(*f)
            )),

            _ => None,
        }
    }

    /// Whether an expression is a Result the file can name as one:
    /// `Ok(v)`, `Err(e)`, a `try`, or a call to a function whose
    /// declared return type is a Result.
    fn is_result_expr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Paren { inner, .. } => self.is_result_expr(inner),

            Expr::Try { .. } | Expr::TryBlock { .. } => true,

            Expr::Call {
                func, method: None, ..
            } => match self.dotted_name(func).as_deref() {
                Some("Ok" | "Err") => true,

                Some(name) => {
                    let ty = self.fn_ret_types.get(name).map(|t| t.trim().to_string());

                    ty.is_some_and(|t| t.starts_with("Result") || self.result_aliases.contains(&t))
                }

                None => false,
            },

            _ => false,
        }
    }

    /// Whether a written type is a Future of a Result. The std spells
    /// the operand of `await` `Awaitable<T>`; a source writes `Future`.
    fn future_of_result(&self, ty: &str) -> bool {
        let inner = ty
            .strip_prefix("Future<")
            .or_else(|| ty.strip_prefix("Awaitable<"))
            .and_then(|rest| rest.strip_suffix('>'));
        let Some(inner) = inner.map(str::trim) else {
            return false;
        };
        let head = inner.split_once('<').map_or(inner, |(head, _)| head).trim();

        head == "Result" || self.result_aliases.contains(head)
    }

    /// Whether the Future an `await` takes settles with a Result. A
    /// call to an async function declared to return one does,
    /// `Future.resolve(r)` settles with exactly `r`, and a binding
    /// annotated `Future<Result<T, E>>` says so outright.
    fn settles_with_result(&self, awaited: &Expr) -> bool {
        match awaited {
            Expr::Paren { inner, .. } => self.settles_with_result(inner),

            // A binding annotated `Future<Result<T, E>>` settles with
            // that Result.
            Expr::Name(n) => self
                .binding_types
                .get(self.text_of(*n))
                .is_some_and(|t| self.future_of_result(t)),

            Expr::Call {
                func,
                method: None,
                args,
                ..
            } => match self.dotted_name(func).as_deref() {
                Some("Future.resolve") => match args {
                    CallArgs::Paren(list) => list.len() == 1 && self.is_result_expr(&list[0]),

                    _ => false,
                },

                Some(name) => self.result_asyncs.contains(name),

                None => false,
            },

            _ => false,
        }
    }

    /// The name of the `fail` a `try do` block hands its closure. The
    /// count of the blocks already open numbers it, so a block inside a
    /// block shadows no name.
    fn fail_name(&self) -> String {
        let open = self.try_targets.iter().flatten().count();

        if open == 0 {
            "__fail".to_string()
        } else {
            format!("__fail{}", open + 1)
        }
    }

    /// The error type a `try` operand carries, where this file can name
    /// it: `Result<T, E>` gives `E`. A call this file cannot see the
    /// return type of gives `None`.
    fn try_error_type(&self, operand: &Expr) -> Option<String> {
        let Expr::Call {
            func, method: None, ..
        } = operand
        else {
            return None;
        };
        let name = self.dotted_name(func)?;

        if name == "Result.pcall" {
            return Some("string".to_string());
        }

        let ty = self.fn_ret_types.get(&name)?.trim();
        let inner = ty.strip_prefix("Result<")?.strip_suffix('>')?;
        let args = super::types::split_generics(inner);

        (args.len() == 2).then(|| args[1].clone())
    }

    /// Whether a `fail` call of this block may pass its payload with the
    /// payload's own type. One error source solves `E` on its own. Luau
    /// reads the type of an unannotated parameter off the first call, so
    /// two sources this file cannot prove equal would pin `E` to the
    /// first and report the second; those pass `any` instead.
    fn block_fail_exact(&self, block: &Block) -> bool {
        let mut found = Vec::new();
        self.block_sources(block, &mut found);

        if found.len() <= 1 {
            return true;
        }

        let mut seen: Option<String> = None;

        for operand in found {
            let Some(ty) = operand.and_then(|e| self.try_error_type(e)) else {
                return false;
            };

            match &seen {
                None => seen = Some(ty),

                Some(first) if *first == ty => {}

                Some(_) => return false,
            }
        }

        true
    }

    /// Every error this block's `fail` can carry: the operand of a `try`
    /// the block owns, and `None` for a `return Err(e)` it owns, whose
    /// payload type this file does not name. A nested function and a
    /// nested `try do` or `async do` block own theirs, so the walk stops
    /// there.
    fn block_sources<'a>(&self, block: &'a Block, out: &mut Vec<Option<&'a Expr>>) {
        for stmt in &block.stmts {
            if let Stmt::Return(r) = stmt
                && self.err_call_args(&r.values).is_some()
            {
                out.push(None);
            }

            for child in stmt_children(stmt) {
                self.child_sources(child, out);
            }
        }
    }

    fn child_sources<'a>(&self, child: Child<'a>, out: &mut Vec<Option<&'a Expr>>) {
        match child {
            Child::Expr(Expr::TryBlock { .. } | Expr::AsyncBlock { .. }) => {}

            Child::Expr(e) => {
                if let Expr::Try { operand, .. } = e {
                    out.push(Some(operand));
                }

                for inner in expr_children(e) {
                    self.child_sources(inner, out);
                }
            }

            Child::Block(b) => self.block_sources(b, out),

            Child::Function(_) => {}
        }
    }

    /// The arguments of `Err(e)` or `Err(e, trace)`, where the name is
    /// the std's and not a local of this file.
    pub(crate) fn err_call_args<'a>(&self, values: &'a [Expr]) -> Option<&'a [Expr]> {
        if values.len() != 1 {
            return None;
        }

        let Expr::Call {
            func,
            method: None,
            args: CallArgs::Paren(list),
            ..
        } = &values[0]
        else {
            return None;
        };

        (self.dotted_name(func).as_deref() == Some("Err")
            && !list.is_empty()
            && list.len() <= 2
            && !self.is_local("Err"))
        .then_some(list.as_slice())
    }

    /// `try expr`: hoist the Result, return it on Err, yield the payload.
    pub(crate) fn try_expr(&mut self, operand: &Expr, span: TokSpan) -> String {
        let anchor = self.byte_start(span);

        let target = self.try_targets.last().cloned().flatten();

        // At the top level `return` leaves the module, which the emit
        // still writes; inside a function the return type must take an
        // Err. Inside a `try do` block the Err leaves that block, so the
        // function around it is free to return anything.
        if target.is_none() && !self.ret_types.is_empty() && !self.in_result_function() {
            self.diagnose(
                span,
                "`try` works only inside a function that returns Result; it returns the Err from that function",
            );
        }

        // A value that is no Result has no `Err` to return. The
        // operand's own type says so, where the file declares it.
        if let Some(ty) = self.non_result_type(operand) {
            let text = self.text_of(operand.span()).trim().to_string();
            self.diagnose(span, &format!("`try` needs a Result; `{text}` is `{ty}`"));
        }

        // The operand keeps its chunks, so the editor maps its names.
        let value = match operand {
            Expr::Await { operand: inner, .. } => {
                let std = self.std();
                // A Future that settles with a Result yields that
                // Result, not an Ok around it: the typed form says so.
                // Without it the checker prints a Result of a Result,
                // and the nested print names the emit's own keys.
                let helper = if self.settles_with_result(inner) {
                    "try_await_result"
                } else {
                    "try_await"
                };

                self.to_side(|d| {
                    d.generate(anchor, &format!("{std}.{helper}("));
                    d.expr(inner);
                    d.generate(anchor, ")");
                })
            }

            other => self.render_to_side(other),
        };
        let temp = self.hoist_rendered(value, anchor);

        match target {
            // Inside a `try do` block the Err leaves through the block's
            // `fail`. A `return` there would decide the closure's value
            // type instead: Luau reads that off the first `return` and
            // checks the rest against it.
            Some(t) => {
                let payload = if t.exact {
                    format!("{temp}._1")
                } else {
                    self.any_cast(&format!("{temp}._1"))
                };
                let fail = &t.fail;
                self.hoist_stmt(
                    format!("if {temp}.tag == \"Err\" then {fail}({payload}, {temp}.trace) end"),
                    anchor,
                );
            }

            None => {
                let returned = self.any_cast(&temp);
                self.hoist_stmt(
                    format!("if {temp}.tag == \"Err\" then return {returned} end"),
                    anchor,
                );
            }
        }

        // `_1` is `T | E` to the checker; `unwrap` is `T`. The ship
        // artifact reads the field, since the tag was just checked.
        if self.options.check {
            format!("{temp}:unwrap()")
        } else {
            format!("{temp}._1")
        }
    }

    /// `new Name(args)`, `new Name<<T>>(args)`, `new Name(args) { init }`.
    /// The head of a `new` expression before its fields table, and
    /// whether one follows: `Name.new(args)` alone, `__alloy.init(
    /// Name.new(args), ` for a call with fields, `__alloy.construct(
    /// Name, ` for fields on a name this file does not declare, which
    /// the check artifact types as `Name.new(`, the typed constructor
    /// an imported struct carries. The caller copies the table after it.
    pub(crate) fn new_head(
        &mut self,
        name: &Expr,
        type_args: Option<TokSpan>,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
        whole: TokSpan,
    ) -> String {
        self.check_new(name, args, init, whole);
        let ctor = self.constructor_of(name);
        let n = self.render_to_string(name);
        let t = match type_args {
            Some(s) => {
                let text = self.text_of(s).to_string();

                self.lower_type_args(&text)
            }

            None => self.expected_args_for(name),
        };

        match (args, init) {
            (Some(a), None) => {
                let a = self.args_text(a);

                format!("{n}.{ctor}{t}{a}")
            }

            (None, None) => format!("{n}.{ctor}{t}()"),

            (Some(a), Some(_)) => {
                let a = self.args_text(a);
                let std = self.std();

                format!("{std}.init({n}.{ctor}{t}{a}, ")
            }

            (None, Some(_)) => {
                if self.options.check {
                    // The fields form builds the value outright, so it
                    // calls the raw constructor the check artifact types,
                    // `__new`. A user `new` in the struct's own impl takes
                    // the parameters it declares, and is not this call.
                    let known = self
                        .constructed_struct(name)
                        .and_then(|(_, s)| self.declared_fields(&s))
                        .is_some();

                    if known {
                        format!("{}{t}(", self.raw_ctor(&n))
                    } else {
                        format!("{n}.{ctor}{t}(")
                    }
                } else {
                    let std = self.std();

                    format!("{std}.construct({n}, ")
                }
            }
        }
    }

    /// `{ ...a, x = 1, ...b }` becomes `spread(a, { x = 1 }, b)`, with the
    /// text between fields copied so the lines hold.
    pub(crate) fn spread_table(&mut self, span: TokSpan, fields: &[TableField]) {
        let anchor = self.byte_start(span);
        let std = self.std();
        let open = if self.options.check { "(" } else { "" };
        self.generate(anchor, &format!("{open}{std}.spread("));

        let open_end = self.toks[span.start as usize].end;
        let close_start = self.toks[span.end as usize - 1].start;
        let mut cursor = open_end;
        let mut in_group = false;
        // The parts of the merged type, in order, for the check artifact.
        let mut parts: Vec<String> = Vec::new();
        let mut group: Vec<String> = Vec::new();
        let mut spread_seen = false;

        for (i, field) in fields.iter().enumerate() {
            let (fs, fe) = self.field_bytes(field);
            let is_spread = matches!(field, TableField::Spread(_));
            let last = i + 1 == fields.len();
            let text = one_line(&self.src[fs as usize..fe as usize]);

            // The gap before a field carries the comma and the newlines.
            self.copy(cursor, fs);

            if let TableField::Spread(e) = field {
                spread_seen = true;
                self.expr(e);
                parts.push(format!("typeof({})", text.trim_start_matches("...").trim()));
            } else {
                if spread_seen && let TableField::Positional(v) = field {
                    self.diagnose(
                        v.span(),
                        "a positional entry after a spread has no place to go; name it, or move it in front of the spread",
                    );
                }

                if !in_group {
                    self.generate(fs, "{ ");
                    in_group = true;
                }

                let children = field_children(field);
                self.stitch_between(fs, fe, &children);
                group.push(text);
                let next_is_spread = matches!(fields.get(i + 1), Some(TableField::Spread(_)));

                if next_is_spread || last {
                    self.generate(fe, " }");
                    in_group = false;
                    parts.push(format!("typeof({{ {} }})", group.join(", ")));
                    group.clear();
                }
            }

            let _ = is_spread;
            cursor = fe;
        }

        // The trailing gap must not carry a comma into the call.
        let tail = &self.src[cursor as usize..close_start as usize];

        match tail.find(',') {
            Some(i) => {
                self.copy(cursor, cursor + i as u32);
                self.copy(cursor + i as u32 + 1, close_start);
            }

            None => self.copy(cursor, close_start),
        }

        // The runtime merge answers a bare table, so the check artifact
        // says what the parts add up to: a key no part names is an error.
        let cast = match (self.options.check, parts.is_empty()) {
            (true, false) => format!(") :: {})", parts.join(" & ")),
            (true, true) => "))".to_string(),
            (false, _) => ")".to_string(),
        };
        self.generate(close_start, &cast);
    }

    pub(crate) fn field_bytes(&self, field: &TableField) -> (u32, u32) {
        match field {
            TableField::Positional(v) => (self.byte_start(v.span()), self.byte_end(v.span())),

            TableField::Named { name, value } => {
                (self.byte_start(*name), self.byte_end(value.span()))
            }

            TableField::Computed { key, value } => (
                self.toks[key.span().start as usize - 1].start,
                self.byte_end(value.span()),
            ),

            TableField::Spread(e) => {
                let tok = self.toks[e.span().start as usize - 1];

                (tok.start, self.byte_end(e.span()))
            }
        }
    }

    // --- postfix chains ----------------------------------------------------

    /*
    Walks a postfix chain and returns its guard and inner text.

    `inner` is the expression as it stands; `guard` is the temp whose nil
    makes the whole result nil. An optional link needs its prefix named
    once, so the prefix becomes a temp (or stays, when it is simple) and
    the guard moves to it. A plain link applies inside the current guard,
    which is the chain rule: one `?` guards every later link. `!` names
    the prefix the same way, then ends the guard with an `error` branch.
    */
    pub(crate) fn chain_parts(&mut self, e: &Expr) -> ChainParts {
        let (base, links) = flatten(e);
        let timed_waits = self.options.wait_timeout.is_some();
        // A timed `WaitForChild` can return nil, so the link after it guards.
        let mut pending_guard = false;

        // A string literal as a receiver needs parentheses in Luau.
        let mut inner = match base {
            Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. } if !links.is_empty() => {
                format!("({})", self.render_to_string(base))
            }

            _ => self.render_to_string(base),
        };

        // `Vector3.zero(...)`: a static declared on a foreign type.
        let mut links = links;

        if let (Expr::Name(n), Some(Link::Plain(Step::Field(f)))) = (base, links.first())
            && let Some(statics) = self.ext_statics.get(self.text_of(*n))
            && statics.contains(self.text_of(*f))
        {
            let target = self.text_of(*n);

            if !self.options.check {
                let std = self.std();
                inner = format!(
                    "{std}.static({}, {})",
                    luau_string(target),
                    luau_string(self.text_of(*f))
                );
                self.ext_hit = true;
                links.remove(0);
            } else if PRIMITIVES.contains(&target) {
                inner = format!("__alloy_{target}.{}", self.text_of(*f));
                links.remove(0);
            }
        }
        // `import(...)` is `require(...)`. A string or an instance chain
        // types itself; a dynamic path is `unknown` unless `<<T>>` says.
        if self.is_import_call(e)
            && let Some(Link::Plain(Step::Call {
                type_args, args, ..
            })) = links.first()
        {
            // A data path drops its extension, as in `import` statements.
            let a = match args {
                CallArgs::Str(s) => crate::data::strip_literal(self.text_of(*s)),

                CallArgs::Paren(list) if list.len() == 1 && matches!(list[0], Expr::String(_)) => {
                    crate::data::strip_literal(&self.render_to_string(&list[0]))
                }

                _ => self.args_text(args),
            };
            let a = if a.starts_with('(') {
                a
            } else {
                format!("({a})")
            };
            let is_static = match args {
                CallArgs::Str(_) => true,

                CallArgs::Paren(list) => list.len() == 1 && is_static_module(&list[0]),

                CallArgs::Table(_) => false,
            };
            let ty = type_args.map(|s| {
                let text = self
                    .text_of(s)
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .trim()
                    .to_string();

                self.lower_type(&text)
            });
            inner = match ty {
                Some(t) => format!("(require{a} :: {t})"),

                None if is_static => format!("require{a}"),

                None => format!("(require{a} :: unknown)"),
            };
            links.remove(0);
        }

        let mut inner_simple = self.is_simple(base);
        let mut guard: Option<String> = None;

        // `HashMap.new()` under `local m: HashMap<K, V>`: the arguments
        // the annotation names go on the call.
        if let (Expr::Name(n), Some((base_name, args_text))) = (base, self.expected_generic.clone())
            && self.text_of(*n) == base_name
            && let [
                Link::Plain(Step::Field(f)),
                Link::Plain(Step::Call {
                    method: None,
                    type_args: None,
                    args,
                }),
            ] = links.as_slice()
            && matches!(self.text_of(*f), "new" | "from" | "with_capacity")
        {
            let method = self.text_of(*f).to_string();
            let a = self.args_text(args);

            let targs = self.lower_type_args(&format!("<<{args_text}>>"));

            return ChainParts {
                guard: None,
                inner: format!("{inner}.{method}{targs}{a}"),
            };
        }

        for link in links {
            let link = match link {
                Link::Plain(step) if pending_guard => Link::Optional(step),

                other => other,
            };
            pending_guard = false;

            match link {
                Link::Plain(step) => {
                    inner_simple = inner_simple && matches!(step, Step::Field(_));
                    pending_guard = timed_waits && matches!(step, Step::Child { wait: true, .. });
                    inner = self.apply(&inner, &step);
                }

                Link::Optional(step) => {
                    let name = self.name_prefix(&mut inner, &mut guard, inner_simple);
                    inner_simple = false;
                    pending_guard = timed_waits && matches!(step, Step::Child { wait: true, .. });
                    // `f?()` on a field of an optional function type: the new
                    // solver loses the field's type under an earlier `== n`
                    // refinement of a metatable-typed `self`, and reports an
                    // error type at the call. The check artifact calls
                    // through `any`; the guard on the value stays typed.
                    let callee =
                        if self.options.check && matches!(step, Step::Call { method: None, .. }) {
                            format!("({name} :: any)")
                        } else {
                            name
                        };
                    inner = self.apply(&callee, &step);
                }

                Link::NonNil { span } => {
                    let name = self.name_prefix(&mut inner, &mut guard, inner_simple);
                    let source = self.text_of(span);
                    let message = luau_string(&format!("{source} is nil"));
                    // The checker gives `error(m)` a type it cannot
                    // index through, and the whole branch takes it:
                    // `x![k].m` then answers nothing. `:: never` says
                    // the branch has no value, so the else side alone
                    // decides. The ship artifact runs the call and
                    // needs no cast.
                    let raised = match self.options.check {
                        true => format!("(error({message}) :: never)"),

                        false => format!("error({message})"),
                    };
                    inner = format!("(if {name} == nil then {raised} else {name})");
                    inner_simple = false;
                    // `!` ends the guard: past it the value is never nil.
                    guard = None;
                }
            }
        }

        ChainParts { guard, inner }
    }

    /// Names the current prefix so a link can test it and then use it.
    pub(crate) fn name_prefix(
        &mut self,
        inner: &mut String,
        guard: &mut Option<String>,
        inner_simple: bool,
    ) -> String {
        let name = if guard.is_none() && inner_simple {
            inner.clone()
        } else {
            let whole = self.guarded(guard.as_deref(), inner);
            let anchor = self.chain_anchor;
            self.hoist_text(whole, anchor)
        };

        *guard = Some(name.clone());
        *inner = name.clone();

        name
    }

    pub(crate) fn guarded(&self, guard: Option<&str>, inner: &str) -> String {
        match guard {
            Some(g) => format!("(if {g} == nil then nil else {inner})"),

            None => inner.to_string(),
        }
    }

    pub(crate) fn apply(&mut self, prefix: &str, step: &Step<'_>) -> String {
        match step {
            Step::Field(name) => format!("{prefix}.{}", self.text_of(*name)),

            Step::Computed(key) => {
                let k = self.render_to_string(key);

                format!("{prefix}[{k}]")
            }

            Step::Call {
                method,
                type_args,
                args,
            } => {
                // A method name declared on a foreign type in this file
                // routes through the dispatcher, which falls back to the
                // receiver's own method.
                if let Some(m) = method {
                    let mname = self.text_of(*m).to_string();

                    if self.ext_methods.contains(&mname) && type_args.is_none() {
                        let a = self.args_text(args);
                        let inner = a.trim_start_matches('(').trim_end_matches(')');
                        let sep = if inner.is_empty() { "" } else { ", " };

                        if !self.options.check {
                            let std = self.std();
                            self.ext_hit = true;

                            return format!("{std}.call({prefix}, \"{mname}\"{sep}{inner})");
                        }

                        match self.ext_primitive.get(&mname) {
                            Some(Some(target)) => {
                                return format!("__alloy_{target}.{mname}({prefix}{sep}{inner})");
                            }

                            // Two primitives declare the name, so the
                            // check artifact keeps the dispatcher the
                            // ship uses; it reads the value's own kind.
                            Some(None) => {
                                let std = self.std();
                                self.ext_hit = true;

                                return format!("{std}.call({prefix}, \"{mname}\"{sep}{inner})");
                            }

                            None => {}
                        }
                    }
                }

                let m = match method {
                    Some(m) => format!(":{}", self.text_of(*m)),

                    None => String::new(),
                };
                let t = match type_args {
                    Some(s) => {
                        let text = self.text_of(*s).to_string();

                        self.lower_type_args(&text)
                    }

                    None => String::new(),
                };
                // `Signal.new<T...>` takes a type pack, not a list of
                // type parameters, so its arguments go in parentheses.
                let t = if prefix == "__alloy.Signal.new" {
                    pack_type_args(&t)
                } else {
                    t
                };
                let a = self.args_text(args);

                format!("{prefix}{m}{t}{a}")
            }

            Step::Child { name, wait } => {
                let n = match name {
                    ChildName::Name(s) => luau_string(self.text_of(*s)),

                    ChildName::Str(s) => self.text_of(*s).to_string(),

                    ChildName::Computed(e) => self.render_to_string(e),
                };

                let call = match (*wait, self.options.wait_timeout) {
                    (true, Some(t)) => format!("{prefix}:WaitForChild({n}, {})", luau_number(t)),

                    (true, None) => format!("{prefix}:WaitForChild({n})"),

                    (false, _) => format!("{prefix}:FindFirstChild({n})"),
                };

                // The checker types the child as `Instance`, which has no
                // `CFrame`; the source names no class, so the check
                // artifact lets the chain continue untyped.
                if self.options.check {
                    format!("({call} :: any)")
                } else {
                    call
                }
            }
        }
    }

    pub(crate) fn args_text(&mut self, args: &CallArgs) -> String {
        match args {
            CallArgs::Paren(list) => {
                let parts: Vec<String> = list.iter().map(|e| self.render_to_string(e)).collect();

                format!("({})", parts.join(", "))
            }

            CallArgs::Table(t) => self.render_to_string(t),

            CallArgs::Str(s) => self.text_of(*s).to_string(),
        }
    }

    /// Emits a lowered chain, copying the field name it ends with. The
    /// lowering is generated text, and a generated byte has no output
    /// position, so completion and hover on the member of an `a?.b` had
    /// nowhere to land. The copied name gives them one.
    pub(crate) fn generate_chain(&mut self, anchor: u32, text: &str, e: &Expr) {
        let Expr::Index {
            key: IndexKey::Field(name),
            ..
        } = e
        else {
            self.generate(anchor, text);

            return;
        };
        let word = self.text_of(*name).to_string();
        let start = self.byte_start(*name);
        let end = self.byte_end(*name);

        // The name has to be the source's own and end the lowering's
        // last field access, or the copy would map bytes out of order.
        let named = self.src.get(start as usize..end as usize) == Some(word.as_str());
        let split = text.rfind(&format!(".{word}")).filter(|at| {
            named
                && text[at + 1 + word.len()..]
                    .chars()
                    .all(|c| c == ')' || c == ' ')
        });

        let Some(at) = split else {
            self.generate(anchor, text);

            return;
        };

        self.generate(anchor, &text[..=at]);
        self.copy(start, end);
        self.generate(anchor, &text[at + 1 + word.len()..]);
    }

    /// A chain in expression position.
    pub(crate) fn chain_expr(&mut self, e: &Expr) -> String {
        self.chain_anchor = self.byte_start(e.span());
        self.check_struct_call(e);
        let parts = self.chain_parts(e);

        self.guarded(parts.guard.as_deref(), &parts.inner)
    }
}

#[cfg(test)]
mod tests {
    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    /// `bnot a` shipped as written: the text scan that routes a
    /// statement through the walk knew every word operator but the
    /// unary one.
    #[test]
    fn bnot_lowers_to_bit32_in_both_artifacts() {
        let src = "local a = 6\nlocal r = bnot a\nlocal t = { v = bnot a }\nprint(r, t.v, (bnot a) + 1)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        for text in [&out.ship, &out.check] {
            assert!(text.contains("local r = bit32.bnot(a)\n"), "{text}");
            assert!(text.contains("{ v = bit32.bnot(a) }"), "{text}");
            assert!(text.contains("(bit32.bnot(a)) + 1"), "{text}");
            assert!(!text.contains("bnot a"), "{text}");
        }
    }

    #[test]
    fn a_top_level_try_returns_the_err_from_the_chunk() {
        let src = "local function one(): Result<number, string>\n    return Ok(1)\nend\nlocal top = try one()\nprint(top)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));
        let out = crate::compile(src).unwrap();
        assert!(
            out.ship
                .contains("if _1.tag == \"Err\" then return _1 end local top = _1._1"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn try_needs_a_function_that_returns_result() {
        let bad = "local function g(): number\n    local v = try Ok(1)\n    return v + 1\nend\nprint(g)\n";
        assert!(
            messages(bad)
                .iter()
                .any(|m| m.starts_with("`try` works only inside a function that returns Result")),
            "{:?}",
            messages(bad)
        );

        let none = "local function g()\n    local v = try Ok(1)\n    return v\nend\nprint(g)\n";
        assert!(
            messages(none)
                .iter()
                .any(|m| m.starts_with("`try` works only inside a function that returns Result")),
            "{:?}",
            messages(none)
        );

        let fine = "local function g(): Result<number, string>\n    local v = try Ok(1)\n    return Ok(v + 1)\nend\nprint(g)\n";
        assert!(messages(fine).is_empty(), "{:?}", messages(fine));

        let alias = "type R = Result<number, string>\nlocal function g(): R\n    local v = try Ok(1)\n    return Ok(v + 1)\nend\nprint(g)\n";
        assert!(messages(alias).is_empty(), "{:?}", messages(alias));
    }

    #[test]
    fn signal_new_takes_its_type_arguments_as_a_pack() {
        let out = crate::compile("local s = Signal.new<<Player, number>>()\nprint(s)\n").unwrap();
        assert!(
            out.check
                .contains("__alloy.Signal.new<<(Player, number)>>()"),
            "{}",
            out.check
        );
    }

    #[test]
    fn a_spread_keeps_the_types_of_its_parts() {
        let out = crate::compile(
            "local base = { x = 1, y = 2 }\nlocal merged = { ...base, z = 3 }\nprint(merged)\n",
        )
        .unwrap();
        assert!(
            out.check.contains(":: typeof(base) & typeof({ z = 3 })"),
            "{}",
            out.check
        );
    }

    #[test]
    fn a_positional_entry_after_a_spread_reports() {
        let src = "local base = { x = 1 }\nlocal t = { ...base, 7 }\nprint(t)\n";
        assert!(
            messages(src)
                .iter()
                .any(|m| m.starts_with("a positional entry after a spread")),
            "{:?}",
            messages(src)
        );
    }
}
