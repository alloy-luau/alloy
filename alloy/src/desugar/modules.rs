//! Import, export, and module-return lowering.

use alloy_syntax::ast::{
    Block, DefaultExport, ExportList, Expr, Import, ImportKind, Local, Stmt, TokSpan,
};

use super::*;

/// The local an `export default <expr>` binds, when the expression is
/// not already a name. The export table is written after the last line,
/// so the value needs a binding to name there.
pub(crate) const DEFAULT_LOCAL: &str = "_default";

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

    pub(crate) fn import_stmt(&mut self, i: &Import) {
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

    pub(crate) fn export_list(&mut self, e: &ExportList) {
        let anchor = self.byte_start(e.span);

        match e.from {
            None => {
                for sp in &e.specs {
                    let name = self.text_of(sp.name).to_string();
                    let exported = sp
                        .alias
                        .map(|a| self.text_of(a).to_string())
                        .unwrap_or(name.clone());
                    self.exports.push((exported, name));
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
    pub(crate) fn exported_local(&mut self, span: TokSpan, l: &Local) {
        for b in &l.names {
            let name = self.text_of(b.name).to_string();
            self.exports.push((name.clone(), name));
        }

        // Skip the `export` token; render the rest as a plain local.
        let rest = TokSpan::new(span.start as usize + 1, span.end as usize);

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

#[cfg(test)]
mod tests {
    #[test]
    fn a_parameter_list_refers_by_name_alone() {
        assert_eq!(super::type_arguments("<T = nil>"), "<T>");
        assert_eq!(super::type_arguments("<T = nil, U: Bound>"), "<T, U>");
        assert_eq!(super::type_arguments("<K, V = { a: number }>"), "<K, V>");
        assert_eq!(super::type_arguments("<T...>"), "<T...>");
        assert_eq!(super::type_arguments(""), "");
    }
}
