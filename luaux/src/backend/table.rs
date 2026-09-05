//! The one-table backend (PLAN.md §6) — `F(class)(propsAndChildren)`.
//!
//! One curried constructor taking a single flat table: string keys are
//! properties and events, and children go in that same table. The library
//! handles parenting, fragment recursion, event connection, ordering, and
//! reactive updates, leaving the compiler only the compile-time work.
//!
//! Vide is the shape this was written against — its `create` takes children as
//! the array part, so LuauX maps onto it almost 1:1. Fusion is the same
//! arrangement with the contents rearranged: children under a `[Children]` key,
//! events wrapped as `[OnEvent(name)]`. Those differences are `[factory]`
//! variables, not code paths here.
//!
//! Output is line-preserving: each entry lands on the source line its attribute
//! or child was written on, and the whole emission spans exactly the lines the
//! LuauX did (PLAN.md §5.5).

use super::common::{child_entries, emit_table, plan_text, Entry, Props, TextPlan};
use super::writer::Writer;
use super::{Backend, EmitContext, EmitError};
use crate::markup::*;
use crate::resolve::Resolution;

// The element factory is configurable (`[factory] create`) and reaches the
// backend through EmitContext, so there is no constant for it here.

pub struct Table;

impl Backend for Table {
    fn name(&self) -> &'static str {
        "table"
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
        Node::Fragment(fragment) => {
            // A fragment is a plain table; Vide recurses tables in numeric
            // position, so it needs no runtime representation of its own.
            let entries = child_entries(&fragment.children, false);
            emit_table(
                &entries,
                Some(fragment.span.end.saturating_sub(1)),
                context,
                writer,
                emit_node,
            )
        }
    }
}

fn emit_element(
    element: &Element,
    context: &EmitContext<'_>,
    writer: &mut Writer<'_>,
) -> Result<(), EmitError> {
    // Resolution rejects a name that is neither a Roblox class nor bound in the
    // file, so a typo like `<Frmae/>` fails here with a did-you-mean instead of
    // compiling into a call to an undefined global (PLAN.md §3.1).
    // An unresolved name is emitted as written and its attributes are left
    // alone: with no class there is nothing to check them against, and checking
    // anyway would report one error per attribute, all of them caused by the
    // single mistake already reported on the tag.
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

    match &intrinsic {
        Some(class) => {
            context.used_create();
            writer.push(&format!("{}(\"{class}\")(", context.create()));
        }
        None => writer.push(&format!("{}(", element.name.as_written())),
    }

    let mut props = Props::build(element, &plan, intrinsic.as_deref(), resolved, context);
    let children = child_entries(&element.children, plan.consumed_expressions);

    match context.children() {
        // Children are the array part of the props table — Vide's convention.
        None => props.extend_last(children),
        // Children go under a key, and the entry is omitted entirely when there
        // are none, so `<Frame/>` stays `F("Frame")({})` rather than gaining an
        // empty `[Children] = {}` (factory-plan.md §3.1).
        //
        // Applies to components as well as intrinsics: the README promises the
        // two are interchangeable at the call site, and splitting them here
        // would break that over a guess about what a component expects.
        Some(key) => {
            if !children.is_empty() {
                context.used_children();
                props.push_last(Entry::Children {
                    offset: element.span.start,
                    key: key.to_string(),
                    entries: children,
                });
            }
        }
    }

    props.emit(
        element,
        Some(element.span.end.saturating_sub(1)),
        context,
        writer,
        emit_node,
    )?;

    // The closing parenthesis sits on the line of the closing tag, so the
    // element spans exactly the lines the LuauX did.
    //
    // `emit_table` already does this for the table it emits, which covers most
    // elements — but an element whose attributes are *all* spreads emits no
    // table at all (the trailing group is empty and is dropped above), and then
    // nothing else would. `<Component {props} />` written across lines is the
    // ordinary way to forward props, and it was losing every line it spanned.
    writer.to(element.span.end.saturating_sub(1));

    writer.push(")");
    Ok(())
}
