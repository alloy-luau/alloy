//! Statement lowering: blocks, assignment, locals, functions, and loops.

use std::collections::HashSet;

use alloy_syntax::ast::{
    After, Assign, Block, CallArgs, Cond, Destructure, Expr, Function, FunctionBody, GenericFor,
    ImportKind, IndexKey, Local, Pattern, Return, Stmt, TableField, TokSpan,
};

use crate::render::Renderer;

use super::expressions::WORD_OPS;
use super::types::{apply_bounds, array_element, generic_bounds, generic_head, strip_bounds};
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
fn names_a_future(declared: &str) -> bool {
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
    let inner = t[open + 1..].strip_suffix('>')?;

    Some(inner.trim().to_string())
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
            let fenced = is_early_exit(stmt) && has_live_stmt(&block.stmts[i + 1..]);

            if fenced {
                self.generate(start, "do ");
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

    pub(crate) fn stmt(&mut self, stmt: &Stmt) {
        // A hoisted global lives in the module beside the script; its
        // lines here go blank, and the injected require binds the name.
        if self.options.hoist_globals && crate::globals::is_global(stmt) {
            let span = stmt.span();
            self.blank_lines(self.byte_start(span), self.byte_end(span));

            return;
        }

        // Declarations come first so a later statement sees them.
        match stmt {
            Stmt::Local(l) => {
                for b in &l.names {
                    self.declare_binding(b);
                }
            }

            Stmt::LocalFunction(f) => self.declare_name(f.name),

            Stmt::Function(f) if f.path.len() == 1 && f.exported => self.declare_name(f.path[0]),

            Stmt::Enum(e) => {
                self.declare_name(e.name);
                let variants = e
                    .variants
                    .iter()
                    .map(|v| (self.text_of(v.name).to_string(), v.payload.len()))
                    .collect();
                self.enums.insert(self.decl_name(e.name), variants);
            }

            Stmt::Import(i) => match &i.kind {
                ImportKind::Namespace(n) | ImportKind::Default(n) => self.declare_name(*n),

                ImportKind::Both(n, specs) => {
                    self.declare_name(*n);

                    for sp in specs {
                        self.declare_name(sp.alias.unwrap_or(sp.name));
                    }
                }

                ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                    for sp in specs {
                        self.declare_name(sp.alias.unwrap_or(sp.name));
                    }
                }
            },

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

        // Render the statement into a side buffer first, so the hoists it
        // asks for can go in front of it on the same line.
        let saved_hoists = std::mem::take(&mut self.hoists);
        let saved_next = self.temp_next;
        self.temp_next = 0;

        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        self.stmt_inner(stmt);
        std::mem::swap(&mut self.r, &mut side);

        let hoists = std::mem::replace(&mut self.hoists, saved_hoists);
        self.temp_next = saved_next;

        for h in hoists {
            match h {
                Hoist::Temp {
                    index,
                    value,
                    anchor,
                } => {
                    let keyword = if self.temp_declared(index) {
                        ""
                    } else {
                        self.declare_temp(index);

                        "local "
                    };

                    match value {
                        HoistValue::Text(text) => {
                            let line = format!("{keyword}_{index} = {text} ");
                            self.generate(anchor, &line);
                        }

                        // The value keeps its source chunks, so a function
                        // literal inside it keeps its lines.
                        HoistValue::Rendered(rendered) => {
                            self.generate(anchor, &format!("{keyword}_{index} = "));
                            self.r.append(rendered);
                            self.generate(anchor, " ");
                        }
                    }
                }

                Hoist::Stmt { text, anchor } => {
                    let line = format!("{text} ");
                    self.generate(anchor, &line);
                }

                Hoist::Fresh {
                    name,
                    value,
                    anchor,
                } => {
                    let line = format!("local {name} = {value} ");
                    self.generate(anchor, &line);
                }
            }
        }

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

        // An `export type { T }` list below the alias adds the word.
        if let Stmt::TypeAlias(t) = s
            && self.export_listed_types.contains(self.text_of(t.name))
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

            _ if self.options.check && self.holds_coalesce_if(s) => return true,

            _ if !self.elem_bounds.is_empty() && self.holds_element_index(s) => return true,

            _ => {}
        }

        AMBIENT.iter().any(|n| text.contains(n))
            // A project global reaches this file without an import, so
            // the walk has to see the name and record the use.
            || self.options.globals.iter().any(|g| text.contains(&g.name))
            || text.contains("import(")
            || text.contains("import<<")
            || self.structs.iter().any(|name| struct_called(text, name))
            || self.structs.iter().any(|name| struct_braced(text, name))
            || WORD_OPS.iter().any(|w| text.contains(w))
            || self.ext_methods.iter().any(|m| text.contains(m.as_str()))
            || self
                .ext_statics
                .values()
                .flatten()
                .any(|m| text.contains(m.as_str()))
    }

    /// A bound `<T: Shape>` names a trait, and the emit erases the
    /// bound, so nothing else reports a name that is nowhere.
    pub(crate) fn check_bounds(&mut self, generics: Option<TokSpan>) {
        let Some(g) = generics else {
            return;
        };
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

    /// The token inside `span` whose text is `name`.
    fn token_named(&self, span: TokSpan, name: &str) -> Option<TokSpan> {
        (span.start..span.end)
            .map(|i| TokSpan::new(i as usize, i as usize + 1))
            .find(|one| self.text_of(*one) == name)
    }

    pub(crate) fn stmt_inner(&mut self, stmt: &Stmt) {
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
                self.exports.push((name.clone(), name));
                // `export function f` becomes `local function f`.
                let fn_tok = self.toks[stmt.span().start as usize + 1];
                self.generate(anchor, "local ");
                let rest = TokSpan::new(stmt.span().start as usize + 1, stmt.span().end as usize);
                let _ = fn_tok;

                if function_needs_rewrite(&f.body) {
                    self.function_with_header(rest, &f.body);
                } else {
                    let children = function_children(&f.body);
                    self.stitch(rest, &children, |d, child| match child {
                        Child::Expr(e) => d.expr(e),

                        Child::Block(b) => d.block(b),

                        Child::Function(b) => d.function_block(b),
                    });
                }
            }

            Stmt::LocalFunction(f) if f.exported => {
                let name = self.text_of(f.name).to_string();
                self.exports.push((name.clone(), name));
                let rest = TokSpan::new(stmt.span().start as usize + 1, stmt.span().end as usize);

                if function_needs_rewrite(&f.body) {
                    self.function_with_header(rest, &f.body);
                } else {
                    let children = function_children(&f.body);
                    self.stitch(rest, &children, |d, child| match child {
                        Child::Expr(e) => d.expr(e),

                        Child::Block(b) => d.block(b),

                        Child::Function(b) => d.function_block(b),
                    });
                }
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

            Stmt::Local(l) if local_needs_rewrite(l) => self.local_stmt(l),

            // `return Err(e)` inside a `try do` block: the block fails
            // with `e`, which is what the block's `fail` says. A
            // `return` of it would decide the closure's value type
            // instead, and Luau then reports every other `return`.
            Stmt::Return(r) if self.block_err_return(r).is_some() => {
                self.block_err_return_stmt(r);
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

                if t.global {
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
                if !t.exported
                    && !t.global
                    && self.export_listed_types.contains(self.text_of(t.name)) =>
            {
                let start = self.byte_start(t.span);
                self.generate(start, "export ");
                self.copy(start, self.byte_end(t.span));
            }

            Stmt::TypeAlias(t) if t.global => {
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

            Stmt::GenericFor(f) if for_needs_rewrite(f) => self.generic_for(stmt.span(), f),

            _ => {
                let span = stmt.span();
                let children = stmt_children(stmt);
                let reevaluated = reevaluated_conditions(stmt);
                let (narrow_blocks, narrow_after) = match stmt {
                    Stmt::If(i) => self.narrowings(i),

                    _ => (Vec::new(), None),
                };
                self.stitch(span, &children, |d, child| match child {
                    Child::Expr(e) => {
                        let guard = reevaluated.contains(&std::ptr::from_ref::<Expr>(e));

                        if guard {
                            d.no_hoist += 1;
                        }

                        d.expr(e);

                        if guard {
                            d.no_hoist -= 1;
                        }
                    }

                    Child::Block(b) => {
                        if let Some((_, prefix)) =
                            narrow_blocks.iter().find(|(at, _)| *at == b.span.start)
                        {
                            let anchor = d.byte_start(b.span);
                            d.generate(anchor, prefix);
                        }

                        d.block(b);
                    }

                    Child::Function(b) => d.function_block(b),
                });

                if let Some(text) = narrow_after {
                    let anchor = self.byte_end(span);
                    self.generate(anchor, &text);
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

        matches!(base, Expr::Name(n) if self.text_of(*n) == "import")
            && matches!(
                links.first(),
                Some(Link::Plain(Step::Call { method: None, .. }))
            )
    }

    /// `Name(...)` with a struct's name and no fields table, or the fields
    /// form on a struct that writes `new`, outside its own impl.
    pub(crate) fn is_struct_call(&self, e: &Expr) -> bool {
        let (base, links) = flatten(e);
        let Expr::Name(n) = base else {
            return false;
        };
        let name = self.text_of(*n);

        if !self.structs.contains(name) {
            return false;
        }

        matches!(
            links.first(),
            Some(Link::Plain(Step::Call { method: None, .. }))
        )
    }

    /// A chain that is a call statement: the guard becomes an `if`.
    pub(crate) fn chain_stmt(&mut self, e: &Expr) -> String {
        self.chain_anchor = self.byte_start(e.span());
        self.check_struct_call(e);
        let parts = self.chain_parts(e);

        match parts.guard {
            Some(g) => format!("if {g} ~= nil then {} end", parts.inner),

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
        let value = self.render_to_string(&a.values[0]);
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

        let text = match guard {
            Some(g) => format!("if {g} ~= nil then {body} end"),

            None => body,
        };

        self.generate(anchor, &text);
    }

    /// The guard and the assignable text of a target. With `twice`, the
    /// object and key of the last link become names safe to read twice.
    pub(crate) fn target_parts(&mut self, target: &Expr, twice: bool) -> (Option<String>, String) {
        let Expr::Index { object, key, .. } = target else {
            // A plain name, or something the parser let through as a target.
            return (None, self.render_to_string(target));
        };

        let parts = self.chain_parts(object);
        let mut guard = parts.guard;
        let mut obj = parts.inner;

        let optional = matches!(target, Expr::Index { optional: true, .. });
        let simple = guard.is_none() && self.is_simple(object);

        if optional {
            let name = self.name_prefix(&mut obj, &mut guard, simple);
            obj = name;
        } else if twice && !(guard.is_none() && self.is_simple(object)) {
            let whole = self.guarded(guard.as_deref(), &obj);
            let anchor = self.chain_anchor;
            obj = self.hoist_text(whole, anchor);
            guard = None;
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
        let mut names = Vec::new();
        let mut values = Vec::new();
        let mut decls: Vec<String> = Vec::new();

        for (b, v) in l.names.iter().zip(&l.values) {
            self.expected_generic = b.ty.and_then(|t| generic_head(self.text_of(t)));
            let value = self.render_to_string(v);
            self.expected_generic = None;
            let ty =
                b.ty.map(|t| format!(": {}", self.text_of(t)))
                    .unwrap_or_default();

            match &b.destructure {
                None => {
                    names.push(format!("{}{ty}", self.text_of(b.name)));
                    values.push(value);
                }

                Some(d) => {
                    let typed = match b.ty {
                        Some(t) => format!("{value} :: {}", self.text_of(t)),

                        None => value,
                    };
                    let temp = self.hoist_text(typed, anchor);
                    let (ns, vs) = self.destructure_parts(d, &temp);
                    decls.push(format!("{keyword} {ns} = {vs}"));
                }
            }
        }

        let mut text = String::new();

        if !names.is_empty() {
            text.push_str(&format!(
                "{keyword} {} = {}",
                names.join(", "),
                values.join(", ")
            ));
        }

        for d in decls {
            if !text.is_empty() {
                text.push(' ');
            }

            text.push_str(&d);
        }

        self.generate(anchor, &text);
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
    pub(crate) fn copy_gap_without_commas(&mut self, start: u32, end: u32) {
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

    /// The names and values of a destructure over a temp.
    pub(crate) fn destructure_parts(&mut self, d: &Destructure, temp: &str) -> (String, String) {
        match d {
            Destructure::Table(fields) => {
                let ns: Vec<String> = fields
                    .iter()
                    .map(|f| self.text_of(f.rename.unwrap_or(f.field)).to_string())
                    .collect();
                let vs: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{temp}.{}", self.text_of(f.field)))
                    .collect();

                (ns.join(", "), vs.join(", "))
            }

            Destructure::Array { items, rest } => {
                let mut ns: Vec<String> =
                    items.iter().map(|i| self.text_of(*i).to_string()).collect();
                let mut vs: Vec<String> =
                    (1..=items.len()).map(|i| format!("{temp}[{i}]")).collect();

                if let Some(r) = rest {
                    ns.push(self.text_of(*r).to_string());
                    let std = self.std();
                    vs.push(format!("{std}.Array.slice({temp}, {})", items.len() + 1));
                }

                (ns.join(", "), vs.join(", "))
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
        let mut prologue: Vec<String> = Vec::new();
        let mut param_temp = 0;

        for p in &body.params {
            let ps = self.byte_start(p.name);
            self.copy(cursor, ps);

            match &p.destructure {
                Some(d) => {
                    param_temp += 1;
                    let temp = format!("_p{param_temp}");
                    self.generate(ps, &temp);
                    let (ns, vs) = self.destructure_parts(d, &temp);
                    prologue.push(format!("local {ns} = {vs}"));
                }

                None => self.copy(ps, self.byte_end(p.name)),
            }

            cursor = self.byte_end(p.name);

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
                let name = self.text_of(p.name).to_string();
                let value = self.render_to_string(default);
                let ty =
                    p.ty.map(|t| format!(": {}", self.text_of(t)))
                        .unwrap_or_default();
                prologue.push(format!(
                    "local {name}{ty} = if {name} == nil then {value} else {name}"
                ));
                // ` = default` disappears from the parameter list.
                cursor = self.byte_end(default.span());
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

        if let Some(p) = self.self_prologue.take() {
            lead.push(' ');
            lead.push_str(&p);
        }

        for p in &prologue {
            lead.push(' ');
            lead.push_str(p);
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
            self.bind_bounds_in(stmt_children(stmt), bounds);
        }
    }

    pub(crate) fn bind_bounds_in(&mut self, children: Vec<Child<'_>>, bounds: &[(String, String)]) {
        for child in children {
            match child {
                Child::Expr(e) => self.bind_bounds_in(expr_children(e), bounds),

                Child::Block(b) => self.bind_nested_bounds(b, bounds),

                Child::Function(f) => {
                    for p in &f.params {
                        let Some(t) = p.ty else {
                            continue;
                        };
                        let text = self.text_of(t).trim().to_string();

                        let Some((_, bound)) = bounds.iter().find(|(n, _)| *n == text) else {
                            continue;
                        };
                        let open = self.byte_start(t);
                        let close = self.byte_end(t);
                        self.inserts.push((open, "(".to_string()));
                        self.inserts.push((close, format!(" & {bound})")));
                    }

                    self.bind_nested_bounds(&f.block, bounds);
                }
            }
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
            let Some((name, bound)) = bounds.iter().find(|(n, _)| n == elem) else {
                continue;
            };
            out.push((
                self.text_of(p.name).to_string(),
                format!("({name} & {bound})"),
            ));
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
        let mut prologue: Vec<String> = Vec::new();
        let mut temp = 0;

        for v in &f.vars {
            let vs = self.byte_start(v.name);
            self.copy(cursor, vs);

            match &v.destructure {
                Some(d) => {
                    temp += 1;
                    let t = format!("_p{temp}");
                    self.generate(vs, &t);
                    let (ns, vals) = self.destructure_parts(d, &t);
                    prologue.push(format!("local {ns} = {vals}"));
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
        self.stitch_between(cursor, last_expr, &children);
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

                for p in &prologue {
                    self.generate(do_end, &format!(" {p}"));
                }

                self.generate(do_end, &format!(" if not ({cond}) then continue end"));
            }

            None => {
                self.copy(cursor, do_end);

                for p in &prologue {
                    self.generate(do_end, &format!(" {p}"));
                }
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

    pub(crate) fn find_tok_after(&self, from: u32, text: &str) -> u32 {
        let mut i = from;

        while self.toks[i as usize].text(self.src) != text {
            i += 1;
        }

        i
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

    /// `after` is reserved, so a local of that name reports.
    #[test]
    fn after_is_a_reserved_word() {
        let got = messages("local after = 1\nprint(after)\n");
        assert!(
            got.iter()
                .any(|m| m == "`after` is a reserved word and cannot be a name"),
            "{got:?}"
        );
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
