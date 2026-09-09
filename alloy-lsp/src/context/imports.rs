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
