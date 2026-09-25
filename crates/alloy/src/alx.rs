//! `.alx`: markup lowered by luaux, then the Alloy desugar.
//!
//! luaux runs first and is text-local: it replaces each markup region with
//! calls and passes every other byte through, so Alloy syntax outside and
//! inside `{ }` holes reaches the desugar unchanged. Every element lands
//! on its source line, so the line count holds across both passes.

use std::collections::HashSet;

use alloy_syntax::lexer::TokKind;

use crate::render::{Edit, SpanMap, apply_edits};
use crate::{CompileError, Diagnostic, EmitOptions, Output};

/// The lint name of luaux's one warning, as `[lint.rules]` writes it.
pub const STATIC_CONDITIONAL_CHILD: &str = "alx.static_conditional_child";

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
    config: luaux::Config,
) -> Result<AlxOutput, CompileError> {
    let spans = luaux::compile::markup_spans(src).map_err(|e| CompileError {
        offset: e.offset,
        message: markup_message(&e.message, None),
    })?;
    let blanked = luaux::resolve::blank_luaux_regions(src, &spans);
    let bound = bound_names(&blanked);
    // A reactive library takes a source where a property wants a value,
    // so only a plain lowering types a Roblox tag's attributes.
    let plain = config.interpolate == luaux::config::Interpolate::Plain;
    let (problems, typed) = component_props_problems(src, &bound, plain);

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

    let lowering = lowering_map(src, &compiled);
    let preamble = compiled.preamble;
    let lowered = compiled.output;
    // A function that returns markup, or that a tag names, is a
    // component, which `[lint.naming] component` styles.
    let mut options = options.clone();
    options.markup = crate::naming::Markup::of(src, &compiled.regions);
    // A lone `{expr}` that the markup gives as `Text`, as bytes of the
    // lowered text. A hole that held markup of its own has no copied
    // bytes to map, and goes unchecked.
    options.text_holes = compiled
        .text_holes
        .iter()
        .filter_map(|&(open, close)| {
            let inner = src.get(open + 1..close.checked_sub(1)?)?;
            let start = open + 1 + inner.len() - inner.trim_start().len();
            let end = open + 1 + inner.trim_end().len();

            (start < end).then_some(())?;

            Some((
                lowering.to_output(start as u32)?,
                lowering.to_output(end as u32 - 1)? + 1,
            ))
        })
        .collect();
    // An attribute value the walk typed, as bytes of the lowered text.
    options.attribute_types = typed
        .into_iter()
        .filter_map(|(start, end, ty)| {
            Some((
                lowering.to_output(start as u32)?,
                lowering.to_output(end as u32 - 1)? + 1,
                ty,
            ))
        })
        .collect();

    let mut output = crate::compile_with(&lowered, &options)?;
    let back = |offset: u32| lowering.to_source(offset);

    // The lowering prepends its helpers in front of the file. They are
    // the emit's text, not the author's, so a lint inside them has
    // nothing the reader can act on, and its rewrite would land on the
    // first line of the source.
    if let Some((at, len)) = preamble {
        let end = (at + len) as u32;
        output
            .lints
            .retain(|l| l.start >= end || l.start < at as u32);
    }

    for d in &mut output.diagnostics {
        d.start = back(d.start);
        d.end = back(d.end);
    }

    // A lint message may quote the lowered text. Where it quotes a
    // markup region, the reader sees the markup they wrote instead.
    for l in &mut output.lints {
        for r in &compiled.regions {
            let written = &lowered[r.out_start..r.out_end];

            if l.message.contains(written) {
                let own = src[r.src_start..r.src_end]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                l.message = l.message.replace(written, &own);
            }
        }
    }

    // A lint inside markup reads the calls the markup lowered to. A tag
    // hands its component the props table, so a component with no
    // parameters is no wrong call. A lint that still quotes code the
    // author never wrote, `create(Menu, {})`, describes the lowering,
    // and its rewrite would write that code into the file; the markup's
    // own lints cover the shape.
    let in_markup = |at: u32| {
        compiled
            .regions
            .iter()
            .any(|r| r.out_start <= at as usize && (at as usize) < r.out_end)
    };
    let written: String = src.split_whitespace().collect::<Vec<_>>().join(" ");

    output.lints.retain(|l| {
        if !in_markup(l.start) {
            return true;
        }

        l.name != "argument_count"
            && l.message.split('`').skip(1).step_by(2).all(|quoted| {
                written.contains(&quoted.split_whitespace().collect::<Vec<_>>().join(" "))
            })
    });

    // A rewrite carries a range of its own, and the lowering moves
    // every byte after the first tag. Without this the rewrite lands at
    // the wrong offset and writes over the author's code.
    crate::lint::to_source(&mut output.lints, src, &lowering);

    // A markup error can drop the code it covers from the lowering, as
    // `Text={pad(x)}` beside text between the tags. A name that only this
    // code reads is not unused: the error is the fix, not the import.
    let dropped: Vec<&str> = compiled
        .errors
        .iter()
        .filter_map(|e| src.get(e.offset..e.offset + e.length))
        .collect();

    output.lints.retain(|l| {
        !matches!(
            l.name,
            "unused_import" | "unused_variable" | "unused_function"
        ) || !l.message.split('`').nth(1).is_some_and(|name| {
            dropped
                .iter()
                .any(|code| whole_word_from(code, name, 0).is_some())
        })
    });

    for e in compiled.errors {
        output.diagnostics.push(Diagnostic {
            start: e.offset as u32,
            end: (e.offset + e.length) as u32,
            message: markup_message(&e.message, e.help.as_deref()),
        });
    }

    output.diagnostics.extend(problems);

    for d in struct_props_problems(&blanked, &spans, &options) {
        output.diagnostics.push(d);
    }

    // luaux warns only for a warn-level markup lint; `deny` comes back as
    // an error above. A diagnostic is an error and fails the build, so the
    // warning goes out as a lint, which the `[lint]` table levels.
    for w in compiled.warnings {
        output.lints.push(crate::lint::Lint {
            name: STATIC_CONDITIONAL_CHILD,
            start: w.offset as u32,
            end: (w.offset + w.length) as u32,
            message: markup_message(&w.message, w.help.as_deref())
                .trim_start_matches("markup: ")
                .to_string(),
            fix: None,
        });
    }

    output.lints.sort_by_key(|l| (l.start, l.name));
    output.diagnostics.sort_by_key(|d| d.start);

    // With the lowering in front of it the map speaks the author's own
    // text, the way it does for a `.aly`; every reader crosses one map.
    output.map = lowering.compose(&output.map);
    output.lowered = Some(lowered.clone());

    Ok(AlxOutput { output, lowered })
}

/// A component whose props parameter names a struct. A tag's attributes
/// lower to a plain table and a struct is nominal, so no tag satisfies
/// the parameter, whatever it writes. The report sits on the declaration,
/// which is the place to change.
///
/// `blanked` is the source with the markup regions blanked, so it parses
/// as Alloy and every offset is still the author's own.
fn struct_props_problems(
    blanked: &str,
    spans: &[(usize, usize)],
    options: &EmitOptions,
) -> Vec<Diagnostic> {
    use alloy_syntax::ast::Stmt;

    let Ok(parsed) = alloy_syntax::parse_lenient(blanked, Default::default()) else {
        return Vec::new();
    };
    let toks = &parsed.lexed.toks;
    let text = |span: alloy_syntax::ast::TokSpan| span.text_or_empty(blanked, toks);
    let bytes = |span: alloy_syntax::ast::TokSpan| {
        let last = (span.end as usize)
            .saturating_sub(1)
            .max(span.start as usize);

        match (toks.get(span.start as usize), toks.get(last)) {
            (Some(first), Some(last)) => Some((first.start as usize, last.end as usize)),

            _ => None,
        }
    };
    // A struct this file declares, or one a module it imports declares.
    let declares_struct = |name: &str| {
        crate::declarations::shapes(blanked)
            .iter()
            .any(|s| matches!(s, crate::declarations::Shape::Struct { .. }) && s.name() == name)
            || options.import_struct_fields.iter().any(|(n, _)| n == name)
    };
    let mut out = Vec::new();

    for stmt in &parsed.chunk.block.stmts {
        let (name, body, span) = match stmt {
            Stmt::Function(f) if f.path.len() == 1 => (f.path[0], &f.body, f.span),

            Stmt::LocalFunction(f) => (f.name, &f.body, f.span),

            _ => continue,
        };

        // A component is a function a tag names, so its name starts with
        // a capital, and its body builds markup.
        if !text(name).starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }

        let Some(ty) = body.params.first().and_then(|p| p.ty) else {
            continue;
        };
        let props = text(ty).trim().trim_start_matches(':').trim().to_string();

        if !declares_struct(&props) {
            continue;
        }

        let (Some((from, to)), Some((at, end))) = (bytes(span), bytes(ty)) else {
            continue;
        };

        if !spans.iter().any(|(start, _)| (from..to).contains(start)) {
            continue;
        }

        out.push(Diagnostic {
            start: at as u32,
            end: end as u32,
            message: format!(
                "markup: a component's props are a plain table; declare `{props}` as a `type` or an `interface`, not a `struct`"
            ),
        });
    }

    out
}

/// Whether a markup diagnostic is one of the attribute checks. The
/// lowering runs past these, so the emit is the same either way and a
/// checker report elsewhere on the line still stands.
pub fn is_attribute_check(message: &str) -> bool {
    message.starts_with("markup: prop ")
        || message.starts_with("markup: property ")
        || message.contains(" has no prop named ")
        || message.contains("` is an event of ")
        || (message.starts_with("markup: `<") && message.contains("` leaves "))
}

/// One prop a component declares.
struct Prop {
    name: String,
    ty: String,
    optional: bool,
}

/// The props a tag may set without its class or its component
/// declaring them. React reads `key` itself and it never reaches the
/// component; `ClassName` is the utility list a styling ingot reads and
/// rewrites into properties before the tag is built.
pub const FREE_PROPS: &[&str] = &["key", "ClassName"];

/// The source bytes of an attribute value and the Luau type it must
/// have, for the check artifact.
type Typed = (usize, usize, String);

/// The attributes of every component tag, against the props the
/// component declares: a prop it does not take, a required prop the tag
/// leaves out, and a literal of the wrong type. The second list holds
/// each other value a declared type covers, for the check artifact. With
/// `plain` false, a Roblox tag adds nothing to it.
fn component_props_problems(
    src: &str,
    bound: &HashSet<String>,
    plain: bool,
) -> (Vec<Diagnostic>, Vec<Typed>) {
    let Ok(spans) = luaux::compile::markup_spans(src) else {
        return (Vec::new(), Vec::new());
    };
    let mut out = Vec::new();
    let mut typed = Vec::new();

    for (start, _) in spans {
        let Ok((node, _)) = luaux::markup::parse_node(src, start) else {
            continue;
        };
        check_node(&node, src, bound, plain, &mut out, &mut typed);
    }

    out.sort_by_key(|d| d.start);
    out.dedup_by(|a, b| a.start == b.start && a.message == b.message);
    typed.sort();
    typed.dedup();

    (out, typed)
}

fn check_node(
    node: &luaux::markup::Node,
    src: &str,
    bound: &HashSet<String>,
    plain: bool,
    out: &mut Vec<Diagnostic>,
    typed: &mut Vec<Typed>,
) {
    use luaux::markup::{Child, Node};

    let children = match node {
        Node::Element(e) => {
            check_element(e, src, bound, plain, out, typed);
            &e.children
        }

        Node::Fragment(f) => &f.children,
    };

    for child in children {
        match child {
            Child::Node(n) => check_node(n, src, bound, plain, out, typed),

            // A tag inside a hole is one expression to the markup
            // parser, so its own region is parsed from the text.
            Child::Expression { expression, span } => {
                let mut at = span.start;

                while let Some(lt) = src.get(at..span.end).and_then(|t| t.find('<')) {
                    at += lt;

                    match luaux::markup::parse_node(src, at) {
                        Ok((inner, next)) => {
                            check_node(&inner, src, bound, plain, out, typed);
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
    plain: bool,
    out: &mut Vec<Diagnostic>,
    typed: &mut Vec<Typed>,
) {
    use luaux::markup::{Attribute, AttributeValue};

    let name = element.name.as_written();

    if luaux::roblox::is_class(&name) {
        check_intrinsic(element, &name, src, plain.then_some(typed), out);

        return;
    }

    if !bound.contains(&name) {
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

        let literal = value.and_then(literal_type);
        let want = prop.ty.trim().trim_end_matches('?');
        let primitive = matches!(want, "string" | "number" | "boolean");

        // A literal against a primitive is the text check's report. The
        // checker types every other value, `item={5}` against `Item` too.
        if !(literal.is_some() && primitive)
            && !matches!(value, Some(AttributeValue::Boolean))
            && let Some(ty) = prop_type(prop)
            && let Some(bytes) = value_bytes(src, span)
        {
            typed.push((bytes.0, bytes.1, ty));
        }

        let Some(got) = literal else {
            continue;
        };

        if primitive && want != got {
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

/// A Roblox tag's attributes against the class: a literal where the
/// property takes another type, and a literal on an event, which takes
/// a function. A property the class does not have is luaux's report.
/// Each other value of a typed property goes to `typed`, when given.
fn check_intrinsic(
    element: &luaux::markup::Element,
    class: &str,
    src: &str,
    mut typed: Option<&mut Vec<Typed>>,
    out: &mut Vec<Diagnostic>,
) {
    use luaux::markup::{Attribute, AttributeValue};

    for attribute in &element.attributes {
        let Attribute::Named { name, span, value } = attribute else {
            continue;
        };
        if FREE_PROPS.contains(&name.as_str()) {
            continue;
        }

        let Some(got) = literal_type(value) else {
            // A nil field of the props table leaves the property unset,
            // so the value may be nil too: `if on then red else nil`.
            if let (Some(typed), AttributeValue::Expression(_)) = (typed.as_mut(), value)
                && !luaux::roblox::is_event(class, name)
                && let Some(want) = crate::roblox_props::property_type(class, name)
                && let Some((start, end)) = value_bytes(src, *span)
            {
                typed.push((start, end, format!("{want}?")));
            }

            continue;
        };
        let report = |out: &mut Vec<Diagnostic>, message: String| {
            out.push(Diagnostic {
                start: span.start as u32,
                end: (span.start + name.len()) as u32,
                message,
            });
        };

        if luaux::roblox::is_event(class, name) {
            report(
                out,
                format!(
                    "markup: `{name}` is an event of {class}; it takes a function, not a {got}"
                ),
            );

            continue;
        }

        let Some(want) = crate::roblox_props::property_type(class, name) else {
            continue;
        };

        // A ContentId is a string in Luau: `Image="rbxassetid://1"`.
        if want != got && !(want == "ContentId" && got == "string") {
            report(
                out,
                format!(
                    "markup: property {name} of {class} is {}, not {got}",
                    readable_type(want)
                ),
            );
        }
    }
}

/// The source bytes of an attribute's value: the expression between its
/// braces, or its quoted text. `None` for a bare attribute.
fn value_bytes(src: &str, span: luaux::markup::Span) -> Option<(usize, usize)> {
    let text = src.get(span.start..span.end)?;
    let after = text.find('=')? + 1;
    let value = text[after..].trim();
    let mut at = span.start + after + (text[after..].len() - text[after..].trim_start().len());
    let value = match value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
        Some(inner) => {
            at += 1 + inner.len() - inner.trim_start().len();
            inner.trim()
        }

        None => value,
    };

    (!value.is_empty()).then_some((at, at + value.len()))
}

/// The type a prop's value must have, on one line, since the check
/// artifact writes it into the line of the tag. A comment in the type
/// would end that line, so such a type is left out.
fn prop_type(prop: &Prop) -> Option<String> {
    let ty = prop.ty.split_whitespace().collect::<Vec<_>>().join(" ");

    if ty.is_empty() || ty.contains("--") {
        return None;
    }

    match prop.optional && !ty.ends_with('?') {
        true => Some(format!("({ty})?")),

        false => Some(ty),
    }
}

/// A Roblox type as the source writes it: the dump spells an enum
/// `EnumFont`, and the reader writes `Enum.Font`.
fn readable_type(name: &str) -> String {
    match name.strip_prefix("Enum") {
        Some(rest) if rest.starts_with(char::is_uppercase) => format!("Enum.{rest}"),

        _ => name.to_string(),
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

    // One literal token, read by the lexer: `"a" == b` starts with a
    // quote and is a comparison, and `inf` parses as a number and is a
    // name.
    let toks = alloy_syntax::lexer::lex(text).ok()?.toks;

    if let [minus, number] = &toks[..]
        && minus.text(text) == "-"
    {
        return (number.kind == TokKind::Number).then_some("number");
    }

    match &toks[..] {
        [t] => match t.kind {
            TokKind::Str { .. } | TokKind::InterpStr => Some("string"),

            TokKind::Number => Some("number"),

            TokKind::Ident if matches!(t.text(text), "true" | "false") => Some("boolean"),

            _ => None,
        },

        // An interpolated string with holes, when its tail ends the text.
        [head, .., tail] if head.kind == TokKind::InterpHead => {
            let mut depth = 0i32;

            for t in &toks {
                match t.kind {
                    TokKind::InterpHead => depth += 1,

                    TokKind::InterpTail => depth -= 1,

                    _ => {}
                }

                if depth == 0 && !std::ptr::eq(t, tail) {
                    return None;
                }
            }

            Some("string")
        }

        _ => None,
    }
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

/// The map from the `.alx` source to the text luaux lowered it to.
///
/// The lowering is text-local: every byte outside a markup region is
/// copied, and inside one the code of each `{ }` hole is copied too.
/// So the map is the source with one edit per region, plus one for the
/// helper preamble. `None` when the pieces do not rebuild the lowered
/// text, which leaves the caller with the line-and-word mapping.
fn lowering_map(src: &str, compiled: &luaux::compile::Compiled) -> SpanMap {
    region_map(src, compiled)
        .or_else(|| line_map(src, &compiled.output))
        // Both readings need the lowering to keep the line count. It
        // does, by construction; the source itself stands in for a
        // build that broke the rule, and holds up to the first tag.
        .unwrap_or_else(|| apply_edits(src, &[]).1)
}

/// The map read from the regions luaux reports: one edit per region,
/// and the holes inside it left in place.
fn region_map(src: &str, compiled: &luaux::compile::Compiled) -> Option<SpanMap> {
    let mut edits = Vec::new();

    if let Some((at, len)) = compiled.preamble {
        // The preamble goes before the first statement, which stands
        // ahead of every region, so its offset is a source offset too.
        if compiled.regions.iter().any(|r| r.out_start < at + len) || at > src.len() {
            return None;
        }

        edits.push(Edit {
            start: at as u32,
            end: at as u32,
            text: compiled.output.get(at..at + len)?.to_string(),
        });
    }

    for region in &compiled.regions {
        let emitted = compiled.output.get(region.out_start..region.out_end)?;
        let coarse = || Edit {
            start: region.src_start as u32,
            end: region.src_end as u32,
            text: emitted.to_string(),
        };

        match region_edits(src, region, emitted) {
            Some(fine) => edits.extend(fine),

            None => edits.push(coarse()),
        }
    }

    let (text, map, errors) = apply_edits(src, &edits);

    (errors.is_empty() && text == compiled.output).then_some(map)
}

/// The map read line by line: on a line the lowering rewrote, the
/// common head and tail are the author's and the middle is the
/// lowering's. It answers where the regions cannot.
fn line_map(src: &str, lowered: &str) -> Option<SpanMap> {
    let mut edits = Vec::new();
    let mut lows = lowered.split('\n');
    let mut at = 0usize;

    for a in src.split('\n') {
        let b = lows.next()?;

        if a != b {
            let (head, tail) = common_ends(a, b);
            edits.push(Edit {
                start: (at + head) as u32,
                end: (at + a.len() - tail) as u32,
                text: b[head..b.len() - tail].to_string(),
            });
        }

        at += a.len() + 1;
    }

    let (text, map, errors) = apply_edits(src, &edits);

    (errors.is_empty() && text == lowered && lows.next().is_none()).then_some(map)
}

/// The bytes two lines share at the head and at the tail, each cut to a
/// character boundary and never overlapping. The shared bytes are equal,
/// so one line's boundaries are the other's there.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let mut head = 0usize;

    while head < a.len().min(b.len()) && a.as_bytes()[head] == b.as_bytes()[head] {
        head += 1;
    }

    while !a.is_char_boundary(head) {
        head -= 1;
    }

    let room = (a.len() - head).min(b.len() - head);
    let mut tail = 0usize;

    while tail < room && a.as_bytes()[a.len() - 1 - tail] == b.as_bytes()[b.len() - 1 - tail] {
        tail += 1;
    }

    while !a.is_char_boundary(a.len() - tail) {
        tail -= 1;
    }

    (head, tail)
}

/// One region as edits that leave every `{ }` hole in place, so the
/// code the author wrote inside a tag keeps its own bytes. `None` when
/// a piece between two holes holds a different number of newlines than
/// the source it replaces, which `apply_edits` refuses.
fn region_edits(src: &str, region: &luaux::compile::Region, emitted: &str) -> Option<Vec<Edit>> {
    let mut kept = Vec::new();
    let (node, _) = luaux::markup::parse_node(src, region.src_start).ok()?;
    copied_spans(src, &node, &mut kept);

    let mut edits = Vec::new();
    let mut at = region.src_start;
    let mut out = 0usize;

    for (start, end) in kept {
        if start < at || end > region.src_end {
            continue;
        }

        // Text the lowering rewrote is not in the emitted call: a hole
        // that held markup of its own, a property an alias renamed. It
        // stays inside the piece around it.
        let Some(found) = whole_word_from(emitted, &src[start..end], out) else {
            continue;
        };

        edits.push(piece(src, at, start, &emitted[out..found])?);
        at = end;
        out = found + (end - start);
    }

    edits.push(piece(src, at, region.src_end, &emitted[out..])?);

    Some(edits)
}

/// Where `needle` next stands in `hay` at or after `from`, as a whole
/// word: a name never matches inside a longer one. The search runs in
/// source order, so an earlier match belongs to an earlier piece.
fn whole_word_from(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let head = needle.starts_with(word);
    let tail = needle.ends_with(word);

    hay.get(from..)?
        .match_indices(needle)
        .map(|(i, _)| from + i)
        .find(|at| {
            let before = !head || !hay[..*at].ends_with(word);
            let after = !tail || !hay[at + needle.len()..].starts_with(word);

            before && after
        })
}

/// One edit, when the text it writes keeps the line count of the span
/// it replaces. `apply_edits` refuses any other, and one refusal drops
/// the whole map.
fn piece(src: &str, start: usize, end: usize, text: &str) -> Option<Edit> {
    let same = src[start..end].matches('\n').count() == text.matches('\n').count();

    same.then(|| Edit {
        start: start as u32,
        end: end as u32,
        text: text.to_string(),
    })
}

/// The byte ranges of a node the lowering copies, in source order: the
/// name of each attribute, the string it is given, and the code in
/// every `{ }` hole. A range whose text is not in the emitted call is
/// dropped later, so a renamed property costs nothing.
fn copied_spans(src: &str, node: &luaux::markup::Node, out: &mut Vec<(usize, usize)>) {
    use luaux::markup::{Attribute, AttributeValue, Child, Node};

    /// The range, when the source there reads as the parser recorded it.
    fn kept(src: &str, start: usize, end: usize, text: &str) -> Option<(usize, usize)> {
        (src.get(start..end) == Some(text) && !text.is_empty()).then_some((start, end))
    }

    let children = match node {
        Node::Element(element) => {
            for attribute in &element.attributes {
                let (expression, span) = match attribute {
                    Attribute::Named { name, value, span } => {
                        out.extend(kept(src, span.start, span.start + name.len(), name));

                        match value {
                            AttributeValue::Expression(expression) => (expression, span),

                            AttributeValue::StringLiteral(text) => {
                                out.extend(kept(src, span.end - text.len(), span.end, text));

                                continue;
                            }

                            AttributeValue::Boolean => continue,
                        }
                    }

                    Attribute::Spread { expression, span }
                    | Attribute::Inferred { expression, span } => (expression, span),
                };
                let open = span.start + src[span.start..span.end].find('{').unwrap_or(0);

                if let Some(range) = hole_range(src, open, span.end, expression) {
                    out.push(range);
                }
            }

            &element.children
        }

        Node::Fragment(fragment) => &fragment.children,
    };

    for child in children {
        match child {
            Child::Node(node) => copied_spans(src, node, out),

            Child::Expression { expression, span } => {
                if let Some(range) = hole_range(src, span.start, span.end, expression) {
                    out.push(range);
                }
            }

            Child::Text { .. } | Child::Comment { .. } => {}
        }
    }
}

/// Where `expression` sits inside the hole that spans `open..close`.
/// `None` when the text is not the source's own, which is what a hole
/// holding markup of its own leaves behind.
fn hole_range(src: &str, open: usize, close: usize, expression: &str) -> Option<(usize, usize)> {
    let inner = src.get(open + 1..close.checked_sub(1)?)?;
    let start = open + 1 + (inner.len() - inner.trim_start().len());
    let end = start + expression.len();

    (src.get(start..end)? == expression && !expression.is_empty()).then_some((start, end))
}

/// The byte ranges of the markup, and the source with every one of
/// them blanked: `nil` and then spaces to the width the region had,
/// with every newline kept. The text is Alloy the parser reads, and
/// every byte outside a region keeps the offset it had, so a position
/// still maps.
///
/// The editor compiles this when the markup cannot lower, so the code
/// around a tag still answers. `None` when the markup does not parse,
/// which leaves no region to blank.
pub fn blank_markup(src: &str) -> Option<(Vec<(usize, usize)>, String)> {
    let spans = luaux::compile::markup_spans(src).ok()?;

    (!spans.is_empty()).then(|| {
        let text = luaux::resolve::blank_luaux_regions(src, &spans);

        (spans, text)
    })
}

pub use luaux::resolve::bound_names;

#[cfg(test)]
mod tests {
    use super::*;

    /// A lint's rewrite after the lowering. The markup grows the text,
    /// so a rewrite that kept the lowered offsets writes over the wrong
    /// bytes of the author's file. The range is a source range, and the
    /// applier drops one that no longer covers what the lint read.
    #[test]
    fn a_rewrite_reads_the_source_the_author_wrote() {
        let src = "import { create } from \"./util\"\n\nlocal function Cond(props: { open: boolean })\n    return <Frame>{function() return if props.open then <TextLabel /> else nil end}</Frame>\nend\n\nlocal function dead(n: number)\n    return n\nend\n\nreturn Cond\n";
        let out = compile_alx(src, &EmitOptions::default(), luaux::Config::bare())
            .expect("the markup compiles")
            .output;
        let lint = out
            .lints
            .iter()
            .find(|l| l.name == "unused_function")
            .expect("`dead` is never called");
        let fix = lint.fix.as_ref().expect("the rewrite");

        assert_eq!(&src[fix.start as usize..fix.end as usize], fix.saw);

        let (text, n) = crate::lint::apply_fixes(src, &out.lints);

        assert_eq!(n, 1);
        assert!(text.contains("local function _dead(n: number)"), "{text}");

        // The guard: a range the source moved under writes nothing.
        let moved = crate::lint::Fix {
            saw: "Gone".to_string(),
            ..fix.clone()
        };

        assert!(!crate::lint::fix_applies(src, &moved));
    }

    /// A function that returns markup, or that a tag names, is a
    /// component and takes `[lint.naming] component`, PascalCase by
    /// default. Any other function takes the function style.
    #[test]
    fn a_component_takes_the_component_style() {
        let src = "local function create(n: string): any return n end\nlocal function Row()\n    return <TextLabel />\nend\nlocal function main_panel()\n    return <Frame><Row /></Frame>\nend\nlocal function FormatText(s: string): string\n    return s:upper()\nend\nreturn { main_panel = main_panel, format = FormatText }\n";
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        let out = compile_alx(src, &EmitOptions::default(), config)
            .expect("the markup compiles")
            .output;
        let naming: Vec<&str> = out
            .lints
            .iter()
            .filter(|l| l.name == "naming_convention")
            .map(|l| l.message.as_str())
            .collect();

        assert_eq!(
            naming,
            [
                "`main_panel` is a component, and components are PascalCase here: `MainPanel`",
                "`FormatText` is a function, and functions are snake_case here: `format_text`",
            ]
        );
    }

    /// A lint message quotes the markup the author wrote, not the
    /// calls it lowers to.
    #[test]
    fn a_lint_message_quotes_the_markup() {
        let src = "local function create(n: string): any return n end\nlocal function Panel()\n    local e = <Frame Name=\"x\">\n        <TextLabel />\n    </Frame>\n    return e\nend\nreturn Panel\n";
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        let out = compile_alx(src, &EmitOptions::default(), config)
            .expect("the markup compiles")
            .output;
        let lint = out
            .lints
            .iter()
            .find(|l| l.name == "local_then_return")
            .expect("the lint");

        assert!(!lint.message.contains("create("), "{}", lint.message);
        assert!(
            lint.message
                .contains("<Frame Name=\"x\"> <TextLabel /> </Frame>"),
            "{}",
            lint.message
        );
    }

    /// A `Text` attribute beside text between the tags is a markup
    /// error. The lowering drops the attribute, so the import only it
    /// reads must not also read as unused.
    #[test]
    fn a_text_conflict_reports_and_keeps_its_names_used() {
        let src = "import { pad } from \"./util\"\nlocal function create(n: string): any return n end\nreturn <TextLabel Text={pad(\"x\")}>hello</TextLabel>\n";
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        let out = compile_alx(src, &EmitOptions::default(), config)
            .expect("the markup compiles")
            .output;
        let errors: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();

        assert_eq!(
            errors,
            [
                "markup: `Text` is set twice: by this attribute and by the text between the tags (remove the attribute, or the text between the tags)"
            ]
        );
        assert_eq!(
            &src[out.diagnostics[0].start as usize..out.diagnostics[0].end as usize],
            "Text={pad(\"x\")}"
        );
        assert!(
            !out.lints.iter().any(|l| l.name == "unused_import"),
            "{:?}",
            out.lints.iter().map(|l| &l.message).collect::<Vec<_>>()
        );
    }

    /// A lone `{expr}` that becomes `Text` must be a string or a number.
    /// With no reactivity, the check artifact passes it through
    /// `__alloy.text`, and the ship artifact keeps it bare. Text with
    /// holes is a string already. A reactive library takes a source
    /// there, so its hole stays bare in both.
    #[test]
    fn a_lone_text_hole_checks_its_type() {
        let src = "local function create(n: string): any return n end\nlocal function Corner(): any return 1 end\nreturn <Frame><TextLabel>{Corner()}</TextLabel><TextBox>n: {Corner()}</TextBox></Frame>\n";
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        config.interpolate = luaux::config::Interpolate::Plain;
        let out = compile_alx(src, &EmitOptions::default(), config.clone())
            .expect("the markup compiles")
            .output;

        assert!(
            out.check.contains("Text = __alloy.text(Corner())"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("Text = `n: {Corner()}`"),
            "{}",
            out.check
        );
        assert!(out.ship.contains("Text = Corner()"), "{}", out.ship);
        assert!(!out.ship.contains("__alloy"), "{}", out.ship);

        config.interpolate = luaux::config::Interpolate::Wrap;
        let out = compile_alx(src, &EmitOptions::default(), config)
            .expect("the markup compiles")
            .output;

        assert!(!out.check.contains("__alloy.text"), "{}", out.check);
    }

    /// An attribute value that is no literal must fit its property or
    /// its prop. The check artifact casts `__alloy.prop` to a function
    /// of that type and passes the value through it. The ship artifact
    /// keeps it bare. A literal against a primitive stays with the text
    /// check, and a reactive library keeps a Roblox tag's values bare.
    #[test]
    fn an_attribute_value_checks_its_type() {
        let src = "local function create(k: any, p: any): any return p end\ntype Props = { item: Item, count: number }\nlocal function Row(props: Props) return nil end\nlocal n = 1\nreturn <Frame><TextLabel Text={n} Visible /><Row item={5} count={1} /></Frame>\n";
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        config.interpolate = luaux::config::Interpolate::Plain;
        let out = compile_alx(src, &EmitOptions::default(), config.clone())
            .expect("the markup compiles")
            .output;

        for want in [
            "Text = (__alloy.prop :: (string?) -> (string?))(n)",
            "item = (__alloy.prop :: (Item) -> (Item))(5)",
            "count = 1",
        ] {
            assert!(out.check.contains(want), "{want}\n{}", out.check);
        }
        assert!(!out.ship.contains("__alloy"), "{}", out.ship);

        config.interpolate = luaux::config::Interpolate::Wrap;
        let out = compile_alx(src, &EmitOptions::default(), config)
            .expect("the markup compiles")
            .output;

        assert!(out.check.contains("Text = n"), "{}", out.check);
        assert!(out.check.contains("(__alloy.prop :: (Item) -> (Item))(5)"));
    }

    /// A warn-level markup lint is a lint. As a diagnostic it was an
    /// error, and the build skipped the file.
    #[test]
    fn a_built_once_child_is_a_lint_not_an_error() {
        let src = "local function create(n: string): any return n end\nreturn <Frame>{if a then <TextLabel /> else nil}</Frame>\n";
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        let out = compile_alx(src, &EmitOptions::default(), config)
            .expect("the markup compiles")
            .output;

        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        let lint = out
            .lints
            .iter()
            .find(|l| l.name == STATIC_CONDITIONAL_CHILD)
            .expect("the lint");

        assert_eq!(lint.start as usize, src.find("{if").unwrap());
    }

    #[test]
    fn the_scan_sees_alloy_bindings() {
        let names = bound_names(
            "import * as React from \"x\"\nimport { a as b, type T } from \"y\"\nconst Row = 1\nlocal { w = h } = t\nstruct Card as end\nlocal function f() end\nnamespace Scope as end\n",
        );

        for n in ["React", "b", "Row", "h", "Card", "f", "Scope"] {
            assert!(names.contains(n), "{n} missing from {names:?}");
        }

        assert!(!names.contains("from"));

        // What full_moon's walk used to add: parameters, loop names, and
        // a global a statement assigns.
        let names = bound_names(
            "local function Wrap<T>(Inner, n: number, opt: { k: T } = d)\nend\nfor i, Item: T in items do end\nReceipt = function() end\n",
        );

        for n in ["Wrap", "Inner", "n", "opt", "i", "Item", "Receipt"] {
            assert!(names.contains(n), "{n} missing from {names:?}");
        }

        for n in ["number", "k", "T", "d", "items"] {
            assert!(!names.contains(n), "{n} is no binding: {names:?}");
        }
    }

    /// A component whose props parameter names a struct: a tag's
    /// attributes lower to a plain table, which a nominal struct never
    /// takes. The report sits on the declaration; a `type` says nothing,
    /// and a PascalCase function that builds no markup is no component.
    #[test]
    fn a_struct_props_parameter_reports_at_the_declaration() {
        let messages = |src: &str| -> Vec<String> {
            compile_alx(src, &EmitOptions::default(), luaux::Config::default())
                .expect("the markup compiles")
                .output
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect()
        };
        let head = "import * as React from \"react\"\n\n";
        let body = "export function Card(props: CardProps): any\n    return (<TextLabel Text={props.title} />)\nend\n";
        let record = format!("{head}export type CardProps = {{ title: string }}\n\n");
        let declared = format!("{head}export struct CardProps as\n    title: string\nend\n\n");

        assert_eq!(
            messages(&format!("{declared}{body}")),
            vec![
                "markup: a component's props are a plain table; declare `CardProps` as a `type` or an `interface`, not a `struct`"
            ]
        );

        let clean = messages(&format!("{record}{body}"));
        assert!(clean.is_empty(), "{clean:?}");

        // No markup in the body: the function is no component.
        let plain = messages(&format!(
            "{declared}export function Make(props: CardProps): any\n    return props.title\nend\n\nlocal function App(): any\n    return (<TextLabel Text=\"a\" />)\nend\nprint(App)\n"
        ));
        assert!(plain.is_empty(), "{plain:?}");
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

    /// A Roblox property takes the type the class declares, and an
    /// event takes a function.
    #[test]
    fn a_roblox_property_checks_the_literal_it_is_given() {
        let src = "import * as React from \"@packages/react\" --@alloy-ignore\n\
local function Panel()\n\
    return (\n\
        <Frame>\n\
            <TextLabel Size={12} Text={5} />\n\
            <TextButton Activated={\"not a function\"} />\n\
            <TextLabel Text=\"fine\" TextSize={14} Visible={true} />\n\
            <ImageLabel Image=\"rbxassetid://1\" />\n\
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
            vec![
                "markup: property Size of TextLabel is UDim2, not number",
                "markup: property Text of TextLabel is string, not number",
                "markup: `Activated` is an event of TextButton; it takes a function, not a string",
            ],
            "{messages:?}"
        );
    }

    /// Only a whole literal has a type the check can name. A comparison
    /// that starts with a quote is a boolean, and `inf` is a name.
    #[test]
    fn a_literal_is_one_token() {
        use luaux::markup::AttributeValue::Expression;

        let ty = |e: &str| literal_type(&Expression(e.to_string()));

        assert_eq!(ty("\"a\""), Some("string"));
        assert_eq!(ty("`a{b}c`"), Some("string"));
        assert_eq!(ty("-1.5"), Some("number"));
        assert_eq!(ty("true"), Some("boolean"));

        for e in [
            "\"admin\" == props.role",
            "inf",
            "nan",
            "`a` .. `b{c}`",
            "-x",
        ] {
            assert_eq!(ty(e), None, "{e}");
        }
    }

    #[test]
    fn an_enum_property_reads_as_the_source_writes_it() {
        assert_eq!(readable_type("EnumFont"), "Enum.Font");
        assert_eq!(readable_type("UDim2"), "UDim2");
        assert_eq!(readable_type("number"), "number");
    }

    /// The lowering of a `.alx` file, with a config the file can reach.
    fn lower(src: &str) -> (luaux::compile::Compiled, SpanMap) {
        let mut config = luaux::Config::bare();
        config.create = "create".to_string();
        let compiled = luaux::compile::compile_recovering(src, &luaux::Table, config)
            .expect("the markup compiles");
        let map = lowering_map(src, &compiled);

        (compiled, map)
    }

    /// A line with no markup on it is copied, so every byte of it keeps
    /// its own place in the lowered text.
    #[test]
    fn a_line_beside_the_markup_maps_byte_for_byte() {
        let src = "local create = f\nlocal a = 1\nlocal e = <Frame />\n";
        let (_, map) = lower(src);

        for at in 0..src.find("<Frame").expect("the tag") as u32 {
            assert_eq!(map.to_output(at), Some(at), "byte {at}");
            assert_eq!(map.to_source(at), at, "byte {at}");
        }
    }

    /// The code in a `{ }` hole is the author's, so it is copied and its
    /// bytes map both ways. The tag around it is the lowering's.
    #[test]
    fn a_hole_keeps_its_own_bytes() {
        let src = "local create = f\nlocal p = { n = 1 }\nlocal e = <Frame Size={p.n} />\n";
        let (compiled, map) = lower(src);
        let hole = src.find("p.n").expect("the hole") as u32;
        let at = map.to_output(hole).expect("the hole is copied");

        assert!(compiled.output[at as usize..].starts_with("p.n"));
        assert_eq!(map.to_source(at), hole);
        assert!(!map.is_generated(at));

        // The call the lowering wrote is no one's text.
        let call = compiled.output.find("create(").expect("the call") as u32;
        assert!(map.is_generated(call));
    }

    /// Every position the map answers stays on the line the author
    /// wrote it on, markup or not.
    #[test]
    fn the_map_keeps_every_line() {
        let src = "local create = f\nlocal p = { n = 1 }\nlocal e = (\n    <Frame Size={p.n}>\n        <Label />\n    </Frame>\n)\n";
        let (compiled, map) = lower(src);
        let lines = |text: &str, at: usize| text[..at].matches('\n').count();

        for at in 0..compiled.output.len() {
            let src_at = map.to_source(at as u32) as usize;
            assert_eq!(
                lines(src, src_at),
                lines(&compiled.output, at),
                "output byte {at}"
            );
        }
    }

    /// The reading of last resort: with no region to go by, the head
    /// and the tail a line shares with its lowering are the author's.
    #[test]
    fn a_rewritten_line_maps_by_its_common_ends() {
        let src = "local a = 1\nlocal b = 2\n";
        let lowered = "local a = 1\nlocal bbbb = 2\n";
        let map = line_map(src, lowered).expect("a map");

        assert_eq!(map.to_output(6), Some(6));
        assert_eq!(map.to_source(6), 6);
        // `2` is past the rewrite on both sides.
        assert_eq!(map.to_source(lowered.find('2').expect("2") as u32), 22);
    }

    /// An attribute's name and the string it is given come through the
    /// lowering as they were written, so they map too.
    #[test]
    fn an_attribute_name_and_its_string_keep_their_bytes() {
        let src = "local create = f\nlocal e = <Frame Name=\"Hud\" />\n";
        let (compiled, map) = lower(src);

        for needle in ["Name", "\"Hud\""] {
            let at = src.find(needle).expect(needle) as u32;
            let out = map.to_output(at).expect(needle) as usize;
            assert!(compiled.output[out..].starts_with(needle), "{needle}");
            assert_eq!(map.to_source(out as u32), at, "{needle}");
        }
    }

    /// The blanked copy is Alloy the parser reads, keeps the width and
    /// the lines of the source, and leaves everything outside the
    /// markup where it was.
    #[test]
    fn blanking_the_markup_keeps_the_width_and_the_lines() {
        let src = "local x = 1\nlocal e = <Frame Size={x}>\n    <Label />\n</Frame>\nreturn e\n";
        let (spans, text) = blank_markup(src).expect("one region");

        assert_eq!(spans.len(), 1);
        assert_eq!(text.len(), src.len());
        assert_eq!(text.matches('\n').count(), src.matches('\n').count());
        assert!(text.starts_with("local x = 1\nlocal e = nil"), "{text}");
        assert!(text.ends_with("\nreturn e\n"), "{text}");
        assert!(!text.contains('<'), "{text}");

        // A file with no markup has nothing to blank.
        assert!(blank_markup("local x = 1\n").is_none());
    }

    #[test]
    fn common_ends_cut_on_a_character() {
        assert_eq!(common_ends("ab", "ab"), (2, 0));
        assert_eq!(common_ends("axb", "ayb"), (1, 1));
        assert_eq!(common_ends("aéb", "axxb"), (1, 1));
        assert_eq!(common_ends("", "abc"), (0, 0));
    }
}
