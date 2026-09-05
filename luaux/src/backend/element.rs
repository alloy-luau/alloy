//! The element backend (backend-plan.md §3) — `F(class, props, children)`.
//!
//! Three positional arguments rather than a curried pair, which is the whole
//! reason this is a backend and not a `[factory]` variable: no key anywhere in a
//! props table expresses a third argument.
//!
//! React is the shape this was written against. Two things follow from the
//! arrangement rather than from the library:
//!
//! * **A component goes through the factory too.** `createElement(Card, props)`
//!   rather than `Card(props)`, differing from an intrinsic only in whether the
//!   first argument is quoted.
//! * **A fragment is an element.** A plain table is a fragment under a one-table
//!   library and is not one here, so `[factory] fragment` names what to build it
//!   with, and the config rejects this backend without it.
//!
//! Children are emitted as a **single table**, not as varargs. `createElement`
//! assigns a lone third argument straight to `props.children`, so both forms
//! reach the reconciler — but the table reuses [`emit_table`] verbatim, which is
//! where line preservation lives, and it leaves room for keyed children that
//! varargs never could.
//!
//! A nil child leaves a hole in that table, and the hole is **correct**. React
//! renders nothing for it and every sibling keeps its index, so a conditional
//! child that toggles does not remount the children after it. Compacting the
//! list would move them, which under React's implicit keys is a remount.
//! Verified against real react-lua under Lune — see `tests/runtime.rs`.
//!
//! Output is line-preserving on the same terms as every other backend
//! (PLAN.md §5.5).

use super::common::{child_entries, emit_table, plan_text, Props, TextPlan};
use super::writer::Writer;
use super::{Backend, EmitContext, EmitError};
use crate::markup::{Element as Markup, Fragment, Node};
use crate::resolve::Resolution;

pub struct Element;

impl Backend for Element {
    fn name(&self) -> &'static str {
        "element"
    }

    fn emit(&self, node: &Node, context: &EmitContext<'_>) -> Result<String, EmitError> {
        let mut writer = Writer::new(context, node.span().start);
        emit_node(node, context, &mut writer)?;
        Ok(writer.finish())
    }
}

fn emit_node(
    node: &Node,
    context: &EmitContext<'_>,
    writer: &mut Writer<'_>,
) -> Result<(), EmitError> {
    match node {
        Node::Element(element) => emit_element(element, context, writer),
        Node::Fragment(fragment) => emit_fragment(fragment, context, writer),
    }
}

/// `F(Fragment, nil, { … })`.
///
/// The props argument is `nil` rather than `{}`: a fragment has no attributes to
/// carry, ever, so there is no line for an empty table to hold and nothing for
/// one to say. The children table still spans the lines the fragment did.
fn emit_fragment(
    fragment: &Fragment,
    context: &EmitContext<'_>,
    writer: &mut Writer<'_>,
) -> Result<(), EmitError> {
    // The config rejects this backend with no fragment, so this is unreachable
    // through `Config::parse`. A hand-built config can still get here, and
    // saying so beats emitting a table React will not accept.
    let Some(name) = context.fragment() else {
        return Err(EmitError::new(
            "a fragment needs [factory] fragment under this backend",
            fragment.span.start,
            2,
        )
        .with_help("set it to the library's fragment component, as React.Fragment"));
    };

    context.used_create();
    context.used_fragment();
    writer.push(&format!("{}({name}, nil, ", context.create()));

    let entries = child_entries(&fragment.children, false);
    emit_table(
        &entries,
        Some(fragment.span.end.saturating_sub(1)),
        context,
        writer,
        emit_node,
    )?;

    writer.push(")");
    Ok(())
}

fn emit_element(
    element: &Markup,
    context: &EmitContext<'_>,
    writer: &mut Writer<'_>,
) -> Result<(), EmitError> {
    // Resolution rejects a name that is neither a Roblox class nor bound in the
    // file, so a typo like `<Frmae/>` fails here with a did-you-mean instead of
    // compiling into a call to an undefined global (PLAN.md §3.1).
    let (intrinsic, resolved) = match context.resolve(&element.name, element.span.start) {
        Resolution::Intrinsic(class) => (Some(class), true),
        Resolution::Component => (None, true),
        Resolution::Unresolved(written) => (Some(written), false),
    };

    // Nothing else about an unresolved element is worth checking. Its text
    // children and its attributes are judged against a class that does not
    // exist, so every complaint would be downstream of the one already recorded
    // on the tag itself.
    let plan = match resolved {
        true => plan_text(element, intrinsic.as_deref(), context)?,
        false => TextPlan::default(),
    };

    // Both an intrinsic and a component are the factory's first argument, and
    // differ only in the quoting. That is simpler than the one-table
    // arrangement, where a component is called directly.
    context.used_create();
    match (&intrinsic, resolved) {
        (Some(class), true) => writer.push(&format!("{}(\"{class}\", ", context.create())),
        _ => writer.push(&format!(
            "{}({}, ",
            context.create(),
            element.name.as_written()
        )),
    }

    let props = Props::build(element, &plan, intrinsic.as_deref(), resolved, context);
    // `None`: the children argument comes after, and a props table that
    // spanned to the closing tag would eat the lines those children need.
    props.emit(element, None, context, writer, emit_node)?;

    let children = child_entries(&element.children, plan.consumed_expressions);

    // Omitted entirely when there are none, so a leaf stays
    // `F("Frame", {})` rather than carrying an empty table nothing reads.
    if !children.is_empty() {
        writer.push(", ");
        emit_table(
            &children,
            Some(element.span.end.saturating_sub(1)),
            context,
            writer,
            emit_node,
        )?;
    }

    // The closing parenthesis sits on the line of the closing tag, so the
    // element spans exactly the lines the LuauX did.
    writer.to(element.span.end.saturating_sub(1));

    writer.push(")");
    Ok(())
}
