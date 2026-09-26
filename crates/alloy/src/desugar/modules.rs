//! Import, export, and module-return lowering.

use alloy_syntax::ast::{
    Block, DefaultExport, ExportList, Expr, Import, ImportKind, ImportSpec, Local, Stmt, TokSpan,
};

use super::*;

/// The type parameters the runtime declares a std type with, `<K, V>`,
/// or an empty string for a type with none. `None` for a name the
/// runtime declares no type for.
fn runtime_type_params(name: &str) -> Option<String> {
    crate::RUNTIME.lines().find_map(|line| {
        let rest = line.strip_prefix("export type ")?.strip_prefix(name)?;

        if rest.starts_with('<') {
            let end = rest.find('>')?;

            Some(rest[..=end].to_string())
        } else {
            rest.trim_start().starts_with('=').then(String::new)
        }
    })
}

/// The local an `export default <expr>` binds, when the expression is
/// not already a name. The export table is written after the last line,
/// so the value needs a binding to name there.
pub(crate) const DEFAULT_LOCAL: &str = "_default";

impl<'s> Desugar<'s> {
    /// The quoted path a `require` of a module spec writes: a data
    /// extension dropped, and a relative path moved to where Luau
    /// resolves it from.
    ///
    /// Luau reads `x/init.luau` as the module `x`, so a `./y` in that
    /// file names a file beside `x`, not one inside it. The emit writes
    /// the path from the folder Luau starts at, the way the runtime
    /// require does. A file that is not an `init`, and a compile with no
    /// output path, keep the path the source wrote.
    pub(crate) fn require_literal(&self, literal: &str) -> String {
        let quoted = crate::data::strip_literal(literal);
        let Some(q @ ('"' | '\'')) = quoted.chars().next() else {
            return quoted;
        };

        if quoted.len() < 2 || !quoted.ends_with(q) {
            return quoted;
        }

        let written = literal.trim_matches(['"', '\'']);

        let moved = self
            .options
            .mount_requires
            .iter()
            .find(|(s, _)| s == written)
            .map(|(_, path)| path);

        if let Some(place) = moved.filter(|p| p.starts_with('@')) {
            return format!("{q}{place}{q}");
        }

        // A spec that climbs out of its mount and back in takes the path
        // inside the mount, and the steps below finish it.
        let quoted = match moved {
            Some(path) => format!("{q}{path}{q}"),

            None => quoted,
        };

        // The build writes `thing.aly` as `thing.luau`, and a require
        // names a module with no extension: `./thing.aly` is `./thing`.
        let inner = &quoted[1..quoted.len() - 1];
        let quoted = match [".aly", ".alx", ".luau"]
            .iter()
            .find_map(|ext| inner.strip_suffix(ext))
        {
            Some(bare) => format!("{q}{bare}{q}"),

            None => quoted,
        };

        match self.init_require(&quoted[1..quoted.len() - 1]) {
            Some(moved) => format!("{q}{moved}{q}"),

            None => quoted,
        }
    }

    /// The path an `init` module requires a relative one by. `None` when
    /// the emit is no `init`, or the path is not relative.
    fn init_require(&self, path: &str) -> Option<String> {
        if !path.starts_with("./") && !path.starts_with("../") {
            return None;
        }

        let rel = std::path::Path::new(&self.options.module_rel);

        if !crate::build::is_init(rel) {
            return None;
        }

        let dir = rel.parent().unwrap_or(std::path::Path::new(""));
        let target = crate::modules::normalize(&dir.join(path));

        // A file inside the folder is `@self/...`. The folder's instance
        // name can differ from its name on disk, so `./src/x` fails in
        // Roblox and under a sourcemap.
        if let Ok(inner) = target.strip_prefix(dir)
            && !inner.starts_with("..")
        {
            let inner: Vec<_> = inner.iter().map(|c| c.to_string_lossy()).collect();

            return Some(format!("@self/{}", inner.join("/")));
        }

        Some(crate::build::relative_require(
            &crate::build::module_base(rel),
            &target,
        ))
    }

    /// `import` becomes `require` plus locals or type aliases. A data
    /// path loses its extension: the build writes `data.json` as
    /// `data.luau`, and `require("./data")` finds it.
    /// Whether the module a quoted spec names exports a type by this
    /// name, from the index the caller built.
    pub(crate) fn module_exports_type(&self, quoted: &str, name: &str) -> bool {
        self.module_type_entry(quoted, name).is_some()
    }

    /// Whether the module exports this name as a type with no value:
    /// `export type`, `export interface`. A bare import of one binds
    /// the type alone; a `local` would read a key the table lacks.
    pub(crate) fn module_exports_type_only(&self, quoted: &str, name: &str) -> bool {
        self.module_type_entry(quoted, name)
            .is_some_and(crate::modules::type_only)
    }

    /// Whether every name a list binds is a type: the spec says `type`,
    /// or the module exports the name as a type alone. Such a line costs
    /// nothing at run time.
    pub(crate) fn list_is_type_only(&self, quoted: &str, specs: &[ImportSpec]) -> bool {
        !specs.is_empty()
            && specs.iter().all(|sp| {
                let name = self.text_of(sp.name);

                sp.is_type || self.module_exports_type_only(quoted, name)
            })
    }

    /// Whether the module keeps a private view of this struct, which
    /// its check artifact exports as `Name__all`.
    pub(crate) fn module_private_view(&self, quoted: &str, name: &str) -> bool {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options
            .import_private_views
            .iter()
            .filter(|(s, _)| s == spec)
            .any(|(_, views)| views.iter().any(|v| v == name))
    }

    fn module_type_entry(&self, quoted: &str, name: &str) -> Option<&str> {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options
            .import_types
            .iter()
            .filter(|(s, _)| s == spec)
            .flat_map(|(_, types)| types.iter())
            .find(|t| crate::modules::type_head(t) == name)
            .map(String::as_str)
    }

    /// The parameter list an imported type declares, `<T>`. A type
    /// alias to a generic has to carry them: `type S<T> = _m1.S<T>`.
    pub(crate) fn module_type_params(&self, quoted: &str, name: &str) -> String {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options
            .import_types
            .iter()
            .filter(|(s, _)| s == spec)
            .flat_map(|(_, types)| types.iter())
            .find(|t| crate::modules::type_head(t) == name)
            .map(|t| crate::modules::type_args(t).to_string())
            .unwrap_or_default()
    }

    /// The type aliases an imported namespace asks for: a module that
    /// exports `namespace Math` exports its types as `Math_Vec2`, and
    /// the file that imports the namespace needs one alias each.
    pub(crate) fn namespace_type_aliases(
        &mut self,
        quoted: &str,
        name: &str,
        local: &str,
        temp: &str,
    ) -> Vec<String> {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);
        let head = format!("{name}_");
        let mut out = Vec::new();
        let word = match self.export_listed_bare.contains(local) {
            true => "export type",

            false => "type",
        };

        for entry in self
            .options
            .import_types
            .iter()
            .filter(|(s, _)| s == spec)
            .flat_map(|(_, types)| types.iter())
        {
            let full = crate::modules::type_head(entry);
            let Some(rest) = full.strip_prefix(&head) else {
                continue;
            };
            let args = crate::modules::type_args(entry).to_string();
            let type_args = type_arguments(&args);
            out.push(format!(
                "{word} {local}_{rest}{args} = {temp}.{full}{type_args}"
            ));

            // A member with private members keeps a full view the
            // declaring file exports. An `impl` of it here types `self`
            // as the view, the way an impl of a plain struct does.
            if self.options.check && self.module_private_view(quoted, full) {
                out.push(format!("type {local}_{rest}__all = {temp}.{full}__all"));
                self.private_view_names.insert(format!("{local}_{rest}"));
            }
        }

        out
    }

    /// `export type G_Vec = Geo_Vec` for each type member of a namespace
    /// the file imports as `name`, sent out as `exported`.
    fn imported_member_types(&self, name: &str, exported: &str) -> Vec<String> {
        let Some(ns) = self.namespaces.get(name).filter(|ns| ns.start == ns.end) else {
            return Vec::new();
        };
        let entries: Vec<&String> = self
            .options
            .import_types
            .iter()
            .flat_map(|(_, types)| types.iter())
            .collect();

        ns.members
            .iter()
            .filter(|m| m.ty)
            .map(|m| {
                let full = format!("{}{}", ns.prefix, m.name);
                let args = entries
                    .iter()
                    .find(|e| crate::modules::type_head(e) == full)
                    .map(|e| crate::modules::type_args(e).to_string())
                    .unwrap_or_default();
                let type_args = type_arguments(&args);

                format!(
                    "export type {exported}_{}{args} = {}{type_args}",
                    m.name, m.rendered
                )
            })
            .collect()
    }

    /// `export type Leaf_Box = Leaf.Box` for each type of the module a
    /// star import binds as `name`, sent out as `exported`. Luau reads
    /// no type path two modules deep, `B.Leaf.Box`, so the module's types
    /// go out under one flat name each, as a namespace's do.
    fn star_member_types(&self, name: &str, exported: &str) -> Vec<String> {
        let Some(spec) = self.star_specs.get(name) else {
            return Vec::new();
        };

        self.options
            .import_types
            .iter()
            .filter(|(s, _)| s == spec)
            .flat_map(|(_, types)| types.iter())
            // The default entry names no type of its own.
            .filter(|entry| crate::modules::default_type(std::slice::from_ref(entry)).is_none())
            .map(|entry| {
                let full = crate::modules::type_head(entry);
                let args = crate::modules::type_args(entry);
                let type_args = type_arguments(args);

                format!("export type {exported}_{full}{args} = {name}.{full}{type_args}")
            })
            .collect()
    }

    /// Whether a quoted spec names a module Alloy does not compile. Such
    /// a module returns one value and has no export table, so its value
    /// is what a default import binds. A spec that carries the
    /// extension says so on its own, with no project to resolve it.
    pub(crate) fn is_plain_module(&self, quoted: &str) -> bool {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options.plain_modules.iter().any(|s| s == spec)
            || spec.ends_with(".luau")
            || spec.ends_with(".lua")
            || crate::data::Format::of(spec).is_some()
    }

    /// What a default import reads off the required module: the
    /// `default` field of an Alloy module's export table, and the whole
    /// value of a plain Luau or data module.
    /// `local P = _m1.default type P = _m1.Player` for a bare import of
    /// a module whose `export default` is a struct or an enum, or `None`
    /// for any other default.
    fn default_type(&mut self, quoted: &str, local: &str, anchor: u32) -> Option<String> {
        // A module that returns its value has no `default` field, and a
        // bare import of it binds the value alone.
        if self.is_plain_module(quoted) {
            return None;
        }

        let target = self.require_literal(quoted);
        self.default_entry(quoted)?;
        let temp = self.hoist_import(&target, anchor);
        let ty = self.default_type_of(quoted, local, &temp)?;

        Some(format!("local {local} = {temp}.default {ty}"))
    }

    /// `type P = _m1.Player`: the type of a module's default struct or
    /// enum, under the name this file binds.
    fn default_type_of(&self, quoted: &str, local: &str, temp: &str) -> Option<String> {
        let word = self.type_word(local);

        self.default_alias(quoted, local, temp)
            .map(|alias| format!("{word} {alias}"))
    }

    /// `P<T> = _m1.Player<T>`: the type of a module's default under
    /// `local`, read off `temp`, the table the require binds. The
    /// `self` type of a class the module returns reads off the value.
    fn default_alias(&self, quoted: &str, local: &str, temp: &str) -> Option<String> {
        let entry = self.default_entry(quoted)?;

        if entry.contains(crate::modules::MODULE_VALUE) {
            let value = format!("{temp}{}", self.default_suffix(quoted));
            let ty = entry.replace(crate::modules::MODULE_VALUE, &value);

            return Some(format!("{local} = {ty}"));
        }

        let head = crate::modules::type_head(entry);
        let args = crate::modules::type_args(entry);
        let type_args = type_arguments(args);

        Some(format!("{local}{args} = {temp}.{head}{type_args}"))
    }

    /// The type entry of a module's default struct or enum.
    fn default_entry(&self, quoted: &str) -> Option<&str> {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options
            .import_types
            .iter()
            .filter(|(s, _)| s == spec)
            .find_map(|(_, types)| crate::modules::default_type(types))
    }

    pub(crate) fn default_suffix(&self, quoted: &str) -> &'static str {
        match self.is_plain_module(quoted) {
            true => "",

            false => ".default",
        }
    }

    /// What a name in braces reads off the required module: `.name`.
    /// `default` reads what a bare import reads, so of a module that
    /// ends in `return K` it is `K` itself, not a field of it.
    fn member_suffix(&self, quoted: &str, name: &str) -> String {
        match name {
            "default" => self.default_suffix(quoted).to_string(),

            _ => format!(".{name}"),
        }
    }

    /// Reports a std name the file writes with no import, once per
    /// name: the first use carries the report, and its fix writes the
    /// import that covers every use.
    pub(crate) fn check_std_name(&mut self, at: TokSpan, name: &str) {
        if crate::std_names::is_std_name(name)
            && !self.options.std_globals.ambient(name)
            && !self.std_imports.contains(name)
            && self.std_reported.insert(name.to_string())
        {
            self.diagnose(at, &crate::std_names::missing_message(name));
        }
    }

    /// `import { HashMap } from "@alloy/std/collections"`. The name
    /// renders as `__alloy.HashMap` wherever it stands, so the import
    /// writes nothing. An alias writes a local and a type for it, and a
    /// star import binds the runtime itself, so `c.HashMap` reads both
    /// the value and the type.
    fn std_import(&mut self, i: &Import, spec: &str, module: &str) {
        use crate::std_names::{MODULES, PREFIX};

        let anchor = self.byte_start(i.span);
        let Some(names) = crate::std_names::names_in(module) else {
            let modules: Vec<String> = MODULES
                .iter()
                .map(|(m, _)| format!("\"{PREFIX}/{m}\""))
                .collect();
            let message = format!(
                "\"{spec}\" is no std module; the std has {}",
                modules.join(", ")
            );
            self.diagnose(i.path, &message);

            return;
        };
        let mut lines: Vec<String> = Vec::new();
        let specs = match &i.kind {
            ImportKind::Namespace(n, specs) => {
                let name = self.text_of(*n).to_string();
                let std = self.require_text(&luau_string(&self.options.std_require));
                lines.push(format!("local {name} = {std}"));

                specs
            }

            ImportKind::Default(n) | ImportKind::Both(n, _) => {
                let name = self.text_of(*n).to_string();
                let message = format!(
                    "the std has no default export; write `import {{ ... }} from \"{spec}\"`, or `import * as {name} from \"{spec}\"` for the module"
                );
                self.diagnose(*n, &message);

                match &i.kind {
                    ImportKind::Both(_, specs) => specs,

                    _ => return,
                }
            }

            ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => specs,
        };

        for s in specs {
            let name = self.text_of(s.name).to_string();

            if !names.contains(&name.as_str()) {
                let message = match crate::std_names::spec_of(&name) {
                    Some(home) => format!("\"{spec}\" has no `{name}`; it is in \"{home}\""),

                    None => format!("the std has no `{name}`"),
                };
                self.diagnose(s.name, &message);

                continue;
            }

            let Some(alias) = s.alias else {
                continue;
            };
            let local = self.text_of(alias).to_string();

            if AMBIENT.contains(&name.as_str()) {
                let std = self.std();
                lines.push(format!("local {local} = {std}.{name}"));
            }

            if let Some(params) = runtime_type_params(&name) {
                let std = self.type_std();
                let args = crate::desugar::strip_bounds(&params);
                let args = match args == "<>" {
                    true => String::new(),

                    false => args,
                };
                lines.push(format!("type {local}{params} = {std}{name}{args}"));
            }
        }

        self.generate(anchor, &lines.join(" "));
    }

    /// Blanks a top-level import in the ship artifact when each name it
    /// binds is read, and read only in code the ship artifact drops: a
    /// test. The require would run a module nothing there reads.
    pub(crate) fn drop_test_only_imports(&mut self) {
        let dropped = self.ship_blanks.clone();
        let inside = |at: u32| dropped.iter().any(|(a, b)| at >= *a && at < *b);

        for (start, end, names) in std::mem::take(&mut self.top_imports) {
            let only_tests = !names.is_empty()
                && names.iter().all(|name| {
                    let mut reads = self
                        .toks
                        .iter()
                        .filter(|t| t.kind == TokKind::Ident && t.text(self.src) == name)
                        .map(|t| t.start)
                        .filter(|at| !(start..end).contains(at))
                        .peekable();

                    reads.peek().is_some() && reads.all(inside)
                });

            if only_tests && !inside(start) {
                self.ship_blanks.push((start, end));
            }
        }
    }

    pub(crate) fn import_stmt(&mut self, i: &Import) {
        let anchor = self.byte_start(i.span);
        // The spec as written: `strip_literal` drops a data extension,
        // and the extension is what says the module is not Alloy's.
        let spec = self.text_of(i.path).to_string();
        let path = crate::data::strip_literal(&spec);
        // What the `require` writes. `path` stays the key the module
        // index is built under: the spec the source wrote.
        let target = self.require_literal(&spec);
        let bare = spec.trim_matches(['"', '\'']);

        if let Some(module) = crate::std_names::module_of_spec(bare) {
            self.std_import(i, bare, module);

            return;
        }

        // `"game"` and `"game:Players"` name services, not modules, so
        // the import binds `game:GetService` calls instead of a
        // `require`. A form the spec does not allow still lowers to the
        // service it names; `import_problems` reports the form.
        if let Some(game) = crate::game_import::game_path(bare) {
            self.service_import(i, &game);

            return;
        }

        if !self.options.tests && self.at_top_level() {
            let (head, specs) = match &i.kind {
                ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                    (Some(*n), &specs[..])
                }

                ImportKind::Default(n) => (Some(*n), &[][..]),

                ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => (None, &specs[..]),
            };
            let names = head
                .into_iter()
                .chain(specs.iter().map(|s| s.alias.unwrap_or(s.name)))
                .map(|n| self.text_of(n).to_string())
                .collect();
            self.top_imports
                .push((anchor, self.byte_end(i.span), names));
        }

        match &i.kind {
            // `import * as M from "p"`, and `import * as M, { a }`,
            // which reads the names off `M` itself: the alias already
            // binds the whole module.
            ImportKind::Namespace(n, specs) => {
                let name = self.text_of(*n).to_string();
                let mut text = format!("local {name} = {}", self.require_text(&target));
                let picked = self.spec_bindings(&path, &name, specs);
                text.push_str(&picked);
                self.generate(anchor, &text);
            }

            // `import M from "p"`: the module's `export default`, under
            // the name written here.
            ImportKind::Default(n) => {
                let name = self.text_of(*n).to_string();

                // `export default struct P`: the type comes along under
                // the name this file binds.
                if let Some(ty) = self.default_type(&path, &name, anchor) {
                    self.generate(anchor, &ty);

                    return;
                }

                let suffix = self.default_suffix(&spec);
                let req = self.require_text(&target);
                self.generate(anchor, &format!("local {name} = {req}{suffix}"));
            }

            // `import M, { a } from "p"`: the default binds as `M`, and
            // the names read from the module beside it.
            ImportKind::Both(module, specs) => {
                let base = self.text_of(*module).to_string();
                // A plain module's value is the whole table, so the
                // binding is what the names read from. An Alloy
                // module's default is one field of its export table, so
                // the table takes a name of its own.
                let (temp, mut text) = match self.is_plain_module(&spec) {
                    true => {
                        let req = self.require_text(&target);

                        (base.clone(), format!("local {base} = {req}"))
                    }

                    false => {
                        let temp = self.hoist_import(&target, anchor);
                        let mut text = format!("local {base} = {temp}.default");

                        if let Some(ty) = self.default_type_of(&path, &base, &temp) {
                            text.push_str(&format!(" {ty}"));
                        }

                        (temp, text)
                    }
                };
                let picked = self.spec_bindings(&path, &temp, specs);
                text.push_str(&picked);
                self.generate(anchor, &text);
            }

            ImportKind::Named(specs) => {
                // `import { type Meters }`, or a list of names the module
                // exports as types alone, says the same thing an
                // `import type { }` line says, so its `require` goes
                // too. A list with one value in it keeps the require and
                // drops nothing.
                if self.list_is_type_only(&path, specs) {
                    self.ship_blanks
                        .push((self.byte_start(i.span), self.byte_end(i.span)));
                }

                let temp = self.hoist_import(&target, anchor);
                let text = self.spec_bindings(&path, &temp, specs);
                self.generate(anchor, text.trim_start());
            }

            ImportKind::TypeOnly(specs) => {
                // The whole statement exists for the type checker, so it
                // is blanked to ship: every output chunk anchored inside
                // it goes. A require there runs the module, and two
                // modules that name each other's types would loop.
                self.ship_blanks
                    .push((self.byte_start(i.span), self.byte_end(i.span)));
                // luau-lsp types no cycle of requires: the module it
                // reaches second has no types. An import that closes one
                // requires nothing here, and its names read as `any`.
                // ponytail: `any` loses the shape; a checker-only copy of
                // the target's types would keep it.
                let cut = self.options.type_cuts.iter().any(|c| c == bare);
                let temp = match cut {
                    true => String::new(),

                    false => self.hoist_import(&target, anchor),
                };
                let mut parts: Vec<String> = Vec::new();

                for sp in specs {
                    let name = self.text_of(sp.name).to_string();
                    let local = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    let args = self.module_type_params(&path, &name);
                    let type_args = type_arguments(&args);
                    // A namespace carries the types, and is none itself:
                    // the module exports `A_B_Shape`, not `A`. So the
                    // spec writes one alias per member, and the alias of
                    // the name alone only when the module exports it.
                    let aliases = self.namespace_type_aliases(&path, &name, &local, &temp);

                    if aliases.is_empty() || self.module_exports_type(&path, &name) {
                        let word = self.type_word(&local);

                        parts.push(format!("{word} {local}{args} = {temp}.{name}{type_args}"));
                    }

                    parts.extend(aliases);
                }

                // The value side goes: a default in the parameters,
                // `<T = number>`, stays with the head.
                if cut {
                    for p in &mut parts {
                        if let Some((head, _)) = p.rsplit_once(" = ") {
                            *p = format!("{head} = any");
                        }
                    }
                }

                self.generate(anchor, &parts.join(" "));
            }
        }
    }

    /*
    The bindings a list in braces writes, as one run of Luau, each part
    with a leading space so it appends to the `local` the form already
    generated.

    `temp` is the table the names read off: the hoisted `require` for
    `import { a }`, the alias for `import * as M, { a }`. The three
    forms that take a list share the body, so a name, a type, and a
    namespace alias lower the same way in each.
    */
    /// The word an imported type's alias takes: `export type` when the
    /// file re-exports the name under its own name, and `type`
    /// otherwise. Luau has no re-export for an alias, and a second
    /// definition of the name is a redefinition, so the alias the
    /// import already writes is the one that goes out.
    fn type_word(&self, local: &str) -> &'static str {
        match self.export_listed_types.contains(local) {
            true => "export type",

            false => "type",
        }
    }

    fn spec_bindings(&mut self, path: &str, temp: &str, specs: &[ImportSpec]) -> String {
        let mut names = Vec::new();
        let mut values = Vec::new();
        let mut types = Vec::new();

        for sp in specs {
            let name = self.text_of(sp.name).to_string();
            let local = sp
                .alias
                .map(|a| self.text_of(a).to_string())
                .unwrap_or(name.clone());

            // An `export macro` is source, not a value: the module's
            // table carries no key for it. The import brings the
            // definition in through `EmitOptions.macros`, and `$name`
            // expands here.
            if self.options.macros.iter().any(|m| m.name == local) {
                continue;
            }

            let args = self.module_type_params(path, &name);
            let type_args = type_arguments(&args);

            let word = self.type_word(&local);

            if sp.is_type || self.module_exports_type_only(path, &name) {
                types.push(format!("{word} {local}{args} = {temp}.{name}{type_args}"));
            } else {
                // A struct or an enum is a value and a type; the type
                // comes along when the module exports one.
                if self.module_exports_type(path, &name) {
                    types.push(format!("{word} {local}{args} = {temp}.{name}{type_args}"));
                }

                // The declaring file's check artifact exports the full
                // view of a struct with private members. An `impl` of it
                // here types `self` as the view, so its methods reach
                // them; the `private_access` lint guards a call outside
                // any impl.
                if self.options.check && self.module_private_view(path, &name) {
                    types.push(format!("type {local}__all = {temp}.{name}__all"));
                    self.private_view_names.insert(local.clone());
                }

                types.extend(self.namespace_type_aliases(path, &name, &local, temp));
                names.push(local);
                values.push(format!("{temp}{}", self.member_suffix(path, &name)));
            }
        }

        let mut text = String::new();

        if !names.is_empty() {
            text.push_str(&format!(
                " local {} = {}",
                names.join(", "),
                values.join(", ")
            ));
        }

        for t in types {
            text.push(' ');
            text.push_str(&t);
        }

        text
    }

    /// `import { Players } from "game"` and `import P from "game:Players"`
    /// both bind `game:GetService`. Every binding lands on the import's
    /// own line, so the line count holds and the analyzer reads the
    /// service class the definitions declare.
    fn service_import(&mut self, i: &Import, game: &crate::game_import::GamePath) {
        use crate::game_import::GamePath;

        let anchor = self.byte_start(i.span);
        // The path names the service for every form of `"game:X"`; a
        // `"game"` import takes each service from the name written.
        let named = |this: &Self, name: TokSpan, alias: Option<TokSpan>| {
            let written = this.text_of(name).to_string();
            let local = alias
                .map(|a| this.text_of(a).to_string())
                .unwrap_or(written.clone());
            let service = match game {
                GamePath::Every => written,

                GamePath::One(service) => service.clone(),
            };

            crate::game_import::get_service(&local, &service)
        };
        let mut lines = Vec::new();

        match &i.kind {
            ImportKind::Default(n) => {
                lines.push(named(self, *n, None));
            }

            ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                lines.push(named(self, *n, None));

                for sp in specs {
                    lines.push(named(self, sp.name, sp.alias));
                }
            }

            ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                for sp in specs {
                    lines.push(named(self, sp.name, sp.alias));
                }
            }
        }

        self.generate(anchor, &lines.join(" "));
    }

    /// What an `export { ... }` list needs to be true, and what a
    /// module's export table needs: every name in a list is a binding
    /// of the module, and no name goes out twice.
    pub(crate) fn check_exports(&mut self, block: &Block) {
        let mut values: Vec<String> = Vec::new();
        let mut types: Vec<String> = Vec::new();

        for stmt in &block.stmts {
            self.binding_names(stmt, &mut values, &mut types);
        }

        // The name each `export` puts in the export table, in the order
        // they are written, with the token to report a repeat on.
        let mut sent: Vec<(String, TokSpan)> = Vec::new();
        let mut hits: Vec<(TokSpan, String)> = Vec::new();

        for stmt in &block.stmts {
            match stmt {
                Stmt::ExportList(e) if e.from.is_none() => {
                    for sp in &e.specs {
                        let name = self.text_of(sp.name).to_string();
                        // A value list may name a type the file
                        // declares, `export { Shape }` of an interface.
                        let file_type = self.file_types.get(&name).copied();
                        let known = match e.type_only || sp.is_type {
                            true => types.contains(&name) || values.contains(&name),

                            false => values.contains(&name) || file_type.is_some(),
                        };

                        // A dotted name reads a namespace member; the
                        // namespace check owns that path.
                        if !known && !name.contains('.') {
                            hits.push((
                                sp.name,
                                format!("`{name}` is not a binding of this module"),
                            ));

                            continue;
                        }

                        // A type goes out as an alias, not as a field
                        // of the export table.
                        if !(e.type_only || sp.is_type || file_type == Some(false)) {
                            let out = sp.alias.unwrap_or(sp.name);
                            sent.push((self.text_of(out).to_string(), out));
                        }
                    }
                }

                other if crate::desugar::namespaces::is_exported(other.under_default()) => {
                    let mut one = Vec::new();
                    let mut none = Vec::new();
                    self.binding_names(other, &mut one, &mut none);

                    let _ = none;

                    for name in one {
                        let span = self.name_span(other, &name);
                        sent.push((name, span));
                    }
                }

                _ => {}
            }
        }

        for (i, (name, span)) in sent.iter().enumerate() {
            if sent[..i].iter().any(|(n, _)| n == name) {
                hits.push((
                    *span,
                    format!("`{name}` is exported twice; a module sends one binding per name"),
                ));
            }
        }

        for (span, message) in hits {
            self.diagnose(span, &message);
        }
    }

    /// The names one top-level statement binds: values first, then the
    /// names that are types alone.
    fn binding_names(&self, stmt: &Stmt, values: &mut Vec<String>, types: &mut Vec<String>) {
        let value = |v: &mut Vec<String>, span: TokSpan| v.push(self.text_of(span).to_string());

        match stmt.under_default() {
            // A destructured local binds through its fields, and each
            // of those names is a binding of the module too.
            Stmt::Local(l) => {
                for name in super::statements::local_names(l) {
                    value(values, name);
                }
            }

            Stmt::LocalFunction(f) => value(values, f.name),

            Stmt::Function(f) if f.path.len() == 1 => value(values, f.path[0]),

            Stmt::Struct(d) => {
                value(values, d.name);
                value(types, d.name);
            }

            Stmt::Enum(d) => {
                value(values, d.name);
                value(types, d.name);
            }

            Stmt::Trait(d) => {
                value(values, d.name);
                value(types, d.name);
            }

            Stmt::Class(d) => {
                value(values, d.name);
                value(types, d.name);
            }

            Stmt::Interface(d) => value(types, d.name),

            Stmt::TypeAlias(d) => value(types, d.name),

            Stmt::Remote(d) => value(values, d.name),

            Stmt::Namespace(d) => value(values, d.name),

            Stmt::Macro(d) => value(values, d.name),

            Stmt::Attribute(d) => value(values, d.name),

            Stmt::Import(i) => match &i.kind {
                ImportKind::Default(n) => value(values, *n),

                ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                    value(values, *n);

                    for sp in specs {
                        let at = sp.alias.unwrap_or(sp.name);

                        match sp.is_type {
                            true => value(types, at),

                            false => value(values, at),
                        }
                    }
                }

                ImportKind::Named(specs) => {
                    for sp in specs {
                        let at = sp.alias.unwrap_or(sp.name);

                        match sp.is_type {
                            true => value(types, at),

                            false => value(values, at),
                        }
                    }
                }

                ImportKind::TypeOnly(specs) => {
                    for sp in specs {
                        value(types, sp.alias.unwrap_or(sp.name));
                    }
                }
            },

            _ => {}
        }
    }

    /// The token a statement declares `name` at, for a message.
    fn name_span(&self, stmt: &Stmt, name: &str) -> TokSpan {
        let span = stmt.span();

        for i in span.start..span.end {
            let one = TokSpan::new(i as usize, i as usize + 1);

            if self.text_of(one) == name {
                return one;
            }
        }

        span
    }

    pub(crate) fn export_list(&mut self, e: &ExportList) {
        let anchor = self.byte_start(e.span);

        match e.from {
            None => {
                let mut types = Vec::new();

                for sp in &e.specs {
                    let name = self.text_of(sp.name).to_string();

                    // A macro is source, not a value; the importer
                    // reads it through `crate::modules::import_macros`.
                    if self.macro_of(&name).is_some() {
                        continue;
                    }

                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(&name).to_string());
                    // A type of this file: an interface or an alias
                    // holds no value, a struct or an enum holds one. A
                    // type the file imports goes out the same way; a
                    // barrel module sends on what it took in.
                    let file_type = self
                        .file_types
                        .get(&name)
                        .or_else(|| self.imported_types.get(&name))
                        .copied();

                    // A type is no value: the module sends it out as an
                    // alias, not as a field of the export table. A
                    // namespace member reads by the name the emit gave
                    // it, `Geom_Point`.
                    if e.type_only || sp.is_type || file_type == Some(false) {
                        // A type alias of this file takes the `export`
                        // word on its own line; an alias of itself is a
                        // cycle, and Luau reads none.
                        if self.export_listed_types.contains(&name) {
                            continue;
                        }

                        let target = self
                            .namespace_path_name(&name)
                            .unwrap_or_else(|| name.clone());
                        types.push(format!("export type {exported} = {target}"));
                    } else {
                        // `export { Named as Other }` of a struct: the
                        // type goes out under the new name too.
                        if file_type == Some(true) && sp.alias.is_some() {
                            types.push(format!("export type {exported} = {name}"));
                        }

                        // An imported namespace under a new name sends
                        // its types out under that name. Under its own
                        // name the import's aliases carry the word.
                        if exported != name {
                            types.extend(self.imported_member_types(&name, &exported));
                        }

                        types.extend(self.star_member_types(&name, &exported));
                        self.exports.push((exported, name));
                    }
                }

                if !types.is_empty() {
                    self.generate(anchor, &types.join(" "));
                    self.blank_lines(anchor, self.byte_end(e.span));
                }
            }

            Some(path) => {
                let spec = crate::data::strip_literal(self.text_of(path));
                let target = self.require_literal(self.text_of(path));
                let temp = self.hoist_import(&target, anchor);
                let mut types = Vec::new();

                for sp in &e.specs {
                    let name = self.text_of(sp.name).to_string();
                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    // `crate::modules::import_macros` reads the list as
                    // an import, under the name it goes out as.
                    if self.options.macros.iter().any(|m| m.name == exported) {
                        continue;
                    }

                    // A generic type carries its parameters on: the
                    // alias reads the bare name otherwise.
                    let args = self.module_type_params(&spec, &name);
                    let type_args = type_arguments(&args);
                    let alias = format!("export type {exported}{args} = {temp}.{name}{type_args}");
                    // An import of the name from this module already
                    // writes the alias with the word; a second is a
                    // redefinition.
                    let imported = sp.alias.is_none() && self.export_listed_types.contains(&name);

                    if e.type_only || sp.is_type || self.module_exports_type_only(&spec, &name) {
                        if !imported {
                            types.push(alias);
                        }
                    } else {
                        // A struct or an enum is a value and a type,
                        // and the list sends both on.
                        if !imported && self.module_exports_type(&spec, &name) {
                            types.push(alias);
                        }

                        if !imported {
                            let members =
                                self.namespace_type_aliases(&spec, &name, &exported, &temp);
                            types.extend(members.into_iter().map(|t| format!("export {t}")));
                        }

                        // `export { default as K }`: the type of the
                        // default goes out under `K`, as the value does.
                        // A type another spec of the file sends out as
                        // `K` stands, and a second one is a redefinition.
                        if name == "default"
                            && !self.reexported_types.contains(&exported)
                            && let Some(alias) = self.default_alias(&spec, &exported, &temp)
                        {
                            types.push(format!("export type {alias}"));
                        }

                        let suffix = self.member_suffix(self.text_of(path), &name);
                        self.exports.push((exported, format!("{temp}{suffix}")));
                    }
                }

                if !types.is_empty() {
                    self.generate(anchor, &types.join(" "));
                }
            }
        }
    }

    /// `export default` puts the value under `default` in the export
    /// table, the way TypeScript compiles one. A declaration keeps its
    /// own emit and the table names it; any other expression binds a
    /// local first, because the table is written after the last line.
    pub(crate) fn export_default(&mut self, span: TokSpan, value: &DefaultExport) {
        let anchor = self.byte_start(span);

        if self.has_default_export {
            self.diagnostics.push(Diagnostic {
                start: anchor,
                end: self.byte_end(span),
                message: "a module has one `export default`; export this one by name".to_string(),
            });
        }

        self.has_default_export = true;

        match value {
            DefaultExport::Decl(inner) => {
                // A type is not a value, and the export table carries
                // values. `export type` is the way to send one out.
                let is_type = matches!(inner.as_ref(), Stmt::TypeAlias(_));

                if let Stmt::TypeAlias(t) = inner.as_ref() {
                    let name = self.text_of(t.name).to_string();
                    self.diagnostics.push(Diagnostic {
                        start: anchor,
                        end: self.byte_end(span),
                        message: format!(
                            "`export default` takes a value, not a type; write \
                             `export type {name} = ...` and import it in braces"
                        ),
                    });
                }

                let name = inner
                    .declared_name()
                    .map(|n| self.text_of(n).to_string())
                    .unwrap_or_default();

                if name.is_empty() {
                    self.diagnostics.push(Diagnostic {
                        start: anchor,
                        end: self.byte_end(span),
                        message: "`export default` needs a name here, or a value".to_string(),
                    });
                } else if !is_type {
                    self.exports.push(("default".to_string(), name.clone()));
                }

                // `function f()` at the top level is a global; the name
                // a module exports is its own. A function under
                // attributes writes the word itself, after them.
                if matches!(inner.as_ref(), Stmt::Function(f) if f.attrs.is_empty()) {
                    self.generate(self.byte_start(inner.span()), "local ");
                }

                // A struct or an enum is a type too, and a bare import
                // binds it; see `crate::modules::default_type`.
                if matches!(inner.as_ref(), Stmt::Struct(_) | Stmt::Enum(_)) {
                    self.export_listed_types.insert(name);
                }

                // The `export default` words are dropped; the
                // declaration renders from its own keyword.
                self.stmt(inner);
            }

            // A name is already a binding: the table reads it directly
            // and the statement emits nothing.
            DefaultExport::Value(Expr::Name(n)) => {
                let name = self.text_of(*n).to_string();
                self.exports.push(("default".to_string(), name));
            }

            DefaultExport::Value(expr) => {
                self.generate(anchor, &format!("local {DEFAULT_LOCAL} = "));
                self.expr(expr);
                self.exports
                    .push(("default".to_string(), DEFAULT_LOCAL.to_string()));
            }
        }
    }

    /// `export local x = 1` becomes `local x = 1` and exports `x`.
    pub(crate) fn exported_local(&mut self, span: TokSpan, l: &Local) {
        // A destructure binds its fields, not the `{ ... }` it is
        // written as: the table sends each of those names out.
        for name in super::statements::local_names(l) {
            let name = self.text_of(name).to_string();
            self.exports.push((name.clone(), name));
        }

        // The attributes stand above the word, and the span opens on
        // the first of them. Their lines go blank; the `export` word
        // after them goes, and the rest renders as a plain local, with
        // the guard of its `@cfg`.
        let cfg = self.local_cfg(l);

        let word = l
            .attrs
            .iter()
            .map(|a| a.span.end)
            .max()
            .unwrap_or(span.start) as usize;
        let rest = TokSpan::new(word + 1, span.end as usize);

        // The newline between the last attribute and the word.
        if let Some(after) = l.attrs.iter().map(|a| self.byte_end(a.span)).max() {
            self.copy(after, self.toks[word].start);
        }

        if let Some(cond) = cfg
            && self.cfg_guarded_local(l, &cond, self.byte_start(rest))
        {
            return;
        }

        if local_needs_rewrite(l) {
            self.local_stmt(l);
        } else {
            // `export const m: HashMap<K, V> = HashMap.new()`: the call
            // takes the annotation's arguments, as for a plain local.
            self.expected_generic = self.annotated_constructor(l);
            let children: Vec<Child<'_>> = l.values.iter().map(Child::Expr).collect();
            self.stitch(rest, &children, |d, child| match child {
                Child::Expr(e) => d.expr(e),

                Child::Block(b) => d.block(b),

                Child::Function(b) => d.function_block(b),
            });
            self.expected_generic = None;
        }
    }

    /// The export table, appended after the last token. The test
    /// artifact has no module to return: the spec's footer follows.
    pub(crate) fn module_return(&mut self, at: u32, block: &Block) {
        // A definitions file is no module, so it returns nothing.
        if self.options.tests || self.options.definitions {
            return;
        }

        // A module that exports only types binds no value, and Luau
        // requires a module to return exactly one. It returns an empty
        // table; the `export type` lines stand on their own.
        let types_only = self.exports.is_empty();

        if types_only && !exports_a_type(block) {
            return;
        }

        if matches!(block.stmts.last(), Some(Stmt::Return(_))) {
            // A module of types alone exports no value, so its own
            // `return` is the one value the module returns.
            if types_only {
                return;
            }

            self.diagnostics.push(Diagnostic {
                start: at,
                end: at,
                message: "a module with `export` returns its exports; remove the `return`"
                    .to_string(),
            });

            return;
        }

        if types_only {
            self.generate(at, " return {}");

            return;
        }

        let fields: Vec<String> = self
            .exports
            .iter()
            .map(|(k, v)| format!("{k} = {v}"))
            .collect();
        self.generate(at, &format!(" return {{ {} }}", fields.join(", ")));
    }

    // --- enums ---------------------------------------------------------------
}

/// The argument list a parameter list names: `<T = nil, U: Bound>`
/// declares, `<T, U>` refers. A default or a bound belongs on the
/// declaring side alone.
pub(crate) fn type_arguments(params: &str) -> String {
    let Some(inner) = params.strip_prefix('<').and_then(|p| p.strip_suffix('>')) else {
        return params.to_string();
    };
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();

    for c in inner.chars() {
        match c {
            '<' | '(' | '{' | '[' => {
                depth += 1;
                current.push(c);
            }
            '>' | ')' | '}' | ']' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => {
                names.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }

    if !current.trim().is_empty() {
        names.push(current);
    }

    let names: Vec<String> = names
        .iter()
        .map(|p| {
            let p = p.trim();
            let end = p.find(['=', ':']).unwrap_or(p.len());

            p[..end].trim().to_string()
        })
        .collect();

    format!("<{}>", names.join(", "))
}

impl Desugar<'_> {
    /// `sig.HashMap` under `import * as sig from "@alloy/std/signal"`:
    /// the local is the whole runtime, so the reach stops here, at the
    /// names the module exports. Every expression counts, a statement
    /// the emit copies as it is included.
    pub(crate) fn check_std_star_members(&mut self, block: &Block) {
        for stmt in &block.stmts {
            for c in stmt_children(stmt) {
                self.std_star_in(c);
            }
        }
    }

    fn std_star_in(&mut self, c: Child<'_>) {
        match c {
            Child::Expr(e) => {
                if let Expr::Index {
                    object,
                    key: IndexKey::Field(f),
                    ..
                } = e
                    && let Expr::Name(n) = object.as_ref()
                    && let Some(module) = self.std_namespaces.get(self.text_of(*n)).cloned()
                {
                    self.check_std_member(*f, &module);
                }

                for c in expr_children(e) {
                    self.std_star_in(c);
                }
            }

            Child::Block(b) => self.check_std_star_members(b),

            Child::Function(f) => self.check_std_star_members(&f.block),
        }
    }

    fn check_std_member(&mut self, f: TokSpan, module: &str) {
        let member = self.text_of(f).to_string();
        let exports = crate::std_names::names_in(module).unwrap_or_default();

        if exports.contains(&member.as_str()) && !crate::std_names::is_std_attribute(&member) {
            return;
        }

        let spec = match module.is_empty() {
            true => crate::std_names::PREFIX.to_string(),

            false => format!("{}/{module}", crate::std_names::PREFIX),
        };
        let message = match crate::std_names::spec_of(&member) {
            Some(home) if home != spec => {
                format!("\"{spec}\" has no `{member}`; it is in \"{home}\"")
            }

            _ => format!("\"{spec}\" has no `{member}`"),
        };
        self.diagnose(f, &message);
    }
}

#[cfg(test)]
mod tests {
    /// An attribute over `export local` left its own name and the
    /// `export` word in the emit, which no Luau reads: the span opens
    /// on the attribute, and only one token was skipped.
    #[test]
    fn an_attribute_over_an_export_local_drops_both_words() {
        let src = "attribute mark on local

@mark
export local ex = 1
print(ex)
";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.ship.contains("local ex = 1"), "{}", out.ship);
        assert!(!out.ship.contains("export local"), "{}", out.ship);
        assert!(!out.ship.contains("\nmark"), "{}", out.ship);
        assert_eq!(
            out.ship.lines().count(),
            src.lines().count(),
            "{}",
            out.ship
        );
    }

    /// `export { Named }` after the declaration sends the type out
    /// with the value: the declaration takes the `export` word, an
    /// interface or an alias goes out as a type alone, and a rename
    /// adds an alias.
    #[test]
    fn an_export_list_sends_the_type_of_a_declaration_out() {
        let src = "struct Named as\n    n: number\nend\n\nenum Kind as\n    A,\n    B,\nend\n\ninterface Shape as\n    area: number\nend\n\ntype Id = number\n\nfunction make(): Named\n    return new Named { n = 1 }\nend\n\nexport { Named, Kind, Shape, Id, make, Named as Other }\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        for text in [&out.ship, &out.check] {
            assert!(text.contains(" export type Named = "), "{text}");
            assert!(text.contains(" export type Kind = "), "{text}");
            assert!(text.contains("\nexport type Shape = "), "{text}");
            assert!(text.contains("\nexport type Id = number"), "{text}");
            assert!(
                text.contains(
                    "export type Other = Named return { Named = Named, Kind = Kind, make = make, Other = Named }"
                ),
                "{text}"
            );
            assert_eq!(text.lines().count(), src.lines().count(), "{text}");
        }
    }

    /// An attribute above `export default` was a syntax error: the
    /// reader took `export` and then asked for `function`. The
    /// attribute now goes on the declaration, as under `export`.
    #[test]
    fn an_attribute_over_export_default_goes_on_the_declaration() {
        for (src, want) in [
            (
                "@derive(Debug)\nexport default struct Bag\n    n: number\nend\n",
                "function Bag.debug(self)",
            ),
            (
                "@deprecated\nexport default function old(): number\n    return 1\nend\n",
                "\n@deprecated local function old(): number",
            ),
            (
                "@inline\nexport default function small(): number\n    return 1\nend\n",
                "\nlocal function small(): number",
            ),
        ] {
            let out = crate::compile(src).unwrap();
            assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
            assert!(out.ship.contains(want), "{}", out.ship);
            assert!(out.ship.contains("return { default = "), "{}", out.ship);
            assert!(!out.ship.contains("export default"), "{}", out.ship);
            assert_eq!(
                out.ship.lines().count(),
                src.lines().count(),
                "{}",
                out.ship
            );
        }
    }

    /// `export { default as Klass } from "./Klass"` of a module that ends
    /// in `return Klass` read `_m1.default`, and the barrel sent on nil:
    /// such a module has no export table. Its returned value is its
    /// default, as `import Klass from` reads it.
    #[test]
    fn the_default_of_a_returning_module_is_its_value() {
        let options = crate::EmitOptions {
            plain_modules: vec!["./Klass".to_string()],
            ..crate::EmitOptions::default()
        };
        let compile = |src: &str| crate::compile_with(src, &options).unwrap().ship;

        let barrel = compile("export { default as Klass } from \"./Klass\"\n");
        assert!(barrel.contains("return { Klass = _m1 }"), "{barrel}");

        let named = compile("import { default as K } from \"./Klass\"\nprint(K)\n");
        assert!(named.contains("local K = _m1"), "{named}");
        assert!(!named.contains(".default"), "{named}");

        // A module with an export table keeps the field.
        let table = compile("export { default as Other } from \"./Other\"\n");
        assert!(table.contains("return { Other = _m1.default }"), "{table}");
    }

    /// The re-export of a default carries its type under the new name:
    /// the type the module exports under the name it returns, the
    /// `self` type of the class it returns, or its default struct. A
    /// type another spec sends out under that name stands alone, and a
    /// bare import binds the value alone, as before.
    #[test]
    fn a_reexported_default_sends_its_type_on() {
        let options = crate::EmitOptions {
            plain_modules: vec!["./Klass".to_string(), "./Class".to_string()],
            import_types: vec![
                (
                    "./Klass".to_string(),
                    vec!["Klass<T>=".to_string(), "default Klass<T>".to_string()],
                ),
                (
                    "./Class".to_string(),
                    vec!["default typeof(@.new(nil :: any))".to_string()],
                ),
                (
                    "./Player".to_string(),
                    vec!["Player".to_string(), "default Player".to_string()],
                ),
            ],
            ..crate::EmitOptions::default()
        };
        let compile = |src: &str| crate::compile_with(src, &options).unwrap().ship;

        let named = compile("export { default as K } from \"./Klass\"\n");
        assert!(named.contains("export type K<T> = _m1.Klass<T>"), "{named}");

        let class = compile("export { default as C } from \"./Class\"\n");
        assert!(
            class.contains("export type C = typeof(_m1.new(nil :: any))"),
            "{class}"
        );

        let record = compile("export { default as P } from \"./Player\"\n");
        assert!(record.contains("export type P = _m1.Player"), "{record}");

        let both = compile(
            "export { default as Klass } from \"./Klass\"\nexport type { Klass } from \"./Klass\"\n",
        );
        assert_eq!(both.matches("export type Klass").count(), 1, "{both}");

        let bare = compile("import K from \"./Klass\"\nprint(K)\n");
        assert!(bare.contains("local K = require(\"./Klass\")\n"), "{bare}");
    }

    #[test]
    fn a_parameter_list_refers_by_name_alone() {
        assert_eq!(super::type_arguments("<T = nil>"), "<T>");
        assert_eq!(super::type_arguments("<T = nil, U: Bound>"), "<T, U>");
        assert_eq!(super::type_arguments("<K, V = { a: number }>"), "<K, V>");
        assert_eq!(super::type_arguments("<T...>"), "<T...>");
        assert_eq!(super::type_arguments(""), "");
    }
}
