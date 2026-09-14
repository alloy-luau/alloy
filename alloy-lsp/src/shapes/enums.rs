//! An enum, printed as the union of its variants: a unit as its quoted
//! name, a payload as a tagged table. Both read back as the enum, and a
//! tagged table the union fold could not pair still names its variant.

use alloy::declarations::Shape;

use super::Known;
use super::strings::{balanced_len, enclosing_brace, group_start, member_len, members};

/// The members a variant table prints, by key.
type Members = Vec<(String, String)>;

/// A printed variant beside the payload types its declaration names.
type Printed<'a> = (&'a [String], &'a Members);

/// The slot a payload key names: `_1` is the first.
fn slot_index(key: &str) -> Option<usize> {
    let digits = key.strip_prefix('_')?;

    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// A variant table the union fold could not pair with its siblings
/// still names one variant; the enum is what the reader wrote. A
/// generic enum's alias is a plain union, so a unit prints as a table
/// with its tag alone beside the methods, `{ read map: t1, read tag:
/// "Nil" }`, and a payload carries the argument in its slot.
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

        if let Some(name) = variant_table_name(&body, known) {
            text.replace_range(open..open + len, &name);
            from = open + name.len();

            continue;
        }

        from = at + 1;
    }
}

/// The enum a lone variant table belongs to, with the arguments its
/// slots carry: one slot per payload type, and any other member is
/// a method of the enum.
fn variant_table_name(body: &str, known: &Known) -> Option<String> {
    let m = members(body);
    let tag = tag_of(&m)?;

    known.shapes.iter().find_map(|s| {
        let Shape::Enum {
            name,
            generics,
            variants,
        } = s
        else {
            return None;
        };
        let (_, payload) = variants.iter().find(|(v, _)| *v == tag)?;

        (payload.len() == slots(&m)).then(|| with_arguments(name, generics, &[(payload, &m)]))
    })
}

/// The variant a table's `tag` literal names.
fn tag_of(m: &[(String, String)]) -> Option<String> {
    m.iter()
        .find(|(k, _)| k == "tag")
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
}

fn slots(m: &[(String, String)]) -> usize {
    m.iter().filter(|(k, _)| slot_index(k).is_some()).count()
}

/// The enum's name with the arguments its printed variants carry: a
/// payload spelled as a parameter holds that argument in its slot. A
/// payload that only mentions one, `Tree<T>`, binds nothing. The bare
/// name when a parameter has no slot to read.
fn with_arguments(name: &str, generics: &[String], tables: &[Printed]) -> String {
    let args: Vec<String> = generics
        .iter()
        .filter_map(|g| {
            tables.iter().find_map(|(payload, m)| {
                let i = payload.iter().position(|p| p.trim() == g)?;

                m.iter()
                    .find(|(k, _)| slot_index(k) == Some(i + 1))
                    .map(|(_, v)| v.trim().to_string())
            })
        })
        .collect();

    match args.len() == generics.len() && !args.is_empty() {
        true => format!("{name}<{}>", args.join(", ")),

        false => name.to_string(),
    }
}

/// A payload enum whose variants folded to its name still prints its
/// unit variants as strings: `Shape | "Empty"` is `Shape`.
pub(crate) fn fold_enum_unions(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum { name, variants, .. } = shape else {
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
/// `{ _1: T, tag: "V" }` for a payload, and a table with its tag alone
/// for a unit of a generic enum, whose alias is a plain union. The
/// members come in the order of their text, and a payload of another
/// enum may already read by name, so the match is a set: a union whose
/// members are the enum's, in any order, reads as the enum, with the
/// arguments its slots carry.
pub(crate) fn fold_enums(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum {
            name,
            generics,
            variants,
        } = shape
        else {
            continue;
        };

        if variants.len() < 2 {
            continue;
        }

        // A unit prints as its quoted name, and a payload's table
        // carries the name as its tag, so the first variant's name
        // anchors the search either way.
        let quoted = format!("\"{}\"", variants[0].0);
        let mut from = 0;

        while let Some(i) = text[from..].find(&quoted) {
            let at = from + i;
            let anchor = tagged_table_at(text, at).unwrap_or(at);
            let folded = union_around(text, anchor).and_then(|(start, end, found)| {
                enum_of_members(&found, name, generics, variants).map(|n| (start, end, n))
            });

            match folded {
                Some((start, end, folded)) => {
                    text.replace_range(start..end, &folded);
                    from = start + folded.len();
                }

                None => from = at + 1,
            }
        }
    }
}

/// The `{` of the variant table whose `tag` literal sits at `at`.
fn tagged_table_at(text: &str, at: usize) -> Option<usize> {
    let open = enclosing_brace(text, at)?;
    let len = balanced_len(&text[open..])?;
    let m = members(&text[open..open + len]);

    m.iter()
        .any(|(k, v)| k == "tag" && text[at..].starts_with(v.trim()))
        .then_some(open)
}

/// The enum's name when the members of a union are its variants, each
/// as a quoted unit or a tagged table with one slot per payload type.
fn enum_of_members(
    found: &[&str],
    name: &str,
    generics: &[String],
    variants: &[(String, Vec<String>)],
) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut tables: Vec<(&[String], Members)> = Vec::new();

    for f in found {
        let f = f.trim();
        let (v, m) = match f.strip_prefix('"').and_then(|u| u.strip_suffix('"')) {
            Some(unit) => (unit.to_string(), Vec::new()),

            None if f.starts_with('{') => {
                let m = members(f);

                (tag_of(&m)?, m)
            }

            None => return None,
        };
        let (_, payload) = variants.iter().find(|(n, _)| *n == v)?;

        if payload.len() != slots(&m) {
            return None;
        }

        seen.push(v);
        tables.push((payload, m));
    }

    seen.sort();
    seen.dedup();
    let mut all: Vec<String> = variants.iter().map(|(v, _)| v.clone()).collect();
    all.sort();

    if seen != all {
        return None;
    }

    let tables: Vec<Printed> = tables.iter().map(|(p, m)| (*p, m)).collect();

    Some(with_arguments(name, generics, &tables))
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
