//! The hover of an `impl` header.
//!
//! The hover of a struct or an enum shows the type alone: its fields or
//! its variants. The hover of an `impl` header shows the type with the
//! methods of every `impl` of it, so the reader sees the whole of what
//! the name holds. Each member keeps its visibility on the left, a
//! private one too.

use super::*;

impl Server {
    pub(crate) fn impl_header_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };

        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        let Some(block) = doc
            .impl_blocks
            .iter()
            .find(|b| offset >= b.start && offset <= b.end)
        else {
            return false;
        };
        let others = st
            .docs
            .iter()
            .filter(|(u, _)| u.as_str() != uri)
            .map(|(_, d)| d.source.as_str());
        let sources: Vec<&str> = std::iter::once(doc.source.as_str())
            .chain(doc.import_sources.iter().map(String::as_str))
            .chain(others)
            .collect();
        let hover = impl_hover(block, &sources);
        let hover = super::formatted_hover(&hover, &st.fmt_config(uri));
        let hover = with_return_arrows(&hover);
        let (sl, sc) = position_of(&doc.source, block.start);
        let (el, ec) = position_of(&doc.source, block.end);
        let result = json!({
            "contents": { "kind": "markdown", "value": hover },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }
}

/*
The hover of an `impl` header: the type its target names, as the type's
own hover writes it, with the methods of every `impl` of the target in
`sources` after the fields or the variants.

The methods keep the order the sources write them: the file that
declares the type first, then the file of the hover, then the rest. A
line two blocks share shows once. A target with no struct or enum in
reach, `impl string`, shows as the block it is, `impl string`, with the
same methods. With nothing to list the head stands alone.
*/
pub(crate) fn impl_hover(block: &alloy::impl_blocks::ImplBlock, sources: &[&str]) -> String {
    let name = block.target.as_str();
    let found = sources
        .iter()
        .enumerate()
        .find_map(|(k, src)| alloy::declarations::type_shape(src, name).map(|s| (k, s)));
    let home = found.as_ref().map(|(k, _)| *k);
    let order = home
        .into_iter()
        .chain((0..sources.len()).filter(|k| Some(*k) != home));
    let mut methods: Vec<String> = Vec::new();
    let mut traits: Vec<String> = Vec::new();

    for k in order {
        for b in alloy::impl_blocks::impl_blocks(sources[k]) {
            if b.target != name {
                continue;
            }

            for m in b.methods {
                if !methods.contains(&m) {
                    methods.push(m);
                }
            }

            if let Some(t) = b.trait_name
                && !traits.contains(&t)
            {
                traits.push(t);
            }
        }
    }

    let (lines, type_doc) = match &found {
        Some((_, shape)) => (shape.lines(&methods), shape.doc.clone()),

        None => {
            let shape = alloy::declarations::TypeShape {
                name,
                head: format!("impl {name}{}", block.generics),
                members: Vec::new(),
                doc: None,
            };

            (shape.lines(&methods), None)
        }
    };
    let mut hover = format!("```alloy\n{}\n```", lines.join("\n"));

    // The block's own comment says what it adds; the type's says what the
    // type is, for a block that writes none.
    if let Some(doc) = block.doc.clone().or(type_doc) {
        hover.push_str("\n\n");
        hover.push_str(&doc);
    }

    if !traits.is_empty() {
        let list: Vec<String> = traits.iter().map(|t| format!("`{t}`")).collect();
        hover.push_str(&format!("\n\nImplements {}.", list.join(", ")));
    }

    hover
}
