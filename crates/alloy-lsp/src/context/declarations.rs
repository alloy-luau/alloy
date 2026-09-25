//! What the declaration of a name says about its type: the annotation
//! it carries, or the expression it starts from.

use super::Declared;
use super::strings::{is_word, type_text};

/// Whether the text before a name binds it: a `local` or a `const`, or
/// the comma of a list either one opened.
fn binds(before: &str) -> bool {
    let t = before.trim_end();

    for word in ["local", "const"] {
        if let Some(head) = t.strip_suffix(word) {
            return !head.ends_with(is_word);
        }
    }

    t.ends_with(',') && {
        let head = t.trim_start();

        head.starts_with("local ") || head.starts_with("const ")
    }
}

/// Whether a name sits in the parameter list of a `function` head.
fn in_parameters(before: &str) -> bool {
    before.contains("function") && before.matches('(').count() > before.matches(')').count()
}

fn declared_in_line(line: &str, name: &str) -> Option<Declared> {
    let mut from = 0;

    while let Some(i) = line[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        from = start + 1;
        let before = &line[..start];

        if before.chars().next_back().is_some_and(is_word)
            || line[end..].chars().next().is_some_and(is_word)
            // A field or a member of something else.
            || before.trim_end().ends_with(['.', ':'])
        {
            continue;
        }

        let after = line[end..].trim_start();

        if let Some(rest) = after.strip_prefix(':')
            && !rest.starts_with(':')
            && (binds(before) || in_parameters(before))
        {
            return Some(Declared::Annotation(type_text(rest)));
        }

        if let Some(rest) = after.strip_prefix('=')
            && !rest.starts_with('=')
            && binds(before)
        {
            return Some(Declared::Init(rest.trim().to_string()));
        }
    }

    None
}

/// What the nearest declaration of `name` above the caret says. A use
/// of the name and a member access are not declarations.
pub fn declared(src: &str, offset: usize, name: &str) -> Option<Declared> {
    declared_at(src, offset, name).map(|(_, d)| d)
}

/// `declared` with the byte the declaring line starts at.
pub fn declared_at(src: &str, offset: usize, name: &str) -> Option<(usize, Declared)> {
    if name.is_empty() {
        return None;
    }

    let head = &src[..offset.min(src.len())];

    head.lines().rev().find_map(|line| {
        let start = line.as_ptr() as usize - head.as_ptr() as usize;

        declared_in_line(line, name).map(|d| (start, d))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_gives_its_annotation_or_its_first_value() {
        let src = concat!(
            "local function handle(msg: Msg, tries: number)\n",
            "    local parsed: Result<number, string> = Ok(1)\n",
            "    const start = Msg.Join(p)\n",
            "    local names: string[] = {}\n",
            "    local t = start\n"
        );
        let end = src.len();
        let ann = |n: &str| declared(src, end, n);

        assert_eq!(ann("msg"), Some(Declared::Annotation("Msg".to_string())));
        assert_eq!(
            ann("tries"),
            Some(Declared::Annotation("number".to_string()))
        );
        assert_eq!(
            ann("parsed"),
            Some(Declared::Annotation("Result<number, string>".to_string()))
        );
        assert_eq!(
            ann("start"),
            Some(Declared::Init("Msg.Join(p)".to_string()))
        );
        assert_eq!(
            ann("names"),
            Some(Declared::Annotation("string[]".to_string()))
        );
        assert_eq!(ann("t"), Some(Declared::Init("start".to_string())));
        assert_eq!(ann("p"), None);
    }
}
