//! One open Alloy document: its source, its check artifact, and the
//! position mapping between them in LSP terms.
//!
//! The emit keeps every line, so a position maps within its line. LSP
//! columns count UTF-16 units; the span map counts bytes.

use alloy::{EmitOptions, Output};
use alloy_syntax::lexer::TokKind;

use crate::imports::Export;

/// The compile of a repaired copy of the source. The child reads it in
/// place of the artifact the author's own text makes, which stops at a
/// syntax error and answers nothing past it.
pub struct Repair {
    /// The source with a placeholder after every dangling operator.
    pub source: String,
    pub output: Output,
    /// Where each placeholder went: the byte offset in the author's
    /// text and the length of the text the pass added there.
    pub spots: Vec<(usize, usize)>,
}

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
    /// Every `impl` block of the file, so its header hovers as the
    /// block instead of as the name it targets.
    pub impl_blocks: Vec<alloy::impl_blocks::ImplBlock>,
    /// The plain `local X = { }` tables with their members, for the
    /// folds: a print of the whole shape reads back as `typeof(X)`.
    pub tables: Vec<(String, Vec<String>)>,
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
    /// The declarations of the modules the file imports. A hover on an
    /// imported name reads them, so it answers before the workspace
    /// pass has opened the module.
    pub import_decls: Vec<alloy::declarations::Declaration>,
    /// The text of those modules, for a declaration the file uses but
    /// does not hold: a `remote`, an exported `const`.
    pub import_sources: Vec<String>,
    /// The error that stopped the compile, when one did. The child then
    /// sees the Alloy source, which it cannot read.
    pub error: Option<alloy::CompileError>,
    pub is_alx: bool,
    /// The repair pass's compile, when a dangling `.`, `:`, `?.`,
    /// `!.`, `?:`, `!:`, `[`, `?[` or `![` stopped the parser.
    /// `shadow` and every position map come
    /// from it while it stands; `output` stays the author's own
    /// compile, so the diagnostics still name what they always did.
    pub repair: Option<Repair>,
    /// For a `.alx` whose markup could not lower: the byte ranges the
    /// shadow blanked. The artifact holds no text of the author's
    /// there, so nothing answers inside one.
    pub blanked: Vec<(usize, usize)>,
    /// Whether the text the editor sent opens with a byte order mark.
    /// `source` holds none either way, and a whole-document edit the
    /// proxy writes back carries the mark again.
    pub bom: bool,
}

/// A byte order mark, which stands at the start of the whole text and
/// nowhere else. An editor hides it, the Luau lexer refuses it, and
/// every position helper counts it, so the document drops it on the way
/// in and `format_document` writes it back.
pub const MARK: &str = "\u{feff}";

/// Drops a leading byte order mark, so the source, the shadow, the
/// mirror and every offset agree.
fn strip_mark(text: &mut String) {
    if text.starts_with(MARK) {
        text.drain(..MARK.len());
    }
}

/// The source with the operators Luau has no reading for blanked, each
/// to the same width so every position still maps: `a?.b` becomes
/// `a .b`, which the child completes as the member access it is.
fn plain_enough(source: &str) -> String {
    source.replace("?.", " .").replace("!.", " .")
}

/// The name the repair pass writes after a dangling member operator.
/// The call keeps the statement a call, the one expression Luau reads
/// as a statement, so `HashMap.new().` repairs in every position.
const HOLE: &str = "__alloy_hole()";

/// The same for a bracket that never closed: `x[` becomes
/// `x[__alloy_hole()]`, one index the parser reads through. The
/// placeholder carries the `]`, so `x?[` and `x![` repair too.
const BRACKET_HOLE: &str = "__alloy_hole()]";

/// The source, or a copy the lexer reads when the source stops it: on
/// each line, a quote that opens a string and never closes becomes a
/// space. The copy has the length of the source, so every byte offset
/// still means what it did.
fn lexable(source: &str) -> std::borrow::Cow<'_, str> {
    if alloy_syntax::lexer::lex(source).is_ok() {
        return std::borrow::Cow::Borrowed(source);
    }

    let mut bytes = source.as_bytes().to_vec();
    let mut from = 0usize;

    while from < bytes.len() {
        let end = bytes[from..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |n| from + n);
        let mut open: Option<(u8, usize)> = None;
        let mut i = from;

        while i < end {
            match (open, bytes[i]) {
                (Some(_), b'\\') => i += 1,

                (Some((q, _)), c) if c == q => open = None,

                (None, c @ (b'"' | b'\'')) => open = Some((c, i)),

                _ => {}
            }

            i += 1;
        }

        if let Some((_, at)) = open {
            bytes[at] = b' ';
        }

        from = end + 1;
    }

    match String::from_utf8(bytes) {
        Ok(text) => std::borrow::Cow::Owned(text),

        Err(_) => std::borrow::Cow::Borrowed(source),
    }
}

/// Whether a token closes what stands before it: `)`, `}`, `]`, `,`.
/// A member operator is never followed by one, so a name is missing
/// there as surely as at the end of a line. The caret inside `{ }` of
/// a tag, a call, or a table sits exactly there.
///
/// `{` is not one of them: `x: { a }` writes a type after a `:`.
fn closes_an_expression(text: &str) -> bool {
    matches!(text.chars().next(), Some(')' | '}' | ']' | ','))
}

/// The byte offsets where an access operator wants a name and none
/// follows: `a.`, `a:`, `a?.`, `a!.`, `a?:`, `a!:`, `a[`, `a?[`,
/// `a![`. Each carries the text the repair writes there. The parser
/// wants a name after a separator and a key inside a bracket.
///
/// A bracket goes by the end of the line alone: `x[]` closes itself,
/// and the placeholder carries a `]` that would then stand twice.
fn dangling_members(source: &str) -> Vec<(usize, &'static str)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(source) else {
        return Vec::new();
    };
    let mut spots = Vec::new();

    for (i, tok) in lexed.toks.iter().enumerate() {
        let (fill, member) = match tok.kind {
            TokKind::Dot | TokKind::Colon => (HOLE, true),

            // `a ??` wants a value after it the way `a.` wants a name,
            // and one dangling `??` stopped the repair for the whole
            // file. The lexer reads the pair as two `?`, so the spot is
            // the second one when the first sits right against it.
            TokKind::Symbol if tok.text(source) == "?" && follows_a_question(&lexed, source, i) => {
                (HOLE, true)
            }

            TokKind::Symbol if tok.text(source) == "[" => (BRACKET_HOLE, false),

            _ => continue,
        };

        let end = tok.end as usize;

        match lexed.toks.get(i + 1) {
            Some(next) => {
                let dangles = source[end..next.start as usize].contains('\n')
                    || (member && closes_an_expression(next.text(source)));

                if dangles {
                    spots.push((end, fill));
                }
            }

            None => spots.push((end, fill)),
        }
    }

    spots
}

/// Whether the token at `i` closes a `??`: the token before it is a `?`
/// with no byte between the two.
fn follows_a_question(lexed: &alloy_syntax::lexer::Lexed, source: &str, i: usize) -> bool {
    i.checked_sub(1)
        .and_then(|k| lexed.toks.get(k))
        .is_some_and(|prev| {
            matches!(prev.kind, TokKind::Symbol)
                && prev.text(source) == "?"
                && prev.end == lexed.toks[i].start
        })
}

/// The source with a placeholder after every dangling access operator,
/// and where each one went. `None` when the source has none.
fn repaired_source(source: &str) -> Option<(String, Vec<(usize, usize)>)> {
    let spots = dangling_members(source);

    if spots.is_empty() {
        return None;
    }

    let mut text = source.to_string();

    for (at, fill) in spots.iter().rev() {
        text.insert_str(*at, fill);
    }

    Some((
        text,
        spots
            .into_iter()
            .map(|(at, fill)| (at, fill.len()))
            .collect(),
    ))
}

impl Doc {
    pub fn new(
        source: String,
        version: i64,
        options: &EmitOptions,
        jsx: &alloy::luaux::Config,
        ingots: Option<&alloy::ingot::Ingots>,
    ) -> Self {
        let mut source = source;
        let bom = source.starts_with(MARK);

        strip_mark(&mut source);

        let mut doc = Self {
            source,
            version,
            output: None,
            shadow: String::new(),
            exports: Vec::new(),
            decls: Vec::new(),
            namespaces: Vec::new(),
            impl_blocks: Vec::new(),
            tables: Vec::new(),
            namespace_ranges: Vec::new(),
            bindings: Vec::new(),
            shapes: Vec::new(),
            import_shapes: Vec::new(),
            import_decls: Vec::new(),
            interfaces: Vec::new(),
            import_interfaces: Vec::new(),
            import_sources: Vec::new(),
            error: None,
            is_alx: options.file_name.ends_with(".alx"),
            repair: None,
            blanked: Vec::new(),
            bom,
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
        // Every fact below reads the tokens, not the parse. A string
        // the author has just opened stops the lexer, so all of them
        // would go empty on the keystroke after the quote; the blanked
        // copy keeps them, and it has the length of the source, so
        // every offset still means what it did.
        let text = lexable(&self.source);
        let text = text.as_ref();
        self.exports = crate::imports::exports_of(text, self.is_alx);
        self.decls = alloy::declarations::summaries(text, options.definitions);
        self.namespaces = alloy::declarations::namespace_names(text);
        self.tables = alloy::tables::plain_tables(text);
        self.impl_blocks = alloy::impl_blocks::impl_blocks(text);
        self.namespace_ranges = alloy::declarations::namespace_ranges(text);
        self.bindings = alloy::declarations::bindings(text);
        self.shapes = alloy::declarations::shapes(text);
        self.interfaces = crate::shapes::interfaces(text);
        self.import_shapes =
            alloy::modules::import_shapes_for_file(std::path::Path::new(&options.file_name), text);
        self.import_sources =
            alloy::modules::import_sources_for_file(std::path::Path::new(&options.file_name), text);
        self.import_interfaces = self
            .import_sources
            .iter()
            .flat_map(|text| crate::shapes::interfaces(text))
            .collect();
        self.import_decls = self
            .import_sources
            .iter()
            .flat_map(|text| alloy::declarations::summaries(text, false))
            .collect();
        // `file_name` is the real path, which is what the ingots see.
        let compiled =
            alloy::compile_file(&options.file_name, &self.source, options, Some(jsx), ingots);

        // A dangling `a.`, `a:`, `a?.`, `a!.` or `a[` stops the
        // parser. The artifact then breaks off at the operator, and
        // the child has no answer for the caret sitting on it. A copy
        // with a placeholder after the operator parses, so the caret
        // still reaches the list of what stands before it.
        let repair = |source: &str| -> Option<Repair> {
            let (text, spots) = repaired_source(source)?;
            let out =
                alloy::compile_file(&options.file_name, &text, options, Some(jsx), ingots).ok()?;

            out.parsed_clean.then_some(Repair {
                source: text,
                output: out,
                spots,
            })
        };

        // A `.alx` whose markup cannot lower, because the factory is
        // not in scope or a tag names nothing, hands the child the
        // author's tags. The child reads none of it, so one bad tag
        // silences the whole file. Blanking every region to the width
        // it had leaves Alloy the parser reads, and every byte outside
        // a region keeps its offset, so the code around the markup
        // answers and each position still maps.
        let blanked = |source: &str| -> Option<(Vec<(usize, usize)>, Repair)> {
            // An unfinished tag stops the span scan, so `blank_markup`
            // reports no region and the child would read the tags. The
            // regions read from the text bound the broken one, and the
            // code after it answers.
            let (spans, text) = match alloy::alx::blank_markup(source) {
                Some(pair) => pair,

                None => {
                    let spans = crate::markup::recovered_spans(source);

                    if spans.is_empty() {
                        return None;
                    }

                    let text = alloy::luaux::resolve::blank_luaux_regions(source, &spans);

                    (spans, text)
                }
            };
            let compile = |text: &str| {
                alloy::compile_file(&options.file_name, text, options, Some(jsx), ingots).ok()
            };

            // A dangling operator stops the parser here as anywhere
            // else. The blanking keeps every offset, so the spots it
            // reports are the author's own.
            if let Some((filled, spots)) = repaired_source(&text)
                && let Some(out) = compile(&filled)
                && out.parsed_clean
            {
                return Some((
                    spans,
                    Repair {
                        source: filled,
                        output: out,
                        spots,
                    },
                ));
            }

            Some((
                spans,
                Repair {
                    output: compile(&text)?,
                    source: text,
                    spots: Vec::new(),
                },
            ))
        };
        self.blanked = Vec::new();

        match compiled {
            Ok(out) => {
                self.repair = (!out.parsed_clean).then(|| repair(&self.source)).flatten();
                self.shadow = match &self.repair {
                    Some(r) => r.output.check.clone(),

                    None => out.check.clone(),
                };
                self.output = Some(out);
                self.error = None;
            }

            Err(e) => {
                self.repair = repair(&self.source);

                if self.repair.is_none()
                    && self.is_alx
                    && let Some((spans, blank)) = blanked(&self.source)
                {
                    self.blanked = spans;
                    self.repair = Some(blank);
                }

                self.shadow = match &self.repair {
                    Some(r) => r.output.check.clone(),

                    None => plain_enough(&self.source),
                };
                self.output = None;
                self.error = Some(e);
            }
        }
    }

    /// The text the shadow came from: the repair pass's copy, or the
    /// source.
    fn compiled(&self) -> &str {
        match &self.repair {
            Some(r) => &r.source,

            None => &self.source,
        }
    }

    /// The compile the shadow and every position map belong to.
    pub fn mapping(&self) -> Option<&Output> {
        match &self.repair {
            Some(r) => Some(&r.output),

            None => self.output.as_ref(),
        }
    }

    fn spots(&self) -> &[(usize, usize)] {
        match &self.repair {
            Some(r) => &r.spots,

            None => &[],
        }
    }

    /// A source byte offset in the text the compile read.
    fn repaired_offset(&self, offset: usize) -> usize {
        offset
            + self
                .spots()
                .iter()
                .filter(|(at, _)| *at < offset)
                .map(|(_, len)| len)
                .sum::<usize>()
    }

    /// A byte offset of the compiled text back in the source. A byte
    /// inside a placeholder maps to the operator the placeholder
    /// follows, so the author never sees text no one wrote.
    fn out_of_repair(&self, offset: usize) -> usize {
        let mut shift = 0usize;

        for (at, len) in self.spots() {
            let start = at + shift;

            if offset < start {
                break;
            }

            if offset < start + len {
                return *at;
            }

            shift += len;
        }

        offset - shift
    }

    /// A source position in the text the compile read.
    fn repaired_position(&self, line: u32, character: u32) -> (u32, u32) {
        if self.repair.is_none() {
            return (line, character);
        }

        let Some(offset) = offset_of(&self.source, line, character) else {
            return (line, character);
        };

        position_of(self.compiled(), self.repaired_offset(offset))
    }

    /// A position of the compiled text back in the source.
    fn out_of_repair_position(&self, line: u32, character: u32) -> (u32, u32) {
        if self.repair.is_none() {
            return (line, character);
        }

        let Some(offset) = offset_of(self.compiled(), line, character) else {
            return (line, character);
        };

        position_of(&self.source, self.out_of_repair(offset))
    }

    /// Whether a source byte was copied into the shadow.
    pub fn maps_to_shadow(&self, offset: usize) -> bool {
        self.mapping().is_some_and(|out| {
            out.map
                .to_output(self.repaired_offset(offset) as u32)
                .is_some()
        })
    }

    /// A source position as a shadow position.
    pub fn to_shadow(&self, line: u32, character: u32) -> (u32, u32) {
        let Some(out) = self.mapping() else {
            return (line, character);
        };
        let (line, character) = self.repaired_position(line, character);
        let text = self.compiled();

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
        let Some(out) = self.mapping() else {
            return (line, character);
        };

        let Some(offset) = offset_of(&self.shadow, line, character) else {
            return (line, character);
        };

        let src = out.map.to_source(offset as u32) as usize;
        let text = self.compiled();
        let (line, col) = position_of(text, src.min(text.len()));

        self.out_of_repair_position(line, col)
    }

    /// Whether a shadow position sits in text no author wrote: the
    /// desugar's, the markup lowering's, or an ingot's edit.
    pub fn generated_at(&self, line: u32, character: u32) -> bool {
        let Some(offset) = offset_of(&self.shadow, line, character) else {
            return false;
        };

        self.generated_offset(offset)
    }

    /// The same, for a shadow byte offset.
    pub fn generated_offset(&self, offset: usize) -> bool {
        self.mapping()
            .is_some_and(|out| out.map.is_generated(offset as u32))
    }

    /// Whether a source byte sits in markup the shadow blanked. The
    /// artifact holds nothing of the author's there.
    pub fn in_blanked_markup(&self, offset: usize) -> bool {
        self.blanked
            .iter()
            .any(|(start, end)| (*start..*end).contains(&offset))
    }

    /// Applies one LSP content change. A whole-document change says
    /// again whether the editor's text opens with the mark.
    pub fn apply_change(&mut self, range: Option<((u32, u32), (u32, u32))>, text: &str) {
        if range.is_none() {
            self.bom = text.starts_with(MARK);
        }

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

    strip_mark(source);
}

/// The byte offset of an LSP position, clamped to the line.
pub fn offset_of(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut start = 0usize;

    for (i, l) in text.split('\n').enumerate() {
        if i as u32 == line {
            let mut units = 0u32;
            // The editor hides a leading byte order mark; column 0 of
            // line 0 is the first character after it.
            let l = match i == 0 {
                true => l
                    .strip_prefix('\u{feff}')
                    .inspect(|_| start += 3)
                    .unwrap_or(l),

                false => l,
            };

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
        .filter(|c| *c != '\u{feff}' || line_start > 0)
        .map(|c| c.len_utf16() as u32)
        .sum();

    (line, character)
}

#[cfg(test)]
mod position_tests {
    use super::*;

    #[test]
    fn a_leading_byte_order_mark_is_no_column() {
        let text = "\u{feff}local x = 1\nlocal y = 2\n";
        assert_eq!(position_of(text, 3 + 6), (0, 6));
        assert_eq!(offset_of(text, 0, 6), Some(3 + 6));
        assert_eq!(position_of(text, 3 + 12 + 6), (1, 6));
        assert_eq!(offset_of(text, 1, 6), Some(3 + 12 + 6));
        assert_eq!(position_of("local x", 6), (0, 6));
        assert_eq!(offset_of("local x", 0, 6), Some(6));
    }
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

    /// A leading byte order mark leaves the document at the door. The
    /// Luau lexer refuses it, and a shadow that carries it moves every
    /// column of the first line one to the right.
    #[test]
    fn a_byte_order_mark_leaves_the_source_and_the_shadow() {
        let mut doc = Doc::new(
            format!("{MARK}local greeting = \"hi\"\nprint(greeting)\n"),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );

        // The mark is remembered, so `format_document` writes it back.
        assert!(doc.bom);
        assert!(!doc.source.starts_with(MARK));
        assert!(!doc.shadow.starts_with(MARK));
        assert_eq!(&doc.source[6..14], "greeting");
        assert_eq!(offset_of(&doc.source, 0, 6), Some(6));
        assert_eq!(position_of(&doc.source, 6), (0, 6));

        // The editor saved the file without the mark.
        doc.apply_change(None, "local greeting = \"ho\"\n");

        assert!(!doc.bom);

        // And put it back.
        doc.apply_change(None, &format!("{MARK}local greeting = \"hi\"\n"));

        assert!(doc.bom);
        assert_eq!(doc.source, "local greeting = \"hi\"\n");
    }

    #[test]
    fn utf16_columns_count_units() {
        let text = "😀x";
        assert_eq!(offset_of(text, 0, 2), Some(4));
        assert_eq!(position_of(text, 4), (0, 2));
    }

    /// `HashMap.new().` with `end` after it: the parser wants a name,
    /// the artifact stops there, and the child answered nothing. The
    /// repair pass writes a placeholder, so the caret still sits on a
    /// member of the call's result.
    #[test]
    fn a_dangling_member_operator_keeps_the_artifact() {
        for op in [".", ":", "?.", "!.", "?:", "!:"] {
            let src = format!("local function go()\n    HashMap.new(){op}\nend\n");
            let doc = Doc::new(
                src.clone(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            );
            let repair = doc.repair.as_ref().unwrap_or_else(|| panic!("{op}"));
            assert!(repair.source.contains(&format!("HashMap.new(){op}{HOLE}")));
            assert_eq!(repair.spots.len(), 1);

            // The caret is past the operator. It maps to a shadow
            // position, which is what the child needs to answer, and
            // that position maps back to the caret.
            let column = 17 + op.len() as u32;
            let shadow = doc.to_shadow(1, column);
            assert_ne!(shadow, (1, column), "{op}");
            assert_eq!(doc.to_source(shadow.0, shadow.1), (1, column), "{op}");
        }
    }

    /// The compiler still reports the syntax error: the repair is for
    /// the child alone, so `output` stays the author's own compile.
    #[test]
    fn a_repair_leaves_the_diagnostics_alone() {
        let src = "local m = HashMap.new()\nm.\nlocal rest = 1\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        assert!(doc.repair.is_some());
        let out = doc.output.as_ref().expect("output");
        assert!(!out.parsed_clean);
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.message.contains("expected a name")),
            "{:?}",
            out.diagnostics
        );
    }

    /// A source with no dangling operator compiles the way it always
    /// did, and every position maps as before.
    #[test]
    fn a_clean_source_needs_no_repair() {
        let src = "local m = HashMap.new()\nprint(m)\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        assert!(doc.repair.is_none());
        assert!(dangling_members(src).is_empty());
    }

    /// `a ??` wants a value the way `a.` wants a name. One dangling
    /// `??` on a line of its own stopped the repair for the whole file,
    /// and a `b?.` in another function then fell through to the global
    /// scope.
    #[test]
    fn a_dangling_coalesce_repairs_too() {
        let src = "local n = 1
local r = n ??
print(r)
";

        assert_eq!(dangling_members(src), vec![(26, HOLE)]);

        // An optional type and a guarded access carry a `?` of their
        // own, and neither one wants a value after it.
        assert!(
            dangling_members(
                "local b: number? = nil
print(b)
"
            )
            .is_empty()
        );
        assert_eq!(
            dangling_members(
                "local b: { v: number }? = nil
local r = b?.
"
            ),
            vec![(43, HOLE)]
        );
    }

    /// A `.` inside a string, a comment, or a number is no operator.
    #[test]
    fn the_repair_reads_tokens_not_text() {
        assert!(dangling_members("local s = \"a.\"\nprint(s)\n").is_empty());
        assert!(dangling_members("-- a.\nlocal x = 1\n").is_empty());
        assert!(dangling_members("local x = 1.\nprint(x)\n").is_empty());
        assert_eq!(dangling_members("local m = a\nm.\n"), vec![(14, HOLE)]);
        assert!(dangling_members("local s = \"a[\"\nprint(s)\n").is_empty());
    }

    /// `x[`, `x?[` and `x![` with nothing after the bracket: the key
    /// and the `]` are both missing, so the placeholder carries them.
    #[test]
    fn a_dangling_bracket_keeps_the_artifact() {
        for op in ["[", "?[", "!["] {
            let src = format!("local t = {{ a = 1 }}\nlocal v = t{op}\nprint(v)\n");
            let doc = Doc::new(
                src.clone(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            );
            let repair = doc.repair.as_ref().unwrap_or_else(|| panic!("{op}"));
            assert!(
                repair.source.contains(&format!("t{op}{BRACKET_HOLE}")),
                "{op}: {}",
                repair.source
            );
            assert_eq!(repair.spots.len(), 1);

            // The caret sits inside the bracket. It maps to a shadow
            // position, and that position maps back to the caret.
            let column = 11 + op.len() as u32;
            let shadow = doc.to_shadow(1, column);
            assert_eq!(doc.to_source(shadow.0, shadow.1), (1, column), "{op}");
        }
    }

    /// A member operator with a closer right after it wants a name as
    /// much as one at the end of a line: the caret inside `{ }` of a
    /// tag, a call, or a table stands there while the author types.
    #[test]
    fn a_closer_after_a_member_operator_dangles_too() {
        for (src, at) in [
            ("local e = <Frame Size={xs?[1]?.}>\n", 31),
            ("local t = { a = xs. }\n", 19),
            ("print(xs.)\n", 9),
            ("local a = [ xs., 1 ]\n", 15),
        ] {
            assert_eq!(dangling_members(src), vec![(at, HOLE)], "{src}");
        }

        // A `:` writes a type as often as it names a method, and a type
        // opens with a token that is no name.
        assert!(dangling_members("type T = { f: (number) -> () }\n").is_empty());
        assert!(dangling_members("local function f(a: { n: number }) end\n").is_empty());

        // The bracket goes by the end of the line alone: `x[]` closes
        // itself, and the placeholder carries a `]` of its own.
        assert!(dangling_members("local v = xs[]\n").is_empty());
    }

    /// A bracket that opens a key on the next line parses on its own,
    /// so the repair never runs and the artifact stays the author's.
    #[test]
    fn a_bracket_that_closes_needs_no_repair() {
        let src = "local t = { a = 1 }\nlocal v = t[\n    \"a\"\n]\nprint(v)\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        assert!(doc.repair.is_none());
    }

    /// A `.alx` whose markup cannot lower: the factory is not in scope.
    /// The shadow used to be the author's markup, which the child reads
    /// as nothing. It is now the file with the tags blanked, so the
    /// code around them still compiles and maps.
    #[test]
    fn markup_that_cannot_lower_blanks_and_the_rest_compiles() {
        let src = "local x = 1\nlocal e = <Frame Size={x}>\n</Frame>\nreturn e\n";
        let options = EmitOptions {
            file_name: "/w/p.alx".to_string(),
            ..EmitOptions::default()
        };
        let doc = Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        );

        // The one error the file gets is the markup's own.
        let error = doc.error.as_ref().expect("the markup error");
        assert!(error.message.contains("not in scope"), "{}", error.message);
        assert_eq!(doc.blanked.len(), 1);

        // The lines around the tag are the author's, byte for byte, and
        // the tag itself is gone.
        assert!(doc.shadow.starts_with("local x = 1\n"), "{}", doc.shadow);
        assert!(doc.shadow.contains("return e"), "{}", doc.shadow);
        assert!(!doc.shadow.contains('<'), "{}", doc.shadow);

        // `x` on the first line still maps both ways.
        assert_eq!(doc.to_shadow(0, 6), (0, 6));
        assert_eq!(doc.to_source(0, 6), (0, 6));

        // Inside the tag there is nothing of the author's.
        let at = src.find("<Frame").expect("the tag") + 2;
        assert!(doc.in_blanked_markup(at));
        assert!(!doc.in_blanked_markup(6));
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
