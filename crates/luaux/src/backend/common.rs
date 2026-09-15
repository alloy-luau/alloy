//! Emission machinery shared by every backend.
//!
//! The backends differ in the *arrangement* of the constructor call — where the
//! props go, where the children go — and agree on everything inside it: how a
//! table's entries are laid out on their source lines, how comments and trailing
//! commas interact, which children fold into the `Text` property, and how
//! interpolated text is encoded.
//!
//! That agreement is what keeps `Element` cheap. Line preservation is the
//! subtlest thing this compiler does, and it lives here once rather than once
//! per backend.

use super::writer::Writer;
use super::{EmitContext, EmitError};
use crate::config::Interpolate;
use crate::markup::*;
use crate::roblox;

/// Merge helper for spread attributes. Inlined into the output rather than
/// required, so luaux has no runtime dependency (see `crate::imports`).
pub(super) const MERGE_PROPS: &str = crate::imports::MERGE_HELPER;

/// Reads a possibly-source value. Inlined rather than taken from the library, so
/// interpolated text costs no dependency (see `crate::imports`).
pub(super) const READ: &str = crate::imports::READ_HELPER;

/// How a backend lowers one nested node.
///
/// Passed into [`emit_table`] rather than called directly, because a table's
/// entries can hold elements and each backend builds an element differently.
/// A function pointer keeps `Entry` free of the context's lifetime.
pub(super) type EmitNode = fn(&Node, &EmitContext<'_>, &mut Writer<'_>) -> Result<(), EmitError>;

/// One entry in an emitted table.
pub(super) enum Entry<'a> {
    /// `Name = value`
    Pair { offset: usize, text: String },
    /// A nested element or fragment.
    Node { offset: usize, node: &'a Node },
    /// An expression child, emitted verbatim.
    Expression { offset: usize, expression: &'a str },
    /// A retained comment, already Luau. Carries no value, so it needs its own
    /// separator handling — a comment cannot sit between two commas.
    Comment { offset: usize, luau: &'a str },
    /// `[E] = { … }` — children under `[factory] children`.
    ///
    /// Holds the key by value rather than borrowing it from the context: one
    /// short string per element, against threading the context's lifetime
    /// through `Entry`.
    Children {
        offset: usize,
        key: String,
        entries: Vec<Entry<'a>>,
    },
}

impl Entry<'_> {
    pub(super) fn offset(&self) -> usize {
        match self {
            Entry::Pair { offset, .. }
            | Entry::Node { offset, .. }
            | Entry::Expression { offset, .. }
            | Entry::Comment { offset, .. }
            | Entry::Children { offset, .. } => *offset,
        }
    }
}

/// Which argument a run of entries belongs to, once spreads have split them.
pub(super) enum Group {
    Table(usize),
    Spread(usize),
}

/// An element's props, grouped around its spreads.
///
/// Attributes split into groups at each spread: a run of named attributes
/// becomes one table, each spread contributes its own argument, and the merge
/// helper joins them in source order. Both backends want exactly this — they
/// differ only in which argument position the result lands in.
pub(super) struct Props<'a> {
    groups: Vec<Vec<Entry<'a>>>,
    spreads: Vec<(usize, &'a str)>,
    order: Vec<Group>,
}

impl<'a> Props<'a> {
    /// Groups `element`'s attributes, resolving names and wrapping events.
    pub(super) fn build(
        element: &'a Element,
        plan: &TextPlan,
        intrinsic: Option<&str>,
        resolved: bool,
        context: &EmitContext<'_>,
    ) -> Self {
        let mut groups: Vec<Vec<Entry>> = vec![Vec::new()];
        let mut spreads: Vec<(usize, &str)> = Vec::new();
        let mut order: Vec<Group> = Vec::new();

        for attribute in &element.attributes {
            // A spread is its own argument and shares nothing below.
            if let Attribute::Spread { expression, span } = attribute {
                if !groups.last().expect("a group").is_empty() {
                    order.push(Group::Table(groups.len() - 1));
                    groups.push(Vec::new());
                }
                order.push(Group::Spread(spreads.len()));
                spreads.push((span.start, expression));
                continue;
            }

            // Written or inferred, an attribute is a name and a value from here
            // on: aliases, the property check, event wrapping, and Rule 5 all
            // apply the same way. Inference decides *which* name, and nothing
            // more — a shorthand that took a different path through any of this
            // would be a second set of rules to learn.
            let (name, value, span) = match attribute {
                Attribute::Spread { .. } => unreachable!("handled above"),
                Attribute::Named { name, value, span } => {
                    (name.clone(), attribute_value(value), *span)
                }
                Attribute::Inferred { expression, span } => {
                    let Some(name) = crate::markup::infer_name(expression) else {
                        context.record(
                            EmitError::new(
                                "cannot tell which property this names",
                                span.start,
                                span.end.saturating_sub(span.start),
                            )
                            .with_help(
                                "`={...}` takes a name or a dotted path, as ={Text} or ={props.Text}",
                            ),
                        );
                        continue;
                    };

                    (name.to_string(), expression.clone(), *span)
                }
            };

            // Aliases resolve to canonical Roblox names here, so emitted
            // code is the same regardless of a project's luaux.toml.
            let key = match (intrinsic, resolved) {
                (Some(class), true) => context.resolve_attribute(class, &name, span.start),
                _ => name.clone(),
            };

            // Rule 5: text between the tags overrides a `Text` attribute.
            //
            // Compared against the **canonical** name, after aliases. Compared
            // against what was written, a project that renamed `Text` — or set
            // any `[properties] all` casing — slipped past this and emitted the
            // property twice in one table. Luau takes the last, so the tags won
            // by accident rather than by rule, and the duplicate key sat there
            // looking deliberate.
            if plan.text.is_some() && key == "Text" {
                continue;
            }

            // An event becomes a wrapped key only on an intrinsic, which
            // is the only place `is_event` can answer. A component's
            // props are arbitrary and are never rewritten — wrapping any
            // name that is an event on *some* class would be a guess, and
            // a wrong guess here is silent (factory-plan.md §3.2).
            //
            // The canonical name is used, so a project that renamed
            // `Activated` in luaux.toml still emits the real event name.
            let key = match (intrinsic, resolved, context.event()) {
                (Some(class), true, Some(event)) if roblox::is_event(class, &key) => {
                    context.used_event();
                    event.key(&key)
                }
                _ => key,
            };

            groups.last_mut().expect("a group").push(Entry::Pair {
                offset: span.start,
                text: format!("{key} = {value}"),
            });
        }

        let last = groups.len() - 1;
        if let Some(text) = &plan.text {
            groups[last].push(Entry::Pair {
                offset: plan.offset,
                text: format!("Text = {text}"),
            });
        }

        Self {
            groups,
            spreads,
            order,
        }
    }

    /// Adds an entry to the last group — for a backend that puts children in
    /// the props table.
    pub(super) fn push_last(&mut self, entry: Entry<'a>) {
        let last = self.groups.len() - 1;
        self.groups[last].push(entry);
    }

    pub(super) fn extend_last(&mut self, entries: impl IntoIterator<Item = Entry<'a>>) {
        let last = self.groups.len() - 1;
        self.groups[last].extend(entries);
    }

    /// Whether any spread is present, and so whether the merge helper is needed.
    pub(super) fn merges(&self) -> bool {
        !self.spreads.is_empty()
    }

    /// Emits the props as one expression, merging when spreads are present.
    ///
    /// Does **not** advance the writer to the closing tag: the caller decides,
    /// because an arrangement with a children argument after this one still has
    /// lines to place.
    pub(super) fn emit(
        mut self,
        element: &Element,
        close: Option<usize>,
        context: &EmitContext<'_>,
        writer: &mut Writer<'_>,
        emit_node: EmitNode,
    ) -> Result<(), EmitError> {
        let last = self.groups.len() - 1;
        if !self.groups[last].is_empty() || self.order.is_empty() {
            self.order.push(Group::Table(last));
        }

        let uses_merge = self.merges();
        if uses_merge {
            // A configured merge replaces the inlined helper rather than
            // joining it: two ways to combine props in one file would be a
            // question with no answer.
            match context.merge() {
                Some(merge) => {
                    context.used_merge();
                    writer.push(&format!("{merge}("));
                }
                None => {
                    context.used_merge_props();
                    writer.push(&format!("{MERGE_PROPS}("));
                }
            }
        }

        for (index, group) in self.order.iter().enumerate() {
            if index > 0 {
                writer.push(",");
                match group {
                    Group::Spread(spread) => writer.break_or_space(self.spreads[*spread].0),
                    Group::Table(table) => {
                        let offset = self.groups[*table]
                            .first()
                            .map(Entry::offset)
                            .unwrap_or(element.span.start);
                        writer.break_or_space(offset);
                    }
                }
            } else if let Group::Spread(spread) = group {
                // A *leading* spread still belongs on the line it was written
                // on. A table positions each of its own entries, so it needs
                // nothing here — but a spread is pushed verbatim, and without
                // this it lands on the opening tag's line while the line it was
                // written on comes out blank. Hovering it then asks about a line
                // with nothing on it.
                //
                // `to` is a no-op when the spread is already on this line, so a
                // single-line element is untouched.
                writer.to(self.spreads[*spread].0);
            }

            match group {
                Group::Spread(spread) => writer.push(self.spreads[*spread].1),
                // Only the group emitted **last** may carry the emission down to
                // the closing tag, and `close` says whether this element owns
                // that line at all. Handed to every group instead, the *first*
                // one ran `writer.to(close)` and burnt every line the element had
                // left: a spread after a named attribute then landed on the
                // closing tag's line with blank lines above it, and a spread
                // holding a multi-line expression pushed its own newlines from
                // there — adding lines the source never had, which is the one
                // invariant this compiler cannot lose. The output stayed valid
                // Luau throughout, so nothing else caught it.
                Group::Table(table) => emit_table(
                    &self.groups[*table],
                    close.filter(|_| index + 1 == self.order.len()),
                    context,
                    writer,
                    emit_node,
                )?,
            }
        }

        if uses_merge {
            writer.push(")");
        }

        Ok(())
    }
}

/// Emits a table, optionally carrying the emission down to a closing line.
///
/// `close` is `Some` when this table is the **last** thing the element emits, so
/// its `}` is what lands on the closing tag's line and makes the output as tall
/// as the source. It is `None` when something follows — the element backend's
/// props table has a children argument after it, and a props table that spanned
/// to the close would eat the lines those children need.
pub(super) fn emit_table(
    entries: &[Entry<'_>],
    close: Option<usize>,
    context: &EmitContext<'_>,
    writer: &mut Writer<'_>,
    emit_node: EmitNode,
) -> Result<(), EmitError> {
    // An empty table still has to span the lines the element did. An opening and
    // closing tag on separate lines with nothing between them is an ordinary
    // shape — a container waiting for its children — and emitting a bare `{}`
    // where the opening tag stood silently shortens the file.
    //
    // Nothing about the output looks wrong afterwards, which is what makes it
    // expensive. Every line below moves up, so no run of text lines up with the
    // source, and a language server mapping luau-lsp's answers back onto the
    // `.luaux` drops all of them: the markup keeps working while the Luau half
    // of the file goes silent.
    if entries.is_empty() {
        match close.filter(|close| writer.will_break(*close)) {
            Some(close) => {
                writer.push("{");
                writer.to(close);
                writer.push("}");
            }
            None => writer.push("{}"),
        }

        return Ok(());
    }

    writer.push("{");

    // A comment is not a table field, so it never takes a comma — and the comma
    // has to be written *immediately after its value*, before any comment that
    // follows. Deferring it until the next entry puts it after the comment,
    // which is valid Lua but reads as though the comment owned it, and no
    // formatter will move it back.
    let last_value = entries
        .iter()
        .rposition(|entry| !matches!(entry, Entry::Comment { .. }));

    for (index, entry) in entries.iter().enumerate() {
        if let Entry::Comment { offset, luau } = entry {
            writer.break_or_space(*offset);
            writer.push(luau);
            continue;
        }

        writer.break_or_space(entry.offset());

        match entry {
            Entry::Comment { .. } => unreachable!("handled above"),
            Entry::Pair { text, .. } => writer.push(text),
            Entry::Node { node, .. } => emit_node(node, context, writer)?,
            // `[E] = {` is anchored at the opening tag, which the writer has
            // already passed, so `break_or_space` gives it a space and the
            // children still break onto their own source lines. The inner table
            // closes on the same offset the outer one does — the closing tag's
            // line — and `writer.to` is a no-op the second time, so the two
            // braces land together as `} }`.
            Entry::Children { key, entries, .. } => {
                writer.push(&format!("[{key}] = "));
                emit_table(entries, close, context, writer, emit_node)?;
            }
            // Emitted bare. An earlier design wrapped these in a one-element
            // table to stop a nil leaving a hole in the array part — but Vide
            // iterates children with generalised `for k, v in t`, which skips
            // absent keys rather than stopping at them. `ipairs` would truncate;
            // Vide does not use it. Verified by tests/runtime.
            Entry::Expression { expression, .. } => writer.push(expression),
        }

        if Some(index) != last_value {
            writer.push(",");
        }
    }

    // The closing brace sits on the line of the closing tag, so the emission
    // spans exactly the lines the LuauX did.
    match close.filter(|close| writer.will_break(*close)) {
        Some(close) => {
            // Trailing comma goes before the break, per Luau style — but only if
            // a value was written, since a comment cannot carry one.
            if last_value.is_some() {
                writer.push(",");
            }
            writer.to(close);
        }
        None => writer.push(" "),
    }

    writer.push("}");
    Ok(())
}

pub(super) fn attribute_value(value: &AttributeValue) -> String {
    match value {
        AttributeValue::Expression(expression) => expression.clone(),
        AttributeValue::StringLiteral(literal) => literal.clone(),
        AttributeValue::Boolean => "true".to_string(),
    }
}

pub(super) fn child_entries<'a>(
    children: &'a [Child],
    expressions_are_text: bool,
) -> Vec<Entry<'a>> {
    let mut entries = Vec::new();

    for child in children {
        match child {
            Child::Node(node) => entries.push(Entry::Node {
                offset: node.span().start,
                node,
            }),
            // Text is always folded into the `Text` property by plan_text.
            Child::Text { .. } => {}
            Child::Comment { luau, span } => entries.push(Entry::Comment {
                offset: span.start,
                luau,
            }),
            Child::Expression { .. } if expressions_are_text => {}
            Child::Expression { expression, span } => entries.push(Entry::Expression {
                offset: span.start,
                expression,
            }),
        }
    }

    entries
}

/// How an element's children divide between the `Text` property and Vide's
/// numeric child slots.
#[derive(Default)]
pub(super) struct TextPlan {
    /// Encoded Luau value for the `Text` property, if any.
    pub(super) text: Option<String>,
    /// Whether expression children were folded into `text` rather than left as
    /// children.
    pub(super) consumed_expressions: bool,
    /// Source offset of the first text part, so `Text = …` lands on its line.
    pub(super) offset: usize,
}

/// Text and expression children that lower to the `Text` property (PLAN.md §6.2).
///
/// Vide's numeric slots take Instances, tables, and functions — not strings — so
/// bare text has to become a property at compile time.
pub(super) fn plan_text(
    element: &Element,
    intrinsic: Option<&str>,
    context: &EmitContext<'_>,
) -> Result<TextPlan, EmitError> {
    let has_text_literal = element
        .children
        .iter()
        .any(|child| matches!(child, Child::Text { .. }));
    let has_nodes = element
        .children
        .iter()
        .any(|child| matches!(child, Child::Node(_)));
    let has_expressions = element
        .children
        .iter()
        .any(|child| matches!(child, Child::Expression { .. }));

    if !has_text_literal && !has_expressions {
        return Ok(TextPlan::default());
    }

    let Some(class) = intrinsic else {
        // Components take children, not text. Nothing here knows their props.
        if has_text_literal {
            return Err(EmitError::new(
                format!(
                    "<{}> is a component, so it cannot take bare text",
                    element.name.as_written()
                ),
                element.span.start,
                element.name.as_written().len() + 1,
            )
            .with_help("pass the text as a prop instead"));
        }
        return Ok(TextPlan::default());
    };

    if !roblox::has_text_property(class) {
        if has_text_literal {
            return Err(EmitError::new(
                format!("<{class}> has no Text property"),
                element.span.start,
                class.len() + 1,
            )
            .with_help("wrap the text in a <TextLabel>"));
        }
        // Expressions are ordinary children on a class with no text.
        return Ok(TextPlan::default());
    }

    // `<TextButton>{label}<UICorner/></TextButton>` is genuinely ambiguous: the
    // expression could be the button's text or another child, and nothing here
    // can tell. Emitting a guess produces code that fails inside Vide at
    // runtime, so refuse and ask for the explicit form.
    if has_expressions && has_nodes {
        return Err(EmitError::new(
            format!(
                "<{class}> has both an expression child and element children, so it is unclear \
                 whether the expression is text or a child"
            ),
            element.span.start,
            class.len() + 1,
        )
        .with_help("write it as Text={...} instead"));
    }

    let mut parts = Vec::new();
    let mut offset = element.span.start;

    for (index, child) in element.children.iter().enumerate() {
        match child {
            Child::Text { text, span } => {
                if index == 0 || parts.is_empty() {
                    offset = span.start;
                }
                parts.push(TextPart::Literal(text.clone()));
            }
            Child::Expression { expression, span } => {
                if parts.is_empty() {
                    offset = span.start;
                }
                parts.push(TextPart::Expression(expression.clone()));
            }
            Child::Node(_) | Child::Comment { .. } => {}
        }
    }

    // `use` is resolved at config load, so it is always present when `compute`
    // is. The fallback keeps a hand-built Config from panicking here.
    let mode = match (context.interpolate(), context.compute()) {
        (Interpolate::Plain, _) => TextMode::Plain,
        (Interpolate::Wrap, Some(wrapper)) => TextMode::Compute {
            wrapper,
            reader: context.use_fn().unwrap_or(crate::config::DEFAULT_USE),
        },
        (Interpolate::Wrap, None) => TextMode::Thunk,
    };

    let (text, references) = encode_text(&parts, mode);

    // Only a wrapped emission names anything. A plain literal, a bare single
    // expression, and plain interpolation all name nothing, so none of them
    // demands a binding or inlines a helper.
    if references {
        match mode {
            TextMode::Compute { .. } => context.used_compute(),
            TextMode::Thunk => context.used_read(),
            TextMode::Plain => {}
        }
    }

    Ok(TextPlan {
        text: Some(text),
        consumed_expressions: has_expressions,
        offset,
    })
}

enum TextPart {
    Literal(String),
    Expression(String),
}

/// How interpolated text is encoded — the resolved form of `[factory]`
/// `interpolate`, `compute`, and `use`.
#[derive(Clone, Copy)]
enum TextMode<'a> {
    /// `function() return `…{__luaux_read(x)}…` end` — the default. The library
    /// re-runs the thunk, and each hole is read in case it is a source.
    Thunk,
    /// `E(function(use) return `…{use(x)}…` end)` — the reader comes from the
    /// callback, so no helper is inlined.
    Compute { wrapper: &'a str, reader: &'a str },
    /// `` `…{x}…` `` — no wrapper and no reader. For a library with no per-prop
    /// reactivity, where a hole is already a value and reading it would mean
    /// calling something the author meant to interpolate.
    Plain,
}

/// Encodes text parts as the value of the `Text` property, and reports whether
/// the result was wrapped.
///
/// Three shapes, and the choice is what makes `<TextLabel>Clicked {count}
/// times</TextLabel>` reactive:
///
/// * **Literals only** — a plain quoted string. Nothing to track.
/// * **A single expression** — emitted bare, and correct under either library:
///   Vide treats a function on a property key as a source, and Fusion accepts a
///   state object there. `Text = label` also stays right for a plain string.
/// * **Mixed** — wrapped, because interpolation builds a string and would
///   stringify a source to `function: 0x…`. Each hole is read, and the whole
///   thing is wrapped so the library re-runs it when anything inside changes.
///
/// The wrapper is the one place the two libraries diverge, and it is the whole
/// reason `compute` exists: a thunk plus the inlined `__luaux_read` by default,
/// or `E(function(use) … end)` when configured, where the reader comes from the
/// callback and no helper is inlined at all.
fn encode_text(parts: &[TextPart], mode: TextMode<'_>) -> (String, bool) {
    let expressions = parts
        .iter()
        .filter(|part| matches!(part, TextPart::Expression(_)))
        .count();

    if expressions == 0 {
        return (encode_plain(parts), false);
    }

    if let [TextPart::Expression(expression)] = parts {
        return (expression.clone(), false);
    }

    match mode {
        TextMode::Compute { wrapper, reader } => (
            format!(
                "{wrapper}(function({reader}) return {} end)",
                encode_interpolated(parts, Some(reader))
            ),
            true,
        ),
        TextMode::Thunk => (
            format!(
                "function() return {} end",
                encode_interpolated(parts, Some(READ))
            ),
            true,
        ),
        TextMode::Plain => (encode_interpolated(parts, None), false),
    }
}

/// Double quotes, matching Luau convention and stylua's default. Attribute
/// literals are captured verbatim and keep whatever the author wrote; this
/// governs only strings luaux itself builds, which is text children.
fn encode_plain(parts: &[TextPart]) -> String {
    let mut out = String::from("\"");

    for part in parts {
        if let TextPart::Literal(text) = part {
            for character in text.chars() {
                match character {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    _ => out.push(character),
                }
            }
        }
    }

    out.push('"');
    out
}

fn encode_interpolated(parts: &[TextPart], reader: Option<&str>) -> String {
    let mut out = String::from("`");

    for part in parts {
        match part {
            TextPart::Literal(text) => {
                for character in text.chars() {
                    match character {
                        '\\' => out.push_str("\\\\"),
                        '`' => out.push_str("\\`"),
                        '{' => out.push_str("\\{"),
                        '}' => out.push_str("\\}"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        _ => out.push(character),
                    }
                }
            }
            TextPart::Expression(expression) => match reader {
                Some(reader) => out.push_str(&format!("{{{reader}({expression})}}")),
                None => out.push_str(&format!("{{{expression}}}")),
            },
        }
    }

    out.push('`');
    out
}
