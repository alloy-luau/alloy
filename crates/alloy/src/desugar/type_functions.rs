//! The body of a `type function`.
//!
//! The type checker runs the body, and the emit copies it as written. A
//! form the emit lowers elsewhere would reach Luau as it stands, so the
//! compiler reports each one here and names it. The body takes no
//! lowering: most forms need the runtime or a name of the module, and a
//! type function reads neither.
//!
//! The rules below follow `stmt_needs_desugar` and `expr_needs_desugar`,
//! which say what the emit rewrites. A new form there needs a name here.

use alloy_syntax::ast::{Attr, Block, Cond, Expr, FunctionBody, Stmt, TableField, TokSpan};

use super::{Child, Desugar, expr_children, stmt_children};

/// The first token of a form, and the words a report names it by.
type Form = (usize, String);

impl Desugar<'_> {
    pub(crate) fn type_function_body(&mut self, body: &FunctionBody) {
        let found = self.form_in_body(body).or_else(|| {
            // Alloy syntax in a type, `T[]` or `~T`: the emit writes a
            // name of the module there, which the body cannot read.
            let (start, end) = (self.byte_start(body.span), self.byte_end(body.span));
            let byte = self.earliest_edit_start(start, end)?;
            let at = self.toks.iter().position(|t| t.start == byte)?;

            Some((at, "this type syntax".to_string()))
        });

        if let Some((at, name)) = found {
            let report = format!(
                "{name} is Alloy; a type function body is Luau: the type checker runs it and the compiler copies it as written"
            );
            self.diagnose(TokSpan::new(at, at + 1), &report);
        }
    }

    fn quoted(&self, at: usize) -> String {
        format!("`{}`", self.text_of(TokSpan::new(at, at + 1)))
    }

    /// Luau has `@native`, `@checked` and `@deprecated` of its own, and
    /// the list form `@[...]`.
    fn alloy_attr(&self, attrs: &[Attr]) -> Option<Form> {
        attrs.iter().find_map(|a| {
            let name = a.name?;

            (!matches!(self.text_of(name), "native" | "checked" | "deprecated"))
                .then(|| (a.span.start as usize, format!("`@{}`", self.text_of(name))))
        })
    }

    fn form_in_body(&self, body: &FunctionBody) -> Option<Form> {
        let at = body.span.start as usize;
        let own = if body.is_async.is_some() {
            Some((at, "`async`".to_string()))
        } else if body.generator.is_some() {
            Some((at, "`function*`".to_string()))
        } else if body.lambda {
            Some((at, "a lambda".to_string()))
        } else if let Some(arrow) = body.ret_arrow {
            Some((arrow.start as usize, "`->` for a return type".to_string()))
        } else if body.has_bounds {
            Some((at, "a bound on a type parameter".to_string()))
        } else {
            body.params.iter().find_map(|p| {
                let name = if p.default.is_some() {
                    "a default of a parameter"
                } else if p.destructure.is_some() {
                    "a pattern in a parameter"
                } else if !p.attributes.is_empty() {
                    "an attribute on a parameter"
                } else {
                    return None;
                };

                Some((p.name.start as usize, name.to_string()))
            })
        };

        own.or_else(|| self.form_in_block(&body.block))
    }

    fn form_in_block(&self, b: &Block) -> Option<Form> {
        b.stmts
            .iter()
            .find_map(|s| self.form_in_stmt(s))
            .or_else(|| {
                // Alloy fences a statement after a `return`; Luau refuses it.
                let exit = b.stmts.iter().find(|s| super::is_early_exit(s))?;

                super::block_has_dead_code(b).then(|| {
                    (
                        exit.span().start as usize,
                        "a statement after the way out of a block".to_string(),
                    )
                })
            })
    }

    fn form_in_stmt(&self, s: &Stmt) -> Option<Form> {
        let at = s.span().start as usize;
        let word = || Some((at, self.quoted(at)));
        let named = |name: &str| Some((at, name.to_string()));
        let own = match s {
            Stmt::Attributed { attrs, .. } => self.alloy_attr(attrs),

            Stmt::Return(r) if r.value_only => named("a value with no `return`"),

            Stmt::Assign(a) if a.op.end - a.op.start >= 2 => {
                Some((a.op.start as usize, format!("`{}`", self.text_of(a.op))))
            }

            Stmt::Local(l) if l.names.iter().any(|b| b.destructure.is_some()) => {
                named("a pattern in a `local`")
            }

            Stmt::Local(l) if l.using || l.exported => word(),

            Stmt::Function(f) if f.exported => word(),

            Stmt::LocalFunction(f) if f.exported => word(),

            Stmt::Function(f) => self.alloy_attr(&f.attrs),

            Stmt::LocalFunction(f) => self.alloy_attr(&f.attrs),

            Stmt::GenericFor(f) if f.filter.is_some() => named("`where` on a loop"),

            Stmt::GenericFor(f) if super::for_needs_rewrite(f) => named("a pattern in a loop"),

            Stmt::If(i) if super::if_has_local(i) => named("`if local`"),

            Stmt::While(w) if matches!(w.cond, Cond::Local { .. }) => named("`while local`"),

            Stmt::Namespace(_)
            | Stmt::Delete { .. }
            | Stmt::Yield { .. }
            | Stmt::Defer { .. }
            | Stmt::Destroy { .. }
            | Stmt::After(_)
            | Stmt::Parallel(_)
            | Stmt::Import(_)
            | Stmt::ExportList(_)
            | Stmt::ExportDefault { .. }
            | Stmt::Enum(_)
            | Stmt::Impl(_)
            | Stmt::Match(_)
            | Stmt::Struct(_)
            | Stmt::Trait(_)
            | Stmt::Interface(_)
            | Stmt::Remote(_)
            | Stmt::Message(_)
            | Stmt::Attribute(_)
            | Stmt::Class(_)
            | Stmt::Macro(_) => word(),

            Stmt::PatternLocal(_) => named("a pattern in a `local`"),

            _ => None,
        };

        own.or_else(|| {
            stmt_children(s)
                .into_iter()
                .find_map(|c| self.form_in_child(c))
        })
    }

    fn form_in_child(&self, c: Child<'_>) -> Option<Form> {
        match c {
            Child::Expr(e) => self.form_in_expr(e),

            Child::Block(b) => self.form_in_block(b),

            Child::Function(f) => self.form_in_body(f),
        }
    }

    fn form_in_expr(&self, e: &Expr) -> Option<Form> {
        let at = e.span().start as usize;
        let named = |name: &str| Some((at, name.to_string()));
        let own = match e {
            Expr::Binary { op, .. } if op.end - op.start == 2 => {
                Some((op.start as usize, format!("`{}`", self.text_of(*op))))
            }

            Expr::Index { optional: true, .. } | Expr::Call { optional: true, .. } => {
                named("an optional chain, `?.`")
            }

            Expr::Child { .. } => named("a child lookup, `->`"),

            Expr::NonNil { .. } => named("a non-nil mark, `!`"),

            Expr::Ternary { .. } => named("a ternary, `? :`"),

            Expr::Pipe { .. } => named("`|>`"),

            Expr::Is { .. } => named("`is`"),

            Expr::Satisfies { .. } => named("`satisfies`"),

            Expr::Array { .. } => named("an array literal"),

            Expr::Comprehension(_) => named("a comprehension"),

            Expr::Bail { .. } => named("a way out after `??`"),

            Expr::MethodRef { .. } => named("a method reference"),

            Expr::Macro { .. } => named("a macro call"),

            Expr::New { .. }
            | Expr::Await { .. }
            | Expr::Try { .. }
            | Expr::AsyncBlock { .. }
            | Expr::TryBlock { .. }
            | Expr::Match(_) => Some((at, self.quoted(at))),

            Expr::IfElse { branches, .. }
                if branches
                    .iter()
                    .any(|(c, _)| matches!(c, Cond::Local { .. })) =>
            {
                named("`if local`")
            }

            Expr::Table { fields, .. }
                if fields.iter().any(|f| matches!(f, TableField::Spread(_))) =>
            {
                named("a spread in a table")
            }

            _ => None,
        };

        own.or_else(|| {
            expr_children(e)
                .into_iter()
                .find_map(|c| self.form_in_child(c))
        })
        .or_else(|| {
            // What is left of a chain is a part with `?` or `<<T>>`.
            (matches!(e, Expr::Index { .. } | Expr::Call { .. })
                && (super::chain_has_alloy(e) || super::expressions::chain_has_type_args(e)))
            .then(|| (at, "an optional chain or `<<T>>`".to_string()))
        })
    }
}
