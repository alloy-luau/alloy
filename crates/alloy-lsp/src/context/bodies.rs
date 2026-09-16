//! The body of a declaration around the caret: a `struct`, an `enum`,
//! an `impl`, or a `trait`, and where an attribute or a payload sits
//! inside one.

use super::strings::{block_closers, block_openers, is_word};

/// The body the cursor sits in, when a declaration opened above it
/// and no `end` at the margin closed it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Body {
    Struct,
    Enum,
    Impl,
    Trait,
    /// The contract of an `attribute ... as ... end`: `requires` clauses
    /// and nothing else.
    Attribute,
}

/// The declaration body around `line_start`: the nearest line at the
/// margin above that opens one, unless a margin `end` or another
/// margin statement sits between. Inside an `impl`, a method's own
/// block counts too: a cursor within one is in ordinary code.
pub(crate) fn enclosing_body(src: &str, line_start: usize) -> Option<Body> {
    let mut depth = 0i32;

    for line in src[..line_start].lines().rev() {
        let trimmed = line.trim_start();
        let at_margin = trimmed.len() == line.len();

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if !at_margin {
            // The blocks a method's body opens and closes, seen from
            // below: a closer first, then its opener.
            depth += block_closers(trimmed) - block_openers(trimmed);

            continue;
        }

        let decl = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let decl = decl.strip_prefix("@").map_or(decl, |_| "");

        // A declaration that closes on its own line, `struct T as end`
        // or `impl T end`, opens no body below it.
        if decl.split_whitespace().last() == Some("end") {
            return None;
        }

        return match decl.split_whitespace().next() {
            Some("struct" | "interface") if decl.contains(" as") || decl.ends_with("as") => {
                Some(Body::Struct)
            }
            Some("enum") if decl.contains(" as") => Some(Body::Enum),
            Some("attribute") if decl.contains(" as") => Some(Body::Attribute),
            // A negative depth means a method opened a block the walk
            // never closed: the caret sits in that method's body, which
            // is ordinary code, not the member column.
            Some("impl") if depth >= 0 => Some(Body::Impl),
            Some("trait") if depth >= 0 => Some(Body::Trait),
            _ => None,
        };
    }

    None
}

/// The body a line opens, for a caret on the opener's own line:
/// `struct S as |`. The body starts at the `as`, so the words the
/// body takes belong there as much as on the line below.
///
/// Only an opener that ends in `as` counts. `impl Drawable for |Point`
/// still names a type, so the caret is in a type slot, not at a member.
pub(crate) fn opens_a_body(head: &str) -> Option<Body> {
    let decl = head.trim();
    let decl = decl.strip_prefix("export ").unwrap_or(decl).trim_start();
    let decl = decl.strip_prefix("global ").unwrap_or(decl).trim_start();

    if !decl
        .strip_suffix("as")
        .is_some_and(|h| h.ends_with(char::is_whitespace) || h.ends_with('>'))
    {
        return None;
    }

    match decl.split_whitespace().next() {
        Some("struct" | "interface") => Some(Body::Struct),

        Some("enum") => Some(Body::Enum),

        Some("impl") => Some(Body::Impl),

        Some("trait") => Some(Body::Trait),

        Some("attribute") => Some(Body::Attribute),

        _ => None,
    }
}

/*
The words an attribute contract takes at the caret. The grammar of one
clause is

    requires <visibility>? <kind> (<name> | each <param>) <shape>?

so the words already on the line say which slot the caret is in. A
member name and a shape are the author's own text, and no position in
the body takes a name from the scope.

`head` is the line up to the word being typed, and `src[..line_start]`
the lines above, where the declaration states its parameters.
*/
pub(crate) fn contract_words(src: &str, line_start: usize, head: &str) -> Vec<String> {
    let words: Vec<&str> = head.split_whitespace().collect();
    let fixed = |list: &[&str]| list.iter().map(|w| w.to_string()).collect();

    match words.as_slice() {
        // The clause column, where `end` closes the body too.
        [] => fixed(&["requires", "end"]),

        ["requires"] => fixed(&["public", "private", "function", "field"]),

        ["requires", "public" | "private"] => fixed(&["function", "field"]),

        // Past the kind word the member name is the author's own.
        // `each` stands in its place, and only where a list parameter
        // can name the members.
        ["requires", "function" | "field"]
        | ["requires", "public" | "private", "function" | "field"] => {
            match list_parameters(src, line_start).is_empty() {
                true => Vec::new(),

                false => fixed(&["each"]),
            }
        }

        ["requires", .., "each"] => list_parameters(src, line_start),

        _ => Vec::new(),
    }
}

/// The parameters of the `attribute` declaration above the caret that
/// `each` can read: a list, or one with no type, which says nothing
/// either way.
fn list_parameters(src: &str, line_start: usize) -> Vec<String> {
    let Some(line) = src[..line_start]
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))
    else {
        return Vec::new();
    };
    let decl = line.trim_start();
    let decl = decl.strip_prefix("export ").unwrap_or(decl);

    if !decl.starts_with("attribute ") {
        return Vec::new();
    }

    // `attribute name(params) on targets as`: the target list follows
    // the parameters, so the parentheses of the head are the ones
    // before ` on `.
    let head = decl.split(" on ").next().unwrap_or(decl);
    let (Some(open), Some(close)) = (head.find('('), head.rfind(')')) else {
        return Vec::new();
    };

    if close < open {
        return Vec::new();
    }

    let params = &head[open + 1..close];
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut entries: Vec<&str> = Vec::new();

    for (i, c) in params.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,

            ')' | ']' | '}' => depth -= 1,

            ',' if depth == 0 => {
                entries.push(&params[start..i]);
                start = i + 1;
            }

            _ => {}
        }
    }

    entries.push(&params[start..]);
    let mut out = Vec::new();

    for entry in entries {
        let (name, ty) = match entry.split_once(':') {
            Some((n, t)) => (n.trim(), Some(t.trim())),

            None => (entry.trim(), None),
        };

        if name.is_empty() || !name.chars().all(is_word) {
            continue;
        }

        if ty.is_none_or(is_a_list) {
            out.push(name.to_string());
        }
    }

    out
}

/// Whether a type text spells a list: `string[]`, `Array<T>`, or a
/// table type with no key. This is the reading
/// `alloy::desugar::contracts` checks an `each` against, so the two
/// stay in step.
fn is_a_list(ty: &str) -> bool {
    let ty = ty.trim();

    ty.ends_with("[]")
        || ty.starts_with("Array<")
        || (ty.starts_with('{') && ty.ends_with('}') && !ty.contains(':'))
}

/// Whether the cursor sits inside the parentheses of a variant, on a
/// line of an `enum` body: `Move(num|`. The head is the line up to the
/// word being typed.
pub(crate) fn in_enum_payload(src: &str, line_start: usize, head: &str) -> bool {
    // `Name(` with the parenthesis still open, attributes aside.
    let t = head.trim_start();
    let mut rest = t;

    while let Some(after) = rest.strip_prefix('@') {
        let end = after
            .find(|c: char| {
                !(is_word(c) || c == '(' || c == ')' || c == ',' || c == ' ' || c == '"')
            })
            .unwrap_or(after.len());
        rest = after[end..].trim_start();

        if rest == t {
            break;
        }
    }

    let name_len = rest.chars().take_while(|c| is_word(*c)).count();
    let after_name = rest[name_len..].trim_start();

    if name_len == 0 || !after_name.starts_with('(') {
        return false;
    }

    let opens = after_name.matches('(').count();
    let closes = after_name.matches(')').count();

    if opens <= closes {
        return false;
    }

    // The nearest declaration above is an `enum` that is still open.
    for line in src[..line_start].lines().rev() {
        let l = line.trim_start();
        let l = l.strip_prefix("export ").unwrap_or(l);

        if l.starts_with("enum ") {
            return true;
        }

        if l == "end"
            || l.starts_with("struct ")
            || l.starts_with("impl ")
            || l.starts_with("trait ")
            || l.starts_with("interface ")
            || l.starts_with("function ")
            || l.starts_with("local ")
        {
            return false;
        }
    }

    false
}

/// The text after a declaration's name and its `<...>` parameters.
fn declaration_name(rest: &str) -> Option<&str> {
    let rest = rest.trim_start();
    let name_len = rest
        .chars()
        .take_while(|c| is_word(*c) || *c == '.')
        .count();

    if name_len == 0 {
        return None;
    }

    let mut after = &rest[name_len..];

    if after.starts_with('<') {
        let close = after.find('>')?;
        after = &after[close + 1..];
    }

    Some(after)
}

/// `struct Name `, `enum Name<T> `, `export interface Name `: the head
/// of a declaration whose body opener comes next. `Some(true)` for an
/// interface, which may take `extends` first.
pub(crate) fn declaration_head(head: &str) -> Option<bool> {
    let t = head.trim_start();
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let (rest, interface, is_impl) = if let Some(r) = t.strip_prefix("struct ") {
        (r, false, false)
    } else if let Some(r) = t.strip_prefix("enum ") {
        (r, false, false)
    } else if let Some(r) = t.strip_prefix("trait ") {
        (r, false, false)
    } else if let Some(r) = t.strip_prefix("impl ") {
        (r, false, true)
    } else {
        (t.strip_prefix("interface ")?, true, false)
    };
    let mut after = declaration_name(rest)?;

    // `impl Trait for Type as`: the target closes the header.
    if is_impl && let Some(target) = after.trim_start().strip_prefix("for ") {
        after = declaration_name(target)?;
    }

    (after.ends_with([' ', '\t']) && after.trim().is_empty()).then_some(interface)
}

/// The declaration keyword a line starts with, `export` and the
/// visibility word of a namespace member aside.
fn declaration_word(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    let t = t
        .strip_prefix("public ")
        .or_else(|| t.strip_prefix("private "))
        .map(str::trim_start)
        .unwrap_or(t);
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let binds = t.starts_with("local ") || t.starts_with("const ");
    let t = t
        .strip_prefix("local ")
        .or_else(|| t.strip_prefix("const "))
        .map(str::trim_start)
        .unwrap_or(t);
    let t = t.strip_prefix("async ").map(str::trim_start).unwrap_or(t);

    for (word, target) in [
        ("function ", "function"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("remote ", "remote"),
        ("interface ", "interface"),
        ("type ", "type"),
        ("namespace ", "namespace"),
        ("impl ", "impl"),
        ("trait ", "trait"),
        ("local ", "local"),
        ("const ", "local"),
    ] {
        if t.starts_with(word) {
            return Some(target);
        }
    }

    // `local x = 1` and `const N = 2` bind a name, which is where
    // `@cfg` goes.
    binds.then_some("local")
}

/// The level of a long bracket at the start of the text: 0 for `[[`,
/// 2 for `[==[`. None when the text opens no long bracket.
fn long_bracket(text: &str) -> Option<usize> {
    let rest = text.strip_prefix('[')?;
    let level = rest.chars().take_while(|c| *c == '=').count();

    rest[level..].starts_with('[').then_some(level)
}

/// The declaration the attribute lines lead to: the first statement
/// past the blank lines, the other attributes, and the comments. Both
/// comment forms sit between an attribute and what it marks.
fn declaration_below(src: &str) -> Option<&'static str> {
    let mut rest = src;

    loop {
        let text = rest.trim_start();

        if text.is_empty() {
            return None;
        }

        let line_end = text.find('\n').unwrap_or(text.len());

        if let Some(after) = text.strip_prefix("--") {
            // A block comment runs to its closing bracket, over as many
            // lines as it takes.
            if let Some(level) = long_bracket(after) {
                let closer = format!("]{}]", "=".repeat(level));
                let at = text.find(&closer)?;
                rest = &text[at + closer.len()..];

                continue;
            }

            rest = &text[line_end..];

            continue;
        }

        // Another attribute of the same declaration.
        if text.starts_with('@') {
            rest = &text[line_end..];

            continue;
        }

        return declaration_word(&text[..line_end]);
    }
}

/// What an attribute at this position would go on: a remote's parameter
/// inside its parentheses, a field or a variant inside a struct or an
/// enum, or the declaration the next non-attribute line starts.
pub(crate) fn attribute_target(
    src: &str,
    line_start: usize,
    line_end: usize,
    head: &str,
) -> (Option<&'static str>, bool) {
    let opens = head.matches('(').count();
    let closes = head.matches(')').count();

    if opens > closes {
        return if head.trim_start().starts_with("remote ")
            || head.trim_start().starts_with("export remote ")
        {
            (Some("param"), false)
        } else {
            (None, false)
        };
    }

    // An indented line sits in a body: the first line above with less
    // indentation opens it. The depth is free, so a struct inside a
    // namespace reads the same as one at the margin.
    let indent = head.len() - head.trim_start().len();

    if indent > 0 {
        let opener = src[..line_start]
            .lines()
            .filter(|l| !l.trim().is_empty())
            .rev()
            .find(|l| l.len() - l.trim_start().len() < indent);

        match opener.and_then(declaration_word) {
            Some("struct") | Some("interface") => return (Some("field"), false),
            Some("enum") => return (Some("variant"), false),
            // A member of an `impl` or a `trait` is a method. Every
            // attribute of a function reads on one, `@test` apart:
            // it registers a name with the runner, and a method
            // takes a receiver no runner can give it.
            Some("impl") | Some("trait") => return (Some("method"), false),
            // A namespace body holds declarations, as the margin does,
            // so the line below names the target.
            _ => {}
        }
    }

    // The attribute precedes a declaration: past the other attribute
    // lines, the blanks, and the comments. Nothing there is no
    // declaration yet, which is a state of its own: only an attribute
    // that goes anywhere reads over blank lines.
    match declaration_below(&src[line_end.min(src.len())..]) {
        Some(word) => (Some(word), false),

        None => (None, true),
    }
}

/// The type the `impl` block around the caret is for: the `X` of
/// `impl X` and of `impl Trait for X`. The first line at the margin
/// above the caret decides.
pub fn impl_target(src: &str, offset: usize) -> Option<String> {
    let line = src[..offset.min(src.len())]
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))?;
    // A foreign impl is exported, so it works project wide.
    let head = line.trim();
    let head = head.strip_prefix("export ").unwrap_or(head);
    let head = head.strip_prefix("global ").unwrap_or(head);
    let rest = head.strip_prefix("impl ")?;
    let target = rest.rsplit(" for ").next().unwrap_or(rest).trim();
    // A namespace member keeps its path: `impl Zoo.Lion`.
    let name: String = target
        .chars()
        .take_while(|c| is_word(*c) || *c == '.')
        .collect();
    let name = name.trim_end_matches('.').to_string();

    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_takes_the_type_the_impl_is_for() {
        let one = "impl Msg as\n    function tag(self)\n        match self with\n";
        assert_eq!(impl_target(one, one.len()), Some("Msg".to_string()));

        let ns = "impl Zoo.Lion as\n    function roar(self)\n";
        assert_eq!(impl_target(ns, ns.len()), Some("Zoo.Lion".to_string()));

        let two = "impl Shape for Circle as\n    function area(self)\n";
        assert_eq!(impl_target(two, two.len()), Some("Circle".to_string()));

        // An exported impl names its type the same way.
        let out = "export impl Point as\n    function length(self)\n        return self.\n";
        assert_eq!(impl_target(out, out.len()), Some("Point".to_string()));

        let none = "local function f()\n    match self with\n";
        assert_eq!(impl_target(none, none.len()), None);
    }
}
