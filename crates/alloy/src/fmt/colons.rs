//! The `:` items that open a type: the annotation of a binding, a
//! parameter, a field, or a return type, and every `:` inside a type,
//! a generic list, a trait signature, or a type alias. `a:b()` and
//! `a: b` lex the same, so the spacing reads the tree for the answer.
//! The same walk finds each `if` that opens an expression, the
//! condition of each `if` statement, and each chain of binary operators
//! the layout may break.

use std::collections::{HashMap, HashSet};

use alloy_syntax::ast::{
    Block, Chunk, ClassMember, Cond, DefaultExport, Expr, FunctionBody, Stmt, TokSpan,
};
use alloy_syntax::lexer::Tok;

use crate::desugar::{Child, expr_children, stmt_children};

/// The byte offsets of every annotation colon in the chunk.
pub(crate) fn annotation_colons(src: &str, toks: &[Tok], chunk: &Chunk) -> HashSet<usize> {
    let walk = Walk::new(src, toks, chunk);
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

/// The byte offsets of the `if` of every `if` expression in the chunk.
/// The tail of a macro opens its line like a statement does, so only
/// the tree tells that `if` from a statement.
pub(crate) fn expr_ifs(src: &str, toks: &[Tok], chunk: &Chunk) -> HashSet<usize> {
    let walk = Walk::new(src, toks, chunk);

    walk.ifs.iter().map(|&i| toks[i].start as usize).collect()
}

/// A run of binary operators of one precedence, `a * 2 + b - c`, by
/// the byte of each token: its first and its last, and each operator
/// between the operands. The layout breaks a long one before each
/// operator. `then` is the `then` after it when the run is the whole
/// condition of an `if` or an `elseif` statement.
#[derive(Debug)]
pub(crate) struct Chain {
    pub first: usize,
    pub last: usize,
    pub ops: Vec<usize>,
    pub then: Option<usize>,
}

/// The precedence of a binary operator, as the parser reads it. `??`
/// spans two tokens, and `!=` reads as `~=`.
fn precedence(op: &str) -> Option<u8> {
    match op {
        "??" => Some(3),

        "!=" => Some(4),

        _ => alloy_syntax::contextual::binop_priority(op).map(|p| p.0),
    }
}

/// Every binary chain of the chunk, outer before inner. A chain breaks
/// only before an operator that can open a line: a word operator, `bor`
/// or `shl`, stays on the line of its left operand. A `-` in a value arm
/// of a `match` opens the next value when it opens a line, so a chain
/// with one there never breaks either.
pub(crate) fn binary_chains(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Chain> {
    let walk = Walk::new(src, toks, chunk);
    let arms = super::structure::structure(src, toks).value_arm;
    let byte = |i: usize| toks[i].start as usize;
    let text = |i: usize| TokSpan::new(i, i + 1).text(src, toks);
    let mut out: Vec<Chain> = walk
        .chains
        .into_iter()
        .filter(|(_, ops)| {
            ops.iter().all(|&o| {
                let op = if text(o) == "?" { "??" } else { text(o) };

                super::continues(op) && !(arms[o] && op == "-")
            })
        })
        .map(|(span, ops)| Chain {
            first: byte(span.start as usize),
            last: byte(span.end as usize - 1),
            ops: ops.into_iter().map(byte).collect(),
            then: walk.conds.get(&(span.start, span.end)).map(|&t| byte(t)),
        })
        .collect();
    out.sort_by_key(|c| (c.first, std::cmp::Reverse(c.last)));

    out
}

/// The `if` or `elseif` and the `then` of each condition of an `if`
/// statement, by byte.
pub(crate) fn if_conditions(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<(usize, usize)> {
    let walk = Walk::new(src, toks, chunk);

    walk.conds
        .iter()
        .map(|(&(start, _), &then)| {
            (
                toks[start as usize - 1].start as usize,
                toks[then].start as usize,
            )
        })
        .collect()
}

/// The type spans of a tree, in source order, the token of each `if`
/// expression, each binary chain with its operators, and the span of
/// each `if` condition with its `then`.
struct Walk<'a> {
    src: &'a str,
    toks: &'a [Tok],
    spans: Vec<TokSpan>,
    ifs: Vec<usize>,
    chains: Vec<(TokSpan, Vec<usize>)>,
    conds: HashMap<(u32, u32), usize>,
    /// The operators a chain holds already, so an operand of the same
    /// precedence opens no chain of its own.
    in_chain: HashSet<u32>,
}

impl<'a> Walk<'a> {
    fn new(src: &'a str, toks: &'a [Tok], chunk: &Chunk) -> Self {
        let mut walk = Walk {
            src,
            toks,
            spans: Vec::new(),
            ifs: Vec::new(),
            chains: Vec::new(),
            conds: HashMap::new(),
            in_chain: HashSet::new(),
        };
        walk.block(&chunk.block);

        walk
    }

    /// The operators of the chain under `e`, each of precedence `prec`.
    /// A parenthesized operand is one operand.
    fn chain_ops(&mut self, e: &Expr, prec: u8, ops: &mut Vec<usize>) {
        if let Expr::Binary { op, lhs, rhs, .. } = e
            && precedence(op.text(self.src, self.toks)) == Some(prec)
        {
            self.in_chain.insert(op.start);
            self.chain_ops(lhs, prec, ops);
            ops.push(op.start as usize);
            self.chain_ops(rhs, prec, ops);
        }
    }

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

                    let span = c.span();

                    if matches!(c, Cond::Expr(_))
                        && self
                            .toks
                            .get(span.end as usize)
                            .is_some_and(|t| t.text(self.src) == "then")
                    {
                        self.conds.insert((span.start, span.end), span.end as usize);
                    }
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

            Expr::IfElse { branches, span, .. } => {
                self.ifs.push(span.start as usize);

                for (c, _) in branches {
                    self.cond(c);
                }
            }

            Expr::Binary { op, span, .. } if !self.in_chain.contains(&op.start) => {
                if let Some(prec) = precedence(op.text(self.src, self.toks)) {
                    let mut ops = Vec::new();
                    self.chain_ops(e, prec, &mut ops);
                    self.chains.push((*span, ops));
                }
            }

            // `c ? a : b` is a chain of its `?` and its `:`, so a long one
            // breaks there, one branch a line, and not inside a branch.
            Expr::Ternary {
                cond,
                then_value,
                span,
                ..
            } => {
                let (q, colon) = (cond.span().end as usize, then_value.span().end as usize);
                let text = |i: usize| TokSpan::new(i, i + 1).text(self.src, self.toks);

                if text(q) == "?" && text(colon) == ":" {
                    self.chains.push((*span, vec![q, colon]));
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
