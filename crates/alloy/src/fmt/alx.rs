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
use crate::fmt::{format_file, format_with, requote};

/// One printed line of markup: an indentation level, relative to the
/// line the markup starts on, and the text.
type Line = (usize, String);

const PLACEHOLDER: &str = "__ALX";

/// Formats `.alx` source. `Err` carries the first lexer or markup error.
/// The markup of an expression hole comes through here too, so the code
/// around it is a fragment, not a whole file.
pub fn format_alx(src: &str, options: &FmtConfig) -> Result<String, String> {
    format_alx_inner(src, options, false)
}

/// Formats a whole `.alx` file: one the parser cannot read keeps its
/// text, and `Err` starts with `fmt::UNPARSED`.
pub fn format_alx_file(src: &str, options: &FmtConfig) -> Result<String, String> {
    format_alx_inner(src, options, true)
}

/// A markup error as `alloy fmt` reports it: the position, then the
/// reason, in the shape `fmt` uses for a file it skipped.
fn unparsed(src: &str, offset: usize, message: &str) -> String {
    let at = offset.min(src.len());
    let before = &src[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, str::len) + 1;

    format!("{}: {line}:{col}: {message}", crate::fmt::UNPARSED)
}

fn format_alx_inner(src: &str, options: &FmtConfig, whole: bool) -> Result<String, String> {
    // The code formats with each markup span held out, so a write inside
    // a handler the markup holds is out of sight. `prefer_const` would
    // read that local as never written; `flux --fix` reads the lowered
    // file and writes `const` where it holds.
    let options = &FmtConfig {
        prefer_const: false,
        ..options.clone()
    };
    let code_fmt = if whole { format_file } else { format_with };
    // Markup the parser cannot read is the same case as Alloy code it
    // cannot read: the file keeps its text and the run says why, with
    // the position the same error carries under `alloy check`.
    // A `<style>` element holds CSS, which an ingot reads, not markup:
    // `{` there opens a rule and `--x` names a property. The spans are
    // found with its text blanked, and a span that holds one keeps its
    // text as written.
    let masked = blank_styles(src);
    let spans =
        luaux::compile::markup_spans(&masked).map_err(|e| unparsed(src, e.offset, &e.message))?;

    if spans.is_empty() {
        return code_fmt(src, options);
    }

    let mut code = String::with_capacity(src.len());
    let mut printed: Vec<Vec<Line>> = Vec::new();
    let mut last = 0;

    for (n, (a, b)) in spans.iter().enumerate() {
        code.push_str(&src[last..*a]);

        if masked[*a..*b] != src[*a..*b] {
            let lines = as_written(src, *a, *b);
            let width = match lines.len() {
                1 => lines[0].1.chars().count(),

                _ => options.column_width + 1,
            };
            code.push_str(&placeholder(n, width));
            printed.push(lines);
            last = *b;

            continue;
        }

        let (node, _) =
            luaux::markup::parse_node(src, *a).map_err(|e| unparsed(src, e.offset, &e.message))?;
        let lines = print_node(src, &node, options, 0);
        let width = if lines.len() == 1 && !parenthesized_block(src, *a, *b) {
            lines[0].1.chars().count()
        } else {
            options.column_width + 1
        };
        code.push_str(&placeholder(n, width));
        printed.push(lines);
        last = *b;
    }

    code.push_str(&src[last..]);
    let formatted = code_fmt(&code, options)?;
    Ok(substitute(&formatted, &printed, options))
}

/// The source with the text of each `<style>` element blanked to
/// spaces, byte for byte, so every offset holds.
fn blank_styles(src: &str) -> String {
    let mut out = src.to_string();
    let mut from = 0;

    while let Some(open) = src[from..].find("<style") {
        let open = from + open;
        let Some(body) = src[open..].find('>').map(|i| open + i + 1) else {
            break;
        };
        let Some(close) = src[body..].find("</style>").map(|i| body + i) else {
            break;
        };
        let blank: String = src[body..close]
            .chars()
            .map(|c| match c {
                '\n' => "\n".to_string(),

                c => " ".repeat(c.len_utf8()),
            })
            .collect();
        out.replace_range(body..close, &blank);
        from = close;
    }

    out
}

/// A span as the source wrote it, one line per source line. A later
/// line keeps its indent over the span's first line, which the printer
/// sets again from where the span now stands.
fn as_written(src: &str, start: usize, end: usize) -> Vec<Line> {
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let base: String = src[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    src[start..end]
        .split('\n')
        .enumerate()
        .map(|(k, text)| match k {
            0 => (0, text.to_string()),

            _ => (
                0,
                text.strip_prefix(base.as_str())
                    .unwrap_or(text)
                    .trim_end()
                    .to_string(),
            ),
        })
        .collect()
}

/// Whether the source put the markup in parentheses of its own, one on
/// the line above and one on the line below. `fmt` keeps that shape:
/// it is the form the examples write, and a tag that fits on one line
/// would otherwise lose it.
fn parenthesized_block(src: &str, start: usize, end: usize) -> bool {
    const BLANK: [char; 3] = [' ', '\t', '\r'];

    let Some(before) = src[..start].trim_end_matches(BLANK).strip_suffix('\n') else {
        return false;
    };

    if !before.trim_end_matches(BLANK).ends_with('(') {
        return false;
    }

    src.get(end..)
        .and_then(|after| after.trim_start_matches(BLANK).strip_prefix('\n'))
        .is_some_and(|after| after.trim_start_matches(BLANK).starts_with(')'))
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

/// Whether the source keeps an attribute on the same line as `<Name`.
/// The close of such a tag follows the last attribute, on the line that
/// attribute ends on.
fn attribute_beside_name(src: &str, start: usize, name: &str) -> bool {
    let after = (start + 1 + name.len()).min(src.len());
    let head = src[after..].split('\n').next().unwrap_or("");

    !head.trim().is_empty()
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

    // Everything on one line. A tag the source wrote on one line keeps
    // its elements beside each other while it fits: a line break between
    // two elements is a space in rendered text, so breaking them would
    // change what the player reads.
    let one_line = !src[start..end].contains('\n');

    if let Some(open) = &open_flat
        && kids
            .iter()
            .all(|k| (k.inline || one_line) && k.flat().is_some())
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
    } else if open_flat.is_none() && attribute_beside_name(src, start, name) {
        // An attribute that prints over several lines, a function body
        // for instance, is not a wide tag. The source decides: an
        // attribute the author wrote beside `<Name` stays there, so the
        // tag keeps the line count the span map was built on.
        out.push((level, format!("<{name}")));

        for a in &attrs {
            let head = out.last_mut().expect("the tag name opened the list");

            head.1.push(' ');
            head.1.push_str(&a[0].1);
            out.extend(a[1..].iter().map(|(l, text)| (level + l, text.clone())));
        }

        out.last_mut()
            .expect("the tag name opened the list")
            .1
            .push_str(close_text);
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

    // The children: inline runs flow, blocks stand alone. A run holds
    // each piece's text, and whether it is text or a hole, which may wrap.
    let mut run: Vec<(&str, bool)> = Vec::new();
    let inner_width = width.saturating_sub(indent_width(options));
    let flush_run = |run: &mut Vec<(&str, bool)>, out: &mut Vec<Line>| {
        if run.is_empty() {
            return;
        }

        let text: String = run.iter().map(|(t, _)| *t).collect();
        let text = keep_edges(&text, options);
        // A break inside a joined element would split its tag.
        let joined = run.iter().any(|(_, inline)| !inline);

        if alx.text_wrap == TextWrap::Fill && !joined && text.chars().count() > inner_width {
            for l in wrap_text(&text, inner_width) {
                out.push((level + 1, l));
            }
        } else if !text.is_empty() {
            out.push((level + 1, text));
        }

        run.clear();
    };

    // A space on one line between text and a child element is text,
    // and a line break there drops it. So the element joins the text
    // beside it: its first line ends the text's line, and its last
    // line starts the next one.
    let spaced = |text: Option<&str>, edge: fn(&str) -> Option<char>| {
        text.and_then(edge).is_some_and(|c| c == ' ' || c == '\t')
    };

    for (i, k) in kids.iter().enumerate() {
        if k.blank_before && alx.blank_lines {
            flush_run(&mut run, &mut out);
            out.push((0, String::new()));
        }

        if k.inline && k.flat().is_some() {
            run.push((&k.lines[0].1, true));

            continue;
        }

        let before = spaced(run.last().map(|(t, _)| *t), |t| t.chars().last());
        let after = spaced(
            kids.get(i + 1).filter(|n| n.inline).and_then(Piece::flat),
            |t| t.chars().next(),
        );

        match k.lines.as_slice() {
            [] => {}

            [(_, only)] if before || after => run.push((only, false)),

            [(_, only)] => {
                flush_run(&mut run, &mut out);
                out.push((level + 1, only.clone()));
            }

            [first, middle @ .., last] => {
                if before {
                    run.push((&first.1, false));
                    flush_run(&mut run, &mut out);
                } else {
                    flush_run(&mut run, &mut out);
                    out.push((level + 1 + first.0, first.1.clone()));
                }

                out.extend(middle.iter().map(|(l, t)| (level + 1 + l, t.clone())));

                match after {
                    true => run.push((&last.1, false)),

                    false => out.push((level + 1 + last.0, last.1.clone())),
                }
            }
        }
    }

    flush_run(&mut run, &mut out);
    out.push((level, format!("</{name}>")));
    out
}

/// A run of text that stands on lines of its own. The compiler drops
/// the whitespace at the edge of a line, and whitespace at the edge of
/// the run is text the author wrote on one line. It goes in a `{" "}`
/// hole, where no line break reaches it.
fn keep_edges(text: &str, options: &FmtConfig) -> String {
    // The hole formats as any other, so a second run keeps it.
    let hole = |ws: &str| match ws.is_empty() {
        true => String::new(),

        false => hole(&format!("\"{ws}\""), options).remove(0).1,
    };
    let body = text.trim();

    if body.is_empty() {
        return hole(text);
    }

    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];

    format!("{}{body}{}", hole(lead), hole(trail))
}

/// Breaks a text run at single spaces outside `{ }` holes. A newline
/// reads as one space, so a run of spaces is text and never breaks.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut depth = 0usize;
    let chars: Vec<char> = text.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        let single = i > 0 && chars[i - 1] != ' ' && chars.get(i + 1).is_some_and(|n| *n != ' ');

        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 && single => {
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

/// Text as the compiler reads it. A whitespace run that holds a newline
/// lays the source out: the compiler reads it as one space inside the
/// text and drops it at either end. A run on one line is text, so it
/// stays as written. `None` when nothing is left.
fn flow_text(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut run = String::new();

    for c in raw.chars() {
        if c.is_whitespace() {
            run.push(c);

            continue;
        }

        if !run.contains('\n') {
            out.push_str(&run);
        } else if !out.is_empty() {
            out.push(' ');
        }

        run.clear();
        out.push(c);
    }

    if !run.contains('\n') {
        out.push_str(&run);
    }

    (!out.is_empty()).then_some(out)
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
        let src = "return (\n  <Frame>\n    <UICorner />\n    <TextLabel>{a}</TextLabel>\n  </Frame>\n)\n";
        assert_eq!(fmt(src), src);
    }

    /// A tag the author wrapped in parentheses of its own keeps that
    /// shape, even when it would fit on one line.
    #[test]
    fn a_parenthesized_return_keeps_its_lines() {
        let src = "local function Badge(props: { title: string })\n  return (\n    <TextLabel Text={props.title} />\n  )\nend\n";
        assert_eq!(fmt(src), src);
        let one = "local function Badge(props: { title: string })\n  return (<TextLabel Text={props.title} />)\nend\n";
        assert_eq!(fmt(one), one);
    }

    #[test]
    fn text_and_holes_flow_together() {
        let src = "return <TextLabel>Showing {#items} items</TextLabel>\n";
        assert_eq!(fmt(src), src);
    }

    /// The Text a tag renders, as the compiler reads it: the text, the
    /// string holes, and a marker for any other hole.
    fn rendered(src: &str) -> String {
        let at = src.find('<').expect("a tag");
        let (node, _) = luaux::markup::parse_node(src, at).expect("the markup parses");
        let Node::Element(e) = node else {
            panic!("an element")
        };

        e.children
            .iter()
            .map(|c| match c {
                Child::Text { text, .. } => text.clone(),

                Child::Expression { expression, .. } => {
                    match expression.strip_prefix(['"', '\'']) {
                        Some(s) => s.trim_end_matches(['"', '\'']).to_string(),

                        // Code in a hole may move; only its words count.
                        None => format!(
                            "{{{}}}",
                            expression.split_whitespace().collect::<Vec<_>>().join(" ")
                        ),
                    }
                }

                _ => String::new(),
            })
            .collect()
    }

    /// `fmt` keeps the program the same, and the text a tag renders is
    /// part of it. It folded runs of spaces and dropped a space beside
    /// a child element it moved to a line of its own.
    #[test]
    fn the_rendered_text_holds() {
        let mut preserve = FmtConfig::default();
        preserve.alx.text_wrap = TextWrap::Preserve;

        for src in [
            "return <TextLabel>Hello <UIPadding PaddingLeft={UDim.new(0, 4)} PaddingRight={UDim.new(0, 4)} /> there {p.x}</TextLabel>\n",
            "return <TextLabel>Price: {p.x}<UIPadding PaddingLeft={UDim.new(0, 4)} PaddingRight={UDim.new(0, 4)} /> coins</TextLabel>\n",
            "return <TextLabel>two  spaces   here</TextLabel>\n",
            "return <TextLabel>{a}   {b}</TextLabel>\n",
            "return <TextLabel>  leading and trailing  </TextLabel>\n",
            "return <TextLabel Size={UDim2.fromScale(1, 1)} BackgroundTransparency={1}>  leading and a long line of text that wraps  </TextLabel>\n",
            "return <TextLabel>Hello <UIPadding PaddingLeft={UDim.new(0, 4)} PaddingRight={UDim.new(0, 4)} PaddingTop={UDim.new(0, 4)} PaddingBottom={UDim.new(0, 4)} /> there</TextLabel>\n",
            "return <TextLabel>Total: {f(function()\n  return 1\nend)} items</TextLabel>\n",
        ] {
            for options in [&FmtConfig::default(), &preserve] {
                let once = format_alx(src, options).unwrap();

                assert_eq!(rendered(&once), rendered(src), "{once}");
                // A hole keeps a space only at the edge of the tag; a
                // hole makes the text a computed value in some backends.
                assert!(
                    src.contains("  leading") || !once.contains("{' '}"),
                    "{once}"
                );
                assert_eq!(
                    format_alx(&once, options).unwrap(),
                    once,
                    "fmt is idempotent"
                );
            }
        }
    }

    #[test]
    fn a_multi_line_hole_keeps_its_lines() {
        let src = "return (\n  <TextButton\n    Activated={function()\n      go()\n    end}\n  >\n    {name} x{count}\n  </TextButton>\n)\n";
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
        let want = "local x = <Frame\n  Size={UDim2.fromScale(1, 1)}\n  BackgroundTransparency={1}\n  Position={UDim2.fromScale(0.5, 0.5)}\n  AnchorPoint={Vector2.new(0.5, 0.5)}\n/>\n";
        assert_eq!(fmt(src), want);
        let mut o = FmtConfig::default();
        o.alx.attribute_per_line = false;
        let want = "local x = <Frame\n  Size={UDim2.fromScale(1, 1)} BackgroundTransparency={1} Position={UDim2.fromScale(0.5, 0.5)}\n  AnchorPoint={Vector2.new(0.5, 0.5)}\n/>\n";
        assert_eq!(format_alx(src, &o).unwrap(), want);
        let mut o = FmtConfig::default();
        o.alx.bracket_same_line = true;
        let want = "local x = <Frame\n  Size={UDim2.fromScale(1, 1)}\n  BackgroundTransparency={1}\n  Position={UDim2.fromScale(0.5, 0.5)}\n  AnchorPoint={Vector2.new(0.5, 0.5)} />\n";
        assert_eq!(format_alx(src, &o).unwrap(), want);
    }

    /// A multi-line attribute is not a wide tag: the tag the author
    /// opened on one line stayed on one line, and the line count with
    /// it. The span map reads the line count, so it never moves.
    #[test]
    fn a_multi_line_attribute_keeps_the_tag_on_its_line() {
        let src = "return (\n  <TextButton Activated={function()\n    go()\n  end}>\n    {name}\n  </TextButton>\n)\n";
        assert_eq!(fmt(src), src);
        assert_eq!(fmt(src).lines().count(), src.lines().count());
        let closed = "local x = <Frame Size={function()\n  go()\nend} />\n";
        assert_eq!(fmt(closed).lines().count(), closed.lines().count());
    }

    #[test]
    fn formatting_is_idempotent_on_the_alx_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");

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
