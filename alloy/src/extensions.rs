//! The extension methods a file declares on foreign types. The language
//! server and `alloy flux` inject them into the analyzer's definitions,
//! so a call such as `v:flat()` types, completes, and hovers like a
//! built-in method. A method joins the target's `declare extern type`
//! block; a static joins its `declare Name: {` table; a primitive gets a
//! helper table, since it has no class block to extend.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Stmt, TokSpan};

use crate::desugar::PRIMITIVES;
use crate::roblox_classes::{DATATYPES, INSTANCE_CLASSES};

/// One method or static declared in `impl Target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub target: String,
    pub name: String,
    /// No `self` parameter: a static such as `Vector3.origin()`.
    pub is_static: bool,
    /// The parameters after `self`, as `name: type` pairs joined by `, `.
    pub params: String,
    /// The return type, when the method declares one.
    pub ret: Option<String>,
}

/// True for a type that is not an Alloy struct: an Instance class, a
/// datatype, or a primitive.
pub fn is_foreign(name: &str) -> bool {
    INSTANCE_CLASSES.contains(&name)
        || DATATYPES.contains(&name)
        || PRIMITIVES.contains(&name)
        || name == "Instance"
}

/// True for a primitive such as `string`. A primitive has no class block
/// in the definitions, so its extensions go through a helper table.
pub fn is_primitive(name: &str) -> bool {
    PRIMITIVES.contains(&name)
}

/// Every extension the source declares. A struct or enum declared in the
/// file is never foreign, whatever its name.
pub fn collect(src: &str) -> Vec<Extension> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };

    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| -> &str {
        if span.end <= span.start {
            return "";
        }

        let start = toks[span.start as usize].start as usize;
        let end = toks[span.end as usize - 1].end as usize;

        &src[start..end]
    };

    let stmts = &parsed.chunk.block.stmts;
    let mut local: HashSet<&str> = HashSet::new();

    for stmt in stmts {
        match stmt {
            Stmt::Struct(d) => {
                local.insert(text(d.name));
            }

            Stmt::Enum(d) => {
                local.insert(text(d.name));
            }

            _ => {}
        }
    }

    let mut out = Vec::new();

    for stmt in stmts {
        let Stmt::Impl(i) = stmt else {
            continue;
        };

        let target = text(i.target);

        if local.contains(target) || !is_foreign(target) {
            continue;
        }

        for m in &i.methods {
            let Some(first) = m.path.first() else {
                continue;
            };

            let params = &m.body.params;
            let has_self = params.first().is_some_and(|p| text(p.name) == "self");
            let rest = params.iter().skip(usize::from(has_self));
            let mut list = Vec::new();

            for p in rest {
                let mut ty = p.ty.map(text).unwrap_or("any").trim().to_string();

                // A default makes the parameter optional, as the emit does.
                if p.default.is_some() && !ty.ends_with('?') {
                    ty.push('?');
                }

                if p.is_vararg {
                    list.push(format!("...: {ty}"));
                } else {
                    list.push(format!("{}: {ty}", text(p.name)));
                }
            }

            out.push(Extension {
                target: target.to_string(),
                name: text(*first).to_string(),
                is_static: !has_self,
                params: list.join(", "),
                ret: m
                    .body
                    .ret_type
                    .map(|t| text(t).trim().trim_start_matches(':').trim().to_string())
                    .filter(|t| !t.is_empty()),
            });
        }
    }

    out
}

/// A definitions file with the extensions injected, written under
/// `dir`. The original path comes back when nothing applies. `done`
/// collects the indexes of the extensions that found their target.
pub fn apply(
    path: &Path,
    exts: &[Extension],
    done: &mut HashSet<usize>,
    dir: &Path,
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
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "definitions.d.luau".to_string());
    let target = dir.join(format!("ext-{name}"));
    std::fs::write(&target, patched).map_err(|e| e.to_string())?;

    Ok(target)
}

/// A definitions file that declares one helper table per primitive with
/// extensions: `declare __alloy_string: { trim: (self: string) -> string }`.
/// The check artifact calls the helper, since a primitive has no class
/// block to extend. None when no primitive has an extension.
pub fn primitives_file(
    exts: &[Extension],
    done: &mut HashSet<usize>,
    dir: &Path,
) -> Result<Option<PathBuf>, String> {
    let Some(text) = primitives_text(exts, done) else {
        return Ok(None);
    };

    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let target = dir.join("ext-primitives.d.luau");
    std::fs::write(&target, text).map_err(|e| e.to_string())?;

    Ok(Some(target))
}

/// The text of the primitive helper tables; none when no primitive has
/// an extension.
pub fn primitives_text(exts: &[Extension], done: &mut HashSet<usize>) -> Option<String> {
    let mut by_target: Vec<(&str, Vec<usize>)> = Vec::new();

    for (index, ext) in exts.iter().enumerate() {
        if !is_primitive(&ext.target) {
            continue;
        }

        match by_target.iter_mut().find(|(t, _)| *t == ext.target) {
            Some((_, list)) => list.push(index),

            None => by_target.push((&ext.target, vec![index])),
        }
    }

    if by_target.is_empty() {
        return None;
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

    Some(text)
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

        // The extension wins over a member of the same name that the
        // definitions already carry, such as the `zero` property of
        // `Vector3` under a `zero()` static.
        match existing_member(&lines, at, ext) {
            Some(i) => lines[i] = line,

            None => lines.insert(at, line),
        }

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

/// A member of the target with the extension's name, inside the block
/// the insertion point belongs to: a `name:` entry of the table, or a
/// `function name(` or `name:` line of the type.
fn existing_member(lines: &[String], at: usize, ext: &Extension) -> Option<usize> {
    let is_member = |l: &str| {
        let t = l.trim_start();

        t.starts_with(&format!("{}:", ext.name))
            || t.starts_with(&format!("{} :", ext.name))
            || t.starts_with(&format!("function {}(", ext.name))
    };

    if ext.is_static {
        // From the line after the head to the `}` that closes the table.
        return lines[at..]
            .iter()
            .take_while(|l| l.trim_end() != "}")
            .position(|l| is_member(l))
            .map(|i| at + i);
    }

    // From the head of the block back up from the `end` at `at`.
    let head = lines[..at]
        .iter()
        .rposition(|l| l.starts_with("declare extern type") || l.starts_with("declare class"))?;

    lines[head + 1..at]
        .iter()
        .position(|l| is_member(l))
        .map(|i| head + 1 + i)
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
        let text = primitives_text(&exts, &mut done).unwrap();
        assert_eq!(
            text,
            "declare __alloy_string: {\n\ttrim: ((self: string) -> string),\n\tshout: ((s: string) -> string),\n}\n"
        );
        assert_eq!(done.len(), 2);
        assert!(!done.contains(&2));
    }

    #[test]
    fn an_extension_replaces_a_member_of_the_same_name() {
        let text = "declare extern type Vector3 with\n\tX: number\n\tfunction Dot(self, other: Vector3): number\nend\ndeclare Vector3: {\n\tzero: Vector3,\n\tone: Vector3,\n}\n";
        let exts = [
            ext("zero", true, Some("Vector3")),
            ext("Dot", false, Some("number")),
        ];
        let (out, applied) = inject(text, &exts);
        assert_eq!(applied, vec![0, 1]);
        assert_eq!(
            out,
            "declare extern type Vector3 with\n\tX: number\n\tfunction Dot(self): number\nend\ndeclare Vector3: {\n\tzero: (() -> Vector3),\n\tone: Vector3,\n}\n"
        );
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

    #[test]
    fn methods_and_statics() {
        let src = "export impl Vector3\n    function flat(self): Vector3\n        return self\n    end\n    function origin(): Vector3\n        return Vector3.zero\n    end\n    function scale(self, by: number, extra = 1)\n    end\nend\nstruct Vec2 as\n    x: number\nend\nimpl Vec2\n    function m(self) end\nend\n";
        let exts = collect(src);
        assert_eq!(exts.len(), 3, "{exts:#?}");
        assert_eq!(exts[0].name, "flat");
        assert!(!exts[0].is_static);
        assert_eq!(exts[0].ret.as_deref(), Some("Vector3"));
        assert_eq!(exts[1].name, "origin");
        assert!(exts[1].is_static);
        assert_eq!(exts[2].params, "by: number, extra: any?");
        assert_eq!(exts[2].ret, None);
    }
}
