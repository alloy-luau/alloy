//! What a dotted path reaches: `<Lib.Widgets.button/>` and `Lib.Store`.
//!
//! One walk answers two questions. A tag slot wants the values a name
//! holds, because a component is a function: a namespace, a table, a
//! struct with an `impl`, and a module another file exports all hold
//! one. A type slot wants the types a name declares. The walk over
//! namespaces and imports is the same for both; only the leaf list
//! differs, so `Want` picks it.
//!
//! A value and a type never mix. `Scribe.version` is no type, and
//! `Scribe.Store` is no value.

use alloy_syntax::ast::{
    DefaultExport, Expr, ImportKind, ImportSpec, NamespaceDecl, Stmt, TableField, TokSpan,
};
use alloy_syntax::lexer::Tok;

/// The LSP completion kinds this module hands out.
const FUNCTION: u64 = 3;
const MODULE: u64 = 9;
const STRUCT: u64 = 7;
const FIELD: u64 = 5;
const INTERFACE: u64 = 8;
const ENUM: u64 = 13;

/// Which half of a module the caret asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    /// The names that hold a value: a function, a table, a field.
    Values,
    /// The names that declare a type, written where the caret sits: a
    /// struct, an enum, a trait, an interface, a type alias, and a
    /// namespace, which the path walks through to a type under it.
    Types,
    /// The types a module binding reaches. Luau writes such a path as
    /// `M.T` and no deeper, so a namespace leads nowhere here and
    /// stays out of the list.
    ImportedTypes,
}

impl Want {
    fn is_types(self) -> bool {
        self != Want::Values
    }

    /// Whether a namespace belongs in the list: it does where the path
    /// can walk on through it, and a path through a module binding
    /// cannot.
    fn takes_a_namespace(self) -> bool {
        self == Want::Types
    }
}

/// How far an import chain is followed before the walk gives up.
const MAX_DEPTH: u8 = 4;

/// One name a tag path reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// The LSP completion kind.
    pub kind: u64,
    /// What the name is, as the completion item's detail.
    pub detail: String,
    /// The line the source writes for it, for a hover.
    pub signature: Option<String>,
}

impl Member {
    fn new(name: &str, kind: u64, detail: &str, signature: Option<String>) -> Self {
        Member {
            name: name.to_string(),
            kind,
            detail: detail.to_string(),
            signature,
        }
    }

    /// A function sorts first: it is the member a tag can stand on.
    pub fn sort_key(&self) -> String {
        match self.kind == FUNCTION {
            true => format!("0{}", self.name),

            false => format!("1{}", self.name),
        }
    }
}

/// Reads the source of the module an import spec names. The proxy
/// answers from an open document first, then from disk.
pub type Load<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Whether a module spec names a module that returns one value. A
/// default import of one binds `require(...)` whole, so `M.T` reads a
/// type the module exports; a module with an export table binds the
/// `default` field of it instead, and no type hangs off that.
pub type Plain<'a> = dyn Fn(&str) -> bool + 'a;

/// What the walk needs from the files around it.
struct Reader<'a> {
    load: &'a Load<'a>,
    plain: &'a Plain<'a>,
}

/// The values the dotted path reaches, with the functions first.
/// `path` is the tag name split on `.`, without the part being typed.
pub fn members(src: &str, path: &[&str], load: &Load) -> Vec<Member> {
    let never = |_: &str| false;

    reach(
        src,
        path,
        &Reader {
            load,
            plain: &never,
        },
        Want::Values,
    )
}

/// The types the dotted path reaches: `Scribe.` in a type slot.
pub fn types(src: &str, path: &[&str], load: &Load, plain: &Plain) -> Vec<Member> {
    reach(src, path, &Reader { load, plain }, Want::Types)
}

fn reach(src: &str, path: &[&str], read: &Reader, want: Want) -> Vec<Member> {
    let mut out = walk(&readable(src), path, read, 0, want);

    out.sort_by_key(Member::sort_key);
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// The member a dotted tag names, for a hover. The last segment is the
/// member; what stands in front of it resolves to its holder.
pub fn member_at(src: &str, path: &[&str], load: &Load) -> Option<Member> {
    let (last, holder) = path.split_last()?;

    if holder.is_empty() {
        return None;
    }

    members(src, holder, load)
        .into_iter()
        .find(|m| m.name == *last)
}

/// Every name in the file that can hold a component: a namespace, a
/// table with a function in it, a struct with an `impl`, and each name
/// an import binds.
///
/// The case of the name says nothing here. A tag slot offers a bare
/// name only when it starts uppercase, which is right for a component
/// but wrong for `local tbl = { card = function() ... end }`: the table
/// holds one whatever it is called.
pub fn containers(src: &str) -> Vec<Member> {
    let src = readable(src);
    let Ok(parsed) = alloy_syntax::parse_lenient(&src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let t = |span: TokSpan| text_of(&src, toks, span);
    let mut out: Vec<Member> = Vec::new();

    for stmt in stmts {
        match stmt.under_default() {
            Stmt::Namespace(ns) => {
                let name = t(ns.name);

                out.push(Member::new(
                    &name,
                    MODULE,
                    "namespace",
                    Some(format!("namespace {name}")),
                ));
            }

            Stmt::Struct(s) => {
                let name = t(s.name);

                if !impl_members(&src, toks, stmts, &name).is_empty() {
                    out.push(Member::new(
                        &name,
                        STRUCT,
                        "struct",
                        Some(format!("struct {name}")),
                    ));
                }
            }

            Stmt::Local(l) => {
                for (i, b) in l.names.iter().enumerate() {
                    let name = t(b.name);

                    if b.destructure.is_some() || name.is_empty() {
                        continue;
                    }

                    let holds = l
                        .values
                        .get(i)
                        .is_some_and(|v| holds_a_component(unwrap_paren(v)))
                        || !dotted_functions(&src, toks, stmts, &[&name]).is_empty();

                    if holds {
                        out.push(Member::new(&name, MODULE, "table", None));
                    }
                }
            }

            Stmt::Import(im) => {
                for (name, detail) in import_bindings(&src, toks, &im.kind) {
                    out.push(Member::new(&name, MODULE, detail, None));
                }
            }

            _ => {}
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// The source with markup blanked, so the parser reads the code around
/// it. A file the author is still typing does not parse as markup, and
/// then the text stands as it is.
fn readable(src: &str) -> String {
    match alloy::luaux::compile::markup_spans(src) {
        Ok(spans) => alloy::luaux::resolve::blank_luaux_regions(src, &spans),

        Err(_) => src.to_string(),
    }
}

fn text_of(src: &str, toks: &[Tok], span: TokSpan) -> String {
    if span.end <= span.start {
        return String::new();
    }

    let (Some(first), Some(last)) = (
        toks.get(span.start as usize),
        toks.get(span.end as usize - 1),
    ) else {
        return String::new();
    };

    src[first.start as usize..last.end as usize].to_string()
}

/// The header of a declaration: what the author wrote up to the end of
/// the first line, so a body never reaches a hover.
fn header(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim_end();

    (!line.is_empty()).then(|| line.to_string())
}

fn unwrap_paren(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => unwrap_paren(inner),

        other => other,
    }
}

/// Whether a value can hold a component: a table with a function or a
/// table of its own inside it.
fn holds_a_component(expr: &Expr) -> bool {
    let Expr::Table { fields, .. } = expr else {
        return false;
    };

    fields.iter().any(|f| match f {
        TableField::Named { value, .. } => matches!(
            unwrap_paren(value),
            Expr::Function { .. } | Expr::Table { .. }
        ),

        _ => false,
    })
}

/// The names an import binds, each with the word a completion shows.
fn import_bindings(src: &str, toks: &[Tok], kind: &ImportKind) -> Vec<(String, &'static str)> {
    let t = |span: TokSpan| text_of(src, toks, span);
    let picked = |specs: &[ImportSpec]| -> Vec<(String, &'static str)> {
        specs
            .iter()
            .filter(|s| !s.is_type)
            .map(|s| (t(s.alias.unwrap_or(s.name)), "import"))
            .collect()
    };

    match kind {
        ImportKind::Namespace(alias, specs) => {
            let mut out = vec![(t(*alias), "module")];

            out.extend(picked(specs));
            out
        }

        ImportKind::Default(name) => vec![(t(*name), "import")],

        ImportKind::Both(name, specs) => {
            let mut out = vec![(t(*name), "import")];

            out.extend(picked(specs));
            out
        }

        ImportKind::Named(specs) => picked(specs),

        ImportKind::TypeOnly(_) => Vec::new(),
    }
}

/// The members of the name at the head of `path`, in one file.
fn walk(src: &str, path: &[&str], read: &Reader, depth: u8, want: Want) -> Vec<Member> {
    let Some(head) = path.first().copied() else {
        return Vec::new();
    };

    if depth >= MAX_DEPTH {
        return Vec::new();
    }

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let t = |span: TokSpan| text_of(src, toks, span);
    let rest = &path[1..];
    let values = want == Want::Values;

    for stmt in stmts {
        match stmt.under_default() {
            Stmt::Namespace(ns) if t(ns.name) == head => {
                return namespace_members(src, toks, ns, rest, want);
            }

            // A struct carries no type of its own, so a type slot
            // after its name reaches nothing.
            Stmt::Struct(s) if t(s.name) == head && rest.is_empty() => {
                return match values {
                    true => impl_members(src, toks, stmts, head),

                    false => Vec::new(),
                };
            }

            Stmt::Local(l) if values => {
                for (i, b) in l.names.iter().enumerate() {
                    if b.destructure.is_some() || t(b.name) != head {
                        continue;
                    }

                    let mut out = match l.values.get(i) {
                        Some(v) => table_members(src, toks, v, rest),

                        None => Vec::new(),
                    };

                    out.extend(dotted_functions(src, toks, stmts, path));

                    return out;
                }
            }

            Stmt::Import(im) => {
                let spec = t(im.path);
                let spec = spec.trim_matches(['"', '\'']).to_string();

                if let Some(found) =
                    import_members(src, toks, &im.kind, &spec, path, read, depth, want)
                {
                    return found;
                }
            }

            _ => {}
        }
    }

    // `function Widgets.button()` beside a plain `local Widgets = {}`
    // is how a Luau module writes a member.
    match values {
        true => dotted_functions(src, toks, stmts, path),

        false => Vec::new(),
    }
}

/// The members an import reaches, when it binds the head of `path`.
#[allow(clippy::too_many_arguments)]
fn import_members(
    src: &str,
    toks: &[Tok],
    kind: &ImportKind,
    spec: &str,
    path: &[&str],
    read: &Reader,
    depth: u8,
    want: Want,
) -> Option<Vec<Member>> {
    let t = |span: TokSpan| text_of(src, toks, span);
    let head = path.first().copied()?;
    let rest = &path[1..];
    let named = |specs: &[ImportSpec]| -> Option<String> {
        specs
            .iter()
            .find(|s| t(s.alias.unwrap_or(s.name)) == head)
            .map(|s| t(s.name))
    };
    // Luau writes a type off a module binding as `M.T`. A second `.`
    // is a syntax error there, so a type path stops at the first name
    // inside the module.
    let types = want.is_types();
    let deeper = !rest.is_empty();
    // `import * as M`: the module itself is the holder, so `M.` lists
    // what it exports and `M.Group.` walks on inside it.
    let whole = matches!(kind, ImportKind::Namespace(alias, _) if t(*alias) == head);
    // `import M from "p"`, and the default half of `import M, { a }`.
    let default_here = match kind {
        ImportKind::Default(name) | ImportKind::Both(name, _) => t(*name) == head,

        _ => false,
    };

    if types && (whole || default_here) {
        // A default import binds the module whole only when the
        // module returns one value; otherwise it binds the `default`
        // field, which carries no type.
        let bound = whole || (read.plain)(spec);

        return Some(match bound && !deeper {
            true => exported_members(&readable(&(read.load)(spec)?), Want::ImportedTypes),

            false => Vec::new(),
        });
    }

    if whole {
        let next = readable(&(read.load)(spec)?);

        return Some(match deeper {
            true => walk(&next, rest, read, depth + 1, want),

            false => exported_members(&next, want),
        });
    }

    let inner = match kind {
        ImportKind::Named(specs) => named(specs),

        // A name picked beside `* as M` reads off the module, the way
        // the list of `import M, { a }` does.
        ImportKind::Namespace(_, specs) => named(specs),

        ImportKind::Both(name, specs) => match t(*name) == head {
            true => default_name(&readable(&(read.load)(spec)?)),

            false => named(specs),
        },

        ImportKind::Default(name) if t(*name) == head => {
            default_name(&readable(&(read.load)(spec)?))
        }

        // `import type { Group } from "./m"` binds a name a type slot
        // reads, and no value at all.
        ImportKind::TypeOnly(specs) if types => named(specs),

        _ => None,
    }?;

    // The binding already spends the one step a type path has.
    if types && deeper {
        return Some(Vec::new());
    }

    let next = readable(&(read.load)(spec)?);
    let mut inner_path = vec![inner.as_str()];

    inner_path.extend(rest.iter().copied());
    Some(walk(
        &next,
        &inner_path,
        read,
        depth + 1,
        match types {
            true => Want::ImportedTypes,

            false => want,
        },
    ))
}

/// The name a module's `export default` declares, which an import binds
/// under a name of its own.
fn default_name(src: &str) -> Option<String> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let t = |span: TokSpan| text_of(src, toks, span);

    parsed.chunk.block.stmts.iter().find_map(|stmt| {
        let Stmt::ExportDefault { value, .. } = stmt else {
            return None;
        };

        // `export default Kit`, the way a module sends a namespace it
        // declared above. The name is the holder the import binds.
        if let DefaultExport::Value(Expr::Name(name)) = value {
            return Some(t(*name));
        }

        match stmt.under_default() {
            Stmt::Namespace(ns) => Some(t(ns.name)),

            Stmt::Struct(s) => Some(t(s.name)),

            Stmt::LocalFunction(f) => Some(t(f.name)),

            Stmt::Function(f) => f.path.first().map(|p| t(*p)),

            _ => None,
        }
    })
}

/// What a module exports: its functions, its namespaces, its tables,
/// and its structs for a value slot; its type declarations for a type
/// slot.
fn exported_members(src: &str, want: Want) -> Vec<Member> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let t = |span: TokSpan| text_of(src, toks, span);
    let types = want.is_types();
    let mut out = Vec::new();

    for stmt in stmts {
        let exported = match stmt {
            Stmt::ExportDefault { .. } => true,

            _ => match stmt {
                Stmt::Namespace(ns) => ns.exported || ns.global,

                Stmt::Struct(s) => s.exported || s.global,

                Stmt::Local(l) => l.exported || l.global,

                Stmt::LocalFunction(f) => f.exported || f.global,

                Stmt::Function(f) => f.exported || f.global,

                Stmt::TypeAlias(a) => a.exported || a.global,

                Stmt::Enum(e) => e.exported || e.global,

                Stmt::Trait(x) => x.exported || x.global,

                Stmt::Interface(i) => i.exported || i.global,

                Stmt::Class(c) => c.exported || c.global,

                _ => false,
            },
        };

        if !exported {
            continue;
        }

        if types {
            out.extend(type_member(src, toks, stmt.under_default(), want));

            continue;
        }

        match stmt.under_default() {
            Stmt::Namespace(ns) => {
                let name = t(ns.name);

                out.push(Member::new(
                    &name,
                    MODULE,
                    "namespace",
                    Some(format!("namespace {name}")),
                ));
            }

            Stmt::Struct(s) => {
                let name = t(s.name);

                out.push(Member::new(
                    &name,
                    STRUCT,
                    "struct",
                    Some(format!("struct {name}")),
                ));
            }

            Stmt::LocalFunction(f) => {
                out.push(Member::new(
                    &t(f.name),
                    FUNCTION,
                    "function",
                    header(&t(f.span)),
                ));
            }

            Stmt::Function(f) if f.path.len() == 1 => {
                out.push(Member::new(
                    &t(f.path[0]),
                    FUNCTION,
                    "function",
                    header(&t(f.span)),
                ));
            }

            Stmt::Local(l) => {
                for (i, b) in l.names.iter().enumerate() {
                    if b.destructure.is_some() {
                        continue;
                    }

                    let value = l.values.get(i).map(unwrap_paren);
                    let (kind, detail) = match value {
                        Some(Expr::Function { .. }) => (FUNCTION, "function"),

                        Some(Expr::Table { .. }) => (MODULE, "table"),

                        _ => (FIELD, "value"),
                    };

                    out.push(Member::new(&t(b.name), kind, detail, None));
                }
            }

            _ => {}
        }
    }

    out.sort_by_key(Member::sort_key);
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// The type one statement declares, for a type slot. A namespace is
/// no type, but it stands on the way to one, so it joins the list when
/// the path can walk on through it and its body holds a type.
/// Everything else, a function or a const, is a value and stays out.
fn type_member(src: &str, toks: &[Tok], stmt: &Stmt, want: Want) -> Option<Member> {
    let t = |span: TokSpan| text_of(src, toks, span);
    let (name, kind, detail) = match stmt {
        Stmt::Struct(s) => (t(s.name), STRUCT, "struct"),

        Stmt::Enum(e) => (t(e.name), ENUM, "enum"),

        Stmt::Trait(x) => (t(x.name), INTERFACE, "trait"),

        Stmt::Interface(i) => (t(i.name), INTERFACE, "interface"),

        Stmt::TypeAlias(a) => (t(a.name), STRUCT, "type"),

        Stmt::Class(c) => (t(c.name), STRUCT, "class"),

        Stmt::Namespace(ns) if want.takes_a_namespace() && holds_a_type(src, toks, ns) => {
            (t(ns.name), MODULE, "namespace")
        }

        _ => return None,
    };

    (!name.is_empty()).then(|| Member::new(&name, kind, detail, Some(format!("{detail} {name}"))))
}

/// Whether a namespace declares a type, at any depth. A namespace of
/// functions alone is a dead end in a type slot, so the list leaves it
/// out.
fn holds_a_type(src: &str, toks: &[Tok], ns: &NamespaceDecl) -> bool {
    ns.members.iter().any(|m| {
        !m.is_private(src, toks)
            && match m.stmt.under_default() {
                Stmt::Namespace(inner) => holds_a_type(src, toks, inner),

                other => matches!(
                    other,
                    Stmt::Struct(_)
                        | Stmt::Enum(_)
                        | Stmt::Trait(_)
                        | Stmt::Interface(_)
                        | Stmt::TypeAlias(_)
                        | Stmt::Class(_)
                ),
            }
    })
}

/// The members of one namespace, or of what a member of it holds.
fn namespace_members(
    src: &str,
    toks: &[Tok],
    ns: &NamespaceDecl,
    rest: &[&str],
    want: Want,
) -> Vec<Member> {
    let t = |span: TokSpan| text_of(src, toks, span);

    // `<Outer.Inner.`: the walk goes on inside the member named next.
    if let Some(next) = rest.first().copied() {
        for m in &ns.members {
            match m.stmt.under_default() {
                Stmt::Namespace(inner) if t(inner.name) == next => {
                    return namespace_members(src, toks, inner, &rest[1..], want);
                }

                Stmt::Local(l) if want == Want::Values => {
                    for (i, b) in l.names.iter().enumerate() {
                        if t(b.name) == next
                            && let Some(v) = l.values.get(i)
                        {
                            return table_members(src, toks, v, &rest[1..]);
                        }
                    }
                }

                _ => {}
            }
        }

        return Vec::new();
    }

    let mut out = Vec::new();

    for m in &ns.members {
        // A private member is out of reach for every other file, and a
        // tag that names one does not compile.
        if m.is_private(src, toks) {
            continue;
        }

        if want.is_types() {
            out.extend(type_member(src, toks, m.stmt.under_default(), want));

            continue;
        }

        let body = t(m.stmt.span());

        match m.stmt.under_default() {
            Stmt::LocalFunction(f) => {
                out.push(Member::new(&t(f.name), FUNCTION, "function", header(&body)));
            }

            Stmt::Function(f) if f.path.len() == 1 => {
                out.push(Member::new(
                    &t(f.path[0]),
                    FUNCTION,
                    "function",
                    header(&body),
                ));
            }

            Stmt::Namespace(inner) => {
                let name = t(inner.name);

                out.push(Member::new(
                    &name,
                    MODULE,
                    "namespace",
                    Some(format!("namespace {name}")),
                ));
            }

            Stmt::Struct(s) => {
                let name = t(s.name);

                out.push(Member::new(
                    &name,
                    STRUCT,
                    "struct",
                    Some(format!("struct {name}")),
                ));
            }

            Stmt::Local(l) => {
                for (i, b) in l.names.iter().enumerate() {
                    if b.destructure.is_some() {
                        continue;
                    }

                    let (kind, detail) = match l.values.get(i).map(unwrap_paren) {
                        Some(Expr::Function { .. }) => (FUNCTION, "function"),

                        Some(Expr::Table { .. }) => (MODULE, "table"),

                        _ => (FIELD, "value"),
                    };

                    out.push(Member::new(&t(b.name), kind, detail, None));
                }
            }

            _ => {}
        }
    }

    out
}

/// The named fields of a table, or of the table one of them holds.
fn table_members(src: &str, toks: &[Tok], expr: &Expr, rest: &[&str]) -> Vec<Member> {
    let Expr::Table { fields, .. } = unwrap_paren(expr) else {
        return Vec::new();
    };
    let t = |span: TokSpan| text_of(src, toks, span);

    if let Some(next) = rest.first().copied() {
        for f in fields {
            if let TableField::Named { name, value } = f
                && t(*name) == next
            {
                return table_members(src, toks, value, &rest[1..]);
            }
        }

        return Vec::new();
    }

    let mut out = Vec::new();

    for f in fields {
        let TableField::Named { name, value } = f else {
            continue;
        };
        let (kind, detail) = match unwrap_paren(value) {
            Expr::Function { .. } => (FUNCTION, "function"),

            Expr::Table { .. } => (MODULE, "table"),

            _ => (FIELD, "field"),
        };
        let field = t(*name);
        let signature = match kind == FUNCTION {
            true => header(&format!("{field} = {}", t(value.span()))),

            false => None,
        };

        out.push(Member::new(&field, kind, detail, signature));
    }

    out
}

/// The functions an `impl` block gives a struct. A method takes `self`
/// and belongs to one value, so a tag cannot stand on it.
fn impl_members(src: &str, toks: &[Tok], stmts: &[Stmt], name: &str) -> Vec<Member> {
    let t = |span: TokSpan| text_of(src, toks, span);
    let mut out = Vec::new();

    for stmt in stmts {
        let Stmt::Impl(i) = stmt.under_default() else {
            continue;
        };

        if t(i.target) != name {
            continue;
        }

        for m in &i.methods {
            let takes_self =
                m.is_method || m.body.params.first().is_some_and(|p| t(p.name) == "self");
            let private = m.visibility.is_some_and(|v| t(v) == "private");

            if takes_self || private {
                continue;
            }

            if let Some(first) = m.path.first() {
                out.push(Member::new(
                    &t(*first),
                    FUNCTION,
                    "function",
                    header(&t(m.span)),
                ));
            }
        }
    }

    out
}

/// The next segment of every `function A.B.c()` whose path opens with
/// `prefix`, which is how a Luau module writes a member of its table.
fn dotted_functions(src: &str, toks: &[Tok], stmts: &[Stmt], prefix: &[&str]) -> Vec<Member> {
    let t = |span: TokSpan| text_of(src, toks, span);
    let mut out = Vec::new();

    for stmt in stmts {
        let Stmt::Function(f) = stmt.under_default() else {
            continue;
        };

        if f.path.len() != prefix.len() + 1 {
            continue;
        }

        let path: Vec<String> = f.path.iter().map(|p| t(*p)).collect();

        if path.iter().zip(prefix).all(|(a, b)| a == b)
            && let Some(last) = path.last()
        {
            out.push(Member::new(last, FUNCTION, "function", header(&t(f.span))));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No module reaches the disk in a unit test.
    fn nothing(_: &str) -> Option<String> {
        None
    }

    fn names(members: &[Member]) -> Vec<&str> {
        members.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn a_namespace_lists_its_public_members() {
        let src = "namespace Scope as\n\
            function component()\n\
                return 1\n\
            end\n\
            private function hidden()\n\
                return 2\n\
            end\n\
        end\n";
        let found = members(src, &["Scope"], &nothing);

        assert_eq!(names(&found), ["component"]);
        assert_eq!(found[0].kind, FUNCTION);
        assert_eq!(found[0].signature.as_deref(), Some("function component()"));
    }

    #[test]
    fn a_nested_namespace_walks_on() {
        let src = "namespace Outer as\n\
            namespace Inner as\n\
                function button()\n\
                    return 1\n\
                end\n\
            end\n\
        end\n";

        assert_eq!(names(&members(src, &["Outer"], &nothing)), ["Inner"]);
        assert_eq!(
            names(&members(src, &["Outer", "Inner"], &nothing)),
            ["button"]
        );
        assert!(members(src, &["Outer", "Missing"], &nothing).is_empty());
    }

    #[test]
    fn a_table_lists_the_fields_it_writes() {
        let src = "local tbl = {\n\
            card = function() return 1 end,\n\
            size = 3,\n\
            deep = { inner = function() return 2 end },\n\
        }\n";
        let found = members(src, &["tbl"], &nothing);

        assert_eq!(names(&found), ["card", "deep", "size"]);
        assert_eq!(names(&members(src, &["tbl", "deep"], &nothing)), ["inner"]);
    }

    #[test]
    fn a_module_table_takes_its_dotted_functions() {
        let src = "local Widgets = {}\n\
            function Widgets.button()\n\
                return 1\n\
            end\n\
            return Widgets\n";

        assert_eq!(names(&members(src, &["Widgets"], &nothing)), ["button"]);
    }

    #[test]
    fn a_struct_offers_the_functions_of_its_impl() {
        let src = "struct Card as\n\
            title: string,\n\
        end\n\
        impl Card as\n\
            function new(title: string): Card\n\
                return new Card { title = title }\n\
            end\n\
            function render(self): string\n\
                return self.title\n\
            end\n\
        end\n";

        assert_eq!(names(&members(src, &["Card"], &nothing)), ["new"]);
    }

    #[test]
    fn an_import_reaches_the_other_file() {
        let lib = "export namespace Widgets as\n\
            function button()\n\
                return 1\n\
            end\n\
        end\n\
        export const Kit = { card = function() return 2 end }\n";
        let load = |spec: &str| match spec {
            "./lib" => Some(lib.to_string()),

            _ => None,
        };
        let named = "import { Widgets, Kit } from \"./lib\"\n";
        let whole = "import * as Lib from \"./lib\"\n";

        assert_eq!(names(&members(named, &["Widgets"], &load)), ["button"]);
        assert_eq!(names(&members(named, &["Kit"], &load)), ["card"]);
        assert_eq!(names(&members(whole, &["Lib"], &load)), ["Kit", "Widgets"]);
        assert_eq!(
            names(&members(whole, &["Lib", "Widgets"], &load)),
            ["button"]
        );
    }

    #[test]
    fn an_alias_keeps_the_name_of_the_other_file() {
        let lib = "export namespace Widgets as\n\
            function button()\n\
                return 1\n\
            end\n\
        end\n";
        let load = |spec: &str| match spec {
            "./lib" => Some(lib.to_string()),

            _ => None,
        };
        let src = "import { Widgets as W } from \"./lib\"\n";

        assert_eq!(names(&members(src, &["W"], &load)), ["button"]);
    }

    #[test]
    fn a_name_that_holds_nothing_offers_nothing() {
        let src = "local n = 3\nlocal s = \"text\"\n";

        assert!(members(src, &["n"], &nothing).is_empty());
        assert!(members(src, &["missing"], &nothing).is_empty());
        assert!(members(src, &["s", "deep"], &nothing).is_empty());
    }

    #[test]
    fn the_holders_of_a_file_read_whatever_their_case() {
        let src = "import * as Lib from \"./lib\"\n\
            import { Widgets } from \"./lib\"\n\
            namespace Scope as\n\
                function one() return 1 end\n\
            end\n\
            local tbl = { card = function() return 1 end }\n\
            local count = 3\n";
        let found = containers(src);

        assert_eq!(names(&found), ["Lib", "Scope", "Widgets", "tbl"]);
    }

    #[test]
    fn a_markup_body_does_not_stop_the_walk() {
        let src = "namespace Scope as\n\
            function component()\n\
                return <Frame></Frame>\n\
            end\n\
        end\n";

        assert_eq!(names(&members(src, &["Scope"], &nothing)), ["component"]);
    }

    /// The module every type test imports. It ends in no `return`,
    /// so it carries an export table.
    fn scribe() -> &'static str {
        "export type Store = { id: number }\n\
         export type Entry = { key: string }\n\
         export const version = 1\n\
         export struct Card as\n\
             title: string,\n\
         end\n\
         export enum Mode as\n\
             Fast,\n\
         end\n\
         export namespace Deep as\n\
             public type Inner = { n: number }\n\
             private type Hidden = { h: number }\n\
             public function helper() return 1 end\n\
         end\n\
         export namespace Only as\n\
             public function act() return 1 end\n\
         end\n\
         export function make(): Store\n\
             return { id = 1 }\n\
         end\n"
    }

    /// A plain Luau package: module-level `export type` beside a table
    /// it returns. `require` of it binds the table whole.
    fn package() -> &'static str {
        "--!strict\n\
         export type Store = { id: number }\n\
         export type Entry = { key: string }\n\
         local Scribe = {}\n\
         function Scribe.open(name: string): Store\n\
             return { id = 1 }\n\
         end\n\
         return Scribe\n"
    }

    fn scribe_load(spec: &str) -> Option<String> {
        match spec {
            "./scribe" => Some(scribe().to_string()),

            "@pkg/scribe" => Some(package().to_string()),

            _ => None,
        }
    }

    /// Only `@pkg/scribe` returns one value.
    fn scribe_plain(spec: &str) -> bool {
        spec == "@pkg/scribe"
    }

    /// No module returns a value, so no default import binds one.
    fn none_plain(_: &str) -> bool {
        false
    }

    #[test]
    fn a_star_import_offers_the_types_of_the_module() {
        let src = "import * as Star from \"./scribe\"\n";
        let found = types(src, &["Star"], &scribe_load, &scribe_plain);

        assert_eq!(names(&found), ["Card", "Entry", "Mode", "Store"]);
    }

    /// Luau writes a type off a module binding as `M.T`; `M.Group.T`
    /// is a syntax error. So a namespace of the module leads nowhere
    /// and stays out of the list, and a second `.` offers nothing.
    #[test]
    fn a_type_path_through_an_import_stops_at_one_name() {
        let src = "import * as Star from \"./scribe\"\n";
        let found = types(src, &["Star"], &scribe_load, &scribe_plain);

        assert!(!names(&found).contains(&"Deep"));
        assert!(types(src, &["Star", "Deep"], &scribe_load, &scribe_plain).is_empty());
        assert!(types(src, &["Star", "Only"], &scribe_load, &scribe_plain).is_empty());
    }

    /// A value is no type. `Scribe.version` and `Scribe.make` are what
    /// a type slot must never offer.
    #[test]
    fn a_type_slot_takes_no_value() {
        let src = "import * as Star from \"./scribe\"\n";
        let reached = types(src, &["Star"], &scribe_load, &scribe_plain);
        let found = names(&reached);

        assert!(!found.contains(&"version"));
        assert!(!found.contains(&"make"));
        assert!(!found.contains(&"helper"));

        // The value walk still answers with them, and with no type.
        let held = members(src, &["Star"], &scribe_load);
        let values = names(&held);

        assert!(values.contains(&"version"));
        assert!(values.contains(&"make"));
    }

    /// `import Scribe from "@pkg/scribe"` on a plain Luau package.
    /// The package returns one table, so the binding is the whole
    /// `require` and every `export type` of it reads as `Scribe.T`.
    #[test]
    fn a_default_import_of_a_plain_module_reaches_its_types() {
        let src = "import Scribe from \"@pkg/scribe\"\n";
        let found = types(src, &["Scribe"], &scribe_load, &scribe_plain);

        assert_eq!(names(&found), ["Entry", "Store"]);
        // `open` is a value of the table, not a type.
        assert!(!names(&found).contains(&"open"));
        // And the path stops at the one name.
        assert!(types(src, &["Scribe", "Store"], &scribe_load, &scribe_plain).is_empty());
    }

    /// A module with an export table binds `require(...).default`, and
    /// no type hangs off that. `Aly.Store` does not typecheck, so the
    /// slot offers nothing rather than a name that fails.
    #[test]
    fn a_default_import_of_an_export_table_reaches_no_type() {
        let src = "import Aly from \"./scribe\"\n";

        assert!(types(src, &["Aly"], &scribe_load, &none_plain).is_empty());
        assert!(types(src, &["Aly"], &scribe_load, &scribe_plain).is_empty());
    }

    #[test]
    fn a_named_import_carries_a_namespace_of_types() {
        let plain = "import { Deep } from \"./scribe\"\n";
        let renamed = "import { Deep as D } from \"./scribe\"\n";
        let type_only = "import type { Deep } from \"./scribe\"\n";

        assert_eq!(
            names(&types(plain, &["Deep"], &scribe_load, &scribe_plain)),
            ["Inner"]
        );
        assert_eq!(
            names(&types(renamed, &["D"], &scribe_load, &scribe_plain)),
            ["Inner"]
        );
        assert_eq!(
            names(&types(type_only, &["Deep"], &scribe_load, &scribe_plain)),
            ["Inner"]
        );
        // One name deep and no further.
        assert!(types(renamed, &["D", "Inner"], &scribe_load, &scribe_plain).is_empty());
    }

    /// A namespace of the file nests as far as the author wrote it:
    /// the emit flattens `Outer.Inner.Leaf` to one name, so the whole
    /// path is a type Luau reads.
    #[test]
    fn a_namespace_of_the_file_offers_its_own_types() {
        let src = "namespace NS as\n\
            public type Thing = { a: number }\n\
            public struct Item as\n\
                id: number,\n\
            end\n\
            private type Secret = { s: string }\n\
            public function act() return 1 end\n\
            public namespace Sub as\n\
                public type Leaf = { l: number }\n\
            end\n\
        end\n";
        let found = types(src, &["NS"], &nothing, &none_plain);

        assert_eq!(names(&found), ["Item", "Sub", "Thing"]);
        assert_eq!(
            names(&types(src, &["NS", "Sub"], &nothing, &none_plain)),
            ["Leaf"]
        );
    }

    #[test]
    fn a_type_path_that_reaches_nothing_offers_nothing() {
        let src = "import * as Star from \"./scribe\"\n\
            local count = 3\n\
            local tbl = { card = function() return 1 end }\n";

        assert!(types(src, &["Missing"], &scribe_load, &scribe_plain).is_empty());
        assert!(types(src, &["count"], &scribe_load, &scribe_plain).is_empty());
        assert!(types(src, &["tbl"], &scribe_load, &scribe_plain).is_empty());
        assert!(types(src, &["Star", "Store"], &scribe_load, &scribe_plain).is_empty());
        assert!(types(src, &["Star", "Missing"], &scribe_load, &scribe_plain).is_empty());
    }

    /// A name the emit writes must never reach the list.
    #[test]
    fn no_type_carries_an_emit_only_name() {
        let src = "import * as Star from \"./scribe\"\n";
        let pkg = "import Scribe from \"@pkg/scribe\"\n";
        let found = types(src, &["Star"], &scribe_load, &scribe_plain);

        for m in found
            .iter()
            .chain(&types(pkg, &["Scribe"], &scribe_load, &scribe_plain))
        {
            assert!(!m.name.contains("__"), "{}", m.name);
            assert!(!m.name.contains('_'), "{}", m.name);
            assert!(!m.detail.contains("__"), "{}", m.detail);
        }
    }

    #[test]
    fn the_member_of_a_path_reads_its_own_line() {
        let src = "namespace Scope as\n\
            function component()\n\
                return 1\n\
            end\n\
        end\n";
        let found = member_at(src, &["Scope", "component"], &nothing).expect("the member");

        assert_eq!(found.signature.as_deref(), Some("function component()"));
        assert!(member_at(src, &["Scope", "missing"], &nothing).is_none());
        assert!(member_at(src, &["Scope"], &nothing).is_none());
    }
}
