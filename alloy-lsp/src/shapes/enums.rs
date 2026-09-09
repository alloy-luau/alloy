//! An enum, printed as the union of its variants: a unit as its quoted
//! name, a payload as a tagged table. Both read back as the enum, and a
//! tagged table the union fold could not pair still names its variant.

use alloy::declarations::Shape;

use super::Known;
use super::naming::{enum_of_variant, is_tagged_variant};
use super::strings::{balanced_len, enclosing_brace, group_start, match_loose, member_len};

/// A tagged table the union fold could not pair with its siblings still
/// names one variant; the enum is what the reader wrote.
pub(crate) fn fold_variant_tables(text: &mut String, known: &Known) {
    let mut from = 0;

    while let Some(i) = text[from..].find("tag: \"") {
        let at = from + i;

        let group = enclosing_brace(text, at)
            .and_then(|open| balanced_len(&text[open..]).map(|len| (open, len)));

        let Some((open, len)) = group else {
            from = at + 1;

            continue;
        };
        let body = text[open..open + len].to_string();

        if is_tagged_variant(&body)
            && let Some(name) = enum_of_variant(&body, known)
        {
            text.replace_range(open..open + len, &name);
            from = open + name.len();

            continue;
        }

        from = at + 1;
    }
}

/// A payload enum whose variants folded to its name still prints its
/// unit variants as strings: `Shape | "Empty"` is `Shape`.
pub(crate) fn fold_enum_unions(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum { name, variants } = shape else {
            continue;
        };
        let units: Vec<String> = variants
            .iter()
            .filter(|(_, p)| p.is_empty())
            .map(|(v, _)| format!("\"{v}\""))
            .collect();

        if units.is_empty() {
            continue;
        }

        let mut from = 0;

        while let Some(at) = word_at_or_after(text, name, from) {
            let mut start = at;
            let mut end = at + name.len();
            let mut grew = false;

            loop {
                let rest = text[end..].trim_start();
                let pad = text[end..].len() - rest.len();

                let Some(after) = rest.strip_prefix("| ") else {
                    break;
                };
                let gap = after.len() - after.trim_start().len();
                let candidate = after.trim_start();

                let Some(unit) = units.iter().find(|u| candidate.starts_with(u.as_str())) else {
                    break;
                };
                end += pad + 2 + gap + unit.len();
                grew = true;
            }

            loop {
                let before = text[..start].trim_end();

                let Some(head) = before.strip_suffix('|') else {
                    break;
                };
                let head = head.trim_end();

                let Some(unit) = units.iter().find(|u| head.ends_with(u.as_str())) else {
                    break;
                };
                start = head.len() - unit.len();
                grew = true;
            }

            match grew {
                true => {
                    text.replace_range(start..end, name);
                    from = start + name.len();
                }

                false => from = at + name.len(),
            }
        }
    }
}

/// The offset of `name` as a whole word at or after `from`.
fn word_at_or_after(text: &str, name: &str, from: usize) -> Option<usize> {
    text[from.min(text.len())..]
        .match_indices(name)
        .map(|(i, _)| from + i)
        .find(|at| {
            let before = text[..*at].chars().next_back();
            let after = text[at + name.len()..].chars().next();

            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// An enum prints as the union of its members: `"Name"` for a unit,
/// `{ _1: T, tag: "V" }` for a payload. The members come in the order
/// of their text, and a payload of another enum may already read by
/// name, so the match is a set: a union whose members are the enum's,
/// in any order, reads as the enum.
pub(crate) fn fold_enums(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum { name, variants } = shape else {
            continue;
        };

        if variants.len() < 2 {
            continue;
        }

        let members: Vec<String> = variants
            .iter()
            .map(|(v, p)| {
                if p.is_empty() {
                    format!("\"{v}\"")
                } else {
                    let fields: Vec<String> = p
                        .iter()
                        .enumerate()
                        .map(|(i, t)| format!("_{}: {t}", i + 1))
                        .collect();

                    format!("{{ {}, tag: \"{v}\" }}", fields.join(", "))
                }
            })
            .collect();
        let first = members[0].split(',').next().unwrap_or("").to_string();
        let mut from = 0;

        while let Some(i) = text[from..].find(&first) {
            let at = from + i;
            let Some((start, end, found)) = union_around(text, at) else {
                from = at + 1;
                continue;
            };
            let all = members.len() == found.len()
                && members.iter().all(|m| {
                    found
                        .iter()
                        .any(|f| match_loose(f, m).is_some_and(|len| len == f.len()))
                });

            if all {
                text.replace_range(start..end, name);
                from = start + name.len();
            } else {
                from = at + 1;
            }
        }
    }
}

/// The union a byte sits in: its start, its end, and its members, with
/// any whitespace around each `|`. A member is a quoted string, a
/// braced table, or a name with its arguments.
fn union_around(text: &str, at: usize) -> Option<(usize, usize, Vec<&str>)> {
    // Back up to the start of the member that holds `at`.
    let mut start = text[..=at].rfind(['{', '"'])?;

    if text[start..].starts_with('"')
        && !text[..start].ends_with([' ', '(', ':', '|', '\n', '<', ','])
    {
        return None;
    }

    // Extend left over `| member` pairs.
    loop {
        let before = text[..start].trim_end();

        if !before.ends_with('|') {
            break;
        }

        let prev_end = before.len() - 1;
        let prev = text[..prev_end].trim_end();
        let prev_start = member_start(text, prev.len())?;
        start = prev_start;
    }

    // Walk right over members and separators.
    let mut members = Vec::new();
    let mut pos = start;

    loop {
        let len = member_len(&text[pos..])?;
        members.push(&text[pos..pos + len]);
        let mut next = pos + len;
        let rest = text[next..].trim_start();

        if !rest.starts_with('|') {
            return Some((start, next, members));
        }

        next += text[next..].len() - rest.len() + 1;
        let gap = text[next..].len() - text[next..].trim_start().len();
        pos = next + gap;
    }
}

/// The start of the union member that ends at `end`.
fn member_start(text: &str, end: usize) -> Option<usize> {
    let last = text[..end].chars().next_back()?;

    match last {
        '}' => group_start(text, end - 1),

        '"' => text[..end - 1].rfind('"'),

        '>' => {
            let mut depth = 0i32;

            for (k, c) in text[..end].char_indices().rev() {
                match c {
                    '>' => depth += 1,
                    '<' => {
                        depth -= 1;

                        if depth == 0 {
                            let head = &text[..k];
                            let name_len = head
                                .chars()
                                .rev()
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                                .map(char::len_utf8)
                                .sum::<usize>();

                            return Some(k - name_len);
                        }
                    }
                    _ => {}
                }
            }

            None
        }

        _ => {
            let name_len = text[..end]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                .map(char::len_utf8)
                .sum::<usize>();

            (name_len > 0).then_some(end - name_len)
        }
    }
}
