//! The hover of an `impl` header.
//!
//! The target of an `impl` names a struct, and the struct answers its
//! own hover everywhere else. On the header line the reader asks about
//! the block: what it adds to the name, and the trait it meets.

use super::*;

impl Server {
    pub(crate) fn impl_header_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

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
        let (sl, sc) = position_of(&doc.source, block.start);
        let (el, ec) = position_of(&doc.source, block.end);
        let result = json!({
            "contents": { "kind": "markdown", "value": block.hover },
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
