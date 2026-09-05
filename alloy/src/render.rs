//! The renderer: output text with provenance, and the span map it yields.
//!
//! Emit copies the source wherever no desugar applies and generates text
//! only at the nodes a pass rewrites. Every chunk of output records where
//! it came from, so the map between source and output is a by-product of
//! rendering, never a separate bookkeeping pass. Generated text never holds
//! a newline; copied text keeps every newline it had. So the output has the
//! line count of the source by construction.

use std::fmt;

/// One run of output text and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// A byte range of the source, copied as is.
    Copied { src_start: u32, src_end: u32 },
    /// Text the compiler wrote, anchored at one source offset for mapping.
    Generated { anchor: u32, len: u32 },
}

/// A mapping between source offsets and output offsets, built by rendering.
#[derive(Debug, Default, Clone)]
pub struct SpanMap {
    /// Output byte offset at which each chunk starts, parallel to `chunks`.
    starts: Vec<u32>,
    chunks: Vec<Chunk>,
    out_len: u32,
}

impl SpanMap {
    /// The output offset of a source offset, when that source byte was
    /// copied. A source byte a desugar replaced has no output position.
    pub fn to_output(&self, src: u32) -> Option<u32> {
        for (i, chunk) in self.chunks.iter().enumerate() {
            if let Chunk::Copied { src_start, src_end } = chunk
                && src >= *src_start
                && src < *src_end
            {
                return Some(self.starts[i] + (src - src_start));
            }
        }

        // The end of the source maps to the end of the output.
        if let Some(Chunk::Copied { src_end, .. }) = self.chunks.last()
            && src == *src_end
        {
            return Some(self.out_len);
        }

        None
    }

    /// The source offset behind an output offset. Generated text maps to
    /// its anchor, so a diagnostic in emitted code points at the construct
    /// that produced it.
    pub fn to_source(&self, out: u32) -> u32 {
        let idx = match self.starts.binary_search(&out) {
            Ok(i) => i,

            Err(0) => 0,

            Err(i) => i - 1,
        };

        match self.chunks.get(idx) {
            Some(Chunk::Copied { src_start, .. }) => src_start + (out - self.starts[idx]),

            Some(Chunk::Generated { anchor, .. }) => *anchor,

            None => 0,
        }
    }

    /// Reports if an output offset sits inside generated text.
    pub fn is_generated(&self, out: u32) -> bool {
        let idx = match self.starts.binary_search(&out) {
            Ok(i) => i,

            Err(0) => 0,

            Err(i) => i - 1,
        };

        matches!(self.chunks.get(idx), Some(Chunk::Generated { .. }))
    }

    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// The output offset at which chunk `i` starts.
    pub fn chunk_start(&self, i: usize) -> u32 {
        self.starts[i]
    }
}

/// Builds output text and its map in one pass.
pub struct Renderer<'s> {
    src: &'s str,
    out: String,
    map: SpanMap,
}

/// A generated chunk that holds a newline. The renderer refuses it, because
/// a newline in generated text would move every later line of the output.
#[derive(Debug)]
pub struct NewlineInGenerated {
    pub anchor: u32,
    pub text: String,
}

impl fmt::Display for NewlineInGenerated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "generated text at byte {} holds a newline: {:?}",
            self.anchor, self.text
        )
    }
}

impl<'s> Renderer<'s> {
    pub fn new(src: &'s str) -> Self {
        Self {
            src,
            out: String::with_capacity(src.len() + src.len() / 8),
            map: SpanMap::default(),
        }
    }

    pub fn source(&self) -> &'s str {
        self.src
    }

    /// The output length so far.
    pub fn out_len(&self) -> u32 {
        self.out.len() as u32
    }

    /// Copies a byte range of the source.
    pub fn copy(&mut self, start: u32, end: u32) {
        if start >= end {
            return;
        }

        // Merge with a preceding copy of the adjacent range, so the map
        // stays small on the common path of untouched code.
        if let Some(Chunk::Copied { src_end, .. }) = self.map.chunks.last_mut()
            && *src_end == start
        {
            *src_end = end;
        } else {
            self.map.starts.push(self.out.len() as u32);
            self.map.chunks.push(Chunk::Copied {
                src_start: start,
                src_end: end,
            });
        }

        self.out.push_str(&self.src[start as usize..end as usize]);
        self.map.out_len = self.out.len() as u32;
    }

    /// Writes generated text anchored at a source offset.
    pub fn generate(&mut self, anchor: u32, text: &str) -> Result<(), NewlineInGenerated> {
        if text.contains('\n') {
            return Err(NewlineInGenerated {
                anchor,
                text: text.to_string(),
            });
        }

        if text.is_empty() {
            return Ok(());
        }

        self.map.starts.push(self.out.len() as u32);
        self.map.chunks.push(Chunk::Generated {
            anchor,
            len: text.len() as u32,
        });
        self.out.push_str(text);
        self.map.out_len = self.out.len() as u32;

        Ok(())
    }

    /// Appends everything another renderer over the same source produced,
    /// chunk by chunk, so provenance survives the move.
    pub fn append(&mut self, other: Renderer<'s>) {
        let (text, map) = other.finish();

        for (i, chunk) in map.chunks.iter().enumerate() {
            match *chunk {
                Chunk::Copied { src_start, src_end } => self.copy(src_start, src_end),

                Chunk::Generated { anchor, len } => {
                    let start = map.starts[i] as usize;
                    let piece = &text[start..start + len as usize];
                    // The other renderer refused newlines already.
                    let _ = self.generate(anchor, piece);
                }
            }
        }
    }

    pub fn finish(self) -> (String, SpanMap) {
        (self.out, self.map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_merge_and_map_both_ways() {
        let src = "local x = a\nprint(x)\n";
        let mut r = Renderer::new(src);
        r.copy(0, 6);
        r.copy(6, 12);
        r.generate(12, "-- gen ").unwrap();
        r.copy(12, src.len() as u32);
        let (out, map) = r.finish();

        assert_eq!(out, "local x = a\n-- gen print(x)\n");
        assert_eq!(map.chunks().len(), 3, "adjacent copies merge");
        assert_eq!(map.to_output(0), Some(0));
        assert_eq!(map.to_output(12), Some(12 + 7));
        assert_eq!(map.to_source(12 + 7), 12);
        assert_eq!(map.to_source(14), 12, "generated text maps to its anchor");
        assert!(map.is_generated(14));
        assert!(!map.is_generated(3));
    }

    #[test]
    fn generated_newlines_are_refused() {
        let mut r = Renderer::new("x");
        assert!(r.generate(0, "a\nb").is_err());
    }
}
