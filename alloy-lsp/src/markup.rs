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
    /// The text of an element body, between the tags. Text is text, so
    /// nothing completes there.
    Text,
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
    let text = || in_markup_text(src, offset).then_some(Spot::Text);
    let lt = src[..offset].rfind('<')?;

    if !opens_markup(src, lt) {
        return text();
    }

    let tag = &src[lt + 1..offset];

    if tag.starts_with('/') {
        return text();
    }

    // Past the opening tag, or inside an expression hole: not ours.
    let mut depth = 0i32;

    for c in tag.chars() {
        match c {
            '{' => depth += 1,

            '}' => depth -= 1,

            '>' if depth == 0 => return text(),

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

/// Where the child can complete a `{ }` hole that opens with a keyword.
/// The hole lowers to its expression alone, so the cursor lands on the
/// keyword and the child answers nothing there. The expression after
/// the keyword carries the same scope the whole hole takes.
pub fn hole_expression_start(src: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(src.len());
    let open = enclosing_hole(src, offset)?;

    if !src[open + 1..offset].chars().all(char::is_whitespace) {
        return None;
    }

    let rest = src.get(offset..)?;
    let word: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();

    if !matches!(
        word.as_str(),
        "if" | "for" | "while" | "match" | "not" | "function"
    ) {
        return None;
    }

    let after = &rest[word.len()..];
    let gap = after.len() - after.trim_start_matches([' ', '\t']).len();

    (gap > 0).then_some(offset + word.len() + gap)
}

/// The `{` of the innermost hole still open at an offset inside markup.
fn enclosing_hole(src: &str, offset: usize) -> Option<usize> {
    let spans = alloy::luaux::compile::markup_spans(src).ok()?;
    let (start, _) = spans
        .iter()
        .copied()
        .find(|(s, e)| *s <= offset && offset < *e)?;
    let mut open: Vec<usize> = Vec::new();

    for (k, c) in src[start..offset].char_indices() {
        match c {
            '{' => open.push(start + k),

            '}' => {
                open.pop();
            }

            _ => {}
        }
    }

    open.pop()
}

/// Whether the cursor sits in the text of an element body: inside a
/// markup region, outside every tag, and outside every `{ }` hole. The
/// child would answer such a spot with the whole value scope.
fn in_markup_text(src: &str, offset: usize) -> bool {
    let Ok(spans) = alloy::luaux::compile::markup_spans(src) else {
        return false;
    };
    let Some((start, _)) = spans
        .iter()
        .copied()
        .find(|(s, e)| *s <= offset && offset < *e)
    else {
        return false;
    };
    let mut depth = 0i32;
    let mut in_tag = false;

    for c in src[start..offset].chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            '<' if depth == 0 => in_tag = true,
            '>' if depth == 0 => in_tag = false,
            _ => {}
        }
    }

    depth == 0 && !in_tag
}

/// The prop names a component takes, read from its parameter type: the
/// record written in place, or the alias that names one.
pub fn component_props(src: &str, name: &str) -> Vec<String> {
    let head = format!("function {name}(");

    let Some(at) = src.find(&head) else {
        return Vec::new();
    };
    let rest = &src[at + head.len()..];

    let Some(close) = rest.find(')') else {
        return Vec::new();
    };
    let param = &rest[..close];

    let Some((_, declared)) = param.split_once(": ") else {
        return Vec::new();
    };
    let declared = declared.trim();

    if declared.starts_with('{') {
        return record_keys(declared);
    }

    // `props: Props`, where `type Props = { ... }`.
    let alias = format!("type {declared} = ");

    match src.find(&alias) {
        Some(k) => record_keys(src[k + alias.len()..].trim_start()),

        None => Vec::new(),
    }
}

/// The keys of a record type that starts the text, `{ a: number, b:
/// string }`, at brace depth one.
fn record_keys(text: &str) -> Vec<String> {
    let Some(body) = text.strip_prefix('{') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let mut out = Vec::new();
    let mut word = String::new();
    let mut key = true;

    for c in body.chars() {
        match c {
            '{' | '(' | '[' | '<' => depth += 1,
            '}' | ')' | ']' | '>' if depth == 0 => break,
            '}' | ')' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                word.clear();
                key = true;
            }
            ':' if depth == 0 && key => {
                let name = word.trim().to_string();

                if !name.is_empty() {
                    out.push(name);
                }

                word.clear();
                key = false;
            }
            _ if depth == 0 && key => word.push(c),
            _ => {}
        }
    }

    out
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

/// The spot for a hover: the parsed tree, and the text for a tag the
/// tree does not hold.
pub fn hover_spot(src: &str, offset: usize) -> Option<Spot> {
    // The `<` itself is punctuation. The child reads the factory the
    // markup lowers to and answers with that, which is emit detail.
    if src[offset..].starts_with('<') && opens_markup(src, offset) {
        return Some(Spot::Text);
    }

    tree_spot(src, offset)
        .or_else(|| tag_name_at(src, offset))
        .or_else(|| attribute_at(src, offset))
}

/// An attribute name read from the text, for a tag the tree holds no
/// element for: a tag inside a `{ }` hole is one expression there.
fn attribute_at(src: &str, offset: usize) -> Option<Spot> {
    let lt = src.get(..offset)?.rfind('<')?;

    if !opens_markup(src, lt) || src[lt..].starts_with("</") {
        return None;
    }

    let mut depth = 0i32;

    for c in src[lt + 1..offset].chars() {
        match c {
            '{' => depth += 1,

            '}' => depth -= 1,

            '>' if depth == 0 => return None,

            _ => {}
        }
    }

    if depth != 0 {
        return None;
    }

    let class: String = src[lt + 1..offset]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    if class.is_empty() {
        return None;
    }

    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = src[..offset]
        .rfind(|c: char| !is_word(c))
        .map_or(0, |i| i + 1);
    let end = offset + src[offset..].find(|c: char| !is_word(c)).unwrap_or(0);
    let name = &src[start..end];

    // The tag's own name is not one of its attributes.
    if name.is_empty() || start <= lt + class.len() {
        return None;
    }

    src[end..]
        .trim_start()
        .starts_with('=')
        .then(|| Spot::Attribute {
            class,
            name: name.to_string(),
        })
}

/// The tag name under the cursor, read from the text. A tag inside a
/// `{ }` hole is one expression to the markup parser, so the tree has
/// no element for it and the hover would fall through to the function
/// the component is.
fn tag_name_at(src: &str, offset: usize) -> Option<Spot> {
    let lt = src.get(..offset)?.rfind('<')?;

    if !opens_markup(src, lt) {
        return None;
    }

    let after = src.get(lt + 1..)?;
    let after = after.strip_prefix('/').unwrap_or(after);
    let start = src.len() - after.len();
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    if name.is_empty() || offset < start || offset > start + name.len() {
        return None;
    }

    Some(Spot::Tag { name })
}

/// The spot for a hover from the parsed tree, so the name and attributes
/// are exact.
fn tree_spot(src: &str, offset: usize) -> Option<Spot> {
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
            if alloy::alx::FREE_PROPS.contains(&name.as_str()) {
                format!("`{name}`: the markup reads it itself; it never reaches `{class}`.")
            } else if !roblox::is_class(class) {
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
pub fn completions(spot: &Spot, bound: &HashSet<String>, src: &str) -> Vec<Value> {
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
            let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();

            // A component takes the props its parameter type declares,
            // not a Roblox class's properties.
            if !roblox::is_class(class) {
                for prop in component_props(src, class) {
                    if prop.starts_with(prefix.as_str()) && !taken.contains(prop.as_str()) {
                        items.push(json!({
                            "label": prop,
                            "kind": 10,
                            "detail": format!("prop of {class}"),
                            "insertText": format!("{prop}={{$1}}"),
                            "insertTextFormat": 2,
                            "sortText": format!("1{prop}"),
                        }));
                    }
                }

                // The framework reads `key` itself, so a tag may set it
                // on any component and no declaration lists it.
                for prop in alloy::alx::FREE_PROPS {
                    if prop.starts_with(prefix.as_str()) && !taken.contains(prop) {
                        items.push(json!({
                            "label": prop,
                            "kind": 10,
                            "detail": "prop of the markup",
                            "insertText": format!("{prop}={{$1}}"),
                            "insertTextFormat": 2,
                            "sortText": format!("2{prop}"),
                        }));
                    }
                }

                return items;
            }

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

    /// A tag inside a `{ }` hole is one expression to the markup
    /// parser, so the text has to name it.
    #[test]
    fn a_tag_inside_a_hole_hovers_as_the_tag() {
        let src =
            "local e = <Frame>{rows:map(function(s) return <Badge label={s} /> end)}</Frame>\n";
        let at = src.find("<Badge").unwrap() + 2;
        assert_eq!(
            hover_spot(src, at),
            Some(Spot::Tag {
                name: "Badge".into()
            })
        );
        // A hole's own expression is not a tag.
        let inner = src.find("rows").unwrap() + 1;
        assert_eq!(hover_spot(src, inner), None);
    }

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
            "",
        );
        assert!(items.iter().any(|i| i["label"] == "TextLabel"));
        let items = completions(
            &Spot::AttributeSlot {
                class: "TextButton".into(),
                prefix: "Act".into(),
                existing: vec![],
            },
            &HashSet::new(),
            "",
        );
        assert!(
            items
                .iter()
                .any(|i| i["label"] == "Activated" && i["kind"] == 23)
        );
    }

    /// A tag inside a `{ }` hole has no element in the tree, so its
    /// attributes read from the text.
    #[test]
    fn an_attribute_of_a_tag_in_a_hole_hovers() {
        let src = "local function V(xs)\n    return <Frame>{xs:map(function(x)\n        return <Row key={x.id} slot={x} />\n    end)}</Frame>\nend\n";
        let at = src.find("key=").expect("key");

        assert_eq!(
            hover_spot(src, at),
            Some(Spot::Attribute {
                class: "Row".into(),
                name: "key".into(),
            })
        );
        let value = hover(
            &Spot::Attribute {
                class: "Row".into(),
                name: "key".into(),
            },
            &HashSet::new(),
        )
        .expect("hover");

        assert!(
            value["contents"]["value"]
                .as_str()
                .is_some_and(|t| t.contains("never reaches `Row`")),
            "{value}"
        );
    }

    #[test]
    fn markup_text_completes_nothing() {
        let src = "local function V()\n    return <TextLabel>hello there</TextLabel>\nend\n";
        let at = src.find("there").expect("text");

        assert_eq!(completion_spot(src, at), Some(Spot::Text));
        assert!(completions(&Spot::Text, &HashSet::new(), src).is_empty());
        // A hole is code, and the child answers it.
        let hole = "local function V()\n    return <TextLabel>{x}</TextLabel>\nend\n";
        let inside = hole.find("x}").expect("hole");

        assert_eq!(completion_spot(hole, inside), None);
    }

    #[test]
    fn a_hole_that_opens_with_a_keyword_completes_past_it() {
        let src = "local function V()\n    return <Frame>{if n == 0 then a else b}</Frame>\nend\n";
        let open = src.find("{if").expect("hole") + 1;

        assert_eq!(hole_expression_start(src, open), src.find("n == 0"));
        // A hole that names a value needs no move.
        let plain = "local function V()\n    return <Frame>{rows}</Frame>\nend\n";

        assert_eq!(
            hole_expression_start(plain, plain.find("rows").expect("hole")),
            None
        );
    }

    #[test]
    fn a_component_offers_the_props_it_declares() {
        let src = "type Props = { title: string, count: number }\nlocal function Panel(props: Props) end\nlocal function Badge(props: { label: string }) end";
        assert_eq!(component_props(src, "Badge"), vec!["label".to_string()]);
        assert_eq!(
            component_props(src, "Panel"),
            vec!["title".to_string(), "count".to_string()]
        );

        let items = completions(
            &Spot::AttributeSlot {
                class: "Badge".into(),
                prefix: String::new(),
                existing: vec![],
            },
            &HashSet::new(),
            src,
        );
        // The declared prop, then `key`, which the markup reads itself.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["label"], "label");
        assert_eq!(items[1]["label"], "key");
    }
}
