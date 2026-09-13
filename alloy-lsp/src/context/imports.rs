//! The module path of an `import`, a `require`, or a dynamic `import`
//! call: where its string starts and what it holds so far.

use super::Context;

/// The string a module path is being typed in, when the cursor is inside
/// one after `from`, `require(`, or `import(`.
pub(crate) fn import_spec(src: &str, line_start: usize, offset: usize) -> Option<Context> {
    let before = &src[line_start..offset];
    let quote = before.rfind(['"', '\''])?;
    let text = &before[quote + 1..];

    // The cursor right after a closed path string: nothing to offer.
    if text.is_empty() {
        let quotes: Vec<usize> = before.match_indices(['"', '\'']).map(|(i, _)| i).collect();

        if quotes.len() >= 2 && quotes.len() % 2 == 0 {
            let open = quotes[quotes.len() - 2];
            let head = before[..open].trim_end();

            if head.ends_with("from") || head.ends_with("require(") || head.ends_with("import(") {
                return Some(Context::Nothing);
            }
        }
    }

    if text.contains(['"', '\'']) {
        return None;
    }

    let head = before[..quote].trim_end();

    if !(head.ends_with("from") || head.ends_with("require(") || head.ends_with("import(")) {
        return None;
    }

    Some(Context::ImportSpec {
        text: text.to_string(),
        start: line_start + quote + 1,
    })
}

/// The path an import line names, in either quote.
pub(crate) fn import_path(line: &str) -> Option<String> {
    let i = line.find("from")?;
    let rest = &line[i..];
    let q = rest.find(['"', '\''])?;
    let quote = rest.as_bytes()[q] as char;
    let inner = &rest[q + 1..];
    let end = inner.find(quote)?;

    Some(inner[..end].to_string())
}

/*
The statement an `import` at or above the cursor opens, as a byte range.

A name list spans lines, so the context of a name cannot come from the
cursor's line alone. The walk takes the nearest `import` line at or
above the cursor and keeps it while the statement is still open: a brace
that never closed, or no `from` yet. A blank line ends the walk, since a
list does not carry one.
*/
fn statement_at(src: &str, line_start: usize, offset: usize) -> Option<usize> {
    /// The offset of the `import` keyword a line opens with.
    fn opens_import(src: &str, at: usize) -> Option<usize> {
        let line = src[at..].split('\n').next().unwrap_or("");
        let indent = line.len() - line.trim_start().len();
        let rest = line[indent..].strip_prefix("import")?;

        match rest.chars().next() {
            Some(c) if super::strings::is_word(c) => None,

            // `import("./m")` is an expression, not a statement; the
            // child answers a position inside one.
            Some('(') => None,

            _ => Some(at + indent),
        }
    }

    if let Some(start) = opens_import(src, line_start) {
        return Some(start);
    }

    let mut at = line_start;

    for _ in 0..32 {
        at = src[..at.saturating_sub(1)].rfind('\n').map_or(0, |i| i + 1);

        let line = src[at..].split('\n').next().unwrap_or("");

        if line.trim().is_empty() {
            return None;
        }

        if let Some(start) = opens_import(src, at) {
            let text = &src[start..offset];
            let open = text.matches('{').count() > text.matches('}').count();

            if !open && text.contains("from") {
                return None;
            }

            return continues_list(src, start, line_start, offset).then_some(start);
        }

        if at == 0 {
            return None;
        }
    }

    None
}

/*
Whether every line under the `import` keyword reads as part of its name
list, so the cursor is inside the statement and not under a list that
was left open.

A list entry is indented past the `import` and holds only what an entry
holds: a name, a dot, a comma, `type`, `as`, and the `@` of an
attribute. A closing brace opens its own line at any column. A line that
fails both is the next statement, and a file with an unfinished import
above it still completes.
*/
fn continues_list(src: &str, start: usize, line_start: usize, offset: usize) -> bool {
    let column = start - src[..start].rfind('\n').map_or(0, |i| i + 1);
    let mut at = src[..start].rfind('\n').map_or(start, |i| i + 1);

    while at <= line_start {
        let line = src[at..].split('\n').next().unwrap_or("");
        let end = (at + line.len()).min(offset);
        let text = match at > start {
            true => &src[at..end.max(at)],

            // The `import` line itself carries the keyword and the
            // brace, which no entry rule covers.
            false => "",
        };
        let trimmed = text.trim_start();
        let indented = text.len() - trimmed.len() > column || trimmed.is_empty();
        let entry_only = trimmed.starts_with('}')
            || trimmed.starts_with("--")
            || trimmed
                .chars()
                .all(|c| super::strings::is_word(c) || " \t,.@}".contains(c));

        if !(entry_only && (indented || trimmed.starts_with('}'))) {
            return false;
        }

        let Some(next) = src[at..].find('\n') else {
            break;
        };

        at += next + 1;
    }

    true
}

/// Whether the entry at the cursor has passed its `as`, so the name
/// being typed is the local one the reader invents.
fn names_an_alias(entry: &str) -> bool {
    entry.split_whitespace().any(|w| w == "as")
}

/*
What an `import` statement takes at the cursor: the head, a name in the
list, the `*`, the braces after a default binding, or the `from`.

Every position inside an `import` answers here, and the last rule
answers `Nothing` rather than nothing at all. A position the proxy left
unanswered would go to the child, which lists the whole global scope
where the reader is picking a name out of one module.
*/
pub(crate) fn import_context(src: &str, line_start: usize, offset: usize) -> Option<Context> {
    let start = statement_at(src, line_start, offset)?;
    let before = &src[start..offset];
    let prefix = super::trailing_word(before);
    let head = &before[..before.len() - prefix.len()];

    // The caret sits inside the word `import` itself: the keyword is
    // what the reader is typing, and the child lists those.
    if head.len() < "import".len() {
        return None;
    }

    // The list writes an attribute with the `@` it is applied with, so
    // the sigil is part of the word the reader is typing.
    let sigil = head.ends_with('@');
    let head = match sigil {
        true => &head[..head.len() - 1],

        false => head,
    };
    let written = match sigil {
        true => format!("@{prefix}"),

        false => prefix.to_string(),
    };
    let rest = head["import".len()..].trim_start();
    let type_only = rest.starts_with("type ") || rest == "type";
    let rest = rest
        .strip_prefix("type")
        .map(str::trim_start)
        .unwrap_or(rest);
    // The path the statement names, from the whole statement: a
    // multi-line list carries it on the line after the cursor.
    let spec = || import_path(&src[start..statement_end(src, offset)]);

    if rest.is_empty() {
        return Some(Context::ImportHead {
            prefix: written,
            type_only,
            spec: spec(),
        });
    }

    if let Some(open) = rest.find('{') {
        if let Some(close) = rest[open..].find('}') {
            let after = rest[open + close + 1..].trim_start();

            // `import { a } |` wants `from`. Once `from` is written the
            // path is next, and a path is a string the reader types.
            return Some(match after.is_empty() {
                true => Context::ImportFrom,

                false => Context::Nothing,
            });
        }

        let inside = &rest[open + 1..];
        // The entry the caret sits in: `type` opens a type-only name, so
        // the list holds the module's types alone.
        let entry = inside.rsplit(',').next().unwrap_or(inside);

        if names_an_alias(entry) {
            return Some(Context::Nothing);
        }

        // The entry may hold the `@` further back than the caret, as in
        // `import { @ |`.
        let sigil = sigil || entry.trim_start().starts_with('@');

        let entry_type_only = entry.trim_start().starts_with("type ") || entry.trim() == "type";
        let after_name = !entry_type_only
            && entry
                .trim_end()
                .chars()
                .last()
                .is_some_and(super::strings::is_word)
            && entry.ends_with(' ')
            && prefix.is_empty();

        return Some(Context::ImportNames {
            prefix: written,
            type_only: type_only || entry_type_only,
            spec: spec(),
            after_name,
            sigil,
        });
    }

    if let Some(after_star) = rest.strip_prefix('*') {
        let after_star = after_star.trim_start();

        if after_star.is_empty() {
            return Some(Context::ImportStar);
        }

        let Some(after_as) = after_star.strip_prefix("as") else {
            return Some(Context::Nothing);
        };
        let after_as = after_as.trim_start();

        // `import * as |`: the name for the module is the reader's own.
        if after_as.is_empty() {
            return Some(Context::Nothing);
        }

        // `import * as M, |`: the braces follow, the way they follow a
        // default binding.
        if after_as.trim_end().ends_with(',') {
            return Some(Context::ImportBrace);
        }

        return Some(
            match after_as.split_whitespace().count() == 1 && after_as.ends_with(' ') {
                true => Context::ImportFrom,

                false => Context::Nothing,
            },
        );
    }

    // `import Name, |`: the braces follow the default binding.
    if rest.trim_end().ends_with(',') {
        return Some(Context::ImportBrace);
    }

    // `import Name |`: a default import wants `from`.
    if rest.split_whitespace().count() == 1 && rest.ends_with(' ') {
        return Some(Context::ImportFrom);
    }

    Some(Context::Nothing)
}

/// The end of the statement the cursor sits in: the line that closes an
/// open name list, or the cursor's own line.
fn statement_end(src: &str, offset: usize) -> usize {
    let line_end = |from: usize| {
        src[from..]
            .find('\n')
            .map(|i| from + i)
            .unwrap_or(src.len())
    };

    match src[offset..].find('}') {
        Some(i) => line_end(offset + i),

        None => line_end(offset),
    }
}
