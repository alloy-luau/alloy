//! The `match` around the caret: its scrutinee, and the names its
//! `case` arms already write.

use super::strings::{block_closers, block_openers, is_word};

/// The last whole-word occurrence of `word` in `text`.
fn last_word_at(text: &str, word: &str) -> Option<usize> {
    let mut found = None;
    let mut from = 0;

    while let Some(i) = text[from..].find(word) {
        let start = from + i;
        let end = start + word.len();

        if !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word)
        {
            found = Some(start);
        }

        from = start + 1;
    }

    found
}

/// Whether the caret sits in the slots of an array pattern, `case
/// [first, |`. Every slot binds a name the author chooses, so no list
/// belongs there.
pub(crate) fn in_array_pattern(head: &str) -> bool {
    let arm = head.trim_start();
    let Some(rest) = arm.strip_prefix("case") else {
        return false;
    };

    if rest.starts_with(is_word) {
        return false;
    }

    rest.matches('[').count() > rest.matches(']').count()
}

/// The scrutinee of a `match` head: the text between `match` and the
/// `with` that ends the head. `local r = match x with` and `return
/// match x with` read the same as the statement form. An `as name`
/// alias names the value; the value itself stands in front of it.
fn scrutinee_of(line: &str) -> Option<String> {
    let with = last_word_at(line, "with")?;
    let start = last_word_at(&line[..with], "match")? + "match".len();
    let head = line[start..with].trim();
    let text = match last_word_at(head, "as") {
        Some(at) => head[..at].trim(),

        None => head,
    };

    (!text.is_empty()).then(|| text.to_string())
}

/// The line the `match` around the caret opens on, as a byte offset,
/// with the expression that `match` takes. The scan counts the blocks
/// upward, so the nearest open `match` wins and an inner one shadows an
/// outer.
fn match_head(src: &str, offset: usize) -> Option<(usize, String)> {
    let head = &src[..offset.min(src.len())];
    let mut depth = 0i32;
    let mut cursor = head.len();

    loop {
        let start = head[..cursor].rfind('\n').map_or(0, |i| i + 1);
        let line = &head[start..cursor];
        let t = line.trim();

        if !(t.is_empty() || t.starts_with("--")) {
            depth += block_closers(t);

            let opens = block_openers(t);

            // More opened here than the scan closed below: this line
            // opens the block the caret sits in.
            if opens > depth {
                return scrutinee_of(t).map(|s| (start, s));
            }

            depth -= opens;
        }

        if start == 0 {
            return None;
        }

        cursor = start - 1;
    }
}

/// The expression the `match` around the caret takes.
pub(crate) fn match_scrutinee(src: &str, offset: usize) -> Option<String> {
    match_head(src, offset).map(|(_, s)| s)
}

/// The value `$matches(value, |` tests, when the caret sits in the
/// pattern slot of the call. `$matches` takes the value first and the
/// pattern second, so one comma at depth one opens the pattern.
pub(crate) fn matches_scrutinee(head: &str) -> Option<String> {
    let open = head.rfind("$matches(")? + "$matches(".len();
    let inside = &head[open..];
    let mut depth = 0i32;
    let mut comma = None;

    for (i, c) in inside.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 && comma.is_none() => comma = Some(i),
            _ => {}
        }
    }

    if depth < 0 {
        return None;
    }

    let at = comma?;
    let value = inside[..at].trim();

    // Past the pattern's own comma the caret is in an argument list of
    // the pattern, not in the pattern slot.
    (!value.is_empty() && !inside[at + 1..].contains(',')).then(|| value.to_string())
}

/// The name each arm of the `match` around the caret opens with:
/// `case Ok(v)` answers `Ok`. An arm already written says what the
/// scrutinee is when its declaration does not.
pub fn match_arms(src: &str, offset: usize) -> Vec<String> {
    let Some((at, _)) = match_head(src, offset) else {
        return Vec::new();
    };
    let opener = src[at..].lines().next().unwrap_or("");
    let indent = opener.len() - opener.trim_start().len();
    let mut out = Vec::new();

    for line in src[at..].lines().skip(1) {
        let t = line.trim_start();

        if t == "end" && line.len() - t.len() <= indent {
            break;
        }

        if let Some(rest) = t.strip_prefix("case ") {
            let word: String = rest
                .trim_start()
                .chars()
                .take_while(|c| is_word(*c))
                .collect();

            if !word.is_empty() {
                out.push(word);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arms already written, for a scrutinee nothing else names.
    #[test]
    fn the_arms_of_a_match_read_back() {
        let src = concat!(
            "for _, result in settled do\n",
            "    match result with\n",
            "        case Ok(profile) then f(profile)\n",
            "        case Err(message) then warn(message)\n",
            "    end\n",
            "end\n"
        );
        let at = src.find("case Ok").unwrap() + "case ".len();
        assert_eq!(match_arms(src, at), ["Ok", "Err"]);
        assert!(match_arms("local x = 1\n", 5).is_empty());
    }

    /// `match e as name with` names the value. The completion reads the
    /// value, not the name.
    #[test]
    fn a_head_alias_leaves_the_scrutinee() {
        let src = "match player.state as state with\n    case \n";
        let at = src.find("case ").unwrap() + "case ".len();
        assert_eq!(match_scrutinee(src, at).as_deref(), Some("player.state"));
        let plain = "match player.state with\n    case \n";
        let at = plain.find("case ").unwrap() + "case ".len();
        assert_eq!(match_scrutinee(plain, at).as_deref(), Some("player.state"));
    }
}
