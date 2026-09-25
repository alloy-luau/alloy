//! Statement lowering: blocks, assignment, locals, functions, and loops.

use std::collections::{HashMap, HashSet};

use alloy_syntax::ast::{
    After, Assign, Binding, Block, CallArgs, Cond, Declare, Destructure, Expr, FieldBinding,
    Function, FunctionBody, GenericFor, ImportKind, IndexKey, Local, MatchExpr, Param, Pattern,
    Return, Stmt, TableField, TokSpan,
};

use crate::render::Renderer;

use super::expressions::WORD_OPS;
use super::types::{
    apply_bounds, array_element, bound_spots, depth_step, generic_bounds, generic_head,
    generic_names, split_top_level, strip_bounds,
};
use super::*;

/// The mechanism a `destroy x after n` uses. The file's own text picks
/// it; `Unknown` leaves the choice to the runtime.
enum Timed {
    /// Debris takes an Instance, so the removal outlives the script.
    Instance,
    /// A table with this method, `destroy` or `Destroy`, on a timer.
    Method(&'static str),
    /// Nothing in the file names the type.
    Unknown,
}

/// Whether an expression holds a function literal, at any depth.
fn holds_function(e: &Expr) -> bool {
    expr_children(e).iter().any(|c| match c {
        Child::Expr(x) => holds_function(x),

        Child::Block(_) => false,

        Child::Function(_) => true,
    })
}

/// Whether a block, or a nested block of it, returns a value. A function
/// literal inside has its own returns and does not count.
pub(crate) fn returns_value(block: &Block) -> bool {
    block.stmts.iter().any(|stmt| match stmt {
        Stmt::Return(r) => !r.values.is_empty(),

        _ => stmt_children(stmt).iter().any(|child| match child {
            Child::Block(b) => returns_value(b),

            _ => false,
        }),
    })
}

/// The names a pattern binds, in order.
pub(crate) fn pattern_binds(p: &Pattern) -> Vec<TokSpan> {
    match p {
        Pattern::Wildcard(_) | Pattern::Literal(_) | Pattern::Path(_) => Vec::new(),

        Pattern::Bind(n) => vec![*n],

        Pattern::Variant { args, .. } => args.iter().flat_map(pattern_binds).collect(),

        Pattern::Struct { fields, .. } => fields
            .iter()
            .flat_map(|f| match &f.pattern {
                Some(p) => pattern_binds(p),

                None => vec![f.field],
            })
            .collect(),

        Pattern::Array { items, rest, .. } => {
            let mut v: Vec<TokSpan> = items.iter().flat_map(pattern_binds).collect();

            if let Some(r) = rest {
                v.push(*r);
            }

            v
        }

        Pattern::Or(a, _, _) => pattern_binds(a),
    }
}

/// The condition expressions a statement evaluates more than once, or
/// after other statements ran: `while`, `repeat`, and every `elseif`.
/// `g ~= nil and h ~= nil` for a chain's guards, or `None` for none.
fn nil_tests(guards: &[String]) -> Option<String> {
    let tests: Vec<String> = guards.iter().map(|g| format!("{g} ~= nil")).collect();

    (!tests.is_empty()).then(|| tests.join(" and "))
}

pub(crate) fn reevaluated_conditions(s: &Stmt) -> Vec<*const Expr> {
    match s {
        Stmt::While(w) => match &w.cond {
            Cond::Expr(e) => vec![e as *const Expr],

            Cond::Local { .. } => Vec::new(),
        },

        Stmt::Repeat(r) => vec![&r.cond as *const Expr],

        Stmt::If(i) => i
            .branches
            .iter()
            .skip(1)
            .filter_map(|(c, _)| match c {
                Cond::Expr(e) => Some(e as *const Expr),

                Cond::Local { .. } => None,
            })
            .collect(),

        _ => Vec::new(),
    }
}

/// Whether the text calls `name(` as a plain name: the struct call the
/// renderer reports. A member `x.Name(` or a longer word is not it.
pub(crate) fn struct_called(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before = start.checked_sub(1).map(|b| bytes[b]);
        let word_before = before
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b':');
        let after = text[end..].trim_start();

        if !word_before && after.starts_with('(') {
            return true;
        }

        from = end;
    }

    false
}

/// Whether the text has `name {` as a plain name: the fields form of a
/// struct, which the renderer checks against a written constructor.
pub(crate) fn struct_braced(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before = start.checked_sub(1).map(|b| bytes[b]);
        let word_before = before
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b':');
        let after = text[end..].trim_start();

        if !word_before && after.starts_with('{') {
            return true;
        }

        from = end;
    }

    false
}

/// Whether a declared return type already names a Future.
///
/// `async function f(): Future<T>` names the answer the caller gets,
/// the way TypeScript writes `Promise<T>`. `async function f(): T`
/// names what the Future settles with. Both spellings mean one thing,
/// so the first is left alone instead of wrapped a second time.
pub fn names_a_future(declared: &str) -> bool {
    let t = declared.trim();
    // A qualified name names the same type: `alloy.Future<T>`.
    let bare = t.rsplit_once('.').map_or(t, |(_, last)| last);

    // `Future<...>` and nothing else: `FutureQueue` is its own type.
    bare.strip_prefix("Future")
        .is_some_and(|rest| rest.starts_with('<'))
}

/// The `T` of a `Future<T>` a header declares, when it declares one.
///
/// The body of `async function f(): Future<T>` returns `T`; only the
/// caller sees the Future. So the lambda inside carries `T`.
fn settled_type(declared: &str) -> Option<String> {
    if !names_a_future(declared) {
        return None;
    }

    let t = declared.trim();
    let open = t.find('<')?;
    let inner = t[open + 1..].strip_suffix('>')?.trim();

    // `Future<A, B>` settles with two values, and a return type of two
    // values takes parens.
    match split_top_level(inner, ',').len() {
        1 => Some(inner.to_string()),

        _ => Some(format!("({inner})")),
    }
}

/// The types inside the parens of a return pack, `(A, B)`, when the
/// parens hold the whole text. `(A) -> B` is a function type and gives
/// `None`.
pub(crate) fn pack_inner(declared: &str) -> Option<&str> {
    let t = declared.trim();

    (t.starts_with('(') && super::types::group_len(t, '(', ')') == Some(t.len()))
        .then(|| t[1..t.len() - 1].trim())
}

impl<'s> Desugar<'s> {
    pub(crate) fn block(&mut self, block: &Block) {
        if block.span.is_empty() {
            return;
        }

        self.declared.push(Vec::new());
        self.scopes.push(HashSet::new());
        let mut cursor = self.byte_start(block.span);

        for (i, stmt) in block.stmts.iter().enumerate() {
            let start = self.byte_start(stmt.span());
            self.copy(cursor, start);
            let before = self.r.out_len();
            // Luau takes `return`, `break` and `continue` only as the
            // last statement of a block. `do ... end` around one that is
            // not last keeps the meaning and parses. `unreachable_code`
            // already names the statements under it.
            let followed = has_live_stmt(&block.stmts[i + 1..]);
            let fenced = is_early_exit(stmt) && followed;

            // A macro body that ends in `return` returns from the
            // function, so the expansion has to be the last statement
            // of its block. The expansion reads this and reports.
            self.macro_followed = followed;

            if fenced {
                self.generate(start, "do ");
            }

            // A plain `function f()` at the top level is a Luau global,
            // and two files with one name would overwrite each other.
            // The file owns the name, so it takes `local`; one the
            // first line declared fills that slot as written.
            if self.at_top_level()
                && let Stmt::Function(f) = stmt
                && f.path.len() == 1
                && !f.exported
                && !self.is_hoisted_fn(f.path[0])
            {
                match f.attrs.is_empty() {
                    true => self.generate(start, "local "),

                    false => self.ns_force_local = true,
                }
            }

            self.stmt(stmt);

            if fenced {
                self.generate(self.byte_end(stmt.span()), " end");
            }

            self.keep_lines(stmt.span(), before);
            cursor = self.byte_end(stmt.span());
        }

        self.copy(cursor, self.byte_end(block.span));
        self.scopes.pop();
        self.declared.pop();
    }

    /// A statement's output holds every newline of its source span, so
    /// the lines after it stay where they are. A replacement written as
    /// one line over several source lines, `local x = if local ... then
    /// ... else ...`, gets the missing newlines copied from the end of
    /// the span.
    pub(crate) fn keep_lines(&mut self, span: TokSpan, before: u32) {
        let start = self.byte_start(span) as usize;
        // Trailing whitespace of the span copies after the statement.
        let end = start
            + self.src[start..self.byte_end(span) as usize]
                .trim_end()
                .len();
        let want = self.src[start..end].matches('\n').count();
        let have = self.r.newlines_since(before);

        if have >= want {
            return;
        }

        let positions: Vec<u32> = self.src[start..end]
            .match_indices('\n')
            .map(|(i, _)| (start + i) as u32)
            .collect();

        for nl in &positions[positions.len() - (want - have)..] {
            self.r.copy(*nl, nl + 1);
        }
    }

    /// Renders a function body: its temps start fresh behind a barrier, and
    /// its parameters are in scope.
    pub(crate) fn function_block(&mut self, body: &FunctionBody) {
        let saved = self.barrier;
        self.barrier = self.declared.len();
        self.scopes.push(HashSet::new());
        self.declare_params(body);
        self.ret_types
            .push(body.ret_type.map(|t| self.text_of(t).trim().to_string()));
        self.try_targets.push(None);
        self.block(&body.block);
        self.try_targets.pop();
        self.ret_types.pop();
        self.scopes.pop();
        self.barrier = saved;
    }

    /// Reports if the function under render returns a `Result`, which is
    /// what `try` needs.
    pub(crate) fn in_result_function(&self) -> bool {
        let Some(Some(ty)) = self.ret_types.last() else {
            return false;
        };
        let head = ty.split(['<', '?']).next().unwrap_or(ty).trim();

        head == "Result" || self.result_aliases.contains(head)
    }

    /// The arguments of a `return Err(...)` that a `try do` block owns,
    /// where the call sits on one line with its first argument. A head
    /// that wraps keeps the plain `return`: the rewrite replaces that
    /// head, and the line count has to hold.
    fn block_err_return<'a>(&self, r: &'a Return) -> Option<&'a [Expr]> {
        self.try_targets.last()?.as_ref()?;

        let list = self.err_call_args(&r.values)?;
        let head = self.byte_start(r.span) as usize;
        let first = self.byte_start(list[0].span()) as usize;

        (!self.src[head..first].contains('\n')).then_some(list)
    }

    /// Writes `fail(e, nil)` where the source wrote `return Err(e)`.
    /// Everything from the first argument on copies, so the lines and
    /// the map hold.
    fn block_err_return_stmt(&mut self, r: &Return) {
        let Some(list) = self.block_err_return(r) else {
            return;
        };
        let Some(target) = self.try_targets.last().cloned().flatten() else {
            return;
        };
        let (open, close) = if target.exact || !self.options.check {
            ("", "")
        } else {
            ("(", " :: any)")
        };
        let fail = target.fail;
        let anchor = self.byte_start(r.span);
        let first = &list[0];
        let second = list.get(1);
        let after_first = self.byte_end(first.span());
        let end = self.byte_end(r.span);
        self.generate(anchor, &format!("{fail}({open}"));
        self.expr(first);

        match second {
            Some(e) => {
                let start = self.byte_start(e.span());
                self.generate(after_first, close);
                self.copy(after_first, start);
                self.expr(e);
                self.copy(self.byte_end(e.span()), end);
            }

            None => {
                // The trace slot carries a type, since Luau reads the
                // whole parameter list off one call: a bare `nil` here
                // would pin it and report the `try` that passes a trace.
                let trace = if self.options.check {
                    "(nil :: string?)"
                } else {
                    "nil"
                };
                self.generate(after_first, &format!("{close}, {trace}"));
                self.copy(after_first, end);
            }
        }
    }

    /// `declare class Name [extends Base] <members> end` as the
    /// spelling Luau reads today: `declare extern type Name [extends
    /// Base] with <members> end`. The members copy, so a type of the
    /// project inside one still lowers.
    fn declare_class(&mut self, d: &Declare) {
        let span = d.span;
        let tok = |i: usize| TokSpan::new(i, i + 1);
        let first = span.start as usize;
        // `declare class Name`, then `extends Base` when it is written.
        let head_end = match self.text_of(tok(first + 3)) == "extends" {
            true => first + 5,

            false => first + 3,
        };

        self.copy(self.byte_start(span), self.byte_end(tok(first)));
        self.generate(self.byte_start(tok(first + 1)), " extern type");
        self.copy(
            self.byte_end(tok(first + 1)),
            self.byte_end(tok(head_end - 1)),
        );
        self.generate(self.byte_end(tok(head_end - 1)), " with");
        self.copy_declare(
            self.byte_end(tok(head_end - 1)),
            self.byte_end(span),
            &d.patterns,
        );
    }

    /// Copies a declaration with each pattern parameter written as a
    /// temp and its type: Luau's definitions take a name for each
    /// parameter.
    fn copy_declare(&mut self, start: u32, end: u32, patterns: &[Binding]) {
        let mut cursor = start;
        let mut n = 0;

        for b in patterns {
            let Some(d) = &b.destructure else {
                continue;
            };
            let (ps, pe) = (self.byte_start(b.name), self.byte_end(b.name));
            self.copy(cursor, ps);
            let shape = self.check_param_pattern(b.name, b.ty, false, d);
            let temp = self.pattern_temp(&mut n);
            self.generate(ps, &temp);
            self.blank_lines(ps, pe);

            if b.ty.is_none() {
                self.generate(pe, &format!(": {}", shape.as_deref().unwrap_or("any")));
            }

            cursor = pe;
        }

        self.copy(cursor, end);
    }

    pub(crate) fn stmt(&mut self, stmt: &Stmt) {
        // Declarations come first so a later statement sees them.
        match stmt {
            Stmt::Local(l) => {
                for (i, b) in l.names.iter().enumerate() {
                    // `->` gives an `Instance?`, and so does `=>` under
                    // a timeout, so a later `=>` on the name guards it.
                    let child = matches!(
                        l.values.get(i),
                        Some(Expr::Child { wait, .. })
                            if !wait || self.options.wait_timeout.is_some()
                    );

                    if child && b.ty.is_none() && b.destructure.is_none() {
                        self.record_type_text(b.name, Some("Instance?"));
                        self.declare_name(b.name);
                    } else {
                        self.declare_binding(b);
                    }
                }
            }

            Stmt::LocalFunction(f) => self.declare_name(f.name),

            Stmt::Function(f) if f.path.len() == 1 && (f.exported || self.at_top_level()) => {
                self.declare_name(f.path[0]);
            }

            Stmt::Enum(e) => {
                self.declare_name(e.name);
                let variants = e
                    .variants
                    .iter()
                    .map(|v| (self.text_of(v.name).to_string(), v.payload.len()))
                    .collect();
                let name = self.decl_name(e.name);

                // `enum Opt<T>`: the alias needs arguments, the same as a
                // generic struct's.
                if let Some(g) = e.generics {
                    self.struct_generics
                        .insert(name.clone(), self.text_of(g).trim().to_string());
                    self.generic_types.insert(name.clone());
                }

                self.enums.insert(name, variants);
            }

            Stmt::Import(i) => {
                // A std name under its own name is no local: it renders
                // as `__alloy.Name`. Under an alias it is one.
                let spec = self.text_of(i.path).trim_matches(['"', '\'']);
                let std = crate::std_names::module_of_spec(spec).is_some();

                match &i.kind {
                    ImportKind::Default(n) => self.declare_name(*n),

                    ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                        self.declare_name(*n);

                        for sp in specs.iter().filter(|sp| !std || sp.alias.is_some()) {
                            self.declare_name(sp.alias.unwrap_or(sp.name));
                        }
                    }

                    ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                        for sp in specs.iter().filter(|sp| !std || sp.alias.is_some()) {
                            self.declare_name(sp.alias.unwrap_or(sp.name));
                        }
                    }
                }
            }

            Stmt::PatternLocal(p) => {
                let names = pattern_binds(&p.pattern);

                for n in names {
                    self.declare_name(n);
                }
            }

            Stmt::Struct(st) => {
                self.declare_name(st.name);
                self.note_struct(st);
            }

            Stmt::Trait(t) => {
                self.declare_name(t.name);
                self.not_constructible
                    .insert(self.decl_name(t.name), "trait");
            }

            Stmt::Remote(r) => {
                self.declare_name(r.name);
                self.not_constructible
                    .insert(self.decl_name(r.name), "remote");
            }

            Stmt::Attribute(a) => {
                self.declare_name(a.name);
                self.not_constructible
                    .insert(self.text_of(a.name).to_string(), "attribute");
            }

            Stmt::Interface(i) => {
                let name = self.decl_name(i.name);
                self.not_constructible.insert(name.clone(), "interface");
                // An interface with no base has its fields here; one
                // with a base keeps the type function, which sees them.
                if i.extends.is_empty() {
                    self.note_field_types(&name, &i.fields);
                }
            }

            _ => {}
        }

        if !self.stmt_needs_desugar(stmt) {
            self.copy_span(stmt.span());

            return;
        }

        self.expected_payload = self.annotated_async_block(stmt);
        // Render the statement into a side buffer first, so the hoists it
        // asks for can go in front of it on the same line.
        let saved_hoists = std::mem::take(&mut self.hoists);
        let saved_next = self.temp_next;
        self.temp_next = 0;
        // A statement inside a closure starts clean: the operand around
        // the closure has not run yet when the body runs.
        let saved_flags = (self.lazy, self.effects, self.reads, self.in_place);
        (self.lazy, self.effects, self.reads, self.in_place) = (false, false, false, false);

        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        self.stmt_inner(stmt);
        std::mem::swap(&mut self.r, &mut side);

        let hoists = std::mem::replace(&mut self.hoists, saved_hoists);
        self.temp_next = saved_next;
        (self.lazy, self.effects, self.reads, self.in_place) = saved_flags;

        self.write_hoists(hoists, false);
        self.r.append(side);
    }

    /// Finds the plain tables of the file that a colon method can take
    /// a `self` type from: `local X = { }` at the top level, with no
    /// later rebind and no metatable of its own.
    ///
    /// Luau gives `self` no type in `function X:m()` on such a table, so
    /// the check artifact writes the parameter out as `typeof(X)`. A
    /// struct, an enum, and a foreign `impl` carry their own `self`
    /// already, and none of them reaches this scan.
    pub(crate) fn scan_plain_tables(&mut self, block: &Block) {
        let colon_method = |stmt: &Stmt| matches!(stmt.under_default(), Stmt::Function(f) if f.is_method && f.path.len() == 2);

        if !self.options.check || !block.stmts.iter().any(colon_method) {
            return;
        }
        // The hover folds read the same list, so both come from one
        // scan of the source.
        let mut out: HashSet<String> = crate::tables::plain_tables(self.src)
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        for stmt in &block.stmts {
            self.drop_rebound_tables(stmt.under_default(), &mut out);
        }

        self.plain_tables = out;
    }

    /// Takes a name out of the plain tables when the file rebinds it,
    /// gives it a metatable, or writes a metamethod on it. A class of
    /// the `C.__index = C` shape holds instances, not the table, so
    /// `typeof(C)` is the wrong type for its `self`.
    fn drop_rebound_tables(&self, stmt: &Stmt, out: &mut HashSet<String>) {
        if let Stmt::Assign(a) = stmt {
            for target in &a.targets {
                match target {
                    Expr::Name(n) => {
                        out.remove(self.text_of(*n));
                    }

                    Expr::Index {
                        object,
                        key: IndexKey::Field(k),
                        ..
                    } => {
                        if let Expr::Name(n) = object.as_ref()
                            && self.text_of(*k).starts_with("__")
                        {
                            out.remove(self.text_of(*n));
                        }
                    }

                    _ => {}
                }
            }
        }

        // `setmetatable(X, M)` as a statement of its own.
        if let Stmt::Call(Expr::Call { func, args, .. }, _) = stmt
            && let Expr::Name(f) = func.as_ref()
            && self.text_of(*f) == "setmetatable"
            && let CallArgs::Paren(list) = args
            && let Some(Expr::Name(n)) = list.first()
        {
            out.remove(self.text_of(*n));
        }

        for child in stmt_children(stmt) {
            match child {
                Child::Block(b) => {
                    for inner in &b.stmts {
                        self.drop_rebound_tables(inner.under_default(), out);
                    }
                }

                Child::Function(f) => {
                    for inner in &f.block.stmts {
                        self.drop_rebound_tables(inner.under_default(), out);
                    }
                }

                Child::Expr(_) => {}
            }
        }
    }

    /// The `self` type a colon method on a plain table takes, or nothing
    /// when the statement is no such method.
    pub(crate) fn table_self_type(&self, f: &Function) -> Option<String> {
        if !self.options.check || !f.is_method || f.path.len() != 2 {
            return None;
        }
        let owner = self.text_of(f.path[0]);

        // The source may write `self` out already, and then it says
        // more than the scan could.
        if f.body.params.iter().any(|p| self.text_of(p.name) == "self") {
            return None;
        }

        self.plain_tables
            .contains(owner)
            .then(|| format!("typeof({owner})"))
    }

    /// `function X:m(...)` on a plain table, written out as
    /// `function X.m(self: typeof(X), ...)` for the check artifact.
    pub(crate) fn table_method(&mut self, f: &Function, target: &str) {
        let start = self.byte_start(f.span);
        let name = f.path[1];
        // The `:` sits between the two names, so the copy stops at the
        // owner and the emit writes the dot.
        let owner_end = self.byte_end(f.path[0]);
        self.copy(start, owner_end);
        self.generate(owner_end, ".");
        self.self_inject = Some(target.to_string());
        let rest = TokSpan::new(name.start as usize, f.span.end as usize);
        self.function_with_header(rest, &f.body);
        self.self_inject = None;
    }

    /// Reports if a statement needs rewriting. The tree check is exact for
    /// nodes; ambient names and word operators need the source text.
    pub(crate) fn stmt_needs_desugar(&self, s: &Stmt) -> bool {
        // Inside a namespace every declaration renders under a name of
        // its own, and every reference to a sibling takes that name, so
        // the walk has to reach each statement.
        if !self.ns_stack.is_empty() {
            return true;
        }

        // The check artifact casts each `return` of a function written in
        // a for-in header, so the walk has to reach the loop and every
        // statement of that function.
        if self.options.check
            && (self.for_header > 0
                || matches!(s, Stmt::GenericFor(f) if f.exprs.iter().any(holds_function)))
        {
            return true;
        }

        // The check artifact wraps a lone `{expr}` that markup gives as
        // `Text`, so the walk has to reach the statement that holds one.
        if self.options.check {
            let (start, end) = (self.byte_start(s.span()), self.byte_end(s.span()));

            if self
                .options
                .text_holes
                .iter()
                .any(|&(a, b)| start <= a && b <= end)
            {
                return true;
            }
        }

        // An `export type { T }` list below the alias adds the word.
        if let Stmt::TypeAlias(t) = s
            && self.export_listed_types.contains(self.text_of(t.name))
        {
            return true;
        }

        // `global type X = T` reports and renders as `export type X = T`.
        if matches!(s, Stmt::TypeAlias(_)) && self.wrote_global(s.span()) {
            return true;
        }

        // `declare class` takes the spelling Luau reads today, and a
        // pattern parameter takes a name.
        if is_declare_class(self.src, self.toks, s)
            || matches!(s, Stmt::Declare(d) if !d.patterns.is_empty())
        {
            return true;
        }

        if stmt_needs_desugar(s) {
            return true;
        }

        let text = self.text_of(s.span());

        match s {
            Stmt::Local(l) if !l.attrs.is_empty() => return true,

            Stmt::TypeAlias(t) if !t.attributes.is_empty() => return true,

            Stmt::Function(f) if self.params_have_attrs(&f.body) => return true,

            Stmt::Function(f) if self.table_self_type(f).is_some() => return true,

            Stmt::LocalFunction(f) if self.params_have_attrs(&f.body) => return true,

            Stmt::LocalFunction(f) if self.is_hoisted_fn(f.name) => return true,

            _ if self.options.check && self.holds_coalesce_if(s) => return true,

            _ if !self.elem_bounds.is_empty() && self.holds_element_index(s) => return true,

            _ => {}
        }

        // `import "./fx"` is Luau's call sugar for `import("./fx")`.
        AMBIENT.iter().any(|n| text.contains(n))
            || text.contains("import(")
            || text.contains("import<<")
            || text.contains("import \"")
            || text.contains("import '")
            || self.constructs_struct(text)
            || WORD_OPS.iter().any(|w| text.contains(w))
            || self.ext_methods.iter().any(|m| text.contains(m.as_str()))
            || self
                .ext_statics
                .values()
                .flatten()
                .any(|m| text.contains(m.as_str()))
    }

    /// Whether a statement's text holds a construction of a struct: the
    /// name followed by `(` or by `{`.
    ///
    /// The source may spell the struct any way it reaches it: the name a
    /// declaration gives it, the path a namespace member reads under,
    /// the name an import binds, and that name under the local of a star
    /// import. Each spelling reads here, so a construction through an
    /// import or a path reaches the walk and its check.
    fn constructs_struct(&self, text: &str) -> bool {
        let hit = |name: &str| struct_called(text, name) || struct_braced(text, name);

        self.structs
            .iter()
            .any(|name| hit(name) || hit(&self.display_name(name)))
            || self.options.import_struct_fields.iter().any(|(name, _)| {
                hit(name)
                    || self
                        .star_modules
                        .iter()
                        .any(|m| hit(&format!("{m}.{name}")))
            })
    }

    /// Two declarations of one name in one file. Each writes a table and
    /// a type under that name, so the second wins in silence and a use
    /// of the first reads the other shape. One report at the second name
    /// says which declaration holds it.
    pub(crate) fn check_duplicate_decls(&mut self, block: &Block) {
        let mut seen: Vec<(String, &'static str, usize)> = Vec::new();
        let mut hits: Vec<(TokSpan, String)> = Vec::new();

        for stmt in &block.stmts {
            // An import binds its names the way a declaration does; a
            // struct of the same name below it would replace the import.
            let names: Vec<(TokSpan, &'static str)> = match stmt {
                Stmt::Import(i) => super::import_names(i)
                    .into_iter()
                    .map(|n| (n, "imported"))
                    .collect(),

                _ => declared_kind(stmt).into_iter().collect(),
            };

            for (span, kind) in names {
                let name = self.text_of(span).to_string();

                match seen.iter().find(|(n, _, _)| *n == name) {
                    // Two imports of one name are the import check's.
                    Some((_, first, _)) if kind == "imported" && *first == "imported" => {}

                    Some((_, first, line)) => hits.push((
                        span,
                        format!(
                            "`{name}` is already {first} on line {line}; one name holds one declaration"
                        ),
                    )),

                    None => seen.push((name, kind, self.line_of(self.byte_start(span)))),
                }
            }

            // A function or a local shadows the way Luau allows, so the
            // pair reports against an import alone: the emit writes the
            // binding under the import, and the import is lost.
            for span in bound_names(stmt) {
                let name = self.text_of(span).to_string();

                if let Some((_, "imported", line)) = seen.iter().find(|(n, _, _)| *n == name) {
                    hits.push((
                        span,
                        format!(
                            "`{name}` is already imported on line {line}; one name holds one declaration"
                        ),
                    ));
                }
            }
        }

        for (span, message) in hits {
            self.diagnose(span, &message);
        }
    }

    /*
    Every call of a bounded function in the block, against the type of
    each argument: `largest<T: Ord>` asks `Ord` of the value it takes,
    and a struct with no `impl Ord` never has the method.

    The check reads what this file says: a struct it declares, a trait it
    declares, and an argument that is a `new` or a name with an
    annotation. A type from another module, or one only the solver knows,
    is the checker's business.
    */
    pub(crate) fn check_bound_calls(&mut self, block: &Block) {
        if self.fn_bounds.is_empty()
            && self.method_bounds.is_empty()
            && !self
                .enum_decls
                .keys()
                .any(|t| self.unit_variant(t).is_some())
        {
            return;
        }

        // The annotations the top level writes, by name, before the
        // walk: a function above a local still reads it.
        let mut annotated: HashMap<String, String> = HashMap::new();

        for stmt in &block.stmts {
            if let Stmt::Local(l) = stmt.under_default() {
                self.note_annotations(l, &mut annotated);
            }
        }

        let mut hits: Vec<(TokSpan, String)> = Vec::new();
        self.bound_calls_in_block(block, &annotated, &mut hits);

        for (span, message) in hits {
            self.diagnose(span, &message);
        }
    }

    /// `x == Item.Tool("a", 1)`: a payload variant and a `new` struct are
    /// a fresh table, and `==` on a table compares identity, so the test
    /// never holds. `@derive(Eq)` writes the `__eq` that compares the
    /// content. A type this file cannot see the derives of stays quiet.
    pub(crate) fn check_identity_compares(&mut self, block: &Block) {
        let mut hits: Vec<(TokSpan, String)> = Vec::new();
        let stmts: Vec<&Stmt> = block.stmts.iter().collect();
        self.identity_compares_at(&stmts, &mut hits);

        for (span, message) in hits {
            let (start, end) = (
                self.toks[span.start as usize],
                self.toks[span.end as usize - 1],
            );
            self.lints.push(Lint {
                name: "identity_compare",
                start: start.start,
                end: end.end,
                message,
                fix: None,
            });
        }
    }

    /// The top-level statements, and the members of each namespace under
    /// its scope, so a member reads by its own name: `Inner.B(1)`.
    fn identity_compares_at(&mut self, stmts: &[&Stmt], hits: &mut Vec<(TokSpan, String)>) {
        for stmt in stmts {
            if let Stmt::Namespace(ns) = stmt.under_default() {
                let key = namespaces::key_of(
                    self.ns_stack.last().map(|f| f.key.as_str()),
                    self.text_of(ns.name),
                );
                self.ns_stack.push(namespaces::NsFrame {
                    key,
                    scope: self.scope_depth(),
                });
                let members: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();
                self.identity_compares_at(&members, hits);
                self.ns_stack.pop();
            }

            self.identity_compares_in(stmt_children(stmt), hits);
        }
    }

    fn identity_compares_in(&self, children: Vec<Child<'_>>, hits: &mut Vec<(TokSpan, String)>) {
        for child in children {
            let inner = match child {
                Child::Block(b) => b.stmts.iter().flat_map(stmt_children).collect(),

                Child::Function(f) => f.block.stmts.iter().flat_map(stmt_children).collect(),

                Child::Expr(e) => {
                    if let Expr::Binary { op, lhs, rhs, span } = e
                        && let op = self.text_of(*op)
                        && matches!(op, "==" | "~=")
                        && let Some((ty, part)) = self
                            .built_without_eq(lhs)
                            .or_else(|| self.built_without_eq(rhs))
                    {
                        let never = match op {
                            "==" => "equals no other",

                            _ => "differs from every other",
                        };
                        hits.push((
                            *span,
                            format!(
                                "this `{op}` compares identity, and a value built here {never}; `@derive(Eq)` on `{ty}` compares the {part}"
                            ),
                        ));
                    }

                    // `$assert_eq` compares with `==` at run time.
                    if let Expr::Macro { name, args, span } = e
                        && self.text_of(*name) == "assert_eq"
                        && let Some((ty, part)) = args.iter().find_map(|a| self.built_without_eq(a))
                    {
                        hits.push((
                            *span,
                            format!(
                                "`$assert_eq` compares with `==`, which compares identity, and a value built here equals no other; `@derive(Eq)` on `{ty}` compares the {part}"
                            ),
                        ));
                    }

                    super::expr_children(e)
                }
            };

            self.identity_compares_in(inner, hits);
        }
    }

    /// The type a payload variant call or a `new` builds, when that type
    /// derives no `Eq`, with what a derived `Eq` would compare.
    fn built_without_eq(&self, e: &Expr) -> Option<(String, &'static str)> {
        let (ty, part) = match e {
            Expr::Paren { inner, .. } => return self.built_without_eq(inner),

            Expr::Call {
                func, method: None, ..
            } => {
                let (ty, v) = self.enum_of_path(&self.dotted_name(func)?)?;
                let (_, arity) = self.enums.get(&ty)?.iter().find(|(n, _)| *n == v)?;

                if *arity == 0 {
                    return None;
                }

                (ty, "payload")
            }

            // `new G.P { }` builds the struct this file declares as `G_P`.
            Expr::New { name, .. } => {
                let ty = self.dotted_name(name)?;

                (self.own_ns_type(&ty).unwrap_or(ty), "fields")
            }

            _ => return None,
        };

        (self.derives_eq(&ty) == Some(false)).then(|| (self.display_name(&ty), part))
    }

    /// Whether `==` on a struct or an enum compares its content: a derived
    /// `Eq` or `PartialEq`, or an `eq` an `impl` writes. `None` for a
    /// type this file neither declares nor imports from the project.
    fn derives_eq(&self, ty: &str) -> Option<bool> {
        if self.structs.contains(ty) || self.enum_payloads.contains_key(ty) {
            let written = |m: &str| {
                self.impl_methods.get(ty).is_some_and(|ms| ms.contains(m))
                    || self
                        .options
                        .foreign_impls
                        .iter()
                        .any(|x| x.head().0 == ty && x.name == m)
            };

            return Some(self.equatable.contains(ty) || written("eq") || written("__eq"));
        }

        let shape = self.imported_type(ty)?;
        let written = ["eq", "__eq"].iter().any(|m| {
            self.options
                .import_callables
                .iter()
                .any(|(k, _)| *k == format!("{ty}:{m}") || *k == format!("{ty}.{m}"))
        });

        Some(written || shape.derives.iter().any(|d| d == "Eq" || d == "PartialEq"))
    }

    /// The types a `local` binds, by name. An annotation names the type.
    /// Without one, `local x = new S { }` names the struct as exactly,
    /// and that is the form most calls hand a bounded parameter.
    fn note_annotations(&self, l: &Local, annotated: &mut HashMap<String, String>) {
        for (i, b) in l.names.iter().enumerate() {
            let ty = match b.ty {
                Some(ty) => Some(self.annotation_text(ty)),

                None => l
                    .values
                    .get(i)
                    .and_then(|v| self.argument_struct(v, annotated)),
            };

            if let Some(ty) = ty {
                annotated.insert(self.text_of(b.name).to_string(), ty);
            }
        }
    }

    /// The type an annotation span names, without its `:`.
    fn annotation_text(&self, ty: TokSpan) -> String {
        self.text_of(ty)
            .trim()
            .trim_start_matches(':')
            .trim()
            .to_string()
    }

    /// Whether this file declares `name` as an enum with no payload: a
    /// string union at runtime.
    /// The first variant of the enum with no payload, a string at
    /// runtime.
    fn unit_variant(&self, name: &str) -> Option<&str> {
        self.enum_decls
            .get(name)?
            .iter()
            .find(|(_, n)| *n == 0)
            .map(|(v, _)| v.as_str())
    }

    /// Whether an `impl` of the enum writes the method: an impl in this
    /// file, or in the module that declares an imported enum.
    fn enum_has_method(&self, target: &str, method: &str) -> bool {
        let key = format!("{target}:{method}");

        self.impl_methods
            .get(target)
            .is_some_and(|ms| ms.contains(method))
            || self.options.import_callables.iter().any(|(k, _)| *k == key)
    }

    fn is_unit_enum(&self, name: &str) -> bool {
        self.enum_decls
            .get(name)
            .is_some_and(|vs| vs.iter().all(|(_, n)| *n == 0))
    }

    fn bound_calls_in_block(
        &self,
        block: &Block,
        annotated: &HashMap<String, String>,
        hits: &mut Vec<(TokSpan, String)>,
    ) {
        // A local of the block is known to the statements below it.
        let mut annotated = annotated.clone();

        for stmt in &block.stmts {
            if let Stmt::Local(l) = stmt.under_default() {
                self.note_annotations(l, &mut annotated);
            }

            // An arm knows the names its patterns bind.
            if let Stmt::Match(m) = stmt {
                let scrutinees = m.scrutinees.iter().map(Child::Expr).collect();
                self.bound_calls_in(scrutinees, &annotated, hits);

                for a in &m.arms {
                    let inner = self.arm_annotations(&a.patterns, &annotated);
                    let guard = a.guard.iter().map(Child::Expr);
                    let body = guard.chain([Child::Block(&a.block)]).collect();
                    self.bound_calls_in(body, &inner, hits);
                }

                let default = m.default.iter().map(Child::Block).collect();
                self.bound_calls_in(default, &annotated, hits);

                continue;
            }

            self.bound_calls_in(stmt_children(stmt), &annotated, hits);
        }
    }

    /// The names the patterns of one arm bind, over the names outside:
    /// a name in a payload slot takes the enum the slot declares, and
    /// `case Missing(item, n)` types `item` as `Item`. Any other name a
    /// pattern binds hides the outer one.
    fn arm_annotations(
        &self,
        patterns: &[Pattern],
        annotated: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut inner = annotated.clone();
        let mut stack: Vec<(&Pattern, Option<(TokSpan, usize)>)> =
            patterns.iter().map(|p| (p, None)).collect();

        while let Some((p, slot)) = stack.pop() {
            match p {
                Pattern::Bind(n) => {
                    let name = self.text_of(*n).to_string();

                    match slot.and_then(|(v, i)| self.slot_enum(v, None, i)) {
                        Some(e) => inner.insert(name, e),

                        None => inner.remove(&name),
                    };
                }

                Pattern::Variant { name, args, .. } => {
                    for (i, a) in args.iter().enumerate() {
                        stack.push((a, Some((*name, i))));
                    }
                }

                Pattern::Or(a, b, _) => {
                    stack.push((a, slot));
                    stack.push((b, slot));
                }

                _ => {}
            }
        }

        inner
    }

    fn bound_calls_in(
        &self,
        children: Vec<Child<'_>>,
        annotated: &HashMap<String, String>,
        hits: &mut Vec<(TokSpan, String)>,
    ) {
        for child in children {
            match child {
                Child::Block(b) => self.bound_calls_in_block(b, annotated, hits),

                // A parameter's annotation is known to the body.
                Child::Function(f) => {
                    let mut inner = annotated.clone();

                    for p in &f.params {
                        if let Some(ty) = p.ty
                            && !p.is_vararg
                            && p.destructure.is_none()
                        {
                            inner
                                .insert(self.text_of(p.name).to_string(), self.annotation_text(ty));
                        }
                    }

                    self.bound_calls_in_block(&f.block, &inner, hits);
                }

                Child::Expr(Expr::Match(m)) => {
                    let scrutinees = m.scrutinees.iter().map(Child::Expr).collect();
                    self.bound_calls_in(scrutinees, annotated, hits);

                    for a in &m.arms {
                        let inner = self.arm_annotations(&a.patterns, annotated);
                        let guard = a.guard.iter().map(Child::Expr);
                        let body = guard.chain([Child::Expr(&a.value)]).collect();
                        self.bound_calls_in(body, &inner, hits);
                    }

                    let default = m.default.iter().map(|d| Child::Expr(d)).collect();
                    self.bound_calls_in(default, annotated, hits);
                }

                Child::Expr(e) => {
                    self.bound_call(e, annotated, hits);
                    self.bound_calls_in(super::expr_children(e), annotated, hits);
                }
            }
        }
    }

    /// One call: an argument whose struct this file declares and whose
    /// impls miss the trait the parameter asks for, or a `:` call of a
    /// unit enum's method.
    fn bound_call(
        &self,
        e: &Expr,
        annotated: &HashMap<String, String>,
        hits: &mut Vec<(TokSpan, String)>,
    ) {
        let Expr::Call {
            func,
            method,
            args: CallArgs::Paren(list),
            ..
        } = e
        else {
            return;
        };

        // A unit enum is a string at runtime, and a string carries no
        // metatable of its own, so `s:m()` finds no method. The impl
        // writes `Status.m`, and the static form reaches it.
        if let (Some(m), Expr::Name(n)) = (method, &**func)
            && let Some(ty) = annotated.get(self.text_of(*n))
            // `Opt<number>` names the generic enum `Opt`.
            && let target = ty.trim_end_matches('?').split('<').next().unwrap_or_default().trim()
            && let Some(unit) = self.unit_variant(target)
            && self.enum_has_method(target, self.text_of(*m))
        {
            let (m, recv) = (self.text_of(*m), self.text_of(*n));
            let what = match self.is_unit_enum(target) {
                true => format!("`{target}` is a unit enum, a string at runtime"),

                false => format!("`{target}.{unit}` is a unit variant, a string at runtime"),
            };
            hits.push((e.span(), format!("{what}; call `{target}.{m}({recv})`")));

            return;
        }

        let Some((name, asks, skip)) = self.call_bounds(func, *method, annotated) else {
            return;
        };

        for (i, arg) in list.iter().enumerate() {
            let Some(Some(bound)) = asks.get(i + skip) else {
                continue;
            };
            let Some(target) = self.argument_struct(arg, annotated) else {
                continue;
            };

            if !self.structs.contains(&target) {
                continue;
            }

            for part in bound.split('&') {
                let want = part.trim();

                // A std shape resolves to the runtime's type, which a
                // struct meets by its shape; a trait this file declares
                // is the one an `impl` has to name.
                if want.is_empty() || !self.traits.contains_key(want) {
                    continue;
                }

                let met = self
                    .impl_traits
                    .get(&target)
                    .is_some_and(|ts| ts.iter().any(|t| t == want));

                if !met {
                    let want = self.display_name(want);
                    hits.push((
                        arg.span(),
                        format!("`{target}` does not implement `{want}`; `{name}` asks for it"),
                    ));

                    // One argument reports one bound. `T: A & B` with
                    // neither impl says the same thing twice otherwise.
                    break;
                }
            }
        }
    }

    /// What the callee of a call asks of its arguments: the name to
    /// report, the bound at each parameter's place, and the places the
    /// arguments start past. A `:` call fills `self` with the receiver,
    /// so its first argument takes the second place; `T.m(x, ...)`
    /// writes `self` out and starts at the first.
    fn call_bounds(
        &self,
        func: &Expr,
        method: Option<TokSpan>,
        annotated: &HashMap<String, String>,
    ) -> Option<(String, &Vec<Option<String>>, usize)> {
        // `x:m(...)`: the receiver names the impl the method sits in.
        if let Some(m) = method {
            let name = self.text_of(m).to_string();
            let target = self.argument_struct(func, annotated)?;
            let asks = self.method_bounds.get(&(target, name.clone()))?;

            return Some((name, asks, 1));
        }

        match func {
            Expr::Name(n) => {
                let name = self.text_of(*n).to_string();
                let asks = self.fn_bounds.get(&name)?;

                Some((name, asks, 0))
            }

            // `T.m(self, ...)`, the method by the name of its impl.
            Expr::Index {
                object,
                key: IndexKey::Field(k),
                ..
            } => {
                let Expr::Name(t) = &**object else {
                    return None;
                };
                let target = self.text_of(*t).to_string();
                let name = self.text_of(*k).to_string();
                let asks = self.method_bounds.get(&(target, name.clone()))?;

                Some((name, asks, 0))
            }

            _ => None,
        }
    }

    /// The struct an argument carries: `new S { }` names it, and a name
    /// takes the annotation its binding wrote, `S`, `{ S }`, or `S[]`.
    fn argument_struct(&self, arg: &Expr, annotated: &HashMap<String, String>) -> Option<String> {
        match arg {
            Expr::Paren { inner, .. } => self.argument_struct(inner, annotated),

            Expr::New { name, .. } => match &**name {
                Expr::Name(n) => Some(self.text_of(*n).to_string()),

                _ => None,
            },

            Expr::Name(n) => {
                let ty = annotated.get(self.text_of(*n))?;
                let head = array_element(ty).unwrap_or(ty);
                let head = head.trim().trim_end_matches('?');

                (!head.is_empty()).then(|| head.to_string())
            }

            _ => None,
        }
    }

    /// A bound `<T: Shape>` names a trait, and the emit erases the
    /// bound, so nothing else reports a name that is nowhere.
    pub(crate) fn check_bounds(&mut self, generics: Option<TokSpan>) {
        let Some(g) = generics else {
            return;
        };

        // A std trait a bound names, `<T: Serialize>`, is a std name the
        // file writes.
        for i in g.start + 1..g.end {
            let text = self.toks[i as usize].text(self.src);

            if matches!(self.toks[i as usize - 1].text(self.src), ":" | "&")
                && crate::std_names::is_std_name(text)
                && !self.traits.contains_key(text)
            {
                self.check_std_name(TokSpan::new(i as usize, i as usize + 1), text);
            }
        }

        let mut hits: Vec<(TokSpan, String)> = Vec::new();

        for (_, bound) in generic_bounds(self.text_of(g)) {
            for part in bound.split('&') {
                let head = part
                    .trim()
                    .split('<')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                // A negation bound: `<T: ~nil>` takes any `T` but nil.
                if let Some(negated) = part.trim().strip_prefix('~') {
                    let negated = negated.trim();
                    let message = if negated.starts_with('~') {
                        Some(format!(
                            "`~{negated}` negates twice; write `{}`",
                            negated.trim_start_matches('~')
                        ))
                    } else {
                        self.unnegatable(negated).map(|kind| {
                            format!(
                                "`~{negated}` negates a {kind} type, which Luau cannot negate; negate a primitive, a singleton, a class, or a union of them"
                            )
                        })
                    };

                    if let Some(message) = message {
                        hits.push((g, message));
                    }

                    continue;
                }

                if head.is_empty()
                    || super::attributes::BUILTIN_BOUNDS.contains(&head.as_str())
                    || self.knows_type(&head)
                {
                    continue;
                }

                let at = self.token_named(g, &head).unwrap_or(g);
                hits.push((
                    at,
                    format!("`{head}` names no trait or interface; a bound needs one"),
                ));
            }
        }

        for (span, message) in hits {
            self.diagnose(span, &message);
        }
    }

    /*
    A bound on the type parameter of a declaration: `struct Shelf<T: Named>`.

    A Luau type alias takes no bound, and a field `items: T[]` is
    invariant, so `T & Named` there accepts no argument. The emit dropped
    the bound, and nothing checked it. `what` is `a struct`, `an enum`, or
    `an interface`.
    */
    pub(crate) fn reject_type_bounds(&mut self, generics: Option<TokSpan>, what: &str) {
        let Some(g) = generics else {
            return;
        };

        for (name, bound) in generic_bounds(self.text_of(g)) {
            let at = self.token_named(g, &name).unwrap_or(g);
            self.diagnose(
                at,
                &format!(
                    "a type parameter of {what} takes no bound; write `{name}` for `{name}: {bound}`, and put the bound on a function that needs it"
                ),
            );
        }
    }

    /// The token inside `span` whose text is `name`.
    fn token_named(&self, span: TokSpan, name: &str) -> Option<TokSpan> {
        (span.start..span.end)
            .map(|i| TokSpan::new(i as usize, i as usize + 1))
            .find(|one| self.text_of(*one) == name)
    }

    /// The payload of `local f: Future<T> = async do ... end`. The
    /// annotation names the answer, not what the block returns, so the
    /// block's closure carries `T`. A binding with no annotation keeps
    /// the inference.
    fn annotated_async_block(&self, stmt: &Stmt) -> Option<String> {
        let Stmt::Local(l) = stmt else {
            return None;
        };

        if l.names.len() != 1 || l.values.len() != 1 {
            return None;
        }

        if !matches!(l.values[0], Expr::AsyncBlock { .. }) {
            return None;
        }

        settled_type(self.text_of(l.names[0].ty?))
    }

    pub(crate) fn stmt_inner(&mut self, stmt: &Stmt) {
        // `try do await f end` gives one value, and `__try_ret` reads the
        // type of the closure's `return`. A bare `await` there returns
        // the open pack of every value, which a type function cannot
        // read; the parens keep the first value alone.
        if let Stmt::Return(r) = stmt
            && self.options.check
            && matches!(self.try_targets.last(), Some(Some(_)))
            && let [e @ Expr::Await { .. }] = r.values.as_slice()
        {
            self.one_value.insert(std::ptr::from_ref(e) as usize);
        }

        match stmt {
            Stmt::Struct(st) => self.struct_decl(st),

            Stmt::Trait(t) => self.trait_decl(t),

            Stmt::Interface(i) => self.interface_decl(i),

            Stmt::Remote(r) => self.remote_decl(r),

            Stmt::Attribute(a) => self.attribute_decl(a),

            Stmt::Macro(_) => {
                // The declaration exists at compile time only; its lines
                // stay blank.
                let span = stmt.span();
                self.blank_lines(self.byte_start(span), self.byte_end(span));
            }

            Stmt::Function(f) if !f.attrs.is_empty() => self.attributed_function(
                stmt.span(),
                &f.attrs,
                &f.body,
                f.path.first().copied(),
                f.exported,
                false,
            ),

            Stmt::LocalFunction(f) if !f.attrs.is_empty() => self.attributed_function(
                stmt.span(),
                &f.attrs,
                &f.body,
                Some(f.name),
                f.exported,
                true,
            ),

            Stmt::Attributed {
                attrs, stmt: inner, ..
            } => self.attributed_plain_stmt(attrs, inner),

            Stmt::Import(i) => self.import_stmt(i),

            Stmt::ExportList(e) => self.export_list(e),

            Stmt::ExportDefault { value, span } => self.export_default(*span, value),

            Stmt::Enum(e) => self.enum_decl(e),

            Stmt::Impl(i) => self.impl_decl(i),

            Stmt::Namespace(ns) => self.namespace_decl(ns),

            Stmt::Match(m) => self.match_stmt(m),

            Stmt::PatternLocal(p) => self.pattern_local(p),

            Stmt::If(i) if if_has_local(i) => self.if_with_locals(stmt.span(), i),

            Stmt::If(i) if self.options.check && self.coalesce_if(i).is_some() => {
                let (name, value) = self.coalesce_if(i).expect("matched above");
                self.coalesce_if_stmt(stmt.span(), i, name, value);
            }

            Stmt::While(w) if matches!(w.cond, Cond::Local { .. }) => {
                self.while_with_local(stmt.span(), w);
            }

            Stmt::Local(l) if l.exported => self.exported_local(stmt.span(), l),

            Stmt::Function(f) if f.exported => {
                let anchor = self.byte_start(stmt.span());
                let name = self.text_of(f.path[0]).to_string();
                self.exports.push((name.clone(), name.clone()));

                // `export function f` becomes `local function f`; one
                // the first line declared fills that slot as written.
                if !self.is_hoisted_fn(f.path[0]) {
                    self.generate(anchor, "local ");
                }

                let rest = TokSpan::new(stmt.span().start as usize + 1, stmt.span().end as usize);
                self.function_rest(rest, &f.body);
            }

            // `export local function f` drops the `export`. One the
            // first line declared drops the `local` too: a second
            // slot would leave the first one nil.
            Stmt::LocalFunction(f)
                if f.attrs.is_empty() && (f.exported || self.is_hoisted_fn(f.name)) =>
            {
                let name = self.text_of(f.name).to_string();
                let hoisted = self.is_hoisted_fn(f.name);

                if f.exported {
                    self.exports.push((name.clone(), name));
                }

                let skip = usize::from(f.exported) + usize::from(hoisted);
                let rest =
                    TokSpan::new(stmt.span().start as usize + skip, stmt.span().end as usize);
                self.function_rest(rest, &f.body);
            }

            Stmt::Assign(a) if self.assign_needs_rewrite(a) => self.assign(a),

            Stmt::Call(e, span) if chain_has_alloy(e) || self.chain_has_ext(e) => {
                let anchor = self.byte_start(*span);
                let text = self.chain_stmt(e);
                self.generate(anchor, &text);
            }

            // `new X(...) { fields }` alone: the instance lands in a temp and
            // each field assigns through it, as the local form does.
            Stmt::Call(
                Expr::New {
                    name,
                    type_args,
                    args,
                    init: Some(init),
                    span: whole,
                },
                span,
            ) if !self.fields_form(name, args.as_ref(), Some(init)) => {
                let anchor = self.byte_start(*span);
                // A fresh local each time: a reused temp would carry
                // the type of an earlier statement into the checker.
                self.new_stmt_next += 1;
                let binding = format!("_n{}", self.new_stmt_next);
                self.new_init(
                    &format!("local {binding}"),
                    &binding,
                    name,
                    *type_args,
                    args.as_ref(),
                    init,
                    *whole,
                    anchor,
                );
            }

            // `$matches(e, Ok(_))` alone is a value and no call. Its
            // expansion, `( if ... )`, is no Luau statement, so the
            // report is the one `x + 1` alone gets.
            Stmt::Call(Expr::Macro { name, span, .. }, _)
                if matches!(self.text_of(*name), "matches" | "nameof" | "stringify")
                    && self.macro_of(self.text_of(*name)).is_none() =>
            {
                self.diagnose(*span, "this expression is not a statement");
                self.blank_lines(self.byte_start(*span), self.byte_end(*span));
            }

            // A macro call that stands alone is a statement, so the
            // body's statements stay statements and a `return` in it
            // returns from the function around the call.
            Stmt::Call(e @ Expr::Macro { .. }, _) => {
                let saved = std::mem::replace(&mut self.macro_stmt, true);
                self.expr(e);
                self.macro_stmt = saved;
            }

            // `new X(...)`, `await f()`, `async do ... end`: a call once
            // rendered. `try f()` drops its value into a throwaway local,
            // since the unwrapped value is a name and a name is no
            // statement.
            Stmt::Call(
                e @ (Expr::New { .. }
                | Expr::Await { .. }
                | Expr::Try { .. }
                | Expr::AsyncBlock { .. }),
                span,
            ) => {
                let anchor = self.byte_start(*span);

                if matches!(e, Expr::Try { .. }) {
                    self.generate(anchor, "local _ = ");
                }

                self.expr(e);
            }

            // `@cfg` on a local guards its value; any other attribute
            // has nothing to attach to: a diagnostic, and the text goes
            // so the output stays Luau.
            Stmt::Local(l) if !l.attrs.is_empty() => {
                let mut cfg = None;

                for a in &l.attrs {
                    let name = a.name.map(|n| self.text_of(n)).unwrap_or("").to_string();

                    if name == "cfg" {
                        match self.cfg_condition(&a.args) {
                            Ok(cond) => cfg = Some(cond),

                            Err(message) => self.diagnose(a.span, &message),
                        }
                    }

                    // The target check runs in `check_attrs`, over every
                    // declaration at once.
                    self.blank_lines(self.byte_start(a.span), self.byte_end(a.span));
                }

                // The copy starts where the last attribute ends, so the
                // newline between it and the keyword survives.
                let after_attrs = l
                    .attrs
                    .iter()
                    .map(|a| self.byte_end(a.span))
                    .max()
                    .unwrap_or(0);

                // The value is read only when the condition holds; the
                // binding keeps the value's type, so the code that uses
                // it reads as before. `typeof` sees the value, it does
                // not run it.
                if let Some(cond) = cfg {
                    let plain = l.names.len() == 1
                        && l.values.len() == 1
                        && !self.text_of(l.names[0].name).starts_with(['{', '[']);

                    if plain {
                        let value = &l.values[0];
                        let vs = self.byte_start(value.span());
                        let ve = self.byte_end(value.span());
                        // The copy in `typeof` sits on one line: each
                        // line of the value loses its indent.
                        let shape = self
                            .render_to_string(value)
                            .lines()
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .join(" ");
                        self.copy(after_attrs, vs);
                        self.generate(vs, &format!("(if {cond} then "));
                        self.expr(value);
                        self.generate(ve, &format!(" else nil) :: typeof({shape})"));
                        self.copy(ve, self.byte_end(l.span));

                        return;
                    }

                    self.diagnose(l.span, "`@cfg` goes on a local with one name and one value");
                }

                if local_needs_rewrite(l) {
                    self.local_stmt(l);
                } else {
                    self.copy(after_attrs, self.byte_end(l.span));
                }
            }

            Stmt::Function(f) if self.params_have_attrs(&f.body) => {
                self.check_param_attrs(&f.body);
                self.copy_span(stmt.span());
            }

            Stmt::LocalFunction(f) if self.params_have_attrs(&f.body) => {
                self.check_param_attrs(&f.body);
                self.copy_span(stmt.span());
            }

            // A match whose arms run statements stands after `local x =`,
            // `x =`, or `return`: it becomes a statement match whose arms
            // end by writing the value there, with no closure.
            Stmt::Local(l)
                if l.names.len() == 1
                    && l.values.len() == 1
                    && l.names[0].destructure.is_none()
                    && block_arm_match(&l.values[0]).is_some() =>
            {
                let m = block_arm_match(&l.values[0]).expect("matched above");
                let name = &l.names[0];
                let head_end = name
                    .ty
                    .map_or(self.byte_end(name.name), |t| self.byte_end(t));
                let m_start = self.byte_start(m.span);

                // Luau's `const` takes its value on its own line, and the
                // arms set it below; the emit writes `local`, and the
                // compiler's own check still refuses a later write.
                if l.is_const {
                    let kw = self.byte_start(l.keyword);
                    self.copy(self.byte_start(l.span), kw);
                    self.generate(kw, "local");
                    self.copy(self.byte_end(l.keyword), head_end);
                } else {
                    self.copy(self.byte_start(l.span), head_end);
                }

                // A bare `local x` is nil until an arm sets it, so the
                // checker reads it `T?` at the name. `never` adds nothing
                // to the arms' union, so the hover and the hint read `T`.
                if self.options.check && name.ty.is_none() {
                    self.generate(head_end, " = nil :: never");
                }

                self.blank_lines(head_end, m_start);
                let sink = format!("{} = ", self.text_of(name.name));
                self.match_hoisted(m, &sink, " ");
                self.declare_binding(name);
            }

            Stmt::Assign(a)
                if a.targets.len() == 1
                    && a.values.len() == 1
                    && matches!(a.targets[0], Expr::Name(_))
                    && self.text_of(a.op) == "="
                    && block_arm_match(&a.values[0]).is_some() =>
            {
                let m = block_arm_match(&a.values[0]).expect("matched above");
                let sink = format!("{} = ", self.text_of(a.targets[0].span()));
                let m_start = self.byte_start(m.span);
                self.blank_lines(self.byte_start(a.span), m_start);
                self.match_hoisted(m, &sink, "");
            }

            Stmt::Return(r) if r.values.len() == 1 && block_arm_match(&r.values[0]).is_some() => {
                let m = block_arm_match(&r.values[0]).expect("matched above");
                // The tail of a value block writes into that block's sink.
                let sink = match r.value_only {
                    true => self
                        .value_sink
                        .clone()
                        .unwrap_or_else(|| "return ".to_string()),

                    false => "return ".to_string(),
                };
                let m_start = self.byte_start(m.span);
                self.blank_lines(self.byte_start(r.span), m_start);
                self.match_hoisted(m, &sink, "");
            }

            Stmt::Local(l) if local_needs_rewrite(l) => self.local_stmt(l),

            // `return Err(e)` inside a `try do` block: the block fails
            // with `e`, which is what the block's `fail` says. A
            // `return` of it would decide the closure's value type
            // instead, and Luau then reports every other `return`.
            Stmt::Return(r) if self.block_err_return(r).is_some() => {
                self.block_err_return_stmt(r);
            }

            // `try do ... x end`: the block's value is its trailing
            // expression. The source writes no `return`, so the emit
            // writes it in front of the expression.
            Stmt::Return(r) if r.value_only => {
                let anchor = self.byte_start(r.span);
                let lead = self
                    .value_sink
                    .clone()
                    .unwrap_or_else(|| "return ".to_string());
                self.generate(anchor, &lead);
                self.stitch(r.span, &stmt_children(stmt), |d, child| match child {
                    Child::Expr(e) => d.expr(e),

                    Child::Block(b) => d.block(b),

                    Child::Function(b) => d.function_block(b),
                });
            }

            // `return HashMap.new()` under a declared `HashMap<K, V>`
            // return type: the same rule as the annotated local below.
            Stmt::Return(r) if self.returned_constructor(&r.values).is_some() => {
                self.expected_generic = self.returned_constructor(&r.values);
                let span = stmt.span();
                let children = stmt_children(stmt);
                self.stitch(span, &children, |d, child| match child {
                    Child::Expr(e) => d.expr(e),

                    Child::Block(b) => d.block(b),

                    Child::Function(b) => d.function_block(b),
                });
                self.expected_generic = None;
            }

            // `local m: HashMap<K, V> = HashMap.new()`: the call takes the
            // annotation's arguments, since the solver infers none.
            Stmt::Local(l) if self.annotated_constructor(l).is_some() => {
                self.expected_generic = self.annotated_constructor(l);
                let span = stmt.span();
                let children = stmt_children(stmt);
                self.stitch(span, &children, |d, child| match child {
                    Child::Expr(e) => d.expr(e),

                    Child::Block(b) => d.block(b),

                    Child::Function(b) => d.function_block(b),
                });
                self.expected_generic = None;
            }

            Stmt::Delete { expr, span } => {
                let anchor = self.byte_start(*span);
                let std = self.std();
                // The target renders in place, so the editor maps a
                // position inside it.
                self.generate(anchor, &format!("{std}.delete("));
                self.expr(expr);
                let close = self.byte_end(*span);
                let mut tail = ")".to_string();

                // A field or an index empties after its value is gone, so
                // the table holds nothing destroyed. The check artifact
                // writes through `any`: a `read` field takes no assignment.
                if let Expr::Index {
                    object,
                    key,
                    optional: false,
                    ..
                } = expr
                {
                    let obj = self.render_to_string(object);
                    let slot = match key {
                        IndexKey::Field(n) => format!(".{}", self.text_of(*n)),

                        IndexKey::Computed(e) => format!("[{}]", self.render_to_string(e)),
                    };
                    // A `;` keeps `f(x) (y).z = nil` from reading as a call.
                    let receiver = if self.options.check {
                        format!("; ({obj} :: any)")
                    } else {
                        format!(" {obj}")
                    };
                    tail.push_str(&format!("{receiver}{slot} = nil"));
                }

                self.generate(close, &tail);
            }

            Stmt::Destroy { expr, delay, span } => self.destroy(*span, expr, delay.as_ref()),

            Stmt::After(a) => self.after_block(a),

            // An attribute on a type alias has no Luau form. The target
            // check runs in `check_attrs`; the lines go blank, so the
            // output stays Luau and keeps its line count.
            Stmt::TypeAlias(t) if !t.attributes.is_empty() => {
                for a in &t.attributes {
                    self.blank_lines(self.byte_start(a.span), self.byte_end(a.span));
                }

                let first = t.span.start;
                let rest = TokSpan::new(
                    t.attributes
                        .iter()
                        .map(|a| a.span.end)
                        .max()
                        .unwrap_or(first) as usize,
                    t.span.end as usize,
                );
                let start = self.byte_start(rest);
                let end = self.byte_end(rest);
                // The copy starts where the last attribute ends, so the
                // newline between it and the keyword survives.
                let after_attrs = t
                    .attributes
                    .iter()
                    .map(|a| self.byte_end(a.span))
                    .max()
                    .unwrap_or(start);

                if self.wrote_global(t.span) {
                    let after_kw = self.toks[rest.start as usize].end;
                    self.copy(after_attrs, start);
                    self.generate(start, "export");
                    self.copy(after_kw, end);
                } else if !t.exported && self.export_listed_types.contains(self.text_of(t.name)) {
                    self.copy(after_attrs, start);
                    self.generate(start, "export ");
                    self.copy(start, end);
                } else {
                    self.copy(after_attrs, end);
                }
            }

            // The modifier is Alloy's; Luau reads the rest. `export`
            // takes its place, and the alias keeps every other byte.
            // `export type { T }` below it sends the alias out; Luau
            // has no other way to re-export one.
            Stmt::TypeAlias(t)
                if !t.exported && self.export_listed_types.contains(self.text_of(t.name)) =>
            {
                let start = self.byte_start(t.span);
                self.generate(start, "export ");
                self.copy(start, self.byte_end(t.span));
            }

            Stmt::TypeAlias(t) if self.wrote_global(t.span) => {
                let start = self.byte_start(t.span);
                let after = self.toks[t.span.start as usize].end;
                self.generate(start, "export");
                self.copy(after, self.byte_end(t.span));
            }

            Stmt::Function(f) if self.table_self_type(f).is_some() => {
                let target = self.table_self_type(f).unwrap_or_default();
                self.table_method(f, &target);
            }

            Stmt::Function(f) if function_needs_rewrite(&f.body) => {
                self.function_with_header(stmt.span(), &f.body);
            }

            Stmt::LocalFunction(f) if function_needs_rewrite(&f.body) => {
                self.function_with_header(stmt.span(), &f.body);
            }

            // `class` parses for the classes RFC and has no lowering yet:
            // a diagnostic, and the text goes, so the output stays Luau.
            Stmt::Class(c) => {
                self.diagnose(
                    c.span,
                    "`class` is parsed and not compiled yet; a `struct` with an `impl` is the form that runs",
                );
                self.blank_lines(self.byte_start(c.span), self.byte_end(c.span));
            }

            // `declare class Name ... end` is the spelling Luau's own
            // definition parser dropped; it reads `declare extern type
            // Name ... with ... end` now. One statement it cannot parse
            // costs the whole definitions file, so every other
            // declaration beside it stops reaching the checker.
            Stmt::Declare(d) if is_declare_class(self.src, self.toks, stmt) => {
                self.declare_class(d);
            }

            Stmt::Declare(d) if !d.patterns.is_empty() => {
                let (start, end) = (self.byte_start(d.span), self.byte_end(d.span));
                self.copy_declare(start, end, &d.patterns);
            }

            Stmt::GenericFor(f) if for_needs_rewrite(f) => self.generic_for(stmt.span(), f),

            /*
            Luau checks a `return` in a function written in a for-in header
            against the function around the loop: the loop's scope spans
            the header and is made first, and the lookup by position takes
            the first scope that encloses the `return`. The check artifact
            casts the value, so `xs:filter(function(v) return v > 2 end)`
            reports nothing.

            ponytail: the cast also hides a wrong return type there; drop
            this arm once Luau makes the loop scope after the header.
            */
            Stmt::Return(r)
                if self.options.check && self.for_header > 0 && !r.values.is_empty() =>
            {
                let mut cursor = self.byte_start(r.span);

                for v in &r.values {
                    let (vs, ve) = (self.byte_start(v.span()), self.byte_end(v.span()));
                    self.copy(cursor, vs);
                    self.generate(vs, "((");
                    self.expr(v);
                    self.generate(ve, ") :: any)");
                    cursor = ve;
                }

                self.copy(cursor, self.byte_end(r.span));
            }

            _ => {
                let span = stmt.span();
                let children = stmt_children(stmt);
                let reevaluated = reevaluated_conditions(stmt);
                let targets: Vec<*const Expr> = match stmt {
                    Stmt::Assign(a) => a.targets.iter().map(std::ptr::from_ref).collect(),

                    _ => Vec::new(),
                };
                let (narrow_blocks, narrow_after) = match stmt {
                    Stmt::If(i) => self.narrowings(i),

                    _ => (Vec::new(), None),
                };
                let header = u32::from(matches!(stmt, Stmt::GenericFor(_)));
                self.stitch(span, &children, |d, child| match child {
                    Child::Expr(e) => {
                        let at = std::ptr::from_ref::<Expr>(e);
                        let reads = d.reads;
                        d.for_header += header;
                        d.expr_lazy(reevaluated.contains(&at), e);
                        d.for_header -= header;

                        // A target is written, not read.
                        if targets.contains(&at) {
                            d.reads = reads;
                        }
                    }

                    Child::Block(b) => {
                        if let Some((_, prefix)) =
                            narrow_blocks.iter().find(|(at, _)| *at == b.span.start)
                        {
                            let anchor = d.byte_start(b.span);
                            d.generate(anchor, prefix);
                            d.r.end_stmt();
                        }

                        d.block(b);
                    }

                    Child::Function(b) => d.function_block(b),
                });

                if let Some(text) = narrow_after {
                    let anchor = self.byte_end(span);
                    self.generate(anchor, &text);
                    self.r.end_stmt();
                }
            }
        }
    }

    /*
    Renders a parent span by copying the text between its children and
    rendering each child. The children come in source order. This is the
    one routine that lets every node kind survive a desugar in one of its
    descendants without a renderer of its own.
    */
    pub(crate) fn stitch<F>(&mut self, span: TokSpan, children: &[Child<'_>], mut render: F)
    where
        F: FnMut(&mut Self, &Child<'_>),
    {
        let mut cursor = self.byte_start(span);
        let end = self.byte_end(span);

        for child in children {
            let cspan = child.span();

            if cspan.is_empty() {
                continue;
            }

            let cs = self.byte_start(cspan);
            self.copy(cursor, cs);
            render(self, child);
            cursor = self.byte_end(cspan);
        }

        self.copy(cursor, end);
    }

    /// Stitches children between two byte offsets, copying the text around them.
    pub(crate) fn stitch_between(&mut self, start: u32, end: u32, children: &[Child<'_>]) {
        let mut cursor = start;

        for child in children {
            let cspan = child.span();

            if cspan.is_empty() {
                continue;
            }

            let cs = self.byte_start(cspan);
            self.copy(cursor, cs);

            match child {
                Child::Expr(e) => self.expr(e),

                Child::Block(b) => self.block(b),

                Child::Function(b) => self.function_block(b),
            }

            cursor = self.byte_end(cspan);
        }

        self.copy(cursor, end);
    }

    // --- temps -------------------------------------------------------------

    /// `import(...)`: the reserved word called, which the parser lets
    /// through for this form alone.
    pub(crate) fn is_import_call(&self, e: &Expr) -> bool {
        let (base, links) = flatten(e);

        // A local named `import` is the file's own function, and a call
        // to it stays a call.
        matches!(base, Expr::Name(n) if self.text_of(*n) == "import")
            && !self.is_local("import")
            && matches!(
                links.first(),
                Some(Link::Plain(Step::Call { method: None, .. }))
            )
    }

    /// `Name(...)` with a struct's name and no fields table, or the fields
    /// form on a struct that writes `new`, outside its own impl.
    pub(crate) fn is_struct_call(&self, e: &Expr) -> bool {
        self.called_struct(e).is_some()
    }

    /// A chain that is a call statement: the guard becomes an `if`.
    pub(crate) fn chain_stmt(&mut self, e: &Expr) -> String {
        self.chain_anchor = self.byte_start(e.span());
        self.check_struct_call(e);
        let parts = self.chain_parts(e);

        match nil_tests(&parts.guards) {
            Some(g) => format!("if {g} then {} end", parts.inner),

            // A statement that opens with `(` reads as a call of the line
            // above in Luau. Inside `do ... end` it is the first statement
            // of a block, so nothing precedes it to be called.
            None if parts.inner.starts_with('(') => format!("do {} end", parts.inner),

            None => parts.inner,
        }
    }

    // --- assignment --------------------------------------------------------

    pub(crate) fn assign_needs_rewrite(&self, a: &Assign) -> bool {
        self.is_coalesce_assign(a.op) || a.targets.iter().any(chain_has_alloy)
    }

    /*
    An assignment whose target is an optional chain, or whose operator is
    `??=`.

    `a?.b.c = v` becomes `if G ~= nil then INNER = v end`, with the value
    inside the guard so it does not evaluate when the chain is nil. `t ??= v`
    becomes `if T == nil then T = v end`, where `T` reads twice, so a
    computed key or a call in the target hoists first. A plain name takes
    `x = if x == nil then v else x` instead: the value still evaluates only
    when `x` is nil, and the checker reads `x` as narrowed after it.
    */
    pub(crate) fn assign(&mut self, a: &Assign) {
        let anchor = self.byte_start(a.span);

        if a.targets.len() != 1 || a.values.len() != 1 {
            self.diagnose(
                a.span,
                "an assignment through `?` or with `??=` takes one target and one value",
            );
            self.copy_span(a.span);

            return;
        }

        let coalesce = self.is_coalesce_assign(a.op);
        self.chain_anchor = anchor;
        let (guard, target) = self.target_parts(&a.targets[0], coalesce);
        // `??=` and a target past `?` write the value on some paths only.
        let value = if coalesce || !guard.is_empty() {
            self.render_lazy(&a.values[0])
        } else {
            self.render_to_string(&a.values[0])
        };
        let op = if coalesce { "=" } else { self.text_of(a.op) };

        // A plain name takes the value through an `if` expression, not
        // an `if` statement. Both write the value only when the name is
        // nil; only the expression narrows, so `x ??= 1` on a `number?`
        // parameter leaves `x` a `number` for the checker.
        let plain = coalesce && matches!(&a.targets[0], Expr::Name(_));

        let body = match (coalesce, plain) {
            (_, true) => format!("{target} = if {target} == nil then {value} else {target}"),

            (true, false) => format!("if {target} == nil then {target} = {value} end"),

            (false, false) => format!("{target} {op} {value}"),
        };

        let text = match nil_tests(&guard) {
            Some(g) => format!("if {g} then {body} end"),

            None => body,
        };

        self.generate(anchor, &text);
    }

    /// The guard and the assignable text of a target. With `twice`, the
    /// object and key of the last link become names safe to read twice.
    pub(crate) fn target_parts(&mut self, target: &Expr, twice: bool) -> (Vec<String>, String) {
        let Expr::Index { object, key, .. } = target else {
            // A plain name, or something the parser let through as a target.
            return (Vec::new(), self.render_to_string(target));
        };

        self.chain_target = true;
        let parts = self.chain_parts(object);
        let mut guard = parts.guards;
        let mut obj = parts.inner;

        let optional = matches!(target, Expr::Index { optional: true, .. });
        let simple = guard.is_empty() && self.is_simple(object);

        if optional {
            let name = self.name_prefix(&mut obj, &mut guard, simple, false);
            obj = name;
        } else if twice && !simple {
            let whole = self.guarded(&guard, &obj);
            let anchor = self.chain_anchor;
            obj = self.hoist_text(whole, anchor);
            guard.clear();
        }

        let text = match key {
            IndexKey::Field(name) => format!("{obj}.{}", self.text_of(*name)),

            IndexKey::Computed(k) => {
                let k = if twice {
                    self.reusable(k)
                } else {
                    self.render_to_string(k)
                };

                format!("{obj}[{k}]")
            }
        };

        (guard, text)
    }

    // --- locals with destructuring or an initializer ----------------------

    /*
    `local { a, b = c }: T = t` becomes `local _1: T = t local a, c = _1.a,
    _1.b`. `local x = new X(args) { fields }` becomes `local _1 = X.new(args)`
    then one `_1.field = value` per field, then `local x = _1` on the line of
    the closing brace, so each field traces to its own line.
    */
    pub(crate) fn local_stmt(&mut self, l: &Local) {
        let anchor = self.byte_start(l.span);

        if let (
            1,
            1,
            Some(Expr::New {
                name,
                type_args,
                args,
                init: Some(init),
                span,
            }),
        ) = (l.names.len(), l.values.len(), l.values.first())
            && l.names[0].destructure.is_none()
            && !self.fields_form(name, args.as_ref(), Some(init))
        {
            self.local_new_init(l, name, *type_args, args.as_ref(), init, *span, anchor);

            return;
        }

        if l.names.len() != l.values.len() {
            self.diagnose(l.span, "destructuring needs one value per binding");
            self.copy_span(l.span);

            return;
        }

        let keyword = self.text_of(l.keyword).to_string();
        // The plain names with their annotations, and their values.
        let mut names: Vec<(TokSpan, String)> = Vec::new();
        let mut values = Vec::new();
        // One declaration per destructure: its names with their types,
        // and their values.
        let mut decls: Vec<(Vec<TypedName>, String)> = Vec::new();

        for (b, v) in l.names.iter().zip(&l.values) {
            self.expected_generic = b.ty.and_then(|t| generic_head(self.text_of(t)));
            let value = self.render_to_string(v);
            self.expected_generic = None;
            let ty =
                b.ty.map(|t| format!(": {}", self.text_of(t)))
                    .unwrap_or_default();

            match &b.destructure {
                None => {
                    names.push((b.name, ty));
                    values.push(value);
                }

                Some(d) => {
                    let typed = match b.ty {
                        Some(t) => format!("{value} :: {}", self.text_of(t)),

                        None => value,
                    };
                    let temp = self.hoist_text(typed, anchor);
                    let entries = self.destructure_entries(d, &temp, None);
                    let vs: Vec<String> = entries.iter().map(|(_, _, v)| v.clone()).collect();
                    decls.push((
                        entries.into_iter().map(|(n, t, _)| (n, t)).collect(),
                        vs.join(", "),
                    ));
                }
            }
        }

        // Each name copies from the source where its line allows, so
        // the editor maps it back to the name the reader wrote.
        let mut lead = "";

        if !names.is_empty() {
            self.generate(anchor, &format!("{keyword} "));

            for (i, (name, ty)) in names.iter().enumerate() {
                if i > 0 {
                    self.generate(anchor, ", ");
                }

                self.copy_on_line(anchor, *name);
                self.generate(anchor, ty);
            }

            self.generate(anchor, &format!(" = {}", values.join(", ")));
            lead = " ";
        }

        for (ns, vs) in decls.into_iter().filter(|(ns, _)| !ns.is_empty()) {
            self.generate(anchor, &format!("{lead}{keyword} "));

            for (i, (name, ty)) in ns.iter().enumerate() {
                if i > 0 {
                    self.generate(anchor, ", ");
                }

                self.copy_on_line(anchor, *name);

                if let Some(t) = ty {
                    self.generate(anchor, &format!(": {t}"));
                }
            }

            self.generate(anchor, &format!(" = {vs}"));
            lead = " ";
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn local_new_init(
        &mut self,
        l: &Local,
        name: &Expr,
        type_args: Option<TokSpan>,
        args: Option<&CallArgs>,
        init: &Expr,
        whole: TokSpan,
        anchor: u32,
    ) {
        // The binding itself holds the instance from the first line, so
        // a hover on its name finds a local there, and each field line
        // assigns through the name. `const` is a `local` in Luau.
        let ty = match l.names[0].ty {
            Some(t) => format!(": {}", self.text_of(t)),

            None => String::new(),
        };
        let binding = self.text_of(l.names[0].name).to_string();
        self.new_init(
            &format!("local {binding}{ty}"),
            &binding,
            name,
            type_args,
            args,
            init,
            whole,
            anchor,
        );
    }

    /// The initializer form: `decl = X.new(args)` on the first line, then
    /// one `binding.field = value` per field line.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_init(
        &mut self,
        decl: &str,
        binding: &str,
        name: &Expr,
        type_args: Option<TokSpan>,
        args: Option<&CallArgs>,
        init: &Expr,
        whole: TokSpan,
        anchor: u32,
    ) {
        self.check_new(name, args, Some(init), whole);
        let ctor = self.constructor_of(name);
        let n = self.render_to_string(name);
        let t = match type_args {
            Some(s) => {
                let text = self.text_of(s).to_string();

                self.lower_type_args(&text)
            }

            None => String::new(),
        };
        // With no arguments the name is one this file did not declare:
        // the runtime constructs it, and the check artifact types the
        // constructor call so the field lines below check against it.
        let head = match args {
            Some(a) => {
                let a = self.args_text(a);

                format!("{n}.{ctor}{t}{a}")
            }

            None if self.options.check => format!("{n}.{ctor}{t}({{}} :: any)"),

            None => {
                let std = self.std();

                format!("{std}.construct({n}, {{}})")
            }
        };
        self.generate(anchor, &format!("{decl} = {head}"));

        let Expr::Table { fields, span } = init else {
            unreachable!("the parser only builds a table initializer");
        };

        let open_end = self.toks[span.start as usize].end;
        let close = self.toks[span.end as usize - 1];
        let mut cursor = open_end;

        for field in fields {
            let (fs, fe) = self.field_bytes(field);
            // The gap holds the newlines; the comma becomes a space since
            // the fields are statements now.
            self.copy_gap_without_commas(cursor, fs);

            match field {
                TableField::Named { name, value } => {
                    let f = self.text_of(*name).to_string();
                    self.generate(fs, &format!("{binding}.{f} = "));
                    self.expr(value);
                }

                TableField::Computed { key, value } => {
                    let k = self.render_to_string(key);
                    self.generate(fs, &format!("{binding}[{k}] = "));
                    self.expr(value);
                }

                other => {
                    self.diagnose(*span, "an initializer takes `name = value` fields only");
                    let children = field_children(other);
                    self.stitch_between(fs, fe, &children);
                }
            }

            cursor = fe;
        }

        self.copy_gap_without_commas(cursor, close.start);
        let _ = close;
    }

    /// Copies a gap, turning each comma into a space so newlines survive.
    ///
    /// An empty gap is no gap, the way `copy` reads one. A lenient parse
    /// leaves a body with no closing `end`, so its last token is the
    /// last member and the gap after it runs backwards.
    pub(crate) fn copy_gap_without_commas(&mut self, start: u32, end: u32) {
        if start >= end {
            return;
        }

        let mut cursor = start;
        let gap = &self.src[start as usize..end as usize];

        for (i, b) in gap.bytes().enumerate() {
            if b == b',' {
                let at = start + i as u32;
                self.copy(cursor, at);
                self.generate(at, " ");
                cursor = at + 1;
            }
        }

        self.copy(cursor, end);
    }

    /// Each name a destructure binds, with its annotation and its value
    /// over a temp. `rest_type` is the type `...rest` takes; unset, it
    /// is `{ [string]: any }`, since the fields left over have no type
    /// the pattern names.
    pub(crate) fn destructure_entries(
        &mut self,
        d: &Destructure,
        temp: &str,
        rest_type: Option<&str>,
    ) -> Vec<(TokSpan, Option<String>, String)> {
        match d {
            Destructure::Table(fields) => {
                let named: Vec<String> = fields
                    .iter()
                    .filter(|f| !f.rest)
                    .map(|f| self.text_of(f.field).to_string())
                    .collect();
                let mut out = Vec::new();

                for f in fields {
                    out.push(match f.rest {
                        true => (
                            f.field,
                            Some(rest_type.unwrap_or("{ [string]: any }").to_string()),
                            format!("{}({temp})", rest_copy(&named)),
                        ),

                        // The field type copies through the type edits,
                        // so `string[]` and `~nil` take their Luau form.
                        false => (
                            f.rename.unwrap_or(f.field),
                            f.ty.map(|t| self.copy_type_to_string(t).trim().to_string()),
                            format!("{temp}.{}", self.text_of(f.field)),
                        ),
                    });
                }

                out
            }

            Destructure::Array { items, rest } => {
                let mut out: Vec<(TokSpan, Option<String>, String)> = items
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (*n, None, format!("{temp}[{}]", i + 1)))
                    .collect();

                if let Some(r) = rest {
                    let std = self.std();
                    out.push((
                        *r,
                        None,
                        format!("{std}.Array.slice({temp}, {})", items.len() + 1),
                    ));
                }

                out
            }
        }
    }

    /// A destructure as a prologue: each name copies from the source
    /// where its line allows, so the editor maps it to the pattern.
    pub(crate) fn destructure_pieces(
        &mut self,
        d: &Destructure,
        temp: &str,
        rest_type: Option<&str>,
    ) -> Vec<Piece> {
        let entries = self.destructure_entries(d, temp, rest_type);

        entry_pieces(&entries)
    }

    /// A bound `<T: Shape>` under a table has no Luau form, so a field of
    /// type `T` reads as `T` alone. Each such local takes the bound back
    /// through a cast, the way a bounded array's element read does.
    fn bind_entry_bounds(
        &mut self,
        p: &Param,
        entries: &mut [(TokSpan, Option<String>, String)],
        bounds: &[(String, String)],
    ) {
        let Some(Destructure::Table(fields)) = &p.destructure else {
            return;
        };
        let annotation = p.ty.map(|t| self.copy_type_to_string(t));
        let declared: Vec<(String, String)> = annotation
            .as_deref()
            .and_then(|a| a.trim().strip_prefix('{')?.strip_suffix('}'))
            .map(|inner| {
                split_top_level(inner, ',')
                    .into_iter()
                    .filter_map(|m| m.split_once(':'))
                    .map(|(n, t)| (n.trim().to_string(), t.trim().to_string()))
                    .collect()
            })
            .unwrap_or_default();

        for (f, (_, ty, value)) in fields.iter().zip(entries.iter_mut()) {
            let field = self.text_of(f.field);
            let Some(field_ty) = ty.clone().or_else(|| {
                declared
                    .iter()
                    .find(|(n, _)| n == field)
                    .map(|(_, t)| t.clone())
            }) else {
                continue;
            };
            let bounded = apply_bounds(&field_ty, bounds);

            if !f.rest && bounded != field_ty {
                *value = format!("({value} :: {bounded})");
                *ty = Some(bounded);
            }
        }
    }

    /// Prologues at an anchor, each behind a space. A bound name copies
    /// from the pattern, so the editor finds the local under the
    /// author's name.
    pub(crate) fn write_pieces(&mut self, anchor: u32, prologue: &[Vec<Piece>]) {
        for pieces in prologue.iter().filter(|p| !p.is_empty()) {
            self.generate(anchor, " ");

            for piece in pieces {
                match piece {
                    Piece::Text(t) => self.generate(anchor, t),

                    Piece::Name(n) => self.copy_on_line(anchor, *n),
                }
            }

            self.r.end_stmt();
        }
    }

    /// The problems of a parameter pattern, reported on the pattern.
    /// Returns the shape the typed fields state, when they state one.
    pub(crate) fn check_param_pattern(
        &mut self,
        pattern: TokSpan,
        ty: Option<TokSpan>,
        has_default: bool,
        d: &Destructure,
    ) -> Option<String> {
        let text = self.text_of(pattern).trim().to_string();

        if let Some(t) = ty {
            let ty = self.text_of(t).trim().to_string();

            // `Point | nil` may be nil as much as `Point?` is.
            let optional =
                ty.ends_with('?') || split_top_level(&ty, '|').iter().any(|m| m.trim() == "nil");

            if optional && !has_default {
                self.diagnose(t, &format!("a pattern needs a value; `{ty}` may be nil"));
            }
        }

        let Destructure::Table(fields) = d else {
            if ty.is_none() {
                self.diagnose(
                    pattern,
                    &format!("`{text}` has no type; annotate the parameter, `{text}: T[]`"),
                );
            }

            return None;
        };
        let named: Vec<&FieldBinding> = fields.iter().filter(|f| !f.rest).collect();
        let typed = named.iter().filter(|f| f.ty.is_some()).count();

        if let Some(t) = ty {
            let ty = self.text_of(t).trim().to_string();

            if typed > 0 {
                self.diagnose(
                    pattern,
                    &format!("`{text}: {ty}` states the shape twice; drop the field types or drop the annotation"),
                );

                return None;
            }

            // A struct, or a record type this file declares, says which
            // fields exist.
            let name = ty.trim_end_matches('?');

            if let Some(declared) = self.record_fields(name) {
                for f in &named {
                    let field = self.text_of(f.field).to_string();

                    if !declared.contains(&field) {
                        self.diagnose(f.field, &format!("`{name}` has no field `{field}`"));
                    }
                }
            }

            return None;
        }

        if typed == 0 {
            self.diagnose(
                pattern,
                &format!("`{text}` has no type; annotate the parameter, `{text}: Options`, or type each field"),
            );

            return None;
        }

        if typed < named.len() {
            self.diagnose(
                pattern,
                &format!("`{text}` types some fields and not others; type every field, or annotate the parameter"),
            );

            return None;
        }

        self.pattern_shape(d)
    }

    /// The shape a pattern states when it types every field it names.
    /// Each field type copies through the type edits, so the shape is
    /// Luau.
    pub(crate) fn pattern_shape(&mut self, d: &Destructure) -> Option<String> {
        let Destructure::Table(fields) = d else {
            return None;
        };
        let mut shape = Vec::new();

        for f in fields.iter().filter(|f| !f.rest) {
            let ty = self.copy_type_to_string(f.ty?);
            shape.push(format!("{}: {}", self.text_of(f.field), ty.trim()));
        }

        (!shape.is_empty()).then(|| format!("{{ {} }}", shape.join(", ")))
    }

    /// The field names of a struct, or of a record alias this file
    /// declares: `type Point = { x: number, y: number }`. `None` for
    /// any other type, and for a table with an indexer, which takes any
    /// key.
    fn record_fields(&self, name: &str) -> Option<Vec<String>> {
        if let Some(declared) = self.declared_fields(name) {
            return Some(declared.into_iter().map(|(n, _)| n).collect());
        }

        let value = self.alias_values.get(name)?.trim();
        let inner = value.strip_prefix('{')?.strip_suffix('}')?;

        split_top_level(inner, ',')
            .iter()
            .map(|m| m.trim())
            .filter(|m| !m.is_empty())
            .map(|m| {
                let m = m
                    .strip_prefix("read ")
                    .or_else(|| m.strip_prefix("write "))
                    .unwrap_or(m);
                let (field, _) = m.split_once(':')?;
                let field = field.trim();

                field
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
                    .then(|| field.to_string())
            })
            .collect()
    }

    /// The next `_pN` a pattern takes, skipping a number the source
    /// names: a `_p1` the author wrote would lose to the temp.
    pub(crate) fn pattern_temp(&self, n: &mut u32) -> String {
        *n += 1;

        while self.taken_temps.contains(n) {
            *n += 1;
        }

        format!("_p{n}")
    }

    /// A parameter list that holds a pattern binds each name once: a
    /// second binding would win in silence.
    pub(crate) fn check_param_names(&mut self, params: &[Param]) {
        if params.iter().all(|p| p.destructure.is_none()) {
            return;
        }

        let mut seen: Vec<String> = Vec::new();

        for p in params.iter().filter(|p| !p.is_vararg) {
            let names = match &p.destructure {
                Some(d) => destructure_names(d),

                None => vec![p.name],
            };

            for n in names {
                let name = self.text_of(n).to_string();

                if seen.contains(&name) {
                    self.diagnose(
                        n,
                        &format!("`{name}` is already a parameter; one name holds one declaration"),
                    );
                } else {
                    seen.push(name);
                }
            }
        }
    }

    // --- functions ---------------------------------------------------------

    /*
    A function with Alloy in its header: `async`, a `->` return type, a
    default, or a destructured parameter.

    The header copies with the `async` token dropped, `->` replaced by `:`,
    and a defaulted parameter's type made optional. A prologue right after
    the header rebinds defaults and unpacks destructures on the same line.
    An async body wraps in `return __alloy.future(function(...) ... end,
    ...)`, so the function returns a Future and the body runs on its own
    thread.
    */
    /// A function from `function` on, when the head before it was
    /// written or dropped already.
    pub(crate) fn function_rest(&mut self, rest: TokSpan, body: &FunctionBody) {
        if function_needs_rewrite(body) {
            self.function_with_header(rest, body);
        } else {
            let children = function_children(body);
            self.stitch(rest, &children, |d, child| match child {
                Child::Expr(e) => d.expr(e),

                Child::Block(b) => d.block(b),

                Child::Function(b) => d.function_block(b),
            });
        }
    }

    pub(crate) fn function_with_header(&mut self, span: TokSpan, body: &FunctionBody) {
        let start = self.byte_start(span);
        let end_tok = self.toks[span.end as usize - 1];
        let params_open = self.toks[self.params_open_tok(body) as usize];
        let mut cursor = start;

        // 1. The header up to `(`, minus the `async` token. An impl
        // method hands over the span past its name, and its `async`
        // sits before that: the caller dropped it already.
        if let Some(a) = body.is_async
            && self.byte_start(a) >= start
        {
            let as_ = self.byte_start(a);
            self.copy(cursor, as_);
            cursor = self.toks[a.end as usize].start;
        }

        self.check_bounds(body.generics);

        // A bound `<T: Shape>` has no Luau form: the generic list loses it
        // and each parameter typed `T` becomes `(T & Shape)`.
        let bounds: Vec<(String, String)> = body
            .generics
            .map(|g| generic_bounds(self.text_of(g)))
            .unwrap_or_default()
            .into_iter()
            .map(|(n, b)| {
                let resolved = self.resolve_bound(&b);

                (n, resolved)
            })
            .collect();

        // A closure inside the body names the same parameter, and the
        // `T` Luau sees there carries no bound. The annotation takes the
        // intersection the top-level parameters take.
        if !bounds.is_empty() {
            self.bind_nested_bounds(&body.block, &bounds);
        }

        // A bounded `T[]` parameter: the loop heads take the annotation
        // now, and `expr` casts the index reads while the body renders.
        let mut elements = Vec::new();

        if self.options.check && !bounds.is_empty() {
            elements = self.bounded_array_params(body, &bounds);

            if !elements.is_empty() {
                self.bind_loop_bounds(&body.block, &elements);
            }
        }

        if let Some(g) = body.generics
            && !bounds.is_empty()
        {
            let gs = self.byte_start(g);
            let ge = self.byte_end(g);
            self.copy(cursor, gs);
            let stripped = strip_bounds(self.text_of(g));
            self.generate(gs, &stripped);
            cursor = ge;
        }

        self.copy(cursor, params_open.end);
        cursor = params_open.end;

        // A colon method on a plain table: the source names no `self`,
        // so the header writes the parameter out.
        if let Some(target) = self.self_inject.take() {
            let sep = if body.params.is_empty() { "" } else { ", " };
            self.generate(params_open.end, &format!("self: {target}{sep}"));
        }

        // 2. Parameters: a destructure becomes a temp, a default makes the
        //    type optional, and both add a prologue line.
        let mut prologue: Vec<Vec<Piece>> = Vec::new();
        let mut param_temp = 0;
        self.check_param_names(&body.params);

        for p in &body.params {
            let ps = self.byte_start(p.name);
            self.copy(cursor, ps);

            // A pattern takes a temp in the list and opens on the
            // header's line; the typed fields state its type.
            let mut shape = None;
            let temp = match &p.destructure {
                Some(d) => {
                    let temp = self.pattern_temp(&mut param_temp);
                    self.generate(ps, &temp);
                    self.blank_lines(ps, self.byte_end(p.name));
                    shape = self.check_param_pattern(p.name, p.ty, p.default.is_some(), d);

                    Some(temp)
                }

                None => {
                    self.copy(ps, self.byte_end(p.name));

                    None
                }
            };

            cursor = self.byte_end(p.name);

            if let Some(shape) = &shape {
                let optional = if p.default.is_some() { "?" } else { "" };
                self.generate(cursor, &format!(": {shape}{optional}"));
            }

            if p.ty.is_none()
                && self.text_of(p.name) == "self"
                && let Some(target) = self.self_type.clone()
            {
                self.generate(cursor, &format!(": {target}"));
            }

            if let Some(t) = p.ty {
                if bounds.is_empty() {
                    self.copy(cursor, self.byte_end(t));
                } else {
                    let ts = self.byte_start(t);
                    self.copy(cursor, ts);
                    let text = self.copy_type_to_string(t);
                    self.generate(ts, &apply_bounds(&text, &bounds));
                }

                cursor = self.byte_end(t);

                if p.default.is_some() && !self.text_of(t).trim_end().ends_with('?') {
                    self.generate(cursor, "?");
                }
            }

            if let Some(default) = &p.default {
                let name = temp
                    .clone()
                    .unwrap_or_else(|| self.text_of(p.name).to_string());
                let value = self.render_to_string(default);
                let ty = match (p.ty, &shape) {
                    // The default settles the value, and the pattern
                    // reads fields off it: a pattern's local drops `nil`.
                    (Some(t), _) => {
                        let ty = self.copy_type_to_string(t);
                        let ty = match temp.is_some() {
                            true => split_top_level(ty.trim().trim_end_matches('?'), '|')
                                .into_iter()
                                .map(str::trim)
                                .filter(|m| *m != "nil")
                                .collect::<Vec<_>>()
                                .join(" | "),

                            false => ty,
                        };

                        format!(": {ty}")
                    }

                    (None, Some(shape)) => format!(": {shape}"),

                    (None, None) => String::new(),
                };
                prologue.push(vec![Piece::Text(format!(
                    "local {name}{ty} = if {name} == nil then {value} else {name}"
                ))]);
                // ` = default` disappears from the parameter list.
                cursor = self.byte_end(default.span());
            }

            // The pattern opens after the default settled the value.
            if let (Some(temp), Some(d)) = (&temp, &p.destructure) {
                let rest_type =
                    p.ty.map(|t| self.copy_type_to_string(t).trim().to_string())
                        .filter(|t| is_string_index_table(t));
                let mut entries = self.destructure_entries(d, temp, rest_type.as_deref());

                if !bounds.is_empty() {
                    self.bind_entry_bounds(p, &mut entries, &bounds);
                }

                prologue.push(entry_pieces(&entries));
            }
        }

        // 3. The close paren and the return type, with `->` as `:` and an
        //    async payload type wrapped in Future.
        let close = self.toks[self.params_close_tok(body) as usize];
        self.copy(cursor, close.end);
        cursor = close.end;

        if let Some(arrow) = body.ret_arrow {
            let ae = self.byte_end(arrow);
            // `) -> T` becomes `): T`: the space before the arrow goes.
            self.generate(cursor, ":");
            cursor = ae;
        }

        if let Some(rt) = body.ret_type {
            let (rs, re) = (self.byte_start(rt), self.byte_end(rt));
            self.copy(cursor, rs);

            if body.is_async.is_some() {
                let std = self.std();

                // `Future<()>` is no Luau type: a type pack is no
                // type argument. `nil` stands in, and the editor reads
                // it back as `nil`, which a reader can write.
                let declared = self.text_of(rt).trim().to_string();

                // `async function f(): Future<T>` names the answer
                // itself, the way a reader of TypeScript writes
                // `Promise<T>`. Wrapping it again would build a Future
                // of a Future, which is never what the line means.
                if declared == "()" {
                    self.generate(rs, &format!("{std}.Future<nil>"));
                } else if names_a_future(&declared) {
                    self.copy(rs, re);
                } else if pack_inner(&declared).is_some() {
                    // `(A, B)` is a type pack, and a pack is no type
                    // argument. The Future takes the values one by one,
                    // `Future<A, B>`, so the parens go.
                    let open = self.toks[rt.start as usize].end;
                    let close = self.toks[rt.end as usize - 1].start;
                    self.generate(rs, &format!("{std}.Future<"));
                    self.copy(open, close);
                    self.generate(close, ">");
                } else {
                    self.generate(rs, &format!("{std}.Future<"));
                    self.copy(rs, re);
                    self.generate(re, ">");
                }
            } else {
                self.copy(rs, re);
            }

            cursor = re;
        } else if body.is_async.is_some() && !returns_value(&body.block) {
            // An async body that returns nothing resolves to nothing: the
            // checker would infer `Future<unknown>` from the wrapper.
            // `()` is no type argument, so nil stands in.
            let std = self.std();
            self.generate(cursor, &format!(": {std}.Future<nil>"));
        }

        // 4. The prologue and the async wrapper, on the header line.
        let has_vararg = body.params.iter().any(|p| p.is_vararg);
        let mut lead = String::new();
        // A prologue ends in an expression. The async wrapper ends in
        // `function()`, and a `;` there opens the body with no statement.
        let ends_open = body.is_async.is_none()
            && (self.self_prologue.is_some() || prologue.iter().any(|p| !p.is_empty()));

        if let Some(p) = self.self_prologue.take() {
            lead.push(' ');
            lead.push_str(&p);
        }

        // Each prologue writes its text, and a bound name copies from the
        // pattern, so the editor finds the local under the author's name.
        for pieces in prologue.iter().filter(|p| !p.is_empty()) {
            lead.push(' ');

            for piece in pieces {
                match piece {
                    Piece::Text(t) => lead.push_str(t),

                    Piece::Name(n) => {
                        self.generate(cursor, &std::mem::take(&mut lead));
                        self.copy_on_line(cursor, *n);
                    }
                }
            }
        }

        // A body that returns nothing resolves to nil: the wrapper alone
        // would infer `Future<unknown>`, which the header's `Future<nil>`
        // refuses.
        let empty_ret = match body.ret_type {
            Some(rt) => self.text_of(rt).trim() == "()",

            None => true,
        };
        let to_nil = body.is_async.is_some() && empty_ret && !returns_value(&body.block);

        if body.is_async.is_some() {
            let std = self.std();
            // The lambda carries the declared payload type. Without it
            // the checker infers the body's result, and a result whose
            // own type is still open lands on `unknown`: `await` over
            // `Future.race` is one such result.
            let payload = match body.ret_type {
                Some(rt) if !to_nil => {
                    let text = self.text_of(rt).trim().to_string();
                    // A header that names `Future<T>` names the answer,
                    // not what the body returns. The body returns `T`,
                    // so the lambda carries that.
                    let text = settled_type(&text).unwrap_or(text);

                    match text.as_str() {
                        "()" => String::new(),
                        _ => format!(": {}", self.lower_type(&text)),
                    }
                }

                _ => String::new(),
            };

            lead.push_str(&format!(
                " return {}{std}.future(function({}){payload}",
                if to_nil { "(" } else { "" },
                if has_vararg { "..." } else { "" }
            ));
        }

        if !lead.is_empty() {
            self.generate(cursor, &lead);
        }

        if ends_open {
            self.r.end_stmt();
        }

        // 5. The body, its trailing trivia, and the close.
        let body_start = self.block_start_or(&body.block, end_tok.start);
        self.copy(cursor, body_start);
        let saved_elems = if elements.is_empty() {
            None
        } else {
            Some(std::mem::replace(&mut self.elem_bounds, elements))
        };
        self.function_block(body);

        if let Some(saved) = saved_elems {
            self.elem_bounds = saved;
        }

        let after = self.block_end_or(&body.block, body_start);
        self.copy(after, end_tok.start);

        if body.is_async.is_some() {
            let std = self.std();
            let close = if has_vararg { "end, ...)" } else { "end)" };
            let text = if to_nil {
                format!("{close} :: any) :: {std}.Future<nil> ")
            } else {
                format!("{close} ")
            };
            self.generate(end_tok.start, &text);
        }

        self.copy(end_tok.start, end_tok.end);
    }

    /// Wraps every nested annotation that names a bounded parameter in
    /// `(T & Bound)`. The wrap is two inserts, so the annotation copies
    /// as the source wrote it with the bound around it.
    pub(crate) fn bind_nested_bounds(&mut self, block: &Block, bounds: &[(String, String)]) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Local(l) => {
                    for b in &l.names {
                        self.bind_type_bounds(b.ty, bounds);
                    }
                }

                Stmt::GenericFor(f) => {
                    for b in &f.vars {
                        self.bind_type_bounds(b.ty, bounds);
                    }
                }

                _ => {}
            }

            self.bind_bounds_in(stmt_children(stmt), bounds);
        }
    }

    pub(crate) fn bind_bounds_in(&mut self, children: Vec<Child<'_>>, bounds: &[(String, String)]) {
        for child in children {
            match child {
                Child::Expr(e) => {
                    if let Expr::TypeAssert { ty, .. } = e {
                        self.bind_type_bounds(Some(*ty), bounds);
                    }

                    self.bind_bounds_in(expr_children(e), bounds);
                }

                Child::Block(b) => self.bind_nested_bounds(b, bounds),

                Child::Function(f) => {
                    // A generic list of its own shadows the outer name.
                    let own: Vec<String> = f
                        .generics
                        .map(|g| generic_names(self.text_of(g)))
                        .unwrap_or_default();
                    let bounds: Vec<(String, String)> = bounds
                        .iter()
                        .filter(|(n, _)| !own.contains(n))
                        .cloned()
                        .collect();

                    for p in &f.params {
                        self.bind_type_bounds(p.ty, &bounds);
                    }

                    self.bind_type_bounds(f.ret_type, &bounds);
                    self.bind_nested_bounds(&f.block, &bounds);
                }
            }
        }
    }

    /// A type written in a bounded function's body names the parameter
    /// Luau sees with no bound: `local best: T?`. Each bare `T` in it
    /// takes the intersection the parameters take.
    fn bind_type_bounds(&mut self, ty: Option<TokSpan>, bounds: &[(String, String)]) {
        let Some(t) = ty else {
            return;
        };
        let open = self.byte_start(t);

        for (start, end, bound) in bound_spots(self.text_of(t), bounds) {
            self.inserts.push((open + start as u32, "(".to_string()));
            self.inserts
                .push((open + end as u32, format!(" & {bound})")));
        }
    }

    /*
    The check artifact reads an element of a bounded `T[]` at its bound.

    Luau's Array is invariant, so `Array<T & Bound>` accepts no concrete
    argument and the parameter stays `Array<T>`. The bound comes back at
    each element read instead: a loop variable takes an annotation, and
    an index expression takes a cast in `expr`.
    */
    pub(crate) fn bounded_array_params(
        &mut self,
        body: &FunctionBody,
        bounds: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();

        for p in &body.params {
            let Some(t) = p.ty else {
                continue;
            };
            let Some(elem) = array_element(self.text_of(t)) else {
                continue;
            };
            // The element carries the bound: `T` reads as `(T & Bound)`,
            // and `Box<T>` as `Box<(T & Bound)>`. An element that names
            // no bounded parameter comes back unchanged.
            let bounded = apply_bounds(elem, bounds);

            if bounded == elem {
                continue;
            }

            out.push((self.text_of(p.name).to_string(), bounded));
        }

        out
    }

    /// Annotates every `for _, x in xs do` whose `xs` is a bounded `T[]`.
    /// The annotation is one insert inside the loop head, so the head
    /// copies as the source wrote it.
    pub(crate) fn bind_loop_bounds(&mut self, block: &Block, elements: &[(String, String)]) {
        for stmt in &block.stmts {
            if let Stmt::GenericFor(f) = stmt
                && let [var, elem] = &f.vars[..]
                && var.destructure.is_none()
                && elem.ty.is_none()
                && elem.destructure.is_none()
                && let [Expr::Name(n)] = &f.exprs[..]
                && let Some((_, ty)) = elements.iter().find(|(p, _)| p == self.text_of(*n))
            {
                let at = self.byte_end(elem.name);
                let ty = ty.clone();
                self.inserts.push((at, format!(": {ty}")));
            }

            self.loop_bounds_in(stmt_children(stmt), elements);
        }
    }

    pub(crate) fn loop_bounds_in(
        &mut self,
        children: Vec<Child<'_>>,
        elements: &[(String, String)],
    ) {
        for child in children {
            match child {
                Child::Expr(_) => {}

                Child::Block(b) => self.bind_loop_bounds(b, elements),

                Child::Function(f) => self.bind_loop_bounds(&f.block, elements),
            }
        }
    }

    /// Whether a statement reads an index of a bounded `T[]`. The walk
    /// copies a statement whole when nothing under it changes, so the
    /// cast has to announce itself here.
    pub(crate) fn holds_element_index(&self, s: &Stmt) -> bool {
        stmt_children(s).iter().any(|c| self.element_index_in(c))
    }

    pub(crate) fn element_index_in(&self, c: &Child<'_>) -> bool {
        match c {
            Child::Expr(e) => {
                matches!(e, Expr::Index { object, key: IndexKey::Computed(_), optional: false, .. }
                    if self.element_bound(object).is_some())
                    || expr_children(e).iter().any(|c| self.element_index_in(c))
            }

            Child::Block(b) => b.stmts.iter().any(|s| self.holds_element_index(s)),

            Child::Function(f) => f.block.stmts.iter().any(|s| self.holds_element_index(s)),
        }
    }

    /// The bound an index of this object reads back as.
    pub(crate) fn element_bound(&self, object: &Expr) -> Option<String> {
        let Expr::Name(n) = object else {
            return None;
        };
        let name = self.text_of(*n);

        self.elem_bounds
            .iter()
            .find(|(p, _)| p == name)
            .map(|(_, ty)| ty.clone())
    }

    pub(crate) fn params_open_tok(&self, body: &FunctionBody) -> u32 {
        let mut i = body.span.start;

        while self.toks[i as usize].text(self.src) != "(" {
            i += 1;
        }

        i
    }

    /// The `)` that closes the parameter list: the last `)` before the
    /// return type, or before the body.
    pub(crate) fn params_close_tok(&self, body: &FunctionBody) -> u32 {
        let limit = match (body.ret_arrow, body.ret_type) {
            (Some(a), _) => a.start,

            (None, Some(t)) => t.start - 1,

            (None, None) => {
                if body.block.span.is_empty() {
                    body.span.end - 1
                } else {
                    body.block.span.start
                }
            }
        };
        let mut i = limit;

        while self.toks[i as usize].text(self.src) != ")" {
            i -= 1;
        }

        i
    }

    // --- destroy and after -------------------------------------------------

    /// `destroy x` and `destroy x after n`.
    ///
    /// Without a delay the runtime calls the one method the value has.
    /// With one the emit picks the mechanism from the type the file
    /// shows: Debris for an Instance, a timer for a table, and the
    /// runtime helper when the file says nothing.
    pub(crate) fn destroy(&mut self, span: TokSpan, expr: &Expr, delay: Option<&Expr>) {
        let start = self.byte_start(span);
        let end = self.byte_end(span);
        let std = self.std();

        let Some(delay) = delay else {
            self.generate(start, &format!("{std}.destroy("));
            self.expr(expr);
            self.generate(end, ")");

            return;
        };

        match self.timed_kind(expr) {
            // Debris outlives the script that scheduled the removal.
            Timed::Instance => {
                self.generate(start, "game:GetService(\"Debris\"):AddItem(");
                self.expr(expr);
                self.generate(self.byte_end(expr.span()), ", ");
                self.expr(delay);
                self.generate(end, ")");
            }

            // The seconds go in front of the target, so they render to
            // text; the target keeps its place, and a hover on it lands.
            Timed::Method(name) => {
                let seconds = self.render_to_string(delay);
                self.generate(start, &format!("task.delay({seconds}, function() "));
                self.expr(expr);
                self.generate(end, &format!(":{name}() end)"));
            }

            Timed::Unknown => {
                self.generate(start, &format!("{std}.destroy_after("));
                self.expr(expr);
                self.generate(self.byte_end(expr.span()), ", ");
                self.expr(delay);
                self.generate(end, ")");
            }
        }
    }

    /// `after 3 do ... end`, with the `where` condition read when the
    /// timer fires and not when it is set.
    pub(crate) fn after_block(&mut self, a: &After) {
        self.generate(self.byte_start(a.span), "task.delay(");
        self.expr(&a.delay);
        let do_tok = self.find_tok_after(a.delay.span().end, "do");
        let do_end = self.toks[do_tok as usize].end;

        match &a.filter {
            Some(c) => {
                let cond = self.render_to_string(c);
                self.generate(do_end, &format!(", function() if {cond} then"));
            }

            None => self.generate(do_end, ", function()"),
        }

        self.scopes.push(HashSet::new());
        let body_start = self.block_start_or(&a.block, do_end);
        self.copy(do_end, body_start);
        self.block(&a.block);
        let body_end = self.block_end_or(&a.block, body_start);
        self.scopes.pop();
        let end_tok = self.toks[a.span.end as usize - 1];
        self.copy(body_end, end_tok.start);

        if a.filter.is_some() {
            self.generate(end_tok.start, "end ");
        }

        self.copy(end_tok.start, end_tok.end);
        self.generate(end_tok.end, ")");
    }

    /// The mechanism a timed `destroy` uses, from what the file says
    /// about the operand's type.
    fn timed_kind(&self, expr: &Expr) -> Timed {
        if self.builds_an_instance(expr) {
            return Timed::Instance;
        }

        let Expr::Name(n) = expr else {
            return Timed::Unknown;
        };
        let name = self.text_of(*n);

        if let Some(ty) = self.annotation_of(name) {
            let base = ty.trim_end_matches('?');

            if base == "Instance" || crate::roblox_classes::INSTANCE_CLASSES.contains(&base) {
                return Timed::Instance;
            }

            if let Some(method) = self.destroy_method_of(base) {
                return Timed::Method(method);
            }
        }

        match self.init_constructor_of(name) {
            Some(built) if built == "Instance" => Timed::Instance,

            Some(built) => match self.destroy_method_of(&built) {
                Some(method) => Timed::Method(method),

                None => Timed::Unknown,
            },

            None => Timed::Unknown,
        }
    }

    /// `Instance.new(...)` or `new Instance(...)`.
    fn builds_an_instance(&self, expr: &Expr) -> bool {
        match self.constructed_name(expr) {
            Some(name) => name == "Instance",

            None => false,
        }
    }

    /// The name a constructor call builds: `new Part(...)` and
    /// `Part.new(...)` both give `Part`.
    fn constructed_name(&self, expr: &Expr) -> Option<String> {
        if let Expr::New { name, .. } = expr {
            return match &**name {
                Expr::Name(n) => Some(self.text_of(*n).to_string()),

                _ => None,
            };
        }

        let Expr::Call { func, .. } = expr else {
            return None;
        };
        let Expr::Index {
            object,
            key: IndexKey::Field(f),
            ..
        } = &**func
        else {
            return None;
        };

        match (&**object, self.text_of(*f)) {
            (Expr::Name(n), "new") => Some(self.text_of(*n).to_string()),

            _ => None,
        }
    }

    /// The annotation on `local name: T`, `const name: T`, or a
    /// parameter of that name. The first one the file writes answers.
    fn annotation_of(&self, name: &str) -> Option<String> {
        let text = |i: usize| self.toks.get(i).map(|t| t.text(self.src)).unwrap_or("");

        for i in 1..self.toks.len() {
            if text(i) != name || text(i + 1) != ":" {
                continue;
            }

            if !matches!(text(i - 1), "local" | "const" | "(" | ",") {
                continue;
            }

            let head = text(i + 2);

            if head.is_empty() || !head.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                return None;
            }

            return Some(match text(i + 3) == "?" {
                true => format!("{head}?"),

                false => head.to_string(),
            });
        }

        None
    }

    /// The name a `local name = ...` constructs: `new Part(...)` and
    /// `Part.new(...)` both give `Part`.
    fn init_constructor_of(&self, name: &str) -> Option<String> {
        let text = |i: usize| self.toks.get(i).map(|t| t.text(self.src)).unwrap_or("");

        for i in 1..self.toks.len() {
            if text(i) != name || text(i + 1) != "=" {
                continue;
            }

            if !matches!(text(i - 1), "local" | "const") {
                continue;
            }

            if text(i + 2) == "new" {
                return Some(text(i + 3).to_string());
            }

            if text(i + 3) == "." && text(i + 4) == "new" {
                return Some(text(i + 2).to_string());
            }

            return None;
        }

        None
    }

    /// The `destroy` or `Destroy` an `impl` of this name writes.
    fn destroy_method_of(&self, name: &str) -> Option<&'static str> {
        let text = |i: usize| self.toks.get(i).map(|t| t.text(self.src)).unwrap_or("");
        let mut i = 0;

        while i + 2 < self.toks.len() {
            if text(i) != "impl" || text(i + 1) != name {
                i += 1;

                continue;
            }

            let mut j = i + 2;

            while j < self.toks.len() && text(j) != "impl" && text(j) != "struct" {
                if text(j) == "function" {
                    match text(j + 1) {
                        "destroy" => return Some("destroy"),
                        "Destroy" => return Some("Destroy"),
                        _ => {}
                    }
                }

                j += 1;
            }

            i = j;
        }

        None
    }

    // --- loops -------------------------------------------------------------

    /// `for a, { x } in t where c do`: a filter and destructured variables.
    pub(crate) fn generic_for(&mut self, span: TokSpan, f: &GenericFor) {
        let start = self.byte_start(span);
        let mut cursor = start;
        let mut prologue: Vec<Vec<Piece>> = Vec::new();
        let mut temp = 0;

        for v in &f.vars {
            let vs = self.byte_start(v.name);
            self.copy(cursor, vs);

            match &v.destructure {
                Some(d) => {
                    let t = self.pattern_temp(&mut temp);
                    self.generate(vs, &t);
                    self.blank_lines(vs, self.byte_end(v.name));
                    prologue.push(self.destructure_pieces(d, &t, None));
                }

                None => self.copy(vs, self.byte_end(v.name)),
            }

            cursor = self.byte_end(v.name);

            if let Some(t) = v.ty {
                self.copy(cursor, self.byte_end(t));
                cursor = self.byte_end(t);
            }
        }

        // `in` and the iterator expressions.
        let last_expr = f
            .exprs
            .last()
            .map(|e| self.byte_end(e.span()))
            .unwrap_or(cursor);
        let children: Vec<Child<'_>> = f.exprs.iter().map(Child::Expr).collect();
        self.for_header += 1;
        self.stitch_between(cursor, last_expr, &children);
        self.for_header -= 1;
        cursor = last_expr;

        // `do`, with the filter turned into a guard after it.
        let do_tok = self.find_tok_after(
            f.exprs.last().map(|e| e.span().end).unwrap_or(span.start),
            "do",
        );
        let do_start = self.toks[do_tok as usize].start;
        let do_end = self.toks[do_tok as usize].end;

        // The destructure prologue comes first, so the filter can read the
        // names it binds.
        match &f.filter {
            Some(c) => {
                let cond = self.render_to_string(c);
                let where_tok = self.toks[c.span().start as usize - 1];
                self.copy(cursor, where_tok.start);
                self.copy(do_start, do_end);
                self.write_pieces(do_end, &prologue);
                self.generate(do_end, &format!(" if not ({cond}) then continue end"));
            }

            None => {
                self.copy(cursor, do_end);
                self.write_pieces(do_end, &prologue);
            }
        }

        cursor = do_end;
        self.scopes.push(HashSet::new());

        for v in &f.vars {
            self.declare_binding(v);
        }

        let body_start = self.block_start_or(&f.block, cursor);
        self.copy(cursor, body_start);
        self.block(&f.block);
        let after = self.block_end_or(&f.block, body_start);
        self.scopes.pop();
        let end_tok = self.toks[span.end as usize - 1];
        self.copy(after, end_tok.start);
        self.copy(end_tok.start, end_tok.end);
    }

    /// The first token at or after `from` that reads `text`. A tree from
    /// a lenient parse may lack it, and the last token answers then, so a
    /// half-typed file reports instead of crashing the server.
    pub(crate) fn find_tok_after(&self, from: u32, text: &str) -> u32 {
        let last = self.toks.len().saturating_sub(1) as u32;
        let mut i = from.min(last);

        while i < last && self.toks[i as usize].text(self.src) != text {
            i += 1;
        }

        i
    }
}

/// A `match` whose arms run statements before their value, which only
/// the statement forms `local x =`, `x =`, and `return` can hold.
pub(crate) fn block_arm_match(e: &Expr) -> Option<&MatchExpr> {
    let Expr::Match(m) = e else {
        return None;
    };
    let block = |v: &Expr| matches!(v, Expr::Block { .. });

    (m.arms.iter().any(|a| block(&a.value)) || m.default.as_deref().is_some_and(block)).then_some(m)
}

/// Whether a statement is `declare class Name ... end`, the definition
/// spelling Luau's own parser dropped.
fn is_declare_class(src: &str, toks: &[alloy_syntax::lexer::Tok], s: &Stmt) -> bool {
    let Stmt::Declare(d) = s else {
        return false;
    };
    let at = d.span.start as usize + 1;

    toks.get(at)
        .is_some_and(|t| &src[t.start as usize..t.end as usize] == "class")
}

/// A name a destructure binds, with its annotation.
type TypedName = (TokSpan, Option<String>);

/// A piece of a prologue: generated text, or a name copied from the
/// source.
#[derive(Debug, Clone)]
pub(crate) enum Piece {
    Text(String),
    Name(TokSpan),
}

/// A destructure's entries as a prologue: `local x: T, y = a.x, a.y`.
/// Each name copies from the source where its line allows, so the
/// editor maps it to the pattern. `{ }` binds nothing: someone is
/// typing, and the list completes.
fn entry_pieces(entries: &[(TokSpan, Option<String>, String)]) -> Vec<Piece> {
    if entries.is_empty() {
        return Vec::new();
    }

    let mut out = vec![Piece::Text("local ".to_string())];

    for (i, (name, ty, _)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(Piece::Text(", ".to_string()));
        }

        out.push(Piece::Name(*name));

        if let Some(t) = ty {
            out.push(Piece::Text(format!(": {t}")));
        }
    }

    let values: Vec<&str> = entries.iter().map(|(_, _, v)| v.as_str()).collect();
    out.push(Piece::Text(format!(" = {}", values.join(", "))));

    out
}

/// A function that copies a table without the named fields, for
/// `...rest`. The loop reads the value through `any`, since a record
/// type has no indexer to iterate.
pub(crate) fn rest_copy(named: &[String]) -> String {
    let keep = match named.is_empty() {
        true => "true".to_string(),

        false => named
            .iter()
            .map(|n| format!("k ~= \"{n}\""))
            .collect::<Vec<_>>()
            .join(" and "),
    };

    format!(
        "(function(t: any): any local r: {{ [any]: unknown }} = {{}} for k, v in t do if {keep} then r[k] = v end end return r end)"
    )
}

/// Whether a type is a table with a string index and nothing else:
/// `{ [string]: T }`, the type `...rest` takes over from its annotation.
pub(crate) fn is_string_index_table(t: &str) -> bool {
    let Some(inner) = t.strip_prefix('{').and_then(|r| r.strip_suffix('}')) else {
        return false;
    };
    let inner = inner.trim();

    inner.starts_with("[string]:") && !inner.contains(',')
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

    /// A statement the desugar writes in front of source code ends in an
    /// expression, so a next line that opens with `(` read as a call on
    /// it: the narrowing of `is table`, a payload binding, a hoisted temp,
    /// a parameter prologue. A `;` now ends it, and both artifacts parse.
    #[test]
    fn a_paren_line_after_a_written_statement_stays_a_statement() {
        let src = "struct Pt\n    x: number\nend\nenum Job\n    Idle\n    Build(string)\nend\nlocal function get(): number?\n    return 1\nend\nlocal function a(value: unknown, i: Instance, job: Job)\n    if value is table then\n        (i :: any).Name = \"t\"\n    end\n    if local v = get() then\n        -- a note\n        (i :: any).Name = tostring(v)\n    end\n    local Build(m) = job else return end\n    (i :: any).Name = m\n    match job with\n        case Build(n) then\n            (i :: any).Name = n\n        case Idle then\n    end\n    (i :: any).Name = tostring(get() ?? 0)\n    if value is not Pt then return end\n    (i :: any).Name = \"p\"\nend\nlocal function b({ x }: Pt, i: Instance, n: number = 1)\n    (i :: any).Name = tostring(x + n)\nend\nprint(a, b)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("{ [any]: any }) ;(i :: any).Name"),
            "{}",
            out.check
        );

        let lua = mlua::Lua::new();

        for text in [&out.ship, &out.check] {
            if let Err(e) = lua.load(text.as_str()).into_function() {
                panic!("{e}\n{text}");
            }
        }
    }

    /// Two declarations of one name in one file. The second wins in
    /// silence, so a use of the first reads the other shape. The report
    /// sits on the second name and says what holds it.
    #[test]
    fn one_name_holds_one_declaration() {
        let pairs = [
            (
                "enum Shape as\n    Circle\nend\n",
                "struct Shape as\n    kind: string\nend\n",
                "`Shape` is already an enum on line 1; one name holds one declaration",
            ),
            (
                "struct Shape as\n    kind: string\nend\n",
                "enum Shape as\n    Circle\nend\n",
                "`Shape` is already a struct on line 1; one name holds one declaration",
            ),
            (
                "trait Shape as\n    function area(self): number\nend\n",
                "interface Shape as\n    kind: string\nend\n",
                "`Shape` is already a trait on line 1; one name holds one declaration",
            ),
            (
                "interface Shape as\n    kind: string\nend\n",
                "namespace Shape as\n    const n = 1\nend\n",
                "`Shape` is already an interface on line 1; one name holds one declaration",
            ),
            (
                "namespace Shape as\n    const n = 1\nend\n",
                "enum Shape as\n    Circle\nend\n",
                "`Shape` is already a namespace on line 1; one name holds one declaration",
            ),
            (
                "attribute Shape on function\n",
                "struct Shape as\n    kind: string\nend\n",
                "`Shape` is already an attribute on line 1; one name holds one declaration",
            ),
        ];

        for (first, second, want) in pairs {
            let src = format!("{first}{second}");

            assert!(
                messages(&src).contains(&want.to_string()),
                "{:?}",
                messages(&src)
            );
        }

        // Two namespaces of one name report once, in the same words.
        let two =
            "namespace Shape as\n    const n = 1\nend\nnamespace Shape as\n    const m = 2\nend\n";
        assert_eq!(
            messages(two),
            vec!["`Shape` is already a namespace on line 1; one name holds one declaration"]
        );

        // One name per declaration is clean.
        let ok = "enum Shape as\n    Circle\nend\nstruct Box as\n    kind: string\nend\nprint(Shape, Box)\n";
        assert!(messages(ok).is_empty(), "{:?}", messages(ok));
    }

    /// A function or a local under an import of its name replaces the
    /// import in silence. The pair reports once, at the binding. A local
    /// inside a function body is its own scope and stays quiet.
    #[test]
    fn a_binding_under_an_import_of_its_name_reports_once() {
        let head = "import { f } from \"./lib\"\n\n";
        let want = vec!["`f` is already imported on line 1; one name holds one declaration"];

        for decl in [
            "function f(): number\n    return 2\nend\n",
            "async function f(): number\n    return 2\nend\n",
            "local function f(): number\n    return 2\nend\n",
            "local f = 2\n",
            "const f = 2\n",
        ] {
            let src = format!("{head}{decl}print(f)\n");
            assert_eq!(messages(&src), want, "{decl}");
        }

        let inner =
            format!("{head}function g(): number\n    local f = 2\n    return f\nend\nprint(g())\n");
        assert!(messages(&inner).is_empty(), "{:?}", messages(&inner));
    }

    /// A remote registers one channel under its name, and a macro binds
    /// one template. A second of either used to win in silence, so the
    /// wire spec and the expansion both changed with no report.
    #[test]
    fn a_remote_and_a_macro_hold_one_name_each() {
        // Two remotes of one name, with different wire specs.
        let src = "remote function Ping(id: number): boolean from client\nremote function Ping(id: string): boolean from client\n";
        assert!(
            messages(src).contains(
                &"`Ping` is already a remote on line 1; one name holds one declaration".to_string()
            ),
            "{:?}",
            messages(src)
        );

        // Two macros of one name. A different arity is no overload, so
        // the second reports as well.
        let src = "macro double(x)\n    x + x\nend\nmacro double(x, y)\n    x * y\nend\nprint($double(3))\n";
        assert!(
            messages(src).contains(
                &"`double` is already a macro on line 1; one name holds one declaration"
                    .to_string()
            ),
            "{:?}",
            messages(src)
        );

        // One name each is clean, and the report comes once.
        let ok = "remote function Ping(id: number): boolean from client\nmacro double(x)\n    x + x\nend\nprint($double(3))\n";
        assert!(messages(ok).is_empty(), "{:?}", messages(ok));
    }

    /// Every duplicate prints one kind and one code: `error(6.1):
    /// DuplicateError`, the section the kind opens, whatever the
    /// declaration is.
    #[test]
    fn every_duplicate_prints_the_kinds_section() {
        let sources = [
            "struct Dup as\n    x: number\nend\nstruct Dup as\n    y: number\nend\n",
            "enum Dup as\n    A\nend\nenum Dup as\n    B\nend\n",
            "remote function Dup(id: number): boolean from client\nremote function Dup(id: string): boolean from client\n",
            "namespace Dup as\n    const n = 1\nend\nnamespace Dup as\n    const m = 2\nend\n",
            "macro Dup(x)\n    x + x\nend\nmacro Dup(x, y)\n    x * y\nend\nprint($Dup(3))\n",
        ];

        for src in sources {
            let got = messages(src);
            let message = got
                .iter()
                .find(|m| m.contains("one name holds one declaration"))
                .unwrap_or_else(|| panic!("{got:?}"));

            assert_eq!(crate::docs::kind_for(message), "DuplicateError", "{src}");
            assert_eq!(crate::docs::code_for(message), Some("6.1"), "{src}");
        }
    }

    /// `{ T }` is Luau's array form, so a bounded `{ T }` parameter
    /// reads its elements back as `(T & Bound)`. Without that the body
    /// calls a method the checker cannot find on `T`.
    #[test]
    fn a_bounded_brace_array_carries_the_bound_into_the_body() {
        let src = "trait Ord as\n    function compare(self, other: Ord): number\nend\n\nfunction largest<T: Ord>(xs: { T }): T\n    local best = xs[1]\n    if xs[2]:compare(best) > 0 then\n        best = xs[2]\n    end\n    return best\nend\nprint(largest)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("(xs[2] :: (T & Ord)):compare(best)"),
            "{}",
            out.check
        );
    }

    /// A bound asks the argument for the trait's methods. A struct this
    /// file declares with no `impl` of the trait never has them, and the
    /// erased bound left the call unchecked.
    #[test]
    fn a_call_that_breaks_a_bound_names_the_trait() {
        let head = "trait Ord as\n    function compare(self, other: Ord): number\nend\n\nfunction largest<T: Ord>(xs: { T }): T\n    return xs[1]\nend\n\nstruct NotOrd as\n    v: number\nend\n\n";
        let src = format!(
            "{head}local xs: {{ NotOrd }} = {{ new NotOrd {{ v = 1 }} }}\nlocal top = largest(xs)\nprint(top)\n"
        );
        assert_eq!(
            messages(&src),
            vec!["`NotOrd` does not implement `Ord`; `largest` asks for it"]
        );
        assert_eq!(
            crate::docs::kind_for(&messages(&src)[0]),
            "BoundError",
            "{:?}",
            messages(&src)
        );

        // The same struct with the impl passes, and so does a bound the
        // std owns, which a shape meets without an `impl`.
        let good = format!(
            "{head}impl Ord for NotOrd as\n    function compare(self, other: Ord): number\n        return 0\n    end\nend\n\nlocal xs: {{ NotOrd }} = {{ new NotOrd {{ v = 1 }} }}\nlocal top = largest(xs)\nprint(top)\n"
        );
        assert_eq!(messages(&good), Vec::<String>::new());
    }

    /// `local x = new S { }` names its struct as exactly as an
    /// annotation does, and that is the form a call hands a bounded
    /// parameter. The scan read annotations only, so the argument
    /// carried no type and every such call went unchecked.
    #[test]
    fn a_bound_reads_the_struct_an_unannotated_local_constructs() {
        let head = "trait Alpha as\n    function a(self): number\nend\n\ntrait Beta as\n    function b(self): number\nend\n\nstruct OnlyAlpha as\n    v: number\nend\n\nimpl Alpha for OnlyAlpha as\n    function a(self): number\n        return self.v\n    end\nend\n\nfunction sum_both<T: Alpha & Beta>(x: T): number\n    return x:a() + x:b()\nend\n\n";
        let src = format!("{head}local only = new OnlyAlpha {{ v = 2 }}\nprint(sum_both(only))\n");

        // The second trait of the `&` bound is the unmet one, and the
        // argument reports it once.
        assert_eq!(
            messages(&src),
            vec!["`OnlyAlpha` does not implement `Beta`; `sum_both` asks for it"]
        );

        // A struct with neither impl reports the first unmet trait only.
        let neither = format!(
            "{head}struct Plain as\n    v: number\nend\n\nlocal p = new Plain {{ v = 1 }}\nprint(sum_both(p))\n"
        );
        assert_eq!(
            messages(&neither),
            vec!["`Plain` does not implement `Alpha`; `sum_both` asks for it"]
        );

        // Both impls present: the call passes and the body types the
        // parameter as `T`, which the bound widens.
        let both = format!(
            "{head}struct Both as\n    v: number\nend\n\nimpl Alpha for Both as\n    function a(self): number\n        return self.v\n    end\nend\n\nimpl Beta for Both as\n    function b(self): number\n        return self.v\n    end\nend\n\nlocal b = new Both {{ v = 1 }}\nprint(sum_both(b))\n"
        );
        assert_eq!(messages(&both), Vec::<String>::new());
    }

    /// A bound on a method's own generic asks the same of its argument
    /// as a bound on a free function. Both call forms read it: `x:m(...)`
    /// where the receiver fills `self`, and `T.m(self, ...)` where the
    /// source writes `self` out.
    #[test]
    fn a_bound_on_a_method_of_an_impl_reads_at_the_call() {
        let head = "trait Alpha as\n    function a(self): number\nend\n\ntrait Beta as\n    function b(self): number\nend\n\nstruct OnlyAlpha as\n    v: number\nend\n\nimpl Alpha for OnlyAlpha as\n    function a(self): number\n        return self.v\n    end\nend\n\nstruct Holder as\n    v: number\nend\n\nimpl Holder as\n    function sum_both<T: Alpha & Beta>(self, x: T): number\n        return x:a() + x:b()\n    end\nend\n\nlocal h = new Holder { v = 0 }\n";
        let colon = format!("{head}print(h:sum_both(new OnlyAlpha {{ v = 2 }}))\n");

        assert_eq!(
            messages(&colon),
            vec!["`OnlyAlpha` does not implement `Beta`; `sum_both` asks for it"]
        );

        // `Holder.sum_both(h, x)` writes `self` out, so the argument
        // sits one place further along.
        let dot = format!("{head}print(Holder.sum_both(h, new OnlyAlpha {{ v = 2 }}))\n");
        assert_eq!(
            messages(&dot),
            vec!["`OnlyAlpha` does not implement `Beta`; `sum_both` asks for it"]
        );

        // A struct with both impls passes, and the receiver itself is no
        // argument of the bound.
        let both = format!(
            "{head}struct Two as\n    v: number\nend\n\nimpl Alpha for Two as\n    function a(self): number\n        return self.v\n    end\nend\n\nimpl Beta for Two as\n    function b(self): number\n        return self.v\n    end\nend\n\nprint(h:sum_both(new Two {{ v = 1 }}))\n"
        );
        assert_eq!(messages(&both), Vec::<String>::new());
    }

    /// A generic enum's unit variant is a string too. The annotation
    /// names the enum with its arguments, `Opt<number>`.
    #[test]
    fn a_colon_call_on_a_generic_enum_with_a_unit_variant_names_the_static_form() {
        let src = "enum Opt<T> as\n    Some(T)\n    None\nend\n\nimpl Opt<T> as\n    function unwrap_or(self, d: T): T\n        return d\n    end\nend\n\nlocal b: Opt<number> = Opt.None\nprint(b:unwrap_or(9))\n";

        assert_eq!(
            messages(src),
            vec!["`Opt.None` is a unit variant, a string at runtime; call `Opt.unwrap_or(b)`"]
        );
    }

    /// The check knew an enum and its methods only from this file. An
    /// imported enum, or a name a match arm binds to a payload slot,
    /// passed `check`, and `flux` gave the checker's "Key 'label' is
    /// missing from 'string'" alone.
    #[test]
    fn a_colon_call_on_an_imported_or_arm_bound_mixed_enum_names_the_static_form() {
        let want = "`Item.Junk` is a unit variant, a string at runtime; call `Item.label(item)`";
        let imported = crate::compile_with(
            "import { Item } from \"./items\"\n\nlocal function f(item: Item): string\n    return item:label()\nend\nprint(f)\n",
            &crate::EmitOptions {
                import_enums: vec![(
                    "Item".to_string(),
                    vec![("Tool".to_string(), 1), ("Junk".to_string(), 0)],
                )],
                import_callables: vec![(
                    "Item:label".to_string(),
                    crate::flux::Callable {
                        params: Some(1),
                        deprecated: None,
                        exported: true,
                    },
                )],
                ..crate::EmitOptions::default()
            },
        )
        .unwrap();
        let got: Vec<&str> = imported
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(got, [want]);

        let bound = "enum Item as\n    Tool(number)\n    Junk\nend\n\nimpl Item as\n    function label(self): string\n        return \"x\"\n    end\nend\n\nenum Why as\n    Lost(Item)\n    Full\nend\n\nlocal function g(w: Why): string\n    return match w with\n        case Lost(item) then item:label()\n        case Full then \"full\"\n    end\nend\nprint(g)\n";
        assert_eq!(messages(bound), [want]);
    }

    /// `x == Item.Tool("a", 1)` compared a fresh table by identity and
    /// was never true, with no word. A type that derives `Eq`, a unit
    /// variant, and a type the file cannot see stay quiet.
    #[test]
    fn an_equality_with_a_new_value_of_a_type_without_eq_warns() {
        let src = "enum Loose\n    Tool(string, number)\n    Junk\nend\n@derive(Eq)\nenum Tight\n    Tool(string, number)\nend\nstruct Point\n    x: number\nend\n@derive(PartialEq)\nstruct Same\n    x: number\nend\nlocal function f(x: Loose, t: Tight, p: Point, s: Same, o: any)\n    print(x == Loose.Tool(\"a\", 1))\n    print(new Point { x = 1 } ~= p)\n    print(x == Loose.Junk)\n    print(t == Tight.Tool(\"a\", 1))\n    print(s == new Same { x = 1 })\n    print(o == Other.Tool(1))\nend\nprint(f)\n";
        let out = crate::compile(src).unwrap();
        let got: Vec<&str> = out
            .lints
            .iter()
            .filter(|l| l.name == "identity_compare")
            .map(|l| l.message.as_str())
            .collect();

        assert_eq!(
            got,
            [
                "this `==` compares identity, and a value built here equals no other; `@derive(Eq)` on `Loose` compares the payload",
                "this `~=` compares identity, and a value built here differs from every other; `@derive(Eq)` on `Point` compares the fields",
            ]
        );
    }

    /// A unit enum is a string at runtime, so `s:describe()` finds no
    /// method. The report names the static form, which the impl writes
    /// and the check artifact types. A payload enum is a table with a
    /// metatable, so its `:` call stays.
    #[test]
    fn a_colon_call_of_a_unit_enum_method_names_the_static_form() {
        let head = "enum Status as\n    Ready\n    Pending\nend\n\nimpl Status as\n    function describe(self): string\n        return \"status\"\n    end\nend\n\n";
        let want = vec!["`Status` is a unit enum, a string at runtime; call `Status.describe(s)`"];

        let param = format!(
            "{head}function classify(s: Status): string\n    return s:describe()\nend\nprint(classify(Status.Ready))\n"
        );
        assert_eq!(messages(&param), want);

        let local = format!(
            "{head}function classify(): string\n    local s: Status = Status.Ready\n    return s:describe()\nend\nprint(classify())\n"
        );
        assert_eq!(messages(&local), want);

        let top = format!("{head}local s: Status = Status.Ready\nprint(s:describe())\n");
        assert_eq!(messages(&top), want);

        // The static form is the one that runs.
        let dot = format!(
            "{head}function classify(s: Status): string\n    return Status.describe(s)\nend\nprint(classify(Status.Ready))\n"
        );
        assert_eq!(messages(&dot), Vec::<String>::new());

        // A string method is the string's own.
        let upper = format!(
            "{head}function classify(s: Status): string\n    return s:upper()\nend\nprint(classify(Status.Ready))\n"
        );
        assert_eq!(messages(&upper), Vec::<String>::new());

        let payload = "enum Shape as\n    Circle(number)\n    Square(number)\nend\n\nimpl Shape as\n    function area(self): number\n        return 1\n    end\nend\n\nfunction measure(s: Shape): number\n    return s:area()\nend\nprint(measure(Shape.Circle(2)))\n";
        assert_eq!(messages(payload), Vec::<String>::new());
    }

    /// The four shapes a `destroy` lowers to: the plain call, Debris for
    /// an Instance, a timer for a value with the method, and the runtime
    /// helper when the file says neither.
    #[test]
    fn destroy_picks_its_mechanism_from_the_file() {
        let out = crate::compile("local part = Instance.new(\"Part\")\ndestroy part\n").unwrap();
        assert!(out.ship.contains("__alloy.destroy(part)"), "{}", out.ship);

        let out =
            crate::compile("local part: Part = workspace.Box\ndestroy part after 3\n").unwrap();
        assert!(
            out.ship
                .contains("game:GetService(\"Debris\"):AddItem(part, 3)"),
            "{}",
            out.ship
        );

        let src = "struct T as\n    n: number\nend\n\nimpl T as\n    function Destroy(self)\n        self.n = 0\n    end\nend\n\nlocal t = new T { n = 1 }\ndestroy t after 2\nprint(t)\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.ship
                .contains("task.delay(2, function() t:Destroy() end)"),
            "{}",
            out.ship
        );

        let out =
            crate::compile("function drop(x)\n    destroy x after 4\nend\nprint(drop)\n").unwrap();
        assert!(
            out.ship.contains("__alloy.destroy_after(x, 4)"),
            "{}",
            out.ship
        );
    }

    /// `destroy t.field` calls the method and leaves the slot alone;
    /// `delete t.field` is the one that empties it.
    #[test]
    fn destroy_leaves_the_slot_it_read() {
        let out = crate::compile("local t = { part = nil }\ndestroy t.part\nprint(t)\n").unwrap();
        assert!(out.ship.contains("__alloy.destroy(t.part)"), "{}", out.ship);
        assert!(!out.ship.contains("t.part = nil\n"), "{}", out.ship);
    }

    /// `after n do ... end` is a `task.delay` over the block, and the
    /// `where` condition sits inside the function, so it reads when the
    /// timer fires.
    #[test]
    fn after_wraps_its_block_in_a_timer() {
        let src = "after 2 do\n    print(1)\nend\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.ship.contains("task.delay(2, function()"),
            "{}",
            out.ship
        );
        assert!(out.ship.trim_end().ends_with("end)"), "{}", out.ship);
        assert_eq!(out.ship.lines().count(), src.lines().count());

        let src = "local ready = false\nafter 0 where ready do\n    print(1)\nend\nprint(ready)\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.ship.contains("task.delay(0, function() if ready then"),
            "{}",
            out.ship
        );
        assert!(out.ship.contains("end end)"), "{}", out.ship);
        assert_eq!(out.ship.lines().count(), src.lines().count());
    }

    /// A client file lowers the block the same way; the timer is not a
    /// server thing.
    #[test]
    fn after_lowers_in_a_client_file() {
        let options = crate::EmitOptions {
            file_name: "ui.client.aly".to_string(),
            ..crate::EmitOptions::default()
        };
        let out = crate::compile_with("after 1 do\n    print(1)\nend\n", &options).unwrap();
        assert!(
            out.ship.contains("task.delay(1, function()"),
            "{}",
            out.ship
        );
    }

    /// `after` is contextual: the keyword takes a delay and a `do`, and
    /// every other shape is the local a file may name after.
    #[test]
    fn after_is_contextual() {
        for src in [
            "local after = 1\nprint(after)\n",
            "local t = {}\nt.after = 1\nprint(t.after)\n",
            "local after = {}\nafter[1] = 2\nprint(after)\n",
            "local after = function(n) return n end\nprint(after(1))\n",
            "local after = 1\nafter += 1\nprint(after)\n",
        ] {
            assert!(messages(src).is_empty(), "{src:?}: {:?}", messages(src));
        }

        // The statement still lowers, with and without the filter.
        let out = crate::compile("after 2 do\n    print(1)\nend\n").unwrap();
        assert!(out.ship.contains("task.delay(2"), "{}", out.ship);

        let out = crate::compile("local ready = true\nafter 3 where ready do\n    print(1)\nend\n")
            .unwrap();
        assert!(out.ship.contains("task.delay(3"), "{}", out.ship);

        // A parenthesized delay keeps the keyword, because the `do` closes it.
        let out = crate::compile("local n = 1\nafter (n + 1) do\n    print(1)\nend\n").unwrap();
        assert!(out.ship.contains("task.delay("), "{}", out.ship);
    }

    #[test]
    fn a_const_without_a_value_is_an_error() {
        let got = messages("const ZEB\nprint(ZEB)\n");
        assert!(got.iter().any(|m| m == "`const` needs a value"), "{got:?}");
        assert!(messages("const ZEB = 1\nprint(ZEB)\n").is_empty());
        // `local` without a value stays legal.
        assert!(messages("local a\nprint(a)\n").is_empty());
    }

    #[test]
    fn a_coalesce_assign_to_a_name_narrows_it() {
        // `if x == nil then x = 1 end` leaves `x` a `number?` for the
        // checker; the `if` expression is what narrows.
        let out = crate::compile(
            "local function f(x: number?): number\n    x ??= 1\n    return x\nend\nprint(f)\n",
        )
        .unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("x = if x == nil then 1 else x"),
            "{}",
            out.check
        );
    }

    /// `async do ... end` stands alone as a statement: the block starts
    /// on a thread of its own and the Future is dropped. It lowers the
    /// same way in every position, and `end)` keeps the line count.
    #[test]
    fn an_async_block_stands_alone_as_a_statement() {
        let src = "async do\n    print(1)\nend\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("__alloy.future(function()"),
            "{}",
            out.ship
        );
        assert!(out.ship.trim_end().ends_with("end)"), "{}", out.ship);
        assert_eq!(out.ship.lines().count(), src.lines().count());

        // Inside a plain function body, which is where the placement
        // rule sends an author who wrote a bare `await`.
        let src = "local function f()\n    async do\n        print(1)\n    end\nend\nprint(f)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("__alloy.future(function()"),
            "{}",
            out.ship
        );
        assert_eq!(out.ship.lines().count(), src.lines().count());
    }

    /// An async function that returns two values settles a Future of
    /// both. `(A, B)` in the header wrote `Future<(A, B)>`, and the
    /// checker read a pack where a type argument goes.
    #[test]
    fn an_async_return_pack_names_each_value() {
        let src = "async function f(): (number, string)\n    return 1, \"x\"\nend\nasync function g(): Future<number, string>\n    return 1, \"x\"\nend\nlocal h: Future<number, string> = async do\n    return 1, \"x\"\nend\nprint(f, g, h)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        for line in [
            "local function f(): __alloy.Future<number, string> return __alloy.future(function(): (number, string)",
            "local function g(): __alloy.Future<number, string> return __alloy.future(function(): (number, string)",
            "local h: __alloy.Future<number, string> = __alloy.future(function(): (number, string)",
        ] {
            assert!(out.check.contains(line), "{line}\n{}", out.check);
        }
    }

    /// The expression form still binds: the value is a Future.
    #[test]
    fn an_async_block_still_reads_as_an_expression() {
        let out =
            crate::compile("local held = async do\n    return 1\nend\nprint(held)\n").unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("local held = __alloy.future(function()"),
            "{}",
            out.ship
        );
    }

    /// `local f: Future<T> = async do ... end` gives the block's
    /// closure `: T`. The checker read the closure's own result before,
    /// and an open result landed on `Future<unknown>`.
    #[test]
    fn an_annotated_async_block_carries_the_payload_type() {
        let src = "async function slow(tag: string): string\n    return tag\nend\n\nasync function run()\n    local first: Future<string> = async do\n        return await slow(\"first\")\n    end\n    local loose = async do\n        return await slow(\"loose\")\n    end\n    local tried: Future<number> = async do\n        local r: Result<number, string> = try do\n            return 1\n        end\n        return r:unwrap_or(0)\n    end\n    print(await first, await loose, await tried)\nend\n\nrun()\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains(
                "local first: __alloy.Future<string> = __alloy.future(function(): string"
            ),
            "{}",
            out.ship
        );
        // No annotation, so the inference stands.
        assert!(
            out.ship
                .contains("local loose = __alloy.future(function()\n"),
            "{}",
            out.ship
        );
        // The hint belongs to the block the binding names; the `try do`
        // inside it takes none.
        assert!(
            out.ship.contains(
                "local tried: __alloy.Future<number> = __alloy.future(function(): number"
            ),
            "{}",
            out.ship
        );
        assert!(
            out.ship.contains("__alloy.try_block(function(__fail)\n"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_coalesce_assign_to_a_field_keeps_the_statement() {
        // A field takes the statement: the expression would write the
        // field back through `__newindex` when it is not nil.
        let out = crate::compile("local t = { a = 1 }\nt.a ??= 2\nprint(t)\n").unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("if t.a == nil then t.a = 2 end"),
            "{}",
            out.check
        );
    }
}

#[cfg(test)]
mod future_header_tests {
    use super::{names_a_future, settled_type};

    #[test]
    fn a_header_that_names_a_future_is_seen() {
        assert!(names_a_future("Future<nil>"));
        assert!(names_a_future("Future<number>"));
        assert!(names_a_future(" alloy.Future<T> "));
    }

    #[test]
    fn another_type_is_not_a_future() {
        assert!(!names_a_future("nil"));
        assert!(!names_a_future("number"));
        // A type whose name merely starts the same way.
        assert!(!names_a_future("FutureQueue<T>"));
        assert!(!names_a_future("Future"));
    }

    #[test]
    fn the_settled_type_is_what_the_body_returns() {
        assert_eq!(settled_type("Future<nil>").as_deref(), Some("nil"));
        assert_eq!(settled_type("Future<number>").as_deref(), Some("number"));
        assert_eq!(settled_type("nil"), None);
    }
}

/// The name a top-level declaration owns, with the word the report names
/// it by. A `local` or a `function` shadows the Luau way, so neither is
/// a duplicate here.
fn declared_kind(stmt: &Stmt) -> Option<(TokSpan, &'static str)> {
    match stmt.under_default() {
        Stmt::Struct(d) => Some((d.name, "a struct")),
        Stmt::Enum(d) => Some((d.name, "an enum")),
        Stmt::Trait(d) => Some((d.name, "a trait")),
        Stmt::Interface(d) => Some((d.name, "an interface")),
        Stmt::Namespace(d) => Some((d.name, "a namespace")),
        Stmt::Attribute(d) => Some((d.name, "an attribute")),

        // A remote registers one channel under its name, and a macro
        // binds one template. A second of either wins in silence.
        Stmt::Remote(d) => Some((d.name, "a remote")),
        Stmt::Macro(d) => Some((d.name, "a macro")),

        _ => None,
    }
}

/// The names a top-level function or local binds. `function M.f()`
/// names a field of `M`, and a destructure binds through its fields.
fn bound_names(stmt: &Stmt) -> Vec<TokSpan> {
    match stmt.under_default() {
        Stmt::Function(f) if f.path.len() == 1 => vec![f.path[0]],
        Stmt::LocalFunction(f) => vec![f.name],
        Stmt::Local(l) => local_names(l),

        _ => Vec::new(),
    }
}

/// The names a `local` binds, a pattern's through its fields.
pub fn local_names(l: &Local) -> Vec<TokSpan> {
    l.names
        .iter()
        .flat_map(|b| match &b.destructure {
            None => vec![b.name],

            Some(d) => destructure_names(d),
        })
        .collect()
}

/// The names a pattern binds: the rename when there is one, else the
/// field; every array item, and the rest.
pub(crate) fn destructure_names(d: &Destructure) -> Vec<TokSpan> {
    match d {
        Destructure::Table(fields) => fields.iter().map(|f| f.rename.unwrap_or(f.field)).collect(),

        Destructure::Array { items, rest } => items.iter().copied().chain(*rest).collect(),
    }
}

/// The type a parameter pattern stands for, from its source text: the
/// annotation after `}`, or the shape its typed fields state.
/// `{ x, y }: Point` is `Point`; `{ bar: string }` is `{ bar: string }`.
/// `None` when the text is no pattern, or a pattern with no type.
pub fn pattern_type(param: &str) -> Option<String> {
    let t = param.trim();

    if !t.starts_with('{') {
        return None;
    }

    let mut depth = 0i32;
    let end = t.char_indices().find_map(|(i, c)| {
        depth += depth_step(t, i, c);

        (depth == 0 && c == '}').then_some(i)
    })?;

    if let Some(ty) = t[end + 1..].trim().strip_prefix(':') {
        // The annotation ends at the default or the next parameter.
        let mut depth = 0i32;
        let stop = ty
            .char_indices()
            .find(|&(i, c)| {
                depth += depth_step(ty, i, c);

                depth < 0 || (matches!(c, ',' | '=') && depth == 0)
            })
            .map_or(ty.len(), |(n, _)| n);
        let ty = ty[..stop].trim();

        return (!ty.is_empty()).then(|| ty.to_string());
    }

    let fields: Vec<String> = split_top_level(&t[1..end], ',')
        .into_iter()
        .map(str::trim)
        .filter(|f| !f.is_empty() && !f.starts_with("..."))
        .map(|f| {
            let (name, ty) = f.split_once(':')?;
            let name = name.split('=').next().unwrap_or(name).trim();

            Some(format!("{name}: {}", ty.trim()))
        })
        .collect::<Option<_>>()?;

    Some(format!("{{ {} }}", fields.join(", ")))
}

/// A signature with each parameter pattern written as its type: the
/// caller passes one value, and the pattern is the body's business.
pub fn signature_with_pattern_types(label: &str) -> Option<String> {
    let open = label.find('(')?;
    let mut depth = 0i32;
    let close = label
        .char_indices()
        .skip_while(|(i, _)| *i < open)
        .find_map(|(i, c)| {
            depth += depth_step(label, i, c);

            (depth == 0 && c == ')').then_some(i)
        })?;
    let params = split_top_level(&label[open + 1..close], ',');

    if !params.iter().any(|p| p.trim_start().starts_with('{')) {
        return None;
    }

    let written: Vec<String> = params
        .iter()
        .map(|p| match pattern_type(p) {
            Some(ty) => format!("{}{ty}", &p[..p.len() - p.trim_start().len()]),

            None => p.to_string(),
        })
        .collect();

    Some(format!(
        "{}{}{}",
        &label[..=open],
        written.join(","),
        &label[close..]
    ))
}
