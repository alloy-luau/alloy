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

/// Whether the head opens a type alias and stops at its `=`:
/// `type A = `, `export type A<T> = `. The word after the `=` is the
/// alias body, which is a type.
fn opens_an_alias(head: &str) -> bool {
    let words: Vec<&str> = head.split_whitespace().collect();
    let named = match words.first() {
        Some(&"type") => 1,

        Some(&"export") | Some(&"global") if words.get(1) == Some(&"type") => 2,

        _ => 0,
    };

    named > 0 && words.len() == named + 2 && words.last() == Some(&"=")
}

/// Whether the last word of the head is one a type goes after:
/// `extends `, `satisfies `, `is `, `impl `, and the `for` of an
/// `impl`. `Context::detect` reads these off a head that ends in a
/// space; this reads them again so a dotted path in the slot resolves.
fn after_a_type_word(head: &str) -> bool {
    let words: Vec<&str> = head.split_whitespace().collect();

    match words.last().copied() {
        Some("satisfies" | "is" | "extends" | "impl") => true,

        Some("for") => words.first() == Some(&"impl"),

        _ => false,
    }
}

/// Whether a type goes at the caret: after a `:` that annotates, after
/// a `->`, inside a type-argument list, after the `=` of a type alias,
/// or after a word that names a type. A `::` is a cast the child
/// reads, and a `:` with no space is a method call.
pub(crate) fn takes_a_type(head: &str) -> bool {
    if in_type_arguments(head) {
        return true;
    }

    if head.ends_with("= ") && opens_an_alias(head) {
        return true;
    }

    if head.ends_with(' ') && after_a_type_word(head) {
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

    // `local x: number | `: a union or an intersection continues the
    // slot the head opened, so the caret still takes a type.
    if let Some(base) = head.strip_suffix("| ").or_else(|| head.strip_suffix("& ")) {
        let cut = base.trim_end_matches(|c: char| is_word(c) || " ?[]{}<>,.()|&".contains(c));

        if cut.len() < base.len()
            && let Some(through) = head.get(..cut.len() + 1)
        {
            return takes_a_type(through);
        }

        return false;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `local x: number | ` takes a type. The head read as a value and
    /// drew the child's globals.
    #[test]
    fn a_union_continues_the_type_slot() {
        assert!(takes_a_type("local x: number | "));
        assert!(takes_a_type("local x: number | string | "));
        assert!(takes_a_type("local x: Result<number, string> | "));
        assert!(takes_a_type("local function f(): number | "));
        assert!(takes_a_type("    inner: number & "));

        // No annotation opened the head, so the `|` says nothing.
        assert!(!takes_a_type("local x = a | "));
        assert!(!takes_a_type("| "));
    }

    /// `export type A = Star.` fell to the child, which answered with
    /// the emit's own names.
    #[test]
    fn the_body_of_a_type_alias_takes_a_type() {
        assert!(takes_a_type("type A = "));
        assert!(takes_a_type("export type A = "));
        assert!(takes_a_type("global type A = "));
        assert!(takes_a_type("export type A<T> = "));
        assert!(takes_a_type("export type A = Star."));

        // An assignment is no alias, and neither is a half-written one.
        assert!(!takes_a_type("local x = "));
        assert!(!takes_a_type("export type A = number | string "));
        assert!(!takes_a_type("type "));
    }

    /// `impl Star.` and `extends Star.` fell to the child, which
    /// listed the values of the module and then every global.
    #[test]
    fn a_path_after_a_type_word_takes_a_type() {
        assert!(takes_a_type("export interface I extends Star."));
        assert!(takes_a_type("impl Star."));
        assert!(takes_a_type("impl Star.Named for Star."));
        assert!(takes_a_type("local v = 1 satisfies Star."));
        assert!(takes_a_type("if v is Star."));

        // `for` opens a type slot only in an `impl` head.
        assert!(!takes_a_type("for item in Star."));
        assert!(!takes_a_type("local v = Star."));
    }
}
