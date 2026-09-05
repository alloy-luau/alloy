//! What files export, auto-import completions, and the edits a rename
//! owes to every import that named the old path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Expr, Stmt};
use alloy_syntax::lexer::TokKind;
use serde_json::{Value, json};

/// One name a file exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub is_type: bool,
    /// `export default Name`: imported bare, not in braces.
    pub is_default: bool,
    /// The completion item kind.
    pub kind: u64,
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
    let name_of = |span: alloy_syntax::ast::TokSpan| -> String {
        toks[span.start as usize].text(text).to_string()
    };
    let mut out = Vec::new();
    let mut push = |name: String, is_type: bool, is_default: bool, kind: u64| {
        if !out
            .iter()
            .any(|e: &Export| e.name == name && e.is_type == is_type && e.is_default == is_default)
        {
            out.push(Export {
                name,
                is_type,
                is_default,
                kind,
            });
        }
    };

    for stmt in &parsed.chunk.block.stmts {
        match stmt {
            Stmt::Local(l) if l.exported => {
                for b in &l.names {
                    if b.destructure.is_none() {
                        push(name_of(b.name), false, false, 6);
                    }
                }
            }

            Stmt::LocalFunction(f) if f.exported => push(name_of(f.name), false, false, 3),

            Stmt::Function(f) if f.exported && f.path.len() == 1 => {
                push(name_of(f.path[0]), false, false, 3);
            }

            Stmt::Struct(s) if s.exported => push(name_of(s.name), false, false, 7),

            Stmt::Enum(e) if e.exported => push(name_of(e.name), false, false, 13),

            Stmt::Trait(t) if t.exported => push(name_of(t.name), false, false, 8),

            Stmt::Interface(i) if i.exported => push(name_of(i.name), true, false, 8),

            Stmt::TypeAlias(t) if t.exported => push(name_of(t.name), true, false, 8),

            Stmt::Remote(r) if r.exported => push(name_of(r.name), false, false, 6),

            Stmt::Attribute(a) if a.exported => push(name_of(a.name), false, false, 6),

            Stmt::Macro(m) if m.exported => push(name_of(m.name), false, false, 3),

            Stmt::ExportList(e) if e.from.is_none() => {
                for spec in &e.specs {
                    let name = spec
                        .alias
                        .map(&name_of)
                        .unwrap_or_else(|| name_of(spec.name));
                    push(name, e.type_only || spec.is_type, false, 6);
                }
            }

            Stmt::ExportDefault {
                value: Expr::Name(span),
                ..
            } => push(name_of(*span), false, true, 6),

            _ => {}
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

/// The edit that imports `export` from `spec` into `src`: a new name in
/// an existing `import { ... } from "spec"` line, or a new line.
pub fn import_edit(src: &str, spec: &str, export: &Export) -> Value {
    let item = if export.is_type {
        format!("type {}", export.name)
    } else {
        export.name.clone()
    };

    if !export.is_default {
        for (i, line) in src.lines().enumerate() {
            let t = line.trim();
            let from_spec =
                t.ends_with(&format!("from \"{spec}\"")) || t.ends_with(&format!("from '{spec}'"));

            if !t.starts_with("import {") || !from_spec {
                continue;
            }

            let (Some(open), Some(close)) = (line.find('{'), line.rfind('}')) else {
                continue;
            };

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
    }

    let text = if export.is_default {
        format!("import {} from \"{spec}\"\n", export.name)
    } else {
        format!("import {{ {item} }} from \"{spec}\"\n")
    };
    let line = import_insertion_line(src);

    json!({
        "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 0 } },
        "newText": text,
    })
}

/// Auto-import completion items: every export of another file whose name
/// starts with the word under the cursor and is not bound here.
pub fn auto_import_items(
    src: &str,
    current: &Path,
    files: &[(PathBuf, &[Export])],
    prefix: &str,
    bound: &HashSet<String>,
) -> Vec<Value> {
    let mut items = Vec::new();
    let from_dir = current.parent().unwrap_or(Path::new("."));

    for (path, exports) in files {
        if *path == current {
            continue;
        }

        let spec = relative_spec(from_dir, &module_path(path));

        for export in exports.iter() {
            if !export.name.starts_with(prefix) || bound.contains(&export.name) {
                continue;
            }

            let shape = if export.is_default {
                format!("import {} from \"{spec}\"", export.name)
            } else if export.is_type {
                format!("import {{ type {} }} from \"{spec}\"", export.name)
            } else {
                format!("import {{ {} }} from \"{spec}\"", export.name)
            };

            items.push(json!({
                "label": export.name,
                "kind": export.kind,
                "detail": format!("auto-import: {shape}"),
                "sortText": format!("zz{}", export.name),
                "additionalTextEdits": [import_edit(src, &spec, export)],
            }));
        }
    }

    items
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
            kind: 6,
        };
        let edit = import_edit(src, "./m", &e);
        assert_eq!(edit["newText"], "import { a, b } from \"./m\"");
        let edit = import_edit(src, "./n", &e);
        assert_eq!(edit["newText"], "import { b } from \"./n\"\n");
        assert_eq!(edit["range"]["start"]["line"], 2);
        assert_eq!(import_insertion_line("--!strict\nlocal x = 1\n"), 1);
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
