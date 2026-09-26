/*!
Attribute contracts: the `requires` clauses of an `attribute ... as ...
end`, checked where the attribute is used.

A clause says what the thing the attribute sits on must carry: a member
of a given kind, name, visibility, and shape. The check is the whole
feature. Nothing here reaches the emit, and `Attributes.get` reads the
attribute at run time the way it always did.

`each <param>` writes one clause per entry of that argument. An attribute
argument is already a literal the compiler reads, which is what makes
`Attributes.get` and `@ratelimit(5)` work, so the expansion is a read of
a constant. It runs no user code.
*/

use alloy_syntax::ast::{
    Attr, AttributeDecl, Expr, Field, FunctionBody, ImplDecl, IndexKey, NamespaceDecl,
    RequireClause, RequireMember, Stmt, TableField, TokSpan, TraitDecl,
};

use super::*;

/// One member of the thing an attribute sits on, as the check reads it.
#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub name: String,
    /// `function` or `field`.
    pub kind: &'static str,
    pub private: bool,
    /// A function's parameter list as written, `(self, dt: number)`, or a
    /// field's type. Empty when the source writes none.
    pub shape: String,
    /// The byte the member's name starts at.
    pub at: u32,
}

/// The declaration a contract is checked against: the target word the
/// attribute names, the owner's own name, and the span the members of
/// that declaration sit in.
#[derive(Clone, Copy)]
pub(crate) struct Owner<'a> {
    pub target: &'a str,
    pub name: &'a str,
    pub body: TokSpan,
}

/// The targets whose members a contract can read. A `requires` clause on
/// any other target has nothing to check, and the declaration says so.
pub(crate) const CONTRACT_TARGETS: &[&str] =
    &["struct", "impl", "namespace", "enum", "interface", "trait"];

impl<'s> Desugar<'s> {
    /// One `requires` clause of an attribute declaration, as the check
    /// keeps it.
    pub(crate) fn require_of(&self, c: &RequireClause) -> Require {
        let (member, each) = match c.member {
            RequireMember::Name(n) => (self.text_of(n).to_string(), false),

            RequireMember::Each(n) => (self.text_of(n).to_string(), true),
        };

        Require {
            private: c.visibility.map(|v| self.text_of(v) == "private"),
            kind: self.text_of(c.kind).to_string(),
            member,
            each,
            shape: c
                .shape
                .map(|s| self.text_of(s).trim().to_string())
                .unwrap_or_default(),
        }
    }

    /*
    The `requires` clauses of one `attribute` declaration, against the
    declaration itself.

    Two things are wrong at the declaration and not at a use: a contract
    on a target that carries no members, and an `each` over a parameter
    that is not a list. Both report here, so the author of the attribute
    reads them and the users of it do not.
    */
    pub(crate) fn check_attribute_decl(&mut self, a: &AttributeDecl) {
        if a.requires.is_empty() {
            return;
        }

        for t in &a.targets {
            let target = self.text_of(*t);

            if !CONTRACT_TARGETS.contains(&target) {
                let message = format!(
                    "a `requires` clause reads the members of what the attribute sits on, and {} {target} has none; the targets that carry members are {}",
                    super::article(target),
                    list_names(CONTRACT_TARGETS)
                );
                self.diagnose(*t, &message);
            }
        }

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

        for c in &a.requires {
            let RequireMember::Each(n) = c.member else {
                continue;
            };
            let name = self.text_of(n).to_string();
            let Some((_, ty)) = params.iter().find(|(p, _)| *p == name) else {
                let message = format!(
                    "`each {name}` needs a parameter named `{name}`; this attribute declares {}",
                    match params.is_empty() {
                        true => "none".to_string(),

                        false =>
                            list_names(&params.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>()),
                    }
                );
                self.diagnose(n, &message);

                continue;
            };

            match ty.as_deref() {
                // A parameter with no type says nothing either way, so
                // the use reads the entries it finds.
                None => {}

                Some(t) if is_list_type(t) => {}

                Some(t) => {
                    let a = super::article(t);
                    let message =
                        format!("`each {name}` needs a list parameter; `{name}` is {a} `{t}`");
                    self.diagnose(n, &message);
                }
            }
        }
    }

    /*
    The contract of every attribute on one declaration.

    `members` is what the declaration carries. A clause that the member
    list does not answer lands on the attribute, because the attribute is
    the promise the file made.
    */
    pub(crate) fn check_contracts(&mut self, attrs: &[Attr], owner: Owner<'_>, members: &[Member]) {
        for a in attrs {
            let Some(n) = a.name else { continue };
            let name = self.text_of(n).to_string();
            let Some(decl) = self.attr_decl_of(&name).cloned() else {
                continue;
            };

            // An attribute on a target it does not take is already a
            // report on that line. Checking its contract there would say
            // one mistake twice.
            if decl.requires.is_empty() || !self.attr_reaches(&name, owner.target) {
                continue;
            }

            for want in self.contract_clauses(a, &decl) {
                self.check_one_clause(a.span, &name, owner, members, &want);
            }
        }
    }

    /// Records what the editor writes for a member the declaration does
    /// not carry: the member, and the `end` it goes in front of.
    fn note_gap(&mut self, at: TokSpan, attr: &str, owner: Owner<'_>, want: &Require) {
        /*
        A field belongs in the struct and a method in an `impl`, even
        where the contract sits on the other half of the same type.

        The declaration the attribute sits on wins when it holds that
        kind: a method gap on `@service impl S` goes in that block, not
        in another `impl S` the file also writes.
        */
        let body = match (want.kind.as_str(), owner.target) {
            ("field", "struct" | "interface" | "namespace") => owner.body,

            ("field", _) => self
                .field_body
                .get(owner.name)
                .copied()
                .unwrap_or(owner.body),

            (_, "impl" | "trait" | "namespace") => owner.body,

            _ => self
                .method_body
                .get(owner.name)
                .copied()
                .unwrap_or(owner.body),
        };
        let last = (body.end as usize).saturating_sub(1);
        let Some(tok) = self.toks.get(last) else {
            return;
        };
        let insert_at = tok.start;
        let before = &self.src[..insert_at as usize];
        let indent = insert_at - before.rfind('\n').map_or(0, |i| i as u32 + 1);
        self.contract_gaps.push(ContractGap {
            attr: attr.to_string(),
            start: self.byte_start(at),
            end: self.byte_end(at),
            member: want.member.clone(),
            kind: want.kind.clone(),
            visibility: match want.private {
                Some(true) => "private".to_string(),

                Some(false) => "public".to_string(),

                None => String::new(),
            },
            shape: want.shape.clone(),
            insert_at,
            indent,
        });
    }

    /*
    The clauses one use of an attribute asks for, with `each` expanded
    against the arguments the use writes.

    A clause over a parameter the use leaves empty asks for nothing. That
    is the point of `each`: `lifecycles = [ Start ]` asks for one member
    and `lifecycles = []` asks for none.
    */
    fn contract_clauses(&self, a: &Attr, decl: &AttrDecl) -> Vec<Require> {
        let mut out = Vec::new();

        for c in &decl.requires {
            if !c.each {
                out.push(c.clone());

                continue;
            }

            for member in self.each_entries(a, decl, &c.member) {
                out.push(Require {
                    member,
                    each: false,
                    ..c.clone()
                });
            }
        }

        out
    }

    /*
    The names an `each <param>` reads from one use of the attribute.

    The argument is a list literal, and a parameter is named two ways: by
    position, `@provider([ Lifecycle.Init ])`, and by key inside a table,
    `@provider({ lifecycles = [ Lifecycle.Init ] })`, which is the form
    the RFC writes. Both read the same list.

    An entry is a string, which names the member by its text, or a path,
    `Lifecycle.Init`, which names it by its last segment. Anything else
    is no name and the entry is skipped: the argument type check reports
    it on the argument, and a second report here would say it twice.
    */
    fn each_entries(&self, a: &Attr, decl: &AttrDecl, param: &str) -> Vec<String> {
        let at = decl.params.iter().position(|(p, _)| p == param);
        let slots = self.attr_slots(a, Some(decl));
        let Some(Expr::Array { items, .. }) = at.and_then(|i| slots.get(i).copied().flatten())
        else {
            return Vec::new();
        };
        let mut names: Vec<String> = items.iter().filter_map(|e| self.entry_name(e)).collect();
        // An entry the parameter's type does not admit is already a
        // report on that entry. Asking for a member named after it would
        // say one mistake twice.
        let admitted = at
            .and_then(|i| decl.params.get(i))
            .and_then(|(_, t)| t.as_deref())
            .map(|t| self.admitted_values(t))
            .unwrap_or_default();

        if !admitted.is_empty() {
            names.retain(|n| admitted.contains(n));
        }

        names
    }

    /*
    The argument of each parameter of one use, in declaration order, with
    `None` where the use gives none.

    A use names its arguments by position, `@options([ Init ], 10)`, or
    by key in one table, `@options({ steps = [ Init ], priority = 10 })`.
    The table is the record form when each of its keys names a parameter.
    The emit, the argument check and `each` all read the slots, so the
    runtime value and the contract agree on each form.
    */
    pub(crate) fn attr_slots<'e>(
        &self,
        a: &'e Attr,
        decl: Option<&AttrDecl>,
    ) -> Vec<Option<&'e Expr>> {
        if let Some(slots) = decl.and_then(|d| self.keyed_slots(a, d)) {
            return slots;
        }

        let params = decl.map_or(0, |d| d.params.len());
        let mut slots: Vec<Option<&Expr>> = a.args.iter().map(Some).collect();
        slots.resize(slots.len().max(params), None);

        slots
    }

    /// The slots of a use in the record form, or `None` for a use by
    /// position.
    pub(crate) fn keyed_slots<'e>(
        &self,
        a: &'e Attr,
        decl: &AttrDecl,
    ) -> Option<Vec<Option<&'e Expr>>> {
        let [Expr::Table { fields, .. }] = a.args.as_slice() else {
            return None;
        };

        if fields.is_empty() {
            return None;
        }

        let mut slots = vec![None; decl.params.len()];

        for f in fields {
            let TableField::Named { name, value } = f else {
                return None;
            };
            let key = self.text_of(*name);
            let i = decl.params.iter().position(|(p, _)| p == key)?;
            slots[i] = Some(value);
        }

        Some(slots)
    }

    /// The member name one entry of an `each` list carries.
    pub(crate) fn entry_name(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::String(s) => {
                let text = self.text_of(*s);

                Some(text.trim_matches(['"', '\'']).to_string())
            }

            // `Lifecycle.Init`: the variant names the member.
            Expr::Index {
                key: IndexKey::Field(f),
                ..
            } => Some(self.text_of(*f).to_string()),

            Expr::Name(n) => Some(self.text_of(*n).to_string()),

            _ => None,
        }
    }

    /*
    The entries of one attribute argument, against the values its declared
    type admits.

    A list parameter, `lifecycles: Lifecycle[]`, is written as a list
    literal, by position or under its key in a record. A parameter with a
    narrowed type of its own, `stage: "a" | "b"`, is one value. Both read
    the same way here: every entry names a value, and a value the type
    does not admit reports on the entry the file wrote.
    */
    pub(crate) fn check_argument_entries(&mut self, attr: &str, param: &str, ty: &str, arg: &Expr) {
        let element = element_type(ty);
        let admitted = self.admitted_values(&element);

        if admitted.is_empty() {
            return;
        }

        let strings = is_string_union(&element);
        let entries: Vec<&Expr> = match arg {
            Expr::Array { items, .. } => items.iter().collect(),

            // A single value stands for itself; a list type written with
            // one entry and no brackets is that entry.
            other => vec![other],
        };

        for e in entries {
            let (Some(name), quoted) = (self.entry_name(e), matches!(e, Expr::String(_))) else {
                continue;
            };

            // A union of string literals takes a string. A variant path
            // is a value, not one of those strings.
            if strings && !quoted {
                let message = format!(
                    "the attribute `{attr}` takes {element} for `{param}`, `{}` given",
                    self.text_of(e.span()).trim()
                );
                self.diagnose(e.span(), &message);

                continue;
            }

            if admitted.contains(&name) {
                continue;
            }

            let names: Vec<&str> = admitted.iter().map(String::as_str).collect();
            let message = match strings {
                true => format!(
                    "the attribute `{attr}` takes {element} for `{param}`, `{}` given",
                    self.text_of(e.span()).trim()
                ),

                false => format!(
                    "`{element}` has no variant `{name}`; its variants are {}",
                    list_names(&names)
                ),
            };
            self.diagnose(e.span(), &message);
        }
    }

    /*
    The values a type admits by name: the members of a union of string
    literals, or the variants of an enum this file reaches.

    Empty for every other type, and the argument check then compares the
    literal's kind the way it always did.
    */
    pub(crate) fn admitted_values(&self, ty: &str) -> Vec<String> {
        let ty = element_type(ty);

        if is_string_union(&ty) {
            return ty
                .split('|')
                .map(|p| p.trim().trim_matches(['"', '\'']).to_string())
                .filter(|p| !p.is_empty())
                .collect();
        }

        match self.enum_decls.get(&ty) {
            Some(variants) => variants.iter().map(|(v, _)| v.clone()).collect(),

            None => Vec::new(),
        }
    }

    /// One clause against the members of the declaration under it.
    fn check_one_clause(
        &mut self,
        at: TokSpan,
        attr: &str,
        owner: Owner<'_>,
        members: &[Member],
        want: &Require,
    ) {
        let found: Vec<&Member> = members
            .iter()
            .filter(|m| m.name == want.member && m.kind == want.kind)
            .collect();
        // The contract fixes the name of each member that answers it. A
        // member of another type that shares the name keeps the lint.
        self.contract_names.extend(found.iter().map(|m| m.at));

        let Some(m) = found.first() else {
            let visibility = match want.private {
                Some(true) => "private ",

                Some(false) => "public ",

                None => "",
            };
            let message = format!(
                "`@{attr}` requires a {visibility}{} `{}`; `{}` declares none",
                want.kind,
                member_sketch(want),
                owner.name
            );
            self.diagnose(at, &message);
            self.note_gap(at, attr, owner, want);

            return;
        };

        if let Some(private) = want.private
            && m.private != private
        {
            let (asked, has) = match private {
                true => ("private", "public"),

                false => ("public", "private"),
            };
            let message = format!(
                "`@{attr}` requires `{}` to be {asked}; `{}` declares it {has}",
                want.member, owner.name
            );
            self.diagnose(at, &message);

            return;
        }

        // A clause with no shape asks for the member alone.
        if want.shape.is_empty() {
            return;
        }

        if normalize_shape(&want.shape) != normalize_shape(&m.shape) {
            let message = format!(
                "`@{attr}` requires `{}`; `{}` declares `{}`",
                member_sketch(want),
                owner.name,
                match m.shape.is_empty() {
                    true => want.member.clone(),

                    false => format!("{}{}", want.member, shape_text(m)),
                }
            );
            self.diagnose(at, &message);
        }
    }

    /*
    Every member the file declares for one type: the fields of its
    `struct` or `interface` and the methods of every `impl` over it.

    A contract on a `struct` reads its impl blocks, and a contract on an
    `impl` reads the struct's fields. Both directions name the same type,
    and the type is what a user means by `@service impl S`. The RFC left
    this open; reading both is the reading that matches the example it
    writes, where `requires private field state: number` sits in an
    `on impl` contract.
    */
    pub(crate) fn type_members_of(&self, name: &str) -> Vec<Member> {
        match self.type_members.get(name) {
            Some(m) => m.clone(),

            None => Vec::new(),
        }
    }

    /// The fields of a struct or an interface, as the check reads them.
    /// An interface field takes no visibility, so `visible` is false
    /// there and every field is public.
    pub(crate) fn field_members(&self, fields: &[Field], visible: bool) -> Vec<Member> {
        fields
            .iter()
            .map(|f| Member {
                name: self.text_of(f.name).to_string(),
                kind: "field",
                private: visible && f.visibility.is_some_and(|v| self.text_of(v) == "private"),
                shape: self.text_of(f.ty).trim().to_string(),
                at: self.byte_start(f.name),
            })
            .collect()
    }

    /// The members of one `impl` block: its methods, with the visibility
    /// and the parameter list each one writes.
    pub(crate) fn impl_block_members(&self, i: &ImplDecl) -> Vec<Member> {
        i.methods
            .iter()
            .filter_map(|m| {
                let first = *m.path.first()?;

                Some(Member {
                    name: self.text_of(first).to_string(),
                    kind: "function",
                    private: m.visibility.is_some_and(|v| self.text_of(v) == "private"),
                    shape: self.params_text(&m.body),
                    at: self.byte_start(first),
                })
            })
            .collect()
    }

    /// The parameter list of a function body, as the source writes it.
    fn params_text(&self, body: &FunctionBody) -> String {
        let parts: Vec<String> = body
            .params
            .iter()
            .map(|p| match p.ty {
                Some(t) => format!("{}: {}", self.text_of(p.name), self.text_of(t).trim()),

                None => self.text_of(p.name).to_string(),
            })
            .collect();

        format!("({})", parts.join(", "))
    }

    /// The members of a trait: one per signature it declares. A trait
    /// member is public, which is what a trait is for.
    pub(crate) fn trait_members(&self, t: &TraitDecl) -> Vec<Member> {
        t.methods
            .iter()
            .map(|m| Member {
                name: self.text_of(m.name).to_string(),
                kind: "function",
                private: false,
                shape: signature_params(self.text_of(m.signature)).to_string(),
                at: self.byte_start(m.name),
            })
            .collect()
    }

    /// The members of a namespace: a function, or a `local` or `const`
    /// binding as a field.
    pub(crate) fn namespace_members(&self, ns: &NamespaceDecl) -> Vec<Member> {
        let mut out = Vec::new();

        for m in &ns.members {
            let private = m.visibility.is_some_and(|v| self.text_of(v) == "private");

            match &m.stmt {
                Stmt::Function(f) => {
                    if let Some(n) = f.path.first() {
                        out.push(Member {
                            name: self.text_of(*n).to_string(),
                            kind: "function",
                            private,
                            shape: self.params_text(&f.body),
                            at: self.byte_start(*n),
                        });
                    }
                }

                Stmt::LocalFunction(f) => out.push(Member {
                    name: self.text_of(f.name).to_string(),
                    kind: "function",
                    private,
                    shape: self.params_text(&f.body),
                    at: self.byte_start(f.name),
                }),

                Stmt::Local(l) => {
                    for b in &l.names {
                        out.push(Member {
                            name: self.text_of(b.name).to_string(),
                            kind: "field",
                            private,
                            shape: b
                                .ty
                                .map(|t| self.text_of(t).trim().to_string())
                                .unwrap_or_default(),
                            at: self.byte_start(b.name),
                        });
                    }
                }

                _ => {}
            }
        }

        out
    }
}

/// The member a clause asks for, as a sentence names it: `Start(self)`
/// for a function, `state: number` for a field.
fn member_sketch(want: &Require) -> String {
    match (want.kind.as_str(), want.shape.is_empty()) {
        (_, true) => want.member.clone(),

        ("function", false) => format!("{}{}", want.member, want.shape),

        _ => format!("{}: {}", want.member, want.shape),
    }
}

/// The shape of a member, spelled the way its kind writes it.
fn shape_text(m: &Member) -> String {
    match m.kind {
        "function" => m.shape.clone(),

        _ => format!(": {}", m.shape),
    }
}

/// The parameter list of a signature: the text up to the matching `)`,
/// so a return type does not join the comparison.
fn signature_params(sig: &str) -> &str {
    let mut depth = 0i32;

    for (i, c) in sig.char_indices() {
        match c {
            '(' => depth += 1,

            ')' => {
                depth -= 1;

                if depth == 0 {
                    return &sig[..i + 1];
                }
            }

            _ => {}
        }
    }

    sig
}

/// A shape with its spacing dropped, so `(self, dt: number)` and
/// `(self,dt : number)` compare equal. The comparison is textual on
/// purpose: a contract asks for the signature the author wrote.
fn normalize_shape(s: &str) -> String {
    signature_params(s)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/*
The element of a list type: `Lifecycle[]` and `Array<Lifecycle>` both give
`Lifecycle`, and the parentheses a union needs come off with it.

A type that is no list is its own element, so a parameter that takes one
value reads the same way.
*/
pub fn element_type(ty: &str) -> String {
    let ty = ty.trim();
    let inner = ty
        .strip_suffix("[]")
        .or_else(|| ty.strip_prefix("Array<").and_then(|r| r.strip_suffix('>')))
        .unwrap_or(ty)
        .trim();
    let inner = inner
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or(inner);

    inner.trim().to_string()
}

/// Reports if a type is a union of string literals, ex: `"a" | "b"`. A
/// single quoted literal counts: it admits one value.
pub fn is_string_union(ty: &str) -> bool {
    let ty = ty.trim();

    !ty.is_empty()
        && ty
            .split('|')
            .all(|p| matches!(p.trim().chars().next(), Some('"' | '\'')))
}

/// Reports if a type is a list: `T[]`, `Array<T>`, or `{ T }`.
pub(crate) fn is_list_type(t: &str) -> bool {
    let t = t.trim();

    t.ends_with("[]")
        || t.starts_with("Array<")
        || (t.starts_with('{') && t.ends_with('}') && !t.contains(':'))
}
