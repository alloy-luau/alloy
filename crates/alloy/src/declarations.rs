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

/// One `.d.aly` inside the merged definitions file: the file, and the
/// zero-based line its text starts on there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub source: std::path::PathBuf,
    pub first_line: usize,
}

/// The compiled `.d.aly` files of a project as definitions files, each
/// part as its path, its artifact, and its source. Luau-lsp loads its
/// definitions in no set order, each against the globals before it, so
/// a type one file names from another was unknown, and that dropped the
/// whole file. Files that name each other merge into one, where order
/// does not matter. A file that names no other stays apart, so its
/// mistake drops no other file.
pub fn merge_definitions(
    parts: &[(std::path::PathBuf, String, String)],
) -> Vec<(String, Vec<Segment>)> {
    let declared: Vec<Vec<String>> = parts
        .iter()
        .map(|(_, _, s)| summaries(s, true).into_iter().map(|d| d.name).collect())
        .collect();
    let words: Vec<std::collections::HashSet<&str>> = parts
        .iter()
        .map(|(_, _, s)| {
            s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .collect()
        })
        .collect();
    let names = |i: usize, j: usize| declared[j].iter().any(|n| words[i].contains(n.as_str()));
    // Each part's group, as the lowest part it reaches.
    let mut group: Vec<usize> = (0..parts.len()).collect();

    // ponytail: a quadratic pass per join, fine for the few files a
    // project declares; a union-find if one ever holds hundreds.
    let mut joined = true;

    while joined {
        joined = false;

        for i in 0..parts.len() {
            for j in 0..parts.len() {
                if group[i] != group[j] && (names(i, j) || names(j, i)) {
                    let (low, high) = (group[i].min(group[j]), group[i].max(group[j]));
                    group
                        .iter_mut()
                        .filter(|g| **g == high)
                        .for_each(|g| *g = low);
                    joined = true;
                }
            }
        }
    }

    let mut out: Vec<(usize, String, Vec<Segment>)> = Vec::new();

    for (i, (source, part, _)) in parts.iter().enumerate() {
        let at = match out.iter().position(|(g, _, _)| *g == group[i]) {
            Some(at) => at,

            None => {
                out.push((group[i], String::new(), Vec::new()));
                out.len() - 1
            }
        };
        let (_, text, segments) = &mut out[at];
        segments.push(Segment {
            source: source.clone(),
            first_line: text.matches('\n').count(),
        });
        text.push_str(part);

        if !part.ends_with('\n') {
            text.push('\n');
        }
    }

    out.into_iter().map(|(_, t, s)| (t, s)).collect()
}

/// The file a zero-based line of the merged definitions file came
/// from, and the line in that file.
pub fn segment_at(segments: &[Segment], line: usize) -> Option<(&Segment, usize)> {
    segments
        .iter()
        .rev()
        .find(|s| s.first_line <= line)
        .map(|s| (s, line - s.first_line))
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
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let stmts = &parsed.chunk.block.stmts;

    // Target -> (traits, methods), from every impl in the file. A
    // method reads as the line an author would write for it, so a
    // struct hovers the way a namespace does.
    let mut impls: HashMap<&str, (Vec<&str>, Vec<String>)> = HashMap::new();

    for stmt in stmts {
        if let Stmt::Impl(i) = stmt {
            let entry = impls.entry(text(i.target)).or_default();

            if let Some(t) = i.trait_name {
                entry.0.push(text(t));
            }

            for m in &i.methods {
                // A private method is out of reach for every reader of
                // the hover, and completion already leaves it out.
                if m.visibility.is_some_and(|v| text(v) == "private") {
                    continue;
                }

                if let Some(line) = method_signature(src, toks, m) {
                    entry.1.push(line);
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
                // The span starts at an attribute above the name, which is
                // not part of the qualified name.
                let written = text(TokSpan {
                    start: v.name.start,
                    end: v.span.end,
                });
                let mut hover = format!(
                    "```alloy\n{enum_name}.{written}\n```\nA variant of `enum {enum_name}`."
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

            // The name, past `declare function` or `declare class`, so a
            // jump lands on the word it came from.
            Stmt::Declare(d) => {
                let whole = text(d.span);
                let at = declared_name(whole)
                    .map_or(0, |n| n.as_ptr() as usize - whole.as_ptr() as usize);

                start_of(d.span) + at
            }

            _ => 0,
        };
        let (name, mut lines) = match stmt {
            Stmt::Struct(d) => {
                let name = text(d.name);
                let generics = d.generics.map(text).unwrap_or("");
                let mut lines = vec![format!("{}struct {name}{generics}", export(d.exported))];
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
                    "{}interface {name}{generics}{extends}",
                    export(d.exported)
                )];
                lines.extend(d.fields.iter().map(|f| format!("    {}", text(f.span))));
                lines.push("end".to_string());

                (name, lines)
            }

            Stmt::Enum(d) => {
                let name = text(d.name);
                let generics = d.generics.map(text).unwrap_or("");
                let mut lines = vec![format!("{}enum {name}{generics}", export(d.exported))];
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

                // The contract, one line per clause. A reader asks what
                // the attribute holds them to, and the clause is the
                // answer, in the words the declaration wrote.
                if !d.requires.is_empty() {
                    let clauses: Vec<String> = d
                        .requires
                        .iter()
                        .map(|c| format!("- `{}`", require_text(c, &text)))
                        .collect();
                    notes_first.push(format!("\n\n**Requires**\n{}", clauses.join("\n")));
                }

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

            // The methods stand in a block of their own, under the
            // declaration. A method is no field, so a body the source
            // left empty reads empty here too.
            if !methods.is_empty() {
                let generics = lines
                    .first()
                    .map(|l| l.strip_suffix(" as").unwrap_or(l))
                    .and_then(|l| l.split_once(name))
                    .map_or(String::new(), |(_, tail)| tail.to_string());

                lines.push(String::new());
                lines.push(format!("impl {name}{generics}"));
                lines.extend(methods.iter().map(|line| format!("    {line}")));
                lines.push("end".to_string());
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

    // A namespace and each of its members. The member reads under the
    // path the source writes, `Math.Vec2`, and under the name the emit
    // gives it, `Math_Vec2`, so a hover on either finds it.
    for stmt in stmts {
        if let Stmt::Namespace(ns) = stmt.under_default() {
            namespace_summaries(src, toks, ns, "", &mut out);
        }
    }

    out
}

/// The hover entries of one namespace: the header with its members
/// listed, then one entry per member under both of its names.
fn namespace_summaries(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    outer: &str,
    out: &mut Vec<Declaration>,
) {
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let start_of = |span: TokSpan| toks[span.start as usize].start as usize;
    let name = text(ns.name);
    let path = match outer.is_empty() {
        true => name.to_string(),

        false => format!("{outer}.{name}"),
    };
    let modifier = match ns.exported {
        true => "export ",

        false => "",
    };
    let mut members: Vec<String> = Vec::new();

    for m in &ns.members {
        let Some(member) = member_name(&m.stmt) else {
            continue;
        };
        let word = text(member);
        let private = m.is_private(src, toks);

        if let Some(line) = member_signature(src, toks, m) {
            members.push(line);
        }

        // The declaration as written, with the path in front of its
        // name, so the reader sees where the member lives. A function
        // shows its header alone; a body says nothing a hover needs.
        let body = text(m.stmt.span());
        let at = start_of(member) - start_of(m.stmt.span());
        // The span starts at an attribute line above the declaration.
        // The hover adds the `@derive` lines itself, so the head is the
        // declaration's own line.
        let own = &body[..at];
        let own = own.rfind('\n').map_or(own, |i| own[i + 1..].trim_start());
        // `local` and `const` take a bare name, never a path, so the
        // word goes and the path stands alone: `Outer.VERSION = 1`.
        let head = own.trim_end();
        let head = head
            .strip_suffix("local")
            .or_else(|| head.strip_suffix("const"))
            .map(str::trim_end)
            .unwrap_or(own);
        let shown = format!("{head}{path}.{}", &body[at..]);
        let shown = match m.stmt.under_default() {
            Stmt::Function(_) | Stmt::LocalFunction(_) => {
                shown.lines().next().unwrap_or(&shown).to_string()
            }

            _ => dedent(&shown),
        };
        let mut lines = vec!["```alloy".to_string()];
        lines.extend(capped(&shown));
        lines.push("```".to_string());

        if private {
            lines.push(format!("\n`{word}` is private to `{path}`."));
        }

        let mut hover = lines.join("\n");

        if let Some(d) = doc_before(src, start_of(m.stmt.span())) {
            hover.push_str("\n\n");
            hover.push_str(&d);
        }

        let offset = start_of(member);
        out.push(Declaration {
            name: format!("{path}.{word}"),
            hover: hover.clone(),
            offset,
        });
        out.push(Declaration {
            name: format!("{}_{word}", path.replace('.', "_")),
            hover,
            offset,
        });

        if let Stmt::Namespace(inner) = m.stmt.under_default() {
            namespace_summaries(src, toks, inner, &path, out);
        }
    }

    // `@deprecated("use Geometry")` has no Luau form on a namespace, so
    // the hover is where the reader meets it.
    let deprecated = ns.attributes.iter().find_map(|a| {
        if a.name.map(text) != Some("deprecated") {
            return None;
        }

        let note = a
            .args
            .first()
            .map(|e| text(e.span()).trim_matches(['"', '\'']).to_string())
            .filter(|t| !t.is_empty());

        Some(match note {
            Some(t) => format!("\n\n**Deprecated.** {t}"),

            None => "\n\n**Deprecated.**".to_string(),
        })
    });
    let deprecated = deprecated.unwrap_or_default();
    // The block reads the way a struct hover reads: the header, one
    // line per member, then `end`. A long namespace stops at the cap,
    // since a hover the reader has to scroll says less than a short one.
    let shown = members.len().min(MEMBER_CAP);
    let mut lines = vec![format!("```alloy\n{modifier}namespace {path}")];
    lines.extend(members.iter().take(shown).cloned());

    if members.len() > shown {
        lines.push(format!("    ... and {} more", members.len() - shown));
    }

    lines.push("end\n```".to_string());
    let mut hover = format!("{}{deprecated}", lines.join("\n"));

    if let Some(d) = doc_before(src, start_of(ns.span)) {
        hover.push_str("\n\n");
        hover.push_str(&d);
    }

    out.push(Declaration {
        name: path.clone(),
        hover: hover.clone(),
        offset: start_of(ns.name),
    });

    // A nested namespace answers to its own name too, the way a
    // top-level one does.
    if !outer.is_empty() {
        out.push(Declaration {
            name: name.to_string(),
            hover,
            offset: start_of(ns.name),
        });
    }
}

/// One namespace of a source, with the byte range its body covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceSpan {
    /// The path the source writes: `Math`, `Outer.Inner`.
    pub path: String,
    pub start: usize,
    pub end: usize,
    /// Each member: its name and whether it is private.
    pub members: Vec<(String, bool)>,
    /// The module exposes the group, so another file may name it.
    pub exported: bool,
}

/// Every namespace of a source, outermost first, with the members each
/// one declares. The editor reads it: a name inside the namespace
/// completes without the path, and outside it takes the path.
pub fn namespace_ranges(src: &str) -> Vec<NamespaceSpan> {
    if !src.contains("namespace") {
        return Vec::new();
    }

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if let Stmt::Namespace(ns) = stmt.under_default() {
            namespace_range(src, toks, ns, "", &mut out);
        }
    }

    out
}

fn namespace_range(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    outer: &str,
    out: &mut Vec<NamespaceSpan>,
) {
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let name = text(ns.name);
    let path = match outer.is_empty() {
        true => name.to_string(),

        false => format!("{outer}.{name}"),
    };
    let mut members = Vec::new();

    for m in &ns.members {
        if let Some(member) = member_name(&m.stmt) {
            members.push((text(member).to_string(), m.is_private(src, toks)));
        }

        if let Stmt::Namespace(inner) = m.stmt.under_default() {
            namespace_range(src, toks, inner, &path, out);
        }
    }

    out.push(NamespaceSpan {
        path,
        start: toks[ns.span.start as usize].start as usize,
        end: toks[ns.span.end as usize - 1].end as usize,
        members,
        exported: ns.exported,
    });
}

/// Every namespace member of a source, as the pair the reader needs:
/// the name the emit writes and the path the source wrote. `Math_Vec2`
/// reads as `Math.Vec2`.
pub fn namespace_names(src: &str) -> Vec<(String, String)> {
    if !src.contains("namespace") {
        return Vec::new();
    }

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        if let Stmt::Namespace(ns) = stmt.under_default() {
            namespace_pairs(src, toks, ns, "", &mut out);
        }
    }

    out
}

fn namespace_pairs(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    outer: &str,
    out: &mut Vec<(String, String)>,
) {
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let name = text(ns.name);
    let path = match outer.is_empty() {
        true => name.to_string(),

        false => format!("{outer}.{name}"),
    };

    for m in &ns.members {
        let Some(member) = member_name(&m.stmt) else {
            continue;
        };
        let word = text(member);

        if let Stmt::Namespace(inner) = m.stmt.under_default() {
            namespace_pairs(src, toks, inner, &path, out);

            continue;
        }

        out.push((
            format!("{}_{word}", path.replace('.', "_")),
            format!("{path}.{word}"),
        ));
    }
}

/// How many members a namespace hover lists before it stops counting.
const MEMBER_CAP: usize = 24;

/// One member of a namespace, as the hover writes it: the visibility
/// word, then the declaration's head. A body says nothing the list
/// needs, so a struct reads as `public struct Vec2` and a function as
/// its signature alone.
/// One method of an `impl`, as the line an author writes for it:
/// `public function len(self): number`. The hover of the type the
/// `impl` targets reads these inside its block.
pub fn method_signature(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    m: &alloy_syntax::ast::Function,
) -> Option<String> {
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let visibility = match m.visibility.map(text) {
        Some("private") => "private",

        _ => "public",
    };
    let name = text(*m.path.first()?);
    let word = match m.body.is_async {
        Some(_) => "async function",

        None => "function",
    };
    let generics = m.body.generics.map(text).unwrap_or("");
    let params: Vec<String> = m.body.params.iter().map(|p| param_text(p, &text)).collect();
    let ret = match m.body.ret_type {
        Some(t) => format!(": {}", text(t)),

        None => String::new(),
    };

    Some(format!(
        "{visibility} {word} {name}{generics}({}){ret}",
        params.join(", ")
    ))
}

/// The head of a function as a hover reads it: the word, the name,
/// its generics, its parameters, and the return type.
pub fn head_of_function<'a>(
    name: &str,
    body: &alloy_syntax::ast::FunctionBody,
    text: &impl Fn(TokSpan) -> &'a str,
) -> String {
    let word = match body.is_async {
        Some(_) => "async function",

        None => "function",
    };
    let generics = body.generics.map(text).unwrap_or("");
    let params: Vec<String> = body.params.iter().map(|p| param_text(p, text)).collect();
    let ret = match body.ret_type {
        Some(t) => format!(": {}", text(t)),

        None => String::new(),
    };

    format!("{word} {name}{generics}({}){ret}", params.join(", "))
}

/*
The declaration an exported name writes, as a hover reads it: the
keywords, the name, and the signature, with the byte offset of the
declaration for the comment above it.

A module the reader imports from declares the name; the emit binds it
here as a local, and the child types that local as `unknown`. The
module's own declaration is the answer.
*/
pub fn export_head(src: &str, name: &str) -> Option<(String, usize)> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let start_of = |span: TokSpan| toks[span.start as usize].start as usize;

    parsed.chunk.block.stmts.iter().find_map(|stmt| {
        match stmt.under_default() {
            Stmt::Function(f) if f.exported => {
                let own = *f.path.first()?;

                (f.path.len() == 1 && text(own) == name).then(|| {
                    (
                        format!("export {}", head_of_function(name, &f.body, &text)),
                        start_of(f.span),
                    )
                })
            }

            // `export local function f()` and `export const function f()`.
            Stmt::LocalFunction(f) if f.exported && text(f.name) == name => {
                let word = match f.is_const {
                    true => "const",

                    false => "local",
                };

                Some((
                    format!("export {word} {}", head_of_function(name, &f.body, &text)),
                    start_of(f.span),
                ))
            }

            _ => None,
        }
    })
}

fn member_signature(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    m: &alloy_syntax::ast::NamespaceMember,
) -> Option<String> {
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let visibility = match m.is_private(src, toks) {
        true => "private",

        false => "public",
    };
    let params_of = |params: &[alloy_syntax::ast::Param]| -> String {
        let list: Vec<String> = params.iter().map(|p| param_text(p, &text)).collect();

        list.join(", ")
    };
    let function_head = |name: &str, body: &alloy_syntax::ast::FunctionBody| -> String {
        head_of_function(name, body, &text)
    };
    let head = match m.stmt.under_default() {
        Stmt::Local(l) => {
            let word = match l.is_const {
                true => "const",

                false => "local",
            };
            let binding = l.names.first()?;
            let name = text(binding.name);
            // Without an annotation the value says the type, when the
            // value is a literal. Anything else needs the checker, and
            // a hover that guesses wrong reads worse than one that says
            // the name alone.
            let ty = binding
                .ty
                .map(|t| text(t).to_string())
                .or_else(|| l.values.first().and_then(|v| literal_type(v, &text)));

            match ty {
                Some(t) => format!("{word} {name}: {t}"),

                None => format!("{word} {name}"),
            }
        }

        Stmt::Function(f) => function_head(text(*f.path.first()?), &f.body),

        Stmt::LocalFunction(f) => function_head(text(f.name), &f.body),

        Stmt::Struct(d) => format!(
            "struct {}{}",
            text(d.name),
            d.generics.map(text).unwrap_or("")
        ),

        Stmt::Interface(d) => format!(
            "interface {}{}",
            text(d.name),
            d.generics.map(text).unwrap_or("")
        ),

        Stmt::Enum(d) => format!(
            "enum {}{}",
            text(d.name),
            d.generics.map(text).unwrap_or("")
        ),

        Stmt::Trait(d) => format!("trait {}", text(d.name)),

        Stmt::Class(d) => format!("class {}", text(d.name)),

        Stmt::Namespace(d) => format!("namespace {}", text(d.name)),

        Stmt::Macro(d) => format!("macro {}({})", text(d.name), params_of(&d.params)),

        Stmt::Attribute(d) => format!("attribute {}({})", text(d.name), params_of(&d.params)),

        Stmt::Remote(d) => {
            let word = match d.is_function {
                true => "remote function",

                false => "remote",
            };

            format!("{word} {}({})", text(d.name), params_of(&d.params))
        }

        // `type Id = number` is short enough to read whole; a longer
        // one stops at its first line.
        Stmt::TypeAlias(d) => {
            let whole = text(d.span);
            let line = whole.lines().next().unwrap_or(whole).trim();
            let line = line
                .strip_prefix("export ")
                .or_else(|| line.strip_prefix("global "))
                .unwrap_or(line);

            line.to_string()
        }

        _ => return None,
    };

    Some(format!("    {visibility} {head}"))
}

/// The type a top-level `local` or `const` binding holds: the
/// annotation it wrote, or the type its literal value writes. A file
/// that reaches the name from somewhere else reads it from here.
pub fn binding_type(src: &str, name: &str) -> Option<String> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let text = |span: TokSpan| span.text_or_empty(src, toks);

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Local(l) = stmt else {
            continue;
        };
        let Some(index) = l.names.iter().position(|b| text(b.name) == name) else {
            continue;
        };

        if let Some(ty) = l.names[index].ty {
            return Some(text(ty).trim().to_string());
        }

        return literal_type(l.values.get(index)?, &text);
    }

    None
}

/// The type a literal value writes, for a `const` with no annotation.
pub(crate) fn literal_type<'a>(
    value: &alloy_syntax::ast::Expr,
    text: &impl Fn(TokSpan) -> &'a str,
) -> Option<String> {
    use alloy_syntax::ast::Expr;

    match value {
        Expr::Number(_) => Some("number".to_string()),

        Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. } => Some("string".to_string()),

        Expr::True(_) | Expr::False(_) => Some("boolean".to_string()),

        // `const ORIGIN = { x = 1, y = 2 }` is a record of what its
        // values read as. One value with no literal type leaves the
        // whole record unnamed: half a record reads worse than none.
        Expr::Table { fields, .. } => {
            let mut parts = Vec::new();

            for field in fields {
                let alloy_syntax::ast::TableField::Named { name, value } = field else {
                    return None;
                };

                parts.push(format!("{}: {}", text(*name), literal_type(value, text)?));
            }

            (!parts.is_empty()).then(|| format!("{{ {} }}", parts.join(", ")))
        }

        // `const scheduler = new Sched { phase = 1 }` names its type
        // outright, a dotted `new Ns.T { }` included.
        Expr::New { name, .. } => Some(text(name.span()).to_string()),

        // `const size = Vector3.new(1, 2, 3)` names its own type.
        Expr::Call { func, method, .. } if method.is_none() => match func.as_ref() {
            Expr::Index { object, key, .. } => match (object.as_ref(), key) {
                (Expr::Name(n), alloy_syntax::ast::IndexKey::Field(k)) if text(*k) == "new" => {
                    Some(text(*n).to_string())
                }

                _ => None,
            },

            _ => None,
        },

        _ => None,
    }
}

/// A block read out of an indented body, with the indent of its last
/// line taken off every line after the first. The head already sits at
/// column zero; the rest came in with the namespace's own indent.
fn dedent(text: &str) -> String {
    let mut lines = text.lines();
    let Some(head) = lines.next() else {
        return text.to_string();
    };
    let rest: Vec<&str> = lines.collect();
    let indent = rest
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = String::from(head);

    for line in rest {
        out.push('\n');
        out.push_str(line.get(indent..).unwrap_or(line.trim_start()));
    }

    out
}

/// The name one namespace member binds.
fn member_name(stmt: &Stmt) -> Option<TokSpan> {
    match stmt.under_default() {
        Stmt::Function(f) if f.path.len() == 1 => f.path.first().copied(),

        Stmt::Attribute(a) => Some(a.name),

        other => other.declared_name(),
    }
}

/*
One `requires` clause as a hover reads it: the visibility, the kind, the
member, and the shape.

`requires private function each lifecycles(self)` keeps the `each`, and
the language server expands it against the arguments of a use. The
declaration knows no arguments.
*/
pub fn require_text<'a>(
    c: &alloy_syntax::ast::RequireClause,
    text: &impl Fn(TokSpan) -> &'a str,
) -> String {
    use alloy_syntax::ast::RequireMember;

    let mut out = String::new();

    if let Some(v) = c.visibility {
        out.push_str(text(v));
        out.push(' ');
    }

    out.push_str(text(c.kind));
    out.push(' ');

    match c.member {
        RequireMember::Name(n) => out.push_str(text(n)),

        RequireMember::Each(n) => {
            out.push_str("each ");
            out.push_str(text(n));
        }
    }

    if let Some(shape) = c.shape {
        let shape = text(shape).trim();

        if text(c.kind) == "field" {
            out.push_str(": ");
        }

        out.push_str(shape);
    }

    out
}

/*
The parameters a declared attribute's hover names, each with its type as
the declaration wrote it.

The hover opens with the use form, `@provider(lifecycles: Lifecycle[])`,
which is the one place the parameter types are already gathered for every
attribute a file reaches: its own, an imported one, and a global.
*/
pub fn attribute_params(hover: &str) -> Vec<(String, String)> {
    let Some(line) = hover
        .lines()
        .find(|l| l.starts_with('@'))
        .and_then(|l| l.split_once('('))
        .map(|(_, rest)| rest)
    else {
        return Vec::new();
    };
    let Some(list) = line.strip_suffix(')') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();

    for c in list.chars() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,

            ')' | '>' | ']' | '}' => depth -= 1,

            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));

                continue;
            }

            _ => {}
        }

        current.push(c);
    }

    out.push(current);
    out.into_iter()
        .filter_map(|part| {
            let (name, ty) = part.split_once(':')?;

            Some((name.trim().to_string(), ty.trim().to_string()))
        })
        .collect()
}

/// `name: T` for a parameter, or `name` alone.
fn param_text<'a>(p: &alloy_syntax::ast::Param, text: &impl Fn(TokSpan) -> &'a str) -> String {
    let head = match p.ty {
        Some(t) => format!("{}: {}", text(p.name), text(t)),

        None => text(p.name).to_string(),
    };

    // `b = 2`: the default is part of the signature the reader wrote.
    match &p.default {
        Some(d) => format!("{head} = {}", text(d.span())),

        None => head,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field type is portable when the other file can write it; one
    /// that names a type or an import of the module is not, since the
    /// name may mean nothing in the other file. A std type an import of
    /// the std binds is the ambient one. A generic struct gives no field.
    #[test]
    fn a_field_type_is_portable_when_the_other_file_can_write_it() {
        let src = "import { HashMap } from \"@alloy/std/collections\"
import { Entry } from \"./entry\"
type Id = number
export struct Ballot
    votes: HashMap<string, Player>
    rows: HashMap<string, Entry>
    ids: HashMap<Id, string>
end
export struct Box<T>
    items: HashMap<string, T>
end
";
        let got = struct_field_types(src);

        let field = |f: &str, ty: &str, portable: bool| (f.to_string(), ty.to_string(), portable);

        assert_eq!(
            got,
            vec![
                (
                    "Ballot".to_string(),
                    vec![
                        field("votes", "HashMap<string, Player>", true),
                        field("rows", "HashMap<string, Entry>", false),
                        field("ids", "HashMap<Id, string>", false),
                    ]
                ),
                ("Box".to_string(), vec![]),
            ]
        );
    }

    /// Luau-lsp loads definitions files in no set order, so a type one
    /// `.d.aly` named from another was unknown. Files that name each
    /// other merge into one, each line still names its file, and a file
    /// that names no other stays apart.
    #[test]
    fn declaration_files_that_name_each_other_merge() {
        let part = |path: &str, text: &str| {
            (
                std::path::PathBuf::from(path),
                text.to_string(),
                text.to_string(),
            )
        };
        let parts = vec![
            part("a.d.aly", "declare function make(): Save\n"),
            part("b.d.aly", "declare other: number\n"),
            part("z.d.aly", "export type Save = { coins: number }"),
        ];
        let merged = merge_definitions(&parts);

        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].0,
            "declare function make(): Save\nexport type Save = { coins: number }\n"
        );
        assert_eq!(merged[1].0, "declare other: number\n");
        assert_eq!(
            segment_at(&merged[0].1, 1).map(|(s, l)| (s.source.clone(), l)),
            Some(("z.d.aly".into(), 0))
        );
    }

    /// A jump to a declared global landed on `declare f`, the first
    /// letters of the statement, and not on the name.
    #[test]
    fn a_declare_points_at_its_name() {
        let src = "declare function warn_once(m: string): ()\ndeclare class Sword\n    damage: number\nend\n";
        let at: Vec<(String, &str)> = summaries(src, true)
            .into_iter()
            .map(|d| {
                let word = &src[d.offset..d.offset + d.name.len()];

                (d.name, word)
            })
            .collect();

        assert_eq!(
            at,
            vec![
                ("warn_once".to_string(), "warn_once"),
                ("Sword".to_string(), "Sword")
            ]
        );
    }

    #[test]
    fn struct_with_impls() {
        let src = "export struct Vec2 as\n    x: number\n    y: number = 0\nend\nimpl Vec2 as\n    function len(self) end\nend\nimpl Display for Vec2 as\n    function to_string(self) end\nend\n";
        let d = summaries(src, false);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "Vec2");
        assert_eq!(
            d[0].hover,
            "```alloy\nexport struct Vec2\n    x: number\n    y: number = 0\nend\n\nimpl Vec2\n    public function len(self)\n    public function to_string(self)\nend\n```\n\nImplements `Display`."
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
    fn an_attribute_above_a_variant_stays_out_of_its_name() {
        let src = "attribute icon(asset: string) on variant\nenum Drop as\n    @icon(\"coin\")\n    Coin\nend\n";
        let d = summaries(src, false);
        let coin = d.iter().find(|x| x.name == "Coin").expect("the variant");
        assert_eq!(
            coin.hover,
            "```alloy\nDrop.Coin\n```\nA variant of `enum Drop`."
        );
    }

    #[test]
    fn a_namespace_local_hovers_as_a_path() {
        let src = "namespace Outer as\n    public local VERSION = 1\n    const LIMIT = 2\nend\n";
        let d = summaries(src, false);
        let v = d
            .iter()
            .find(|x| x.name == "Outer.VERSION")
            .expect("the member");
        assert!(v.hover.contains("Outer.VERSION = 1"), "{}", v.hover);
        assert!(!v.hover.contains("local Outer"), "{}", v.hover);
        let l = d
            .iter()
            .find(|x| x.name == "Outer.LIMIT")
            .expect("the const");
        assert!(l.hover.contains("Outer.LIMIT = 2"), "{}", l.hover);
        assert!(!l.hover.contains("const Outer"), "{}", l.hover);
    }

    #[test]
    fn a_macro_hover_keeps_a_parameter_default() {
        let src = "export macro withDefault(a, b = 2)\n    a + b\nend\n";
        let d = summaries(src, false);
        let m = d
            .iter()
            .find(|x| x.name == "$withDefault")
            .expect("the macro");
        assert!(
            m.hover.contains("export macro withDefault(a, b = 2)"),
            "{}",
            m.hover
        );
    }

    #[test]
    fn a_namespace_and_its_members_hover() {
        let src = "-- Numbers.\nexport namespace Math as\n    const PI = 3.14\n    private const E = 2.7\n    struct Vec2 as\n        x: number\n    end\nend\n";
        let d = summaries(src, false);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"Math"), "{names:?}");
        assert!(names.contains(&"Math.PI"), "{names:?}");
        assert!(names.contains(&"Math_PI"), "{names:?}");
        assert!(names.contains(&"Math.Vec2"), "{names:?}");
        assert!(names.contains(&"Math_Vec2"), "{names:?}");

        let ns = d.iter().find(|x| x.name == "Math").unwrap();
        // The block reads the way a struct hover reads, and a private
        // member says so on its own line.
        assert_eq!(
            ns.hover,
            "```alloy\nexport namespace Math\n    public const PI: number\n    private const E: number\n    public struct Vec2\nend\n```\n\nNumbers."
        );

        let vec2 = d.iter().find(|x| x.name == "Math.Vec2").unwrap();
        assert_eq!(
            vec2.hover,
            "```alloy\nstruct Math.Vec2 as\n    x: number\nend\n```"
        );

        let e = d.iter().find(|x| x.name == "Math.E").unwrap();
        assert!(e.hover.contains("`E` is private to `Math`."), "{}", e.hover);
    }

    #[test]
    fn every_member_kind_reads_as_its_signature() {
        let src = "export namespace Big as\n    public function helper(x: number): number\n        return x\n    end\n    public enum Kind as\n        A\n    end\n    public type Id = number\n    public interface Named as\n        name: string\n    end\n    public trait Show as\n        function show(self): string\n    end\n    public namespace Inner as\n        const B = 2\n    end\n    public const NAME = \"a\"\n    public const ON = true\nend\n";
        let d = summaries(src, false);
        let ns = d.iter().find(|x| x.name == "Big").unwrap();
        assert_eq!(
            ns.hover,
            "```alloy\nexport namespace Big\n    public function helper(x: number): number\n    public enum Kind\n    public type Id = number\n    public interface Named\n    public trait Show\n    public namespace Inner\n    public const NAME: string\n    public const ON: boolean\nend\n```"
        );
    }

    #[test]
    fn a_long_namespace_stops_at_the_cap() {
        let members: String = (0..30).map(|i| format!("    const C{i} = {i}\n")).collect();
        let src = format!("namespace Many as\n{members}end\n");
        let d = summaries(&src, false);
        let ns = d.iter().find(|x| x.name == "Many").unwrap();
        assert_eq!(
            ns.hover
                .lines()
                .filter(|l| l.contains("public const"))
                .count(),
            24
        );
        assert!(ns.hover.contains("    ... and 6 more"), "{}", ns.hover);
    }

    #[test]
    fn a_deprecated_namespace_says_so_in_its_hover() {
        let src = "@deprecated(\"use Geometry\")\nnamespace Old as\n    const x = 1\nend\n";
        let d = summaries(src, false);
        let ns = d.iter().find(|x| x.name == "Old").unwrap();
        assert!(
            ns.hover.contains("**Deprecated.** use Geometry"),
            "{}",
            ns.hover
        );
    }

    #[test]
    fn a_function_member_hovers_by_its_header() {
        let src =
            "namespace M\n    function f(x: number): number\n        return x\n    end\nend\n";
        let d = summaries(src, false);
        let f = d.iter().find(|x| x.name == "M.f").unwrap();
        assert_eq!(f.hover, "```alloy\nfunction M.f(x: number): number\n```");
    }

    #[test]
    fn the_ranges_carry_the_body_and_the_members() {
        let src = "export namespace Math as\n    const PI = 3.14\n    private const seed = 7\nend\n\nprint(Math.PI)\n";
        let r = namespace_ranges(src);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].path, "Math");
        assert!(r[0].exported);
        assert_eq!(
            r[0].members,
            vec![("PI".to_string(), false), ("seed".to_string(), true)]
        );
        assert_eq!(&src[r[0].start..r[0].start + 6], "export");
        assert_eq!(&src[r[0].end - 3..r[0].end], "end");
    }

    #[test]
    fn a_nested_namespace_reads_its_whole_path() {
        let src = "namespace Outer as\n    namespace Inner as\n        const B = 2\n    end\nend\n";
        let d = summaries(src, false);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert!(names.contains(&"Outer.Inner"), "{names:?}");
        assert!(names.contains(&"Inner"), "{names:?}");
        assert!(names.contains(&"Outer.Inner.B"), "{names:?}");
        assert!(names.contains(&"Outer_Inner_B"), "{names:?}");
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
                .contains("interface Entity extends Named, Positioned\n    id: number\nend")
        );
    }

    /// `enum Opt<T>` hovers with its parameter list, and a variant
    /// keeps the payload text that names the parameter.
    #[test]
    fn a_generic_enum_hovers_with_its_parameters() {
        let src = "export enum Opt<T> as\n    Some(T)\n    Nil\nend\n";
        let d = summaries(src, false);
        let opt = d.iter().find(|x| x.name == "Opt").unwrap();

        assert!(
            opt.hover
                .contains("export enum Opt<T>\n    Some(T)\n    Nil\nend"),
            "{}",
            opt.hover
        );
    }

    #[test]
    fn enum_and_trait() {
        let src = "enum Msg as\n    Quit\n    Move(number, number)\nend\ntrait Shape as\n    function area(self): number\nend\n";
        let d = summaries(src, false);
        let find = |name: &str| d.iter().find(|x| x.name == name).unwrap();
        assert!(
            find("Msg")
                .hover
                .contains("enum Msg\n    Quit\n    Move(number, number)\nend")
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

const DECL_WORDS: [&str; 7] = [
    "global", "export", "local", "const", "async", "function", "type",
];

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

    /// A declaration with no annotation still has a type when its
    /// value is a literal. A file that reaches the name from another
    /// module reads it from here.
    #[test]
    fn a_binding_takes_the_type_its_value_writes() {
        let src = "local limit = 3\nconst name: string = read()\nconst made = build()\n";
        assert_eq!(binding_type(src, "limit").as_deref(), Some("number"));
        assert_eq!(binding_type(src, "name").as_deref(), Some("string"));
        assert_eq!(binding_type(src, "made"), None);
        assert_eq!(binding_type(src, "gone"), None);
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

/// The shape of a struct or an enum, for a reader that folds the
/// checker's printed types back to their names: the fields a struct's
/// instance table shows, and the variants of an enum with their
/// payload types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    Struct {
        name: String,
        /// Every field, in order, with whether it is private.
        fields: Vec<(String, bool)>,
        /// `struct Slotted<T>`: the parameter names, in order.
        generics: Vec<String>,
        /// The declared type of each field, as source text, parallel to
        /// `fields`. A print names the struct's arguments through them.
        types: Vec<String>,
    },
    Enum {
        name: String,
        /// `enum Opt<T>`: the parameter names, in order. A default stays
        /// on its name, `R = string`, so a fold can fill it. A payload
        /// spelled as a parameter carries its argument.
        generics: Vec<String>,
        /// Each variant with its payload types as source text.
        variants: Vec<(String, Vec<String>)>,
    },
    /// `type Snapshot = Readonly<Profile>`: a mapped type over a
    /// struct, so a hover names the alias.
    Alias { name: String, target: String },
}

impl Shape {
    pub fn name(&self) -> &str {
        match self {
            Shape::Struct { name, .. } | Shape::Enum { name, .. } | Shape::Alias { name, .. } => {
                name
            }
        }
    }
}

/// The names in a generic list: `<T, U: Shape>` gives `T` and `U`. A
/// default stays on its name, `B = string`, so a fold can fill it.
fn generic_names(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches('<').trim_end_matches('>');

    crate::shapes::top_level_parts(inner)
        .iter()
        .map(|item| item.split(':').next().unwrap_or("").trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// The structs and enums a source declares.
pub fn shapes(src: &str) -> Vec<Shape> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        // `export default struct P` declares `P` like any other.
        match stmt.under_default() {
            Stmt::Struct(s) => out.push(Shape::Struct {
                name: text(s.name),
                fields: s
                    .fields
                    .iter()
                    .map(|f| {
                        let private = f.visibility.is_some_and(|v| text(v) == "private");

                        (text(f.name), private)
                    })
                    .collect(),
                generics: s
                    .generics
                    .map(|g| generic_names(&text(g)))
                    .unwrap_or_default(),
                types: s.fields.iter().map(|f| text(f.ty)).collect(),
            }),

            Stmt::Enum(e) => out.push(Shape::Enum {
                name: text(e.name),
                generics: e
                    .generics
                    .map(|g| generic_names(&text(g)))
                    .unwrap_or_default(),
                variants: e
                    .variants
                    .iter()
                    .map(|v| (text(v.name), v.payload.iter().map(|p| text(*p)).collect()))
                    .collect(),
            }),

            Stmt::TypeAlias(a) => {
                let whole = text(a.span);
                let Some((_, rhs)) = whole.split_once('=') else {
                    continue;
                };
                let target = rhs.trim();

                if is_mapped_over_name(target) {
                    out.push(Shape::Alias {
                        name: text(a.name),
                        target: target.to_string(),
                    });
                }
            }

            // A namespace member has no top-level declaration, so an
            // enum inside one reads under its path, `Geo.Kind`, the way
            // `struct_fields_by_name` lists a struct member.
            Stmt::Namespace(ns) => namespace_enums(src, toks, ns, "", &mut out),

            _ => {}
        }
    }

    out
}

/// The enums one namespace declares, under the path each one reads by.
fn namespace_enums(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    ns: &alloy_syntax::ast::NamespaceDecl,
    path: &str,
    out: &mut Vec<Shape>,
) {
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();
    let inner = match path.is_empty() {
        true => text(ns.name),

        false => format!("{path}.{}", text(ns.name)),
    };

    for m in &ns.members {
        match m.stmt.under_default() {
            Stmt::Enum(e) => out.push(Shape::Enum {
                name: format!("{inner}.{}", text(e.name)),
                generics: e
                    .generics
                    .map(|g| generic_names(&text(g)))
                    .unwrap_or_default(),
                variants: e
                    .variants
                    .iter()
                    .map(|v| (text(v.name), v.payload.iter().map(|p| text(*p)).collect()))
                    .collect(),
            }),

            Stmt::Namespace(deeper) => namespace_enums(src, toks, deeper, &inner, out),

            _ => {}
        }
    }
}

/// One struct under one of its names: the name, then each field with
/// whether it carries a default, whether it is private, and its type
/// text. A generic struct gives an empty type text, since a field type
/// may name the struct's own parameters.
type StructFields = (String, Vec<(String, bool, bool, String)>);

/// Every struct a source declares, under each name a module that
/// imports it writes, with each field's default and its visibility.
///
/// A struct inside a namespace reads under its path, `Zoo.Box`, and
/// under the name the emit gives it, `Zoo_Box`, the way `declarations`
/// lists a member under both. Without the path the field checks of an
/// imported namespace member have no shape to read.
fn struct_fields_by_name(src: &str) -> Vec<StructFields> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        struct_fields_of(src, toks, stmt, "", &mut out);
    }

    out
}

/// One statement's structs, for `struct_fields_by_name`: a `struct`
/// under `path`, or the members of a `namespace` under the path it
/// extends.
fn struct_fields_of(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    stmt: &Stmt,
    path: &str,
    out: &mut Vec<StructFields>,
) {
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();

    match stmt.under_default() {
        Stmt::Struct(s) => {
            let fields: Vec<(String, bool, bool, String)> = s
                .fields
                .iter()
                .map(|f| {
                    let ty = text(f.ty).trim().to_string();

                    (
                        text(f.name),
                        field_can_stay_unset(&ty, f.default.is_some()),
                        f.visibility.is_some_and(|v| text(v) == "private"),
                        if s.generics.is_some() {
                            String::new()
                        } else {
                            ty
                        },
                    )
                })
                .collect();
            let name = text(s.name);

            if path.is_empty() {
                out.push((name, fields));

                return;
            }

            out.push((format!("{path}.{name}"), fields.clone()));
            out.push((format!("{}_{name}", path.replace('.', "_")), fields));
        }

        Stmt::Namespace(ns) => {
            let inner = match path.is_empty() {
                true => text(ns.name),

                false => format!("{path}.{}", text(ns.name)),
            };

            for m in &ns.members {
                struct_fields_of(src, toks, &m.stmt, &inner, out);
            }
        }

        _ => {}
    }
}

/// Every struct a source writes a constructor for: the struct's name
/// with the `new` or `New` its `impl` declares. A check of a
/// construction of a struct another module declares reads this, so a
/// report names the constructor instead of saying the struct writes
/// none.
///
/// A struct inside a namespace reads under both its names, the way
/// `struct_fields_by_name` lists one.
pub fn struct_ctors(src: &str) -> Vec<(String, String)> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        struct_ctors_of(src, toks, stmt, "", &mut out);
    }

    out
}

/// One statement's constructors, for `struct_ctors`: an `impl` under
/// `path`, or the members of a `namespace` under the path it extends.
fn struct_ctors_of(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    stmt: &Stmt,
    path: &str,
    out: &mut Vec<(String, String)>,
) {
    let text = |span: alloy_syntax::ast::TokSpan| span.text(src, toks).to_string();

    match stmt.under_default() {
        // A trait `impl` writes the trait's methods, so a `new` there
        // is the trait's, not the struct's constructor.
        Stmt::Impl(i) if i.trait_name.is_none() => {
            let Some(ctor) = i.methods.iter().find_map(|m| {
                m.path
                    .first()
                    .map(|n| text(*n))
                    .filter(|n| matches!(n.as_str(), "new" | "New"))
            }) else {
                return;
            };
            let name = text(i.target);

            if path.is_empty() {
                out.push((name, ctor));

                return;
            }

            out.push((format!("{path}.{name}"), ctor.clone()));
            out.push((format!("{}_{name}", path.replace('.', "_")), ctor));
        }

        Stmt::Namespace(ns) => {
            let inner = match path.is_empty() {
                true => text(ns.name),

                false => format!("{path}.{}", text(ns.name)),
            };

            for m in &ns.members {
                struct_ctors_of(src, toks, &m.stmt, &inner, out);
            }
        }

        _ => {}
    }
}

/// Whether a field can stay unset in `new Name { }`: it carries a
/// default, or its type takes `nil`. `number?` and `nil | number` both
/// read as optional; a missing key is `nil` at run time either way.
pub fn field_can_stay_unset(ty: &str, has_default: bool) -> bool {
    let ty = ty.trim();

    has_default || ty.ends_with('?') || ty.split('|').any(|part| part.trim() == "nil")
}

/// Every struct a source declares, with each field and whether it
/// can stay unset. `new Name { }` needs a value for every field
/// without a default or a `nil` in its type, so the check of a
/// construction of a struct another module declares reads this.
pub fn struct_field_defaults(src: &str) -> Vec<(String, Vec<(String, bool)>)> {
    struct_fields_by_name(src)
        .into_iter()
        .map(|(name, fields)| {
            let fields = fields
                .into_iter()
                .map(|(f, default, _, _)| (f, default))
                .collect();

            (name, fields)
        })
        .collect()
}

/// Every struct a source declares that has a private field, with those
/// field names. The `private_access` lint reads one of an imported
/// struct through it.
pub fn struct_privates(src: &str) -> Vec<(String, Vec<String>)> {
    struct_fields_by_name(src)
        .into_iter()
        .filter_map(|(name, fields)| {
            let private: Vec<String> = fields
                .into_iter()
                .filter(|(_, _, p, _)| *p)
                .map(|(f, _, _, _)| f)
                .collect();

            (!private.is_empty()).then_some((name, private))
        })
        .collect()
}

/// One field of a struct: its name, its type text, and whether another
/// file can write that text.
pub type FieldText = (String, String, bool);

/// Every struct a source declares, with the type text of each field and
/// whether another file can write that text the way the source does. A
/// construction in that file reads it, so `new Ballot { votes =
/// HashMap.new() }` passes `<<string, string>>` to the constructor, as
/// the declaring file does.
///
/// A name the source binds, a type, an import, or a namespace, means
/// one thing there and may mean nothing in the other file, so a field
/// type that names one cannot be written there. A std type an import of
/// the std binds is the ambient one, and can. A generic struct gives no
/// field, since a field type may name the struct's own parameters.
pub fn struct_field_types(src: &str) -> Vec<(String, Vec<FieldText>)> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let stmts = &parsed.chunk.block.stmts;
    let mut own = crate::desugar::top_level_names(src, toks, &parsed.chunk);

    for stmt in stmts {
        match stmt.under_default() {
            Stmt::Namespace(n) => {
                own.insert(n.name.text(src, toks).to_string());
            }

            Stmt::Import(i) => {
                let spec = i.path.text(src, toks).trim_matches(['"', '\'', '`']);

                if crate::std_names::module_of_spec(spec).is_some() {
                    for n in crate::desugar::import_names(i) {
                        own.remove(n.text(src, toks));
                    }
                }
            }

            _ => {}
        }
    }

    let mut structs = Vec::new();

    for stmt in stmts {
        struct_fields_of(src, toks, stmt, "", &mut structs);
    }

    let names_own = |ty: &str| {
        ty.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| own.contains(word))
    };

    structs
        .into_iter()
        .map(|(name, fields)| {
            let typed = fields
                .into_iter()
                .filter(|(_, _, _, ty)| !ty.is_empty())
                .map(|(f, _, _, ty)| {
                    let portable = !names_own(&ty);

                    (f, ty, portable)
                })
                .collect();

            (name, typed)
        })
        .collect()
}

/// `Readonly<Profile>`, `Partial<Profile>`, or `Sink<Profile>`.
fn is_mapped_over_name(target: &str) -> bool {
    ["Readonly<", "Partial<", "Sink<"].iter().any(|head| {
        target
            .strip_prefix(head)
            .and_then(|rest| rest.strip_suffix('>'))
            .is_some_and(|inner| {
                !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
    })
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn structs_and_enums_report_their_shape() {
        let src = "struct S as\n    read x: number\n    private n: number = 0\nend\nenum E as\n    A\n    B(number, string)\nend\n";
        let got = shapes(src);
        assert_eq!(
            got,
            vec![
                Shape::Struct {
                    name: "S".into(),
                    fields: vec![("x".into(), false), ("n".into(), true)],
                    generics: vec![],
                    types: vec!["number".into(), "number".into()],
                },
                Shape::Enum {
                    name: "E".into(),
                    generics: vec![],
                    variants: vec![
                        ("A".into(), vec![]),
                        ("B".into(), vec!["number".into(), "string".into()])
                    ],
                },
            ]
        );
    }

    /// A generic enum and a struct name their parameters, and a
    /// default stays on its name for the fold that fills it.
    #[test]
    fn a_generic_enum_names_its_parameters() {
        let src = "enum Either<L, R = string> as\n    Left(L),\n    Right(R)\nend\nstruct Pair<A: Shape<X, Y>, B = string> as\n    read a: A\nend\n";
        let got = shapes(src);
        assert_eq!(
            got[0],
            Shape::Enum {
                name: "Either".into(),
                generics: vec!["L".into(), "R = string".into()],
                variants: vec![
                    ("Left".into(), vec!["L".into()]),
                    ("Right".into(), vec!["R".into()])
                ],
            }
        );
        let Shape::Struct { generics, .. } = &got[1] else {
            panic!("a struct");
        };
        assert_eq!(generics, &vec!["A".to_string(), "B = string".to_string()]);
    }

    /// A reader of another module sees the emitted local, which the
    /// child types as `unknown`. The head of the declaration is the
    /// answer, and a `const` carries the type its value names.
    #[test]
    fn an_exported_name_carries_its_head() {
        let src = "export const scheduler = new Sched { phase = 1 }\nexport const ORIGIN = { x = 1, y = 2 }\nexport const NAME = \"main\"\nexport local function build(): Sched\n    return new Sched { phase = 2 }\nend\n\nexport async function poll(): number\n    return 1\nend\n";
        let head = |name: &str| export_head(src, name).map(|(h, _)| h);

        assert_eq!(binding_type(src, "scheduler").as_deref(), Some("Sched"));
        assert_eq!(
            binding_type(src, "ORIGIN").as_deref(),
            Some("{ x: number, y: number }")
        );
        assert_eq!(binding_type(src, "NAME").as_deref(), Some("string"));
        assert_eq!(
            head("build").as_deref(),
            Some("export local function build(): Sched")
        );
        assert_eq!(
            head("poll").as_deref(),
            Some("export async function poll(): number")
        );

        // A value no literal names leaves the type out, and a `const`
        // is no function.
        assert_eq!(binding_type("const seed = os.time()\n", "seed"), None);
        assert_eq!(head("scheduler"), None);
    }
}
