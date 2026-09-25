//! A member completion, `a.b` and its guarded and wrapped forms: the
//! receiver, the guard, and where the member sits once the emit moves
//! or wraps the receiver.

/// The guard a member access carries before its separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// `a.b` and `a:b`: the emit copies the access.
    Plain,
    /// `a?.b`, which lowers to `(if a == nil then nil else a.b)`.
    Optional,
    /// `a!.b`, which lowers to `(if a == nil then error(..) else a).b`.
    Asserted,
    /// `"abc":upper()`, which lowers to `("abc"):upper()`: the emit
    /// wraps the literal so the call parses.
    Wrapped,
}

impl Access {
    /// The text between the receiver and the separator in the source.
    fn source_guard(self) -> &'static str {
        match self {
            Access::Plain | Access::Wrapped => "",
            Access::Optional => "?",
            Access::Asserted => "!",
        }
    }

    /// The same in the lowered text: `!` closes the guard expression
    /// before the separator, and the other two write nothing.
    fn shadow_guard(self) -> &'static str {
        match self {
            Access::Asserted | Access::Wrapped => ")",
            _ => "",
        }
    }
}

fn is_word_byte(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The receiver, the guard, the separator, and the typed prefix of a
/// member completion. `bx?.na` answers `("bx", Optional, '.', 2)`.
/// `None` when the cursor sits somewhere else.
pub fn member_at(src: &str, offset: usize) -> Option<(String, Access, char, usize)> {
    let head = src.get(..offset)?;
    let word = head.len() - head.trim_end_matches(is_word_byte).len();
    let before = &head[..head.len() - word];
    let sep = before.chars().next_back()?;

    if !matches!(sep, '.' | ':') {
        return None;
    }

    let before = &before[..before.len() - sep.len_utf8()];
    let (before, access) = match before.chars().next_back() {
        Some('?') => (&before[..before.len() - 1], Access::Optional),
        Some('!') => (&before[..before.len() - 1], Access::Asserted),
        Some('.' | ':') => return None,
        _ => (before, Access::Plain),
    };
    // `"abc":upper()` and `` `a{b}`:split(",") ``: the receiver is the
    // literal, which the emit wraps in parentheses.
    if access == Access::Plain
        && let Some(quote) = before.chars().next_back()
        && matches!(quote, '"' | '\'' | '`')
        && let Some(open) = before[..before.len() - quote.len_utf8()].rfind(quote)
    {
        return Some((before[open..].to_string(), Access::Wrapped, sep, word));
    }

    let start = match before.ends_with(']') {
        // `profile["coins"].`, `parts?[1]!.`: the index is part of the
        // receiver, and each bracket carries its own guard.
        true => bracketed_start(before)?,

        false => before
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word_byte(*c) || *c == '.')
            .last()
            .map(|(i, _)| i)?,
    };
    let base = &before[start..];

    // `o?.inner.`: a guard stands inside the chain, so the path after it
    // names no receiver alone. `guarded_member_column` answers there.
    (!base.is_empty()
        && !base.starts_with('.')
        && !base.ends_with('.')
        && !base.starts_with(|c: char| c.is_numeric()))
    .then(|| (base.to_string(), access, sep, word))
}

/// Where a receiver that ends in a bracket starts: `mo?["k"]` from the
/// `m`, `xs[i][j]` from the `x`. Walks back over each balanced group,
/// the `?` or `!` that guards it, and the path before them. `None`
/// when a bracket never opens or nothing names the head.
fn bracketed_start(before: &str) -> Option<usize> {
    let bytes = before.as_bytes();
    let mut end = before.len();

    while end > 0 && bytes[end - 1] == b']' {
        let mut depth = 0i32;
        let mut i = end;

        loop {
            i = i.checked_sub(1)?;

            match bytes[i] {
                b']' => depth += 1,

                b'[' => depth -= 1,

                _ => {}
            }

            if depth == 0 {
                break;
            }
        }

        if i > 0 && matches!(bytes[i - 1], b'?' | b'!') {
            i -= 1;
        }

        end = i;
    }

    if end == before.len() {
        return None;
    }

    let start = before[..end]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word_byte(*c) || *c == '.')
        .last()
        .map(|(i, _)| i)?;

    (start < end).then_some(start)
}

/// The receiver as the lowering spells it. `mo?["k"]` keeps the index
/// and drops the `?`, which the guard around the whole expression
/// carries instead; `mo!["k"]` closes that guard before the bracket.
fn shadow_base(base: &str) -> String {
    match base.contains(['?', '!']) {
        true => base.replace("?[", "[").replace("![", ")["),

        false => base.to_string(),
    }
}

/// The receiver of an index that never closed, what the author typed
/// inside it, and the quote that opened the key. `profile["na|`
/// answers `("profile", "na", Some('"'))` and `parts![|` answers
/// `("parts", "", None)`. `None` when no bracket stands open on the
/// line, when a literal opened it, or when the key is already whole.
pub fn index_key_at(src: &str, offset: usize) -> Option<(String, String, Option<char>)> {
    let line_start = src[..offset.min(src.len())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let head = src.get(line_start..offset)?;
    let open = open_bracket(head)?;
    let inside = head[open + 1..].trim_start();
    let (typed, quote) = match inside.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let rest = &inside[q.len_utf8()..];

            // A closed string is a whole key already.
            match rest.contains(q) {
                true => return None,

                false => (rest.to_string(), Some(q)),
            }
        }

        None => (String::new(), None),

        // A key the author is writing as an expression, `t[i + 1]`, is
        // no string; only a bare word can still become one.
        Some(_) => match inside.chars().all(is_word_byte) {
            true => (inside.to_string(), None),

            false => return None,
        },
    };
    let before = &head[..open];
    let before = before.strip_suffix(['?', '!']).unwrap_or(before);
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word_byte(*c) || *c == '.')
        .last()
        .map(|(i, _)| i)?;
    let receiver = &before[start..];

    (!receiver.ends_with('.') && !receiver.starts_with(|c: char| c.is_numeric()))
        .then(|| (receiver.to_string(), typed, quote))
}

/// The receiver and the guard of a guarded index the caret sits in:
/// `mo?[k|]` and `parts![|1]` answer the receiver, the guard, and the
/// byte offset of the `[`. `None` for a plain index, which the emit
/// copies, and for a receiver with no name of its own.
pub fn index_at(src: &str, offset: usize) -> Option<(String, Access, usize)> {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[offset..].find('\n').map_or(src.len(), |i| offset + i);
    let line = &src[line_start..line_end];
    let here = offset - line_start;
    let bytes = line.as_bytes();

    for i in 1..bytes.len() {
        if bytes[i] != b'[' {
            continue;
        }

        let access = match bytes[i - 1] {
            b'?' => Access::Optional,

            b'!' => Access::Asserted,

            _ => continue,
        };

        // The bracket itself, either end of it. A key inside is an
        // expression of its own, and the child reads that.
        if here != i && here != closing_bracket(line, i) {
            continue;
        }

        let before = &line[..i - 1];
        let start = before
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_word_byte(*c) || *c == '.')
            .last()
            .map(|(k, _)| k)?;
        let base = &before[start..];

        if base.ends_with('.') || base.starts_with(|c: char| c.is_numeric()) {
            return None;
        }

        return Some((base.to_string(), access, line_start + i));
    }

    None
}

/// The offset of the `]` that closes the `[` at `open`, or the end of
/// the line when nothing closes it.
fn closing_bracket(line: &str, open: usize) -> usize {
    let mut depth = 0i32;

    for (i, b) in line.bytes().enumerate().skip(open) {
        match b {
            b'[' => depth += 1,

            b']' => {
                depth -= 1;

                if depth == 0 {
                    return i;
                }
            }

            _ => {}
        }
    }

    line.len()
}

/// The byte offset of the `[` still open at the end of `head`. Reads
/// the line forward so a bracket inside a string counts for nothing.
fn open_bracket(head: &str) -> Option<usize> {
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut quote: Option<char> = None;
    let mut chars = head.char_indices();

    while let Some((i, c)) = chars.next() {
        match quote {
            Some(q) => match c {
                '\\' => {
                    chars.next();
                }

                _ if c == q => quote = None,

                _ => {}
            },

            None => match c {
                '"' | '\'' => quote = Some(c),

                '[' | '(' | '{' => stack.push((c, i)),

                ']' | ')' | '}' => {
                    stack.pop();
                }

                _ => {}
            },
        }
    }

    stack.last().filter(|(c, _)| *c == '[').map(|(_, i)| *i)
}

/// Every byte offset in `line` where `needle` starts a whole access:
/// the byte before it names no word. A `.` before it is allowed, since
/// the emit qualifies a std name as `__alloy.Name`.
fn access_starts(line: &str, needle: &str) -> Vec<usize> {
    // An intrinsic quotes its argument for the message it prints, so
    // `"p:len()"` stands before the `p:len()` the child can type.
    line.match_indices(needle)
        .filter(|(i, _)| !super::in_string(line, *i))
        .filter(|(i, _)| {
            line[..*i]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_byte(c))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Where the member of an access sits on the lowered line. The emit
/// moves the receiver: `await Future.all(p)` becomes
/// `__alloy.await(__alloy.Future.all(p))`, and `a!.b` becomes a guarded
/// expression, so the member the author types has no position of its
/// own. The nth access on the source line is the nth on the lowered
/// one, which keeps a line with two accesses to the same receiver
/// apart.
pub fn member_column(
    source_line: &str,
    shadow_line: &str,
    base: &str,
    access: Access,
    sep: char,
    prefix: usize,
    source_column: usize,
) -> Option<usize> {
    let typed = format!("{base}{}{sep}", access.source_guard());
    let nth = access_starts(source_line, &typed)
        .into_iter()
        .filter(|i| i + typed.len() <= source_column)
        .count()
        .checked_sub(1)?;
    let lowered = format!("{}{}{sep}", shadow_base(base), access.shadow_guard());
    let at = *access_starts(shadow_line, &lowered).get(nth)?;
    let col = at + lowered.len() + prefix;

    (col <= shadow_line.len()).then_some(col)
}

/// Where the member of a guarded access sits on the lowered line, when
/// the receiver has no name of its own: a call, an index, or a chain
/// that already carries a guard. `f(x)?:m()` lowers to
/// `local _1 = f(x) if _1 ~= nil then _1:m() end`, `f(x)?.m` and
/// `xs[1]?.m` to `(if _1 == nil then nil else _1.m)`, and `o!.a!.b`
/// binds a temp per link, so the member follows the guard's `then` or
/// `else`. A plain name before the guard keeps its own place, and
/// `member_column` answers for it.
pub fn guarded_member_column(head: &str, shadow_line: &str, sep: char) -> Option<usize> {
    let guarded = head.trim_end_matches(is_word_byte);
    let guarded = guarded.strip_suffix(sep)?;
    // `o?.inner.`: the guard stands on an earlier link, and the plain
    // path after it, `.inner`, follows the guard's word on the lowered
    // line too: `else o.inner.`.
    let path_len = guarded.len()
        - guarded
            .trim_end_matches(|c: char| is_word_byte(c) || c == '.')
            .len();
    let (guarded, path) = guarded.split_at(guarded.len() - path_len);

    if !path.is_empty() && !path.starts_with('.') {
        return None;
    }

    if !guarded.ends_with(['?', '!']) {
        return None;
    }

    let receiver = guarded.trim_end_matches(['?', '!']);

    if path.is_empty() && !receiver.ends_with([')', ']']) && !receiver.contains(['?', '!']) {
        return None;
    }

    if !shadow_line.contains("== nil") && !shadow_line.contains("~= nil") {
        return None;
    }

    let after = format!("{path}{sep}");

    // The word the guard hands on, right after the branch it opens.
    for opener in ["then ", "else "] {
        let mut from = 0;
        let mut found = None;

        while let Some(i) = shadow_line[from..].find(opener) {
            let at = from + i + opener.len();
            let rest = &shadow_line[at..];
            let word = rest.trim_start_matches(is_word_byte);
            // `!` closes its guard before the separator: `_2).kills`.
            let word = word.strip_prefix(')').unwrap_or(word);

            if word.len() < rest.len() && word.starts_with(&after) {
                found = Some(shadow_line.len() - word.len() + after.len());
            }

            from = from + i + 1;
        }

        if found.is_some() {
            return found;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base and the typed prefix of a completion after `?.`, and
    /// where the member sits on the lowered line.
    #[test]
    fn an_optional_member_reads_its_base_and_its_prefix() {
        let src = "local deep = bx?.na";
        assert_eq!(
            member_at(src, src.len()),
            Some(("bx".to_string(), Access::Optional, '.', 2))
        );
        let dangling = "local deep = bx?.";
        assert_eq!(
            member_at(dangling, dangling.len()),
            Some(("bx".to_string(), Access::Optional, '.', 0))
        );
        assert_eq!(
            member_at("local x = bx.na", 15),
            Some(("bx".to_string(), Access::Plain, '.', 2))
        );
        assert_eq!(member_at("local x = bx", 12), None);

        let source = "local deep = bx?.name";
        let line = "local deep = (if bx == nil then nil else bx.name)";
        assert_eq!(
            member_column(source, line, "bx", Access::Optional, '.', 0, 17),
            Some(line.find("else bx.").unwrap() + 8)
        );
        // A base that only ends another name does not match.
        assert_eq!(
            member_column(
                "local q = bx?.name",
                "local q = abx.name",
                "bx",
                Access::Optional,
                '.',
                0,
                13
            ),
            None
        );
    }

    /// `!.` closes its guard before the separator, and a receiver the
    /// emit qualified is still the same access.
    #[test]
    fn an_asserted_member_and_a_moved_receiver_find_their_column() {
        let src = "    print(profile!.stats.kills)";
        let at = src.find("!.").unwrap() + 2;
        assert_eq!(
            member_at(src, at),
            Some(("profile".to_string(), Access::Asserted, '.', 0))
        );

        let shadow = "    print((if profile == nil then error(\"profile is nil\") else profile).stats.kills)";
        assert_eq!(
            member_column(src, shadow, "profile", Access::Asserted, '.', 0, at),
            Some(shadow.find("else profile).").unwrap() + "else profile).".len())
        );

        // `await Future.all(p)` lowers to `__alloy.await(__alloy.Future.all(p))`.
        let awaited = "    local s = await Future.all(p)";
        let lowered = "    local s = __alloy.await(__alloy.Future.all(p))";
        let col = awaited.find("Future.").unwrap() + "Future.".len();
        assert_eq!(
            member_at(awaited, col),
            Some(("Future".to_string(), Access::Plain, '.', 0))
        );
        assert_eq!(
            member_column(awaited, lowered, "Future", Access::Plain, '.', 0, col),
            Some(lowered.find("Future.").unwrap() + "Future.".len())
        );

        // Two accesses to one receiver on a line keep their order.
        let twice = "local v = Future.all(Future.race(p))";
        let second = twice.rfind("Future.").unwrap() + "Future.".len();
        assert_eq!(
            member_column(twice, twice, "Future", Access::Plain, '.', 0, second),
            Some(second)
        );
    }

    /// An index is part of the receiver: `profile["a"].`, `map?[k].`
    /// and `parts![1].` each read the member of what the index
    /// answers, and the emit spells the guard before the bracket.
    #[test]
    fn an_index_before_the_member_reads_as_the_receiver() {
        for (src, base) in [
            ("local v = profile[\"a\"].na", "profile[\"a\"]"),
            ("local v = map?[key].na", "map?[key]"),
            ("local v = parts![1].na", "parts![1]"),
            ("local v = grid[1][2].na", "grid[1][2]"),
        ] {
            assert_eq!(
                member_at(src, src.len()),
                Some((base.to_string(), Access::Plain, '.', 2)),
                "{src}"
            );
        }

        // `?[` drops its `?`: the guard wraps the whole expression.
        let source = "local v = map?[key].name";
        let shadow = "local v = (if map == nil then nil else map[key].name)";
        assert_eq!(
            member_column(source, shadow, "map?[key]", Access::Plain, '.', 0, 20),
            Some(shadow.find("map[key].").unwrap() + "map[key].".len())
        );

        // `![` closes its guard before the bracket.
        let asserted = "local v = map![key].name";
        let lowered =
            "local v = (if map == nil then (error(\"map is nil\") :: never) else map)[key].name";
        assert_eq!(
            member_column(asserted, lowered, "map![key]", Access::Plain, '.', 0, 20),
            Some(lowered.find("map)[key].").unwrap() + "map)[key].".len())
        );

        // A bracket that never opens names no receiver.
        assert_eq!(member_at("local v = ].na", 14), None);
    }

    /// The bracket of a guarded index: either end of it names the
    /// receiver and the guard, and a key inside does not.
    #[test]
    fn a_guarded_bracket_names_its_receiver() {
        let src = "local v = mo?[\"k\"]";
        let open = src.find('[').unwrap();
        assert_eq!(
            index_at(src, open),
            Some(("mo".to_string(), Access::Optional, open))
        );
        assert_eq!(
            index_at(src, src.len() - 1),
            Some(("mo".to_string(), Access::Optional, open))
        );
        assert_eq!(index_at(src, open + 2), None);

        let asserted = "local v = parts![1]";
        let at = asserted.find('[').unwrap();
        assert_eq!(
            index_at(asserted, at),
            Some(("parts".to_string(), Access::Asserted, at))
        );

        // A plain index and an array literal carry no guard.
        assert_eq!(index_at("local v = m[\"k\"]", 11), None);
        assert_eq!(index_at("local v = [ 1, 2 ]", 10), None);

        // The lowered bracket: `?[` drops its guard, `![` closes one.
        let source = "local v = mo?[\"k\"]";
        let shadow = "local v = (if mo == nil then nil else mo[\"k\"])";
        assert_eq!(
            member_column(source, shadow, "mo", Access::Optional, '[', 0, open + 1),
            Some(shadow.find("mo[").unwrap() + 3)
        );
    }

    /// `o?.inner.`: the guard stands on an earlier link. The chain names
    /// no receiver alone, and the member follows the path after the
    /// guard's branch on the lowered line.
    #[test]
    fn a_path_after_a_guard_finds_its_member() {
        let head = "print(o?.inner.";
        assert_eq!(member_at(head, head.len()), None);

        let shadow = "print((if o == nil then nil else o.inner.HOLE))";
        assert_eq!(
            guarded_member_column(head, shadow, '.'),
            Some(shadow.find("o.inner.").unwrap() + "o.inner.".len())
        );
        assert_eq!(
            guarded_member_column(
                "local x = o!.inner.",
                "local x = (if o == nil then (error(\"o is nil\") :: never) else o).inner.HOLE",
                '.'
            ),
            Some("local x = (if o == nil then (error(\"o is nil\") :: never) else o).inner.".len())
        );
    }

    /// A call before a guard has no name on the lowered line; the
    /// member follows the branch the guard opens.
    #[test]
    fn a_guarded_call_finds_its_member() {
        let shadow = "    local _1 = session_of(sender) if _1 ~= nil then _1:swing() end";
        assert_eq!(
            guarded_member_column("    session_of(sender)?:", shadow, ':'),
            Some(shadow.find("_1:swing").unwrap() + 3)
        );

        let optional = "    local v = (if _1 == nil then nil else _1.name)";
        assert_eq!(
            guarded_member_column("    local v = f(x)?.", optional, '.'),
            Some(optional.find("_1.name").unwrap() + 3)
        );

        // An index before the guard hoists the same way a call does.
        let indexed = "    local v = (if _1 == nil then nil else _1.name)";
        assert_eq!(
            guarded_member_column("    local v = parts[1]?.", indexed, '.'),
            Some(indexed.find("_1.name").unwrap() + 3)
        );

        // A chain of asserts binds a temp per link, and each `!`
        // closes its guard before the separator.
        let chained = concat!(
            "local _1 = (if o == nil then (error(\"o is nil\") :: never) else o).mid ",
            "local a = (if _1 == nil then (error(\"o!.mid is nil\") :: never) else _1).stats",
        );
        assert_eq!(
            guarded_member_column("local a = o!.mid!.", chained, '.'),
            Some(chained.rfind("_1).stats").unwrap() + "_1).".len())
        );

        // A plain call keeps its own receiver, so nothing moves.
        assert_eq!(
            guarded_member_column("    f(x):", "    f(x):m()", ':'),
            None
        );
        // A plain name before the guard keeps its place too.
        assert_eq!(
            guarded_member_column(
                "local v = bx?.",
                "local v = (if bx == nil then nil else bx.name)",
                '.'
            ),
            None
        );
        assert_eq!(
            guarded_member_column("    parts[1]:", "    parts[1]:m()", ':'),
            None
        );
    }

    /// A string literal is a receiver: the emit wraps it, so the member
    /// sits past the closing parenthesis.
    #[test]
    fn a_string_literal_receiver_finds_its_member() {
        let src = "local u = \"abc\":up";
        assert_eq!(
            member_at(src, src.len()),
            Some(("\"abc\"".to_string(), Access::Wrapped, ':', 2))
        );

        let source = "local u = \"abc\":upper()";
        let shadow = "local u = (\"abc\"):upper()";
        assert_eq!(
            member_column(source, shadow, "\"abc\"", Access::Wrapped, ':', 0, 16),
            Some(shadow.find("):upper").unwrap() + 2)
        );
    }
}
