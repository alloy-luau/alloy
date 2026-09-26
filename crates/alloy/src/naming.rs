//! Naming conventions: the case style of each kind of name, the lint
//! that reports a name in another case, and the rename that fixes it.
//!
//! `[lint.naming]` in `alloy.toml` gives each kind of name one style or
//! a list of styles. A name passes when it fits any style in its list,
//! and the fix writes the first. The defaults follow Rust.
//!
//! The fix renames a name in every place the file reads it. It reads
//! the tree for the scope of each binding, and it writes nothing when a
//! token of the old name has a role the tree does not explain.

use std::collections::{HashMap, HashSet};

use alloy_syntax::ast::{
    Attr, Block, ChildName, Chunk, Cond, DefaultExport, Destructure, Expr, FunctionBody,
    ImportKind, IndexKey, Param, Pattern, Stmt, TableField, TokSpan,
};
use alloy_syntax::lexer::{Tok, TokKind};
use serde::{Deserialize, Serialize};

use crate::desugar::{Child, expr_children, stmt_children};
use crate::lint::{Fix, Lint};

/// The name of the lint.
pub const LINT: &str = "naming_convention";

/// One case style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// `player_count`.
    Snake,
    /// `playerCount`.
    Camel,
    /// `PlayerCount`.
    Pascal,
    /// `PLAYER_COUNT`.
    Screaming,
    /// Every name passes.
    Any,
}

impl Style {
    pub const ALL: &[Style] = &[
        Style::Snake,
        Style::Camel,
        Style::Pascal,
        Style::Screaming,
        Style::Any,
    ];

    /// The name `[lint.naming]` spells the style with.
    pub fn name(self) -> &'static str {
        match self {
            Style::Snake => "snake_case",
            Style::Camel => "camelCase",
            Style::Pascal => "PascalCase",
            Style::Screaming => "SCREAMING_SNAKE_CASE",
            Style::Any => "any",
        }
    }

    pub fn from_name(name: &str) -> Option<Style> {
        Style::ALL.iter().copied().find(|s| s.name() == name)
    }

    /// Whether `name` is written in this style. A name of one letter,
    /// or of digits after letters, fits each style its letters allow:
    /// `x` is snake_case and camelCase, `T` is PascalCase and
    /// SCREAMING_SNAKE_CASE.
    pub fn fits(self, name: &str) -> bool {
        let first = name.chars().next();
        let tidy = !name.contains("__") && !name.ends_with('_');
        let all = |ok: fn(char) -> bool| name.chars().all(ok);

        match self {
            Style::Any => true,

            Style::Snake => {
                first.is_some_and(|c| c.is_ascii_lowercase())
                    && tidy
                    && all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            }

            Style::Screaming => {
                first.is_some_and(|c| c.is_ascii_uppercase())
                    && tidy
                    && all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            }

            Style::Camel => {
                first.is_some_and(|c| c.is_ascii_lowercase()) && all(|c| c.is_ascii_alphanumeric())
            }

            Style::Pascal => {
                first.is_some_and(|c| c.is_ascii_uppercase()) && all(|c| c.is_ascii_alphanumeric())
            }
        }
    }

    /// `name` written in this style, word by word.
    pub fn convert(self, name: &str) -> String {
        let words = words(name);
        let lower = |w: &str| w.to_ascii_lowercase();
        let upper = |w: &str| w.to_ascii_uppercase();
        let title = |w: &str| {
            let mut out = lower(w);

            if let Some(c) = out.get_mut(..1) {
                c.make_ascii_uppercase();
            }

            out
        };

        match self {
            Style::Any => name.to_string(),

            Style::Snake => words.iter().map(|w| lower(w)).collect::<Vec<_>>().join("_"),

            Style::Screaming => words.iter().map(|w| upper(w)).collect::<Vec<_>>().join("_"),

            Style::Camel => words
                .iter()
                .enumerate()
                .map(|(i, w)| if i == 0 { lower(w) } else { title(w) })
                .collect(),

            Style::Pascal => words.iter().map(|w| title(w)).collect(),
        }
    }
}

/// The words of a name. A word ends at `_`, before a capital that
/// follows a lowercase letter or a digit, and before the last capital
/// of an acronym: `HTTPServer` is `HTTP` and `Server`.
pub fn words(name: &str) -> Vec<&str> {
    let mut out = Vec::new();

    for part in name.split('_').filter(|p| !p.is_empty()) {
        let b = part.as_bytes();
        let mut from = 0;

        for i in 1..b.len() {
            let acronym_end = b[i - 1].is_ascii_uppercase()
                && b.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let starts = b[i].is_ascii_uppercase()
                && (b[i - 1].is_ascii_lowercase() || b[i - 1].is_ascii_digit() || acronym_end);

            if starts {
                out.push(&part[from..i]);
                from = i;
            }
        }

        out.push(&part[from..]);
    }

    out
}

/// The styles one kind of name takes. A name passes when it fits any of
/// them, and the fix writes the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Styles(pub Vec<Style>);

impl Styles {
    fn of(styles: &[Style]) -> Self {
        Styles(styles.to_vec())
    }

    pub fn fits(&self, name: &str) -> bool {
        self.0.iter().any(|s| s.fits(name))
    }

    /// `snake_case or SCREAMING_SNAKE_CASE`.
    fn describe(&self) -> String {
        self.0
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

impl Serialize for Styles {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|style| style.name()))
    }
}

impl<'de> Deserialize<'de> for Styles {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let one = |value: &toml::Value| {
            let text = value.as_str().unwrap_or_default();

            Style::from_name(text).ok_or_else(|| {
                let names: Vec<&str> = Style::ALL.iter().map(|s| s.name()).collect();

                D::Error::custom(format!(
                    "`{value}` is not a naming style; the styles are {}",
                    names.join(", ")
                ))
            })
        };

        match toml::Value::deserialize(d)? {
            toml::Value::Array(items) if !items.is_empty() => {
                items.iter().map(one).collect::<Result<_, _>>().map(Styles)
            }

            value @ toml::Value::String(_) => Ok(Styles(vec![one(&value)?])),

            _ => Err(D::Error::custom(
                "a naming style is a string, or a list of one or more strings",
            )),
        }
    }
}

/// One kind of name, as `[lint.naming]` keys it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Variable,
    Const,
    Function,
    /// A function of an `.alx` file that returns markup or that a tag
    /// names, `<Row />`.
    Component,
    Method,
    Parameter,
    Field,
    Struct,
    Enum,
    Variant,
    Trait,
    Interface,
    Type,
    Namespace,
    Attribute,
    Macro,
    Remote,
}

impl Kind {
    /// The key of `[lint.naming]`, which is also the word a message uses.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Variable => "variable",
            Kind::Const => "const",
            Kind::Function => "function",
            Kind::Component => "component",
            Kind::Method => "method",
            Kind::Parameter => "parameter",
            Kind::Field => "field",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Variant => "variant",
            Kind::Trait => "trait",
            Kind::Interface => "interface",
            Kind::Type => "type",
            Kind::Namespace => "namespace",
            Kind::Attribute => "attribute",
            Kind::Macro => "macro",
            Kind::Remote => "remote",
        }
    }

    /// A name another value owns: a field, a variant, a method. A read
    /// of one goes through that value, so it never shadows a binding.
    fn is_member(self) -> bool {
        matches!(self, Kind::Field | Kind::Variant | Kind::Method)
    }

    /// A name a type position reads.
    fn is_type(self) -> bool {
        matches!(
            self,
            Kind::Struct | Kind::Enum | Kind::Trait | Kind::Interface | Kind::Type
        )
    }
}

/// `[lint.naming]`: the styles of each kind of name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Naming {
    pub variable: Styles,
    pub r#const: Styles,
    pub function: Styles,
    pub component: Styles,
    pub method: Styles,
    pub parameter: Styles,
    pub field: Styles,
    pub r#struct: Styles,
    pub r#enum: Styles,
    pub variant: Styles,
    pub r#trait: Styles,
    pub interface: Styles,
    pub r#type: Styles,
    pub namespace: Styles,
    pub attribute: Styles,
    pub r#macro: Styles,
    pub remote: Styles,
    /// A `local` that nothing assigns again takes the const style. The
    /// `prefer_const` lint or `[fmt] prefer_const` makes it a `const`,
    /// and a rename to the variable style would then fire again. The
    /// config sets it from those two keys, and no file writes it.
    #[serde(skip)]
    pub locals_as_const: bool,
}

impl Default for Naming {
    fn default() -> Self {
        let snake = Styles::of(&[Style::Snake]);
        let pascal = Styles::of(&[Style::Pascal]);

        Self {
            variable: snake.clone(),
            // A `const` marks any binding the file never assigns again,
            // as in JavaScript, so both cases read as a constant.
            r#const: Styles::of(&[Style::Snake, Style::Screaming]),
            function: snake.clone(),
            // React's rule: a tag names a component by a capital letter.
            component: pascal.clone(),
            method: snake.clone(),
            parameter: snake.clone(),
            field: snake.clone(),
            r#struct: pascal.clone(),
            r#enum: pascal.clone(),
            variant: pascal.clone(),
            r#trait: pascal.clone(),
            interface: pascal.clone(),
            r#type: pascal.clone(),
            namespace: pascal.clone(),
            attribute: snake.clone(),
            r#macro: snake,
            remote: pascal,
            // Both `prefer_const` and `[fmt] prefer_const` are on by
            // default.
            locals_as_const: true,
        }
    }
}

impl Naming {
    pub fn get(&self, kind: Kind) -> &Styles {
        match kind {
            Kind::Variable => &self.variable,
            Kind::Const => &self.r#const,
            Kind::Function => &self.function,
            Kind::Component => &self.component,
            Kind::Method => &self.method,
            Kind::Parameter => &self.parameter,
            Kind::Field => &self.field,
            Kind::Struct => &self.r#struct,
            Kind::Enum => &self.r#enum,
            Kind::Variant => &self.variant,
            Kind::Trait => &self.r#trait,
            Kind::Interface => &self.interface,
            Kind::Type => &self.r#type,
            Kind::Namespace => &self.namespace,
            Kind::Attribute => &self.attribute,
            Kind::Macro => &self.r#macro,
            Kind::Remote => &self.remote,
        }
    }
}

/// How far a rename of one declaration may reach.
#[derive(Debug, Clone)]
enum Reach {
    /// No rename: another file or saved data reads the name, or the
    /// walk does not know the scope.
    None,
    /// Every token of the file: a file-level function or type.
    File,
    /// These token ranges, end exclusive: the scope of a local or a
    /// parameter.
    Scope(Vec<(usize, usize)>),
}

/// What an identifier token is, as the tree says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The walk did not reach it. A rename that meets one stops.
    Unknown,
    /// A read or a write of a binding.
    Value,
    /// A name in a type.
    Type,
    /// A field, a method, a key, or a variant: a name a value owns.
    Member,
}

/// One declared name.
struct Decl {
    /// `None` for a binding the lint does not check: an import, or a
    /// name a match pattern binds.
    kind: Option<Kind>,
    tok: usize,
    reach: Reach,
}

impl Decl {
    fn is_member(&self) -> bool {
        self.kind.is_some_and(Kind::is_member)
    }

    fn is_type(&self) -> bool {
        self.kind.is_some_and(Kind::is_type)
    }
}

/// The walk over one file's tree: every declaration with its scope, and
/// the role of each identifier token.
struct Walk<'a> {
    src: &'a str,
    toks: &'a [Tok],
    decls: Vec<Decl>,
    roles: Vec<Role>,
    /// The names an `export { }` list or an `export default` sends out.
    exported: Vec<&'a str>,
    /// The byte offset of each `local` that `prefer_const` makes a
    /// `const`, when `[lint.naming]` reads such a local as one.
    const_locals: HashSet<u32>,
    /// The declarations of those locals, by index into `decls`.
    promoted: Vec<usize>,
}

impl<'a> Walk<'a> {
    fn text(&self, i: usize) -> &'a str {
        self.toks.get(i).map_or("", |t| t.text(self.src))
    }

    fn role(&mut self, i: u32, role: Role) {
        if let Some(r) = self.roles.get_mut(i as usize) {
            *r = role;
        }
    }

    /// A dotted path: the head reads a value, and the rest are members.
    /// A path of one name takes `alone`, the role the caller gives it.
    fn path(&mut self, span: TokSpan, alone: Role) {
        let single = span.end <= span.start + 1;

        for i in span.start..span.end {
            let role = match (i == span.start, single) {
                (true, true) => alone,
                (true, false) => Role::Value,
                (false, _) => Role::Member,
            };
            self.role(i, role);
        }
    }

    /// Records a declaration. A scope takes the name token itself too.
    fn declare(&mut self, kind: Option<Kind>, span: TokSpan, reach: Reach) {
        let tok = span.start as usize;
        let reach = match reach {
            Reach::Scope(mut ranges) => {
                ranges.push((tok, tok + 1));
                Reach::Scope(ranges)
            }

            other => other,
        };

        if kind.is_some_and(Kind::is_member) {
            self.role(span.start, Role::Member);
        }

        self.decls.push(Decl { kind, tok, reach });
    }

    /// The reach of a file-level declaration: the whole file, unless the
    /// name leaves it or sits in a block.
    fn file_reach(exported: bool, top: bool, ns: bool) -> Reach {
        match exported || ns || !top {
            true => Reach::None,

            false => Reach::File,
        }
    }

    /// Whether an attribute other than `@allow` marks a declaration. The
    /// runtime or the compiler may read the name it marks: `@test` names
    /// a test, and a declared attribute may register a name.
    fn marked(&self, attrs: &[Attr]) -> bool {
        attrs.iter().any(|a| {
            a.name
                .is_some_and(|n| self.text(n.start as usize) != "allow")
        })
    }

    fn block(&mut self, stmts: &'a [Stmt], end: usize, top: bool, ns: bool) {
        for s in stmts {
            self.stmt(s, end, top, ns);
        }
    }

    /// One statement in a block that ends at token `end`. `top` is the
    /// file level, and `ns` a namespace body, whose members other files
    /// read as `Name.member`.
    fn stmt(&mut self, s: &'a Stmt, end: usize, top: bool, ns: bool) {
        match s {
            Stmt::Local(l) => {
                // `prefer_const` writes `const` over the `local` word,
                // which sits in front of the first name.
                let first = l.names.first().map_or(l.span.start, |b| b.name.start);
                let promoted = !l.is_const
                    && (l.span.start..first)
                        .any(|i| self.const_locals.contains(&self.toks[i as usize].start));
                let kind = if l.is_const || promoted {
                    Kind::Const
                } else {
                    Kind::Variable
                };
                let before = self.decls.len();
                // `local Players = game:GetService("Players")` takes the
                // case of the service or the module it names.
                let named_after =
                    l.names.len() == 1 && l.values.first().is_some_and(|v| self.names_module(v));
                let kind = (!named_after).then_some(kind);

                for b in &l.names {
                    let reach = match l.exported || ns || self.marked(&l.attrs) {
                        true => Reach::None,

                        false => Reach::Scope(vec![(l.span.end as usize, end)]),
                    };
                    self.binding(b.name, b.destructure.as_ref(), kind, reach);
                }

                if promoted {
                    self.promoted.extend(before..self.decls.len());
                }
            }

            Stmt::LocalFunction(f) => {
                let t = f.name.start as usize;
                // A file-level `local function` is declared on the
                // first line of the emit, so a body above it reads it.
                let reach = match (f.exported || ns || self.marked(&f.attrs), top) {
                    (true, _) => Reach::None,
                    (false, true) => Reach::File,
                    (false, false) => Reach::Scope(vec![(t, end)]),
                };
                self.declare(Some(Kind::Function), f.name, reach);
            }

            Stmt::Function(f) => match f.path.as_slice() {
                [name] => self.declare(
                    Some(Kind::Function),
                    *name,
                    Self::file_reach(f.exported || self.marked(&f.attrs), top, ns),
                ),

                [head, ..] => {
                    self.role(head.start, Role::Value);

                    for p in &f.path[1..] {
                        self.role(p.start, Role::Member);
                    }
                }

                [] => {}
            },

            Stmt::Struct(d) => {
                self.declare(
                    Some(Kind::Struct),
                    d.name,
                    Self::file_reach(d.exported || self.marked(&d.attributes), top, ns),
                );

                for f in &d.fields {
                    self.declare(Some(Kind::Field), f.name, Reach::None);
                }
            }

            Stmt::Enum(e) => {
                self.declare(
                    Some(Kind::Enum),
                    e.name,
                    Self::file_reach(e.exported || self.marked(&e.attributes), top, ns),
                );

                for v in &e.variants {
                    self.declare(Some(Kind::Variant), v.name, Reach::None);
                }
            }

            Stmt::Trait(t) => {
                self.declare(
                    Some(Kind::Trait),
                    t.name,
                    Self::file_reach(t.exported || self.marked(&t.attributes), top, ns),
                );

                // A default body reads the parameters of the signature;
                // its own list is empty.
                for m in &t.methods {
                    self.declare(Some(Kind::Method), m.name, Reach::None);
                    self.params(&m.params, m.body.as_ref().map(|b| b.span.end as usize));
                }
            }

            Stmt::Interface(i) => {
                self.declare(
                    Some(Kind::Interface),
                    i.name,
                    Self::file_reach(i.exported || self.marked(&i.attributes), top, ns),
                );

                for f in &i.fields {
                    self.declare(Some(Kind::Field), f.name, Reach::None);
                }

                for x in &i.extends {
                    self.role(x.start, Role::Type);
                }
            }

            // `type function f()` names a type function, not a type.
            Stmt::TypeAlias(t)
                if self.text((t.name.start as usize).wrapping_sub(1)) != "function" =>
            {
                self.declare(
                    Some(Kind::Type),
                    t.name,
                    Self::file_reach(t.exported || self.marked(&t.attributes), top, ns),
                );
            }

            // The wire, an `@name`, and a `$name` read these three by
            // their names, so they keep the lint alone.
            Stmt::Remote(r) => {
                self.declare(Some(Kind::Remote), r.name, Reach::None);
                self.params(&r.params, None);
            }

            Stmt::Attribute(a) => {
                self.declare(Some(Kind::Attribute), a.name, Reach::None);
                self.params(&a.params, None);
            }

            Stmt::Macro(m) => {
                self.declare(Some(Kind::Macro), m.name, Reach::None);
                self.params(&m.params, None);
            }

            Stmt::Namespace(n) => {
                self.declare(
                    Some(Kind::Namespace),
                    n.name,
                    Self::file_reach(n.exported || self.marked(&n.attributes), top, ns),
                );

                for m in &n.members {
                    self.stmt(&m.stmt, end, false, true);
                }
            }

            Stmt::Impl(i) => {
                self.path(i.target, Role::Type);

                if let Some(t) = i.trait_name {
                    self.path(t, Role::Type);
                }

                for f in &i.methods {
                    let Some(name) = f.path.last() else { continue };

                    // A trait names the methods of its impls.
                    match i.trait_name {
                        Some(_) => self.role(name.start, Role::Member),

                        None => self.declare(Some(Kind::Method), *name, Reach::None),
                    }
                }
            }

            Stmt::Import(im) => {
                let specs = match &im.kind {
                    ImportKind::Default(n) => {
                        self.declare(None, *n, Reach::None);
                        &[][..]
                    }

                    ImportKind::Namespace(n, specs) | ImportKind::Both(n, specs) => {
                        self.declare(None, *n, Reach::None);
                        &specs[..]
                    }

                    ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => &specs[..],
                };

                for spec in specs {
                    self.role(spec.name.start, Role::Member);
                    self.declare(None, spec.alias.unwrap_or(spec.name), Reach::None);
                }
            }

            Stmt::ExportList(x) => {
                for spec in &x.specs {
                    self.exported.push(self.text(spec.name.start as usize));
                    self.role(spec.name.start, Role::Member);
                }
            }

            Stmt::ExportDefault { value, .. } => match value {
                DefaultExport::Decl(inner) => {
                    let first = self.decls.len();
                    self.stmt(inner, end, top, ns);

                    // The declaration's own name comes first.
                    if let Some(d) = self.decls.get_mut(first) {
                        d.reach = Reach::None;
                    }

                    return;
                }

                DefaultExport::Value(Expr::Name(n)) => {
                    self.exported.push(self.text(n.start as usize));
                }

                DefaultExport::Value(_) => {}
            },

            Stmt::NumericFor(f) => {
                let body = (f.block.span.start as usize, f.block.span.end as usize);
                self.binding(
                    f.var.name,
                    f.var.destructure.as_ref(),
                    Some(Kind::Variable),
                    Reach::Scope(vec![body]),
                );
            }

            Stmt::GenericFor(f) => {
                let from = f
                    .filter
                    .as_ref()
                    .map_or(f.block.span.start, |e| e.span().start);
                let body = (from as usize, f.block.span.end as usize);

                for v in &f.vars {
                    self.binding(
                        v.name,
                        v.destructure.as_ref(),
                        Some(Kind::Variable),
                        Reach::Scope(vec![body]),
                    );
                }
            }

            // The `until` reads the locals of the body.
            Stmt::Repeat(r) => {
                self.block(&r.block.stmts, r.cond.span().end as usize, false, false);
                self.expr(&r.cond);

                return;
            }

            Stmt::If(i) => {
                for (c, _) in &i.branches {
                    self.cond(c);
                }
            }

            Stmt::While(w) => self.cond(&w.cond),

            Stmt::Match(m) => {
                for a in m.aliases.iter().flatten() {
                    self.declare(Some(Kind::Variable), *a, Reach::None);
                }

                for p in m.arms.iter().flat_map(|a| &a.patterns) {
                    self.pattern(p, None);
                }
            }

            Stmt::PatternLocal(p) => self.pattern(&p.pattern, Some(Kind::Variable)),

            Stmt::Attributed { stmt, .. } => {
                self.stmt(stmt, end, top, ns);

                return;
            }

            // A class has no lowering, and a `declare` names what another
            // program owns.
            Stmt::Class(_) | Stmt::Declare(_) => return,

            _ => {}
        }

        for c in stmt_children(s) {
            self.child(c);
        }
    }

    /// Whether a value is `require(...)` or `x:GetService(...)`.
    fn names_module(&self, e: &Expr) -> bool {
        match e {
            Expr::Call {
                func, method: None, ..
            } => matches!(**func, Expr::Name(n) if self.text(n.start as usize) == "require"),

            Expr::Call {
                method: Some(m), ..
            } => self.text(m.start as usize) == "GetService",

            Expr::TypeAssert { expr, .. } | Expr::Paren { inner: expr, .. } => {
                self.names_module(expr)
            }

            _ => false,
        }
    }

    /// The names one binding introduces: a plain name, or the names of a
    /// destructure. A shorthand entry, `{ a }`, is also the field it
    /// reads, so it keeps its name.
    fn binding(
        &mut self,
        name: TokSpan,
        destructure: Option<&'a Destructure>,
        kind: Option<Kind>,
        reach: Reach,
    ) {
        match destructure {
            None => self.declare(kind, name, reach),

            Some(Destructure::Table(fields)) => {
                for f in fields {
                    match (f.rest, f.rename) {
                        (false, None) => self.declare(kind, f.field, Reach::None),

                        (false, Some(local)) => {
                            self.role(f.field.start, Role::Member);
                            self.declare(kind, local, reach.clone());
                        }

                        (true, _) => self.declare(kind, f.field, reach.clone()),
                    }
                }
            }

            Some(Destructure::Array { items, rest }) => {
                for t in items.iter().chain(rest) {
                    self.declare(kind, *t, reach.clone());
                }
            }
        }
    }

    /// The parameters of a function whose body ends at `end`, or of a
    /// signature with no body when `end` is `None`. `self` is the
    /// receiver, not a name the author picks.
    fn params(&mut self, params: &'a [Param], end: Option<usize>) {
        for p in params {
            if p.is_vararg
                || (p.destructure.is_none() && self.text(p.name.start as usize) == "self")
            {
                continue;
            }

            let reach = match end {
                Some(end) => Reach::Scope(vec![(p.name.start as usize, end)]),

                None => Reach::None,
            };
            self.binding(p.name, p.destructure.as_ref(), Some(Kind::Parameter), reach);
        }
    }

    fn body(&mut self, body: &'a FunctionBody) {
        self.params(&body.params, Some(body.span.end as usize));
        self.block(
            &body.block.stmts,
            body.block.span.end as usize,
            false,
            false,
        );
    }

    fn cond(&mut self, c: &'a Cond) {
        if let Cond::Local { bindings, .. } = c {
            for b in bindings {
                let kind = if b.is_const {
                    Kind::Const
                } else {
                    Kind::Variable
                };
                self.pattern(&b.pattern, Some(kind));
            }
        }
    }

    /// The names a pattern binds. A bare name in a `match` arm may name
    /// a unit variant, so those bind with no kind; `if local x = e`
    /// passes the kind of its local.
    fn pattern(&mut self, p: &'a Pattern, kind: Option<Kind>) {
        match p {
            Pattern::Bind(t) => self.declare(kind, *t, Reach::None),

            Pattern::Path(t) => self.path(*t, Role::Member),

            Pattern::Variant { name, args, .. } => {
                self.path(*name, Role::Member);

                for a in args {
                    self.pattern(a, None);
                }
            }

            Pattern::Struct { name, fields, .. } => {
                if let Some(n) = name {
                    self.path(*n, Role::Value);
                }

                for f in fields {
                    match &f.pattern {
                        None => self.declare(None, f.field, Reach::None),

                        Some(inner) => {
                            self.role(f.field.start, Role::Member);
                            self.pattern(inner, None);
                        }
                    }
                }
            }

            Pattern::Array { items, rest, .. } => {
                for i in items {
                    self.pattern(i, None);
                }

                if let Some(r) = rest {
                    self.declare(None, *r, Reach::None);
                }
            }

            Pattern::Or(a, b, _) => {
                self.pattern(a, None);
                self.pattern(b, None);
            }

            Pattern::Literal(e) => self.expr(e),

            Pattern::Wildcard(_) => {}
        }
    }

    fn expr(&mut self, e: &'a Expr) {
        match e {
            Expr::Name(t) => self.role(t.start, Role::Value),

            Expr::Index {
                key: IndexKey::Field(t),
                ..
            }
            | Expr::MethodRef { name: t, .. }
            | Expr::Call {
                method: Some(t), ..
            }
            | Expr::Child {
                name: ChildName::Name(t),
                ..
            } => self.role(t.start, Role::Member),

            Expr::Table { fields, .. } => {
                for f in fields {
                    if let TableField::Named { name, .. } = f {
                        self.role(name.start, Role::Member);
                    }
                }
            }

            Expr::Is { name, .. } => self.path(*name, Role::Type),

            // `$twice(x)` calls a macro, `$Math.twice(x)` one in a
            // namespace.
            Expr::Macro { name, .. } => self.path(*name, Role::Member),

            Expr::IfElse { branches, .. } => {
                for (c, _) in branches {
                    self.cond(c);
                }
            }

            Expr::Match(m) => {
                for a in m.aliases.iter().flatten() {
                    self.declare(Some(Kind::Variable), *a, Reach::None);
                }

                for p in m.arms.iter().flat_map(|a| &a.patterns) {
                    self.pattern(p, None);
                }
            }

            _ => {}
        }

        for c in expr_children(e) {
            self.child(c);
        }
    }

    fn child(&mut self, c: Child<'a>) {
        match c {
            Child::Expr(e) => self.expr(e),

            Child::Block(b) => self.block(&b.stmts, b.span.end as usize, false, false),

            Child::Function(f) => self.body(f),
        }
    }

    /// `{ name: T }` and `(name: T) -> U` in a type name a field or a
    /// parameter of the type. No expression writes a name there: a
    /// method call on a name, `{ x:m() }`, reads the name as a value.
    fn type_fields(&mut self) {
        for j in 1..self.toks.len() {
            if self.roles[j] == Role::Unknown
                && self.toks[j].kind == TokKind::Ident
                && self.text(j + 1) == ":"
                && matches!(self.text(j - 1), "{" | "," | ";" | "(")
            {
                self.roles[j] = Role::Member;
            }
        }
    }

    /// `@name` names an attribute, and `@Space.name` one in a namespace.
    fn attributes(&mut self) {
        for j in 1..self.toks.len() {
            if self.text(j - 1) != "@" || self.toks[j].kind != TokKind::Ident {
                continue;
            }

            let mut k = j;
            self.roles[k] = match self.text(k + 1) == "." {
                true => Role::Value,

                false => Role::Member,
            };

            while self.text(k + 1) == "." && k + 2 < self.roles.len() {
                k += 2;
                self.roles[k] = Role::Member;
            }
        }
    }

    /// The tokens a rename of `d` writes, or `None` when the rename is
    /// not safe: the name leaves the file, a binding of the same name
    /// has a scope the walk does not know, or a token of the name has a
    /// role the tree does not explain.
    fn rename(&self, d: &Decl, by_text: &HashMap<&str, Vec<usize>>) -> Option<Vec<usize>> {
        let old = self.text(d.tok);
        let within = |j: usize| match &d.reach {
            Reach::None => false,
            Reach::File => true,
            Reach::Scope(ranges) => ranges.iter().any(|&(a, b)| a <= j && j < b),
        };

        if matches!(d.reach, Reach::None) || self.exported.contains(&old) {
            return None;
        }

        // A nested binding of the same name shadows this one: its scope
        // keeps its own name.
        let mut shadows: Vec<(usize, usize)> = Vec::new();

        for e in &self.decls {
            if e.tok == d.tok || e.is_member() || self.text(e.tok) != old {
                continue;
            }

            // A type and a value of one name read as each other.
            if e.is_type() != d.is_type() {
                return None;
            }

            if !within(e.tok) {
                continue;
            }

            match &e.reach {
                Reach::Scope(ranges) => shadows.extend(ranges),

                _ => return None,
            }
        }

        let mut out = Vec::new();

        for &j in by_text.get(old)? {
            if !within(j) || shadows.iter().any(|&(a, b)| a <= j && j < b) {
                continue;
            }

            match self.roles[j] {
                _ if j == d.tok => out.push(j),
                Role::Value => out.push(j),
                Role::Type if d.is_type() => out.push(j),
                Role::Type | Role::Member => {}
                Role::Unknown => return None,
            }
        }

        Some(out)
    }
}

/// The naming lint over one file. A name that breaks its style carries
/// a rename when one is safe: the file alone reads the name, and the
/// new name is free.
/// What an `.alx` file's markup says about its functions: the ranges its
/// markup lowered to, in the lowered text, and every name a tag writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markup {
    pub regions: Vec<(u32, u32)>,
    pub tags: std::collections::HashSet<String>,
}

impl Markup {
    /// The markup of one `.alx` source, from the regions its lowering
    /// reports.
    pub fn of(src: &str, regions: &[luaux::compile::Region]) -> Self {
        let mut tags = std::collections::HashSet::new();

        for r in regions {
            let text = src.get(r.src_start..r.src_end).unwrap_or_default();

            for (i, _) in text.match_indices('<') {
                let name: String = text[i + 1..]
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();

                if !name.is_empty() {
                    tags.insert(name);
                }
            }
        }

        Self {
            regions: regions
                .iter()
                .map(|r| (r.out_start as u32, r.out_end as u32))
                .collect(),
            tags,
        }
    }
}

/// The functions of a file that return markup, by the token of their
/// name: a `return` whose value lies in a markup region.
fn markup_returns(toks: &[Tok], block: &Block, markup: &Markup, out: &mut HashSet<usize>) {
    fn returns(toks: &[Tok], block: &Block, markup: &Markup) -> bool {
        block.stmts.iter().any(|s| match s {
            // A region inside the value: `return <Frame />`, and
            // `return ( <Frame /> )` in parentheses too.
            Stmt::Return(r) => r.values.iter().any(|v| {
                let span = v.span();
                let from = toks.get(span.start as usize).map_or(0, |t| t.start);
                let to = toks
                    .get((span.end as usize).saturating_sub(1))
                    .map_or(0, |t| t.end);

                markup.regions.iter().any(|(a, _)| from <= *a && *a < to)
            }),

            // A nested function returns for itself.
            Stmt::LocalFunction(_) | Stmt::Function(_) => false,

            other => stmt_children(other).iter().any(|c| match c {
                Child::Block(b) => returns(toks, b, markup),

                _ => false,
            }),
        })
    }

    for s in &block.stmts {
        match s.under_default() {
            Stmt::LocalFunction(f) => {
                if returns(toks, &f.body.block, markup) {
                    out.insert(f.name.start as usize);
                }

                markup_returns(toks, &f.body.block, markup, out);
            }

            Stmt::Function(f) => {
                if let [name] = f.path.as_slice()
                    && returns(toks, &f.body.block, markup)
                {
                    out.insert(name.start as usize);
                }

                markup_returns(toks, &f.body.block, markup, out);
            }

            // `local Row = function(props) return <Frame /> end`.
            Stmt::Local(l) if l.names.len() == 1 && l.values.len() == 1 => {
                if let Expr::Function { body, .. } = &l.values[0] {
                    if returns(toks, &body.block, markup) {
                        out.insert(l.names[0].name.start as usize);
                    }

                    markup_returns(toks, &body.block, markup, out);
                }
            }

            other => {
                for c in stmt_children(other) {
                    if let Child::Block(b) = c {
                        markup_returns(toks, b, markup, out);
                    }
                }
            }
        }
    }
}

/// A binding's name, the token that declares it, and the token ranges
/// its scope holds; `None` for a scope the walk does not know.
pub(crate) type ScopedBinding = (String, usize, Option<Vec<(usize, usize)>>);

/// Each value binding of a block, other than an import, a remote or a
/// namespace, with the token ranges its scope holds, end exclusive: a
/// local, a parameter, a loop variable, a function. A namespace holds
/// remotes as `Net.Up`, so it is no shadow. `None` stands for a scope
/// the walk does not know, such as a name a pattern binds, and a
/// reader takes it to hold every token.
pub(crate) fn scoped_bindings(src: &str, toks: &[Tok], block: &Block) -> Vec<ScopedBinding> {
    let mut w = Walk {
        src,
        toks,
        decls: Vec::new(),
        roles: vec![Role::Unknown; toks.len()],
        exported: Vec::new(),
        const_locals: HashSet::new(),
        promoted: Vec::new(),
    };
    w.block(&block.stmts, toks.len(), true, false);

    let imports: HashSet<usize> = block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Import(i) => Some(crate::desugar::import_names(i)),

            _ => None,
        })
        .flatten()
        .map(|t| t.start as usize)
        .collect();

    w.decls
        .iter()
        .filter(|d| !imports.contains(&d.tok))
        .filter(|d| {
            !d.kind.is_some_and(|k| {
                k.is_member()
                    || k.is_type()
                    || matches!(
                        k,
                        Kind::Remote | Kind::Namespace | Kind::Attribute | Kind::Macro
                    )
            })
        })
        .map(|d| {
            let reach = match &d.reach {
                Reach::Scope(ranges) => Some(ranges.clone()),

                Reach::File => Some(vec![(0, toks.len())]),

                Reach::None => None,
            };

            (w.text(d.tok).to_string(), d.tok, reach)
        })
        .collect()
}

/// `fixed` holds the byte each member name starts at that an attribute
/// contract asks for: the contract fixes the name, so a rename breaks it.
pub(crate) fn lints(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    naming: &Naming,
    markup: &Markup,
    fixed: &HashSet<u32>,
) -> Vec<Lint> {
    let mut w = Walk {
        src,
        toks,
        decls: Vec::new(),
        roles: vec![Role::Unknown; toks.len()],
        exported: Vec::new(),
        const_locals: match naming.locals_as_const {
            true => crate::flux::prefer_const_fixes(src)
                .iter()
                .map(|f| f.start)
                .collect(),

            false => HashSet::new(),
        },
        promoted: Vec::new(),
    };

    for t in &chunk.type_names {
        w.role(t.start, Role::Type);
    }

    w.block(&chunk.block.stmts, toks.len(), true, false);
    w.attributes();
    w.type_fields();

    let mut by_text: HashMap<&str, Vec<usize>> = HashMap::new();

    for (i, t) in toks.iter().enumerate() {
        if t.kind == TokKind::Ident {
            by_text.entry(t.text(src)).or_default().push(i);
        }
    }

    // Two old names that convert to one new name collide, so the first
    // takes it and the second waits for the next pass.
    let mut claimed: HashMap<String, &str> = HashMap::new();
    let mut out = Vec::new();

    // In an `.alx` file a function that returns markup, or that a tag
    // names, is a component and takes the component styles.
    let mut components = HashSet::new();
    let classes = class_tables(src, toks, &chunk.block.stmts);

    if !markup.regions.is_empty() {
        markup_returns(toks, &chunk.block, markup, &mut components);
    }

    for (at, d) in w.decls.iter().enumerate() {
        let Some(kind) = d.kind else { continue };
        let name = w.text(d.tok);
        // A local that holds a component is one too.
        let kind = match kind {
            Kind::Function | Kind::Variable | Kind::Const
                if components.contains(&d.tok) || markup.tags.contains(name) =>
            {
                Kind::Component
            }

            Kind::Variable | Kind::Const if classes.contains(&d.tok) => Kind::Struct,

            other => other,
        };
        let styles = naming.get(kind);
        let Some(first) = styles.0.first() else {
            continue;
        };

        if name.starts_with('_')
            || styles.fits(name)
            || (d.is_member() && fixed.contains(&toks[d.tok].start))
        {
            continue;
        }

        let fixed = first.convert(name);
        let free = !by_text.contains_key(fixed.as_str())
            && !crate::fmt::is_keyword(&fixed)
            && !crate::desugar::attributes::LUAU_GLOBALS.contains(&fixed.as_str())
            && fixed != "self"
            && claimed.get(&fixed).is_none_or(|old| *old == name);
        let fix = match free {
            true => w.rename(d, &by_text),

            false => None,
        }
        .map(|at| {
            claimed.insert(fixed.clone(), name);
            let edit = |j: usize| Fix::new(src, toks[j].start, toks[j].end, fixed.clone());

            Fix {
                more: at
                    .iter()
                    .filter(|j| **j != d.tok)
                    .map(|j| edit(*j))
                    .collect(),
                ..edit(d.tok)
            }
        });
        let key = kind.key();
        let article = if key.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        };

        // A `local` that `prefer_const` makes a `const` says why it is one.
        let what = match kind == Kind::Const && w.promoted.contains(&at) {
            true => "never assigned again, so it is a const".to_string(),

            false if classes.contains(&d.tok) => {
                "a class table, which takes the struct style".to_string()
            }

            false => format!("{article} {key}"),
        };

        out.push(Lint {
            name: LINT,
            start: toks[d.tok].start,
            end: toks[d.tok].end,
            message: format!(
                "`{name}` is {what}, and {key}s are {} here: `{fixed}`",
                styles.describe()
            ),
            fix,
        });
    }

    out
}

/// The name token of each top-level local that holds a class or a
/// module table: the file writes `X.__index = X` or a colon method
/// `function X:m()` on it. Such a name reads as a type, `Timer.new()`,
/// so it takes the struct style. A plain data table stays a variable.
fn class_tables(src: &str, toks: &[Tok], stmts: &[Stmt]) -> HashSet<usize> {
    let text = |span: TokSpan| span.text(src, toks);
    let mut owners: HashSet<&str> = HashSet::new();

    for stmt in stmts {
        match stmt.under_default() {
            Stmt::Assign(a) => {
                if let ([Expr::Index { object, key, .. }], [Expr::Name(v)]) =
                    (a.targets.as_slice(), a.values.as_slice())
                    && let (Expr::Name(o), IndexKey::Field(k)) = (object.as_ref(), key)
                    && text(*k) == "__index"
                    && text(*o) == text(*v)
                {
                    owners.insert(text(*o));
                }
            }

            Stmt::Function(f) if f.is_method && f.path.len() == 2 => {
                owners.insert(text(f.path[0]));
            }

            _ => {}
        }
    }

    stmts
        .iter()
        .filter_map(|stmt| match stmt.under_default() {
            Stmt::Local(l) if l.names.len() == 1 && l.names[0].destructure.is_none() => {
                let name = l.names[0].name;

                owners.contains(text(name)).then_some(name.start as usize)
            }

            _ => None,
        })
        .collect()
}

/// The source with each name that breaks its `[lint.naming]` style
/// renamed, for `alloy fmt`. `None` when nothing changes: `[fmt]
/// fix_naming` is off, the lint is off for the file, the file does not
/// parse, or no rename is safe. The lint's own rules apply, so a line
/// that `@allow`, `--@alloy-ignore`, or `--@alloy-preserve` covers keeps
/// its name.
pub fn renamed(src: &str, options: &crate::config::FmtConfig) -> Option<String> {
    use crate::directives::line_of;

    if !options.fix_naming {
        return None;
    }

    let directives = crate::directives::scan(src);

    if crate::lint::level_in(&options.lint, &directives, LINT) == crate::lint::Level::Allow {
        return None;
    }

    let parsed = alloy_syntax::parse_lenient(src, crate::fmt::parse_options()).ok()?;

    if parsed
        .diagnostics
        .iter()
        .any(|d| !d.message.ends_with(crate::fmt::NEEDS_AS))
    {
        return None;
    }

    let fixes: Vec<Lint> = lints(
        src,
        &parsed.lexed.toks,
        &parsed.chunk,
        &options.lint.naming,
        &Markup::default(),
        &HashSet::new(),
    )
    .into_iter()
    .filter(|l| {
        let line = line_of(src, l.start as usize);

        directives.allows_lint(line, LINT) && !directives.preserves(line)
    })
    .collect();
    let (text, n) = crate::lint::apply_fixes(src, &fixes);

    (n > 0).then_some(text)
}

/// The rename lints of `src` once the `prefer_const` rewrites `consts`
/// land. A local that becomes a `const` then takes the const style, as
/// in `alloy fmt`. `local` and `const` have one length, so each offset
/// holds in `src`.
pub fn lints_after_consts(src: &str, consts: &[Fix], naming: &Naming) -> Vec<Lint> {
    let text = crate::std_names::apply(src, consts);
    let Ok(parsed) = alloy_syntax::parse_lenient(&text, crate::fmt::parse_options()) else {
        return Vec::new();
    };

    lints(
        &text,
        &parsed.lexed.toks,
        &parsed.chunk,
        naming,
        &Markup::default(),
        &HashSet::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_style_reads_the_case_and_one_letter_fits_what_it_can() {
        use Style::*;

        let fits = |name: &str| -> Vec<Style> {
            [Snake, Camel, Pascal, Screaming]
                .into_iter()
                .filter(|s| s.fits(name))
                .collect()
        };

        assert_eq!(fits("player_count"), vec![Snake]);
        assert_eq!(fits("playerCount"), vec![Camel]);
        assert_eq!(fits("PlayerCount"), vec![Pascal]);
        assert_eq!(fits("PLAYER_COUNT"), vec![Screaming]);
        assert_eq!(fits("x"), vec![Snake, Camel]);
        assert_eq!(fits("x2"), vec![Snake, Camel]);
        assert_eq!(fits("T"), vec![Pascal, Screaming]);
        assert_eq!(fits("HTTPServer"), vec![Pascal]);
        assert_eq!(fits("player__count"), vec![]);
        assert_eq!(fits("Player_Count"), vec![]);
        assert!(Any.fits("any_Name"));
    }

    #[test]
    fn a_conversion_splits_words_and_acronyms() {
        use Style::*;

        assert_eq!(words("HTTPServer"), vec!["HTTP", "Server"]);
        assert_eq!(words("myValue2Go"), vec!["my", "Value2", "Go"]);
        assert_eq!(Snake.convert("HTTPServer"), "http_server");
        assert_eq!(Camel.convert("HTTPServer"), "httpServer");
        assert_eq!(Screaming.convert("HTTPServer"), "HTTP_SERVER");
        assert_eq!(Pascal.convert("HTTP_SERVER"), "HttpServer");
        assert_eq!(Snake.convert("myValue"), "my_value");
        assert_eq!(Pascal.convert("player_state"), "PlayerState");
        assert_eq!(Camel.convert("max_hp"), "maxHp");
        assert_eq!(Snake.convert("getHTTP"), "get_http");
        assert_eq!(Snake.convert("vector3Value"), "vector3_value");
    }

    fn naming(text: &str) -> Result<Naming, String> {
        crate::config::Config::parse(text, std::path::Path::new("alloy.toml"))
            .map(|c| c.lint.naming)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_style_is_a_string_or_a_list() {
        let n = naming("[lint.naming]\nvariable = \"camelCase\"\nconst = [\"SCREAMING_SNAKE_CASE\", \"snake_case\"]\ntype = \"any\"\n").unwrap();
        assert_eq!(n.variable, Styles(vec![Style::Camel]));
        assert_eq!(n.r#const, Styles(vec![Style::Screaming, Style::Snake]));
        assert_eq!(n.r#type, Styles(vec![Style::Any]));
        // The rest keep their defaults.
        assert_eq!(n.function, Styles(vec![Style::Snake]));
        assert_eq!(n.remote, Styles(vec![Style::Pascal]));

        let bad = naming("[lint.naming]\nvariable = \"kebab-case\"\n").unwrap_err();
        assert!(
            bad.contains("`\"kebab-case\"` is not a naming style"),
            "{bad}"
        );
        assert!(naming("[lint.naming]\nvariable = []\n").is_err());
        assert!(naming("[lint.naming]\nvariable = 1\n").is_err());
        assert!(naming("[lint.naming]\nlocals = \"snake_case\"\n").is_err());
    }

    /// The naming lints of a source under a config, with their messages.
    fn hits_with(src: &str, naming: &Naming) -> Vec<String> {
        let options = crate::EmitOptions {
            naming: naming.clone(),
            ..Default::default()
        };

        crate::compile_with(src, &options)
            .unwrap()
            .lints
            .into_iter()
            .filter(|l| l.name == LINT)
            .map(|l| l.message)
            .collect()
    }

    fn hits(src: &str) -> Vec<String> {
        hits_with(src, &Naming::default())
    }

    #[test]
    fn each_kind_reads_its_own_default() {
        let src = concat!(
            "local playerCount = 1\n",
            "playerCount += 1\n",
            "const maxHp = 2\n",
            "const MAX_HP = 3\n",
            "local function LoadMap(mapName: string) return mapName end\n",
            "struct player_state\n    CoinCount: number\nend\n",
            "impl player_state as\n    function GetCoins(self): number return self.CoinCount end\nend\n",
            "enum color as red, Green end\n",
            "trait shape as\n    function Area(self): number\nend\n",
            "interface has_name as\n    name: string\nend\n",
            "type point_list = { number }\n",
            "namespace util as\n    const Two = 2\nend\n",
            "remote fire_shot(power: number) from client\n",
            "print(playerCount, maxHp, MAX_HP, LoadMap(\"a\"), color, util, fire_shot)\n",
        );
        assert_eq!(
            hits(src),
            vec![
                "`playerCount` is a variable, and variables are snake_case here: `player_count`",
                "`maxHp` is a const, and consts are snake_case or SCREAMING_SNAKE_CASE here: `max_hp`",
                "`LoadMap` is a function, and functions are snake_case here: `load_map`",
                "`mapName` is a parameter, and parameters are snake_case here: `map_name`",
                "`player_state` is a struct, and structs are PascalCase here: `PlayerState`",
                "`CoinCount` is a field, and fields are snake_case here: `coin_count`",
                "`GetCoins` is a method, and methods are snake_case here: `get_coins`",
                "`color` is an enum, and enums are PascalCase here: `Color`",
                "`red` is a variant, and variants are PascalCase here: `Red`",
                "`shape` is a trait, and traits are PascalCase here: `Shape`",
                "`Area` is a method, and methods are snake_case here: `area`",
                "`has_name` is an interface, and interfaces are PascalCase here: `HasName`",
                "`point_list` is a type, and types are PascalCase here: `PointList`",
                "`util` is a namespace, and namespaces are PascalCase here: `Util`",
                "`Two` is a const, and consts are snake_case or SCREAMING_SNAKE_CASE here: `two`",
                "`fire_shot` is a remote, and remotes are PascalCase here: `FireShot`",
            ]
        );

        // `_` marks a name as unused, a service or a module keeps its
        // own case, `self` is the receiver, and a function stored on a
        // table is a member of the table. `M` carries a colon method,
        // so it is a module table and takes the struct style. A plain
        // data table stays a local and takes the variable style.
        assert_eq!(
            hits_with(
                "local _unusedThing = 1\nlocal Players = game:GetService(\"Players\")\nlocal M = {}\nfunction M:Destroy() end\nfunction M.OnLoad() end\nlocal Config = { a = 1 }\nprint(Players, Config)\n",
                &Naming {
                    locals_as_const: false,
                    ..Naming::default()
                }
            ),
            vec!["`Config` is a variable, and variables are snake_case here: `config`"]
        );

        let camel = Naming {
            variable: Styles(vec![Style::Camel]),
            locals_as_const: false,
            ..Naming::default()
        };
        assert_eq!(
            hits_with(
                "local playerCount = 1\nlocal player_count2 = 2\nprint(playerCount, player_count2)\n",
                &camel
            ),
            vec!["`player_count2` is a variable, and variables are camelCase here: `playerCount2`"]
        );
    }

    /// A `requires` clause fixes a member's name. The lint asked for
    /// `init` where the `each` example needs `Init`, and the rename then
    /// broke the contract. A member no clause names still reports.
    #[test]
    fn a_name_a_contract_requires_keeps_its_case() {
        let src = concat!(
            "enum Lifecycle\n    Init\n    Start\nend\n",
            "attribute provider(lifecycles: Lifecycle[]) on impl as\n",
            "    requires private function each lifecycles(self)\nend\n",
            "attribute service on impl as\n    requires function Boot(self)\nend\n",
            "struct Data\n    n: number\nend\n",
            "@provider({ lifecycles = [ Lifecycle.Init, Lifecycle.Start ] })\n@service\n",
            "impl Data\n",
            "    private function Init(self) print(self.n) end\n",
            "    private function Start(self) print(self.n) end\n",
            "    function Boot(self): () print(self.n) end\n",
            "    function Other(self): () print(self.n) end\n",
            "end\n",
        );
        assert_eq!(
            hits(src),
            vec!["`Other` is a method, and methods are snake_case here: `other`"]
        );
    }

    /// The skip keyed by name alone, so one `@service impl Door` turned
    /// the lint off for `Start` on every type in the file. A `Start` that
    /// no contract asks for reports again.
    #[test]
    fn a_contract_fixes_the_name_on_its_own_type_alone() {
        let src = concat!(
            "attribute service on impl as\n    requires function Start(self)\nend\n",
            "struct Door\n    n: number\nend\n",
            "struct Window\n    n: number\nend\n",
            "@service\nimpl Door\n    function Start(self): () print(self.n) end\nend\n",
            "impl Window\n    function Start(self): () print(self.n) end\nend\n",
        );
        let out = crate::compile(src).unwrap();
        let hits: Vec<(u32, String)> = out
            .lints
            .into_iter()
            .filter(|l| l.name == LINT)
            .map(|l| (l.start, l.message))
            .collect();
        let window = src.rfind("Start").unwrap() as u32;
        assert_eq!(
            hits,
            vec![(
                window,
                "`Start` is a method, and methods are snake_case here: `start`".to_string()
            )]
        );
    }

    /// A `local` that nothing assigns again becomes a `const` under
    /// `prefer_const`, so it takes the const style. The variable style
    /// named `maxHealth`, and `prefer_const` then `alloy fmt` renamed it
    /// again to `MAX_HEALTH`.
    #[test]
    fn a_local_that_prefer_const_makes_a_const_takes_the_const_style() {
        let src = "local max_health = 100\nlocal walk_speed = 16\nwalk_speed = 20\nprint(max_health, walk_speed)\n";
        let naming = Naming {
            variable: Styles(vec![Style::Camel]),
            r#const: Styles(vec![Style::Screaming]),
            ..Naming::default()
        };

        assert_eq!(
            hits_with(src, &naming),
            vec![
                "`max_health` is never assigned again, so it is a const, and consts are SCREAMING_SNAKE_CASE here: `MAX_HEALTH`",
                "`walk_speed` is a variable, and variables are camelCase here: `walkSpeed`",
            ]
        );

        // With `prefer_const` and `[fmt] prefer_const` both off, the
        // local stays a local.
        let off = |text: &str| {
            crate::config::Config::parse(
                &format!("[lint.naming]\nvariable = \"camelCase\"\n{text}"),
                std::path::Path::new("alloy.toml"),
            )
            .unwrap()
            .lint
            .naming
        };

        assert!(off("").locals_as_const);
        assert!(off("[fmt]\nprefer_const = false\n").locals_as_const);
        assert!(
            !off("[fmt]\nprefer_const = false\n[lint.rules]\nprefer_const = \"allow\"\n")
                .locals_as_const
        );
        assert_eq!(
            hits_with(
                src,
                &Naming {
                    locals_as_const: false,
                    ..naming
                }
            )[0],
            "`max_health` is a variable, and variables are camelCase here: `maxHealth`"
        );
    }

    #[test]
    fn a_trait_impl_takes_its_method_names_from_the_trait() {
        let src = "trait Shape as\n    function area(self): number\nend\nstruct Sq as\n    s: number\nend\nimpl Shape for Sq as\n    function area(self): number return self.s end\nend\nprint(Sq)\n";
        assert_eq!(hits(src), Vec::<String>::new());
    }

    /// The rewrite of a source under the default styles.
    fn fixed(src: &str) -> String {
        let lints: Vec<Lint> = crate::compile(src)
            .unwrap()
            .lints
            .into_iter()
            .filter(|l| l.name == LINT)
            .collect();

        crate::lint::apply_fixes(src, &lints).0
    }

    #[test]
    fn a_rename_reaches_every_read_in_scope() {
        let src = "local function addCoins(playerCoins: number, t: { playerCoins: number })\n    local newTotal = playerCoins + t.playerCoins\n    return { playerCoins = newTotal, `{newTotal}` }\nend\nprint(addCoins(1, { playerCoins = 2 }))\n";
        let out = fixed(src);
        assert_eq!(
            out,
            "local function add_coins(player_coins: number, t: { playerCoins: number })\n    local new_total = player_coins + t.playerCoins\n    return { playerCoins = new_total, `{new_total}` }\nend\nprint(add_coins(1, { playerCoins = 2 }))\n"
        );
        assert!(crate::compile(&out).unwrap().diagnostics.is_empty());
    }

    #[test]
    fn a_rename_skips_a_scope_that_shadows_it() {
        let src = "local myValue = 1\nlocal function f(myValue: number)\n    return myValue\nend\nlocal myValue2 = myValue\nprint(f(myValue), myValue2)\n";
        let camel_params = Naming {
            parameter: Styles(vec![Style::Camel]),
            ..Naming::default()
        };
        let options = crate::EmitOptions {
            naming: camel_params,
            ..Default::default()
        };
        let lints: Vec<Lint> = crate::compile_with(src, &options)
            .unwrap()
            .lints
            .into_iter()
            .filter(|l| l.name == LINT)
            .collect();
        assert_eq!(
            crate::lint::apply_fixes(src, &lints).0,
            "local my_value = 1\nlocal function f(myValue: number)\n    return myValue\nend\nlocal my_value2 = my_value\nprint(f(my_value), my_value2)\n"
        );

        // The value of a local reads the name before the local binds
        // it, so the outer rename reaches it and the inner one does not.
        assert_eq!(
            fixed(
                "local myCount = 1\ndo\n    local myCount = myCount + 1\n    print(myCount)\nend\nprint(myCount)\n"
            ),
            "local my_count = 1\ndo\n    local my_count = my_count + 1\n    print(my_count)\nend\nprint(my_count)\n"
        );
        assert_eq!(
            fixed(
                "local count = 1\ndo\n    local newCount = count + 1\n    print(newCount)\nend\n"
            ),
            "local count = 1\ndo\n    local new_count = count + 1\n    print(new_count)\nend\n"
        );
    }

    #[test]
    fn a_rename_that_is_not_safe_is_left_to_the_author() {
        // Exported, another file reads the name. Under an attribute,
        // the runtime may read it: `@test` names a test.
        for src in [
            "export function loadMap() end\n",
            "export const maxHp = 1\n",
            "local function loadMap() end\nexport { loadMap }\n",
            "export struct player_state as\n    x: number\nend\n",
            "@test\nfunction loadsTheMap() end\n",
        ] {
            let lints: Vec<Lint> = crate::compile(src)
                .unwrap()
                .lints
                .into_iter()
                .filter(|l| l.name == LINT)
                .collect();
            assert_eq!(lints.len(), 1, "{src}");
            assert!(lints[0].fix.is_none(), "{src}");
        }

        // The new name is taken.
        let src = "local myValue = 1\nlocal my_value = 2\nprint(myValue, my_value)\n";
        assert_eq!(fixed(src), src);
        // Two names that convert to one: the first takes it.
        assert_eq!(
            fixed("local myValue = 1\nlocal MyValue = 2\nprint(myValue)\nprint(MyValue)\n"),
            "local my_value = 1\nlocal MyValue = 2\nprint(my_value)\nprint(MyValue)\n"
        );
        // A shorthand entry reads the field of its name.
        let src = "local t = { myValue = 1 }\nlocal { myValue } = t\nprint(myValue)\n";
        assert_eq!(fixed(src), src);
        // A keyword, and a global the emit may call.
        let src = "local End = 1\nlocal Game = 2\nprint(End, Game)\n";
        assert_eq!(fixed(src), src);
        // A field, a variant, and a method keep their names.
        let src = "struct S as\n    myField: number\nend\nenum E as red end\nimpl S as\n    function GetIt(self): number return self.myField end\nend\nprint(E.red)\n";
        assert_eq!(fixed(src), src);
    }

    #[test]
    fn a_file_level_type_renames_its_type_reads() {
        let src = "struct player_state as\n    coins: number\nend\nimpl player_state as\n    function get(self): number return self.coins end\nend\nlocal function make(): player_state\n    return new player_state { coins = 1 }\nend\nlocal s: player_state = make()\nprint(s:get(), s is player_state)\n";
        let out = fixed(src);
        assert_eq!(out.matches("PlayerState").count(), 6, "{out}");
        assert!(!out.contains("player_state"), "{out}");
        assert!(crate::compile(&out).unwrap().diagnostics.is_empty());
    }

    /// `alloy fmt` renamed `local Timer = {}` with `Timer.__index =
    /// Timer` to `timer` everywhere. A table the file writes a class
    /// shape or a colon method on reads as a type, so it takes the
    /// struct style. A plain data table stays a variable.
    #[test]
    fn a_class_or_module_table_takes_the_struct_style() {
        let class = "local Timer = {}\nTimer.__index = Timer\nfunction Timer.new(d: number)\n    return setmetatable({ left = d }, Timer)\nend\nreturn Timer\n";
        assert_eq!(fixed(class), class);
        let module = "local Shop = {}\nfunction Shop:open() end\nreturn Shop\n";
        assert_eq!(fixed(module), module);

        let lower = "local timer = {}\ntimer.__index = timer\nreturn timer\n";
        let lints: Vec<Lint> = crate::compile(lower)
            .unwrap()
            .lints
            .into_iter()
            .filter(|l| l.name == LINT)
            .collect();
        assert_eq!(
            lints[0].message,
            "`timer` is a class table, which takes the struct style, and structs are PascalCase here: `Timer`"
        );
        assert_eq!(
            fixed(lower),
            "local Timer = {}\nTimer.__index = Timer\nreturn Timer\n"
        );

        assert_eq!(
            fixed("local Prices = { sword = 1 }\nprint(Prices.sword)\n"),
            "local prices = { sword = 1 }\nprint(prices.sword)\n"
        );
    }
}
