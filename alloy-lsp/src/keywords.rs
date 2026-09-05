//! Hover text for the Alloy-only syntax. The child sees the emitted
//! Luau, where a `struct` line or a `??=` no longer exists, so the
//! proxy answers a hover on those bytes itself.

/// The hover for the token at `offset` in the source: its byte range
/// and its Markdown. None when the token has no entry.
pub fn hover(source: &str, offset: usize) -> Option<(usize, usize, &'static str)> {
    let bytes = source.as_bytes();

    if offset >= bytes.len() {
        return None;
    }

    if is_word(bytes[offset]) {
        let (start, end) = word_at(bytes, offset);
        let word = &source[start..end];

        // A keyword used as a name is the name: `function new`, `T.new`,
        // `obj:match`, and `new = ...` in a table. The child answers.
        let before = source[..start].trim_end();
        let after = source[end..].trim_start();

        // A member's `.` or `:` touches the name; an annotation's `:` has
        // a space after it, and that word is a type.
        if before.ends_with("function")
            || source[..start].ends_with(['.', ':'])
            || (after.starts_with('=') && !after.starts_with("=="))
        {
            return None;
        }

        // An intrinsic or an attribute carries its sigil in the key.
        if start > 0 && matches!(bytes[start - 1], b'$' | b'@') {
            let key = &source[start - 1..end];

            if let Some(text) = lookup(key) {
                return Some((start - 1, end, text));
            }
        }

        // A derive name inside `@derive( )` has its own entry.
        let line_start = source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let before = &source[line_start..start];

        if let Some(i) = before.rfind("@derive(")
            && !before[i..].contains(')')
            && let Some(text) = lookup(&format!("derive:{word}"))
        {
            return Some((start, end, text));
        }

        return lookup(word).map(|text| (start, end, text));
    }

    // The longest operator that covers the byte wins.
    let mut best: Option<(usize, usize, &'static str)> = None;

    for (key, text) in alloy::docs::TABLE {
        let key = key.as_bytes();

        if key.is_empty() || is_word(key[0]) {
            continue;
        }

        for start in offset.saturating_sub(key.len() - 1)..=offset {
            if bytes.get(start..start + key.len()) == Some(key)
                && best.is_none_or(|(s, e, _)| key.len() > e - s)
            {
                best = Some((start, start + key.len(), text));
            }
        }
    }

    best
}

/// True when the byte at `offset` belongs to a word.
pub fn is_word_at(source: &str, offset: usize) -> bool {
    source.as_bytes().get(offset).is_some_and(|b| is_word(*b))
}

/// The byte range of the word at `offset`.
pub fn word_range(source: &str, offset: usize) -> (usize, usize) {
    word_at(source.as_bytes(), offset)
}

/// The column of `word` as a whole word in `line`, if it occurs.
pub fn find_word(line: &str, word: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut from = 0;

    while let Some(i) = line[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before = start > 0 && is_word(bytes[start - 1]);
        let after = end < bytes.len() && is_word(bytes[end]);

        if !before && !after {
            return Some(start);
        }

        from = start + 1;
    }

    None
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn word_at(bytes: &[u8], offset: usize) -> (usize, usize) {
    let mut start = offset;
    let mut end = offset;

    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }

    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }

    (start, end)
}

fn lookup(key: &str) -> Option<&'static str> {
    alloy::docs::lookup(key)
}

/// The Markdown for a key, for a completion item's documentation.
pub fn doc(key: &str) -> Option<&'static str> {
    lookup(key)
}

/// Every documented key with the prefix: `@` for the attributes, `$`
/// for the intrinsics, `derive:` for the derive names.
pub fn keys_with_prefix(prefix: &str) -> Vec<&'static str> {
    alloy::docs::keys_with_prefix(prefix)
}

pub use alloy::docs::ALLOY_KEYWORDS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyword_as_a_name_is_not_the_keyword() {
        assert!(hover("function new() end", 10).is_none());
        assert!(hover("local x = V.new()", 13).is_none());
        assert!(hover("local t = { new = 1 }", 13).is_none());
        assert!(hover("local v = new V { }", 11).is_some());
        assert!(hover("local p: Partial<V> = {}", 10).is_some());
    }

    #[test]
    fn longest_operator_wins() {
        let src = "cache[key] ??= f()";
        let at = src.find("??=").unwrap();

        for o in at..at + 3 {
            let (s, e, text) = hover(src, o).unwrap();
            assert_eq!((s, e), (at, at + 3));
            assert!(text.starts_with("```alloy\na ??= b"));
        }
    }

    #[test]
    fn keywords_and_sigils() {
        assert!(hover("struct V as", 2).unwrap().2.contains("A record"));
        assert!(hover("impl V", 0).unwrap().2.contains("Methods"));

        let (s, e, _) = hover("x = $dbg(y)", 6).unwrap();
        assert_eq!((s, e), (4, 8));

        let (s, e, _) = hover("@derive(Eq)", 3).unwrap();
        assert_eq!((s, e), (0, 7));
    }

    #[test]
    fn whole_word_search() {
        assert_eq!(find_word("local Vec2Fields = Vec2", "Vec2"), Some(19));
        assert_eq!(find_word("local x = 1", "y"), None);
    }

    #[test]
    fn unknown_tokens_have_none() {
        assert!(hover("local x = 1", 0).is_none());
        assert!(hover("a + b", 2).is_none());
        assert!(is_word_at("a + b", 0));
        assert!(!is_word_at("a + b", 2));
    }
}
