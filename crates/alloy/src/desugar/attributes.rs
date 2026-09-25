//! Attribute checks, cfg, and the prescan that gathers names other
//! phases route through.

use std::collections::HashSet;

use alloy_syntax::ast::{
    Attr, AttributeDecl, Block, CallArgs, Expr, FunctionBody, ImportKind, IndexKey, Stmt, TokSpan,
};

use crate::roblox_classes::{DATATYPES, INSTANCE_CLASSES};

use super::contracts::{Owner, is_list_type};
use super::types::{literal_kind, literal_type, strip_bounds};
use super::*;

/// The traits the std declares, which a bound may name without the
/// file declaring one. `resolve_bound` routes them to the std table.
pub(crate) const BUILTIN_BOUNDS: &[&str] = &[
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

/// The targets a built-in attribute takes, or `None` when the name is
/// not one. The list mirrors `builtin_attribute_targets` in alloy-lsp.
pub(crate) fn builtin_attr_targets(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "derive" | "sealed" => &["struct", "enum"],

        "cfg" => &["function", "local", "namespace"],

        "deprecated" => &["function", "namespace"],

        // `@test` on a namespace makes every public function of the
        // group a test, nested public namespaces included.
        "test" => &["function", "namespace"],

        "native" | "checked" | "inline" | "noinline" => &["function"],

        "unreliable" | "ratelimit" | "timeout" | "validate" | "immediate" | "wire" => &["remote"],

        "u8" | "u16" | "u32" | "i8" | "i16" | "i32" | "f32" => &["param", "field"],

        "rename" | "skip" | "alias" => &["field"],

        // serde's container options, on the struct the derive reads.
        "deny_unknown_fields" | "rename_all" => &["struct"],

        // `@allow(too_many_arguments)` quiets a lint over what it sits
        // on, the way Rust's `#[allow]` does.
        "allow" => &[
            "function",
            "local",
            "struct",
            "enum",
            "namespace",
            "trait",
            "interface",
            "impl",
            "remote",
            "type",
            "field",
            "variant",
        ],

        _ => return None,
    })
}

/// The key an attribute's data takes in the table the runtime reads.
/// A use through a path, `@Ns.tag`, names the same attribute as the
/// bare `tag`, and `alloy.attribute` records the name the declaration
/// wrote.
///
/// ponytail: one key per bare name, so two namespaces that declare the
/// same name share it. A canonical path per attribute is the fix if a
/// file ever needs both on one declaration.
pub(crate) fn attr_key(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((_, last)) => last,

        None => name,
    }
}

/// Whether a block hands a value back: a `return` with values in it.
/// A nested function keeps its own returns, so the walk stops at one.
fn block_returns_value(block: &Block) -> bool {
    block.stmts.iter().any(stmt_returns_value)
}

fn stmt_returns_value(s: &Stmt) -> bool {
    match s {
        Stmt::Return(r) => !r.values.is_empty(),

        _ => stmt_children(s).iter().any(child_returns_value),
    }
}

fn child_returns_value(c: &Child<'_>) -> bool {
    match c {
        Child::Block(b) => block_returns_value(b),

        Child::Expr(e) => expr_children(e).iter().any(child_returns_value),

        Child::Function(_) => false,
    }
}

/// The return type a trait method's signature declares. The signature
/// starts at `(`; the type follows the closing `)` after a `:` or a
/// `->`.
pub fn signature_ret_type(sig: &str) -> Option<&str> {
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

            Stmt::TypeAlias(t) => self.check_attrs(&t.attributes, "type"),

            Stmt::Struct(st) => {
                self.check_attrs(&st.attributes, "struct");
                let name = self.text_of(st.name).to_string();
                let members = self.type_members_of(&self.decl_name(st.name));
                self.check_contracts(
                    &st.attributes,
                    Owner {
                        target: "struct",
                        name: &name,
                        body: st.span,
                    },
                    &members,
                );
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
                let name = self.text_of(e.name).to_string();
                let members = self.type_members_of(&self.decl_name(e.name));
                self.check_contracts(
                    &e.attributes,
                    Owner {
                        target: "enum",
                        name: &name,
                        body: e.span,
                    },
                    &members,
                );

                for v in &e.variants {
                    self.check_attrs(&v.attributes, "variant");
                }
            }

            // A namespace takes attributes, and so does each member.
            Stmt::Namespace(ns) => {
                self.check_attrs(&ns.attributes, "namespace");
                let name = self.text_of(ns.name).to_string();
                let members = self.namespace_members(ns);
                self.check_contracts(
                    &ns.attributes,
                    Owner {
                        target: "namespace",
                        name: &name,
                        body: ns.span,
                    },
                    &members,
                );

                // A member reads a sibling attribute by its bare
                // name, so the walk carries the namespace.
                let key = crate::desugar::namespaces::key_of(
                    self.ns_stack.last().map(|f| f.key.as_str()),
                    &name,
                );
                self.ns_stack.push(crate::desugar::namespaces::NsFrame {
                    key,
                    scope: self.scope_depth(),
                });

                for m in &ns.members {
                    self.check_stmt_attrs(&m.stmt);
                }

                self.ns_stack.pop();
            }

            Stmt::Trait(t) => {
                self.check_attrs(&t.attributes, "trait");

                // A trait method emits no function of its own to carry an
                // attribute; `@allow` reads the source, so it goes on one.
                for a in t.methods.iter().flat_map(|m| &m.attributes) {
                    let name = a.name.map(|n| self.text_of(n).to_string());

                    match name.as_deref() {
                        Some("allow") => self.check_attr_args(a, "allow", None),

                        _ => self.diagnose(
                            a.span,
                            "a trait method takes `@allow` alone; put another attribute on the method of the `impl`",
                        ),
                    }
                }
                let name = self.text_of(t.name).to_string();
                let members = self.trait_members(t);
                self.check_contracts(
                    &t.attributes,
                    Owner {
                        target: "trait",
                        name: &name,
                        body: t.span,
                    },
                    &members,
                );
            }

            Stmt::Interface(i) => {
                self.check_attrs(&i.attributes, "interface");
                let name = self.text_of(i.name).to_string();
                let members = self.type_members_of(&self.decl_name(i.name));
                self.check_contracts(
                    &i.attributes,
                    Owner {
                        target: "interface",
                        name: &name,
                        body: i.span,
                    },
                    &members,
                );
            }

            // An `impl` carries its attributes on the block. The target
            // names the owner, because that is what a report should say.
            Stmt::Impl(i) => {
                self.check_attrs(&i.attributes, "impl");
                let name = self.impl_target_name(i.target);
                let members = self.type_members_of(&name);
                self.check_contracts(
                    &i.attributes,
                    Owner {
                        target: "impl",
                        name: &name,
                        body: i.span,
                    },
                    &members,
                );
            }

            Stmt::Attribute(a) => self.check_attribute_decl(a),

            _ => {}
        }
    }

    /// One attribute list: every name is declared, takes this target, and
    /// carries the arguments it declares.
    pub(crate) fn check_attrs(&mut self, attrs: &[Attr], target: &str) {
        let mut inline: Option<TokSpan> = None;
        let mut noinline: Option<TokSpan> = None;

        for a in attrs {
            let Some(n) = a.name else {
                self.check_luau_attr_list(a, target);

                continue;
            };
            let name = self.attr_name(n);

            // serde's options live in `@alloy/std/serde`, as the derives
            // that read them do. A path through a star import reaches
            // one; a bare name needs its import.
            if self.text_of(n) == name
                && crate::std_names::is_std_attribute(&name)
                && !self.options.std_globals.ambient(&name)
                && !self.std_imports.contains(&name)
                && self.std_reported.insert(name.clone())
            {
                self.diagnose(n, &crate::std_names::missing_message(&name));
            }

            match name.as_str() {
                "inline" => inline = Some(a.span),
                "noinline" => noinline = Some(a.span),
                _ => {}
            }

            // `@Ns.tag` reads an attribute of a namespace. A path that
            // reaches none reports here and asks no more of the name.
            if let Some(message) = self.attr_path_error(&name) {
                self.diagnose(a.span, &message);

                continue;
            }

            let declared = self.attr_decl_of(&name).cloned();
            let targets: Vec<String> = match (builtin_attr_targets(&name), &declared) {
                (Some(t), _) => t.iter().map(|s| (*s).to_string()).collect(),

                (None, Some(d)) => d.targets.clone(),

                (None, None) => {
                    // An imported attribute keeps its targets in the
                    // module that declares it.
                    if !self.imported_names.contains(&name) && !self.star_path(&name) {
                        // A namespace holds the name, so the report
                        // names the path that reaches it.
                        let message = self
                            .ns_member_hint(&self.attr_decls, &name, "an attribute", '@')
                            .unwrap_or_else(|| {
                                format!(
                                    "no attribute named `{name}`; `attribute {name} on {target}` declares one"
                                )
                            });
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

            self.check_attr_args(a, &name, declared.as_ref());
        }

        if let (Some(_), Some(at)) = (inline, noinline) {
            self.diagnose(
                at,
                "the attributes `inline` and `noinline` ask for opposite things; keep one",
            );
        }
    }

    /// `@[native, deprecated {use = "f", reason = "..."}]`: Luau's own
    /// attribute list, which Alloy passes through. It is valid Luau, so
    /// a Luau file stays valid Alloy; the list takes Luau's attributes
    /// alone, on a function, with the arguments Luau accepts.
    fn check_luau_attr_list(&mut self, a: &Attr, target: &str) {
        const LUAU: &[&str] = &["checked", "native", "deprecated"];

        if target != "function" {
            let message = format!(
                "Luau's attribute list goes on a function, and this is a {target}; write the Alloy form, `@name`"
            );
            self.diagnose(a.span, &message);

            return;
        }

        let (start, end) = (a.span.start as usize, a.span.end as usize);
        let text = |i: usize| self.toks[i].text(self.src);
        // Inside `@[` and before the closing `]`.
        let mut i = start + 2;
        let close = end.saturating_sub(1);
        let mut seen: Vec<String> = Vec::new();

        if i >= close {
            self.diagnose(a.span, "Luau's attribute list cannot be empty: `@[native]`");

            return;
        }

        while i < close {
            let name = text(i).to_string();
            let at = TokSpan::new(i, i + 1);
            i += 1;

            // The argument: a string, a table, or a parenthesized list.
            let arg_start = i;

            if matches!(text(i), "{" | "(") {
                let mut depth = 0i32;

                while i < close {
                    match text(i) {
                        "{" | "(" | "[" => depth += 1,

                        "}" | ")" | "]" => depth -= 1,

                        _ => {}
                    }

                    i += 1;

                    if depth == 0 {
                        break;
                    }
                }
            } else if matches!(self.toks[i].kind, TokKind::Str { .. }) {
                i += 1;
            }

            let arg = TokSpan::new(arg_start, i);

            if !LUAU.contains(&name.as_str()) {
                let message = match builtin_attr_targets(&name).is_some()
                    || self.attr_decl_of(&name).is_some()
                {
                    true => format!(
                        "`@[{name}]` is Luau's attribute list, which takes `checked`, `native`, and `deprecated`; write `@{name}` for the Alloy attribute"
                    ),

                    false => format!(
                        "Luau has no attribute `{name}`; its list takes `checked`, `native`, and `deprecated`"
                    ),
                };
                self.diagnose(at, &message);
            } else if seen.contains(&name) {
                self.diagnose(at, &format!("`{name}` is in this list twice; keep one"));
            } else if name != "deprecated" && !arg.is_empty() {
                self.diagnose(arg, &format!("`{name}` takes no argument"));
            } else if name == "deprecated" && !arg.is_empty() {
                self.check_deprecated_table(arg);
            }

            seen.push(name);

            if text(i) == "," {
                i += 1;
            } else if i < close {
                self.diagnose(
                    TokSpan::new(i, i + 1),
                    "Luau's attribute list separates its entries with `,`",
                );

                return;
            }
        }
    }

    /// The argument of `deprecated` in Luau's list: one table of string
    /// constants under `use` and `reason`, as Luau checks it.
    fn check_deprecated_table(&mut self, arg: TokSpan) {
        let (mut start, mut end) = (arg.start as usize, arg.end as usize);
        let text = |i: usize| self.toks[i].text(self.src);

        // `deprecated({ ... })` is the call form of the same table, and
        // Luau reads both.
        if end - start >= 2 && text(start) == "(" && text(end - 1) == ")" {
            start += 1;
            end -= 1;
        }

        if text(start) != "{" || text(end - 1) != "}" {
            self.diagnose(
                arg,
                "`deprecated` takes a table: `@[deprecated {use = \"new_name\", reason = \"...\"}]`",
            );

            return;
        }

        let mut i = start + 1;

        while i < end - 1 {
            let key = text(i).to_string();

            if !matches!(key.as_str(), "use" | "reason") || text(i + 1) != "=" {
                self.diagnose(
                    TokSpan::new(i, i + 1),
                    "`deprecated` takes the keys `use` and `reason`, each a string",
                );

                return;
            }

            if !matches!(self.toks[i + 2].kind, TokKind::Str { .. }) {
                let message = format!("`{key}` takes a string constant");
                self.diagnose(TokSpan::new(i + 2, i + 3), &message);

                return;
            }

            i += 3;

            if matches!(text(i), "," | ";") {
                i += 1;
            }
        }
    }

    /// `@allow(name, ...)`: each argument names a lint, a group, or a
    /// lint under its tool, `flux.too_many_arguments`. `luau.` names a
    /// checker kind and another prefix an ingot's lint, which the
    /// compiler cannot list, so those pass.
    fn check_allow_args(&mut self, a: &Attr) {
        if a.args.is_empty() {
            self.diagnose(
                a.span,
                "`@allow` names the lints it quiets: `@allow(too_many_arguments)`",
            );

            return;
        }

        for arg in &a.args {
            let written: String = self.text_of(arg.span()).split_whitespace().collect();

            match crate::directives::allow_check(&written) {
                Ok(()) => {}

                Err(message) => self.diagnose(arg.span(), &message),
            }
        }
    }

    /// The name an attribute use reads. `@serde.rename` through
    /// `import * as serde from "@alloy/std/serde"` is the std's `rename`.
    pub(crate) fn attr_name(&self, n: TokSpan) -> String {
        let text = self.text_of(n);

        self.std_member(text, crate::std_names::attribute_module)
            .unwrap_or(text)
            .to_string()
    }

    /// A derive's name, read through a star import of the std the same
    /// way: `@derive(serde.Serialize)` derives `Serialize`.
    pub(crate) fn derive_name(&self, arg: &Expr) -> String {
        let text = self.text_of(arg.span());

        self.std_member(text, crate::std_names::module_of)
            .unwrap_or(text)
            .to_string()
    }

    /// The member of `alias.member` when `alias` is a star import of the
    /// std module that holds it, or of the facade.
    fn std_member<'t>(
        &self,
        text: &'t str,
        home: fn(&str) -> Option<&'static str>,
    ) -> Option<&'t str> {
        let (head, member) = text.split_once('.')?;
        let module = self.std_namespaces.get(head)?;
        let at = home(member)?;

        (module.is_empty() || module == at).then_some(member)
    }

    /// Whether a name is a path through a star import of a module,
    /// `@M.tag`. The module keeps the attribute's targets.
    fn star_path(&self, name: &str) -> bool {
        name.split_once('.')
            .is_some_and(|(head, _)| self.star_modules.contains(head))
    }

    /// The declaration one attribute name reads. A member of the
    /// namespace under render wins over a name of the file, and a
    /// path names the member it writes.
    pub(crate) fn attr_decl_of(&self, name: &str) -> Option<&AttrDecl> {
        self.scoped_decl(&self.attr_decls, name)
    }

    /// The template one macro name reads, under the same rule as an
    /// attribute.
    pub(crate) fn macro_of(&self, name: &str) -> Option<&MacroRef> {
        self.scoped_decl(&self.macros, name)
    }

    /// The report a dotted attribute name earns, when the path reaches
    /// no attribute. `None` for a bare name and for a path that
    /// resolves.
    fn attr_path_error(&self, name: &str) -> Option<String> {
        let (owner, member) = name.rsplit_once('.')?;

        if self.attr_decl_of(name).is_some() {
            return None;
        }

        let head = owner.split('.').next().unwrap_or(owner);

        // `import * as M`: `@M.tag` names the module's own attribute.
        if self.star_path(name) {
            return None;
        }

        // A named import binds a module's attribute under its own name.
        if self.imported_names.contains(head) {
            return Some(format!(
                "an attribute of a module is used by its bare name; import it with `import {{ {member} }} from ...`"
            ));
        }

        Some(match self.is_namespace_path(owner) {
            true => format!("`{owner}` declares no attribute `{member}`"),

            false => format!("`{owner}` is no namespace, so `{name}` names no attribute"),
        })
    }

    /// Whether an attribute reaches a target. `check_attrs` reports the
    /// ones that do not; the emit leaves them out so the artifact holds
    /// no name the source never bound.
    pub(crate) fn attr_reaches(&self, name: &str, target: &str) -> bool {
        match (builtin_attr_targets(name), self.attr_decl_of(name)) {
            (Some(t), _) => t.contains(&target),

            (None, Some(d)) => d.targets.iter().any(|t| t == target),

            // An imported attribute keeps its targets in the module
            // that declares it; nothing here can say no.
            (None, None) => self.imported_names.contains(name) || self.star_path(name),
        }
    }

    /// The arguments a use of an attribute carries, as Luau. A parameter
    /// the use leaves out takes the default its declaration writes.
    // ponytail: a default is its source text; an Alloy literal, `[1]`,
    // needs the renderer.
    pub(crate) fn attr_args(&mut self, a: &Attr, name: &str) -> Vec<String> {
        let mut args: Vec<String> = a.args.iter().map(|e| self.render_to_string(e)).collect();
        let defaults = self
            .attr_decl_of(name)
            .map(|d| d.defaults.clone())
            .unwrap_or_default();

        for default in defaults.iter().skip(args.len()) {
            let Some(default) = default else { break };
            args.push(default.clone());
        }

        args
    }

    /// The arguments of one attribute: the count a built-in takes, and the
    /// count and the literal types a declared one takes.
    pub(crate) fn check_attr_args(&mut self, a: &Attr, name: &str, decl: Option<&AttrDecl>) {
        const NO_ARGS: &[&str] = &[
            "native",
            "checked",
            "inline",
            "noinline",
            "test",
            "sealed",
            "skip",
            "deny_unknown_fields",
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

        if name == "allow" {
            self.check_allow_args(a);

            return;
        }

        // `@wire(buffer)` or `@wire(table)`: how a remote's payload
        // travels.
        if name == "wire" {
            let arg: Vec<&str> = a.args.iter().map(|x| self.text_of(x.span())).collect();

            if !matches!(arg.as_slice(), ["buffer"] | ["table"]) {
                self.diagnose(
                    a.span,
                    "`@wire` takes `buffer` or `table`: `@wire(buffer)` packs the payload, `@wire(table)` sends it as Roblox encodes a table",
                );
            }

            return;
        }

        // `@rename_all("camelCase")` takes one style, and `@alias`
        // takes one key or more, each a string.
        if name == "rename_all" || name == "alias" {
            let strings: Vec<Option<String>> = a
                .args
                .iter()
                .map(|e| crate::data::literal_text(self.text_of(e.span())))
                .collect();
            let styles = super::structs::RENAME_STYLES;
            let message = match name {
                "rename_all" => match strings.as_slice() {
                    [Some(style)] if styles.contains(&style.as_str()) => None,

                    _ => Some(format!(
                        "`@rename_all` takes one style: {}",
                        styles
                            .iter()
                            .map(|s| format!("\"{s}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                },

                _ if strings.is_empty() || strings.iter().any(Option::is_none) => {
                    Some("`@alias` takes the other keys as strings: `@alias(\"hp\")`".to_string())
                }

                _ => None,
            };

            if let Some(message) = message {
                self.diagnose(a.span, &message);
            }

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

        // `@deprecated("why")`, or the table Luau's own list takes,
        // `@deprecated({ use = "f", reason = "why" })`. Luau reports any
        // other argument on the declaration below, which is not the line
        // the author wrote it on.
        if name == "deprecated"
            && let Some(arg) = a.args.first()
        {
            if matches!(arg, Expr::Table { .. }) {
                self.check_deprecated_table(arg.span());

                return;
            }

            if let Some(got) = literal_kind(arg)
                && got != "string"
            {
                let message = format!(
                    "the attribute `deprecated` takes a string message or a table of `use` and `reason`, {got} given"
                );
                self.diagnose(arg.span(), &message);

                return;
            }
        }

        let Some(decl) = decl else {
            return;
        };
        let params = decl.params.as_slice();
        // The arguments are positional, so a default makes its parameter
        // optional only when every parameter after it has one too.
        let required = decl
            .defaults
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |i| i + 1);

        if a.args.len() > params.len() || a.args.len() < required {
            let count = if required == params.len() {
                format!(
                    "{} argument{}",
                    params.len(),
                    if params.len() == 1 { "" } else { "s" }
                )
            } else {
                format!("{required} to {} arguments", params.len())
            };
            let message = format!(
                "the attribute `{name}` takes {count}, {} given",
                a.args.len()
            );
            self.diagnose(a.span, &message);

            return;
        }

        let params: Vec<(String, Option<String>)> = params.to_vec();

        for (arg, (pname, ty)) in a.args.iter().zip(&params) {
            let Some(want) = ty.as_deref() else { continue };

            // A list parameter carries entries, and the element type
            // says what each one takes.
            if is_list_type(want) {
                self.check_argument_entries(name, pname, want, arg);

                continue;
            }

            // A narrowed type names the values it takes. The literal is
            // one of them, or the report names the one the file wrote.
            if !self.admitted_values(want).is_empty() {
                self.check_argument_entries(name, pname, want, arg);

                continue;
            }

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

        // The methods of an imported enum stay in the module that
        // declares it: `project_impls` carries only the impls other
        // files add. The checker reads them off the import's type, so a
        // member of an imported enum is its report, not this one. A
        // macro body sees the enums of the file it expands in the same
        // way, without their impls.
        let elsewhere = self
            .options
            .import_enums
            .iter()
            .chain(&self.options.macro_enums)
            .any(|(n, _)| *n == ename);

        if elsewhere {
            return;
        }

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

    /// The trait each parameter of a bounded signature asks of its
    /// argument, by the parameter's place. `largest<T: Ord>(xs: { T })`
    /// asks `Ord` at its first place. `None` when the signature writes
    /// no bound, or no parameter takes the bounded type.
    fn param_bounds(&self, body: &FunctionBody) -> Option<Vec<Option<String>>> {
        if !body.has_bounds {
            return None;
        }

        let bounds = super::types::generic_bounds(self.text_of(body.generics?));
        let asks: Vec<Option<String>> = body
            .params
            .iter()
            .map(|p| {
                let ty = p.ty.map(|t| self.text_of(t)).unwrap_or_default();
                let ty = ty.trim().trim_start_matches(':').trim();
                let head = super::types::array_element(ty).unwrap_or(ty);
                let head = head.trim().trim_end_matches('?');

                bounds
                    .iter()
                    .find(|(n, _)| n == head)
                    .map(|(_, b)| b.clone())
            })
            .collect();

        asks.iter().any(Option::is_some).then_some(asks)
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
                    self.ns_stack.last().map(|f| f.key.as_str()),
                    self.text_of(ns.name),
                );
                let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();
                self.ns_stack.push(crate::desugar::namespaces::NsFrame {
                    key,
                    scope: self.scope_depth(),
                });
                self.prescan_stmts(&inner);
                self.ns_stack.pop();
            }

            // `export default struct S` declares `S` like any other
            // top-level declaration; the prescan reads through it.
            let stmt = stmt.under_default();

            if let Stmt::Struct(s) = stmt {
                let derives = |which: &str| {
                    s.attributes.iter().any(|a| {
                        a.name.is_some_and(|n| self.text_of(n) == "derive")
                            && a.args.iter().any(|x| self.derive_name(x) == which)
                    })
                };
                let (ser, de) = (derives("Serialize"), derives("Deserialize"));
                let (clone, default) = (derives("Clone"), derives("Default"));
                let name = self.decl_name(s.name);

                if clone {
                    self.cloneable.insert(name.clone());
                }

                if default {
                    self.defaultable.insert(name.clone());
                }

                if ser {
                    self.serializable.insert(name.clone());
                }

                if de {
                    self.deserializable.insert(name);
                }
            }
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

            // The declared return type of each function of this file,
            // for the `try` check.
            let signature = match stmt {
                Stmt::Function(f) if f.path.len() == 1 => Some((f.path[0], &f.body)),

                Stmt::LocalFunction(f) => Some((f.name, &f.body)),

                _ => None,
            };

            if let Some((name, body)) = signature
                && let Some(rt) = body.ret_type
            {
                let ty = self.text_of(rt).trim().trim_start_matches(':').trim();
                let key = self.decl_name(name);

                if body.is_async.is_none() {
                    self.plain_fns.insert(key.clone());
                }

                self.fn_ret_types.insert(key, ty.to_string());
            }

            // `<T: Ord>`: the trait each parameter asks of its argument.
            // A call reads it back and names an argument that has no
            // `impl` of that trait.
            if let Some((name, body)) = signature
                && let Some(asks) = self.param_bounds(body)
            {
                let key = self.decl_name(name);
                self.fn_bounds.insert(key, asks);
            }

            if let Stmt::TypeAlias(t) = stmt
                && let Some((_, value)) = self.text_of(t.span).split_once('=')
            {
                let value = value.trim();
                let name = self.decl_name(t.name);

                if value.starts_with("Result") {
                    self.result_aliases.insert(name.clone());
                }

                // The value lands here, not in the statement walk, so
                // `is` reads through an alias declared below the use.
                // See `alias_head`.
                self.alias_values.insert(name, value.to_string());
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
                    let defaults: Vec<Option<String>> = a
                        .params
                        .iter()
                        .map(|p| {
                            p.default
                                .as_ref()
                                .map(|d| self.text_of(d.span()).to_string())
                        })
                        .collect();
                    let requires: Vec<Require> =
                        a.requires.iter().map(|c| self.require_of(c)).collect();
                    let decl = AttrDecl {
                        targets,
                        params,
                        defaults,
                        requires,
                    };

                    // A member of a namespace is keyed by its path,
                    // `Testing.tag`, so a file-level `tag` stays its
                    // own declaration. `ns_scope_keys` reads the path
                    // back for a bare name inside the body.
                    let key = self.ns_member_path(&name).unwrap_or(name);
                    // The kind under the bare name the statement walk
                    // uses, so `is` refuses the name above the
                    // declaration too, and under the path, so
                    // `x is Net.tag` refuses it as well. See
                    // `no_nominal_test`.
                    self.not_constructible
                        .insert(self.text_of(a.name).to_string(), "attribute");
                    self.not_constructible.insert(key.clone(), "attribute");
                    self.attr_decls.insert(key, decl);
                }

                // The same, for a channel. The prescan reads nothing
                // else off a remote.
                Stmt::Remote(r) => {
                    self.not_constructible
                        .insert(self.decl_name(r.name), "remote");
                }

                Stmt::Import(i) => match &i.kind {
                    ImportKind::Default(n) => {
                        self.imported_names.insert(self.text_of(*n).to_string());
                    }

                    ImportKind::Namespace(n, specs) => {
                        let module = self.text_of(*n).to_string();
                        self.star_modules.insert(module.clone());
                        self.imported_names.insert(module);
                        self.note_specs(specs);
                    }

                    ImportKind::Both(n, specs) => {
                        self.imported_names.insert(self.text_of(*n).to_string());
                        self.note_specs(specs);
                    }

                    ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                        self.note_specs(specs);
                    }
                },

                // A function body reads an enum declared below it, and
                // its `match` covers the variants before the
                // declaration fills them.
                Stmt::Enum(e) => {
                    let name = self.decl_name(e.name);
                    let variants: Vec<(String, usize)> = e
                        .variants
                        .iter()
                        .map(|v| (self.text_of(v.name).to_string(), v.payload.len()))
                        .collect();
                    self.enums.insert(name.clone(), variants.clone());
                    // An `impl` above the enum writes onto a nil table.
                    let at = self.byte_start(e.span);
                    self.struct_at.entry(name.clone()).or_insert(at);
                    self.enum_decls.insert(name, variants);
                }

                Stmt::Impl(i) => {
                    let target = self.impl_target_name(i.target);

                    // A bound on a method's own generic asks the same of
                    // its argument as a bound on a free function. The
                    // target and the name key it, so a method of another
                    // impl with the same name stays its own.
                    for m in &i.methods {
                        if let Some(name) = m.path.first()
                            && let Some(asks) = self.param_bounds(&m.body)
                        {
                            let key = (target.clone(), self.text_of(*name).to_string());
                            self.method_bounds.insert(key, asks);
                        }
                    }

                    let names: HashSet<String> = i
                        .methods
                        .iter()
                        .filter_map(|m| m.path.first().map(|n| self.text_of(*n).to_string()))
                        .collect();
                    self.impl_methods
                        .entry(target.clone())
                        .or_default()
                        .extend(names);
                    let members = self.impl_block_members(i);
                    self.method_body.entry(target.clone()).or_insert(i.span);
                    self.type_members
                        .entry(target.clone())
                        .or_default()
                        .extend(members);

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
                    if let Some(t) = i.trait_name {
                        let met = self.text_of(t).to_string();
                        self.impl_traits
                            .entry(target.clone())
                            .or_default()
                            .push(met);
                    }

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
                    let defaults: Vec<Option<String>> = m
                        .params
                        .iter()
                        .filter(|p| !p.is_vararg)
                        .map(|p| {
                            p.default
                                .as_ref()
                                .map(|d| self.text_of(d.span()).to_string())
                        })
                        .collect();
                    let patterns: Vec<Vec<(String, String)>> = m
                        .params
                        .iter()
                        .filter(|p| !p.is_vararg)
                        .map(|p| super::pattern_accesses(p, |t| self.text_of(t).to_string()))
                        .collect();
                    let variadic = m.params.iter().any(|p| p.is_vararg);
                    let name = self.text_of(m.name).to_string();
                    let body = self.join_tokens(m.body.span);
                    let tail = m.tail.as_ref().map(|t| self.join_tokens(t.span()));
                    let mac = MacroRef {
                        params,
                        defaults,
                        patterns,
                        variadic,
                        body,
                        tail,
                    };

                    // A member of a namespace is keyed by its path,
                    // `Testing.shout`, the way an attribute is.
                    let key = self.ns_member_path(&name).unwrap_or(name);

                    self.macros.insert(key, mac);
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
                            let ret = signature_ret_type(self.text_of(m.signature));
                            // `async function f(self): T` answers with
                            // `Future<T>`, the same as an `async
                            // function` does, so the contract asks for
                            // that.
                            let ret = match (ret, m.is_async.is_some()) {
                                (Some(t), true) => Some(format!("Future<{}>", t.trim())),

                                (t, _) => t.map(str::to_string),
                            };

                            (self.text_of(m.name).to_string(), m.params.len(), ret)
                        })
                        .collect();
                    self.traits.insert(name.clone(), defaults);
                    self.trait_required.insert(name, required);
                }

                Stmt::Struct(st) => {
                    self.note_struct(st);
                    let name = self.decl_name(st.name);
                    let fields = self.field_members(&st.fields, true);
                    self.field_body.insert(name.clone(), st.span);
                    self.type_members.entry(name).or_default().extend(fields);
                }

                // An interface field takes no visibility, so every one
                // is public and a contract reads it that way.
                //
                // The kind lands here, not in the statement walk alone,
                // so a body above the declaration reads it too: `is`
                // refuses an interface wherever the file declares one.
                Stmt::Interface(i) => {
                    let name = self.decl_name(i.name);
                    let fields = self.field_members(&i.fields, false);
                    self.not_constructible.insert(name.clone(), "interface");
                    self.field_body.insert(name.clone(), i.span);
                    self.type_members.entry(name).or_default().extend(fields);
                }

                _ => {}
            }
        }

        // An imported enum is as exhaustible as one declared here; the
        // file names its variants the same way.
        for (name, variants) in &self.options.import_enums {
            // An enum inside an imported namespace reads under its path,
            // `Geo.Kind`, and the import list binds the head, `Geo`.
            let bound = name.split('.').next().unwrap_or(name);

            if self.imported_names.contains(bound) && !self.enums.contains_key(name) {
                self.enums.insert(name.clone(), variants.clone());
                self.enum_decls.insert(name.clone(), variants.clone());

                // An imported `enum Opt<T>` exports as `Opt<T>`; its
                // alias takes arguments here too.
                if self.imported_type_is_generic(name) {
                    self.generic_types.insert(name.clone());
                }
            }
        }

        // A macro body is a fragment of the file it expands in, so the
        // enums of that file are in scope with no import behind them.
        for (name, variants) in &self.options.macro_enums {
            if !self.enums.contains_key(name) {
                self.enums.insert(name.clone(), variants.clone());
                self.enum_decls.insert(name.clone(), variants.clone());
            }
        }
    }

    /// Whether an imported type carries a parameter list. The import
    /// index keys an enum by the path this file spells, `M.Geo.Kind`,
    /// and the export list names it flat, `Geo_Kind<T>`: some tail of
    /// the path, joined with `_`, is the exported head.
    fn imported_type_is_generic(&self, name: &str) -> bool {
        let parts: Vec<&str> = name.split('.').collect();
        let tails: Vec<String> = (0..parts.len()).map(|i| parts[i..].join("_")).collect();

        self.options
            .import_types
            .iter()
            .flat_map(|(_, types)| types.iter())
            .any(|t| {
                !crate::modules::type_args(t).is_empty()
                    && tails
                        .iter()
                        .any(|tail| crate::modules::type_head(t) == tail)
            })
    }

    /// The names one import list binds, with the name each one renames:
    /// `{ Box as B }` binds `B` and records that it stands for `Box`.
    fn note_specs(&mut self, specs: &[alloy_syntax::ast::ImportSpec]) {
        for sp in specs {
            let name = self.text_of(sp.name).to_string();
            let local = match sp.alias {
                Some(a) => self.text_of(a).to_string(),

                None => name.clone(),
            };

            // A struct another file declares derives for this file too:
            // a field of it clones, defaults, and serializes through it.
            if let Some(shape) = self.options.shapes.iter().find(|s| s.name == name) {
                for d in &shape.derives {
                    let set = match d.as_str() {
                        "Clone" => &mut self.cloneable,

                        "Default" => &mut self.defaultable,

                        "Serialize" => &mut self.serializable,

                        "Deserialize" => &mut self.deserializable,

                        _ => continue,
                    };
                    set.insert(local.clone());
                }
            }

            if local != name {
                self.import_renames.insert(local.clone(), name);
            }

            self.imported_names.insert(local);
        }
    }

    /// A bound with each operator trait routed to the runtime type, unless
    /// the file declares a trait of that name.
    pub(crate) fn resolve_bound(&mut self, bound: &str) -> String {
        const BUILTIN: &[&str] = BUILTIN_BOUNDS;
        let parts: Vec<String> = bound
            .split('&')
            .map(|part| {
                let part = part.trim();
                let name = part.split('<').next().unwrap_or(part).trim();

                // `<T: ~nil>`: the parameter takes `T & ~nil`.
                if let Some(negated) = part.strip_prefix('~') {
                    self.uses_neg = true;

                    return format!("__neg<{}>", negated.trim());
                }

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
        let mut prev_end = None;

        for i in span.start..span.end {
            let tok = self.toks[i as usize];

            // The gap the source wrote decides the space, so `Choice.Yes`
            // and `f(x)` keep their shape in the expansion. The text is
            // one line, and a gap may hold a comment, so any gap at all
            // becomes one space.
            if prev_end.is_some_and(|end| end < tok.start) {
                out.push(' ');
            }

            out.push_str(tok.text(self.src));
            prev_end = Some(tok.end);
        }

        out
    }

    /// Whether a name in a type position resolves to something: a
    /// declaration of this file, an import, a project global, a name a
    /// `.d.aly` declares, an engine class, or a name of the std.
    ///
    /// The header of an `impl` and the bound of a generic carry no type
    /// slot into the emit, so nothing else reports a name that is not
    /// there.
    pub(crate) fn knows_type(&self, name: &str) -> bool {
        let head = name.split('<').next().unwrap_or(name).trim();

        if head.is_empty() {
            return true;
        }

        // A dotted path reads a namespace or a module record; the head
        // is the name that has to exist.
        if let Some((base, _)) = head.split_once('.') {
            return self.knows_type(base);
        }

        self.structs.contains(head)
            || self.enums.contains_key(head)
            || self.enum_decls.contains_key(head)
            || self.traits.contains_key(head)
            || self.declared_types.contains(head)
            || self.own_names.contains(head)
            || self.imported_names.contains(head)
            || self.namespaces.contains_key(head)
            || self.attr_decl_of(head).is_some()
            || self.is_local(head)
            || AMBIENT.contains(&head)
            || AMBIENT_TYPES.contains(&head)
            || PRIMITIVES.contains(&head)
            || crate::extensions::is_foreign(head)
            || LUAU_GLOBALS.contains(&head)
            || self.options.ambient_names.iter().any(|n| n == head)
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
        // The check artifact types the value by its arguments, the way
        // the runtime reads them: one parameter is that parameter's
        // type, so `Attributes.get(S, icon)` reads as `string?`;
        // several are `{ min: number, max: number }`; none is the
        // `boolean` that marks the attribute as there.
        let value = if self.options.check {
            let types: Vec<String> = a
                .params
                .iter()
                .map(|p| match p.ty {
                    Some(t) => self.copy_type_to_string(t).trim().to_string(),

                    None => "any".to_string(),
                })
                .collect();
            let read = match types.len() {
                0 => "boolean".to_string(),

                1 => types[0].clone(),

                _ => {
                    let fields: Vec<String> = a
                        .params
                        .iter()
                        .zip(&types)
                        .map(|(p, ty)| format!("{}: {ty}", self.text_of(p.name)))
                        .collect();

                    format!("{{ {} }}", fields.join(", "))
                }
            };

            format!("({value} :: any) :: {std}.Attribute<{read}>")
        } else {
            value
        };
        // A member of a namespace renders under the namespace's name,
        // `Testing_tag`, and the table line puts it on the table. The
        // name the runtime records stays the one the source wrote, so
        // `Attributes.get` reads the same key whichever way a use
        // spells the attribute.
        let local = self.decl_name(a.name);

        self.generate(start, &format!("local {local} = {value}"));
        self.blank_lines(start, self.byte_end(a.span));

        if a.exported {
            self.exports.push((name, local));
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
                // `@allow` is the lints' alone.
                Some("inline" | "noinline" | "allow") => {}

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

                        // Luau reads the message of `@deprecated` from a
                        // table: `@[deprecated {reason = "..."}]`. A table
                        // the source wrote goes as it is.
                        match n {
                            "deprecated" if matches!(a.args[0], Expr::Table { .. }) => {
                                upstream.push(format!("@[deprecated {}]", args[0]));
                            }

                            "deprecated" => upstream
                                .push(format!("@[deprecated {{reason = {}}}]", args.join(", "))),

                            _ => upstream.push(format!("@[{n}({})]", args.join(", "))),
                        }
                    }
                }

                None => upstream.push(self.text_of(a.span).to_string()),

                // An attribute that does not reach a function is
                // already a diagnostic. Its arguments are not values:
                // `@derive(Clone)` would write `Clone` into the attach
                // table, and the checker would call it an unknown global.
                Some(n) if !self.attr_reaches(n, "function") => {}

                Some(n) => {
                    let args = self.attr_args(a, n);
                    user.push(format!("{} = {{ {} }}", attr_key(n), args.join(", ")));
                }
            }
        }

        // The declaration starts after the attributes.
        let mut first_tok = attrs.last().map(|a| a.span.end).unwrap_or(span.start);
        let fname = name.map(|n| self.text_of(n).to_string());
        let hoisted = name.is_some_and(|n| self.is_hoisted_fn(n));

        // A namespace member writes `public` or `private` between the
        // attributes and the declaration. The word is Alloy's; Luau
        // reads none of it, so the lead takes its place.
        if self
            .toks
            .get(first_tok as usize)
            .is_some_and(|t| matches!(t.text(self.src), "public" | "private"))
        {
            first_tok += 1;
        }

        // The first line declared the name: a `local` here would open
        // a second slot and leave the first one nil.
        if hoisted && is_local {
            first_tok += 1 + u32::from(exported);
        } else if exported {
            // `export` goes: `local` takes its place in the lead.
            first_tok += 1;
        }

        let decl_start = self.toks[first_tok as usize].start;

        if is_test && !self.options.tests {
            self.ship_blanks.push((start, self.byte_end(span)));
        }

        let mut lead = upstream.join(" ");

        if !lead.is_empty() {
            lead.push(' ');
        }

        let forced = std::mem::take(&mut self.ns_force_local);

        if (((is_test || exported) && !is_local) || forced) && !hoisted {
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
            // A function with nothing to give does nothing on the other
            // side; one that owes the caller a value says it cannot run
            // there. A silent nil where the signature promises a value
            // would fail somewhere else instead of naming the mistake.
            let wrong_side = match self.returns_no_value(body) {
                true => "return".to_string(),

                false => format!(
                    "error({}, 2)",
                    luau_string(&format!("{what} is {text} and cannot run here"))
                ),
            };
            self.inserts.push((
                at,
                format!("{}if not ({cond}) then {wrong_side} end{}", pad.0, pad.1),
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
            // A namespace member renders under its own name,
            // `Suite_case`, and the namespace table carries it. Every
            // line that names the function reads that name; the source
            // name binds nothing.
            let rendered = name.map_or_else(|| f.clone(), |n| self.decl_name(n));

            if is_test {
                // The test is `Suite.ns_case` to the reader and to the
                // spec, `Suite_ns_case` to the line that registers it.
                let path = self.display_name(&rendered);
                self.test_names
                    .push((path.clone(), body.is_async.is_some()));

                if !self.options.tests {
                    let std = self.std();
                    tail.push_str(&format!(" {std}.test({}, {rendered})", luau_string(&path)));
                } else {
                    // The spec calls `__alloy.set_testing`, so the spec
                    // requires the runtime even when the body does not.
                    self.std();
                }
            }

            if !user.is_empty() {
                let std = self.std();
                tail.push_str(&format!(
                    " {std}.attach({rendered}, {{ {} }})",
                    user.join(", ")
                ));
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

    /*
    `@cfg(cond) <statement>`: the statement runs where the condition
    holds, and the other side skips it. A statement hands nobody a
    value, so skipping it is what the author asked for.

    The guard sits on the statement's own line, `if cond then f() end`,
    so the line count of the file holds. Two `@cfg` lines on one
    statement read as one condition, joined by `and`.
    */
    pub(crate) fn attributed_plain_stmt(&mut self, attrs: &[Attr], inner: &Stmt) {
        let mut conds = Vec::new();

        for a in attrs {
            match a.name.map(|n| self.text_of(n)) {
                Some("cfg") => match self.cfg_condition(&a.args) {
                    Ok(cond) => conds.push(cond),

                    Err(message) => self.diagnose(a.span, &message),
                },

                // The lints read `@allow` from the source; the emit
                // writes nothing for it.
                Some("allow") => {}

                Some(n) => self.diagnose(
                    a.span,
                    &format!(
                        "`@{n}` goes on a declaration; `@cfg` and `@allow` are the attributes a statement takes"
                    ),
                ),

                None => self.diagnose(
                    a.span,
                    "`@cfg` is the attribute a statement takes; this one goes on a declaration",
                ),
            }

            self.blank_lines(self.byte_start(a.span), self.byte_end(a.span));
        }

        // The copy starts where the last attribute ends, so the newline
        // between it and the statement survives.
        let after_attrs = attrs
            .iter()
            .map(|a| self.byte_end(a.span))
            .max()
            .unwrap_or(0);
        let start = self.byte_start(inner.span());
        let end = self.byte_end(inner.span());
        self.copy(after_attrs, start);

        if conds.is_empty() {
            self.stmt(inner);

            return;
        }

        self.generate(start, &format!("if {} then ", conds.join(" and ")));
        self.stmt(inner);
        self.generate(end, " end");
    }

    /// Whether a function hands its caller no value: it declares `()`
    /// as its return type, or declares none and returns nothing. An
    /// `async` function with no declared type resolves its future with
    /// nil, so it reads the same way.
    ///
    /// A declared `nil` is not in the list: Luau reads a bare `return`
    /// as `()`, so a skip there would report the type.
    pub(crate) fn returns_no_value(&self, body: &FunctionBody) -> bool {
        match body.ret_type {
            Some(t) => {
                let text: String = self
                    .text_of(t)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();

                text == "()"
            }

            None => !block_returns_value(&body.block),
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

/// The names a Luau or Roblox program already has. An attribute
/// argument may name one of them.
const LUAU_GLOBALS: &[&str] = &[
    "_G",
    "_VERSION",
    "assert",
    "bit32",
    "buffer",
    "collectgarbage",
    "coroutine",
    "debug",
    "delay",
    "error",
    "game",
    "getfenv",
    "getmetatable",
    "gcinfo",
    "ipairs",
    "loadstring",
    "math",
    "newproxy",
    "next",
    "os",
    "pairs",
    "pcall",
    "plugin",
    "print",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "require",
    "script",
    "select",
    "setfenv",
    "setmetatable",
    "shared",
    "spawn",
    "string",
    "table",
    "task",
    "tick",
    "time",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "unpack",
    "utf8",
    "wait",
    "warn",
    "workspace",
    "xpcall",
    "Enum",
    "Instance",
];

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
            vec![
                "the attribute `deprecated` takes a string message or a table of `use` and `reason`, number given"
            ]
        );
        assert!(
            messages("@deprecated(\"use `new`\")\nlocal function old(): number\n    return 1\nend\nprint(old())\n")
                .is_empty()
        );
    }

    /// `@deprecated` over a type alias read `expected `function`,
    /// found `type``, a parse error. A type alias is an attribute
    /// target, so the refusal reads the way it does on a local.
    #[test]
    fn an_attribute_on_a_type_alias_reports_its_target() {
        let got = messages(
            "@deprecated
export type Old = { a: number }
",
        );
        assert_eq!(
            got,
            vec![
                "the attribute `deprecated` has no meaning on a type; it goes on `function` and `namespace`"
            ]
        );
    }

    /// A declared attribute reaches a type alias, and the emit drops
    /// the line: `@tag` is no Luau.
    #[test]
    fn a_declared_attribute_reaches_a_type_alias() {
        let src = "attribute tag on type

@tag
export type Id = number

local a: Id = 1
print(a)
";
        assert!(messages(src).is_empty(), "{:?}", messages(src));
        let out = crate::compile(src).unwrap();
        assert!(!out.ship.contains("@tag"), "{}", out.ship);
        assert!(!out.ship.contains("\ntag"), "{}", out.ship);
        assert!(out.ship.contains("export type Id = number"), "{}", out.ship);
        assert_eq!(
            out.ship.lines().count(),
            src.lines().count(),
            "{}",
            out.ship
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

    /// The check artifact types the attribute the way the runtime
    /// reads it: one parameter bare, several as a named table, none
    /// as the `boolean` that marks its presence.
    #[test]
    fn an_attribute_takes_the_type_its_parameters_read_as() {
        let src = "attribute icon(asset: string) on struct\nattribute range(min: number, max: number) on field\nattribute server_only on function\nprint(icon, range, server_only)\n";
        let out = crate::compile_with(
            src,
            &crate::EmitOptions {
                check: true,
                ..crate::EmitOptions::default()
            },
        )
        .unwrap();
        assert!(out.check.contains("Attribute<string>"), "{}", out.check);
        assert!(
            out.check
                .contains("Attribute<{ min: number, max: number }>"),
            "{}",
            out.check
        );
        assert!(out.check.contains("Attribute<boolean>"), "{}", out.check);
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

    /// A parameter with a default is optional: `@icon()` writes the
    /// declared value into the attrs table, so `Attributes.get` reads
    /// it; `@icon("x")` keeps its own; a parameter without a default
    /// still reports.
    #[test]
    fn an_attribute_parameter_default_fills_an_omitted_argument() {
        let src = "attribute icon(asset: string = \"rbxassetid://0\") on struct, function\n@icon()\nstruct Sword as damage: number end\n@icon(\"x\")\nstruct Axe as damage: number end\n@icon()\nlocal function f() end\nprint(Sword, Axe, f)\n";
        assert!(messages(src).is_empty(), "{:?}", messages(src));
        let ship = crate::compile(src).unwrap().ship;
        assert_eq!(
            ship.matches("icon = { \"rbxassetid://0\" }").count(),
            2,
            "{ship}"
        );
        assert!(ship.contains("icon = { \"x\" }"), "{ship}");

        let required = "attribute icon(asset: string) on struct\n@icon()\nstruct Sword as damage: number end\nprint(Sword)\n";
        assert_eq!(
            messages(required),
            vec!["the attribute `icon` takes 1 argument, 0 given"]
        );

        let mixed = "attribute weight(n: number, tag: string = \"x\") on struct\n@weight()\nstruct S as x: number end\nprint(S)\n";
        assert_eq!(
            messages(mixed),
            vec!["the attribute `weight` takes 1 to 2 arguments, 0 given"]
        );
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
