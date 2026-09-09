//! Attribute checks, cfg, and the prescan that gathers names other
//! phases route through.

use std::collections::HashSet;

use alloy_syntax::ast::{
    Attr, AttributeDecl, Block, CallArgs, Expr, FunctionBody, ImportKind, IndexKey, Stmt, TokSpan,
};

use crate::roblox_classes::{DATATYPES, INSTANCE_CLASSES};

use super::types::{literal_kind, literal_type, strip_bounds};
use super::*;

/// The targets a built-in attribute takes, or `None` when the name is
/// not one. The list mirrors `builtin_attribute_targets` in alloy-lsp.
pub(crate) fn builtin_attr_targets(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "derive" | "sealed" => &["struct", "enum"],

        "cfg" => &["function", "local", "namespace"],

        "deprecated" => &["function", "namespace"],

        "test" | "native" | "checked" | "inline" | "noinline" => &["function"],

        "unreliable" | "ratelimit" | "timeout" | "validate" | "immediate" => &["remote"],

        "u8" | "u16" | "u32" | "i8" | "i16" | "i32" | "f32" => &["param", "field"],

        "rename" | "skip" => &["field"],

        _ => return None,
    })
}

/// The return type a trait method's signature declares. The signature
/// starts at `(`; the type follows the closing `)` after a `:` or a
/// `->`.
pub(crate) fn signature_ret_type(sig: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut after = None;

    for (i, c) in sig.char_indices() {
        match c {
            '(' => depth += 1,

            ')' => {
                depth -= 1;

                if depth == 0 {
                    after = Some(i + 1);

                    break;
                }
            }

            _ => {}
        }
    }

    let rest = sig[after?..].trim();
    let rest = rest.strip_prefix("->").or_else(|| rest.strip_prefix(':'))?;
    let rest = rest.trim();

    (!rest.is_empty()).then_some(rest)
}

impl<'s> Desugar<'s> {
    /// Gathers the names that other statements route through: extension
    /// methods, statics, macros, traits. One pass over the top level.
    /// Finds every `reduce` whose initial value is a literal and whose
    /// function leaves its accumulator untyped: the parameter takes the
    /// literal's type, since the checker reads the function before the
    /// value and would leave it generic.
    pub(crate) fn scan_reduce_inserts(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.scan_children(stmt_children(stmt));
        }
    }

    pub(crate) fn scan_children(&mut self, children: Vec<Child<'_>>) {
        for child in children {
            match child {
                Child::Expr(e) => self.scan_expr_for_reduce(e),

                Child::Block(b) => self.scan_reduce_inserts(b),

                Child::Function(f) => self.scan_reduce_inserts(&f.block),
            }
        }
    }

    /// Walks the file for checks the emit does not need. A statement that
    /// desugars to itself is copied whole, so its expressions never reach
    /// `expr`; this pass sees them all.
    pub(crate) fn scan_static_checks(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.check_stmt_attrs(stmt);
            self.check_children_of(stmt_children(stmt));
        }
    }

    /// The attributes of one declaration, against the target each one
    /// takes.
    pub(crate) fn check_stmt_attrs(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Function(f) => self.check_attrs(&f.attrs, "function"),

            Stmt::LocalFunction(f) => self.check_attrs(&f.attrs, "function"),

            Stmt::Local(l) => {
                self.check_attrs(&l.attrs, "local");

                // Luau reports this as a syntax error in the emit, which
                // only `alloy flux` runs. The source says it first.
                if l.is_const && l.values.is_empty() {
                    self.diagnose(l.keyword, "`const` needs a value");
                }
            }

            Stmt::Remote(r) => self.check_attrs(&r.attributes, "remote"),

            Stmt::Struct(st) => {
                self.check_attrs(&st.attributes, "struct");
                let name = self.text_of(st.name).to_string();
                let mut seen: HashSet<String> = HashSet::new();

                for f in &st.fields {
                    self.check_attrs(&f.attributes, "field");
                    let fname = self.text_of(f.name).to_string();

                    if !seen.insert(fname.clone()) {
                        let message = format!("`{name}` declares the field `{fname}` twice");
                        self.diagnose(f.name, &message);
                    }
                }
            }

            Stmt::Enum(e) => {
                self.check_attrs(&e.attributes, "enum");

                for v in &e.variants {
                    self.check_attrs(&v.attributes, "variant");
                }
            }

            // A namespace takes attributes, and so does each member.
            Stmt::Namespace(ns) => {
                self.check_attrs(&ns.attributes, "namespace");

                for m in &ns.members {
                    self.check_stmt_attrs(&m.stmt);
                }
            }

            _ => {}
        }
    }

    /// One attribute list: every name is declared, takes this target, and
    /// carries the arguments it declares.
    pub(crate) fn check_attrs(&mut self, attrs: &[Attr], target: &str) {
        let mut inline: Option<TokSpan> = None;
        let mut noinline: Option<TokSpan> = None;

        for a in attrs {
            let Some(n) = a.name else { continue };
            let name = self.text_of(n).to_string();

            match name.as_str() {
                "inline" => inline = Some(a.span),
                "noinline" => noinline = Some(a.span),
                _ => {}
            }

            let declared = self.attr_decls.get(&name).cloned();
            let targets: Vec<String> = match (builtin_attr_targets(&name), &declared) {
                (Some(t), _) => t.iter().map(|s| (*s).to_string()).collect(),

                (None, Some((t, _))) => t.clone(),

                (None, None) => {
                    // An imported attribute keeps its targets in the
                    // module that declares it.
                    if !self.imported_names.contains(&name) {
                        let message = format!(
                            "no attribute named `{name}`; `attribute {name} on {target}` declares one"
                        );
                        self.diagnose(a.span, &message);
                    }

                    continue;
                }
            };

            if !targets.iter().any(|t| t == target) {
                let list: Vec<&str> = targets.iter().map(String::as_str).collect();
                let message = format!(
                    "the attribute `{name}` has no meaning on a {target}; it goes on {}",
                    list_names(&list)
                );
                self.diagnose(a.span, &message);

                continue;
            }

            self.check_attr_args(a, &name, declared.as_ref().map(|(_, p)| p.as_slice()));
        }

        if let (Some(_), Some(at)) = (inline, noinline) {
            self.diagnose(
                at,
                "the attributes `inline` and `noinline` ask for opposite things; keep one",
            );
        }
    }

    /// Whether an attribute reaches a target. `check_attrs` reports the
    /// ones that do not; the emit leaves them out so the artifact holds
    /// no name the source never bound.
    pub(crate) fn attr_reaches(&self, name: &str, target: &str) -> bool {
        match (builtin_attr_targets(name), self.attr_decls.get(name)) {
            (Some(t), _) => t.contains(&target),

            (None, Some((t, _))) => t.iter().any(|t| t == target),

            // An imported attribute keeps its targets in the module
            // that declares it; nothing here can say no.
            (None, None) => self.imported_names.contains(name),
        }
    }

    /// The arguments of one attribute: the count a built-in takes, and the
    /// count and the literal types a declared one takes.
    pub(crate) fn check_attr_args(
        &mut self,
        a: &Attr,
        name: &str,
        params: Option<&[(String, Option<String>)]>,
    ) {
        const NO_ARGS: &[&str] = &[
            "native",
            "checked",
            "inline",
            "noinline",
            "test",
            "sealed",
            "skip",
            "unreliable",
            "immediate",
            "u8",
            "u16",
            "u32",
            "i8",
            "i16",
            "i32",
            "f32",
        ];

        if NO_ARGS.contains(&name) && !a.args.is_empty() {
            let message = format!("the attribute `{name}` takes no argument");
            self.diagnose(a.span, &message);

            return;
        }

        if name == "deprecated" && a.args.len() > 1 {
            let message = format!(
                "the attribute `deprecated` takes one message, {} given",
                a.args.len()
            );
            self.diagnose(a.span, &message);

            return;
        }

        // Luau takes a string for `@deprecated` and reports anything
        // else on the declaration below, which is not the line the
        // author wrote it on.
        if name == "deprecated"
            && let Some(arg) = a.args.first()
            && let Some(got) = literal_kind(arg)
            && got != "string"
        {
            let message = format!("the attribute `deprecated` takes a string message, {got} given");
            self.diagnose(arg.span(), &message);

            return;
        }

        let Some(params) = params else {
            return;
        };

        if a.args.len() != params.len() {
            let message = format!(
                "the attribute `{name}` takes {} argument{}, {} given",
                params.len(),
                if params.len() == 1 { "" } else { "s" },
                a.args.len()
            );
            self.diagnose(a.span, &message);

            return;
        }

        for (arg, (pname, ty)) in a.args.iter().zip(params) {
            let Some(want) = ty.as_deref() else { continue };
            let Some(got) = literal_kind(arg) else {
                continue;
            };

            if want != got {
                let message =
                    format!("the attribute `{name}` takes {want} for `{pname}`, {got} given");
                self.diagnose(arg.span(), &message);
            }
        }
    }

    pub(crate) fn check_children_of(&mut self, children: Vec<Child<'_>>) {
        for child in children {
            match child {
                Child::Expr(e) => {
                    self.check_enum_member(e);
                    self.check_await(e);
                    self.check_children_of(expr_children(e));
                }

                Child::Block(b) => self.scan_static_checks(b),

                Child::Function(f) => self.scan_static_checks(&f.block),
            }
        }
    }

    /// `await 5`: the operand is a literal no Future can be.
    pub(crate) fn check_await(&mut self, e: &Expr) {
        let Expr::Await { operand, span } = e else {
            return;
        };
        let Some(kind) = literal_kind(operand) else {
            return;
        };
        let message = format!("`await` takes a Future or an awaitable, not a {kind}");
        self.diagnose(*span, &message);
    }

    /// `Shape.Triangle` on an enum the file declares: the member is a
    /// variant, a method the impl writes, or nothing at all.
    pub(crate) fn check_enum_member(&mut self, e: &Expr) {
        const BUILT_IN: &[&str] = &["is", "clone", "__index", "__tostring", "__eq", "__call"];

        let Expr::Index {
            object,
            key: IndexKey::Field(field),
            ..
        } = e
        else {
            return;
        };
        let Expr::Name(n) = object.as_ref() else {
            return;
        };
        let ename = self.text_of(*n).to_string();
        let Some(variants) = self.enum_decls.get(&ename) else {
            return;
        };
        let member = self.text_of(*field).to_string();

        if BUILT_IN.contains(&member.as_str())
            || variants.iter().any(|(v, _)| *v == member)
            || self
                .impl_methods
                .get(&ename)
                .is_some_and(|m| m.contains(&member))
        {
            return;
        }

        let names: Vec<&str> = variants.iter().map(|(v, _)| v.as_str()).collect();
        let message = format!(
            "`{ename}` has no variant `{member}`; its variants are {}",
            list_names(&names)
        );
        self.diagnose(*field, &message);
    }

    pub(crate) fn scan_expr_for_reduce(&mut self, e: &Expr) {
        if let Expr::Call {
            method: Some(m),
            args: CallArgs::Paren(args),
            ..
        } = e
            && self.text_of(*m) == "reduce"
            && let [Expr::Function { body, .. }, init] = args.as_slice()
            && body
                .params
                .first()
                .is_some_and(|p| p.ty.is_none() && p.destructure.is_none() && !p.is_vararg)
            && let Some(ty) = literal_type(init)
        {
            let at = self.byte_end(body.params[0].name);
            self.inserts.push((at, format!(": {ty}")));
        }

        self.scan_children(expr_children(e));
    }

    /// Records the primitive an extension method belongs to. A second
    /// primitive with the same method name leaves the target open.
    pub(crate) fn note_primitive(&mut self, name: &str, target: &str) {
        let open = matches!(self.ext_primitive.get(name), Some(Some(other)) if other != target);
        let value = match open {
            true => None,

            false => Some(target.to_string()),
        };

        match self.ext_primitive.get(name) {
            Some(None) => {}

            _ => {
                self.ext_primitive.insert(name.to_string(), value);
            }
        }
    }

    pub(crate) fn prescan(&mut self, block: &Block) {
        let declared: Vec<(bool, String, String)> = self
            .options
            .extensions
            .iter()
            .map(|e| (e.is_static, e.name.clone(), e.target.clone()))
            .collect();

        for (is_static, name, target) in declared {
            if is_static {
                self.ext_statics.entry(target).or_default().insert(name);
            } else {
                if PRIMITIVES.contains(&target.as_str()) {
                    self.note_primitive(&name, &target);
                }

                self.ext_methods.insert(name);
            }
        }

        let stmts: Vec<&Stmt> = block.stmts.iter().collect();
        self.prescan_stmts(&stmts);
    }

    /// The body of the prescan, over a list of statements. A namespace
    /// runs it again over its members, under the namespace's scope, so
    /// a member is indexed by the name the emit gives it.
    fn prescan_stmts(&mut self, stmts: &[&Stmt]) {
        for stmt in stmts {
            // A namespace member is a declaration of the file too. The
            // scope makes `decl_name` give it the namespace's prefix.
            if let Stmt::Namespace(ns) = stmt.under_default() {
                let key = crate::desugar::namespaces::key_of(
                    self.ns_stack.last().map(String::as_str),
                    self.text_of(ns.name),
                );
                let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();
                self.ns_stack.push(key);
                self.prescan_stmts(&inner);
                self.ns_stack.pop();
            }

            // `export default struct S` declares `S` like any other
            // top-level declaration; the prescan reads through it.
            let stmt = stmt.under_default();
            let returns_result = |body: &FunctionBody| {
                body.is_async.is_some()
                    && body
                        .ret_type
                        .is_some_and(|rt| self.text_of(rt).trim_start().starts_with("Result<"))
            };

            match stmt {
                Stmt::Function(f) if f.path.len() == 1 && returns_result(&f.body) => {
                    let name = self.decl_name(f.path[0]);
                    self.result_asyncs.insert(name);
                }

                Stmt::LocalFunction(f) if returns_result(&f.body) => {
                    let name = self.decl_name(f.name);
                    self.result_asyncs.insert(name);
                }

                _ => {}
            }

            if let Stmt::TypeAlias(t) = stmt
                && let Some((_, value)) = self.text_of(t.span).split_once('=')
                && value.trim_start().starts_with("Result")
            {
                self.result_aliases.insert(self.decl_name(t.name));
            }

            let declared = match stmt {
                Stmt::TypeAlias(t) => Some(t.name),

                Stmt::Struct(s) => Some(s.name),

                Stmt::Enum(e) => Some(e.name),

                Stmt::Interface(i) => Some(i.name),

                Stmt::Trait(t) => Some(t.name),

                _ => None,
            };

            if let Some(name) = declared {
                self.declared_types.insert(self.decl_name(name));
            }

            match stmt {
                Stmt::Attribute(a) => {
                    let name = self.text_of(a.name).to_string();
                    let targets: Vec<String> = a
                        .targets
                        .iter()
                        .map(|t| self.text_of(*t).to_string())
                        .collect();
                    let params: Vec<(String, Option<String>)> = a
                        .params
                        .iter()
                        .map(|p| {
                            (
                                self.text_of(p.name).to_string(),
                                p.ty.map(|t| self.text_of(t).trim().to_string()),
                            )
                        })
                        .collect();
                    self.attr_decls.insert(name, (targets, params));
                }

                Stmt::Import(i) => match &i.kind {
                    ImportKind::Namespace(n) | ImportKind::Default(n) => {
                        self.imported_names.insert(self.text_of(*n).to_string());
                    }

                    ImportKind::Both(n, specs) => {
                        self.imported_names.insert(self.text_of(*n).to_string());

                        for sp in specs {
                            let name = sp.alias.unwrap_or(sp.name);
                            self.imported_names.insert(self.text_of(name).to_string());
                        }
                    }

                    ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                        for sp in specs {
                            let name = sp.alias.unwrap_or(sp.name);
                            self.imported_names.insert(self.text_of(name).to_string());
                        }
                    }
                },

                Stmt::Enum(e) => {
                    let name = self.decl_name(e.name);
                    let variants: Vec<(String, usize)> = e
                        .variants
                        .iter()
                        .map(|v| (self.text_of(v.name).to_string(), v.payload.len()))
                        .collect();
                    self.enum_decls.insert(name, variants);
                }

                Stmt::Impl(i) => {
                    let target = self.impl_target_name(i.target);
                    let names: HashSet<String> = i
                        .methods
                        .iter()
                        .filter_map(|m| m.path.first().map(|n| self.text_of(*n).to_string()))
                        .collect();
                    self.impl_methods
                        .entry(target.clone())
                        .or_default()
                        .extend(names);

                    if i.methods.iter().any(|m| {
                        m.path
                            .first()
                            .is_some_and(|n| self.text_of(*n) == "to_string")
                    }) {
                        self.structs_with_to_string.insert(target.clone());
                    }

                    if i.methods
                        .iter()
                        .any(|m| m.visibility.is_some_and(|v| self.text_of(v) == "private"))
                    {
                        self.private_types.insert(target.clone());
                    }

                    // A generic struct's alias lists its methods, so the
                    // solver names the type argument. A trait impl adds
                    // methods this scan cannot see, and closes that door.
                    if i.trait_name.is_some() {
                        self.trait_impl_targets.insert(target.clone());
                    } else {
                        let methods: Vec<&alloy_syntax::ast::Function> = i
                            .methods
                            .iter()
                            .filter(|m| {
                                m.body
                                    .params
                                    .first()
                                    .is_some_and(|p| self.text_of(p.name) == "self")
                                    && !m.visibility.is_some_and(|v| self.text_of(v) == "private")
                            })
                            .collect();
                        let sigs: Vec<MethodSig> = methods
                            .iter()
                            .filter_map(|m| {
                                Some(MethodSig {
                                    name: *m.path.first()?,
                                    generics: m.body.generics,
                                    params: m
                                        .body
                                        .params
                                        .iter()
                                        .skip(1)
                                        .map(|p| (p.name, p.ty, p.is_vararg, p.default.is_some()))
                                        .collect(),
                                    ret: m.body.ret_type,
                                })
                            })
                            .collect();

                        // Every method the alias lists has to have a
                        // signature it can spell.
                        let listable = methods.len() == sigs.len()
                            && methods.iter().all(|m| {
                                m.body.is_async.is_none()
                                    && !m.body.has_bounds
                                    && m.body.params.iter().all(|p| p.destructure.is_none())
                            });

                        if listable {
                            let generics = i
                                .generics
                                .map(|g| strip_bounds(self.text_of(g)))
                                .unwrap_or_default();
                            self.impl_generics.insert(target.clone(), generics);
                            self.struct_methods
                                .entry(target.clone())
                                .or_default()
                                .extend(sigs);
                        } else {
                            self.trait_impl_targets.insert(target.clone());
                        }
                    }

                    if i.trait_name.is_none()
                        && let Some(ctor) = i.methods.iter().find_map(|m| {
                            m.path
                                .first()
                                .map(|n| self.text_of(*n))
                                .filter(|n| matches!(*n, "new" | "New"))
                        })
                    {
                        self.structs_with_new
                            .insert(target.clone(), ctor.to_string());
                    }

                    if self.is_foreign(&target) {
                        for m in &i.methods {
                            let name = self.text_of(m.path[0]).to_string();
                            let has_self = m
                                .body
                                .params
                                .first()
                                .map(|p| self.text_of(p.name) == "self")
                                .unwrap_or(false);

                            if has_self {
                                if PRIMITIVES.contains(&target.as_str()) {
                                    self.note_primitive(&name, &target);
                                }

                                self.ext_methods.insert(name);
                            } else {
                                self.ext_statics
                                    .entry(target.clone())
                                    .or_default()
                                    .insert(name);
                            }
                        }
                    }
                }

                Stmt::Macro(m) => {
                    let params: Vec<String> = m
                        .params
                        .iter()
                        .filter(|p| !p.is_vararg)
                        .map(|p| self.text_of(p.name).to_string())
                        .collect();
                    let variadic = m.params.iter().any(|p| p.is_vararg);
                    let name = self.text_of(m.name).to_string();
                    let body = self.join_tokens(m.body.span);
                    let tail = m.tail.as_ref().map(|t| self.join_tokens(t.span()));
                    self.macros.insert(
                        name,
                        MacroRef {
                            params,
                            variadic,
                            body,
                            tail,
                        },
                    );
                }

                Stmt::Trait(t) => {
                    let name = self.decl_name(t.name);
                    let defaults = t
                        .methods
                        .iter()
                        .filter(|m| m.body.is_some())
                        .map(|m| self.text_of(m.name).to_string())
                        .collect();
                    let required = t
                        .methods
                        .iter()
                        .filter(|m| m.body.is_none())
                        .map(|m| {
                            (
                                self.text_of(m.name).to_string(),
                                m.params.len(),
                                signature_ret_type(self.text_of(m.signature)).map(str::to_string),
                            )
                        })
                        .collect();
                    self.traits.insert(name.clone(), defaults);
                    self.trait_required.insert(name, required);
                }

                Stmt::Struct(st) => {
                    self.note_struct(st);
                }

                _ => {}
            }
        }

        // An imported enum is as exhaustible as one declared here; the
        // file names its variants the same way.
        for (name, variants) in &self.options.import_enums {
            if self.imported_names.contains(name) && !self.enums.contains_key(name) {
                self.enums.insert(name.clone(), variants.clone());
                self.enum_decls.insert(name.clone(), variants.clone());
            }
        }
    }

    /// A bound with each operator trait routed to the runtime type, unless
    /// the file declares a trait of that name.
    pub(crate) fn resolve_bound(&mut self, bound: &str) -> String {
        const BUILTIN: &[&str] = &[
            "Display",
            "Debug",
            "Clone",
            "Eq",
            "PartialEq",
            "Ord",
            "Add",
            "Sub",
            "Mul",
            "Div",
            "Serialize",
        ];
        let parts: Vec<String> = bound
            .split('&')
            .map(|part| {
                let part = part.trim();
                let name = part.split('<').next().unwrap_or(part).trim();

                if BUILTIN.contains(&name) && !self.traits.contains_key(name) {
                    format!("{}.{part}", self.std())
                } else {
                    part.to_string()
                }
            })
            .collect();

        parts.join(" & ")
    }

    /// The tokens of a span on one line, joined by spaces, comments gone.
    pub(crate) fn join_tokens(&self, span: TokSpan) -> String {
        let mut out = String::new();

        for i in span.start..span.end {
            let tok = self.toks[i as usize];

            if !out.is_empty() {
                out.push(' ');
            }

            out.push_str(tok.text(self.src));
        }

        out
    }

    /// A type that is not an Alloy struct or enum: an engine class, a
    /// datatype, or a primitive. Its metatable cannot take methods.
    pub(crate) fn is_foreign(&self, name: &str) -> bool {
        !self.structs.contains(name)
            && !self.enums.contains_key(name)
            && (INSTANCE_CLASSES.contains(&name)
                || DATATYPES.contains(&name)
                || PRIMITIVES.contains(&name)
                || name == "Instance")
    }

    pub(crate) fn chain_has_ext(&self, e: &Expr) -> bool {
        if self.ext_methods.is_empty() && self.ext_statics.is_empty() {
            return false;
        }

        let (base, links) = flatten(e);

        // The check artifact keeps a call on a class as written; only a
        // primitive needs the helper table rewrite.
        if self.options.check {
            let primitive_method = links.iter().any(|l| match l {
                Link::Plain(Step::Call {
                    method: Some(m), ..
                })
                | Link::Optional(Step::Call {
                    method: Some(m), ..
                }) => self.ext_primitive.contains_key(self.text_of(*m)),

                _ => false,
            });

            if primitive_method {
                return true;
            }

            return matches!(
                (base, links.first(), links.get(1)),
                (
                    Expr::Name(n),
                    Some(Link::Plain(Step::Field(f))),
                    Some(Link::Plain(Step::Call { method: None, .. })),
                ) if PRIMITIVES.contains(&self.text_of(*n))
                    && self
                        .ext_statics
                        .get(self.text_of(*n))
                        .is_some_and(|s| s.contains(self.text_of(*f)))
            );
        }

        if links.iter().any(|l| match l {
            Link::Plain(Step::Call {
                method: Some(m), ..
            })
            | Link::Optional(Step::Call {
                method: Some(m), ..
            }) => self.ext_methods.contains(self.text_of(*m)),

            _ => false,
        }) {
            return true;
        }

        // `Vector3.zero(...)`: a static on a foreign type.
        if let (
            Expr::Name(n),
            Some(Link::Plain(Step::Field(f))),
            Some(Link::Plain(Step::Call { method: None, .. })),
        ) = (base, links.first(), links.get(1))
        {
            let target = self.text_of(*n);

            if let Some(statics) = self.ext_statics.get(target)
                && statics.contains(self.text_of(*f))
            {
                return true;
            }
        }

        false
    }

    // --- structs -------------------------------------------------------------

    pub(crate) fn attribute_decl(&mut self, a: &AttributeDecl) {
        let name = self.text_of(a.name).to_string();
        let start = self.byte_start(a.span);

        if self.options.definitions {
            self.blank_lines(start, self.byte_end(a.span));

            return;
        }

        let targets: Vec<String> = a
            .targets
            .iter()
            .map(|t| luau_string(self.text_of(*t)))
            .collect();
        let params: Vec<String> = a
            .params
            .iter()
            .map(|p| luau_string(self.text_of(p.name)))
            .collect();
        let std = self.std();
        let value = format!(
            "{std}.attribute({}, {{ {} }}, {{ {} }})",
            luau_string(&name),
            targets.join(", "),
            params.join(", ")
        );
        // The check artifact types the value by its arguments, so
        // `Attributes.get(S, attr)` reads as `{ min: number, max: number }?`.
        let value = if self.options.check {
            let fields: Vec<String> = a
                .params
                .iter()
                .map(|p| {
                    let ty = match p.ty {
                        Some(t) => self.copy_type_to_string(t),

                        None => "any".to_string(),
                    };

                    format!("{}: {}", self.text_of(p.name), ty.trim())
                })
                .collect();

            format!(
                "({value} :: any) :: {std}.Attribute<{{ {} }}>",
                fields.join(", ")
            )
        } else {
            value
        };
        self.generate(start, &format!("local {name} = {value}"));
        self.blank_lines(start, self.byte_end(a.span));

        if a.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /*
    A function with attributes. Upstream attributes stay in front of it,
    in the bracket form when they carry arguments. `@test` makes the
    function local, registers it for the runner, and blanks it from the
    ship artifact. User attributes attach to the function value after its
    `end`, on the same line.
    */
    pub(crate) fn attributed_function(
        &mut self,
        span: TokSpan,
        attrs: &[Attr],
        body: &FunctionBody,
        name: Option<TokSpan>,
        exported: bool,
        is_local: bool,
    ) {
        let start = self.byte_start(span);
        let mut upstream = Vec::new();
        let mut user = Vec::new();
        let mut is_test = false;
        let mut cfg = None;

        for a in attrs {
            match a.name.map(|n| self.text_of(n)) {
                Some("test") => is_test = true,

                Some("cfg") => match self.cfg_condition(&a.args) {
                    Ok(cond) => cfg = Some((cond, self.text_of(a.span).to_string())),

                    Err(message) => self.diagnose(a.span, &message),
                },

                // Luau has no `@inline` or `@noinline`; the emit would
                // report an invalid attribute on the declaration's line.
                Some("inline" | "noinline") => {}

                // The count and the type checks report on the
                // attribute; the emit would report again, on the
                // declaration's line.
                Some("deprecated")
                    if a.args.len() > 1
                        || a.args
                            .first()
                            .and_then(literal_kind)
                            .is_some_and(|k| k != "string") => {}

                Some(n @ ("native" | "checked" | "deprecated")) => {
                    if a.args.is_empty() {
                        upstream.push(format!("@{n}"));
                    } else {
                        let args: Vec<String> =
                            a.args.iter().map(|e| self.render_to_string(e)).collect();
                        upstream.push(format!("@[{n}({})]", args.join(", ")));
                    }
                }

                None => upstream.push(self.text_of(a.span).to_string()),

                // An attribute that does not reach a function is
                // already a diagnostic. Its arguments are not values:
                // `@derive(Clone)` would write `Clone` into the attach
                // table, and the checker would call it an unknown global.
                Some(n) if !self.attr_reaches(n, "function") => {}

                Some(n) => {
                    let args: Vec<String> =
                        a.args.iter().map(|e| self.render_to_string(e)).collect();
                    user.push(format!("{n} = {{ {} }}", args.join(", ")));
                }
            }
        }

        // The declaration starts after the attributes.
        let first_tok = attrs.last().map(|a| a.span.end).unwrap_or(span.start);
        let decl_start = self.toks[first_tok as usize].start;
        let fname = name.map(|n| self.text_of(n).to_string());

        if is_test && !self.options.tests {
            self.ship_blanks.push((start, self.byte_end(span)));
        }

        let mut lead = upstream.join(" ");

        if !lead.is_empty() {
            lead.push(' ');
        }

        if ((is_test || exported) && !is_local) || std::mem::take(&mut self.ns_force_local) {
            lead.push_str("local ");
        }

        // The lead sits on the declaration's line, so the function and
        // its fold start there. An attribute on its own line keeps that
        // line, blank.
        self.blank_lines(start, decl_start);
        self.generate(decl_start, &lead);
        let rest = TokSpan::new(first_tok as usize, span.end as usize);

        // `@cfg(server)`: the function stays, typed as written, and its
        // body opens with the check. A shared module loads on both
        // sides, so the condition is read when the function runs.
        if let Some((cond, text)) = cfg {
            // An empty body puts the check behind the header; a body
            // with statements puts it before the first.
            let (at, pad) = if body.block.span.is_empty() {
                (self.toks[span.end as usize - 2].end, (" ", ""))
            } else {
                (self.byte_start(body.block.span), ("", " "))
            };
            let what = fname
                .as_deref()
                .map_or("this function".to_string(), |f| format!("`{f}`"));
            self.inserts.push((
                at,
                format!(
                    "{}if not ({cond}) then error({}, 2) end{}",
                    pad.0,
                    luau_string(&format!("{what} is {text} and cannot run here")),
                    pad.1
                ),
            ));
        }

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

        let _ = decl_start;
        let mut tail = String::new();

        if let Some(f) = &fname {
            if is_test {
                self.test_names.push((f.clone(), body.is_async.is_some()));

                if !self.options.tests {
                    let std = self.std();
                    tail.push_str(&format!(" {std}.test({}, {f})", luau_string(f)));
                }
            }

            if !user.is_empty() {
                let std = self.std();
                tail.push_str(&format!(" {std}.attach({f}, {{ {} }})", user.join(", ")));
            }

            if exported {
                self.exports.push((f.clone(), f.clone()));
            }
        }

        if !tail.is_empty() {
            let end = self.byte_end(span);
            self.generate(end, &tail);
        }
    }

    // --- macros ----------------------------------------------------------------

    /// The Luau of a `@cfg` condition: names the runtime reads, joined
    /// by `not`, `and`, `or`, or by `any(...)` and `all(...)`.
    pub(crate) fn cfg_condition(&mut self, args: &[Expr]) -> Result<String, String> {
        let [arg] = args else {
            return Err("`@cfg` takes one condition: `@cfg(server)`, `@cfg(not client)`, `@cfg(server and studio)`".to_string());
        };
        let std = self.std();

        self.cfg_expr(arg, std)
    }

    pub(crate) fn cfg_expr(&self, e: &Expr, std: &str) -> Result<String, String> {
        const FLAGS: &[&str] = &["server", "client", "studio", "edit", "running", "test"];

        match e {
            Expr::Name(t) => {
                let name = self.text_of(*t);

                if FLAGS.contains(&name) {
                    Ok(format!("{std}.cfg.{name}()"))
                } else {
                    Err(format!(
                        "`@cfg({name})`: the conditions are {}",
                        FLAGS.join(", ")
                    ))
                }
            }

            Expr::Paren { inner, .. } => Ok(format!("({})", self.cfg_expr(inner, std)?)),

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "not" => {
                Ok(format!("not {}", self.cfg_expr(operand, std)?))
            }

            Expr::Binary { op, lhs, rhs, .. } if matches!(self.text_of(*op), "and" | "or") => {
                Ok(format!(
                    "({} {} {})",
                    self.cfg_expr(lhs, std)?,
                    self.text_of(*op),
                    self.cfg_expr(rhs, std)?
                ))
            }

            Expr::Call {
                func,
                method: None,
                args: CallArgs::Paren(args),
                ..
            } if matches!(&**func, Expr::Name(f) if matches!(self.text_of(*f), "any" | "all")) => {
                let Expr::Name(f) = &**func else {
                    unreachable!()
                };
                let joiner = if self.text_of(*f) == "any" { " or " } else { " and " };
                let parts = args
                    .iter()
                    .map(|a| self.cfg_expr(a, std))
                    .collect::<Result<Vec<_>, _>>()?;

                if parts.is_empty() {
                    return Err(format!("`@cfg({}())` names no condition", self.text_of(*f)));
                }

                Ok(format!("({})", parts.join(joiner)))
            }

            _ => Err("`@cfg` takes names joined by `not`, `and`, `or`, `any(...)`, or `all(...)`: `@cfg(server and not studio)`".to_string()),
        }
    }

    /// Whether a plain function's parameter list carries `@attr`. The
    /// parser keeps the text; only a remote reads a wire width.
    pub(crate) fn params_have_attrs(&self, body: &FunctionBody) -> bool {
        !self.param_attr_spans(body).is_empty()
    }

    /// The `@name` texts in front of parameters, with their byte ranges.
    pub(crate) fn param_attr_spans(&self, body: &FunctionBody) -> Vec<(u32, u32, String)> {
        let open = self.toks[self.params_open_tok(body) as usize].end;
        let mut found = Vec::new();

        for (i, p) in body.params.iter().enumerate() {
            let from = if i == 0 {
                open
            } else {
                self.byte_end(body.params[i - 1].name)
            };
            let gap = &self.src[from as usize..self.byte_start(p.name) as usize];

            if let Some(at) = gap.find('@') {
                let name: String = gap[at + 1..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let start = from + at as u32;
                found.push((start, start + 1 + name.len() as u32, name));
            }
        }

        found
    }

    pub(crate) fn check_param_attrs(&mut self, body: &FunctionBody) {
        for (start, end, name) in self.param_attr_spans(body) {
            self.diagnostics.push(Diagnostic {
                start,
                end,
                message: format!(
                    "`@{name}` applies to a remote parameter or a struct field, not a function parameter"
                ),
            });
        }
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

    /// Luau takes a string for `@deprecated` and reports anything else
    /// on the declaration below, which is not the line the author wrote.
    #[test]
    fn a_deprecated_attribute_takes_a_string() {
        let got = messages(
            "@deprecated(7)\nlocal function old(): number\n    return 1\nend\nprint(old())\n",
        );
        assert_eq!(
            got,
            vec!["the attribute `deprecated` takes a string message, number given"]
        );
        assert!(
            messages("@deprecated(\"use `new`\")\nlocal function old(): number\n    return 1\nend\nprint(old())\n")
                .is_empty()
        );
    }

    #[test]
    fn an_enum_member_that_is_no_variant_reports() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\n    Empty\nend\nlocal nope = Shape.Triangle(1)\nprint(nope)\n";
        let got = messages(src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "`Shape` has no variant `Triangle`; its variants are `Circle`, `Rect` and `Empty`"
        );
    }

    #[test]
    fn an_enum_method_and_the_is_test_are_not_variants() {
        let src = "enum Shape as\n    Circle(number)\nend\nimpl Shape as\n    function area(self): number\n        return 1\n    end\nend\nprint(Shape.is(1), Shape.area)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));
    }

    #[test]
    fn an_attribute_checks_its_name_its_target_and_its_arguments() {
        let unknown = "@bogus\nlocal function one() end\nprint(one)\n";
        assert!(
            messages(unknown)
                .iter()
                .any(|m| m.starts_with("no attribute named `bogus`")),
            "{:?}",
            messages(unknown)
        );

        let target = "@derive(Clone)\nlocal function four() end\nprint(four)\n";
        assert!(
            messages(target)
                .iter()
                .any(|m| m == "the attribute `derive` has no meaning on a function; it goes on `struct` and `enum`"),
            "{:?}",
            messages(target)
        );

        let both = "@inline\n@noinline\nlocal function seven() end\nprint(seven)\n";
        assert!(
            messages(both).iter().any(|m| m.contains("opposite things")),
            "{:?}",
            messages(both)
        );

        let count = "@deprecated(1, 2, 3)\nlocal function six() end\nprint(six)\n";
        assert!(
            messages(count)
                .iter()
                .any(|m| m == "the attribute `deprecated` takes one message, 3 given"),
            "{:?}",
            messages(count)
        );

        let wrong =
            "attribute tag(name: string) on struct\n@tag(5)\nstruct S as x: number end\nprint(S)\n";
        assert!(
            messages(wrong)
                .iter()
                .any(|m| m == "the attribute `tag` takes string for `name`, number given"),
            "{:?}",
            messages(wrong)
        );

        let fine = "attribute range(min: number, max: number) on field\nstruct S as\n    @range(0, 1)\n    x: number\nend\nprint(S)\n";
        assert!(messages(fine).is_empty(), "{:?}", messages(fine));
    }

    #[test]
    fn inline_and_noinline_never_reach_the_emit() {
        let out = crate::compile(
            "@inline\nlocal function small(): number\n    return 1\nend\nprint(small())\n",
        )
        .unwrap();
        assert!(!out.ship.contains("@inline"), "{}", out.ship);
        assert!(!out.check.contains("@inline"), "{}", out.check);
    }

    #[test]
    fn await_on_a_literal_reports() {
        let src = "local bad = await 5\nprint(bad)\n";
        assert!(
            messages(src)
                .iter()
                .any(|m| m == "`await` takes a Future or an awaitable, not a number"),
            "{:?}",
            messages(src)
        );
    }
}
