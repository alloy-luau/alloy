//! `.alx`: markup lowered by luaux, then the Alloy desugar.
//!
//! luaux runs first and is text-local: it replaces each markup region with
//! calls and passes every other byte through, so Alloy syntax outside and
//! inside `{ }` holes reaches the desugar unchanged. Every element lands
//! on its source line, so the line count holds across both passes.

use std::collections::HashSet;

use alloy_syntax::lexer::{Tok, TokKind};

use crate::{CompileError, Diagnostic, EmitOptions, Output};

/// One `.alx` compile: the Alloy output of the lowered text, plus the
/// text itself for a caller that maps positions.
pub struct AlxOutput {
    pub output: Output,
    /// The lowered Alloy source luaux produced.
    pub lowered: String,
}

/// Compiles `.alx` source with a luaux config, usually from `luaux.toml`.
///
/// Markup errors and Alloy diagnostics both land in `output.diagnostics`
/// with offsets into the `.alx` source. An Alloy diagnostic keeps its line
/// and clamps its column, because the lowered line may be longer.
pub fn compile_alx(
    src: &str,
    options: &EmitOptions,
    mut config: luaux::Config,
) -> Result<AlxOutput, CompileError> {
    let spans = luaux::compile::markup_spans(src).map_err(|e| CompileError {
        offset: e.offset,
        message: e.message,
    })?;
    let blanked = luaux::resolve::blank_luaux_regions(src, &spans);
    config.extra_bound = bound_names(&blanked);

    let compiled = match config.backend {
        luaux::config::BackendKind::Table => {
            luaux::compile::compile_recovering(src, &luaux::Table, config)
        }

        luaux::config::BackendKind::Element => {
            luaux::compile::compile_recovering(src, &luaux::Element, config)
        }
    }
    .map_err(|e| CompileError {
        // luaux reports a scope error against the whole file; the first
        // tag is the place the reader can act on.
        offset: if e.offset == 0 {
            spans.first().map_or(0, |(at, _)| *at)
        } else {
            e.offset
        },
        message: markup_message(&e.message, e.help.as_deref()),
    })?;

    let lowered = compiled.output;
    let mut output = crate::compile_with(&lowered, options)?;

    for d in &mut output.diagnostics {
        d.start = remap(&lowered, src, d.start);
        d.end = remap(&lowered, src, d.end);
    }

    // A component is a function a tag names, `<Row />`, so its name
    // is PascalCase by the markup's own rule.
    output.lints.retain(|l| l.name != "pascal_case_function");

    for l in &mut output.lints {
        l.start = remap(&lowered, src, l.start);
        l.end = remap(&lowered, src, l.end);
    }

    for e in compiled.errors {
        output.diagnostics.push(Diagnostic {
            start: e.offset as u32,
            end: (e.offset + e.length) as u32,
            message: markup_message(&e.message, e.help.as_deref()),
        });
    }

    for w in compiled.warnings {
        output.diagnostics.push(Diagnostic {
            start: w.offset as u32,
            end: (w.offset + w.length) as u32,
            message: markup_message(&w.message, w.help.as_deref()),
        });
    }

    output.diagnostics.sort_by_key(|d| d.start);
    output.lowered = Some(lowered.clone());

    Ok(AlxOutput { output, lowered })
}

fn markup_message(message: &str, help: Option<&str>) -> String {
    // luaux names its own `luaux.toml` tables; an Alloy project writes
    // the same keys under `[alx]`.
    let alloy_keys = |text: &str| {
        text.replace("[factory]", "[alx.factory]")
            .replace("[lints]", "[alx.lints]")
            .replace("[build]", "[alx.build]")
            .replace("luaux.toml", "alloy.toml")
    };

    match help {
        Some(h) => format!("markup: {} ({})", alloy_keys(message), alloy_keys(h)),

        None => format!("markup: {}", alloy_keys(message)),
    }
}

/// An offset in the lowered text as an offset in the source: same line,
/// column through `map_column`.
fn remap(lowered: &str, src: &str, offset: u32) -> u32 {
    let offset = (offset as usize).min(lowered.len());
    let line = lowered[..offset].matches('\n').count();
    let line_start = lowered[..offset].rfind('\n').map_or(0, |i| i + 1);
    let col = offset - line_start;
    let low_line = lowered[line_start..].split('\n').next().unwrap_or("");

    let mut start = 0usize;

    for (i, l) in src.split('\n').enumerate() {
        if i == line {
            return (start + map_column(low_line, l, col)) as u32;
        }

        start += l.len() + 1;
    }

    src.len() as u32
}

/// A column of one line as a column of the other, for a line the
/// lowering rewrote. The word under the column finds its n-th twin in
/// the other line; a column outside a word, or a word the other line
/// lacks, clamps.
pub fn map_column(from: &str, to: &str, col: usize) -> usize {
    let col = col.min(from.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let bytes = from.as_bytes();

    let mut start = col;
    while start > 0 && is_word(bytes[start - 1] as char) {
        start -= 1;
    }

    let mut end = col;
    while end < bytes.len() && is_word(bytes[end] as char) {
        end += 1;
    }

    if start == end {
        return col.min(to.len());
    }

    let word = &from[start..end];
    let nth = word_starts(from, word).filter(|&s| s < start).count();

    match word_starts(to, word).nth(nth) {
        Some(at) => at + (col - start),

        None => col.min(to.len()),
    }
}

/// The starts of `word` in `text`, as whole words.
fn word_starts<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';

    text.match_indices(word).filter_map(move |(at, _)| {
        let before = text[..at].chars().next_back().is_some_and(is_word);
        let after = text[at + word.len()..].chars().next().is_some_and(is_word);

        (!before && !after).then_some(at)
    })
}

/// The names the file binds, by a token scan of the blanked source.
///
/// luaux collects bindings with full_moon, which does not read Alloy
/// syntax; this scan sees `import`, `const`, `struct`, and the rest. A
/// name that is not a binding but looks like one costs nothing: it only
/// lets `<Name>` resolve to a component.
pub fn bound_names(src: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return names;
    };
    let toks = &lexed.toks;
    let text = |t: &Tok| t.text(src);
    let is_ident = |t: &Tok| t.kind == TokKind::Ident;
    let mut i = 0;

    while i < toks.len() {
        let word = text(&toks[i]);

        match word {
            "local" | "const" => {
                i += 1;

                if i < toks.len() && text(&toks[i]) == "function" {
                    if let Some(t) = toks.get(i + 1).filter(|t| is_ident(t)) {
                        names.insert(text(t).to_string());
                    }

                    continue;
                }

                // `local a, b`, `local { a, b = c }`, `local [ x, ...rest ]`.
                let mut depth = 0i32;

                while i < toks.len() {
                    let t = &toks[i];
                    let s = text(t);

                    match s {
                        "{" | "[" => depth += 1,

                        "}" | "]" => depth -= 1,

                        "=" if depth == 0 => break,

                        ":" if depth == 0 => break,

                        _ if is_ident(t) => {
                            // In a table destructure `a = b` binds `b`; the
                            // name before `=` is a key. Keeping both is safe.
                            names.insert(s.to_string());
                        }

                        _ => {}
                    }

                    if depth == 0
                        && s != ","
                        && !is_ident(t)
                        && !matches!(s, "{" | "[" | "}" | "]" | "...")
                    {
                        break;
                    }

                    i += 1;
                }

                continue;
            }

            "function" => {
                if let Some(t) = toks.get(i + 1).filter(|t| is_ident(t)) {
                    names.insert(text(t).to_string());
                }
            }

            "struct" | "enum" | "trait" | "interface" | "remote" | "attribute" | "macro"
            | "class" => {
                if let Some(t) = toks.get(i + 1).filter(|t| is_ident(t)) {
                    names.insert(text(t).to_string());
                }
            }

            "import" => {
                // `import * as N`, `import D from`, `import { a as b, c }`.
                let mut j = i + 1;

                while j < toks.len() {
                    let t = &toks[j];
                    let s = text(t);

                    if s == "from" || matches!(t.kind, TokKind::Str { .. }) {
                        break;
                    }

                    // An alias `a as b` binds `b`; keeping `a` too is
                    // harmless, since a name only lets a tag resolve.
                    if is_ident(t) && s != "type" && s != "as" {
                        names.insert(s.to_string());
                    }

                    j += 1;
                }
            }

            _ => {}
        }

        i += 1;
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_sees_alloy_bindings() {
        let names = bound_names(
            "import * as React from \"x\"\nimport { a as b, type T } from \"y\"\nconst Row = 1\nlocal { w = h } = t\nstruct Card as end\nlocal function f() end\n",
        );

        for n in ["React", "b", "Row", "h", "Card", "f"] {
            assert!(names.contains(n), "{n} missing from {names:?}");
        }

        assert!(!names.contains("from"));
    }

    #[test]
    fn remap_keeps_the_line_and_clamps_the_column() {
        let lowered = "aaaa\nbbbbbbbbbb\ncc";
        let src = "aaaa\nbbb\ncc";
        assert_eq!(remap(lowered, src, 5), 5);
        assert_eq!(remap(lowered, src, 12), 8);
        assert_eq!(remap(lowered, src, 16), 9);
    }
}
