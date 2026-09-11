//! What a dotted tag name reaches: `<Lib.Widgets.button/>`.
//!
//! A component is a function that returns an instance, so anything that
//! holds functions can hold one: a namespace, a table, a struct with an
//! `impl`, and a module another file exports. A tag slot after a `.`
//! offers the members this module resolves, and the hover on a dotted
//! tag names the member it finds.

use alloy_syntax::ast::{Expr, ImportKind, ImportSpec, NamespaceDecl, Stmt, TableField, TokSpan};
use alloy_syntax::lexer::Tok;

/// The LSP completion kinds this module hands out.
const FUNCTION: u64 = 3;
const MODULE: u64 = 9;
const STRUCT: u64 = 7;
const FIELD: u64 = 5;

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

/// The members the dotted path reaches, with the functions first.
/// `path` is the tag name split on `.`, without the part being typed.
pub fn members(src: &str, path: &[&str], load: &Load) -> Vec<Member> {
    let mut out = walk(&readable(src), path, load, 0);

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
        ImportKind::Namespace(alias) => vec![(t(*alias), "module")],

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
fn walk(src: &str, path: &[&str], load: &Load, depth: u8) -> Vec<Member> {
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

    for stmt in stmts {
        match stmt.under_default() {
            Stmt::Namespace(ns) if t(ns.name) == head => {
                return namespace_members(src, toks, ns, rest);
            }

            Stmt::Struct(s) if t(s.name) == head && rest.is_empty() => {
                return impl_members(src, toks, stmts, head);
            }

            Stmt::Local(l) => {
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

                if let Some(found) = import_members(src, toks, &im.kind, &spec, path, load, depth) {
                    return found;
                }
            }

            _ => {}
        }
    }

    // `function Widgets.button()` beside a plain `local Widgets = {}`
    // is how a Luau module writes a member.
    dotted_functions(src, toks, stmts, path)
}

/// The members an import reaches, when it binds the head of `path`.
fn import_members(
    src: &str,
    toks: &[Tok],
    kind: &ImportKind,
    spec: &str,
    path: &[&str],
    load: &Load,
    depth: u8,
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
    // `import * as M`: the module itself is the holder, so `M.` lists
    // what it exports and `M.Group.` walks on inside it.
    let whole = matches!(kind, ImportKind::Namespace(alias) if t(*alias) == head);

    if whole {
        let next = readable(&load(spec)?);

        return Some(match rest.is_empty() {
            true => exported_members(&next),

            false => walk(&next, rest, load, depth + 1),
        });
    }

    let inner = match kind {
        ImportKind::Named(specs) => named(specs),

        ImportKind::Both(name, specs) => match t(*name) == head {
            true => default_name(&readable(&load(spec)?)),

            false => named(specs),
        },

        ImportKind::Default(name) if t(*name) == head => default_name(&readable(&load(spec)?)),

        _ => None,
    }?;
    let next = readable(&load(spec)?);
    let mut inner_path = vec![inner.as_str()];

    inner_path.extend(rest.iter().copied());

    Some(walk(&next, &inner_path, load, depth + 1))
}

/// The name a module's `export default` declares, which an import binds
/// under a name of its own.
fn default_name(src: &str) -> Option<String> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let t = |span: TokSpan| text_of(src, toks, span);

    parsed.chunk.block.stmts.iter().find_map(|stmt| {
        let Stmt::ExportDefault { .. } = stmt else {
            return None;
        };

        match stmt.under_default() {
            Stmt::Namespace(ns) => Some(t(ns.name)),

            Stmt::Struct(s) => Some(t(s.name)),

            Stmt::LocalFunction(f) => Some(t(f.name)),

            Stmt::Function(f) => f.path.first().map(|p| t(*p)),

            _ => None,
        }
    })
}

/// What a module exports and a tag can name: its functions, its
/// namespaces, its tables, and its structs.
fn exported_members(src: &str) -> Vec<Member> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let t = |span: TokSpan| text_of(src, toks, span);
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

                _ => false,
            },
        };

        if !exported {
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

/// The members of one namespace, or of what a member of it holds.
fn namespace_members(src: &str, toks: &[Tok], ns: &NamespaceDecl, rest: &[&str]) -> Vec<Member> {
    let t = |span: TokSpan| text_of(src, toks, span);

    // `<Outer.Inner.`: the walk goes on inside the member named next.
    if let Some(next) = rest.first().copied() {
        for m in &ns.members {
            match m.stmt.under_default() {
                Stmt::Namespace(inner) if t(inner.name) == next => {
                    return namespace_members(src, toks, inner, &rest[1..]);
                }

                Stmt::Local(l) => {
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
