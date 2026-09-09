//! Whether a type, rather than a value, goes at the caret: a type
//! argument list, an annotation, or a return type.

use super::strings::is_word;

/// Whether the caret sits in the type-argument list of a named type:
/// `Result<|`, `HashMap<string, |`. The name must touch its `<`, so a
/// comparison never reads as one.
fn in_type_arguments(head: &str) -> bool {
    let mut depth = 0i32;
    let mut open = None;
    let mut prev = ' ';

    for (i, c) in head.char_indices() {
        match c {
            '<' => {
                depth += 1;

                if depth == 1 {
                    open = Some(i);
                }
            }

            '>' if prev != '-' => {
                depth -= 1;

                if depth <= 0 {
                    depth = 0;
                    open = None;
                }
            }

            '(' | ')' | ';' | '"' | '\'' => {
                depth = 0;
                open = None;
            }

            _ => {}
        }

        prev = c;
    }

    let Some(open) = open else {
        return false;
    };
    let before = &head[..open];
    let start = before.len() - before.trim_end_matches(is_word).len();

    start > 0
        && before[before.len() - start..].starts_with(|c: char| c.is_uppercase())
        && head[open + 1..]
            .chars()
            .all(|c| is_word(c) || " ,<>?[]{}:.&|".contains(c))
}

/// Whether a type goes at the caret: after a `:` that annotates, after
/// a `->`, or inside a type-argument list. A `::` is a cast the child
/// reads, and a `:` with no space is a method call.
pub(crate) fn takes_a_type(head: &str) -> bool {
    if in_type_arguments(head) {
        return true;
    }

    // `local p: Math.`: a namespace path stands in the slot, and the
    // head before it is the one that takes the type.
    if let Some(base) = head.strip_suffix('.') {
        let cut = base.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '.');

        if cut.len() < base.len() {
            return takes_a_type(cut);
        }
    }

    if head.ends_with("-> ") {
        return true;
    }

    let annotation = head.ends_with(": ")
        || head.ends_with(": read ")
        || head.ends_with(": write ")
        || head.ends_with(": ...");

    // `c ? a : b` ends its else with a `:` that takes a value.
    annotation && !head.trim_end().ends_with("::") && !super::ternary_else(head)
}
