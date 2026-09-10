//! What a module exports for the compiler's own use: the type names of
//! an imported file, so `import { Zone } from "./types"` brings the
//! type `Zone` in beside the value. Luau binds a value with `local` and
//! a type with `type`; a struct or an enum is both, and the author
//! writes the name once.
//!
//! The index is a line scan of the target file, not a compile: it runs
//! for every import of every file on every edit in the editor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::Config;

/// The type names a source exports: `export struct X`, `export enum X`,
/// `export interface X`, `export trait X`, `export type X`, and
/// `export class X`. A plain Luau module's `export type X` counts too.
///
/// A generic type carries its parameter list, `Slotted<T>`. An alias to
/// it in another module has to pass the parameters on, or Luau reads
/// the alias as the bare name and asks for the argument that is gone.
pub fn exported_types(source: &str) -> Vec<String> {
    let mut out = exported_namespace_types(source);

    for line in source.lines() {
        let rest = line.trim_start();
        let Some(rest) = rest.strip_prefix("export") else {
            continue;
        };

        if !rest.starts_with(char::is_whitespace) {
            continue;
        }

        let rest = rest.trim_start();
        let kind: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();

        if !matches!(
            kind.as_str(),
            "struct" | "enum" | "interface" | "trait" | "type" | "class"
        ) {
            continue;
        }

        let after = rest[kind.len()..].trim_start();
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }

        out.push(format!("{name}{}", type_params(&after[name.len()..])));
    }

    out
}

/// The types an exported namespace carries, under the names the emit
/// gives them: `export namespace Math as struct Vec2 ... end` exports
/// `Math_Vec2`. A namespace body is not a line the scan above can read,
/// so this one goes through the parser.
fn exported_namespace_types(source: &str) -> Vec<String> {
    use alloy_syntax::ast::Stmt;

    if !source.contains("namespace") {
        return Vec::new();
    }

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| -> String {
        let a = toks[span.start as usize].start as usize;
        let b = toks[(span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize)]
        .end as usize;

        source[a..b].to_string()
    };
    let mut listed: Vec<String> = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if let Stmt::ExportList(list) = stmt {
            for spec in &list.specs {
                listed.push(text(spec.name));
            }
        }
    }

    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Namespace(ns) = stmt.under_default() else {
            continue;
        };
        let name = text(ns.name);

        if !ns.exported && !ns.global && !listed.contains(&name) {
            continue;
        }

        collect_namespace_types(source, toks, ns, &name, &mut out);
    }

    out
}

/// The type members of one namespace, with a nested namespace folded
/// into the name the same way.
fn collect_namespace_types(
    source: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    prefix: &str,
    out: &mut Vec<String>,
) {
    use alloy_syntax::ast::Stmt;

    let text = |span: alloy_syntax::ast::TokSpan| -> String {
        let a = toks[span.start as usize].start as usize;
        let b = toks[(span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize)]
        .end as usize;

        source[a..b].to_string()
    };

    for m in &ns.members {
        if m.is_private(source, toks) {
            continue;
        }

        let (name, generics) = match m.stmt.under_default() {
            Stmt::Struct(d) => (text(d.name), d.generics.map(text)),

            Stmt::Enum(d) => (text(d.name), None),

            Stmt::Trait(d) => (text(d.name), None),

            Stmt::Interface(d) => (text(d.name), d.generics.map(text)),

            Stmt::TypeAlias(d) => {
                let after = toks[d.name.end as usize - 1].end as usize;

                (text(d.name), Some(source[after..].to_string()))
            }

            Stmt::Namespace(inner) => {
                let deeper = format!("{prefix}_{}", text(inner.name));
                collect_namespace_types(source, toks, inner, &deeper, out);

                continue;
            }

            _ => continue,
        };
        let params = generics
            .map(|g| type_params(g.trim_start()))
            .unwrap_or_default();
        out.push(format!("{prefix}_{name}{params}"));
    }
}

/// The parameter list a declaration opens with, as Luau takes it: the
/// bounds go, since a Luau alias holds none. An empty text when the
/// text does not open with `<`.
pub fn type_params(text: &str) -> String {
    let Some(open) = text.strip_prefix('<') else {
        return String::new();
    };
    let Some(close) = open.find('>') else {
        return String::new();
    };
    let names: Vec<&str> = open[..close]
        .split(',')
        .map(|p| p.split(':').next().unwrap_or(p).trim())
        .filter(|p| !p.is_empty())
        .collect();

    match names.is_empty() {
        true => String::new(),

        false => format!("<{}>", names.join(", ")),
    }
}

/// The name a type entry carries, without its parameter list.
pub fn type_head(entry: &str) -> &str {
    entry.split('<').next().unwrap_or(entry)
}

/// The traits a source exports with their default methods, the ones
/// with a body. An `impl Trait for S` in another file flattens them in.
pub fn exported_trait_defaults(source: &str) -> Vec<(String, Vec<String>)> {
    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| {
        let t = toks[span.start as usize];

        source[t.start as usize..t.end as usize].to_string()
    };
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if let alloy_syntax::ast::Stmt::Trait(t) = stmt
            && t.exported
        {
            let defaults: Vec<String> = t
                .methods
                .iter()
                .filter(|m| m.body.is_some())
                .map(|m| text(m.name))
                .collect();
            out.push((text(t.name), defaults));
        }
    }

    out
}

/// The exported async functions whose declared return type is a
/// `Result`. A `try await` on one yields the Result itself, so the
/// emit calls `try_await_result` there.
pub fn exported_result_asyncs(source: &str) -> Vec<String> {
    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| {
        let t = toks[span.start as usize];

        source[t.start as usize..t.end as usize].to_string()
    };
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if let alloy_syntax::ast::Stmt::Function(f) = stmt
            && f.exported
            && f.body.is_async.is_some()
            && f.body.ret_type.is_some_and(|rt| text(rt) == "Result")
            && f.path.len() == 1
        {
            out.push(text(f.path[0]));
        }
    }

    out
}

/// The `Result`-returning async functions of every module a source
/// imports, flat.
pub fn import_result_asyncs(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();

    for spec in import_specs(source) {
        let Some(path) = resolve(&spec, from, aliases) else {
            continue;
        };

        if seen.contains(&path) {
            continue;
        }

        seen.push(path.clone());

        if let Ok(text) = std::fs::read_to_string(&path) {
            out.extend(exported_result_asyncs(&text));
        }
    }

    out
}

/// The import result asyncs of a file under the nearest `alloy.toml`.
pub fn import_result_asyncs_for_file(path: &Path, source: &str) -> Vec<String> {
    let (from, aliases) = file_context(path);

    import_result_asyncs(source, &from, &aliases)
}

/// For each import of a source, the default methods of the traits the
/// module exports, flat: `(trait, methods)`.
pub fn import_trait_defaults(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    for spec in import_specs(source) {
        let Some(path) = resolve(&spec, from, aliases) else {
            continue;
        };

        if seen.contains(&path) {
            continue;
        }

        seen.push(path.clone());

        if let Ok(text) = std::fs::read_to_string(&path) {
            for (t, d) in exported_trait_defaults(&text) {
                if !d.is_empty() && !out.iter().any(|(n, _)| *n == t) {
                    out.push((t, d));
                }
            }
        }
    }

    out
}

/// The import types and trait defaults of a file under the nearest
/// `alloy.toml`; see `import_types_for_file`.
pub fn import_trait_defaults_for_file(path: &Path, source: &str) -> Vec<(String, Vec<String>)> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let aliases = match Config::find(dir) {
        Some(config_path) => match Config::load(&config_path) {
            Ok(config) => {
                let root = config_path.parent().unwrap_or(dir);

                aliases(root, &crate::project::Tree::load(root, &config))
            }

            Err(_) => Vec::new(),
        },

        None => Vec::new(),
    };
    let from = normalize(&std::env::current_dir().unwrap_or_default().join(path));

    import_trait_defaults(source, &from, &aliases)
}

/// The alias table of a project, each alias to an absolute folder: the
/// aliases the tree carries, which come from `.config.luau` or
/// `.luaurc`, and from the `[mount]` table when that is the tree.
pub fn aliases(root: &Path, tree: &crate::project::Tree) -> Vec<(String, PathBuf)> {
    tree.aliases
        .iter()
        .map(|(a, p)| (a.clone(), normalize(&root.join(p))))
        .collect()
}

/// The file an import spec names from a source file: `./x`, `../x`, or
/// `@alias/x`, with `.aly`, `.alx`, `.luau`, `.lua`, or an `init` file.
pub fn resolve(spec: &str, from: &Path, aliases: &[(String, PathBuf)]) -> Option<PathBuf> {
    let base = if let Some(rest) = spec.strip_prefix('@') {
        let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let (_, dir) = aliases.iter().find(|(a, _)| a == alias)?;

        dir.join(tail)
    } else if spec.starts_with("./") || spec.starts_with("../") {
        from.parent()?.join(spec)
    } else {
        return None;
    };
    let base = normalize(&base);

    if is_file_exact(&base) {
        return Some(base);
    }

    for ext in ["aly", "alx", "luau", "lua"] {
        let candidate = base.with_extension(ext);

        if is_file_exact(&candidate) {
            return Some(candidate);
        }

        let init = base.join(format!("init.{ext}"));

        if is_file_exact(&init) {
            return Some(init);
        }
    }

    None
}

/// The project root a source sits under: its absolute path with as
/// many steps removed as its path from the root has. A source given by
/// its own name alone leaves no root, and a message shows the whole
/// path then.
fn root_of(from: &Path, rel: &Path) -> Option<PathBuf> {
    let mut root = from.to_path_buf();

    for _ in rel.components() {
        if !root.pop() {
            return None;
        }
    }

    (root.components().next().is_some()).then_some(root)
}

/// The folder an `@alias/tail` spec names, when the project declares
/// the alias: the alias's folder with the rest of the path under it,
/// shown from `root` when it sits there. `None` when no alias matches.
pub fn alias_target(
    spec: &str,
    aliases: &[(String, PathBuf)],
    root: Option<&Path>,
) -> Option<PathBuf> {
    let rest = spec.strip_prefix('@')?;
    let (alias, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let (_, dir) = aliases.iter().find(|(a, _)| a == alias)?;
    let target = normalize(&dir.join(tail));

    Some(match root.and_then(|r| target.strip_prefix(r).ok()) {
        Some(under) => under.to_path_buf(),

        None => target,
    })
}

/// Whether a file name names a script rather than a module: `.server`
/// or `.client` before the extension. Roblox runs a script on its own,
/// and a script returns nothing, so no import can name one.
pub fn is_script(name: &str) -> bool {
    let stem = ["d.aly", "aly", "alx", "luau", "lua"]
        .iter()
        .find_map(|ext| name.strip_suffix(&format!(".{ext}")))
        .unwrap_or(name);

    stem.ends_with(".server") || stem.ends_with(".client")
}

/// Whether the file exists under exactly this name.
///
/// macOS and Windows match a file name without regard to case, so
/// `import x from "./Foo"` finds `foo.aly` there and then fails in
/// Roblox, where an instance name is exact. The resolver reads the
/// directory, so a project resolves the same on every platform. The
/// read costs one call, and only for a name the file system already
/// found.
fn is_file_exact(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return true;
    };

    match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().any(|e| e.file_name() == name),

        // A directory the process cannot list: the file system's own
        // answer stands.
        Err(_) => true,
    }
}

/// For each `import ... from "spec"` of a source, the type names the
/// module exports, read from disk. A spec that resolves to nothing has
/// no entry, and the import stays a value import.
pub fn import_types(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();

    for spec in import_specs(source) {
        if out.iter().any(|(s, _)| *s == spec) {
            continue;
        }

        let Some(path) = resolve(&spec, from, aliases) else {
            continue;
        };
        let types = cache
            .entry(path.clone())
            .or_insert_with(|| {
                std::fs::read_to_string(&path)
                    .map(|t| exported_types(&t))
                    .unwrap_or_default()
            })
            .clone();

        if !types.is_empty() {
            out.push((spec, types));
        }
    }

    out
}

/// The struct and enum shapes of every module the source imports, for
/// a hover that names an imported struct by its fields.
pub fn import_shapes(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<crate::declarations::Shape> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();

    for spec in import_specs(source) {
        let Some(path) = resolve(&spec, from, aliases) else {
            continue;
        };

        if seen.contains(&path) {
            continue;
        }

        seen.push(path.clone());

        if let Ok(text) = std::fs::read_to_string(&path) {
            out.extend(crate::declarations::shapes(&text));
        }
    }

    out
}

/// The private fields of every struct a module the source imports
/// declares: the struct's name with its private field names. The
/// `private_access` lint reads a field of an imported struct through it.
pub fn import_privates(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();

    for shape in import_shapes(source, from, aliases) {
        let crate::declarations::Shape::Struct { name, fields, .. } = shape else {
            continue;
        };
        let private: Vec<String> = fields
            .into_iter()
            .filter(|(_, p)| *p)
            .map(|(n, _)| n)
            .collect();

        if !private.is_empty() && !out.iter().any(|(n, _)| *n == name) {
            out.push((name, private));
        }
    }

    out
}

/// The private fields of the imported structs of a file under the
/// nearest `alloy.toml`.
pub fn import_privates_for_file(path: &Path, source: &str) -> Vec<(String, Vec<String>)> {
    let (from, aliases) = file_context(path);

    import_privates(source, &from, &aliases)
}

/// The import shapes of a file under the nearest `alloy.toml`.
/// The enums every module a source imports declares, each with its
/// variants and how many values they carry. A `match` over an imported
/// enum reads them to prove it covers every variant.
pub fn import_enums(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<(String, usize)>)> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();

    for spec in import_specs(source) {
        let Some(path) = resolve(&spec, from, aliases) else {
            continue;
        };

        if seen.contains(&path) {
            continue;
        }

        seen.push(path.clone());

        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        for shape in crate::declarations::shapes(&text) {
            let crate::declarations::Shape::Enum { name, variants } = shape else {
                continue;
            };

            if !out.iter().any(|(n, _)| *n == name) {
                out.push((
                    name,
                    variants.into_iter().map(|(v, p)| (v, p.len())).collect(),
                ));
            }
        }
    }

    out
}

/// The file each import of a source resolves to, under the nearest
/// `alloy.toml`. A path that names no file is left out.
pub fn import_targets_for_file(path: &Path, source: &str) -> Vec<PathBuf> {
    let (from, aliases) = file_context(path);
    let mut out: Vec<PathBuf> = Vec::new();

    for spec in import_specs(source) {
        if let Some(target) = resolve(&spec, &from, &aliases)
            && !out.contains(&target)
        {
            out.push(target);
        }
    }

    out
}

/// The imported enums of a file under the nearest `alloy.toml`.
pub fn import_enums_for_file(path: &Path, source: &str) -> Vec<(String, Vec<(String, usize)>)> {
    let (from, aliases) = file_context(path);

    import_enums(source, &from, &aliases)
}

/// The text of every module a file imports, under the nearest
/// `alloy.toml`. A reader of the file needs the declarations its
/// imports bring in, and only the source carries them all.
pub fn import_sources_for_file(path: &Path, source: &str) -> Vec<String> {
    let (from, aliases) = file_context(path);
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();

    for spec in import_specs(source) {
        let Some(target) = resolve(&spec, &from, &aliases) else {
            continue;
        };

        if seen.contains(&target) {
            continue;
        }

        seen.push(target.clone());

        if let Ok(text) = std::fs::read_to_string(&target) {
            out.push(text);
        }
    }

    out
}

pub fn import_shapes_for_file(path: &Path, source: &str) -> Vec<crate::declarations::Shape> {
    let (from, aliases) = file_context(path);

    import_shapes(source, &from, &aliases)
}

/// The absolute path of a file and the aliases of its project.
fn file_context(path: &Path) -> (PathBuf, Vec<(String, PathBuf)>) {
    let dir = path.parent().unwrap_or(Path::new("."));
    let aliases = match Config::find(dir) {
        Some(config_path) => match Config::load(&config_path) {
            Ok(config) => {
                let root = config_path.parent().unwrap_or(dir);

                aliases(root, &crate::project::Tree::load(root, &config))
            }

            Err(_) => Vec::new(),
        },

        None => Vec::new(),
    };
    let from = normalize(&std::env::current_dir().unwrap_or_default().join(path));

    (from, aliases)
}

/// The specs of a source whose module has no export table: a `.luau`
/// or `.lua` file, a data file, and an Alloy module that ends in
/// `return <expr>`. Such a module returns one value, so `import X from`
/// binds that value, not a `default` field of it.
pub fn plain_modules(source: &str, from: &Path, aliases: &[(String, PathBuf)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    for spec in import_specs(source) {
        if out.contains(&spec) {
            continue;
        }

        let plain = crate::data::Format::of(&spec).is_some()
            || resolve(&spec, from, aliases).is_some_and(|p| match is_alloy(&p) {
                true => std::fs::read_to_string(&p).is_ok_and(|t| returns_value(&t)),

                false => true,
            });

        if plain {
            out.push(spec);
        }
    }

    out
}

/// Whether a path names a file the Alloy compiler reads.
fn is_alloy(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "aly" || e == "alx")
}

/// The plain modules a file imports, under the nearest `alloy.toml`.
pub fn plain_modules_for_file(path: &Path, source: &str) -> Vec<String> {
    let (from, aliases) = file_context(path);

    plain_modules(source, &from, &aliases)
}

/// The import problems of a file under the nearest `alloy.toml`. `rel`
/// is the path a message names, which reads best from the project root;
/// `None` names the file as it was given.
pub fn import_problems_for_file(
    path: &Path,
    rel: Option<&Path>,
    source: &str,
) -> Vec<ImportProblem> {
    let (from, aliases) = file_context(path);

    import_problems(source, rel.unwrap_or(path), &from, &aliases)
}

/// The import types of a file under the nearest `alloy.toml`, or none
/// when the file sits in no project.
pub fn import_types_for_file(path: &Path, source: &str) -> Vec<(String, Vec<String>)> {
    let (from, aliases) = file_context(path);

    import_types(source, &from, &aliases)
}

/// One problem with an import: the module, a name the module does not
/// export, or a name the file imports twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProblem {
    /// Byte offsets into the source, over the path or the name.
    pub start: u32,
    pub end: u32,
    pub kind: &'static str,
    pub message: String,
}

/// The names a module exposes to an `import { ... }`: every declaration
/// the source marks `export`, and every name an `export { ... }` list
/// carries. The order is the order of the file.
pub fn exported_names(source: &str) -> Vec<String> {
    use alloy_syntax::ast::Stmt;

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| -> String {
        let a = toks[span.start as usize].start as usize;
        let b = toks[(span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize)]
        .end as usize;

        source[a..b].to_string()
    };
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        match stmt {
            Stmt::Struct(d) if d.exported => out.push(text(d.name)),
            Stmt::Enum(d) if d.exported => out.push(text(d.name)),
            Stmt::Trait(d) if d.exported => out.push(text(d.name)),
            Stmt::Interface(d) if d.exported => out.push(text(d.name)),
            Stmt::Class(d) if d.exported => out.push(text(d.name)),
            Stmt::TypeAlias(d) if d.exported => out.push(text(d.name)),
            Stmt::Remote(d) if d.exported => out.push(text(d.name)),
            Stmt::Macro(d) if d.exported => out.push(text(d.name)),
            Stmt::LocalFunction(d) if d.exported => out.push(text(d.name)),
            Stmt::Namespace(d) if d.exported => out.push(text(d.name)),

            Stmt::Function(d) if d.exported => {
                if let Some(first) = d.path.first() {
                    out.push(text(*first));
                }
            }

            Stmt::Local(d) if d.exported => {
                for b in &d.names {
                    out.push(text(b.name));
                }
            }

            Stmt::ExportList(list) => {
                for spec in &list.specs {
                    out.push(text(spec.alias.unwrap_or(spec.name)));
                }
            }

            _ => {}
        }
    }

    out.retain(|n| !n.is_empty());
    out.dedup();
    out
}

/// Whether a module has an `export default`, the value a bare
/// `import X from "./m"` binds.
pub fn exports_default(source: &str) -> bool {
    use alloy_syntax::ast::Stmt;

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return false;
    };

    parsed
        .chunk
        .block
        .stmts
        .iter()
        .any(|s| matches!(s, Stmt::ExportDefault { .. }))
}

/// Whether a module ends its top-level statements in `return <expr>`.
/// Luau reads that value as the module, and Alloy reads it the same
/// way: the returned value is the module's default export.
pub fn returns_value(source: &str) -> bool {
    use alloy_syntax::ast::Stmt;

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return false;
    };

    matches!(
        parsed.chunk.block.stmts.last(),
        Some(Stmt::Return(r)) if !r.values.is_empty()
    )
}

/// Whether a module returns a value and exports names too. The two
/// rules each name one value for the module, so the build reports it.
pub fn returns_and_exports(source: &str) -> bool {
    returns_value(source) && exports_values(source)
}

/// Whether a module puts a value in its export table. `export type`
/// and `export interface` name types alone, and a module of those
/// still returns its own value.
pub fn exports_values(source: &str) -> bool {
    use alloy_syntax::ast::Stmt;

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return false;
    };

    parsed.chunk.block.stmts.iter().any(|stmt| match stmt {
        Stmt::Struct(d) => d.exported,
        Stmt::Enum(d) => d.exported,
        Stmt::Trait(d) => d.exported,
        Stmt::Class(d) => d.exported,
        Stmt::Remote(d) => d.exported,
        Stmt::Macro(d) => d.exported,
        Stmt::Attribute(d) => d.exported,
        Stmt::Function(d) => d.exported,
        Stmt::LocalFunction(d) => d.exported,
        Stmt::Local(d) => d.exported,
        Stmt::Namespace(d) => d.exported,
        Stmt::ExportDefault { .. } => true,
        Stmt::ExportList(list) => !list.type_only && list.specs.iter().any(|sp| !sp.is_type),

        _ => false,
    })
}

/// The keys of the value a module returns, when the compiler can read
/// them: a table literal with named keys, a local the file fills in by
/// name, or a struct the file declares. `None` when the value's keys
/// are out of reach, and an `import { }` of that module is left alone.
pub fn returned_keys(source: &str) -> Option<Vec<String>> {
    use alloy_syntax::ast::{Expr, Stmt};

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let parsed = alloy_syntax::parse_lenient(source, options).ok()?;
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let text = |span: alloy_syntax::ast::TokSpan| -> String {
        let a = toks[span.start as usize].start as usize;
        let b = toks[(span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize)]
        .end as usize;

        source[a..b].to_string()
    };

    let Some(Stmt::Return(r)) = stmts.last() else {
        return None;
    };

    if r.values.len() != 1 {
        return None;
    }

    match &r.values[0] {
        Expr::Table { fields, .. } => table_keys(fields, toks, source),

        // `local M = { }` with `M.f` filled in below it, the shape a
        // Luau module is written in.
        Expr::Name(n) => {
            let name = text(*n);
            let mut keys: Option<Vec<String>> = None;

            for stmt in stmts {
                let Stmt::Local(l) = stmt else {
                    continue;
                };

                if l.names.len() != 1 || text(l.names[0].name) != name {
                    continue;
                }

                let Some(Expr::Table { fields, .. }) = l.values.first() else {
                    return None;
                };

                keys = Some(table_keys(fields, toks, source)?);
            }

            let mut keys = keys?;
            keys.extend(assigned_keys(source, &name));
            keys.dedup();

            Some(keys)
        }

        // `new Vec2 { ... }`: the struct the file declares says which
        // fields the value carries.
        Expr::New { name, .. } => {
            let head = match name.as_ref() {
                Expr::Name(n) => text(*n),

                _ => return None,
            };

            crate::declarations::shapes(source)
                .into_iter()
                .find_map(|s| match s {
                    crate::declarations::Shape::Struct { name, fields, .. } if name == head => {
                        Some(fields.into_iter().map(|(f, _)| f).collect())
                    }

                    _ => None,
                })
        }

        _ => None,
    }
}

/// The named keys of a table literal. A spread copies keys the reader
/// cannot see here, so a table with one is out of reach.
fn table_keys(
    fields: &[alloy_syntax::ast::TableField],
    toks: &[alloy_syntax::lexer::Tok],
    source: &str,
) -> Option<Vec<String>> {
    use alloy_syntax::ast::TableField;

    let mut out = Vec::new();

    for field in fields {
        match field {
            TableField::Named { name, .. } => {
                let t = toks[name.start as usize];
                out.push(source[t.start as usize..t.end as usize].to_string());
            }

            TableField::Spread(_) => return None,

            _ => {}
        }
    }

    Some(out)
}

/// The keys a file writes onto one name after it binds it:
/// `M.f = ...`, `function M.f()`, and `function M:f()`.
fn assigned_keys(source: &str, name: &str) -> Vec<String> {
    use alloy_syntax::ast::Stmt;

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let word = |span: alloy_syntax::ast::TokSpan| -> String {
        match toks.get(span.start as usize) {
            Some(t) => source[t.start as usize..t.end as usize].to_string(),

            None => String::new(),
        }
    };
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        match stmt {
            Stmt::Function(f) if f.path.len() == 2 && word(f.path[0]) == name => {
                out.push(word(f.path[1]));
            }

            Stmt::Assign(a) => {
                for target in &a.targets {
                    if let alloy_syntax::ast::Expr::Index {
                        object,
                        key: alloy_syntax::ast::IndexKey::Field(k),
                        ..
                    } = target
                        && matches!(object.as_ref(), alloy_syntax::ast::Expr::Name(n) if word(*n) == name)
                    {
                        out.push(word(*k));
                    }
                }
            }

            _ => {}
        }
    }

    out
}

/// The message for `import X from "./m"` where `m` has no default. It
/// names the export the author probably meant when one carries the
/// binding's name.
fn no_default_message(spec: &str, local: &str, names: &[String]) -> String {
    // The name written here when the module exports it, else the one
    // name it exports.
    let meant = names
        .iter()
        .find(|n| *n == local)
        .or_else(|| names.first().filter(|_| names.len() == 1));

    if let Some(name) = meant {
        return format!(
            "\"{spec}\" has no default export; write `import {{ {name} }} from \"{spec}\"` \
             or add `export default` to it"
        );
    }

    if names.is_empty() {
        return format!("\"{spec}\" has no default export; add `export default` to it");
    }

    format!(
        "\"{spec}\" has no default export; it exports {}, or add `export default` to it",
        and_list(names)
    )
}

/// `a`, `b` and `c` as `` `a`, `b` and `c` ``, for a message that lists
/// what a module exports.
fn and_list(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();

    match quoted.split_last() {
        None => "nothing".to_string(),

        Some((last, [])) => last.clone(),

        Some((last, head)) => format!("{} and {last}", head.join(", ")),
    }
}

/// What one module offers an import: the names it exports, whether it
/// has a default, whether it returns a value instead of an export
/// table, and the keys of that value when the compiler can read them.
#[derive(Debug, Clone, Default)]
struct Surface {
    names: Vec<String>,
    has_default: bool,
    /// The module ends in `return <expr>` and exports no value.
    returns: bool,
    /// The module returns a value and exports names too, which is an
    /// error: each rule names a different value for the module.
    both: bool,
    keys: Option<Vec<String>>,
}

impl Surface {
    fn of(source: &str) -> Self {
        let returns = returns_value(source);
        let both = returns && exports_values(source);

        Surface {
            names: exported_names(source),
            has_default: exports_default(source),
            returns: returns && !both,
            both,
            keys: match returns && !both {
                true => returned_keys(source),

                false => None,
            },
        }
    }
}

/// Every problem the imports of one source have: a module that names no
/// file, a name the module does not export, and a name imported twice.
/// `rel` is the source's path for the message, relative to the root.
pub fn import_problems(
    source: &str,
    rel: &Path,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<ImportProblem> {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let range = |span: alloy_syntax::ast::TokSpan| -> (u32, u32) {
        let a = toks[span.start as usize].start;
        let b = toks[(span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize)]
        .end;

        (a, b)
    };
    let text = |span: alloy_syntax::ast::TokSpan| -> &str {
        let (a, b) = range(span);

        &source[a as usize..b as usize]
    };
    let mut out = Vec::new();
    let mut exports: HashMap<PathBuf, Surface> = HashMap::new();
    // Every name the file binds through an import. A type and a value
    // live in their own namespace, so `import * as M` and `import
    // { type M }` from the same module both bind and neither is a
    // duplicate of the other.
    let mut bound_values: Vec<String> = Vec::new();
    let mut bound_types: Vec<String> = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Import(node) = stmt else {
            continue;
        };
        let spec = text(node.path).trim_matches(['"', '\'']).to_string();

        // A `.json` or `.toml` import builds a module of its own; the
        // build reports what is wrong with one.
        if crate::data::Format::of(&spec).is_some() {
            continue;
        }

        // `"@game"` and `"@game/Players"` name Roblox services, not
        // modules. The path decides the form: `"@game"` takes a list in
        // braces, `"@game/X"` takes one name.
        if let Some(game) = crate::game_import::game_path(&spec) {
            use crate::game_import::GamePath;

            let quote = text(node.path).chars().next().unwrap_or('"');
            let (a, b) = range(node.span);
            // The service the name stands for, and the local it binds:
            // an unknown service is reported on the name, a name bound
            // twice on the local.
            let service_name = |service_at: alloy_syntax::ast::TokSpan,
                                service: &str,
                                local_at: alloy_syntax::ast::TokSpan,
                                out: &mut Vec<ImportProblem>,
                                bound: &mut Vec<String>| {
                if !crate::game_import::is_service(service) {
                    let (sa, sb) = range(service_at);
                    out.push(ImportProblem {
                        start: sa,
                        end: sb,
                        kind: "ImportError",
                        message: crate::game_import::unknown_message(service),
                    });
                }

                let local = text(local_at).to_string();

                if bound.contains(&local) {
                    let (la, lb) = range(local_at);
                    out.push(ImportProblem {
                        start: la,
                        end: lb,
                        kind: "ImportError",
                        message: format!("`{local}` is already imported in this file"),
                    });
                }

                bound.push(local);
            };
            // The name a wrong form wrote, for the message that shows
            // the two forms that work.
            let first = match &node.kind {
                ImportKind::Namespace(n) | ImportKind::Default(n) | ImportKind::Both(n, _) => {
                    Some(text(*n))
                }

                ImportKind::Named(list) | ImportKind::TypeOnly(list) => {
                    list.first().map(|s| text(s.name))
                }
            };

            // A first segment that names no service resolves nowhere,
            // whatever form the import takes, so the name is the whole
            // report and the form goes unsaid.
            if let GamePath::One(service) = &game
                && !crate::game_import::is_service(service)
            {
                let (pa, pb) = range(node.path);
                out.push(ImportProblem {
                    start: pa,
                    end: pb,
                    kind: "ImportError",
                    message: crate::game_import::unknown_message(service),
                });

                continue;
            }

            match (&game, &node.kind) {
                (GamePath::Every, ImportKind::Named(list)) => {
                    for item in list {
                        let written = text(item.name).to_string();
                        service_name(
                            item.name,
                            &written,
                            item.alias.unwrap_or(item.name),
                            &mut out,
                            &mut bound_values,
                        );
                    }
                }

                (GamePath::Every, _) => {
                    out.push(ImportProblem {
                        start: a,
                        end: b,
                        kind: "ImportError",
                        message: crate::game_import::braces_message(quote, first.unwrap_or("X")),
                    });
                }

                (GamePath::One(service), ImportKind::Default(name)) => {
                    service_name(node.path, service, *name, &mut out, &mut bound_values);
                }

                (GamePath::One(service), _) => {
                    out.push(ImportProblem {
                        start: a,
                        end: b,
                        kind: "ImportError",
                        message: crate::game_import::single_message(quote, service),
                    });
                }
            }

            continue;
        }

        let (path_start, path_end) = range(node.path);
        // A module that names no file is reported, and its names still
        // bind here, so a second import of one is a duplicate.
        let target = resolve(&spec, from, aliases);

        // Roblox runs a `.server` or `.client` file on its own, and the
        // file returns nothing, so an import takes no value from one.
        // The path never resolves either: `Foo.server` is not a stem.
        let names_script = is_script(&spec)
            || target
                .as_ref()
                .and_then(|p| p.file_name())
                .is_some_and(|n| is_script(&n.to_string_lossy()));

        if names_script {
            out.push(ImportProblem {
                start: path_start,
                end: path_end,
                kind: "UnknownModule",
                message: crate::typecheck::script_import_message(&spec),
            });
        } else if target.is_none() {
            let named = alias_target(&spec, aliases, root_of(from, rel).as_deref());
            out.push(ImportProblem {
                start: path_start,
                end: path_end,
                kind: "UnknownModule",
                message: crate::typecheck::unknown_module_message(&spec, rel, named.as_deref()),
            });
        }
        let type_only = matches!(&node.kind, ImportKind::TypeOnly(_));
        // A plain Luau module returns a table; its keys are not
        // declarations, so only an Alloy module's names are checked.
        let alloy_module = target.as_ref().is_some_and(|t| is_alloy(t));
        let surface = match &target {
            Some(target) => exports
                .entry(target.clone())
                .or_insert_with(|| {
                    std::fs::read_to_string(target)
                        .map(|t| Surface::of(&t))
                        .unwrap_or_default()
                })
                .clone(),

            None => Surface::default(),
        };
        let Surface {
            names,
            has_default,
            returns,
            both,
            keys,
        } = surface;

        // A module that returns a value names one value, and an
        // `export` names another. The reader cannot tell which one an
        // import binds, so the module has to pick.
        if alloy_module && both {
            out.push(ImportProblem {
                start: path_start,
                end: path_end,
                kind: "ImportError",
                message: format!("`{spec}` returns a value and exports names; use one"),
            });

            // Which value the import binds is the open question; every
            // check below rests on the answer.
            continue;
        }

        // The returned value is the module, the way a plain Luau module
        // reads. A bare name binds it, and a name in braces reads a key
        // of it when the compiler can see the keys.
        let alloy_module = alloy_module && !returns;
        let has_default = has_default || returns;
        // `import X from "./m"` reads the module's `export default`,
        // whatever `X` is called. A plain Luau or data module has no
        // export table, so its value is the default.
        let default_binding = |name: &alloy_syntax::ast::TokSpan,
                               out: &mut Vec<ImportProblem>,
                               bound_values: &mut Vec<String>| {
            let local = text(*name).to_string();
            let (a, b) = range(*name);

            if alloy_module && !has_default {
                out.push(ImportProblem {
                    start: a,
                    end: b,
                    kind: "ImportError",
                    message: no_default_message(&spec, &local, &names),
                });
            } else if bound_values.contains(&local) {
                out.push(ImportProblem {
                    start: a,
                    end: b,
                    kind: "ImportError",
                    message: format!("`{local}` is already imported in this file"),
                });
            }

            bound_values.push(local);
        };
        let specs = match &node.kind {
            ImportKind::Namespace(name) => {
                let local = text(*name).to_string();

                if bound_values.contains(&local) {
                    let (a, b) = range(*name);
                    out.push(ImportProblem {
                        start: a,
                        end: b,
                        kind: "ImportError",
                        message: format!("`{local}` is already imported in this file"),
                    });
                }

                bound_values.push(local);

                continue;
            }

            ImportKind::Default(name) => {
                default_binding(name, &mut out, &mut bound_values);

                continue;
            }

            ImportKind::Both(name, list) => {
                default_binding(name, &mut out, &mut bound_values);

                list
            }

            ImportKind::Named(list) | ImportKind::TypeOnly(list) => list,
        };

        for item in specs {
            let name = text(item.name).to_string();
            let local = text(item.alias.unwrap_or(item.name)).to_string();
            let (a, b) = range(item.name);
            // The clash is on the name this file binds, which an `as`
            // moves off the exported name.
            let (la, lb) = range(item.alias.unwrap_or(item.name));

            // The module returns a table whose keys the compiler can
            // read: a name in braces takes one key of it.
            let missing_key = match (returns, &keys, type_only || item.is_type) {
                (true, Some(keys), false) => !keys.contains(&name),

                _ => false,
            };

            if alloy_module && !names.contains(&name) {
                out.push(ImportProblem {
                    start: a,
                    end: b,
                    kind: "ImportError",
                    message: format!(
                        "\"{spec}\" does not export `{name}`; it exports {}",
                        and_list(&names)
                    ),
                });
            } else if missing_key {
                out.push(ImportProblem {
                    start: a,
                    end: b,
                    kind: "ImportError",
                    message: format!(
                        "the module \"{spec}\" returns a table with no `{name}`; it has {}",
                        and_list(keys.as_deref().unwrap_or_default())
                    ),
                });
            } else {
                let seen = match type_only || item.is_type {
                    true => &mut bound_types,

                    false => &mut bound_values,
                };

                if seen.contains(&local) {
                    out.push(ImportProblem {
                        start: la,
                        end: lb,
                        kind: "ImportError",
                        message: format!("`{local}` is already imported in this file"),
                    });
                }
            }

            match type_only || item.is_type {
                true => bound_types.push(local),

                false => bound_values.push(local),
            }
        }
    }

    out
}

/// The quoted path of every `import ... from "..."` and `export ... from "..."`.
fn import_specs(source: &str) -> Vec<String> {
    let mut out = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim_start();

        if !(trimmed.starts_with("import ") || trimmed.starts_with("export ")) {
            continue;
        }

        let Some(at) = trimmed.find(" from ") else {
            continue;
        };
        let after = trimmed[at + 6..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let body = &after[1..];

        if let Some(end) = body.find(quote) {
            out.push(body[..end].to_string());
        }
    }

    out
}

/// A path with `.` and `..` folded, no file system access.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}

            std::path::Component::ParentDir => {
                out.pop();
            }

            other => out.push(other),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duplicates(src: &str) -> Vec<String> {
        import_problems(src, Path::new("src/main.aly"), Path::new("/nowhere"), &[])
            .into_iter()
            .filter(|p| p.message.contains("already imported"))
            .map(|p| p.message)
            .collect()
    }

    /// A `.server` or `.client` file is a script: Roblox runs it, and
    /// it returns nothing, so an import of one is an error of its own.
    #[test]
    fn an_import_of_a_script_says_so() {
        let dir = std::env::temp_dir().join(format!("alloy-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(dir.join("src/boot.server.aly"), "print(1)\n").expect("script");
        std::fs::write(dir.join("src/ui.client.luau"), "print(1)\n").expect("script");
        std::fs::write(dir.join("src/util.aly"), "export local a = 1\n").expect("module");
        let from = dir.join("src/main.aly");
        let src = "import b from \"./boot.server\"\nimport u from \"./ui.client\"\nimport { a } from \"./util\"\nprint(b, u, a)\n";
        let problems = import_problems(src, Path::new("src/main.aly"), &from, &[]);
        let messages: Vec<String> = problems.into_iter().map(|p| p.message).collect();
        assert_eq!(
            messages,
            vec![
                crate::typecheck::script_import_message("./boot.server"),
                crate::typecheck::script_import_message("./ui.client"),
            ],
            "{messages:?}"
        );

        assert!(is_script("main.server.aly"));
        assert!(is_script("hud.client.luau"));
        assert!(is_script("./main.server"));
        assert!(!is_script("main.aly"));
        assert!(!is_script("server.aly"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spec resolves by the name on disk, letter for letter. macOS
    /// and Windows would answer `./Item` with `item.aly`, and the
    /// require that the build writes then fails in Roblox.
    #[test]
    fn a_spec_resolves_by_the_exact_file_name() {
        let dir = std::env::temp_dir().join(format!("alloy-case-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(dir.join("src/item.aly"), "export local a = 1\n").expect("module");
        let from = dir.join("src/main.aly");

        assert_eq!(
            resolve("./item", &from, &[]),
            Some(dir.join("src/item.aly"))
        );
        assert_eq!(resolve("./Item", &from, &[]), None);
        assert_eq!(resolve("./ITEM", &from, &[]), None);
        assert!(super::is_file_exact(&dir.join("src/item.aly")));
        assert!(!super::is_file_exact(&dir.join("src/Item.aly")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_type_and_a_value_of_one_name_are_not_a_duplicate_import() {
        let src = "import * as Inv from \"./inv\"\nimport { add, type Inv } from \"./inv\"\nimport type { Item } from \"./inv\"\nprint(add, Inv)\n";
        assert_eq!(duplicates(src), Vec::<String>::new());
    }

    #[test]
    fn one_name_imported_twice_in_one_namespace_is_a_duplicate() {
        let value = "import * as Inv from \"./inv\"\nimport { Inv } from \"./inv\"\nprint(Inv)\n";
        assert_eq!(duplicates(value).len(), 1);

        let ty = "import type { Item } from \"./inv\"\nimport { type Item } from \"./other\"\nprint(1)\n";
        assert_eq!(duplicates(ty).len(), 1);
    }

    #[test]
    fn exported_type_names_come_from_the_declarations() {
        let src = "export struct A as\nend\nexport enum B as C end\nexport interface D as\nend\nexport trait E as\nend\nexport type F<T> = { T }\nexport function g() end\nexport const H = 1\nlocal exported = 1\nexport { exported }\n";
        // A generic alias carries its parameter list, so a re-export
        // passes the parameters on.
        assert_eq!(exported_types(src), vec!["A", "B", "D", "E", "F<T>"]);
    }

    #[test]
    fn trait_defaults_are_the_methods_with_bodies() {
        let src = "export trait Describable as\n    function describe(self): string\n\n    function label(self): string\n        return `[{self:describe()}]`\n    end\nend\ntrait Local as\n    function x(self)\n        return 1\n    end\nend\n";
        assert_eq!(
            exported_trait_defaults(src),
            vec![("Describable".to_string(), vec!["label".to_string()])]
        );
    }

    #[test]
    fn specs_are_read_from_import_lines() {
        let src = "import a from \"./a\"\nimport { b, type C } from '../b'\nexport { d } from \"@shared/d\"\nlocal x = 1\n";
        assert_eq!(import_specs(src), vec!["./a", "../b", "@shared/d"]);
    }

    #[test]
    fn imports_resolve_to_files_and_their_types() {
        let dir = std::env::temp_dir().join(format!("alloy-modules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::write(
            dir.join("shared/types.aly"),
            "export enum Zone as A end\nexport function f() end\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("plain.luau"),
            "export type P = number\nreturn {}\n",
        )
        .unwrap();
        let from = dir.join("main.aly");
        let aliases = vec![("shared".to_string(), dir.join("shared"))];
        let src = "import { Zone, f } from \"@shared/types\"\nimport { type P } from \"./plain\"\nimport x from \"./none\"\n";
        let types = import_types(src, &from, &aliases);
        assert_eq!(
            types,
            vec![
                ("@shared/types".to_string(), vec!["Zone".to_string()]),
                ("./plain".to_string(), vec!["P".to_string()]),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
