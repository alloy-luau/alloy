//! `alloy fmt` for `.alx` files: the code through Anneal, the markup
//! through a printer over the luaux tree, with the `[fmt.alx]` options.
//!
//! Each outermost markup region becomes a placeholder name while the
//! code formats, so the code formatter never sees a tag. The printed
//! markup then takes the placeholder's place, indented from the line it
//! lands on. An expression hole formats the same way, so markup nested
//! in a hole prints through the same printer.

use luaux::markup::{Attribute, AttributeValue, Child, Element, ElementName, Node};

use crate::config::{AttributeQuotes, FmtConfig, IndentType, QuoteStyle, TextWrap};
use crate::fmt::{format_with, requote};

/// One printed line of markup: an indentation level, relative to the
/// line the markup starts on, and the text.
type Line = (usize, String);

const PLACEHOLDER: &str = "__ALX";

/// Formats `.alx` source. `Err` carries the first lexer or markup error.
pub fn format_alx(src: &str, options: &FmtConfig) -> Result<String, String> {
    let spans = luaux::compile::markup_spans(src).map_err(|e| e.message)?;

    if spans.is_empty() {
        return format_with(src, options);
    }

    let mut code = String::with_capacity(src.len());
    let mut printed: Vec<Vec<Line>> = Vec::new();
    let mut last = 0;

    for (n, (a, b)) in spans.iter().enumerate() {
        code.push_str(&src[last..*a]);
        let (node, _) = luaux::markup::parse_node(src, *a).map_err(|e| e.message)?;
        let lines = print_node(src, &node, options, 0);
        let width = if lines.len() == 1 {
            lines[0].1.chars().count()
        } else {
            options.column_width + 1
        };
        code.push_str(&placeholder(n, width));
        printed.push(lines);
        last = *b;
    }

    code.push_str(&src[last..]);
    let formatted = format_with(&code, options)?;
    Ok(substitute(&formatted, &printed, options))
}

/// A name of the given width; the code formatter lays it out as one
/// long token, so multi-line markup breaks the group it sits in.
fn placeholder(n: usize, width: usize) -> String {
    let mut name = format!("{PLACEHOLDER}{n}_");

    while name.chars().count() < width {
        name.push('_');
    }

    name
}

fn substitute(formatted: &str, printed: &[Vec<Line>], options: &FmtConfig) -> String {
    let mut out = String::with_capacity(formatted.len() * 2);

    for line in formatted.lines() {
        let base: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        let mut rest = line;

        while let Some(at) = rest.find(PLACEHOLDER) {
            out.push_str(&rest[..at]);
            let tail = &rest[at + PLACEHOLDER.len()..];
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            let n: usize = digits.parse().unwrap_or(0);
            let after = tail[digits.len()..].trim_start_matches('_');
            let lines = &printed[n];

            for (k, (level, text)) in lines.iter().enumerate() {
                if k > 0 {
                    out.push('\n');
                    out.push_str(&base);
                    out.push_str(&indent(options, *level));
                }

                out.push_str(text);
            }

            rest = after;
        }

        out.push_str(rest);
        out.push('\n');
    }

    out
}

fn indent(options: &FmtConfig, level: usize) -> String {
    match options.indent_type {
        IndentType::Tabs => "\t".repeat(level),
        IndentType::Spaces => " ".repeat(level * options.indent_width),
    }
}

fn indent_width(options: &FmtConfig) -> usize {
    match options.indent_type {
        IndentType::Tabs => 4,
        IndentType::Spaces => options.indent_width,
    }
}

// --- the printer ---------------------------------------------------------------------

fn print_node(src: &str, node: &Node, options: &FmtConfig, level: usize) -> Vec<Line> {
    match node {
        Node::Element(el) => print_element(src, el, options, level),

        Node::Fragment(f) => print_tag(
            src,
            "",
            &[],
            &f.children,
            f.span.start,
            f.span.end,
            options,
            level,
        ),
    }
}

fn print_element(src: &str, el: &Element, options: &FmtConfig, level: usize) -> Vec<Line> {
    let name = match &el.name {
        ElementName::Simple(n) => n.clone(),
        ElementName::Member(parts) => parts.join("."),
    };

    print_tag(
        src,
        &name,
        &el.attributes,
        &el.children,
        el.span.start,
        el.span.end,
        options,
        level,
    )
}

/// A piece of the markup, already printed: one or more lines.
struct Piece {
    lines: Vec<Line>,
    /// Text and single-line holes flow together on one line; an element
    /// or a multi-line hole takes lines of its own.
    inline: bool,
    /// A blank line stood before this child in the source.
    blank_before: bool,
}

impl Piece {
    fn flat(&self) -> Option<&str> {
        if self.lines.len() == 1 {
            Some(&self.lines[0].1)
        } else {
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn print_tag(
    src: &str,
    name: &str,
    attributes: &[Attribute],
    children: &[Child],
    start: usize,
    end: usize,
    options: &FmtConfig,
    level: usize,
) -> Vec<Line> {
    let alx = &options.alx;
    let width = options
        .column_width
        .saturating_sub(level * indent_width(options));
    let attrs: Vec<Vec<Line>> = attributes
        .iter()
        .map(|a| print_attribute(a, options))
        .collect();
    let attrs_flat: Option<Vec<&str>> = attrs
        .iter()
        .map(|a| {
            if a.len() == 1 {
                Some(a[0].1.as_str())
            } else {
                None
            }
        })
        .collect();
    let kids = print_children(src, children, options, start, end);
    let self_closing =
        children.is_empty() && !src[start..end].trim_end().ends_with(&format!("</{name}>"));
    let close_text = if self_closing {
        if alx.self_closing_space { " />" } else { "/>" }
    } else {
        ">"
    };

    // The open tag on one line: `<Name a={1} b="2">`.
    let open_flat = attrs_flat.as_ref().map(|list| {
        let mut s = format!("<{name}");

        for a in list {
            s.push(' ');
            s.push_str(a);
        }

        s.push_str(close_text);
        s
    });

    // Everything on one line.
    if let Some(open) = &open_flat
        && kids.iter().all(|k| k.inline && k.flat().is_some())
    {
        let mut s = open.clone();

        for k in &kids {
            s.push_str(k.flat().unwrap_or_default());
        }

        if !self_closing {
            s.push_str(&format!("</{name}>"));
        }

        if s.chars().count() <= width || (attrs.is_empty() && kids.is_empty()) {
            return vec![(level, s)];
        }
    }

    let mut out: Vec<Line> = Vec::new();

    // The open tag.
    if let Some(open) = &open_flat
        && open.chars().count() <= width
    {
        out.push((level, open.clone()));
    } else {
        out.push((level, format!("<{name}")));
        let fill = !alx.attribute_per_line && attrs_flat.is_some();

        if fill {
            // As many attributes per line as fit.
            let inner = width.saturating_sub(indent_width(options));
            let mut line = String::new();

            for a in attrs_flat.as_ref().unwrap() {
                if !line.is_empty() && line.chars().count() + 1 + a.chars().count() > inner {
                    out.push((level + 1, std::mem::take(&mut line)));
                }

                if !line.is_empty() {
                    line.push(' ');
                }

                line.push_str(a);
            }

            if !line.is_empty() {
                out.push((level + 1, line));
            }
        } else {
            for a in &attrs {
                for (k, (l, text)) in a.iter().enumerate() {
                    out.push((level + 1 + if k == 0 { 0 } else { *l }, text.clone()));
                }
            }
        }

        if alx.bracket_same_line
            && let Some(last) = out.last_mut()
        {
            last.1.push_str(close_text);
        } else {
            out.push((level, close_text.trim_start().to_string()));
        }
    }

    if self_closing {
        return out;
    }

    // The children: inline runs flow, blocks stand alone.
    let mut run: Vec<&Piece> = Vec::new();
    let inner_width = width.saturating_sub(indent_width(options));
    let flush_run = |run: &mut Vec<&Piece>, out: &mut Vec<Line>| {
        if run.is_empty() {
            return;
        }

        let text: String = run.iter().map(|p| p.flat().unwrap_or_default()).collect();
        let text = text.trim().to_string();

        if alx.text_wrap == TextWrap::Fill && text.chars().count() > inner_width {
            for l in wrap_text(&text, inner_width) {
                out.push((level + 1, l));
            }
        } else if !text.is_empty() {
            out.push((level + 1, text));
        }

        run.clear();
    };

    for k in &kids {
        if k.blank_before && alx.blank_lines {
            flush_run(&mut run, &mut out);
            out.push((0, String::new()));
        }

        if k.inline && k.flat().is_some() {
            run.push(k);

            continue;
        }

        flush_run(&mut run, &mut out);

        for (l, text) in &k.lines {
            out.push((level + 1 + l, text.clone()));
        }
    }

    flush_run(&mut run, &mut out);
    out.push((level, format!("</{name}>")));
    out
}

/// Breaks a text run at spaces outside `{ }` holes.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut depth = 0usize;

    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }

                continue;
            }
            _ => {}
        }

        word.push(c);
    }

    if !word.is_empty() {
        words.push(word);
    }

    let mut lines = Vec::new();
    let mut line = String::new();

    for w in words {
        if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }

        if !line.is_empty() {
            line.push(' ');
        }

        line.push_str(&w);
    }

    if !line.is_empty() {
        lines.push(line);
    }

    lines
}

fn print_attribute(attr: &Attribute, options: &FmtConfig) -> Vec<Line> {
    match attr {
        Attribute::Named { name, value, .. } => match value {
            AttributeValue::Boolean => vec![(0, name.clone())],

            AttributeValue::StringLiteral(raw) => {
                let style = match options.alx.attribute_quotes {
                    AttributeQuotes::Double => QuoteStyle::ForceDouble,
                    AttributeQuotes::Single => QuoteStyle::ForceSingle,
                    AttributeQuotes::Preserve => QuoteStyle::Preserve,
                };

                vec![(0, format!("{name}={}", requote(raw, style)))]
            }

            AttributeValue::Expression(expr) => {
                let mut lines = hole(expr, options);
                lines[0].1 = format!("{name}={}", lines[0].1);
                lines
            }
        },

        Attribute::Spread { expression, .. } => hole(expression, options),

        Attribute::Inferred { expression, .. } => {
            let mut lines = hole(expression, options);
            lines[0].1 = format!("={}", lines[0].1);
            lines
        }
    }
}

/// `{ expr }`, with the expression formatted as Alloy code. Its lines
/// keep the code formatter's own indentation.
fn hole(expr: &str, options: &FmtConfig) -> Vec<Line> {
    let body = match format_alx(expr.trim(), options) {
        Ok(text) => text.trim_end().to_string(),
        Err(_) => expr.trim().to_string(),
    };
    let mut lines: Vec<Line> = body.lines().map(|l| (0, l.to_string())).collect();

    if lines.is_empty() {
        lines.push((0, String::new()));
    }

    lines[0].1.insert(0, '{');
    lines.last_mut().unwrap().1.push('}');
    lines
}

fn print_children(
    src: &str,
    children: &[Child],
    options: &FmtConfig,
    start: usize,
    end: usize,
) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut prev_end = start;
    let _ = end;

    for child in children {
        let span = child.span();
        let between = &src[prev_end.min(span.start)..span.start];
        let blank_before = between.matches('\n').count() >= 2 && !out.is_empty();

        let piece = match child {
            Child::Node(node) => Piece {
                lines: print_node(src, node, options, 0),
                inline: false,
                blank_before,
            },

            Child::Expression { expression, .. } => {
                let lines = hole(expression, options);
                let inline = lines.len() == 1;

                Piece {
                    lines,
                    inline,
                    blank_before,
                }
            }

            Child::Text { span, .. } => {
                let raw = &src[span.start..span.end];
                let Some(text) = flow_text(raw) else {
                    prev_end = span.end;

                    continue;
                };

                Piece {
                    lines: vec![(0, text)],
                    inline: true,
                    blank_before,
                }
            }

            Child::Comment { span, .. } => {
                let raw = src[span.start..span.end].trim();
                let lines: Vec<Line> = raw.lines().map(|l| (0, l.trim().to_string())).collect();

                Piece {
                    lines,
                    inline: false,
                    blank_before,
                }
            }
        };

        prev_end = span.end;
        out.push(piece);
    }

    out
}

/// Text with its whitespace folded: a run of spaces is one space, and
/// the whitespace that only lays the source out, the kind that holds a
/// newline, goes. Whitespace alone between two holes stays one space.
fn flow_text(raw: &str) -> Option<String> {
    let lead = raw.len() - raw.trim_start().len();
    let trail = raw.len() - raw.trim_end().len();
    let body = raw.trim();

    if body.is_empty() {
        return if raw.contains('\n') {
            None
        } else {
            Some(" ".to_string())
        };
    }

    let mut s = String::new();

    if lead > 0 && !raw[..lead].contains('\n') {
        s.push(' ');
    }

    let mut last_space = false;

    for c in body.chars() {
        if c.is_whitespace() {
            if !last_space {
                s.push(' ');
            }

            last_space = true;
        } else {
            s.push(c);
            last_space = false;
        }
    }

    if trail > 0 && !raw[raw.len() - trail..].contains('\n') {
        s.push(' ');
    }

    Some(s)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format_alx(s, &FmtConfig::default()).unwrap()
    }

    #[test]
    fn a_short_element_stays_on_one_line() {
        assert_eq!(
            fmt("local x = <Frame Size={a} />\n"),
            "local x = <Frame Size={a} />\n"
        );
        assert_eq!(
            fmt("local x = <TextLabel>{title}</TextLabel>\n"),
            "local x = <TextLabel>{title}</TextLabel>\n"
        );
    }

    #[test]
    fn children_go_on_their_own_lines() {
        let src = "return (\n    <Frame>\n        <UICorner />\n        <TextLabel>{a}</TextLabel>\n    </Frame>\n)\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn text_and_holes_flow_together() {
        let src = "return <TextLabel>Showing {#items} items</TextLabel>\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn a_multi_line_hole_keeps_its_lines() {
        let src = "return (\n    <TextButton\n        Activated={function()\n            go()\n        end}\n    >\n        {name} x{count}\n    </TextButton>\n)\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn attribute_quotes_follow_the_option() {
        assert_eq!(
            fmt("local x = <Frame Name='a' />\n"),
            "local x = <Frame Name=\"a\" />\n"
        );
    }

    #[test]
    fn self_closing_space_follows_the_option() {
        let mut o = FmtConfig::default();
        o.alx.self_closing_space = false;
        assert_eq!(
            format_alx("local x = <Frame/>\n", &o).unwrap(),
            "local x = <Frame/>\n"
        );
    }

    #[test]
    fn a_wide_tag_breaks_its_attributes() {
        let src = "local x = <Frame Size={UDim2.fromScale(1, 1)} BackgroundTransparency={1} Position={UDim2.fromScale(0.5, 0.5)} AnchorPoint={Vector2.new(0.5, 0.5)} />\n";
        let want = "local x = <Frame\n    Size={UDim2.fromScale(1, 1)} BackgroundTransparency={1} Position={UDim2.fromScale(0.5, 0.5)}\n    AnchorPoint={Vector2.new(0.5, 0.5)}\n/>\n";
        assert_eq!(fmt(src), want);
        let mut o = FmtConfig::default();
        o.alx.attribute_per_line = true;
        o.alx.bracket_same_line = true;
        let want = "local x = <Frame\n    Size={UDim2.fromScale(1, 1)}\n    BackgroundTransparency={1}\n    Position={UDim2.fromScale(0.5, 0.5)}\n    AnchorPoint={Vector2.new(0.5, 0.5)} />\n";
        assert_eq!(format_alx(src, &o).unwrap(), want);
    }

    #[test]
    fn formatting_is_idempotent_on_the_alx_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");

        if !dir.is_dir() {
            return;
        }

        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "alx") {
                let src = std::fs::read_to_string(&path).unwrap();
                let once = fmt(&src);
                let twice = fmt(&once);
                assert_eq!(once, twice, "{}", path.display());
            }
        }
    }
}
