//! Import, export, and module-return lowering.

use alloy_syntax::ast::{
    Block, DefaultExport, ExportList, Expr, Import, ImportKind, Local, Stmt, TokSpan,
};

use super::*;

/// The local an `export default <expr>` binds, when the expression is
/// not already a name. The export table is written after the last line,
/// so the value needs a binding to name there.
pub(crate) const DEFAULT_LOCAL: &str = "_default";

/// The table a module's `global local` values live on. The module
/// returns it, so every file that names one reads and writes the same
/// slot.
pub(crate) const GLOBAL_STATE: &str = "_gs";

impl<'s> Desugar<'s> {
    /// `import` becomes `require` plus locals or type aliases. A data
    /// path loses its extension: the build writes `data.json` as
    /// `data.luau`, and `require("./data")` finds it.
    /// Whether the module a quoted spec names exports a type by this
    /// name, from the index the caller built.
    pub(crate) fn module_exports_type(&self, quoted: &str, name: &str) -> bool {
        let spec = quoted
            .strip_prefix(['"', '\''])
            .and_then(|s| s.strip_suffix(['"', '\'']))
            .unwrap_or(quoted);

        self.options.import_types.iter().any(|(s, types)| {
            s == spec && types.iter().any(|t| crate::modules::type_head(t) == name)
        })
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
            .map(|t| t[name.len()..].to_string())
            .unwrap_or_default()
    }

    /// The type aliases an imported namespace asks for: a module that
    /// exports `namespace Math` exports its types as `Math_Vec2`, and
    /// the file that imports the namespace needs one alias each.
    pub(crate) fn namespace_type_aliases(
        &self,
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
            let args = entry[full.len()..].to_string();
            let type_args = type_arguments(&args);
            out.push(format!(
                "type {local}_{rest}{args} = {temp}.{full}{type_args}"
            ));
        }

        out
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
    pub(crate) fn default_suffix(&self, quoted: &str) -> &'static str {
        match self.is_plain_module(quoted) {
            true => "",

            false => ".default",
        }
    }

    /// A name that is already global needs no import, and an import of
    /// one hides that the name reaches every file. The spec reports.
    fn reject_global_imports(&mut self, i: &Import) {
        let specs = match &i.kind {
            ImportKind::Both(_, specs) | ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => {
                specs.clone()
            }

            _ => Vec::new(),
        };

        for sp in specs {
            let name = self.text_of(sp.name).to_string();

            if self.options.globals.iter().any(|g| g.name == name) {
                self.diagnostics.push(Diagnostic {
                    start: self.byte_start(sp.name),
                    end: self.byte_end(sp.name),
                    message: format!("`{name}` is global; it is in scope without an import"),
                });
            }
        }
    }

    pub(crate) fn import_stmt(&mut self, i: &Import) {
        self.reject_global_imports(i);

        let anchor = self.byte_start(i.span);
        // The spec as written: `strip_literal` drops a data extension,
        // and the extension is what says the module is not Alloy's.
        let spec = self.text_of(i.path).to_string();
        let path = crate::data::strip_literal(&spec);
        let bare = spec.trim_matches(['"', '\'']);

        // `"game"` and `"game:Players"` name services, not modules, so
        // the import binds `game:GetService` calls instead of a
        // `require`. A form the spec does not allow still lowers to the
        // service it names; `import_problems` reports the form.
        if let Some(game) = crate::game_import::game_path(bare) {
            self.service_import(i, &game);

            return;
        }

        match &i.kind {
            ImportKind::Namespace(n) => {
                let name = self.text_of(*n).to_string();
                self.generate(anchor, &format!("local {name} = require({path})"));
            }

            // `import M from "p"`: the module's `export default`, under
            // the name written here.
            ImportKind::Default(n) => {
                let name = self.text_of(*n).to_string();
                let suffix = self.default_suffix(&spec);
                self.generate(anchor, &format!("local {name} = require({path}){suffix}"));
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
                    true => (base.clone(), format!("local {base} = require({path})")),

                    false => {
                        let temp = self.hoist_import(&path, anchor);
                        let text = format!("local {base} = {temp}.default");

                        (temp, text)
                    }
                };
                let mut names = Vec::new();
                let mut values = Vec::new();
                let mut types = Vec::new();

                for sp in specs {
                    let name = self.text_of(sp.name).to_string();
                    let local = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    let args = self.module_type_params(&path, &name);
                    let type_args = type_arguments(&args);

                    if sp.is_type {
                        types.push(format!("type {local}{args} = {temp}.{name}{type_args}"));
                    } else {
                        // A struct or an enum is a value and a type; the
                        // type comes along when the module exports one.
                        if self.module_exports_type(&path, &name) {
                            types.push(format!("type {local}{args} = {temp}.{name}{type_args}"));
                        }

                        types.extend(self.namespace_type_aliases(&path, &name, &local, &temp));
                        names.push(local);
                        values.push(format!("{temp}.{name}"));
                    }
                }

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

                self.generate(anchor, &text);
            }

            ImportKind::Named(specs) => {
                let temp = self.hoist_import(&path, anchor);
                let mut names = Vec::new();
                let mut values = Vec::new();
                let mut types = Vec::new();

                for sp in specs {
                    let name = self.text_of(sp.name).to_string();
                    let local = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    let args = self.module_type_params(&path, &name);
                    let type_args = type_arguments(&args);

                    if sp.is_type {
                        types.push(format!("type {local}{args} = {temp}.{name}{type_args}"));
                    } else {
                        if self.module_exports_type(&path, &name) {
                            types.push(format!("type {local}{args} = {temp}.{name}{type_args}"));
                        }

                        types.extend(self.namespace_type_aliases(&path, &name, &local, &temp));
                        names.push(local);
                        values.push(format!("{temp}.{name}"));
                    }
                }

                let mut text = String::new();

                if !names.is_empty() {
                    text.push_str(&format!(
                        "local {} = {}",
                        names.join(", "),
                        values.join(", ")
                    ));
                }

                for t in types {
                    if !text.is_empty() {
                        text.push(' ');
                    }

                    text.push_str(&t);
                }

                self.generate(anchor, &text);
            }

            ImportKind::TypeOnly(specs) => {
                // The whole statement exists for the type checker. With
                // `erase_type_imports` it is blanked to ship: every output
                // chunk anchored inside it goes.
                if self.options.erase_type_imports {
                    self.ship_blanks
                        .push((self.byte_start(i.span), self.byte_end(i.span)));
                }
                let temp = self.hoist_import(&path, anchor);
                let parts: Vec<String> = specs
                    .iter()
                    .map(|sp| {
                        let name = self.text_of(sp.name).to_string();
                        let local = sp
                            .alias
                            .map(|a| self.text_of(a).to_string())
                            .unwrap_or(name.clone());

                        let args = self.module_type_params(&path, &name);
                        let type_args = type_arguments(&args);

                        format!("type {local}{args} = {temp}.{name}{type_args}")
                    })
                    .collect();
                self.generate(anchor, &parts.join(" "));
            }
        }
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
            ImportKind::Namespace(n) | ImportKind::Default(n) => {
                lines.push(named(self, *n, None));
            }

            ImportKind::Both(n, specs) => {
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

    /// An `import` inside a function or a block. The emit lifts every
    /// `require` to the top of the file, so a buried one binds nothing
    /// where it stands.
    pub(crate) fn check_import_places(&mut self, block: &Block) {
        let mut buried = Vec::new();

        for stmt in &block.stmts {
            crate::desugar::modules::buried_imports(stmt, &mut buried);
        }

        for span in buried {
            self.diagnose(
                span,
                "an import belongs at the top level of a file; the emit lifts the require above the block it sits in",
            );
        }
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
                        let known = match e.type_only || sp.is_type {
                            true => types.contains(&name) || values.contains(&name),

                            false => values.contains(&name),
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
                        if !(e.type_only || sp.is_type) {
                            let out = sp.alias.unwrap_or(sp.name);
                            sent.push((self.text_of(out).to_string(), out));
                        }
                    }
                }

                // A `global` exports too, and two globals of one name
                // are the globals check's report, not this one.
                other
                    if crate::desugar::namespaces::is_exported(other.under_default())
                        && !crate::globals::is_global(other.under_default()) =>
                {
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
            Stmt::Local(l) => {
                for b in &l.names {
                    match b.destructure {
                        Some(_) => {}

                        None => value(values, b.name),
                    }
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
                ImportKind::Namespace(n) | ImportKind::Default(n) => value(values, *n),

                ImportKind::Both(n, specs) => {
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
                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(&name).to_string());

                    // A type is no value: the module sends it out as an
                    // alias, not as a field of the export table. A
                    // namespace member reads by the name the emit gave
                    // it, `Geom_Point`.
                    if e.type_only || sp.is_type {
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
                        self.exports.push((exported, name));
                    }
                }

                if !types.is_empty() {
                    self.generate(anchor, &types.join(" "));
                    self.blank_lines(anchor, self.byte_end(e.span));
                }
            }

            Some(path) => {
                let path = crate::data::strip_literal(self.text_of(path));
                let temp = self.hoist_import(&path, anchor);
                let mut types = Vec::new();

                for sp in &e.specs {
                    let name = self.text_of(sp.name).to_string();
                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());

                    if e.type_only || sp.is_type {
                        types.push(format!("export type {exported} = {temp}.{name}"));
                    } else {
                        self.exports.push((exported, format!("{temp}.{name}")));
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
                    self.exports.push(("default".to_string(), name));
                }

                // `function f()` at the top level is a global; the name
                // a module exports is its own.
                if matches!(inner.as_ref(), Stmt::Function(_)) {
                    self.generate(self.byte_start(inner.span()), "local ");
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
    ///
    /// A `global local` is one value for the whole project, so its slot
    /// is on the table the module returns. The local still stands: it
    /// carries the annotation the author wrote and the type the value
    /// infers, and the line right after it puts the value in the slot.
    pub(crate) fn exported_local(&mut self, span: TokSpan, l: &Local) {
        let shared = l.global && !l.is_const && self.declares_shared_globals();
        let names: Vec<String> = l
            .names
            .iter()
            .map(|b| self.text_of(b.name).to_string())
            .collect();

        for name in &names {
            if !shared {
                self.exports.push((name.clone(), name.clone()));
            }
        }

        // The attributes stand above the word, and the span opens on
        // the first of them. Their lines go blank; the `export` or
        // `global` word after them goes, and the rest renders as a
        // plain local.
        for a in &l.attrs {
            self.blank_lines(self.byte_start(a.span), self.byte_end(a.span));
        }

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

        if local_needs_rewrite(l) {
            self.local_stmt(l);
        } else {
            let children: Vec<Child<'_>> = l.values.iter().map(Child::Expr).collect();
            self.stitch(rest, &children, |d, child| match child {
                Child::Expr(e) => d.expr(e),

                Child::Block(b) => d.block(b),

                Child::Function(b) => d.function_block(b),
            });
        }

        if shared {
            let slots: Vec<String> = names
                .iter()
                .map(|n| format!("{GLOBAL_STATE}.{n}"))
                .collect();
            let at = self.byte_end(span);
            self.generate(at, &format!(" {} = {}", slots.join(", "), names.join(", ")));
        }
    }

    /// Whether this file owns a `global local`, so the module returns
    /// the table its values live on.
    pub(crate) fn declares_shared_globals(&self) -> bool {
        !self.own_mutable.is_empty()
    }

    /// The export table, appended after the last token. The test
    /// artifact has no module to return: the spec's footer follows.
    pub(crate) fn module_return(&mut self, at: u32, block: &Block) {
        if self.options.tests {
            return;
        }

        // A module that exports only types binds no value, and Luau
        // requires a module to return exactly one. It returns an empty
        // table; the `export type` lines stand on their own.
        let shared = self.declares_shared_globals();
        let types_only = self.exports.is_empty() && !shared;

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

        // The `global local` values already sit on `_gs`, so the
        // module returns that table with its exports put in. A fresh
        // table would copy the values and every file would hold its own.
        if shared {
            let mut text = String::new();

            for (k, v) in &self.exports {
                text.push_str(&format!(" {GLOBAL_STATE}.{k} = {v}"));
            }

            text.push_str(&format!(" return {GLOBAL_STATE}"));
            self.generate(at, &text);

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
fn type_arguments(params: &str) -> String {
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

/// Every `import` under a statement, at any depth.
pub(crate) fn buried_imports(stmt: &Stmt, out: &mut Vec<TokSpan>) {
    for child in crate::desugar::stmt_children(stmt) {
        match child {
            crate::desugar::Child::Block(b) => block_imports(b, out),

            crate::desugar::Child::Function(f) => block_imports(&f.block, out),

            crate::desugar::Child::Expr(_) => {}
        }
    }
}

fn block_imports(block: &Block, out: &mut Vec<TokSpan>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Import(i) => out.push(i.span),

            other => buried_imports(other, out),
        }
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

    #[test]
    fn a_parameter_list_refers_by_name_alone() {
        assert_eq!(super::type_arguments("<T = nil>"), "<T>");
        assert_eq!(super::type_arguments("<T = nil, U: Bound>"), "<T, U>");
        assert_eq!(super::type_arguments("<K, V = { a: number }>"), "<K, V>");
        assert_eq!(super::type_arguments("<T...>"), "<T...>");
        assert_eq!(super::type_arguments(""), "");
    }
}
