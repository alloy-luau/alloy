//! Struct, impl, trait, interface, and derive lowering, plus the
//! construction and narrowing checks that go with them.

use std::collections::HashSet;

use alloy_syntax::ast::{
    Attr, Block, CallArgs, Cond, Expr, Field, If, ImplDecl, InterfaceDecl, Local, Stmt, StructDecl,
    TableField, TokSpan, TraitDecl, TraitMethod,
};
use alloy_syntax::lexer::TokKind;

use crate::render::Renderer;

use super::enums::same_type_text;
use super::remotes::WIRE_WIDTHS;
use super::types::{generic_head, names_other_args, strip_bounds};
use super::*;

/// Datatypes the definitions declare as a type alias, not a class. A
/// `typeof` test on one names it at run time, but the checker cannot
/// refine a value by it, so the check artifact narrows by a cast.
pub(crate) const ALIAS_DATATYPES: &[&str] = &["RBXScriptSignal"];

impl<'s> Desugar<'s> {
    /// `impl X ... end`: each method lands on `X`; operator traits map to
    /// metamethods.
    pub(crate) fn impl_decl(&mut self, i: &ImplDecl) {
        // A struct of the namespace this `impl` sits in renders under
        // the namespace's own name.
        let target_name = self.impl_target_name(i.target);
        let start = self.byte_start(i.span);
        // The header runs to the end of the target, and past `<T>` when
        // the impl declares parameters: Luau has no such header.
        let head_tok = i
            .generics
            .filter(|g| g.start >= i.target.end)
            .map_or(i.target.end, |g| g.end);
        // `impl T as`: the header may close with `as`, so the emit
        // starts after it. Without this the `as` reaches the output.
        let head_tok = if self.toks[head_tok as usize].text(self.src) == "as" {
            head_tok + 1
        } else {
            head_tok
        };
        let header_end = self.toks[head_tok as usize - 1].end;

        if self.options.definitions {
            self.blank_lines(start, self.byte_end(i.span));

            return;
        }

        // A foreign target gets a registry table instead of its metatable.
        let foreign = self.is_foreign(&target_name);
        self.ext_hit |= foreign;
        let target = if foreign {
            "__impl".to_string()
        } else {
            target_name.clone()
        };

        if foreign {
            let std = self.std();
            self.generate(
                start,
                &format!(
                    "do local __impl = {std}.impl_for({})",
                    luau_string(&target_name)
                ),
            );
        } else {
            self.generate(start, "do");
        }
        let mut cursor = header_end;

        // The check artifact types an untyped `self`: a foreign type by
        // its name, a struct or enum of this file by its alias, unless
        // the alias takes parameters that are not in scope here.
        let local_type = (self.structs.contains(&target_name)
            || self.enums.contains_key(&target_name))
            && !self.generic_types.contains(&target_name);

        // `impl Box<T>`: the parameters go on each method, so its body
        // and its signature may name them.
        let impl_generics = i
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();

        // `impl Box` on a `struct Box<T>` binds no `T`, so a method that
        // names one has a type the checker cannot find.
        if i.generics.is_none()
            && let Some(params) = self.struct_generics.get(&target_name).cloned()
        {
            let message = format!(
                "the struct `{target_name}` takes `{params}`; write `impl {target_name}{params}` so its methods can name them"
            );
            self.diagnose(i.target, &message);
        }
        let types_self = self.options.check && (foreign || local_type || !impl_generics.is_empty());
        // A struct with private members: a private method lands on
        // `Target__private` in the check artifact, and a public method
        // rebinds `self` to the full view on its first line.
        let split = !foreign && self.has_private_view(&target_name);

        self.impl_target = Some(target_name.clone());

        for m in &i.methods {
            let is_private = m.visibility.is_some_and(|v| self.text_of(v) == "private");

            if types_self {
                self.self_type = Some(if split && is_private {
                    format!("{target_name}__all")
                } else {
                    format!("{target_name}{impl_generics}")
                });
            }

            let has_self = m
                .body
                .params
                .first()
                .is_some_and(|p| self.text_of(p.name) == "self");

            if split && !is_private && has_self {
                self.self_prologue =
                    Some(format!("local self = (self :: any) :: {target_name}__all"));
            }

            let ms = self.byte_start(m.span);
            self.copy(cursor, ms);
            let fn_tok = (m.span.start as usize..m.span.end as usize)
                .find(|&k| self.toks[k].text(self.src) == "function")
                .unwrap_or(m.span.start as usize);
            let fn_tok_end = self.toks[fn_tok].end;
            let name_span = m.path[0];
            let mname = self.text_of(name_span).to_string();
            // `function name(` becomes `function Target.name(`. The
            // `private` or `public` word has no Luau form and goes.
            match m.visibility {
                Some(v) => {
                    self.copy(ms, self.byte_start(v));
                    self.copy(self.byte_end(v), fn_tok_end);
                }

                None => self.copy(ms, fn_tok_end),
            }

            let owner = if split && is_private {
                format!("{target}__private")
            } else {
                target.clone()
            };
            // The insert anchors on the method's own name, not on the
            // gap after `function`: a diagnostic the checker puts on the
            // inserted owner then lands on a name the source shows.
            // A method of a generic impl carries the impl's parameters,
            // unless it declares its own.
            let method_generics = if m.body.generics.is_none() {
                impl_generics.as_str()
            } else {
                ""
            };
            self.generate(
                self.byte_start(name_span),
                &format!(" {owner}.{mname}{method_generics}"),
            );
            let after_name = self.byte_end(name_span);
            let rest = TokSpan::new(name_span.end as usize, m.span.end as usize);
            let _ = after_name;

            if function_needs_rewrite(&m.body) || self.self_type.is_some() {
                self.function_with_header(rest, &m.body);
            } else {
                let children = function_children(&m.body);
                self.stitch(rest, &children, |d, child| match child {
                    Child::Expr(e) => d.expr(e),

                    Child::Block(b) => d.block(b),

                    Child::Function(b) => d.function_block(b),
                });
            }

            cursor = self.byte_end(m.span);
            self.self_prologue = None;
        }

        self.self_type = None;
        self.impl_target = None;

        let end_tok = self.toks[i.span.end as usize - 1];
        self.copy(cursor, end_tok.start);

        // Operator traits.
        let mut tail = String::new();

        if let Some(t) = i.trait_name {
            let trait_name = self.text_of(t).to_string();
            let mapping: &[(&str, &str, &str)] = &[
                ("Add", "add", "__add"),
                ("Sub", "sub", "__sub"),
                ("Mul", "mul", "__mul"),
                ("Div", "div", "__div"),
                ("Eq", "eq", "__eq"),
                ("Lt", "lt", "__lt"),
                ("Le", "le", "__le"),
                ("Display", "to_string", "__tostring"),
                ("Call", "call", "__call"),
                ("Len", "len", "__len"),
                ("Concat", "concat", "__concat"),
                ("Drop", "drop", "Destroy"),
            ];

            for (tr, method, meta) in mapping {
                if trait_name == *tr {
                    // `delete` takes a `Deletable`, whose `Destroy` is
                    // `(self: any) -> ()`; the check artifact says so.
                    let value = if self.options.check && *tr == "Drop" {
                        format!("({target}.{method} :: (self: any) -> ())")
                    } else {
                        format!("{target}.{method}")
                    };
                    tail.push_str(&format!(" {target}.{meta} = {value}"));
                }
            }

            // A trait declared in this file is a contract: every method
            // without a body appears in the impl, with the same arity.
            if let Some(required) = self.trait_required.get(&trait_name).cloned() {
                for (m, arity, ret) in required {
                    let written = i.methods.iter().find(|f| self.text_of(f.path[0]) == m);

                    match written {
                        None => self.diagnose(
                            t,
                            &format!(
                                "`impl {trait_name} for {target_name}` does not write `{m}`; the trait declares it"
                            ),
                        ),

                        Some(f)
                            if f.body.params.len() != arity
                                && !f.body.params.iter().any(|p| p.is_vararg) =>
                        {
                            self.diagnose(
                                f.path[0],
                                &format!(
                                    "the trait method `{m}` takes {} parameter{} in `{trait_name}`, {} here",
                                    arity,
                                    if arity == 1 { "" } else { "s" },
                                    f.body.params.len()
                                ),
                            );
                        }

                        // The trait declares the return type, so an impl
                        // that writes another one breaks the contract.
                        // The checker sees two unrelated functions and
                        // says nothing about the trait.
                        Some(f) => {
                            if let (Some(want), Some(t)) = (&ret, f.body.ret_type) {
                                let got = self.text_of(t).trim().to_string();

                                if !same_type_text(want, &got) {
                                    self.diagnose(
                                        t,
                                        &format!(
                                            "the trait method `{m}` returns {want} in `{trait_name}`, {got} here"
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
            }

            // Default methods of a trait declared in this file flatten in.
            // The check artifact assigns without the guard: a conditional
            // assignment would make the property optional to the checker,
            // and the struct would then not satisfy the trait.
            let defaults = self.traits.get(&trait_name).cloned().or_else(|| {
                self.options
                    .import_trait_defaults
                    .iter()
                    .find(|(t, _)| *t == trait_name)
                    .map(|(_, d)| d.clone())
            });

            if let Some(defaults) = defaults {
                let written: Vec<String> = i
                    .methods
                    .iter()
                    .map(|f| self.text_of(f.path[0]).to_string())
                    .collect();

                for m in defaults {
                    if self.options.check {
                        // The impl's own method keeps its type.
                        if !written.contains(&m) {
                            tail.push_str(&format!(" {target}.{m} = {trait_name}.{m}"));
                        }
                    } else {
                        tail.push_str(&format!(
                            " if rawget({target}, \"{m}\") == nil then {target}.{m} = {trait_name}.{m} end"
                        ));
                    }
                }
            }
        }

        // A struct's `end` line carried its tables; the impl adds after.
        if i.exported && !foreign {
            self.exports
                .push((target_name.clone(), target_name.clone()));
        }

        self.generate(end_tok.start, &format!("end{tail}"));
    }

    // --- patterns ------------------------------------------------------------

    /*
    The members of a generic struct's alias in the check artifact: the
    fields, then every instance method the `impl` block writes.

    Luau prints no type argument for an alias whose body is
    `typeof(setmetatable(...))`, so `Slotted.new(5)` hovers `Slotted`
    where the reader wrote `Slotted<number>`. A plain table alias prints
    `Slotted<number>`, and it has to carry the methods itself. `None`
    where the alias cannot be complete: a struct with no parameters
    needs no change, and a trait impl adds methods this cannot list.
    */
    pub(crate) fn generic_alias_members(
        &mut self,
        name: &str,
        generics: &str,
        field_types: &[String],
    ) -> Option<Vec<String>> {
        if !self.options.check || generics.is_empty() || self.trait_impl_targets.contains(name) {
            return None;
        }

        if self.impl_generics.get(name).map(String::as_str) != Some(generics) {
            return None;
        }

        let sigs = std::mem::take(self.struct_methods.get_mut(name)?);
        let mut members: Vec<String> = field_types.to_vec();

        for m in &sigs {
            let mname = self.text_of(m.name).to_string();
            let mut params = vec![format!("self: {name}{generics}")];

            for (pname, pty, vararg, default) in &m.params {
                let ty = pty
                    .map(|t| self.copy_type_to_string(t))
                    .unwrap_or_else(|| "any".to_string());

                if *vararg {
                    params.push(format!("...{ty}"));
                    continue;
                }

                let opt = if *default && !ty.trim_end().ends_with('?') {
                    "?"
                } else {
                    ""
                };
                params.push(format!("{}: {ty}{opt}", self.text_of(*pname)));
            }

            let ret = m
                .ret
                .map(|t| self.copy_type_to_string(t))
                .unwrap_or_else(|| "()".to_string());
            let mg = m.generics.map(|g| self.text_of(g)).unwrap_or_default();
            // A method is a read property: the checker holds a struct's
            // methods read-only, and a read-write slot would reject them.
            members.push(format!(
                "read {mname}: {mg}({}) -> {ret}",
                params.join(", ")
            ));
        }

        self.struct_methods.insert(name.to_string(), sigs);

        // `function swap(self): Pair<B, A>` would make the alias name
        // itself with other arguments, which Luau rejects. The metatable
        // form takes those structs back.
        let whole = format!("{name}{generics}");

        if members.iter().any(|m| names_other_args(m, name, &whole)) {
            return None;
        }

        Some(members)
    }

    /*
    A struct is a class table with `__index`, a raw constructor on the class
    table's own metatable, and a type. Field lines hold nothing at runtime;
    the header carries the tables and the `end` line carries the type and
    the derives. A `.d.aly` keeps only the type.
    */
    pub(crate) fn struct_decl(&mut self, st: &StructDecl) {
        let name = self.decl_name(st.name);
        let start = self.byte_start(st.span);
        let end_tok = self.toks[st.span.end as usize - 1];
        let generics = st
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let export = if st.exported || self.ns_export {
            "export "
        } else {
            ""
        };

        // Field types and defaults.
        let mut field_types = Vec::new();
        // The constructor's record: a field with a default may be left out.
        let mut param_types = Vec::new();
        let mut defaults = Vec::new();
        let mut field_names = Vec::new();
        let mut field_attrs = Vec::new();
        // The check artifact keeps a private field out of the public view.
        let split = self.has_private_view(&name);
        let mut public_types = Vec::new();
        let mut private_types = Vec::new();

        for f in &st.fields {
            let fname = self.text_of(f.name).to_string();
            let ty = self.copy_type_to_string(f.ty);
            let modifier = f
                .modifier
                .map(|m| format!("{} ", self.text_of(m)))
                .unwrap_or_default();
            let opt = if f.default.is_some() && !ty.trim_end().ends_with('?') {
                "?"
            } else {
                ""
            };
            field_types.push(format!("{modifier}{fname}: {ty}"));
            // The constructor fills defaults into this table, so a
            // `read` field is plain here.
            param_types.push(format!("{fname}: {ty}{opt}"));

            if f.visibility.is_some_and(|v| self.text_of(v) == "private") {
                private_types.push(format!("{modifier}{fname}: {ty}"));
            } else {
                public_types.push(format!("{modifier}{fname}: {ty}"));
            }

            if let Some(dv) = &f.default {
                self.expected_generic = generic_head(&ty);
                let v = self.render_to_string(dv);
                self.expected_generic = None;
                defaults.push(format!("if f.{fname} == nil then f.{fname} = {v} end"));
            }

            let attrs = self.attr_table(&f.attributes);

            if attrs != "{}" {
                field_attrs.push(format!("{fname} = {attrs}"));
            }

            field_names.push(fname);
        }

        let type_line = if split {
            // `Name` is what the world sees; `Name__all` is the same table
            // with the private fields and the private methods, which the
            // impl's methods put on `Name__private` in this artifact.
            let hidden = if private_types.is_empty() {
                String::new()
            } else {
                format!(" & {{ {} }}", private_types.join(", "))
            };

            format!(
                "{export}type {name} = typeof(setmetatable({{}} :: {{ {} }}, {name})) type {name}__all = {name}{hidden} & typeof({name}__private)",
                public_types.join(", ")
            )
        } else if let Some(members) = self.generic_alias_members(&name, &generics, &field_types) {
            format!(
                "{export}type {name}{generics} = {{ {} }}",
                members.join(", ")
            )
        } else {
            format!(
                "{export}type {name}{generics} = typeof(setmetatable({{}} :: {{ {} }}, {name}))",
                field_types.join(", ")
            )
        };

        if self.options.definitions {
            self.generate(
                start,
                &format!(
                    "{export}type {name}{generics} = {{ {} }}",
                    field_types.join(", ")
                ),
            );
            self.blank_lines(start, end_tok.end);

            return;
        }

        // Header.
        // The check artifact types the raw constructor, so `new Name { }`
        // is a `Name` to the checker and not a metatable over a literal.
        // A generic struct stays untyped: its parameters are not in
        // scope here. A struct whose impl writes `new` gets no generated
        // one in the check artifact, so the two do not clash.
        // The check artifact has no metatable on the class table: with one,
        // the instance type nests a metatable of its own, and the checker
        // then rejects the struct as a trait or a `Deletable`. The fields
        // form calls `__new`, a typed raw constructor, instead of the
        // class. A generic struct stays untyped, its parameters being out
        // of scope.
        // A generic struct types its constructor too: the parameters go
        // on the function, so the field types and the result name them.
        let typed = self.options.check && (generics.is_empty() || !split);
        let fn_generics = if generics.is_empty() {
            String::new()
        } else {
            generics.clone()
        };
        let (param, ret) = if typed {
            (
                format!("f: {{ {} }}", param_types.join(", ")),
                format!(": {name}{fn_generics}"),
            )
        } else {
            ("f".to_string(), String::new())
        };
        let d = defaults.join(" ");
        let header = if self.options.check {
            let new_fn = if self.structs_with_new.contains_key(&name) {
                String::new()
            } else {
                format!(
                    " function {name}.new{fn_generics}({param}){ret} return {name}.__new(f) end"
                )
            };

            let private_table = if split {
                format!(" local {name}__private = {{}}")
            } else {
                String::new()
            };

            format!(
                "local {name} = {{}} {name}.__index = {name}{private_table} function {name}.__new{fn_generics}({param}){ret} {d} return (setmetatable(f, {name}) :: any) end{new_fn}"
            )
        } else {
            format!(
                "local {name} = {{}} {name}.__index = {name} setmetatable({name}, {{ __call = function(_, f) {d} return setmetatable(f, {name}) end }}) function {name}.new(f) return {name}(f) end"
            )
        };
        self.generate(start, &header);

        // Field lines carry only their trivia. The range starts at the
        // attributes, so an attribute line above `struct` keeps its newline.
        self.blank_lines(start, end_tok.start);

        // Derives and attributes on the `end` line.
        let mut tail = type_line;
        let mut derives_debug = false;
        let mut derived: HashSet<String> = HashSet::new();

        for a in &st.attributes {
            let Some(aname) = a.name else { continue };

            if self.text_of(aname) == "derive" {
                for arg in &a.args {
                    let which = self.text_of(arg.span()).to_string();
                    derives_debug |= which == "Debug";

                    // `Eq` and `PartialEq` write the same `__eq`; naming
                    // both must not write it twice.
                    let key = if which == "PartialEq" {
                        "Eq".to_string()
                    } else {
                        which.clone()
                    };

                    if !derived.insert(key) {
                        continue;
                    }

                    tail.push(' ');
                    tail.push_str(&self.derive_struct(
                        &name,
                        &which,
                        arg.span(),
                        &field_names,
                        &st.fields,
                    ));
                }
            }
        }

        // The default printer: `Name { x = 1, y = 2 }`. A `to_string` in
        // the struct's impl, or `@derive(Debug)`, writes its own and this
        // one stays out; a `__tostring` set later replaces it either way.
        if !derives_debug && !self.structs_with_to_string.contains(&name) {
            let std = self.std();
            let sn = if self.options.check && !self.generic_types.contains(&name) {
                format!(": {}", self.self_alias(&name))
            } else {
                String::new()
            };
            let fields: Vec<String> = field_names.iter().map(|f| luau_string(f)).collect();
            tail.push_str(&format!(
                " {name}.__tostring = function(s{sn}) return {std}.show_struct({}, s, {{ {} }}) end",
                luau_string(&self.display_name(&name)),
                fields.join(", ")
            ));
        }

        let own = self.attr_table(&st.attributes);

        if own != "{}" || !field_attrs.is_empty() {
            let std = self.std();
            tail.push_str(&format!(
                " {std}.attrs({name}, {{ own = {own}, fields = {{ {} }} }})",
                field_attrs.join(", ")
            ));
        }

        // `@sealed`: a write to a key the struct does not declare raises.
        // Declared keys are present from construction, so `__newindex`
        // only sees a declared key when its value was nil; that write
        // goes through.
        // The check artifact leaves it out: the struct type already
        // rejects an unknown key, and a `__newindex` on the metatable
        // stops the solver from reducing a mapped type over the struct.
        if !self.options.check
            && st
                .attributes
                .iter()
                .any(|a| a.name.is_some_and(|n| self.text_of(n) == "sealed"))
        {
            let keys: Vec<String> = field_names.iter().map(|f| format!("{f} = true")).collect();
            let shown = luau_string(&self.display_name(&name));
            tail.push_str(&format!(
                " {name}.__newindex = function(t, k, v) if ({{ {} }})[k] then rawset(t, k, v) else error(string.format(\"%s has no field %s\", {shown}, tostring(k)), 2) end end",
                keys.join(", "),
            ));
        }

        self.generate(end_tok.start, &format!(" {tail}"));

        if st.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// Copies the lines in a range as blank lines, keeping the newlines.
    pub(crate) fn blank_lines(&mut self, start: u32, end: u32) {
        let text = &self.src[start as usize..end as usize];
        let mut cursor = start;

        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                let at = start + i as u32;
                // Nothing before the newline copies; the newline does.
                cursor = at;
                self.copy(cursor, at + 1);
                cursor = at + 1;
            }
        }

        let _ = cursor;
    }

    /// A type span rendered with its edits applied.
    pub(crate) fn copy_type_to_string(&mut self, span: TokSpan) -> String {
        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        self.copy_span(span);
        std::mem::swap(&mut self.r, &mut side);

        side.finish().0
    }

    /// `{ range = { 0, 100 }, skip = {} }` from a list of attributes,
    /// skipping the ones the compiler consumes itself.
    pub(crate) fn attr_table(&mut self, attrs: &[Attr]) -> String {
        let mut parts = Vec::new();

        for a in attrs {
            let Some(n) = a.name else { continue };
            let name = self.text_of(n).to_string();

            if matches!(
                name.as_str(),
                "derive" | "test" | "native" | "checked" | "deprecated" | "cfg"
            ) {
                continue;
            }

            let args: Vec<String> = a.args.iter().map(|e| self.render_to_string(e)).collect();
            parts.push(format!("{name} = {{ {} }}", args.join(", ")));
        }

        format!("{{ {} }}", parts.join(", ")).replace("{  }", "{}")
    }

    pub(crate) fn derive_struct(
        &mut self,
        name: &str,
        which: &str,
        at: TokSpan,
        fields: &[String],
        decls: &[Field],
    ) -> String {
        // The check artifact types the receiver, as an impl method's self.
        // A derive reads every field, and a `write` field is not
        // readable through the struct's own type, so the receiver meets
        // a read view of those fields.
        let readable = self.read_view(decls);
        let sn = if self.options.check && !self.generic_types.contains(name) {
            format!(": {}{readable}", self.self_alias(name))
        } else {
            String::new()
        };
        let tn = if self.options.check { ": any" } else { "" };
        match which {
            "Eq" | "PartialEq" => {
                let cmp: Vec<String> = fields.iter().map(|f| format!("a.{f} == b.{f}")).collect();
                let body = if cmp.is_empty() {
                    "true".to_string()
                } else {
                    cmp.join(" and ")
                };

                format!("{name}.__eq = function(a{sn}, b{sn}) return {body} end")
            }

            // Field by field, in declaration order: the first field that
            // differs decides, as a tuple compares.
            "Ord" => {
                let steps: Vec<String> = fields
                    .iter()
                    .map(|f| format!("if a.{f} ~= b.{f} then return a.{f} < b.{f} end"))
                    .collect();
                let steps = steps.join(" ");

                format!(
                    "{name}.__lt = function(a{sn}, b{sn}) {steps} return false end {name}.__le = function(a{sn}, b{sn}) {steps} return true end"
                )
            }

            "Debug" => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| format!("\"{f} = \" .. tostring(s.{f})"))
                    .collect();
                let inner = if parts.is_empty() {
                    "\"\"".to_string()
                } else {
                    parts.join(" .. \", \" .. ")
                };

                let ret = if self.options.check { ": string" } else { "" };

                format!(
                    "{name}.__tostring = function(s{sn}) return \"{name} {{ \" .. {inner} .. \" }}\" end function {name}.debug(self{sn}){ret} return tostring(self) end"
                )
            }

            "Clone" => {
                // The checker reads `setmetatable` of a metatable type as a
                // new shape; the annotation keeps the struct's own.
                let ret = if self.options.check {
                    format!(": {name}")
                } else {
                    String::new()
                };
                let value = self.any_cast(&format!("setmetatable(table.clone(self), {name})"));

                format!("function {name}.clone(self{sn}){ret} return {value} end")
            }

            "Serialize" => {
                let mut to = Vec::new();
                let mut from = Vec::new();

                for f in decls {
                    let fname = self.text_of(f.name).to_string();
                    let skip = f
                        .attributes
                        .iter()
                        .any(|a| a.name.map(|n| self.text_of(n) == "skip").unwrap_or(false));

                    if skip {
                        continue;
                    }

                    let key = f
                        .attributes
                        .iter()
                        .find(|a| a.name.map(|n| self.text_of(n) == "rename").unwrap_or(false))
                        .and_then(|a| a.args.first())
                        .map(|e| self.text_of(e.span()).trim_matches('"').to_string())
                        .unwrap_or(fname.clone());
                    to.push(format!("{key} = self.{fname}"));
                    from.push(format!("{fname} = t.{key}"));
                }

                // `serialize` is the name the `Serialize` bound asks
                // for. It calls `to_table`, so a derived struct meets
                // `T: Serialize` and both names give one table.
                let ret = if self.options.check { ": any" } else { "" };

                format!(
                    "function {name}.to_table(self{sn}) return {{ {} }} end function {name}.from_table(t{tn}) return {}({{ {} }}) end function {name}.serialize(self{sn}){ret} return {name}.to_table(self) end",
                    to.join(", "),
                    self.raw_ctor(name),
                    from.join(", ")
                )
            }

            other => {
                let message = format!("unknown derive `{other}`");
                self.diagnose(at, &message);

                String::new()
            }
        }
    }

    // --- traits --------------------------------------------------------------

    /// A trait is a type of its method signatures plus a table holding the
    /// default bodies, which `impl` copies onto the struct.
    pub(crate) fn trait_decl(&mut self, t: &TraitDecl) {
        let name = self.decl_name(t.name);
        let start = self.byte_start(t.span);
        let end_tok = self.toks[t.span.end as usize - 1];
        let export = if t.exported || self.ns_export {
            "export "
        } else {
            ""
        };
        let sigs: Vec<String> = t
            .methods
            .iter()
            .map(|m| self.trait_method_type(m))
            .collect();
        // Methods are read properties: a struct's methods are read-only to
        // the checker, and a read-write slot would reject them.
        let props: Vec<String> = sigs.iter().map(|s| format!("read {s}")).collect();
        let type_line = format!("{export}type {name} = {{ {} }}", props.join(", "));

        if self.options.definitions {
            self.generate(start, &type_line);
            self.blank_lines(self.byte_end(t.name), end_tok.end);

            return;
        }

        // A signature without a body is a typed nil on the table, so
        // `Trait.` completes and hovers it. Assigning nil adds no key, so
        // the runtime table holds the default bodies only.
        let stubs: String = t
            .methods
            .iter()
            .filter(|m| m.body.is_none())
            .map(|m| {
                let sig = self.trait_method_type(m);
                let ty = sig.split_once(": ").map(|(_, t)| t).unwrap_or("any");
                let mname = self.text_of(m.name);

                format!(" {name}.{mname} = (nil :: any) :: {ty}")
            })
            .collect();
        self.generate(start, &format!("local {name} = {{}} {type_line}{stubs}"));
        let mut cursor = self.byte_end(t.name);

        for m in &t.methods {
            let ms = self.byte_start(m.span);
            self.blank_lines(cursor, ms);

            match &m.body {
                Some(body) => {
                    // `function name(params): R` becomes `function Trait.name(params): R`.
                    let mname = self.text_of(m.name).to_string();
                    let mut sig = self.signature_text(m.signature);

                    // An untyped `self` is `any` in the check artifact: the
                    // default lands on every implementing struct.
                    if self.options.check {
                        if sig.starts_with("(self)") {
                            sig = sig.replacen("(self)", "(self: any)", 1);
                        } else if sig.starts_with("(self,") {
                            sig = sig.replacen("(self,", "(self: any,", 1);
                        }
                    }

                    self.generate(ms, &format!("function {name}.{mname}{sig}"));
                    let body_start = self.block_start_or(&body.block, self.byte_end(m.span));
                    self.copy(self.byte_end(m.signature), body_start);
                    self.block(&body.block);
                    let after = self.block_end_or(&body.block, body_start);
                    self.copy(after, self.byte_end(m.span));
                }

                None => self.blank_lines(ms, self.byte_end(m.span)),
            }

            cursor = self.byte_end(m.span);
        }

        self.blank_lines(cursor, end_tok.end);

        if t.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// A signature span with `->` written as `:`.
    pub(crate) fn signature_text(&mut self, sig: TokSpan) -> String {
        let mut out = String::new();

        for i in sig.start..sig.end {
            let tok = self.toks[i as usize];
            let text = tok.text(self.src);

            if i > sig.start {
                let prev = self.toks[i as usize - 1];
                out.push_str(&self.src[prev.end as usize..tok.start as usize]);
            }

            out.push_str(if text == "->" { ":" } else { text });
        }

        out
    }

    pub(crate) fn trait_method_type(&mut self, m: &TraitMethod) -> String {
        let mname = self.text_of(m.name).to_string();
        let mut params = Vec::new();

        for p in &m.params {
            let pname = self.text_of(p.name).to_string();

            if pname == "self" {
                params.push("self: any".to_string());
            } else if p.is_vararg {
                let ty =
                    p.ty.map(|t| self.copy_type_to_string(t))
                        .unwrap_or("any".to_string());
                params.push(format!("...{ty}"));
            } else {
                let ty =
                    p.ty.map(|t| self.copy_type_to_string(t))
                        .unwrap_or("any".to_string());
                params.push(format!("{pname}: {ty}"));
            }
        }

        // The return type follows the `)` in the signature.
        let sig = self.text_of(m.signature).to_string();
        let ret = sig
            .rsplit_once(')')
            .map(|(_, r)| r.trim())
            .map(|r| {
                r.trim_start_matches("->")
                    .trim_start_matches(':')
                    .trim()
                    .to_string()
            })
            .filter(|r| !r.is_empty())
            .unwrap_or("()".to_string());

        format!("{mname}: ({}) -> {ret}", params.join(", "))
    }

    // --- interfaces ------------------------------------------------------------

    pub(crate) fn interface_decl(&mut self, i: &InterfaceDecl) {
        let name = self.decl_name(i.name);
        let start = self.byte_start(i.span);
        let end_tok = self.toks[i.span.end as usize - 1];
        let export = if i.exported || self.ns_export {
            "export "
        } else {
            ""
        };
        let generics = i
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let mut parts: Vec<String> = i
            .extends
            .iter()
            .map(|b| self.text_of(*b).to_string())
            .collect();
        let fields: Vec<String> = i
            .fields
            .iter()
            .map(|f| {
                let modifier = f
                    .modifier
                    .map(|m| format!("{} ", self.text_of(m)))
                    .unwrap_or_default();
                let fname = self.text_of(f.name).to_string();
                let ty = self.copy_type_to_string(f.ty);

                format!("{modifier}{fname}: {ty}")
            })
            .collect();
        parts.push(format!("{{ {} }}", fields.join(", ")));
        self.generate(
            start,
            &format!("{export}type {name}{generics} = {}", parts.join(" & ")),
        );
        self.blank_lines(self.byte_end(i.name), end_tok.end);
    }

    // --- remotes -----------------------------------------------------------

    /// A struct constructs through `new` alone. `Vec2 { ... }` and
    /// `Vec2(1, 2)` are diagnostics: the first reads as a call with a
    /// table, the second calls the class, which takes the fields table. A
    /// foreign class has no class call of Alloy's, so `new` stays optional
    /// there.
    pub(crate) fn check_struct_call(&mut self, e: &Expr) {
        let (base, links) = flatten(e);

        if let Expr::Name(n) = base
            && self.is_struct_call(e)
        {
            let name = self.text_of(*n).to_string();
            let raw = matches!(
                links.first(),
                Some(Link::Plain(Step::Call {
                    args: CallArgs::Table(_),
                    ..
                }))
            );
            let ctor = self.structs_with_new.get(&name).cloned();
            let message = match (raw, ctor) {
                (true, Some(_)) => {
                    format!("`{name}` writes a constructor: construct it with `new {name}(...)`")
                }

                (true, None) => format!("construct `{name}` with `new {name} {{ ... }}`"),

                (false, Some(_)) => {
                    format!("`{name}(...)` is not a call: construct it with `new {name}(...)`")
                }

                (false, None) => format!(
                    "`{name}(...)` is not a constructor: construct it with `new {name} {{ ... }}`, since `{name}` writes no `new`"
                ),
            };
            self.diagnostics.push(Diagnostic {
                start: self.byte_start(*n),
                end: self.byte_end(*n),
                message,
            });
        }
    }

    /// `new Name { ... }` with no parentheses on a struct: the table is
    /// the fields, and the class call takes it.
    pub(crate) fn fields_form(
        &self,
        name: &Expr,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
    ) -> bool {
        let Expr::Name(n) = name else {
            return false;
        };

        args.is_none() && init.is_some() && self.structs.contains(self.text_of(*n))
    }

    /// The constructor `new Name(...)` calls: the `new` or `New` the impl
    /// wrote, else `new`, which a foreign class or an imported struct has.
    pub(crate) fn constructor_of(&self, name: &Expr) -> String {
        match name {
            Expr::Name(n) => self
                .structs_with_new
                .get(self.text_of(*n))
                .cloned()
                .unwrap_or_else(|| "new".to_string()),

            _ => "new".to_string(),
        }
    }

    /// Records a struct's name and fields for construction checks.
    pub(crate) fn note_struct(&mut self, st: &StructDecl) {
        let name = self.decl_name(st.name);
        let fields = st
            .fields
            .iter()
            .map(|f| (self.text_of(f.name).to_string(), f.default.is_some()))
            .collect();
        self.structs.insert(name.clone());

        if let Some(g) = st.generics {
            self.struct_generics
                .insert(name.clone(), self.text_of(g).trim().to_string());
        }

        self.note_field_types(&name, &st.fields);
        let wire_fields = st
            .fields
            .iter()
            .map(|f| WireField {
                name: self.text_of(f.name).to_string(),
                ty: self.text_of(f.ty).trim().to_string(),
                width: f.attributes.iter().find_map(|a| {
                    let n = self.text_of(a.name?).to_string();

                    WIRE_WIDTHS.contains(&n.as_str()).then_some(n)
                }),
            })
            .collect();
        self.struct_wire.insert(name.clone(), wire_fields);

        if st.generics.is_some() {
            self.generic_types.insert(name.clone());
        }

        if st
            .fields
            .iter()
            .any(|f| f.visibility.is_some_and(|v| self.text_of(v) == "private"))
        {
            self.private_types.insert(name.clone());
        }

        self.struct_fields.insert(name, fields);
    }

    /// `Partial<S>` at an ambient name token, with `S` declared here:
    /// the expanded table and the byte after the closing `>`.
    pub(crate) fn mapped_over_declared(
        &mut self,
        span: TokSpan,
        end: u32,
    ) -> Option<(String, u32)> {
        let name = self.text_of(span).to_string();

        if !matches!(name.as_str(), "Partial" | "Readonly" | "Sink") {
            return None;
        }

        let i = span.end as usize;
        let lt = self.toks.get(i)?;
        let target = self.toks.get(i + 1)?;
        let gt = self.toks.get(i + 2)?;

        if lt.text(self.src) != "<" || target.kind != TokKind::Ident || gt.end > end {
            return None;
        }

        let closes = gt.text(self.src) == ">" || gt.text(self.src) == ">>";

        if !closes {
            return None;
        }

        let target_name = target.text(self.src).to_string();
        let table = self.inline_mapped(&name, &target_name)?;
        // `>>` closes an outer list too; only the first `>` is this one.
        let after = if gt.text(self.src) == ">>" {
            gt.start + 1
        } else {
            gt.end
        };

        Some((table, after))
    }

    /// The annotation's base and arguments when a one-name local with a
    /// std container annotation holds that container's constructor call
    /// and nothing else: `Base.new()`, `Base.from(x)`, `new Base()`.
    pub(crate) fn annotated_constructor(&self, l: &Local) -> Option<(String, String)> {
        if l.names.len() != 1 || l.values.len() != 1 || l.names[0].destructure.is_some() {
            return None;
        }

        let head = generic_head(self.text_of(l.names[0].ty?))?;

        self.is_constructor_call(&l.values[0], &head.0)
            .then_some(head)
    }

    /// The return type's base and arguments when a `return` hands back
    /// that container's constructor call: `return HashMap.new()` under
    /// `function f(): HashMap<K, V>`. The solver infers no arguments for
    /// the call, so the annotation's arguments go on it.
    pub(crate) fn returned_constructor(&self, values: &[Expr]) -> Option<(String, String)> {
        if values.len() != 1 {
            return None;
        }

        let ty = self.ret_types.last()?.clone()?;
        let head = generic_head(&ty)?;

        self.is_constructor_call(&values[0], &head.0)
            .then_some(head)
    }

    /// A call that builds `base` and nothing else: `Base.new()`,
    /// `Base.from(x)`, `Base.with_capacity(n)`, or `new Base()`.
    pub(crate) fn is_constructor_call(&self, value: &Expr, base_name: &str) -> bool {
        match value {
            Expr::New {
                name,
                type_args: None,
                ..
            } => matches!(name.as_ref(), Expr::Name(n) if self.text_of(*n) == base_name),

            _ => {
                let (base, links) = flatten(value);

                matches!(base, Expr::Name(n) if self.text_of(*n) == base_name)
                    && matches!(
                        links.as_slice(),
                        [
                            Link::Plain(Step::Field(f)),
                            Link::Plain(Step::Call {
                                method: None,
                                type_args: None,
                                ..
                            })
                        ] if matches!(self.text_of(*f), "new" | "from" | "with_capacity")
                    )
            }
        }
    }

    /// `<K, V>` for `new Base(...)` under an annotation `Base<K, V>`.
    pub(crate) fn expected_args_for(&mut self, name: &Expr) -> String {
        let args = match (name, &self.expected_generic) {
            (Expr::Name(n), Some((base, args))) if self.text_of(*n) == base => args.clone(),

            _ => return String::new(),
        };

        self.lower_type_args(&format!("<<{args}>>"))
    }

    pub(crate) fn note_field_types(&mut self, name: &str, fields: &[Field]) {
        let types = fields
            .iter()
            .map(|f| FieldType {
                name: self.text_of(f.name).to_string(),
                ty: f.ty,
                private: f.visibility.is_some_and(|v| self.text_of(v) == "private"),
            })
            .collect();
        self.struct_field_types.insert(name.to_string(), types);
    }

    /// `Partial<S>`, `Readonly<S>`, `Sink<S>` over a struct or an
    /// interface declared here, as a plain table type. The solver's type
    /// functions cannot reduce a type that holds `Array<T>` or another
    /// recursive generic, and the fields are known, so the check
    /// artifact writes them out. `None` when the name is not one of
    /// these or the argument is not a declared type.
    pub(crate) fn inline_mapped(&mut self, mapped: &str, target: &str) -> Option<String> {
        let (prefix, optional) = match mapped {
            "Partial" => ("", "?"),
            "Readonly" => ("read ", ""),
            "Sink" => ("write ", ""),
            _ => return None,
        };
        let fields = self.struct_field_types.get(target)?.clone();
        let mut parts = Vec::new();

        for f in fields.iter().filter(|f| !f.private) {
            let ty = self.copy_type_to_string(f.ty);
            let ty = ty.trim();
            let opt = if optional.is_empty() || ty.ends_with('?') {
                ""
            } else {
                optional
            };
            parts.push(format!("{prefix}{}: {ty}{opt}", f.name));
        }

        Some(format!("{{ {} }}", parts.join(", ")))
    }

    /// Whether the check artifact splits a struct into a public view and
    /// a full one: it has a private member and no type parameters.
    pub(crate) fn has_private_view(&self, name: &str) -> bool {
        self.options.check
            && self.private_types.contains(name)
            && !self.generic_types.contains(name)
    }

    /// The type of `self` inside a struct's own code: the full view when
    /// the struct has private members, else the struct.
    /// What the check artifact adds to an `if` so the checker sees an
    /// `is` test on a type it cannot refine: a struct, an enum, an alias
    /// datatype. Each branch that tests `x is T` on a plain name starts
    /// with `local x = ((x :: any) :: T)`; `x is not T` types the else
    /// branch, or the code after a guard that leaves.
    pub(crate) fn narrowings(&self, i: &If) -> (Vec<(u32, String)>, Option<String>) {
        let mut blocks = Vec::new();

        if !self.options.check {
            return (blocks, None);
        }

        for (cond, block) in &i.branches {
            let Cond::Expr(e) = cond else {
                continue;
            };
            let mut tests = Vec::new();
            self.positive_tests(e, &mut tests);
            let prefix: String = tests
                .iter()
                .map(|(name, ty)| format!("local {name} = (({name} :: any) :: {ty}) "))
                .collect();

            if !prefix.is_empty() {
                blocks.push((block.span.start, prefix));
            }
        }

        let mut after = None;

        if i.branches.len() == 1
            && let Cond::Expr(e) = &i.branches[0].0
            && let Some((name, ty)) = self.negative_test(e)
        {
            let text = format!("local {name} = (({name} :: any) :: {ty})");

            match &i.else_block {
                Some(b) => blocks.push((b.span.start, format!("{text} "))),

                None if self.block_leaves(&i.branches[0].1) => after = Some(format!(" {text}")),

                None => {}
            }
        }

        (blocks, after)
    }

    /// The `x is T` tests an `and` chain holds, as (name, type).
    pub(crate) fn positive_tests(&self, e: &Expr, out: &mut Vec<(String, String)>) {
        match e {
            Expr::Paren { inner, .. } => self.positive_tests(inner, out),

            Expr::Binary { op, lhs, rhs, .. } if self.text_of(*op) == "and" => {
                self.positive_tests(lhs, out);
                self.positive_tests(rhs, out);
            }

            Expr::Is {
                expr,
                negated: false,
                name,
                ..
            } => {
                if let Expr::Name(n) = &**expr
                    && let Some(ty) = self.narrow_type(self.text_of(*name), self.text_of(*n))
                {
                    out.push((self.text_of(*n).to_string(), ty));
                }
            }

            _ => {}
        }
    }

    /// `x is not T`, or `not (x is T)`, as (name, type).
    pub(crate) fn negative_test(&self, e: &Expr) -> Option<(String, String)> {
        match e {
            Expr::Paren { inner, .. } => self.negative_test(inner),

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "not" => {
                let mut tests = Vec::new();
                self.positive_tests(operand, &mut tests);

                (tests.len() == 1).then(|| tests.remove(0))
            }

            Expr::Is {
                expr,
                negated: true,
                name,
                ..
            } => match &**expr {
                Expr::Name(n) => {
                    let ty = self.narrow_type(self.text_of(*name), self.text_of(*n))?;

                    Some((self.text_of(*n).to_string(), ty))
                }

                _ => None,
            },

            _ => None,
        }
    }

    /// The type an `is` narrows `value` to when Luau cannot: a struct,
    /// an enum, an imported type, or a datatype the definitions declare
    /// as an alias. A class, a primitive, and a datatype class refine on
    /// their own; `table` and `function` refine to the top types, which
    /// no index or call accepts, so those meet the value's own type.
    pub(crate) fn narrow_type(&self, name: &str, value: &str) -> Option<String> {
        match name {
            "table" => return Some(format!("typeof({value}) & {{ [any]: any }}")),

            // Both function shapes: a call yields values, and a callback
            // parameter that returns nothing accepts it.
            "function" => {
                return Some(format!(
                    "typeof({value}) & ((...any) -> ...any) & ((...any) -> ())"
                ));
            }

            _ => {}
        }

        if self.generic_types.contains(name) {
            return None;
        }

        if self.structs.contains(name) {
            return Some(if self.impl_target.as_deref() == Some(name) {
                self.self_alias(name)
            } else {
                name.to_string()
            });
        }

        if self.enums.contains_key(name)
            || self
                .options
                .import_types
                .iter()
                .any(|(_, names)| names.iter().any(|n| crate::modules::type_head(n) == name))
            || ALIAS_DATATYPES.contains(&name)
        {
            return Some(name.to_string());
        }

        None
    }

    /// Whether a block ends in a statement that leaves it: a return, a
    /// break, a continue, or an `error` call.
    pub(crate) fn block_leaves(&self, block: &Block) -> bool {
        match block.stmts.last() {
            Some(Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_)) => true,

            Some(Stmt::Call(
                Expr::Call {
                    func, method: None, ..
                },
                _,
            )) => {
                matches!(&**func, Expr::Name(n) if self.text_of(*n) == "error")
            }

            _ => false,
        }
    }

    /// ` & { read f: T }` for every `write` field a struct declares. A
    /// `write` field answers no read, and the derived code reads them
    /// all. An empty text when the struct has none.
    pub(crate) fn read_view(&mut self, decls: &[Field]) -> String {
        let mut parts = Vec::new();

        for f in decls {
            if !f.modifier.is_some_and(|m| self.text_of(m) == "write") {
                continue;
            }

            let name = self.text_of(f.name).to_string();
            let ty = self.copy_type_to_string(f.ty);
            parts.push(format!("read {name}: {ty}"));
        }

        match parts.is_empty() {
            true => String::new(),

            false => format!(" & {{ {} }}", parts.join(", ")),
        }
    }

    pub(crate) fn self_alias(&self, name: &str) -> String {
        if self.has_private_view(name) {
            format!("{name}__all")
        } else {
            name.to_string()
        }
    }

    /// The fields form names every field without a default and no field
    /// the struct lacks. A spread or a computed key turns the check off,
    /// because the table's keys are then not in the source.
    pub(crate) fn check_struct_fields(&mut self, name: &Expr, table: &Expr) {
        let Expr::Name(n) = name else {
            return;
        };
        let sname = self.text_of(*n).to_string();
        let Some(declared) = self.struct_fields.get(&sname).cloned() else {
            return;
        };
        let Expr::Table { fields, .. } = table else {
            return;
        };
        let mut given: Vec<String> = Vec::new();
        let mut open = false;

        for f in fields {
            match f {
                TableField::Named { name, .. } => {
                    let fname = self.text_of(*name).to_string();

                    if !declared.iter().any(|(d, _)| *d == fname) {
                        let known: Vec<&str> = declared.iter().map(|(d, _)| d.as_str()).collect();
                        self.diagnose(
                            *name,
                            &format!(
                                "`{sname}` has no field `{fname}`; its fields are {}",
                                list_names(&known)
                            ),
                        );
                    }

                    given.push(fname);
                }

                _ => open = true,
            }
        }

        if open {
            return;
        }

        let missing: Vec<&str> = declared
            .iter()
            .filter(|(d, has_default)| !has_default && !given.contains(d))
            .map(|(d, _)| d.as_str())
            .collect();

        if !missing.is_empty() {
            self.diagnose(
                *n,
                &format!(
                    "`new {sname} {{ ... }}` leaves {} unset; a field without a default needs a value",
                    list_names(&missing)
                ),
            );
        }
    }

    /// The two mistakes with `new` on a struct: the fields form on one that
    /// writes a constructor, outside that constructor's impl, and
    /// parentheses on one that writes none.
    pub(crate) fn check_new(
        &mut self,
        name: &Expr,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
        whole: TokSpan,
    ) {
        let Expr::Name(n) = name else {
            return;
        };
        let text = self.text_of(*n).to_string();

        // A name that is not a value with a constructor: the whole
        // expression is the error, since none of it can run.
        let what = if self.enums.contains_key(&text) && !self.structs.contains(&text) {
            Some("enum")
        } else {
            self.not_constructible.get(&text).copied()
        };

        if let Some(what) = what {
            let hint = match what {
                "enum" => format!("pick a variant: `{text}.Variant(...)`"),
                "attribute" => format!("write it above a declaration: `@{text}(...)`"),
                "trait" => "construct a struct that implements it".to_string(),
                "interface" => "construct a struct that has its fields".to_string(),
                _ => "a remote is called, not constructed".to_string(),
            };
            self.diagnostics.push(Diagnostic {
                start: self.byte_start(whole),
                end: self.byte_end(whole),
                message: format!(
                    "`{text}` is an {what} and cannot be constructed with `new`; {hint}"
                )
                .replace("an trait", "a trait")
                .replace("an remote", "a remote"),
            });

            return;
        }

        if !self.structs.contains(&text) {
            return;
        }

        let ctor = self.structs_with_new.get(&text).cloned();

        let message = if self.fields_form(name, args, init) {
            match ctor {
                Some(ctor) if self.impl_target.as_deref() != Some(text.as_str()) => format!(
                    "`{text}` writes `{ctor}`: construct it with `new {text}(...)`; the fields form is the constructor's own"
                ),

                _ => {
                    if let Some(table) = init {
                        self.check_struct_fields(name, table);
                    }

                    return;
                }
            }
        } else {
            match ctor {
                Some(_) => return,

                None => format!(
                    "`{text}` writes no `new` or `New`: construct it with `new {text} {{ ... }}`, or write `function new` in `impl {text}`"
                ),
            }
        };

        self.diagnostics.push(Diagnostic {
            start: self.byte_start(*n),
            end: self.byte_end(*n),
            message,
        });
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

    /// A trait names the return type of every method it declares, so an
    /// impl that writes another one breaks the contract.
    #[test]
    fn a_trait_method_keeps_the_return_type_the_trait_declares() {
        let src = "trait Priced as\n    function price(self): number\nend\nstruct Sword as\n    cost: number\nend\nimpl Priced for Sword as\n    function price(self): string\n        return \"free\"\n    end\nend\nprint(new Sword { cost = 1 })\n";
        assert_eq!(
            messages(src),
            vec!["the trait method `price` returns number in `Priced`, string here"]
        );

        // The same type written with other spacing is the same type.
        let same = "trait Held as\n    function slot(self): Array<number>\nend\nstruct Bag as\n    n: number\nend\nimpl Held for Bag as\n    function slot(self): Array< number >\n        return Array.new()\n    end\nend\nprint(new Bag { n = 1 })\n";
        assert!(messages(same).is_empty(), "{:?}", messages(same));
    }

    #[test]
    fn a_struct_declares_each_field_once() {
        let src = "struct S as\n    a: number\n    a: string\nend\nprint(S)\n";
        assert!(
            messages(src)
                .iter()
                .any(|m| m == "`S` declares the field `a` twice"),
            "{:?}",
            messages(src)
        );
    }

    #[test]
    fn derive_debug_writes_a_debug_method() {
        let out =
            crate::compile("@derive(Debug)\nstruct V as\n    x: number\nend\nprint(V)\n").unwrap();
        assert!(out.ship.contains("function V.debug(self)"), "{}", out.ship);
        assert!(out.ship.contains("V.__tostring = "), "{}", out.ship);
    }

    #[test]
    fn derive_eq_and_partial_eq_write_one_metamethod() {
        let out =
            crate::compile("@derive(Eq, PartialEq)\nstruct V as\n    x: number\nend\nprint(V)\n")
                .unwrap();
        assert_eq!(out.ship.matches("V.__eq = ").count(), 1, "{}", out.ship);
    }

    #[test]
    fn an_unknown_derive_lands_on_the_derive_name() {
        let src = "@derive(Sparkle)\nstruct A as\n    x: number\nend\nprint(A)\n";
        let out = crate::compile(src).unwrap();
        assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
        assert_eq!(
            &src[out.diagnostics[0].start as usize..out.diagnostics[0].end as usize],
            "Sparkle"
        );
    }

    #[test]
    fn a_generic_struct_types_its_constructor() {
        let out = crate::compile(
            "struct Box<T> as\n    value: T\n    count: number = 1\nend\nlocal b = new Box<<number>> { value = 5 }\nprint(b)\n",
        )
        .unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("function Box.__new<T>(f: { value: T, count: number? }): Box<T>"),
            "{}",
            out.check
        );
    }

    #[test]
    fn an_impl_that_leaves_the_parameters_out_reports() {
        let src = "struct Box<T> as\n    value: T\nend\nimpl Box as\n    function get(self): T\n        return self.value\n    end\nend\nprint(Box)\n";
        let out = crate::compile(src).unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages.contains(
                &"the struct `Box` takes `<T>`; write `impl Box<T>` so its methods can name them"
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn an_impl_may_name_the_structs_parameters() {
        let src = "struct Box<T> as\n    value: T\nend\nimpl Box<T> as\n    function get(self): T\n        return self.value\n    end\nend\nprint(Box)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("function Box.get<T>(self: Box<T>): T"),
            "{}",
            out.check
        );
        // The header has no Luau form and never reaches the output.
        assert!(!out.ship.contains("impl"), "{}", out.ship);
    }

    #[test]
    fn a_returned_constructor_takes_the_return_type_arguments() {
        // `return HashMap.new()` infers nothing on its own, so the
        // declared return type names the arguments.
        let src = "function make(): HashMap<string, number[]>\n    return HashMap.new()\nend\nprint(make())\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("HashMap.new<<string, __alloy.Array<number>>>()"),
            "{}",
            out.check
        );
    }
}
