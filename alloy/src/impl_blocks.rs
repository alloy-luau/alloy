//! The `impl` blocks of a file, as the hover of their own header.
//!
//! The target of an `impl` names a struct, and the struct has a hover
//! of its own. On the header line the reader asks about the block in
//! front of them: what it adds to that name, and the trait it meets.

use alloy_syntax::ast::{Stmt, TokSpan};

/// One `impl` block: the header line the source wrote, the byte range
/// that line covers, and the hover the header answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplBlock {
    /// The name the block targets.
    pub target: String,
    /// The trait the block meets, for `impl Display for Vec2`.
    pub trait_name: Option<String>,
    /// The hover Markdown for the header.
    pub hover: String,
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
    let text = |span: TokSpan| -> &str {
        if span.end <= span.start {
            return "";
        }

        &src[toks[span.start as usize].start as usize..toks[span.end as usize - 1].end as usize]
    };
    let target = text(i.target).to_string();
    let generics = i
        .generics
        .filter(|g| g.start >= i.target.end)
        .map(text)
        .unwrap_or("");
    let trait_name = i.trait_name.map(|t| text(t).to_string());
    let head = match &trait_name {
        Some(t) => format!("impl {t} for {target}{generics} as"),

        None => format!("impl {target}{generics} as"),
    };
    let mut lines = vec!["```alloy".to_string(), head];

    for m in &i.methods {
        // A private method is out of reach for every reader of the
        // hover, and completion already leaves it out.
        if m.visibility.is_some_and(|v| text(v) == "private") {
            continue;
        }

        if let Some(line) = crate::declarations::method_signature(src, toks, m) {
            lines.push(format!("    {line}"));
        }
    }

    lines.push("end".to_string());
    lines.push("```".to_string());
    let mut hover = lines.join("\n");
    let start = toks[i.span.start as usize].start as usize;

    if let Some(doc) = crate::declarations::doc_before(src, start) {
        hover.push_str("\n\n");
        hover.push_str(&doc);
    }

    // The header runs to the target, and past the trait when the block
    // names one; a caret anywhere on it asks about the block.
    let last = i
        .generics
        .filter(|g| g.start >= i.target.end)
        .unwrap_or(i.target);
    let end = toks[last.end as usize - 1].end as usize;

    ImplBlock {
        target,
        trait_name,
        hover,
        start,
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_reads_its_own_block() {
        let src = "struct Test as\n    x: number\nend\n\n--- What it adds.\nimpl Test as\n    function test()\n    end\n\n    private function hidden()\n    end\nend\n";
        let blocks = impl_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].target, "Test");
        assert_eq!(
            blocks[0].hover,
            "```alloy\nimpl Test as\n    public function test()\nend\n```\n\nWhat it adds."
        );
        let head = &src[blocks[0].start..blocks[0].end];
        assert_eq!(head, "impl Test");
    }

    #[test]
    fn a_trait_block_names_the_trait_first() {
        let src = "struct Vec2 as\n    x: number\nend\n\ntrait Display as\n    function show(self): string\nend\n\nimpl Display for Vec2 as\n    function show(self): string\n        return \"v\"\n    end\nend\n";
        let blocks = impl_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].trait_name.as_deref(), Some("Display"));
        assert!(
            blocks[0].hover.contains("impl Display for Vec2 as"),
            "{}",
            blocks[0].hover
        );
        assert!(
            blocks[0]
                .hover
                .contains("    public function show(self): string"),
            "{}",
            blocks[0].hover
        );
    }

    #[test]
    fn a_generic_block_keeps_its_parameters() {
        let src = "struct Box<T> as\n    value: T\nend\n\nimpl Box<T> as\n    function get(self): T\n        return self.value\n    end\nend\n";
        let blocks = impl_blocks(src);
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0].hover.contains("impl Box<T> as"),
            "{}",
            blocks[0].hover
        );
        assert_eq!(&src[blocks[0].start..blocks[0].end], "impl Box<T>");
    }
}
