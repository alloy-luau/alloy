//! One open Alloy document: its source, its check artifact, and the
//! position mapping between them in LSP terms.
//!
//! The emit keeps every line, so a position maps within its line. LSP
//! columns count UTF-16 units; the span map counts bytes.

use alloy::{EmitOptions, Output};

use crate::imports::Export;

pub struct Doc {
    pub source: String,
    pub version: i64,
    pub output: Option<Output>,
    /// The text the child sees: the check artifact, or the source when
    /// the compile failed outright.
    pub shadow: String,
    /// What the file exports, for auto-imports elsewhere.
    pub exports: Vec<Export>,
    /// The declarations of the file, for hover on their names.
    pub decls: Vec<alloy::declarations::Declaration>,
    /// The bindings of the file with their declaring keywords.
    pub bindings: Vec<alloy::declarations::Binding>,
    pub is_alx: bool,
}

impl Doc {
    pub fn new(
        source: String,
        version: i64,
        options: &EmitOptions,
        jsx: &alloy::luaux::Config,
    ) -> Self {
        let mut doc = Self {
            source,
            version,
            output: None,
            shadow: String::new(),
            exports: Vec::new(),
            decls: Vec::new(),
            bindings: Vec::new(),
            is_alx: options.file_name.ends_with(".alx"),
        };
        doc.compile(options, jsx);

        doc
    }

    /// Recompiles after an edit.
    pub fn compile(&mut self, options: &EmitOptions, jsx: &alloy::luaux::Config) {
        self.exports = crate::imports::exports_of(&self.source, self.is_alx);
        self.decls = alloy::declarations::summaries(&self.source, options.definitions);
        self.bindings = alloy::declarations::bindings(&self.source);
        let compiled = if self.is_alx {
            alloy::compile_alx(&self.source, options, jsx.clone()).map(|a| a.output)
        } else {
            alloy::compile_with(&self.source, options)
        };

        match compiled {
            Ok(out) => {
                self.shadow = out.check.clone();
                self.output = Some(out);
            }

            Err(_) => {
                self.shadow = self.source.clone();
                self.output = None;
            }
        }
    }

    /// A source position as a shadow position.
    pub fn to_shadow(&self, line: u32, character: u32) -> (u32, u32) {
        let Some(out) = &self.output else {
            return (line, character);
        };

        let Some(offset) = offset_of(&self.source, line, character) else {
            return (line, character);
        };

        // A byte a desugar replaced has no output position: take the next
        // copied byte on the line, else the previous one.
        let (ls, le) = line_bounds(&self.source, offset);
        let mapped = (offset..=le)
            .find_map(|o| out.map.to_output(o as u32))
            .or_else(|| (ls..offset).rev().find_map(|o| out.map.to_output(o as u32)));

        match mapped {
            Some(m) => position_of(&self.shadow, m as usize),

            None => (line, 0),
        }
    }

    /// A shadow position as a source position. Generated text maps to
    /// the construct that produced it.
    pub fn to_source(&self, line: u32, character: u32) -> (u32, u32) {
        let Some(out) = &self.output else {
            return (line, character);
        };

        let Some(offset) = offset_of(&self.shadow, line, character) else {
            return (line, character);
        };

        let src = out.map.to_source(offset as u32) as usize;

        position_of(&self.source, src.min(self.source.len()))
    }

    /// Applies one LSP content change.
    pub fn apply_change(&mut self, range: Option<((u32, u32), (u32, u32))>, text: &str) {
        apply_change(&mut self.source, range, text);
    }
}

/// Applies one LSP content change to a text.
pub fn apply_change(source: &mut String, range: Option<((u32, u32), (u32, u32))>, text: &str) {
    match range {
        None => *source = text.to_string(),

        Some(((sl, sc), (el, ec))) => {
            let start = offset_of(source, sl, sc).unwrap_or(source.len());
            let end = offset_of(source, el, ec).unwrap_or(source.len());
            let (start, end) = (start.min(end), end.max(start));
            source.replace_range(start..end, text);
        }
    }
}

/// The byte offset of an LSP position, clamped to the line.
pub fn offset_of(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut start = 0usize;

    for (i, l) in text.split('\n').enumerate() {
        if i as u32 == line {
            let mut units = 0u32;

            for (b, ch) in l.char_indices() {
                if units >= character {
                    return Some(start + b);
                }

                units += ch.len_utf16() as u32;
            }

            return Some(start + l.len());
        }

        start += l.len() + 1;
    }

    None
}

/// The LSP position of a byte offset.
pub fn position_of(text: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(text.len());
    let line = text[..offset].matches('\n').count() as u32;
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let character = text[line_start..offset]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum();

    (line, character)
}

/// The byte bounds of the line holding `offset`, end exclusive of `\n`.
fn line_bounds(text: &str, offset: usize) -> (usize, usize) {
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);

    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_map_within_the_line() {
        let src = "local v = a ?? 0\nprint(v)\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
        );
        assert_eq!(
            doc.shadow,
            "local v = (if a == nil then 0 else a)\nprint(v)\n"
        );

        // `print` is copied: same column.
        assert_eq!(doc.to_shadow(1, 0), (1, 0));
        assert_eq!(doc.to_source(1, 3), (1, 3));

        // The `??` byte was replaced: it lands on the generated text, and
        // the generated text maps back to the construct.
        let (line, _) = doc.to_shadow(0, 12);
        assert_eq!(line, 0);
        assert_eq!(doc.to_source(0, 20).0, 0);
    }

    #[test]
    fn utf16_columns_count_units() {
        let text = "😀x";
        assert_eq!(offset_of(text, 0, 2), Some(4));
        assert_eq!(position_of(text, 4), (0, 2));
    }

    #[test]
    fn a_change_applies_by_range() {
        let mut doc = Doc::new(
            "ab\ncd\n".to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
        );
        doc.apply_change(Some(((0, 1), (1, 1))), "X");
        assert_eq!(doc.source, "aXd\n");
    }
}
