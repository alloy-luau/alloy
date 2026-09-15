//! The `:` items that open a type: the annotation of a binding, a
//! parameter, a field, or a return type, and every `:` inside a type,
//! a generic list, a trait signature, or a type alias. `a:b()` and
//! `a: b` lex the same, so the spacing reads the tree for the answer.

use std::collections::HashSet;

use alloy_syntax::ast::{
    Block, Chunk, ClassMember, Cond, DefaultExport, Expr, FunctionBody, Stmt, TokSpan,
};
use alloy_syntax::lexer::Tok;

use crate::desugar::{Child, expr_children, stmt_children};

/// The byte offsets of every annotation colon in the chunk.
pub(crate) fn annotation_colons(src: &str, toks: &[Tok], chunk: &Chunk) -> HashSet<usize> {
    let mut walk = Walk {
        src,
        toks,
        spans: Vec::new(),
    };
    walk.block(&chunk.block);
    let mut out = HashSet::new();

    for span in walk.spans {
        // The colon in front of the type, `x: T`. A return type after
        // `->`, a generic list, and a trait signature have none.
        if let Some(before) = (span.start as usize).checked_sub(1)
            && toks[before].text(src) == ":"
        {
            out.insert(toks[before].start as usize);
        }

        for t in &toks[span.start as usize..span.end as usize] {
            if t.text(src) == ":" {
                out.insert(t.start as usize);
            }
        }
    }

    out
}

/// The type spans of a tree, in source order.
struct Walk<'a> {
    src: &'a str,
    toks: &'a [Tok],
    spans: Vec<TokSpan>,
}

impl Walk<'_> {
    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        let out = &mut self.spans;

        match s {
            Stmt::Local(l) => out.extend(l.names.iter().filter_map(|b| b.ty)),

            Stmt::GenericFor(f) => out.extend(f.vars.iter().filter_map(|b| b.ty)),

            Stmt::NumericFor(f) => out.extend(f.var.ty),

            Stmt::Struct(st) => {
                out.extend(st.generics);
                out.extend(st.fields.iter().map(|f| f.ty));
            }

            Stmt::Interface(i) => {
                out.extend(i.generics);
                out.extend(i.extends.iter().copied());
                out.extend(i.fields.iter().map(|f| f.ty));
            }

            Stmt::Trait(t) => out.extend(t.methods.iter().map(|m| m.signature)),

            Stmt::Enum(e) => out.extend(e.generics),

            Stmt::Impl(i) => out.extend(i.generics),

            Stmt::Remote(r) => {
                out.extend(r.params.iter().filter_map(|p| p.ty));
                out.extend(r.ret_type);
            }

            Stmt::Attribute(a) => out.extend(a.params.iter().filter_map(|p| p.ty)),

            Stmt::Macro(m) => out.extend(m.params.iter().filter_map(|p| p.ty)),

            Stmt::Class(c) => {
                for m in &c.members {
                    if let ClassMember::Field { ty, .. } = m {
                        out.extend(*ty);
                    }
                }
            }

            // `type function f() ... end` is a body of Luau, not a type;
            // its method calls keep their colons.
            Stmt::TypeAlias(t) => {
                let head = TokSpan::new(t.span.start as usize, t.name.start as usize);

                if !head.text(self.src, self.toks).contains("function") {
                    out.push(t.span);
                }
            }

            Stmt::Declare(d) => out.push(d.span),

            Stmt::If(i) => {
                for (c, _) in &i.branches {
                    self.cond(c);
                }
            }

            Stmt::While(w) => self.cond(&w.cond),

            // The walker leaves a namespace's members to the namespace.
            Stmt::Namespace(ns) => {
                for m in &ns.members {
                    self.stmt(&m.stmt);
                }

                return;
            }

            // The walker hands out the declaration's children, not the
            // declaration, so its own slots are read here.
            Stmt::ExportDefault {
                value: DefaultExport::Decl(inner),
                ..
            } => {
                self.stmt(inner);

                return;
            }

            _ => {}
        }

        for c in stmt_children(s) {
            self.child(c);
        }
    }

    fn child(&mut self, c: Child<'_>) {
        match c {
            Child::Expr(e) => self.expr(e),

            Child::Block(b) => self.block(b),

            Child::Function(f) => self.function(f),
        }
    }

    fn function(&mut self, f: &FunctionBody) {
        self.spans.extend(f.generics);
        self.spans.extend(f.params.iter().filter_map(|p| p.ty));
        self.spans.extend(f.ret_type);
        self.block(&f.block);
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::TypeAssert { ty, .. } | Expr::Satisfies { ty, .. } => self.spans.push(*ty),

            Expr::IfElse { branches, .. } => {
                for (c, _) in branches {
                    self.cond(c);
                }
            }

            _ => {}
        }

        for c in expr_children(e) {
            self.child(c);
        }
    }

    fn cond(&mut self, c: &Cond) {
        if let Cond::Local { bindings, .. } = c {
            self.spans.extend(bindings.iter().filter_map(|b| b.ty));
        }
    }
}
