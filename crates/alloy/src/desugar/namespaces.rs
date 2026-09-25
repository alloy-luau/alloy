//! `namespace Name as ... end`: one table over a group of declarations.
//!
//! A namespace has no Luau form, so the emit gives each member a name of
//! its own and puts it on a table. `struct Vec2` inside `namespace Math`
//! renders as `Math_Vec2`, and the header line assigns `Math.Vec2`. The
//! rendered name is what a type slot reads, so `Math.Vec2` in a type
//! position becomes `Math_Vec2`; the proxy folds the name back for the
//! reader.
//!
//! Every line of the source keeps its line in both artifacts: the header
//! and the closing `end` carry the generated text, and a member renders
//! where it stands.

use alloy_syntax::ast::{
    Attr, Block, FunctionBody, ImportKind, NamespaceDecl, NamespaceMember, Stmt, TokSpan,
};

use super::Desugar;

/// What one member of a namespace binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NsMember {
    /// The name the source wrote.
    pub name: String,
    /// The name the emit renders it under, `Math_Vec2`.
    pub rendered: String,
    /// `private member`: a use from outside the namespace is an error.
    pub private: bool,
    /// The name binds a value, so the table carries it.
    pub value: bool,
    /// The name is a type, so a type slot reads the rendered name.
    pub ty: bool,
    /// The member is a namespace of its own.
    pub nested: bool,
}

/// One namespace a file declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamespaceInfo {
    /// The name the source wrote.
    pub name: String,
    /// The table path the emit writes: `Math`, or `Math.Geo` for a
    /// nested one.
    pub path: String,
    /// What a member's rendered name starts with: `Math_`.
    pub prefix: String,
    pub members: Vec<NsMember>,
    /// The namespace this one sits in, by its key.
    pub parent: Option<String>,
    /// The byte range of the header, for a message.
    pub start: u32,
    pub end: u32,
    /// The module exposes the namespace, so its type members carry
    /// `export` and another file can name them.
    pub exported: bool,
}

impl NamespaceInfo {
    pub fn member(&self, name: &str) -> Option<&NsMember> {
        self.members.iter().find(|m| m.name == name)
    }
}

/// One namespace member a sibling body names above its declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NsHoist {
    /// The key of the namespace the member belongs to.
    pub key: String,
    /// The name the header declares, `Suite_helper`.
    pub rendered: String,
    /// The token the member's own name sits at.
    pub name_tok: u32,
}

/// One namespace under render: its key, and the scope depth its body
/// opened at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NsFrame {
    pub key: String,
    /// How many scopes stood open when the body started. A local
    /// declared at or past this depth is inside the namespace and
    /// shadows a member; one below it does not.
    pub scope: usize,
}

/// The key a namespace is indexed under: its path with `_` for the dot,
/// so a nested one never collides with a top-level namespace.
pub(crate) fn key_of(parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(p) => format!("{p}_{name}"),

        None => name.to_string(),
    }
}

impl<'s> Desugar<'s> {
    /// Reads every namespace of a block, with the members each one
    /// binds and the name the emit gives them. The walk runs before the
    /// prescan, so a member is known before any statement names it.
    pub(crate) fn scan_namespaces(&mut self, block: &Block) {
        // `namespace Math as ... end` and `export { Math }` below it
        // export the group the way `export namespace` does.
        // The types the file declares, so an `export { T }` list can
        // say which name is one of its own.
        for stmt in &block.stmts {
            let (name, value, exported) = match stmt.under_default() {
                Stmt::TypeAlias(t) => (t.name, false, t.exported),

                Stmt::Interface(i) => (i.name, false, i.exported),

                Stmt::Struct(s) => (s.name, true, s.exported),

                Stmt::Enum(e) => (e.name, true, e.exported),

                Stmt::Trait(t) => (t.name, true, t.exported),

                _ => continue,
            };

            if !exported {
                self.file_types
                    .insert(self.text_of(name).to_string(), value);
            }
        }

        // Luau makes a type of a definitions file global only when it
        // says `export`, and a `.d.aly` declares globals, so every type
        // takes the word. A `__` name is a helper the emit wrote, such
        // as the type function of a mapped type, and stays local.
        if self.options.definitions {
            self.export_listed_types.extend(
                self.file_types
                    .keys()
                    .filter(|name| !name.starts_with("__"))
                    .cloned(),
            );
        }

        self.scan_imported_types(&block.stmts);

        for stmt in &block.stmts {
            let Stmt::ExportList(list) = stmt else {
                continue;
            };

            for spec in &list.specs {
                let name = self.text_of(spec.name).to_string();

                // `export { T }` of a type the file declares or
                // imports: Luau has no re-export for an alias, and
                // `export type T = T` is a cycle. The declaration, or
                // the import's own alias, takes the word instead, and
                // a struct or an enum sends its type out with its value.
                if list.from.is_none()
                    && spec.alias.is_none()
                    && (self.file_types.contains_key(&name)
                        || self.imported_types.contains_key(&name))
                {
                    self.export_listed_types.insert(name.clone());
                }

                if list.from.is_none() && spec.alias.is_none() {
                    self.export_listed_bare.insert(name.clone());
                }

                // `export { E } from "./m"` beside `import { E } from
                // "./m"`: each writes an alias of `E`, and Luau reads the
                // second as a redefinition. The import's alias takes the
                // word, and the list writes none.
                if let Some(from) = list.from
                    && spec.alias.is_none()
                    && self.imports_by_own_name(&block.stmts, self.text_of(from), &name)
                {
                    self.export_listed_types.insert(name.clone());
                    self.export_listed_bare.insert(name.clone());
                }

                self.export_listed.insert(name);
            }
        }

        self.scan_imported_namespaces(&block.stmts);
        self.scan_namespaces_in(&block.stmts, None);
    }

    /// The namespace members a sibling body names above their
    /// declaration. Luau reads a local from its declaration down, so a
    /// member that calls one below it would call nil. The header line
    /// declares the name, and the declaration fills it. A use at the
    /// namespace's own level runs before the declaration, so it
    /// reports instead.
    pub(crate) fn scan_ns_hoists(&mut self, block: &Block) {
        self.scan_ns_hoists_in(&block.stmts, None);
    }

    fn scan_ns_hoists_in(&mut self, stmts: &[Stmt], parent: Option<&str>) {
        for stmt in stmts {
            let Stmt::Namespace(ns) = stmt.under_default() else {
                continue;
            };
            let key = key_of(parent, self.text_of(ns.name));

            self.ns_hoists_of(ns, &key);

            let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();

            for one in inner {
                self.scan_ns_hoists_in(std::slice::from_ref(one), Some(&key));
            }
        }
    }

    /// The hoists one namespace asks for, and the reports its own
    /// level earns.
    fn ns_hoists_of(&mut self, ns: &NamespaceDecl, key: &str) {
        let Some(info) = self.namespaces.get(key).cloned() else {
            return;
        };
        let Some(start) = ns.members.first().map(|m| m.span.start as usize) else {
            return;
        };
        let mut bodies = Vec::new();

        for m in &ns.members {
            super::stmts_function_spans(std::slice::from_ref(&m.stmt), &mut bodies);
        }

        for m in &ns.members {
            let Some(span) = hoistable_name(m.stmt.under_default()) else {
                continue;
            };
            let name = self.text_of(span);
            // A member the emit leaves under its own name needs no
            // header line: the source name is the Luau name.
            let Some(rendered) = info
                .member(name)
                .map(|x| x.rendered.clone())
                .filter(|r| r != name && !r.contains('.'))
            else {
                continue;
            };
            let mut deferred = false;

            for k in start..m.span.start as usize {
                if !self.reads_name(k, name, true) {
                    continue;
                }

                if bodies
                    .iter()
                    .any(|b| (b.start as usize..b.end as usize).contains(&k))
                {
                    deferred = true;
                    continue;
                }

                let message =
                    format!("`{name}` is declared below this use; move the function above it");
                self.diagnose(TokSpan::new(k, k + 1), &message);
                break;
            }

            if deferred {
                self.ns_hoisted.push(NsHoist {
                    key: key.to_string(),
                    rendered,
                    name_tok: span.start,
                });
            }
        }
    }

    /// The types the file imports, under the name they bind here. A
    /// module that re-exports one of them sends the type on, and the
    /// import's own alias is where the `export` word goes.
    fn scan_imported_types(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            let Stmt::Import(i) = stmt else {
                continue;
            };
            let path = crate::data::strip_literal(self.text_of(i.path));
            let (specs, type_only) = match &i.kind {
                ImportKind::Named(v) | ImportKind::Both(_, v) | ImportKind::Namespace(_, v) => {
                    (v, false)
                }

                ImportKind::TypeOnly(v) => (v, true),

                ImportKind::Default(_) => continue,
            };

            for sp in specs {
                let name = self.text_of(sp.name).to_string();
                let local = sp
                    .alias
                    .map(|a| self.text_of(a).to_string())
                    .unwrap_or_else(|| name.clone());
                let is_type = type_only || sp.is_type || self.module_exports_type(&path, &name);

                if !is_type {
                    continue;
                }

                let value =
                    !(type_only || sp.is_type || self.module_exports_type_only(&path, &name));
                self.imported_types.insert(local, value);
            }
        }
    }

    /// Whether an import of the file binds `name` under its own name
    /// from the module `quoted` names.
    fn imports_by_own_name(&self, stmts: &[Stmt], quoted: &str, name: &str) -> bool {
        let bare = |q: &str| q.trim_matches(['"', '\'']).to_string();

        stmts.iter().any(|stmt| {
            let Stmt::Import(i) = stmt else {
                return false;
            };
            let specs = match &i.kind {
                ImportKind::Named(v)
                | ImportKind::Both(_, v)
                | ImportKind::Namespace(_, v)
                | ImportKind::TypeOnly(v) => v,

                ImportKind::Default(_) => return false,
            };

            bare(self.text_of(i.path)) == bare(quoted)
                && specs
                    .iter()
                    .any(|sp| sp.alias.is_none() && self.text_of(sp.name) == name)
        })
    }

    /// The namespaces the file imports. A module that exports
    /// `namespace Math` exports its types as `Math_Vec2`, so the names
    /// the import brought in say which namespaces reached this file.
    fn scan_imported_namespaces(&mut self, stmts: &[Stmt]) {
        if self.options.import_types.is_empty() {
            return;
        }

        for stmt in stmts {
            let Stmt::Import(i) = stmt else {
                continue;
            };
            let spec = self.text_of(i.path).to_string();
            let bare = spec.trim_matches(['"', '\'']).to_string();
            let specs = match &i.kind {
                ImportKind::Named(v) | ImportKind::TypeOnly(v) | ImportKind::Both(_, v) => {
                    v.clone()
                }

                // `import * as M` binds the whole module, so a namespace
                // of it reads one level deeper: `M.Ns.Type`. The names in
                // braces beside it read off the same local.
                ImportKind::Namespace(n, v) => {
                    let local = self.text_of(*n).to_string();

                    self.scan_star_namespace(&bare, &local);

                    v.clone()
                }

                _ => continue,
            };

            for sp in specs {
                let name = self.text_of(sp.name).to_string();
                let local = sp
                    .alias
                    .map(|a| self.text_of(a).to_string())
                    .unwrap_or_else(|| name.clone());
                let head = format!("{name}_");
                let members: Vec<NsMember> = self
                    .options
                    .import_types
                    .iter()
                    .filter(|(s, _)| *s == bare)
                    .flat_map(|(_, types)| types.iter())
                    .filter_map(|entry| {
                        let full = crate::modules::type_head(entry);
                        let rest = full.strip_prefix(&head)?;

                        Some(NsMember {
                            name: rest.to_string(),
                            rendered: format!("{local}_{rest}"),
                            private: false,
                            value: true,
                            ty: true,
                            nested: false,
                        })
                    })
                    .collect();

                if members.is_empty() {
                    continue;
                }

                self.namespaces.insert(
                    local.clone(),
                    NamespaceInfo {
                        name: local.clone(),
                        path: local,
                        prefix: format!("{name}_"),
                        members,
                        parent: None,
                        start: 0,
                        end: 0,
                        exported: false,
                    },
                );
            }
        }
    }

    /// The namespaces a star import reaches. `import * as M` binds the
    /// module table, so a namespace of the module reads `M.Ns.Type`, one
    /// level deeper than `import { Ns }` reads it. The module exports the
    /// type as `Ns_Type`, and `M.Ns_Type` is a Luau type path, so the
    /// member renders under the local and asks for no alias.
    fn scan_star_namespace(&mut self, bare: &str, local: &str) {
        let members: Vec<NsMember> = self
            .options
            .import_types
            .iter()
            .filter(|(s, _)| s == bare)
            .flat_map(|(_, types)| types.iter())
            .map(|entry| crate::modules::type_head(entry))
            .filter(|full| full.contains('_'))
            .map(|full| NsMember {
                name: full.to_string(),
                rendered: format!("{local}.{full}"),
                private: false,
                value: false,
                ty: true,
                nested: false,
            })
            .collect();

        if members.is_empty() {
            return;
        }

        self.namespaces.insert(
            local.to_string(),
            NamespaceInfo {
                name: local.to_string(),
                path: local.to_string(),
                prefix: String::new(),
                members,
                parent: None,
                start: 0,
                end: 0,
                exported: false,
            },
        );
    }

    fn scan_namespaces_in(&mut self, stmts: &[Stmt], parent: Option<&str>) {
        for stmt in stmts {
            let Stmt::Namespace(ns) = stmt.under_default() else {
                continue;
            };
            let name = self.text_of(ns.name).to_string();
            let key = key_of(parent, &name);
            let (parent_path, parent_prefix) = match parent.and_then(|p| self.namespaces.get(p)) {
                Some(i) => (i.path.clone(), i.prefix.clone()),

                None => (String::new(), String::new()),
            };
            let (path, prefix) = match parent {
                Some(_) => (
                    format!("{parent_path}.{name}"),
                    format!("{parent_prefix}{name}_"),
                ),

                None => (name.clone(), format!("{name}_")),
            };
            // A namespace an `export { ... }` list names exports too.
            let exported = ns.exported
                || match parent {
                    Some(p) => self.namespaces.get(p).is_some_and(|i| i.exported),

                    None => self.export_listed.contains(&name),
                };
            let mut members = Vec::new();

            for m in &ns.members {
                let private = m.is_private(self.src, self.toks);

                for b in member_bindings(m, self.src, self.toks) {
                    let member = self.text_of(b.name).to_string();
                    let rendered = match b.nested {
                        true => format!("{path}.{member}"),

                        false => format!("{prefix}{member}"),
                    };

                    if b.prefixed {
                        self.member_names.insert(b.name.start, rendered.clone());
                    }

                    members.push(NsMember {
                        name: member,
                        rendered: match b.prefixed || b.nested {
                            true => rendered,

                            false => self.text_of(b.name).to_string(),
                        },
                        private,
                        value: b.value,
                        ty: b.ty,
                        nested: b.nested,
                    });
                }
            }

            self.namespaces.insert(
                key.clone(),
                NamespaceInfo {
                    name,
                    path,
                    prefix,
                    members,
                    parent: parent.map(str::to_string),
                    start: self.byte_start(ns.span),
                    end: self.byte_end(ns.span),
                    exported,
                },
            );

            // A nested namespace reads the outer prefix, so the entry
            // above has to exist before the walk goes in.
            let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();

            for one in inner {
                self.scan_namespaces_in(std::slice::from_ref(one), Some(&key));
            }
        }
    }

    /// The name a bare reference takes inside a namespace body: the
    /// innermost namespace that declares it wins, and a local of the
    /// same name shadows it.
    pub(crate) fn ns_member_name(&self, name: &str) -> Option<String> {
        if self.ns_stack.is_empty() {
            return None;
        }

        for frame in self.ns_stack.iter().rev() {
            let Some(info) = self.namespaces.get(&frame.key) else {
                continue;
            };
            let Some(m) = info.member(name) else {
                continue;
            };

            // A local of the namespace body shadows the member. A local
            // of the file around it does not: the member is the nearer
            // declaration.
            if self.is_local_since(frame.scope, name) {
                return None;
            }

            return match m.rendered == name {
                true => None,

                false => Some(m.rendered.clone()),
            };
        }

        None
    }

    /// The path a member of the namespace under render is named by
    /// from outside: `Testing.tag`, and `Outer.Inner.tag` through a
    /// nested namespace. `None` at the top level.
    pub(crate) fn ns_member_path(&self, name: &str) -> Option<String> {
        let key = &self.ns_stack.last()?.key;
        let info = self.namespaces.get(key)?;

        Some(format!("{}.{name}", info.path))
    }

    /// The keys a compile-time name reads under, innermost first: a
    /// member of the namespace under render, then a member of the one
    /// around it, then a name of the file. A name the source wrote as
    /// a path reads as written.
    ///
    /// A macro and an attribute are keyed by their path, so a member
    /// named `tag` and a file-level `tag` stay two declarations.
    pub(crate) fn ns_scope_keys(&self, name: &str) -> Vec<String> {
        if name.contains('.') {
            return vec![name.to_string()];
        }

        let mut out: Vec<String> = self
            .ns_stack
            .iter()
            .rev()
            .filter_map(|f| self.namespaces.get(&f.key))
            .map(|i| format!("{}.{name}", i.path))
            .collect();

        out.push(name.to_string());

        out
    }

    /// What a compile-time name reads in a map of macros or of
    /// attributes. The innermost namespace under render wins, then a
    /// name of the file.
    ///
    /// A member of a namespace reads by its bare name inside the body
    /// that declares it, and by its path from outside. There is no
    /// last-resort scan for the bare name: two namespaces can declare
    /// one name, and the map is a `HashMap`, so the winner followed
    /// the hash order.
    pub(crate) fn scoped_decl<'m, T>(
        &self,
        map: &'m std::collections::HashMap<String, T>,
        name: &str,
    ) -> Option<&'m T> {
        self.ns_scope_keys(name).iter().find_map(|k| map.get(k))
    }

    /// The report a bare name earns when only a namespace declares it.
    /// `kind` is `an attribute` or `a macro`, and `sigil` is what the
    /// use writes in front of the path. `None` when no namespace holds
    /// the name. Every namespace that declares it lands in the hint.
    pub(crate) fn ns_member_hint<T>(
        &self,
        map: &std::collections::HashMap<String, T>,
        name: &str,
        kind: &str,
        sigil: char,
    ) -> Option<String> {
        if name.contains('.') {
            return None;
        }

        let tail = format!(".{name}");
        let mut owners: Vec<&str> = map
            .keys()
            .filter_map(|k| k.strip_suffix(&tail))
            .filter(|o| self.is_namespace_path(o))
            .collect();

        owners.sort_unstable();

        if owners.is_empty() {
            return None;
        }

        let paths: Vec<String> = owners
            .iter()
            .map(|o| format!("`{sigil}{o}.{name}`"))
            .collect();

        Some(format!(
            "`{name}` is {kind} of {}; write {}",
            super::list_names(&owners),
            paths.join(" or ")
        ))
    }

    /// Whether a dotted path names a namespace of this file:
    /// `Outer.Inner` is the key `Outer_Inner`.
    pub(crate) fn is_namespace_path(&self, path: &str) -> bool {
        self.namespaces.contains_key(&path.replace('.', "_"))
    }

    /// The rendered name a dotted path through the file's namespaces
    /// names: `Zoo.Lion` is `Zoo_Lion`, and `A.B.S` through a nested
    /// namespace is `A_B_S`. `None` when the head names no namespace,
    /// or the path runs off its members.
    pub(crate) fn ns_path_name(&self, path: &str) -> Option<String> {
        let parts: Vec<&str> = path.split('.').map(str::trim).collect();
        let (head, rest) = parts.split_first()?;
        let mut key = (*head).to_string();
        let mut info = self.namespaces.get(&key)?;
        let mut at = 0;

        while at < rest.len() {
            // An imported namespace flattens its nesting into one member
            // name, `B_S`, because that is the type the module exports.
            if let Some(m) = info.member(&rest[at..].join("_")) {
                return Some(m.rendered.clone());
            }

            let m = info.member(rest[at])?;

            if !m.nested {
                return Some(m.rendered.clone());
            }

            key = key_of(Some(&key), rest[at]);
            info = self.namespaces.get(&key)?;
            at += 1;
        }

        None
    }

    /// The target an `impl` inside a namespace writes: a member of the
    /// namespace renders under its own name.
    pub(crate) fn impl_target_name(&self, span: TokSpan) -> String {
        let name = self.text_of(span).to_string();

        // `impl Zoo.Lion` targets the member the path names. The struct
        // renders as `Zoo_Lion`, and every index a struct's impl reads
        // is keyed by that name.
        match self
            .ns_member_name(&name)
            .or_else(|| self.ns_path_name(&name))
        {
            Some(r) => r,

            None => name,
        }
    }

    /// The type of a namespace inside the byte range, when one sits
    /// there: the range to replace and the name to write. `Math.Vec2`
    /// in a type slot is `Math_Vec2`, and inside `namespace Math` the
    /// bare `Vec2` is the same name.
    pub(crate) fn namespace_type_at(&self, start: u32, end: u32) -> Option<(u32, u32, String)> {
        if self.namespaces.is_empty() {
            return None;
        }

        let mut best: Option<(u32, u32, String)> = None;

        for span in &self.type_name_spans {
            let (s, e) = (self.byte_start(*span), self.byte_end(*span));

            if s < start || e > end {
                continue;
            }

            if best.as_ref().is_some_and(|(bs, _, _)| *bs <= s) {
                continue;
            }

            if let Some(hit) = self.namespace_type_of(*span, end) {
                best = Some(hit);
            }
        }

        // An edit that starts at or before the name owns the text; its
        // own copy comes back through here for the name. `M.G[]` is an
        // array edit that starts on the name itself.
        let (s, _, _) = best.as_ref()?;

        match self.earliest_edit_start(start, end) {
            Some(at) if at <= *s => None,

            _ => best,
        }
    }

    /// The rewrite one type name asks for, when it names a namespace or
    /// a type of the namespace under render.
    fn namespace_type_of(&self, span: TokSpan, limit: u32) -> Option<(u32, u32, String)> {
        let name = self.text_of(span).to_string();
        let (s, e) = (self.byte_start(span), self.byte_end(span));

        // `Math.Vec2`, and `Outer.Inner.Point` through a nested one.
        if let Some(info) = self.namespaces.get(&name) {
            let mut key = name.clone();
            let mut info = info;

            for (at, (member, fe)) in self.dotted_chain(span)?.iter().enumerate() {
                if *fe > limit {
                    return None;
                }

                // An imported namespace flattens its nesting into one
                // member name, `Ns_Shape`, because that is the type the
                // module exports. The longest path a member answers
                // wins, so `M.Ns.Shape` folds whole.
                if let Some((end, rendered)) = self.flat_member(info, span, at, limit) {
                    return Some((s, end, rendered));
                }

                let m = info.member(member)?;

                if !m.nested {
                    return m.ty.then(|| (s, *fe, m.rendered.clone()));
                }

                key = key_of(Some(&key), member);
                info = self.namespaces.get(&key)?;
            }

            return None;
        }

        // Inside the namespace the bare name reads the same type.
        let rendered = self.ns_type_name(&name)?;

        Some((s, e, rendered))
    }

    /// The `.field` steps after a type name, each with the byte its own
    /// name ends at: `M.Ns.Shape` gives `Ns` and `Shape`. `None` when a
    /// dot runs off the end of the tokens, as a half-written line does.
    fn dotted_chain(&self, span: TokSpan) -> Option<Vec<(String, u32)>> {
        let mut chain = Vec::new();
        let mut at = span.end as usize;

        while self.tok_text(at) == "." {
            if at + 2 > self.toks.len() {
                return None;
            }

            let field = TokSpan::new(at + 1, at + 2);

            chain.push((self.text_of(field).to_string(), self.byte_end(field)));
            at = field.end as usize;
        }

        Some(chain)
    }

    /// The member a flattened path names, from the step `at` to the end
    /// of the chain: `Ns_Shape` for `M.Ns.Shape`. The longest path a
    /// member answers wins. The byte the path ends at comes back with
    /// the name, and a member that is no type answers nothing.
    fn flat_member(
        &self,
        info: &NamespaceInfo,
        span: TokSpan,
        at: usize,
        limit: u32,
    ) -> Option<(u32, String)> {
        let chain = self.dotted_chain(span)?;

        for take in (at + 2..=chain.len()).rev() {
            let end = chain[take - 1].1;

            if end > limit {
                continue;
            }

            let flat = chain[at..take]
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join("_");

            if let Some(m) = info.member(&flat) {
                return m.ty.then(|| (end, m.rendered.clone()));
            }
        }

        None
    }

    /// The text of one token, or an empty string past the end. The
    /// lenient parse of a half-written line runs off it.
    fn tok_text(&self, at: usize) -> &'s str {
        match self.toks.get(at) {
            Some(t) => &self.src[t.start as usize..t.end as usize],

            None => "",
        }
    }

    /// The name a bare type takes inside a namespace body.
    fn ns_type_name(&self, name: &str) -> Option<String> {
        for frame in self.ns_stack.iter().rev() {
            let Some(info) = self.namespaces.get(&frame.key) else {
                continue;
            };
            let Some(m) = info.member(name) else {
                continue;
            };

            if !m.ty || m.rendered == name {
                return None;
            }

            return Some(m.rendered.clone());
        }

        None
    }

    /// What each namespace of the file needs to be true: one name per
    /// file, no `global` inside, and no use of a private member from
    /// outside.
    pub(crate) fn check_namespaces(&mut self, block: &Block) {
        self.check_namespace_names(&block.stmts, None);
        self.check_nesting(&block.stmts);
        self.check_member_exports(&block.stmts);
        self.check_private_uses();
    }

    /// A namespace inside a function or a block. The emit gives its
    /// members file-level names and a table the file returns, so the
    /// only places one stands are the top level and another namespace.
    fn check_nesting(&mut self, stmts: &[Stmt]) {
        let mut buried = Vec::new();

        for stmt in stmts {
            match stmt.under_default() {
                // The members are checked as their own level.
                Stmt::Namespace(ns) => {
                    let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();

                    for one in inner {
                        self.check_nesting(std::slice::from_ref(one));
                    }
                }

                other => buried_namespaces(other, &mut buried),
            }
        }

        for span in buried {
            self.diagnose(
                span,
                "a namespace goes at the top level of a file or inside another namespace; the emit gives its members names of the file",
            );
        }
    }

    /// `export` on a namespace member. `alloy doc namespace` gives a
    /// member `public` or `private`; the group is what `export` sends.
    fn check_member_exports(&mut self, stmts: &[Stmt]) {
        let mut hits: Vec<TokSpan> = Vec::new();
        collect_member_exports(stmts, &mut hits);

        for span in hits {
            // A member that wrote `global` has the removal report; one
            // message about the word is enough.
            if self.wrote_global(span) {
                continue;
            }

            let word = TokSpan::new(span.start as usize, span.start as usize + 1);
            let at = match self.text_of(word) == "export" {
                true => word,

                false => span,
            };
            self.diagnose(
                at,
                "`export` on a namespace member exports nothing; `export namespace` sends the group, and a member takes `public` or `private`",
            );
        }
    }

    fn check_namespace_names(&mut self, stmts: &[Stmt], parent: Option<&str>) {
        let mut seen: Vec<(String, TokSpan)> = Vec::new();

        for stmt in stmts {
            let Stmt::Namespace(ns) = stmt.under_default() else {
                continue;
            };
            let name = self.text_of(ns.name).to_string();

            // At the top level `check_duplicate_decls` reports the pair
            // with every other declaration; a nested pair is only here.
            match seen.iter().find(|(n, _)| *n == name) {
                Some((_, first)) if parent.is_some() => {
                    let line = self.line_of(self.byte_start(*first));
                    let message = format!(
                        "`{name}` is already a namespace on line {line}; one name holds one declaration"
                    );
                    self.diagnose(ns.name, &message);
                }

                Some(_) => {}

                None => seen.push((name.clone(), ns.name)),
            }

            let key = key_of(parent, &name);
            let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();

            for one in inner {
                self.check_namespace_names(std::slice::from_ref(one), Some(&key));
            }
        }
    }

    /// A private member named from outside its namespace. The scan runs
    /// over the tokens, so a type slot reports the way a value does.
    fn check_private_uses(&mut self) {
        if self.namespaces.is_empty() {
            return;
        }

        let mut hits: Vec<(TokSpan, String)> = Vec::new();

        for i in 0..self.toks.len().saturating_sub(2) {
            let head = TokSpan::new(i, i + 1);
            let dot = TokSpan::new(i + 1, i + 2);
            let field = TokSpan::new(i + 2, i + 3);

            if self.text_of(dot) != "." {
                continue;
            }

            // A field of another value spelled the same is not the
            // namespace: `t.Math.helper` names no namespace. A `:` in
            // front is a type annotation, and the type reads the same.
            if i > 0 && self.text_of(TokSpan::new(i - 1, i)) == "." {
                continue;
            }

            let name = self.text_of(head).to_string();
            let Some(info) = self.namespaces.get(&name) else {
                continue;
            };
            let member = self.text_of(field).to_string();
            let Some(m) = info.member(&member) else {
                continue;
            };

            if !m.private {
                continue;
            }

            // Inside the namespace the name is in scope; the path is
            // the long way to write it.
            let at = self.byte_start(head);

            if at >= info.start && at < info.end {
                continue;
            }

            hits.push((field, format!("`{member}` is private to `{}`", info.path)));
        }

        for (span, message) in hits {
            self.diagnose(span, &message);
        }
    }

    /// The name a dotted path renders under, when it names a member of
    /// a namespace: `Math.Shape` is `Math_Shape`, and `A.B.C` walks the
    /// nested namespaces.
    pub(crate) fn namespace_path_name(&self, path: &str) -> Option<String> {
        let mut parts = path.split('.');
        let head = parts.next()?;
        let mut key = head.to_string();
        let mut info = self.namespaces.get(head)?;

        for part in parts {
            let m = info.member(part)?;

            if !m.nested {
                return Some(m.rendered.clone());
            }

            key = key_of(Some(&key), part);
            info = self.namespaces.get(&key)?;
        }

        None
    }

    /// The enum a dotted pattern names, with the variant: `Math.Shape`
    /// of `case Math.Shape.Circle` is the enum `Math_Shape`.
    pub(crate) fn enum_of_path(&self, text: &str) -> Option<(String, String)> {
        // A macro body travels as tokens joined by spaces, so a path
        // reaches here as `Choice . Yes`. No name holds a space, so the
        // join drops the spaces out again.
        let text: String = text.split_whitespace().collect();
        let (head, variant) = text.rsplit_once('.')?;

        Some((self.enum_named(head)?, variant.to_string()))
    }

    /// The key `enums` holds for the enum a type names: `Kind`,
    /// `Geo.Kind`, `Kind?`, or `Opt<number>`.
    pub(crate) fn enum_named(&self, ty: &str) -> Option<String> {
        let head = ty.trim().trim_end_matches('?').split('<').next()?.trim();
        // A namespace this file declares renders its enum under one
        // name, `Geo_Kind`. An imported namespace has no declaration
        // here, so the enum index keys it by the path the source writes,
        // the way `import_struct_fields` keys a struct member.
        let rendered = self.namespace_path_name(head);
        // Inside its namespace an enum reads by its own name, `Kind`,
        // which renders as `Geo_Kind`.
        let member = self.ns_member_name(head);

        [rendered.as_deref(), member.as_deref(), Some(head)]
            .into_iter()
            .flatten()
            .find(|n| self.enums.contains_key(*n))
            .map(str::to_string)
    }

    /// The path a message names a declaration by. `Math_Vec2` is the
    /// name the emit writes; the reader knows it as `Math.Vec2`.
    pub(crate) fn display_name(&self, rendered: &str) -> String {
        for info in self.namespaces.values() {
            if let Some(m) = info.members.iter().find(|m| m.rendered == rendered) {
                return format!("{}.{}", info.path, m.name);
            }
        }

        rendered.to_string()
    }

    /// The rendered name of a declaration: a namespace member takes the
    /// namespace's prefix, and every other declaration keeps its own.
    pub(crate) fn decl_name(&self, span: TokSpan) -> String {
        match self.member_names.get(&span.start) {
            Some(n) => n.clone(),

            None => self.text_of(span).to_string(),
        }
    }

    /// The byte the header ends at: the name, or the `as` after it.
    fn namespace_head_end(&self, ns: &NamespaceDecl) -> u32 {
        let after = TokSpan::new(ns.name.end as usize, ns.name.end as usize + 1);

        match self.text_of(after) == "as" {
            true => self.byte_end(after),

            false => self.byte_end(ns.name),
        }
    }

    /// Renders `namespace Name as ... end`.
    pub(crate) fn namespace_decl(&mut self, ns: &NamespaceDecl) {
        let key = key_of(self.ns_stack.last().map(|f| f.key.as_str()), &{
            self.text_of(ns.name).to_string()
        });
        let Some(info) = self.namespaces.get(&key).cloned() else {
            return;
        };
        let start = self.byte_start(ns.span);
        let end_tok = self.toks[ns.span.end as usize - 1];
        // `@cfg(server)` on the group: each member's line on the table
        // runs only on that side, so the name is nil on the other.
        let mut cfg = None;

        for a in &ns.attributes {
            if a.name.map(|n| self.text_of(n)) == Some("cfg") {
                match self.cfg_condition(&a.args) {
                    Ok(cond) => cfg = Some(cond),

                    Err(message) => self.diagnose(a.span, &message),
                }
            }
        }

        // The attributes are read at compile time. Their lines stay,
        // blank, and the header sits on the declaration's own line.
        let decl_start = match ns.attributes.last() {
            Some(a) => self.toks[a.span.end as usize].start,

            None => start,
        };
        self.blank_lines(start, decl_start);
        // The header line opens the table. A nested namespace is a field
        // of the one around it, so it takes no `local`.
        let mut header = match info.parent.is_some() {
            true => format!("{} = {{}}", info.path),

            false => self.decl_head(&info.path).trim_end().to_string(),
        };
        // A member a sibling body reads above its declaration takes its
        // slot here. A bare `local f` is enough: the checker types the
        // slot from the `function f()` that fills it.
        let ahead: Vec<String> = self
            .ns_hoisted
            .iter()
            .filter(|h| h.key == key)
            .map(|h| h.rendered.clone())
            .collect();

        if !ahead.is_empty() {
            if !header.is_empty() {
                header.push(' ');
            }

            header.push_str(&format!("local {}", ahead.join(", ")));
        }

        self.generate(decl_start, &header);

        // The header text goes and its line stays. The `as` token ends
        // it, so the first member keeps the trivia in front of it.
        let head_end = self.namespace_head_end(ns);
        self.blank_lines(decl_start, head_end);
        let first = ns
            .members
            .first()
            .map(|m| self.byte_start(m.span))
            .unwrap_or(end_tok.start);
        self.copy(head_end, first);

        self.ns_stack.push(NsFrame {
            key: key.clone(),
            scope: self.scope_depth(),
        });
        let saved_export = self.ns_export;
        // `@test` on the group makes every public function of it a
        // test. The flag reaches a public nested namespace too, since
        // the group means "everything public here is a test". A group
        // of tests holds nothing the game runs, so the ship artifact
        // keeps none of it.
        let saved_test = self.ns_test;

        if has_attr(self, &ns.attributes, "test") {
            self.ns_test = true;

            if !self.options.tests {
                self.ship_blanks.push((start, self.byte_end(ns.span)));
            }
        }

        // A member exports nothing on its own; the group carries the
        // export. `check_member_exports` reports the `export` word.
        let mark = self.exports.len();
        let mut cursor = first;

        for m in &ns.members {
            let m_start = self.byte_start(m.span);
            self.copy(cursor, m_start);
            self.namespace_member(&info, m, cfg.as_deref());
            cursor = self.byte_end(m.span);
        }

        self.ns_export = saved_export;
        self.ns_test = saved_test;
        self.ns_stack.pop();
        self.exports.truncate(mark);
        self.copy(cursor, end_tok.start);
        // The closing `end` has nothing to close.
        self.blank_lines(end_tok.start, end_tok.end);

        // A definitions file returns no module, so a `local` table
        // reaches no other file. The closing line declares the name
        // ambient, which is what the group means in a `.d.aly`. A
        // nested one travels as a field of the outermost table.
        if self.options.definitions && info.parent.is_none() {
            self.generate(
                end_tok.start,
                &format!("declare {0}: typeof({0})", info.path),
            );
        }

        if ns.exported {
            self.exports.push((info.name.clone(), info.path.clone()));
        }
    }

    /// One member: the declaration under its rendered name, then the
    /// line that puts it on the table.
    fn namespace_member(&mut self, info: &NamespaceInfo, m: &NamespaceMember, cfg: Option<&str>) {
        let span = m.span;
        let start = self.byte_start(span);
        let stmt_start = self.byte_start(m.stmt.span());
        let private = m.is_private(self.src, self.toks);
        // A private member stays inside the module, whatever the
        // namespace does.
        self.ns_export = info.exported && !private;
        // A private member is no test, and a `@test` on the group does
        // not reach into it.
        let saved_test = self.ns_test;
        self.ns_test = self.ns_test && !private;

        // `private` and `public` are Alloy's; Luau reads none of them.
        if start < stmt_start {
            self.blank_lines(start, stmt_start);
        }

        // A declaration whose head copies from source takes the prefix
        // as an insert; one whose head is generated reads `decl_name`.
        for b in member_bindings(m, self.src, self.toks) {
            if !b.prefixed || !copies_its_name(m.stmt.under_default()) {
                continue;
            }

            let at = self.byte_start(b.name);
            self.inserts.push((at, info.prefix.clone()));
        }

        // A type alias copies its own head, so the modifier goes in
        // front of it.
        if self.ns_export && matches!(m.stmt.under_default(), Stmt::TypeAlias(_)) {
            self.generate(stmt_start, "export ");
        }

        // A member never leaks into the file, so a plain `function f()`
        // takes the `local` a Luau global would not have. With
        // attributes above it the modifier goes after their lines, so
        // the attributed path writes it instead.
        // `export function f` already writes the `local` itself, in the
        // export arm of `stmt`.
        // One the header line declared drops the `local` too: a second
        // slot would leave the first one nil.
        if let Stmt::Function(f) = m.stmt.under_default()
            && f.path.len() == 1
            && !f.exported
            && !self.is_hoisted_fn(f.path[0])
        {
            match f.attrs.is_empty() {
                true => self.generate(stmt_start, "local "),

                false => self.ns_force_local = true,
            }
        }

        // `declare function f(p: P): R` inside a namespace has no body
        // to render, so it becomes the local slot the table reads:
        // `local Name_f: (p: P) -> R`. Copying the type from the source
        // keeps the rewrite a namespace type asks for.
        let declared = match m.stmt.under_default() {
            Stmt::Declare(d) => declare_head(d.span, self.src, self.toks),

            _ => None,
        };

        if let Some(h) = declared {
            let name_start = self.byte_start(h.name);

            self.generate(stmt_start, "local ");
            self.generate(name_start, &info.prefix);
            self.copy(name_start, self.byte_end(h.name));
            self.generate(self.byte_end(h.name), ": ");
            self.copy(self.byte_start(h.ty), self.byte_end(h.ty));

            match h.ret {
                Some(r) => {
                    self.generate(self.byte_end(h.ty), " -> ");
                    self.copy(self.byte_start(r), self.byte_end(r));
                }

                // A `declare function` with no return type returns
                // nothing, the way Luau reads the same head.
                None if h.ret.is_none() && self.text_of(h.ty).starts_with('(') => {
                    self.generate(self.byte_end(h.ty), " -> ()");
                }

                None => {}
            }
        } else {
            self.stmt(&m.stmt);
        }

        // The table takes every public member that binds a value. A
        // private one stays a local, so `Math.helper` finds nothing at
        // run time either.
        let mut tail = String::new();

        // `@test` on the group registers each public function of it.
        // A member that carries its own `@test` registers through the
        // attribute, so the group leaves it alone.
        if self.ns_test
            && let Some((name, body, attrs)) = member_function(m.stmt.under_default())
            && !has_attr(self, attrs, "test")
        {
            let path = format!("{}.{}", info.path, self.text_of(name));
            let rendered = self.decl_name(name);
            self.test_names
                .push((path.clone(), body.is_async.is_some()));

            if !self.options.tests {
                let std = self.std();
                tail.push_str(&format!(
                    " {std}.test({}, {rendered})",
                    super::luau_string(&path)
                ));
            }
        }

        // A `local` member is one variable. The ship artifact reaches it
        // from the table through accessors, since a copy would miss every
        // later write. The check artifact keeps the copy for its type.
        let variable = matches!(m.stmt.under_default(), Stmt::Local(l) if self.text_of(l.keyword) == "local")
            && !self.options.check;

        for b in member_bindings(m, self.src, self.toks) {
            if !b.value || b.nested || private {
                continue;
            }

            let name = self.text_of(b.name).to_string();
            let rendered = self.decl_name(b.name);

            if rendered == name {
                continue;
            }

            if variable {
                let std = self.std();
                tail.push_str(&format!(
                    " {std}.ns_local({}, {}, function() return {rendered} end, function(v) {rendered} = v end)",
                    info.path,
                    super::luau_string(&name)
                ));
            } else {
                tail.push_str(&format!(" {}.{name} = {rendered}", info.path));
            }
        }

        if !tail.is_empty() {
            let text = match cfg {
                Some(cond) => format!(" if {cond} then{tail} end"),

                None => tail,
            };
            self.generate(self.byte_end(span), &text);
        }

        self.ns_test = saved_test;
    }
}

/// Whether an attribute list holds `name`.
fn has_attr(d: &Desugar<'_>, attrs: &[Attr], name: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.name.is_some_and(|n| d.text_of(n) == name))
}

/// A namespace member that declares a function, as its name, its body
/// and its attributes. A dotted path names a member of another value,
/// and every other statement is no function.
fn member_function(stmt: &Stmt) -> Option<(TokSpan, &FunctionBody, &[Attr])> {
    match stmt {
        Stmt::Function(f) if f.path.len() == 1 => Some((f.path[0], &f.body, f.attrs.as_slice())),

        Stmt::LocalFunction(f) => Some((f.name, &f.body, f.attrs.as_slice())),

        _ => None,
    }
}

/// The name a namespace member declares as a plain function, the one
/// form the header line can declare ahead of. An `export` member
/// writes its own `local`, and a dotted path names no member.
fn hoistable_name(stmt: &Stmt) -> Option<TokSpan> {
    match stmt {
        Stmt::Function(f) if f.path.len() == 1 && !f.exported => Some(f.path[0]),

        Stmt::LocalFunction(f) if !f.exported => Some(f.name),

        _ => None,
    }
}

/// Every namespace under a statement, at any depth. A namespace of
/// its own is not one: its members are their own level.
fn buried_namespaces(stmt: &Stmt, out: &mut Vec<TokSpan>) {
    for child in crate::desugar::stmt_children(stmt) {
        match child {
            crate::desugar::Child::Block(b) => block_namespaces(b, out),

            crate::desugar::Child::Function(f) => block_namespaces(&f.block, out),

            crate::desugar::Child::Expr(_) => {}
        }
    }
}

fn block_namespaces(block: &Block, out: &mut Vec<TokSpan>) {
    for stmt in &block.stmts {
        match stmt.under_default() {
            Stmt::Namespace(ns) => out.push(ns.name),

            other => buried_namespaces(other, out),
        }
    }
}

/// Whether a declaration's emit copies its own name from the source.
/// Those take the namespace prefix as an insert; every other head is
/// generated and reads `decl_name`.
fn copies_its_name(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Function(_) | Stmt::LocalFunction(_) | Stmt::Local(_) | Stmt::TypeAlias(_)
    )
}

/// The parts of a `declare` member: the name it binds, the type text
/// that follows it, and whether the source spelled it as a function.
/// `declare class` and `declare extern type` name a type with no value,
/// so neither is a member and both answer `None`.
pub(crate) struct DeclareHead {
    pub name: TokSpan,
    /// `(params)` of a `declare function`, or the type of `declare x: T`.
    pub ty: TokSpan,
    /// The return type of a `declare function`, when it writes one.
    pub ret: Option<TokSpan>,
}

pub(crate) fn declare_head(
    span: TokSpan,
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
) -> Option<DeclareHead> {
    let text = |i: usize| match toks.get(i) {
        Some(t) => &src[t.start as usize..t.end as usize],

        None => "",
    };
    let first = span.start as usize;
    let last = span.end as usize;

    if text(first + 1) != "function" {
        // `declare x: T`.
        return (text(first + 2) == ":" && first + 3 < last).then(|| DeclareHead {
            name: TokSpan::new(first + 1, first + 2),
            ty: TokSpan::new(first + 3, last),
            ret: None,
        });
    }

    let name = TokSpan::new(first + 2, first + 3);
    // The parameter list ends where the paren depth returns to zero.
    // The `:` after it opens the return type.
    let mut depth = 0usize;
    let mut close = None;

    for i in name.end as usize..last {
        match text(i) {
            "(" => depth += 1,

            ")" => {
                depth -= 1;

                if depth == 0 {
                    close = Some(i);

                    break;
                }
            }

            _ => {}
        }
    }

    let close = close?;

    Some(DeclareHead {
        name,
        ty: TokSpan::new(name.end as usize, close + 1),
        ret: match text(close + 1) == ":" && close + 2 < last {
            true => Some(TokSpan::new(close + 2, last)),

            false => None,
        },
    })
}

/// One name a namespace member binds.
pub(crate) struct MemberBinding {
    pub name: TokSpan,
    /// The name goes on the namespace table.
    pub value: bool,
    /// The name is a type, so a type slot reads the rendered name.
    pub ty: bool,
    /// The member is a namespace of its own.
    pub nested: bool,
    /// The emit renames the declaration. A macro runs at compile time
    /// and keeps the name the source wrote.
    pub prefixed: bool,
}

fn binds(name: TokSpan, value: bool, ty: bool) -> MemberBinding {
    MemberBinding {
        name,
        value,
        ty,
        nested: false,
        prefixed: true,
    }
}

/// Every name a member binds.
pub(crate) fn member_bindings(
    m: &NamespaceMember,
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
) -> Vec<MemberBinding> {
    match m.stmt.under_default() {
        // `declare function f(...)` and `declare x: T` in a `.d.aly`
        // name a member the checker reads. The emit gives each one a
        // local of its own, the way every other member goes.
        Stmt::Declare(d) => declare_head(d.span, src, toks)
            .map(|h| binds(h.name, true, false))
            .into_iter()
            .collect(),

        Stmt::Function(f) if f.path.len() == 1 => vec![binds(f.path[0], true, false)],

        Stmt::LocalFunction(f) => vec![binds(f.name, true, false)],

        Stmt::Local(l) => l
            .names
            .iter()
            .filter(|b| b.destructure.is_none())
            .map(|b| binds(b.name, true, false))
            .collect(),

        Stmt::Struct(d) => vec![binds(d.name, true, true)],

        Stmt::Enum(d) => vec![binds(d.name, true, true)],

        Stmt::Trait(d) => vec![binds(d.name, true, true)],

        Stmt::Interface(d) => vec![binds(d.name, false, true)],

        Stmt::TypeAlias(d) => vec![binds(d.name, false, true)],

        Stmt::Class(d) => vec![binds(d.name, true, true)],

        Stmt::Remote(d) => vec![binds(d.name, true, false)],

        Stmt::Namespace(d) => vec![MemberBinding {
            name: d.name,
            value: true,
            ty: false,
            nested: true,
            prefixed: false,
        }],

        // A macro is source, not a value. It reaches no output, so it
        // takes no name of its own and the table carries none.
        Stmt::Macro(d) => vec![MemberBinding {
            name: d.name,
            value: false,
            ty: false,
            nested: false,
            prefixed: false,
        }],

        // An attribute declares a value the runtime reads:
        // `Attributes.get(f, Ns.tag)`. It goes on the table as every
        // other value member does.
        Stmt::Attribute(d) => vec![binds(d.name, true, false)],

        _ => Vec::new(),
    }
}

/// Every namespace member of a block that wears `export`, with the
/// span of its declaration. A nested namespace's members count too.
fn collect_member_exports(stmts: &[Stmt], out: &mut Vec<TokSpan>) {
    for stmt in stmts {
        let Stmt::Namespace(ns) = stmt.under_default() else {
            continue;
        };

        for m in &ns.members {
            let inner = m.stmt.under_default();

            // An `impl` binds no name on the table, so it is not a
            // member export.
            if !matches!(inner, Stmt::Impl(_)) && is_exported(inner) {
                out.push(inner.span());
            }

            collect_member_exports(std::slice::from_ref(&m.stmt), out);
        }
    }
}

/// Whether a declaration wears `export`.
pub(crate) fn is_exported(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Function(d) => d.exported,
        Stmt::LocalFunction(d) => d.exported,
        Stmt::Local(d) => d.exported,
        Stmt::Struct(d) => d.exported,
        Stmt::Enum(d) => d.exported,
        Stmt::Trait(d) => d.exported,
        Stmt::Interface(d) => d.exported,
        Stmt::Class(d) => d.exported,
        Stmt::TypeAlias(d) => d.exported,
        Stmt::Remote(d) => d.exported,
        Stmt::Macro(d) => d.exported,
        Stmt::Attribute(d) => d.exported,
        Stmt::Namespace(d) => d.exported,

        _ => false,
    }
}
