//! What a module exports for the compiler's own use: the type names of
//! an imported file, so `import { Zone } from "./types"` brings the
//! type `Zone` in beside the value. Luau binds a value with `local` and
//! a type with `type`; a struct or an enum is both, and the author
//! writes the name once.
//!
//! The index is a line scan of the target file, not a compile: it runs
//! for every import of every file on every edit in the editor.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use alloy_syntax::ast::TokSpan;

use crate::config::{Config, Mount};

/*
The sources an editor holds, which the disk does not carry yet.

Every index below reads the text of a module the file imports, and the
disk has the text of the last save. A reader who edits a module and
looks at an importer would see the module as it was saved, so a
language server puts its open buffers here. A build writes none, and
the map stays empty.
*/
static OPEN_SOURCES: OnceLock<RwLock<HashMap<PathBuf, String>>> = OnceLock::new();

fn open_sources() -> &'static RwLock<HashMap<PathBuf, String>> {
    OPEN_SOURCES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Puts the text an editor holds for a file in front of the disk.
/// `None` takes it away again, and the file reads from the disk.
pub fn set_open_source(path: &Path, text: Option<&str>) {
    let key = normalize(path);
    let Ok(mut open) = open_sources().write() else {
        return;
    };

    match text {
        Some(text) => open.insert(key, text.to_string()),

        None => open.remove(&key),
    };
}

/// The text of one module: the editor's buffer where it holds one,
/// else the file on disk.
fn module_text(path: &Path) -> std::io::Result<String> {
    let held = open_sources()
        .read()
        .ok()
        .and_then(|open| open.get(&normalize(path)).cloned());

    match held {
        Some(text) => Ok(text),

        None => std::fs::read_to_string(path),
    }
}

/// The type names a source exports: `export struct X`, `export enum X`,/// The type names a source exports: `export struct X`, `export enum X`,
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

        // A `type` or an `interface` has no value at run time. The entry
        // ends with `=` so a bare import of it binds the type alone.
        let marker = match kind.as_str() {
            "type" | "interface" => "=",

            _ => "",
        };
        out.push(format!(
            "{name}{}{marker}",
            type_params(&after[name.len()..])
        ));
    }

    out
}

/// The types an `export { ... }` list sends out: a struct, an enum, an
/// interface, or an alias the file declares, and the types an exported
/// namespace carries under the names the emit gives them, `Math_Vec2`.
/// Neither is a line the scan above can read, so this one goes
/// through the parser.
fn exported_namespace_types(source: &str) -> Vec<String> {
    use alloy_syntax::ast::Stmt;

    if !source.contains("namespace") && !source.contains("export") {
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
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
    // Each listed name with the name it goes out under.
    let mut listed: Vec<(String, String)> = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if let Stmt::ExportList(list) = stmt
            && list.from.is_none()
        {
            for spec in &list.specs {
                listed.push((text(spec.name), text(spec.alias.unwrap_or(spec.name))));
            }
        }
    }

    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        // `export default struct P` sends the type out under its own
        // name. The `default` entry says which one a bare import binds.
        if let Stmt::ExportDefault {
            value: alloy_syntax::ast::DefaultExport::Decl(inner),
            ..
        } = stmt
            && let Stmt::Struct(alloy_syntax::ast::StructDecl { name, generics, .. })
            | Stmt::Enum(alloy_syntax::ast::EnumDecl { name, generics, .. }) = inner.as_ref()
        {
            let params = generics
                .map(|g| type_params(text(g).trim_start()))
                .unwrap_or_default();
            out.push(format!("{}{params}", text(*name)));
            out.push(format!("{DEFAULT_ENTRY}{}{params}", text(*name)));

            continue;
        }

        // An entry the line scan wrote already, for `export struct`.
        let (name, generics, exported, marker) = match stmt.under_default() {
            Stmt::Namespace(ns) => {
                let name = text(ns.name);

                if ns.exported || listed.iter().any(|(n, _)| *n == name) {
                    collect_namespace_types(source, toks, ns, &name, &mut out);
                }

                continue;
            }

            Stmt::Struct(d) => (text(d.name), d.generics.map(text), d.exported, ""),

            Stmt::Enum(d) => (text(d.name), d.generics.map(text), d.exported, ""),

            Stmt::Trait(d) => (text(d.name), None, d.exported, ""),

            Stmt::Interface(d) => (text(d.name), d.generics.map(text), d.exported, "="),

            Stmt::TypeAlias(d) => {
                let after = toks[d.name.end as usize - 1].end as usize;

                (
                    text(d.name),
                    Some(source[after..].to_string()),
                    d.exported,
                    "=",
                )
            }

            _ => continue,
        };

        if exported {
            continue;
        }

        let params = generics
            .map(|g| type_params(g.trim_start()))
            .unwrap_or_default();

        for (_, alias) in listed.iter().filter(|(n, _)| *n == name) {
            out.push(format!("{alias}{params}{marker}"));
        }
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

    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();

    for m in &ns.members {
        if m.is_private(source, toks) {
            continue;
        }

        let (name, generics) = match m.stmt.under_default() {
            Stmt::Struct(d) => (text(d.name), d.generics.map(text)),

            Stmt::Enum(d) => (text(d.name), d.generics.map(text)),

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

/// What a type entry opens with when it names the type an `export
/// default` sends out. The space keeps it from reading as a name.
const DEFAULT_ENTRY: &str = "default ";

/// The type entry of a module's `export default struct` or `enum`,
/// among the entries of its types.
pub fn default_type(entries: &[String]) -> Option<&str> {
    entries.iter().find_map(|e| e.strip_prefix(DEFAULT_ENTRY))
}

/// The name a type entry carries, without its parameter list.
pub fn type_head(entry: &str) -> &str {
    entry
        .split('<')
        .next()
        .unwrap_or(entry)
        .trim_end_matches('=')
}

/// The parameter list a type entry carries, `<T>`, or nothing.
pub fn type_args(entry: &str) -> &str {
    entry[type_head(entry).len()..].trim_end_matches('=')
}

/// Whether a type entry names a type with no value at run time.
pub fn type_only(entry: &str) -> bool {
    entry.ends_with('=')
}

/// The traits a source exports with their default methods, the ones
/// with a body. An `impl Trait for S` in another file flattens them in.
pub fn exported_trait_defaults(source: &str) -> Vec<(String, Vec<String>)> {
    exported_traits(source)
        .into_iter()
        .map(|(name, _, defaults)| (name, defaults))
        .collect()
}

/// One exported trait, under one of its names: the name, the methods
/// it leaves to the impl, then its default methods.
type TraitExport = (String, Vec<TraitMethodSig>, Vec<String>);

/// Every trait a source exports, under each name a module that imports
/// it writes.
///
/// A trait inside an exported namespace reads under its path,
/// `Ns.Greet`, and under the name the emit gives it, `Ns_Greet`, the
/// way `declarations` lists a member under both. Without the path an
/// `impl Ns.Greet for S` has no contract to read.
fn exported_traits(source: &str) -> Vec<TraitExport> {
    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        match stmt {
            alloy_syntax::ast::Stmt::Trait(t) if t.exported => {
                push_trait_export(source, toks, t, "", &mut out);
            }

            alloy_syntax::ast::Stmt::Namespace(ns) if ns.exported => {
                namespace_trait_exports(source, toks, ns, "", &mut out);
            }

            _ => {}
        }
    }

    out
}

/// The exported traits of one namespace, for `exported_traits`. A
/// private member leaves the namespace behind, so it is no contract
/// another module can read.
fn namespace_trait_exports(
    source: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    outer: &str,
    out: &mut Vec<TraitExport>,
) {
    let name = ns.name.text(source, toks);
    let path = match outer.is_empty() {
        true => name.to_string(),

        false => format!("{outer}.{name}"),
    };

    for m in &ns.members {
        if m.is_private(source, toks) {
            continue;
        }

        match &m.stmt {
            alloy_syntax::ast::Stmt::Trait(t) => {
                push_trait_export(source, toks, t, &path, out);
            }

            alloy_syntax::ast::Stmt::Namespace(inner) => {
                namespace_trait_exports(source, toks, inner, &path, out);
            }

            _ => {}
        }
    }
}

/// One trait's export entries: its own name under `path`, and the
/// underscore form the emit writes when the path is not empty.
fn push_trait_export(
    source: &str,
    toks: &[alloy_syntax::lexer::Tok],
    t: &alloy_syntax::ast::TraitDecl,
    path: &str,
    out: &mut Vec<TraitExport>,
) {
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
    let required: Vec<TraitMethodSig> = t
        .methods
        .iter()
        .filter(|m| m.body.is_none())
        .map(|m| {
            (
                text(m.name),
                m.params.len(),
                crate::desugar::signature_ret_type(&text(m.signature)).map(str::to_string),
            )
        })
        .collect();
    let defaults: Vec<String> = t
        .methods
        .iter()
        .filter(|m| m.body.is_some())
        .map(|m| text(m.name))
        .collect();
    let name = text(t.name);

    if path.is_empty() {
        out.push((name, required, defaults));

        return;
    }

    out.push((format!("{path}.{name}"), required.clone(), defaults.clone()));
    out.push((
        format!("{}_{name}", path.replace('.', "_")),
        required,
        defaults,
    ));
}

/// One method a trait leaves to the impl: the name, the parameter count
/// with `self` counted, and the return type the signature declares.
pub type TraitMethodSig = (String, usize, Option<String>);

/// Per trait, the methods an impl of it has to write.
pub type TraitRequired = Vec<(String, Vec<TraitMethodSig>)>;

/// The traits a source exports with the methods an impl has to write:
/// the name, the parameter count with `self` counted, and the return
/// type the signature declares. A method with a body is a default, so
/// an impl may leave it out and it stays out of this list.
pub fn exported_trait_methods(source: &str) -> TraitRequired {
    exported_traits(source)
        .into_iter()
        .map(|(name, required, _)| (name, required))
        .collect()
}

/// For each import of a source, the methods the traits the module
/// exports leave to the impl: `(trait, methods)`. An `impl Trait for S`
/// reads them, so a method the trait declares and the impl skips
/// reports wherever the trait is declared.
pub fn import_trait_methods(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> TraitRequired {
    let mut out: TraitRequired = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    for spec in import_specs(source) {
        let Some(path) = resolve(&spec, from, aliases) else {
            continue;
        };

        if seen.contains(&path) {
            continue;
        }

        seen.push(path.clone());

        if let Ok(text) = module_text(&path) {
            for (t, m) in exported_trait_methods(&text) {
                if !m.is_empty() && !out.iter().any(|(n, _)| *n == t) {
                    out.push((t, m));
                }
            }
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

        if let Ok(text) = module_text(&path) {
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

        if let Ok(text) = module_text(&path) {
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

/// One alias the compiler owns. A project that declares it gets the
/// compiler's meaning, never its own, so the declaration is an error.
pub struct ReservedAlias {
    /// The name, without the `@`.
    pub name: &'static str,
    /// What the compiler keeps the name for, for the message.
    pub owns: &'static str,
    /// Whether a Luau configuration may still declare it. `alloy init`
    /// writes `@alloy` into `.luaurc` or `.config.luau`, so that one
    /// belongs there; nothing writes `@game`, which the compiler
    /// answers on its own.
    pub in_luau_config: bool,
}

/// Every alias the compiler owns.
pub const RESERVED_ALIASES: &[ReservedAlias] = &[
    ReservedAlias {
        name: "game",
        owns: "the Roblox services",
        in_luau_config: false,
    },
    ReservedAlias {
        name: "alloy",
        owns: "the runtime",
        in_luau_config: true,
    },
];

/// What a reserved alias reads when a project declares it.
pub fn reserved_alias_message(alias: &ReservedAlias) -> String {
    format!(
        "`{}` is reserved for {}; rename this alias",
        alias.name, alias.owns
    )
}

/// One alias a project declares wrongly: the file it came from, the
/// name, the diagnostic code, and what to say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasProblem {
    pub file: PathBuf,
    pub alias: String,
    pub code: &'static str,
    pub message: String,
}

/// What a `.config.luau` reads when it writes a Luau key at the top
/// level. The file declares nothing there, so every alias in it is
/// absent and the report names the key to move.
pub fn misplaced_key_message(key: &str) -> String {
    format!(
        "`{key}` sits at the top level. Luau reads `.config.luau` under a `luau` table, so it ignores this key. Move the key into `luau = {{ ... }}`."
    )
}

/// What an alias-only `[mount]` entry reads when no mount holds its
/// folder. The build resolves the name and Roblox has no such
/// instance, so the report comes before the build writes anything.
pub fn unmounted_alias_message(name: &str, path: &str) -> String {
    format!(
        "`{path}` sits under no mount. `@{name}` resolves at build time and finds nothing at run time. Mount the folder, or give this entry a place."
    )
}

/// What an alias-only `[mount]` entry reads when its path is no folder.
/// An alias names a folder in every tool that reads one, the editor's
/// child included, so a file or a missing path reports here.
pub fn alias_folder_message(name: &str, path: &str) -> String {
    format!("`{path}` is no folder. An alias names a folder, and `@{name}/x` reads a file in it.")
}

/// Every alias a project declares wrongly, each with the file that
/// declares it. `alloy check` reports these, and so does the editor.
///
/// Two shapes are wrong. A reserved name never reaches the folder it
/// named: the `[mount]` table reserves both names, since a mount serves
/// as an alias, and a Luau configuration reserves `game` alone. An
/// alias-only `[mount]` entry names a folder no mount carries, so the
/// name resolves here and names nothing in the DataModel.
pub fn alias_problems(root: &Path, config: &Config) -> Vec<AliasProblem> {
    let mut out = Vec::new();
    let mut push = |file: PathBuf, alias: &str, code: &'static str, message: String| {
        out.push(AliasProblem {
            file,
            alias: alias.to_string(),
            code,
            message,
        });
    };

    // A `.config.luau` that writes a key above the `luau` table loses
    // it, so the aliases in it reach no file. The parse
    // then falls through to `.luaurc`, which hides the mistake.
    let config_luau = root.join(".config.luau");

    if let Ok(text) = std::fs::read_to_string(&config_luau)
        && let Some(key) = crate::luau_config::misplaced_key(&text)
    {
        push(config_luau, key, "LuauConfig", misplaced_key_message(key));
    }

    if let Some((path, luau)) = crate::luau_config::read_dir(root) {
        for (name, _) in &luau.aliases {
            if let Some(alias) = RESERVED_ALIASES
                .iter()
                .find(|a| a.name == name && !a.in_luau_config)
            {
                push(
                    path.clone(),
                    alias.name,
                    "ReservedAlias",
                    reserved_alias_message(alias),
                );
            }
        }
    }

    for name in config.mount.keys() {
        if let Some(alias) = RESERVED_ALIASES.iter().find(|a| a.name == name) {
            push(
                crate::config::Config::file_of(root),
                alias.name,
                "ReservedAlias",
                reserved_alias_message(alias),
            );
        }
    }

    // An alias-only entry serves a name alone, so the check only
    // matters while the mount names are aliases at all.
    if config.project.mount_aliases && config.mount.values().any(Mount::alias_only) {
        let tree = crate::project::Tree::load(root, config);

        for (name, m) in &config.mount {
            if !m.alias_only() {
                continue;
            }

            let rel = m.0.replace('\\', "/");
            let toml = crate::config::Config::file_of(root);

            if !root.join(&rel).is_dir() {
                push(toml, name, "MountAlias", alias_folder_message(name, &m.0));
            } else if crate::project::instance_path(&tree, Path::new(&rel)).is_none() {
                push(
                    toml,
                    name,
                    "MountAlias",
                    unmounted_alias_message(name, &m.0),
                );
            }
        }
    }

    out
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

/// The file an import spec names from a source file: `./x`, `../x`,
/// `@self/x`, or `@alias/x`, with `.aly`, `.alx`, `.luau`, `.lua`, or an
/// `init` file.
pub fn resolve(spec: &str, from: &Path, aliases: &[(String, PathBuf)]) -> Option<PathBuf> {
    let base = if let Some(tail) = spec.strip_prefix("@self/") {
        // `@self` is the folder of an `init` file. Any other file has no
        // folder of its own, so the spec names no module there.
        if !crate::build::is_init(from) {
            return None;
        }

        from.parent()?.join(tail)
    } else if let Some(rest) = spec.strip_prefix('@') {
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
            .or_insert_with(|| module_types(&path, aliases, BARREL_DEPTH))
            .clone();

        if !types.is_empty() {
            out.push((spec, types));
        }
    }

    out
}

/// How many modules a re-export is followed through. A barrel of a
/// barrel is rare, a cycle of them is a mistake, and the walk reads a
/// file per step.
const BARREL_DEPTH: u8 = 4;

/// The types a module sends out under names of its own: the ones it
/// declares, and the ones it passes on from another module. Where
/// those came from reads from the module's own folder, so the walk
/// takes its path and not just its text.
fn module_types(path: &Path, aliases: &[(String, PathBuf)], depth: u8) -> Vec<String> {
    let Ok(source) = module_text(path) else {
        return Vec::new();
    };
    let mut out = exported_types(&source);

    if depth == 0 {
        return out;
    }

    let mut inner: HashMap<String, Vec<String>> = HashMap::new();
    let mut types_of = |spec: &str| -> Vec<String> {
        inner
            .entry(spec.to_string())
            .or_insert_with(|| {
                resolve(spec, path, aliases)
                    .filter(|target| target != path)
                    .map(|target| module_types(&target, aliases, depth - 1))
                    .unwrap_or_default()
            })
            .clone()
    };
    let (named, stars) = passes(&source);

    // A module passed on whole sends each type out under one flat name,
    // `Leaf_Box`, and the export table holds no value of that name.
    for (exported, spec) in stars {
        for entry in types_of(&spec) {
            out.push(format!(
                "{exported}_{}{}=",
                type_head(&entry),
                type_args(&entry)
            ));
        }
    }

    for (name, exported, spec) in named {
        let types = types_of(&spec);
        // A namespace sends its members on too, `Geo_Vec` as `G_Vec`.
        let members = format!("{name}_");

        for entry in types.iter() {
            let head = type_head(entry);
            let renamed = match head.strip_prefix(&members) {
                Some(rest) => format!("{exported}_{rest}"),

                None if head == name => exported.clone(),

                None => continue,
            };
            let marker = match type_only(entry) {
                true => "=",

                false => "",
            };

            out.push(format!("{renamed}{}{marker}", type_args(entry)));
        }
    }

    out
}

/// What a module passes on from another: the name that module knows it
/// by, the name it goes out under here, and the spec that names it. A
/// barrel writes `export { Point } from "./model"`, or imports `Point`
/// and names it in an `export { ... }` list of its own.
pub(crate) fn reexports(source: &str) -> Vec<(String, String, String)> {
    passes(source).0
}

/// A module a barrel passes on whole: `import * as Leaf from "./leaf"`
/// and then `export { Leaf }`. The name it goes out under, and the spec.
type StarPass = (String, String);

/// `reexports`, and the modules the source passes on whole.
fn passes(source: &str) -> (Vec<(String, String, String)>, Vec<StarPass>) {
    use alloy_syntax::ast::{ImportKind, Stmt};

    if !source.contains("export") {
        return Default::default();
    }

    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(source, options) else {
        return Default::default();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| span.text(source, toks).to_string();
    let bare = |span: TokSpan| text(span).trim_matches(['"', '\'']).to_string();
    // What each import binds here, with the module it came from and
    // the name that module knows it by.
    let mut bound: Vec<(String, String, String)> = Vec::new();

    for i in crate::desugar::imports_in(&parsed.chunk.block) {
        // `*` stands for the whole module the local binds.
        if let ImportKind::Namespace(n, _) = &i.kind {
            bound.push((text(*n), bare(i.path), "*".to_string()));
        }

        let specs = match &i.kind {
            ImportKind::Named(v)
            | ImportKind::TypeOnly(v)
            | ImportKind::Both(_, v)
            | ImportKind::Namespace(_, v) => v,

            ImportKind::Default(_) => continue,
        };

        for sp in specs {
            let name = text(sp.name);
            let local = sp.alias.map(text).unwrap_or_else(|| name.clone());

            bound.push((local, bare(i.path), name));
        }
    }

    let mut out = Vec::new();
    let mut stars = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::ExportList(list) = stmt else {
            continue;
        };

        for sp in &list.specs {
            let name = text(sp.name);
            let exported = text(sp.alias.unwrap_or(sp.name));

            match list.from {
                Some(from) => out.push((name, exported, bare(from))),

                None => match bound.iter().find(|(l, _, _)| *l == name) {
                    Some((_, spec, from_name)) if from_name == "*" => {
                        stars.push((exported, spec.clone()));
                    }

                    Some((_, spec, from_name)) => {
                        out.push((from_name.clone(), exported, spec.clone()));
                    }

                    None => {}
                },
            }
        }
    }

    (out, stars)
}

/// Per import spec, the structs the module declares whose check
/// artifact keeps a private view. An `impl` of one of them types `self`
/// as the view, so its methods reach the private members wherever the
/// impl sits. See `crate::extensions::private_views`.
pub fn import_private_views(
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
        let views = cache
            .entry(path.clone())
            .or_insert_with(|| {
                module_text(&path)
                    .map(|t| crate::extensions::private_views(&t))
                    .unwrap_or_default()
            })
            .clone();

        if !views.is_empty() {
            out.push((spec, views));
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

        if let Ok(text) = module_text(&path) {
            out.extend(crate::declarations::shapes(&text));
        }
    }

    out
}

/// Every `import { Name }` and `import { Name as Local }` of a source:
/// the module the name comes from, the name the module declares, and
/// the name this file binds. Two modules can each declare a `Point`, so
/// an index keyed by the declared name alone holds one of them. The
/// name this file binds is a key of its own, and it names the right
/// module.
pub(crate) fn named_specs(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(PathBuf, String, String)> {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Import(node) = stmt else {
            continue;
        };
        let spec = text(node.path);
        let Some(path) = resolve(spec.trim_matches(['"', '\'']), from, aliases) else {
            continue;
        };

        // A bare import binds the module's `default`, see `sent_decls`.
        if let ImportKind::Default(n) | ImportKind::Both(n, _) = &node.kind {
            out.push((path.clone(), "default".to_string(), text(*n)));
        }

        let specs = match &node.kind {
            ImportKind::Named(list)
            | ImportKind::Both(_, list)
            | ImportKind::Namespace(_, list)
            | ImportKind::TypeOnly(list) => list,

            ImportKind::Default(_) => continue,
        };

        for sp in specs {
            out.push((
                path.clone(),
                text(sp.name),
                text(sp.alias.unwrap_or(sp.name)),
            ));
        }
    }

    out
}

/// The module each `import * as X` binds, with the local name it binds
/// it under. A star import binds the module table, so a declaration of
/// the module reads one level deeper here: `X.State`.
pub(crate) fn star_locals(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(PathBuf, String)> {
    use alloy_syntax::ast::{ImportKind, Stmt};

    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Import(node) = stmt else {
            continue;
        };
        let ImportKind::Namespace(local, _) = &node.kind else {
            continue;
        };
        let spec = node.path.text(source, toks).to_string();
        let Some(path) = resolve(spec.trim_matches(['"', '\'']), from, aliases) else {
            continue;
        };

        out.push((path, local.text(source, toks).to_string()));
    }

    out
}

/// Every declaration of one kind that the modules a source imports
/// make, module by module, in import order. A module reads once.
fn module_decls<T: Clone>(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
    read: impl Fn(&str) -> Vec<(String, T)>,
) -> Vec<(PathBuf, Vec<(String, T)>)> {
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

        let Ok(text) = module_text(&path) else {
            continue;
        };
        let decls = sent_decls(&path, &text, aliases, &read, BARREL_DEPTH);

        out.push((path, decls));
    }

    out
}

/*
The declarations a module sends out, under the names it sends them out
as: its own, the ones a barrel passes on from another module, and its
`export default` declaration once more as `default`.

A namespace member reads under its path, `Geo.Vec`, so a barrel that
passes `Geo` on as `G` passes `G.Vec` too.
*/
fn sent_decls<T: Clone>(
    path: &Path,
    text: &str,
    aliases: &[(String, PathBuf)],
    read: &impl Fn(&str) -> Vec<(String, T)>,
    depth: u8,
) -> Vec<(String, T)> {
    let mut out = read(text);

    if let Some(name) = default_decl(text)
        && let Some((_, payload)) = out.iter().find(|(n, _)| *n == name)
    {
        out.push(("default".to_string(), payload.clone()));
    }

    if depth == 0 {
        return out;
    }

    for (name, exported, spec) in reexports(text) {
        let Some(target) = resolve(&spec, path, aliases).filter(|t| t != path) else {
            continue;
        };
        let Ok(inner) = module_text(&target) else {
            continue;
        };
        let members = format!("{name}.");

        for (decl, payload) in sent_decls(&target, &inner, aliases, read, depth - 1) {
            let renamed = match decl.strip_prefix(&members) {
                Some(rest) => format!("{exported}.{rest}"),

                None if decl == name => exported.clone(),

                None => continue,
            };

            out.push((renamed, payload));
        }
    }

    out
}

/// The name of the declaration a module's `export default` makes: a
/// struct, an enum, or a function.
fn default_decl(source: &str) -> Option<String> {
    use alloy_syntax::ast::{DefaultExport, Stmt};

    if !source.contains("default") {
        return None;
    }

    let parsed = alloy_syntax::parse_lenient(source, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;

    parsed.chunk.block.stmts.iter().find_map(|s| match s {
        Stmt::ExportDefault {
            value: DefaultExport::Decl(inner),
            ..
        } => inner
            .declared_name()
            .map(|n| n.text(source, toks).to_string()),

        _ => None,
    })
}

/// Keys the declarations of the imported modules by every name this
/// file spells them with. Two modules can each declare a `T`, so an
/// index keyed by the declared name alone holds one of them.
///
/// The names this file binds come first: the local of an
/// `import { T as U }`, and `<alias>.<name>` for an `import * as A`,
/// which binds the module table and reads the declaration one level
/// deeper. Each of those names the module it comes from. The declared
/// name comes last, for an index that holds only it.
fn keyed_by_local<T: Clone>(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
    modules: &[(PathBuf, Vec<(String, T)>)],
) -> Vec<(String, T)> {
    let mut out = keyed_by_binding(source, from, aliases, modules);

    // `default` is a key a bare import reads through, not a name.
    for (_, decls) in modules {
        for (name, payload) in decls.iter().filter(|(n, _)| n != "default") {
            if !out.iter().any(|(n, _)| n == name) {
                out.push((name.clone(), payload.clone()));
            }
        }
    }

    out
}

/// `keyed_by_local` with the names this file binds alone, for an index
/// that must not answer for a name the file never imported.
fn keyed_by_binding<T: Clone>(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
    modules: &[(PathBuf, Vec<(String, T)>)],
) -> Vec<(String, T)> {
    let named = named_specs(source, from, aliases);
    let stars = star_locals(source, from, aliases);
    let mut out: Vec<(String, T)> = Vec::new();
    let mut push = |key: String, payload: &T| {
        if !out.iter().any(|(n, _)| *n == key) {
            out.push((key, payload.clone()));
        }
    };

    for (path, decls) in modules {
        for (name, payload) in decls {
            // `Net.Up` is a member of `Net`, so `import { Net as N }`
            // reaches it as `N.Up`.
            let (head, rest) = name
                .find(['.', ':'])
                .map_or((name.as_str(), ""), |at| name.split_at(at));

            for (_, _, local) in named
                .iter()
                .filter(|(p, declared, _)| p == path && declared == head)
            {
                push(format!("{local}{rest}"), payload);
            }

            for (_, local) in stars.iter().filter(|(p, _)| p == path) {
                push(format!("{local}.{name}"), payload);
            }
        }
    }

    out
}

/// The remotes every module a source imports declares, keyed by the
/// name this file binds: whether the client fires each one, and whether
/// the server does. A `.server.aly` file reads it to refuse a fire that
/// only the client can send.
pub fn import_remotes(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, (bool, bool))> {
    let modules = module_decls(source, from, aliases, |text| {
        let Ok(parsed) = alloy_syntax::parse_lenient(text, Default::default()) else {
            return Vec::new();
        };
        let mut out = Vec::new();

        for stmt in &parsed.chunk.block.stmts {
            remote_sides(text, &parsed.lexed.toks, stmt, "", &mut out);
        }

        out
    });

    keyed_by_binding(source, from, aliases, &modules)
}

/// The remote a statement declares, by its path from the top of the
/// file, with whether the client and the server fire it. A remote in
/// `namespace Net` is `Net.Up`, so the side check reads `Net.Up.fire`.
pub(crate) fn remote_sides(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    stmt: &alloy_syntax::ast::Stmt,
    prefix: &str,
    out: &mut Vec<(String, (bool, bool))>,
) {
    use alloy_syntax::ast::Stmt;

    match stmt.under_default() {
        Stmt::Remote(r) => out.push((
            format!("{prefix}{}", r.name.text(src, toks)),
            (r.from_client, r.from_server),
        )),

        Stmt::Namespace(n) => {
            let prefix = format!("{prefix}{}.", n.name.text(src, toks));

            for m in &n.members {
                remote_sides(src, toks, &m.stmt, &prefix, out);
            }
        }

        _ => {}
    }
}

/// The fields of every struct a module the source imports declares,
/// each with whether it carries a default. The construction check reads
/// them, so `new Box { }` on an imported struct names the fields it
/// leaves unset.
pub fn import_struct_fields(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<(String, bool)>)> {
    let modules = module_decls(source, from, aliases, |text| {
        crate::declarations::struct_field_defaults(text)
    });

    keyed_by_local(source, from, aliases, &modules)
}

/// The field types of every struct a module the source imports
/// declares, each with whether this file can write it. A field of `new
/// S { }` then constructs under its declared type, as in the module.
pub fn import_field_types(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<crate::declarations::FieldText>)> {
    let modules = module_decls(source, from, aliases, |text| {
        crate::declarations::struct_field_types(text)
    });

    keyed_by_local(source, from, aliases, &modules)
}

/// The constructor every struct a module the source imports declares
/// writes: the struct's name with its `new` or `New`. The construction
/// check reads it, so a report of `Box(1)` names the constructor the
/// module wrote.
pub fn import_struct_ctors(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, String)> {
    let modules = module_decls(source, from, aliases, |text| {
        crate::declarations::struct_ctors(text)
    });

    keyed_by_local(source, from, aliases, &modules)
}

/// The private fields of every struct a module the source imports
/// declares: the struct's name with its private field names. The
/// `private_access` lint reads a field of an imported struct through it.
pub fn import_privates(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<String>)> {
    // `struct_privates` names a namespace member under its path as
    // well, so `new Zoo.Box { secret = 1 }` finds the shape.
    let modules = module_decls(source, from, aliases, |text| {
        crate::declarations::struct_privates(text)
    });

    keyed_by_local(source, from, aliases, &modules)
}

/// The functions every module the source imports sends out: an
/// `export function` and each method and static of a struct, with the
/// parameter count and the `@deprecated` note. `argument_count` and
/// `deprecated_call` read a call of one through it.
pub fn import_callables(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, crate::flux::Callable)> {
    let modules = module_decls(source, from, aliases, crate::flux::exported_callables);

    keyed_by_local(source, from, aliases, &modules)
}

/// The private fields of the imported structs of a file under the
/// nearest `alloy.toml`.
pub fn import_privates_for_file(path: &Path, source: &str) -> Vec<(String, Vec<String>)> {
    let (from, aliases) = file_context(path);

    import_privates(source, &from, &aliases)
}

/// One enum a module declares: its name, and each variant with how
/// many values it carries.
type ImportedEnum = (String, Vec<(String, usize)>);

/// The import shapes of a file under the nearest `alloy.toml`.
/// The enums every module a source imports declares, each with its
/// variants and how many values they carry. A `match` over an imported
/// enum reads them to prove it covers every variant.
pub fn import_enums(source: &str, from: &Path, aliases: &[(String, PathBuf)]) -> Vec<ImportedEnum> {
    let modules = module_decls(source, from, aliases, |text| {
        crate::declarations::shapes(text)
            .into_iter()
            .filter_map(|shape| match shape {
                crate::declarations::Shape::Enum { name, variants, .. } => Some((
                    name,
                    variants
                        .into_iter()
                        .map(|(v, p)| (v, p.len()))
                        .collect::<Vec<_>>(),
                )),

                _ => None,
            })
            .collect()
    });

    keyed_by_local(source, from, aliases, &modules)
}

/*
The `export attribute` declarations of every module a source imports,
with the targets, the parameters, and the `requires` clauses each one
states.

An attribute contract is checked where the attribute is used, and a use
in this file reaches the declaration through an import. Without this the
check would hold only inside the declaring module.

Each one is keyed as a use spells it: `price` for a named import, and
`M.price` through `import * as M`, so the path fills the defaults and
checks the targets the way the bare name does. A public member of an
exported namespace reads under its path, `M.Ns.tag`. A path whose head
this file does not bind is left out.
*/
pub fn import_attributes(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, crate::desugar::AttrDecl)> {
    let modules = module_decls(source, from, aliases, exported_attribute_decls);
    let bound: HashSet<String> = named_specs(source, from, aliases)
        .into_iter()
        .map(|(_, _, local)| local)
        .chain(
            star_locals(source, from, aliases)
                .into_iter()
                .map(|(_, l)| l),
        )
        .collect();
    let mut out = keyed_by_local(source, from, aliases, &modules);
    out.retain(|(key, _)| {
        key.split_once('.')
            .is_none_or(|(head, _)| bound.contains(head))
    });

    out
}

/// Each star import of an Alloy module: the local, the namespaces the
/// module declares, and every name it exports. The attribute index
/// holds each attribute of the module and of those namespaces, so a
/// path it lacks reports. See `EmitOptions::import_star_modules`.
pub fn import_star_modules(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, Vec<String>, Vec<String>)> {
    star_locals(source, from, aliases)
        .into_iter()
        .filter(|(path, _)| is_alloy(path))
        .map(|(path, local)| {
            let text = module_text(&path).unwrap_or_default();
            let mut namespaces = Vec::new();
            attribute_walk(
                &text,
                &mut Vec::new(),
                &mut namespaces,
                &mut Vec::new(),
                false,
            );

            (local, namespaces, exported_names(&text))
        })
        .collect()
}

/// The `export attribute` declarations of one source, for a file that
/// imports it, with the public attributes of its exported namespaces.
pub fn exported_attribute_decls(src: &str) -> Vec<(String, crate::desugar::AttrDecl)> {
    let mut out = Vec::new();
    attribute_walk(src, &mut out, &mut Vec::new(), &mut Vec::new(), false);

    out
}

/// Every attribute a file can name, by its path: its own declarations,
/// exported or not, and the public members of its namespaces. The
/// editor completes `@Ns.` from it.
pub fn attribute_paths(src: &str) -> Vec<(String, crate::desugar::AttrDecl)> {
    let mut out = Vec::new();
    attribute_walk(src, &mut out, &mut Vec::new(), &mut Vec::new(), true);

    out
}

/// The private attributes of the namespaces the modules a source
/// imports export, by the path this file would write: `Kit.secret`
/// through `import { Kit }`, `P.Kit.secret` through `import * as P`.
/// A use of one reports that it is private, not that it is missing.
pub fn import_private_attributes(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<String> {
    let modules = module_decls(source, from, aliases, |text| {
        let mut private = Vec::new();
        attribute_walk(text, &mut Vec::new(), &mut Vec::new(), &mut private, false);

        private.into_iter().map(|p| (p, ())).collect()
    });
    let bound: HashSet<String> = named_specs(source, from, aliases)
        .into_iter()
        .map(|(_, _, local)| local)
        .chain(
            star_locals(source, from, aliases)
                .into_iter()
                .map(|(_, l)| l),
        )
        .collect();

    keyed_by_local(source, from, aliases, &modules)
        .into_iter()
        .map(|(key, ())| key)
        .filter(|key| {
            key.split_once('.')
                .is_some_and(|(head, _)| bound.contains(head))
        })
        .collect()
}

/// The attributes a module exports, and the path of every namespace
/// the walk reads them from. A top-level declaration counts when it is
/// exported, or always under `every`, and a namespace member when it is
/// not private. `private` gets the path of each private attribute of
/// those namespaces.
fn attribute_walk(
    src: &str,
    out: &mut Vec<(String, crate::desugar::AttrDecl)>,
    namespaces: &mut Vec<String>,
    private: &mut Vec<String>,
    every: bool,
) {
    use alloy_syntax::ast::Stmt;

    fn walk(
        src: &str,
        toks: &[alloy_syntax::lexer::Tok],
        stmt: &Stmt,
        prefix: &str,
        out: &mut Vec<(String, crate::desugar::AttrDecl)>,
        namespaces: &mut Vec<String>,
        private: &mut Vec<String>,
    ) {
        match stmt {
            Stmt::Attribute(a) => out.push((
                format!("{prefix}{}", token_text(src, toks, a.name)),
                attribute_decl(src, toks, a),
            )),

            Stmt::Namespace(ns) => {
                let path = format!("{prefix}{}", token_text(src, toks, ns.name));

                for m in &ns.members {
                    match (m.is_private(src, toks), &m.stmt) {
                        (false, stmt) => walk(
                            src,
                            toks,
                            stmt,
                            &format!("{path}."),
                            out,
                            namespaces,
                            private,
                        ),

                        (true, Stmt::Attribute(a)) => {
                            private.push(format!("{path}.{}", token_text(src, toks, a.name)))
                        }

                        (true, _) => {}
                    }
                }

                namespaces.push(path);
            }

            _ => {}
        }
    }

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return;
    };
    let toks = &parsed.lexed.toks;

    for stmt in &parsed.chunk.block.stmts {
        let exported = match stmt {
            Stmt::Attribute(a) => a.exported,

            Stmt::Namespace(ns) => ns.exported,

            _ => false,
        };

        if exported || every {
            walk(src, toks, stmt, "", out, namespaces, private);
        }
    }
}

/// One `attribute` declaration as the check reads it.
fn attribute_decl(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    a: &alloy_syntax::ast::AttributeDecl,
) -> crate::desugar::AttrDecl {
    crate::desugar::AttrDecl {
        targets: a
            .targets
            .iter()
            .map(|t| token_text(src, toks, *t))
            .collect(),
        params: a
            .params
            .iter()
            .map(|p| {
                (
                    token_text(src, toks, p.name),
                    p.ty.map(|t| span_text(src, toks, t).trim().to_string()),
                )
            })
            .collect(),
        defaults: a
            .params
            .iter()
            .map(|p| p.default.as_ref().map(|d| span_text(src, toks, d.span())))
            .collect(),
        requires: a
            .requires
            .iter()
            .map(|c| require_of(src, toks, c))
            .collect(),
    }
}

/// One `requires` clause of an `attribute`, as the check reads it.
fn require_of(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    c: &alloy_syntax::ast::RequireClause,
) -> crate::desugar::Require {
    let (member, each) = match c.member {
        alloy_syntax::ast::RequireMember::Name(n) => (token_text(src, toks, n), false),

        alloy_syntax::ast::RequireMember::Each(n) => (token_text(src, toks, n), true),
    };

    crate::desugar::Require {
        private: c.visibility.map(|v| token_text(src, toks, v) == "private"),
        kind: token_text(src, toks, c.kind),
        member,
        each,
        shape: c
            .shape
            .map(|s| span_text(src, toks, s).trim().to_string())
            .unwrap_or_default(),
    }
}

/// The text of the first token of a span.
fn token_text(src: &str, toks: &[alloy_syntax::lexer::Tok], span: TokSpan) -> String {
    span.text(src, toks).to_string()
}

/// The macros an import brings in: an `export macro` of the module a
/// name in braces comes from, under the name this file binds.
///
/// A macro is source, not a value, so it travels as text and expands
/// where it is called. The body sees its parameters and the globals of
/// the file it lands in. A name the defining file binds, an import or a
/// local, does not come along, and the Luau checker reports it as an
/// unknown global at the call.
pub fn import_macros(
    source: &str,
    from: &Path,
    aliases: &[(String, PathBuf)],
) -> Vec<crate::MacroSource> {
    use alloy_syntax::ast::{ExportList, ImportKind, Stmt};

    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
    let mut out: Vec<crate::MacroSource> = Vec::new();
    let mut exports: HashMap<PathBuf, Vec<crate::MacroSource>> = HashMap::new();

    for stmt in &parsed.chunk.block.stmts {
        // `export { sq } from "./m"` names the macro too. The barrel
        // then knows `sq` is no value its table can carry.
        let (specs, spec_path) = match stmt {
            Stmt::Import(node) => match &node.kind {
                ImportKind::Named(list)
                | ImportKind::Both(_, list)
                | ImportKind::Namespace(_, list) => (list, node.path),

                ImportKind::Default(_) | ImportKind::TypeOnly(_) => continue,
            },

            Stmt::ExportList(ExportList {
                specs,
                from: Some(from),
                type_only: false,
                ..
            }) => (specs, *from),

            _ => continue,
        };

        if specs.is_empty() {
            continue;
        }

        let spec = text(spec_path);
        let Some(path) = resolve(spec.trim_matches(['"', '\'']), from, aliases) else {
            continue;
        };
        let found = exports
            .entry(path.clone())
            .or_insert_with(|| sent_macros(&path, aliases, BARREL_DEPTH));

        for sp in specs {
            if sp.is_type || sp.is_attribute {
                continue;
            }

            let name = text(sp.name);
            let Some(m) = found.iter().find(|m| m.name == name && !m.hidden) else {
                continue;
            };
            let local = sp.alias.map(&text).unwrap_or(name);

            if !out.iter().any(|had| had.name == local) {
                out.push(crate::MacroSource {
                    name: local,
                    ..m.clone()
                });
            }
        }

        // An exported macro may call a private macro of its own module.
        // The body expands here, so the private one travels with it,
        // under its own name and callable from an expansion alone.
        for m in found.iter().filter(|m| m.hidden) {
            if !out.iter().any(|had| had.name == m.name) {
                out.push(m.clone());
            }
        }
    }

    out
}

/// The macros a module sends out: its own, and the ones a barrel passes
/// on from another module under the name it gives them. A private macro
/// of that module comes along hidden, since an expansion may call it.
fn sent_macros(path: &Path, aliases: &[(String, PathBuf)], depth: u8) -> Vec<crate::MacroSource> {
    let Ok(text) = module_text(path) else {
        return Vec::new();
    };
    let mut out = module_macros(&text);

    if depth == 0 {
        return out;
    }

    for (name, exported, spec) in reexports(&text) {
        let Some(target) = resolve(&spec, path, aliases).filter(|t| t != path) else {
            continue;
        };

        for m in sent_macros(&target, aliases, depth - 1) {
            let sent = match (m.hidden, m.name == name) {
                (false, true) => crate::MacroSource {
                    name: exported.clone(),
                    ..m
                },

                (true, _) => m,

                (false, false) => continue,
            };

            // The barrel imported the macro, so its own list read it
            // hidden; the pass-on is what the importer calls.
            out.retain(|had| had.name != sent.name);
            out.push(sent);
        }
    }

    out
}

/// The `macro` declarations of one source, as the text a nested compile
/// expands. A macro the module does not export reads as hidden: an
/// expansion of the module's own macros can call it, the import list
/// cannot.
pub fn module_macros(source: &str) -> Vec<crate::MacroSource> {
    use alloy_syntax::ast::Stmt;

    let Ok(parsed) = alloy_syntax::parse_lenient(source, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
    // The body and each default are one line of the tokens the
    // declaration wrote, the shape the expander reads. The gap decides
    // the space, so `Choice.Yes` and `f(x)` keep their shape; see
    // `Desugar::join_tokens`.
    let join = |span: alloy_syntax::ast::TokSpan| {
        let mut out = String::new();
        let mut prev_end = None;

        for i in span.start..span.end {
            let tok = toks[i as usize];

            if prev_end.is_some_and(|end| end < tok.start) {
                out.push(' ');
            }

            out.push_str(tok.text(source));
            prev_end = Some(tok.end);
        }

        out
    };
    let mut out = Vec::new();
    // `macro sq(x) ... end` and `export { sq }` below it export it.
    let listed: Vec<String> = parsed
        .chunk
        .block
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::ExportList(list) if list.from.is_none() => Some(list),

            _ => None,
        })
        .flat_map(|list| list.specs.iter().map(|sp| text(sp.name)))
        .collect();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Macro(m) = stmt else {
            continue;
        };

        let named = || m.params.iter().filter(|p| !p.is_vararg);

        out.push(crate::MacroSource {
            name: text(m.name),
            hidden: !m.exported && !listed.contains(&text(m.name)),
            params: named().map(|p| text(p.name)).collect(),
            defaults: named()
                .map(|p| p.default.as_ref().map(|d| join(d.span())))
                .collect(),
            patterns: named()
                .map(|p| crate::desugar::pattern_accesses(p, text))
                .collect(),
            variadic: m.params.iter().any(|p| p.is_vararg),
            body: join(m.body.span),
            tail: m.tail.as_ref().map(|t| join(t.span())),
        });
    }

    out
}

/// The source a span covers, as written.
fn span_text(src: &str, toks: &[alloy_syntax::lexer::Tok], span: TokSpan) -> String {
    span.text_or_empty(src, toks).to_string()
}

/// The imported attribute declarations of a file under the nearest
/// `alloy.toml`.
pub fn import_attributes_for_file(
    path: &Path,
    source: &str,
) -> Vec<(String, crate::desugar::AttrDecl)> {
    let (from, aliases) = file_context(path);

    import_attributes(source, &from, &aliases)
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

        if let Ok(text) = module_text(&target) {
            out.push(text);
        }
    }

    out
}

/// The declarations of every module a file imports, under the names
/// each module sends them out as. A barrel's `export { T } from` reads
/// as the declaration of the module it names.
pub fn import_summaries_for_file(
    path: &Path,
    source: &str,
) -> Vec<crate::declarations::Declaration> {
    let (from, aliases) = file_context(path);

    module_decls(source, &from, &aliases, summary_pairs)
        .into_iter()
        .flat_map(|(_, decls)| decls)
        .map(|(name, d)| crate::declarations::Declaration { name, ..d })
        .collect()
}

/// The declarations the module at `path` sends out, its text given.
/// A name a barrel passes on reads as the module it names declares it.
pub fn sent_summaries(path: &Path, text: &str) -> Vec<crate::declarations::Declaration> {
    let (_, aliases) = file_context(path);

    sent_decls(path, text, &aliases, &summary_pairs, BARREL_DEPTH)
        .into_iter()
        .map(|(name, d)| crate::declarations::Declaration { name, ..d })
        .collect()
}

fn summary_pairs(text: &str) -> Vec<(String, crate::declarations::Declaration)> {
    crate::declarations::summaries(text, false)
        .into_iter()
        .map(|d| (d.name.clone(), d))
        .collect()
}

pub fn import_shapes_for_file(path: &Path, source: &str) -> Vec<crate::declarations::Shape> {
    let (from, aliases) = file_context(path);

    import_shapes(source, &from, &aliases)
}

impl crate::EmitOptions {
    /// Everything the emit reads from the modules a source imports:
    /// types, enums, private fields, attributes, macros, plain modules,
    /// async Results, and trait defaults. Every producer of options
    /// goes through here, so one new index reaches all of them.
    pub fn imports(mut self, source: &str, from: &Path, aliases: &[(String, PathBuf)]) -> Self {
        self.import_types = import_types(source, from, aliases);
        self.import_enums = import_enums(source, from, aliases);
        self.import_remotes = import_remotes(source, from, aliases);
        self.import_privates = import_privates(source, from, aliases);
        self.import_callables = import_callables(source, from, aliases);
        self.import_struct_fields = import_struct_fields(source, from, aliases);
        self.import_field_types = import_field_types(source, from, aliases);
        self.import_struct_ctors = import_struct_ctors(source, from, aliases);
        self.import_private_views = import_private_views(source, from, aliases);
        self.import_attributes = import_attributes(source, from, aliases);
        self.import_private_attributes = import_private_attributes(source, from, aliases);
        self.import_star_modules = import_star_modules(source, from, aliases);
        self.macros = import_macros(source, from, aliases);
        self.plain_modules = plain_modules(source, from, aliases);
        self.import_result_asyncs = import_result_asyncs(source, from, aliases);
        self.import_trait_defaults = import_trait_defaults(source, from, aliases);
        self.import_trait_methods = import_trait_methods(source, from, aliases);

        self
    }

    /// `imports`, with the project read from the nearest `alloy.toml`.
    /// One file compiled on its own reads the structs of the modules it
    /// imports, so an imported struct clones, defaults, and crosses a
    /// remote the way the project build writes it. The file itself
    /// joins them, since its imports say what a name in it means.
    pub fn imports_for_file(self, path: &Path, source: &str) -> Self {
        let (from, aliases, config) = project_context(path);
        // The file and the chain of modules it imports: a type in a
        // remote's layout reads through the imports of each one.
        let mut files = vec![from.clone()];
        let mut next = 0;

        while let Some(file) = files.get(next).cloned() {
            let text = match next {
                0 => source.to_string(),

                _ => module_text(&file).unwrap_or_default(),
            };
            next += 1;

            for target in import_specs(&text)
                .iter()
                .filter_map(|spec| resolve(spec, &file, &aliases))
            {
                if is_alloy(&target) && !files.contains(&target) {
                    files.push(target);
                }
            }
        }

        // The project's `in` folder keys each module as the project
        // build does, so a key here names the table a build registers.
        let base = match &config {
            Some((root, c)) => root.join(&c.build.input),

            None => from.parent().unwrap_or(&from).to_path_buf(),
        };
        let (shapes, wire_scopes) = crate::build::struct_shapes(&files, &base, &aliases);
        let mount_requires = match &config {
            Some((root, c)) => {
                let root = normalize(&std::env::current_dir().unwrap_or_default().join(root));
                let rel = from.strip_prefix(&root).unwrap_or(&from);

                crate::project::mount_requires(&crate::project::Tree::load(&root, c), rel, source)
            }

            None => Vec::new(),
        };

        Self {
            mount_requires,
            std_globals: config.map(|(_, c)| c.std.globals).unwrap_or_default(),
            shapes,
            wire_scopes,
            ..self.imports(source, &from, &aliases)
        }
    }

    /// Every private member of a struct this file does not declare: the
    /// private fields an imported shape carries, and the private methods
    /// of an `impl` of the struct anywhere in the project. The
    /// `private_access` lint reads one list, so the two merge here.
    pub fn privates(&self) -> Vec<(String, Vec<String>)> {
        let mut out = self.import_privates.clone();

        for (target, names) in &self.foreign_privates {
            match out.iter_mut().find(|(t, _)| t == target) {
                Some((_, list)) => {
                    for name in names {
                        if !list.contains(name) {
                            list.push(name.clone());
                        }
                    }
                }

                None => out.push((target.clone(), names.clone())),
            }
        }

        out
    }
}

/// The absolute path of a file and the aliases of its project.
fn file_context(path: &Path) -> (PathBuf, Vec<(String, PathBuf)>) {
    let (from, aliases, _) = project_context(path);

    (from, aliases)
}

/// A project's configuration, with the folder it sits in.
type Project = Option<(PathBuf, Config)>;

/// `file_context`, with the configuration it read and its root.
fn project_context(path: &Path) -> (PathBuf, Vec<(String, PathBuf)>, Project) {
    let dir = path.parent().unwrap_or(Path::new("."));
    let config = Config::find(dir).and_then(|p| {
        let root = p.parent().unwrap_or(dir).to_path_buf();

        Config::load(&p).ok().map(|c| (root, c))
    });
    let aliases = match &config {
        Some((root, config)) => aliases(root, &crate::project::Tree::load(root, config)),

        None => Vec::new(),
    };
    let from = normalize(&std::env::current_dir().unwrap_or_default().join(path));

    (from, aliases, config)
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
                true => module_text(&p).is_ok_and(|t| returns_value(&t)),

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

/// The spec of an import the file already writes whose module exports
/// `name`: where a name the file forgot to import belongs. The checker
/// calls it an unknown global, and the fix is one word in that list.
pub fn import_that_exports(path: &Path, source: &str, name: &str) -> Option<String> {
    let (from, aliases) = file_context(path);
    let parsed = alloy_syntax::parse_lenient(source, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;

    crate::desugar::imports_in(&parsed.chunk.block)
        .into_iter()
        .find_map(|i| {
            let spec = i.path.text(source, toks).trim_matches(['"', '\'']);
            let target = resolve(spec, &from, &aliases)?;
            let text = module_text(&target).ok()?;

            exported_names(&text)
                .iter()
                .any(|n| n == name)
                .then(|| spec.to_string())
        })
}

/// The spec of a module of the project that exports `name`, as the
/// file at `path` would import it: where a name the file never imported
/// lives. The first source in path order answers.
pub fn module_that_exports(path: &Path, name: &str) -> Option<String> {
    let config_path = Config::find(path.parent()?)?;
    let config = Config::load(&config_path).ok()?;
    let root = config_path.parent()?;
    let abs = |p: &Path| normalize(&std::env::current_dir().unwrap_or_default().join(p));
    let input = abs(&root.join(&config.build.input));
    let from = abs(path);
    let written = crate::build::written_dirs(root, &config);
    let target = crate::build::sources(&input, &written)
        .ok()?
        .into_iter()
        .map(|p| abs(&p))
        .filter(|p| *p != from && !p.to_string_lossy().ends_with(".d.aly"))
        .find(|p| {
            std::fs::read_to_string(p)
                .is_ok_and(|text| exported_names(&text).iter().any(|n| n == name))
        })?;

    Some(crate::build::relative_require(
        from.strip_prefix(&input).ok()?,
        &target.strip_prefix(&input).ok()?.with_extension(""),
    ))
}

/// A type or interface that a module exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExport {
    pub name: String,
    pub interface: bool,
    /// The line and UTF-16 column of the name in the module.
    pub at: (u32, u32),
}

/// The file a module resolves to and what it exports, when it exports
/// only types and interfaces, in the order the file writes them. `None`
/// for a module that exports a value, or nothing.
///
/// Such a module emits an empty table, so a binding of it hints `{}`;
/// the editor names the types instead.
pub fn type_only_exports(from: &Path, spec: &str) -> Option<(PathBuf, Vec<TypeExport>)> {
    use alloy_syntax::ast::{Stmt, TokSpan};

    let (from, aliases) = file_context(from);
    let target = resolve(spec, &from, &aliases)?;

    if !target.extension().is_some_and(|e| e == "aly" || e == "alx") {
        return None;
    }

    let source = module_text(&target).ok()?;
    let parsed = alloy_syntax::parse_lenient(&source, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let export = |span: TokSpan, interface: bool| {
        let offset = toks[span.start as usize].start as usize;
        let line_start = source[..offset].rfind('\n').map_or(0, |n| n + 1);

        TypeExport {
            name: span.text(&source, toks).to_string(),
            interface,
            at: (
                source[..offset].matches('\n').count() as u32,
                source[line_start..offset].encode_utf16().count() as u32,
            ),
        }
    };
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        match stmt {
            Stmt::TypeAlias(d) if d.exported => out.push(export(d.name, false)),

            Stmt::Interface(d) if d.exported => out.push(export(d.name, true)),

            // `export type { A }` sends types on; a plain list sends values.
            Stmt::ExportList(list) if list.type_only => {
                out.extend(
                    list.specs
                        .iter()
                        .map(|sp| export(sp.alias.unwrap_or(sp.name), false)),
                );
            }

            Stmt::ExportList(_) | Stmt::ExportDefault { .. } | Stmt::Return(_) => return None,

            Stmt::Local(d) if d.exported => return None,

            Stmt::LocalFunction(d) if d.exported => return None,

            Stmt::Function(d) if d.exported => return None,

            Stmt::Struct(d) if d.exported => return None,

            Stmt::Enum(d) if d.exported => return None,

            Stmt::Trait(d) if d.exported => return None,

            Stmt::Class(d) if d.exported => return None,

            Stmt::Remote(d) if d.exported => return None,

            Stmt::Namespace(d) if d.exported => return None,

            Stmt::Macro(d) if d.exported => return None,

            Stmt::Attribute(d) if d.exported => return None,

            _ => {}
        }
    }

    (!out.is_empty()).then_some((target, out))
}

/// The report for a name the file forgot to import, when an import it
/// already writes reaches a module that exports the name.
pub fn missing_import_message(message: &str, path: &Path, source: &str) -> Option<String> {
    let name = message
        .split("Unknown global '")
        .nth(1)?
        .split('\'')
        .next()?;
    let spec = import_that_exports(path, source, name)?;

    // `import type { Item }` binds the type alone; a derive or a call
    // reads the value, which the type import leaves out.
    let as_type = alloy_syntax::scan::import_statements(source)
        .iter()
        .any(|s| {
            let line = s.text.as_str();
            let words = |from: &str| {
                from.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| w == name)
            };

            (line.starts_with("import type") && words(line.split(" from").next().unwrap_or("")))
                || (line.starts_with("import") && line.contains(&format!("type {name}")))
        });

    if as_type {
        return Some(format!(
            "`{name}` is imported as a type, and this line reads its value, the table its functions live on; import it from \"{spec}\" without `type`"
        ));
    }

    Some(format!(
        "`{name}` is not imported; \"{spec}\" exports it, so add it to that import"
    ))
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
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
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
            Stmt::Attribute(d) if d.exported => out.push(text(d.name)),

            Stmt::Function(d) if d.exported => {
                if let Some(first) = d.path.first() {
                    out.push(text(*first));
                }
            }

            Stmt::Local(d) if d.exported => {
                for name in crate::desugar::statements::local_names(d) {
                    out.push(text(name));
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

/// A module's source as the Alloy parser reads it. A `.alx` file
/// carries markup, which the parser has no reading for, so the markup
/// blanks to text of the same width: every statement around it keeps
/// its place, and a trailing `return` reads as the return it is.
pub fn parsable(source: &str) -> std::borrow::Cow<'_, str> {
    match crate::alx::blank_markup(source) {
        Some((_, text)) => std::borrow::Cow::Owned(text),

        None => std::borrow::Cow::Borrowed(source),
    }
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
    let source = parsable(source);
    let Ok(parsed) = alloy_syntax::parse_lenient(&source, options) else {
        return false;
    };

    matches!(
        parsed.chunk.block.stmts.last(),
        Some(Stmt::Return(r)) if !r.values.is_empty()
    )
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
    let source = parsable(source);
    let parsed = alloy_syntax::parse_lenient(&source, options).ok()?;
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let text = |span: alloy_syntax::ast::TokSpan| span.text(&source, toks).to_string();

    let Some(Stmt::Return(r)) = stmts.last() else {
        return None;
    };

    if r.values.len() != 1 {
        return None;
    }

    match &r.values[0] {
        Expr::Table { fields, .. } => table_keys(fields, toks, &source),

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

                keys = Some(table_keys(fields, toks, &source)?);
            }

            let mut keys = keys?;
            keys.extend(assigned_keys(&source, &name));
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

            crate::declarations::shapes(&source)
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
    let word = |span: alloy_syntax::ast::TokSpan| span.text(source, toks).to_string();
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
    /// The names among them that an `export attribute` declares. An
    /// attribute is written `@name` everywhere it is applied, and the
    /// import list writes it the same way.
    attributes: Vec<String>,
    has_default: bool,
    /// The module ends in `return <expr>` and exports no value.
    returns: bool,
    /// The module returns a value and exports names too, which is an
    /// error: each rule names a different value for the module.
    both: bool,
    keys: Option<Vec<String>>,
    /// The names the module exports as a type with no value at run
    /// time: `export type`, `export interface`. A bare import of one
    /// binds the type alone, so the returned table needs no key for it.
    type_only_names: Vec<String>,
    /// The module does not parse, so its surface says nothing. A single
    /// file's check compiles the importer alone and would report
    /// nothing at all.
    broken: bool,
    /// The module does not parse, and no report names an Alloy keyword.
    /// A plain Luau file may name a local `impl`, which Alloy reserves;
    /// that file still runs, so it is no broken import.
    broken_as_luau: bool,
}

impl Surface {
    fn of(source: &str) -> Self {
        let returns = returns_value(source);
        let both = returns && exports_values(source);
        // A parse that fails outright says nothing about the source, so
        // it counts as broken under both readings.
        let reports: Option<Vec<String>> = alloy_syntax::parse_lenient(source, Default::default())
            .ok()
            .map(|p| p.diagnostics.iter().map(|d| d.message.clone()).collect());

        Surface {
            broken: reports.as_ref().is_none_or(|r| !r.is_empty()),
            broken_as_luau: reports
                .as_ref()
                .is_none_or(|r| r.iter().any(|m| !m.contains("is a reserved word"))),
            names: exported_names(source),
            // An import list names a top-level attribute alone.
            attributes: exported_attribute_decls(source)
                .into_iter()
                .map(|(name, _)| name)
                .filter(|name| !name.contains('.'))
                .collect(),
            has_default: exports_default(source),
            returns: returns && !both,
            both,
            keys: match returns && !both {
                true => returned_keys(source),

                false => None,
            },
            type_only_names: match returns && !both {
                true => exported_types(source)
                    .iter()
                    .filter(|e| type_only(e))
                    .map(|e| type_head(e).to_string())
                    .collect(),

                false => Vec::new(),
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
    let text = |span: alloy_syntax::ast::TokSpan| span.text(source, toks);
    let mut out = Vec::new();
    let mut exports: HashMap<PathBuf, Surface> = HashMap::new();
    // Every name the file binds through an import. A type and a value
    // live in their own namespace, so `import * as M` and `import
    // { type M }` from the same module both bind and neither is a
    // duplicate of the other.
    let mut bound_values: Vec<String> = Vec::new();
    let mut bound_types: Vec<String> = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        // `export { X } from "./m"` reads `X` off the module as an
        // import does, and a name it lacks is nil at run time.
        if let Stmt::ExportList(list) = stmt
            && let Some(path) = list.from
        {
            let spec = text(path).trim_matches(['"', '\'']).to_string();
            let Some(target) = resolve(&spec, from, aliases).filter(|t| is_alloy(t)) else {
                continue;
            };
            let surface = exports
                .entry(target.clone())
                .or_insert_with(|| {
                    module_text(&target)
                        .map(|t| Surface::of(&t))
                        .unwrap_or_default()
                })
                .clone();

            if surface.broken || surface.returns || surface.both {
                continue;
            }

            for item in &list.specs {
                let name = text(item.name).to_string();

                if !surface.names.contains(&name) {
                    let (a, b) = range(item.name);
                    out.push(ImportProblem {
                        start: a,
                        end: b,
                        kind: "ImportError",
                        message: format!(
                            "\"{spec}\" does not export `{name}`; it exports {}",
                            and_list(&surface.names)
                        ),
                    });
                }
            }

            continue;
        }

        let Stmt::Import(node) = stmt else {
            continue;
        };
        let spec = text(node.path).trim_matches(['"', '\'']).to_string();

        // A `.json` or `.toml` import builds a module of its own; the
        // build reports what is wrong with one. The compile reports a
        // std import, which names no file.
        if crate::data::Format::of(&spec).is_some()
            || crate::std_names::module_of_spec(&spec).is_some()
        {
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
                ImportKind::Namespace(n, _) | ImportKind::Default(n) | ImportKind::Both(n, _) => {
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
                    module_text(target)
                        .map(|t| Surface::of(&t))
                        .unwrap_or_default()
                })
                .clone(),

            None => Surface::default(),
        };
        let Surface {
            names,
            attributes,
            has_default,
            returns,
            both,
            keys,
            type_only_names,
            broken,
            broken_as_luau,
        } = surface;
        let name = target
            .as_ref()
            .map(|t| t.to_string_lossy().to_string())
            .unwrap_or_default();
        // A `.d.aly`, a `.d.luau` and a `.d.lua` parse under their own
        // options, so the plain parse says nothing about them.
        let definitions =
            name.ends_with(".d.aly") || name.ends_with(".d.luau") || name.ends_with(".d.lua");
        // A `.luau` or `.lua` file that does not parse gives the import
        // nothing either, and the no-return rule below would name the
        // missing `return` instead of the real fault.
        let plain_luau = name.ends_with(".luau") || name.ends_with(".lua");

        // A module that does not parse exports nothing this file can
        // read, so every check below would report the whole list of its
        // names as missing. The file it names is the one to fix.
        // A `.alx` holds markup, which the plain parser has no reading
        // for. The surface reader takes it apart another way.
        let plain_alloy = alloy_module && name.ends_with(".aly") && !definitions;
        let broken_module = match plain_luau {
            true => broken_as_luau,

            false => broken && plain_alloy,
        };

        if broken_module && !definitions {
            out.push(ImportProblem {
                start: path_start,
                end: path_end,
                kind: "ImportError",
                message: format!("\"{spec}\" does not parse; check it first"),
            });

            continue;
        }

        // Luau takes one value from a module, so a `.luau` or `.lua`
        // file with no `return` gives the import nothing. An
        // `export type` is not a value there. The Luau checker says
        // the same, and `alloy check` has to say it too.
        if plain_luau && !returns {
            out.push(ImportProblem {
                start: path_start,
                end: path_end,
                kind: "UnknownModule",
                message: crate::typecheck::no_module_return_message(&spec, true),
            });

            continue;
        }

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
            // `import * as M`: the alias binds the whole module, so no
            // export name is read and only the local can clash.
            ImportKind::Namespace(name, list) => {
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

                list
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
            // read: a name in braces takes one key of it. A name the
            // module exports as a type alone is the exception: it has
            // no value at run time, so the import binds the type and
            // the table needs no key. The emit does the same.
            let missing_key = match (returns, &keys, type_only || item.is_type) {
                (true, Some(keys), false) => {
                    !keys.contains(&name) && !type_only_names.contains(&name)
                }

                _ => false,
            };

            // An attribute imports by its bare name, or under the `@`
            // it is applied with. A `@` on a name that is no attribute
            // is the part that is wrong.
            let in_type_list = type_only || item.is_type;
            let is_attribute = alloy_module && attributes.contains(&name);
            // The `@` belongs to the report when the sigil is the part
            // that is wrong.
            let (sa, sb) = match item.is_attribute {
                true => (a.saturating_sub(1), b),

                false => (a, b),
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
            } else if is_attribute && in_type_list {
                out.push(ImportProblem {
                    start: sa,
                    end: sb,
                    kind: "ImportError",
                    message: format!(
                        "`{name}` is an attribute, not a type; import it as `@{name}` in a value list"
                    ),
                });
            } else if item.is_attribute && alloy_module && !is_attribute {
                out.push(ImportProblem {
                    start: sa,
                    end: sb,
                    kind: "ImportError",
                    message: format!("`{name}` is not an attribute; import it as `{name}`"),
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
pub(crate) fn import_specs(source: &str) -> Vec<String> {
    alloy_syntax::scan::import_statements(source)
        .into_iter()
        .map(|s| s.spec)
        .collect()
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
    /// A module of types and interfaces lists them apart, in the file's
    /// order; one value in it, or a return, makes it an ordinary module.
    #[test]
    fn a_type_only_module_lists_its_types_and_interfaces() {
        let dir = std::env::temp_dir().join(format!("alloy-type-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("alloy.toml"), "[build]\nin = \"src\"\n").unwrap();
        let write =
            |name: &str, src: &str| std::fs::write(dir.join("src").join(name), src).unwrap();
        write(
            "shapes.aly",
            "export type Id = number\nexport interface Named as\n    name: string\nend\nexport type Label = string\n",
        );
        write(
            "contracts.aly",
            "export interface Named as\n    name: string\nend\n",
        );
        write(
            "mixed.aly",
            "export type Id = number\nexport const FIRST = 1\n",
        );
        write("returns.aly", "export type Id = number\nreturn {}\n");
        let main = dir.join("src/main.aly");
        let exports = |spec: &str| {
            super::type_only_exports(&main, spec).map(|(_, e)| {
                e.into_iter()
                    .map(|e| (e.name, e.interface))
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(
            exports("./shapes"),
            Some(vec![
                ("Id".to_string(), false),
                ("Named".to_string(), true),
                ("Label".to_string(), false)
            ])
        );
        assert_eq!(
            exports("./contracts"),
            Some(vec![("Named".to_string(), true)])
        );
        let (target, shapes) = super::type_only_exports(&main, "./shapes").unwrap();
        assert!(target.ends_with("shapes.aly"), "{}", target.display());
        assert_eq!(shapes[0].at, (0, 12));
        assert_eq!(exports("./mixed"), None);
        assert_eq!(exports("./returns"), None);
        assert_eq!(exports("./missing"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name the file forgot names the import that would bring it in;
    /// a name no import reaches keeps the checker's own words.
    #[test]
    fn a_forgotten_name_names_the_import_that_exports_it() {
        let dir = std::env::temp_dir().join(format!("alloy-missing-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("alloy.toml"), "[build]\nin = \"src\"\n").unwrap();
        std::fs::write(
            dir.join("src/items.aly"),
            "export enum Rarity as\n    Common\nend\nexport struct Item as\n    x: number\nend\n",
        )
        .unwrap();
        let main = dir.join("src/main.aly");
        let source = "import { Item } from \"./items\"\nprint(Item, Rarity.Common)\n";

        assert_eq!(
            super::missing_import_message(
                "Unknown global 'Rarity'; consider assigning to it first",
                &main,
                source,
            )
            .as_deref(),
            Some("`Rarity` is not imported; \"./items\" exports it, so add it to that import")
        );
        assert_eq!(
            super::missing_import_message("Unknown global 'Other'", &main, source),
            None
        );

        // A type import over several lines binds the type alone.
        let source = "import type {\n    Item,\n} from \"./items\"\nprint(Item.new)\n";
        let message = super::missing_import_message("Unknown global 'Item'", &main, source);
        assert!(
            message
                .as_deref()
                .is_some_and(|m| m.starts_with("`Item` is imported as a type")),
            "{message:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn duplicates(src: &str) -> Vec<String> {
        import_problems(src, Path::new("src/main.aly"), Path::new("/nowhere"), &[])
            .into_iter()
            .filter(|p| p.message.contains("already imported"))
            .map(|p| p.message)
            .collect()
    }

    /// A single file's check compiles the importer alone, so a module
    /// that does not parse said nothing: its surface reads empty and
    /// every name it exports goes missing in silence.
    #[test]
    fn an_import_of_a_module_that_does_not_parse_says_so() {
        let dir = std::env::temp_dir().join(format!("alloy-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/widget.aly"),
            "export struct Widget as\n    read label: string\nend\n\nimpl Widget as\n    function new(label: string): Widget\n        return new Widget { label = label\n    end\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let src = "import { Widget } from \"./widget\"\n\nprint(new Widget { label = \"a\" })\n";
        let problems = import_problems(src, Path::new("src/main.aly"), &from, &[]);
        let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

        assert_eq!(
            messages,
            vec!["\"./widget\" does not parse; check it first"]
        );
        assert_eq!(problems[0].kind, "ImportError");
        assert_eq!(crate::docs::kind_for(&problems[0].message), "ImportError");

        // The whole module parses: the import is clean again.
        std::fs::write(
            dir.join("src/widget.aly"),
            "export struct Widget as\n    read label: string\nend\n",
        )
        .expect("module");
        assert!(
            import_problems(src, Path::new("src/main.aly"), &from, &[]).is_empty(),
            "{:?}",
            import_problems(src, Path::new("src/main.aly"), &from, &[])
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A call of an imported function or method reads the count and the
    /// `@deprecated` note the module declares. Luau's solver misses a
    /// call with too many arguments.
    #[test]
    fn an_imported_function_carries_its_arity_and_deprecation() {
        let dir = std::env::temp_dir().join(format!("alloy-callables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/lib.aly"),
            "export function one(x: number): number\n    return x\nend\n\nlocal function hidden(x: number): number\n    return x\nend\n\nexport struct Crate\n    v: number\nend\n\nimpl Crate\n    @deprecated(\"use get\")\n    function value(self): number\n        return self.v + hidden(1)\n    end\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let src = "import { one, Crate } from \"./lib\"\nimport * as M from \"./lib\"\n\nconst a: Crate = new Crate { v = 1 }\nprint(one(1, 2), M.one(1, 2), a:value(3), one(1), a:value())\n";
        let options = crate::EmitOptions::default().imports(src, &from, &[]);
        let out = crate::compile_with(src, &options).expect("compile");
        let messages: Vec<&str> = out
            .lints
            .iter()
            .filter(|l| matches!(l.name, "argument_count" | "deprecated_call"))
            .map(|l| l.message.as_str())
            .collect();

        assert_eq!(
            messages,
            vec![
                "`one` takes 1 argument; this call passes 2",
                "`M.one` takes 1 argument; this call passes 2",
                "`a:value` takes 0 arguments; this call passes 1",
                "`Crate:value` is deprecated; use get",
                "`Crate:value` is deprecated; use get",
            ]
        );
        // A function the module keeps to itself is no key.
        assert!(!options.import_callables.iter().any(|(k, _)| k == "hidden"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name a barrel passes on with `export { T } from` reads as the
    /// declaration of the module it names, under the name the barrel
    /// sends out. The editor hover of an import through a barrel read
    /// only the barrel, and found no enum there.
    #[test]
    fn a_barrel_passes_the_declaration_on() {
        let dir = std::env::temp_dir().join(format!("alloy-barrel-decls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/leaf.aly"),
            "-- The tier of a thing.\nexport enum Tier\n    Low\n    High\nend\n",
        )
        .expect("module");
        let barrel = "export { Tier, Tier as Rank } from \"./leaf\"\n";
        std::fs::write(dir.join("src/barrel.aly"), barrel).expect("module");
        let hover = |decls: &[crate::declarations::Declaration], name: &str| {
            decls
                .iter()
                .find(|d| d.name == name)
                .map(|d| d.hover.clone())
        };

        let sent = sent_summaries(&dir.join("src/barrel.aly"), barrel);
        let tier = hover(&sent, "Tier").expect("the barrel sends Tier on");
        assert!(tier.contains("export enum Tier"), "{tier}");
        assert!(tier.contains("The tier of a thing."), "{tier}");
        assert_eq!(hover(&sent, "Rank"), Some(tier.clone()));

        let src = "import { Tier } from \"./barrel\"\nprint(Tier.Low)\n";
        let imported = import_summaries_for_file(&dir.join("src/use.aly"), src);
        assert_eq!(hover(&imported, "Tier"), Some(tier));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A function in a namespace is `Ns.one` to a call, in its own file
    /// and through a named import, a rename, or a star path. The count
    /// read nothing for it, so `Ns.one(1, 2)` passed.
    #[test]
    fn a_namespace_function_carries_its_arity() {
        let dir = std::env::temp_dir().join(format!("alloy-ns-callables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/lib.aly"),
            "export namespace Stations\n    function menu(f: number): number\n        return f\n    end\n    private function secret(a: number): number\n        return a\n    end\n    namespace Deep\n        function dig(): number\n            return secret(1)\n        end\n    end\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let src = "import { Stations } from \"./lib\"\nimport { Stations as St } from \"./lib\"\nimport * as M from \"./lib\"\n\nnamespace Ns\n    function one(a: number): number\n        return a\n    end\nend\n\nprint(Ns.one(1, 2), Stations.menu(1, 2), St.menu(1, 2), M.Stations.Deep.dig(1), Stations.menu(1))\n";
        let options = crate::EmitOptions::default().imports(src, &from, &[]);
        let out = crate::compile_with(src, &options).expect("compile");
        let messages: Vec<&str> = out
            .lints
            .iter()
            .filter(|l| l.name == "argument_count")
            .map(|l| l.message.as_str())
            .collect();

        assert_eq!(
            messages,
            vec![
                "`Ns.one` takes 1 argument; this call passes 2",
                "`Stations.menu` takes 1 argument; this call passes 2",
                "`St.menu` takes 1 argument; this call passes 2",
                "`M.Stations.Deep.dig` takes 0 arguments; this call passes 1",
            ]
        );
        // A private member is no key another module reads.
        assert!(
            !options
                .import_callables
                .iter()
                .any(|(k, _)| k.ends_with("secret"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two modules each declare a `Point`, and the file imports both
    /// under aliases. The field index keys the local name too, so each
    /// construction reads its own module.
    #[test]
    fn two_modules_of_one_struct_name_stay_apart() {
        let dir = std::env::temp_dir().join(format!("alloy-two-points-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/a.aly"),
            "export struct Point as\n    x: number\nend\n",
        )
        .expect("module");
        std::fs::write(
            dir.join("src/b.aly"),
            "export struct Point as\n    private name: string\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let src =
            "import { Point as PointA } from \"./a\"\nimport { Point as PointB } from \"./b\"\n";
        let fields = import_struct_fields(src, &from, &[]);

        assert_eq!(
            fields
                .iter()
                .find(|(n, _)| n == "PointA")
                .map(|(_, f)| f.clone()),
            Some(vec![("x".to_string(), false)])
        );
        assert_eq!(
            fields
                .iter()
                .find(|(n, _)| n == "PointB")
                .map(|(_, f)| f.clone()),
            Some(vec![("name".to_string(), false)])
        );

        // The private fields key the local name the same way.
        let privates = import_privates(src, &from, &[]);
        assert_eq!(
            privates
                .iter()
                .find(|(n, _)| n == "PointB")
                .map(|(_, f)| f.clone()),
            Some(vec![("name".to_string())])
        );

        // The check then reads the right fields on each side: the
        // construction of one is no report for the other.
        let main = format!(
            "{src}\nlocal a = new PointA {{ x = 1 }}\nlocal b = new PointB {{ name = \"hi\" }}\nprint(a, b)\n"
        );
        let options = crate::EmitOptions::default().imports(&main, &from, &[]);
        let out = crate::compile_with(&main, &options).expect("compile");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        // A field of the other module's `Point` still reports.
        let swapped = format!("{src}\nlocal a = new PointA {{ name = \"hi\" }}\nprint(a)\n");
        let options = crate::EmitOptions::default().imports(&swapped, &from, &[]);
        let out = crate::compile_with(&swapped, &options).expect("compile");
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            messages,
            vec![
                "`new PointA { ... }` leaves `x` unset; a field without a default needs a value",
                "`PointA` has no field `name`; its fields are `x`",
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two modules each declare a `State`, and the file imports both
    /// under aliases. The enum index keys the local name too, so each
    /// match proves its own variants.
    #[test]
    fn two_modules_of_one_enum_name_stay_apart() {
        let dir = std::env::temp_dir().join(format!("alloy-two-states-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/a.aly"),
            "export enum State as\n    On\n    Off\nend\n",
        )
        .expect("module");
        std::fs::write(
            dir.join("src/b.aly"),
            "export enum State as\n    Up(number)\n    Down\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let src = "import { State as S1 } from \"./a\"\nimport { State as S2 } from \"./b\"\n";
        let enums = import_enums(src, &from, &[]);

        assert_eq!(
            enums
                .iter()
                .find(|(n, _)| n == "S1")
                .map(|(_, v)| v.clone()),
            Some(vec![("On".to_string(), 0), ("Off".to_string(), 0)])
        );
        assert_eq!(
            enums
                .iter()
                .find(|(n, _)| n == "S2")
                .map(|(_, v)| v.clone()),
            Some(vec![("Up".to_string(), 1), ("Down".to_string(), 0)])
        );

        // A nested match over both covers every variant, so neither
        // arm reports.
        let main = format!(
            "{src}\nfunction useBoth(a: S1, b: S2): number\n    match a with\n        case S1.On then\n            match b with\n                case S2.Up(n) then return n\n                case S2.Down then return 2\n            end\n        case S1.Off then return 0\n    end\nend\nprint(useBoth(S1.On, S2.Down))\n"
        );
        let options = crate::EmitOptions::default().imports(&main, &from, &[]);
        let out = crate::compile_with(&main, &options).expect("compile");

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        // A missing variant of the aliased enum still reports.
        let short = format!(
            "{src}\nfunction one(b: S2): number\n    match b with\n        case S2.Down then return 2\n    end\nend\nprint(one(S2.Down))\n"
        );
        let options = crate::EmitOptions::default().imports(&short, &from, &[]);
        let out = crate::compile_with(&short, &options).expect("compile");
        let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            messages,
            vec![
                "this match is not exhaustive: `S2` has no arm for `Up`; add it or a `default` arm"
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /*
    Two star imports of two modules that each declare a `State`. The
    star form registers the module table as a namespace, whose enum
    lookup fell back to the declared name, so `B.State` read the first
    module's variants and every arm of the second match reported.

    The enum index keys a star import by `<alias>.<name>`, so each
    alias names the module it stands for.
    */
    #[test]
    fn two_star_aliases_of_one_enum_name_stay_apart() {
        let dir = std::env::temp_dir().join(format!("alloy-star-states-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/a.aly"),
            "export enum State as\n    Idle\n    Running\nend\n",
        )
        .expect("module");
        std::fs::write(
            dir.join("src/b.aly"),
            "export enum State as\n    Off\n    On\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let clean = |src: &str| -> Vec<String> {
            let options = crate::EmitOptions::default().imports(src, &from, &[]);

            crate::compile_with(src, &options)
                .expect("compile")
                .diagnostics
                .into_iter()
                .map(|d| d.message)
                .collect()
        };

        // Two star aliases. Each match covers its own module's enum.
        let src = "import * as A from \"./a\"\nimport * as B from \"./b\"\nfunction a(s: A.State): number\n    match s with\n        case A.State.Idle then return 0\n        case A.State.Running then return 1\n    end\nend\nfunction b(s: B.State): number\n    match s with\n        case B.State.Off then return 0\n        case B.State.On then return 1\n    end\nend\nprint(a(A.State.Idle), b(B.State.On))\n";
        let enums = import_enums(src, &from, &[]);

        assert_eq!(
            enums
                .iter()
                .find(|(n, _)| n == "B.State")
                .map(|(_, v)| v.clone()),
            Some(vec![("Off".to_string(), 0), ("On".to_string(), 0)])
        );
        assert!(clean(src).is_empty(), "{:?}", clean(src));

        // One star alias and one name list. The name this file binds
        // names the module it comes from, whichever module reads first.
        let mixed = "import * as A from \"./a\"\nimport { State } from \"./b\"\nfunction b(s: State): number\n    match s with\n        case State.Off then return 0\n        case State.On then return 1\n    end\nend\nfunction a(s: A.State): number\n    match s with\n        case A.State.Idle then return 0\n        case A.State.Running then return 1\n    end\nend\nprint(a(A.State.Idle), b(State.On))\n";

        assert!(clean(mixed).is_empty(), "{:?}", clean(mixed));

        // A missing variant of the star-imported enum still reports.
        let short = "import * as A from \"./a\"\nimport * as B from \"./b\"\nfunction b(s: B.State): number\n    match s with\n        case B.State.Off then return 0\n    end\nend\nprint(b(B.State.Off))\n";

        assert_eq!(
            clean(short),
            vec![
                "this match is not exhaustive: `B.State` has no arm for `On`; add it or a `default` arm"
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /*
    Two star imports of two modules that each declare a struct `T`.
    The struct index keyed `T` by the declared name alone, so `new B.T`
    read the first module's fields and every field of the second
    reported.

    The index keys a star import by `<alias>.<name>`, the way the enum
    index does, and the construction resolves the alias first.
    */
    #[test]
    fn two_star_aliases_of_one_struct_name_stay_apart() {
        let dir = std::env::temp_dir().join(format!("alloy-star-structs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/a.aly"),
            "export struct T as\n    v: number,\n    private key: number = 0,\nend\n",
        )
        .expect("module");
        std::fs::write(
            dir.join("src/b.aly"),
            "export struct T as\n    name: string,\n    private tag: number = 0,\nend\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let reports = |src: &str| -> Vec<String> {
            let options = crate::EmitOptions::default().imports(src, &from, &[]);
            let out = crate::compile_with(src, &options).expect("compile");

            out.diagnostics
                .into_iter()
                .map(|d| d.message)
                .chain(
                    out.lints
                        .into_iter()
                        .filter(|l| l.name == "private_access")
                        .map(|l| l.message),
                )
                .collect()
        };

        // Two star aliases. Each construction reads its own module.
        let src = "import * as A from \"./a\"\nimport * as B from \"./b\"\nlocal a = new A.T { v = 1 }\nlocal b = new B.T { name = \"x\" }\nprint(a.v, b.name)\n";
        let fields = import_struct_fields(src, &from, &[]);

        assert_eq!(
            fields
                .iter()
                .find(|(n, _)| n == "B.T")
                .map(|(_, f)| f.clone()),
            Some(vec![("name".to_string(), false), ("tag".to_string(), true)])
        );
        assert!(reports(src).is_empty(), "{:?}", reports(src));

        // One star alias and one name list. The name this file binds
        // names the module it comes from.
        let mixed = "import * as A from \"./a\"\nimport { T as BT } from \"./b\"\nlocal a = new A.T { v = 1 }\nlocal b = new BT { name = \"x\" }\nprint(a.v, b.name)\n";

        assert!(reports(mixed).is_empty(), "{:?}", reports(mixed));

        // A private field set through each alias names its own struct.
        let private = "import * as A from \"./a\"\nimport * as B from \"./b\"\nlocal a = new A.T { v = 1, key = 2 }\nlocal b = new B.T { name = \"x\", tag = 3 }\nprint(a.v, b.name)\n";

        assert_eq!(
            reports(private),
            vec![
                "`key` is private to `A.T`; only its impl sets it",
                "`tag` is private to `B.T`; only its impl sets it"
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `.luau` module that does not parse reads as one with no
    /// `return`, since a `return` inside the unclosed body still stands.
    /// The fault is the parse, and the report names it.
    #[test]
    fn an_import_of_a_luau_module_that_does_not_parse_says_so() {
        let dir = std::env::temp_dir().join(format!("alloy-broken-luau-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(
            dir.join("src/helper.luau"),
            "local function f(\n    return 1\n",
        )
        .expect("module");
        let from = dir.join("src/main.aly");
        let src = "import { f } from \"./helper.luau\"\n\nprint(f())\n";
        let problems = import_problems(src, Path::new("src/main.aly"), &from, &[]);
        let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

        assert_eq!(
            messages,
            vec!["\"./helper.luau\" does not parse; check it first"]
        );
        assert_eq!(problems[0].kind, "ImportError");

        // A name Alloy reserves is a name Luau allows, so the module
        // still runs and the import is clean.
        std::fs::write(
            dir.join("src/helper.luau"),
            "local impl = 1\nreturn { f = function() return impl end }\n",
        )
        .expect("module");
        assert!(
            import_problems(src, Path::new("src/main.aly"), &from, &[]).is_empty(),
            "{:?}",
            import_problems(src, Path::new("src/main.aly"), &from, &[])
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(exported_types(src), vec!["A", "B", "D=", "E", "F<T>="]);
        assert_eq!(type_head("F<T>="), "F");
        assert_eq!(type_args("F<T>="), "<T>");
        assert!(type_only("D=") && !type_only("E"));
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
                ("./plain".to_string(), vec!["P=".to_string()]),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The editor holds a module the disk has not taken yet. Every
    /// index reads the module's text, so a file that imports it would
    /// read the last save and miss the edit.
    #[test]
    fn an_open_source_stands_in_front_of_the_disk() {
        let dir = std::env::temp_dir().join(format!("alloy-open-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let module = dir.join("shapes.aly");
        std::fs::write(&module, "export struct Saved as\n    a: number\nend\n").expect("module");

        let main = dir.join("main.aly");
        let src = "import { Saved } from \"./shapes\"\n";
        let names = || -> Vec<String> {
            import_types(src, &main, &[])
                .into_iter()
                .flat_map(|(_, types)| types)
                .collect()
        };

        assert_eq!(names(), vec!["Saved".to_string()]);

        set_open_source(
            &module,
            Some("export struct Edited as\n    a: number\nend\n"),
        );

        assert_eq!(names(), vec!["Edited".to_string()]);

        // The buffer is gone: the disk answers again.
        set_open_source(&module, None);

        assert_eq!(names(), vec!["Saved".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One file compiled on its own reads the chain of modules it
    /// imports, so a remote's layout reaches a type two imports away,
    /// and keys it as the project build does. It read the direct
    /// imports alone, and the enum slot fell to `any`.
    #[test]
    fn one_file_reads_the_import_chain_of_a_layout() {
        let dir = std::env::temp_dir().join(format!("alloy-wire-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/shared")).expect("temp dir");
        let write = |rel: &str, text: &str| std::fs::write(dir.join(rel), text).expect("write");
        write("alloy.toml", "[build]\nin = \"src\"\n");
        write(
            "src/shared/inner.aly",
            "export struct Inner\n    n: number\nend\n",
        );
        write(
            "src/shared/kind.aly",
            "import { Inner } from \"./inner\"\nexport enum Kind\n    Big(Inner)\n    Small\nend\n",
        );
        write("src/other.aly", "struct Inner\n    label: string\nend\n");
        let src = "import * as K from \"./shared/kind\"\nexport remote R1(k: K.Kind) from client\n";
        write("src/net.aly", src);

        let net = dir.join("src/net.aly");
        let options = crate::EmitOptions {
            file_name: net.to_string_lossy().into_owned(),
            ..crate::EmitOptions::default().imports_for_file(&net, src)
        };
        let out = crate::compile_with(src, &options).expect("compiles");

        assert!(
            out.ship.contains("slots = { Big = { { fields = { { \"n\", \"f64\" } }, struct = \"shared/inner.aly:Inner\" } } }"),
            "{}",
            out.ship
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
