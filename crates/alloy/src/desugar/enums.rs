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
    /// The bindings. The name's span lets a declaration copy the name,
    /// so the editor maps it back to the pattern site.
    binds: Vec<Bind>,
}

/// One name a pattern binds: its source span, the access path it
/// reads, and the annotation an `if local v: T` wrote on it.
type Bind = (TokSpan, String, Option<TokSpan>);

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
/// A constructor's generic list and the arguments of the enum it
/// returns. A parameter the payload names stays a parameter; one it
/// does not name takes its default, or `any`: `Either.Left(4)` is an
/// `Either<number, string>`, and `Opt.Nil` an `Opt<any>`.
fn variant_arguments(alias_generics: &str, payload: &[String]) -> (String, String) {
    let mut names = Vec::new();
    let mut args = Vec::new();

    for item in split_generics(alias_generics) {
        let (name, default) = match item.split_once('=') {
            Some((n, d)) => (n.trim(), d.trim()),

            None => (item.as_str(), "any"),
        };

        if payload.iter().any(|t| names_word(t, name)) {
            names.push(name.to_string());
            args.push(name.to_string());
        } else {
            args.push(default.to_string());
        }
    }

    let generics = if names.is_empty() {
        String::new()
    } else {
        format!("<{}>", names.join(", "))
    };

    (generics, format!("<{}>", args.join(", ")))
}

/// True when the type text writes `name` as a whole word.
fn names_word(text: &str, name: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';

    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back().is_none_or(|c| !is_word(c));
        let after = text[at + name.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word(c));

        before && after
    })
}

pub(crate) fn is_variant_name(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

/// `MAX` and `MAX_HP`: a constant's name, never a variant's or a binding's.
fn is_screaming(name: &str) -> bool {
    name.len() > 1
        && name.chars().any(char::is_uppercase)
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The body of one arm the statement chain writes.
pub(crate) enum ArmBody<'a> {
    /// A statement match's block.
    Block(&'a Block),
    /// A value arm of a hoisted match: one expression, or a block that
    /// ends in its value.
    Value(&'a Expr),
}

/// One arm of the chain, from either form of `match`.
pub(crate) struct ChainArm<'a> {
    patterns: &'a [Pattern],
    guard: Option<&'a Expr>,
    span: TokSpan,
    body: ArmBody<'a>,
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
        // constructor takes the names its payload writes, and the
        // enum it returns fills the rest with a default or `any`.
        let alias_generics = e
            .generics
            .map(|g| strip_bounds(self.text_of(g)))
            .unwrap_or_default();
        let fn_generics = if alias_generics.is_empty() {
            String::new()
        } else {
            super::modules::type_arguments(&alias_generics)
        };

        if self.options.definitions {
            self.enum_type_only(e, &name, &alias_generics);

            return;
        }
        // The check artifact types a generic enum as a plain union of
        // `read` tables, one per variant, that list the methods its
        // impls write. The solver finds no `T` through a metatable
        // alias whose payload names the enum, `Node(Tree<T>, Tree<T>)`,
        // and a metatable alias prints no argument, so `Opt.Some(1)`
        // hovered `Opt`. The unit variant is a tagged table too: a
        // method call on a union with a string in it fails.
        let plain = self.options.check && !alias_generics.is_empty();
        let methods = if plain {
            let derives_clone = e
                .attributes
                .iter()
                .filter(|a| a.name.is_some_and(|n| self.text_of(n) == "derive"))
                .flat_map(|a| a.args.iter())
                .any(|arg| self.text_of(arg.span()) == "Clone");

            self.enum_alias_methods(&name, derives_clone)
        } else {
            String::new()
        };

        // Header line. An attribute line above `enum` keeps its newline.
        let head = self.decl_head(&name);
        self.generate(start, &format!("{head}{name}.__index = {name}"));
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
                let (_, args) = variant_arguments(&alias_generics, &[]);
                let value = if plain {
                    format!("((\"{vname}\" :: any) :: {name}{args})")
                } else if self.options.check {
                    format!("(\"{vname}\" :: {name})")
                } else {
                    format!("\"{vname}\"")
                };
                self.generate(vs, &format!("{name}.{vname} = {value}"));
                types.push(if plain {
                    format!("{{ read tag: \"{vname}\"{methods} }}")
                } else {
                    format!("\"{vname}\"")
                });
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
                // writes on the enum resolves on a payload value. A
                // generic enum lists the methods instead.
                let variant_type = if plain {
                    let fields: Vec<String> =
                        field_types.iter().map(|f| format!("read {f}")).collect();

                    format!(
                        "{{ read tag: \"{vname}\", {}{methods} }}",
                        fields.join(", ")
                    )
                } else {
                    format!(
                        "typeof(setmetatable({{}} :: {{ tag: \"{vname}\", {} }}, {name}))",
                        field_types.join(", ")
                    )
                };
                // The check artifact types a plain constructor as this
                // one variant, not as the whole enum: a mixed enum's
                // union holds strings, and a method call on it would
                // read one. The variant is a subtype, so a `{name}`
                // annotation still takes the value. A generic enum's
                // constructor returns the enum, so `Opt.Some(1)` is an
                // `Opt<number>`.
                let (fn_generics, plist, ret) = if plain {
                    let (generics, args) = variant_arguments(&alias_generics, &field_types);

                    (generics, field_types.join(", "), format!(": {name}{args}"))
                } else if self.options.check {
                    (
                        fn_generics.clone(),
                        field_types.join(", "),
                        format!(": {variant_type}"),
                    )
                } else {
                    (fn_generics.clone(), params.join(", "), String::new())
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

        let export = if e.exported || self.ns_export || self.export_listed_types.contains(&name) {
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
                self.check_std_name(arg.span(), &which);
                // `Eq` and `PartialEq` write the same `__eq`.
                let key = if which == "PartialEq" { "Eq" } else { &which };

                if !derived.insert(key.to_string()) {
                    continue;
                }

                match which.as_str() {
                    "Eq" | "PartialEq" => {
                        let std = self.std();
                        let slots: Vec<String> = (1..=max_arity)
                            .map(|i| format!(" and {std}.deep_eq(a._{i}, b._{i})"))
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

                    // `Debug` is every enum's printer already.
                    "Debug" => {}

                    // A derive that writes nothing must not pass in silence:
                    // a unit variant is a string, and a payload variant a
                    // plain tagged table.
                    other => {
                        let message = match other {
                            "Ord" => "`@derive(Ord)` has no meaning on an enum: a unit variant is a string, which compares by its text; compare the variants' order yourself".to_string(),

                            "Default" => "`@derive(Default)` has no meaning on an enum; name the variant a value starts as where you build it".to_string(),

                            "Serialize" | "Deserialize" => format!(
                                "`@derive({other})` has nothing to write on an enum: a unit variant is already a string, and a payload variant a plain table"
                            ),

                            _ => format!("unknown derive `{other}`"),
                        };
                        self.diagnose(arg.span(), &message);
                    }
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

        // `E.is` is the enum's type test, and a second variant of one
        // name would overwrite the first constructor.
        let mut seen: Vec<&str> = Vec::new();

        for v in &e.variants {
            let variant = self.text_of(v.name);

            if variant == "is" {
                self.diagnose(
                    v.name,
                    "a variant cannot be named `is`; the enum's type test `E.is(v)` takes that name",
                );
            } else if seen.contains(&variant) {
                let message = format!("`{variant}` is already a variant of this enum");
                self.diagnose(v.name, &message);
            }

            seen.push(variant);
        }

        let union = match types.is_empty() {
            true => "never".to_string(),

            false => types.join(" | "),
        };
        // The methods another file's `impl` writes, declared on the
        // class table so that file's `function {name}.m` adds no key.
        let foreign = self.foreign_impl_lines(&name);
        self.generate(
            end_tok.start,
            &format!(
                "function {name}.is(v) return {test} end{printer} {export}type {name}{alias_generics} = {union}{foreign}"
            ),
        );

        if e.exported {
            self.exports.push((name.clone(), name));
        }
    }

    /// An enum in a definitions file: the global type alone. The file
    /// runs nowhere, so no constructor table stands behind the type.
    fn enum_type_only(&mut self, e: &EnumDecl, name: &str, alias_generics: &str) {
        let mut types = Vec::new();

        for v in &e.variants {
            let vname = self.text_of(v.name).to_string();

            types.push(match &v.value {
                Some(value) => format!("typeof({})", self.render_to_string(value)),

                None if v.payload.is_empty() => format!("\"{vname}\""),

                None => {
                    let fields: Vec<String> = v
                        .payload
                        .iter()
                        .enumerate()
                        .map(|(i, t)| format!("_{}: {}", i + 1, self.copy_type_to_string(*t)))
                        .collect();

                    format!("{{ tag: \"{vname}\", {} }}", fields.join(", "))
                }
            });
        }

        if types.is_empty() {
            self.diagnose(e.name, "an `enum` needs at least one variant");
            types.push("never".to_string());
        }

        let start = self.byte_start(e.span);
        self.generate(
            start,
            &format!("export type {name}{alias_generics} = {}", types.join(" | ")),
        );
        self.blank_lines(start, self.byte_end(e.span));
    }

    /// The methods a generic enum's alias lists, `read m: typeof(Opt.m)`
    /// each: the impls of this file, the impls of other files, and a
    /// derived `clone`. `typeof` keeps the method's own generic list;
    /// a spelled `self: Opt<T>` names the alias inside itself, and the
    /// solver then finds no `T` for a call that takes an `Opt<T>`.
    fn enum_alias_methods(&self, name: &str, derives_clone: bool) -> String {
        let mut names: Vec<String> = self
            .type_members
            .get(name)
            .into_iter()
            .flatten()
            .filter(|m| m.kind == "function" && m.shape.starts_with("(self"))
            .map(|m| m.name.clone())
            .collect();

        for e in &self.options.foreign_impls {
            if e.head().0 == name && !e.is_static {
                names.push(e.name.clone());
            }
        }

        if derives_clone {
            names.push("clone".to_string());
        }

        names.sort();
        names.dedup();

        names
            .iter()
            .map(|m| format!(", read {m}: typeof({name}.{m})"))
            .collect()
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

        // `import * as Sh` binds the module, and `Sh.Shape` is a type
        // path Luau reads through it.
        if let Some((head, rest)) = name.split_once('.')
            && self.star_modules.contains(head)
        {
            return format!("{head}.{}", rest.replace('.', "_"));
        }

        self.ns_path_name(name)
            .unwrap_or_else(|| name.replace('.', "_"))
    }

    /// A fresh local for a match scrutinee in the check artifact, cast
    /// to the enum its arms name. `type(x) == "table"` reads a lone
    /// variant type wrong, and a value the code just built has one; read
    /// through the enum it narrows right. The ship artifact keeps the
    /// scrutinee as it is.
    ///
    /// A head with an alias takes no hoist: its locals live inside the
    /// closure `match_expr` writes, see `head_locals`.
    pub(crate) fn scrutinee_local(
        &mut self,
        value: Renderer<'s>,
        ename: &str,
        anchor: u32,
    ) -> String {
        self.bump_temp();
        let name = format!("_v{}", self.temp_next);
        let cast = self.render_side(|d| {
            d.generate(anchor, "(");
            d.r.append(value);
            d.generate(anchor, &format!(") :: {ename}"));
        });
        self.hoists.push(Hoist::Fresh {
            name: name.clone(),
            value: HoistValue::Rendered(cast),
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
            let ename = if self.options.check {
                self.column_enum(arms, col)
            } else {
                None
            };

            match ename {
                Some(e) => {
                    let anchor = self.byte_start(sc.span());
                    let value = self.render_hoisted(|d| d.render_to_side(sc));
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

    /// `local a = x local b = y` for a match head, one local per
    /// scrutinee: the alias the head wrote, or a temp. The check
    /// artifact casts each value to the enum the arms name, the way
    /// `scrutinee_local` does. The caller opens the block that holds
    /// them, so an alias lives and dies with the match.
    fn head_locals(
        &mut self,
        anchor: u32,
        scrutinees: &[Expr],
        aliases: &[Option<TokSpan>],
        arms: &[&[Pattern]],
    ) -> Vec<String> {
        let mut paths = Vec::new();

        for (col, sc) in scrutinees.iter().enumerate() {
            let value = self.render_to_side(sc);
            let cast = self
                .options
                .check
                .then(|| self.column_enum(arms, col))
                .flatten();
            self.generate(anchor, " local ");

            // `match e as n with` names the local `n`. The name copies
            // from the source, so the editor maps it to the alias.
            match aliases.get(col).copied().flatten() {
                Some(alias) => {
                    self.copy_on_line(anchor, alias);
                    paths.push(self.text_of(alias).to_string());
                }

                None => {
                    self.bump_temp();
                    let index = self.temp_next;
                    self.generate(anchor, &format!("_m{index}"));
                    paths.push(format!("_m{index}"));
                }
            }

            self.generate(anchor, " = ");

            match cast {
                Some(e) => {
                    self.generate(anchor, "(");
                    self.r.append(value);
                    self.generate(anchor, &format!(") :: {e}"));
                }

                None => self.r.append(value),
            }
        }

        paths
    }

    /// The names a bind list declares, in order.
    fn bind_names(&self, binds: &[Bind]) -> Vec<String> {
        binds
            .iter()
            .map(|(n, ..)| self.text_of(*n).to_string())
            .collect()
    }

    /// Each bound name and the path it reads, for a rename pass.
    fn bind_map(&self, binds: &[Bind]) -> HashMap<String, String> {
        binds
            .iter()
            .map(|(n, p, _)| (self.text_of(*n).to_string(), p.clone()))
            .collect()
    }

    /// Writes `<keyword> a: T, b = x, y` at `anchor`; `keyword` carries
    /// the space the caller wants in front of it. A name on the
    /// anchor's line copies from the source, so the editor maps it to
    /// the pattern site. A name on another line, as in a let-else whose
    /// declaration follows the `end`, stays generated: a copied byte
    /// keeps its line. Nothing for an empty list.
    fn write_binds(&mut self, anchor: u32, keyword: &str, binds: &[Bind]) {
        if binds.is_empty() {
            return;
        }

        self.generate(anchor, &format!("{keyword} "));

        for (i, (name, _, ty)) in binds.iter().enumerate() {
            if i > 0 {
                self.generate(anchor, ", ");
            }

            self.copy_on_line(anchor, *name);

            if let Some(ty) = ty {
                self.generate(anchor, ": ");
                self.copy_on_line(anchor, *ty);
            }
        }

        let values: Vec<&str> = binds.iter().map(|(_, v, _)| v.as_str()).collect();
        self.generate(anchor, &format!(" = {}", values.join(", ")));
    }

    /// Copies a span when it sits on the anchor's line, and generates
    /// its text otherwise.
    pub(crate) fn copy_on_line(&mut self, anchor: u32, span: TokSpan) {
        if self.line_of(self.byte_start(span)) == self.line_of(anchor) {
            self.copy_span(span);
        } else {
            let text = self.text_of(span);
            self.generate(anchor, text);
        }
    }

    /// Compiles a pattern against an access path.
    pub(crate) fn compile_pattern(&mut self, p: &Pattern, path: &str, out: &mut Compiled) {
        match p {
            Pattern::Wildcard(_) => {}

            Pattern::Bind(name) => {
                let n = self.text_of(*name).to_string();

                match self.unit_variant_of(&n) {
                    // The check artifact reads a generic enum's unit
                    // variant as a tagged table; the tag test narrows
                    // the union the way a payload test does.
                    Some(e) if self.options.check && self.castable_enum(&e).is_none() => {
                        out.tests.push(format!(
                            "type({path}) == \"table\" and {path}.tag == \"{n}\""
                        ));
                    }

                    Some(_) => out.tests.push(format!("{path} == \"{n}\"")),

                    None => out.binds.push((*name, path.to_string(), None)),
                }
            }

            Pattern::Literal(e) => {
                let lit = self.render_to_string(e);
                out.tests.push(format!("{path} == {lit}"));
            }

            Pattern::Path(span) => {
                // `Kind.A` inside the namespace that declares `Kind`
                // renders under the namespace's name, `Geo_Kind.A`.
                let written: String = self.text_of(*span).split_whitespace().collect();
                let text = match written
                    .split_once('.')
                    .and_then(|(head, v)| Some((self.ns_member_name(head)?, v)))
                {
                    Some((e, v)) => format!("{e}.{v}"),

                    None => written.clone(),
                };
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

                        None => out.binds.push((*field, sub, None)),
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
                    let std = self.std();
                    out.binds.push((
                        *r,
                        format!("{std}.Array.slice({path}, {})", items.len() + 1),
                        None,
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

                if self.bind_names(&ca.binds) != self.bind_names(&cb.binds) {
                    self.diagnose(
                        *span,
                        "both sides of an `or` pattern must bind the same names",
                    );
                }

                for (n, pa, _) in &ca.binds {
                    let pb = cb
                        .binds
                        .iter()
                        .find(|(m, ..)| self.text_of(*m) == self.text_of(*n))
                        .map(|(_, p, _)| p.clone())
                        .unwrap_or_else(|| pa.clone());
                    // The checker refines each side by its own tag and
                    // cannot pick one across the `or`; the check artifact
                    // reads the payload untyped.
                    let (pa, pb) = (self.cast_root(pa), self.cast_root(&pb));
                    out.binds
                        .push((*n, format!("(if {ta} then {pa} else {pb})"), None));
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
            let map = self.bind_map(&c.binds);
            self.renames.push(map);
            // The guard runs only when the patterns match.
            let text = self.render_lazy(g);
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
        let mut paths: Vec<TokSpan> = Vec::new();

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

                    Pattern::Path(span) => paths.push(*span),

                    // `case Lobby then` writes a unit variant as a bare
                    // name, which the parser reads as a binding. A
                    // capital says the author meant a variant, so a name
                    // no enum owns is a typo, not a catch-all.
                    Pattern::Bind(name) if is_variant_name(self.text_of(*name)) => {
                        flat.push((*name, 0));
                    }

                    // `case target then` with `target` in scope binds a
                    // new name over it, and the arm takes every value.
                    Pattern::Bind(name) if self.is_local(self.text_of(*name)) => {
                        let text = self.text_of(*name).to_string();
                        let tok = self.toks[name.start as usize];
                        self.lints.push(Lint {
                            name: "pattern_shadows_local",
                            start: tok.start,
                            end: tok.end,
                            message: format!(
                                "`{text}` is a local in scope, and this pattern binds a new `{text}` that takes every value; to compare, write `case v where v == {text}`, and to bind, pick another name"
                            ),
                            fix: None,
                        });
                    }

                    _ => {}
                }
            }
        }

        // `case R.Aa`: a dotted path into a declared enum that names no
        // variant compares with a nil field and never holds.
        for span in paths {
            let text = self.text_of(span).to_string();

            if let Some((e, v)) = self.enum_of_path(&text)
                && let Some(vs) = self.enums.get(&e)
                && !vs.iter().any(|(n, _)| *n == v)
            {
                let names: Vec<&str> = vs.iter().map(|(n, _)| n.as_str()).collect();
                let message = format!(
                    "`{}` has no variant `{v}`; its variants are {}",
                    self.display_name(&e),
                    list_names(&names)
                );
                self.diagnose(span, &message);
                reported = true;
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
                    } else if binds == 0 && (self.is_local(&vname) || is_screaming(&vname)) {
                        // `case MAX then` reads as a comparison and binds a
                        // new `MAX` that takes every value, so a `default`
                        // below it never runs and nothing says so.
                        let message = match self.is_local(&vname) {
                            true => format!(
                                "`{vname}` names a value, and a bare name in a pattern binds a new one, so this arm takes every value; compare in a guard: `case n where n == {vname}`"
                            ),

                            false => format!(
                                "`{vname}` is written as a constant's name, and a bare name in a pattern binds a new one that takes every value; bind a lowercase name, or compare in a guard: `case n where n == {vname}`"
                            ),
                        };
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

    /// Reports a guard written with `and`; the fix writes `where`.
    fn guard_lints<'e>(&mut self, guards: impl Iterator<Item = &'e Expr>) {
        for g in guards {
            let Some(k) = (g.span().start as usize).checked_sub(1) else {
                continue;
            };
            let tok = self.toks[k];

            if self.text_of(TokSpan::new(k, k + 1)) == "and" {
                self.lints.push(Lint {
                    name: "match_guard_and",
                    start: tok.start,
                    end: tok.end,
                    message:
                        "a match guard takes `where`, as a `for` filter does: `case n where n > 5`"
                            .to_string(),
                    fix: Some(crate::lint::Fix::new(self.src, tok.start, tok.end, "where")),
                });
            }
        }
    }

    /// Reports a head alias that no arm, guard, or `default` reads, as
    /// `unused_variable`. The fix drops the ` as name` from the head.
    fn alias_lints(&mut self, span: TokSpan, aliases: &[Option<TokSpan>]) {
        for alias in aliases.iter().flatten() {
            let name = self.text_of(*alias);

            let read =
                (alias.end as usize..span.end as usize).any(|k| self.reads_name(k, name, false));

            if name.starts_with('_') || read {
                continue;
            }

            // The scrutinee ends right before `as`, so the fix takes
            // the space, the keyword, and the name with it.
            let from = self.toks[alias.start as usize - 2].end;
            let to = self.byte_end(*alias);
            self.lints.push(Lint {
                name: "unused_variable",
                start: self.byte_start(*alias),
                end: to,
                message: format!(
                    "`{name}` is never read; prefix it with `_` or drop the `as {name}`"
                ),
                fix: Some(crate::lint::Fix::new(self.src, from, to, "")),
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

        // The reader knows `Geo_Kind` as `Geo.Kind`.
        let e = self.display_name(&e);

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
                        continue;
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
                        continue;
                    };

                    match self.enums.get(&e) {
                        Some(vs) if vs.iter().any(|(n, c)| *n == v && *c == 0) => {
                            enum_name.get_or_insert(e);
                            rows.push((v, Vec::new()));
                        }

                        _ => continue,
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
                        continue;
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
                Pattern::Struct { .. } if self.struct_pattern_covers(p) => return true,

                // A refutable row covers part of the value, so the scan
                // reads on: a later `case _` or binding still covers.
                _ => {}
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
        let arms: Vec<ChainArm<'_>> = m
            .arms
            .iter()
            .map(|a| ChainArm {
                patterns: &a.patterns,
                guard: a.guard.as_ref(),
                span: a.span,
                body: ArmBody::Block(&a.block),
            })
            .collect();
        let default = m.default.as_ref().map(ArmBody::Block);

        self.match_chain(m.span, &m.scrutinees, &m.aliases, &arms, default, None, "");
    }

    /// `local s = match ...` whose arms run statements: the statement
    /// chain, each arm ending by writing its value after `sink`, `s = `
    /// or `return `. `lead` goes before the chain's `do`.
    pub(crate) fn match_hoisted(&mut self, m: &MatchExpr, sink: &str, lead: &str) {
        let arms: Vec<ChainArm<'_>> = m
            .arms
            .iter()
            .map(|a| ChainArm {
                patterns: &a.patterns,
                guard: a.guard.as_ref(),
                span: a.span,
                body: ArmBody::Value(&a.value),
            })
            .collect();
        let default = m.default.as_deref().map(ArmBody::Value);

        self.match_chain(
            m.span,
            &m.scrutinees,
            &m.aliases,
            &arms,
            default,
            Some(sink),
            lead,
        );
    }

    /// One arm's body in the chain; returns the byte the next copy starts
    /// at. A value goes after the sink; a block of an arm that gives a
    /// value writes its last line into the sink.
    fn chain_body(&mut self, body: &ArmBody<'_>, cursor: u32, sink: Option<&str>) -> u32 {
        let block = match body {
            ArmBody::Block(b) => Some(*b),

            ArmBody::Value(Expr::Block { block, .. }) => Some(block),

            ArmBody::Value(_) => None,
        };

        match (block, body) {
            (Some(b), _) => {
                let body_start = self.block_start_or(b, cursor);
                self.copy(cursor, body_start);
                let saved = std::mem::replace(&mut self.value_sink, sink.map(str::to_string));
                self.block(b);
                self.value_sink = saved;

                self.block_end_or(b, body_start)
            }

            (None, ArmBody::Value(e)) => {
                let at = self.byte_start(e.span());
                self.copy(cursor, at);
                self.generate(at, sink.unwrap_or("return "));
                self.expr(e);

                self.byte_end(e.span())
            }

            (None, ArmBody::Block(_)) => cursor,
        }
    }

    /// The statement match: `do local _m1 = x` and an if-chain on it,
    /// one arm per line group.
    #[allow(clippy::too_many_arguments)]
    fn match_chain(
        &mut self,
        span: TokSpan,
        scrutinees: &[Expr],
        aliases: &[Option<TokSpan>],
        arms: &[ChainArm<'_>],
        default: Option<ArmBody<'_>>,
        sink: Option<&str>,
        lead: &str,
    ) {
        let start = self.byte_start(span);
        let with_end =
            self.toks[arms.first().map(|a| a.span.start).unwrap_or(span.end - 1) as usize - 1].end;

        let pats: Vec<&[Pattern]> = arms.iter().map(|a| a.patterns).collect();

        // `match a, b with` becomes `do local _1 = a local _2 = b`. Each
        // scrutinee keeps its chunks, so the editor maps its names.
        self.generate(start, &format!("{lead}do"));
        let paths = self.head_locals(start, scrutinees, aliases, &pats);
        self.alias_lints(span, aliases);
        self.guard_lints(arms.iter().filter_map(|a| a.guard));

        let mut cursor = with_end;
        let guards: Vec<bool> = arms.iter().map(|a| a.guard.is_some()).collect();

        let exhaustive = self.match_is_exhaustive(&pats, &guards);
        // A rejected pattern makes the arm list unreliable, so the
        // exhaustiveness message would name the wrong variant.
        let bad_arm = self.check_variant_patterns(&pats);

        if default.is_none() && !exhaustive && !bad_arm {
            let msg = self.not_exhaustive_message(&pats);
            self.diagnose(span, &msg);
        }

        // `case n` or `case _` last, after another arm, writes `else`: an
        // `elseif true` reads to the checker as a path that falls through.
        let catch_all = arms.len() > 1
            && default.is_none()
            && arms.last().is_some_and(|a| {
                a.guard.is_none() && matches!(a.patterns, [p] if self.irrefutable(p))
            });

        if let Some(d) = &default {
            let arms_end = arms.last().map_or(span.start, |a| a.span.end);
            let empty = matches!(d, ArmBody::Block(b) if b.stmts.is_empty());
            self.default_lints(span, arms_end, exhaustive, empty);
        }

        for (i, arm) in arms.iter().enumerate() {
            let arm_start = self.byte_start(arm.span);
            self.copy(cursor, arm_start);
            let (test, c) = self.arm_test(arm.patterns, &paths, arm.guard);
            let keyword = if i == 0 { "if" } else { "elseif" };
            let text = if catch_all && i + 1 == arms.len() {
                "else".to_string()
            } else {
                format!("{keyword} {test} then")
            };

            // The arm head runs to `then`; the body follows.
            let then_tok = self.find_tok_after(
                arm.patterns
                    .last()
                    .map(|p| p.span().end)
                    .unwrap_or(arm.span.start),
                "then",
            );
            let then_end = self.toks[then_tok as usize].end;
            self.generate(arm_start, &text);
            self.write_binds(arm_start, " local", &c.binds);
            cursor = then_end;

            self.scopes.push(HashSet::new());

            for name in self.bind_names(&c.binds) {
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(name);
                }
            }

            cursor = self.chain_body(&arm.body, cursor, sink);
            self.scopes.pop();
        }

        if let Some(d) = &default {
            // The `default` token sits before the body.
            let default_tok = match d {
                ArmBody::Block(b) if b.span.is_empty() => self.toks[span.end as usize - 2],

                ArmBody::Block(b) => self.toks[b.span.start as usize - 1],

                ArmBody::Value(e) => self.toks[e.span().start as usize - 1],
            };
            self.copy(cursor, default_tok.start);
            self.generate(default_tok.start, "else");
            cursor = default_tok.end;
            cursor = self.chain_body(d, cursor, sink);
        }

        let end_tok = self.toks[span.end as usize - 1];
        self.copy(cursor, end_tok.start);

        // Every variant has an arm, so the chain has no `else` and the
        // checker reads a path that falls through. The raise closes it,
        // and an exhaustive match whose arms all return counts as one. A
        // last arm that takes every value is the `else` already.
        if default.is_none() && exhaustive && !arms.is_empty() && !catch_all {
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
        // A Luau if-expression holds no local, so a head with an alias
        // becomes a closure: the name lives and dies inside it, the way
        // the `do` block of the statement form holds one.
        let aliased = m.aliases.iter().any(Option::is_some);
        let mut paths = if aliased {
            Vec::new()
        } else {
            self.scrutinee_paths(&m.scrutinees, &pats)
        };
        self.alias_lints(m.span, &m.aliases);
        self.guard_lints(m.arms.iter().filter_map(|a| a.guard.as_ref()));
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
        // The closure an alias needs holds no `...` of its own, so the
        // function's arguments pass through it, as `expr_in_place` does.
        let vararg = |e: &Expr| super::any_part(e, &|x| matches!(x, Expr::Vararg(_)));
        let forwards = aliased
            && (m.scrutinees.iter().any(vararg)
                || m.arms
                    .iter()
                    .any(|a| vararg(&a.value) || a.guard.as_ref().is_some_and(vararg))
                || m.default.as_deref().is_some_and(vararg));

        if aliased {
            self.generate(
                start,
                if forwards {
                    "(function(...)"
                } else {
                    "(function()"
                },
            );
            paths = self.head_locals(start, &m.scrutinees, &m.aliases, &pats);
            self.generate(start, " return (");
        } else {
            self.generate(start, "(");
        }

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
            let map = self.bind_map(&c.binds);
            self.renames.push(map);
            self.expr_lazy(true, &arm.value);
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
            self.expr_lazy(true, d);
            cursor = self.byte_end(d.span());
        } else if !exhaustive {
            self.generate(cursor, " else nil");
        }

        let end_tok = self.toks[m.span.end as usize - 1];
        self.copy(cursor, end_tok.start);
        let close = match (aliased, forwards) {
            (true, true) => ") end)(...)",

            (true, false) => ") end)()",

            _ => ")",
        };
        self.generate(end_tok.start, close);
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

        match &p.else_block {
            None => {
                let pat = self.text_of(p.pattern.span()).to_string();
                self.hoist_stmt(
                    format!(
                        "if not ({test}) then error({} .. tostring(if type({temp}) == \"table\" then {temp}.tag else {temp})) end",
                        luau_string(&format!("pattern `{pat}` did not match, got "))
                    ),
                    anchor,
                    false,
                );
                self.write_binds(anchor, &keyword, &c.binds);
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

                self.write_binds(end_tok.end, &format!(" {keyword}"), &c.binds);
            }
        }
    }

    // --- conditions with bindings -------------------------------------------

    /// The declarations and test for a `Cond::Local`, for a block context
    /// where temps and bindings can be locals. Each declaration keeps
    /// the chunks of its value, so a name in the value maps to its own
    /// source text and the editor finds references through it.
    pub(crate) fn cond_local_parts(
        &mut self,
        cond: &Cond,
        anchor: u32,
    ) -> (Vec<Renderer<'s>>, String, Vec<Bind>) {
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
        let mut binds: Vec<Bind> = Vec::new();
        let mut prior: Vec<String> = Vec::new();

        for b in bindings {
            // An earlier name in this condition is its temp by now.
            let map = self.bind_map(&binds);
            self.renames.push(map);
            let value = self.render_to_side(&b.value);
            self.renames.pop();
            let ty =
                b.ty.map(|t| format!(": {}", self.text_of(t)))
                    .unwrap_or_default();

            // The name each declaration binds, its type, and the
            // source span of the name when the declaration is the
            // reader's own binding.
            let (head, ty, span) = match &b.pattern {
                // The branch declares the name from a temp the test refined,
                // so the name is `T`, not `T?`, in the branch and in any
                // closure there. The annotation goes on the name there
                // too: the temp holds the `T?` the value has, and the
                // reader's `v: T` is the narrowed one. A negated
                // condition keeps the name, since it must stay in scope
                // after a guard clause.
                Pattern::Bind(n) if self.unit_variant_of(self.text_of(*n)).is_none() => {
                    let name = self.text_of(*n).to_string();

                    if *negated {
                        tests.push(name.clone());
                        prior.push(name.clone());
                        // The name holds the `T?` the value has until
                        // the guard returns; the checker narrows it to
                        // `T` after that. A wrong `T` still reports.
                        let ty = if ty.is_empty() || ty.ends_with('?') {
                            ty
                        } else {
                            format!("{ty}?")
                        };

                        (name, ty, Some(*n))
                    } else {
                        self.bump_temp();
                        let temp = format!("_c{}", self.temp_next);
                        tests.push(temp.clone());
                        prior.push(temp.clone());
                        binds.push((*n, temp.clone(), b.ty));

                        (temp, String::new(), None)
                    }
                }

                pat => {
                    self.bump_temp();
                    let temp = format!("_c{}", self.temp_next);
                    let mut c = Compiled::default();
                    self.compile_pattern(pat, &temp, &mut c);
                    let test = join_tests(&c.tests);
                    tests.push(format!("({test})"));
                    prior.push(format!("({test})"));
                    binds.extend(c.binds);

                    (temp, String::new(), None)
                }
            };

            // A later binding runs only when the earlier ones are truthy.
            // The test this binding adds is the last one in `prior`.
            let earlier = &prior[..prior.len() - 1];
            let decl = self.render_side(|d| {
                d.generate(anchor, "local ");

                // The negated form declares the reader's name, so the
                // name copies from the source and the editor maps it
                // back to the condition.
                match span {
                    Some(n) => d.copy_on_line(anchor, n),

                    None => d.generate(anchor, &head),
                }

                d.generate(anchor, &format!("{ty} = "));

                if !earlier.is_empty() {
                    d.generate(anchor, &format!("if {} then ", earlier.join(" and ")));
                }

                d.r.append(value);

                if !earlier.is_empty() {
                    d.generate(anchor, " else nil");
                }
            });
            decls.push(decl);
        }

        let mut test = tests.join(" and ");

        if let Some(f) = filter {
            let map = self.bind_map(&binds);
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

    /// Writes the declarations of a condition, one space apart.
    fn append_decls(&mut self, anchor: u32, decls: Vec<Renderer<'s>>) {
        for (i, decl) in decls.into_iter().enumerate() {
            if i > 0 {
                self.generate(anchor, " ");
            }

            self.r.append(decl);
        }
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
                    let (decls, test, binds) = self.cond_local_parts(cond, kw_tok.start);
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

                    self.generate(kw_tok.start, lead);
                    self.append_decls(kw_tok.start, decls);
                    self.generate(kw_tok.start, &format!(" if {test} then"));

                    if !matches!(cond, Cond::Local { negated: true, .. }) {
                        self.write_binds(kw_tok.start, " local", &binds);
                    }
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
        let (decls, test, binds) = self.cond_local_parts(&w.cond, start);
        let do_tok = self.find_tok_after(w.cond.span().end, "do");
        let do_end = self.toks[do_tok as usize].end;
        self.generate(start, "while true do ");
        self.append_decls(start, decls);
        self.generate(start, &format!(" if not ({test}) then break end"));
        self.write_binds(start, " local", &binds);
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
                // Past the first condition, every part runs on some paths
                // only, and so does every value.
                Cond::Expr(e) => {
                    let c = if idx == 0 {
                        self.render_to_string(e)
                    } else {
                        self.render_lazy(e)
                    };
                    let v = self.render_lazy(value);
                    text.push_str(&format!("{keyword} {c} then {v} "));
                }

                Cond::Local { .. } => {
                    let (decls, test, binds) = self.cond_local_parts(cond, anchor);

                    for d in decls {
                        // `local x = e` hoists as a temp-like declaration.
                        let d = d.finish().0;
                        let d = d.strip_prefix("local ").unwrap_or(&d).to_string();
                        let (name, value) = d
                            .split_once(" = ")
                            .map(|(a, b)| (a.to_string(), b.to_string()))
                            .unwrap_or((d.clone(), "nil".to_string()));
                        let name = name.split(':').next().unwrap_or(&name).trim().to_string();
                        self.hoists.push(Hoist::Stmt {
                            text: format!("local {name} = {value}"),
                            anchor,
                            exits: false,
                        });
                    }

                    let map = self.bind_map(&binds);
                    self.renames.push(map);
                    let v = self.render_lazy(value);
                    self.renames.pop();
                    text.push_str(&format!("{keyword} {test} then {v} "));
                }
            }
        }

        let e = self.render_lazy(else_value);
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

    /// The unused-name lints of a compile; a `print` in a fixture draws
    /// its own lint, which these tests are not about.
    fn unused(lints: &[crate::Lint]) -> Vec<crate::Lint> {
        lints
            .iter()
            .filter(|l| l.name == "unused_variable")
            .cloned()
            .collect()
    }

    /// `match e as name with` names the scrutinee's local, and every
    /// arm tests that name. The head stays on its own line.
    #[test]
    fn a_match_alias_names_the_scrutinee_local() {
        let src = "enum State as\n    Loading\n    Ready(number)\nend\nlocal s = State.Loading\nmatch s as state with\n    case Loading then print(state)\n    case Ready(n) then print(n, state)\nend\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("do local state = s\n"), "{}", out.ship);
        assert!(
            out.ship.contains("if state == \"Loading\" then"),
            "{}",
            out.ship
        );
        assert!(unused(&out.lints).is_empty(), "{:?}", out.lints);
        assert_eq!(
            src.lines().count(),
            out.ship.lines().count(),
            "the emit keeps the source lines:\n{}",
            out.ship
        );
    }

    /// Two scrutinees take two locals, and the check artifact keeps the
    /// cast that narrows each one.
    #[test]
    fn two_aliases_name_two_locals() {
        let src = "enum State as\n    Loading\n    Ready(number)\nend\nlocal a = State.Loading\nlocal b = State.Loading\nmatch a as left, b as right with\n    case Loading, Loading then print(left, right)\n    default print(left, right)\nend\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship.contains("do local left = a local right = b\n"),
            "{}",
            out.ship
        );
        assert!(
            out.check
                .contains("do local left = (a) :: State local right = (b) :: State\n"),
            "{}",
            out.check
        );
    }

    /// The expression form holds the alias in a closure of its own, so
    /// a guard reads it and nothing after the `end` does.
    #[test]
    fn a_match_expression_alias_takes_a_local() {
        let src = "enum State as\n    Loading\n    Ready(number)\nend\nlocal s = State.Loading\nlocal v = match s as st with\n    case Loading then 0\n    case Ready(n) and n > #tostring(st) then n\n    default 1\nend\nprint(v)\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.ship
                .contains("local v = (function() local st = s return ("),
            "{}",
            out.ship
        );
        assert!(out.ship.contains(") end)()"), "{}", out.ship);
        assert!(out.ship.contains("#tostring(st)"), "{}", out.ship);
        // The guard reads the alias, so nothing calls it unused.
        assert!(unused(&out.lints).is_empty(), "{:?}", out.lints);

        // The closure holds the name, so a condition that reads its
        // operands again each pass needs no hoist for it.
        let loop_src = "enum State as\n    Loading\n    Ready(number)\nend\nlocal s = State.Loading\nwhile match s as st with\n    case Loading then #tostring(st) > 0\n    case Ready(n) then n > 0\nend do\n    print(1)\nend\n";
        let loops = crate::compile(loop_src).unwrap();

        assert!(loops.diagnostics.is_empty(), "{:?}", loops.diagnostics);
        assert!(
            loops
                .ship
                .contains("while (function() local st = s return ("),
            "{}",
            loops.ship
        );
        assert_eq!(
            src.lines().count(),
            out.ship.lines().count(),
            "the emit keeps the source lines:\n{}",
            out.ship
        );
    }

    /// An alias no arm reads is the ordinary unused-name lint, and its
    /// fix drops the ` as name` from the head.
    #[test]
    fn an_unused_alias_reports_with_a_fix() {
        let src = "enum State as\n    Loading\n    Ready(number)\nend\nlocal s = State.Loading\nmatch s as state with\n    case Loading then print(1)\n    case Ready(n) then print(n)\nend\n";
        let out = crate::compile(src).unwrap();
        let found = unused(&out.lints);
        let [lint] = found.as_slice() else {
            panic!("{:?}", out.lints)
        };

        assert_eq!(
            lint.message,
            "`state` is never read; prefix it with `_` or drop the `as state`"
        );
        assert_eq!(&src[lint.start as usize..lint.end as usize], "state");
        let (fixed, n) = crate::lint::apply_fixes(src, &found);
        assert_eq!(n, 1);
        assert!(fixed.contains("match s with\n"), "{fixed}");
        // An alias the arms read reports nothing, and `_` silences one.
        let read = src.replace("print(1)", "print(state)");
        assert!(unused(&crate::compile(&read).unwrap().lints).is_empty());
        let silenced = src.replace("as state", "as _state");
        assert!(unused(&crate::compile(&silenced).unwrap().lints).is_empty());
    }

    /// The expression form reports an unread alias the same way, and
    /// the source its fix writes still compiles and reports nothing.
    #[test]
    fn the_fix_for_an_unused_alias_still_compiles() {
        let src = "enum State as\n    Loading\n    Ready(number)\nend\nlocal s = State.Loading\nlocal v = match s as st with\n    case Loading then 0\n    case Ready(n) then n\nend\nprint(v)\n";
        let out = crate::compile(src).unwrap();
        let found = unused(&out.lints);
        let [lint] = found.as_slice() else {
            panic!("{:?}", out.lints)
        };

        assert_eq!(&src[lint.start as usize..lint.end as usize], "st");
        let (fixed, n) = crate::lint::apply_fixes(src, &found);

        assert_eq!(n, 1);
        assert!(fixed.contains("local v = match s with\n"), "{fixed}");
        let after = crate::compile(&fixed).unwrap();

        assert!(after.diagnostics.is_empty(), "{:?}", after.diagnostics);
        assert!(unused(&after.lints).is_empty(), "{:?}", after.lints);
    }

    /// `enum Opt<T>`: a constructor returns the enum, so `Opt.Some(1)`
    /// is an `Opt<number>`; a unit variant is a tagged table that casts
    /// to the enum of `any`, so a method call on the union types. The
    /// ship artifact keeps the tagged table and the string.
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
            out.check
                .contains("function Opt.Some<T>(_1: T): Opt<T> return"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("Opt.Nil = ((\"Nil\" :: any) :: Opt<any>)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains(
                "export type Opt<T> = { read tag: \"Some\", read _1: T } | { read tag: \"Nil\" }"
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

    /// A default belongs to the alias alone. A constructor names the
    /// parameters its payload writes, and the enum it returns fills
    /// the rest with the default, or `any`.
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
            out.check
                .contains("function Either.Left<L>(_1: L): Either<L, string>"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("function Either.Right<R>(_1: R): Either<any, R>"),
            "{}",
            out.check
        );
    }

    /// `impl Opt<T>` in another file: the declaring file's check
    /// artifact declares the method on the class table with the impl's
    /// own generic list, so the impl's `function Opt.map` adds no key
    /// and a call on any `Opt<T>` finds it. A `case Nil` arm tests the
    /// tag, since the unit variant is a table there.
    #[test]
    fn a_cross_file_impl_of_a_generic_enum_declares_its_methods() {
        let src = "export enum Opt<T> as\n    Some(T)\n    Nil\nend\nlocal function f(o: Opt<number>): number\n    return match o with\n        case Nil then 0\n        case Some(v) then v\n    end\nend\nprint(f)\n";
        let options = EmitOptions {
            foreign_impls: vec![crate::extensions::Extension {
                target: "Opt<T>".to_string(),
                name: "map".to_string(),
                is_static: false,
                params: "f: (T) -> T".to_string(),
                ret: Some("Opt<T>".to_string()),
            }],
            ..EmitOptions::default()
        };
        let out = crate::compile_with(src, &options).expect("compiles");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("Opt.map = (nil :: any) :: <T>(self: Opt<T>, f: (T) -> T) -> Opt<T>"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("if type(o) == \"table\" and o.tag == \"Nil\" then 0"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains(
                "export type Opt<T> = { read tag: \"Some\", read _1: T, read map: typeof(Opt.map) } | { read tag: \"Nil\", read map: typeof(Opt.map) }"
            ),
            "{}",
            out.check
        );
        assert!(!out.ship.contains("Opt.map"), "{}", out.ship);
        assert!(out.ship.contains("if o == \"Nil\" then 0"), "{}", out.ship);
    }

    /// `Node(Tree<T>, Tree<T>)`: a payload that names the enum. The
    /// solver finds no `T` for `Tree.Node(l, r)` through a metatable
    /// alias, so a generic enum is a plain union of `read` tables, and
    /// the methods of its impl and a derived `clone` sit in each
    /// member as `typeof(Tree.m)`, which keeps the method's own `<T>`.
    #[test]
    fn a_recursive_generic_payload_is_a_plain_union() {
        let src = "@derive(Clone)\nexport enum Tree<T> as\n    Leaf(T)\n    Node(Tree<T>, Tree<T>)\nend\n\nimpl Tree<T> as\n    function depth(self): number\n        return 1\n    end\nend\n\nfunction sumTree(t: Tree<number>): number\n    match t with\n        case Leaf(v) then return v\n        case Node(l, r) then return sumTree(l) + sumTree(r)\n    end\nend\nprint(sumTree(Tree.Leaf(1)))\n";
        let out = crate::compile(src).unwrap();

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("function Tree.Node<T>(_1: Tree<T>, _2: Tree<T>): Tree<T> return"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains(
                "export type Tree<T> = { read tag: \"Leaf\", read _1: T, read clone: typeof(Tree.clone), read depth: typeof(Tree.depth) } | { read tag: \"Node\", read _1: Tree<T>, read _2: Tree<T>, read clone: typeof(Tree.clone), read depth: typeof(Tree.depth) }"
            ),
            "{}",
            out.check
        );
        assert!(!out.check.contains("setmetatable({} ::"), "{}", out.check);
        assert!(
            out.check
                .contains("local l, r = _m1._1, _m1._2 return sumTree(l) + sumTree(r)"),
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

    /// `import * as Sh` binds the module, so the cast of a match over
    /// `Sh.Shape` reads the type through it. `Sh_Shape` names nothing.
    #[test]
    fn a_star_imported_enum_casts_through_the_module() {
        let options = EmitOptions {
            import_enums: vec![(
                "Sh.Shape".to_string(),
                vec![("Circle".to_string(), 1), ("Dot".to_string(), 0)],
            )],
            import_types: vec![("./shapes".to_string(), vec!["Shape".to_string()])],
            check: true,
            ..EmitOptions::default()
        };
        let src = "import * as Sh from \"./shapes\"\nlocal function f(s: Sh.Shape): number\n    return match s with\n        case Sh.Shape.Circle(r) then r\n        case Sh.Shape.Dot then 0\n    end\nend\nprint(f)\n";
        let out = crate::compile_with(src, &options).expect("compiles");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.check.contains(":: Sh.Shape"), "{}", out.check);
        assert!(!out.check.contains("Sh_Shape"), "{}", out.check);
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

    /// A literal arm covers part of the value, so a `case _` or a
    /// binding after it still makes the match exhaustive.
    #[test]
    fn a_catch_all_after_a_literal_covers() {
        for arm in ["case _ then print(2)", "case n then print(n)"] {
            let src = format!(
                "local function f(x: number)\n    match x with\n        case 1 then print(1)\n        {arm}\n    end\nend\nf(1)\n"
            );
            assert!(messages(&src).is_empty(), "{arm}: {:?}", messages(&src));
        }
        let src = "local function f(x: number)\n    match x with\n        case 1 then print(1)\n        case 2 then print(2)\n    end\nend\nf(1)\n";
        assert_eq!(
            messages(src),
            ["this match is not exhaustive; add a `default` arm"]
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

    /// The value of an `if local`, a match scrutinee, a `try` operand, and
    /// an `await` operand keep their chunks in the output, so the editor
    /// maps a name in them back to its own text, and references and
    /// rename find that site. A whole-line generate mapped every byte
    /// to the statement.
    #[test]
    fn a_copied_operand_maps_back_to_its_own_text() {
        let cases = [
            (
                "local items = {}\nlocal function f(key: string)\n    if local v = items:get(key) then\n        print(v)\n    end\nend\n",
                "items:get(key)",
            ),
            (
                "local items = {}\nlocal function f(key: string)\n    if not local v = items:get(key) then\n        return\n    end\n    print(v)\nend\n",
                "items:get(key)",
            ),
            (
                "local items = {}\nwhile local job = items:pop() do\n    print(job)\nend\n",
                "items:pop()",
            ),
            (
                "enum Shape as\n    Circle(number)\n    Empty\nend\nlocal shapes = { Shape.Empty }\nmatch shapes[1] with\n    case Circle(r) then print(r)\n    case Empty then print(0)\nend\n",
                "shapes[1]",
            ),
            (
                "enum Shape as\n    Circle(number)\n    Empty\nend\nlocal shapes = { Shape.Empty }\nlocal n = match shapes[1] with\n    case Circle(r) then r\n    case Empty then 0\nend\nprint(n)\n",
                "shapes[1]",
            ),
            (
                "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\nlocal function run(): Result<number, string>\n    local v = try parse(\"1\")\n    return Ok(v)\nend\nprint(run)\n",
                "parse(\"1\")",
            ),
            (
                "local function load(): Future<number>\n    return Future.resolved(1)\nend\nasync function run()\n    local v = await load()\n    print(v)\nend\n",
                "load()",
            ),
        ];

        for (src, needle) in cases {
            let options = EmitOptions {
                check: true,
                ..EmitOptions::default()
            };
            let out = crate::compile_with(src, &options).unwrap();
            assert!(
                out.diagnostics.is_empty(),
                "{needle}: {:?}",
                out.diagnostics
            );
            let at = out
                .check
                .find(needle)
                .unwrap_or_else(|| panic!("{needle}: {}", out.check)) as u32;
            let want = src.find(needle).unwrap() as u32;

            for i in 0..u32::try_from(needle.len()).unwrap() {
                assert_eq!(out.map.to_source(at + i), want + i, "{needle}: offset {i}");
            }
        }
    }

    /// The name a condition binds, `v` in `if local v = f() then`, is
    /// copied from the source into the `local v = _c1` the lowering
    /// writes, so the editor maps the declaration back to the pattern
    /// site and references on `v` find it. A destructuring pattern and
    /// a match arm copy each name the same way.
    #[test]
    fn a_bound_name_maps_back_to_its_pattern_site() {
        let cases = [
            (
                "local function f(): number?\n    return 1\nend\nif local v = f() then\n    print(v)\nend\n",
                "local v = _c1",
                "v = f()",
            ),
            (
                "local function f(): number?\n    return 1\nend\nwhile local w = f() do\n    print(w)\nend\n",
                "local w = _c1",
                "w = f()",
            ),
            (
                "local function pt(): { x: number, y: number }?\n    return { x = 1, y = 2 }\nend\nif local { x, y } = pt() then\n    print(x + y)\nend\n",
                "local x, y = _c1.x, _c1.y",
                "x, y } = pt()",
            ),
            (
                "enum Shape as\n    Circle(number)\n    Empty\nend\nlocal s = Shape.Empty\nmatch s with\n    case Circle(r) then print(r)\n    case Empty then print(0)\nend\n",
                "local r = _m1._1",
                "r) then",
            ),
        ];

        for (src, generated, at_pattern) in cases {
            let options = EmitOptions {
                check: true,
                ..EmitOptions::default()
            };
            let out = crate::compile_with(src, &options).unwrap();
            assert!(out.diagnostics.is_empty(), "{src}: {:?}", out.diagnostics);
            let at =
                out.check
                    .find(generated)
                    .unwrap_or_else(|| panic!("{generated}: {}", out.check)) as u32;
            let want = src.find(at_pattern).unwrap() as u32;
            let name_at = at + "local ".len() as u32;

            assert_eq!(out.map.to_source(name_at), want, "{generated}");
            assert!(!out.map.is_generated(name_at), "{generated}");
        }
    }
}
