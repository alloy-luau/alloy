//! The declarations of a file as Alloy hover text. The child sees a
//! struct as a table and an interface as a type alias, so the editor
//! shows the declaration the way the source wrote it instead.

use std::collections::HashMap;

use alloy_syntax::ast::{Stmt, TokSpan};

/// One declaration and its hover Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub hover: String,
    /// The byte offset of the declared name, for go to definition.
    pub offset: usize,
}

/// The comment block right above a byte offset, as Markdown: the `--`
/// or `---` lines that end on the line before, with attribute lines
/// between them and the declaration skipped. A blank line ends the block.
pub fn doc_before(src: &str, offset: usize) -> Option<String> {
    let before = &src[..offset.min(src.len())];
    let mut lines: Vec<&str> = Vec::new();
    let mut iter = before.lines().rev();

    // The declaration's own line, up to the offset, is not a comment.
    if !before.ends_with('\n') {
        iter.next();
    }

    for line in iter {
        let t = line.trim();

        if t.starts_with('@') && lines.is_empty() {
            continue;
        }

        match t.strip_prefix("---").or_else(|| t.strip_prefix("--")) {
            // `--[[` opens a block; `--!strict` is a directive.
            Some(rest) if !rest.starts_with('[') && !rest.starts_with('!') => {
                lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            }

            _ => break,
        }
    }

    if lines.is_empty() {
        return None;
    }

    lines.reverse();

    Some(lines.join("\n").trim().to_string())
}

/// Every struct, interface, enum, trait, type alias, class, and, in a
/// definition file, every `declare` at the top level. A struct
/// or enum lists the traits it implements and its methods from the
/// `impl` blocks of the same file.
pub fn summaries(src: &str, definitions: bool) -> Vec<Declaration> {
    let options = alloy_syntax::parser::ParseOptions {
        definitions,
        ..Default::default()
    };
    let Ok(parsed) = alloy_syntax::parse_lenient(src, options) else {
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

    // Target -> (traits, methods), from every impl in the file.
    let mut impls: HashMap<&str, (Vec<&str>, Vec<&str>)> = HashMap::new();

    for stmt in stmts {
        if let Stmt::Impl(i) = stmt {
            let entry = impls.entry(text(i.target)).or_default();

            if let Some(t) = i.trait_name {
                entry.0.push(text(t));
            }

            for m in &i.methods {
                if let Some(first) = m.path.first() {
                    entry.1.push(text(*first));
                }
            }
        }
    }

    let export = |exported: bool| if exported { "export " } else { "" };
    let start_of = |span: TokSpan| toks[span.start as usize].start as usize;
    let mut out = Vec::new();
    let mut notes_first: Vec<String> = Vec::new();

    for stmt in stmts {
        let doc = match stmt {
            Stmt::Struct(d) => doc_before(src, start_of(d.span)),

            Stmt::Interface(d) => doc_before(src, start_of(d.span)),

            Stmt::Enum(d) => doc_before(src, start_of(d.span)),

            Stmt::Trait(d) => doc_before(src, start_of(d.span)),

            Stmt::Declare(d) => doc_before(src, start_of(d.span)),

            Stmt::TypeAlias(d) => doc_before(src, start_of(d.span)),

            Stmt::Class(d) => doc_before(src, start_of(d.span)),

            Stmt::Macro(d) => doc_before(src, start_of(d.span)),

            Stmt::Attribute(d) => doc_before(src, start_of(d.span)),

            _ => None,
        };

        // A variant hovers on its own, from `Msg.Join` and from a pattern.
        if let Stmt::Enum(d) = stmt {
            let enum_name = text(d.name);

            for v in &d.variants {
                let vname = text(v.name);
                let vdoc = doc_before(src, start_of(v.span));
                let mut hover = format!(
                    "```alloy\n{enum_name}.{}\n```\nA variant of `enum {enum_name}`.",
                    text(v.span)
                );

                if let Some(d) = vdoc {
                    hover.push_str("\n\n");
                    hover.push_str(&d);
                }

                let offset = start_of(v.name);
                out.push(Declaration {
                    name: vname.to_string(),
                    hover: hover.clone(),
                    offset,
                });
                out.push(Declaration {
                    name: format!("{enum_name}.{vname}"),
                    hover,
                    offset,
                });
            }
        }

        // Sigil names for a macro or an attribute, beside the bare name.
        let mut names: Vec<String> = Vec::new();
        let offset = match stmt {
            Stmt::Struct(d) => start_of(d.name),

            Stmt::Interface(d) => start_of(d.name),

            Stmt::Enum(d) => start_of(d.name),

            Stmt::Trait(d) => start_of(d.name),

            Stmt::TypeAlias(d) => start_of(d.name),

            Stmt::Class(d) => start_of(d.name),

            Stmt::Macro(d) => start_of(d.name),

            Stmt::Attribute(d) => start_of(d.name),

            Stmt::Declare(d) => start_of(d.span),

            _ => 0,
        };
        let (name, mut lines) = match stmt {
            Stmt::Struct(d) => {
                let name = text(d.name);
                let generics = d.generics.map(text).unwrap_or("");
                let mut lines = vec![format!("{}struct {name}{generics} as", export(d.exported))];
                lines.extend(d.fields.iter().map(|f| format!("    {}", text(f.span))));
                lines.push("end".to_string());

                (name, lines)
            }

            Stmt::Interface(d) => {
                let name = text(d.name);
                let generics = d.generics.map(text).unwrap_or("");
                let extends = if d.extends.is_empty() {
                    String::new()
                } else {
                    let names: Vec<&str> = d.extends.iter().map(|e| text(*e)).collect();

                    format!(" extends {}", names.join(", "))
                };
                let mut lines = vec![format!(
                    "{}interface {name}{generics}{extends} as",
                    export(d.exported)
                )];
                lines.extend(d.fields.iter().map(|f| format!("    {}", text(f.span))));
                lines.push("end".to_string());

                (name, lines)
            }

            Stmt::Enum(d) => {
                let name = text(d.name);
                let mut lines = vec![format!("{}enum {name} as", export(d.exported))];
                lines.extend(d.variants.iter().map(|v| format!("    {}", text(v.span))));
                lines.push("end".to_string());

                (name, lines)
            }

            // A definition-file statement, a type alias, and a class show
            // their own text, capped so a long class stays a hover.
            Stmt::Declare(d) => match declared_name(text(d.span)) {
                Some(name) => (name, capped(text(d.span))),

                None => continue,
            },

            Stmt::TypeAlias(d) => (text(d.name), capped(text(d.span))),

            // A macro and an attribute carry their sigil in the name, so a
            // hover on `$name` or `@name` finds them and nothing else does.
            Stmt::Macro(d) => {
                let params: Vec<String> = d.params.iter().map(|p| param_text(p, &text)).collect();
                let header = format!(
                    "{}macro {}({})",
                    export(d.exported),
                    text(d.name),
                    params.join(", ")
                );
                // A long body stays out of the hover: the header says
                // enough, and the definition is one jump away.
                let body: Vec<String> = text(d.span).lines().skip(1).map(str::to_string).collect();
                let mut lines = vec![header];

                if body.len() <= 6 {
                    lines.extend(body);
                }

                names.push(format!("${}", text(d.name)));

                (text(d.name), lines)
            }

            // An attribute hovers the way a built-in one does: its use,
            // then what it goes on.
            Stmt::Attribute(d) => {
                let params: Vec<String> = d.params.iter().map(|p| param_text(p, &text)).collect();
                let targets: Vec<&str> = d.targets.iter().map(|t| text(*t)).collect();
                let params = if params.is_empty() {
                    String::new()
                } else {
                    format!("({})", params.join(", "))
                };
                names.push(format!("@{}", text(d.name)));
                let list: Vec<String> = targets.iter().map(|t| format!("`{t}`")).collect();
                notes_first.push(format!("**Applies to** {}", list.join(" · ")));

                (text(d.name), vec![format!("@{}{params}", text(d.name))])
            }

            Stmt::Class(d) => (text(d.name), capped(text(d.span))),

            Stmt::Trait(d) => {
                let name = text(d.name);
                let mut lines = vec![format!("{}trait {name}", export(d.exported))];
                lines.extend(
                    d.methods
                        .iter()
                        .map(|m| format!("    function {}{}", text(m.name), text(m.signature))),
                );
                lines.push("end".to_string());

                (name, lines)
            }

            _ => continue,
        };

        // Interfaces and traits have no impl blocks of their own.
        let mut notes = std::mem::take(&mut notes_first);

        if let Some((traits, methods)) = impls.get(name) {
            if !traits.is_empty() {
                let list: Vec<String> = traits.iter().map(|t| format!("`{t}`")).collect();
                notes.push(format!("Implements {}.", list.join(", ")));
            }

            if !methods.is_empty() {
                let list: Vec<String> = methods.iter().map(|m| format!("`{m}`")).collect();
                notes.push(format!("Methods: {}.", list.join(", ")));
            }
        }

        lines.insert(0, "```alloy".to_string());
        lines.push("```".to_string());
        let mut hover = lines.join("\n");

        if let Some(d) = &doc {
            hover.push_str("\n\n");
            hover.push_str(d);
        }

        if !notes.is_empty() {
            hover.push_str("\n\n");
            hover.push_str(&notes.join(" "));
        }

        // A macro or an attribute is its sigil form alone: a variable that
        // shares the bare name is not it.
        let bare = !matches!(stmt, Stmt::Macro(_) | Stmt::Attribute(_));

        for sigil in names {
            out.push(Declaration {
                name: sigil,
                hover: hover.clone(),
                offset,
            });
        }

        if bare {
            out.push(Declaration {
                name: name.to_string(),
                hover,
                offset,
            });
        }
    }

    out
}

/// `name: T` for a parameter, or `name` alone.
fn param_text<'a>(p: &alloy_syntax::ast::Param, text: &impl Fn(TokSpan) -> &'a str) -> String {
    match p.ty {
        Some(t) => format!("{}: {}", text(p.name), text(t)),

        None => text(p.name).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_with_impls() {
        let src = "export struct Vec2 as\n    x: number\n    y: number = 0\nend\nimpl Vec2\n    function len(self) end\nend\nimpl Display for Vec2\n    function to_string(self) end\nend\n";
        let d = summaries(src, false);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "Vec2");
        assert_eq!(
            d[0].hover,
            "```alloy\nexport struct Vec2 as\n    x: number\n    y: number = 0\nend\n```\n\nImplements `Display`. Methods: `len`, `to_string`."
        );
    }

    #[test]
    fn doc_comment_and_variants() {
        let src = "-- The message a client sends.\n-- Two lines.\n@derive(Eq)\nenum Msg as\n    --- Leave now.\n    Quit\n    Move(number)\nend\n";
        let d = summaries(src, false);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["Quit", "Msg.Quit", "Move", "Msg.Move", "Msg"]);
        assert!(
            d[4].hover
                .ends_with("end\n```\n\nThe message a client sends.\nTwo lines."),
            "{}",
            d[4].hover
        );
        assert_eq!(
            d[0].hover,
            "```alloy\nMsg.Quit\n```\nA variant of `enum Msg`.\n\nLeave now."
        );
        assert_eq!(
            d[2].hover,
            "```alloy\nMsg.Move(number)\n```\nA variant of `enum Msg`."
        );
    }

    #[test]
    fn macros_and_attributes_carry_their_sigil() {
        let src = "--- Twice the value.\nmacro twice(x)\n    x * 2\nend\nexport attribute range(min: number, max: number) on field\n";
        let d = summaries(src, false);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["$twice", "@range"]);
        assert_eq!(
            d[0].hover,
            "```alloy\nmacro twice(x)\n    x * 2\nend\n```\n\nTwice the value."
        );
        assert_eq!(
            d[1].hover,
            "```alloy\n@range(min: number, max: number)\n```\n\n**Applies to** `field`"
        );
        assert_eq!(d[0].offset, src.find("twice").unwrap());
        assert_eq!(d[1].offset, src.find("range").unwrap());
    }

    #[test]
    fn definition_file_statements() {
        let src = "-- Once per message.\ndeclare function warn_once(message: string): ()\ndeclare extern type PluginToolbar with\n    function CreateButton(self, id: string): PluginToolbarButton\nend\ndeclare plugin: Plugin\nexport type Patch<T> = { [K in keyof T]: T[K]? }\n";
        let d = summaries(src, true);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["warn_once", "PluginToolbar", "plugin", "Patch"]);
        assert_eq!(
            d[0].hover,
            "```alloy\ndeclare function warn_once(message: string): ()\n```\n\nOnce per message."
        );
        assert!(
            d[1].hover
                .contains("declare extern type PluginToolbar with\n    function CreateButton")
        );
        assert!(d[3].hover.contains("export type Patch<T> ="));
    }

    #[test]
    fn interface_extends() {
        let src = "interface Named as\n    name: string\nend\ninterface Entity extends Named, Positioned as\n    id: number\nend\n";
        let d = summaries(src, false);
        assert_eq!(d.len(), 2);
        assert!(
            d[1].hover
                .contains("interface Entity extends Named, Positioned as\n    id: number\nend")
        );
    }

    #[test]
    fn enum_and_trait() {
        let src = "enum Msg as\n    Quit\n    Move(number, number)\nend\ntrait Shape\n    function area(self): number\nend\n";
        let d = summaries(src, false);
        let find = |name: &str| d.iter().find(|x| x.name == name).unwrap();
        assert!(
            find("Msg")
                .hover
                .contains("enum Msg as\n    Quit\n    Move(number, number)\nend")
        );
        assert!(
            find("Shape")
                .hover
                .contains("trait Shape\n    function area(self): number\nend")
        );
    }
}

/// The first twelve lines of a declaration's text, with a marker when
/// more follow.
fn capped(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().take(12).map(str::to_string).collect();

    if text.lines().count() > 12 {
        lines.push("-- ...".to_string());
    }

    lines
}

/// The name a `declare` statement introduces: `declare function f(`,
/// `declare extern type T with`, `declare class C`, or `declare x: T`.
fn declared_name(text: &str) -> Option<&str> {
    let first = text.lines().next()?;
    let mut words = first.split(|c: char| !(c.is_alphanumeric() || c == '_'));
    let mut seen_declare = false;

    for word in words.by_ref() {
        if word.is_empty() {
            continue;
        }

        if !seen_declare {
            if word != "declare" {
                return None;
            }

            seen_declare = true;

            continue;
        }

        if matches!(word, "extern" | "type" | "class" | "function") {
            continue;
        }

        return Some(word);
    }

    None
}

/// A name and the keywords that declared it: `const`, `export const`,
/// `async function`, `local async function`, and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub prefix: String,
    /// The comment block above the declaration, as Markdown.
    pub doc: Option<String>,
}

const DECL_WORDS: [&str; 6] = ["export", "local", "const", "async", "function", "type"];

/// Every binding in the source with its declaring keywords, at any
/// depth. A scan over the lexer's tokens, not the tree: the hover only
/// needs the keywords in front of a name, and the lexer already leaves
/// comments and strings out, so a word in a comment never reaches a run.
pub fn bindings(src: &str) -> Vec<Binding> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };

    let toks = &lexed.toks;
    let text = |i: usize| &src[toks[i].start as usize..toks[i].end as usize];
    let is_name = |t: &str| {
        t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let mut out = Vec::new();
    let mut i = 0;

    while i < toks.len() {
        let mut run = Vec::new();
        let run_start = toks[i].start as usize;

        while i < toks.len() && DECL_WORDS.contains(&text(i)) {
            run.push(text(i));
            i += 1;
        }

        if run.is_empty() {
            i += 1;

            continue;
        }

        let declares = run
            .iter()
            .any(|k| matches!(*k, "local" | "const" | "function"));

        if !declares || i >= toks.len() || !is_name(text(i)) {
            continue;
        }

        let prefix = run.join(" ");
        let doc = doc_before(src, run_start);
        let mut name = text(i).to_string();
        let mut last = text(i).to_string();
        i += 1;

        // `function M.f` and `function M:f` also answer a hover on `f`. A
        // local's `:` starts its type, which is no part of the name.
        while run.contains(&"function")
            && i + 1 < toks.len()
            && matches!(text(i), "." | ":")
            && is_name(text(i + 1))
        {
            name.push_str(text(i));
            name.push_str(text(i + 1));
            last = text(i + 1).to_string();
            i += 2;
        }

        out.push(Binding {
            name: last.clone(),
            prefix: prefix.clone(),
            doc: doc.clone(),
        });

        if last != name {
            out.push(Binding {
                name,
                prefix: prefix.clone(),
                doc: doc.clone(),
            });
        }

        // `local a, b = 1, 2`: the names after a comma share the keywords.
        if !run.contains(&"function") {
            while i + 1 < toks.len() && text(i) == "," && is_name(text(i + 1)) {
                out.push(Binding {
                    name: text(i + 1).to_string(),
                    prefix: prefix.clone(),
                    doc: doc.clone(),
                });
                i += 2;
            }
        }
    }

    out
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    fn prefix_of(src: &str, name: &str) -> Option<String> {
        bindings(src)
            .into_iter()
            .find(|b| b.name == name)
            .map(|b| b.prefix)
    }

    #[test]
    fn keywords_in_front_of_a_name() {
        let src = "const limit = 3\nexport const answer = 42\nasync function fetch_it(): number\nend\nlocal async function later() end\nexport function M.run() end\nlocal a, b = 1, 2\nlocal x = y\n";
        assert_eq!(prefix_of(src, "limit").as_deref(), Some("const"));
        assert_eq!(prefix_of(src, "answer").as_deref(), Some("export const"));
        assert_eq!(
            prefix_of(src, "fetch_it").as_deref(),
            Some("async function")
        );
        assert_eq!(
            prefix_of(src, "later").as_deref(),
            Some("local async function")
        );
        assert_eq!(prefix_of(src, "run").as_deref(), Some("export function"));
        assert_eq!(prefix_of(src, "M.run").as_deref(), Some("export function"));
        assert_eq!(prefix_of(src, "b").as_deref(), Some("local"));
        assert_eq!(prefix_of(src, "y"), None);
    }

    #[test]
    fn binding_doc_comes_from_the_lines_above() {
        let src = "--- Adds one.\nlocal function inc(n) end\n\n-- not attached\n\nconst k = 1\n";
        let b = bindings(src);
        assert_eq!(b[0].doc.as_deref(), Some("Adds one."));
        assert_eq!(b[1].doc, None);
    }

    #[test]
    fn a_type_annotation_is_not_a_path() {
        let src = "local part: Partial<{ x: number }> = {}\n";
        let b = bindings(src);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name, "part");
    }

    #[test]
    fn a_type_function_keeps_both_words() {
        let src = "type Alias = number\ntype function Keys(t)\n    return t\nend\n";
        assert_eq!(prefix_of(src, "Keys").as_deref(), Some("type function"));
        assert_eq!(prefix_of(src, "Alias"), None);
    }

    #[test]
    fn a_word_in_a_comment_is_not_a_keyword() {
        let src = "-- export sits where local sits\nlocal async function later() end\nlocal s = \"const x\"\n";
        assert_eq!(
            prefix_of(src, "later").as_deref(),
            Some("local async function")
        );
        assert_eq!(prefix_of(src, "x"), None);
    }
}
