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
pub(crate) fn document_symbols(src: &str) -> Option<Vec<Value>> {
    let parsed = alloy_syntax::parse_lenient(src, Default::default()).ok()?;
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
        walk(&document_symbols(src).expect("outline"), &mut out);

        out
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

        let text = serde_json::to_string(&document_symbols(src).expect("outline")).unwrap();

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
