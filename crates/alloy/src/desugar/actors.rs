/*!
Parallel Luau: the `parallel do ... end` block, the `message` declaration
and its three calls, and the rules of the parallel phase.

A message is the engine's own API with the topic and the arguments filled
in. The ship artifact writes `actor:SendMessage("Step", ...)` and
`script:GetActor():BindToMessage("Step", handler)` where the source calls
`Step.fire` and `Step.on`. The check artifact keeps the calls as written
against a value typed from the declaration, so the analyzer types both
ends from one line.

The phase check reads this file and no further. A call into another module
is not followed. A call to a function or a struct method of this file is
followed at any depth. The engine has the last word.
*/

use alloy_syntax::ast::{
    CallArgs, Destructure, FunctionBody, GenericFor, Local, MessageDecl, ParallelBlock, Param,
};

use super::expressions::one_line;
use super::*;

/// Who a refusal of the parallel phase names, and what its fix says.
#[derive(Clone, Copy)]
enum Phase<'a> {
    /// A `parallel do ... end` block.
    Block,
    /// The handler of a `message ... as parallel`, by the message name.
    Handler(&'a str),
}

impl Phase<'_> {
    fn who(self) -> String {
        match self {
            Phase::Block => "a `parallel` block".to_string(),

            Phase::Handler(name) => format!("the parallel handler of `{name}`"),
        }
    }

    /// The fix for a statement the phase refuses: `what` is "write" or
    /// "call".
    fn fix(self, what: &str) -> String {
        match self {
            Phase::Block => format!("move the {what} after the block"),

            Phase::Handler(_) => format!("call `task.synchronize()` before the {what}"),
        }
    }
}

/// What the check of one block reads: the phase, and how many loops
/// stand between the statement and the block, so a `break` of an inner
/// loop stays inside it.
#[derive(Clone, Copy)]
struct Walk<'a> {
    phase: Phase<'a>,
    loops: usize,
}

/// The Roblox globals that hold an Instance.
const INSTANCE_GLOBALS: &[&str] = &["workspace", "game", "script"];

/// The Instance methods that make or remove an instance.
const INSTANCE_METHODS: &[&str] = &["Destroy", "Clone"];

/// The Instance methods that give back an Instance.
const FINDS_INSTANCE: &[&str] = &[
    "Clone",
    "FindFirstAncestor",
    "FindFirstAncestorOfClass",
    "FindFirstAncestorWhichIsA",
    "FindFirstChild",
    "FindFirstChildOfClass",
    "FindFirstChildWhichIsA",
    "FindFirstDescendant",
    "FindService",
    "GetService",
    "WaitForChild",
];

/// The Instance methods that give back a list of Instances.
const LISTS_INSTANCES: &[&str] = &["GetChildren", "GetDescendants"];

/// What the phase check knows about a value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    /// An Instance.
    Instance,
    /// A table whose values are Instances: `{ BasePart }` or
    /// `{ [string]: Part }`.
    Instances,
    /// A value of a struct this file declares, by the struct's name.
    Struct(String),
}

/// One scope of the check: each name it binds, with what the check knows
/// about the value. `None` still hides an outer name of the same spelling.
type Scope = HashMap<String, Option<Shape>>;

impl<'s> Desugar<'s> {
    /*
    The messages this file declares join the imported ones, and the
    names that could shadow one are noted. A file that holds a `parallel`
    word gets what it knows about each name, and the functions that write
    an instance, for the phase check.
    */
    pub(crate) fn note_messages(&mut self, block: &Block) {
        for stmt in &block.stmts {
            if let Stmt::Message(m) = stmt.under_default() {
                let sig = MessageSig::of(m, self.src, self.toks);
                self.messages.insert(sig.topic.clone(), sig);
            }
        }

        if !self.messages.is_empty() {
            let heads: HashSet<String> = self
                .messages
                .keys()
                .map(|k| k.split('.').next().unwrap_or(k).to_string())
                .collect();
            self.message_shadows = crate::naming::scoped_bindings(self.src, self.toks, block)
                .into_iter()
                .filter(|(name, _, _)| heads.contains(name))
                .collect();
        }

        if self.toks.iter().any(|t| t.text(self.src) == "parallel") {
            let mut shapes = Scope::new();
            self.collect_shapes(&block.stmts, &mut shapes);
            self.parallel_shapes = shapes;
            self.parallel_writers = self.writers_of(block);
        }
    }

    /*
    What the file knows about each name, for a name the check meets
    outside the scopes it walks: an annotation, a cast, or the value
    that builds it. The first binding that tells answers, as a name
    reads in the rest of the file.
    */
    fn collect_shapes(&self, stmts: &[Stmt], out: &mut Scope) {
        for stmt in stmts {
            match stmt.under_default() {
                Stmt::Local(l) => {
                    for (i, b) in l.names.iter().enumerate() {
                        let name = self.text_of(b.name);

                        if b.destructure.is_some() || out.contains_key(name) {
                            continue;
                        }

                        let shape =
                            self.binding_shape(b.ty, l.values.get(i), std::slice::from_ref(out));

                        if shape.is_some() {
                            out.insert(name.to_string(), shape);
                        }
                    }
                }

                Stmt::GenericFor(f) => {
                    for b in &f.vars {
                        self.note_typed(b.name, b.ty, out);
                    }
                }

                Stmt::NumericFor(f) => self.note_typed(f.var.name, f.var.ty, out),

                _ => {}
            }

            for c in stmt_children(stmt) {
                self.shapes_in_child(c, out);
            }
        }
    }

    fn shapes_in_child(&self, c: Child<'_>, out: &mut Scope) {
        match c {
            Child::Block(b) => self.collect_shapes(&b.stmts, out),

            Child::Function(f) => {
                for p in &f.params {
                    if p.destructure.is_none() && !p.is_vararg {
                        self.note_typed(p.name, p.ty, out);
                    }
                }

                self.collect_shapes(&f.block.stmts, out);
            }

            Child::Expr(e) => {
                for c in expr_children(e) {
                    self.shapes_in_child(c, out);
                }
            }
        }
    }

    /// Notes a name whose annotation tells its shape, unless the file
    /// already told it.
    fn note_typed(&self, name: TokSpan, ty: Option<TokSpan>, out: &mut Scope) {
        let name = self.text_of(name);

        if !out.contains_key(name)
            && let Some(shape) = ty.and_then(|t| self.type_shape(self.text_of(t)))
        {
            out.insert(name.to_string(), Some(shape));
        }
    }

    /// The shape of a binding: its annotation when it has one, else the
    /// value it takes.
    fn binding_shape(
        &self,
        ty: Option<TokSpan>,
        value: Option<&Expr>,
        scopes: &[Scope],
    ) -> Option<Shape> {
        match ty {
            Some(t) => self.type_shape(self.text_of(t)),

            None => value.and_then(|v| self.expr_shape(v, scopes)),
        }
    }

    /*
    The shape a type names: an Instance class, a table of one,
    `{ BasePart }` or `{ [string]: Part }`, or a struct of this file.
    */
    fn type_shape(&self, ty: &str) -> Option<Shape> {
        let ty = ty.trim().trim_end_matches('?').trim();

        if let Some(inner) = ty.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            let inner = inner.trim();
            // `{ [K]: V }` holds V, and `{ V }` is a list of V.
            let value = match inner.strip_prefix('[') {
                Some(rest) => rest.split_once(']')?.1.trim_start().strip_prefix(':')?,

                None => inner,
            };

            return is_instance_type(value).then_some(Shape::Instances);
        }

        if is_instance_type(ty) {
            return Some(Shape::Instance);
        }

        self.struct_field_types
            .contains_key(ty)
            .then(|| Shape::Struct(ty.to_string()))
    }

    /*
    What the check knows about a value. A name reads the scopes, the
    innermost first, and then the Roblox globals. A field of an Instance
    is a child or a property, and a property of a Roblox datatype takes
    no write, so a write through the chain writes an instance.
    */
    fn expr_shape(&self, e: &Expr, scopes: &[Scope]) -> Option<Shape> {
        match e {
            Expr::Paren { inner, .. } => self.expr_shape(inner, scopes),

            Expr::TypeAssert { expr, ty, .. } => self
                .type_shape(self.text_of(*ty))
                .or_else(|| self.expr_shape(expr, scopes)),

            Expr::Name(n) => {
                let name = self.text_of(*n);

                for scope in scopes.iter().rev() {
                    if let Some(shape) = scope.get(name) {
                        return shape.clone();
                    }
                }

                INSTANCE_GLOBALS.contains(&name).then_some(Shape::Instance)
            }

            Expr::Index { object, key, .. } => match self.expr_shape(object, scopes)? {
                Shape::Instance | Shape::Instances => Some(Shape::Instance),

                Shape::Struct(s) => match key {
                    IndexKey::Field(f) => {
                        let field = self.text_of(*f);

                        self.struct_field_types
                            .get(&s)?
                            .iter()
                            .find(|x| x.name == field)
                            .and_then(|x| self.type_shape(self.text_of(x.ty)))
                    }

                    IndexKey::Computed(_) => None,
                },
            },

            Expr::Child { object, .. } => {
                (self.expr_shape(object, scopes)? == Shape::Instance).then_some(Shape::Instance)
            }

            Expr::Call {
                func,
                method: Some(m),
                ..
            } => {
                let m = self.text_of(*m);

                if self.expr_shape(func, scopes)? != Shape::Instance {
                    return None;
                }

                if FINDS_INSTANCE.contains(&m) {
                    Some(Shape::Instance)
                } else {
                    LISTS_INSTANCES.contains(&m).then_some(Shape::Instances)
                }
            }

            Expr::Call {
                func,
                method: None,
                args,
                ..
            } => match self.dotted_name(func).as_deref() {
                Some("Instance.new") => Some(Shape::Instance),

                // `ipairs(t)` and `pairs(t)` walk the table they take.
                Some("ipairs" | "pairs") => match args {
                    CallArgs::Paren(a) => a.first().and_then(|t| self.expr_shape(t, scopes)),

                    _ => None,
                },

                _ => None,
            },

            Expr::New { name, .. } => match name.as_ref() {
                Expr::Name(n) if self.text_of(*n) == "Instance" => Some(Shape::Instance),

                Expr::Name(n) => self
                    .struct_field_types
                    .contains_key(self.text_of(*n))
                    .then(|| Shape::Struct(self.text_of(*n).to_string())),

                _ => None,
            },

            _ => None,
        }
    }

    /// Binds the names of a `local` in the innermost scope, after its
    /// values read the scopes outside it.
    fn bind_local(&self, l: &Local, scopes: &mut [Scope]) {
        let mut bound = Vec::new();

        for (i, b) in l.names.iter().enumerate() {
            match &b.destructure {
                None => bound.push((
                    self.text_of(b.name).to_string(),
                    self.binding_shape(b.ty, l.values.get(i), scopes),
                )),

                Some(d) => {
                    for name in destructured_names(d) {
                        bound.push((self.text_of(name).to_string(), None));
                    }
                }
            }
        }

        if let Some(top) = scopes.last_mut() {
            top.extend(bound);
        }
    }

    /// The scope a `for ... in` opens: the value of a loop over a table
    /// of Instances is an Instance.
    fn for_scope(&self, f: &GenericFor, scopes: &[Scope]) -> Scope {
        let values = f.exprs.first().and_then(|e| self.expr_shape(e, scopes));

        f.vars
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let shape = match b.ty {
                    Some(t) => self.type_shape(self.text_of(t)),

                    None => (i == 1 && values == Some(Shape::Instances)).then_some(Shape::Instance),
                };

                (self.text_of(b.name).to_string(), shape)
            })
            .collect()
    }

    /// The scope of a function's parameters. `self` in a method of a
    /// struct is a value of that struct.
    fn params_scope(&self, body: &FunctionBody, owner: Option<&str>) -> Scope {
        let mut scope: Scope = body
            .params
            .iter()
            .filter(|p| p.destructure.is_none() && !p.is_vararg)
            .map(|p| {
                (
                    self.text_of(p.name).to_string(),
                    p.ty.and_then(|t| self.type_shape(self.text_of(t))),
                )
            })
            .collect();

        if let Some(s) = owner {
            scope.insert("self".to_string(), Some(Shape::Struct(s.to_string())));
        }

        scope
    }

    /*
    Each function and method the file declares that writes an instance,
    by the path a call names, with what it does: "writes `p.Parent`", or
    "calls `f`, which writes `p.Parent`". A function that calls one of
    them writes too, at any depth, so the list grows until it holds.
    */
    fn writers_of(&self, block: &Block) -> HashMap<String, String> {
        let mut bodies = Vec::new();
        self.collect_bodies(&block.stmts, &mut bodies);
        let mut writers = HashMap::new();

        loop {
            let mut grew = false;

            for (path, body, owner) in &bodies {
                if writers.contains_key(path) {
                    continue;
                }

                if let Some(what) = self.first_write(body, owner.as_deref(), &writers) {
                    writers.insert(path.clone(), what);
                    grew = true;
                }
            }

            if !grew {
                return writers;
            }
        }
    }

    /// Each function the file declares: the path a call names, its body,
    /// and the struct it is a method of. A method takes `T:name`, and
    /// `T.name` too for a call through the type.
    fn collect_bodies<'b>(
        &self,
        stmts: &'b [Stmt],
        out: &mut Vec<(String, &'b FunctionBody, Option<String>)>,
    ) {
        for stmt in stmts {
            match stmt.under_default() {
                Stmt::Function(f) => {
                    let parts: Vec<&str> = f.path.iter().map(|p| self.text_of(*p)).collect();

                    match (f.is_method, parts.split_last()) {
                        (true, Some((name, owner))) if !owner.is_empty() => {
                            let owner = owner.join(".");
                            out.push((format!("{owner}:{name}"), &f.body, Some(owner.clone())));
                            out.push((format!("{owner}.{name}"), &f.body, Some(owner)));
                        }

                        _ => out.push((parts.join("."), &f.body, None)),
                    }
                }

                Stmt::LocalFunction(f) => {
                    out.push((self.text_of(f.name).to_string(), &f.body, None))
                }

                Stmt::Local(l) => {
                    for (b, v) in l.names.iter().zip(&l.values) {
                        if let Expr::Function { body, .. } = v {
                            out.push((self.text_of(b.name).to_string(), body, None));
                        }
                    }
                }

                Stmt::Impl(i) => {
                    let owner = one_line(self.text_of(i.target));

                    for f in &i.methods {
                        if let Some(name) = f.path.last() {
                            let name = self.text_of(*name);
                            out.push((format!("{owner}:{name}"), &f.body, Some(owner.clone())));
                            out.push((format!("{owner}.{name}"), &f.body, Some(owner.clone())));
                        }
                    }
                }

                _ => {}
            }

            for c in stmt_children(stmt) {
                match c {
                    Child::Block(b) => self.collect_bodies(&b.stmts, out),

                    Child::Function(f) => self.collect_bodies(&f.block.stmts, out),

                    Child::Expr(_) => {}
                }
            }
        }
    }

    /// What the first instance write of a function body does, as a
    /// report names it.
    fn first_write(
        &self,
        body: &FunctionBody,
        owner: Option<&str>,
        writers: &HashMap<String, String>,
    ) -> Option<String> {
        let mut scopes = vec![self.parallel_shapes.clone(), self.params_scope(body, owner)];
        let mut found = Vec::new();
        self.writes_in_block(&body.block, &mut scopes, writers, &mut found);

        found.into_iter().next()
    }

    /// The instance writes of a block, in source order, as a report
    /// names each one. A nested function is not followed, since its body
    /// runs when it is called.
    fn writes_in_block(
        &self,
        block: &Block,
        scopes: &mut Vec<Scope>,
        writers: &HashMap<String, String>,
        out: &mut Vec<String>,
    ) {
        scopes.push(Scope::new());

        for stmt in &block.stmts {
            self.writes_in_stmt(stmt, scopes, writers, out);
        }

        scopes.pop();
    }

    fn writes_in_stmt(
        &self,
        stmt: &Stmt,
        scopes: &mut Vec<Scope>,
        writers: &HashMap<String, String>,
        out: &mut Vec<String>,
    ) {
        match stmt {
            Stmt::Assign(a) => {
                for t in &a.targets {
                    if let Some(what) = self.instance_write(t, scopes) {
                        out.push(format!("writes `{what}`"));
                    }
                }
            }

            Stmt::Delete { expr, .. } | Stmt::Destroy { expr, .. } => {
                let what = one_line(self.text_of(expr.span()));
                out.push(format!("writes `destroy {what}`"));
            }

            Stmt::LocalFunction(f) => {
                if let Some(top) = scopes.last_mut() {
                    top.insert(self.text_of(f.name).to_string(), None);
                }

                return;
            }

            Stmt::Function(_) => return,

            Stmt::GenericFor(f) => {
                for e in &f.exprs {
                    self.writes_in_expr(e, scopes, writers, out);
                }

                let inner = self.for_scope(f, scopes);
                scopes.push(inner);

                if let Some(c) = &f.filter {
                    self.writes_in_expr(c, scopes, writers, out);
                }

                self.writes_in_block(&f.block, scopes, writers, out);
                scopes.pop();

                return;
            }

            Stmt::NumericFor(f) => {
                for e in [Some(&f.start), Some(&f.limit), f.step.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    self.writes_in_expr(e, scopes, writers, out);
                }

                scopes.push(Scope::from([(self.text_of(f.var.name).to_string(), None)]));
                self.writes_in_block(&f.block, scopes, writers, out);
                scopes.pop();

                return;
            }

            _ => {}
        }

        for c in stmt_children(stmt) {
            match c {
                Child::Expr(e) => self.writes_in_expr(e, scopes, writers, out),

                Child::Block(b) => self.writes_in_block(b, scopes, writers, out),

                Child::Function(_) => {}
            }
        }

        if let Stmt::Local(l) = stmt {
            self.bind_local(l, scopes);
        }
    }

    fn writes_in_expr(
        &self,
        e: &Expr,
        scopes: &mut Vec<Scope>,
        writers: &HashMap<String, String>,
        out: &mut Vec<String>,
    ) {
        if let Expr::Function { .. } | Expr::AsyncBlock { .. } = e {
            return;
        }

        if let Some(what) = self.refused_call(e, scopes, writers) {
            out.push(what.as_writer());
        }

        for c in expr_children(e) {
            match c {
                Child::Expr(x) => self.writes_in_expr(x, scopes, writers, out),

                Child::Block(b) => self.writes_in_block(b, scopes, writers, out),

                Child::Function(_) => {}
            }
        }
    }

    /*
    A call the parallel phase refuses: one that makes or removes an
    instance, `Instance.new`, `new Instance`, `Destroy`, and `Clone` in
    either spelling, or a call of a function or method of this file that
    writes an instance.
    */
    fn refused_call(
        &self,
        e: &Expr,
        scopes: &[Scope],
        writers: &HashMap<String, String>,
    ) -> Option<RefusedCall> {
        match e {
            Expr::Call {
                func,
                method: Some(m),
                ..
            } => {
                let name = self.text_of(*m);

                if INSTANCE_METHODS.contains(&name) {
                    return Some(RefusedCall::Instance(one_line(self.text_of(e.span()))));
                }

                let Shape::Struct(s) = self.expr_shape(func, scopes)? else {
                    return None;
                };
                let what = writers.get(&format!("{s}:{name}"))?;

                Some(RefusedCall::Writer {
                    call: one_line(self.text_of(e.span())),
                    what: what.clone(),
                })
            }

            Expr::Call {
                func, method: None, ..
            } => {
                if let Expr::Index {
                    key: IndexKey::Field(f),
                    ..
                } = func.as_ref()
                    && INSTANCE_METHODS.contains(&self.text_of(*f))
                {
                    return Some(RefusedCall::Instance(one_line(self.text_of(e.span()))));
                }

                let path = self.dotted_name(func)?;

                if path == "Instance.new" {
                    return Some(RefusedCall::Instance(path));
                }

                let what = writers.get(&path)?;

                Some(RefusedCall::Writer {
                    call: path,
                    what: what.clone(),
                })
            }

            Expr::New { name, .. } if matches!(name.as_ref(), Expr::Name(n) if self.text_of(*n) == "Instance") => {
                Some(RefusedCall::Instance("new Instance".to_string()))
            }

            _ => None,
        }
    }

    /*
    The property an assignment target writes on an instance, as the
    source spells it, or `None` when the file cannot tell.

    A `Parent` write is always one: no other value names the field in a
    way a parallel block could mean. Any other property counts when the
    check knows the receiver is an Instance. A value typed `any` cannot
    be seen, and a write through it does not report.
    */
    fn instance_write(&self, target: &Expr, scopes: &[Scope]) -> Option<String> {
        let Expr::Index { object, key, .. } = target else {
            return None;
        };

        let parent = matches!(key, IndexKey::Field(f) if self.text_of(*f) == "Parent");

        (parent || self.expr_shape(object, scopes) == Some(Shape::Instance))
            .then(|| one_line(self.text_of(target.span())))
    }

    /// `parallel do ... end`: `task.desynchronize()` on the line of the
    /// header and `task.synchronize()` on the line of the `end`, so the
    /// line count holds. The block keeps its own scope.
    pub(crate) fn parallel_block(&mut self, p: &ParallelBlock) {
        self.check_phase(
            &p.block,
            Walk {
                phase: Phase::Block,
                loops: 0,
            },
        );

        let start = self.byte_start(p.span);
        let do_tok = self.toks[p.span.start as usize + 1];
        self.generate(start, "task.desynchronize() ");
        self.copy(do_tok.start, do_tok.end);
        self.scopes.push(HashSet::new());
        let body_start = self.block_start_or(&p.block, do_tok.end);
        self.copy(do_tok.end, body_start);
        self.block(&p.block);
        let body_end = self.block_end_or(&p.block, body_start);
        self.scopes.pop();
        let end_tok = self.toks[p.span.end as usize - 1];
        self.copy(body_end, end_tok.start);
        self.generate(end_tok.start, "task.synchronize() ");
        self.copy(end_tok.start, end_tok.end);
    }

    /// Reports what the parallel phase refuses in a block.
    fn check_phase(&mut self, block: &Block, walk: Walk<'_>) {
        self.check_phase_until(block, walk, None, Scope::new());
    }

    /*
    The check of a parallel handler. A statement of the body that calls
    `task.synchronize()`, or the handler's `respond`, which synchronizes
    before it fires, ends the parallel phase, so the statements after it
    run serial and take no check. A call inside a branch or a loop may
    not run, so only one that the body holds directly ends the phase.
    `params` is the scope of the handler's parameters.
    */
    fn check_phase_until(
        &mut self,
        block: &Block,
        walk: Walk<'_>,
        respond: Option<&str>,
        params: Scope,
    ) {
        let mut found = Vec::new();
        let mut scopes = vec![self.parallel_shapes.clone(), params, Scope::new()];

        for stmt in &block.stmts {
            self.phase_stmt(stmt, walk, &mut scopes, &mut found);

            if let Phase::Handler(_) = walk.phase
                && let Stmt::Call(Expr::Call { func, .. }, _) = stmt
                && let Some(path) = self.dotted_name(func)
                && (path == "task.synchronize" || Some(path.as_str()) == respond)
            {
                break;
            }
        }

        for (span, message) in found {
            self.diagnose(span, &message);
        }
    }

    fn phase_block(
        &self,
        block: &Block,
        walk: Walk<'_>,
        scopes: &mut Vec<Scope>,
        out: &mut Vec<(TokSpan, String)>,
    ) {
        scopes.push(Scope::new());

        for stmt in &block.stmts {
            self.phase_stmt(stmt, walk, scopes, out);
        }

        scopes.pop();
    }

    fn phase_stmt(
        &self,
        stmt: &Stmt,
        walk: Walk<'_>,
        scopes: &mut Vec<Scope>,
        out: &mut Vec<(TokSpan, String)>,
    ) {
        let who = walk.phase.who();
        let block_only = matches!(walk.phase, Phase::Block);

        match stmt {
            // The inner block renders on its own and runs its own check.
            Stmt::Parallel(p) => {
                let at = TokSpan::new(p.span.start as usize, p.span.start as usize + 1);
                out.push((
                    at,
                    format!(
                        "{who} cannot hold a `parallel` block; the engine reads it as already desynchronized, so remove the inner `parallel do`"
                    ),
                ));

                return;
            }

            Stmt::Return(r) if block_only && !r.value_only => {
                out.push((
                    stmt.span(),
                    format!(
                        "{who} cannot `return`; the thread would leave in the parallel phase and skip `task.synchronize()`; set a local and return after the block"
                    ),
                ));
            }

            Stmt::Break(s) | Stmt::Continue(s) if block_only && walk.loops == 0 => {
                let word = self.text_of(*s);
                out.push((
                    *s,
                    format!(
                        "{who} cannot `{word}` out of the block; the loop would go on in the parallel phase and skip `task.synchronize()`; set a local and `{word}` after the block"
                    ),
                ));
            }

            Stmt::Assign(a) => {
                for t in &a.targets {
                    if let Some(what) = self.instance_write(t, scopes) {
                        out.push((
                            t.span(),
                            format!("{who} cannot write `{what}`; {}", walk.phase.fix("write")),
                        ));
                    }
                }
            }

            Stmt::Delete { expr, .. } | Stmt::Destroy { expr, .. } => {
                let what = one_line(self.text_of(expr.span()));
                out.push((
                    stmt.span(),
                    format!(
                        "{who} cannot destroy `{what}`; {}",
                        walk.phase.fix("statement")
                    ),
                ));
            }

            // A function declared here runs when it is called.
            Stmt::LocalFunction(f) => {
                if let Some(top) = scopes.last_mut() {
                    top.insert(self.text_of(f.name).to_string(), None);
                }

                return;
            }

            Stmt::Function(_) => return,

            Stmt::GenericFor(f) => {
                for e in &f.exprs {
                    self.phase_expr(e, walk, scopes, out);
                }

                let inner = self.for_scope(f, scopes);
                scopes.push(inner);

                if let Some(c) = &f.filter {
                    self.phase_expr(c, walk, scopes, out);
                }

                let walk = Walk {
                    loops: walk.loops + 1,
                    ..walk
                };
                self.phase_block(&f.block, walk, scopes, out);
                scopes.pop();

                return;
            }

            Stmt::NumericFor(f) => {
                for e in [Some(&f.start), Some(&f.limit), f.step.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    self.phase_expr(e, walk, scopes, out);
                }

                scopes.push(Scope::from([(self.text_of(f.var.name).to_string(), None)]));
                let walk = Walk {
                    loops: walk.loops + 1,
                    ..walk
                };
                self.phase_block(&f.block, walk, scopes, out);
                scopes.pop();

                return;
            }

            _ => {}
        }

        let loops = match stmt {
            Stmt::While(_) | Stmt::Repeat(_) => walk.loops + 1,

            _ => walk.loops,
        };

        for c in stmt_children(stmt) {
            match c {
                Child::Expr(e) => self.phase_expr(e, walk, scopes, out),

                Child::Block(b) => self.phase_block(b, Walk { loops, ..walk }, scopes, out),

                Child::Function(_) => {}
            }
        }

        if let Stmt::Local(l) = stmt {
            self.bind_local(l, scopes);
        }
    }

    fn phase_expr(
        &self,
        e: &Expr,
        walk: Walk<'_>,
        scopes: &mut Vec<Scope>,
        out: &mut Vec<(TokSpan, String)>,
    ) {
        let who = walk.phase.who();

        match e {
            // A closure runs when it is called, and an `async do` on a
            // thread of its own.
            Expr::Function { .. } | Expr::AsyncBlock { .. } => return,

            Expr::Await { .. } => out.push((
                e.span(),
                format!(
                    "{who} cannot `await`; a Future resumes in the serial phase, so the block would end where the source does not say; await after the block"
                ),
            )),

            _ => {}
        }

        match self.refused_call(e, scopes, &self.parallel_writers) {
            Some(RefusedCall::Instance(what)) => out.push((
                e.span(),
                format!("{who} cannot call `{what}`; {}", walk.phase.fix("call")),
            )),

            Some(RefusedCall::Writer { call, what }) => out.push((
                e.span(),
                format!(
                    "{who} cannot call `{call}`, which {what}; {}",
                    walk.phase.fix("call")
                ),
            )),

            None => {}
        }

        for c in expr_children(e) {
            match c {
                Child::Expr(x) => self.phase_expr(x, walk, scopes, out),

                Child::Block(b) => self.phase_block(b, walk, scopes, out),

                Child::Function(_) => {}
            }
        }
    }

    /*
    `message Name(params)`: the name holds a table with the topic and a
    `fire` that sends it, so the value passes on and `Attributes.get`
    reads it. The check artifact types the three calls from the
    parameters.

    A message carries what the engine carries: the arguments cross as
    they are, with no serializer. A function, a coroutine, and a value
    whose methods live on a metatable do not arrive, and each reports
    with the rule named for actors.
    */
    pub(crate) fn message_decl(&mut self, m: &MessageDecl) {
        let name = self.decl_name(m.name);
        let start = self.byte_start(m.span);
        let end = self.byte_end(m.span);

        if self.options.definitions {
            self.blank_lines(start, end);

            return;
        }

        // A namespace opens no scope, and the check below reads scopes.
        if self.scopes.len() != self.top_scope + 1 || !self.ns_stack.is_empty() {
            self.diagnose(
                m.name,
                &format!(
                    "message `{name}` is declared inside a block; a message declares at the top level of a file, where every script can import it"
                ),
            );
        }

        for p in &m.params {
            let pname = self.text_of(p.name).to_string();

            if p.destructure.is_some() {
                self.diagnose(
                    p.name,
                    &format!(
                        "a message takes no pattern: `{pname}` has no name for the handler to read; name the parameter"
                    ),
                );

                continue;
            }

            if let Some(d) = &p.default {
                self.diagnose(
                    d.span(),
                    &format!(
                        "parameter `{pname}` of message `{name}` takes no default; the engine passes what `fire` sends, so pass the value there"
                    ),
                );
            }

            let Some(ty) = p.ty else { continue };
            let text = self.text_of(ty).trim().to_string();

            if let Some(why) = self.message_offender(&text) {
                self.diagnose(
                    ty,
                    &format!(
                        "message `{name}`: parameter `{pname}` {why}; an actor message carries only data"
                    ),
                );
            }
        }

        if let Some((word, params)) = &m.reply {
            self.check_reply(&name, m, *word, params);
        }

        let attrs = self.attr_table(&m.attributes);
        let topic = luau_string(self.text_of(m.name));
        let value = match &m.reply {
            // The table makes the BindableEvent of the reply on first
            // use, and `fire` sends it after the arguments. A message
            // with no reply keeps the plain table.
            Some(_) => {
                let std = self.std();

                format!("{std}.message({topic}, {})", m.params.len())
            }

            None => format!(
                "{{ topic = {topic}, fire = function(actor: Actor, ...) actor:SendMessage({topic}, ...) end }}"
            ),
        };
        let value = match attrs.as_str() {
            "{}" => value,

            _ => {
                let std = self.std();

                format!("{std}.attrs({value}, {{ own = {attrs} }})")
            }
        };
        let text = match self.options.check {
            true => format!(
                "local {name} = ({value} :: any) :: {}",
                self.message_type(m)
            ),

            false => format!("local {name} = {value}"),
        };
        self.generate(start, &text);
        self.blank_lines(start, end);

        let members: Vec<contracts::Member> = m
            .params
            .iter()
            .map(|p| contracts::Member {
                name: self.text_of(p.name).to_string(),
                kind: "field",
                private: false,
                shape: p
                    .ty
                    .map(|t| self.text_of(t).trim().to_string())
                    .unwrap_or_default(),
                at: self.byte_start(p.name),
            })
            .collect();
        let owner = contracts::Owner {
            target: "message",
            name: &name,
            body: m.span,
        };
        self.check_contracts(&m.attributes, owner, &members);

        if m.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /*
    `reply(params)`: the answer crosses on a BindableEvent, which copies
    a table and drops its metatable the way a message does. So a reply
    parameter takes the rules of a message parameter. `respond` goes
    after the parameters of the handler, and `...` has no end to put it
    after.
    */
    fn check_reply(&mut self, name: &str, m: &MessageDecl, word: TokSpan, params: &[Param]) {
        if m.params.last().is_some_and(|p| p.is_vararg) {
            self.diagnose(
                word,
                &format!(
                    "message `{name}` ends its parameters with `...`, so it takes no `reply`: the handler takes `respond` after the parameters, and `...` has no end; name each parameter"
                ),
            );
        }

        for p in params {
            let pname = self.text_of(p.name).to_string();

            if p.destructure.is_some() {
                self.diagnose(
                    p.name,
                    &format!(
                        "a message takes no pattern: `{pname}` in the reply of `{name}` has no name for `replied` to read; name the parameter"
                    ),
                );

                continue;
            }

            if let Some(d) = &p.default {
                self.diagnose(
                    d.span(),
                    &format!(
                        "reply parameter `{pname}` of message `{name}` takes no default; the event passes what `respond` sends, so pass the value there"
                    ),
                );
            }

            let Some(ty) = p.ty else { continue };
            let text = self.text_of(ty).trim().to_string();

            if let Some(why) = self.message_offender(&text) {
                self.diagnose(
                    ty,
                    &format!(
                        "message `{name}`: reply parameter `{pname}` {why}; an actor message carries only data"
                    ),
                );
            }
        }
    }

    /// Why a parameter type cannot cross an actor boundary, or `None`
    /// when the engine passes it.
    fn message_offender(&self, ty: &str) -> Option<String> {
        if let Some((field, bad, why)) = self.offender_of(ty) {
            let why = why.replace("that the wire cannot pack", "that an actor message drops");

            return Some(match field {
                Some(f) => format!("has field `{f}` of type `{bad}`, which {why}"),

                None => format!("has type `{bad}`, which {why}"),
            });
        }

        // A struct or a payload enum keeps its methods on a metatable,
        // and the engine copies the table without it.
        let mut base = ty.trim();

        while let Some(t) = base.strip_suffix('?').or_else(|| base.strip_suffix("[]")) {
            base = t.trim();
        }

        let own_struct = self.structs.contains(base);
        let own_enum = self
            .enum_payloads
            .get(base)
            .is_some_and(|v| v.iter().any(|(_, p)| !p.is_empty()));
        let imported = self.imported_type(base).is_some_and(|sh| {
            sh.variants.is_empty() || sh.variants.iter().any(|(_, p)| !p.is_empty())
        });

        (own_struct || own_enum || imported).then(|| {
            format!(
                "has type `{base}`, whose methods live on a metatable that the engine drops on the way"
            )
        })
    }

    /// The type the check artifact gives a message: `fire` takes the
    /// Actor and the parameters, and `on` and `once` take a handler of
    /// the parameters. With a reply, the handler takes `respond` after
    /// them, and `replied` takes a handler of the reply.
    fn message_type(&mut self, m: &MessageDecl) -> String {
        let list = self.typed_params(&m.params);
        let fire = match list.is_empty() {
            true => "actor: Actor".to_string(),

            false => format!("actor: Actor, {list}"),
        };
        let Some((_, reply)) = &m.reply else {
            let handler = format!("(handler: ({list}) -> ()) -> RBXScriptConnection");

            return format!(
                "{{ topic: string, fire: ({fire}) -> (), on: {handler}, once: {handler} }}"
            );
        };
        let answer = self.typed_params(reply);
        let respond = format!("respond: ({answer}) -> ()");
        let with = match list.is_empty() {
            true => respond,

            false => format!("{list}, {respond}"),
        };
        let handler = format!("(handler: ({with}) -> ()) -> RBXScriptConnection");

        format!(
            "{{ topic: string, fire: ({fire}) -> (), on: {handler}, once: {handler}, replied: (handler: ({answer}) -> ()) -> RBXScriptConnection }}"
        )
    }

    /// A parameter list as a function type writes it: `dt: number`.
    fn typed_params(&mut self, params: &[Param]) -> String {
        params
            .iter()
            .map(|p| {
                let name = self.text_of(p.name).to_string();
                let ty =
                    p.ty.map(|t| self.copy_type_to_string(t).trim().to_string())
                        .unwrap_or_else(|| "any".to_string());

                match p.is_vararg {
                    true => format!("...: {ty}"),

                    false => format!("{name}: {ty}"),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The message a call's callee names, with the verb: `Step.fire`, or
    /// `M.Step.on` through a star import. The topic is the declared
    /// name, which an import may bind under another. `None` for any
    /// other call, and for a local that shadows the message's name.
    pub(crate) fn message_call_of(&self, func: &Expr) -> Option<(String, String, MessageSig)> {
        if self.messages.is_empty() {
            return None;
        }

        let Expr::Index {
            object,
            key: IndexKey::Field(verb),
            optional: false,
            ..
        } = func
        else {
            return None;
        };
        let verb = self.text_of(*verb);

        if !matches!(verb, "fire" | "on" | "once" | "replied") {
            return None;
        }

        let path = self.dotted_name(object)?;
        let sig = self.messages.get(&path)?.clone();
        let head = path.split('.').next().unwrap_or(&path);
        let at = object.span().start as usize;
        let shadowed = self.message_shadows.iter().any(|(n, _, reach)| {
            n == head
                && reach
                    .as_ref()
                    .is_none_or(|ranges| ranges.iter().any(|&(a, b)| a <= at && at < b))
        });

        (!shadowed).then(|| (path, verb.to_string(), sig))
    }

    /// Whether the ship artifact writes a message call as the engine's
    /// own: `fire` of a message with no reply, `on`, and `once`. The
    /// table of a message with a reply sends through its own `fire`,
    /// and `replied` is a function of the table.
    pub(crate) fn rewrites_message_call(&self, func: &Expr) -> bool {
        self.message_call_of(func)
            .is_some_and(|(_, verb, sig)| match verb.as_str() {
                "fire" => sig.reply.is_none(),

                "replied" => false,

                _ => true,
            })
    }

    /*
    The checks of a message call, in both artifacts: `on` and `once`
    bind on the Actor of this script, so the file needs the `.actor.`
    infix, and a parallel handler takes the rules of a `parallel` block.
    */
    pub(crate) fn check_message_call(&mut self, e: &Expr) {
        let Expr::Call { func, args, .. } = e else {
            return;
        };
        let Some((path, verb, sig)) = self.message_call_of(func) else {
            return;
        };
        let call = format!("{path}.{verb}");
        let name = path.rsplit('.').next().unwrap_or(&path).to_string();

        let CallArgs::Paren(list) = args else {
            self.diagnose(
                e.span(),
                &format!("a message call takes its arguments in parentheses: `{call}(...)`"),
            );

            return;
        };

        if verb == "fire" {
            if list.is_empty() {
                self.diagnose(
                    e.span(),
                    &format!(
                        "`{call}` takes the Actor to send to first: `{call}(actor, ...)`; a message reaches an Actor and nothing else"
                    ),
                );
            }

            return;
        }

        if verb == "replied" {
            self.check_replied(e, &call, &sig, list);

            return;
        }

        if !crate::directives::is_actor(&self.options.file_name) {
            let file = std::path::Path::new(&self.options.file_name)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let fix = match crate::directives::file_side(&file) {
                Some(_) => {
                    let (stem, ext) = file.rsplit_once('.').unwrap_or((&file, "aly"));

                    format!("name the file `{stem}.actor.{ext}`")
                }

                None => "a module sits under no Actor, so bind in a `.server.actor.aly` or `.client.actor.aly` script".to_string(),
            };
            self.diagnose(
                func.span(),
                &format!(
                    "`{call}` binds on the Actor of this script, and `{file}` has no `.actor.` infix, so the build places it under no Actor; {fix}"
                ),
            );
        }

        if list.len() != 1 {
            self.diagnose(
                e.span(),
                &format!(
                    "`{call}` takes one handler, since a message binds one function: `{call}(function(...) end)`"
                ),
            );

            return;
        }

        let respond = match &list[0] {
            Expr::Function { body, .. } => self.check_handler(&call, &sig, body),

            _ => None,
        };

        if !sig.parallel {
            return;
        }

        match &list[0] {
            Expr::Function { body, .. } => {
                // A parameter the handler leaves untyped takes the type the
                // message declares.
                let mut params = self.params_scope(body, None);

                for (p, ty) in body.params.iter().zip(&sig.param_types) {
                    if p.ty.is_none()
                        && let Some(ty) = ty
                        && let Some(entry) = params.get_mut(self.text_of(p.name))
                    {
                        *entry = self.type_shape(ty);
                    }
                }

                self.check_phase_until(
                    &body.block,
                    Walk {
                        phase: Phase::Handler(&name),
                        loops: 0,
                    },
                    respond.as_deref(),
                    params,
                );
            }

            handler => {
                if let Some(f) = self.dotted_name(handler)
                    && let Some(what) = self.parallel_writers.get(&f).cloned()
                {
                    self.diagnose(
                        handler.span(),
                        &format!(
                            "the parallel handler of `{name}` is `{f}`, which {what}; the handler runs in the parallel phase, so call `task.synchronize()` before the write"
                        ),
                    );
                }
            }
        }
    }

    /*
    The parameters of a handler written in place against its message.
    A parameter past the message's own is `respond` when the message
    declares a reply, and has nothing behind it when it does not. Each
    call of `respond` passes the values of the reply. Gives the name the
    handler binds `respond` to.
    */
    fn check_handler(
        &mut self,
        call: &str,
        sig: &MessageSig,
        body: &FunctionBody,
    ) -> Option<String> {
        let topic = &sig.topic;

        if sig.vararg || body.params.iter().any(|p| p.is_vararg) {
            return None;
        }

        let extra = body.params.get(sig.params)?;
        let word = self.text_of(extra.name).to_string();

        let Some(reply) = &sig.reply else {
            self.diagnose(
                extra.name,
                &format!(
                    "the handler of `{call}` names `{word}` after the parameters of message `{topic}`, and the message declares no `reply`, so `{word}` has nothing behind it; write `reply(...)` after the parameters to answer"
                ),
            );

            return None;
        };

        if let Some(more) = body.params.get(sig.params + 1) {
            let text = self.text_of(more.name).to_string();
            self.diagnose(
                more.name,
                &format!(
                    "the handler of `{call}` takes the parameters of message `{topic}` and then `respond`; `{text}` has nothing behind it"
                ),
            );
        }

        if !reply.iter().any(|p| p.starts_with("...")) {
            let mut calls = Vec::new();
            self.calls_of_block(&word, &body.block, &mut calls);
            let shape = reply.join(", ");
            let most = reply.len();
            // A call may leave out the optional values at the end.
            let least = reply
                .iter()
                .rposition(|p| !p.trim_end().ends_with('?'))
                .map_or(0, |i| i + 1);

            for (span, got) in calls
                .into_iter()
                .filter(|(_, got)| *got < least || *got > most)
            {
                let values = match (least, most) {
                    (1, 1) => "1 value".to_string(),

                    (a, b) if a == b => format!("{b} values"),

                    (a, b) => format!("{a} to {b} values"),
                };
                self.diagnose(
                    span,
                    &format!(
                        "`{word}` answers message `{topic}` with its reply, `({shape})`, which takes {values}; this call passes {got}"
                    ),
                );
            }
        }

        Some(word)
    }

    /// `replied` binds a handler of the reply, on any script, so it
    /// takes no `.actor.` infix. It needs a reply to bind.
    fn check_replied(&mut self, e: &Expr, call: &str, sig: &MessageSig, list: &[Expr]) {
        let topic = &sig.topic;

        let Some(reply) = &sig.reply else {
            self.diagnose(
                e.span(),
                &format!(
                    "message `{topic}` declares no `reply`, so `{call}` has no answer to bind; write `reply(...)` after its parameters"
                ),
            );

            return;
        };

        if list.len() != 1 {
            let names: Vec<&str> = reply
                .iter()
                .map(|p| p.split(':').next().unwrap_or(p).trim())
                .collect();
            self.diagnose(
                e.span(),
                &format!(
                    "`{call}` takes one handler of the reply of message `{topic}`: `{call}(function({}) end)`",
                    names.join(", ")
                ),
            );

            return;
        }

        if let Expr::Function { body, .. } = &list[0]
            && !reply.iter().any(|p| p.starts_with("..."))
            && !body.params.iter().any(|p| p.is_vararg)
            && let Some(more) = body.params.get(reply.len())
        {
            let word = self.text_of(more.name).to_string();
            self.diagnose(
                more.name,
                &format!(
                    "the handler of `{call}` takes the reply of message `{topic}`, `({})`; `{word}` has nothing behind it",
                    reply.join(", ")
                ),
            );
        }
    }

    /// Each call of `name` in a block, with the count of values it
    /// passes. A call whose last argument is a call or `...` passes an
    /// open count and stays out. A function that binds `name` again
    /// hides it.
    fn calls_of_block(&self, name: &str, block: &Block, out: &mut Vec<(TokSpan, usize)>) {
        for stmt in &block.stmts {
            if let Stmt::LocalFunction(f) = stmt
                && self.text_of(f.name) == name
            {
                return;
            }

            if let Stmt::Local(l) = stmt
                && l.names.iter().any(|b| self.text_of(b.name) == name)
            {
                for v in &l.values {
                    self.calls_of_expr(name, v, out);
                }

                return;
            }

            for c in stmt_children(stmt) {
                self.calls_of_child(name, c, out);
            }
        }
    }

    fn calls_of_child(&self, name: &str, c: Child<'_>, out: &mut Vec<(TokSpan, usize)>) {
        match c {
            Child::Expr(x) => self.calls_of_expr(name, x, out),

            Child::Block(b) => self.calls_of_block(name, b, out),

            Child::Function(f) => {
                if !f.params.iter().any(|p| self.text_of(p.name) == name) {
                    self.calls_of_block(name, &f.block, out);
                }
            }
        }
    }

    fn calls_of_expr(&self, name: &str, e: &Expr, out: &mut Vec<(TokSpan, usize)>) {
        if let Expr::Call {
            func,
            method: None,
            args: CallArgs::Paren(list),
            ..
        } = e
            && matches!(func.as_ref(), Expr::Name(n) if self.text_of(*n) == name)
            && !list
                .last()
                .is_some_and(|a| matches!(a, Expr::Call { .. } | Expr::Vararg(_)))
        {
            out.push((e.span(), list.len()));
        }

        if let Expr::Function { body, .. } = e {
            if !body.params.iter().any(|p| self.text_of(p.name) == name) {
                self.calls_of_block(name, &body.block, out);
            }

            return;
        }

        for c in expr_children(e) {
            self.calls_of_child(name, c, out);
        }
    }

    /// The ship artifact of a message call: the engine's own API with
    /// the topic filled in. The arguments render in place, so a handler
    /// that spans lines keeps its lines.
    pub(crate) fn message_call(&mut self, e: &Expr) {
        let Expr::Call {
            func,
            args: CallArgs::Paren(list),
            ..
        } = e
        else {
            return;
        };
        let Some((_, verb, sig)) = self.message_call_of(func) else {
            return;
        };
        let parallel = sig.parallel;
        let anchor = self.byte_start(e.span());
        let close = self.byte_end(e.span());
        let topic = luau_string(&sig.topic);

        let Some((first, rest)) = list.split_first() else {
            self.generate(anchor, "nil");

            return;
        };

        let head = match verb.as_str() {
            "fire" => {
                // `a or b` needs its parentheses in front of `:`.
                let wrap = !matches!(
                    first,
                    Expr::Name(_) | Expr::Index { .. } | Expr::Call { .. } | Expr::Paren { .. }
                );

                if wrap {
                    self.generate(anchor, "(");
                }

                self.expr(first);
                let at = self.byte_end(first.span());
                let paren = if wrap { ")" } else { "" };
                self.generate(at, &format!("{paren}:SendMessage({topic}"));

                at
            }

            _ => {
                let bind = match parallel {
                    true => "BindToMessageParallel",

                    false => "BindToMessage",
                };
                let prefix = match verb.as_str() {
                    "once" => {
                        let std = self.std();

                        format!("{std}.message_once(script:GetActor(), {topic}, {parallel}, ")
                    }

                    _ => format!("script:GetActor():{bind}({topic}, "),
                };
                self.generate(anchor, &prefix);

                // With a reply, the engine passes the event after the
                // arguments, and the handler takes `respond` there.
                let respond = sig.reply.is_some().then(|| self.std());

                if let Some(std) = &respond {
                    self.generate(anchor, &format!("{std}.message_respond("));
                }

                self.expr(first);
                let at = self.byte_end(first.span());

                if respond.is_some() {
                    self.generate(at, &format!(", {}, {parallel})", sig.params));
                }

                at
            }
        };

        let mut prev = head;

        for a in rest {
            let s = self.byte_start(a.span());
            self.copy(prev, s);
            self.expr(a);
            prev = self.byte_end(a.span());
        }

        self.copy(prev, close);
    }
}

/// The quick fix of a statement a `parallel` block refuses: cut its
/// line out of the block and write it on the line after the `end`.
pub struct ParallelMove {
    /// The bytes of the statement's lines, newline included.
    pub cut: (u32, u32),
    /// The byte the moved line goes in front of: the line after `end`.
    pub insert_at: u32,
    /// The moved line, at the indent of the `parallel` line.
    pub text: String,
}

/*
The quick fix for the refused statement that holds the byte `at`, when
moving it keeps the program's meaning.

The statement moves when it is a write, a call, or a `destroy` that the
block holds directly, alone on its lines. A statement inside a loop or a
branch runs a number of times the move would change. Three more things
keep it in place: a name it reads that the block declares, since the
name ends with the block; a later statement that reads what it writes;
and a later statement that assigns a name it reads, since the value
would change under it.
*/
pub fn parallel_move(src: &str, at: u32) -> Option<ParallelMove> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let block = find_parallel(&parsed.chunk.block, toks, at)?;
    let (p, stmt_at) = block;
    let body = &p.block.stmts;
    let stmt = &body[stmt_at];
    let text = |span: TokSpan| span.text_or_empty(src, toks);

    let target = match stmt {
        Stmt::Assign(a) => Some(text(a.targets.first()?.span()).to_string()),

        Stmt::Call(..) | Stmt::Delete { .. } | Stmt::Destroy { .. } => None,

        _ => return None,
    };

    let span = stmt.span();
    let start = toks[span.start as usize].start as usize;
    let end = toks[span.end as usize - 1].end as usize;
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[end..].find('\n').map_or(src.len(), |i| end + i);

    if !src[line_start..start].trim().is_empty() || !src[end..line_end].trim().is_empty() {
        return None;
    }

    let words = |span: TokSpan| -> Vec<&str> {
        (span.start as usize..span.end as usize)
            .filter(|&i| toks[i].kind == alloy_syntax::lexer::TokKind::Ident)
            .filter(|&i| i == 0 || !matches!(toks[i - 1].text(src), "." | ":"))
            .map(|i| toks[i].text(src))
            .collect()
    };
    let read = words(span);
    let declared: Vec<&str> = body[..stmt_at]
        .iter()
        .flat_map(|s| match s {
            Stmt::Local(l) => l.names.iter().map(|b| text(b.name)).collect(),

            Stmt::LocalFunction(f) => vec![text(f.name)],

            _ => Vec::new(),
        })
        .collect();

    if read.iter().any(|w| declared.contains(w)) {
        return None;
    }

    for later in &body[stmt_at + 1..] {
        let later_text = text(later.span());

        if target.as_deref().is_some_and(|t| later_text.contains(t)) {
            return None;
        }

        if let Stmt::Assign(a) = later
            && a.targets
                .iter()
                .any(|t| words(t.span()).first().is_some_and(|w| read.contains(w)))
        {
            return None;
        }
    }

    let end_tok = toks[p.span.end as usize - 1];
    let after_end = src[end_tok.end as usize..]
        .find('\n')
        .map_or(src.len(), |i| end_tok.end as usize + i + 1);
    let p_start = toks[p.span.start as usize].start as usize;
    let p_line = src[..p_start].rfind('\n').map_or(0, |i| i + 1);
    let indent = &src[p_line..p_start];
    let newline = if after_end == src.len() && !src.ends_with('\n') {
        "\n"
    } else {
        ""
    };

    Some(ParallelMove {
        cut: (line_start as u32, (line_end + 1).min(src.len()) as u32),
        insert_at: after_end as u32,
        text: format!("{newline}{indent}{}\n", &src[start..end]),
    })
}

/// The innermost `parallel` block whose own statements hold the byte
/// `at`, with the index of that statement.
fn find_parallel<'a>(
    block: &'a Block,
    toks: &[alloy_syntax::lexer::Tok],
    at: u32,
) -> Option<(&'a ParallelBlock, usize)> {
    let holds = |s: &Stmt| {
        let span = s.span();

        span.end > span.start
            && toks[span.start as usize].start <= at
            && at < toks[span.end as usize - 1].end
    };
    let stmt = block.stmts.iter().find(|s| holds(s))?;

    if let Stmt::Parallel(p) = stmt
        && let Some(i) = p.block.stmts.iter().position(holds)
    {
        let inner = &p.block.stmts[i];

        return match inner {
            Stmt::Parallel(_) => find_parallel(&p.block, toks, at),

            _ => Some((p, i)),
        };
    }

    stmt_children(stmt).into_iter().find_map(|c| match c {
        Child::Block(b) => find_parallel(b, toks, at),

        Child::Function(f) => find_parallel(&f.block, toks, at),

        Child::Expr(e) => expr_blocks(e)
            .into_iter()
            .find_map(|b| find_parallel(b, toks, at)),
    })
}

/// The blocks an expression holds at any depth: a function literal's
/// body, the block of an `async do` or a `try do`.
fn expr_blocks(e: &Expr) -> Vec<&Block> {
    expr_children(e)
        .into_iter()
        .flat_map(|c| match c {
            Child::Block(b) => vec![b],

            Child::Function(f) => vec![&f.block],

            Child::Expr(x) => expr_blocks(x),
        })
        .collect()
}

/// A call the parallel phase refuses.
enum RefusedCall {
    /// A call that makes or removes an instance, as the source spells it.
    Instance(String),
    /// A call of a function or method of this file, with what it does.
    Writer { call: String, what: String },
}

impl RefusedCall {
    /// What a function that holds this call does, as a report of a call
    /// to that function names it.
    fn as_writer(&self) -> String {
        match self {
            RefusedCall::Instance(what) => format!("writes `{what}`"),

            RefusedCall::Writer { call, what } => format!("calls `{call}`, which {what}"),
        }
    }
}

/// Whether a type names an Instance class.
fn is_instance_type(ty: &str) -> bool {
    let ty = ty.trim().trim_end_matches('?').trim();

    ty == "Instance" || crate::roblox_classes::INSTANCE_CLASSES.contains(&ty)
}

/// The names a destructure binds.
fn destructured_names(d: &Destructure) -> Vec<TokSpan> {
    match d {
        Destructure::Table(fields) => fields.iter().map(|f| f.rename.unwrap_or(f.field)).collect(),

        Destructure::Array { items, rest } => items.iter().copied().chain(*rest).collect(),
    }
}
