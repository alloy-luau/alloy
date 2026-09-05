//! `.alx` intellisense inside markup: what the cursor is on, hover text,
//! and completion items. Outside markup the child answers.

use std::collections::HashSet;

use alloy::luaux::markup::{Attribute, Child, Element, Node};
use alloy::luaux::roblox;
use serde_json::{Value, json};

/// What sits under the cursor inside markup.
#[derive(Debug, PartialEq, Eq)]
pub enum Spot {
    /// The name of an element, whole.
    Tag { name: String },
    /// An attribute name of an element.
    Attribute { class: String, name: String },
    /// Typing a tag name: `<Fra|`.
    TagSlot { prefix: String },
    /// Typing an attribute: `<Frame Si|`.
    AttributeSlot {
        class: String,
        prefix: String,
        existing: Vec<String>,
    },
}

/// Reports if `<` at `lt` opens markup rather than a comparison, by the
/// token before it.
fn opens_markup(src: &str, lt: usize) -> bool {
    let before = src[..lt].trim_end();

    if before.is_empty() {
        return true;
    }

    let last = before.chars().last().unwrap_or(' ');

    if matches!(last, '(' | '=' | ',' | '{' | '>' | '[' | '?') {
        return true;
    }

    let word = crate::imports::word_before(before, before.len());

    matches!(
        word.as_str(),
        "return" | "then" | "else" | "do" | "and" | "or" | "not"
    )
}

/// The spot for a completion: text-based, so an unfinished tag counts.
pub fn completion_spot(src: &str, offset: usize) -> Option<Spot> {
    let offset = offset.min(src.len());
    let lt = src[..offset].rfind('<')?;

    if !opens_markup(src, lt) {
        return None;
    }

    let tag = &src[lt + 1..offset];

    if tag.starts_with('/') {
        return None;
    }

    // Past the opening tag, or inside an expression hole: not ours.
    let mut depth = 0i32;

    for c in tag.chars() {
        match c {
            '{' => depth += 1,

            '}' => depth -= 1,

            '>' if depth == 0 => return None,

            _ => {}
        }
    }

    if depth > 0 {
        return None;
    }

    let is_name_char = |c: char| c.is_alphanumeric() || c == '_' || c == '.';

    if tag.chars().all(is_name_char) {
        return Some(Spot::TagSlot {
            prefix: tag.to_string(),
        });
    }

    let class: String = tag.chars().take_while(|c| is_name_char(*c)).collect();
    let prefix = crate::imports::word_before(tag, tag.len());
    // The word under the cursor is the one being typed, not one set.
    let settled = &tag[..tag.len() - prefix.len()];
    let existing = attribute_names(settled);

    Some(Spot::AttributeSlot {
        class,
        prefix,
        existing,
    })
}

/// The attribute names written in an opening tag's text.
fn attribute_names(tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut word = String::new();
    let mut first = true;

    for c in tag.chars() {
        match c {
            '{' => {
                depth += 1;
                word.clear();
            }

            '}' => depth -= 1,

            _ if depth > 0 => {}

            c if c.is_alphanumeric() || c == '_' => word.push(c),

            _ => {
                if !word.is_empty() {
                    if !first {
                        out.push(std::mem::take(&mut word));
                    } else {
                        word.clear();
                    }

                    first = false;
                }
            }
        }
    }

    if !word.is_empty() && !first {
        out.push(word);
    }

    out
}

/// The spot for a hover: the parsed tree, so the name and attributes are
/// exact.
pub fn hover_spot(src: &str, offset: usize) -> Option<Spot> {
    let spans = alloy::luaux::compile::markup_spans(src).ok()?;
    let (start, _) = spans
        .iter()
        .copied()
        .find(|(s, e)| *s <= offset && offset < *e)?;
    let (node, _) = alloy::luaux::markup::parse_node(src, start).ok()?;
    let element = deepest(&node, offset)?;
    let name = element.name.as_written();
    let name_start = element.span.start + 1;

    if offset >= name_start && offset <= name_start + name.len() {
        return Some(Spot::Tag { name });
    }

    // The closing tag names the element too: `</Frame>` at the end.
    let text = &src[element.span.start..element.span.end];

    if text.ends_with('>')
        && !text.ends_with("/>")
        && let Some(close) = text.rfind("</")
    {
        let close_start = element.span.start + close + 2;

        if offset >= close_start && offset <= close_start + name.len() {
            return Some(Spot::Tag { name });
        }
    }

    for attribute in &element.attributes {
        if let Attribute::Named {
            name: attr, span, ..
        } = attribute
            && offset >= span.start
            && offset <= span.start + attr.len()
        {
            return Some(Spot::Attribute {
                class: name,
                name: attr.clone(),
            });
        }
    }

    None
}

fn deepest(node: &Node, offset: usize) -> Option<&Element> {
    let children = match node {
        Node::Element(e) => {
            if offset < e.span.start || offset >= e.span.end {
                return None;
            }

            for child in &e.children {
                if let Child::Node(n) = child
                    && let Some(inner) = deepest(n, offset)
                {
                    return Some(inner);
                }
            }

            return Some(e);
        }

        Node::Fragment(f) => &f.children,
    };

    for child in children {
        if let Child::Node(n) = child
            && let Some(inner) = deepest(n, offset)
        {
            return Some(inner);
        }
    }

    None
}

fn chain(class: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut cur = roblox::superclass(class);

    while let Some(c) = cur {
        if c == "<<<ROOT>>>" || c.is_empty() {
            break;
        }

        out.push(c);
        cur = roblox::superclass(c);
    }

    out
}

/// Hover text for a spot, when there is something to say.
pub fn hover(spot: &Spot, bound: &HashSet<String>) -> Option<Value> {
    let text = match spot {
        Spot::Tag { name } => {
            if roblox::is_class(name) {
                let props = roblox::properties(name).count();
                let events = roblox::events(name).count();
                let parents = chain(name);
                let extends = if parents.is_empty() {
                    String::new()
                } else {
                    format!("\n\nExtends {}.", parents.join(" > "))
                };

                format!(
                    "```alx\n<{name}>\n```\nRoblox class `{name}`.{extends}\n\n{props} properties, {events} events."
                )
            } else if bound.contains(name.split('.').next().unwrap_or(name)) {
                format!("```alx\n<{name}>\n```\nComponent bound in this file.")
            } else {
                let hint = roblox::closest_class(name)
                    .map(|c| format!(" Did you mean `{c}`?"))
                    .unwrap_or_default();

                format!("```alx\n<{name}>\n```\nNot a Roblox class and not bound here.{hint}")
            }
        }

        Spot::Attribute { class, name } => {
            if !roblox::is_class(class) {
                format!("`{name}`: a prop of component `{class}`.")
            } else if roblox::is_event(class, name) {
                format!("`{name}`: event of `{class}`. The value is the handler.")
            } else if roblox::has_property(class, name) {
                let deprecated = if roblox::is_deprecated(name) {
                    " Deprecated."
                } else {
                    ""
                };

                format!("`{name}`: property of `{class}`.{deprecated}")
            } else {
                let close = roblox::closest_members(class, name);
                let hint = if close.is_empty() {
                    String::new()
                } else {
                    format!(" Did you mean `{}`?", close.join("`, `"))
                };

                format!("`{name}`: `{class}` has no property or event of that name.{hint}")
            }
        }

        _ => return None,
    };

    Some(json!({ "contents": { "kind": "markdown", "value": text } }))
}

/// Completion items for a slot.
pub fn completions(spot: &Spot, bound: &HashSet<String>) -> Vec<Value> {
    let mut items = Vec::new();

    match spot {
        Spot::TagSlot { prefix } => {
            for class in roblox::creatable_classes() {
                if class.starts_with(prefix.as_str()) {
                    items.push(json!({
                        "label": class,
                        "kind": 7,
                        "detail": "Roblox class",
                        "sortText": format!("1{class}"),
                    }));
                }
            }

            let mut names: Vec<&String> = bound
                .iter()
                .filter(|n| n.starts_with(prefix.as_str()))
                .collect();
            names.sort();

            for name in names {
                if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    items.push(json!({
                        "label": name,
                        "kind": 3,
                        "detail": "component",
                        "sortText": format!("0{name}"),
                    }));
                }
            }
        }

        Spot::AttributeSlot {
            class,
            prefix,
            existing,
        } => {
            if !roblox::is_class(class) {
                return items;
            }

            let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();

            for prop in roblox::properties(class) {
                if prop.starts_with(prefix.as_str()) && !taken.contains(prop) {
                    items.push(json!({
                        "label": prop,
                        "kind": 10,
                        "detail": format!("property of {class}"),
                        "insertText": format!("{prop}={{$1}}"),
                        "insertTextFormat": 2,
                        "sortText": format!("1{prop}"),
                    }));
                }
            }

            for event in roblox::events(class) {
                if event.starts_with(prefix.as_str()) && !taken.contains(event) {
                    items.push(json!({
                        "label": event,
                        "kind": 23,
                        "detail": format!("event of {class}"),
                        "insertText": format!("{event}={{function()\n\t$0\nend}}"),
                        "insertTextFormat": 2,
                        "sortText": format!("2{event}"),
                    }));
                }
            }
        }

        _ => {}
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_come_from_the_open_tag_text() {
        let src = "return <Fra";
        assert_eq!(
            completion_spot(src, src.len()),
            Some(Spot::TagSlot {
                prefix: "Fra".into()
            })
        );
        let src = "return <Frame Size={x} Vis";
        assert_eq!(
            completion_spot(src, src.len()),
            Some(Spot::AttributeSlot {
                class: "Frame".into(),
                prefix: "Vis".into(),
                existing: vec!["Size".into()]
            })
        );
        assert_eq!(completion_spot("local a = b < c", 15), None);
        assert_eq!(completion_spot("return <Frame Size={a.", 22), None);
    }

    #[test]
    fn hover_finds_tags_and_attributes() {
        let src = "return <Frame Size={x}><TextLabel Text=\"a\" /></Frame>\n";
        assert_eq!(
            hover_spot(src, 9),
            Some(Spot::Tag {
                name: "Frame".into()
            })
        );
        assert_eq!(
            hover_spot(src, 15),
            Some(Spot::Attribute {
                class: "Frame".into(),
                name: "Size".into()
            })
        );
        assert_eq!(
            hover_spot(src, 25),
            Some(Spot::Tag {
                name: "TextLabel".into()
            })
        );
        let h = hover(
            &Spot::Tag {
                name: "Frame".into(),
            },
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            h["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("GuiObject")
        );
    }

    #[test]
    fn closing_tag_hovers_as_the_element() {
        let src = "local x = <Frame Size={1}>\n    <TextLabel>hi</TextLabel>\n</Frame>\n";
        let close = src.rfind("Frame").unwrap();
        assert_eq!(
            hover_spot(src, close + 2),
            Some(Spot::Tag {
                name: "Frame".to_string()
            })
        );
        let inner_close = src.rfind("TextLabel").unwrap();
        assert_eq!(
            hover_spot(src, inner_close),
            Some(Spot::Tag {
                name: "TextLabel".to_string()
            })
        );
    }

    #[test]
    fn completions_list_classes_and_members() {
        let items = completions(
            &Spot::TagSlot {
                prefix: "TextL".into(),
            },
            &HashSet::new(),
        );
        assert!(items.iter().any(|i| i["label"] == "TextLabel"));
        let items = completions(
            &Spot::AttributeSlot {
                class: "TextButton".into(),
                prefix: "Act".into(),
                existing: vec![],
            },
            &HashSet::new(),
        );
        assert!(
            items
                .iter()
                .any(|i| i["label"] == "Activated" && i["kind"] == 23)
        );
    }
}
