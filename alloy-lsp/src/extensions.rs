//! Extension methods on foreign types reach the analyzer through the
//! definitions. At startup the server reads every `.aly` under the root,
//! collects the `impl` blocks on Instance classes and datatypes, and
//! writes a patched copy of each definitions file that mentions the
//! target. A method joins the `declare extern type` block; a static
//! joins the `declare Name: {` table.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use alloy::extensions::Extension;

use crate::log;

/// Every extension declared in the given files.
pub fn collect(files: &[PathBuf]) -> Vec<Extension> {
    let mut out = Vec::new();

    for path in files {
        if let Ok(source) = std::fs::read_to_string(path) {
            out.extend(alloy::extensions::collect(&source));
        }
    }

    out
}

/// A definitions file with the extensions injected, written to the
/// cache directory. The original path comes back when nothing applies.
pub fn apply(
    path: &Path,
    exts: &[Extension],
    done: &mut HashSet<usize>,
) -> Result<PathBuf, String> {
    if exts.is_empty() {
        return Ok(path.to_path_buf());
    }

    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (patched, applied) = inject(&text, exts);

    if applied.is_empty() {
        return Ok(path.to_path_buf());
    }

    done.extend(applied.iter().copied());

    let dir = std::env::temp_dir().join("alloy-lsp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "definitions.d.luau".to_string());
    let target = dir.join(format!("ext-{name}"));
    std::fs::write(&target, patched).map_err(|e| e.to_string())?;
    log::info(&format!(
        "{} extension methods injected into {}",
        applied.len(),
        path.display()
    ));

    Ok(target)
}

/// A definitions file that declares one helper table per primitive with
/// extensions: `declare __alloy_string: { trim: (self: string) -> string }`.
/// The check artifact calls the helper, since a primitive has no class
/// block to extend. None when no primitive has an extension.
pub fn primitives_file(
    exts: &[Extension],
    done: &mut HashSet<usize>,
) -> Result<Option<PathBuf>, String> {
    let mut by_target: Vec<(&str, Vec<usize>)> = Vec::new();

    for (index, ext) in exts.iter().enumerate() {
        if !alloy::extensions::is_primitive(&ext.target) {
            continue;
        }

        match by_target.iter_mut().find(|(t, _)| *t == ext.target) {
            Some((_, list)) => list.push(index),

            None => by_target.push((&ext.target, vec![index])),
        }
    }

    if by_target.is_empty() {
        return Ok(None);
    }

    let mut text = String::new();

    for (target, indexes) in &by_target {
        text.push_str(&format!("declare __alloy_{target}: {{\n"));

        for &index in indexes {
            let ext = &exts[index];
            let ret = ext.ret.as_deref().unwrap_or("()");
            let params = if ext.is_static {
                ext.params.clone()
            } else if ext.params.is_empty() {
                format!("self: {target}")
            } else {
                format!("self: {target}, {}", ext.params)
            };
            text.push_str(&format!("\t{}: (({params}) -> {ret}),\n", ext.name));
            done.insert(index);
        }

        text.push_str("}\n");
    }

    let dir = std::env::temp_dir().join("alloy-lsp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join("ext-primitives.d.luau");
    std::fs::write(&target, text).map_err(|e| e.to_string())?;
    log::info(&format!(
        "{} primitive extension helpers written to {}",
        by_target.len(),
        target.display()
    ));

    Ok(Some(target))
}

/// Injects the extensions into a definitions text. Returns the new text
/// and the indexes of the extensions that found their target.
pub fn inject(text: &str, exts: &[Extension]) -> (String, Vec<usize>) {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut applied = Vec::new();

    for (index, ext) in exts.iter().enumerate() {
        let line = if ext.is_static {
            static_line(ext)
        } else {
            method_line(ext)
        };

        let Some(at) = insertion_point(&lines, ext) else {
            continue;
        };

        lines.insert(at, line);
        applied.push(index);
    }

    let mut out = lines.join("\n");

    if text.ends_with('\n') {
        out.push('\n');
    }

    (out, applied)
}

/// `\tfunction flat(self, by: number): Vector3`
fn method_line(ext: &Extension) -> String {
    let params = if ext.params.is_empty() {
        String::new()
    } else {
        format!(", {}", ext.params)
    };
    let ret = ext
        .ret
        .as_deref()
        .map(|r| format!(": {r}"))
        .unwrap_or_default();

    format!("\tfunction {}(self{params}){ret}", ext.name)
}

/// `\torigin: (() -> Vector3),`
fn static_line(ext: &Extension) -> String {
    let ret = ext.ret.as_deref().unwrap_or("()");

    format!("\t{}: (({}) -> {ret}),", ext.name, ext.params)
}

/// The line index to insert at: before the `end` of the target's
/// `declare extern type` block, or after the `declare Name: {` line.
fn insertion_point(lines: &[String], ext: &Extension) -> Option<usize> {
    if ext.is_static {
        let head = format!("declare {}: {{", ext.target);

        return lines
            .iter()
            .position(|l| l.trim_end() == head)
            .map(|i| i + 1);
    }

    let heads = [
        format!("declare extern type {} with", ext.target),
        format!("declare extern type {} extends ", ext.target),
        format!("declare class {} ", ext.target),
        format!("declare class {}", ext.target),
    ];
    let start = lines.iter().position(|l| {
        heads
            .iter()
            .any(|h| l.starts_with(h) || l.trim_end() == h.trim_end())
    })?;

    lines[start + 1..]
        .iter()
        .position(|l| l.trim_end() == "end")
        .map(|i| start + 1 + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(name: &str, is_static: bool, ret: Option<&str>) -> Extension {
        Extension {
            target: "Vector3".to_string(),
            name: name.to_string(),
            is_static,
            params: String::new(),
            ret: ret.map(str::to_string),
        }
    }

    #[test]
    fn injects_into_type_and_table() {
        let text = "declare extern type Vector3 with\n\tX: number\nend\ndeclare Vector3: {\n\tzero: Vector3,\n}\n";
        let exts = [
            ext("flat", false, Some("Vector3")),
            ext("origin", true, Some("Vector3")),
        ];
        let (out, applied) = inject(text, &exts);
        assert_eq!(applied, vec![0, 1]);
        assert_eq!(
            out,
            "declare extern type Vector3 with\n\tX: number\n\tfunction flat(self): Vector3\nend\ndeclare Vector3: {\n\torigin: (() -> Vector3),\n\tzero: Vector3,\n}\n"
        );
    }

    #[test]
    fn primitives_get_a_helper_table() {
        let exts = [
            Extension {
                target: "string".to_string(),
                name: "trim".to_string(),
                is_static: false,
                params: String::new(),
                ret: Some("string".to_string()),
            },
            Extension {
                target: "string".to_string(),
                name: "shout".to_string(),
                is_static: true,
                params: "s: string".to_string(),
                ret: Some("string".to_string()),
            },
            ext("flat", false, None),
        ];
        let mut done = HashSet::new();
        let path = primitives_file(&exts, &mut done).unwrap().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            text,
            "declare __alloy_string: {\n\ttrim: ((self: string) -> string),\n\tshout: ((s: string) -> string),\n}\n"
        );
        assert_eq!(done.len(), 2);
        assert!(!done.contains(&2));
    }

    #[test]
    fn unknown_target_is_skipped() {
        let (out, applied) = inject(
            "declare extern type Part with\nend\n",
            &[ext("flat", false, None)],
        );
        assert!(applied.is_empty());
        assert_eq!(out, "declare extern type Part with\nend\n");
    }
}
