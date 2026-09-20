//! An Array, in every form the checker prints one: `T[]` sugar over
//! `Array<T>`, a receiver's table form, an `Iter` two maps deep, and a
//! `where` clause the child cut short but that still names one.

use super::parse_bindings;
use super::strings::{balanced_len, enclosing_open, group_len, head_of, outside_angles, type_len};

/// The arity report names a declaration, not a value: `Array<T>` is the
/// alias the source has to give one argument to, and the `T[]` sugar
/// reads there as the type of a value instead.
pub(crate) fn fold_generic_arity(text: &mut String) {
    if text.contains("Generic type '") && text.contains("type argument") {
        *text = text.replace("Generic type 'T[]'", "Generic type 'Array<T>'");
    }
}

/// An `Iter` two maps deep prints as its shape, `{ next: (self: any) ->
/// T? }`, since the std spells it under a second name. It reads as
/// `Iter<T>`.
pub(crate) fn fold_iter_shapes(text: &mut String) {
    const HEAD: &str = "{ next: (self: any) -> ";
    let mut from = 0;

    while let Some(i) = text[from..].find(HEAD) {
        let at = from + i;
        let Some(len) = group_len(&text[at..], '{', '}') else {
            break;
        };
        let inner = &text[at + HEAD.len()..at + len - 1];
        let Some(element) = inner.trim_end().strip_suffix('?') else {
            from = at + 1;

            continue;
        };
        // The shape holds `next` alone when the child cut the print; a
        // second member means another table with a `next` field.
        if element.contains(',') || element.contains('(') {
            from = at + 1;

            continue;
        }

        let name = format!("Iter<{}>", element.trim());
        text.replace_range(at..at + len, &name);
        from = at + name.len();
    }
}

/// `(number[])[]`, a union that folded to one member under an array,
/// reads as `number[][]`.
pub(crate) fn fold_array_parens(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find(")[]") {
        let close = from + i;
        let Some(open) = enclosing_open(text, close) else {
            from = close + 3;
            continue;
        };
        let inner = &text[open + 1..close];

        if inner.contains(" | ")
            || inner.contains(" & ")
            || inner.contains("->")
            || inner.contains(' ')
        {
            from = close + 3;
            continue;
        }

        text.replace_range(close..close + 1, "");
        text.replace_range(open..open + 1, "");
        from = close - 1;
    }
}

/// An Array method's receiver, `{ read [number]: T }`, prints as
/// `{read T}`; the sugar is `read T[]`.
pub(crate) fn fold_read_arrays(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find("{read ") {
        let at = from + i;
        let Some(len) = balanced_len(&text[at..]) else {
            break;
        };
        let inner = text[at + 6..at + len - 1].trim().to_string();

        // One element type, `Result<Profile, any>` included; a space
        // outside the type arguments means the group holds more.
        if inner.contains('{') || outside_angles(&inner, ' ') {
            from = at + 6;
            continue;
        }

        text.replace_range(at..at + len, &format!("read {inner}[]"));
        from = at;
    }
}

/// A `where` clause the child cut short still names an array: the head
/// is a solver variable whose binding opens with `[number]: T` and an
/// Array method. The pair reads as `T[]`, wherever the cut landed.
pub(crate) fn fold_cut_array(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find(" where ") {
        let at = from + i;
        let (head_start, head) = head_of(text, at);
        let Some((var_start, var, optional)) = solver_head(head_start, head) else {
            from = at + 1;
            continue;
        };
        // The head starts after the quote that opens it in a report.
        let quoted = text[..head_start].ends_with('\'');
        let clause_start = at + " where ".len();
        let Some(mut name) = cut_name(&text[clause_start..], &var) else {
            from = at + 1;
            continue;
        };
        let end = clause_end(text, clause_start);

        if optional {
            name = format!("{name}?");
        }

        // The cut may have taken the closing quote with it; the
        // replacement writes the pair.
        if quoted && !text[end..].starts_with('\'') {
            name.push('\'');
        }

        text.replace_range(var_start..end, &name);
        from = var_start + name.len();
    }
}

/// A `where` clause head that is one solver variable: its offset, the
/// variable, and whether it is optional. `None` when the head is
/// anything else.
fn solver_head(head_start: usize, head: &str) -> Option<(usize, String, bool)> {
    let lead = head.len() - head.trim_start().len();
    let at = head_start + lead;
    let var = head.trim();
    let optional = var.ends_with('?');
    let var = var.trim_end_matches('?');

    if var.len() < 2 || !var.starts_with('t') || !var[1..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    Some((at, var.to_string(), optional))
}

/// The end of a `where` clause: the parsed length when the bindings are
/// whole, else the end of the fenced block or of the text, since a cut
/// clause runs to the end of what the child sent.
fn clause_end(text: &str, clause_start: usize) -> usize {
    let (bindings, len) = parse_bindings(&text[clause_start..]);

    if !bindings.is_empty() {
        return clause_start + len;
    }

    text[clause_start..]
        .find("\n```")
        .map_or(text.len(), |k| clause_start + k)
}

/// The name the binding `var` stands for, read from the head of its
/// body so a cut clause still answers: an Array by its indexer and one
/// method, a struct by the return of the `__new` its metatable carries.
/// A binding that only aliases another variable is followed.
fn cut_name(clause: &str, var: &str) -> Option<String> {
    let mut var = var.to_string();

    for _ in 0..4 {
        // The cut may have taken the head's own binding; one Array in
        // the clause is still what the head stands for.
        let body = match binding_body(clause, &var) {
            Some(body) => body,

            None => lone_array_binding(clause)?,
        };
        let trimmed = body.trim_start();

        if let Some(inner) = trimmed.strip_prefix('{') {
            let inner = inner.trim_start();

            if let Some(rest) = inner.strip_prefix("@metatable ") {
                return constructed_name(rest);
            }

            let rest = inner.strip_prefix("[number]: ")?;
            let len = type_len(rest);
            let elem = rest[..len].trim().to_string();
            let next = rest[len..].strip_prefix(',')?.trim_start();
            let key = &next[..next.find(':')?];

            if !matches!(
                key,
                "concat"
                    | "contains"
                    | "filter"
                    | "find"
                    | "for_each"
                    | "is_empty"
                    | "join"
                    | "len"
                    | "map"
                    | "pop"
                    | "push"
            ) {
                return None;
            }

            let compound = elem.contains(" | ")
                || elem.contains(" & ")
                || elem.contains("->")
                || elem.ends_with('?');

            return Some(match compound {
                true => format!("({elem})[]"),

                false => format!("{elem}[]"),
            });
        }

        // `t2 = t1`: the head stands for another binding.
        let next: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();

        if next.len() < 2 || !next.starts_with('t') || next == var {
            return None;
        }

        var = next;
    }

    None
}

/// The one binding of a clause that opens an Array, when exactly one
/// does. A cut clause names the head's binding nowhere else.
fn lone_array_binding(clause: &str) -> Option<&str> {
    let mut found = None;

    for (i, _) in clause.match_indices("= { [number]: ") {
        if found.is_some() {
            return None;
        }

        found = Some(&clause[i + 2..]);
    }

    found
}

/// The struct a metatable builds: `__new: ({ ... }) -> Node` names it.
/// The body may be cut after the constructor; that much is enough.
fn constructed_name(body: &str) -> Option<String> {
    let at = body.find("__new: ").or_else(|| body.find("new: "))?;
    let rest = &body[at..];
    let arrow = rest.find(") -> ")? + ") -> ".len();
    let tail = &rest[arrow..];
    let name = tail[..type_len(tail)].trim().to_string();

    (!name.is_empty() && name.chars().next().is_some_and(char::is_alphabetic)).then_some(name)
}

/// The body of the binding `var` names, from its `= ` to the end of the
/// clause. `None` when the clause binds no such variable.
fn binding_body<'a>(clause: &'a str, var: &str) -> Option<&'a str> {
    let needle = format!("{var} = ");
    let mut from = 0;

    while let Some(i) = clause[from..].find(&needle) {
        let at = from + i;
        let starts = at == 0 || clause[..at].ends_with(" ; ");

        if starts {
            return Some(&clause[at + needle.len()..]);
        }

        from = at + 1;
    }

    None
}

/// `Array<T>` reads as the sugar the source has, `T[]`, when `T` is a
/// name, a dotted path, or an array of one, `Array<number[]>`; a
/// compound argument keeps the alias.
pub(crate) fn fold_array_alias(text: &mut String) {
    // The inner alias of `Array<Array<number>>` folds on the first
    // pass and the outer on the next.
    for _ in 0..4 {
        let before = text.len();
        fold_array_alias_once(text);

        if text.len() == before {
            break;
        }
    }
}

fn fold_array_alias_once(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find("Array<") {
        let start = from + i;
        let inner_start = start + "Array<".len();
        // Not the tail of another name, `ReadArray<`.
        let prefixed = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');

        // The element carries its own arguments, `Result<Profile, any>`;
        // a union, an intersection, or an arrow under the array keeps
        // the form it came with, since `A | B[]` and `(A) -> B[]` read
        // the other way.
        match group_len(&text[inner_start - 1..], '<', '>') {
            Some(len)
                if !prefixed && {
                    let elem = &text[inner_start..inner_start + len - 2];

                    !elem.is_empty()
                        && !elem.contains('{')
                        && !outside_angles(elem, '|')
                        && !outside_angles(elem, '&')
                        && !outside_angles(elem, ',')
                        && !outside_angles(elem, '-')
                } =>
            {
                let elem = text[inner_start..inner_start + len - 2].to_string();
                text.replace_range(start..inner_start + len - 1, &format!("{elem}[]"));
                from = start + elem.len() + 2;
            }

            _ => from = inner_start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Known, fold};
    use super::*;

    #[test]
    fn the_arity_report_names_the_alias() {
        let known = Known::default();

        assert_eq!(
            fold(
                "Generic type 'Array<T>' expects 1 type argument, but 2 are specified",
                &known
            ),
            "Generic type 'Array<T>' expects 1 type argument, but 2 are specified"
        );
        // The editor keeps the checker's kind in front of the sentence.
        assert_eq!(
            fold(
                "TypeError: Generic type 'Array<T>' expects 1 type argument, but 2 are specified",
                &known
            ),
            "TypeError: Generic type 'Array<T>' expects 1 type argument, but 2 are specified"
        );
        // A value's type still reads as the sugar the source writes.
        assert_eq!(
            fold("Expected this to be 'Array<number>'", &known),
            "Expected this to be 'number[]'"
        );
    }

    #[test]
    fn an_array_alias_of_an_array_folds_to_the_sugar() {
        let mut text = ": Array<number[]>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": number[][]");
        let mut text = ": Array<Array<number>>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": number[][]");
        assert_eq!(
            fold(": Array<Array<number> | Array<number>>", &Known::default()),
            ": number[][]"
        );
        let mut text = ": Array<number | string>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": Array<number | string>");
    }

    #[test]
    fn an_arrow_under_the_array_keeps_the_alias() {
        // `() -> ()[]` reads as a function that gives an array, and
        // the paren fold then takes the empty pack: `() -> []`.
        let mut text = ": Array<() -> ()>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": Array<() -> ()>");
        let mut text = ": Array<(number) -> string>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": Array<(number) -> string>");
        // An arrow inside the element's own arguments closes nothing.
        let mut text = ": Array<HashMap<string, () -> ()>>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": HashMap<string, () -> ()>[]");
    }

    #[test]
    fn a_cut_array_clause_reads_by_its_element() {
        let text = "Expected this to be\n\t'string[]'\nbut got\n\t't1 where t1 = { [number]: number, concat: (read number[], t1) -> t1, contains: (read number[], number) -> b";
        assert_eq!(
            fold(text, &Known::default()),
            "Expected this to be\n\t'string[]'\nbut got\n\t'number[]'"
        );
    }

    #[test]
    fn a_whole_clause_in_a_diagnostic_keeps_the_closing_quote() {
        let text = "Expected this to be 'string[]' but got 't2 where t1 = { [number]: number, concat: (read number[], t1) -> t1, push: (read number[], ...number) -> () } ; t2 = t1'";
        assert_eq!(
            fold(text, &Known::default()),
            "Expected this to be 'string[]' but got 'number[]'"
        );
    }

    #[test]
    fn a_clause_over_several_lines_folds_too() {
        let text = "```luau\nlocal rs: t1 where t1 = {\n    [number]: Result<number, any>,\n    concat: (self: {read Result<number, any>}, other: t1) -> t1,\n    len: (self: {read ResultErr<number, any>... *TRUNCATED*\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nlocal rs: Result<number, any>[]\n```"
        );
    }

    #[test]
    fn an_optional_head_keeps_its_question_mark() {
        let text =
            "local xs: t1? where t1 = { [number]: string, push: (self: t1, value: string) -> ()";
        assert_eq!(fold(text, &Known::default()), "local xs: string[]?");
    }
}
