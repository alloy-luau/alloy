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

        let line_start = source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_before = &source[line_start..start];

        // `@serde.rename` and `@derive(serde.Serialize)`: a name through a
        // star import reads the doc of the name it reaches.
        if let Some(path) = line_before.strip_suffix('.') {
            let holder = path.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

            if holder.ends_with('@')
                && holder.len() < path.len()
                && let Some(text) = lookup(&format!("@{word}"))
            {
                return Some((start, end, text));
            }

            if let Some(i) = line_before.rfind("@derive(")
                && !line_before[i..].contains(')')
                && let Some(text) = lookup(&format!("derive:{word}"))
            {
                return Some((start, end, text));
            }
        }

        // A name in an import list of the std: a derive or an attribute
        // has no entry under its bare name.
        if in_std_import(source, start) && lookup(word).is_none() {
            let text = lookup(&format!("derive:{word}")).or_else(|| lookup(&format!("@{word}")));

            if let Some(text) = text {
                return Some((start, end, text));
            }
        }

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

        // A contextual word, ex: the `new` of `local new = Instance.new`,
        // is the local it names. The child holds its type and its
        // definition, so it answers the hover.
        if alloy_syntax::contextual::is_contextual(word)
            && !alloy_syntax::contextual::keyword_at_byte(source, start)
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
        let before = line_before;

        if let Some(i) = before.rfind("@derive(")
            && !before[i..].contains(')')
            && let Some(text) = lookup(&format!("derive:{word}"))
        {
            return Some((start, end, text));
        }

        return lookup(word).map(|text| (start, end, meaning(source, start, word, text)));
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

/// Whether the byte sits inside the braces of an `import { ... } from`
/// whose spec names the std.
fn in_std_import(source: &str, at: usize) -> bool {
    let Some(open) = source[..at].rfind('{') else {
        return false;
    };

    if source[open..at].contains('}') || !source[..open].trim_end().ends_with("import") {
        return false;
    }

    let Some(close) = source[at..].find('}') else {
        return false;
    };
    let rest = source[at + close + 1..].trim_start();

    rest.strip_prefix("from")
        .map(str::trim_start)
        .and_then(|r| r.strip_prefix(['"', '\'']))
        .is_some_and(|r| r.starts_with("@alloy/std"))
}

/// The hover of the name after a child lookup: `->systems` finds the
/// child with `FindFirstChild`, and `=>systems` waits for it with
/// `WaitForChild`. The name is a string in the emit, so the child has
/// nothing to answer for it. `ty` gives the type the compiler wrote for
/// the lookup that starts at a byte offset. The compiler owns that
/// rule, so the hover does not repeat it.
pub fn child_hover(
    source: &str,
    offset: usize,
    ty: impl Fn(usize) -> Option<String>,
) -> Option<(usize, usize, String)> {
    let bytes = source.as_bytes();

    if offset >= bytes.len() || !is_word(bytes[offset]) {
        return None;
    }

    let (start, end) = word_at(bytes, offset);
    let before = source[..start].trim_end();
    let wait = before.ends_with("=>");

    if !wait && !before.ends_with("->") {
        return None;
    }

    // The receiver: the path in front of the arrow, on this line.
    let head = &before[..before.len() - 2];
    let from = head
        .rfind(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '-' | '>' | '=')))
        .map_or(0, |i| i + 1);
    let receiver = head[from..].trim_start_matches(['=', '-', '>']);
    let name = &source[start..end];
    let (arrow, how) = match wait {
        true => ("=>", "waits for it with `WaitForChild`"),

        false => (
            "->",
            "finds it with `FindFirstChild`, and gives nil when there is none",
        ),
    };
    let ty = ty(start);
    // A sourcemap names the class; with none, the lookup is an `Instance`.
    let unnamed = match ty.as_deref().map(|t| t.trim_end_matches('?')) {
        None | Some("Instance" | "any") => {
            " The source names no class, so `is` or a cast says which one it is."
        }

        Some(_) => "",
    };
    let ty = ty.map(|t| format!(": {t}")).unwrap_or_default();

    Some((
        start,
        end,
        format!(
            "```alloy\n{receiver}{arrow}{name}{ty}\n```\nThe child of `{receiver}` named `{name}`. `{arrow}` {how}.{unnamed}"
        ),
    ))
}

/// The hover of a word inside `@allow( )` or Luau's `@[ ]`: a lint's
/// doc, a group, a tool prefix, or one of Luau's attributes and the keys
/// `deprecated` takes. The text is built, so it is owned.
pub fn attribute_argument_hover(source: &str, offset: usize) -> Option<(usize, usize, String)> {
    let bytes = source.as_bytes();

    if offset >= bytes.len() || !is_word(bytes[offset]) {
        return None;
    }

    let (start, end) = word_at(bytes, offset);
    let word = &source[start..end];
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let before = &source[line_start..start];
    let open =
        |head: &str, close: char| before.rfind(head).filter(|i| !before[*i..].contains(close));

    if open("@allow(", ')').is_some() {
        let text = if let Some(l) = alloy::lint::LINTS.iter().find(|l| l.name == word) {
            format!(
                "**{}**, a lint of the `{}` group\n\n{}\n\n{}",
                l.name,
                l.group.name(),
                l.summary,
                l.detail
            )
        } else if let Some(group) = alloy::lint::Group::from_name(word) {
            format!(
                "**{}**, a lint group: `@allow({})` quiets each of its lints.",
                group.name(),
                group.name()
            )
        } else {
            match word {
                "flux" => "`flux.`: a lint of the compiler, `@allow(flux.too_many_arguments)`.".to_string(),

                "luau" => "`luau.`: a lint of luau-lsp, `@allow(luau.LocalShadow)`. An error stays an error.".to_string(),

                "alx" => "`alx.`: a markup lint, `@allow(alx.static_conditional_child)`.".to_string(),

                _ => return None,
            }
        };

        return Some((start, end, text));
    }

    if open("@[", ']').is_some() {
        let text = match word {
            "native" | "checked" | "deprecated" => lookup(&format!("@{word}"))?.to_string(),

            "use" => "The name to call instead of the deprecated function, a string.".to_string(),

            "reason" => "Why the function is deprecated, a string.".to_string(),

            _ => return None,
        };

        return Some((start, end, text));
    }

    None
}

/*
The doc of a keyword, cut to the meaning the position reads.

Two keywords write one paragraph per meaning. `default` is the fallback
arm of a `match`, and it is also the one value `export default` sends
out. `as` marks where a declaration body begins, and it also renames a
name in an `import` or an `export` list. A position that names neither
meaning keeps the whole doc, which is what `alloy doc` prints.
*/
fn meaning(source: &str, start: usize, word: &str, text: &'static str) -> &'static str {
    let paragraph = |n: usize| text.split("\n\n").nth(n).unwrap_or(text);
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let head = source[line_start..start].trim_end();

    match word {
        "default" if last_word(head) == "export" => paragraph(1),

        "default" if crate::context::match_scrutinee(source, start).is_some() => paragraph(0),

        // `import * as M` renames the whole module. The rename is the
        // doc's last paragraph, after the header's example.
        "as" if in_name_list(source, start) || head.ends_with('*') => {
            text.rsplit("\n\n").next().unwrap_or(text)
        }

        "as" if opens_a_body(head) => paragraph(0),

        _ => text,
    }
}

/// The word a text ends with, empty when it ends in punctuation.
fn last_word(text: &str) -> &str {
    let end = text.trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');

    &text[end.len()..]
}

/// Whether a `{` stands open in front of the offset. The name list of
/// an `import` or an `export` spans lines and holds no blank line, so
/// the scan stops at one.
fn in_name_list(source: &str, offset: usize) -> bool {
    let mut depth = 0i32;

    for line in source[..offset].rsplit('\n').take(16) {
        if line.trim().is_empty() {
            break;
        }

        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
    }

    depth > 0
}

/// Whether the line opens a declaration, so its `as` marks where the
/// members begin.
fn opens_a_body(head: &str) -> bool {
    for word in head.split_whitespace() {
        if matches!(
            word,
            "export" | "public" | "private" | "local" | "declare" | "open"
        ) {
            continue;
        }

        return matches!(
            word,
            "struct"
                | "trait"
                | "impl"
                | "enum"
                | "interface"
                | "namespace"
                | "attribute"
                | "class"
        );
    }

    false
}

/// True when the byte at `offset` belongs to a word.
pub fn is_word_at(source: &str, offset: usize) -> bool {
    source.as_bytes().get(offset).is_some_and(|b| is_word(*b))
}

/// True when the caret at `offset` sits on a word: the byte there
/// belongs to one, or the caret stands right after a word's last byte,
/// the position left by typing the name, where `word_range` reads it.
pub fn is_word_caret(source: &str, offset: usize) -> bool {
    is_word_at(source, offset) || (offset > 0 && is_word_at(source, offset - 1))
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

/// Every word Alloy and Luau reserve. A completion whose typed word
/// begins one of these offers a keyword, not a module: `end` in
/// `if x then return end` drew `EncodingService` from a package before
/// the list held `end` itself.
pub const WORDS: &[&str] = &[
    "after",
    "and",
    "as",
    "async",
    "attribute",
    "await",
    "band",
    "bnot",
    "bor",
    "break",
    "bxor",
    "case",
    "class",
    "const",
    "continue",
    "declare",
    "default",
    "delete",
    "destroy",
    "do",
    "each",
    "else",
    "elseif",
    "end",
    "enum",
    "export",
    "extends",
    "extern",
    "false",
    "for",
    "from",
    "function",
    "if",
    "impl",
    "import",
    "in",
    "interface",
    "is",
    "local",
    "macro",
    "match",
    "namespace",
    "new",
    "nil",
    "not",
    "on",
    "open",
    "or",
    "private",
    "public",
    "read",
    "remote",
    "repeat",
    "requires",
    "return",
    "satisfies",
    "shl",
    "shr",
    "struct",
    "then",
    "trait",
    "true",
    "try",
    "type",
    "until",
    "where",
    "while",
    "with",
    "write",
];

/// The keywords a typed word begins, in order.
pub fn starting_with(word: &str) -> Vec<&'static str> {
    match word.is_empty() {
        true => Vec::new(),

        false => WORDS
            .iter()
            .copied()
            .filter(|k| k.starts_with(word))
            .collect(),
    }
}

/// Whether the word is a keyword of its own.
pub fn is_keyword(word: &str) -> bool {
    WORDS.contains(&word)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name inside `@allow( )` reads its lint, a group its members, and
    /// a name inside Luau's `@[ ]` its attribute.
    #[test]
    fn an_attribute_argument_hovers_its_meaning() {
        let src = "@allow(too_many_arguments, pedantic)\n@[native, deprecated {use = \"f\"}]\n";
        let at = |needle: &str| {
            attribute_argument_hover(src, src.find(needle).unwrap() + 1).map(|(_, _, t)| t)
        };
        assert!(
            at("too_many")
                .unwrap()
                .starts_with("**too_many_arguments**")
        );
        assert!(at("pedantic").unwrap().contains("a lint group"));
        assert!(at("native").unwrap().contains("Luau's own"));
        assert!(at("use =").unwrap().starts_with("The name to call instead"));
        assert_eq!(attribute_argument_hover("local too_many = 1\n", 7), None);
    }

    #[test]
    fn a_keyword_as_a_name_is_not_the_keyword() {
        assert!(hover("function new() end", 10).is_none());
        assert!(hover("local x = V.new()", 13).is_none());
        assert!(hover("local t = { new = 1 }", 13).is_none());
        assert!(hover("local v = new V { }", 11).is_some());
        assert!(hover("local p: Partial<V> = {}", 10).is_some());
    }

    /*
    A contextual word used as a name answers nothing here, so the child
    answers with the local's own type.

    The offsets below sit inside the word: `local new` puts `new` at 6,
    `print(try` puts `try` at 6.
    */
    #[test]
    fn a_contextual_name_leaves_the_hover_to_the_child() {
        assert!(hover("local new = Instance.new\n", 6).is_none());
        assert!(hover("local try = pcall\nprint(try)\n", 24).is_none());
        assert!(hover("local match = string.match\nprint(match)\n", 33).is_none());
        assert!(hover("local const = 1\nprint(const + 1)\n", 22).is_none());
        assert!(hover("local export = {}\nreturn export\n", 25).is_none());

        // The keyword reading still answers from the table.
        assert!(hover("local v = try f()\n", 10).is_some());
        assert!(hover("match x with\ncase 1 then print(1)\nend\n", 0).is_some());
        assert!(hover("const LIMIT = 5\n", 0).is_some());
        assert!(hover("export type T = number\n", 0).is_some());
    }

    /// `destroy` and `after` answer from the table, and each one used
    /// as a member is the member.
    #[test]
    fn destroy_and_after_hover() {
        let src = "destroy part after 3\n";
        let (s, e, text) = hover(src, 0).unwrap();
        assert_eq!(&src[s..e], "destroy");
        assert!(text.contains("destroy method and nothing else"), "{text}");

        let at = src.find("after").unwrap();
        let (s, e, text) = hover(src, at).unwrap();
        assert_eq!(&src[s..e], "after");
        assert!(text.contains("Runs a block later"), "{text}");

        let (s, _, _) = hover("after 3 where ready do", 8).unwrap();
        assert_eq!(s, 8);

        // A method of that name is the method; the child answers.
        assert!(hover("bag:destroy()", 4).is_none());
        assert!(hover("function destroy(self) end", 9).is_none());
    }

    /// `default` writes one paragraph per meaning. The arm inside a
    /// `match` reads the first, and `export default` reads the second.
    #[test]
    fn default_reads_the_meaning_of_its_position() {
        let src = concat!(
            "match color with\n",
            "    case \"Red\" then 1\n",
            "    default 0\n",
            "end\n",
            "export default f\n",
        );

        let at = src.find("    default").unwrap() + 4;
        let text = hover(src, at).unwrap().2;
        assert!(text.starts_with("The fallback arm"), "{text}");
        assert!(!text.contains("export default expr"), "{text}");

        let at = src.find("export default").unwrap() + "export ".len();
        let text = hover(src, at).unwrap().2;
        assert!(text.starts_with("After `export`"), "{text}");
        assert!(!text.contains("fallback arm"), "{text}");
    }

    /// `as` splits a header from a one-line body and renames a name in a
    /// list. A `match` alias names neither meaning, so the whole doc
    /// stands.
    #[test]
    fn as_reads_the_meaning_of_its_position() {
        let decl = "enum Color as Red, Green end\n";
        let text = hover(decl, decl.find(" as").unwrap() + 1).unwrap().2;
        assert!(text.starts_with("Splits a declaration"), "{text}");

        let list = "import { shade as tint } from \"./m\"\n";
        let text = hover(list, list.find(" as").unwrap() + 1).unwrap().2;
        assert!(text.starts_with("In `import"), "{text}");

        let star = "import * as M from \"./m\"\n";
        let text = hover(star, star.find(" as").unwrap() + 1).unwrap().2;
        assert!(text.starts_with("In `import"), "{text}");

        let alias = "match msg as m with\n    default 0\nend\n";
        let text = hover(alias, alias.find(" as").unwrap() + 1).unwrap().2;
        assert!(text.contains("Splits a declaration"), "{text}");
        assert!(text.contains("renames a name"), "{text}");
    }

    /// A keyword whose doc holds one meaning keeps all of it.
    #[test]
    fn one_meaning_keeps_the_whole_doc() {
        let src = "read x: number\n";
        let text = hover(src, 0).unwrap().2;
        assert_eq!(text, lookup("read").unwrap());
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
        assert!(hover("foo = 1", 0).is_none());
        assert!(hover("a + b", 2).is_none());
        assert!(is_word_at("a + b", 0));
        assert!(!is_word_at("a + b", 2));
        // The caret right after a name is on it; `+` is on nothing.
        assert!(is_word_caret("a + b", 1));
        assert!(!is_word_caret("a + b", 2));
    }
}
