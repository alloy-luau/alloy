//! The `global` declarations of a project.
//!
//! A `global` is a name every file reaches without an import. The build
//! knows every file, so it resolves the name at compile time: the file
//! that uses one gets the `require` of the declaring module and the
//! binding on its first line, the way the runtime require is injected.
//! Nothing goes through `_G` or `shared`.
//!
//! This file owns the index. It reads what a source declares, what a
//! definitions file declares beside it, and the require string one file
//! uses to reach another.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Stmt, TokSpan};

use crate::luaux;

/// What a `global` declaration binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Function,
    /// `global const X = 1` and `global local x = 1`.
    Value,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Type,
    Remote,
    /// `global macro twice(x) ... end`: `$twice` expands in every file.
    Macro,
    /// `global attribute tag(...) on struct`: `@tag` reads in every file.
    Attribute,
    /// `global impl BasePart as ... end`: methods, no name to bind.
    Impl,
    /// `global namespace Math as ... end`: one table over a group.
    Namespace,
}

impl Kind {
    /// Whether the name binds a value the emit reads off the module.
    pub fn is_value(self) -> bool {
        matches!(
            self,
            Kind::Function
                | Kind::Value
                | Kind::Struct
                | Kind::Enum
                | Kind::Trait
                | Kind::Class
                | Kind::Remote
                | Kind::Namespace
        )
    }

    /// Whether the name is a type as well, so the file needs an alias.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Kind::Struct | Kind::Enum | Kind::Class | Kind::Interface | Kind::Type
        )
    }

    /// The keyword the source wrote, for a message and a hover.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Value => "const",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Trait => "trait",
            Kind::Interface => "interface",
            Kind::Class => "class",
            Kind::Type => "type",
            Kind::Remote => "remote",
            Kind::Macro => "macro",
            Kind::Attribute => "attribute",
            Kind::Impl => "impl",
            Kind::Namespace => "namespace",
        }
    }
}

/// One `global` declaration of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    /// The declaring file, relative to `[build] in`, as a message names it.
    pub file: PathBuf,
    /// The byte offset of the declared name, for go to definition.
    pub offset: u32,
    /// The byte range of the whole declaration head, for a diagnostic.
    pub start: u32,
    pub end: u32,
    pub kind: Kind,
    /// The parameter list of a generic type, `<T>`; empty otherwise.
    pub type_params: String,
    /// The side this global reaches: the directive above it when it
    /// has one, else the side of the declaring file.
    pub side: Option<crate::directives::Side>,
    /// The `--@alloy-side` right above the declaration, when it has
    /// one. It beats every rule the file's own side follows.
    pub side_directive: Option<Option<crate::directives::Side>>,
}

/// The names a Luau or Roblox program already has. A global by one of
/// these names would shadow the language itself, so it is an error.
pub const LUAU_GLOBALS: &[&str] = &[
    "_G",
    "_VERSION",
    "assert",
    "bit32",
    "buffer",
    "collectgarbage",
    "coroutine",
    "debug",
    "delay",
    "error",
    "game",
    "getfenv",
    "getmetatable",
    "gcinfo",
    "ipairs",
    "loadstring",
    "math",
    "newproxy",
    "next",
    "os",
    "pairs",
    "pcall",
    "plugin",
    "print",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "require",
    "script",
    "select",
    "setfenv",
    "setmetatable",
    "shared",
    "spawn",
    "string",
    "table",
    "task",
    "tick",
    "time",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "unpack",
    "utf8",
    "wait",
    "warn",
    "workspace",
    "xpcall",
    "Enum",
    "Instance",
];

/// Every `global` a source declares. `file` is the path a message
/// names, relative to `[build] in`.
pub fn declared(src: &str, file: &Path) -> Vec<Global> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };

    declared_in(src, &parsed.lexed.toks, &parsed.chunk, file)
}

/// The same index over a tree the caller already parsed. The compiler
/// takes this one: it reads the globals of the file it is rendering.
pub fn declared_in(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    chunk: &alloy_syntax::ast::Chunk,
    file: &Path,
) -> Vec<Global> {
    let text = |span: TokSpan| -> &str {
        if span.end <= span.start || span.end as usize > toks.len() {
            return "";
        }

        let start = toks[span.start as usize].start as usize;
        let end = toks[span.end as usize - 1].end as usize;

        &src[start..end]
    };
    let at = |span: TokSpan| -> u32 {
        match toks.get(span.start as usize) {
            Some(t) => t.start,

            None => 0,
        }
    };
    let side = crate::directives::effective_side(src, &file.to_string_lossy());
    let scanned = crate::directives::scan(src);
    let mut out = Vec::new();
    let mut push = |name: &str, kind: Kind, name_span: TokSpan, span: TokSpan, params: String| {
        if name.is_empty() {
            return;
        }

        let line = src[..at(span) as usize].matches('\n').count();
        let directive = scanned.side_above(src, line);
        out.push(Global {
            name: name.to_string(),
            file: file.to_path_buf(),
            offset: at(name_span),
            start: at(span),
            end: toks
                .get((span.end as usize).saturating_sub(1))
                .map(|t| t.end)
                .unwrap_or(0),
            kind,
            type_params: params,
            side: directive.unwrap_or(side),
            side_directive: directive,
        });
    };

    for stmt in &chunk.block.stmts {
        match stmt {
            Stmt::Function(f) if f.global => {
                if let Some(first) = f.path.first() {
                    push(text(*first), Kind::Function, *first, f.span, String::new());
                }
            }

            Stmt::LocalFunction(f) if f.global => {
                push(text(f.name), Kind::Function, f.name, f.span, String::new())
            }

            Stmt::Local(l) if l.global => {
                for b in &l.names {
                    push(text(b.name), Kind::Value, b.name, l.span, String::new());
                }
            }

            Stmt::Struct(d) if d.global => push(
                text(d.name),
                Kind::Struct,
                d.name,
                d.span,
                params_of(d.generics.map(text)),
            ),

            Stmt::Enum(d) if d.global => {
                push(text(d.name), Kind::Enum, d.name, d.span, String::new())
            }

            Stmt::Trait(d) if d.global => {
                push(text(d.name), Kind::Trait, d.name, d.span, String::new())
            }

            Stmt::Interface(d) if d.global => push(
                text(d.name),
                Kind::Interface,
                d.name,
                d.span,
                params_of(d.generics.map(text)),
            ),

            Stmt::Class(d) if d.global => {
                push(text(d.name), Kind::Class, d.name, d.span, String::new())
            }

            Stmt::Remote(d) if d.global => {
                push(text(d.name), Kind::Remote, d.name, d.span, String::new())
            }

            Stmt::TypeAlias(d) if d.global => {
                // The alias keeps its parameters: a file that names it
                // has to pass them on, or Luau asks for the one that is
                // gone. They sit between the name and the `=`.
                let after = toks
                    .get(d.name.end as usize - 1)
                    .map(|t| t.end as usize)
                    .unwrap_or(src.len());
                let params = crate::modules::type_params(src[after..].trim_start());
                push(text(d.name), Kind::Type, d.name, d.span, params);
            }

            // A macro expands at compile time and an attribute is read
            // at compile time, so neither needs a require. The compiler
            // carries the declaration itself to every file.
            Stmt::Macro(d) if d.global => {
                push(text(d.name), Kind::Macro, d.name, d.span, String::new())
            }

            Stmt::Attribute(d) if d.global => {
                push(text(d.name), Kind::Attribute, d.name, d.span, String::new())
            }

            Stmt::Impl(d) if d.global => {
                push(text(d.target), Kind::Impl, d.target, d.span, String::new())
            }

            // A namespace reaches a file as one name. Its public types
            // reach it as `Math_Vec2`, the name the emit gives them, so
            // each one is a global of its own.
            Stmt::Namespace(d) if d.global => {
                let name = text(d.name);
                push(name, Kind::Namespace, d.name, d.span, String::new());

                for (member, params) in namespace_types(src, toks, d, name) {
                    push(&member, Kind::Type, d.name, d.span, params);
                }
            }

            _ => {}
        }
    }

    out
}

/// The public types of a `global namespace`, under the names the emit
/// gives them, each with its parameter list.
fn namespace_types(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    prefix: &str,
) -> Vec<(String, String)> {
    let text = |span: TokSpan| -> &str {
        if span.end <= span.start || span.end as usize > toks.len() {
            return "";
        }

        &src[toks[span.start as usize].start as usize..toks[span.end as usize - 1].end as usize]
    };
    let mut out = Vec::new();

    for m in &ns.members {
        if m.is_private(src, toks) {
            continue;
        }

        let (name, params) = match m.stmt.under_default() {
            Stmt::Struct(d) => (text(d.name), params_of(d.generics.map(text))),

            Stmt::Enum(d) => (text(d.name), String::new()),

            Stmt::Trait(d) => (text(d.name), String::new()),

            Stmt::Interface(d) => (text(d.name), params_of(d.generics.map(text))),

            Stmt::TypeAlias(d) => {
                let after = toks[d.name.end as usize - 1].end as usize;

                (
                    text(d.name),
                    crate::modules::type_params(src[after..].trim_start()),
                )
            }

            Stmt::Namespace(inner) => {
                let deeper = format!("{prefix}_{}", text(inner.name));
                out.extend(namespace_types(src, toks, inner, &deeper));

                continue;
            }

            _ => continue,
        };
        out.push((format!("{prefix}_{name}"), params));
    }

    out
}

/// The parameter list of a generic head, as Luau takes it.
fn params_of(generics: Option<&str>) -> String {
    generics
        .map(crate::modules::type_params)
        .unwrap_or_default()
}

/// Every `global` the sources under one root declare, by file.
///
/// `files` pairs each source's path relative to `[build] in` with its
/// text. The order of the result follows the order of `files`, so two
/// builds of one project report the same duplicate first.
pub fn index(files: &[(PathBuf, String)]) -> Vec<Global> {
    let mut out = Vec::new();

    for (rel, src) in files {
        if rel.to_string_lossy().ends_with(".d.aly") {
            continue;
        }

        out.extend(declared(src, rel));
    }

    out
}

/// The ambient names a definitions file declares, by file. A `.d.aly`
/// declares a name for the checker with no module behind it, so a
/// `global` by the same name has two declarations and no way to pick.
pub fn ambient_names(files: &[(PathBuf, String)]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();

    for (rel, src) in files {
        if !rel.to_string_lossy().ends_with(".d.aly") {
            continue;
        }

        for d in crate::declarations::summaries(src, true) {
            out.push((d.name, rel.clone()));
        }
    }

    out
}

/// The `global macro` declarations of the project, as the compiler
/// takes them. A macro expands where it is written, so the declaration
/// travels to every file instead of a require.
pub fn macro_sources(files: &[(PathBuf, String)]) -> Vec<crate::desugar::MacroSource> {
    let mut out = Vec::new();

    for (rel, src) in files {
        let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
            continue;
        };
        let toks = &parsed.lexed.toks;
        let _ = rel;

        for stmt in &parsed.chunk.block.stmts {
            let Stmt::Macro(m) = stmt else {
                continue;
            };

            if !m.global {
                continue;
            }

            out.push(crate::desugar::MacroSource {
                name: token_text(src, toks, m.name),
                params: m
                    .params
                    .iter()
                    .filter(|p| !p.is_vararg)
                    .map(|p| token_text(src, toks, p.name))
                    .collect(),
                variadic: m.params.iter().any(|p| p.is_vararg),
                body: join_tokens(src, toks, m.body.span),
                tail: m.tail.as_ref().map(|t| join_tokens(src, toks, t.span())),
            });
        }
    }

    out
}

/// The `global attribute` declarations of the project: each name with
/// the targets it takes and its parameters, the way the file that
/// declares it holds them.
pub fn attribute_decls(files: &[(PathBuf, String)]) -> Vec<(String, crate::desugar::AttrDecl)> {
    let mut out = Vec::new();

    for (_, src) in files {
        let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
            continue;
        };
        let toks = &parsed.lexed.toks;

        for stmt in &parsed.chunk.block.stmts {
            let Stmt::Attribute(a) = stmt else {
                continue;
            };

            if !a.global {
                continue;
            }

            let targets = a
                .targets
                .iter()
                .map(|t| token_text(src, toks, *t))
                .collect();
            let params = a
                .params
                .iter()
                .map(|p| {
                    (
                        token_text(src, toks, p.name),
                        p.ty.map(|t| span_text(src, toks, t).trim().to_string()),
                    )
                })
                .collect();
            out.push((token_text(src, toks, a.name), (targets, params)));
        }
    }

    out
}

/// The text of the first token of a span.
fn token_text(src: &str, toks: &[alloy_syntax::lexer::Tok], span: TokSpan) -> String {
    match toks.get(span.start as usize) {
        Some(t) => src[t.start as usize..t.end as usize].to_string(),

        None => String::new(),
    }
}

/// The source a span covers, as written.
fn span_text(src: &str, toks: &[alloy_syntax::lexer::Tok], span: TokSpan) -> String {
    if span.end <= span.start || span.end as usize > toks.len() {
        return String::new();
    }

    src[toks[span.start as usize].start as usize..toks[span.end as usize - 1].end as usize]
        .to_string()
}

/// The tokens of a span joined by one space, the shape a macro body
/// takes for its expansion.
fn join_tokens(src: &str, toks: &[alloy_syntax::lexer::Tok], span: TokSpan) -> String {
    let mut out = String::new();

    for i in span.start..span.end {
        let Some(t) = toks.get(i as usize) else {
            break;
        };

        if !out.is_empty() {
            out.push(' ');
        }

        out.push_str(&src[t.start as usize..t.end as usize]);
    }

    out
}

/// The text of a source as the index reads it. A `.alx` file holds
/// markup the Alloy parser does not take, so the markup regions blank;
/// every other byte, and every offset, stays where it is.
pub fn index_text(path: &Path, src: &str) -> String {
    if path.extension().and_then(|e| e.to_str()) != Some("alx") {
        return src.to_string();
    }

    match luaux::compile::markup_spans(src) {
        Ok(spans) => luaux::resolve::blank_luaux_regions(src, &spans),

        Err(_) => src.to_string(),
    }
}

/// The name of the module a script's globals move into, from the
/// script's own file name: `main.server.aly` gives
/// `main.server.globals.aly`.
pub fn hoist_name(file: &str) -> String {
    let stem = file
        .strip_suffix(".aly")
        .or_else(|| file.strip_suffix(".alx"))
        .unwrap_or(file);

    format!("{stem}.globals.aly")
}

/// The module a script's globals are hoisted into: the script's text
/// with everything but its imports and its globals blanked. A script
/// cannot be required, so its globals move to a module beside it, and
/// the script requires that module like every other file.
///
/// The blank keeps every newline, so a line of the module is the line
/// of the script that wrote it.
pub fn hoisted_module(src: &str) -> String {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return String::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = src.as_bytes().to_vec();

    for stmt in &parsed.chunk.block.stmts {
        if matches!(stmt, Stmt::Import(_)) || is_global(stmt) {
            continue;
        }

        let span = stmt.span();
        let (Some(first), Some(last)) = (
            toks.get(span.start as usize),
            toks.get((span.end as usize).saturating_sub(1)),
        ) else {
            continue;
        };

        for byte in out
            .iter_mut()
            .take(last.end as usize)
            .skip(first.start as usize)
        {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }

    String::from_utf8(out).unwrap_or_default()
}

/// Whether a statement carries the `global` modifier.
pub fn is_global(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Function(d) => d.global,
        Stmt::LocalFunction(d) => d.global,
        Stmt::Local(d) => d.global,
        Stmt::Struct(d) => d.global,
        Stmt::Enum(d) => d.global,
        Stmt::Trait(d) => d.global,
        Stmt::Interface(d) => d.global,
        Stmt::Class(d) => d.global,
        Stmt::TypeAlias(d) => d.global,
        Stmt::Remote(d) => d.global,
        Stmt::Macro(d) => d.global,
        Stmt::Attribute(d) => d.global,
        Stmt::Impl(d) => d.global,
        Stmt::Namespace(d) => d.global,

        _ => false,
    }
}

/// The names a script's globals reach for that the hoisted module will
/// not hold: a top-level name of the script that is not an import and
/// not a global itself. Each one with the offset it sits at.
///
/// A global in a script has to stand on its own, since it moves into a
/// module of its own. Only imports, other globals, and literals travel.
pub fn script_leaks(src: &str) -> Vec<(String, u32)> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut left_behind: std::collections::HashSet<String> = std::collections::HashSet::new();

    for stmt in &parsed.chunk.block.stmts {
        let names = crate::desugar::bound_names(src, toks, stmt);

        if matches!(stmt, Stmt::Import(_)) || is_global(stmt) {
            kept.extend(names);
        } else {
            left_behind.extend(names);
        }
    }

    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if !is_global(stmt) {
            continue;
        }

        let span = stmt.span();

        for i in span.start..span.end {
            let Some(t) = toks.get(i as usize) else {
                break;
            };
            let word = &src[t.start as usize..t.end as usize];
            let after_dot = i > span.start
                && toks
                    .get(i as usize - 1)
                    .is_some_and(|p| matches!(&src[p.start as usize..p.end as usize], "." | ":"));

            if after_dot || kept.contains(word) || !left_behind.contains(word) {
                continue;
            }

            if !out.iter().any(|(n, _): &(String, u32)| n == word) {
                out.push((word.to_string(), t.start));
            }
        }
    }

    out
}

/// The require string one file uses to reach another, both relative to
/// `[build] in`: `./other`, `../shared/log`. The extension goes, the
/// way an `import` writes the spec.
pub fn require_from(user: &Path, target: &Path) -> String {
    let stem = strip_extension(target);
    let user_dir: Vec<String> = components(user.parent().unwrap_or(Path::new("")));
    let target_parts: Vec<String> = components(&stem);
    let shared = user_dir
        .iter()
        .zip(&target_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = Vec::new();

    for _ in shared..user_dir.len() {
        parts.push("..".to_string());
    }

    if parts.is_empty() {
        parts.push(".".to_string());
    }

    parts.extend(target_parts[shared..].iter().cloned());

    parts.join("/")
}

fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),

            _ => None,
        })
        .collect()
}

/// The path with the Alloy extension removed: `a/b.aly` is `a/b`.
fn strip_extension(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let Some(name) = name else {
        return path.to_path_buf();
    };
    let stem = name
        .strip_suffix(".aly")
        .or_else(|| name.strip_suffix(".alx"))
        .unwrap_or(&name);

    path.with_file_name(stem)
}

/// The globals of a project as one file sees them: each name with the
/// require that reaches it from `user`. A global the file declares
/// itself is left out; the file already has the name.
pub fn refs_for(
    globals: &[Global],
    user: &Path,
    ship: &HashMap<PathBuf, String>,
) -> Vec<crate::desugar::GlobalRef> {
    let mut out = Vec::new();

    for g in globals {
        // An `impl` binds no name, and a macro and an attribute travel
        // as declarations, not as a require.
        if matches!(g.kind, Kind::Impl | Kind::Macro | Kind::Attribute) || g.file == user {
            continue;
        }

        out.push(crate::desugar::GlobalRef {
            namespace: g.kind == Kind::Namespace,
            side: g.side,
            name: g.name.clone(),
            file: g.file.to_string_lossy().replace('\\', "/"),
            require: require_from(user, &g.file),
            ship_require: ship.get(&g.file).cloned(),
            value: g.kind.is_value(),
            ty: g.kind.is_type(),
            type_params: g.type_params.clone(),
        });
    }

    out
}

/// The project globals a source names, each with the byte offset of
/// its first use. The render decides: a name the file binds itself is
/// the file's own, and a field or a key is not the name at all.
pub fn used(src: &str, refs: &[crate::desugar::GlobalRef]) -> Vec<(String, u32)> {
    if refs.is_empty() {
        return Vec::new();
    }

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let options = crate::desugar::EmitOptions {
        globals: refs.to_vec(),
        in_project: true,
        ..crate::desugar::EmitOptions::default()
    };

    crate::desugar::render(src, &parsed.lexed.toks, &parsed.chunk, &options).globals_used
}

/// The names two files both declare as global, each with the two files.
/// A hidden dependency the reader cannot see is the reason the build
/// reports it: neither file names the other.
pub fn duplicates(globals: &[Global]) -> Vec<(String, PathBuf, PathBuf)> {
    let mut seen: HashMap<&str, &Global> = HashMap::new();
    let mut out = Vec::new();

    for g in globals {
        match seen.get(g.name.as_str()) {
            Some(first) if first.file != g.file => {
                out.push((g.name.clone(), first.file.clone(), g.file.clone()));
            }

            Some(_) => {}

            None => {
                seen.insert(&g.name, g);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_global_function_is_indexed() {
        let g = declared(
            "global function log(m: string)\nend\n",
            Path::new("shared/log.aly"),
        );
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].name, "log");
        assert_eq!(g[0].kind, Kind::Function);
        assert!(g[0].kind.is_value());
        assert!(!g[0].kind.is_type());
    }

    #[test]
    fn a_global_struct_is_a_value_and_a_type() {
        let g = declared(
            "global struct Vec2 as\n    x: number\nend\n",
            Path::new("v.aly"),
        );
        assert_eq!(g[0].kind, Kind::Struct);
        assert!(g[0].kind.is_value() && g[0].kind.is_type());
    }

    #[test]
    fn a_generic_global_keeps_its_parameters() {
        let g = declared(
            "global struct Slot<T> as\n    v: T\nend\n",
            Path::new("s.aly"),
        );
        assert_eq!(g[0].type_params, "<T>");
    }

    #[test]
    fn an_export_is_not_a_global() {
        assert!(declared("export function log()\nend\n", Path::new("a.aly")).is_empty());
    }

    #[test]
    fn the_require_climbs_out_of_the_folder() {
        assert_eq!(
            require_from(Path::new("main.aly"), Path::new("shared/log.aly")),
            "./shared/log"
        );
        assert_eq!(
            require_from(Path::new("server/hit.aly"), Path::new("shared/log.aly")),
            "../shared/log"
        );
        assert_eq!(
            require_from(Path::new("shared/a.aly"), Path::new("shared/log.aly")),
            "./log"
        );
        assert_eq!(
            require_from(Path::new("a/b/c/deep.aly"), Path::new("a/other.aly")),
            "../../other"
        );
    }

    #[test]
    fn two_files_with_one_name_are_a_duplicate() {
        let mut all = declared("global function log()\nend\n", Path::new("a.aly"));
        all.extend(declared("global const log = 1\n", Path::new("b.aly")));
        let dups = duplicates(&all);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].0, "log");
    }
}
