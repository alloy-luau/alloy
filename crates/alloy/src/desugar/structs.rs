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
use super::modules::type_arguments;
use super::remotes::WIRE_WIDTHS;
use super::statements::{Piece, is_string_index_table};
use super::types::{generic_head, names_other_args, strip_bounds};
use super::*;

/// Datatypes the definitions declare as a type alias, not a class. A
/// `typeof` test on one names it at run time, but the checker cannot
/// refine a value by it, so the check artifact narrows by a cast.
pub(crate) const ALIAS_DATATYPES: &[&str] = &["RBXScriptSignal"];

/// The traits an `impl` writes a metamethod for: the trait's name, the
/// method it asks for, and the metamethod the emit sets. `alloy doc
/// trait` lists the same set.
pub(crate) const OPERATOR_TRAITS: &[(&str, &str, &str)] = &[
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

impl<'s> Desugar<'s> {
    /// `impl X ... end`: each method lands on `X`; operator traits map to
    /// metamethods.
    pub(crate) fn impl_decl(&mut self, i: &ImplDecl) {
        // A struct of the namespace this `impl` sits in renders under
        // the namespace's own name.
        let target_name = self.impl_target_name(i.target);
        let start = self.byte_start(i.span);

        // The methods land on the struct's table, which is a nil local
        // until the struct's line runs: the file failed at load.
        if self
            .struct_at
            .get(&target_name)
            .is_some_and(|at| *at > start)
        {
            let shown = self.display_name(&target_name);
            let kind = if self.enums.contains_key(&target_name) {
                "enum"
            } else {
                "struct"
            };
            let message = format!(
                "`{shown}` is declared below this impl; move the impl below the {kind}, since its methods land on the {kind}'s table"
            );
            self.diagnose(i.target, &message);
        }

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

        // The header carries no type slot into the emit, so a name that
        // is nowhere reads as a Luau global inside the body, or says
        // nothing at all when the body is empty. Either spelling
        // answers: a namespace member renders under a name only the
        // declaring file binds, and `Zoo.Lion` reads the import.
        let written = self.text_of(i.target).to_string();

        if !self.knows_type(&target_name) && !self.knows_type(&written) {
            let message = format!(
                "nothing declares `{written}`; an `impl` targets a struct, an enum, or a foreign type"
            );
            self.diagnose(i.target, &message);
        }

        // Each variant keeps its name in the field `tag`, so a method
        // of that name is shadowed on every value.
        if self.enums.contains_key(&target_name) {
            for m in &i.methods {
                if let Some(&n) = m.path.last()
                    && self.text_of(n) == "tag"
                {
                    self.diagnose(
                        n,
                        "an enum method cannot be named `tag`; each variant keeps its name in the field `tag`",
                    );
                }
            }
        }

        // A trait of the namespace this `impl` sits in is keyed under
        // the namespace's prefix, `Group_Greeter`; the source writes
        // `Greeter`. The resolved name reads the contract, and the emit
        // reads the default methods off the table the file binds.
        let trait_name = i.trait_name.map(|t| {
            let resolved = self.impl_target_name(t);

            match self.traits.contains_key(&resolved) {
                true => resolved,

                false => self.text_of(t).to_string(),
            }
        });

        if let Some(t) = i.trait_name
            && let Some(name) = trait_name.as_deref()
            && !super::attributes::BUILTIN_BOUNDS.contains(&name)
            && !OPERATOR_TRAITS.iter().any(|(n, _, _)| *n == name)
            && !self.knows_type(name)
        {
            self.diagnose(t, &format!("nothing declares the trait `{name}`"));
        }

        // A foreign target gets a registry table instead of its metatable.
        let foreign = self.is_foreign(&target_name);
        self.ext_hit |= foreign;

        // A struct with private members: a private method lands on
        // `Target__private` in the check artifact, and a public method
        // rebinds `self` to the full view on its first line.
        // Only the struct's own file declares the private table and the
        // full view, so an impl on an imported struct writes its private
        // methods on the class table, as the ship artifact does.
        let declares_target =
            self.structs.contains(&target_name) || self.enums.contains_key(&target_name);
        // The table each method lands on. A namespace member renders
        // under `Zoo_Lion`, which the declaring file binds as a local;
        // a file that imports the namespace binds the path alone, so
        // there the methods land on `Zoo.Lion`.
        let target = match (foreign, declares_target) {
            (true, _) => "__impl".to_string(),

            (false, true) => target_name.clone(),

            (false, false) => written.clone(),
        };
        let split = !foreign && declares_target && self.has_private_view(&target_name);
        // An impl of a struct another file declares: its private methods
        // go on the class table, and `self` reads the full view the
        // import aliased, so a call of a private member still types.
        let imported_view = !foreign && !declares_target && self.reads_private_view(&target_name);

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
            let kind = if self.enums.contains_key(&target_name) {
                "enum"
            } else {
                "struct"
            };
            let message = format!(
                "the {kind} `{target_name}` takes `{params}`; write `impl {target_name}{params}` so its methods can name them"
            );
            self.diagnose(i.target, &message);
        }
        let types_self = self.options.check && (foreign || local_type || !impl_generics.is_empty());

        self.impl_target = Some(target_name.clone());

        // A method of a field's name is written on the class table, and
        // a field lives on the instance; the method wins on every
        // instance that leaves the field nil.
        if let Some(fields) = self.struct_fields.get(&target_name).cloned() {
            let mut clashes = Vec::new();

            for m in &i.methods {
                let Some(first) = m.path.first() else {
                    continue;
                };
                let name = self.text_of(*first).to_string();

                if fields.iter().any(|(f, _)| *f == name) {
                    clashes.push((*first, name));
                }
            }

            for (span, name) in clashes {
                let shown = self.display_name(&target_name);
                let message = format!(
                    "`{name}` is a field of `{shown}` and a method of its impl; one name holds one of the two"
                );
                self.diagnose(span, &message);
            }
        }

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

            if ((split && !is_private) || imported_view) && has_self {
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
            self.impl_method = Some(mname.clone());
            // `function name(` becomes `function Target.name(`. The
            // `private` or `public` word has no Luau form and goes, and
            // so does `async`: the body wrap is what it turns into, and
            // a copy of it would sit in front of the Luau header.
            // A cut range, with the text that takes its place.
            let mut cut: Vec<(u32, u32, String)> = Vec::new();
            // User attributes, which attach to the method after its `end`.
            let mut user: Vec<String> = Vec::new();

            for a in &m.attrs {
                let range = (self.byte_start(a.span), self.byte_end(a.span));

                match a.name.map(|n| self.text_of(n).to_string()).as_deref() {
                    // `@test` makes a function local and registers it with
                    // the runner, which calls it by name. A method takes a
                    // receiver, so no runner can call it. Luau has no
                    // `@test` either, so the attribute leaves the emit.
                    Some("test") => {
                        self.diagnose(a.span, "`@test` goes on a function, not a method");
                        cut.push((range.0, range.1, String::new()));
                    }

                    // Luau reads the message of `@deprecated` from a table,
                    // as `attributed_function` writes it.
                    Some(n @ ("native" | "checked" | "deprecated")) if !a.args.is_empty() => {
                        let args: Vec<String> =
                            a.args.iter().map(|e| self.render_to_string(e)).collect();
                        let text = match n {
                            "deprecated" => {
                                format!("@[deprecated {{reason = {}}}]", args.join(", "))
                            }

                            _ => format!("@[{n}({})]", args.join(", ")),
                        };
                        cut.push((range.0, range.1, text));
                    }

                    Some("native" | "checked" | "deprecated") | None => {}

                    // The lints read `@allow`; Luau reads none of it.
                    Some("allow") => cut.push((range.0, range.1, String::new())),

                    // An attribute that reaches no function is a
                    // diagnostic already, and Luau reads none of it.
                    Some(n) => {
                        if self.attr_reaches(n, "function") {
                            let args = self.attr_args(a, n);
                            user.push(format!(
                                "{} = {{ {} }}",
                                super::attributes::attr_key(n),
                                args.join(", ")
                            ));
                        }

                        cut.push((range.0, range.1, String::new()));
                    }
                }
            }

            if let Some(v) = m.visibility {
                cut.push((self.byte_start(v), self.byte_end(v), String::new()));
            }

            if let Some(a) = m.body.is_async {
                cut.push((self.byte_start(a), self.byte_end(a), String::new()));
            }

            cut.sort_unstable();
            let mut head = ms;

            for (from, to, text) in cut {
                if from >= head {
                    self.copy(head, from);

                    if !text.is_empty() {
                        self.generate(from, &text);
                    }

                    head = to;
                }
            }

            self.copy(head, fn_tok_end);

            let owner = if split && is_private {
                format!("{target}__private")
            } else {
                target.clone()
            };
            // The insert anchors on the method's own name, not on the
            // gap after `function`: a diagnostic the checker puts on the
            // inserted owner then lands on a name the source shows.
            // A method of a generic impl carries the impl's parameters.
            // One with its own list gets them in front of its own:
            // `map<U>` under `impl Box<T>` reads `map<T, U>`. A static
            // method that names none of them takes none: a `T` nothing
            // reads is one an explicit `of<<number>>` binds by mistake.
            let impl_params: Vec<&str> = impl_generics
                .trim_start_matches('<')
                .trim_end_matches('>')
                .split(',')
                .map(|p| p.trim().trim_end_matches("..."))
                .filter(|p| !p.is_empty())
                .collect();
            let carries = has_self
                || (m.span.start as usize..m.span.end as usize).any(|k| {
                    self.toks[k].kind == TokKind::Ident
                        && impl_params.contains(&self.toks[k].text(self.src))
                });
            let method_generics = if m.body.generics.is_none() && carries {
                impl_generics.as_str()
            } else {
                ""
            };
            self.generate(
                self.byte_start(name_span),
                &format!(" {owner}.{mname}{method_generics}"),
            );

            let mut rest = TokSpan::new(name_span.end as usize, m.span.end as usize);

            if let Some(g) = m.body.generics
                && carries
                && let Some(inner) = impl_generics
                    .strip_prefix('<')
                    .and_then(|rest| rest.strip_suffix('>'))
                && !inner.is_empty()
            {
                // The renderer writes in order: copy through the `<`,
                // write the impl's list, and the rest starts after it.
                let open = self.byte_start(g);
                self.copy(self.byte_end(name_span), open + 1);
                self.generate(open + 1, &format!("{inner}, "));
                rest = TokSpan::new(g.start as usize + 1, m.span.end as usize);
            }

            // The prologue sits on the header line, so a method that
            // carries one takes the header path too.
            if function_needs_rewrite(&m.body)
                || self.self_type.is_some()
                || self.self_prologue.is_some()
            {
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

            if !user.is_empty() {
                let std = self.std();
                self.generate(
                    cursor,
                    &format!(" {std}.attach({owner}.{mname}, {{ {} }})", user.join(", ")),
                );
            }

            self.self_prologue = None;
            self.impl_method = None;
        }

        self.self_type = None;
        self.impl_target = None;

        let end_tok = self.toks[i.span.end as usize - 1];
        self.copy(cursor, end_tok.start);

        // Operator traits.
        let mut tail = String::new();

        if let (Some(t), Some(trait_name)) = (i.trait_name, trait_name) {
            for (tr, method, meta) in OPERATOR_TRAITS {
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

            // A trait is a contract: every method without a body appears
            // in the impl, with the same arity. The trait may sit in
            // another module, which carries the same list.
            // The contract keys by the declared name. The source may
            // write an import alias, `impl G for B`, or a namespace
            // path, `impl Ns.Greet for C`; both name the same trait.
            let names = self.name_candidates(&trait_name);
            let shown = self.display_name(&trait_name);
            let required = names
                .iter()
                .find_map(|n| self.trait_required.get(n).cloned())
                .or_else(|| {
                    self.options
                        .import_trait_methods
                        .iter()
                        .find(|(t, _)| names.contains(t))
                        .map(|(_, m)| m.clone())
                });

            if let Some(required) = required {
                for (m, arity, ret) in required {
                    let written = i.methods.iter().find(|f| self.text_of(f.path[0]) == m);

                    match written {
                        None => self.diagnose(
                            t,
                            &format!(
                                "`impl {shown} for {}` does not write `{m}`; the trait declares it",
                                self.display_name(&target_name)
                            ),
                        ),

                        Some(f)
                            if f.body.params.len() != arity
                                && !f.body.params.iter().any(|p| p.is_vararg) =>
                        {
                            self.diagnose(
                                f.path[0],
                                &format!(
                                    "the trait method `{m}` takes {} parameter{} in `{shown}`, {} here",
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
                                // An `async` impl of an `async`
                                // signature settles with the type it
                                // writes, so it answers `Future<T>`.
                                let got = match f.body.is_async.is_some()
                                    && crate::desugar::names_a_future(want)
                                {
                                    true => format!("Future<{got}>"),

                                    false => got,
                                };

                                if !same_type_text(want, &got) {
                                    self.diagnose(
                                        t,
                                        &format!(
                                            "the trait method `{m}` returns {want} in `{shown}`, {got} here"
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
            let defaults = names
                .iter()
                .find_map(|n| self.traits.get(n).cloned())
                .or_else(|| {
                    self.options
                        .import_trait_defaults
                        .iter()
                        .find(|(t, _)| names.contains(t))
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

        // The derives write instance methods too; the alias lists them,
        // or `b:clone()` on a `Box<number>` finds no key.
        let whole = format!("{name}{generics}");

        if self.cloneable.contains(name) {
            members.push(format!("read clone: (self: {whole}) -> {whole}"));
        }

        if self.serializable.contains(name) {
            members.push(format!("read to_table: (self: {whole}) -> any"));
            members.push(format!("read serialize: (self: {whole}) -> any"));
        }

        // `function swap(self): Pair<B, A>` would make the alias name
        // itself with other arguments, which Luau rejects. The metatable
        // form takes those structs back.

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
    /// The wire widths on the fields of a struct. A field packs at one
    /// width, so a second one went in silence: the report names the
    /// width already on the field.
    pub(crate) fn check_field_widths(&mut self, st: &StructDecl) {
        let mut hits: Vec<(TokSpan, String)> = Vec::new();

        for f in &st.fields {
            let widths: Vec<(TokSpan, String)> = f
                .attributes
                .iter()
                .filter_map(|a| {
                    let name = a.name?;
                    let word = self.text_of(name).to_string();

                    WIRE_WIDTHS
                        .contains(&word.as_str())
                        .then_some((a.span, word))
                })
                .collect();
            let Some((_, first)) = widths.first() else {
                continue;
            };
            let fname = self.text_of(f.name).to_string();
            // A width packs a number. The wire spec read the field's own
            // type and dropped the width, so an array field took one and
            // crossed at 64 bits.
            let fty = self.text_of(f.ty).trim();
            let seen = self
                .alias_values
                .get(fty.trim_end_matches('?').trim())
                .cloned();

            if let Some(base) = super::remotes::width_misfit(seen.as_deref().unwrap_or(fty)) {
                hits.push((
                    f.name,
                    format!("`@{first}` packs a `number`; field `{fname}` is `{base}`"),
                ));
            }

            // A default the width cannot hold fails every value built
            // without the field, the way a remote default does.
            if let (Some(d), Some((lo, hi))) = (&f.default, super::remotes::width_range(first))
                && let Ok(v) = self
                    .text_of(d.span())
                    .replace(['_', ' '], "")
                    .parse::<f64>()
                && (v < lo || v > hi || v.fract() != 0.0)
            {
                let shown = self.text_of(d.span()).to_string();
                hits.push((
                    d.span(),
                    format!("`@{first}` holds a whole number from {lo} to {hi}; the default of `{fname}`, {shown}, does not fit"),
                ));
            }

            for (span, _) in widths.iter().skip(1) {
                hits.push((
                    *span,
                    format!("`{fname}` takes one wire width; `@{first}` is already on it"),
                ));
            }
        }

        for (span, message) in hits {
            self.diagnose(span, &message);
        }
    }

    pub(crate) fn struct_decl(&mut self, st: &StructDecl) {
        self.check_field_widths(st);
        self.check_serde_attrs(st);
        let name = self.decl_name(st.name);
        let start = self.byte_start(st.span);
        let end_tok = self.toks[st.span.end as usize - 1];
        let generics = st
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let export = if st.exported || self.ns_export || self.export_listed_types.contains(&name) {
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
        // Each `@alias` key and the field it stands for.
        let mut aliases: Vec<(String, String)> = Vec::new();

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

            // `@alias("hp")`: another key that reads and writes the same
            // slot, so the type holds it too.
            for alias in self.attr_strings(&f.attributes, "alias") {
                let key = crate::data::luau_key(&alias);
                field_types.push(format!("{modifier}{key}: {ty}"));

                if f.visibility.is_some_and(|v| self.text_of(v) == "private") {
                    private_types.push(format!("{modifier}{key}: {ty}"));
                } else {
                    public_types.push(format!("{modifier}{key}: {ty}"));
                }

                aliases.push((alias, fname.clone()));
            }

            if let Some(dv) = &f.default {
                // The constructor fills a default before the struct
                // exists, so a sibling field's name reads a global.
                let span = dv.span();

                for k in span.start as usize..span.end as usize {
                    let word = self.toks[k].text(self.src);
                    let sibling = st.fields.iter().any(|g| self.text_of(g.name) == word);

                    if sibling
                        && !self.own_names.contains(word)
                        && !self.imported_names.contains(word)
                        && self.reads_name(k, word, false)
                    {
                        let message = format!(
                            "a default cannot read the field `{word}`; the constructor fills defaults before the fields exist"
                        );
                        self.diagnose(TokSpan::new(k, k + 1), &message);
                    }
                }

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
                "{export}type {name} = typeof(setmetatable({{}} :: {{ {} }}, {name})) {export}type {name}__all = {name}{hidden} & typeof({name}__private)",
                public_types.join(", ")
            )
        } else if let Some(members) =
            self.generic_alias_members(&name, &type_arguments(&generics), &field_types)
        {
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
        // A default belongs to the `type` alias alone. A function's
        // generic list and an instantiation of the struct take the
        // parameter names, the way a Luau alias refers to its own.
        let fn_generics = if generics.is_empty() {
            String::new()
        } else {
            type_arguments(&generics)
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
        let head = self.decl_head(&name);
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
                "{head}{name}.__index = {name}{private_table} function {name}.__new{fn_generics}({param}){ret} {d} return (setmetatable(f, {name}) :: any) end{new_fn}"
            )
        } else {
            format!(
                "{head}{name}.__index = {name} setmetatable({name}, {{ __call = function(_, f) {d} return setmetatable(f, {name}) end }}) function {name}.new(f) return {name}(f) end"
            )
        };
        self.generate(start, &header);

        // Field lines carry only their trivia, comments kept. The range
        // starts at the attributes, so an attribute line above `struct`
        // keeps its newline.
        self.blank_keeping_comments(start, end_tok.start);

        // Derives and attributes on the `end` line.
        let mut tail = type_line;
        let mut derives_debug = false;
        let mut derived: HashSet<String> = HashSet::new();

        for a in &st.attributes {
            let Some(aname) = a.name else { continue };

            if self.text_of(aname) == "derive" {
                for arg in &a.args {
                    let which = self.derive_name(arg);

                    // A path through a star import needs no import of the
                    // name; the star import is one.
                    if self.text_of(arg.span()) == which {
                        self.check_std_name(arg.span(), &which);
                    }

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
                        &st.attributes,
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
        let sealed = st
            .attributes
            .iter()
            .any(|a| a.name.is_some_and(|n| self.text_of(n) == "sealed"));

        // `@alias("hp")` on `health`: `x.hp` reads and writes the slot
        // of `health` itself. The alias key is never a key of the
        // value, so both metamethods see every use of it.
        let alias_map = match aliases.is_empty() || self.options.check {
            true => String::new(),

            false => {
                let pairs: Vec<String> = aliases
                    .iter()
                    .map(|(a, f)| format!("{} = {}", crate::data::luau_key(a), luau_string(f)))
                    .collect();
                tail.push_str(&format!(
                    " local {name}__alias = {{ {} }} {name}.__index = function(t, k) local a = {name}__alias[k] if a ~= nil then return rawget(t, a) end return {name}[k] end",
                    pairs.join(", ")
                ));

                format!("local a = {name}__alias[k] if a ~= nil then rawset(t, a, v) return end ")
            }
        };

        if !self.options.check && sealed {
            let keys: Vec<String> = field_names.iter().map(|f| format!("{f} = true")).collect();
            let shown = luau_string(&self.display_name(&name));
            tail.push_str(&format!(
                " {name}.__newindex = function(t, k, v) {alias_map}if ({{ {} }})[k] then rawset(t, k, v) else error(string.format(\"%s has no field %s\", {shown}, tostring(k)), 2) end end",
                keys.join(", "),
            ));
        } else if !alias_map.is_empty() {
            tail.push_str(&format!(
                " {name}.__newindex = function(t, k, v) {alias_map}rawset(t, k, v) end"
            ));
        }

        tail.push_str(&self.foreign_impl_lines(&name));
        tail.push_str(&self.wire_registration(&name));
        self.generate(end_tok.start, &format!(" {tail}"));

        if st.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// The methods another file's `impl` puts on this type, declared on
    /// the class table. The runtime attaches them through the require;
    /// without the declaration the checker calls the write an added
    /// property and every reader a missing key. The check artifact
    /// alone carries them: the ship artifact must write no key the
    /// source did not.
    pub(crate) fn foreign_impl_lines(&self, name: &str) -> String {
        if !self.options.check {
            return String::new();
        }

        let mut out = String::new();

        for e in self
            .options
            .foreign_impls
            .iter()
            .filter(|e| e.head().0 == name)
        {
            let generics = e.head().1;
            let mut params: Vec<String> = Vec::new();

            // `impl Box<T>` in another file: the stub binds `T` itself.
            if !e.is_static {
                let args = super::modules::type_arguments(generics);
                params.push(format!("self: {name}{args}"));
            }

            if !e.params.is_empty() {
                params.push(e.params.clone());
            }

            // A method that declares no return type returns what its
            // body returns; `()` would reject the body's own value.
            let ret = e.ret.clone().unwrap_or_else(|| "any".to_string());
            out.push_str(&format!(
                " {name}.{} = (nil :: any) :: {generics}({}) -> {ret}",
                e.name,
                params.join(", ")
            ));
        }

        out
    }

    /// Copies the lines in a range as blank lines, keeping the newlines
    /// and the comments between the tokens.
    fn blank_keeping_comments(&mut self, start: u32, end: u32) {
        let spans: Vec<(u32, u32)> = self
            .toks
            .iter()
            .filter(|t| t.start >= start && t.end <= end)
            .map(|t| (t.start, t.end))
            .collect();
        let mut cursor = start;

        for (a, b) in spans.into_iter().chain([(end, end)]) {
            let gap = &self.src[cursor as usize..a as usize];
            let mut i = 0;

            while i < gap.len() {
                let at = cursor + i as u32;

                if gap[i..].starts_with("--") {
                    let long = gap[i + 2..]
                        .strip_prefix('[')
                        .map(|r| r.bytes().take_while(|&c| c == b'=').count())
                        .filter(|&n| gap[i + 3 + n..].starts_with('['))
                        .and_then(|n| {
                            let close = format!("]{}]", "=".repeat(n));

                            gap[i..].find(&close).map(|e| i + e + close.len())
                        });
                    let stop = long
                        .or_else(|| gap[i..].find('\n').map(|e| i + e))
                        .unwrap_or(gap.len());
                    self.copy(at, cursor + stop as u32);
                    i = stop;
                } else {
                    if gap.as_bytes()[i] == b'\n' {
                        self.copy(at, at + 1);
                    }

                    i += 1;
                }
            }

            self.blank_lines(a, b);
            cursor = b;
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
            let name = self.attr_name(n);

            // `@wire(buffer)` names a mode with a bare word, which would
            // emit the `buffer` library; the runtime reads `pack` instead.
            if matches!(
                name.as_str(),
                "derive" | "test" | "native" | "checked" | "deprecated" | "cfg" | "allow" | "wire"
            ) {
                continue;
            }

            let args = self.attr_args(a, &name);
            parts.push(format!(
                "{} = {{ {} }}",
                super::attributes::attr_key(&name),
                args.join(", ")
            ));
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
        struct_attrs: &[Attr],
    ) -> String {
        // The check artifact types the receiver, as an impl method's self.
        // A derive reads every field, and a `write` field is not
        // readable through the struct's own type, so the receiver meets
        // a read view of those fields.
        let readable = self.read_view(decls);
        let sn = if self.options.check && !self.generic_types.contains(name) {
            format!(": {name}{readable}")
        } else {
            String::new()
        };
        // A derived method takes the struct's public type, so a caller
        // outside the impl passes the value it holds. A body reads the
        // private fields through the full view, a subtype it casts to.
        let full =
            self.options.check && !self.generic_types.contains(name) && self.has_private_view(name);
        let view = |x: &str| match full {
            true => format!("({x} :: {name}__all)"),

            false => x.to_string(),
        };
        let (a, b, s_, this) = (view("a"), view("b"), view("s"), view("self"));
        let tn = if self.options.check { ": any" } else { "" };
        // A named derived method of a generic struct takes the struct's
        // parameters, `Box.clone<T>(self: Box<T>): Box<T>`; a bare `Box`
        // is no type there.
        let g = match self.struct_generics.get(name) {
            Some(t) if self.options.check => super::modules::type_arguments(t),

            _ => String::new(),
        };
        let gself = match g.is_empty() {
            true => sn.clone(),

            false => format!(": {name}{g}"),
        };
        let gret = if self.options.check {
            format!(": {name}{g}")
        } else {
            String::new()
        };

        // A derive writes these methods on every value; a field of the
        // name would hide the method.
        let writes: &[&str] = match which {
            "Clone" => &["clone"],

            "Debug" => &["debug"],

            "Serialize" => &["to_table", "serialize"],

            "Deserialize" => &["from_table"],

            "Default" => &["default"],

            "Eq" | "PartialEq" => &["eq"],

            "Ord" => &["lt", "le"],

            _ => &[],
        };

        for f in decls {
            let fname = self.text_of(f.name);

            if writes.contains(&fname) {
                let message = format!(
                    "`{fname}` is a field of `{name}` and a method `@derive({which})` writes; one name holds one of the two"
                );
                self.diagnose(f.name, &message);
            }
        }
        match which {
            "Eq" | "PartialEq" => {
                // A scalar compares with `==`; a field that holds a table,
                // an array or a map, compares by content, as Rust's derive
                // compares a Vec. `==` on two arrays asked for identity.
                let std = self.std();
                let cmp: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let scalar = decls
                            .iter()
                            .find(|d| self.text_of(d.name) == f.as_str())
                            .map(|d| self.text_of(d.ty).trim().trim_end_matches('?').trim())
                            .is_some_and(|t| matches!(t, "number" | "string" | "boolean"));

                        match scalar {
                            true => format!("{a}.{f} == {b}.{f}"),

                            false => format!("{std}.deep_eq({a}.{f}, {b}.{f})"),
                        }
                    })
                    .collect();
                let body = if cmp.is_empty() {
                    "true".to_string()
                } else {
                    cmp.join(" and ")
                };

                // The `Eq` bound asks for `eq`, so the derive writes the
                // method beside the metamethod, the way an `impl` does.
                format!(
                    "{name}.__eq = function(a{sn}, b{sn}) return {body} end {name}.eq = {name}.__eq"
                )
            }

            // Field by field, in declaration order: the first field that
            // differs decides, as a tuple compares.
            "Ord" => {
                let steps: Vec<String> = fields
                    .iter()
                    .map(|f| format!("if {a}.{f} ~= {b}.{f} then return {a}.{f} < {b}.{f} end"))
                    .collect();
                let steps = steps.join(" ");

                // The `Ord` bound asks for `lt` and `le`, the methods an
                // `impl Ord` writes.
                format!(
                    "{name}.__lt = function(a{sn}, b{sn}) {steps} return false end {name}.__le = function(a{sn}, b{sn}) {steps} return true end {name}.lt = {name}.__lt {name}.le = {name}.__le"
                )
            }

            "Debug" => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| format!("\"{f} = \" .. tostring({s_}.{f})"))
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
                let ret = &gret;
                // A field clones the way Rust's derive clones it: a struct
                // that derives Clone through its own `clone`, an array or
                // a table as a new one. Any other value is shared.
                let mut deep = String::new();

                for f in decls {
                    let fname = self.text_of(f.name).to_string();
                    let ty = self.text_of(f.ty).trim().to_string();
                    let (inner, optional) = match ty.strip_suffix('?') {
                        Some(t) => (t.trim().to_string(), true),

                        None => (ty.clone(), false),
                    };
                    let field = format!("v.{fname}");
                    let copy = match self.clone_of(&inner, &field) {
                        Some(copy) => copy,

                        None => continue,
                    };

                    deep.push_str(&match optional {
                        true => format!(" if {field} ~= nil then {field} = {copy} end"),

                        false => format!(" {field} = {copy}"),
                    });
                }

                let value = self.any_cast(&format!("setmetatable(table.clone(self), {name})"));

                match deep.is_empty() {
                    true => {
                        format!("function {name}.clone{g}(self{gself}){ret} return {value} end")
                    }

                    false => format!(
                        "function {name}.clone{g}(self{gself}){ret} local v = {}{deep} return v end",
                        self.any_cast(&format!("setmetatable(table.clone(self), {name})"))
                    ),
                }
            }

            "Default" => {
                let ret = &gret;
                let mut parts: Vec<String> = Vec::new();

                for f in decls {
                    // The constructor writes a default the field declares.
                    if f.default.is_some() {
                        continue;
                    }

                    let fname = self.text_of(f.name).to_string();
                    let ty = self.text_of(f.ty).trim().to_string();

                    match self.zero_of(&ty) {
                        Some(Some(value)) => parts.push(format!("{fname} = {value}")),

                        Some(None) => {}

                        None => {
                            let message = format!(
                                "`@derive(Default)` needs a starting value for `{fname}: {ty}`; write one, `{fname}: {ty} = ...`, or derive Default on the type"
                            );
                            self.diagnose(f.name, &message);
                        }
                    }
                }

                // A generic struct's table is still being typed here, and
                // Luau refuses the fields table against the raw
                // constructor's own `T`; the table casts, and `default<T>`
                // names the result.
                let fields = format!("{{ {} }}", parts.join(", "));
                let fields = match g.is_empty() {
                    true => fields,

                    false => self.any_cast(&fields),
                };
                let value = format!("{}({fields})", self.raw_ctor(name));

                // `T` sits in the result alone, which a call site cannot
                // fill from `local b: Box<number> =`; a generic default
                // answers `any` and the binding's annotation types it.
                match g.is_empty() {
                    true => format!("function {name}.default(){ret} return {value} end"),

                    false => format!(
                        "function {name}.default() return {} end",
                        self.any_cast(&value)
                    ),
                }
            }

            "Serialize" | "Deserialize" => {
                // A struct that derives both reads its fields once for
                // the reports; the second derive writes its half alone.
                let report = which == "Serialize" || !self.serializable.contains(name);
                // The container options, serde's: every key under one
                // case, and a table that holds no key the struct lacks.
                let rename_all = self
                    .attr_strings(struct_attrs, "rename_all")
                    .into_iter()
                    .next();
                let deny = struct_attrs.iter().any(|a| {
                    a.name
                        .is_some_and(|n| self.attr_name(n) == "deny_unknown_fields")
                });
                let mut known: Vec<String> = Vec::new();
                let mut to = Vec::new();
                let mut from = Vec::new();
                let mut keys: Vec<(String, String)> = Vec::new();

                for f in decls {
                    let fname = self.text_of(f.name).to_string();
                    let skip = f
                        .attributes
                        .iter()
                        .any(|a| a.name.is_some_and(|n| self.attr_name(n) == "skip"));

                    if skip {
                        continue;
                    }

                    // The serializer writes what a type names, and `~T`
                    // names what a value is not.
                    if report && self.has_negation(f.ty) {
                        let ty = self.text_of(f.ty).to_string();
                        let message = format!(
                            "a struct that derives {which} writes each field's type: `{fname}` has type `{ty}`, a negation; name the types it holds, or mark it @skip"
                        );
                        self.diagnose(f.ty, &message);
                    }

                    let key = f
                        .attributes
                        .iter()
                        .find(|a| a.name.is_some_and(|n| self.attr_name(n) == "rename"))
                        .and_then(|a| a.args.first())
                        .map(|e| {
                            // The key is the text the literal stands for,
                            // so a quote style or an escape changes nothing.
                            let text = self.text_of(e.span());

                            crate::data::literal_text(text).unwrap_or_else(|| text.to_string())
                        })
                        .unwrap_or_else(|| {
                            rename_all
                                .as_deref()
                                .and_then(|style| rename_case(&fname, style))
                                .unwrap_or(fname.clone())
                        });
                    let aliases = self.attr_strings(&f.attributes, "alias");
                    known.push(key.clone());
                    known.extend(aliases.iter().cloned());

                    // Two fields under one key: the table holds one.
                    if let Some((_, other)) = keys.iter().find(|(k, _)| *k == key)
                        && report
                    {
                        let message = format!(
                            "`{other}` and `{fname}` serialize under one key, `{key}`, and the derived table keeps one; give one of them another `@rename`"
                        );
                        self.diagnose(f.name, &message);
                    }

                    keys.push((key.clone(), fname.clone()));
                    // A key that is no Luau name, `regen-per-second` or
                    // `end`, goes in brackets on both sides.
                    let key = crate::data::luau_key(&key);
                    let at_key = |k: &str| match k.starts_with('[') {
                        true => format!("t{k}"),

                        false => format!("t.{k}"),
                    };
                    // An alias is another key the table may hold the
                    // field under: the field's own key wins, and the last
                    // alias is the fallback. An `else nil` would type the
                    // read as nil and fail a field that is not optional.
                    let read = match aliases.split_last() {
                        None => at_key(&key),

                        Some((last, rest)) => {
                            let mut chain = format!("(if {0} ~= nil then {0}", at_key(&key));

                            for alias in rest {
                                let other = at_key(&crate::data::luau_key(alias));
                                chain.push_str(&format!(" elseif {other} ~= nil then {other}"));
                            }

                            chain.push_str(&format!(
                                " else {})",
                                at_key(&crate::data::luau_key(last))
                            ));
                            chain
                        }
                    };
                    let field = format!("{this}.{fname}");
                    let ty = self.text_of(f.ty).trim().to_string();
                    let (out, back) = (
                        self.serde_of("Serialize", &ty, &field, 0),
                        self.serde_of("Deserialize", &ty, &read, 0),
                    );
                    // A key an older save lacks reads nil, and the
                    // constructor then writes the field's default; a
                    // rebuild of the missing value would raise instead.
                    let back = match back {
                        // A required field's slot takes no nil, so the
                        // guarded read casts; the table from JSON is
                        // untyped either way.
                        Some(b) if !ty.ends_with('?') => {
                            let guarded = format!("(if {read} == nil then nil else {b})");

                            match f.default.is_some() {
                                true => guarded,

                                false => self.any_cast(&guarded),
                            }
                        }

                        Some(b) => b,

                        None => read,
                    };
                    to.push(format!("{key} = {}", out.unwrap_or(field)));
                    from.push(format!("{fname} = {back}"));
                }

                // `serialize` is the name the `Serialize` bound asks
                // for. It calls `to_table`, so a derived struct meets
                // `T: Serialize` and both names give one table.
                let ret = if self.options.check { ": any" } else { "" };

                match which {
                    "Serialize" => format!(
                        "function {name}.to_table{g}(self{gself}) return {{ {} }} end function {name}.serialize{g}(self{gself}){ret} return {name}.to_table(self) end",
                        to.join(", ")
                    ),

                    _ => {
                        // `@deny_unknown_fields`: a key the struct neither
                        // names nor aliases raises, as serde refuses one.
                        let check = match deny {
                            true => {
                                let keys: Vec<String> = known
                                    .iter()
                                    .map(|k| format!("{} = true", crate::data::luau_key(k)))
                                    .collect();
                                let shown = luau_string(&self.display_name(name));

                                format!(
                                    "for k in pairs(t) do if not ({{ {} }})[k] then error(string.format(\"%s has no field %s\", {shown}, tostring(k)), 2) end end ",
                                    keys.join(", ")
                                )
                            }

                            false => String::new(),
                        };

                        // The return names the struct: inferred, a struct
                        // with a `next: Node?` field read as `Node?`.
                        // A generic struct's `from_table` answers `any`, as
                        // its `default` does: `T` is in the result alone.
                        let value = format!("{}({{ {} }})", self.raw_ctor(name), from.join(", "));

                        match g.is_empty() {
                            true => format!(
                                "function {name}.from_table(t{tn}){gret} {check}return {value} end"
                            ),

                            false => format!(
                                "function {name}.from_table(t{tn}) {check}return {} end",
                                self.any_cast(&value)
                            ),
                        }
                    }
                }
            }

            other => {
                let message = format!("unknown derive `{other}`");
                self.diagnose(at, &message);

                String::new()
            }
        }
    }

    /// serde's options, where they can go wrong: `@rename_all` and
    /// `@deny_unknown_fields` shape the tables the derives write and
    /// read, and an `@alias` key must name no other slot.
    fn check_serde_attrs(&mut self, st: &StructDecl) {
        let derives = |which: &str| {
            st.attributes.iter().any(|a| {
                a.name.is_some_and(|n| self.text_of(n) == "derive")
                    && a.args.iter().any(|x| self.derive_name(x) == which)
            })
        };
        let (ser, de) = (derives("Serialize"), derives("Deserialize"));
        let mut hits: Vec<(TokSpan, String)> = Vec::new();

        for a in &st.attributes {
            let Some(n) = a.name else { continue };

            match self.attr_name(n).as_str() {
                "rename_all" if !ser && !de => hits.push((
                    a.span,
                    "`@rename_all` sets the keys `Serialize` and `Deserialize` use; derive one of them".to_string(),
                )),

                "deny_unknown_fields" if !de => hits.push((
                    a.span,
                    "`@deny_unknown_fields` checks the table `Deserialize` reads; derive Deserialize".to_string(),
                )),

                _ => {}
            }
        }

        let names: Vec<&str> = st.fields.iter().map(|f| self.text_of(f.name)).collect();
        let mut taken: Vec<String> = Vec::new();

        for f in &st.fields {
            for alias in self.attr_strings(&f.attributes, "alias") {
                let at = f
                    .attributes
                    .iter()
                    .find(|a| a.name.is_some_and(|n| self.text_of(n) == "alias"))
                    .map_or(f.name, |a| a.span);

                if names.contains(&alias.as_str()) {
                    hits.push((
                        at,
                        format!("`{alias}` is a field of this struct; an alias names another key"),
                    ));
                } else if taken.contains(&alias) {
                    hits.push((at, format!("two fields take the alias `{alias}`; keep one")));
                }

                taken.push(alias);
            }
        }

        for (at, message) in hits {
            self.diagnose(at, &message);
        }
    }

    /// The string arguments of every `@name(...)` in a list, as the text
    /// each literal stands for.
    pub(crate) fn attr_strings(&self, attrs: &[Attr], name: &str) -> Vec<String> {
        attrs
            .iter()
            .filter(|a| a.name.is_some_and(|n| self.attr_name(n) == name))
            .flat_map(|a| a.args.iter())
            .filter_map(|e| crate::data::literal_text(self.text_of(e.span())))
            .collect()
    }

    /*
    The value a serde half writes for a field of type `ty` read at `x`, or
    `None` to copy it as it is. A struct that derives the half goes
    through its own function, an array maps its items, and a map or a set
    is rebuilt. A table from JSON or a DataStore has no metatable, so
    without this a loaded array had no `push` and a loaded struct no
    methods.
    */
    fn serde_of(&mut self, which: &str, ty: &str, x: &str, depth: usize) -> Option<String> {
        let ty = ty.trim();

        if let Some(inner) = ty.strip_suffix('?') {
            let value = self.serde_of(which, inner, x, depth)?;

            return Some(format!("if {x} == nil then nil else {value}"));
        }

        let derived = match which {
            "Serialize" => self.serializable.contains(ty) || self.star_derives(ty, which),

            _ => self.deserializable.contains(ty) || self.star_derives(ty, which),
        };

        if derived {
            let f = if which == "Serialize" {
                "to_table"
            } else {
                "from_table"
            };

            return Some(format!("{ty}.{f}({x})"));
        }

        // A payload variant is a table under the enum's metatable, which
        // carries the enum's methods; a unit variant is its own string.
        let payload_enum = self
            .enum_decls
            .get(ty)
            .is_some_and(|vs| vs.iter().any(|(_, n)| *n > 0));

        if payload_enum && which == "Deserialize" {
            let text = format!("if type({x}) == \"table\" then setmetatable({x}, {ty}) else {x}");

            return Some(self.any_cast(&format!("({text})")));
        }

        let std = self.std();
        let element = super::types::array_element(ty).or_else(|| {
            ty.strip_prefix("Array<")
                .and_then(|t| t.strip_suffix('>'))
                .map(str::trim)
        });

        if let Some(element) = element {
            let v = format!("_v{depth}");
            let text = match self.serde_of(which, element, &v, depth + 1) {
                Some(item) => format!("{std}.Array.map({x}, function({v}) return {item} end)"),

                // An array of plain values keeps its items; the way back
                // restores the metatable that carries the methods.
                None if which == "Serialize" => return None,

                None => format!("{std}.Array.from({x})"),
            };

            return Some(self.any_cast(&text));
        }

        // A map's values go through their own half, one entry at a time:
        // `HashMap<string, Item>` and `{ [string]: Item }`.
        let head = ty.split('<').next().unwrap_or(ty).trim();
        let value_ty = match head {
            "HashMap" => super::types::split_generics(&ty[head.len()..])
                .get(1)
                .cloned(),

            _ if ty.starts_with("{ [") || ty.starts_with("{[") => ty
                .trim_start_matches('{')
                .trim_end_matches('}')
                .split_once("]:")
                .map(|(_, v)| v.trim().to_string()),

            _ => None,
        };
        let each = value_ty.and_then(|v| {
            let v_var = format!("_e{depth}");

            self.serde_of(which, &v, &v_var, depth + 1)
                .map(|item| format!("function({v_var}) return {item} end"))
        });

        if let Some(f) = each {
            let text = match (head, which) {
                ("HashMap", "Serialize") => format!("{std}.map_values({x}:to_table(), {f})"),

                ("HashMap", _) => format!("{std}.HashMap.from({std}.map_values({x}, {f}))"),

                _ => format!("{std}.map_values({x}, {f})"),
            };

            return Some(self.any_cast(&text));
        }

        let text = match (head, which) {
            ("HashMap", "Serialize") => format!("{x}:to_table()"),

            ("HashMap", _) => format!("{std}.HashMap.from({x})"),

            ("Set", "Serialize") => format!("{x}:to_array()"),

            ("Set", _) => format!("{std}.Set.from({x})"),

            _ => return None,
        };

        Some(self.any_cast(&text))
    }

    /// Whether `ty` names a struct through a star import, `I.Item`, and
    /// that struct derives `which` in the file that declares it.
    fn star_derives(&self, ty: &str, which: &str) -> bool {
        ty.contains('.')
            && self
                .imported_type(ty)
                .is_some_and(|s| s.derives.iter().any(|d| d == which))
    }

    /// The copy `Clone` writes for a field of type `ty` read at `field`,
    /// `None` for a value the clone shares.
    fn clone_of(&mut self, ty: &str, field: &str) -> Option<String> {
        let element = super::types::array_element(ty).or_else(|| {
            ty.strip_prefix("Array<")
                .and_then(|t| t.strip_suffix('>'))
                .map(str::trim)
        });

        if self.cloneable.contains(ty) || self.star_derives(ty, "Clone") {
            return Some(format!("{ty}.clone({field})"));
        }

        if let Some(element) = element {
            let std = self.std();

            // The checker infers `unknown[]` for a copy; the field's own
            // type is the one to keep.
            let element = element.trim();
            let copy = match self.cloneable.contains(element) || self.star_derives(element, "Clone")
            {
                true => format!("{std}.Array.map({field}, {}.clone)", element.trim()),

                false => format!("{std}.Array.from(table.clone({field}))"),
            };

            return Some(self.any_cast(&copy));
        }

        // A map and a set are new collections of the same entries, as
        // Rust's derive clones a HashMap; a shared one leaked every write.
        let std = self.std();

        match ty.split('<').next().unwrap_or(ty).trim() {
            "HashMap" => {
                return Some(self.any_cast(&format!(
                    "{std}.HashMap.from(table.clone({field}:to_table()))"
                )));
            }

            "Set" => return Some(self.any_cast(&format!("{std}.Set.from({field}:to_array())"))),

            _ => {}
        }

        // A table type, `{ T }` or `{ [K]: V }`, copies its entries.
        (ty.starts_with('{') && ty.ends_with('}')).then(|| format!("table.clone({field})"))
    }

    /// The value `Default` starts a field of type `ty` at: `Some(None)`
    /// for nil, `None` when the type has no default.
    fn zero_of(&mut self, ty: &str) -> Option<Option<String>> {
        let ty = ty.trim();

        if ty.ends_with('?') || matches!(ty, "any" | "unknown" | "nil") {
            return Some(None);
        }

        let value = match ty {
            "number" => "0".to_string(),

            "string" => "\"\"".to_string(),

            "boolean" => "false".to_string(),

            "Vector3" | "Vector2" => format!("{ty}.zero"),

            "CFrame" => "CFrame.identity".to_string(),

            "UDim2" | "UDim" | "Color3" => format!("{ty}.new()"),

            // An empty container infers `unknown` elements; the field's
            // type is the one to keep.
            _ if ty.ends_with("[]") || ty.starts_with("Array<") => {
                let std = self.std();

                self.any_cast(&format!("{std}.Array.from({{}})"))
            }

            _ if ty.starts_with("HashMap<") || ty.starts_with("Set<") => {
                let std = self.std();
                let head = ty.split('<').next().unwrap_or(ty);

                self.any_cast(&format!("{std}.{head}.new()"))
            }

            // A list or a map; a record has fields no empty table holds.
            _ if ty.starts_with('{') && (!ty.contains(':') || ty.contains("]:")) => {
                "{}".to_string()
            }

            _ if self.defaultable.contains(ty) || self.star_derives(ty, "Default") => {
                format!("{ty}.default()")
            }

            _ => return None,
        };

        Some(Some(value))
    }

    // --- traits --------------------------------------------------------------

    /// A trait is a type of its method signatures plus a table holding the
    /// default bodies, which `impl` copies onto the struct.
    pub(crate) fn trait_decl(&mut self, t: &TraitDecl) {
        let name = self.decl_name(t.name);
        let start = self.byte_start(t.span);
        let end_tok = self.toks[t.span.end as usize - 1];
        let export = if t.exported || self.ns_export || self.export_listed_types.contains(&name) {
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
            self.check_param_names(&m.params);

            for p in &m.params {
                if let Some(d) = &p.destructure {
                    self.check_param_pattern(p.name, p.ty, p.default.is_some(), d);
                }
            }

            let ms = self.byte_start(m.span);
            self.blank_lines(cursor, ms);

            match &m.body {
                Some(body) => {
                    // `function name(params): R` becomes `function Trait.name(params): R`.
                    let mname = self.text_of(m.name).to_string();
                    let (mut sig, prologue) = self.signature_text(m);

                    // An untyped `self` is the trait's own interface in
                    // the check artifact. The default body then reads
                    // the trait's methods and nothing else, so a typo
                    // or a private field of an implementing struct
                    // reports here, in the trait's own file.
                    if self.options.check {
                        if sig.starts_with("(self)") {
                            sig = sig.replacen("(self)", &format!("(self: {name})"), 1);
                        } else if sig.starts_with("(self,") {
                            sig = sig.replacen("(self,", &format!("(self: {name},"), 1);
                        }
                    }

                    self.generate(ms, &format!("function {name}.{mname}{sig}"));
                    self.write_pieces(self.byte_end(m.signature), &prologue);
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

    /// A default method's signature as Luau: the return arrow is `:`,
    /// the types copy through the type edits, and each pattern is a
    /// temp. The prologue opens the patterns after the signature.
    pub(crate) fn signature_text(&mut self, m: &TraitMethod) -> (String, Vec<Vec<Piece>>) {
        let sig = m.signature;
        let mut side = Renderer::new(self.src);
        std::mem::swap(&mut self.r, &mut side);
        let mut cursor = self.byte_start(sig);
        let mut prologue = Vec::new();
        let mut n = 0;

        for p in &m.params {
            let Some(d) = &p.destructure else {
                continue;
            };
            let (ps, pe) = (self.byte_start(p.name), self.byte_end(p.name));
            self.copy(cursor, ps);
            let temp = self.pattern_temp(&mut n);
            self.generate(ps, &temp);
            self.blank_lines(ps, pe);

            if p.ty.is_none() {
                let shape = self.pattern_shape(d).unwrap_or("any".to_string());
                self.generate(pe, &format!(": {shape}"));
            }

            let rest_type =
                p.ty.map(|t| self.copy_type_to_string(t).trim().to_string())
                    .filter(|t| is_string_index_table(t));
            prologue.push(self.destructure_pieces(d, &temp, rest_type.as_deref()));
            cursor = pe;
        }

        // The return arrow follows the `)` that closes the parameters;
        // an arrow inside a parameter's function type stays.
        let mut depth = 0;
        let close = (sig.start..sig.end).find(|&i| {
            match self.toks[i as usize].text(self.src) {
                "(" => depth += 1,

                ")" => depth -= 1,

                _ => {}
            }

            depth == 0
        });

        if let Some(close) = close
            && close + 1 < sig.end
            && self.toks[close as usize + 1].text(self.src) == "->"
        {
            let arrow = self.toks[close as usize + 1];
            self.copy(cursor, arrow.start);
            self.generate(arrow.start, ":");
            cursor = arrow.end;
        }

        self.copy(cursor, self.byte_end(sig));
        std::mem::swap(&mut self.r, &mut side);

        (side.finish().0, prologue)
    }

    pub(crate) fn trait_method_type(&mut self, m: &TraitMethod) -> String {
        let mname = self.text_of(m.name).to_string();
        let mut params = Vec::new();

        for p in &m.params {
            let pname = self.text_of(p.name).to_string();

            // The caller passes one value for a pattern, so the type
            // names no parameter.
            if let Some(d) = &p.destructure {
                let ty = match p.ty {
                    Some(t) => self.copy_type_to_string(t).trim().to_string(),

                    None => self.pattern_shape(d).unwrap_or("any".to_string()),
                };
                params.push(ty);
            } else if pname == "self" {
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
        // The signature holds source text: an Alloy spelling such as
        // `T[]` and an ambient std name both need the Luau form, the
        // same as a parameter type does.
        let ret = match ret.as_str() {
            "()" => ret,

            other => self.lower_type(other),
        };
        // `async function f(self): T` answers with `Future<T>`, as
        // `alloy doc async` says for a function. A signature with no
        // return type answers with `Future<nil>`.
        let ret = match m.is_async.is_some() {
            true => {
                let inner = match ret.as_str() {
                    "()" => "nil",

                    other => other,
                };

                format!("{}.Future<{inner}>", self.std())
            }

            false => ret,
        };

        format!("{mname}: ({}) -> {ret}", params.join(", "))
    }

    // --- interfaces ------------------------------------------------------------

    pub(crate) fn interface_decl(&mut self, i: &InterfaceDecl) {
        let name = self.decl_name(i.name);
        let start = self.byte_start(i.span);
        let end_tok = self.toks[i.span.end as usize - 1];
        let export = if i.exported || self.ns_export || self.export_listed_types.contains(&name) {
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
        if let Some((at, resolved, raw)) = self.called_struct(e) {
            // The report names the struct the way the source writes it:
            // an import alias, or a namespace path of any depth.
            let name = self.text_of(at).to_string();
            let ctor = self.struct_ctor(&resolved);
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
                start: self.byte_start(at),
                end: self.byte_end(at),
                message,
            });
        }
    }

    /// The struct a construction without `new` names: the span the
    /// report lands on, the name every struct index is keyed by, and
    /// whether the source wrote the fields form, `Name { ... }`.
    ///
    /// The written name may be a bare name, an import alias, or a
    /// namespace path of any depth, so the whole path in front of the
    /// call folds into one span and resolves the way `new Ns.T { }`
    /// does. A name that no struct declares gives `None`, and a call of
    /// an ordinary function with a table stays a call.
    pub(crate) fn called_struct(&self, e: &Expr) -> Option<(TokSpan, String, bool)> {
        let (base, links) = flatten(e);
        let Expr::Name(n) = base else {
            return None;
        };
        let mut span = *n;
        let mut rest = links.as_slice();

        // The links read in source order, so every field step in front
        // of the call is part of the path the source wrote.
        while let [Link::Plain(Step::Field(f)), tail @ ..] = rest {
            span = TokSpan::new(span.start as usize, f.end as usize);
            rest = tail;
        }

        let Some(Link::Plain(Step::Call {
            method: None, args, ..
        })) = rest.first()
        else {
            return None;
        };
        let raw = matches!(args, CallArgs::Table(_));
        let (at, name) = self.constructed_struct(&Expr::Name(span))?;

        self.declared_fields(&name)
            .is_some()
            .then_some((at, name, raw))
    }

    /// `new Name { ... }` with no parentheses on a struct: the table is
    /// the fields, and the class call takes it.
    pub(crate) fn fields_form(
        &self,
        name: &Expr,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
    ) -> bool {
        args.is_none()
            && init.is_some()
            && self
                .constructed_struct(name)
                .is_some_and(|(_, n)| self.structs.contains(&n))
    }

    /// `new Name()` where the constructor itself writes it: the raw
    /// construct with no fields, the same text `new Name { }` gives.
    pub(crate) fn empty_construct(&self, name: &str) -> String {
        let ctor = self.raw_ctor(name);
        // Inside the struct's own impl the instance carries the full
        // view, so `self.count` in `new` type checks.
        if self.impl_target.as_deref() == Some(name) && self.has_private_view(name) {
            format!("(({ctor}({{}}) :: any) :: {name}__all)")
        } else {
            format!("{ctor}({{}})")
        }
    }

    /// `new Self()` inside the constructor that `new Self(...)` calls.
    /// The call would be the constructor calling itself, which never
    /// returns, so the emit builds the value the way `new Self { }`
    /// does. Only the empty argument list counts: `new Self(a)` in the
    /// same body may be a recursion the source means.
    pub(crate) fn self_construct(
        &self,
        name: &Expr,
        args: Option<&CallArgs>,
        init: Option<&Expr>,
    ) -> bool {
        let Expr::Name(n) = name else {
            return false;
        };
        let text = self.text_of(*n);

        if init.is_some() || self.impl_target.as_deref() != Some(text) {
            return false;
        }

        if self.structs_with_new.get(text) != self.impl_method.as_ref() {
            return false;
        }

        match args {
            None => true,

            Some(CallArgs::Paren(list)) => list.is_empty(),

            Some(_) => false,
        }
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
            .map(|f| {
                (
                    self.text_of(f.name).to_string(),
                    crate::declarations::field_can_stay_unset(
                        self.text_of(f.ty),
                        f.default.is_some(),
                    ),
                )
            })
            .collect();
        self.structs.insert(name.clone());

        if let Some(g) = st.generics {
            self.struct_generics
                .insert(name.clone(), self.text_of(g).trim().to_string());
        }

        self.note_field_types(&name, &st.fields);
        // A `@skip` field stays off the wire, as it stays out of the
        // derived table; the reader's constructor fills its default.
        let wire_fields = st
            .fields
            .iter()
            .filter(|f| {
                !f.attributes
                    .iter()
                    .any(|a| a.name.is_some_and(|n| self.attr_name(n) == "skip"))
            })
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
        self.struct_at
            .insert(name.clone(), self.byte_start(st.span));

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

        let head = self.generic_annotation(self.text_of(l.names[0].ty?))?;

        self.is_constructor_call(&l.values[0], &head.0)
            .then_some(head)
    }

    /// The base and arguments of an annotation the solver cannot pass to
    /// a constructor on its own: a std container, or a generic struct or
    /// enum of this file, `Stack<number>`.
    fn generic_annotation(&self, ty: &str) -> Option<(String, String)> {
        if let Some(head) = generic_head(ty) {
            return Some(head);
        }

        let ty = ty.trim();
        let (base, args) = ty.strip_suffix('>')?.split_once('<')?;
        let base = base.trim();

        // `import { HashMap as Map }`: the alias names the std type, and
        // `Map.new()` takes the arguments `Map<K, V>` writes.
        if let Some(original) = self.import_renames.get(base)
            && generic_head(&format!("{original}<{args}>")).is_some()
        {
            return Some((base.to_string(), args.trim().to_string()));
        }

        self.generic_types
            .contains(base)
            .then(|| (base.to_string(), args.trim().to_string()))
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
        let head = self.generic_annotation(&ty)?;

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
                let ctor =
                    |f: &TokSpan| matches!(self.text_of(*f), "new" | "from" | "with_capacity");
                // The type the annotation names, without a module path.
                let bare = base_name.rsplit('.').next().unwrap_or(base_name);

                match (base, links.as_slice()) {
                    (
                        Expr::Name(n),
                        [
                            Link::Plain(Step::Field(f)),
                            Link::Plain(Step::Call {
                                method: None,
                                type_args: None,
                                ..
                            }),
                        ],
                    ) => self.text_of(*n) == base_name && ctor(f),

                    // `c.HashMap.new()` through `import * as c`.
                    (
                        Expr::Name(n),
                        [
                            Link::Plain(Step::Field(m)),
                            Link::Plain(Step::Field(f)),
                            Link::Plain(Step::Call {
                                method: None,
                                type_args: None,
                                ..
                            }),
                        ],
                    ) => {
                        self.star_modules.contains(self.text_of(*n))
                            && self.text_of(*m) == bare
                            && ctor(f)
                    }

                    _ => false,
                }
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
    /// Whether this file aliased the full view of an imported struct,
    /// `Name__all`. Only the check artifact carries the view.
    pub(crate) fn reads_private_view(&self, name: &str) -> bool {
        self.options.check && self.private_view_names.contains(name)
    }

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
    ///
    /// An alias narrows the way the name it spells does: `is` reads
    /// through an alias, so the branch it opens has to agree.
    pub(crate) fn narrow_type(&self, name: &str, value: &str) -> Option<String> {
        let alias = self.alias_head(name);
        let name = alias.as_deref().unwrap_or(name);

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

    /// The struct a `new` names, with the span a report lands on. The
    /// name the parser keeps is what the source wrote, which may be an
    /// import alias or a dotted path; every index of a struct is keyed
    /// by the name the declaring file gives it, so the checks read one
    /// name however the source spells it.
    /// Every name a written type name can stand for: the name itself,
    /// the declaration an import alias renames, and the name a dotted
    /// path resolves to, through a star module or a namespace.
    ///
    /// A check keyed by the declared name reads one of these, so the
    /// source may spell the type any way it likes.
    pub(crate) fn name_candidates(&self, text: &str) -> Vec<String> {
        let mut out = vec![text.to_string()];

        let Some((head, rest)) = text.split_once('.') else {
            if let Some(declared) = self.import_renames.get(text) {
                out.push(declared.clone());
            }

            return out;
        };

        if self.star_modules.contains(head.trim()) {
            out.push(rest.trim().to_string());
        }

        if let Some(r) = self.ns_path_name(text) {
            out.push(r);
        }

        // An imported namespace has no declaration here, so the emitted
        // form is the name the module's shape carries.
        out.push(text.replace('.', "_"));
        out.dedup();

        out
    }

    pub(crate) fn constructed_struct(&self, name: &Expr) -> Option<(TokSpan, String)> {
        let Expr::Name(n) = name else {
            return None;
        };
        let text = self.text_of(*n).to_string();

        // `new B { }` inside the namespace that declares `B`: the member
        // renders under one name, `NS_B`, and every struct index is keyed
        // by that one. The bare name has to resolve the way the emit
        // renames it, else the fields form reads a user `new` instead.
        if let Some(rendered) = self.ns_member_name(&text) {
            return Some((*n, rendered));
        }

        if self.struct_fields.contains_key(&text) {
            return Some((*n, text));
        }

        // `new M.Box` through `import * as M`: the module declares
        // `Box`. `new Zoo.Lion` on a namespace member: the emit renders
        // it as `Zoo_Lion`, nesting and all.
        if let Some((head, rest)) = text.split_once('.') {
            // `import { M as Mod }`: the module declares the namespace
            // under its own name, and every import index is keyed by
            // that one, so the head folds back before the lookup.
            let head = self
                .import_renames
                .get(head.trim())
                .map_or(head.trim(), String::as_str);
            let dotted = format!("{head}.{}", rest.trim());

            // `import * as B`: the index keys the module's struct under
            // `B.T`, so two modules' `T` stay apart. The bare name is
            // the fallback for an index that holds only it.
            let path = match self.star_modules.contains(head)
                && self.imported_shape_name(&dotted).is_none()
            {
                true => rest.trim().to_string(),

                false => dotted,
            };

            // A namespace this file declares renders under one name.
            // An imported one has no declaration here, so the path is
            // the name its shape carries; `struct_privates` and
            // `struct_field_defaults` list a member under both.
            let resolved = match self.ns_path_name(&path) {
                Some(r) => r,

                None => self.imported_shape_name(&path)?,
            };

            return Some((*n, resolved));
        }

        // `import { Box as B }`: the module declares `Box`, and the
        // import index carries the local name too, so two modules'
        // `Point` stay apart. The name the source wrote answers first;
        // the declared name is the fallback for an index that holds
        // only it.
        match self.import_renames.get(&text) {
            Some(declared) if self.declared_fields(&text).is_none() => Some((*n, declared.clone())),

            _ => Some((*n, text)),
        }
    }

    /// The fields form names every field without a default and no field
    /// the struct lacks. A spread or a computed key turns the check off,
    /// because the table's keys are then not in the source.
    pub(crate) fn check_struct_fields(&mut self, name: &Expr, table: &Expr) {
        let Some((n, sname)) = self.constructed_struct(name) else {
            return;
        };
        let Some(declared) = self.declared_fields(&sname) else {
            return;
        };
        // The message quotes the path the source wrote. The index key
        // is the declared name, `M.Ns_T` through a star alias, and
        // `display_name` only unfolds a namespace this file declares.
        let shown = self.text_of(n).to_string();
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
                                "`{shown}` has no field `{fname}`; its fields are {}",
                                list_names(&known)
                            ),
                        );
                    } else if self.sets_imported_private(&sname, &fname) {
                        self.lints.push(crate::lint::Lint {
                            name: "private_access",
                            start: self.byte_start(*name),
                            end: self.byte_end(*name),
                            message: format!(
                                "`{fname}` is private to `{shown}`; only its impl sets it"
                            ),
                            fix: None,
                        });
                    }

                    given.push(fname);
                }

                // Luau reads a value with no name as an array item, and
                // its report says nothing of the struct. Alloy has no
                // Rust shorthand, so the report names the full form.
                TableField::Positional(value) => {
                    let shape = match value {
                        Expr::Name(v) => {
                            let v = self.text_of(*v);

                            format!("{v} = {v}")
                        }

                        _ => "field = value".to_string(),
                    };
                    self.diagnose(
                        value.span(),
                        &format!("a struct takes each field by name: write `{shape}`"),
                    );
                    open = true;
                }

                _ => open = true,
            }
        }

        if open {
            return;
        }

        self.check_missing_fields(n, &sname, &given, "fields");
    }

    /// A private field of an imported struct, named in a `new` outside
    /// the struct's impl. A private field without a default has to be
    /// set, so that one stays. `private_access` reads a struct this file
    /// declares from its own tokens; an imported one has none here, so
    /// the import index answers instead.
    fn sets_imported_private(&self, sname: &str, fname: &str) -> bool {
        if self.struct_fields.contains_key(sname) || self.impl_target.as_deref() == Some(sname) {
            return false;
        }

        let private = self
            .options
            .import_privates
            .iter()
            .any(|(s, fields)| s == sname && fields.iter().any(|f| f == fname));

        private
            && self
                .declared_fields(sname)
                .is_some_and(|fields| fields.iter().any(|(d, default)| d == fname && *default))
    }

    /// A dotted path a module this file imports declares a struct for,
    /// `Zoo.Box` through `import { Zoo }`. The shape lists a namespace
    /// member under its path, so the name the source writes answers.
    fn imported_shape_name(&self, path: &str) -> Option<String> {
        let known = self
            .options
            .import_struct_fields
            .iter()
            .any(|(s, _)| s == path)
            || self.options.import_privates.iter().any(|(s, _)| s == path);

        known.then(|| path.to_string())
    }

    /// The constructor a struct writes: the `new` or `New` of this
    /// file's own `impl`, else the one a module it imports writes.
    /// `None` for a struct that writes none.
    fn struct_ctor(&self, name: &str) -> Option<String> {
        self.structs_with_new.get(name).cloned().or_else(|| {
            self.options
                .import_struct_ctors
                .iter()
                .find(|(s, _)| s == name)
                .map(|(_, ctor)| ctor.clone())
        })
    }

    /// The fields of a struct with whether each carries a default: this
    /// file's own declaration, else the shape a module it imports
    /// declares.
    pub(crate) fn declared_fields(&self, name: &str) -> Option<Vec<(String, bool)>> {
        self.struct_fields.get(name).cloned().or_else(|| {
            self.options
                .import_struct_fields
                .iter()
                .find(|(s, _)| s == name)
                .map(|(_, fields)| fields.clone())
        })
    }

    /// Reports the fields a construction leaves unset. `form` says what
    /// the source wrote, so the message quotes it back.
    fn check_missing_fields(&mut self, n: TokSpan, sname: &str, given: &[String], form: &str) {
        let Some(declared) = self.declared_fields(sname) else {
            return;
        };
        let missing: Vec<&str> = declared
            .iter()
            .filter(|(d, has_default)| !has_default && !given.contains(d))
            .map(|(d, _)| d.as_str())
            .collect();

        if missing.is_empty() {
            return;
        }
        let shown = self.text_of(n).to_string();
        let written = match form {
            "paren" => format!("new {shown}()"),

            _ => format!("new {shown} {{ ... }}"),
        };
        self.diagnose(
            n,
            &format!(
                "`{written}` leaves {} unset; a field without a default needs a value",
                list_names(&missing)
            ),
        );
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
            // A struct another module declares. The imported shape
            // carries its fields, so the fields form reads the same
            // checks; every check below reads this file's own
            // declaration, so the imported case ends here.
            if args.is_none()
                && let Some(table @ Expr::Table { .. }) = init
            {
                self.check_struct_fields(name, table);
            }

            return;
        }

        let ctor = self.structs_with_new.get(&text).cloned();

        let message = if self.fields_form(name, args, init) {
            match ctor {
                // The fields form builds the value outright, so it is
                // the one way past a constructor. Naming what the call
                // skips reads better than naming the rule, and the two
                // ways out cover both reasons to write fields here: to
                // call `new`, and to hand it the fields to work on.
                Some(ctor) if self.impl_target.as_deref() != Some(text.as_str()) => format!(
                    "`new {text} {{ ... }}` skips `{text}.{ctor}`; write `new {text}()` to call it, or `new {text}({{ ... }})` to give it the fields"
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
                Some(_) => {
                    // The constructor building its own value names no
                    // field, so every field without a default is unset.
                    if self.self_construct(name, args, init) {
                        self.check_missing_fields(*n, &text, &[], "paren");
                    }

                    return;
                }

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

/// A field name under a serde case: `max_hp` becomes `maxHp` under
/// `camelCase`. `None` for a style serde does not name.
pub(crate) fn rename_case(name: &str, style: &str) -> Option<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();

    for c in name.chars() {
        if c == '_' || c == '-' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }

            continue;
        }

        let boundary = c.is_uppercase()
            && word
                .chars()
                .last()
                .is_some_and(|p| p.is_lowercase() || p.is_ascii_digit());

        if boundary {
            words.push(std::mem::take(&mut word));
        }

        word.extend(c.to_lowercase());
    }

    if !word.is_empty() {
        words.push(word);
    }

    let capital = |w: &String| {
        let mut c = w.chars();

        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };

    Some(match style {
        // serde's two case styles change the case alone: `max_hp` stays
        // `max_hp`, and UPPERCASE writes `MAX_HP`.
        "lowercase" => name.to_lowercase(),

        "UPPERCASE" => name.to_uppercase(),

        "PascalCase" => words.iter().map(capital).collect(),

        "camelCase" => words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.clone() } else { capital(w) })
            .collect(),

        "snake_case" => words.join("_"),

        "SCREAMING_SNAKE_CASE" => words.join("_").to_uppercase(),

        "kebab-case" => words.join("-"),

        "SCREAMING-KEBAB-CASE" => words.join("-").to_uppercase(),

        _ => return None,
    })
}

/// The styles `@rename_all` takes, serde's names for them.
pub const RENAME_STYLES: &[&str] = &[
    "lowercase",
    "UPPERCASE",
    "PascalCase",
    "camelCase",
    "snake_case",
    "SCREAMING_SNAKE_CASE",
    "kebab-case",
    "SCREAMING-KEBAB-CASE",
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

    /// `@test` registers a free function with the runner, which calls
    /// it by name; a method takes a receiver. Luau has no `@test`, so a
    /// copy of the attribute breaks the artifact it lands in.
    #[test]
    fn a_test_attribute_on_a_method_reports_and_leaves_the_emit() {
        let src = "struct Vec2 as\n    x: number\nend\n\nimpl Vec2 as\n    @test\n    function methodTest(self): number\n        return 1\n    end\nend\n";
        assert_eq!(
            messages(src),
            vec!["`@test` goes on a function, not a method"]
        );

        let out = crate::compile(src).unwrap();

        assert!(!out.ship.contains("@test"), "{}", out.ship);
        assert!(!out.check.contains("@test"), "{}", out.check);
    }

    /// A static method of a generic impl takes the impl's parameters
    /// only when it names one: `of<U>` alone binds `U` to an explicit
    /// `of<<number>>`, and `make(): Box<T>` still needs its `T`.
    #[test]
    fn a_static_method_carries_the_impl_generics_it_names() {
        let src = "struct Box<T> as\n    value: T\nend\nimpl Box<T> as\n    function of<U>(v: U): Box<U>\n        return new Box<<U>> { value = v }\n    end\n    function make(): Box<T>\n        return new Box<<T>> { value = nil :: any }\n    end\n    function get(self): T\n        return self.value\n    end\nend\nprint(Box)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        for header in [
            "function Box.of<U>(v: U): Box<U>",
            "function Box.make<T>(): Box<T>",
            "function Box.get<T>(self: Box<T>): T",
        ] {
            assert!(out.check.contains(header), "{header}: {}", out.check);
        }
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

    /// A default body's `self` is the trait's own interface in the
    /// check artifact. A field of an implementing struct is not on that
    /// record, so a typo, or a private field, reports in the trait's
    /// file. A call of another trait method still resolves.
    #[test]
    fn a_trait_default_types_self_as_the_trait() {
        let src = "trait Shape as\n    function area(self): number\n    function label(self): string\n        return `area {self:area()}`\n    end\nend\n";
        let out = crate::compile(src).unwrap();

        assert!(
            out.check
                .contains("function Shape.label(self: Shape): string"),
            "{}",
            out.check
        );

        // A second parameter keeps its place.
        let two = "trait Tagged as\n    function tag(self, n: number): string\n        return tostring(n)\n    end\nend\n";
        let out = crate::compile(two).unwrap();

        assert!(
            out.check
                .contains("function Tagged.tag(self: Tagged, n: number): string"),
            "{}",
            out.check
        );
    }

    /// `async function` is a trait signature too. The body reader used
    /// to stop at the `async`, and the trait then wanted an `end`. The
    /// signature answers with `Future<T>`, so an `async` impl satisfies
    /// it and a plain one breaks the contract.
    #[test]
    fn a_trait_declares_an_async_signature() {
        let decl = "trait Fetcher as\n    async function fetch(self): string\nend\n";

        assert!(messages(decl).is_empty(), "{:?}", messages(decl));

        let out = crate::compile(decl).unwrap();

        assert!(
            out.check
                .contains("type Fetcher = { read fetch: (self: any) -> __alloy.Future<string> }"),
            "{}",
            out.check
        );

        let body = "\nstruct Remote as\n    url: string,\nend\n\nimpl Fetcher for Remote as\n    {}function fetch(self): string\n        return self.url\n    end\nend\n\nprint(new Remote { url = \"a\" })\n";
        let good = format!("{decl}{}", body.replace("{}", "async "));

        assert!(messages(&good).is_empty(), "{:?}", messages(&good));

        let bad = format!("{decl}{}", body.replace("{}", ""));

        assert_eq!(
            messages(&bad),
            vec!["the trait method `fetch` returns Future<string> in `Fetcher`, string here"]
        );
    }

    /// The contract keys by the declared name. An import alias and a
    /// namespace path write another name for the same trait, so the
    /// written name alone found no contract and nothing reported.
    #[test]
    fn the_trait_contract_resolves_an_alias_and_a_namespace_path() {
        let sig = |name: &str| (name.to_string(), 1, Some("string".to_string()));
        let options = crate::EmitOptions {
            import_trait_methods: vec![
                ("Greet".to_string(), vec![sig("hello"), sig("bye")]),
                ("Ns.Greet".to_string(), vec![sig("hi")]),
            ],
            ..Default::default()
        };
        let run = |src: &str| -> Vec<String> {
            crate::compile_with(src, &options)
                .unwrap()
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect()
        };

        // `import { Greet as G }`, then `impl G for B`.
        let alias = "import { Greet as G } from \"./greet\"\nstruct B as\n    n: number\nend\n\nimpl G for B as\n    function hello(self): string\n        return \"hi\"\n    end\nend\n";
        assert_eq!(
            run(alias),
            vec!["`impl G for B` does not write `bye`; the trait declares it"]
        );

        // `impl Ns.Greet for C` through an imported namespace.
        let path = "import { Ns } from \"./greet\"\nstruct C as\n    n: number\nend\n\nimpl Ns.Greet for C as\nend\n";
        assert_eq!(
            run(path),
            vec!["`impl Ns.Greet for C` does not write `hi`; the trait declares it"]
        );

        // A namespace of this file declares the trait.
        let same = "namespace Local as\n    trait Greet as\n        function hello(self): string\n        function bye(self): string\n    end\nend\n\nstruct Z as\n    n: number\nend\n\nimpl Local.Greet for Z as\n    function hello(self): string\n        return \"hi\"\n    end\nend\n";
        assert_eq!(
            run(same),
            vec!["`impl Local.Greet for Z` does not write `bye`; the trait declares it"]
        );

        // A trait a namespace exports reads under its path, so the
        // index the importing file builds carries that name.
        let module = "export namespace Ns as\n    public trait Greet as\n        function hi(self): string\n    end\nend\n";
        let names: Vec<String> = crate::modules::exported_trait_methods(module)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["Ns.Greet", "Ns_Greet"], "{names:?}");
    }

    /// A trait is a contract wherever it is declared. The imported
    /// list carries the methods with no body, so an impl that skips one
    /// reports at the impl header, and a default method stays optional.
    #[test]
    fn an_impl_of_an_imported_trait_writes_every_method_the_trait_declares() {
        let options = crate::EmitOptions {
            import_trait_methods: vec![(
                "Greet".to_string(),
                vec![("hello".to_string(), 1, Some("string".to_string()))],
            )],
            import_trait_defaults: vec![("Greet".to_string(), vec!["wave".to_string()])],
            ..Default::default()
        };
        let run = |src: &str| -> Vec<String> {
            crate::compile_with(src, &options)
                .unwrap()
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect()
        };
        let head = "import { Greet } from \"./greet\"\nstruct Beta as\n    n: number\nend\n";

        assert_eq!(
            run(&format!("{head}impl Greet for Beta as\nend\nprint(Beta)\n")),
            vec!["`impl Greet for Beta` does not write `hello`; the trait declares it"]
        );

        // The method written, and the default left out: both are right.
        let written = run(&format!(
            "{head}impl Greet for Beta as\n    function hello(self): string\n        return \"b\"\n    end\nend\nprint(Beta)\n"
        ));
        assert!(written.is_empty(), "{written:?}");

        // The arity is part of the contract, and Alloy reports it: the
        // checker sees two unrelated functions.
        assert_eq!(
            run(&format!(
                "{head}impl Greet for Beta as\n    function hello(self, extra: number): string\n        return \"b\"\n    end\nend\nprint(Beta)\n"
            )),
            vec!["the trait method `hello` takes 1 parameter in `Greet`, 2 here"]
        );
    }

    /// A struct another module declares is still built whole: the
    /// imported shape says which fields carry a default, so the fields
    /// form names the ones it leaves unset.
    #[test]
    fn a_construction_of_an_imported_struct_names_its_unset_fields() {
        let options = crate::EmitOptions {
            import_struct_fields: vec![(
                "Box".to_string(),
                vec![("w".to_string(), true), ("label".to_string(), false)],
            )],
            ..Default::default()
        };
        let messages = |src: &str| -> Vec<String> {
            crate::compile_with(src, &options)
                .unwrap()
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect()
        };

        assert_eq!(
            messages("import { Box } from \"./box\"\n\nprint(new Box { })\n"),
            vec!["`new Box { ... }` leaves `label` unset; a field without a default needs a value"]
        );

        let whole = messages("import { Box } from \"./box\"\n\nprint(new Box { label = \"a\" })\n");
        assert!(whole.is_empty(), "{whole:?}");
    }

    #[test]
    fn an_optional_field_can_stay_unset() {
        let src = "struct Health as\n    current: number\n    last_hit: number?\n    note: nil | string\nend\n\nlocal h = new Health { current = 1 }\n";
        let messages: Vec<String> = crate::compile_with(src, &Default::default())
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect();

        assert_eq!(messages, Vec::<String>::new());
    }

    /// The fields form on an imported struct reads every check the
    /// struct's own file gets: a field the struct lacks, and a private
    /// field with a default that only its impl sets.
    #[test]
    fn a_construction_of_an_imported_struct_names_an_unknown_and_a_private_field() {
        let options = crate::EmitOptions {
            import_struct_fields: vec![(
                "Box".to_string(),
                vec![("id".to_string(), false), ("secret".to_string(), true)],
            )],
            import_privates: vec![("Box".to_string(), vec!["secret".to_string()])],
            ..Default::default()
        };
        let out = crate::compile_with(
            "import { Box } from \"./box\"\n\nprint(new Box { id = 1, gone = 2 })\n",
            &options,
        )
        .unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            messages,
            vec!["`Box` has no field `gone`; its fields are `id` and `secret`"]
        );

        let private = crate::compile_with(
            "import { Box } from \"./box\"\n\nprint(new Box { id = 1, secret = 5 })\n",
            &options,
        )
        .unwrap();
        let private_lints: Vec<&str> = private
            .lints
            .iter()
            .map(|l| l.name)
            .filter(|n| *n == "private_access")
            .collect();

        assert!(private.diagnostics.is_empty(), "{:?}", private.diagnostics);
        assert_eq!(private_lints, vec!["private_access"]);
    }

    /// The fields form builds the value outright, so the check artifact
    /// calls `__new`, the typed raw constructor. A `new` the struct's
    /// impl writes takes the parameters it declares, and the call is not
    /// that one.
    #[test]
    fn the_fields_form_on_an_imported_struct_calls_the_raw_constructor() {
        let options = crate::EmitOptions {
            check: true,
            import_struct_fields: vec![("Box".to_string(), vec![("label".to_string(), false)])],
            ..Default::default()
        };
        let out = crate::compile_with(
            "import { Box } from \"./box\"\n\nprint(new Box { label = \"a\" })\n",
            &options,
        )
        .unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("Box.__new({ label = \"a\" })"),
            "{}",
            out.check
        );
        // A name the file knows nothing about keeps `new`: it has no
        // `__new` of Alloy's making.
        let foreign = crate::compile_with(
            "local Part = require(\"./part\")\nprint(new Part { n = 1 })\n",
            &options,
        )
        .unwrap();
        assert!(
            foreign.check.contains("Part.new({ n = 1 })"),
            "{}",
            foreign.check
        );
    }

    /// A call of an imported struct reads the module's `impl`: one
    /// that writes a `new` reports the call to make, and one that
    /// writes none keeps the wording that names the fields form. The
    /// bare name and the star import read the same index.
    #[test]
    fn a_call_of_an_imported_struct_reads_its_constructor() {
        let options = crate::EmitOptions {
            import_struct_fields: vec![
                ("Sched".to_string(), vec![("phase".to_string(), false)]),
                ("Plain".to_string(), vec![("tag".to_string(), false)]),
                ("M.Sched".to_string(), vec![("phase".to_string(), false)]),
            ],
            import_struct_ctors: vec![
                ("Sched".to_string(), "new".to_string()),
                ("M.Sched".to_string(), "new".to_string()),
            ],
            ..Default::default()
        };
        let run = |src: &str| -> Vec<String> {
            crate::compile_with(src, &options)
                .unwrap()
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect()
        };

        assert_eq!(
            run("import { Sched } from \"./s\"\n\nprint(Sched(1))\n"),
            vec!["`Sched(...)` is not a call: construct it with `new Sched(...)`"]
        );
        assert_eq!(
            run("import * as M from \"./s\"\n\nprint(M.Sched(1))\n"),
            vec!["`M.Sched(...)` is not a call: construct it with `new M.Sched(...)`"]
        );
        assert_eq!(
            run("import { Plain } from \"./s\"\n\nprint(Plain(1))\n"),
            vec![
                "`Plain(...)` is not a constructor: construct it with `new Plain { ... }`, since `Plain` writes no `new`"
            ]
        );
    }

    /// The name a `new` writes is not always the name the struct is
    /// declared under: an import alias, a star import, and a namespace
    /// path all reach the same struct, so each reads the same checks,
    /// and each report quotes the name the source wrote.
    #[test]
    fn a_construction_through_an_alias_a_star_import_or_a_namespace_checks() {
        let options = crate::EmitOptions {
            import_struct_fields: vec![(
                "Box".to_string(),
                vec![("id".to_string(), false), ("secret".to_string(), true)],
            )],
            import_privates: vec![("Box".to_string(), vec!["secret".to_string()])],
            ..Default::default()
        };
        let run = |src: &str| -> (Vec<String>, Vec<String>) {
            let out = crate::compile_with(src, &options).unwrap();

            (
                out.diagnostics.iter().map(|d| d.message.clone()).collect(),
                out.lints
                    .iter()
                    .filter(|l| l.name == "private_access")
                    .map(|l| l.message.clone())
                    .collect(),
            )
        };

        let (alias, alias_lints) = run(
            "import { Box as B } from \"./box\"\n\nprint(new B { id = 1, gone = 2 })\nprint(new B { id = 1, secret = 5 })\n",
        );
        assert_eq!(
            alias,
            vec!["`B` has no field `gone`; its fields are `id` and `secret`"]
        );
        assert_eq!(
            alias_lints,
            vec!["`secret` is private to `B`; only its impl sets it"]
        );

        let (star, star_lints) = run(
            "import * as M from \"./box\"\n\nprint(new M.Box { id = 1, gone = 2 })\nprint(new M.Box { id = 1, secret = 5 })\n",
        );
        assert_eq!(
            star,
            vec!["`M.Box` has no field `gone`; its fields are `id` and `secret`"]
        );
        assert_eq!(
            star_lints,
            vec!["`secret` is private to `M.Box`; only its impl sets it"]
        );
    }

    /// `import { M as Mod }` renames the namespace here; the module
    /// declares it as `M`, which is the name every import index is keyed
    /// by. Reading the written head found no struct, so the field checks
    /// and `private_access` went silent.
    #[test]
    fn a_construction_through_an_alias_of_a_namespace_module_checks() {
        let options = crate::EmitOptions {
            import_struct_fields: vec![(
                "M.A.S".to_string(),
                vec![("ok".to_string(), false), ("secret".to_string(), true)],
            )],
            import_privates: vec![("M.A.S".to_string(), vec!["secret".to_string()])],
            ..Default::default()
        };
        let run = |src: &str| -> (Vec<String>, Vec<&'static str>) {
            let out = crate::compile_with(src, &options).unwrap();

            (
                out.diagnostics.iter().map(|d| d.message.clone()).collect(),
                out.lints
                    .iter()
                    .map(|l| l.name)
                    .filter(|n| *n == "private_access")
                    .collect(),
            )
        };

        let (_, private) =
            run("import { M as Mod } from \"./m\"\n\nprint(new Mod.A.S { ok = 1, secret = 9 })\n");
        assert_eq!(private, vec!["private_access"]);

        let (unknown, _) =
            run("import { M as Mod } from \"./m\"\n\nprint(new Mod.A.S { ok = 1, bad = 2 })\n");
        assert_eq!(
            unknown,
            vec!["`Mod.A.S` has no field `bad`; its fields are `ok` and `secret`"]
        );
    }

    /// `import * as M` keys the module's namespace struct as `M.Ns_T`,
    /// the name its shape carries. The report quotes the path the
    /// source wrote, `M.Ns.T`, not the key.
    #[test]
    fn a_report_through_a_star_alias_quotes_the_written_path() {
        let options = crate::EmitOptions {
            import_struct_fields: ["M.Ns.T", "M.Ns_T"]
                .iter()
                .map(|key| {
                    (
                        key.to_string(),
                        vec![("x".to_string(), false), ("y".to_string(), false)],
                    )
                })
                .collect(),
            ..Default::default()
        };
        let messages = |src: &str| -> Vec<String> {
            crate::compile_with(src, &options)
                .unwrap()
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect()
        };

        assert_eq!(
            messages("import * as M from \"./mod\"\n\nprint(new M.Ns.T { })\n"),
            vec![
                "`new M.Ns.T { ... }` leaves `x` and `y` unset; a field without a default needs a value"
            ]
        );
        assert_eq!(
            messages("import * as M from \"./mod\"\n\nprint(new M.Ns.T { x = 1, y = 2, z = 3 })\n"),
            vec!["`M.Ns.T` has no field `z`; its fields are `x` and `y`"]
        );
    }

    /// A struct of a namespace renders under the namespace's name, and
    /// `new Zoo.Lion { }` names it through the path. The report quotes
    /// the path back, at one level and at two.
    #[test]
    fn a_construction_of_a_namespace_struct_checks_its_fields() {
        let one = "namespace Zoo as\n    struct Lion as\n        name: string\n    end\nend\nprint(new Zoo.Lion { name = \"a\", bad = 1 })\n";
        assert_eq!(
            messages(one),
            vec!["`Zoo.Lion` has no field `bad`; its fields are `name`"]
        );

        let two = "namespace A as\n    namespace B as\n        struct S as\n            n: number\n        end\n    end\nend\nprint(new A.B.S { bad = 1 })\n";
        let got = messages(two);
        assert!(
            got.contains(&"`A.B.S` has no field `bad`; its fields are `n`".to_string()),
            "{got:?}"
        );
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

    /// A default type parameter belongs to the `type` alias. On a
    /// function's generic list and on an instantiation it is a Luau
    /// syntax error, so the check artifact writes the names alone.
    #[test]
    fn a_generic_default_stays_on_the_type_alias() {
        let out = crate::compile(
            "struct Pair<A, B = number> as\n    first: A\n    second: B\nend\nlocal p: Pair<string> = new Pair { first = \"x\", second = 1 }\nprint(p)\n",
        )
        .unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("function Pair.__new<A, B>(f: { first: A, second: B }): Pair<A, B>"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("type Pair<A, B = number> ="),
            "{}",
            out.check
        );
        assert!(!out.check.contains("<A, B = number>("), "{}", out.check);
    }

    /// `async function load(self)` in an impl wrote `async function
    /// Loader.loadfunction load(self)`: the head copied the `async` the
    /// body wrap replaces, and the rewrite then wrote the header again.
    #[test]
    fn an_async_method_writes_one_header() {
        let src = "struct Loader as\n    n: number\nend\n\nimpl Loader as\n    async function load(self): number\n        return self.n\n    end\n\n    private async function hidden(self): number\n        return self.n\n    end\nend\nprint(Loader)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(!out.ship.contains("async"), "{}", out.ship);
        assert_eq!(
            out.ship.matches("function Loader.load").count(),
            1,
            "{}",
            out.ship
        );
        assert_eq!(
            out.ship.matches("function load(").count(),
            0,
            "{}",
            out.ship
        );
        assert_eq!(
            out.ship.matches("function Loader.hidden").count(),
            1,
            "{}",
            out.ship
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

    /*
    A struct constructs through `new` alone, and the report said so only
    for a bare name this file declares. The check keyed on that name, so
    an import and a namespace path both went silent and the Luau checker
    reported the call instead.

    The written path now resolves the way `new` resolves it, and the
    report names the path the source wrote.
    */
    #[test]
    fn a_construction_without_new_reports_through_an_import_and_a_path() {
        // A namespace path this file declares, at two depths.
        let src = "namespace Ns as\n    struct T as\n        v: number,\n    end\n    namespace In as\n        struct D as\n            w: number,\n        end\n    end\nend\nlocal t = Ns.T { v = 1 }\nlocal d = Ns.In.D { w = 2 }\nprint(t, d)\n";

        assert_eq!(
            messages(src),
            vec![
                "construct `Ns.T` with `new Ns.T { ... }`",
                "construct `Ns.In.D` with `new Ns.In.D { ... }`",
            ]
        );

        // An imported struct, bare and under an alias. The fields index
        // carries both names, the way the import binds them.
        let options = crate::EmitOptions {
            import_struct_fields: vec![
                ("Point".to_string(), vec![("x".to_string(), false)]),
                ("P".to_string(), vec![("x".to_string(), false)]),
            ],
            ..crate::EmitOptions::default()
        };
        let src = "import { Point, Point as P } from \"./geo\"\nlocal a = Point { x = 1 }\nlocal b = P { x = 2 }\nprint(a, b)\n";
        let out = crate::compile_with(src, &options).unwrap();
        let got: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            got,
            vec![
                "construct `Point` with `new Point { ... }`",
                "construct `P` with `new P { ... }`",
            ]
        );

        // `new` on each of them stays silent.
        let ok = "namespace Ns as\n    struct T as\n        v: number,\n    end\nend\nlocal t = new Ns.T { v = 1 }\nprint(t)\n";

        assert!(messages(ok).is_empty(), "{:?}", messages(ok));

        // A call of an ordinary function with a table is still a call.
        let call = "local function style(t: { n: number }): number\n    return t.n\nend\nprint(style { n = 1 })\n";

        assert!(messages(call).is_empty(), "{:?}", messages(call));
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

    /// An exported local takes the annotation's arguments as a plain one
    /// does. `export const` wrote `HashMap.new()`, and the checker
    /// reported that the type arguments differ.
    #[test]
    fn an_exported_constructor_takes_the_annotation_arguments() {
        for head in ["export const", "export local", "const"] {
            let src = format!("{head} m: HashMap<string, number> = HashMap.new()\nprint(m)\n");
            let out = crate::compile(&src).unwrap();
            assert!(out.diagnostics.is_empty(), "{head}: {:?}", out.diagnostics);
            assert!(
                out.check.contains("HashMap.new<<string, number>>()"),
                "{head}: {}",
                out.check
            );
        }
    }

    /// `is` reads through a type alias, and the branch it opens has to
    /// agree. `type B = Box` narrowed nothing, so a field read under
    /// `if x is B` reported on `unknown` inside a branch that holds.
    /// The else branch and the guard take the cast the same way.
    #[test]
    fn a_narrowing_test_reads_through_a_type_alias() {
        let src = "struct Box as\n    width: number\nend\n\nenum Color as\n    Red\n    Blue\nend\n\ntype B = Box\ntype Hue = Color\n\nlocal function one(x: unknown)\n    if x is B then\n        print(x.width)\n    end\nend\n\nlocal function two(y: unknown)\n    if y is not Hue then\n        print(\"no\")\n    else\n        print(y)\n    end\nend\n\nlocal function three(z: unknown): number\n    if z is not B then\n        return 0\n    end\n\n    return z.width\nend\n\nprint(one, two, three)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        for want in [
            "local x = ((x :: any) :: Box)",
            "local y = ((y :: any) :: Color)",
            "local z = ((z :: any) :: Box)",
        ] {
            assert!(out.check.contains(want), "{want}\n{}", out.check);
        }
    }
}
