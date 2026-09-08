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
        message: markup_message(&e.message, None),
    })?;
    let blanked = luaux::resolve::blank_luaux_regions(src, &spans);
    let bound = bound_names(&blanked);
    config.extra_bound = bound.clone();

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

    for d in component_props_problems(src, &bound) {
        output.diagnostics.push(d);
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

/// One prop a component declares.
struct Prop {
    name: String,
    ty: String,
    optional: bool,
}

/// The props a tag may set without the component declaring them: React
/// reads `key` itself, and it never reaches the component.
pub const FREE_PROPS: &[&str] = &["key"];

/// The attributes of every component tag, against the props the
/// component declares: a prop it does not take, a required prop the tag
/// leaves out, and a literal of the wrong type.
fn component_props_problems(src: &str, bound: &HashSet<String>) -> Vec<Diagnostic> {
    let Ok(spans) = luaux::compile::markup_spans(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for (start, _) in spans {
        let Ok((node, _)) = luaux::markup::parse_node(src, start) else {
            continue;
        };
        check_node(&node, src, bound, &mut out);
    }

    out.sort_by_key(|d| d.start);
    out.dedup_by(|a, b| a.start == b.start && a.message == b.message);

    out
}

fn check_node(
    node: &luaux::markup::Node,
    src: &str,
    bound: &HashSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    use luaux::markup::{Child, Node};

    let children = match node {
        Node::Element(e) => {
            check_element(e, src, bound, out);
            &e.children
        }

        Node::Fragment(f) => &f.children,
    };

    for child in children {
        match child {
            Child::Node(n) => check_node(n, src, bound, out),

            // A tag inside a hole is one expression to the markup
            // parser, so its own region is parsed from the text.
            Child::Expression { expression, span } => {
                let mut at = span.start;

                while let Some(lt) = src.get(at..span.end).and_then(|t| t.find('<')) {
                    at += lt;

                    match luaux::markup::parse_node(src, at) {
                        Ok((inner, next)) => {
                            check_node(&inner, src, bound, out);
                            at = next.max(at + 1);
                        }

                        Err(_) => at += 1,
                    }
                }

                let _ = expression;
            }

            _ => {}
        }
    }
}

fn check_element(
    element: &luaux::markup::Element,
    src: &str,
    bound: &HashSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    use luaux::markup::Attribute;

    let name = element.name.as_written();

    if luaux::roblox::is_class(&name) || !bound.contains(&name) {
        return;
    }

    let Some(props) = component_props(src, &name) else {
        return;
    };
    let mut set: Vec<String> = Vec::new();
    let mut spread = false;

    for attribute in &element.attributes {
        let (attr, span, value) = match attribute {
            Attribute::Named { name, span, value } => (name.clone(), *span, Some(value)),

            Attribute::Inferred { expression, span } => {
                let last = expression.rsplit('.').next().unwrap_or(expression).trim();

                if last.is_empty() || !last.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }

                (last.to_string(), *span, None)
            }

            Attribute::Spread { .. } => {
                spread = true;
                continue;
            }
        };
        set.push(attr.clone());

        if FREE_PROPS.contains(&attr.as_str()) {
            continue;
        }

        let Some(prop) = props.iter().find(|p| p.name == attr) else {
            let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
            let message = match nearest(&attr, &names) {
                Some(m) => format!("markup: {name} has no prop named {attr} (did you mean {m}?)"),

                None => format!("markup: {name} has no prop named {attr}"),
            };
            out.push(Diagnostic {
                start: span.start as u32,
                end: (span.start + attr.len()) as u32,
                message,
            });
            continue;
        };

        let Some(got) = value.and_then(literal_type) else {
            continue;
        };
        let want = prop.ty.trim().trim_end_matches('?');

        if matches!(want, "string" | "number" | "boolean") && want != got {
            out.push(Diagnostic {
                start: span.start as u32,
                end: (span.start + attr.len()) as u32,
                message: format!(
                    "markup: prop {attr} of {name} is {}, not {got}",
                    prop.ty.trim()
                ),
            });
        }
    }

    if spread {
        return;
    }

    let missing: Vec<&str> = props
        .iter()
        .filter(|p| !p.optional && !set.contains(&p.name))
        .map(|p| p.name.as_str())
        .collect();

    if !missing.is_empty() {
        let tag = element.span.start + 1;
        out.push(Diagnostic {
            start: tag as u32,
            end: (tag + name.len()) as u32,
            message: format!(
                "markup: `<{name}>` leaves {} unset",
                crate::desugar::list_names(&missing)
            ),
        });
    }
}

/// The nearest name within an edit distance of two.
fn nearest(word: &str, names: &[&str]) -> Option<String> {
    names
        .iter()
        .map(|n| (edit_distance(n, word), *n))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, n)| n.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }

    row[b.len()]
}

/// The type of an attribute value the reader can name without the
/// checker: a literal. Anything else is `None`.
fn literal_type(value: &luaux::markup::AttributeValue) -> Option<&'static str> {
    use luaux::markup::AttributeValue;

    let text = match value {
        AttributeValue::StringLiteral(_) => return Some("string"),

        AttributeValue::Boolean => return Some("boolean"),

        AttributeValue::Expression(e) => e.trim(),
    };

    if text.starts_with('"') || text.starts_with('\'') || text.starts_with('`') {
        return Some("string");
    }

    if text == "true" || text == "false" {
        return Some("boolean");
    }

    text.parse::<f64>().is_ok().then_some("number")
}

/// The props a component declares, from the record its parameter names.
/// `None` when the file does not declare the component, or it writes no
/// parameter type: there is then nothing to check against.
fn component_props(src: &str, name: &str) -> Option<Vec<Prop>> {
    let at = src.find(&format!("function {name}("))?;
    let rest = &src[at..];
    let open = rest.find('(')?;
    let close = rest.find(')')?;
    let (_, declared) = rest.get(open + 1..close)?.split_once(':')?;
    let declared = declared.trim();

    let record = match declared.starts_with('{') {
        true => balanced_record(declared)?.to_string(),

        // `props: Props`, where `type Props = { ... }`.
        false => {
            let alias = src.find(&format!("type {declared} ="))?;
            let body = src[alias..].split_once('=')?.1.trim_start();

            balanced_record(body)?.to_string()
        }
    };

    Some(record_props(&record))
}

/// The text from `{` to the `}` that closes it.
fn balanced_record(text: &str) -> Option<&str> {
    if !text.starts_with('{') {
        return None;
    }

    let mut depth = 0i32;

    for (i, c) in text.char_indices() {
        match c {
            '{' => depth += 1,

            '}' => {
                depth -= 1;

                if depth == 0 {
                    return Some(&text[..=i]);
                }
            }

            _ => {}
        }
    }

    None
}

/// `{ label: string, size: number? }` as its fields.
fn record_props(record: &str) -> Vec<Prop> {
    let inner = record
        .trim()
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .unwrap_or("");
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut field = String::new();

    let mut previous = ' ';

    for c in inner.chars() {
        match c {
            '{' | '(' | '[' | '<' => depth += 1,

            // The `>` of `->` closes nothing; a function type in a
            // record would otherwise unbalance the count and swallow
            // the comma that ends the field.
            '>' if matches!(previous, '-' | '=') => {}

            '}' | ')' | ']' | '>' => depth -= 1,

            ',' | ';' if depth == 0 => {
                push_prop(&field, &mut out);
                field.clear();
                continue;
            }

            _ => {}
        }

        field.push(c);
        previous = c;
    }

    push_prop(&field, &mut out);

    out
}

fn push_prop(field: &str, out: &mut Vec<Prop>) {
    let Some((name, ty)) = field.split_once(':') else {
        return;
    };
    let name = name.trim();
    let ty = ty.trim().trim_end_matches([',', ';']).trim();
    let optional = name.ends_with('?') || ty.ends_with('?');
    let name = name.trim_end_matches('?');

    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return;
    }

    out.push(Prop {
        name: name.to_string(),
        ty: ty.to_string(),
        optional,
    });
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
        Some(h) => format!(
            "markup: {} ({})",
            quote_tags(&alloy_keys(message)),
            quote_tags(&alloy_keys(h))
        ),

        None => format!("markup: {}", quote_tags(&alloy_keys(message))),
    }
}

/// A tag inside a message reads as code, the way every other name the
/// compiler prints does: `` `</Frame>` ``.
fn quote_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;

    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let slash = usize::from(after.starts_with('/'));
        let len = after[slash..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count();
        let name_end = at + 1 + slash + len;

        if len == 0 || out.ends_with('`') {
            out.push('<');
            rest = after;

            continue;
        }

        let end = name_end + usize::from(rest[name_end..].starts_with('>'));
        out.push('`');
        out.push_str(&rest[at..end]);
        out.push('`');
        rest = &rest[end..];
    }

    out.push_str(rest);

    out
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

    /// The props of a component tag: a name it does not take, a
    /// required one it leaves out, and a literal of the wrong type.
    #[test]
    fn a_component_tag_checks_its_props() {
        let src = "import * as React from \"@packages/react\" --@alloy-ignore\n\
local function Badge(props: { label: string, size: number? })\n\
    return (<TextLabel Text={props.label} />)\n\
end\n\
\n\
local function Bad(props: { title: string })\n\
    return (\n\
        <Frame>\n\
            <Badge title={props.title} />\n\
            <Badge label={7} />\n\
            <Badge label=\"ok\" />\n\
        </Frame>\n\
    )\n\
end\n\
\n\
return Bad\n";
        let out = compile_alx(src, &EmitOptions::default(), luaux::Config::default())
            .expect("the markup compiles");
        let messages: Vec<&str> = out
            .output
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert!(
            messages.contains(&"markup: Badge has no prop named title"),
            "{messages:?}"
        );
        assert!(
            messages.contains(&"markup: `<Badge>` leaves `label` unset"),
            "{messages:?}"
        );
        assert!(
            messages.contains(&"markup: prop label of Badge is string, not number"),
            "{messages:?}"
        );
        // An optional prop left out, and a good tag, say nothing.
        assert_eq!(messages.len(), 3, "{messages:?}");
    }

    /// A prop whose type ends in `?` needs no value. The `>` of a
    /// function type closes nothing, so the field before it still ends
    /// at its comma.
    #[test]
    fn an_optional_prop_with_a_function_type_needs_no_value() {
        let src = "import * as React from \"@packages/react\" --@alloy-ignore\n\
type Props = {\n\
    title: string,\n\
    on_click: (() -> ())?,\n\
}\n\
\n\
local function Badge(props: Props)\n\
    return (<TextLabel Text={props.title} />)\n\
end\n\
\n\
local function Panel()\n\
    return (\n\
        <Frame>\n\
            <Badge title=\"ok\" />\n\
            <Badge />\n\
        </Frame>\n\
    )\n\
end\n\
\n\
return Panel\n";
        let out = compile_alx(src, &EmitOptions::default(), luaux::Config::default())
            .expect("the markup compiles");
        let messages: Vec<&str> = out
            .output
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            messages,
            vec!["markup: `<Badge>` leaves `title` unset"],
            "{messages:?}"
        );
    }

    /// A tag inside a `{ }` hole is checked too, and `key` is React's.
    #[test]
    fn a_component_tag_inside_a_hole_checks_its_props() {
        let src = "import * as React from \"@packages/react\" --@alloy-ignore\n\
local function Badge(props: { label: string })\n\
    return (<TextLabel Text={props.label} />)\n\
end\n\
\n\
local function List(props: { rows: string[] })\n\
    return (\n\
        <Frame>\n\
            {props.rows:map(function(s) return <Badge key={s} lable={s} /> end)}\n\
        </Frame>\n\
    )\n\
end\n\
\n\
return List\n";
        let out = compile_alx(src, &EmitOptions::default(), luaux::Config::default())
            .expect("the markup compiles");
        let messages: Vec<&str> = out
            .output
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert!(
            messages.contains(&"markup: Badge has no prop named lable (did you mean label?)"),
            "{messages:?}"
        );
        assert!(
            !messages.iter().any(|m| m.contains("key")),
            "`key` is React's own: {messages:?}"
        );
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
