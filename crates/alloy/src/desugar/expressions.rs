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
    pub(crate) guards: Vec<String>,
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

/// A written type that is one plain name, `Box` or `Enum.Material`.
/// Any other shape gives `None`: a record type, a union, a generic
/// instantiation, a function type.
fn plain_type_name(value: &str) -> Option<&str> {
    let head = value.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    let rest = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');

    (head && rest).then_some(value)
}

/// The child operator a report quotes: `=>` waits, `->` finds.
fn child_op(wait: bool) -> &'static str {
    match wait {
        true => "=>",

        false => "->",
    }
}

/// `a` or `an` for the type name a report quotes. The test reads both
/// cases, or a type named `E` takes `a`.
fn article(ty: &str) -> &'static str {
    match ty.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) {
        true => "an",

        false => "a",
    }
}

/// The first `...`, `break`, or `continue` in a value block's body that
/// would leave it: `...` anywhere outside a nested function, and a
/// `break` or `continue` outside a loop of the block's own.
pub(crate) fn body_escape(block: &Block) -> Option<(TokSpan, &'static str)> {
    fn in_expr(e: &Expr) -> Option<(TokSpan, &'static str)> {
        if let Expr::Vararg(at) = e {
            return Some((*at, "`...`"));
        }

        children(super::expr_children(e), false)
    }

    fn children(kids: Vec<super::Child<'_>>, in_loop: bool) -> Option<(TokSpan, &'static str)> {
        kids.into_iter().find_map(|c| match c {
            super::Child::Expr(e) => in_expr(e),

            super::Child::Block(b) => in_block(b, in_loop),

            // A nested function has `...` and loops of its own.
            super::Child::Function(_) => None,
        })
    }

    fn in_block(b: &Block, in_loop: bool) -> Option<(TokSpan, &'static str)> {
        b.stmts.iter().find_map(|s| match s {
            Stmt::Break(at) if !in_loop => Some((*at, "`break`")),

            Stmt::Continue(at) if !in_loop => Some((*at, "`continue`")),

            Stmt::While(_) | Stmt::Repeat(_) | Stmt::NumericFor(_) | Stmt::GenericFor(_) => {
                children(super::stmt_children(s), true)
            }

            _ => children(super::stmt_children(s), in_loop),
        })
    }

    in_block(block, false)
}

/// A source slice with every run of whitespace as one space, so it fits/// A source slice with every run of whitespace as one space, so it fits
/// on the line the generated text sits on.
pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl<'s> Desugar<'s> {
    /*
    Renders an expression. A hoist goes in front of the statement, so it
    runs first. That is wrong when `e` runs on some paths only, when the
    statement has called code already, and when `e` calls code after the
    statement has read a value. Then `e` keeps its hoists, see
    `expr_in_place`.
    */
    pub(crate) fn expr(&mut self, e: &Expr) {
        // A field of `new S { }` constructs under its declared type.
        if let Some(g) = self
            .field_expected
            .remove(&(std::ptr::from_ref(e) as usize))
        {
            let saved = self.expected_generic.replace(g);
            self.expr(e);
            self.expected_generic = saved;

            return;
        }

        // A table literal runs nothing before its fields, so each field
        // keeps its own hoists instead. A closure around the whole literal
        // types it `{ x: number }`, and Luau rejects that where `{ x:
        // number? }` is asked, because a table's fields are invariant.
        let plain_table = matches!(e, Expr::Table { fields, .. }
            if !fields.iter().any(|f| matches!(f, TableField::Spread(_))));

        if !plain_table && (self.lazy || self.effects || (self.reads && any_part(e, &calls_code))) {
            self.expr_in_place(e, |d| d.expr_node(e));
        } else {
            self.expr_node(e);
        }

        if calls_code(e) {
            self.effects = true;
        } else if matches!(e, Expr::Name(_) | Expr::Index { .. }) {
            self.reads = true;
        }
    }

    fn expr_node(&mut self, e: &Expr) {
        let anchor = self.byte_start(e.span());

        match e {
            Expr::Name(span) => {
                let name = self.text_of(*span);

                if let Some(path) = self.renamed(name) {
                    self.generate(anchor, &path);
                } else if AMBIENT.contains(&name) && !self.is_local(name) {
                    self.check_std_name(*span, name);
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
                        // `2 in { 5, 6 }` searches a raw table by key, so
                        // it holds. A `{ }` literal with items meant the
                        // values; a keyed one, `{ rect = true }`, is the
                        // Lua set idiom and reads right.
                        let mut table = rhs.as_ref();

                        while let Expr::Paren { inner, .. } = table {
                            table = inner;
                        }

                        if let Expr::Table { fields, span } = table
                            && fields
                                .iter()
                                .any(|f| matches!(f, TableField::Positional(_)))
                        {
                            // The array form of the literal as written.
                            let written = one_line(self.text_of(*span));
                            let items =
                                written.trim_start_matches('{').trim_end_matches('}').trim();
                            let message = format!(
                                "`in` on a `{{ }}` literal searches its keys, 1, 2, and on, not its items; write `[ {items} ]` to search the items"
                            );
                            self.diagnose(*span, &message);
                        }

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
                if let Expr::Index { object, .. } = func.as_ref()
                    && let Expr::Name(n) = object.as_ref()
                {
                    self.check_std_name(*n, "Signal");
                }

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
                let a = self.render_lazy(then_value);
                let b = self.render_lazy(else_value);
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
                // An empty literal has no element type. The check artifact
                // casts it to `Array<any>`, so the binding still reads as
                // an array. An annotation on the left still wins: `any`
                // accepts every element type.
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
                let close_text = if cast {
                    format!("}}) :: {}Array<any>)", self.type_std())
                } else {
                    "})".to_string()
                };
                self.generate(close, &close_text);
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
                    // `new Stack<<number>> { }` and `local s: Stack<number>`
                    // name the arguments the typed constructor takes, the
                    // way the call form passes them.
                    let t = match type_args {
                        Some(s) => {
                            let text = self.text_of(*s).to_string();

                            self.lower_type_args(&text)
                        }

                        None => self.expected_args_for(name),
                    };
                    let ctor = self.raw_ctor(&n);
                    let open = if full_view {
                        format!("(({ctor}{t}(")
                    } else {
                        format!("{ctor}{t}(")
                    };
                    self.generate(anchor, &open);

                    // A field's constructor takes the arguments its declared
                    // type names, as a `local` under an annotation does.
                    if let Expr::Table { fields, .. } = table
                        && let Some(types) = self.struct_field_types.get(&n).cloned()
                    {
                        for f in fields {
                            if let TableField::Named { name, value } = f
                                && let Some(ft) =
                                    types.iter().find(|t| t.name == self.text_of(*name))
                                && let Some(g) = super::types::generic_head(self.text_of(ft.ty))
                            {
                                self.field_expected
                                    .insert(std::ptr::from_ref(value) as usize, g);
                            }
                        }
                    }

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

                // The body runs as a function of its own, so a `...`, a
                // `break`, or a `continue` that reaches past it emits Luau
                // the compiler refuses.
                if let Some((at, what)) = body_escape(block) {
                    let word = if is_try { "try do" } else { "async do" };
                    let advice = match what {
                        "`...`" => "copy it to a local before the block, `local args = { ... }`",

                        _ => "give the block a value and act on it after the block",
                    };
                    let message = format!(
                        "{what} cannot reach past a `{word}` block, which runs as a function of its own; {advice}"
                    );
                    self.diagnose(at, &message);
                }
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
                // The body is a function of its own: its last line is its
                // own `return`, whatever sink an arm around it writes.
                let sink = self.value_sink.take();
                self.block(block);
                self.value_sink = sink;
                self.try_targets.pop();
                let after_block = self.block_end_or(block, body_start);
                self.copy(after_block, end_tok.start);
                self.copy(end_tok.start, end_tok.end);
                self.generate(end_tok.end, ")");
            }

            Expr::Macro { name, args, span } => {
                let mname = self.text_of(*name).to_string();

                if let Some(m) = self.macro_of(&mname).cloned() {
                    let text = self.expand_macro(&m, &mname, args, *span);
                    self.generate(anchor, &text);
                } else {
                    let text = self.intrinsic(*name, args, *span);
                    self.generate(anchor, &text);
                }
            }

            // A match whose arms run statements has no expression form:
            // Luau's if-expression holds no statement, and a closure would
            // stop a `return` in an arm from leaving the function.
            Expr::Match(m) if super::statements::block_arm_match(e).is_some() => {
                self.diagnose(
                    m.span,
                    "a match whose arms run statements stands after `local x =`, `x =`, or `return`; bind it to a local first",
                );
                self.blank_lines(anchor, self.byte_end(m.span));
                self.generate(anchor, "nil");
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
                    // The right side of `and` and `or`, and an `if`
                    // expression past its first condition, run on some
                    // paths only.
                    let lazy_from = match e {
                        Expr::Binary { op, .. } if matches!(self.text_of(*op), "and" | "or") => 1,

                        Expr::IfElse { .. } => 1,

                        _ => usize::MAX,
                    };
                    // A call reads its callee before its arguments. A
                    // call rarely changes the callee, so that read does
                    // not keep an argument's hoists in place.
                    let callee = match e {
                        Expr::Call { func, .. } => Some(std::ptr::from_ref::<Expr>(func)),

                        _ => None,
                    };
                    let mut at = 0;
                    self.stitch(e.span(), &children, |d, child| {
                        at += 1;

                        match child {
                            Child::Expr(c) if callee == Some(std::ptr::from_ref::<Expr>(c)) => {
                                let reads = d.reads;
                                d.expr(c);
                                d.reads = reads;
                            }

                            Child::Expr(c) => d.expr_lazy(at > lazy_from, c),

                            Child::Block(b) => d.block(b),

                            Child::Function(b) => d.function_block(b),
                        }
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

    /// `Signal.new` on the std `Signal`, not on a local of that name, or
    /// `s.Signal.new` through a star import of the std.
    pub(crate) fn is_signal_new(&self, func: &Expr) -> bool {
        let Expr::Index {
            object,
            key: IndexKey::Field(f),
            ..
        } = func
        else {
            return false;
        };
        let through_std = |e: &Expr| {
            matches!(e, Expr::Index { object, key: IndexKey::Field(k), .. }
                if self.text_of(*k) == "Signal"
                    && matches!(object.as_ref(), Expr::Name(n)
                        if self.std_namespaces.contains_key(self.text_of(*n))))
        };

        (matches!(object.as_ref(), Expr::Name(n)
            if self.text_of(*n) == "Signal" && !self.is_local("Signal"))
            || through_std(object))
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
    a temp so it evaluates once. The right side evaluates only when the left
    is nil, which is the point, so it keeps its own hoists: see
    `expr_in_place`.
    */
    pub(crate) fn coalesce(&mut self, span: TokSpan, lhs: &Expr, rhs: &Expr) {
        let anchor = self.byte_start(span);
        let left = self.reusable(lhs);
        let right = self.render_lazy(rhs);
        self.generate(
            anchor,
            &format!("(if {left} == nil then {right} else {left})"),
        );
    }

    /*
    `x is T` by the name on the right. A primitive tests `type`, a Roblox
    datatype tests `typeof`, an Instance class tests `IsA`, an enum tests
    the `EnumType`, and any other name is an Alloy struct's metatable.

    A trait and an interface name no metatable, so `is` refuses them;
    see `no_nominal_test`.

    A type alias is a second spelling of one type, so the test reads
    through it: `type B = Box` tests `Box`. See `alias_head`.
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

        // One step through an alias, and only to a name the chain
        // below answers. A trait's alias lands on the trait's report,
        // since the resolve runs before the guard.
        let n = self.alias_head(&n).unwrap_or(n);
        let test = if PRIMITIVES.contains(&n.as_str()) {
            format!("type({x}) == \"{n}\"")
        } else if let Some(item) = n.strip_prefix("Enum.") {
            format!("typeof({x}) == \"EnumItem\" and {x}.EnumType == Enum.{item}")
        } else if INSTANCE_CLASSES.contains(&n.as_str()) {
            if n == "Instance" {
                // The root class: `typeof` alone answers.
                format!("typeof({x}) == \"Instance\"")
            } else {
                // The solver refines a value typed `any` through `typeof`
                // to a type whose `IsA` it cannot call. The cast calls it
                // on an Instance, and the solver still narrows `x`.
                format!("typeof({x}) == \"Instance\" and ({x} :: Instance):IsA(\"{n}\")")
            }
        } else if DATATYPES.contains(&n.as_str()) {
            format!("typeof({x}) == \"{n}\"")
        } else if self.enums.contains_key(&n) {
            format!("{n}.is({x})")
        } else if let Some(message) = self.no_nominal_test(&n, expr) {
            self.diagnose(name, &message);

            // The metatable test would never be true, so the branch
            // would be dead. `false` says that and reads as one.
            "false".to_string()
        } else {
            format!("getmetatable({}) == {n}", self.any_cast(&x))
        };

        if negated {
            format!("(not ({test}))")
        } else {
            format!("({test})")
        }
    }

    /// Why `x is T` cannot hold for this name, or `None` when it can.
    ///
    /// The metatable test needs a name a value carries. A trait is a
    /// table of default methods, never a metatable; an interface is a
    /// shape with no value at all; a remote is a channel and an
    /// attribute is metadata. `new` refuses the same four names.
    fn no_nominal_test(&self, n: &str, expr: &Expr) -> Option<String> {
        let names = self.name_candidates(n);
        // A trait reads from the prescan's own index, so a test above
        // the declaration reports too, and from the import index, so a
        // trait another module declares reports here.
        let is_trait = names.iter().any(|k| self.trait_required.contains_key(k))
            || self
                .options
                .import_trait_methods
                .iter()
                .any(|(t, _)| names.contains(t));

        if is_trait {
            return Some(format!(
                "`{n}` is a trait; a value is never exactly a trait; name the type that implements it{}",
                self.implementor_hint(&names, expr)
            ));
        }

        if let Some(value) = self.alias_shape(n) {
            return Some(format!(
                "`{n}` is an alias of `{value}`; `is` has no test for that type; name a struct, an enum, or a primitive"
            ));
        }

        let kind = names
            .iter()
            .find_map(|k| self.not_constructible.get(k.as_str()).copied())?;

        match kind {
            "interface" => Some(format!(
                "`{n}` is an interface; a value is never exactly an interface; name a struct that has its fields"
            )),

            "remote" => Some(format!(
                "`{n}` is a remote, a channel and not a type; `is` takes a type name"
            )),

            "attribute" => Some(format!(
                "`{n}` is an attribute, metadata and not a type; `is` takes a type name"
            )),

            // A trait answers above, through an index the prescan fills.
            _ => None,
        }
    }

    /// The name `x is T` tests when `T` is a type alias of this file:
    /// one step, and only to a name the chain in `is_test` answers or
    /// refuses. `None` leaves the name as the source writes it.
    ///
    /// A chain of aliases resolves no further than one step, so
    /// `type A = B` over another alias reports the way any name the
    /// file cannot resolve does.
    pub(crate) fn alias_head(&self, n: &str) -> Option<String> {
        let head = plain_type_name(self.alias_values.get(n)?)?;
        let answered = PRIMITIVES.contains(&head)
            || head.starts_with("Enum.")
            || INSTANCE_CLASSES.contains(&head)
            || DATATYPES.contains(&head)
            || self.enums.contains_key(head)
            || self.structs.contains(head)
            || self.imported_names.contains(head)
            || self.trait_required.contains_key(head)
            || self.not_constructible.contains_key(head);

        answered.then(|| head.to_string())
    }

    /// The value of an alias that names no type `is` can test: a record
    /// type, a union, a generic instantiation. A value carries a name
    /// for none of them, so the metatable test could never hold.
    fn alias_shape(&self, n: &str) -> Option<&str> {
        let value = self.alias_values.get(n)?;

        plain_type_name(value).is_none().then_some(value.as_str())
    }

    /// The tail that names a concrete target, ", `thing is Box`", when
    /// this file writes an `impl Trait for Box` and the operand is a
    /// plain name. An empty text otherwise.
    fn implementor_hint(&self, names: &[String], expr: &Expr) -> String {
        let Expr::Name(v) = expr else {
            return String::new();
        };
        let Some((target, _)) = self
            .impl_traits
            .iter()
            .find(|(_, met)| met.iter().any(|t| names.contains(t)))
        else {
            return String::new();
        };

        format!(", `{} is {target}`", self.text_of(*v))
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
        let flags = (self.effects, self.reads);
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

                self.render_side(|d| {
                    d.generate(anchor, &format!("{std}.{helper}("));
                    d.expr(inner);
                    d.generate(anchor, ")");
                })
            }

            other => self.render_to_side(other),
        };
        // The value runs in front of the statement, see `render_hoisted`.
        (self.effects, self.reads) = flags;
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
                    false,
                );
            }

            None => {
                let returned = self.any_cast(&temp);
                self.hoist_stmt(
                    format!("if {temp}.tag == \"Err\" then return {returned} end"),
                    anchor,
                    true,
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
        // The struct name counts as no read: the arguments cannot change
        // what it holds, so their hoists may go in front of the statement.
        // The closure a read forces defeats Luau's checker on `??`.
        let reads = self.reads;
        let n = self.render_to_string(name);
        self.reads = reads;
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

    `inner` is the expression as it stands; `guards` are the values whose
    nil makes the whole result nil. An optional link needs its prefix named
    once, so the prefix becomes a temp (or stays, when it is simple) and
    the guard moves to it. A plain link applies inside the current guard,
    which is the chain rule: one `?` guards every later link. `!` names
    the prefix the same way, then ends the guard with an `error` branch.

    In place, a prefix of names and fields is read again instead: each
    `?` adds its prefix to `guards`, and the chain needs no temp.
    */
    pub(crate) fn chain_parts(&mut self, e: &Expr) -> ChainParts {
        let (base, links) = flatten(e);
        self.check_child_chain(base, &links);
        let timed_waits = self.options.wait_timeout.is_some();
        // A timed `WaitForChild` can return nil, so the link after it guards.
        let mut pending_guard = false;

        // What the chain has called or read when a prefix becomes a temp
        // runs in front of the statement, see `render_hoisted`.
        let start = (self.effects, self.reads);
        // The base is the callee or the receiver of what follows. Its
        // read is the chain's own, so it does not count as an earlier one.
        let reads = self.reads;
        // A string literal as a receiver needs parentheses in Luau.
        let mut inner = match base {
            Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. } if !links.is_empty() => {
                format!("({})", self.render_to_string(base))
            }

            _ => self.render_to_string(base),
        };
        self.reads = reads;

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
            // A relative path in an `init.luau` starts one folder up, as
            // it does for an `import` statement.
            let a = self.require_literal(&a);
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
        // Names and fields only, `?` links included: safe to read again.
        // The checker narrows a field path and not a computed key, so a
        // key ends it.
        let mut rereadable = inner_simple;
        let mut guards: Vec<String> = Vec::new();

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
                guards: Vec::new(),
                inner: format!("{inner}.{method}{targs}{a}"),
            };
        }

        // `c.HashMap.new()` through `import * as c` takes the same
        // arguments, under `c.HashMap<K, V>` or a bare `HashMap<K, V>`.
        if let (Expr::Name(n), Some((base_name, args_text))) = (base, self.expected_generic.clone())
            && let [
                Link::Plain(Step::Field(m)),
                Link::Plain(Step::Field(f)),
                Link::Plain(Step::Call {
                    method: None,
                    type_args: None,
                    args,
                }),
            ] = links.as_slice()
            && matches!(self.text_of(*f), "new" | "from" | "with_capacity")
            && self.star_modules.contains(self.text_of(*n))
            && (base_name == format!("{}.{}", self.text_of(*n), self.text_of(*m))
                || base_name == self.text_of(*m))
        {
            let (ty, method) = (self.text_of(*m).to_string(), self.text_of(*f).to_string());
            let a = self.args_text(args);
            let targs = self.lower_type_args(&format!("<<{args_text}>>"));

            return ChainParts {
                guards: Vec::new(),
                inner: format!("{inner}.{ty}.{method}{targs}{a}"),
            };
        }

        let count = links.len();

        for (i, link) in links.into_iter().enumerate() {
            let link = match link {
                Link::Plain(step) if pending_guard => Link::Optional(step),

                other => other,
            };
            pending_guard = false;
            self.last_link = i + 1 == count;

            match link {
                Link::Plain(step) => {
                    inner_simple = inner_simple && matches!(step, Step::Field(_));
                    rereadable = rereadable && matches!(step, Step::Field(_));
                    pending_guard = timed_waits && matches!(step, Step::Child { wait: true, .. });
                    // Past a `?`, a step runs only when the prefix is not
                    // nil, and so do its arguments.
                    inner = self.apply_step(!guards.is_empty(), &inner, &step);
                }

                Link::Optional(step) => {
                    let hoists = self.hoists.len();
                    let name = self.name_prefix(&mut inner, &mut guards, inner_simple, rereadable);

                    if self.hoists.len() > hoists {
                        (self.effects, self.reads) = start;
                    }

                    inner_simple = false;
                    rereadable = rereadable && matches!(step, Step::Field(_));
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
                    inner = self.apply_step(true, &callee, &step);
                }

                Link::NonNil { span } => {
                    let hoists = self.hoists.len();
                    let name = self.name_prefix(&mut inner, &mut guards, inner_simple, false);

                    if self.hoists.len() > hoists {
                        (self.effects, self.reads) = start;
                    }

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
                    rereadable = false;
                    // `!` ends the guard: past it the value is never nil.
                    guards.clear();
                }
            }
        }

        ChainParts { guards, inner }
    }

    /// Applies a step, as code that runs on some paths only when `lazy`
    /// is set. A call runs after its arguments, so a later argument
    /// counts it.
    fn apply_step(&mut self, lazy: bool, prefix: &str, step: &Step<'_>) -> String {
        let saved = std::mem::replace(&mut self.lazy, lazy);
        let text = self.apply(prefix, step);
        self.lazy = saved;

        if matches!(step, Step::Call { .. } | Step::Child { .. }) {
            self.effects = true;
        }

        text
    }

    /// Names the current prefix so a link can test it and then use it.
    /// In place, a prefix safe to read again stays as it is and joins the
    /// guards; any other becomes a temp that replaces them.
    pub(crate) fn name_prefix(
        &mut self,
        inner: &mut String,
        guards: &mut Vec<String>,
        inner_simple: bool,
        rereadable: bool,
    ) -> String {
        if (guards.is_empty() && inner_simple) || (self.in_place && rereadable) {
            guards.push(inner.clone());

            return inner.clone();
        }

        let whole = self.guarded(guards, inner);
        let anchor = self.chain_anchor;
        let name = self.hoist_text(whole, anchor);
        *guards = vec![name.clone()];
        *inner = name.clone();

        name
    }

    /// `(if g == nil then nil else inner)`, with one test per guard.
    pub(crate) fn guarded(&self, guards: &[String], inner: &str) -> String {
        if guards.is_empty() {
            return inner.to_string();
        }

        let tests: Vec<String> = guards.iter().map(|g| format!("{g} == nil")).collect();

        format!("(if {} then nil else {inner})", tests.join(" or "))
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

                // The source names no class. A chain that goes on past the
                // child continues untyped, since `Instance` has no
                // `CFrame`. A child that ends the chain is what the call
                // returns: `->` finds an `Instance` or nil, so `is`
                // narrows it, and `=>` an `Instance`, as Roblox types
                // `WaitForChild` with a timeout too.
                if self.options.check {
                    let ty = match (*wait, self.last_link) {
                        (_, false) => "any",

                        (true, true) => "Instance",

                        (false, true) => "Instance?",
                    };

                    format!("({call} :: {ty})")
                } else {
                    call
                }
            }
        }
    }

    /// Reports the two mistakes a child access makes, in the words the
    /// file wrote. `->` lowers to `FindFirstChild` and `=>` lowers to
    /// `WaitForChild`, so the checker names a method the author never
    /// typed; the check artifact also casts a child to `any`, which
    /// hides a call on it. Both reports land on the operator.
    fn check_child_chain(&mut self, base: &Expr, links: &[Link<'_>]) {
        // Only the first link reads the base. A later child reads a
        // child, and a child is an `Instance`.
        if let Some(
            Link::Plain(Step::Child { name, wait }) | Link::Optional(Step::Child { name, wait }),
        ) = links.first()
            && let Expr::Name(n) = base
        {
            let who = self.text_of(*n).to_string();
            let ty = self.binding_types.get(&who).cloned().unwrap_or_default();

            if !ty.is_empty() && self.not_instance(ty.trim_end_matches('?')) {
                let verb = if *wait { "waits for" } else { "reads" };
                let span = self.child_op_span(name);
                self.diagnose(
                    span,
                    &format!(
                        "`{}` {verb} a child of an `Instance`; `{who}` is {} `{ty}`",
                        child_op(*wait),
                        article(&ty)
                    ),
                );
            }
        }

        for pair in links.windows(2) {
            let [
                Link::Plain(Step::Child { name, wait })
                | Link::Optional(Step::Child { name, wait }),
                Link::Plain(Step::Call { method: None, .. })
                | Link::Optional(Step::Call { method: None, .. }),
            ] = pair
            else {
                continue;
            };
            let span = self.child_op_span(name);
            self.diagnose(
                span,
                &format!(
                    "`{}` gives an `Instance`; an `Instance` is not a function",
                    child_op(*wait)
                ),
            );
        }
    }

    /// Whether a written type can never be an `Instance`. The types the
    /// file names for certain answer here; a class, an `any`, a generic,
    /// or an alias leaves the report to the checker.
    fn not_instance(&self, ty: &str) -> bool {
        self.known_type(ty)
            || ty == "unknown"
            || ty.starts_with('{')
            || self.declared_fields(ty).is_some()
    }

    /// The `->` or `=>` token of a child access. The name follows the
    /// operator, and `->[k]` puts a bracket between the two.
    fn child_op_span(&self, name: &ChildName) -> TokSpan {
        let at = match name {
            ChildName::Name(s) | ChildName::Str(s) => s.start as usize,

            ChildName::Computed(e) => e.span().start as usize,
        };
        let back = (1..=2)
            .find(|b| {
                at.checked_sub(*b)
                    .and_then(|i| self.toks.get(i))
                    .is_some_and(|t| matches!(t.text(self.src), "->" | "=>"))
            })
            .unwrap_or(1);
        let op = at.saturating_sub(back);

        TokSpan::new(op, op + 1)
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

        self.guarded(&parts.guards, &parts.inner)
    }
}

#[cfg(test)]
mod tests {
    use crate::EmitOptions;

    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    /// A hoist that must stay in place wraps the field value, never the
    /// table around it: a closure types its table `{ x: number }`, which
    /// Luau refuses where `{ x: number? }` is asked. A struct name read
    /// before the fields keeps nothing in place.
    #[test]
    fn a_table_keeps_its_place_and_a_field_keeps_its_hoist() {
        let options = EmitOptions {
            check: true,
            ..EmitOptions::default()
        };
        let out = crate::compile_with("take(g(), { x = tonumber(s) ?? 0 })\n", &options).unwrap();
        assert!(
            out.check
                .contains("take(g(), { x = (function() local _1 = tonumber(s) return"),
            "{}",
            out.check
        );

        let out = crate::compile_with(
            "import { P } from \"./p\"\nlocal p = new P { x = tonumber(s) ?? 0 }\n",
            &options,
        )
        .unwrap();
        assert!(!out.check.contains("(function()"), "{}", out.check);
    }

    /// An empty `[ ]` carries `Array<any>` in the check artifact, so a
    /// binding of one reads as an array and not as `any`. Every typed
    /// position keeps the type it writes.
    #[test]
    fn an_empty_array_literal_types_as_an_array() {
        let src = concat!(
            "struct Hub as\n",
            "    systems: number[] = []\n",
            "end\n",
            "function take(xs: number[])\n",
            "    print(xs)\n",
            "end\n",
            "function make(): number[]\n",
            "    return []\n",
            "end\n",
            "local a = []\n",
            "local b: number[] = []\n",
            "local filled = [1, 2]\n",
            "local nested = [ [], [] ]\n",
            "take([])\n",
            "print(a, b, filled, nested, make(), Hub)\n",
        );
        let options = EmitOptions {
            check: true,
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        // The struct default, the return, the two bindings, the two of
        // the nested literal, and the argument.
        assert_eq!(
            out.check
                .matches("(__alloy.Array.from({}) :: __alloy.Array<any>)")
                .count(),
            7,
            "{}",
            out.check
        );
        // The annotation on the left still decides the element type.
        assert!(
            out.check
                .contains("local b: __alloy.Array<number> = (__alloy.Array.from({})"),
            "{}",
            out.check
        );
        // A literal with items infers its own element type, with no cast.
        assert!(
            out.check
                .contains("local filled = __alloy.Array.from({1, 2})"),
            "{}",
            out.check
        );
        // The ship artifact carries no cast at all.
        let ship = crate::compile(src).unwrap().ship;

        assert!(!ship.contains("Array<any>"), "{ship}");
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

    /// The lowering reads `FindFirstChild`, and the checker then named
    /// a method the file never wrote. The compiler names the operator.
    #[test]
    fn a_child_operator_reports_its_own_receiver() {
        let src = "struct Box as\n    width: number\nend\nlocal function t(n: number, s: string, b: Box, u: unknown, tbl: { x: number })\n    print(n->Foo, s->Foo, b->Foo, u->Foo, tbl->Foo, n=>Foo)\nend\nprint(t)\n";

        assert_eq!(
            messages(src),
            vec![
                "`->` reads a child of an `Instance`; `n` is a `number`",
                "`->` reads a child of an `Instance`; `s` is a `string`",
                "`->` reads a child of an `Instance`; `b` is a `Box`",
                "`->` reads a child of an `Instance`; `u` is an `unknown`",
                "`->` reads a child of an `Instance`; `tbl` is a `{ x: number }`",
                "`=>` waits for a child of an `Instance`; `n` is a `number`",
            ]
        );
    }

    /// A receiver whose type this file cannot name stays with the
    /// checker; a guess here would be a false report.
    #[test]
    fn a_child_operator_leaves_a_type_it_cannot_name() {
        let src = "local function t(i: Instance, a: any, p: BasePart)\n    print(workspace->Map, i->Map, a->Map, p=>Map)\nend\nprint(t)\n";

        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    /// A name the file binds a second time with no annotation types
    /// nothing. The map of annotations is flat, so the parameter of one
    /// function must not decide the same name in another.
    #[test]
    fn a_child_operator_reads_no_type_across_two_bindings() {
        let src = "local function a(n: number)\n    print(n)\nend\nlocal function b(n)\n    print(n->X)\nend\nprint(a, b)\n";

        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    /// `ins=>Foo(2)` calls the child. The check artifact casts a child
    /// to `any`, so the checker saw no call on an `Instance` at all.
    #[test]
    fn a_call_on_a_child_reports() {
        let src = "local ins = script\nprint(ins=>Foo(2), ins->Foo(2), ins=>Foo=>Bar(2))\n";

        assert_eq!(
            messages(src),
            vec![
                "`=>` gives an `Instance`; an `Instance` is not a function",
                "`->` gives an `Instance`; an `Instance` is not a function",
                "`=>` gives an `Instance`; an `Instance` is not a function",
            ]
        );

        // A method call and an index read the child, which is what the
        // operators are for.
        let fine =
            "local ins = script\nprint(ins=>Foo:IsA(\"Part\"), ins=>Foo[1], ins=>Foo.Name)\n";
        assert!(messages(fine).is_empty(), "{:?}", messages(fine));
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

    /// `x is Drawable` emitted `getmetatable(x) == Drawable`. A value
    /// carries its own struct for a metatable, never a trait and never
    /// an interface, so the branch was dead and nothing said so.
    #[test]
    fn is_refuses_a_trait_and_an_interface() {
        let src = "trait Drawable as\n    function draw(self): string\nend\n\nstruct Box as\n    width: number\nend\n\nimpl Drawable for Box as\n    function draw(self): string\n        return \"box\"\n    end\nend\n\nlocal function check(thing: unknown)\n    if thing is Drawable then print(1) end\n    if thing is not Sized then print(2) end\nend\nprint(check)\n\ninterface Sized as\n    width: number\nend\n";
        assert_eq!(
            messages(src),
            vec![
                "`Drawable` is a trait; a value is never exactly a trait; name the type that implements it, `thing is Box`".to_string(),
                "`Sized` is an interface; a value is never exactly an interface; name a struct that has its fields".to_string(),
            ]
        );

        let out = crate::compile(src).unwrap();

        for text in [&out.ship, &out.check] {
            assert!(!text.contains("getmetatable"), "{text}");
        }
    }

    /// A remote and an attribute reached the metatable test too, and
    /// each is a value at run time, so the Luau checker said nothing
    /// either: the branch was dead and silent. The declarations sit
    /// below the use, which the prescan sees and the statement walk
    /// does not.
    #[test]
    fn is_refuses_a_remote_and_an_attribute() {
        let src = "local function probe(x: unknown)\n    print(x is Hit)\n    print(x is tag)\nend\nprint(probe)\n\nattribute tag(name: string) on function\n\nremote Hit(id: string) from client\n";
        assert_eq!(
            messages(src),
            vec![
                "`Hit` is a remote, a channel and not a type; `is` takes a type name".to_string(),
                "`tag` is an attribute, metadata and not a type; `is` takes a type name"
                    .to_string(),
            ]
        );

        let out = crate::compile(src).unwrap();

        for text in [&out.ship, &out.check] {
            assert!(!text.contains("getmetatable"), "{text}");
        }
    }

    /// The same for a trait another module declares: the import index
    /// carries the trait's methods, and the name is a value here, so
    /// the metatable test compiled and never held.
    #[test]
    fn is_refuses_an_imported_trait() {
        let options = EmitOptions {
            import_trait_methods: vec![("Drawable".to_string(), Vec::new())],
            import_types: vec![("./lib".to_string(), vec!["Drawable".to_string()])],
            ..EmitOptions::default()
        };
        let src = "import { Drawable } from \"./lib\"\nlocal function check(thing: unknown)\n    print(thing is Drawable)\nend\nprint(check)\n";
        let out = crate::compile_with(src, &options).expect("compiles");

        assert_eq!(
            out.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
            vec![
                "`Drawable` is a trait; a value is never exactly a trait; name the type that implements it".to_string(),
            ]
        );
        assert!(!out.ship.contains("getmetatable"), "{}", out.ship);
    }

    /// A struct keeps the metatable test, and the report lands on the
    /// type name, not on the operand.
    #[test]
    fn is_keeps_the_metatable_test_for_a_struct() {
        let src = "struct Box as\n    width: number\nend\nlocal function check(thing: unknown)\n    if thing is Box then print(thing.width) end\nend\nprint(check)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));

        let out = crate::compile(src).unwrap();
        assert!(
            out.ship.contains("getmetatable(thing) == Box"),
            "{}",
            out.ship
        );

        let bad = "trait Drawable as\n    function draw(self): string\nend\nlocal function check(thing: unknown)\n    print(thing is Drawable)\nend\nprint(check)\n";
        let out = crate::compile(bad).unwrap();
        let at = out.diagnostics.first().expect("one report");
        assert_eq!(&bad[at.start as usize..at.end as usize], "Drawable");
    }

    /// `is` was the one construct that read a type alias as a type of
    /// its own: `type B = Box` then `x is B` tested a name no value
    /// carries. The test reads through the alias now, one step, to the
    /// name the alias spells another way. `Below` sits under the use,
    /// which the prescan sees.
    #[test]
    fn is_reads_through_a_type_alias() {
        let src = "import { Crate } from \"./lib\"\n\nstruct Box as\n    width: number\nend\n\nenum Color as\n    Red\n    Blue\nend\n\ntype Num = number\ntype Vec = Vector3\ntype PartLike = Part\ntype Material = Enum.Material\ntype B = Box\ntype Hue = Color\ntype Crated = Crate\n\nlocal function probe(x: unknown)\n    print(x is Num)\n    print(x is Vec)\n    print(x is PartLike)\n    print(x is Material)\n    print(x is B)\n    print(x is Hue)\n    print(x is Crated)\n    print(x is Below)\n    print(x is not B)\nend\n\ntype Below = Box\n\nprint(probe)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));

        let out = crate::compile(src).unwrap();

        for want in [
            "type(x) == \"number\"",
            "typeof(x) == \"Vector3\"",
            "typeof(x) == \"Instance\" and (x :: Instance):IsA(\"Part\")",
            "typeof(x) == \"EnumItem\" and x.EnumType == Enum.Material",
            "getmetatable(x) == Box",
            "Color.is(x)",
            "getmetatable(x) == Crate",
            "not (getmetatable(x) == Box)",
        ] {
            assert!(out.ship.contains(want), "{want}\n{}", out.ship);
        }
    }

    /// An alias of a shape has no nominal answer: no value carries the
    /// name of a record type, a union, or one instantiation of a
    /// generic. An alias of a trait and of an interface resolves first,
    /// so each lands on the report the name itself gets.
    #[test]
    fn is_refuses_an_alias_with_no_nominal_test() {
        let src = "struct Box as\n    width: number\nend\n\ntrait Drawable as\n    function draw(self): string\nend\n\ninterface Sized as\n    width: number\nend\n\nimpl Drawable for Box as\n    function draw(self): string\n        return \"box\"\n    end\nend\n\ntype Rec = { a: number }\ntype Either = number | string\ntype Holder = Box<number>\ntype Draw = Drawable\ntype Fits = Sized\n\nlocal function probe(x: unknown)\n    print(x is Rec)\n    print(x is Either)\n    print(x is Holder)\n    print(x is Draw)\n    print(x is Fits)\nend\n\nprint(probe)\n";
        assert_eq!(
            messages(src),
            vec![
                "`Rec` is an alias of `{ a: number }`; `is` has no test for that type; name a struct, an enum, or a primitive".to_string(),
                "`Either` is an alias of `number | string`; `is` has no test for that type; name a struct, an enum, or a primitive".to_string(),
                "`Holder` is an alias of `Box<number>`; `is` has no test for that type; name a struct, an enum, or a primitive".to_string(),
                "`Drawable` is a trait; a value is never exactly a trait; name the type that implements it, `x is Box`".to_string(),
                "`Sized` is an interface; a value is never exactly an interface; name a struct that has its fields".to_string(),
            ]
        );

        let out = crate::compile(src).unwrap();

        for text in [&out.ship, &out.check] {
            assert!(!text.contains("getmetatable"), "{text}");
        }
    }

    /// One step, not a walk: a chain of aliases needs a visited set to
    /// stop a loop, and nobody would maintain one. `x is Chain` keeps
    /// the name the source writes, and the checker reports it the way
    /// it reports any name it cannot resolve.
    #[test]
    fn is_resolves_one_alias_and_no_further() {
        let src = "struct Box as\n    width: number\nend\n\ntype B = Box\ntype Chain = B\n\nlocal function probe(x: unknown)\n    print(x is Chain)\nend\n\nprint(probe)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));

        let out = crate::compile(src).unwrap();
        assert!(
            out.ship.contains("getmetatable(x) == Chain"),
            "{}",
            out.ship
        );
    }
}
