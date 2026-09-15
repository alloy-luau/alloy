//! The extension methods a file declares on foreign types. The language
//! server and `alloy flux` inject them into the analyzer's definitions,
//! so a call such as `v:flat()` types, completes, and hovers like a
//! built-in method. A method joins the target's `declare extern type`
//! block; a static joins its `declare Name: {` table; a primitive gets a
//! helper table, since it has no class block to extend.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use alloy_syntax::ast::{Param, Stmt, TokSpan};

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

impl Extension {
    /// The target's name and the generic list the `impl` head wrote:
    /// `Box<T>` is `("Box", "<T>")`, and `Box` is `("Box", "")`.
    pub fn head(&self) -> (&str, &str) {
        match self.target.find('<') {
            Some(at) => self.target.split_at(at),

            None => (self.target.as_str(), ""),
        }
    }
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
    impls(src, false).methods
}

/// Every `impl X as` the source writes on a struct or an enum another
/// file declares. The runtime attaches those methods to the table the
/// require brought in; the declaring file's check artifact declares
/// them, so every file reads the same shape.
pub fn struct_impls(src: &str) -> Vec<Extension> {
    impls(src, true).methods
}

/// What the other files of a project put on a struct or an enum one
/// file declares.
#[derive(Debug, Default)]
pub struct ProjectImpls {
    /// Every method, for the declaring file's check artifact.
    pub methods: Vec<Extension>,
    /// Per target, the methods its impls declare private. A private
    /// method still reaches the check artifact, because the impl's own
    /// file calls it through the struct's table; the `private_access`
    /// lint reads this list, so a call from a file that holds no impl
    /// of the struct reports.
    pub privates: Vec<(String, Vec<String>)>,
}

/// The `impl` blocks every source of a project writes on a struct or an
/// enum another file declares. One walk per file answers every reader:
/// the check artifact, the privacy lint, and the default methods an
/// `impl Trait for S` brings with it.
pub fn project_impls(sources: &[String]) -> ProjectImpls {
    let files: Vec<FileImpls> = sources.iter().map(|src| impls(src, true)).collect();
    let mut out = ProjectImpls::default();

    for file in &files {
        out.methods.extend(file.methods.iter().cloned());

        for (target, name) in &file.privates {
            match out.privates.iter_mut().find(|(t, _)| t == target) {
                Some((_, list)) => {
                    if !list.contains(name) {
                        list.push(name.clone());
                    }
                }

                None => out.privates.push((target.clone(), vec![name.clone()])),
            }
        }
    }

    // `impl Trait for S` flattens the trait's default methods onto `S`,
    // so the struct carries them too. The trait may sit in a third file,
    // which is why the pass runs over the whole project and after every
    // method the impls write: one the impl wrote keeps its own type.
    for file in &files {
        for (trait_name, target) in &file.trait_impls {
            let defaults = files
                .iter()
                .flat_map(|f| &f.defaults)
                .filter(|d| d.target == *trait_name);

            for d in defaults {
                if out
                    .methods
                    .iter()
                    .any(|m| m.target == *target && m.name == d.name)
                {
                    continue;
                }

                out.methods.push(Extension {
                    target: target.clone(),
                    ..d.clone()
                });
            }
        }
    }

    out
}

/// What one file says about the methods of a struct or an enum another
/// file declares.
#[derive(Default)]
struct FileImpls {
    methods: Vec<Extension>,
    /// `(target, method)` for every method an `impl` declares private.
    privates: Vec<(String, String)>,
    /// `(trait, target)` for every `impl Trait for S` the file writes.
    trait_impls: Vec<(String, String)>,
    /// The default methods of every `trait` the file declares, each with
    /// the trait as its target.
    defaults: Vec<Extension>,
}

/// The structs a source declares whose check artifact keeps a private
/// view, `Name__all`: a struct with a private field, or one an `impl`
/// here gives a private method. A generic struct keeps one view. The
/// declaring file exports the view, so an `impl` of the struct in
/// another file types `self` as it and reaches the private members.
pub fn private_views(src: &str) -> Vec<String> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };

    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let stmts = flat_stmts(&parsed.chunk.block.stmts, &text);
    let mut plain: HashSet<String> = HashSet::new();

    for (prefix, stmt) in &stmts {
        if let Stmt::Struct(d) = stmt
            && d.generics.is_none()
        {
            plain.insert(format!("{prefix}{}", text(d.name)));
        }
    }

    let private = |v: Option<TokSpan>| v.is_some_and(|v| text(v) == "private");
    let mut out: Vec<String> = Vec::new();

    for (prefix, stmt) in &stmts {
        let name = match stmt {
            Stmt::Struct(d) if d.fields.iter().any(|f| private(f.visibility)) => {
                format!("{prefix}{}", text(d.name))
            }

            Stmt::Impl(i) if i.methods.iter().any(|m| private(m.visibility)) => {
                rendered_target(prefix, text(i.target))
            }

            _ => continue,
        };

        if plain.contains(&name) && !out.contains(&name) {
            out.push(name);
        }
    }

    out
}

/// Every statement of a file with the prefix the emit puts on the names
/// it declares: the empty string at the top level, and `Zoo_` inside
/// `namespace Zoo`, nesting and all. A namespace has no Luau form, so a
/// struct of one is `Zoo_Lion` everywhere the project names it.
fn flat_stmts<'a>(
    stmts: &'a [Stmt],
    text: &impl Fn(TokSpan) -> &'a str,
) -> Vec<(String, &'a Stmt)> {
    let mut out: Vec<(String, &'a Stmt)> = Vec::new();
    let mut stack: Vec<(String, &'a Stmt)> =
        stmts.iter().rev().map(|s| (String::new(), s)).collect();

    while let Some((prefix, stmt)) = stack.pop() {
        if let Stmt::Namespace(ns) = stmt {
            let head = format!("{prefix}{}_", text(ns.name));

            for m in ns.members.iter().rev() {
                stack.push((head.clone(), &m.stmt));
            }

            continue;
        }

        out.push((prefix, stmt));
    }

    out
}

/// The name an `impl` target renders under: the prefix of the namespace
/// the block sits in, and the dots of a path joined the way the emit
/// joins them. `impl Zoo.Lion` targets `Zoo_Lion`.
fn rendered_target(prefix: &str, written: &str) -> String {
    format!("{prefix}{}", written.replace('.', "_"))
}

/// The `impl` blocks of one file, the methods they keep private, and the
/// traits they name. `own` picks which targets count: a type of the
/// project, or a foreign one.
fn impls(src: &str, own: bool) -> FileImpls {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return FileImpls::default();
    };

    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| span.text_or_empty(src, toks);

    let stmts = flat_stmts(&parsed.chunk.block.stmts, &text);
    let mut local: HashSet<String> = HashSet::new();

    for (prefix, stmt) in &stmts {
        match stmt {
            Stmt::Struct(d) => {
                local.insert(format!("{prefix}{}", text(d.name)));
            }

            Stmt::Enum(d) => {
                local.insert(format!("{prefix}{}", text(d.name)));
            }

            _ => {}
        }
    }

    let mut out = FileImpls::default();
    // The parameters after `self`, as `name: type` pairs, with whether
    // the method takes `self`. An impl method and a trait's default
    // method read the same way; only their return types differ, because
    // a trait method keeps its whole signature as one span.
    let signature = |params: &[Param]| {
        let has_self = params.first().is_some_and(|p| text(p.name) == "self");
        let mut list = Vec::new();

        for p in params.iter().skip(usize::from(has_self)) {
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

        (has_self, list.join(", "))
    };
    let declared_ret = |span: TokSpan| {
        let t = text(span).trim().trim_start_matches(':').trim().to_string();

        (!t.is_empty()).then_some(t)
    };

    for (prefix, stmt) in &stmts {
        if let Stmt::Trait(t) = stmt {
            for m in &t.methods {
                // A method with no body is abstract: the impl writes it.
                if m.body.is_none() {
                    continue;
                }

                let (has_self, params) = signature(&m.params);
                out.defaults.push(Extension {
                    target: format!("{prefix}{}", text(t.name)),
                    name: text(m.name).to_string(),
                    is_static: !has_self,
                    params,
                    ret: trait_ret(text(m.signature)),
                });
            }
        }

        let Stmt::Impl(i) = stmt else {
            continue;
        };

        let target = rendered_target(prefix, text(i.target));
        let target = target.as_str();

        if local.contains(target) {
            // The declaring file's own impl: its methods travel nowhere,
            // the file declares them. The private ones still do, because
            // `private_access` reads the project's list, and a call from
            // another file has to report.
            for m in &i.methods {
                let Some(first) = m.path.first() else {
                    continue;
                };

                if m.visibility.is_some_and(|v| text(v) == "private") {
                    out.privates
                        .push((target.to_string(), text(*first).to_string()));
                }
            }

            continue;
        }

        if is_foreign(target) == own {
            continue;
        }

        // `impl Box<T>`: the head travels with its generic list, so the
        // declaring file's stub binds `T` itself. A trait impl writes
        // its methods on the same table a plain impl does, so those
        // travel the way a plain impl's do.
        let head = format!(
            "{target}{}",
            i.generics
                .map(|g| crate::desugar::strip_bounds(text(g)))
                .unwrap_or_default()
        );

        if let Some(name) = i.trait_name {
            out.trait_impls.push((text(name).to_string(), head.clone()));
        }

        for m in &i.methods {
            let Some(first) = m.path.first() else {
                continue;
            };

            let (has_self, params) = signature(&m.body.params);
            let ret = m.body.ret_type.and_then(declared_ret);

            if m.visibility.is_some_and(|v| text(v) == "private") {
                out.privates
                    .push((target.to_string(), text(*first).to_string()));
            }

            out.methods.push(Extension {
                target: head.clone(),
                name: text(*first).to_string(),
                is_static: !has_self,
                params,
                ret,
            });
        }
    }

    out
}

/// The return type a trait method declares. Its signature is one span
/// from the `(`, so the type is the text after the `)` that closes the
/// parameter list. None when the method declares none.
fn trait_ret(signature: &str) -> Option<String> {
    let mut depth = 0i32;

    for (at, c) in signature.char_indices() {
        match c {
            '(' => depth += 1,

            ')' => {
                depth -= 1;

                if depth == 0 {
                    let rest = signature[at + c.len_utf8()..]
                        .trim()
                        .trim_start_matches(':')
                        .trim();

                    return (!rest.is_empty()).then(|| rest.to_string());
                }
            }

            _ => {}
        }
    }

    None
}

/// A definitions file with the extensions injected and `Player.Character`
/// typed by `rig`, written under `dir`. The original path comes back
/// when nothing applies. `done` collects the indexes of the extensions
/// that found their target.
pub fn apply(
    path: &Path,
    exts: &[Extension],
    rig: &str,
    done: &mut HashSet<usize>,
    dir: &Path,
) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (patched, applied) = inject(&text, exts);
    let rigged = rig_character(&patched, rig);

    if applied.is_empty() && rigged.is_none() {
        return Ok(path.to_path_buf());
    }

    let patched = rigged.unwrap_or(patched);
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

/// The two rig types, cut from the std's own text, so the definitions
/// and the runtime say one thing. A definitions file declares a type
/// with `type`, not `export type`.
pub fn rig_types() -> String {
    let text = crate::RUNTIME;
    let start = text
        .find("export type R15Character = ")
        .expect("R15Character in the std");
    let r6 = start
        + text[start..]
            .find("export type R6Character = ")
            .expect("R6Character in the std");
    let end = r6 + text[r6..].find("\n}\n").expect("end of R6Character") + 3;

    text[start..end].replace("export type ", "type ")
}

/// The definitions text with the `Character` of `Player` typed by the
/// rig, `R15Character?` or `R6Character?`, and the two types declared
/// at the end. `None` when the text declares no `Player.Character`.
pub fn rig_character(text: &str, rig: &str) -> Option<String> {
    let name = if rig == "R6" {
        "R6Character"
    } else {
        "R15Character"
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let head = lines.iter().position(|l| {
        l.starts_with("declare extern type Player ")
            || l.starts_with("declare class Player ")
            || l.trim_end() == "declare class Player"
    })?;
    let at = head
        + 1
        + lines[head + 1..]
            .iter()
            .take_while(|l| l.trim_end() != "end")
            .position(|l| l.trim() == "Character: Model?")?;
    lines[at] = format!("\tCharacter: {name}?");
    let mut out = lines.join("\n");
    out.push('\n');
    out.push_str(&rig_types());

    Some(out)
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

    /// `Player.Character` reads as the rig's type, and the definitions
    /// carry the two types the std declares, so the child resolves them.
    #[test]
    fn the_rig_types_the_character_of_a_player() {
        let text = "declare extern type Model extends Instance with\nend\ndeclare extern type Player extends Instance with\n\tCharacter: Model?\n\tCharacterAdded: RBXScriptSignal<Model>\nend\n";

        let r15 = rig_character(text, "R15").expect("Player");
        assert!(r15.contains("\tCharacter: R15Character?\n"), "{r15}");
        assert!(
            r15.contains("\tCharacterAdded: RBXScriptSignal<Model>\n"),
            "{r15}"
        );
        assert!(r15.contains("\ntype R15Character = Model & {\n"), "{r15}");
        assert!(r15.contains("\ntype R6Character = Model & {\n"), "{r15}");
        assert!(!r15.contains("export type"), "{r15}");
        assert!(r15.contains("\t[\"Left Arm\"]: Part?,\n"), "{r15}");

        let r6 = rig_character(text, "R6").expect("Player");
        assert!(r6.contains("\tCharacter: R6Character?\n"), "{r6}");

        // A definitions file with no Player stays as it is.
        assert!(
            rig_character(
                "declare extern type Model extends Instance with\nend\n",
                "R15"
            )
            .is_none()
        );
    }

    fn ext(name: &str, is_static: bool, ret: Option<&str>) -> Extension {
        Extension {
            target: "Vector3".to_string(),
            name: name.to_string(),
            is_static,
            params: String::new(),
            ret: ret.map(str::to_string),
        }
    }

    /// An `impl Trait for S` on a struct another file declares writes
    /// the methods on the same table a plain `impl S` does, so the
    /// declaring file's check artifact has to declare them too.
    #[test]
    fn a_trait_impl_on_an_imported_struct_travels() {
        let src = "import { Box, Area } from \"./shapes\"\n\nimpl Area for Box as\n    function area(self): number\n        return self.w * self.h\n    end\nend\n";
        let found = struct_impls(src);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].target, "Box");
        assert_eq!(found[0].name, "area");
        assert_eq!(found[0].ret.as_deref(), Some("number"));
        assert!(!found[0].is_static);

        // A generic target travels with its generic list, so the
        // declaring file's stub binds `T` itself.
        let generic = "import { Bag } from \"./bag\"\n\nimpl Bag<T: Ord> as\n    function first(self): T\n        return self.items[1]\n    end\nend\n";
        let found = struct_impls(generic);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].target, "Bag<T>");
        assert_eq!(found[0].head(), ("Bag", "<T>"));
        assert_eq!(found[0].ret.as_deref(), Some("T"));
    }

    /// An `impl Trait for S` on a struct another file declares brings
    /// the trait's default methods with it, from a third file. The
    /// struct's own file declares them, so a reader anywhere finds
    /// `greet` on `Cat`.
    #[test]
    fn a_trait_default_reaches_the_struct_of_a_cross_file_impl() {
        let greeter = "export trait Greeter as\n    function name(self): string\n    function greet(self): string\n        return \"hi\"\n    end\nend\n";
        let catimpl = "import { Greeter } from \"./greeter\"\nimport { Cat } from \"./animal\"\n\nimpl Greeter for Cat as\n    function name(self): string\n        return self.label\n    end\nend\n";
        let found = project_impls(&[catimpl.to_string(), greeter.to_string()]);
        let greet = found
            .methods
            .iter()
            .find(|m| m.name == "greet")
            .unwrap_or_else(|| panic!("{:?}", found.methods));

        assert_eq!(greet.target, "Cat");
        assert_eq!(greet.ret.as_deref(), Some("string"));
        assert!(!greet.is_static);
        // A method the impl writes keeps the impl's own type, once.
        assert_eq!(found.methods.iter().filter(|m| m.name == "name").count(), 1);
    }

    /// An `impl` in another file keeps its private methods private. The
    /// project index names them, so the `private_access` lint reports a
    /// call from a file that holds no impl of the struct. The check
    /// artifact still declares them: the impl's own file calls them
    /// through the struct's table.
    #[test]
    fn a_private_method_of_a_cross_file_impl_travels() {
        let src = "import { Cat } from \"./animal\"\n\nimpl Cat as\n    private function purr(self): string\n        return self.label\n    end\n\n    function speak(self): string\n        return self:purr()\n    end\nend\n";
        let project = project_impls(&[src.to_string()]);

        assert_eq!(
            project.privates,
            vec![("Cat".to_string(), vec!["purr".to_string()])]
        );
        assert_eq!(project.methods.len(), 2, "{:?}", project.methods);
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
        let src = "export impl Vector3 as\n    function flat(self): Vector3\n        return self\n    end\n    function origin(): Vector3\n        return Vector3.zero\n    end\n    function scale(self, by: number, extra = 1)\n    end\nend\nstruct Vec2 as\n    x: number\nend\nimpl Vec2 as\n    function m(self) end\nend\n";
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
