//! A struct value's metatable, printed in place: an empty one adds
//! nothing a reader can use, and a named one reads as the struct, the
//! enum, or the variant it belongs to.

use alloy::declarations::Shape;

use super::Known;
use super::naming::{is_tagged_variant, name_of_body};
use super::strings::{balanced_len, group_len, type_len};

/// A table with an empty metatable, `{ @metatable {  }, { x: T } }`,
/// prints as the table alone: the metatable adds nothing a reader
/// can use.
pub(crate) fn fold_empty_metatables(text: &mut String) {
    // An earlier fold may have closed the two spaces of the empty half,
    // or emptied a half the child printed over two lines.
    for head in [
        "{ @metatable {  },",
        "{ @metatable { },",
        "{ @metatable {},",
    ] {
        while let Some(i) = text.find(head) {
            let Some(len) = group_len(&text[i..], '{', '}') else {
                break;
            };
            let inner = text[i + head.len()..i + len - 1].trim().to_string();
            text.replace_range(i..i + len, &inner);
        }
    }
}

/// A struct value printed in place, `{ @metatable t1, { x: number } }`,
/// reads by the struct's name wherever it stands, not only after a `: `.
pub(crate) fn fold_metatable_groups(text: &mut String, known: &Known) {
    let mut from = 0;

    while let Some(i) = text[from..].find("{ @metatable ") {
        let open = from + i;
        let Some(len) = balanced_len(&text[open..]) else {
            break;
        };
        let body = text[open..open + len].to_string();
        let rest = &body["{ @metatable ".len()..];
        let head_len = type_len(rest);
        let head = rest[..head_len].trim().to_string();

        let replacement = match known.shapes.iter().find(|s| s.name() == head) {
            // Every variant of a payload enum carries the enum's own
            // metatable, so the metatable names none of them. The union
            // of the data tables is what reads as the enum.
            Some(Shape::Enum { .. }) => rest
                .get(head_len + 1..)
                .map(str::trim)
                .and_then(|t| t.strip_suffix('}'))
                .map(|t| t.trim().to_string()),

            // A struct's metatable prints by the struct's name.
            Some(Shape::Struct { name, .. }) => Some(name.clone()),

            _ => {
                let data = rest
                    .get(head_len + 1..)
                    .map(str::trim)
                    .and_then(|t| t.strip_suffix('}'))
                    .map(|t| t.trim().to_string());

                // The metatable is a solver variable the clause never
                // named. A tagged table under it is a variant, and the
                // union of the variants names the enum.
                match data.as_deref().is_some_and(is_tagged_variant) {
                    true => data,

                    false => name_of_body(&body, known),
                }
            }
        };

        match replacement {
            Some(name) => {
                text.replace_range(open..open + len, &name);
                from = open + name.len();
            }

            None => from = open + 1,
        }
    }
}
