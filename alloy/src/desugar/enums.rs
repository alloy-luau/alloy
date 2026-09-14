//! Enum lowering, match/pattern compiling, and if-let/while-let sugar.

use std::collections::HashMap;
use std::collections::HashSet;

use alloy_syntax::ast::{
    Cond, EnumDecl, Expr, FieldPattern, If, MatchExpr, MatchStmt, Pattern, PatternLocal, Stmt,
    TokSpan, While,
};

use super::types::{split_generics, strip_bounds};
use super::*;

/// What a pattern compiles to against one access path.
#[derive(Default)]
pub(crate) struct Compiled {
    /// The tests, joined with `and`. Empty means the pattern always matches.
    tests: Vec<String>,
    /// The bindings, as name and access path.
    binds: Vec<(String, String)>,
}

pub(crate) fn join_tests(tests: &[String]) -> String {
    if tests.is_empty() {
        "true".to_string()
    } else {
        tests.join(" and ")
    }
}

/// Two type spellings that name one type. The comparison drops the
/// spacing, which the author is free to write either way.
pub(crate) fn same_type_text(a: &str, b: &str) -> bool {
    let strip = |t: &str| -> String { t.chars().filter(|c| !c.is_whitespace()).collect() };

    strip(a) == strip(b)
}

/// Whether a bare pattern name reads as a variant: it starts with a
/// capital. A binding the author meant starts lowercase.
pub(crate) fn is_variant_name(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

impl<'s> Desugar<'s> {
    /*
    A payload variant is a constructor that returns a tagged table with the
    enum as its metatable. A unit variant is its own name as a string. The
    type is the union of both, and `is` tests membership. Everything sits
    on the lines the declaration used.
    */
    pub(crate) fn enum_decl(&mut self, e: &EnumDecl) {
        let name = self.decl_name(e.name);
        // The `as` token follows the name, or the parameter list,
        // whether or not `export` leads.
        let header_end = self.toks[e.generics.unwrap_or(e.name).end as usize].end;
        let start = self.byte_start(e.span);
        // `enum Opt<T>`: the alias keeps the list with its defaults, a
        // constructor takes the names alone, and a unit variant casts
        // to the enum of `any` arguments.
        let alias_generics = e
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let fn_generics = if alias_generics.is_empty() {
            String::new()
        } else {
            super::modules::type_arguments(&alias_generics)
        };
        let any_args = if alias_generics.is_empty() {
            String::new()
        } else {
            let anys = vec!["any"; split_generics(&alias_generics).len()];

            format!("<{}>", anys.join(", "))
        };

        // Header line. An attribute line above `enum` keeps its newline.
        self.generate(
            start,
            &format!("local {name} = {{}} {name}.__index = {name}"),
        );
        self.blank_lines(start, header_end);
        let mut cursor = header_end;
        let mut types = Vec::new();
        let mut unit_tests = Vec::new();

        for v in &e.variants {
            let vs = self.byte_start(v.span);
            let ve = self.byte_end(v.span);
            self.copy_gap_without_commas(cursor, vs);
            let vname = self.text_of(v.name).to_string();

            if let Some(value) = &v.value {
                let val = self.render_to_string(value);
                self.generate(vs, &format!("{name}.{vname} = {val}"));
                types.push(format!("typeof({name}.{vname})"));
                unit_tests.push(format!("v == {name}.{vname}"));
            } else if v.payload.is_empty() {
                // The checker widens a string field to `string`; the cast
                // to the enum keeps `Ok(Zone.Spawn)` a `Result<Zone, _>`.
                let value = if self.options.check {
                    format!("(\"{vname}\" :: {name}{any_args})")
                } else {
                    format!("\"{vname}\"")
                };
                self.generate(vs, &format!("{name}.{vname} = {value}"));
                types.push(format!("\"{vname}\""));
                unit_tests.push(format!("v == \"{vname}\""));
            } else {
                let params: Vec<String> = (1..=v.payload.len()).map(|i| format!("_{i}")).collect();
                let fields: Vec<String> = (1..=v.payload.len())
                    .map(|i| format!("_{i} = _{i}"))
                    .collect();
                let field_types: Vec<String> = v
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("_{}: {}", i + 1, self.copy_type_to_string(*t)))
                    .collect();
                // The alias carries the metatable, so a method an `impl`
                // writes on the enum resolves on a payload value.
                let variant_type = format!(
                    "typeof(setmetatable({{}} :: {{ tag: \"{vname}\", {} }}, {name}))",
                    field_types.join(", ")
                );
                // The check artifact types the constructor as this one
                // variant, not as the whole enum: a mixed enum's union
                // holds strings, and a method call on it would read one.
                // The variant is a subtype, so a `{name}` annotation
                // still takes the value.
                let (plist, ret) = if self.options.check {
                    (field_types.join(", "), format!(": {variant_type}"))
                } else {
                    (params.join(", "), String::new())
                };
                let value = format!(
                    "setmetatable({{ tag = \"{vname}\", {} }}, {name})",
                    fields.join(", ")
                );
                let value = self.any_cast(&value);
                self.generate(
                    vs,
                    &format!(
                        "function {name}.{vname}{fn_generics}({plist}){ret} return {value} end"
                    ),
                );
                types.push(variant_type);
            }

            // An attribute line above the variant keeps its newline.
            self.blank_lines(vs, self.byte_start(v.name));
            cursor = ve;
        }

        let end_tok = self.toks[e.span.end as usize - 1];
        self.copy_gap_without_commas(cursor, end_tok.start);

        let mut test = format!("(type(v) == \"table\" and getmetatable(v) == {name})");

        for t in unit_tests {
            test.push_str(&format!(" or {t}"));
        }

        let export = if e.exported || self.ns_export {
            "export "
        } else {
            ""
        };
        // A variant with a payload prints as `Msg.Move(1, 2)`; a unit
        // variant is a string and prints as its name already.
        let mut printer = if self.options.definitions {
            String::new()
        } else {
            let std = self.std();

            format!(
                " {name}.__tostring = function(v) return {std}.show_variant({}, v) end",
                luau_string(&self.display_name(&name))
            )
        };

        // `@derive(Eq)` compares the tag and the payload slots; a unit
        // variant is a string and compares on its own. `Clone` copies a
        // payload variant with its metatable; `Debug` is the printer above.
        let max_arity = e
            .variants
            .iter()
            .map(|v| v.payload.len())
            .max()
            .unwrap_or(0);
        let mut derived: HashSet<String> = HashSet::new();

        for a in &e.attributes {
            let Some(aname) = a.name else { continue };

            if self.text_of(aname) != "derive" {
                continue;
            }

            for arg in &a.args {
                let which = self.text_of(arg.span()).to_string();
                // `Eq` and `PartialEq` write the same `__eq`.
                let key = if which == "PartialEq" { "Eq" } else { &which };

                if !derived.insert(key.to_string()) {
                    continue;
                }

                match which.as_str() {
                    "Eq" | "PartialEq" => {
                        let slots: Vec<String> = (1..=max_arity)
                            .map(|i| format!(" and a._{i} == b._{i}"))
                            .collect();
                        printer.push_str(&format!(
                            " {name}.__eq = function(a: any, b: any): boolean return a.tag == b.tag{} end",
                            slots.join("")
                        ));
                    }

                    "Clone" => {
                        printer.push_str(&format!(
                            " function {name}.clone(v: any): any return if type(v) == \"table\" then setmetatable(table.clone(v), {name}) else v end"
                        ));
                    }

                    _ => {}
                }
            }
        }

        // The attributes on the enum and on its variants, for
        // `Attributes.get(Enum, attr)` and `Attributes.variant`.
        let own = self.attr_table(&e.attributes);
        let variant_attrs: Vec<String> = e
            .variants
            .iter()
            .filter_map(|v| {
                let table = self.attr_table(&v.attributes);

                (table != "{}").then(|| format!("{} = {table}", self.text_of(v.name)))
            })
            .collect();

        if !self.options.definitions && (own != "{}" || !variant_attrs.is_empty()) {
            let std = self.std();
            printer.push_str(&format!(
                " {std}.attrs({name}, {{ own = {own}, variants = {{ {} }} }})",
                variant_attrs.join(", ")
            ));
        }
        // An enum with no variant has no type to write: `type E = `
        // is not Luau, so the artifact says `never` and the report
        // names what the body wants.
        if e.variants.is_empty() {
            self.diagnose(e.name, "an `enum` needs at least one variant");
        }

        let union = match types.is_empty() {
            true => "never".to_string(),

            false => types.join(" | "),
        };
        self.generate(
            end_tok.start,
            &format!(
                "function {name}.is(v) return {test} end{printer} {export}type {name}{alias_generics} = {union}"
            ),
        );

        if e.exported {
            self.exports.push((name.clone(), name));
        }
    }

    pub(crate) fn renamed(&self, name: &str) -> Option<String> {
        self.renames
            .iter()
            .rev()
            .find_map(|m| m.get(name).cloned())
            .or_else(|| self.ns_member_name(name))
    }

    /// Reports if a bare name is a unit variant of a known enum.
    pub(crate) fn unit_variant_of(&self, name: &str) -> Option<String> {
        self.enums
            .iter()
            .find(|(_, vs)| vs.iter().any(|(v, n)| v == name && *n == 0))
            .map(|(e, _)| e.clone())
    }

    /// The variant a pattern name spells, with the enum when a dotted
    /// path names one: `Kind.Big` answers `("Big", Some("Kind"))`, and
    /// the bare `Big` answers `("Big", None)` for the caller to find the
    /// enum by its variant.
    pub(crate) fn pattern_variant(&self, name: TokSpan) -> (String, Option<String>) {
        // A macro body travels as tokens joined by spaces, so a path
        // reaches here as `Choice . Yes`. No name holds a space, so the
        // join drops out again.
        let text: String = self.text_of(name).split_whitespace().collect();

        if !text.contains('.') {
            return (text, None);
        }

        match self.enum_of_path(&text) {
            Some((e, v)) => (v, Some(e)),

            None => {
                let last = text.rsplit('.').next().unwrap_or(&text);

                (last.to_string(), None)
            }
        }
    }

    /// The enum a scrutinee column names, when this file declares it.
    /// A variant pattern or a unit variant in any arm answers.
    pub(crate) fn column_enum(&self, arms: &[&[Pattern]], col: usize) -> Option<String> {
        for pats in arms {
            let Some(top) = pats.get(col) else { continue };
            let mut stack = vec![top];

            while let Some(p) = stack.pop() {
                // A bare name is a variant only when it carries nothing;
                // otherwise it binds the value under its own name.
                let (name, unit) = match p {
                    Pattern::Or(a, b, _) => {
                        stack.push(a);
                        stack.push(b);
                        continue;
                    }

                    // A dotted path names the enum itself, so no search
                    // over the variant names has to guess it.
                    Pattern::Variant { name, .. } => match self.pattern_variant(*name) {
                        (_, Some(e)) => return self.castable_enum(&e),

                        (v, None) => (v, false),
                    },

                    Pattern::Bind(name) => (self.text_of(*name).to_string(), true),

                    _ => continue,
                };
                let found = self
                    .enum_decls
                    .iter()
                    .find(|(_, vs)| vs.iter().any(|(v, n)| *v == name && (!unit || *n == 0)));

                if let Some((e, _)) = found {
                    return self.castable_enum(e);
                }
            }
        }

        None
    }

    /// The type name a scrutinee casts to. A generic enum's alias asks
    /// for arguments the arms do not spell, so the scrutinee keeps the
    /// type it has.
    fn castable_enum(&self, e: &str) -> Option<String> {
        let name = self.enum_type_name(e);
        let generic = self.generic_types.contains(e) || self.generic_types.contains(&name);

        (!generic).then_some(name)
    }

    /// The type name an enum writes. `enum_decls` keys an enum inside a
    /// namespace by its written path, `A.Kind`, but the emit renders the
    /// member under one flat name, `A_Kind`, and that is the only name
    /// Luau knows.
    pub(crate) fn enum_type_name(&self, name: &str) -> String {
        if !name.contains('.') {
            return name.to_string();
        }

        self.ns_path_name(name)
            .unwrap_or_else(|| name.replace('.', "_"))
    }

    /// A fresh local for a match scrutinee in the check artifact, cast
    /// to the enum its arms name. `type(x) == "table"` reads a lone
    /// variant type wrong, and a value the code just built has one; read
    /// through the enum it narrows right. The ship artifact keeps the
    /// scrutinee as it is.
    pub(crate) fn scrutinee_local(&mut self, value: String, ename: &str, anchor: u32) -> String {
        self.temp_next += 1;
        let name = format!("_v{}", self.temp_next);
        self.hoists.push(Hoist::Fresh {
            name: name.clone(),
            value: format!("({value}) :: {ename}"),
            anchor,
        });

        name
    }

    /// The path each scrutinee reads through: a name or a temp, and in
    /// the check artifact a local typed as the enum the arms name.
    pub(crate) fn scrutinee_paths(
        &mut self,
        scrutinees: &[Expr],
        arms: &[&[Pattern]],
    ) -> Vec<String> {
        let mut paths = Vec::new();

        for (col, sc) in scrutinees.iter().enumerate() {
            let ename = if self.options.check && self.no_hoist == 0 {
                self.column_enum(arms, col)
            } else {
                None
            };

            match ename {
                Some(e) => {
                    let anchor = self.byte_start(sc.span());
                    let value = self.render_to_string(sc);
                    let path = self.scrutinee_local(value, &e, anchor);
                    paths.push(path);
                }

                None => {
                    let path = self.reusable(sc);
                    paths.push(path);
                }
            }
        }

        paths
    }

    /// Compiles a pattern against an access path.
    pub(crate) fn compile_pattern(&mut self, p: &Pattern, path: &str, out: &mut Compiled) {
        match p {
            Pattern::Wildcard(_) => {}

            Pattern::Bind(name) => {
                let n = self.text_of(*name).to_string();

                if self.unit_variant_of(&n).is_some() {
                    out.tests.push(format!("{path} == \"{n}\""));
                } else {
                    out.binds.push((n, path.to_string()));
                }
            }

            Pattern::Literal(e) => {
                let lit = self.render_to_string(e);
                out.tests.push(format!("{path} == {lit}"));
            }

            Pattern::Path(span) => {
                let text = self.text_of(*span).to_string();
                out.tests.push(format!("{path} == {text}"));
            }

            Pattern::Variant { name, args, .. } => {
                let (vname, _) = self.pattern_variant(*name);
                out.tests.push(format!(
                    "type({path}) == \"table\" and {path}.tag == \"{vname}\""
                ));

                for (i, a) in args.iter().enumerate() {
                    let sub = format!("{path}._{}", i + 1);
                    self.compile_pattern(a, &sub, out);
                }
            }

            Pattern::Struct { name, fields, .. } => {
                match name {
                    Some(n) => {
                        let sname = self.text_of(*n).to_string();
                        out.tests
                            .push(format!("getmetatable({}) == {sname}", self.any_cast(path)));
                    }

                    None => out.tests.push(format!("type({path}) == \"table\"")),
                }

                for FieldPattern { field, pattern } in fields {
                    let fname = self.text_of(*field).to_string();
                    let sub = format!("{path}.{fname}");

                    match pattern {
                        Some(sub_pat) => self.compile_pattern(sub_pat, &sub, out),

                        None => out.binds.push((fname, sub)),
                    }
                }
            }

            Pattern::Array { items, rest, .. } => {
                let op = if rest.is_some() { ">=" } else { "==" };
                out.tests.push(format!(
                    "type({path}) == \"table\" and #{path} {op} {}",
                    items.len()
                ));

                for (i, item) in items.iter().enumerate() {
                    let sub = format!("{path}[{}]", i + 1);
                    self.compile_pattern(item, &sub, out);
                }

                if let Some(r) = rest {
                    let rname = self.text_of(*r).to_string();
                    let std = self.std();
                    out.binds.push((
                        rname,
                        format!("{std}.Array.slice({path}, {})", items.len() + 1),
                    ));
                }
            }

            Pattern::Or(a, b, span) => {
                let mut ca = Compiled::default();
                let mut cb = Compiled::default();
                self.compile_pattern(a, path, &mut ca);
                self.compile_pattern(b, path, &mut cb);
                let ta = self.cast_tests(&join_tests(&ca.tests), path);
                let tb = self.cast_tests(&join_tests(&cb.tests), path);
                out.tests.push(format!("(({ta}) or ({tb}))"));

                let names_a: Vec<&String> = ca.binds.iter().map(|(n, _)| n).collect();
                let names_b: Vec<&String> = cb.binds.iter().map(|(n, _)| n).collect();

                if names_a != names_b {
                    self.diagnose(
                        *span,
                        "both sides of an `or` pattern must bind the same names",
                    );
                }

                for (n, pa) in &ca.binds {
                    let pb = cb
                        .binds
                        .iter()
                        .find(|(m, _)| m == n)
                        .map(|(_, p)| p.clone())
                        .unwrap_or_else(|| pa.clone());
                    // The checker refines each side by its own tag and
                    // cannot pick one across the `or`; the check artifact
                    // reads the payload untyped.
                    let (pa, pb) = (self.cast_root(pa), self.cast_root(&pb));
                    out.binds
                        .push((n.clone(), format!("(if {ta} then {pa} else {pb})")));
                }
            }
        }
    }

    /// The test text for several patterns against several paths, plus a
    /// guard rendered with the bindings substituted.
    pub(crate) fn arm_test(
        &mut self,
        patterns: &[Pattern],
        paths: &[String],
        guard: Option<&Expr>,
    ) -> (String, Compiled) {
        let mut c = Compiled::default();

        for (p, path) in patterns.iter().zip(paths) {
            self.compile_pattern(p, path, &mut c);
        }

        let mut test = join_tests(&c.tests);

        if let Some(g) = guard {
            let map: HashMap<String, String> = c.binds.iter().cloned().collect();
            self.renames.push(map);
            let text = self.render_to_string(g);
            self.renames.pop();
            test = if test == "true" {
                format!("({text})")
            } else {
                format!("{test} and ({text})")
            };
        }

        (test, c)
    }

    /// Checks the variant patterns of a match against the enum they name.
    ///
    /// A pattern whose variant belongs to a known enum must bind the
    /// payload the variant carries. A name no enum owns is a missing
    /// variant of the enum the other arms name.
    pub(crate) fn check_variant_patterns(&mut self, arms: &[&[Pattern]]) -> bool {
        let mut reported = false;
        let mut flat: Vec<(TokSpan, usize)> = Vec::new();

        for pats in arms {
            let mut stack: Vec<&Pattern> = pats.iter().collect();

            while let Some(p) = stack.pop() {
                match p {
                    Pattern::Or(a, b, _) => {
                        stack.push(a);
                        stack.push(b);
                    }

                    Pattern::Variant { name, args, .. } => {
                        flat.push((*name, args.len()));

                        for a in args {
                            stack.push(a);
                        }
                    }

                    // `case Lobby then` writes a unit variant as a bare
                    // name, which the parser reads as a binding. A
                    // capital says the author meant a variant, so a name
                    // no enum owns is a typo, not a catch-all.
                    Pattern::Bind(name) if is_variant_name(self.text_of(*name)) => {
                        flat.push((*name, 0));
                    }

                    _ => {}
                }
            }
        }

        // The enum of the match: the first variant name a declared enum
        // owns. A name none owns is then a variant that enum lacks.
        let owner = flat.iter().find_map(|(name, _)| {
            let (vname, path_enum) = self.pattern_variant(*name);

            path_enum.or_else(|| {
                self.enums
                    .iter()
                    .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                    .map(|(e, _)| e.clone())
            })
        });

        for (name, binds) in flat {
            let (vname, path_enum) = self.pattern_variant(name);
            let found = self
                .enums
                .iter()
                .filter(|(e, _)| path_enum.as_ref().is_none_or(|p| p == *e))
                .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                .map(|(e, vs)| {
                    (
                        e.clone(),
                        vs.iter()
                            .find(|(v, _)| *v == vname)
                            .map(|(_, n)| *n)
                            .unwrap_or(0),
                    )
                });

            match found {
                Some((_, arity)) if arity != binds => {
                    let message = format!(
                        "the variant `{vname}` carries {}, the arm binds {binds}",
                        if arity == 1 {
                            "1 value".to_string()
                        } else {
                            format!("{arity} values")
                        }
                    );
                    self.diagnose(name, &message);
                    reported = true;
                }

                Some(_) => {}

                None => {
                    if let Some(e) = &owner
                        && let Some(vs) = self.enum_decls.get(e)
                    {
                        let names: Vec<&str> = vs.iter().map(|(v, _)| v.as_str()).collect();
                        let message = format!(
                            "`{e}` has no variant `{vname}`; its variants are {}",
                            list_names(&names)
                        );
                        self.diagnose(name, &message);
                        reported = true;
                    }
                }
            }
        }

        reported
    }

    /// Checks single-level exhaustiveness over a known enum.
    pub(crate) fn match_is_exhaustive(&self, arms: &[&[Pattern]], guards: &[bool]) -> bool {
        // One scrutinee; a guarded arm proves nothing.
        let mut column: Vec<&Pattern> = Vec::new();

        for (pats, guarded) in arms.iter().zip(guards) {
            let [p] = pats else {
                return false;
            };

            if !*guarded {
                column.push(p);
            }
        }

        self.column_covers(&column)
    }

    /// The lints on a `default` arm: one that cannot run, and one with
    /// nothing in it. The arm is the `default` token after the arms.
    pub(crate) fn default_lints(
        &mut self,
        span: TokSpan,
        arms_end: u32,
        exhaustive: bool,
        empty: bool,
    ) {
        let at = (arms_end as usize..span.end as usize)
            .find(|&i| self.text_of(TokSpan::new(i, i + 1)) == "default")
            .map(|i| self.toks[i])
            .unwrap_or(self.toks[span.start as usize]);

        if exhaustive {
            self.lints.push(Lint {
                name: "unreachable_default",
                start: at.start,
                end: at.end,
                message: "this `default` never runs: the arms cover every variant; delete it so a new variant is a missing arm, not a silent fallback".to_string(),
                fix: None,
            });
        }

        if empty {
            self.lints.push(Lint {
                name: "empty_default",
                start: at.start,
                end: at.end,
                message: "this `default` is empty and swallows every variant without an arm; name them, or write the fallback".to_string(),
                fix: None,
            });
        }
    }

    /// The diagnostic for a match with no `default` that leaves values
    /// out. When the arms name variants of one enum, the message lists the
    /// variants with no arm.
    pub(crate) fn not_exhaustive_message(&self, arms: &[&[Pattern]]) -> String {
        let generic = "this match is not exhaustive; add a `default` arm".to_string();
        let mut named: Vec<String> = Vec::new();
        let mut enum_name: Option<String> = None;

        for pats in arms {
            let mut stack: Vec<&Pattern> = pats.iter().collect();

            while let Some(p) = stack.pop() {
                match p {
                    Pattern::Or(a, b, _) => {
                        stack.push(a);
                        stack.push(b);
                    }

                    Pattern::Bind(n) => {
                        let name = self.text_of(*n).to_string();

                        if let Some(e) = self.unit_variant_of(&name) {
                            enum_name.get_or_insert(e);
                            named.push(name);
                        }
                    }

                    Pattern::Path(span) => {
                        let text = self.text_of(*span).to_string();

                        if let Some((e, v)) = self.enum_of_path(&text) {
                            enum_name.get_or_insert(e);
                            named.push(v);
                        }
                    }

                    // `case "Red"` matches a unit variant: the emit
                    // compares the same string.
                    Pattern::Literal(v) => {
                        if let Expr::String(span) = v.as_ref() {
                            let text = self.text_of(*span);
                            let name = text.trim_matches(|c| c == '"' || c == '\'').to_string();

                            if let Some(e) = self.unit_variant_of(&name) {
                                enum_name.get_or_insert(e);
                                named.push(name);
                            }
                        }
                    }

                    Pattern::Variant { name, .. } => {
                        let (vname, path_enum) = self.pattern_variant(*name);
                        let owner = path_enum.or_else(|| {
                            self.enums
                                .iter()
                                .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                                .map(|(e, _)| e.clone())
                        });

                        if let Some(e) = owner {
                            enum_name.get_or_insert(e);
                            named.push(vname);
                        }
                    }

                    _ => {}
                }
            }
        }

        let Some(e) = enum_name else {
            return generic;
        };
        let mut missing: Vec<String> = self.enums[&e]
            .iter()
            .filter(|(v, _)| !named.contains(v))
            .map(|(v, _)| v.clone())
            .collect();

        // Every variant has an arm, so what is left out sits in a
        // payload: `Some(Err(_))`. The path names it.
        if missing.is_empty() {
            let column: Vec<&Pattern> = arms
                .iter()
                .filter_map(|pats| match pats {
                    [p] => Some(p),

                    _ => None,
                })
                .collect();

            if column.len() != arms.len() {
                return generic;
            }

            match self.uncovered_case(&column) {
                Some(path) => missing.push(path),

                None => return generic,
            }
        }

        let missing: Vec<&str> = missing.iter().map(|v| v.as_str()).collect();

        format!(
            "this match is not exhaustive: `{e}` has no arm for {}; add {} or a `default` arm",
            list_names(&missing),
            if missing.len() == 1 { "it" } else { "them" }
        )
    }

    /// The path of one case the arms leave out, `Some(Err(_))`, read
    /// off a column of patterns over one value. `None` when the column
    /// covers, or when no path names what is left.
    fn uncovered_case(&self, column: &[&Pattern]) -> Option<String> {
        let mut flat: Vec<&Pattern> = Vec::new();
        let mut stack: Vec<&Pattern> = column.to_vec();

        while let Some(q) = stack.pop() {
            match q {
                Pattern::Or(a, b, _) => {
                    stack.push(a);
                    stack.push(b);
                }

                other => flat.push(other),
            }
        }

        let mut enum_name: Option<String> = None;
        // Variant name -> the payload rows seen for it.
        let mut rows: Vec<(String, Vec<&Pattern>)> = Vec::new();

        for p in flat {
            match p {
                Pattern::Wildcard(_) => return None,

                Pattern::Bind(n) => {
                    let name = self.text_of(*n).to_string();
                    let e = self.unit_variant_of(&name)?;
                    enum_name.get_or_insert(e);
                    rows.push((name, Vec::new()));
                }

                Pattern::Path(span) => {
                    let text = self.text_of(*span).to_string();
                    let (e, v) = self.enum_of_path(&text)?;
                    enum_name.get_or_insert(e);
                    rows.push((v, Vec::new()));
                }

                Pattern::Variant { name, args, .. } => {
                    let (vname, path_enum) = self.pattern_variant(*name);
                    let owner = path_enum.or_else(|| {
                        self.enums
                            .iter()
                            .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                            .map(|(e, _)| e.clone())
                    })?;
                    enum_name.get_or_insert(owner);
                    rows.push((vname, args.iter().collect()));
                }

                _ => return None,
            }
        }

        let e = enum_name?;

        for (v, arity) in self.enums.get(&e)? {
            let payloads: Vec<&Vec<&Pattern>> = rows
                .iter()
                .filter(|(n, _)| n == v)
                .map(|(_, args)| args)
                .collect();

            if payloads.is_empty() {
                return Some(match arity {
                    0 => v.clone(),

                    n => format!("{v}({})", vec!["_"; *n].join(", ")),
                });
            }

            if *arity == 0 || self.payloads_cover(&payloads, *arity) {
                continue;
            }

            // The field that refutes: every other one binds in each row,
            // so its own column says what is left out.
            for j in 0..*arity {
                let others_bind = payloads.iter().all(|r| {
                    r.iter()
                        .enumerate()
                        .all(|(i, p)| i == j || self.irrefutable(p))
                });

                if !others_bind {
                    continue;
                }

                let inner: Vec<&Pattern> =
                    payloads.iter().filter_map(|r| r.get(j).copied()).collect();

                if let Some(path) = self.uncovered_case(&inner) {
                    let args: Vec<String> = (0..*arity)
                        .map(|i| match i == j {
                            true => path.clone(),

                            false => "_".to_string(),
                        })
                        .collect();

                    return Some(format!("{v}({})", args.join(", ")));
                }
            }

            return None;
        }

        None
    }

    /// Reports if a set of patterns over one value leaves no value out.
    ///
    /// An irrefutable pattern covers everything. Enum variants cover their
    /// enum when every variant appears with covering payloads. Array
    /// patterns cover when an open pattern at length k comes with every
    /// exact length below k.
    pub(crate) fn column_covers(&self, column: &[&Pattern]) -> bool {
        // Flatten `a or b` into two rows.
        let mut flat: Vec<&Pattern> = Vec::new();

        for p in column {
            let mut stack: Vec<&Pattern> = vec![p];

            while let Some(q) = stack.pop() {
                match q {
                    Pattern::Or(a, b, _) => {
                        stack.push(a.as_ref());
                        stack.push(b.as_ref());
                    }

                    other => flat.push(other),
                }
            }
        }

        let mut enum_name: Option<String> = None;
        // Variant name -> the payload rows seen for it.
        let mut rows: Vec<(String, Vec<&Pattern>)> = Vec::new();
        let mut exact_lengths: HashSet<usize> = HashSet::new();
        let mut open_from: Option<usize> = None;

        for p in flat {
            match p {
                Pattern::Wildcard(_) => return true,

                Pattern::Bind(n) => {
                    let name = self.text_of(*n).to_string();

                    match self.unit_variant_of(&name) {
                        Some(e) => {
                            enum_name.get_or_insert(e);
                            rows.push((name, Vec::new()));
                        }

                        // A capitalised name no enum owns is a misspelt
                        // variant, not a catch-all. Counting it as one
                        // would let `unreachable_default` claim the arms
                        // cover the enum.
                        None if is_variant_name(&name) => return false,

                        None => return true,
                    }
                }

                Pattern::Array { items, rest, .. } => {
                    let all_bind = items.iter().all(|i| self.irrefutable(i));

                    if !all_bind {
                        return false;
                    }

                    match rest {
                        Some(_) => {
                            open_from = Some(open_from.map_or(items.len(), |o| o.min(items.len())));
                        }

                        None => {
                            exact_lengths.insert(items.len());
                        }
                    }
                }

                Pattern::Path(span) => {
                    // `Color.Red` covers the unit variant `Red` of `Color`.
                    let text = self.text_of(*span).to_string();
                    let Some((e, v)) = self.enum_of_path(&text) else {
                        return false;
                    };

                    match self.enums.get(&e) {
                        Some(vs) if vs.iter().any(|(n, c)| *n == v && *c == 0) => {
                            enum_name.get_or_insert(e);
                            rows.push((v, Vec::new()));
                        }

                        _ => return false,
                    }
                }

                Pattern::Variant { name, args, .. } => {
                    let (vname, path_enum) = self.pattern_variant(*name);
                    let owner = path_enum.or_else(|| {
                        self.enums
                            .iter()
                            .find(|(_, vs)| vs.iter().any(|(v, _)| *v == vname))
                            .map(|(e, _)| e.clone())
                    });

                    let Some(e) = owner else {
                        return false;
                    };

                    enum_name.get_or_insert(e);
                    rows.push((vname, args.iter().collect()));
                }

                // A struct has one shape, so a pattern that names one
                // covers it when each field it names binds. A bare
                // table pattern tests fields on a value of any shape
                // and covers nothing. One arm that tests a literal
                // field covers nothing on its own, so the scan reads
                // the next arm instead of answering for the column.
                Pattern::Struct { .. } => {
                    if self.struct_pattern_covers(p) {
                        return true;
                    }
                }

                _ => return false,
            }
        }

        if let Some(k) = open_from
            && enum_name.is_none()
            && (0..k).all(|n| exact_lengths.contains(&n))
        {
            return true;
        }

        let Some(e) = enum_name else {
            return false;
        };

        let variants = self.enums[&e].clone();

        variants.iter().all(|(v, arity)| {
            let payloads: Vec<&Vec<&Pattern>> = rows
                .iter()
                .filter(|(n, _)| n == v)
                .map(|(_, args)| args)
                .collect();

            if payloads.is_empty() {
                return false;
            }

            if *arity == 0 {
                return true;
            }

            self.payloads_cover(&payloads, *arity)
        })
    }

    /// Whether a struct pattern covers the shape it names: the name is
    /// a struct this file declares, and every field it names binds.
    fn struct_pattern_covers(&self, p: &Pattern) -> bool {
        let Pattern::Struct { name, fields, .. } = p else {
            return self.irrefutable(p);
        };
        let Some(n) = name else {
            return false;
        };

        self.structs.contains(self.text_of(*n))
            && fields.iter().all(|f| match &f.pattern {
                None => true,

                Some(inner) => self.struct_pattern_covers(inner),
            })
    }

    /// Whether a pattern matches every value: a wildcard, or a name that
    /// binds. A bare name that spells a unit variant tests for it (see
    /// `compile_pattern`), so it refutes.
    pub(crate) fn irrefutable(&self, p: &Pattern) -> bool {
        match p {
            Pattern::Wildcard(_) => true,

            Pattern::Bind(n) => self.unit_variant_of(self.text_of(*n)).is_none(),

            _ => false,
        }
    }

    /// Reports if the payload rows of one variant cover every payload.
    ///
    /// A row of irrefutable patterns covers. Otherwise one field must be
    /// the only refutable field in every row, and that field's column must
    /// cover on its own.
    pub(crate) fn payloads_cover(&self, rows: &[&Vec<&Pattern>], arity: usize) -> bool {
        if rows.iter().any(|r| r.iter().all(|p| self.irrefutable(p))) {
            return true;
        }

        (0..arity).any(|j| {
            let others_bind = rows.iter().all(|r| {
                r.iter()
                    .enumerate()
                    .all(|(i, p)| i == j || self.irrefutable(p))
            });

            if !others_bind {
                return false;
            }

            let column: Vec<&Pattern> = rows.iter().filter_map(|r| r.get(j).copied()).collect();

            self.column_covers(&column)
        })
    }

    /// `match` as a statement: an if-chain on temps, one arm per line.
    pub(crate) fn match_stmt(&mut self, m: &MatchStmt) {
        let start = self.byte_start(m.span);
        let with_end = self.toks[m
            .arms
            .first()
            .map(|a| a.span.start)
            .unwrap_or(m.span.end - 1) as usize
            - 1]
        .end;

        let pats: Vec<&[Pattern]> = m.arms.iter().map(|a| a.patterns.as_slice()).collect();

        // `match a, b with` becomes `do local _1 = a local _2 = b`.
        let mut paths = Vec::new();
        let mut header = String::from("do");

        for (col, sc) in m.scrutinees.iter().enumerate() {
            let value = self.render_to_string(sc);
            // The check artifact reads the scrutinee as the enum the arms
            // name; see `scrutinee_local`.
            let value = match self
                .options
                .check
                .then(|| self.column_enum(&pats, col))
                .flatten()
            {
                Some(e) => format!("({value}) :: {e}"),

                None => value,
            };
            self.temp_next += 1;
            let index = self.temp_next;
            header.push_str(&format!(" local _m{index} = {value}"));
            paths.push(format!("_m{index}"));
        }

        self.generate(start, &header);
        let mut cursor = with_end;
        let guards: Vec<bool> = m.arms.iter().map(|a| a.guard.is_some()).collect();

        let exhaustive = self.match_is_exhaustive(&pats, &guards);
        // A rejected pattern makes the arm list unreliable, so the
        // exhaustiveness message would name the wrong variant.
        let bad_arm = self.check_variant_patterns(&pats);

        if m.default.is_none() && !exhaustive && !bad_arm {
            let msg = self.not_exhaustive_message(&pats);
            self.diagnose(m.span, &msg);
        }

        if let Some(d) = &m.default {
            let arms_end = m.arms.last().map_or(m.span.start, |a| a.span.end);
            self.default_lints(m.span, arms_end, exhaustive, d.stmts.is_empty());
        }

        for (i, arm) in m.arms.iter().enumerate() {
            let arm_start = self.byte_start(arm.span);
            self.copy(cursor, arm_start);
            let (test, c) = self.arm_test(&arm.patterns, &paths, arm.guard.as_ref());
            let keyword = if i == 0 { "if" } else { "elseif" };
            let mut text = format!("{keyword} {test} then");

            if !c.binds.is_empty() {
                let names: Vec<String> = c.binds.iter().map(|(n, _)| n.clone()).collect();
                let values: Vec<String> = c.binds.iter().map(|(_, p)| p.clone()).collect();
                text.push_str(&format!(
                    " local {} = {}",
                    names.join(", "),
                    values.join(", ")
                ));
            }

            // The arm head runs to `then`; the block follows.
            let then_tok = self.find_tok_after(
                arm.patterns
                    .last()
                    .map(|p| p.span().end)
                    .unwrap_or(arm.span.start),
                "then",
            );
            let then_end = self.toks[then_tok as usize].end;
            self.generate(arm_start, &text);
            cursor = then_end;

            self.scopes.push(HashSet::new());

            for (n, _) in &c.binds {
                let names = n.clone();

                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(names);
                }
            }

            let body_start = self.block_start_or(&arm.block, cursor);
            self.copy(cursor, body_start);
            self.block(&arm.block);
            self.scopes.pop();
            cursor = self.block_end_or(&arm.block, body_start);
        }

        if let Some(d) = &m.default {
            // The `default` token sits before the block.
            let default_tok = self.toks[d.span.start as usize - 1];
            let default_tok = if d.span.is_empty() {
                self.toks[m.span.end as usize - 2]
            } else {
                default_tok
            };
            self.copy(cursor, default_tok.start);
            self.generate(default_tok.start, "else");
            cursor = default_tok.end;
            let body_start = self.block_start_or(d, cursor);
            self.copy(cursor, body_start);
            self.block(d);
            cursor = self.block_end_or(d, body_start);
        }

        let end_tok = self.toks[m.span.end as usize - 1];
        self.copy(cursor, end_tok.start);

        // Every variant has an arm, so the chain has no `else` and the
        // checker reads a path that falls through. The raise closes it,
        // and an exhaustive match whose arms all return counts as one.
        if m.default.is_none() && exhaustive && !m.arms.is_empty() {
            self.generate(
                end_tok.start,
                "else error(\"match: no arm covers this value\", 2) ",
            );
        }

        self.copy(end_tok.start, end_tok.end);
        self.generate(end_tok.end, " end");
    }

    /// `match` as an expression: nested if-expressions with bindings
    /// substituted, arms staying on their lines.
    pub(crate) fn match_expr(&mut self, m: &MatchExpr) {
        let start = self.byte_start(m.span);
        let pats: Vec<&[Pattern]> = m.arms.iter().map(|a| a.patterns.as_slice()).collect();
        let paths = self.scrutinee_paths(&m.scrutinees, &pats);
        let guards: Vec<bool> = m.arms.iter().map(|a| a.guard.is_some()).collect();
        let exhaustive = self.match_is_exhaustive(&pats, &guards);
        // A rejected pattern makes the arm list unreliable, so the
        // exhaustiveness message would name the wrong variant.
        let bad_arm = self.check_variant_patterns(&pats);

        if m.default.is_none() && !exhaustive && !bad_arm {
            let msg = self.not_exhaustive_message(&pats);
            self.diagnose(m.span, &msg);
        }

        if m.default.is_some() {
            let arms_end = m.arms.last().map_or(m.span.start, |a| a.span.end);
            // A match expression has no empty default: the parser needs
            // a value after `default`. `default nil` is the fallback the
            // source wrote, and the only spelling for "no value here",
            // so it is not the empty body the lint looks for.
            self.default_lints(m.span, arms_end, exhaustive, false);
        }

        let with_end = self.toks[m
            .arms
            .first()
            .map(|a| a.span.start)
            .unwrap_or(m.span.end - 1) as usize
            - 1]
        .end;
        self.generate(start, "(");
        let mut cursor = with_end;
        let last_index = m.arms.len().saturating_sub(1);

        for (i, arm) in m.arms.iter().enumerate() {
            let arm_start = self.byte_start(arm.span);
            self.copy(cursor, arm_start);
            let (test, c) = self.arm_test(&arm.patterns, &paths, arm.guard.as_ref());
            let is_last_without_default = m.default.is_none() && i == last_index && exhaustive;
            // One arm that covers every value has nothing to branch on.
            // `(else v)` is not Luau, so the value stands alone.
            let keyword = if is_last_without_default && i == 0 {
                String::new()
            } else if is_last_without_default {
                "else".to_string()
            } else if i == 0 {
                format!("if {test} then")
            } else {
                format!("elseif {test} then")
            };

            if !keyword.is_empty() {
                self.generate(arm_start, &keyword);
            }
            let then_tok = self.find_tok_after(
                arm.patterns
                    .last()
                    .map(|p| p.span().end)
                    .unwrap_or(arm.span.start),
                "then",
            );
            let then_end = self.toks[then_tok as usize].end;
            cursor = then_end;
            let vs = self.byte_start(arm.value.span());
            self.copy(cursor, vs);
            let map: HashMap<String, String> = c.binds.iter().cloned().collect();
            self.renames.push(map);
            self.expr(&arm.value);
            self.renames.pop();
            cursor = self.byte_end(arm.value.span());
        }

        if let Some(d) = &m.default {
            let default_tok = self.toks[d.span().start as usize - 1];
            self.copy(cursor, default_tok.start);
            self.generate(default_tok.start, "else");
            cursor = default_tok.end;
            let vs = self.byte_start(d.span());
            self.copy(cursor, vs);
            self.expr(d);
            cursor = self.byte_end(d.span());
        } else if !exhaustive {
            self.generate(cursor, " else nil");
        }

        let end_tok = self.toks[m.span.end as usize - 1];
        self.copy(cursor, end_tok.start);
        self.generate(end_tok.start, ")");
    }

    /// `local Ok(v) = e` and let-else.
    pub(crate) fn pattern_local(&mut self, p: &PatternLocal) {
        let anchor = self.byte_start(p.span);
        let value = self.render_to_string(&p.value);
        let temp = self.hoist_text(value, anchor);
        let mut c = Compiled::default();
        self.compile_pattern(&p.pattern, &temp, &mut c);
        let test = join_tests(&c.tests);
        // Luau has `const` of its own, so the keyword passes through and
        // a reassignment is Luau's compile error.
        let keyword = self.text_of(p.keyword).to_string();
        let binds = if c.binds.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = c.binds.iter().map(|(n, _)| n.clone()).collect();
            let values: Vec<String> = c.binds.iter().map(|(_, v)| v.clone()).collect();

            format!("{keyword} {} = {}", names.join(", "), values.join(", "))
        };

        match &p.else_block {
            None => {
                let pat = self.text_of(p.pattern.span()).to_string();
                self.hoist_stmt(
                    format!(
                        "if not ({test}) then error({} .. tostring(if type({temp}) == \"table\" then {temp}.tag else {temp})) end",
                        luau_string(&format!("pattern `{pat}` did not match, got "))
                    ),
                    anchor,
                );
                self.generate(anchor, &binds);
            }

            Some(block) => {
                self.generate(anchor, &format!("if not ({test}) then"));
                let else_tok = self.toks[block.span.start as usize - 1];
                let else_tok = if block.span.is_empty() {
                    self.toks[p.span.end as usize - 2]
                } else {
                    else_tok
                };
                let body_start = self.block_start_or(block, else_tok.end);
                self.copy(else_tok.end, body_start);
                self.block(block);
                let after = self.block_end_or(block, body_start);
                let end_tok = self.toks[p.span.end as usize - 1];
                self.copy(after, end_tok.start);
                self.copy(end_tok.start, end_tok.end);

                if !binds.is_empty() {
                    self.generate(end_tok.end, &format!(" {binds}"));
                }
            }
        }
    }

    // --- conditions with bindings -------------------------------------------

    /// The declarations and test for a `Cond::Local`, for a block context
    /// where temps and bindings can be locals.
    pub(crate) fn cond_local_parts(
        &mut self,
        cond: &Cond,
    ) -> (Vec<String>, String, Vec<(String, String)>) {
        let Cond::Local {
            negated,
            bindings,
            filter,
            ..
        } = cond
        else {
            unreachable!("only local conditions have parts");
        };

        let mut decls = Vec::new();
        let mut tests = Vec::new();
        let mut binds: Vec<(String, String)> = Vec::new();
        let mut prior: Vec<String> = Vec::new();

        for b in bindings {
            // An earlier name in this condition is its temp by now.
            let map: HashMap<String, String> = binds.iter().cloned().collect();
            self.renames.push(map);
            let value = self.render_to_string(&b.value);
            self.renames.pop();
            let ty =
                b.ty.map(|t| format!(": {}", self.text_of(t)))
                    .unwrap_or_default();
            // A later binding runs only when the earlier ones are truthy.
            let value = if prior.is_empty() {
                value
            } else {
                format!("if {} then {value} else nil", prior.join(" and "))
            };

            match &b.pattern {
                // The branch declares the name from a temp the test refined,
                // so the name is `T`, not `T?`, in the branch and in any
                // closure there. A negated condition keeps the name, since
                // it must stay in scope after a guard clause.
                Pattern::Bind(n) if self.unit_variant_of(self.text_of(*n)).is_none() => {
                    let name = self.text_of(*n).to_string();

                    if *negated {
                        decls.push(format!("local {name}{ty} = {value}"));
                        tests.push(name.clone());
                        prior.push(name);
                    } else {
                        self.temp_next += 1;
                        let temp = format!("_c{}", self.temp_next);
                        decls.push(format!("local {temp}{ty} = {value}"));
                        tests.push(temp.clone());
                        prior.push(temp.clone());
                        binds.push((name, temp));
                    }
                }

                pat => {
                    self.temp_next += 1;
                    let temp = format!("_c{}", self.temp_next);
                    decls.push(format!("local {temp} = {value}"));
                    let mut c = Compiled::default();
                    self.compile_pattern(pat, &temp, &mut c);
                    let test = join_tests(&c.tests);
                    tests.push(format!("({test})"));
                    prior.push(format!("({test})"));
                    binds.extend(c.binds);
                }
            }
        }

        let mut test = tests.join(" and ");

        if let Some(f) = filter {
            let map: HashMap<String, String> = binds.iter().cloned().collect();
            self.renames.push(map);
            let text = self.render_to_string(f);
            self.renames.pop();
            test = format!("{test} and ({text})");
        }

        if *negated {
            test = format!("not ({test})");
        }

        (decls, test, binds)
    }

    pub(crate) fn if_with_locals(&mut self, span: TokSpan, i: &If) {
        let start = self.byte_start(span);
        let mut cursor = start;
        // Every block this rewrite opens beyond the source `if`'s own `end`.
        let mut extra_ends = 0usize;
        // `if not local x = e then return end` is the guard clause: `x`
        // stays in scope after it, so no `do` wraps the declaration.
        let guard_clause = i.branches.len() == 1
            && i.else_block.is_none()
            && matches!(i.branches[0].0, Cond::Local { negated: true, .. });

        for (idx, (cond, block)) in i.branches.iter().enumerate() {
            // The keyword token before the condition.
            let kw_tok = self.toks[cond.span().start as usize - 1];
            self.copy(cursor, kw_tok.start);
            let then_tok = self.find_tok_after(cond.span().end, "then");
            let then_end = self.toks[then_tok as usize].end;

            match cond {
                Cond::Expr(e) => {
                    let keyword = if idx == 0 { "if" } else { "elseif" };
                    let c = self.render_to_string(e);
                    self.generate(kw_tok.start, &format!("{keyword} {c} then"));
                }

                Cond::Local { .. } => {
                    let (decls, test, binds) = self.cond_local_parts(cond);
                    let lead = if guard_clause {
                        ""
                    } else if idx == 0 {
                        "do "
                    } else {
                        "else do "
                    };

                    // `do` and `if` open two blocks; the source `end` closes
                    // one of them, except in the guard clause, which opens
                    // only the `if`.
                    if !guard_clause {
                        extra_ends += 1;
                    }

                    if idx > 0 {
                        extra_ends += 1;
                    }

                    let mut text = format!("{lead}{} if {test} then", decls.join(" "));

                    if !binds.is_empty() && !matches!(cond, Cond::Local { negated: true, .. }) {
                        let names: Vec<String> = binds.iter().map(|(n, _)| n.clone()).collect();
                        let values: Vec<String> = binds.iter().map(|(_, v)| v.clone()).collect();
                        text.push_str(&format!(
                            " local {} = {}",
                            names.join(", "),
                            values.join(", ")
                        ));
                    }

                    self.generate(kw_tok.start, &text);
                }
            }

            cursor = then_end;
            let body_start = self.block_start_or(block, cursor);
            self.copy(cursor, body_start);
            self.block(block);
            cursor = self.block_end_or(block, body_start);
        }

        if let Some(e) = &i.else_block {
            let else_tok = self.toks[e.span.start as usize - 1];
            let else_tok = if e.span.is_empty() {
                self.toks[span.end as usize - 2]
            } else {
                else_tok
            };
            self.copy(cursor, else_tok.end);
            cursor = else_tok.end;
            let body_start = self.block_start_or(e, cursor);
            self.copy(cursor, body_start);
            self.block(e);
            cursor = self.block_end_or(e, body_start);
        }

        let end_tok = self.toks[span.end as usize - 1];
        self.copy(cursor, end_tok.start);
        self.copy(end_tok.start, end_tok.end);
        self.generate(end_tok.end, &" end".repeat(extra_ends));
    }

    /// The name a condition tests for nil: `x == nil` or `not x`, on a
    /// plain local or parameter.
    pub(crate) fn nil_test_name(&self, c: &Expr) -> Option<TokSpan> {
        match c {
            Expr::Binary { op, lhs, rhs, .. } if self.text_of(*op) == "==" => {
                match (&**lhs, &**rhs) {
                    (Expr::Name(n), Expr::Nil(_)) => Some(*n),

                    _ => None,
                }
            }

            Expr::Unary { op, operand, .. } if self.text_of(*op) == "not" => match &**operand {
                Expr::Name(n) => Some(*n),

                _ => None,
            },

            _ => None,
        }
    }

    /*
    `if x == nil then x = v end` on a plain local or parameter, with the
    `not x` form and the `else` form beside it. The then-body must be that
    one assignment and nothing else.

    Luau narrows no name after an assignment inside a branch, in either
    solver, so the check artifact writes the same store as an `if`
    expression. The ship artifact keeps the statement, and the
    `manual_coalesce` lint still asks for `??=`.
    */
    pub(crate) fn coalesce_if<'a>(&self, i: &'a If) -> Option<(TokSpan, &'a Expr)> {
        if i.branches.len() != 1 {
            return None;
        }

        let (cond, block) = &i.branches[0];
        let Cond::Expr(c) = cond else {
            return None;
        };
        let name = self.nil_test_name(c)?;

        let [Stmt::Assign(a)] = &block.stmts[..] else {
            return None;
        };

        if a.targets.len() != 1 || a.values.len() != 1 || self.text_of(a.op) != "=" {
            return None;
        }

        let Expr::Name(t) = &a.targets[0] else {
            return None;
        };

        if self.text_of(*t) != self.text_of(name) {
            return None;
        }

        // `x =` is dropped, so it must hold no newline: the output keeps
        // the line count of the source.
        let head = self.byte_start(a.span) as usize..self.byte_start(a.values[0].span()) as usize;

        if self.src[head].contains('\n') {
            return None;
        }

        Some((name, &a.values[0]))
    }

    /// Whether this statement, or anything under it, is a coalescing `if`.
    /// The walk copies a statement whole when nothing under it changes,
    /// so the outer function has to answer for its body.
    pub(crate) fn holds_coalesce_if(&self, s: &Stmt) -> bool {
        if let Stmt::If(i) = s
            && self.coalesce_if(i).is_some()
        {
            return true;
        }

        stmt_children(s).iter().any(|c| match c {
            Child::Expr(_) => false,

            Child::Block(b) => b.stmts.iter().any(|s| self.holds_coalesce_if(s)),

            Child::Function(f) => f.block.stmts.iter().any(|s| self.holds_coalesce_if(s)),
        })
    }

    /// Renders the statement `coalesce_if` matched as
    /// `x = if x == nil then v else x`, then reopens the source `if` with
    /// an empty body so the trailing `else` and `end` keep their places.
    pub(crate) fn coalesce_if_stmt(&mut self, span: TokSpan, i: &If, name: TokSpan, value: &Expr) {
        let start = self.byte_start(span);
        let n = self.text_of(name).to_string();
        let (_, block) = &i.branches[0];
        self.generate(start, &format!("{n} = "));
        // `if COND then` copies from the source, so a condition that
        // spans lines keeps every newline.
        self.copy(start, self.byte_start(block.span));
        let vspan = value.span();
        self.expr(value);
        let mut cursor = self.byte_end(vspan);
        self.generate(cursor, &format!(" else {n} if {n} == nil then"));

        if let Some(e) = &i.else_block {
            let body_start = self.block_start_or(e, cursor);
            self.copy(cursor, body_start);
            self.block(e);
            cursor = self.block_end_or(e, body_start);
        }

        self.copy(cursor, self.byte_end(span));
    }

    pub(crate) fn while_with_local(&mut self, span: TokSpan, w: &While) {
        let start = self.byte_start(span);
        let (decls, test, binds) = self.cond_local_parts(&w.cond);
        let do_tok = self.find_tok_after(w.cond.span().end, "do");
        let do_end = self.toks[do_tok as usize].end;
        let mut text = format!(
            "while true do {} if not ({test}) then break end",
            decls.join(" ")
        );

        if !binds.is_empty() {
            let names: Vec<String> = binds.iter().map(|(n, _)| n.clone()).collect();
            let values: Vec<String> = binds.iter().map(|(_, v)| v.clone()).collect();
            text.push_str(&format!(
                " local {} = {}",
                names.join(", "),
                values.join(", ")
            ));
        }

        self.generate(start, &text);
        let body_start = self.block_start_or(&w.block, do_end);
        self.copy(do_end, body_start);
        self.block(&w.block);
        let after = self.block_end_or(&w.block, body_start);
        let end_tok = self.toks[span.end as usize - 1];
        self.copy(after, end_tok.start);
        self.copy(end_tok.start, end_tok.end);
    }

    /// `if local x = f() then a else b` as an expression: the binding hoists,
    /// and a pattern's names substitute.
    pub(crate) fn if_expr_with_locals(
        &mut self,
        span: TokSpan,
        branches: &[(Cond, Expr)],
        else_value: &Expr,
    ) {
        let anchor = self.byte_start(span);
        let mut text = String::from("(");

        for (idx, (cond, value)) in branches.iter().enumerate() {
            let keyword = if idx == 0 { "if" } else { "elseif" };

            match cond {
                Cond::Expr(e) => {
                    let c = self.render_to_string(e);
                    let v = self.render_to_string(value);
                    text.push_str(&format!("{keyword} {c} then {v} "));
                }

                Cond::Local { .. } => {
                    let (decls, test, binds) = self.cond_local_parts(cond);

                    for d in decls {
                        // `local x = e` hoists as a temp-like declaration.
                        let d = d.strip_prefix("local ").unwrap_or(&d).to_string();
                        let (name, value) = d
                            .split_once(" = ")
                            .map(|(a, b)| (a.to_string(), b.to_string()))
                            .unwrap_or((d.clone(), "nil".to_string()));
                        let name = name.split(':').next().unwrap_or(&name).trim().to_string();
                        self.hoists.push(Hoist::Stmt {
                            text: format!("local {name} = {value}"),
                            anchor,
                        });
                    }

                    let map: HashMap<String, String> = binds.iter().cloned().collect();
                    self.renames.push(map);
                    let v = self.render_to_string(value);
                    self.renames.pop();
                    text.push_str(&format!("{keyword} {test} then {v} "));
                }
            }
        }

        let e = self.render_to_string(else_value);
        text.push_str(&format!("else {e})"));
        self.generate(anchor, &text);
    }

    /// A test text with every read from `path` cast to `any` in the check
    /// artifact: across an `or`, the checker refines each side by its own
    /// tag and cannot read the other side's fields.
    pub(crate) fn cast_tests(&self, test: &str, path: &str) -> String {
        if !self.options.check {
            return test.to_string();
        }

        test.replace(&format!("{path}."), &format!("({path} :: any)."))
    }

    /// An access path with its root cast to `any` in the check artifact:
    /// `_m1._1` becomes `(_m1 :: any)._1`. The ship artifact keeps it.
    pub(crate) fn cast_root(&self, path: &str) -> String {
        if !self.options.check {
            return path.to_string();
        }

        match path.find(['.', '[']) {
            Some(i) => format!("({} :: any){}", &path[..i], &path[i..]),

            None => path.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    /// `enum E as end` wrote `type E = `, which is not Luau. The body
    /// wants a variant, and the report says so.
    #[test]
    fn an_empty_enum_reports_and_still_types() {
        let out = crate::compile("enum Empty as end\nprint(Empty)\n").unwrap();
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert!(
            messages.contains(&"an `enum` needs at least one variant"),
            "{messages:?}"
        );
        assert!(out.check.contains("type Empty = never"), "{}", out.check);
    }

    use crate::EmitOptions;

    fn messages(src: &str) -> Vec<String> {
        crate::compile(src)
            .unwrap()
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect()
    }

    fn lint_names(src: &str) -> Vec<&'static str> {
        crate::compile(src)
            .unwrap()
            .lints
            .iter()
            .map(|l| l.name)
            .collect()
    }

    /// `enum Opt<T>`: the alias and each constructor carry the
    /// parameter, a unit variant casts to the enum of `any`, and the
    /// ship artifact keeps the tagged table.
    #[test]
    fn a_generic_enum_types_its_constructors_and_alias() {
        let src = "export enum Opt<T> as\n    Some(T)\n    Nil\nend\nlocal a: Opt<number> = Opt.Some(1)\nlocal b: Opt<string> = Opt.Nil\nprint(a, b)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.starts_with(
                "local __alloy = require(\"@alloy\") local Opt = {} Opt.__index = Opt\n"
            ),
            "{}",
            out.check
        );
        assert!(
            out.check.contains(
                "function Opt.Some<T>(_1: T): typeof(setmetatable({} :: { tag: \"Some\", _1: T }, Opt)) return"
            ),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("Opt.Nil = (\"Nil\" :: Opt<any>)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains(
                "export type Opt<T> = typeof(setmetatable({} :: { tag: \"Some\", _1: T }, Opt)) | \"Nil\""
            ),
            "{}",
            out.check
        );
        assert!(
            out.ship.contains(
                "function Opt.Some<T>(_1) return setmetatable({ tag = \"Some\", _1 = _1 }, Opt) end"
            ),
            "{}",
            out.ship
        );
        assert!(out.ship.contains("Opt.Nil = \"Nil\""), "{}", out.ship);
    }

    /// A default belongs to the alias alone; a constructor names the
    /// parameters, the way a struct's `__new` does.
    #[test]
    fn a_generic_default_stays_on_the_enum_alias() {
        let src = "enum Either<L, R = string> as\n    Left(L)\n    Right(R)\nend\nlocal e: Either<number> = Either.Left(1)\nprint(e)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check.contains("type Either<L, R = string> ="),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("function Either.Left<L, R>(_1: L)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("function Either.Right<L, R>(_1: R)"),
            "{}",
            out.check
        );
    }

    /// A generic enum's alias asks for arguments the arms do not spell,
    /// so the match reads the scrutinee as it is. The arms still cover
    /// the variants, and one short still reports.
    #[test]
    fn a_match_over_a_generic_enum_keeps_the_scrutinee_type() {
        let src = "enum Opt<T> as\n    Some(T)\n    Nil\nend\nlocal function f(o: Opt<number>): number\n    return match o with\n        case Some(v) then v\n        case Nil then 0\n    end\nend\nprint(f)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(!out.check.contains(":: Opt\n"), "{}", out.check);
        assert!(!out.check.contains(":: Opt "), "{}", out.check);

        let short = "enum Opt<T> as\n    Some(T)\n    Nil\nend\nlocal function f(o: Opt<number>): number\n    return match o with\n        case Some(v) then v\n    end\nend\nprint(f)\n";

        assert_eq!(
            messages(short),
            vec![
                "this match is not exhaustive: `Opt` has no arm for `Nil`; add it or a `default` arm"
            ]
        );
    }

    /// `impl Opt` on `enum Opt<T>` binds no `T`; the report names the
    /// enum, not a struct.
    #[test]
    fn an_impl_without_the_enum_parameters_reports() {
        let src = "enum Opt<T> as\n    Some(T)\n    Nil\nend\nimpl Opt as\n    function get(self): T\n        return (self :: any)._1\n    end\nend\nprint(Opt)\n";

        assert_eq!(
            messages(src),
            vec!["the enum `Opt` takes `<T>`; write `impl Opt<T>` so its methods can name them"]
        );

        let with = "enum Opt<T> as\n    Some(T)\n    Nil\nend\nimpl Opt<T> as\n    function get(self, fallback: T): T\n        return match self with\n            case Some(v) then v\n            case Nil then fallback\n        end\n    end\nend\nprint(Opt)\n";
        let out = crate::compile(with).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("function Opt.get<T>(self: Opt<T>, fallback: T): T"),
            "{}",
            out.check
        );
    }

    /// A generic enum inside a namespace: the match covers its variants
    /// and casts nothing to the flat name.
    #[test]
    fn a_generic_enum_in_a_namespace_matches() {
        let src = "namespace A as\n    enum Opt<T> as\n        Some(T)\n        Nil\n    end\nend\nlocal function f(o: A.Opt<number>): number\n    return match o with\n        case A.Opt.Some(v) then v\n        case A.Opt.Nil then 0\n    end\nend\nprint(f)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.check.contains("type A_Opt<T> ="), "{}", out.check);
        assert!(!out.check.contains(":: A_Opt "), "{}", out.check);
        assert!(!out.check.contains(":: A_Opt\n"), "{}", out.check);
    }

    /// An imported `enum Opt<T>` reads as generic through the export
    /// list, by name and through `import * as M`, so the match casts
    /// nothing to the bare name and still covers the variants.
    #[test]
    fn an_imported_generic_enum_skips_the_cast() {
        let variants = vec![("Some".to_string(), 1), ("Nil".to_string(), 0)];
        let types = vec![("./lib".to_string(), vec!["Opt<T>".to_string()])];
        let src = "import { Opt } from \"./lib\"\nlocal function f(o: Opt<number>): number\n    return match o with\n        case Some(v) then v\n        case Nil then 0\n    end\nend\nprint(f)\n";
        let options = EmitOptions {
            import_enums: vec![("Opt".to_string(), variants.clone())],
            import_types: types.clone(),
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).expect("compiles");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(!out.check.contains(":: Opt\n"), "{}", out.check);

        let star = "import * as M from \"./lib\"\nlocal function f(o: M.Opt<number>): number\n    return match o with\n        case M.Opt.Some(v) then v\n        case M.Opt.Nil then 0\n    end\nend\nprint(f)\n";
        let options = EmitOptions {
            import_enums: vec![("M.Opt".to_string(), variants)],
            import_types: types,
            ..EmitOptions::default()
        };
        let out = crate::compile_with(star, &options).expect("compiles");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(!out.check.contains(":: M_Opt"), "{}", out.check);
        assert!(!out.check.contains(":: M.Opt"), "{}", out.check);
    }

    /// A static call of a method an imported enum's impl writes,
    /// `Opt.or_else(o, 5)`, is no missing variant: the impl sits in the
    /// module that declares the enum, and the checker types the call
    /// off the import. An enum of this file still reports.
    #[test]
    fn a_method_of_an_imported_enum_is_no_missing_variant() {
        let variants = vec![("Some".to_string(), 1), ("Nil".to_string(), 0)];
        let options = EmitOptions {
            import_enums: vec![("Opt".to_string(), variants)],
            import_types: vec![("./lib".to_string(), vec!["Opt<T>".to_string()])],
            ..EmitOptions::default()
        };
        let src = "import { Opt } from \"./lib\"\nlocal o: Opt<number> = Opt.Some(1)\nprint(Opt.or_else(o, 5))\n";
        let out = crate::compile_with(src, &options).expect("compiles");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        let own = "enum Opt as\n    Some(number)\n    Nil\nend\nlocal o = Opt.Some(1)\nprint(Opt.or_else(o, 5))\n";
        let got = messages(own);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "`Opt` has no variant `or_else`; its variants are `Some` and `Nil`"
        );
    }

    #[test]
    fn a_match_over_an_imported_enum_is_exhaustive() {
        let src = "import { R } from \"./e1\"\nlocal function t(r: R): number\n    return match r with\n        case R.A then 1\n        case R.B then 2\n    end\nend\nprint(t)\n";
        let options = EmitOptions {
            import_enums: vec![(
                "R".to_string(),
                vec![("A".to_string(), 0), ("B".to_string(), 0)],
            )],
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).expect("compiles");
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert!(messages.is_empty(), "{messages:?}");
    }

    /// An enum inside an imported namespace reads under its path,
    /// `Geo.Kind`, because the module has no top-level declaration of
    /// it. The path resolved to the rendered name a local namespace
    /// writes, `Geo_Kind`, which the enum index never holds, so every
    /// arm covered nothing and the match reported.
    #[test]
    fn a_match_over_an_imported_namespace_enum_is_exhaustive() {
        let src = "import { Geo } from \"./lib\"\nlocal function t(k: Geo.Kind): number\n    return match k with\n        case Geo.Kind.Round then 1\n        case Geo.Kind.Square then 2\n    end\nend\nprint(t)\n";
        let variants = vec![("Round".to_string(), 0), ("Square".to_string(), 0)];
        let options = EmitOptions {
            import_enums: vec![("Geo.Kind".to_string(), variants.clone())],
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).expect("compiles");
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert!(messages.is_empty(), "{messages:?}");

        // Two levels deep, and one arm short: the message names the enum
        // by its path and the variant with no arm.
        let deep = "import { Outer } from \"./lib\"\nlocal function t(k: Outer.Inner.Kind): number\n    return match k with\n        case Outer.Inner.Kind.Round then 1\n    end\nend\nprint(t)\n";
        let options = EmitOptions {
            import_enums: vec![("Outer.Inner.Kind".to_string(), variants)],
            ..EmitOptions::default()
        };
        let out = crate::compile_with(deep, &options).expect("compiles");
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            messages,
            vec![
                "this match is not exhaustive: `Outer.Inner.Kind` has no arm for `Square`; add it or a `default` arm"
            ]
        );
    }

    /// The check artifact casts a match scrutinee to the enum. An enum
    /// inside an imported namespace reads under its path, `Geo.Kind`,
    /// which is no Luau type, so the cast wrote `:: Geo.Kind` and the
    /// Luau checker reported `Unknown type`. The emit renders the member
    /// under one flat name, and the cast now writes that name.
    #[test]
    fn a_match_casts_an_imported_namespace_enum_to_its_flat_name() {
        let variants = vec![("Small".to_string(), 0), ("Big".to_string(), 1)];
        let src = "import { Geo } from \"./lib\"\nlocal function t(k: Geo.Kind): number\n    return match k with\n        case Geo.Kind.Small then 1\n        case Big(n) then n\n    end\nend\nprint(t)\n";
        let options = EmitOptions {
            import_enums: vec![("Geo.Kind".to_string(), variants.clone())],
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).expect("compiles");
        assert!(out.check.contains(":: Geo_Kind"), "{}", out.check);
        assert!(!out.check.contains(":: Geo.Kind"), "{}", out.check);

        // Two levels deep flattens every step.
        let deep = "import { Outer } from \"./lib\"\nlocal function t(k: Outer.Inner.Kind): number\n    return match k with\n        case Outer.Inner.Kind.Small then 1\n        case Big(n) then n\n    end\nend\nprint(t)\n";
        let options = EmitOptions {
            import_enums: vec![("Outer.Inner.Kind".to_string(), variants)],
            ..EmitOptions::default()
        };
        let out = crate::compile_with(deep, &options).expect("compiles");
        assert!(out.check.contains(":: Outer_Inner_Kind"), "{}", out.check);
    }

    /// The shape scan reads a namespace member, so the module that
    /// imports the namespace knows the enum's variants.
    #[test]
    fn the_shape_scan_lists_an_enum_inside_a_namespace() {
        let src = "export namespace Outer as\n    public namespace Inner as\n        public enum Kind as\n            Round\n            Square\n        end\n    end\nend\n";
        let names: Vec<String> = crate::declarations::shapes(src)
            .iter()
            .map(|s| s.name().to_string())
            .collect();

        assert_eq!(names, vec!["Outer.Inner.Kind".to_string()]);
    }

    #[test]
    fn one_arm_that_covers_every_value_needs_no_branch() {
        // `(else v)` is not Luau, so a match expression with one arm and
        // no default writes the value alone.
        let src = "enum Msg as\n    Join(number)\nend\nlocal function h(m: Msg): number\n    return match m with\n        case Join(n) then n\n    end\nend\nprint(h)\n";
        let out = crate::compile(src).unwrap();
        assert!(!out.ship.contains("else"), "{}", out.ship);
        assert!(!out.check.contains("else"), "{}", out.check);
    }

    #[test]
    fn a_rejected_arm_still_writes_a_branch_that_parses() {
        let src = "enum Msg as\n    Join(number)\nend\nlocal function h(m: Msg): number\n    return match m with\n        case Join() then 0\n    end\nend\nprint(h)\n";
        let out = crate::compile(src).unwrap();
        assert!(!out.diagnostics.is_empty());
        assert!(
            out.ship.contains("return (\n         0\n    )"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_payload_constructor_returns_its_own_variant() {
        // A mixed enum's union holds strings, so a method call on the
        // whole union reports. The constructor gives back one variant,
        // which carries the metatable and the impl methods.
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\n    Empty\nend\nprint(Shape.Rect(1, 2))\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.check.contains(
                "function Shape.Rect(_1: number, _2: number): typeof(setmetatable({} :: { tag: \"Rect\", _1: number, _2: number }, Shape))"
            ),
            "{}",
            out.check
        );
        // The ship artifact keeps the untyped constructor.
        assert!(
            out.ship.contains("function Shape.Rect(_1, _2) return"),
            "{}",
            out.ship
        );
    }

    #[test]
    fn a_match_reads_its_scrutinee_as_the_enum_in_the_check_artifact() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\n    Empty\nend\nlocal s = Shape.Rect(1, 2)\nlocal n = match s with\n    case Circle(r) then r\n    case Rect(w, h) then w\n    case Empty then 0\nend\nprint(n)\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.check.contains("local _v1 = (s) :: Shape"),
            "{}",
            out.check
        );
        // The ship artifact reads the scrutinee where it stands.
        assert!(!out.ship.contains(":: Shape"), "{}", out.ship);
    }

    #[test]
    fn a_payload_enum_alias_carries_the_metatable() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\n";
        let out = crate::compile(src).unwrap();
        assert!(
            out.check.contains(
                "type Shape = typeof(setmetatable({} :: { tag: \"Circle\", _1: number }, Shape))"
            ),
            "{}",
            out.check
        );
    }

    /// `case Kind.Big(n) then`: a dotted path names the variant, so a
    /// payload list may follow it. The parser stopped at the path and
    /// reported `expected `then`, found `(``.
    #[test]
    fn a_dotted_path_carries_its_payload() {
        let src = "enum Shape as\n    Circle(number)\n    Empty\nend\nlocal s = Shape.Circle(1)\nmatch s with\n    case Shape.Circle(r) then print(r)\n    case Empty then print(0)\nend\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship
                .contains("_m1.tag == \"Circle\" then local r = _m1._1"),
            "{}",
            out.ship
        );

        // A nested payload pattern reads the same, and a dotted arm
        // counts for exhaustiveness: dropping `Empty` reports it.
        let short = "enum Shape as\n    Circle(number)\n    Empty\nend\nlocal s = Shape.Circle(1)\nmatch s with\n    case Shape.Circle(r) then print(r)\nend\n";
        let got = messages(short);
        assert_eq!(
            got,
            vec![
                "this match is not exhaustive: `Shape` has no arm for `Empty`; add it or a `default` arm"
            ]
        );
    }

    /// Every variant has an arm, so what the match leaves out sits in
    /// a payload. The report names the path, one level and two.
    #[test]
    fn a_missing_nested_arm_names_its_path() {
        let one = "enum Loaded as\n    Some(Result<number, string>)\n    None\nend\nlocal l = Loaded.None\nmatch l with\n    case Some(Ok(v)) then print(v)\n    case Loaded.None then print(0)\nend\n";
        assert_eq!(
            messages(one),
            vec![
                "this match is not exhaustive: `Loaded` has no arm for `Some(Err(_))`; add it or a `default` arm"
            ]
        );

        let two = "enum Loaded as\n    Some(Result<number, string>)\n    None\nend\nenum Box as\n    Hold(Loaded)\n    Empty\nend\nlocal b = Box.Empty\nmatch b with\n    case Hold(Some(Ok(v))) then print(v)\n    case Hold(Loaded.None) then print(0)\n    case Box.Empty then print(1)\nend\n";
        assert_eq!(
            messages(two),
            vec![
                "this match is not exhaustive: `Box` has no arm for `Hold(Some(Err(_)))`; add it or a `default` arm"
            ]
        );
    }

    #[test]
    fn a_match_arm_binds_the_payload_the_variant_carries() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\nlocal s = Shape.Circle(1)\nlocal n = match s with\n    case Circle(r, extra) then r\n    case Rect(w) then w\nend\nprint(n)\n";
        let got = messages(src);
        assert!(
            got.iter()
                .any(|m| m == "the variant `Circle` carries 1 value, the arm binds 2"),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|m| m == "the variant `Rect` carries 2 values, the arm binds 1"),
            "{got:?}"
        );
    }

    #[test]
    fn a_match_arm_names_a_variant_the_enum_has() {
        let src = "enum Msg as\n    Join(number)\n    Chat(number, string)\nend\nlocal m = Msg.Join(1)\nlocal t = match m with\n    case Join(p) then \"j\"\n    case Chat(p, s) then \"c\"\n    case Quit(p) then \"q\"\nend\nprint(t)\n";
        let got = messages(src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "`Msg` has no variant `Quit`; its variants are `Join` and `Chat`"
        );
    }

    #[test]
    fn an_exhaustive_match_statement_closes_its_chain() {
        let src = "enum Shape as\n    Circle(number)\n    Rect(number, number)\nend\nlocal function area(s: Shape): number\n    match s with\n        case Circle(r) then\n            return r\n        case Rect(w, h) then\n            return w * h\n    end\nend\nprint(area)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship
                .contains("else error(\"match: no arm covers this value\", 2)"),
            "{}",
            out.ship
        );
    }

    /// A unit variant is a bare name in a pattern, so a misspelt one
    /// reads as a binding. A capital says the author meant a variant.
    #[test]
    fn a_misspelt_unit_variant_in_an_arm_reports() {
        let src = "enum Phase as\n    Lobby\n    Playing(number)\nend\nlocal function d(p: Phase): string\n    return match p with\n        case Lobby then \"a\"\n        case Playing(n) then \"b\"\n        case Finished then \"c\"\n        default \"?\"\n    end\nend\nprint(d)\n";
        let got = messages(src);
        assert!(
            got.iter().any(|m| m
                == "`Phase` has no variant `Finished`; its variants are `Lobby` and `Playing`"),
            "{got:?}"
        );
        // The arms do not cover the enum, so the `default` does run.
        assert!(
            !lint_names(src).contains(&"unreachable_default"),
            "{:?}",
            lint_names(src)
        );
        // A lowercase name is still the catch-all it has always been.
        let bound = "enum Phase as\n    Lobby\n    Playing(number)\nend\nlocal function d(p: Phase): string\n    return match p with\n        case Lobby then \"a\"\n        case rest then \"b\"\n    end\nend\nprint(d)\n";
        assert!(messages(bound).is_empty(), "{:?}", messages(bound));
    }

    #[test]
    fn an_unreachable_default_fires_on_a_match_expression() {
        let src = "enum Color as Red, Green, Blue end\nlocal function full(c: Color): string\n    return match c with\n        case Color.Red then \"r\"\n        case Color.Green then \"g\"\n        case Color.Blue then \"b\"\n        default \"?\"\n    end\nend\nprint(full)\n";
        assert!(
            lint_names(src).contains(&"unreachable_default"),
            "{:?}",
            lint_names(src)
        );
    }

    #[test]
    fn a_default_that_writes_nil_is_not_an_empty_default() {
        let src = "local function pick(t: string): string?\n    return match t with\n        case \"a\" then \"A\"\n        default nil\n    end\nend\nprint(pick)\n";
        assert!(
            !lint_names(src).contains(&"empty_default"),
            "{:?}",
            lint_names(src)
        );
    }

    #[test]
    fn a_default_with_an_empty_block_is_an_empty_default() {
        let src = "enum Color as Red, Green, Blue end\nlocal function f(c: Color)\n    match c with\n        case Color.Red then print(\"r\")\n        default\n    end\nend\nprint(f)\n";
        assert!(
            lint_names(src).contains(&"empty_default"),
            "{:?}",
            lint_names(src)
        );
    }
}
