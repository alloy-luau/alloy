//! What files export, auto-import completions, and the edits a rename
//! owes to every import that named the old path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use alloy::config::QuoteStyle;
use alloy_syntax::ast::{DefaultExport, Expr, Stmt};
use alloy_syntax::lexer::TokKind;
use serde_json::{Value, json};

/// One name a file exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub is_type: bool,
    /// `export default Name`: imported bare, not in braces.
    pub is_default: bool,
    /// `export attribute name`: an import list reads it by its bare
    /// name, or under the `@` it is applied with.
    pub is_attribute: bool,
    /// The completion item kind.
    pub kind: u64,
}

impl Export {
    /// The name as an import list writes it. An attribute reads by its
    /// bare name there; the `@` it is applied with is accepted too.
    pub fn written(&self) -> String {
        self.name.clone()
    }
}

/// The completion kind of a declaration under an `export default`.
fn default_kind(stmt: &Stmt) -> u64 {
    match stmt {
        Stmt::Function(_) | Stmt::LocalFunction(_) | Stmt::Macro(_) => 3,
        Stmt::Struct(_) => 7,
        Stmt::Enum(_) => 13,
        Stmt::Trait(_) | Stmt::Interface(_) | Stmt::TypeAlias(_) => 8,
        Stmt::Class(_) => 7,

        _ => 6,
    }
}

/// Adds an export to a list, unless the same name is already there
/// under the same namespace and the same default flag.
fn push_export(
    out: &mut Vec<Export>,
    name: String,
    is_type: bool,
    is_default: bool,
    is_attribute: bool,
    kind: u64,
) {
    if out
        .iter()
        .any(|e| e.name == name && e.is_type == is_type && e.is_default == is_default)
    {
        return;
    }

    out.push(Export {
        name,
        is_type,
        is_default,
        is_attribute,
        kind,
    });
}

/// The exports of one file, from its top-level statements. Markup is
/// blanked first for an `.alx` file.
pub fn exports_of(src: &str, is_alx: bool) -> Vec<Export> {
    let blanked;
    let text = if is_alx {
        match alloy::luaux::compile::markup_spans(src) {
            Ok(spans) => {
                blanked = alloy::luaux::resolve::blank_luaux_regions(src, &spans);

                blanked.as_str()
            }

            Err(_) => src,
        }
    } else {
        src
    };

    let Ok(parsed) = alloy_syntax::parse_lenient(text, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let name_of = |span: alloy_syntax::ast::TokSpan| span.text(text, toks).to_string();
    let mut out = Vec::new();
    let mut push =
        |name: String, is_type: bool, is_default: bool, is_attribute: bool, kind: u64| {
            push_export(&mut out, name, is_type, is_default, is_attribute, kind);
        };

    for stmt in &parsed.chunk.block.stmts {
        match stmt {
            Stmt::Local(l) if l.exported => {
                for name in alloy::desugar::statements::local_names(l) {
                    push(name_of(name), false, false, false, 6);
                }
            }

            Stmt::LocalFunction(f) if f.exported => push(name_of(f.name), false, false, false, 3),

            Stmt::Function(f) if f.exported && f.path.len() == 1 => {
                push(name_of(f.path[0]), false, false, false, 3);
            }

            Stmt::Struct(s) if s.exported => push(name_of(s.name), false, false, false, 7),

            Stmt::Enum(e) if e.exported => push(name_of(e.name), false, false, false, 13),

            Stmt::Trait(t) if t.exported => push(name_of(t.name), false, false, false, 8),

            Stmt::Interface(i) if i.exported => push(name_of(i.name), true, false, false, 8),

            Stmt::TypeAlias(t) if t.exported => push(name_of(t.name), true, false, false, 8),

            Stmt::Remote(r) if r.exported => push(name_of(r.name), false, false, false, 6),

            // An attribute is a value the module exports, and the `@`
            // is how the list writes it.
            Stmt::Attribute(a) if a.exported => push(name_of(a.name), false, false, true, 6),

            Stmt::Macro(m) if m.exported => push(name_of(m.name), false, false, false, 3),

            // A namespace is a table of members, and markup names one:
            // `import { Widgets }` then `<Widgets.button/>`.
            Stmt::Namespace(n) if n.exported => push(name_of(n.name), false, false, false, 9),

            Stmt::ExportList(e) if e.from.is_none() => {
                for spec in &e.specs {
                    let name = spec
                        .alias
                        .map(&name_of)
                        .unwrap_or_else(|| name_of(spec.name));
                    push(name, e.type_only || spec.is_type, false, false, 6);
                }
            }

            // `export default`: imported bare, under any name. The
            // name here is what an auto-import writes, and the
            // declaration under one binds it in the module too.
            Stmt::ExportDefault { value, .. } => match value {
                DefaultExport::Value(Expr::Name(span)) => {
                    push(name_of(*span), false, true, false, 6)
                }

                DefaultExport::Decl(inner) => {
                    if let Some(name) = inner.declared_name() {
                        push(name_of(name), false, true, false, default_kind(inner));
                    }
                }

                DefaultExport::Value(_) => {}
            },

            _ => {}
        }
    }

    // A plain Luau module exports what it returns: the keys of a table
    // literal, or the members set on the name it returns.
    if let Some(Stmt::Return(r)) = parsed.chunk.block.stmts.last() {
        match r.values.first() {
            Some(Expr::Table { fields, .. }) => {
                for f in fields {
                    if let alloy_syntax::ast::TableField::Named { name, value } = f {
                        let kind = if matches!(value, Expr::Function { .. }) {
                            3
                        } else {
                            6
                        };
                        push(name_of(*name), false, false, false, kind);
                    }
                }
            }

            Some(Expr::Name(span)) => {
                let module = name_of(*span);
                let text_at = |i: usize| toks.get(i).map(|t| t.text(text)).unwrap_or("");

                // The name binds the module, so a bare import reads it.
                push(module.clone(), false, true, false, 6);

                // `local M = { a = 1 }`: the keys the literal opens with.
                for stmt in &parsed.chunk.block.stmts {
                    let Stmt::Local(l) = stmt else {
                        continue;
                    };

                    if l.names.len() != 1 || name_of(l.names[0].name) != module {
                        continue;
                    }

                    if let Some(Expr::Table { fields, .. }) = l.values.first() {
                        for f in fields {
                            if let alloy_syntax::ast::TableField::Named { name, value } = f {
                                let kind = match value {
                                    Expr::Function { .. } => 3,

                                    _ => 6,
                                };
                                push(name_of(*name), false, false, false, kind);
                            }
                        }
                    }
                }

                for i in 0..toks.len() {
                    // `M.key = ...` at the start of a line.
                    if text_at(i) == module
                        && text_at(i + 1) == "."
                        && toks.get(i + 2).is_some_and(|t| t.kind == TokKind::Ident)
                        && text_at(i + 3) == "="
                        && (i == 0
                            || text[toks[i - 1].end as usize..toks[i].start as usize]
                                .contains('\n'))
                    {
                        let kind = if text_at(i + 4) == "function" { 3 } else { 6 };
                        push(text_at(i + 2).to_string(), false, false, false, kind);
                    }

                    // `function M.key(` and `function M:key(`.
                    if text_at(i) == "function"
                        && text_at(i + 1) == module
                        && matches!(text_at(i + 2), "." | ":")
                        && toks.get(i + 3).is_some_and(|t| t.kind == TokKind::Ident)
                    {
                        push(text_at(i + 3).to_string(), false, false, false, 3);
                    }
                }
            }

            _ => {}
        }
    }

    out
}

/// Where a module's `export default` sits: the byte range of the name
/// it binds, else of the `export` word. A default import's binding
/// points here.
pub fn default_span(src: &str, is_alx: bool) -> Option<(u32, u32)> {
    let blanked;
    let text = if is_alx {
        match alloy::luaux::compile::markup_spans(src) {
            Ok(spans) => {
                blanked = alloy::luaux::resolve::blank_luaux_regions(src, &spans);

                blanked.as_str()
            }

            Err(_) => src,
        }
    } else {
        src
    };
    let parsed = alloy_syntax::parse_lenient(text, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let at = |span: alloy_syntax::ast::TokSpan| -> (u32, u32) {
        let t = toks[span.start as usize];

        (t.start, t.end)
    };

    parsed.chunk.block.stmts.iter().find_map(|stmt| match stmt {
        Stmt::ExportDefault { value, span } => Some(match value {
            DefaultExport::Decl(inner) => inner.declared_name().map_or(at(*span), at),

            DefaultExport::Value(Expr::Name(n)) => at(*n),

            DefaultExport::Value(_) => at(*span),
        }),

        _ => None,
    })
}

/// The module a plain file re-exports: `local M = require("./x")` with
/// `return M` at the end. The spec, for the caller to follow.
pub fn follow_of(src: &str) -> Option<String> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let Some(Stmt::Return(r)) = parsed.chunk.block.stmts.last() else {
        return None;
    };
    let Some(Expr::Name(span)) = r.values.first() else {
        return None;
    };
    let module = toks[span.start as usize].text(src);
    let text_at = |i: usize| toks.get(i).map(|t| t.text(src)).unwrap_or("");

    (0..toks.len()).find_map(|i| {
        (text_at(i) == "local"
            && text_at(i + 1) == module
            && text_at(i + 2) == "="
            && text_at(i + 3) == "require"
            && text_at(i + 4) == "(")
            .then(|| text_at(i + 5))
            .filter(|t| t.len() >= 2 && t.starts_with(['"', '\'']))
            .map(|t| t[1..t.len() - 1].to_string())
    })
}

/// The file a module path names on disk: the Alloy or Luau file with
/// that stem, or the `init` file of that directory.
pub fn module_file(target: &Path) -> Option<PathBuf> {
    let stem = target.to_string_lossy().into_owned();

    ["aly", "alx", "luau", "lua"]
        .iter()
        .map(|ext| PathBuf::from(format!("{stem}.{ext}")))
        .chain(
            ["init.aly", "init.alx", "init.luau", "init.lua"]
                .iter()
                .map(|n| target.join(n)),
        )
        .find(|p| p.is_file())
}

/// The exports of a file on disk, following `return M` to the module
/// `M` was required from, a few hops at most.
pub fn exports_of_file(path: &Path, depth: u8) -> Vec<Export> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let is_alx = path.extension().is_some_and(|e| e == "alx");
    let mut out = exports_of(&src, is_alx);

    if depth < 3
        && let Some(spec) = follow_of(&src)
        && let Some(dir) = path.parent()
        && let Some(next) = module_file(&module_path(&lexical(dir, &spec)))
    {
        for e in exports_of_file(&next, depth + 1) {
            if !out
                .iter()
                .any(|o| o.name == e.name && o.is_type == e.is_type)
            {
                out.push(e);
            }
        }
    }

    out
}

/// The module path a spec names: the file without its extension, or the
/// directory of an `init` file.
pub fn module_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = ["d.aly", "aly", "alx", "luau", "lua"]
        .iter()
        .find_map(|ext| name.strip_suffix(&format!(".{ext}")))
        .unwrap_or(&name)
        .to_string();

    if stem == "init" {
        return path.parent().map(Path::to_path_buf).unwrap_or_default();
    }

    path.with_file_name(stem)
}

/// A relative spec from a directory to a module path: `./x` or `../y/x`.
pub fn relative_spec(from_dir: &Path, target: &Path) -> String {
    let a: Vec<_> = from_dir.components().collect();
    let b: Vec<_> = target.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let ups = a.len() - common;
    let rest: Vec<String> = b[common..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    if ups == 0 {
        format!("./{}", rest.join("/"))
    } else {
        format!("{}{}", "../".repeat(ups), rest.join("/"))
    }
}

/// A spec resolved by path arithmetic alone. Nothing asks the disk: after
/// a rename the old path is gone.
pub fn lexical(base_dir: &Path, spec: &str) -> PathBuf {
    let mut out = base_dir.to_path_buf();

    for part in spec.split('/') {
        match part {
            "" | "." => {}

            ".." => {
                out.pop();
            }

            name => out.push(name),
        }
    }

    out
}

/// The line an import lands on: after the last `import` line, else after
/// the hot comments at the top.
/// The names the file's `import` statements bind: `* as M`, a default
/// `Name`, and each `{ a, b as c, type T }` entry, by its bound name.
pub fn bound_names(src: &str) -> Vec<String> {
    let mut out = Vec::new();

    for line in src.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("import ") else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix("type ")
            .map(str::trim_start)
            .unwrap_or(rest);

        if let Some(after_star) = rest.strip_prefix('*') {
            if let Some(name) = after_star.trim_start().strip_prefix("as ") {
                out.push(name.split_whitespace().next().unwrap_or("").to_string());
            }

            continue;
        }

        if let Some(open) = rest.find('{') {
            // `import M, { a }`: the module binds by its name too.
            let head = rest[..open].trim_end();

            if let Some(module) = head.strip_suffix(',') {
                let module = module.trim();

                if !module.is_empty() && module.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    out.push(module.to_string());
                }
            }

            let close = rest[open..]
                .find('}')
                .map(|c| open + c)
                .unwrap_or(rest.len());

            for entry in rest[open + 1..close].split(',') {
                let words: Vec<&str> = entry.split_whitespace().collect();
                let bound = match words.as_slice() {
                    [_, "as", name] | ["type", _, "as", name] => name,
                    ["type", name] | [name] => name,
                    _ => continue,
                };
                out.push((*bound).to_string());
            }

            continue;
        }

        if let Some(name) = rest.split_whitespace().next()
            && rest.contains(" from ")
        {
            out.push(name.to_string());
        }
    }

    out.retain(|n| !n.is_empty());

    out
}

pub fn import_insertion_line(src: &str) -> u32 {
    let mut after_hot = 0u32;
    let mut last_import = None;

    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();

        if t.starts_with("--!") && last_import.is_none() && after_hot == i as u32 {
            after_hot = i as u32 + 1;
        }

        if t.starts_with("import ") {
            last_import = Some(i as u32 + 1);
        }
    }

    last_import.unwrap_or(after_hot)
}

/// The `import { ... } from "spec"` line of a source: its index and its
/// text. A list takes one more name; a default or a star import does
/// not.
fn list_import_line<'a>(src: &'a str, spec: &str) -> Option<(usize, &'a str)> {
    src.lines().enumerate().find(|(_, line)| {
        let t = line.trim();

        t.starts_with("import {")
            && (t.ends_with(&format!("from \"{spec}\"")) || t.ends_with(&format!("from '{spec}'")))
    })
}

/// The quote a generated string takes: the project's `[fmt]
/// quote_style`, over the imports the file already holds.
///
/// A `force` style wins outright, since the formatter rewrites every
/// other quote on its next run. Under the rest the last import of the
/// file decides, so a new line reads like the ones above it; a file
/// with no import takes what the style prefers. `preserve` has no
/// string of its own to preserve and falls back the same way, to the
/// double quote the project default writes.
pub fn quote_for(src: &str, style: QuoteStyle) -> char {
    let preferred = match style {
        QuoteStyle::ForceSingle | QuoteStyle::AutoPreferSingle => '\'',

        _ => '"',
    };

    match style {
        QuoteStyle::ForceDouble | QuoteStyle::ForceSingle => preferred,

        _ => file_quote(src).unwrap_or(preferred),
    }
}

/// The quote of the last import line of a file.
fn file_quote(src: &str) -> Option<char> {
    src.lines()
        .filter(|line| line.trim_start().starts_with("import "))
        .filter_map(|line| {
            line.rfind(" from ")
                .and_then(|at| line[at + " from ".len()..].trim_start().chars().next())
                .filter(|c| *c == '"' || *c == '\'')
        })
        .next_back()
}

/// The import line as the completion detail and the quick fix title
/// write it.
pub fn import_shape(spec: &str, export: &Export, quote: char) -> String {
    let q = quote;

    if export.is_default {
        format!("import {} from {q}{spec}{q}", export.name)
    } else if export.is_type {
        format!("import {{ type {} }} from {q}{spec}{q}", export.name)
    } else {
        format!("import {{ {} }} from {q}{spec}{q}", export.written())
    }
}

/// The edit that imports `export` from `spec` into `src`: a new name in
/// an existing `import { ... } from "spec"` line, which keeps the quotes
/// it has, or a new line in `quote`.
pub fn import_edit(src: &str, spec: &str, export: &Export, quote: char) -> Value {
    let item = if export.is_type {
        format!("type {}", export.name)
    } else {
        export.written()
    };

    if !export.is_default
        && let Some((i, line)) = list_import_line(src, spec)
        && let (Some(open), Some(close)) = (line.find('{'), line.rfind('}'))
    {
        let mut names: Vec<String> = line[open + 1..close]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        names.push(item);
        let new_line = format!(
            "{}{{ {} }}{}",
            &line[..open],
            names.join(", "),
            &line[close + 1..]
        );
        let len = line.chars().map(|c| c.len_utf16() as u32).sum::<u32>();

        return json!({
            "range": { "start": { "line": i, "character": 0 }, "end": { "line": i, "character": len } },
            "newText": new_line,
        });
    }

    let q = quote;
    let text = if export.is_default {
        format!("import {} from {q}{spec}{q}\n", export.name)
    } else {
        format!("import {{ {item} }} from {q}{spec}{q}\n")
    };
    let line = import_insertion_line(src);

    json!({
        "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 0 } },
        "newText": text,
    })
}

/// The edit that binds a whole module under `name`:
/// `import * as Name from "spec"` on a new line. An Alloy module's
/// value is its export table, and a bare name would read its default.
pub fn namespace_import_edit(src: &str, spec: &str, name: &str, quote: char) -> Value {
    let line = import_insertion_line(src);
    let q = quote;

    json!({
        "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 0 } },
        "newText": format!("import * as {name} from {q}{spec}{q}\n"),
    })
}

/// The specs a file already imports, so an auto-import never offers a
/// module the file reads.
pub fn imported_specs(src: &str) -> HashSet<String> {
    let mut out = HashSet::new();

    for line in src.lines() {
        let t = line.trim();

        if !t.starts_with("import ") {
            continue;
        }

        if let Some(at) = t.rfind("from ") {
            let spec = t[at + "from ".len()..].trim();

            if spec.len() >= 2 && spec.starts_with(['"', '\'']) {
                out.insert(spec[1..spec.len() - 1].to_string());
            }
        }
    }

    out
}

/// The spec that names `target` from `from_dir`: the shortest alias
/// path, else the relative one. A target a dot folder holds has no
/// spec: `packages/.ember/x` is a package's own store, not a module the
/// author writes.
pub fn best_spec(from_dir: &Path, target: &Path, aliases: &[(String, PathBuf)]) -> Option<String> {
    if target
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        return None;
    }

    let mut best: Option<String> = None;

    for (name, dir) in aliases {
        let Ok(rest) = target.strip_prefix(dir) else {
            continue;
        };
        let tail = rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let spec = match tail.is_empty() {
            true => format!("@{name}"),

            false => format!("@{name}/{tail}"),
        };

        if best.as_ref().is_none_or(|b| spec.len() < b.len()) {
            best = Some(spec);
        }
    }

    Some(best.unwrap_or_else(|| relative_spec(from_dir, target)))
}

/// Every export of another file whose name starts with `prefix` and
/// is not bound here, with the spec that reaches its module. A module
/// the file already reads offers nothing, unless an `import { ... }`
/// list of it can take one more name.
pub fn auto_import_candidates<'a>(
    src: &str,
    current: &Path,
    files: &[(PathBuf, &'a [Export])],
    prefix: &str,
    bound: &HashSet<String>,
    aliases: &[(String, PathBuf)],
) -> Vec<(String, &'a Export)> {
    let mut out = Vec::new();
    let from_dir = current.parent().unwrap_or(Path::new("."));
    let taken = imported_specs(src);

    for (path, exports) in files {
        if *path == current {
            continue;
        }

        let Some(spec) = best_spec(from_dir, &module_path(path), aliases) else {
            continue;
        };

        if taken.contains(&spec) && list_import_line(src, &spec).is_none() {
            continue;
        }

        for export in exports.iter() {
            if export.name.starts_with(prefix) && !bound.contains(&export.name) {
                out.push((spec.clone(), export));
            }
        }
    }

    out
}

/// Auto-import completion items: every export of another file whose name
/// starts with the word under the cursor and is not bound here.
pub fn auto_import_items(
    src: &str,
    current: &Path,
    files: &[(PathBuf, &[Export])],
    prefix: &str,
    bound: &HashSet<String>,
    aliases: &[(String, PathBuf)],
    quote: char,
) -> Vec<Value> {
    auto_import_candidates(src, current, files, prefix, bound, aliases)
        .into_iter()
        .map(|(spec, export)| {
            json!({
                "label": export.name,
                "kind": export.kind,
                "detail": format!("auto-import: {}", import_shape(&spec, export, quote)),
                "sortText": format!("zz{}", export.name),
                "additionalTextEdits": [import_edit(src, &spec, export, quote)],
            })
        })
        .collect()
}

/// The word ending at a byte offset.
pub fn word_before(src: &str, offset: usize) -> String {
    let offset = offset.min(src.len());
    let start = src[..offset]
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);

    src[start..offset].to_string()
}

/// One rename the editor reported, as paths.
pub struct Rename {
    pub old: PathBuf,
    pub new: PathBuf,
}

/// A path after the renames: a file, a module path without its
/// extension, or anything under a moved folder.
fn map_path(path: &Path, renames: &[Rename]) -> PathBuf {
    for r in renames {
        if path == r.old {
            return r.new.clone();
        }

        if path == module_path(&r.old) {
            return module_path(&r.new);
        }

        if let Ok(rest) = path.strip_prefix(&r.old) {
            return r.new.join(rest);
        }
    }

    path.to_path_buf()
}

/// The edits that keep every relative import and require right after
/// the renames, keyed by the file's new URI. A file that moved has its
/// own specs re-based too.
pub fn rename_edits(
    docs: &[(String, PathBuf, String)],
    renames: &[Rename],
) -> HashMap<String, Vec<Value>> {
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();

    for (uri, old_path, src) in docs {
        let new_path = map_path(old_path, renames);
        let old_dir = old_path.parent().unwrap_or(Path::new("."));
        let new_dir = new_path.parent().unwrap_or(Path::new("."));
        let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
            continue;
        };
        let toks = &lexed.toks;
        let mut edits = Vec::new();

        for (i, tok) in toks.iter().enumerate() {
            let TokKind::Str {
                inner_start,
                inner_end,
            } = tok.kind
            else {
                continue;
            };
            let prev = i.checked_sub(1).map(|j| toks[j].text(src)).unwrap_or("");
            let prev2 = i.checked_sub(2).map(|j| toks[j].text(src)).unwrap_or("");
            let is_spec = prev == "from" || (prev == "(" && prev2 == "require");

            if !is_spec {
                continue;
            }

            let spec = &src[inner_start as usize..inner_end as usize];

            if !(spec.starts_with("./") || spec.starts_with("../")) {
                continue;
            }

            let target = map_path(&lexical(old_dir, spec), renames);
            let new_spec = relative_spec(new_dir, &target);

            if new_spec == spec {
                continue;
            }

            let (sl, sc) = crate::doc::position_of(src, inner_start as usize);
            let (el, ec) = crate::doc::position_of(src, inner_end as usize);
            edits.push(json!({
                "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
                "newText": new_spec,
            }));
        }

        if !edits.is_empty() {
            let _ = uri;
            out.insert(crate::proxy::path_to_uri(&new_path), edits);
        }
    }

    out
}

// --- the Roblox services -------------------------------------------------

/// The `(local, service)` pairs one import line binds, when its path
/// names services. `import { RunService as Run } from "@game"` binds
/// `Run` to `RunService`; `import P from "@game/Players"` binds `P` to
/// `Players`.
pub fn service_bindings(line: &str) -> Vec<(String, String)> {
    use alloy::game_import::GamePath;

    let text = line.trim();
    let Some(rest) = text.strip_prefix("import ") else {
        return Vec::new();
    };
    let Some(game) = spec_of(text)
        .as_deref()
        .and_then(alloy::game_import::game_path)
    else {
        return Vec::new();
    };
    let head = rest.split(" from ").next().unwrap_or("").trim();
    let head = head.strip_prefix("type ").unwrap_or(head).trim_start();
    let mut out = Vec::new();
    let mut bind = |local: &str, name: &str| {
        if local.is_empty() {
            return;
        }

        out.push(match &game {
            GamePath::Every => (local.to_string(), name.to_string()),

            GamePath::One(service) => (local.to_string(), service.clone()),
        });
    };

    match head.find('{') {
        Some(open) => {
            let close = head.rfind('}').unwrap_or(head.len());

            for entry in head[open + 1..close].split(',') {
                let words: Vec<&str> = entry.split_whitespace().collect();

                match words.as_slice() {
                    [name, "as", local] | ["type", name, "as", local] => bind(local, name),
                    [name] | ["type", name] => bind(name, name),
                    _ => {}
                }
            }
        }

        // `import Players from "@game/Players"`, and `import * as P`,
        // which is no form the path takes but still binds a name.
        None => {
            let name = head
                .trim_start_matches('*')
                .trim_start()
                .strip_prefix("as ")
                .unwrap_or(head)
                .split_whitespace()
                .next()
                .unwrap_or("");
            bind(name, name);
        }
    }

    out
}

/// Every service a file already imports, so no auto-import offers one
/// twice and neither form is added beside the other.
pub fn imported_services(src: &str) -> HashSet<String> {
    src.lines()
        .flat_map(service_bindings)
        .map(|(_, service)| service)
        .collect()
}

/// The edit that imports a Roblox service. A file that already reads
/// `import { ... } from "@game"` takes the name into those braces; any
/// other file takes a line of its own. A file still on the old
/// spelling keeps it, so the edit adds no second list beside the one
/// the file has.
pub fn service_import_edit(src: &str, service: &str, quote: char) -> Value {
    let list_spec = src.lines().find_map(|line| {
        let t = line.trim();

        if !t.starts_with("import {") {
            return None;
        }

        ["@game", "game"].into_iter().find(|spec| {
            t.ends_with(&format!("from \"{spec}\"")) || t.ends_with(&format!("from '{spec}'"))
        })
    });

    if let Some(spec) = list_spec {
        return import_edit(
            src,
            spec,
            &Export {
                name: service.to_string(),
                is_type: false,
                is_default: false,
                is_attribute: false,
                kind: 9,
            },
            quote,
        );
    }

    let line = import_insertion_line(src);

    json!({
        "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 0 } },
        "newText": format!(
            "import {service} from {quote}{}/{service}{quote}\n",
            alloy::game_import::ALIAS
        ),
    })
}

/// The path an import line names, in either quote.
fn spec_of(line: &str) -> Option<String> {
    let at = line.rfind(" from ")? + " from ".len();
    let rest = line[at..].trim();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &rest[quote.len_utf8()..];

    Some(body[..body.find(quote)?].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_names_cover_every_import_form() {
        let src = "import * as Signal from \"@packages/Signal\"\nimport Panel from \"./ui\"\nimport { add, total as sum, type Item } from \"./inv\"\nimport type { Patch } from \"./t\"\nlocal x = 1\n";
        assert_eq!(
            bound_names(src),
            ["Signal", "Panel", "add", "sum", "Item", "Patch"]
        );
    }

    #[test]
    fn exports_come_from_every_declaration_form() {
        let src = "export const A = 1\nexport function f() end\nexport struct S as end\nexport type T = number\nlocal b = 2\nexport { b as c }\nexport default f\n";
        let names: Vec<(String, bool, bool)> = exports_of(src, false)
            .into_iter()
            .map(|e| (e.name, e.is_type, e.is_default))
            .collect();
        assert!(names.contains(&("A".into(), false, false)));
        assert!(names.contains(&("f".into(), false, false)));
        assert!(names.contains(&("S".into(), false, false)));
        assert!(names.contains(&("T".into(), true, false)));
        assert!(names.contains(&("c".into(), false, false)));
        assert!(names.contains(&("f".into(), false, true)), "{names:?}");
    }

    /// A namespace holds components, so markup in another file names
    /// it: `import { Widgets }` then `<Widgets.button/>`.
    #[test]
    fn an_exported_namespace_is_an_export() {
        let src = "export namespace Widgets as
    function button() end
end
namespace Inner as end
";
        let names: Vec<String> = exports_of(src, false).into_iter().map(|e| e.name).collect();

        assert_eq!(names, ["Widgets"]);
    }

    #[test]
    fn a_declaration_under_export_default_is_the_default() {
        let src = "export function keep() end\nexport default struct Theme as\n    primary: string\nend\n";
        let exports: Vec<(String, bool, u64)> = exports_of(src, false)
            .into_iter()
            .map(|e| (e.name, e.is_default, e.kind))
            .collect();
        assert!(exports.contains(&("keep".into(), false, 3)), "{exports:?}");
        assert!(exports.contains(&("Theme".into(), true, 7)), "{exports:?}");
    }

    /// A module that ends in `return <expr>` sends the value out. Its
    /// keys read in braces, and the name it returns is the default a
    /// bare import binds.
    #[test]
    fn a_returning_module_sends_out_its_value() {
        let src = "local Palette = { dark = \"#111111\" }\n\nfunction Palette.tint(hex: string): string\n    return hex\nend\n\nPalette.light = \"#eeeeee\"\n\nreturn Palette\n";
        let exports: Vec<(String, bool, u64)> = exports_of(src, false)
            .into_iter()
            .map(|e| (e.name, e.is_default, e.kind))
            .collect();
        assert!(
            exports.contains(&("Palette".into(), true, 6)),
            "{exports:?}"
        );
        assert!(exports.contains(&("dark".into(), false, 6)), "{exports:?}");
        assert!(exports.contains(&("light".into(), false, 6)), "{exports:?}");
        assert!(exports.contains(&("tint".into(), false, 3)), "{exports:?}");
    }

    #[test]
    fn a_whole_module_import_binds_under_a_name() {
        let src = "--!strict\nlocal x = 1\n";
        let edit = namespace_import_edit(src, "./ui/panel", "Panel", '"');
        assert_eq!(edit["newText"], "import * as Panel from \"./ui/panel\"\n");
    }

    #[test]
    fn specs_are_relative_and_init_folds() {
        assert_eq!(relative_spec(Path::new("/w/a"), Path::new("/w/a/b")), "./b");
        assert_eq!(
            relative_spec(Path::new("/w/a/c"), Path::new("/w/x/y")),
            "../../x/y"
        );
        assert_eq!(
            module_path(Path::new("/w/m/init.aly")),
            PathBuf::from("/w/m")
        );
        assert_eq!(
            module_path(Path::new("/w/m/a.d.aly")),
            PathBuf::from("/w/m/a")
        );
        assert_eq!(
            lexical(Path::new("/w/a"), "../b/c"),
            PathBuf::from("/w/b/c")
        );
    }

    #[test]
    fn an_import_joins_an_existing_line_or_opens_one() {
        let src = "--!strict\nimport { a } from \"./m\"\nlocal x = 1\n";
        let e = Export {
            name: "b".into(),
            is_type: false,
            is_default: false,
            is_attribute: false,
            kind: 6,
        };
        let edit = import_edit(src, "./m", &e, '"');
        assert_eq!(edit["newText"], "import { a, b } from \"./m\"");
        let edit = import_edit(src, "./n", &e, '"');
        assert_eq!(edit["newText"], "import { b } from \"./n\"\n");
        assert_eq!(edit["range"]["start"]["line"], 2);
        assert_eq!(import_insertion_line("--!strict\nlocal x = 1\n"), 1);
    }

    /// The project's `quote_style` writes a generated import, and the
    /// imports the file already holds win under every style but a
    /// forced one.
    #[test]
    fn a_generated_import_takes_the_projects_quote() {
        let bare = "--!strict\nlocal x = 1\n";
        let single = "import { a } from './m'\nlocal x = 1\n";

        assert_eq!(quote_for(bare, QuoteStyle::AutoPreferDouble), '"');
        assert_eq!(quote_for(bare, QuoteStyle::AutoPreferSingle), '\'');
        assert_eq!(quote_for(bare, QuoteStyle::Preserve), '"');
        assert_eq!(quote_for(bare, QuoteStyle::ForceSingle), '\'');
        assert_eq!(quote_for(bare, QuoteStyle::ForceDouble), '"');

        // The file already answers the question a `force` style does
        // not settle.
        assert_eq!(quote_for(single, QuoteStyle::AutoPreferDouble), '\'');
        assert_eq!(quote_for(single, QuoteStyle::Preserve), '\'');
        assert_eq!(quote_for(single, QuoteStyle::ForceDouble), '"');

        let e = Export {
            name: "b".into(),
            is_type: false,
            is_default: false,
            is_attribute: false,
            kind: 6,
        };

        assert_eq!(
            import_edit(bare, "./n", &e, '\'')["newText"],
            "import { b } from './n'\n"
        );
        assert_eq!(import_shape("./n", &e, '\''), "import { b } from './n'");
        assert_eq!(
            namespace_import_edit(bare, "./n", "N", '\'')["newText"],
            "import * as N from './n'\n"
        );
        assert_eq!(
            service_import_edit(bare, "Players", '\'')["newText"],
            "import Players from '@game/Players'\n"
        );

        // A name joins a list that is already there, and the line keeps
        // the quotes it wrote.
        assert_eq!(
            import_edit(single, "./m", &e, '"')["newText"],
            "import { a, b } from './m'"
        );
    }

    #[test]
    fn a_rename_rewrites_both_sides() {
        let docs = vec![
            (
                "file:///w/a.aly".to_string(),
                PathBuf::from("/w/a.aly"),
                "import { x } from \"./lib/b\"\n".to_string(),
            ),
            (
                "file:///w/lib/b.aly".to_string(),
                PathBuf::from("/w/lib/b.aly"),
                "local a = require(\"../a\")\n".to_string(),
            ),
        ];
        let renames = vec![Rename {
            old: PathBuf::from("/w/lib/b.aly"),
            new: PathBuf::from("/w/src/deep/b.aly"),
        }];
        let edits = rename_edits(&docs, &renames);
        assert_eq!(
            edits["file:///w/a.aly"][0]["newText"], "./src/deep/b",
            "{edits:?}"
        );
        assert_eq!(
            edits["file:///w/src/deep/b.aly"][0]["newText"], "../../a",
            "{edits:?}"
        );
        assert_eq!(edits.len(), 2);
    }
}
