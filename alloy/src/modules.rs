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
pub fn exported_types(source: &str) -> Vec<String> {
    let mut out = Vec::new();

    for line in source.lines() {
        let rest = line.trim_start();
        let Some(rest) = rest.strip_prefix("export") else {
            continue;
        };

        if !rest.starts_with(char::is_whitespace) {
            continue;
        }

        let mut words = rest.split_whitespace();
        let Some(kind) = words.next() else { continue };

        if !matches!(
            kind,
            "struct" | "enum" | "interface" | "trait" | "type" | "class"
        ) {
            continue;
        }

        if let Some(name) = words.next() {
            let name: String = name
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) {
                out.push(name);
            }
        }
    }

    out
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
            Ok(config) => aliases(config_path.parent().unwrap_or(dir), &config),

            Err(_) => Vec::new(),
        },

        None => Vec::new(),
    };
    let from = normalize(&std::env::current_dir().unwrap_or_default().join(path));

    import_trait_defaults(source, &from, &aliases)
}

/// The alias table of a project: the mounts, then `.luaurc`, each to an
/// absolute folder.
pub fn aliases(root: &Path, config: &Config) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = config
        .mount
        .iter()
        .map(|(a, m)| (a.clone(), normalize(&root.join(&m.0))))
        .collect();

    if let Some((_, luau)) = crate::luau_config::read_dir(root) {
        for (alias, target) in luau.aliases {
            if !out.iter().any(|(a, _)| *a == alias) {
                out.push((alias, normalize(&root.join(target))));
            }
        }
    }

    out
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

    if base.is_file() {
        return Some(base);
    }

    for ext in ["aly", "alx", "luau", "lua"] {
        let candidate = base.with_extension(ext);

        if candidate.is_file() {
            return Some(candidate);
        }

        let init = base.join(format!("init.{ext}"));

        if init.is_file() {
            return Some(init);
        }
    }

    None
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
        let crate::declarations::Shape::Struct { name, fields } = shape else {
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
pub fn import_shapes_for_file(path: &Path, source: &str) -> Vec<crate::declarations::Shape> {
    let (from, aliases) = file_context(path);

    import_shapes(source, &from, &aliases)
}

/// The absolute path of a file and the aliases of its project.
fn file_context(path: &Path) -> (PathBuf, Vec<(String, PathBuf)>) {
    let dir = path.parent().unwrap_or(Path::new("."));
    let aliases = match Config::find(dir) {
        Some(config_path) => match Config::load(&config_path) {
            Ok(config) => aliases(config_path.parent().unwrap_or(dir), &config),

            Err(_) => Vec::new(),
        },

        None => Vec::new(),
    };
    let from = normalize(&std::env::current_dir().unwrap_or_default().join(path));

    (from, aliases)
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
    let mut exports: HashMap<PathBuf, Vec<String>> = HashMap::new();
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

        let (path_start, path_end) = range(node.path);
        // A module that names no file is reported, and its names still
        // bind here, so a second import of one is a duplicate.
        let target = resolve(&spec, from, aliases);

        if target.is_none() {
            out.push(ImportProblem {
                start: path_start,
                end: path_end,
                kind: "UnknownModule",
                message: crate::typecheck::unknown_module_message(&spec, rel),
            });
        }
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

            ImportKind::Both(name, list) => {
                bound_values.push(text(*name).to_string());

                list
            }

            ImportKind::Named(list) | ImportKind::TypeOnly(list) => list,
        };
        let type_only = matches!(&node.kind, ImportKind::TypeOnly(_));
        // A plain Luau module returns a table; its keys are not
        // declarations, so only an Alloy module's names are checked.
        let alloy_module = target
            .as_ref()
            .is_some_and(|t| t.extension().is_some_and(|e| e == "aly" || e == "alx"));
        let names = match &target {
            Some(target) => exports
                .entry(target.clone())
                .or_insert_with(|| {
                    std::fs::read_to_string(target)
                        .map(|t| exported_names(&t))
                        .unwrap_or_default()
                })
                .clone(),

            None => Vec::new(),
        };

        for item in specs {
            let name = text(item.name).to_string();
            let local = text(item.alias.unwrap_or(item.name)).to_string();
            let (a, b) = range(item.name);

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
            } else {
                let seen = match type_only || item.is_type {
                    true => &mut bound_types,

                    false => &mut bound_values,
                };

                if seen.contains(&local) {
                    out.push(ImportProblem {
                        start: a,
                        end: b,
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
        let src = "export struct A as\nend\nexport enum B as C end\nexport interface D as\nend\nexport trait E\nend\nexport type F<T> = { T }\nexport function g() end\nexport const H = 1\nlocal exported = 1\nexport { exported }\n";
        assert_eq!(exported_types(src), vec!["A", "B", "D", "E", "F"]);
    }

    #[test]
    fn trait_defaults_are_the_methods_with_bodies() {
        let src = "export trait Describable\n    function describe(self): string\n\n    function label(self): string\n        return `[{self:describe()}]`\n    end\nend\ntrait Local\n    function x(self)\n        return 1\n    end\nend\n";
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
