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
    /// Every namespace member: the name the emit writes and the path
    /// the source wrote, for the folds.
    pub namespaces: Vec<(String, String)>,
    /// Every namespace the file declares, with the byte range of its
    /// body and the members it holds.
    pub namespace_ranges: Vec<alloy::declarations::NamespaceSpan>,
    /// The bindings of the file with their declaring keywords.
    pub bindings: Vec<alloy::declarations::Binding>,
    /// The structs and enums, so a printed type folds back to its name.
    pub shapes: Vec<alloy::declarations::Shape>,
    /// The shapes of the modules the file imports.
    pub import_shapes: Vec<alloy::declarations::Shape>,
    /// The interfaces the file declares, so a printed intersection
    /// reads by the name the source gave it.
    pub interfaces: Vec<crate::shapes::Interface>,
    /// The interfaces of the modules the file imports. A file names an
    /// interface it took from another module.
    pub import_interfaces: Vec<crate::shapes::Interface>,
    /// The text of those modules, for a declaration the file uses but
    /// does not hold: a `remote`, an exported `const`.
    pub import_sources: Vec<String>,
    /// The error that stopped the compile, when one did. The child then
    /// sees the Alloy source, which it cannot read.
    pub error: Option<alloy::CompileError>,
    pub is_alx: bool,
    /// The `global` declarations of this file. The workspace's set is
    /// every document's, and it says what each file reaches for free.
    pub globals: Vec<alloy::globals::Global>,
}

/// The source with the operators Luau has no reading for blanked, each
/// to the same width so every position still maps: `a?.b` becomes
/// `a .b`, which the child completes as the member access it is.
fn plain_enough(source: &str) -> String {
    source.replace("?.", " .").replace("!.", " .")
}

impl Doc {
    pub fn new(
        source: String,
        version: i64,
        options: &EmitOptions,
        jsx: &alloy::luaux::Config,
        ingots: Option<&alloy::ingot::Ingots>,
    ) -> Self {
        let mut doc = Self {
            source,
            version,
            output: None,
            shadow: String::new(),
            exports: Vec::new(),
            decls: Vec::new(),
            namespaces: Vec::new(),
            namespace_ranges: Vec::new(),
            bindings: Vec::new(),
            shapes: Vec::new(),
            import_shapes: Vec::new(),
            interfaces: Vec::new(),
            import_interfaces: Vec::new(),
            import_sources: Vec::new(),
            error: None,
            is_alx: options.file_name.ends_with(".alx"),
            globals: Vec::new(),
        };
        doc.compile(options, jsx, ingots);

        doc
    }

    /// Recompiles after an edit.
    pub fn compile(
        &mut self,
        options: &EmitOptions,
        jsx: &alloy::luaux::Config,
        ingots: Option<&alloy::ingot::Ingots>,
    ) {
        self.exports = crate::imports::exports_of(&self.source, self.is_alx);
        // The path here is the real one; the workspace fills the path a
        // message names when it gathers the set.
        let path = std::path::Path::new(&options.file_name);
        self.globals =
            alloy::globals::declared(&alloy::globals::index_text(path, &self.source), path);
        self.decls = alloy::declarations::summaries(&self.source, options.definitions);
        self.namespaces = alloy::declarations::namespace_names(&self.source);
        self.namespace_ranges = alloy::declarations::namespace_ranges(&self.source);
        self.bindings = alloy::declarations::bindings(&self.source);
        self.shapes = alloy::declarations::shapes(&self.source);
        self.interfaces = crate::shapes::interfaces(&self.source);
        self.import_shapes = alloy::modules::import_shapes_for_file(
            std::path::Path::new(&options.file_name),
            &self.source,
        );
        self.import_sources = alloy::modules::import_sources_for_file(
            std::path::Path::new(&options.file_name),
            &self.source,
        );
        self.import_interfaces = self
            .import_sources
            .iter()
            .flat_map(|text| crate::shapes::interfaces(text))
            .collect();
        // `file_name` is the real path, which is what the ingots see.
        let compiled =
            alloy::compile_file(&options.file_name, &self.source, options, Some(jsx), ingots);

        match compiled {
            Ok(out) => {
                self.shadow = out.check.clone();
                self.output = Some(out);
                self.error = None;
            }

            Err(e) => {
                self.shadow = plain_enough(&self.source);
                self.output = None;
                self.error = Some(e);
            }
        }
    }

    /// A source position as a shadow position.
    pub fn to_shadow(&self, line: u32, character: u32) -> (u32, u32) {
        let Some(out) = &self.output else {
            return (line, character);
        };

        // For `.alx`, the map speaks lowered positions: same line, the
        // column through the word under it. An ingot's edit sits between
        // the author's text and the lowering.
        let (text, line, character) = match &out.lowered {
            Some(low) => {
                let (layered, line, character) = match (&out.layer, &out.layered) {
                    (Some(layer), Some(layered)) => {
                        let at = offset_of(&self.source, line, character).unwrap_or(0);
                        let (ls, le) = line_bounds(&self.source, at);
                        let mapped = (at..=le)
                            .find_map(|o| layer.to_output(o as u32))
                            .or_else(|| (ls..at).rev().find_map(|o| layer.to_output(o as u32)))
                            .unwrap_or(0) as usize;
                        let (l, c) = position_of(layered, mapped.min(layered.len()));

                        (layered.as_str(), l, c)
                    }

                    _ => (self.source.as_str(), line, character),
                };
                let from = line_text(layered, line);
                let to = line_text(low, line);
                let col = alloy::alx::map_column(from, to, character as usize) as u32;

                (low.as_str(), line, col)
            }

            None => (self.source.as_str(), line, character),
        };

        let Some(offset) = offset_of(text, line, character) else {
            return (line, character);
        };

        // A byte a desugar replaced has no output position: take the next
        // copied byte on the line, else the previous one.
        let (ls, le) = line_bounds(text, offset);
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

        match &out.lowered {
            Some(low) => {
                let (line, col) = position_of(low, src.min(low.len()));
                let from = line_text(low, line);

                match (&out.layer, &out.layered) {
                    (Some(layer), Some(layered)) => {
                        let to = line_text(layered, line);
                        let col = alloy::alx::map_column(from, to, col as usize) as u32;
                        let at = offset_of(layered, line, col).unwrap_or(0);
                        let original = layer.to_source(at as u32) as usize;

                        position_of(&self.source, original.min(self.source.len()))
                    }

                    _ => {
                        let to = line_text(&self.source, line);

                        (line, alloy::alx::map_column(from, to, col as usize) as u32)
                    }
                }
            }

            None => position_of(&self.source, src.min(self.source.len())),
        }
    }

    /// Whether a shadow position sits in text no author wrote: the
    /// desugar's, or an ingot's edit behind a `.alx` lowering.
    pub fn generated_at(&self, line: u32, character: u32) -> bool {
        let Some(offset) = offset_of(&self.shadow, line, character) else {
            return false;
        };

        self.generated_offset(offset)
    }

    /// The same, for a shadow byte offset.
    pub fn generated_offset(&self, offset: usize) -> bool {
        let Some(out) = &self.output else {
            return false;
        };

        if out.map.is_generated(offset as u32) {
            return true;
        }

        let (Some(low), Some(layer), Some(layered)) = (&out.lowered, &out.layer, &out.layered)
        else {
            return false;
        };
        let src = out.map.to_source(offset as u32) as usize;
        let (line, col) = position_of(low, src.min(low.len()));
        let from = line_text(low, line);
        let to = line_text(layered, line);
        let col = alloy::alx::map_column(from, to, col as usize) as u32;
        let at = offset_of(layered, line, col).unwrap_or(0);

        layer.is_generated(at as u32)
    }

    /// For `.alx`: whether the byte before a shadow offset differs from
    /// the byte before the source position it maps to. A call the
    /// lowering wrote, `create("Frame")` for `<Frame`, has a quote where
    /// the source has `<`; a call the author wrote reads the same.
    pub fn lowering_differs_before(&self, offset: usize) -> bool {
        let Some(out) = &self.output else {
            return false;
        };
        let Some(low) = &out.lowered else {
            return false;
        };
        let src = out.map.to_source(offset as u32) as usize;
        let (line, col) = position_of(low, src.min(low.len()));
        let from = line_text(low, line);
        let to = line_text(&self.source, line);
        let mapped = alloy::alx::map_column(from, to, col as usize);
        let before_low = from[..(col as usize).min(from.len())].chars().next_back();
        let before_src = to[..mapped.min(to.len())].chars().next_back();

        before_low != before_src
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
/// The text of line `line`, without its newline; empty past the end.
fn line_text(text: &str, line: u32) -> &str {
    text.split('\n').nth(line as usize).unwrap_or("")
}

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
            None,
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
            None,
        );
        doc.apply_change(Some(((0, 1), (1, 1))), "X");
        assert_eq!(doc.source, "aXd\n");
    }
}
