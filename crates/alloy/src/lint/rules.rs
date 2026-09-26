//! The lint pass itself: `run` walks the tokens and the top-level
//! statements of one file and reports every hit. `directive_lints`,
//! `const_reassignments`, and `matching` are its private helpers.

use std::collections::HashSet;

use alloy_syntax::ast::{Chunk, ImportKind, Stmt};
use alloy_syntax::lexer::{Tok, TokKind};

use crate::fmt::structure;

use super::{Fix, Lint, Thresholds};

/// The lints about the directives themselves: an
/// `--@alloy-expect-error` with no reason after it.
fn directive_lints(src: &str) -> Vec<Lint> {
    let directives = crate::directives::scan(src);

    directives
        .missing_reason
        .iter()
        .map(|at| {
            let (line_start, end) = crate::directives::span_of_line(src, *at);
            // The directive may trail code; the lint is about the
            // directive, so the range starts at the comment.
            let start = src[line_start..end]
                .find(crate::directives::EXPECT)
                .map_or(line_start, |i| line_start + i);

            Lint {
                name: crate::directives::MISSING_REASON,
                start: start as u32,
                end: end as u32,
                message: format!(
                    "`{}` says nothing about why the line must fail; write the reason after it",
                    crate::directives::EXPECT
                ),
                fix: None,
            }
        })
        .collect()
}

/// The `const` members a namespace holds, as the dotted paths a source
/// writes: `Cfg.LIMIT`, `Cfg.Inner.LIMIT`. A private member reaches no
/// other file, so it is left out.
pub fn namespace_consts(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    prefix: &str,
) -> Vec<String> {
    let text = |span: alloy_syntax::ast::TokSpan| span.text_or_empty(src, toks);
    let mut out = Vec::new();

    for m in &ns.members {
        if m.is_private(src, toks) {
            continue;
        }

        match m.stmt.under_default() {
            alloy_syntax::ast::Stmt::Local(l) if l.is_const => {
                for b in &l.names {
                    out.push(format!("{prefix}.{}", text(b.name)));
                }
            }

            alloy_syntax::ast::Stmt::Namespace(inner) => {
                let deeper = format!("{prefix}.{}", text(inner.name));
                out.extend(namespace_consts(src, toks, inner, &deeper));
            }

            _ => {}
        }
    }

    out
}

/// `const N = 3` then `N = 4`: the byte range of each reassignment and
/// its message.
///
/// `alloy doc const` says a reassignment is a compile error. Luau
/// reports it as a syntax error in the emit, which only `alloy flux`
/// runs, and in the checker's words.
///
/// The walk keeps a scope per block, so a `local` or a parameter of the
/// same name hides the `const`, and every name of `const a, b` and of a
/// `const { a, b }` destructure counts. A `const` inside a namespace is
/// named by its dotted path, `Cfg.LIMIT`, since that is what a source
/// writes to reach it; `namespace_consts` holds those paths.
pub fn const_reassignments(
    src: &str,
    toks: &[Tok],
    block: &alloy_syntax::ast::Block,
    namespace_consts: &[String],
) -> Vec<(u32, u32, String)> {
    walk_bindings(src, toks, block, namespace_consts).out
}

/// `export local count = 0`, then `count += 1` inside a function. The
/// module returns its exports as a table it builds when it loads, and an
/// import copies each value, so the write never reaches an importer. A
/// write at the top level runs before the return, and stays quiet.
fn stale_exports(src: &str, toks: &[Tok], block: &alloy_syntax::ast::Block) -> Vec<Lint> {
    walk_bindings(src, toks, block, &[]).stale
}

fn walk_bindings<'a>(
    src: &'a str,
    toks: &'a [Tok],
    block: &alloy_syntax::ast::Block,
    namespace_consts: &'a [String],
) -> ConstWalk<'a> {
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();
    let mut exported = Vec::new();

    for s in &block.stmts {
        match s {
            Stmt::Local(l) if l.exported => exported.extend(
                crate::desugar::statements::local_names(l)
                    .into_iter()
                    .map(text),
            ),

            Stmt::ExportList(e) if e.from.is_none() => {
                exported.extend(e.specs.iter().map(|spec| text(spec.name)))
            }

            _ => {}
        }
    }

    let mut walk = ConstWalk {
        src,
        toks,
        scopes: Vec::new(),
        namespace_consts,
        exported,
        deferred: 0,
        out: Vec::new(),
        stale: Vec::new(),
    };
    walk.block(block);

    walk
}

/// The scopes of one `const_reassignments` walk: each name a block
/// binds, with whether it is a `const`.
struct ConstWalk<'a> {
    src: &'a str,
    toks: &'a [Tok],
    scopes: Vec<Vec<(String, bool)>>,
    namespace_consts: &'a [String],
    /// The names the module exports: `export local x` and the names of
    /// an `export { }` list.
    exported: Vec<String>,
    /// How many function bodies hold the statement the walk reads. A
    /// statement in one runs after the module has loaded.
    deferred: usize,
    out: Vec<(u32, u32, String)>,
    /// The `stale_export` hits.
    stale: Vec<Lint>,
}

impl ConstWalk<'_> {
    fn text(&self, span: alloy_syntax::ast::TokSpan) -> String {
        span.text(self.src, self.toks).to_string()
    }

    fn bind(&mut self, span: alloy_syntax::ast::TokSpan, is_const: bool) {
        let name = self.text(span);

        if let Some(scope) = self.scopes.last_mut() {
            scope.push((name, is_const));
        }
    }

    /// Whether the innermost binding of `name` is a `const`.
    fn is_const(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|(n, _)| n == name)
            .is_some_and(|(_, c)| *c)
    }

    /// Whether `name` reads a top-level binding that the module exports
    /// by value: not a `const`, and not a nearer binding of the name.
    fn is_exported(&self, name: &str) -> bool {
        self.exported.iter().any(|n| n == name)
            && !self.is_const(name)
            && self
                .scopes
                .iter()
                .rposition(|scope| scope.iter().any(|(n, _)| n == name))
                == Some(0)
    }

    fn block(&mut self, b: &alloy_syntax::ast::Block) {
        self.scopes.push(Vec::new());

        for s in &b.stmts {
            self.stmt(s);
        }

        self.scopes.pop();
    }

    fn function(&mut self, body: &alloy_syntax::ast::FunctionBody) {
        self.deferred += 1;
        self.scopes.push(Vec::new());

        for p in body.params.iter().filter(|p| !p.is_vararg) {
            match &p.destructure {
                Some(d) => {
                    for n in crate::desugar::statements::destructure_names(d) {
                        self.bind(n, false);
                    }
                }

                None => self.bind(p.name, false),
            }
        }

        for s in &body.block.stmts {
            self.stmt(s);
        }

        self.scopes.pop();
        self.deferred -= 1;
    }

    fn children(&mut self, children: Vec<crate::desugar::Child<'_>>) {
        for child in children {
            match child {
                crate::desugar::Child::Expr(e) => self.children(crate::desugar::expr_children(e)),

                crate::desugar::Child::Block(b) => self.block(b),

                crate::desugar::Child::Function(f) => self.function(f),
            }
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        use alloy_syntax::ast::Expr;

        match s.under_default() {
            Stmt::Local(l) => {
                self.children(crate::desugar::stmt_children(s.under_default()));

                for n in crate::desugar::statements::local_names(l) {
                    self.bind(n, l.is_const);
                }
            }

            Stmt::PatternLocal(p) => {
                self.children(crate::desugar::stmt_children(s.under_default()));
                let is_const = self.text(p.keyword) == "const";

                for n in crate::desugar::statements::pattern_binds(&p.pattern) {
                    self.bind(n, is_const);
                }
            }

            Stmt::LocalFunction(f) => {
                self.bind(f.name, f.is_const);
                self.function(&f.body);
            }

            Stmt::NumericFor(f) => {
                for e in [&f.start, &f.limit].into_iter().chain(f.step.as_ref()) {
                    self.children(crate::desugar::expr_children(e));
                }

                self.scopes.push(Vec::new());
                self.bind(f.var.name, false);
                self.block(&f.block);
                self.scopes.pop();
            }

            Stmt::GenericFor(f) => {
                for e in &f.exprs {
                    self.children(crate::desugar::expr_children(e));
                }

                self.scopes.push(Vec::new());

                for v in &f.vars {
                    self.bind(v.name, false);
                }

                self.block(&f.block);
                self.scopes.pop();
            }

            // A namespace member reads its siblings by their bare names.
            Stmt::Namespace(ns) => {
                self.scopes.push(Vec::new());

                for m in &ns.members {
                    self.stmt(&m.stmt);
                }

                self.scopes.pop();
            }

            // A macro body is source for another place.
            Stmt::Macro(_) => {}

            // `after 3 do ... end` runs its block once the module has
            // loaded, as a function body does.
            Stmt::After(_) => {
                self.deferred += 1;
                self.children(crate::desugar::stmt_children(s.under_default()));
                self.deferred -= 1;
            }

            Stmt::Assign(a) => {
                for t in &a.targets {
                    // `Cfg.LIMIT`: the path of a namespace `const`.
                    let mut path = Vec::new();
                    let mut at = t;

                    while let Expr::Index {
                        object,
                        key: alloy_syntax::ast::IndexKey::Field(k),
                        optional: false,
                        ..
                    } = at
                    {
                        path.push(self.text(*k));
                        at = object;
                    }

                    let Expr::Name(root) = at else {
                        continue;
                    };
                    path.push(self.text(*root));
                    path.reverse();
                    let path = path.join(".");
                    let reassigned = match path.contains('.') {
                        true => self.namespace_consts.contains(&path),

                        false => self.is_const(&path),
                    };
                    let span = t.span();
                    let start = self.toks[span.start as usize].start;
                    let end = self.toks[span.end as usize - 1].end;

                    if self.deferred > 0 && self.is_exported(&path) {
                        self.stale.push(Lint {
                            name: "stale_export",
                            start,
                            end,
                            message: format!(
                                "`{path}` is an exported `local`; importers keep the value they read when they loaded, so they never see this write. Export a function that returns it, or hold it in a table that stays `local` and write its field: `export local state = {{ {path} = ... }}`, then `state.{path} = ...`"
                            ),
                            fix: None,
                        });
                    }

                    if reassigned {
                        let message = format!(
                            "`{path}` is a `const`; its value is set once and a reassignment is an error"
                        );
                        self.out.push((start, end, message));
                    }
                }

                self.children(crate::desugar::stmt_children(s.under_default()));
            }

            other => self.children(crate::desugar::stmt_children(other)),
        }
    }
}

/// One function in the token stream.
struct Fn {
    /// The `function` token.
    at: usize,
    /// The name path, empty for an anonymous function.
    path: Vec<usize>,
    /// The `(` of the parameter list.
    open: usize,
    /// The `)` of the parameter list.
    close: usize,
    /// The `end`, when the structure found it.
    end: Option<usize>,
    /// Parameters: name token, has a type, the type is optional.
    params: Vec<(usize, bool, bool)>,
    has_return_type: bool,
    optional_return: bool,
    /// The return annotation names `Result`.
    returns_result: bool,
    exported: bool,
    /// Inside an `impl` or a `trait`.
    in_impl: bool,
}

/// A use of a namespace the file declares `@deprecated`. The attribute
/// has no Luau form on a namespace, so nothing else reports it.
fn deprecated_namespaces(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks);
    // The name, the message the attribute carries, and the byte range
    // of the declaration. A member reads a sibling by its own name, so
    // a hit inside the body is the long way to write it, not a use.
    let mut marked: Vec<(&str, String, u32, u32)> = Vec::new();

    for stmt in &chunk.block.stmts {
        let Stmt::Namespace(ns) = stmt.under_default() else {
            continue;
        };
        let Some(attr) = ns
            .attributes
            .iter()
            .find(|a| a.name.map(text) == Some("deprecated"))
        else {
            continue;
        };
        let note = attr
            .args
            .first()
            .map(|e| {
                let text_of = |e: &alloy_syntax::ast::Expr| {
                    src[toks[e.span().start as usize].start as usize
                        ..toks[e.span().end as usize - 1].end as usize]
                        .trim_matches(['"', '\''])
                        .to_string()
                };

                // `{ use = "New", reason = "why" }` reads as the reason,
                // then the name to use.
                let alloy_syntax::ast::Expr::Table { fields, .. } = e else {
                    return format!("; {}", text_of(e));
                };
                let key = |k: &str| {
                    fields.iter().find_map(|f| match f {
                        alloy_syntax::ast::TableField::Named { name, value }
                            if text(*name) == k =>
                        {
                            Some(text_of(value))
                        }

                        _ => None,
                    })
                };

                match (key("reason"), key("use")) {
                    (Some(r), Some(u)) => format!("; {r}; use `{u}`"),

                    (Some(r), None) => format!("; {r}"),

                    (None, Some(u)) => format!("; use `{u}`"),

                    (None, None) => String::new(),
                }
            })
            .unwrap_or_default();
        let start = toks[ns.span.start as usize].start;
        let end = toks[ns.span.end as usize - 1].end;
        marked.push((text(ns.name), note, start, end));
    }

    if marked.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Ident {
            continue;
        }

        // A field of another value spelled the same names no namespace.
        if i > 0 && matches!(toks[i - 1].text(src), "." | ":" | "?." | "?:") {
            continue;
        }

        let Some((name, note, start, end)) = marked.iter().find(|(n, _, _, _)| *n == t.text(src))
        else {
            continue;
        };

        if t.start >= *start && t.start < *end {
            continue;
        }

        out.push(Lint {
            name: "deprecated_namespace",
            start: t.start,
            end: t.end,
            message: format!("`{name}` is deprecated{note}"),
            fix: None,
        });
    }

    out
}

/// `struct P as` over a body on the next line. `as` joins a header to a
/// body on its own line, `enum Dir as Up, Down end`; below it, the line
/// break opens the body, so each layout has one spelling. fmt drops the
/// word too.
fn redundant_as(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    fn headers(stmt: &Stmt, out: &mut Vec<alloy_syntax::ast::TokSpan>) {
        match stmt.under_default() {
            Stmt::Namespace(ns) => {
                out.push(ns.span);

                for m in &ns.members {
                    headers(&m.stmt, out);
                }
            }

            Stmt::Struct(s) => out.push(s.span),

            Stmt::Enum(e) => out.push(e.span),

            Stmt::Trait(t) => out.push(t.span),

            Stmt::Interface(i) => out.push(i.span),

            Stmt::Impl(i) => out.push(i.span),

            _ => {}
        }
    }

    let mut spans = Vec::new();

    for stmt in &chunk.block.stmts {
        headers(stmt, &mut spans);
    }

    let same_line =
        |a: usize, b: usize| !src[toks[a].end as usize..toks[b].start as usize].contains('\n');
    let mut out = Vec::new();

    for span in spans {
        let (start, end) = (span.start as usize, (span.end as usize).min(toks.len()));
        let Some(word) = (start..end).find(|&i| {
            matches!(
                toks[i].text(src),
                "struct" | "enum" | "trait" | "interface" | "namespace" | "impl"
            )
        }) else {
            continue;
        };
        // The last token of the header's line.
        let mut last = word;

        while last + 1 < end && same_line(last, last + 1) {
            last += 1;
        }

        if last > word && last + 1 < end && toks[last].text(src) == "as" {
            out.push(Lint {
                name: "redundant_as",
                start: toks[last].start,
                end: toks[last].end,
                message: "`as` joins a header to a body on the same line; this body starts on the next line, so drop `as`".to_string(),
                fix: Some(Fix::new(src, toks[last - 1].end, toks[last].end, "")),
            });
        }
    }

    out
}

/// `import Players from "game:Players"`: the service path of the
/// release before this one. The fix writes the alias form, `"@game"`
/// and `"@game/Players"`, and keeps the quote the line wrote.
fn game_alias(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    let mut out = Vec::new();

    for stmt in &chunk.block.stmts {
        let Stmt::Import(i) = stmt else {
            continue;
        };
        let Some(first) = toks.get(i.path.start as usize) else {
            continue;
        };
        let end = toks[(i.path.end as usize)
            .saturating_sub(1)
            .max(i.path.start as usize)]
        .end;
        let written = &src[first.start as usize..end as usize];
        let quote = written.chars().next().unwrap_or('"');
        let spec = written.trim_matches(['"', '\'']);

        if !crate::game_import::is_old_spelling(spec) {
            continue;
        }

        let Some(alias) = crate::game_import::alias_form(spec) else {
            continue;
        };

        out.push(Lint {
            name: "game_alias",
            start: first.start,
            end,
            message: format!(
                "`{quote}{spec}{quote}` is the old service path; write `{quote}{alias}{quote}`"
            ),
            fix: Some(Fix::new(
                src,
                first.start,
                end,
                format!("{quote}{alias}{quote}"),
            )),
        });
    }

    out
}

pub fn run(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    definitions: bool,
    thresholds: &Thresholds,
    import_privates: &[(String, Vec<String>)],
    import_callables: &[(String, crate::flux::Callable)],
) -> Vec<Lint> {
    let mut lints = Vec::new();

    if definitions {
        return lints;
    }

    lints.extend(directive_lints(src));
    lints.extend(redundant_as(src, toks, chunk));
    lints.extend(deprecated_namespaces(src, toks, chunk));
    lints.extend(game_alias(src, toks, chunk));
    lints.extend(stale_exports(src, toks, &chunk.block));

    let text = |i: usize| toks[i].text(src);
    let st = structure(src, toks);
    let line_of = |i: usize| st.lines[i];

    // The `impl` and `trait` blocks, as token ranges.
    let impl_ranges: Vec<(usize, usize)> = toks
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            matches!(t.text(src), "impl" | "trait")
                && !matches!(
                    i.checked_sub(1).map(text),
                    Some("." | ":" | "?." | "?:" | "function" | "local")
                )
        })
        .filter_map(|(i, _)| st.ends[i].map(|e| (i, e)))
        .collect();

    // `impl Trait for X as`: the ranges whose methods a trait types.
    let trait_impls: Vec<(usize, usize)> = impl_ranges
        .iter()
        .filter(|(a, _)| {
            text(*a) == "impl"
                && (a + 1..toks.len())
                    .take_while(|&j| text(j) != "as")
                    .any(|j| text(j) == "for")
        })
        .copied()
        .collect();

    // Every function.
    let mut fns: Vec<Fn> = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        // A `type function` takes types, which Luau lets no one annotate.
        // `x is function` names a type and declares nothing.
        if t.text(src) != "function"
            || matches!(i.checked_sub(1).map(text), Some("." | ":" | "type"))
            || alloy_syntax::contextual::tested_type_at(src, toks, i)
        {
            continue;
        }

        let mut path = Vec::new();
        let mut j = i + 1;

        while j < toks.len()
            && matches!(toks[j].kind, TokKind::Ident | TokKind::Dot | TokKind::Colon)
        {
            if toks[j].kind == TokKind::Ident {
                path.push(j);
            }

            j += 1;
        }

        if j >= toks.len() || text(j) != "(" {
            continue;
        }

        let open = j;
        let Some(close) = matching(src, toks, open) else {
            continue;
        };
        let mut params = Vec::new();
        let mut k = open + 1;

        while k < close {
            // One parameter runs to the comma at depth zero.
            let mut depth = 0i32;
            let mut m = k;

            while m < close {
                let tt = text(m);

                // `<` opens a type argument list: `Result<T, E>` holds a
                // comma that separates no parameters.
                if tt.ends_with('(') || tt.ends_with('[') || tt.ends_with('{') || tt == "<" {
                    depth += 1;
                } else if matches!(tt, ")" | "]" | "}" | ">") {
                    depth -= 1;
                } else if tt == ">>" {
                    depth -= 2;
                } else if tt == "," && depth == 0 {
                    break;
                }

                m += 1;
            }

            if toks[k].kind == TokKind::Ident && text(k) != "self" {
                let typed = k + 1 < m && text(k + 1) == ":";
                let mut ty_end = m;

                for x in k + 2..m {
                    if text(x) == "=" {
                        ty_end = x;

                        break;
                    }
                }

                let optional = typed && ty_end > k + 2 && text(ty_end - 1) == "?";
                params.push((k, typed, optional));
            }

            k = m + 1;
        }

        let after = close + 1;
        let has_return_type = after < toks.len() && matches!(text(after), ":" | "->");
        let mut optional_return = false;
        let mut returns_result = false;

        if has_return_type {
            // The annotation runs to the end of the `)` line.
            let line = line_of(close);
            let mut last = after;

            while last + 1 < toks.len() && line_of(last + 1) == line {
                last += 1;
            }

            optional_return = text(last) == "?";
            returns_result = (after..=last).any(|k| text(k) == "Result");
        }

        let prev = i.checked_sub(1).map(text);
        let prev2 = i.checked_sub(2).map(text);
        let exported = matches!(prev, Some("export") | Some("global"))
            || (prev == Some("async") && matches!(prev2, Some("export") | Some("global")));
        let private =
            prev == Some("private") || (prev == Some("async") && prev2 == Some("private"));
        let in_impl = !private && impl_ranges.iter().any(|(a, b)| *a < i && i < *b);

        fns.push(Fn {
            at: i,
            path,
            open,
            close,
            end: st.ends[i],
            params,
            has_return_type,
            optional_return,
            returns_result,
            exported,
            in_impl,
        });
    }

    // Names the file declares, so a global of the same name is not one.
    let mut declared: HashSet<&str> = HashSet::new();

    for f in &fns {
        for (p, _, _) in &f.params {
            declared.insert(text(*p));
        }

        if let Some(&n) = f.path.first() {
            declared.insert(text(n));
        }
    }

    for (i, t) in toks.iter().enumerate() {
        match t.text(src) {
            "local" => {
                let mut j = i + 1;

                while j < toks.len() && toks[j].kind == TokKind::Ident {
                    declared.insert(text(j));
                    j += 1;

                    if j < toks.len() && text(j) == "," {
                        j += 1;
                    } else {
                        break;
                    }
                }
            }

            "for" => {
                let mut j = i + 1;

                while j < toks.len() && !matches!(text(j), "in" | "=" | "do") {
                    if toks[j].kind == TokKind::Ident {
                        declared.insert(text(j));
                    }

                    j += 1;
                }
            }

            _ => {}
        }
    }

    // implicit_any and missing_return_type.
    for f in &fns {
        let named = !f.path.is_empty();

        if named {
            for (p, typed, _) in &f.params {
                if !typed {
                    lints.push(Lint {
                        name: "implicit_any",
                        start: toks[*p].start,
                        end: toks[*p].end,
                        message: format!(
                            "parameter `{}` has no type, so it is `any`; write `{}: T`",
                            text(*p),
                            text(*p)
                        ),
                        fix: None,
                    });
                }
            }
        }

        let name_tok = *f.path.last().unwrap_or(&f.at);
        // A metamethod's shape is Luau's, and a trait impl's method
        // takes the return type its trait declares.
        let answered = text(name_tok).starts_with("__")
            || trait_impls.iter().any(|(a, b)| *a < f.at && f.at < *b);

        if named && !f.has_return_type && (f.exported || f.in_impl) && !answered {
            lints.push(Lint {
                name: "missing_return_type",
                start: toks[name_tok].start,
                end: toks[name_tok].end,
                message: format!(
                    "`{}` is public and has no return type; write `): T` after the parameters",
                    text(name_tok)
                ),
                fix: None,
            });
        }
    }

    // implicit_any on a binding whose only value is an array literal
    // with an empty `[ ]` in it. Nothing writes the element type, so
    // the checker reads `any[]`.
    for i in 0..toks.len() {
        if text(i) != "local" {
            continue;
        }

        // One name, then the `=`: `local xs, ys = ...` names no single
        // binding for the message.
        let name = i + 1;

        if toks.get(name).map(|t| t.kind) != Some(TokKind::Ident)
            || toks.get(name + 1).map(|t| t.text(src)) != Some("=")
            || toks.get(name + 2).map(|t| t.text(src)) != Some("[")
        {
            continue;
        }

        let Some(close) = matching(src, toks, name + 2) else {
            continue;
        };

        // An empty `[ ]` anywhere in the literal: `[]` itself, and the
        // inner ones of `[[], []]`.
        if !(name + 2..close).any(|k| text(k) == "[" && text(k + 1) == "]") {
            continue;
        }

        lints.push(Lint {
            name: "implicit_any",
            start: toks[name].start,
            end: toks[name].end,
            message: format!(
                "`{}` has an empty array literal and no type, so its elements are `any`; write `{}: T[]`",
                text(name),
                text(name)
            ),
            fix: None,
        });
    }

    // optional_access: a `T?` parameter indexed with nothing guarding it.
    for f in &fns {
        let Some(end) = f.end else { continue };

        for (p, _, optional) in &f.params {
            if !optional {
                continue;
            }

            let name = text(*p);
            let body = f.close + 1..end;
            let mut guarded = false;
            let mut first_access: Option<usize> = None;

            for i in body.clone() {
                if toks[i].kind != TokKind::Ident
                    || text(i) != name
                    || matches!(i.checked_sub(1).map(text), Some("." | ":" | "?." | "?:"))
                {
                    continue;
                }

                let prev = i.checked_sub(1).map(text);
                let prev2 = i.checked_sub(2).map(text);
                let next = toks.get(i + 1).map(|t| t.text(src));
                let guard_after = next.is_some_and(|n| {
                    matches!(n, "and" | "or" | "==" | "~=" | "=" | "??" | "!" | "," | ")")
                        || n.starts_with('?')
                });
                let guard_before = matches!(
                    prev,
                    Some(
                        "if" | "elseif"
                            | "not"
                            | "while"
                            | "until"
                            | "return"
                            | "="
                            | ","
                            | "("
                            | "{"
                    )
                ) && !(prev == Some("(")
                    && !matches!(prev2, Some("assert" | "typeof" | "type")))
                    || prev.is_none();
                // `return t` passes the optional on; `return t.x` reads
                // through it. The token after the name decides, so a
                // guard word before it never covers an access.
                let access = matches!(next, Some("." | ":" | "["));

                if !access && (guard_after || guard_before) {
                    guarded = true;

                    break;
                }

                if access && first_access.is_none() {
                    first_access = Some(i);
                }
            }

            if let (false, Some(i)) = (guarded, first_access) {
                lints.push(Lint {
                    name: "optional_access",
                    start: toks[i].start,
                    end: toks[i].end,
                    message: format!(
                        "`{name}` may be nil and nothing checks it; guard it with `if {name} then`, or index with `?.`"
                    ),
                    fix: None,
                });
            }
        }
    }

    // optional_access: a local annotated `T?` indexed with nothing
    // guarding it, the same shape the parameter rule reads.
    let mut optional_locals: Vec<(usize, usize)> = Vec::new();

    for i in 0..toks.len() {
        if text(i) != "local" && text(i) != "const" {
            continue;
        }

        let mut j = i + 1;

        while j + 1 < toks.len() && toks[j].kind == TokKind::Ident {
            let name_at = j;
            j += 1;

            // `local a: T?`: the type runs to the next `,` or `=` of
            // this statement.
            if j < toks.len() && text(j) == ":" {
                j += 1;
                let mut depth = 0i32;
                let mut last = j;

                while j < toks.len() {
                    let t = text(j);

                    if matches!(t, "(" | "[" | "{" | "<") {
                        depth += 1;
                    } else if matches!(t, ")" | "]" | "}" | ">") {
                        depth -= 1;
                    } else if depth == 0 && matches!(t, "," | "=") {
                        break;
                    }

                    last = j;
                    j += 1;
                }

                if last < toks.len() && text(last) == "?" {
                    optional_locals.push((name_at, j));
                }
            }

            if j >= toks.len() || text(j) != "," {
                break;
            }

            j += 1;
        }
    }

    for (name_at, from) in optional_locals {
        let name = text(name_at);
        let mut guarded = false;
        let mut first_access: Option<usize> = None;

        for i in from..toks.len() {
            if toks[i].kind != TokKind::Ident
                || text(i) != name
                || matches!(i.checked_sub(1).map(text), Some("." | ":" | "?." | "?:"))
            {
                continue;
            }

            let prev = i.checked_sub(1).map(text);
            let next = toks.get(i + 1).map(|t| t.text(src));
            let guard_after = next.is_some_and(|n| {
                matches!(n, "and" | "or" | "==" | "~=" | "=" | "??" | "!" | "," | ")")
                    || n.starts_with('?')
            });
            let guard_before = matches!(
                prev,
                Some("if" | "elseif" | "not" | "while" | "until" | "assert")
            );
            let access = matches!(next, Some("." | ":" | "["));

            if !access && (guard_after || guard_before) {
                guarded = true;

                break;
            }

            if access && first_access.is_none() {
                first_access = Some(i);
            }
        }

        if let (false, Some(i)) = (guarded, first_access) {
            lints.push(Lint {
                name: "optional_access",
                start: toks[i].start,
                end: toks[i].end,
                message: format!(
                    "`{name}` may be nil and nothing checks it; guard it with `if {name} then`, or index with `?.`"
                ),
                fix: None,
            });
        }
    }

    // optional_access: `f().x` where `f` returns `T?`.
    let optional_fns: HashSet<&str> = fns
        .iter()
        .filter(|f| f.optional_return && f.path.len() == 1)
        .map(|f| text(f.path[0]))
        .collect();

    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Ident
            || !optional_fns.contains(t.text(src))
            || matches!(
                i.checked_sub(1).map(text),
                Some("." | ":" | "function" | "local")
            )
            || toks.get(i + 1).map(|t| t.text(src)) != Some("(")
        {
            continue;
        }

        if let Some(close) = matching(src, toks, i + 1)
            && matches!(
                toks.get(close + 1).map(|t| t.text(src)),
                Some("." | ":" | "[")
            )
        {
            lints.push(Lint {
                name: "optional_access",
                start: t.start,
                end: toks[close].end,
                message: format!(
                    "`{}` returns a value that may be nil; guard the result before indexing it, or use `?.`",
                    t.text(src)
                ),
                fix: None,
            });
        }
    }

    // A token's text, or the empty string past the end.
    let word = |i: usize| toks.get(i).map(|t| t.text(src)).unwrap_or("");

    // dropped_result: a call statement of a function that answers with
    // a `Result`. The failure passes in silence.
    {
        let answers: Vec<&str> = fns
            .iter()
            .filter(|f| f.returns_result && f.path.len() == 1)
            .map(|f| text(f.path[0]))
            .collect();

        for i in 0..toks.len() {
            if toks[i].kind != TokKind::Ident
                || !answers.contains(&word(i))
                || word(i + 1) != "("
                || matches!(
                    i.checked_sub(1).map(text),
                    Some("." | ":" | "?." | "?:" | "function" | "local")
                )
                || (i > 0 && line_of(i - 1) == line_of(i))
            {
                continue;
            }

            let Some(close) = matching(src, toks, i + 1) else {
                continue;
            };

            // Anything after the call on the same line reads the value.
            if close + 1 < toks.len() && line_of(close + 1) == line_of(close) {
                continue;
            }

            let name = word(i);
            lints.push(Lint {
                name: "dropped_result",
                start: toks[i].start,
                end: toks[close].end,
                message: format!(
                    "`{name}` answers with a `Result` and nothing reads it; a failure passes in silence. Match it, write `if local Ok(v) = {name}(...)`, or take the value with a method"
                ),
                fix: None,
            });
        }
    }

    // static_call: `Wallet:new()` on a function that takes no `self`.
    // The colon passes the table as the first argument, which the
    // static never asked for.
    {
        let mut statics: Vec<(&str, &str)> = Vec::new();

        for (a, b) in &impl_ranges {
            if text(*a) != "impl" {
                continue;
            }

            let target = word(a + 1);

            for f in &fns {
                if f.path.len() != 1 || f.at < *a || f.at > *b || word(f.open + 1) == "self" {
                    continue;
                }

                statics.push((target, text(f.path[0])));
            }
        }

        for i in 0..toks.len() {
            if toks[i].kind != TokKind::Ident
                || word(i + 1) != ":"
                || toks.get(i + 2).map(|t| t.kind) != Some(TokKind::Ident)
                || word(i + 3) != "("
                || matches!(i.checked_sub(1).map(text), Some("." | ":" | "?." | "?:"))
            {
                continue;
            }

            let (owner, member) = (word(i), word(i + 2));

            if !statics.contains(&(owner, member)) {
                continue;
            }

            lints.push(Lint {
                name: "static_call",
                start: toks[i].start,
                end: toks[i + 2].end,
                message: format!(
                    "`{member}` is not a method; call it with `{owner}.{member}(...)`, not `{owner}:{member}(...)`"
                ),
                fix: Some(Fix::new(src, toks[i + 1].start, toks[i + 1].end, ".")),
            });
        }
    }

    // deprecated_global.
    for (i, t) in toks.iter().enumerate() {
        let name = t.text(src);

        if t.kind != TokKind::Ident
            || !matches!(name, "wait" | "spawn" | "delay" | "unpack")
            || declared.contains(name)
            || matches!(
                i.checked_sub(1).map(text),
                Some("." | ":" | "?." | "?:" | "function" | "local")
            )
            || toks.get(i + 1).map(|t| t.text(src)) != Some("(")
        {
            continue;
        }

        let replacement = if name == "unpack" {
            "table.unpack".to_string()
        } else {
            format!("task.{name}")
        };
        lints.push(Lint {
            name: "deprecated_global",
            start: t.start,
            end: t.end,
            message: if name == "unpack" {
                "`unpack` is the legacy global; call `table.unpack` instead".to_string()
            } else {
                format!("`{name}` is the legacy scheduler; call `task.{name}` instead")
            },
            fix: Some(Fix::new(src, t.start, t.end, replacement)),
        });
    }

    // unused_import.
    for stmt in &chunk.block.stmts {
        let Stmt::Import(im) = stmt else { continue };
        let after = toks[im.span.end as usize - 1].end;
        let head = match &im.kind {
            ImportKind::Default(n) | ImportKind::Namespace(n, _) | ImportKind::Both(n, _) => {
                Some(n.start as usize)
            }

            ImportKind::Named(_) | ImportKind::TypeOnly(_) => None,
        };
        let bound: Vec<u32> = match &im.kind {
            ImportKind::Default(n) => vec![n.start],

            ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                std::iter::once(n.start)
                    .chain(specs.iter().map(|s| s.alias.unwrap_or(s.name).start))
                    .collect()
            }

            ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => specs
                .iter()
                .map(|s| s.alias.unwrap_or(s.name).start)
                .collect(),
        };
        let dead: Vec<u32> = bound
            .iter()
            .copied()
            .filter(|i| {
                let name = toks[*i as usize].text(src);

                !toks
                    .iter()
                    .any(|t| t.start >= after && t.kind == TokKind::Ident && t.text(src) == name)
            })
            .collect();
        let cuts = import_cuts(
            src,
            toks,
            im.span.start as usize..im.span.end as usize,
            head,
            &dead,
            &bound,
        );

        for tok_index in &dead {
            let n = toks[*tok_index as usize];
            let name = n.text(src);

            lints.push(Lint {
                name: "unused_import",
                start: n.start,
                end: n.end,
                message: format!("`{name}` is imported and never used"),
                fix: cuts
                    .iter()
                    .find(|(t, _)| t == tok_index)
                    .map(|(_, f)| f.clone()),
            });
        }
    }

    let scan = crate::flux::scan::Scan::new(src, toks, &st)
        .with_privates(import_privates)
        .with_callables(import_callables);
    lints.extend(crate::flux::run(&scan));
    lints.extend(crate::flux::correctness::run(&scan));
    lints.extend(crate::flux::complexity::run(&scan, thresholds));
    lints.extend(crate::flux::roblox::run(&scan));
    lints.sort_by_key(|l| l.start);
    lints
}

/// The bytes a statement owns: the space that indents it and the newline
/// that ends it, so a cut leaves no blank line behind. Code before the
/// statement on the same line keeps its bytes.
fn statement_bytes(src: &str, from: u32, to: u32) -> (u32, u32) {
    let lead = src[..from as usize]
        .bytes()
        .rev()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count() as u32;
    let rest = &src[to as usize..];
    let tail = match rest.find('\n') {
        Some(i) if rest[..i].trim().is_empty() => i as u32 + 1,

        _ => 0,
    };

    (from - lead, to + tail)
}

/// The rewrite that drops each unused name of one `import`, by the token
/// index of the name. Every name gone takes the whole statement. A dead
/// list under a live head drops the list and leaves `import * as M`.
/// Else each dead entry goes with the comma that joins it to its
/// neighbour.
fn import_cuts(
    src: &str,
    toks: &[Tok],
    span: std::ops::Range<usize>,
    head: Option<usize>,
    dead: &[u32],
    bound: &[u32],
) -> Vec<(u32, Fix)> {
    if dead.is_empty() {
        return Vec::new();
    }

    let text = |i: usize| toks.get(i).map(|t| t.text(src)).unwrap_or_default();

    if dead.len() == bound.len() {
        let (a, b) = statement_bytes(src, toks[span.start].start, toks[span.end - 1].end);
        let fix = Fix::new(src, a, b, "");

        return dead.iter().map(|t| (*t, fix.clone())).collect();
    }

    let Some(open) = span.clone().find(|i| text(*i) == "{") else {
        return Vec::new();
    };
    let Some(close) = matching(src, toks, open) else {
        return Vec::new();
    };
    // Each entry of the list, as a token range. An entry may read
    // `type T as U`, and its last token is the name it binds.
    let mut entries: Vec<(usize, usize)> = Vec::new();
    let mut start = open + 1;

    for i in open + 1..close {
        if text(i) == "," {
            if i > start {
                entries.push((start, i));
            }

            start = i + 1;
        }
    }

    if close > start {
        entries.push((start, close));
    }

    let gone = |i: usize| dead.contains(&(i as u32));
    let head_dead = head.is_some_and(gone);
    let mut out = Vec::new();

    // The list is dead whole and the head lives: one cut takes the list
    // from the head name to the `}`.
    if let Some(h) = head.filter(|_| !head_dead)
        && !entries.is_empty()
        && entries.iter().all(|e| gone(e.1 - 1))
    {
        let fix = Fix::new(src, toks[h].end, toks[close].end, "");

        return entries
            .iter()
            .map(|e| ((e.1 - 1) as u32, fix.clone()))
            .collect();
    }

    if let Some(h) = head.filter(|h| gone(*h)) {
        // `import * as M, { a }`: the head runs from the `*` or the
        // default name to the `{`, so the cut leaves `import { a }`.
        let from = toks[span.start + 1].start;

        out.push((h as u32, Fix::new(src, from, toks[open].start, "")));
    }

    for (k, (s, e)) in entries.iter().enumerate() {
        if !gone(e - 1) {
            continue;
        }

        // A list over several lines: an entry on a line of its own goes
        // with that line, so the comment of a neighbour stays.
        let line = own_line(src, toks[*s].start, toks[e - 1].end);
        let (a, b) = match (line, entries.get(k + 1), k) {
            (Some(line), _, _) => line,

            (None, Some(next), _) => (toks[*s].start, toks[next.0].start),

            (None, None, 0) => (toks[*s].start, toks[e - 1].end),

            (None, None, _) => (toks[entries[k - 1].1 - 1].end, toks[e - 1].end),
        };

        out.push(((e - 1) as u32, Fix::new(src, a, b, "")));
    }

    out
}

/// The bytes of the line that holds the text from `from` to `to`, its
/// line break included, when nothing else is on that line: a comma and
/// a comment may follow.
fn own_line(src: &str, from: u32, to: u32) -> Option<(u32, u32)> {
    let start = src[..from as usize].rfind('\n').map_or(0, |i| i + 1);
    let end = src[to as usize..]
        .find('\n')
        .map_or(src.len(), |i| to as usize + i + 1);
    let after = src[to as usize..end].trim_start();
    let after = after.strip_prefix(',').unwrap_or(after).trim();

    (src[start..from as usize].trim().is_empty() && (after.is_empty() || after.starts_with("--")))
        .then_some((start as u32, end as u32))
}

/// The index of the bracket that closes the one at `open`.
fn matching(src: &str, toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, t) in toks.iter().enumerate().skip(open) {
        let step = depth_step(t, src);
        depth += step;

        if step < 0 && depth == 0 {
            return Some(i);
        }
    }

    None
}

/// How a token moves the bracket depth. The text of a string is no
/// bracket. An interpolated string opens with its head and closes with
/// its tail, so a comma in a hole stays inside it.
fn depth_step(t: &Tok, src: &str) -> i32 {
    let text = t.text(src);

    match t.kind {
        TokKind::InterpHead => 1,

        TokKind::InterpTail => -1,

        TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpMid => 0,

        _ if text.ends_with('(') || text.ends_with('[') || text.ends_with('{') => 1,

        _ if matches!(text, ")" | "]" | "}") => -1,

        _ => 0,
    }
}
