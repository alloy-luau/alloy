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
        let idx = self.chunk_at(out);

        match self.chunks.get(idx) {
            Some(Chunk::Copied { src_start, .. }) => src_start + (out - self.starts[idx]),

            Some(Chunk::Generated { anchor, .. }) => *anchor,

            None => 0,
        }
    }

    /// Reports if an output offset sits inside generated text.
    pub fn is_generated(&self, out: u32) -> bool {
        matches!(
            self.chunks.get(self.chunk_at(out)),
            Some(Chunk::Generated { .. })
        )
    }

    /// The chunk that holds an output offset: the last one that starts
    /// at or before it, so an empty chunk at the same start yields to
    /// the one with the text.
    fn chunk_at(&self, out: u32) -> usize {
        self.starts.partition_point(|s| *s <= out).saturating_sub(1)
    }

    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// The output length the map covers.
    pub fn out_len(&self) -> u32 {
        self.out_len
    }

    fn push_copied(&mut self, src_start: u32, src_end: u32) {
        if src_start >= src_end {
            return;
        }

        if let Some(Chunk::Copied { src_end: e, .. }) = self.chunks.last_mut()
            && *e == src_start
        {
            *e = src_end;
        } else {
            self.starts.push(self.out_len);
            self.chunks.push(Chunk::Copied { src_start, src_end });
        }

        self.out_len += src_end - src_start;
    }

    fn push_generated(&mut self, anchor: u32, len: u32) {
        if len == 0 {
            return;
        }

        self.starts.push(self.out_len);
        self.chunks.push(Chunk::Generated { anchor, len });
        self.out_len += len;
    }

    /// Composes this map, source to middle, with `inner`, middle to
    /// output, into one map from source to output. An ingot's edits form
    /// the outer layer and the desugar the inner one; the editor reads
    /// the composed map and never sees the middle text.
    pub fn compose(&self, inner: &SpanMap) -> SpanMap {
        let mut out = SpanMap::default();

        for (i, chunk) in inner.chunks.iter().enumerate() {
            match *chunk {
                Chunk::Generated { anchor, len } => {
                    out.push_generated(self.to_source(anchor), len);
                }

                Chunk::Copied {
                    src_start: mid_start,
                    src_end: mid_end,
                } => {
                    // Walk the outer chunks that cover [mid_start, mid_end).
                    let mut at = mid_start;
                    let mut j = self.chunk_at(mid_start);

                    while at < mid_end && j < self.chunks.len() {
                        let start = self.starts[j];
                        let len = match self.chunks[j] {
                            Chunk::Copied { src_start, src_end } => src_end - src_start,

                            Chunk::Generated { len, .. } => len,
                        };
                        let end = start + len;
                        let take_end = end.min(mid_end);

                        if take_end > at {
                            match self.chunks[j] {
                                Chunk::Copied { src_start, .. } => {
                                    let from = src_start + (at - start);
                                    out.push_copied(from, from + (take_end - at));
                                }

                                Chunk::Generated { anchor, .. } => {
                                    out.push_generated(anchor, take_end - at);
                                }
                            }

                            at = take_end;
                        }

                        j += 1;
                    }

                    // Middle text past the outer map's end: the identity.
                    if at < mid_end {
                        let src_end = self.chunks.last().map_or(0, |c| match c {
                            Chunk::Copied { src_end, .. } => *src_end,

                            Chunk::Generated { anchor, .. } => *anchor,
                        });
                        out.push_copied(
                            src_end + (at - self.out_len),
                            src_end + (mid_end - self.out_len),
                        );
                    }
                }
            }

            debug_assert_eq!(out.out_len, inner.starts[i] + inner_len(chunk));
        }

        out
    }
}

fn inner_len(chunk: &Chunk) -> u32 {
    match chunk {
        Chunk::Copied { src_start, src_end } => src_end - src_start,

        Chunk::Generated { len, .. } => *len,
    }
}

/// One whole span replacement against the source: the start byte, the
/// end byte, and the new text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

/// An edit the layer refuses: it leaves the source, overlaps another, or
/// changes the line count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditError {
    pub edit: Edit,
    pub message: String,
}

/// Applies edits to a source and returns the new text with the map from
/// the source to it. The line count holds: an edit's text must hold as
/// many newlines as the span it replaces, and each newline of the text
/// maps onto the matching newline of the span, so a later layer still
/// sees every line where the author wrote it. Edits that overlap or
/// break the rule are returned as errors and skipped.
pub fn apply_edits(src: &str, edits: &[Edit]) -> (String, SpanMap, Vec<EditError>) {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.start, e.end));
    let mut errors = Vec::new();
    let mut r = Renderer::new(src);
    let mut at = 0u32;
    let len = src.len() as u32;

    for e in sorted {
        let refuse = |message: &str| EditError {
            edit: e.clone(),
            message: message.to_string(),
        };

        if e.start > e.end || e.end > len {
            errors.push(refuse("the span leaves the file"));
            continue;
        }

        if !src.is_char_boundary(e.start as usize) || !src.is_char_boundary(e.end as usize) {
            errors.push(refuse("the span splits a character"));
            continue;
        }

        if e.start < at {
            errors.push(refuse("the span overlaps an earlier edit"));
            continue;
        }

        let old = &src[e.start as usize..e.end as usize];
        let old_lines = old.matches('\n').count();
        let new_lines = e.text.matches('\n').count();

        if old_lines != new_lines {
            errors.push(refuse(&format!(
                "the text holds {new_lines} newlines where the span holds {old_lines}; the line count must hold"
            )));
            continue;
        }

        r.copy(at, e.start);

        // Each piece between newlines is generated; the newline itself is
        // copied from the span, so the renderer's rule holds. A piece
        // anchors at the start of the line it lands on, so generated
        // text never maps to a line the author reads elsewhere.
        let mut newline_at: Vec<u32> = old
            .match_indices('\n')
            .map(|(i, _)| e.start + i as u32)
            .collect();
        newline_at.reverse();
        let mut anchor = e.start;

        for piece in e.text.split('\n') {
            let _ = r.generate(anchor, piece);

            if let Some(nl) = newline_at.pop() {
                r.copy(nl, nl + 1);
                anchor = nl + 1;
            }
        }

        at = e.end;
    }

    r.copy(at, len);
    let (text, map) = r.finish();

    (text, map, errors)
}

/// The first character of `text` that is code, past spaces and Luau
/// comments. `None` when a comment runs past the end of `text`.
fn first_code_char(text: &str) -> Option<char> {
    let mut s = text;

    loop {
        s = s.trim_start();

        let Some(comment) = s.strip_prefix("--") else {
            return s.chars().next();
        };
        // `--[[ ]]` and `--[==[ ]==]` close on their own bracket.
        let level = comment
            .strip_prefix('[')
            .map(|r| r.len() - r.trim_start_matches('=').len())
            .filter(|n| comment[1 + n..].starts_with('['));

        s = match level {
            Some(n) => {
                let close = format!("]{}]", "=".repeat(n));
                let body = &comment[2 + n..];

                &body[body.find(&close)? + close.len()..]
            }

            None => &comment[comment.find('\n')?..],
        };
    }
}

impl SpanMap {
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
    /// A generated statement that ends in an expression stands last, and
    /// no code has followed it yet. See [`Renderer::end_stmt`].
    open_stmt: bool,
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
            open_stmt: false,
        }
    }

    /// Marks the end of a generated statement that ends in an
    /// expression. Luau reads a `(` after an expression as a call, so the
    /// next code gets a `;` in front when it starts with `(`.
    pub fn end_stmt(&mut self) {
        self.open_stmt = true;
    }

    /// Writes the `;` that [`Renderer::end_stmt`] asks for, when `text`
    /// is the code after the statement and starts with `(`. Spaces and
    /// comments leave the statement open.
    fn close_stmt(&mut self, anchor: u32, text: &str) {
        if !self.open_stmt {
            return;
        }

        if let Some(c) = first_code_char(text) {
            self.open_stmt = false;

            if c == '(' {
                self.push_generated(anchor, ";");
            }
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
    /// The newlines the output gained since it was `from` bytes long.
    pub fn newlines_since(&self, from: u32) -> usize {
        self.out[(from as usize).min(self.out.len())..]
            .matches('\n')
            .count()
    }

    pub fn copy(&mut self, start: u32, end: u32) {
        if start >= end {
            return;
        }

        let src = self.src;
        self.close_stmt(start, &src[start as usize..end as usize]);

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

        self.close_stmt(anchor, text);
        self.push_generated(anchor, text);

        Ok(())
    }

    fn push_generated(&mut self, anchor: u32, text: &str) {
        self.map.starts.push(self.out.len() as u32);
        self.map.chunks.push(Chunk::Generated {
            anchor,
            len: text.len() as u32,
        });
        self.out.push_str(text);
        self.map.out_len = self.out.len() as u32;
    }

    /// Appends everything another renderer over the same source produced,
    /// chunk by chunk, so provenance survives the move.
    pub fn append(&mut self, other: Renderer<'s>) {
        let open = other.open_stmt;
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

        self.open_stmt |= open;
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
    fn edits_keep_lines_and_map_back() {
        let src = "local a = 1\nprint(a)\n";
        let edits = vec![
            Edit {
                start: 10,
                end: 11,
                text: "22".into(),
            },
            Edit {
                start: 12,
                end: 17,
                text: "warn".into(),
            },
        ];
        let (text, map, errors) = apply_edits(src, &edits);

        assert_eq!(text, "local a = 22\nwarn(a)\n");
        assert!(errors.is_empty());
        assert_eq!(
            map.to_source(11),
            10,
            "generated text maps to the edit start"
        );
        assert_eq!(map.to_source(13), 12);
        assert_eq!(map.to_source(17), 17, "the `(` after the edit");
        assert_eq!(map.to_output(6), Some(6));
        assert_eq!(
            map.to_output(10),
            None,
            "a replaced byte has no output position"
        );
    }

    #[test]
    fn an_edit_that_changes_the_line_count_is_refused() {
        let src = "a\nb\n";
        let (text, _, errors) = apply_edits(
            src,
            &[Edit {
                start: 0,
                end: 1,
                text: "x\ny".into(),
            }],
        );

        assert_eq!(text, src);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("line count"));
    }

    #[test]
    fn a_multiline_edit_maps_each_line() {
        let src = "a\nb\nc";
        let (text, map, errors) = apply_edits(
            src,
            &[Edit {
                start: 0,
                end: 3,
                text: "xx\nyy".into(),
            }],
        );

        assert!(errors.is_empty());
        assert_eq!(text, "xx\nyy\nc");
        assert_eq!(map.to_source(2), 1, "the newline is copied");
        assert_eq!(map.to_source(6), 4);
    }

    #[test]
    fn composed_maps_reach_the_source() {
        let src = "local a = 1\nprint(a)\n";
        let (mid, outer, _) = apply_edits(
            src,
            &[Edit {
                start: 12,
                end: 17,
                text: "warn".into(),
            }],
        );
        let mut r = Renderer::new(&mid);
        r.copy(0, 12);
        r.generate(12, "-- g ").unwrap();
        r.copy(12, mid.len() as u32);
        let (out, inner) = r.finish();
        let map = outer.compose(&inner);

        assert_eq!(out, "local a = 1\n-- g warn(a)\n");
        assert_eq!(map.out_len(), out.len() as u32);
        assert_eq!(map.to_source(0), 0);
        assert_eq!(map.to_source(13), 12, "generated by the inner layer");
        assert!(map.is_generated(18), "generated by the outer layer");
        assert_eq!(map.to_source(18), 12);
        assert_eq!(map.to_source(21), 17, "the `(` copied through both");
        assert_eq!(map.to_output(17), Some(21));
        assert_eq!(map.to_output(12), None);
    }

    #[test]
    fn generated_newlines_are_refused() {
        let mut r = Renderer::new("x");
        assert!(r.generate(0, "a\nb").is_err());
    }
}
