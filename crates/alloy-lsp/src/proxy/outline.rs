/*!
The outline of an Alloy file, built from the source.

The child sees the check artifact, where a struct is a table with a
`__new` on it, an enum variant is a record with `_1` payload slots, and
a namespace member is one flat `MathX_triple`. An outline built from
that text names the emit and gives a struct the kind of a variable, and
it lists each type twice: once for the table and once for the type the
emit writes beside it.

The source says what the author declared, so the outline reads it
instead. Every entry carries the kind the construct has, the name as
written, and the range of the declaration.
*/

use alloy_syntax::ast::{DefaultExport, Stmt};
use alloy_syntax::lexer::Tok;
use serde_json::{Value, json};

use std::path::Path;

use super::{State, normalize, range_value, relative, uri_to_path};
use crate::doc::position_of;

// `SymbolKind` of the protocol.
const NAMESPACE: u8 = 3;
const CLASS: u8 = 5;
const METHOD: u8 = 6;
const PROPERTY: u8 = 7;
const FIELD: u8 = 8;
const ENUM: u8 = 10;
const INTERFACE: u8 = 11;
const FUNCTION: u8 = 12;
const VARIABLE: u8 = 13;
const CONSTANT: u8 = 14;
const STRUCT: u8 = 23;
const EVENT: u8 = 24;
const ENUM_MEMBER: u8 = 22;

/// The `declare Name: T` values of the open definitions files, for a
/// workspace symbol query. The child indexes the compiled definitions,
/// where that form binds no name it can point at; a `declare function`
/// it does index, and a name it already sent comes once.
pub(crate) fn ambient_symbols(st: &State, query: Option<&str>, out: &mut Vec<Value>) {
    let query = query.unwrap_or_default().to_lowercase();

    for (uri, doc) in &st.docs {
        if !uri.ends_with(".d.aly") {
            continue;
        }

        let Some(path) = uri_to_path(uri) else {
            continue;
        };

        for (name, (from, to)) in ambient_values(&doc.source, &path) {
            let known = out
                .iter()
                .any(|v| v.get("name").and_then(Value::as_str) == Some(name.as_str()));

            if known || !name.to_lowercase().contains(&query) {
                continue;
            }

            out.push(json!({
                "name": name,
                "kind": VARIABLE,
                "location": {
                    "uri": uri,
                    "range": range_value(position_of(&doc.source, from), position_of(&doc.source, to)),
                },
            }));
        }
    }
}

/// Every declaration of the workspace's Alloy sources, as the source
/// spells it. The child reads the check artifact, where `Ns.T` is
/// `Ns_T` and carries a `__new` and a `new` the author never wrote, so
/// an Alloy file answers a workspace query from its own outline. A
/// member reads under its owner: `Ns.T`, `Box.value`.
pub(crate) fn source_symbols(st: &State, query: Option<&str>, out: &mut Vec<Value>) {
    let query = query.unwrap_or_default().to_lowercase();

    for (uri, doc) in &st.docs {
        let Some(path) = uri_to_path(uri) else {
            continue;
        };
        let Some(items) = document_symbols(&doc.source, &path) else {
            continue;
        };
        // A file of another project reads under its path, so the
        // reader sees where the name lives.
        let container = match st.root.as_deref().map(normalize) {
            Some(root) if !normalize(&path).starts_with(&root) => relative(&root, &path),

            _ => String::new(),
        };

        flatten_symbols(&items, "", &container, uri, &query, out);
    }
}

/// The outline as a flat list, each entry named under its owner; a
/// top-level entry names `container` as its owner.
fn flatten_symbols(
    items: &[Value],
    owner: &str,
    container: &str,
    uri: &str,
    query: &str,
    out: &mut Vec<Value>,
) {
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let full = match owner.is_empty() {
            true => name.to_string(),

            false => format!("{owner}.{name}"),
        };

        if full.to_lowercase().contains(query) {
            out.push(json!({
                "name": full,
                "kind": item.get("kind").cloned().unwrap_or(json!(VARIABLE)),
                "containerName": match owner.is_empty() {
                    true => container,

                    false => owner,
                },
                "location": {
                    "uri": uri,
                    "range": item.get("selectionRange").cloned().unwrap_or_default(),
                },
            }));
        }

        if let Some(children) = item.get("children").and_then(Value::as_array) {
            flatten_symbols(children, &full, container, uri, query, out);
        }
    }
}

/// Every `declare Name: T` of a source, with the byte range of its name.
fn ambient_values(src: &str, path: &Path) -> Vec<(String, (usize, usize))> {
    let options = alloy_syntax::parser::ParseOptions::for_path(path);
    let Ok(parsed) = alloy_syntax::parse_lenient(src, options) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Declare(d) = stmt else {
            continue;
        };
        let at = d.span.start as usize;
        let word = |i: usize| toks.get(i).map(|t| &src[t.start as usize..t.end as usize]);

        if word(at + 2) != Some(":") {
            continue;
        }

        let name = alloy_syntax::ast::TokSpan::new(at + 1, at + 2);

        if let Some(bytes) = bytes_of(name, toks) {
            out.push((text_of(name, src, toks), bytes));
        }
    }

    out
}

/// One entry of the outline, before it becomes JSON.
struct Entry {
    name: String,
    kind: u8,
    /// The byte range of the whole declaration.
    span: (usize, usize),
    /// The byte range of the name alone.
    name_at: (usize, usize),
    children: Vec<Entry>,
}

impl Entry {
    fn to_value(&self, src: &str) -> Value {
        let range = |(a, b): (usize, usize)| {
            let (sl, sc) = position_of(src, a);
            let (el, ec) = position_of(src, b);

            json!({
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec },
            })
        };

        json!({
            "name": self.name,
            "kind": self.kind,
            "range": range(self.span),
            "selectionRange": range(self.name_at),
            "children": self.children.iter().map(|c| c.to_value(src)).collect::<Vec<_>>(),
        })
    }
}

/// The outline of an Alloy source, or `None` when the file does not
/// parse far enough to hold one.
pub(crate) fn document_symbols(src: &str, path: &Path) -> Option<Vec<Value>> {
    let options = alloy_syntax::parser::ParseOptions::for_path(path);
    let parsed = alloy_syntax::parse_lenient(src, options).ok()?;
    let toks = &parsed.lexed.toks;
    let mut entries: Vec<Entry> = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        push_statement(stmt, src, toks, &mut entries);
    }

    Some(entries.iter().map(|e| e.to_value(src)).collect())
}

/*
Every enum variant the source declares: the enum's name, the variant's,
and the byte range of the variant's own name.

A variant has no binding of its own in the emit. It is a tag inside the
record the enum builds, so the child has no place to point at and no
name to rename. The source holds both.
*/
pub(crate) fn enum_variants(src: &str) -> Vec<(String, String, (usize, usize))> {
    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Enum(e) = stmt.under_default() else {
            continue;
        };
        let owner = text_of(e.name, src, toks);

        for v in &e.variants {
            if let Some(at) = bytes_of(v.name, toks) {
                out.push((owner.clone(), text_of(v.name, src, toks), at));
            }
        }
    }

    out
}

/// The byte range a token span covers. An empty span reads the token it
/// starts on, so a name still has a place.
fn bytes_of(span: alloy_syntax::ast::TokSpan, toks: &[Tok]) -> Option<(usize, usize)> {
    let last = (span.end as usize)
        .saturating_sub(1)
        .max(span.start as usize);
    let first = toks.get(span.start as usize)?;
    let last = toks.get(last)?;

    Some((first.start as usize, last.end as usize))
}

fn text_of(span: alloy_syntax::ast::TokSpan, src: &str, toks: &[Tok]) -> String {
    span.text(src, toks).to_string()
}

/// Adds the entry a top-level statement declares. An `impl` block joins
/// the type it extends, so a struct with methods is one entry.
fn push_statement(stmt: &Stmt, src: &str, toks: &[Tok], out: &mut Vec<Entry>) {
    // `export default struct S` declares `S`; the wrapper declares
    // nothing of its own.
    let inner = match stmt {
        Stmt::ExportDefault {
            value: DefaultExport::Decl(decl),
            ..
        } => decl.as_ref(),

        other => other,
    };

    let Some(span) = bytes_of(inner.span(), toks) else {
        return;
    };
    let named = |name_span: alloy_syntax::ast::TokSpan, kind: u8, children: Vec<Entry>| Entry {
        name: text_of(name_span, src, toks),
        kind,
        span,
        name_at: bytes_of(name_span, toks).unwrap_or(span),
        children,
    };
    let member =
        |name_span: alloy_syntax::ast::TokSpan, own: alloy_syntax::ast::TokSpan, kind: u8| {
            let at = bytes_of(name_span, toks)?;

            Some(Entry {
                name: text_of(name_span, src, toks),
                kind,
                span: bytes_of(own, toks).unwrap_or(at),
                name_at: at,
                children: Vec::new(),
            })
        };

    match inner {
        Stmt::Struct(s) => out.push(named(
            s.name,
            STRUCT,
            s.fields
                .iter()
                .filter_map(|f| member(f.name, f.span, FIELD))
                .collect(),
        )),

        Stmt::Enum(e) => out.push(named(
            e.name,
            ENUM,
            e.variants
                .iter()
                .filter_map(|v| member(v.name, v.span, ENUM_MEMBER))
                .collect(),
        )),

        // A trait is a contract over methods, which is what the
        // protocol's `Interface` names.
        Stmt::Trait(t) => out.push(named(
            t.name,
            INTERFACE,
            t.methods
                .iter()
                .filter_map(|m| member(m.name, m.span, METHOD))
                .collect(),
        )),

        Stmt::Interface(i) => out.push(named(
            i.name,
            INTERFACE,
            i.fields
                .iter()
                .filter_map(|f| member(f.name, f.span, FIELD))
                .collect(),
        )),

        Stmt::Namespace(n) => {
            let mut members = Vec::new();

            for m in &n.members {
                push_statement(&m.stmt, src, toks, &mut members);
            }

            out.push(named(n.name, NAMESPACE, members));
        }

        Stmt::Class(c) => out.push(named(c.name, CLASS, Vec::new())),

        Stmt::TypeAlias(t) => out.push(named(t.name, INTERFACE, Vec::new())),

        Stmt::Remote(r) => out.push(named(r.name, EVENT, Vec::new())),

        Stmt::Macro(m) => out.push(named(m.name, FUNCTION, Vec::new())),

        Stmt::Attribute(a) => out.push(named(a.name, PROPERTY, Vec::new())),

        Stmt::LocalFunction(f) => out.push(named(f.name, FUNCTION, Vec::new())),

        // `function M.f()` names a member of `M`; the last word of the
        // path is the one the reader looks for.
        Stmt::Function(f) => {
            if let Some(name) = f.path.last() {
                let kind = match f.path.len() > 1 || f.is_method {
                    true => METHOD,

                    false => FUNCTION,
                };
                out.push(named(*name, kind, Vec::new()));
            }
        }

        Stmt::Local(l) => {
            for binding in &l.names {
                let Some(at) = bytes_of(binding.name, toks) else {
                    continue;
                };
                let kind = match l.is_const {
                    true => CONSTANT,

                    false => VARIABLE,
                };
                out.push(Entry {
                    name: text_of(binding.name, src, toks),
                    kind,
                    span,
                    name_at: at,
                    children: Vec::new(),
                });
            }
        }

        // `declare function f(...)`, `declare x: T`, `declare class N`,
        // and `declare extern type N`. The tree keeps the span alone, so
        // the name is the token the form's keyword is followed by.
        Stmt::Declare(d) => {
            let at = d.span.start as usize;
            let word = |i: usize| toks.get(i).map(|t| &src[t.start as usize..t.end as usize]);
            let (name_at, kind) = match word(at + 1) {
                Some("function") => (at + 2, FUNCTION),

                Some("class") => (at + 2, CLASS),

                Some("extern") => (at + 3, CLASS),

                // `declare x: T` binds a value; anything else is no
                // declaration this walk knows.
                _ if word(at + 2) == Some(":") => (at + 1, VARIABLE),

                _ => return,
            };
            let name = alloy_syntax::ast::TokSpan::new(name_at, name_at + 1);

            if bytes_of(name, toks).is_some() {
                out.push(named(name, kind, Vec::new()));
            }
        }

        // The methods of an `impl` belong to the type it names, so they
        // join that entry instead of opening a second one under the
        // same word.
        Stmt::Impl(i) => {
            let target = text_of(i.target, src, toks);
            let methods: Vec<Entry> = i
                .methods
                .iter()
                .filter_map(|m| member(*m.path.last()?, m.span, METHOD))
                .collect();

            match out.iter_mut().find(|e| e.name == target) {
                Some(owner) => owner.children.extend(methods),

                None => out.push(named(i.target, STRUCT, methods)),
            }
        }

        _ => {}
    }
}

/// The folding ranges of a source: each block from the line of its
/// opener to the line before its closer, each comment that spans lines,
/// and each run of line comments. The child folds the shadow, whose
/// blocks and lines are the emit's.
pub(crate) fn folding_ranges(src: &str) -> Vec<Value> {
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return Vec::new();
    };
    let structure = alloy::fmt::structure(src, &lexed.toks);
    let line_of = |offset: usize| src[..offset.min(src.len())].matches('\n').count();
    let mut out: Vec<(usize, usize, Option<&str>)> = Vec::new();

    for (i, end) in structure.ends.iter().enumerate() {
        let Some(j) = end else {
            continue;
        };
        let (open, close) = (structure.lines[i], structure.lines[*j]);

        if close > open + 1 {
            out.push((open, close - 1, None));
        }
    }

    let mut run: Option<(usize, usize)> = None;
    let close_run = |run: Option<(usize, usize)>, out: &mut Vec<_>| {
        if let Some((a, b)) = run
            && b > a
        {
            out.push((a, b, Some("comment")));
        }
    };

    for &(s, e) in &lexed.comments {
        let (first, last) = (line_of(s as usize), line_of((e as usize).saturating_sub(1)));
        let line_start = src[..s as usize].rfind('\n').map_or(0, |n| n + 1);
        let alone = src[line_start..s as usize].trim().is_empty();

        if last > first {
            out.push((first, last, Some("comment")));
        }

        run = match run {
            Some((a, b)) if alone && first == last && first == b + 1 => Some((a, first)),

            _ => {
                close_run(run, &mut out);

                (alone && first == last).then_some((first, first))
            }
        };
    }

    close_run(run, &mut out);
    out.sort();
    out.dedup();

    out.into_iter()
        .map(|(start, end, kind)| {
            let mut range = json!({ "startLine": start, "endLine": end });

            if let Some(kind) = kind {
                range["kind"] = json!(kind);
            }

            range
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(src: &str) -> Vec<(String, u8, u32)> {
        fn walk(items: &[Value], out: &mut Vec<(String, u8, u32)>) {
            for it in items {
                out.push((
                    it["name"].as_str().unwrap_or("").to_string(),
                    it["kind"].as_u64().unwrap_or(0) as u8,
                    it["range"]["start"]["line"].as_u64().unwrap_or(0) as u32,
                ));

                if let Some(kids) = it["children"].as_array() {
                    walk(kids, out);
                }
            }
        }

        let mut out = Vec::new();
        walk(
            &document_symbols(src, Path::new("t.aly")).expect("outline"),
            &mut out,
        );

        out
    }

    /// A block folds from its opener to the line before its `end`, a
    /// run of comment lines folds as a comment, and a trailing comment
    /// joins no run. The child's ranges were the shadow's.
    #[test]
    fn folds_follow_the_blocks_of_the_source() {
        let src = concat!(
            "-- One.\n",
            "-- Two.\n",
            "struct P as\n",
            "  x: number\n",
            "end\n",
            "local v = 1 -- trailing\n",
            "-- Alone.\n",
            "enum E as\n",
            "  A\n",
            "  B\n",
            "end\n",
            "local q = match v with\n",
            "  case 1 then \"a\"\n",
            "  default \"b\"\n",
            "end\n",
        );
        let folds: Vec<(u64, u64, Option<String>)> = folding_ranges(src)
            .iter()
            .map(|r| {
                (
                    r["startLine"].as_u64().unwrap_or(0),
                    r["endLine"].as_u64().unwrap_or(0),
                    r["kind"].as_str().map(str::to_string),
                )
            })
            .collect();

        assert_eq!(
            folds,
            [
                (0, 1, Some("comment".to_string())),
                (2, 3, None),
                (7, 9, None),
                (11, 13, None),
            ]
        );
    }

    /// Every declaration once, under the name the source wrote, with
    /// the kind the construct has. No `__alloy`, no `Cursor.__new`, no
    /// `_1`, no `MathX_triple`.
    #[test]
    fn the_outline_reads_the_source() {
        let src = concat!(
            "struct Cursor as\n",
            "    row: number\n",
            "end\n",
            "\n",
            "impl Cursor as\n",
            "    function advance(self): number\n",
            "        return self.row\n",
            "    end\n",
            "end\n",
            "\n",
            "enum Direction as\n",
            "    North(number)\n",
            "end\n",
            "\n",
            "trait Greet as\n",
            "    function hello(self): string\n",
            "end\n",
            "\n",
            "namespace MathX as\n",
            "    function triple(n: number): number\n",
            "        return n * 3\n",
            "    end\n",
            "end\n",
        );
        assert_eq!(
            flat(src),
            vec![
                ("Cursor".to_string(), STRUCT, 0),
                ("row".to_string(), FIELD, 1),
                ("advance".to_string(), METHOD, 5),
                ("Direction".to_string(), ENUM, 10),
                ("North".to_string(), ENUM_MEMBER, 11),
                ("Greet".to_string(), INTERFACE, 14),
                ("hello".to_string(), METHOD, 15),
                ("MathX".to_string(), NAMESPACE, 18),
                ("triple".to_string(), FUNCTION, 19),
            ]
        );

        let text =
            serde_json::to_string(&document_symbols(src, Path::new("t.aly")).expect("outline"))
                .unwrap();

        for leak in ["__alloy", "__new", "_1", "MathX_triple"] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
    }

    /// A variant's name, with the enum that declares it and where it
    /// sits.
    #[test]
    fn a_variant_names_its_enum() {
        let src = "enum Shape as\n    Circle(number)\n    Square(number)\nend\n";
        assert_eq!(
            enum_variants(src),
            vec![
                (
                    "Shape".to_string(),
                    "Circle".to_string(),
                    (
                        src.find("Circle").expect("Circle"),
                        src.find("Circle").expect("Circle") + 6
                    )
                ),
                (
                    "Shape".to_string(),
                    "Square".to_string(),
                    (
                        src.find("Square").expect("Square"),
                        src.find("Square").expect("Square") + 6
                    )
                ),
            ]
        );
        assert_eq!(enum_variants("local x = 1\n"), Vec::new());
    }

    /// A `.d.aly` parses only as a definitions file, so the outline read
    /// nothing and the child answered with the emit's names.
    #[test]
    fn a_definitions_file_lists_its_declarations() {
        let src = "declare AmbientThing: {\n    value: number,\n}\n\ndeclare function ambientFn(x: number): number\n";
        let symbols = document_symbols(src, Path::new("amb.d.aly")).expect("outline");
        let names: Vec<(&str, u64)> = symbols
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap_or(""),
                    s["kind"].as_u64().unwrap_or(0),
                )
            })
            .collect();

        assert_eq!(
            names,
            vec![
                ("AmbientThing", u64::from(VARIABLE)),
                ("ambientFn", u64::from(FUNCTION)),
            ]
        );

        // The workspace walk finds the value form the child cannot index.
        assert_eq!(
            ambient_values(src, Path::new("amb.d.aly")),
            vec![("AmbientThing".to_string(), (8, 20))]
        );
    }

    /// Plain code keeps its outline: a local, a function, a type alias.
    #[test]
    fn plain_declarations_stay() {
        let src = "local count = 1\nlocal function bump(n: number): number\n    return n + 1\nend\ntype Id = string\n";
        assert_eq!(
            flat(src),
            vec![
                ("count".to_string(), VARIABLE, 0),
                ("bump".to_string(), FUNCTION, 1),
                ("Id".to_string(), INTERFACE, 4),
            ]
        );
    }
}
