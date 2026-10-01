//! The `impl` blocks of a file, as the hover of their own header.
//!
//! The target of an `impl` names a struct or an enum. Its own hover
//! shows the type alone; the hover of an `impl` header shows the type
//! with the methods of every `impl` of it, so each block keeps its
//! methods, its trait and its doc comment here.

use alloy_syntax::ast::{Stmt, TokSpan};

/// One `impl` block: the header line the source wrote, the byte range
/// that line covers, and what the hover of the header reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplBlock {
    /// The name the block targets.
    pub target: String,
    /// `<T>` of `impl Box<T>`, or empty.
    pub generics: String,
    /// The trait the block meets, for `impl Display for Vec2`.
    pub trait_name: Option<String>,
    /// One line per method, in the order the block writes them, each
    /// with its visibility on the left: `private function bump(self)`.
    pub methods: Vec<String>,
    /// The doc comment above the block.
    pub doc: Option<String>,
    /// The byte range of the header, from `impl` to the end of the
    /// target or the trait, whichever comes last.
    pub start: usize,
    pub end: usize,
}

/// Every `impl` block of a source, in the order the file writes them.
/// A block inside a namespace counts too: its header reads the same
/// way.
pub fn impl_blocks(src: &str) -> Vec<ImplBlock> {
    if !src.contains("impl") {
        return Vec::new();
    }

    let Ok(parsed) = alloy_syntax::parse_lenient(src, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let mut out = Vec::new();
    collect(src, toks, &parsed.chunk.block.stmts, &mut out);

    out
}

fn collect(src: &str, toks: &[alloy_syntax::lexer::Tok], stmts: &[Stmt], out: &mut Vec<ImplBlock>) {
    for stmt in stmts {
        match stmt {
            Stmt::Impl(i) => out.push(block_of(src, toks, i)),

            Stmt::Namespace(ns) => {
                let inner: Vec<&Stmt> = ns.members.iter().map(|m| &m.stmt).collect();

                for member in inner {
                    collect(src, toks, std::slice::from_ref(member), out);
                }
            }

            _ => {}
        }
    }
}

fn block_of(
    src: &str,
    toks: &[alloy_syntax::lexer::Tok],
    i: &alloy_syntax::ast::ImplDecl,
) -> ImplBlock {
    let text = |span: TokSpan| span.text_or_empty(src, toks);
    let target = text(i.target).to_string();
    let generics = i
        .generics
        .filter(|g| g.start >= i.target.end)
        .map(text)
        .unwrap_or("");
    let trait_name = i.trait_name.map(|t| text(t).to_string());
    let methods = i
        .methods
        .iter()
        .filter_map(|m| crate::declarations::method_signature(src, toks, m))
        .collect();
    let start = toks[i.span.start as usize].start as usize;
    let doc = crate::declarations::doc_before(src, start);

    // The header runs to the target, and past the trait when the block
    // names one; a caret anywhere on it asks about the block.
    let last = i
        .generics
        .filter(|g| g.start >= i.target.end)
        .unwrap_or(i.target);
    let end = toks[last.end as usize - 1].end as usize;
    // The header opens at the `impl`, not at the statement. An `@attr`
    // above the block is part of the statement's span, and a caret on it
    // asks about the attribute.
    let header = (i.span.start as usize..i.target.start as usize)
        .find(|n| {
            let t = toks[*n];

            &src[t.start as usize..t.end as usize] == "impl"
        })
        .map(|n| toks[n].start as usize)
        .unwrap_or(start);

    ImplBlock {
        target,
        generics: generics.to_string(),
        trait_name,
        methods,
        doc,
        start: header,
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block keeps every method, a private one too, with its
    /// visibility on the left, and the doc comment above it.
    #[test]
    fn a_header_reads_its_own_block() {
        let src = "struct Test as\n    x: number\nend\n\n--- What it adds.\nimpl Test as\n    function test()\n    end\n\n    private function hidden()\n    end\nend\n";
        let blocks = impl_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].target, "Test");
        assert_eq!(
            blocks[0].methods,
            ["public function test()", "private function hidden()"]
        );
        assert_eq!(blocks[0].doc.as_deref(), Some("What it adds."));
        let head = &src[blocks[0].start..blocks[0].end];
        assert_eq!(head, "impl Test");
    }

    #[test]
    fn a_trait_block_names_the_trait() {
        let src = "struct Vec2 as\n    x: number\nend\n\ntrait Display as\n    function show(self): string\nend\n\nimpl Display for Vec2 as\n    function show(self): string\n        return \"v\"\n    end\nend\n";
        let blocks = impl_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].trait_name.as_deref(), Some("Display"));
        assert_eq!(blocks[0].methods, ["public function show(self): string"]);
    }

    #[test]
    fn a_generic_block_keeps_its_parameters() {
        let src = "struct Box<T> as\n    value: T\nend\n\nimpl Box<T> as\n    function get(self): T\n        return self.value\n    end\nend\n";
        let blocks = impl_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].generics, "<T>");
        assert_eq!(&src[blocks[0].start..blocks[0].end], "impl Box<T>");
    }
}
