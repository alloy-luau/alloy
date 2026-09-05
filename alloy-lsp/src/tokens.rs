//! Semantic tokens from the shadow, moved to the source.
//!
//! The child encodes tokens as deltas over the shadow text. A token that
//! sits wholly in copied text keeps its length and moves to its source
//! column; a token in generated text describes a temp or a helper the
//! author never wrote, so it goes. The result re-encodes in source order.

use crate::doc::{Doc, offset_of};

pub fn remap(data: &[u64], doc: &Doc) -> Vec<u64> {
    let Some(out) = &doc.output else {
        return data.to_vec();
    };

    let mut line = 0u64;
    let mut start = 0u64;
    let mut tokens: Vec<(u32, u32, u64, u64, u64)> = Vec::new();

    for t in data.chunks_exact(5) {
        let (dl, ds, len, kind, mods) = (t[0], t[1], t[2], t[3], t[4]);
        line += dl;
        start = if dl > 0 { ds } else { start + ds };

        let (l, s) = (line as u32, start as u32);
        let first = offset_of(&doc.shadow, l, s);
        let last = offset_of(&doc.shadow, l, s + len as u32).map(|e| e.saturating_sub(1));

        let (Some(first), Some(last)) = (first, last) else {
            continue;
        };

        if out.map.is_generated(first as u32) || out.map.is_generated(last as u32) {
            continue;
        }

        let (sl, sc) = doc.to_source(l, s);
        tokens.push((sl, sc, len, kind, mods));
    }

    tokens.sort_by_key(|t| (t.0, t.1));
    tokens.dedup_by_key(|t| (t.0, t.1));

    let mut encoded = Vec::with_capacity(tokens.len() * 5);
    let (mut pl, mut pc) = (0u32, 0u32);

    for (l, c, len, kind, mods) in tokens {
        let dl = l - pl;
        let dc = if dl > 0 { c } else { c - pc };
        encoded.extend_from_slice(&[dl as u64, dc as u64, len, kind, mods]);
        pl = l;
        pc = c;
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::EmitOptions;

    #[test]
    fn tokens_in_generated_text_go_and_the_rest_move() {
        let src = "local v = a ?? 0\nprint(v)\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
        );
        // Shadow: `local v = (if a == nil then 0 else a)` / `print(v)`.
        // Tokens: `local` (0,0,5), `nil` inside the generated text (0,18,3),
        // `print` (1,0,5).
        let data = [0, 0, 5, 1, 0, 0, 18, 3, 2, 0, 1, 0, 5, 3, 0];
        let out = remap(&data, &doc);
        assert_eq!(out, vec![0, 0, 5, 1, 0, 1, 0, 5, 3, 0]);
    }
}
