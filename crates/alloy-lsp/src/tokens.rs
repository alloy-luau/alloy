//! Semantic tokens from the shadow, moved to the source, and the ones
//! the proxy draws itself.
//!
//! The child encodes tokens as deltas over the shadow text. A token that
//! sits wholly in copied text keeps its length and moves to its source
//! column; a token in generated text describes a temp or a helper the
//! author never wrote, so it goes.
//!
//! An Alloy construct is no Luau, so the shadow holds no word where the
//! author wrote one: an attribute, a macro call, a namespace segment, an
//! enum with its variant, and the name inside an interpolation hole. The
//! proxy draws those from the source. The result re-encodes in source
//! order, which is what the delta encoding requires.

use alloy_syntax::lexer::TokKind;

use crate::doc::{Doc, offset_of, position_of};

/// One token in source coordinates: line, column, length, type, and
/// modifiers.
type Token = (u32, u32, u64, u64, u64);

pub fn remap(data: &[u64], doc: &Doc, types: &[String], modifiers: &[String]) -> Vec<u64> {
    let Some(out) = doc.mapping() else {
        return data.to_vec();
    };

    let mut line = 0u64;
    let mut start = 0u64;
    let mut tokens: Vec<Token> = Vec::new();
    let copies = macro_copies(doc, out);

    for t in data.chunks_exact(5) {
        let (dl, ds, len, kind, mods) = (t[0], t[1], t[2], t[3], t[4]);
        line += dl;
        start = if dl > 0 { ds } else { start + ds };

        let (l, s) = (line as u32, start as u32);
        let first = offset_of(&doc.shadow, l, s);
        let last = offset_of(&doc.shadow, l, s + len as u32).map(|e| e.saturating_sub(1));

        let (Some(first), Some(last)) = (first, last) else {
            continue;
        };

        // A token in the code copy of a macro argument is a token of
        // the argument the author wrote.
        if out.map.is_generated(first as u32) || out.map.is_generated(last as u32) {
            if let Some(&(from, _, to)) = copies.iter().find(|c| c.0 <= first && last < c.1) {
                let (sl, sc) = position_of(&doc.source, to + first - from);
                tokens.push((sl, sc, len, kind, mods));
            }

            continue;
        }

        // The child reads `self` as a property of the table it stands
        // for, and the editor paints a semantic token over the grammar.
        // `self` is the receiver, and the grammar already scopes it
        // `variable.language.self`; no token here lets that color show
        // in every place the word appears.
        if doc.shadow.get(first..=last) == Some("self") {
            continue;
        }

        // The child reads the array shorthand `{ T }` as a table with an
        // implicit `[number]` index, and gives that index a token at the
        // brace with the width of the word `number`. The span covers
        // punctuation and half of the type behind it, so an arrow there
        // loses the color of its `>`. A name never opens on a brace.
        if doc.shadow[first..].starts_with('{') {
            continue;
        }

        let (sl, sc) = doc.to_source(l, s);
        tokens.push((sl, sc, len, kind, mods));
    }

    // The child's tokens stand first, so a word both of them describe
    // keeps the child's reading.
    tokens.extend(alloy_tokens(doc, types, modifiers));
    tokens.sort_by_key(|t| (t.0, t.1));
    tokens.dedup_by_key(|t| (t.0, t.1));

    let mut encoded = Vec::with_capacity(tokens.len() * 5);
    let (mut pl, mut pc) = (0u32, 0u32);

    for (l, c, len, kind, mods) in tokens {
        let dl = l - pl;
        let dc = if dl > 0 { c } else { c - pc };
        encoded.extend_from_slice(&[dl as u64, dc as u64, len, kind, mods]);
        pl = l;
        pc = c;
    }

    encoded
}

/// The code copy of each macro argument. `$assert(x > 0)` lowers to
/// `assert(x > 0, "assertion failed: x > 0")`, all of it text the
/// lowering generated. Each entry is the copy's byte range in the
/// shadow and the byte where the argument starts in the source. An
/// argument the lowering rewrote has no copy, and `$nameof` writes none.
fn macro_copies(doc: &Doc, out: &alloy::Output) -> Vec<(usize, usize, usize)> {
    let Ok(lexed) = alloy_syntax::lexer::lex(&doc.source) else {
        return Vec::new();
    };
    let (src, shadow, toks) = (&doc.source, &doc.shadow, &lexed.toks);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut copies = Vec::new();

    for i in 0..toks.len() {
        let opens = toks[i].text(src) == "$"
            && toks
                .get(i + 1)
                .is_some_and(|n| n.kind == TokKind::Ident && n.start == toks[i].end)
            && toks.get(i + 2).is_some_and(|t| t.kind == TokKind::LParen);

        if !opens {
            continue;
        }

        // Each argument at the list's own depth, as a byte range.
        let mut args = Vec::new();
        let mut from = toks[i + 2].end as usize;
        let mut depth = 0;

        for t in &toks[i + 2..] {
            match t.text(src) {
                "(" | "[" | "{" => depth += 1,

                ")" | "]" | "}" => {
                    depth -= 1;

                    if depth == 0 {
                        args.push((from, t.start as usize));

                        break;
                    }
                }

                "," if depth == 1 => {
                    args.push((from, t.start as usize));
                    from = t.end as usize;
                }

                _ => {}
            }
        }

        let (line, column) = position_of(src, toks[i].start as usize);
        let Some(line_start) = offset_of(shadow, doc.to_shadow(line, column).0, 0) else {
            continue;
        };
        let line_end = shadow[line_start..]
            .find('\n')
            .map_or(shadow.len(), |n| line_start + n);

        for (a, b) in args {
            let text = src[a..b].trim();
            let at = a + (src[a..b].len() - src[a..b].trim_start().len());

            if text.is_empty() {
                continue;
            }

            // A copy starts on the macro's line. It stands outside every
            // string, the message holds the other one, and in text the
            // lowering wrote, since copied text is another expression.
            let mut end = (line_end + text.len()).min(shadow.len());

            while !shadow.is_char_boundary(end) {
                end += 1;
            }

            let bounded = |k: usize| {
                let before = shadow[..k].chars().next_back();
                let after = shadow[k + text.len()..].chars().next();

                !(text.starts_with(is_word) && before.is_some_and(is_word))
                    && !(text.ends_with(is_word) && after.is_some_and(is_word))
            };
            let copy = shadow[line_start..end]
                .match_indices(text)
                .map(|(k, _)| line_start + k)
                .filter(|&k| {
                    bounded(k)
                        && !crate::context::in_string(shadow, k)
                        && out.map.is_generated(k as u32)
                })
                .last();

            if let Some(k) = copy {
                copies.push((k, k + text.len(), at));
            }
        }
    }

    copies
}

/// The index of a token type in the child's legend. The proxy paints
/// with the child's own numbers, so nothing here fixes an order; a name
/// the legend leaves out draws nothing.
fn type_index(types: &[String], name: &str) -> Option<u64> {
    types.iter().position(|t| t == name).map(|i| i as u64)
}

/// What a name in reach declares, from the head line of its hover. The
/// declaration index holds a namespace member under its path, `Geo.Kind`,
/// which is what a segment walk asks about.
fn declared_kind(doc: &Doc, path: &str) -> Option<&'static str> {
    declared_word(doc, path).filter(|k| matches!(*k, "namespace" | "enum"))
}

/// The keyword the head line of a declaration's hover opens with.
fn declared_word(doc: &Doc, path: &str) -> Option<&'static str> {
    doc.decls
        .iter()
        .chain(doc.import_decls.iter())
        .filter(|d| d.name == path)
        .find_map(|d| {
            let mut line = d.hover.lines().nth(1)?.trim_start();

            for word in ["export ", "global ", "public ", "private "] {
                line = line.strip_prefix(word).unwrap_or(line);
            }

            [
                "namespace",
                "enum",
                "struct",
                "interface",
                "trait",
                "class",
                "type",
            ]
            .into_iter()
            .find(|k| line.starts_with(&format!("{k} ")))
        })
}

/// What `member` draws as when an `impl` of the type at `path` writes
/// it: `method` with `self` first, as its call sites paint, and
/// `function` without. The hover of the type lists what its `impl`
/// blocks write, as `public function describe(self): string`.
fn impl_function_kind(doc: &Doc, path: &str, member: &str) -> Option<&'static str> {
    let hover = &doc
        .decls
        .iter()
        .chain(doc.import_decls.iter())
        .find(|d| d.name == path)?
        .hover;

    hover.lines().find_map(|line| {
        let mut line = line.trim_start();

        for word in ["public ", "private ", "async "] {
            line = line.strip_prefix(word).unwrap_or(line);
        }

        let rest = line.strip_prefix("function ")?.strip_prefix(member)?;

        if !rest.starts_with(['(', '<']) {
            return None;
        }

        let params = &rest[rest.find('(')? + 1..];

        Some(match params.starts_with("self") {
            true => "method",

            false => "function",
        })
    })
}

/// The legend name a member of a dotted path draws under. A trait is a
/// contract over methods, which the protocol calls an interface.
fn member_kind(doc: &Doc, path: &str) -> Option<&'static str> {
    Some(match declared_word(doc, path)? {
        "trait" => "interface",

        other => other,
    })
}

/// The contract body of an `attribute` declaration, as the token index it
/// opens at and the one past its `end`. `from` is the token after the
/// name and the parameter list, so the walk reads the rest of the head:
/// an `as` there opens the body. `None` when the declaration has none.
fn contract_body(
    toks: &[alloy_syntax::lexer::Tok],
    src: &str,
    from: usize,
) -> Option<(usize, usize)> {
    let head = position_of(src, toks.get(from)?.start as usize).0;
    let mut at = from;

    while toks
        .get(at)
        .is_some_and(|t| t.text(src) != "as" && position_of(src, t.start as usize).0 == head)
    {
        at += 1;
    }

    if toks.get(at)?.text(src) != "as" {
        return None;
    }

    let first = at + 1;
    let past = (first..toks.len())
        .find(|k| toks[*k].text(src) == "end")
        .map_or(toks.len(), |k| k + 1);

    Some((first, past))
}

/// The tokens the proxy draws from the source itself.
fn alloy_tokens(doc: &Doc, types: &[String], modifiers: &[String]) -> Vec<Token> {
    if types.is_empty() {
        return Vec::new();
    }

    // A source the lexer cannot read is mid-edit; the child's tokens are
    // the whole answer until it parses again.
    let Ok(lexed) = alloy_syntax::lexer::lex(&doc.source) else {
        return Vec::new();
    };
    let src = &doc.source;
    let toks = &lexed.toks;
    let mut out: Vec<Token> = Vec::new();
    let mut push = |start: u32, end: u32, name: &str, mods: u64| {
        let Some(kind) = type_index(types, name) else {
            return;
        };
        let (line, column) = position_of(src, start as usize);
        let width = src[start as usize..end as usize].encode_utf16().count() as u64;
        out.push((line, column, width, kind, mods));
    };
    // The `declaration` bit of the child's modifier legend; a legend
    // without it paints the plain kind.
    let declaration = type_index(modifiers, "declaration").map_or(0, |i| 1 << i);
    // Whether the walk stands inside a hole of an interpolated string:
    // the head and each middle piece open one, and the tail closes it.
    let mut in_hole = false;
    let mut i = 0;

    while i < toks.len() {
        let tok = toks[i];
        let text = tok.text(src);

        match tok.kind {
            TokKind::InterpHead | TokKind::InterpMid => in_hole = true,

            TokKind::InterpTail => in_hole = false,

            _ => {}
        }

        // `@[native, deprecated {...}]`: Luau's list, where each entry
        // opens with the name of an attribute. The walk goes on inside
        // the list, so the table of `deprecated` keeps its own tokens.
        if tok.kind == TokKind::Symbol
            && text == "@"
            && toks
                .get(i + 1)
                .is_some_and(|n| n.text(src) == "[" && n.start == tok.end)
        {
            let mut depth = 0;

            for k in i + 1..toks.len() {
                let t = toks[k];

                // A list still being typed ends at its line.
                if depth == 1 && src[toks[k - 1].end as usize..t.start as usize].contains('\n') {
                    break;
                }

                match t.text(src) {
                    "[" | "(" | "{" => depth += 1,

                    "]" | ")" | "}" => depth -= 1,

                    _ if t.kind == TokKind::Ident
                        && depth == 1
                        && matches!(toks[k - 1].text(src), "[" | ",") =>
                    {
                        push(t.start, t.end, "decorator", 0)
                    }

                    _ => {}
                }

                if depth == 0 {
                    break;
                }
            }

            i += 2;

            continue;
        }

        // `@Contracted` and `$triple`: the name draws, and the sigil
        // stays with the grammar, which gives it a punctuation scope.
        // `@serde.rename` is one attribute, so its whole path draws as one.
        if tok.kind == TokKind::Symbol && matches!(text, "@" | "$") {
            let name = match toks.get(i + 1) {
                Some(n) if n.kind == TokKind::Ident && n.start == tok.end => *n,

                _ => {
                    i += 1;

                    continue;
                }
            };
            let mut end = i + 1;

            while text == "@"
                && toks
                    .get(end + 1)
                    .is_some_and(|d| d.kind == TokKind::Dot && d.start == toks[end].end)
                && toks
                    .get(end + 2)
                    .is_some_and(|n| n.kind == TokKind::Ident && n.start == toks[end + 1].end)
            {
                end += 2;
            }

            push(
                name.start,
                toks[end].end,
                if text == "@" { "decorator" } else { "macro" },
                0,
            );
            i = end + 1;

            continue;
        }

        if tok.kind != TokKind::Ident {
            i += 1;

            continue;
        }

        // The name a hole reads. The child sees the same bytes, and it
        // draws nothing inside a string.
        if in_hole && matches!(toks[i - 1].kind, TokKind::InterpHead | TokKind::InterpMid) {
            push(tok.start, tok.end, "variable", 0);
            i += 1;

            continue;
        }

        // `local trait = 1`: a contextual word away from its construct
        // is a plain name. The child paints no local, so without a token
        // the grammar paints the word as the keyword it spells. A member,
        // `Instance.new`, and a declared name keep the walks below.
        let after = |w: &[&str]| i > 0 && w.contains(&toks[i - 1].text(src));

        if alloy_syntax::contextual::is_contextual(text)
            && !after(&[".", ":", "function"])
            && !alloy_syntax::contextual::keyword_at(src, toks, i)
        {
            push(tok.start, tok.end, "variable", 0);
            i += 1;

            continue;
        }

        // The head of a `macro` or an `attribute` declaration. The emit
        // keeps neither, so the child draws nothing on the line: the
        // name reads as its call site does, and the list holds
        // parameters.
        if matches!(text, "macro" | "attribute")
            && toks.get(i + 1).is_some_and(|n| n.kind == TokKind::Ident)
            && alloy_syntax::contextual::keyword_at(src, toks, i)
        {
            let name = toks[i + 1];
            push(
                name.start,
                name.end,
                match text {
                    "macro" => "macro",

                    _ => "decorator",
                },
                0,
            );
            i += 2;

            // `(x)` and `(min: number, max: number)`: the name that
            // opens each entry of the list.
            if toks.get(i).is_some_and(|t| t.kind == TokKind::LParen) {
                let mut depth = 0i32;

                while let Some(t) = toks.get(i) {
                    let opens =
                        matches!(toks[i - 1].kind, TokKind::LParen) || toks[i - 1].text(src) == ",";

                    match t.kind {
                        TokKind::LParen => depth += 1,

                        TokKind::RParen => depth -= 1,

                        TokKind::Ident if depth == 1 && opens => {
                            push(t.start, t.end, "parameter", 0)
                        }

                        _ => {}
                    }

                    i += 1;

                    if depth == 0 {
                        break;
                    }
                }
            }

            // `attribute service on impl as ... end`: the contract body
            // holds the clauses of the members an `impl` has to write.
            // The emit keeps no attribute, so the child draws nothing on
            // them either.
            if text == "attribute"
                && let Some((first, past)) = contract_body(toks, src, i)
            {
                // A legend without `modifier` still paints the
                // visibility: the word is a keyword of the clause.
                let visibility = match type_index(types, "modifier") {
                    Some(_) => "modifier",

                    None => "keyword",
                };
                let mut depth = 0i32;

                for k in first..past {
                    let t = toks[k];
                    let word = t.text(src);
                    let after = |w: &str| toks[k - 1].text(src) == w;

                    match t.kind {
                        TokKind::LParen => depth += 1,

                        TokKind::RParen => depth -= 1,

                        // The name of the member, and the list parameter
                        // an `each` clause writes one member per entry
                        // of.
                        TokKind::Ident if after("function") || after("field") || after("each") => {
                            let name = if word == "each" {
                                "keyword"
                            } else if after("each") {
                                "parameter"
                            } else if after("field") {
                                "property"
                            } else {
                                "method"
                            };

                            push(t.start, t.end, name, 0);
                        }

                        TokKind::Ident if depth > 0 && (after("(") || after(",")) => {
                            push(t.start, t.end, "parameter", 0);
                        }

                        TokKind::Ident if matches!(word, "requires" | "each") => {
                            push(t.start, t.end, "keyword", 0);
                        }

                        TokKind::Ident if matches!(word, "public" | "private") => {
                            push(t.start, t.end, visibility, 0);
                        }

                        TokKind::Ident if matches!(word, "function" | "field") => {
                            push(t.start, t.end, "keyword", 0);
                        }

                        _ => {}
                    }
                }

                i = past;

                continue;
            }

            continue;
        }

        // `function helper(` and `function map<U>(`: the name a
        // declaration gives. The child paints the call sites and leaves
        // the declaration alone. A head whose first parameter is `self`
        // is a method, the way its call sites paint.
        if i > 0
            && toks[i - 1].text(src) == "function"
            && let Some(open) = (i + 1..toks.len().min(i + 12))
                .take_while(|k| *k == i + 1 || toks[i + 1].text(src) == "<")
                .find(|k| toks[*k].kind == TokKind::LParen)
        {
            let kind = match toks.get(open + 1).map(|t| t.text(src)) {
                Some("self") => "method",

                _ => "function",
            };
            push(tok.start, tok.end, kind, declaration);
            i += 1;

            continue;
        }

        // A dotted path, from a segment no `.` stands in front of: each
        // prefix of it may name a declaration.
        let mut segments = vec![tok];
        let mut j = i;

        while toks.get(j + 1).is_some_and(|t| t.kind == TokKind::Dot)
            && let Some(next) = toks.get(j + 2)
            && next.kind == TokKind::Ident
        {
            segments.push(*next);
            j += 2;
        }

        let mut path = String::new();

        for (k, segment) in segments.iter().enumerate() {
            let parent = path.clone();

            if k > 0 {
                path.push('.');
            }

            path.push_str(segment.text(src));

            let name = match declared_kind(doc, &path) {
                Some(kind) => kind,

                // A variant stands under no keyword of its own; the
                // enum in front of it says what the segment is. A
                // function of its `impl` is no variant.
                None if declared_kind(doc, &parent) == Some("enum") => {
                    impl_function_kind(doc, &parent, segment.text(src)).unwrap_or("enumMember")
                }

                // `Ns.T`: the emit writes one flat name, `Ns_T`, so the
                // child paints nothing on the word the source wrote for
                // the member. The declaration says what it is.
                None if k > 0 => match member_kind(doc, &path) {
                    Some(kind) => kind,

                    None => continue,
                },

                None => continue,
            };
            push(segment.start, segment.end, name, 0);
        }

        i = j + 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::EmitOptions;

    /// The legend the child sends, in the order the protocol lists.
    fn legend() -> Vec<String> {
        [
            "namespace",
            "type",
            "class",
            "enum",
            "interface",
            "struct",
            "typeParameter",
            "parameter",
            "variable",
            "property",
            "enumMember",
            "event",
            "function",
            "method",
            "macro",
            "keyword",
            "modifier",
            "comment",
            "string",
            "number",
            "regexp",
            "operator",
            "decorator",
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// `match e as name with` names the value. The alias is a local of
    /// the match, so the proxy draws nothing on it and nothing on the
    /// `as`: the grammar paints the word, and the child the name.
    #[test]
    fn a_match_alias_draws_no_token_of_its_own() {
        const SRC: &str = "struct Pt as\n    x: number,\nend\n\nlocal function f(p: Pt): number\n    match p as pt with\n        case Pt { x } then\n            return pt.x\n    end\n\n    return 0\nend\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let drawn = alloy_tokens(&doc, &types, &[]);
        let at = |needle: &str, word: &str| {
            let (line, column) = position_of(SRC, SRC.find(needle).expect(needle));
            let column = column + needle.find(word).expect(word) as u32;

            drawn.iter().find(|t| t.0 == line && t.1 == column).copied()
        };

        // A name the proxy does draw, so the three below read as a
        // rule and not as an empty answer.
        let function = type_index(&types, "function").expect("the type");

        assert_eq!(
            at(" f(p: Pt)", "f").map(|t| t.3),
            Some(function),
            "{drawn:?}"
        );

        assert_eq!(at("match p as pt with", "as"), None, "{drawn:?}");
        assert_eq!(at("match p as pt with", "pt"), None, "{drawn:?}");
        assert_eq!(at("return pt.x", "pt"), None, "{drawn:?}");
    }

    /// A contextual word used as a name draws as a variable, so the
    /// grammar's keyword color does not show. The same word as its
    /// construct, and a member after a `.`, draw nothing here.
    #[test]
    fn a_contextual_word_as_a_name_draws_as_a_variable() {
        const SRC: &str = "local trait = 1\nlocal remote = 2\nprint(trait, remote)\ntrait Show\n    function show(self): string\nend\nlocal p = Instance.new(\"Part\")\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let variable = type_index(&types, "variable").expect("the type");
        let drawn: Vec<(u32, u32)> = alloy_tokens(&doc, &types, &[])
            .into_iter()
            .filter(|t| t.3 == variable)
            .map(|t| (t.0, t.1))
            .collect();

        assert_eq!(drawn, [(0, 6), (1, 6), (2, 6), (2, 13)]);
    }

    /// The words of a `match` and of a guard draw as names where they
    /// are names, the way `trait` does, and draw nothing as keywords.
    #[test]
    fn a_match_word_as_a_name_draws_as_a_variable() {
        const SRC: &str = "local where = 1\nlocal case, default, with = 2, 3, 4\nmatch where with\n    case 1 then print(case)\n    case n where n > 1 then print(n)\n    default print(default, with)\nend\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let variable = type_index(&types, "variable").expect("the type");
        let drawn: Vec<(u32, u32)> = alloy_tokens(&doc, &types, &[])
            .into_iter()
            .filter(|t| t.3 == variable)
            .map(|t| (t.0, t.1))
            .collect();

        assert_eq!(
            drawn,
            [
                (0, 6),
                (1, 6),
                (1, 12),
                (1, 21),
                (2, 6),
                (3, 22),
                (5, 18),
                (5, 27)
            ]
        );
    }

    /// Every attribute name draws as one: the whole dotted path, and
    /// each name of Luau's list.
    #[test]
    fn every_attribute_form_draws_its_name() {
        const SRC: &str = "@[native, deprecated {use = \"g\"}]\nlocal function f() end\n@serde.rename_all(\"camelCase\")\nstruct S\n    @T.label(\"x\")\n    x: number\nend\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let decorator = type_index(&types, "decorator").expect("the type");
        let drawn: Vec<(u32, u32, u64)> = alloy_tokens(&doc, &types, &[])
            .into_iter()
            .filter(|t| t.3 == decorator)
            .map(|t| (t.0, t.1, t.2))
            .collect();

        assert_eq!(drawn, [(0, 2, 6), (0, 10, 10), (2, 1, 16), (4, 5, 7)]);
    }

    /// `match macro with`: the word is the value the match reads, so the
    /// `with` after it names no macro.
    #[test]
    fn a_contextual_scrutinee_draws_no_declaration() {
        const SRC: &str = "local macro = 3\nmatch macro with\n    case 3 then print(macro)\n    default print(0)\nend\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let kind = type_index(&types, "macro").expect("the type");
        let drawn = alloy_tokens(&doc, &types, &[]);

        assert!(!drawn.iter().any(|t| t.3 == kind), "{drawn:?}");
    }

    /// The emit gives a namespace member one flat name, `Ns_T`, so the
    /// child paints nothing on the word the source wrote for it. The
    /// declaration says what the member is.
    #[test]
    fn a_member_of_a_namespace_draws_under_its_own_kind() {
        const SRC: &str = "namespace Ns as\n    public struct T as\n        value: number,\n    end\n\n    public trait Show as\n        function show(self): string\n    end\nend\n\nlocal y: Ns.T = new Ns.T { value = 5 }\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let drawn = alloy_tokens(&doc, &types, &[]);
        let named = |name: &str| {
            let kind = type_index(&types, name).expect("the type");

            drawn
                .iter()
                .filter(|t| t.3 == kind)
                .map(|t| (t.0, t.1, t.2))
                .collect::<Vec<_>>()
        };
        let (line, column) = position_of(SRC, SRC.find("Ns.T").expect("the path"));

        assert!(named("namespace").contains(&(line, column, 2)), "{drawn:?}");
        assert!(
            named("struct").contains(&(line, column + 3, 1)),
            "{drawn:?}"
        );
        // A bare name keeps the child's own answer.
        assert!(named("struct").iter().all(|t| t.0 == line), "{drawn:?}");
    }

    /// An Alloy construct is no Luau, so the shadow holds no word where
    /// the author wrote one. The proxy draws those itself, in source
    /// positions, and the merge keeps the delta encoding valid.
    #[test]
    fn the_proxy_draws_the_alloy_constructs_itself() {
        const SRC: &str = "attribute Contracted on function\nmacro triple(x)\n    x * 3\nend\n\nnamespace Geo as\n    public enum Kind as\n        Round\n    end\nend\n\n@Contracted\nfunction f(): number\n    return $triple(2)\nend\n\nlocal k: Geo.Kind = Geo.Kind.Round\nmatch k with\n    case Geo.Kind.Round then\n        print(`hello {k}!`)\nend\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let drawn = alloy_tokens(&doc, &types, &[]);
        let named = |name: &str| {
            let kind = type_index(&types, name).expect("the type");

            drawn
                .iter()
                .filter(|t| t.3 == kind)
                .map(|t| (t.0, t.1, t.2))
                .collect::<Vec<_>>()
        };
        let line_of = |needle: &str| position_of(SRC, SRC.find(needle).expect(needle));

        // `@Contracted` and `$triple(2)`: the name alone, one column
        // past the sigil.
        assert!(named("decorator").contains(&{
            let (l, c) = line_of("@Contracted");

            (l, c + 1, 10)
        }));
        assert!(named("macro").contains(&{
            let (l, c) = line_of("$triple(2)");

            (l, c + 1, 6)
        }));

        // `case Geo.Kind.Round then`: the namespace, the enum, and the
        // variant, each at the column the author wrote.
        let (line, column) = line_of("case Geo.Kind.Round");
        let at = column + "case ".len() as u32;

        assert!(named("namespace").contains(&(line, at, 3)), "{drawn:?}");
        assert!(named("enum").contains(&(line, at + 4, 4)), "{drawn:?}");
        assert!(
            named("enumMember").contains(&(line, at + 9, 5)),
            "{drawn:?}"
        );

        // The name inside an interpolation hole.
        let (line, column) = line_of("{k}!");

        assert!(
            named("variable").contains(&(line, column + 1, 1)),
            "{drawn:?}"
        );

        // The merge sorts by line then column, so the deltas hold.
        let out = remap(&[], &doc, &types, &[]);
        let mut place = (0u64, 0u64);

        for chunk in out.chunks_exact(5) {
            if chunk[0] > 0 {
                place = (place.0 + chunk[0], chunk[1]);
            } else {
                place = (place.0, place.1 + chunk[1]);
            }
        }

        assert!(place.0 > 0 && !out.is_empty());
    }

    /// The child paints a call site and leaves the declaration's own
    /// name plain. The proxy paints the name with the `declaration`
    /// modifier: a function for a top-level or a local head, a method
    /// for a head whose first parameter is `self`.
    #[test]
    fn a_declaration_name_paints_its_kind_with_the_declaration_modifier() {
        const SRC: &str = concat!(
            "struct P as\n    x: number\nend\n",
            "impl P as\n    function get(self): number\n        return self.x\n    end\n",
            "    function map<U>(self, f: (number) -> U): U\n        return f(self.x)\n    end\n",
            "    function make(x: number): P\n        return new P { x = x }\n    end\nend\n",
            "function helper()\n    return 1\nend\n",
            "local function loc()\n    return helper()\nend\n",
        );
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let modifiers = ["definition".to_string(), "declaration".to_string()];
        let drawn = alloy_tokens(&doc, &types, &modifiers);
        let at = |needle: &str, skip: usize| {
            let (line, column) = position_of(SRC, SRC.find(needle).expect(needle) + skip);
            let token = drawn
                .iter()
                .find(|t| t.0 == line && t.1 == column)
                .unwrap_or_else(|| panic!("{needle}: {drawn:?}"));

            (types[token.3 as usize].as_str(), token.4)
        };

        assert_eq!(at("function get(", "function ".len()), ("method", 2));
        assert_eq!(at("function map<U>(", "function ".len()), ("method", 2));
        assert_eq!(at("function make(", "function ".len()), ("function", 2));
        assert_eq!(at("function helper(", "function ".len()), ("function", 2));
        assert_eq!(at("function loc(", "function ".len()), ("function", 2));
        // A call site keeps the child's own token.
        let (line, column) = position_of(SRC, SRC.find("return helper()").unwrap() + 7);
        assert!(
            !drawn.iter().any(|t| t.0 == line && t.1 == column),
            "{drawn:?}"
        );
    }

    /// A `macro` and an `attribute` declaration carry the name and the
    /// parameters the author wrote. The emit keeps neither line, so the
    /// child drew nothing on them: the call site read as a macro while
    /// its own declaration read as plain text.
    #[test]
    fn a_macro_and_an_attribute_declaration_carry_their_names() {
        const SRC: &str = concat!(
            "attribute range(min: number, max: number) on field\n",
            "macro double(x) x * 2 end\n",
            "local macro = 1\n",
            "print(macro)\n",
        );
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let drawn = alloy_tokens(&doc, &types, &[]);
        let at = |name: &str, needle: &str| {
            let kind = type_index(&types, name).expect("the type");
            let (line, column) = position_of(SRC, SRC.find(needle).expect(needle));

            drawn
                .iter()
                .any(|t| (t.0, t.1, t.3) == (line, column, kind))
        };

        assert!(at("decorator", "range("), "{drawn:?}");
        assert!(at("parameter", "min:"), "{drawn:?}");
        assert!(at("parameter", "max:"), "{drawn:?}");
        assert!(at("macro", "double("), "{drawn:?}");
        assert!(at("parameter", "x) x"), "{drawn:?}");

        // `local macro = 1` declares nothing: the word is a name there.
        // The child paints no local, so the name draws as a variable.
        let (line, _) = position_of(SRC, SRC.find("local macro").expect("the local"));
        let variable = type_index(&types, "variable").expect("the type");

        assert!(
            drawn
                .iter()
                .filter(|t| t.0 == line)
                .all(|t| t.3 == variable),
            "{drawn:?}"
        );
    }

    /// The contract body of an `attribute` declaration carries its own
    /// tokens. The emit keeps no attribute, so the child drew nothing on
    /// the clauses: the name and the uses painted, and the body between
    /// them read as plain text.
    #[test]
    fn an_attribute_contract_carries_its_clauses() {
        const SRC: &str = concat!(
            "enum Lifecycle as\n",
            "    Init,\n",
            "end\n",
            "\n",
            "attribute provider(lifecycles: Lifecycle[]) on impl as\n",
            "    requires public function Start(self)\n",
            "    requires private field state: number\n",
            "    requires function each lifecycles (self)\n",
            "end\n",
            "\n",
            "attribute plain on function\n",
        );
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let drawn = alloy_tokens(&doc, &types, &[]);
        let at = |needle: &str| {
            let (line, column) = position_of(SRC, SRC.find(needle).expect(needle));

            drawn
                .iter()
                .find(|t| (t.0, t.1) == (line, column))
                .map(|t| types[t.3 as usize].as_str())
        };

        assert_eq!(at("requires public"), Some("keyword"), "{drawn:?}");
        assert_eq!(at("public function"), Some("modifier"), "{drawn:?}");
        assert_eq!(at("function Start"), Some("keyword"), "{drawn:?}");
        assert_eq!(at("Start(self)"), Some("method"), "{drawn:?}");
        assert_eq!(at("self)\n    requires private"), Some("parameter"));

        // A `field` clause names a property, and an `each` clause names
        // the list parameter that writes one member per entry.
        assert_eq!(at("private field"), Some("modifier"), "{drawn:?}");
        assert_eq!(at("field state"), Some("keyword"), "{drawn:?}");
        assert_eq!(at("state: number"), Some("property"), "{drawn:?}");
        assert_eq!(at("each lifecycles ("), Some("keyword"), "{drawn:?}");
        assert_eq!(at("lifecycles (self)"), Some("parameter"), "{drawn:?}");

        // `attribute plain on function` opens no body, so the `function`
        // of its target line stays a word of the grammar.
        assert_eq!(at("function\n"), None, "{drawn:?}");
    }

    #[test]
    fn tokens_in_generated_text_go_and_the_rest_move() {
        let src = "local v = a ?? 0\nprint(v)\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        // Shadow: `local v = (if a == nil then 0 else a)` / `print(v)`.
        // Tokens: `local` (0,0,5), `nil` inside the generated text (0,18,3),
        // `print` (1,0,5).
        let data = [0, 0, 5, 1, 0, 0, 18, 3, 2, 0, 1, 0, 5, 3, 0];
        let out = remap(&data, &doc, &[], &[]);
        assert_eq!(out, vec![0, 0, 5, 1, 0, 1, 0, 5, 3, 0]);
    }

    /// `self` carries no semantic token, so the grammar's own scope for
    /// the word paints it wherever it stands.
    #[test]
    fn a_self_token_goes_and_the_rest_move() {
        let src = "struct P as\n    x: number\nend\nimpl P as\n    function get(self): number\n        return self.x\n    end\nend\nprint(new P { x = 1 })\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let line = doc.shadow.lines().nth(5).unwrap();
        let col = line.find("self").unwrap() as u64;
        // `self` on the return line, then `x` right after the dot.
        let data = [5, col, 4, 9, 0, 0, 5, 1, 9, 0];
        let out = remap(&data, &doc, &[], &[]);
        assert_eq!(out.len(), 5, "{out:?}");
        let source_line = src.lines().nth(5).unwrap();
        let x_col = source_line.find(".x").unwrap() as u64 + 1;
        assert_eq!(out, vec![5, x_col, 1, 9, 0]);
    }

    /// The child reads the array shorthand `{ T }` as a table with an
    /// implicit `[number]` index. It gives that index a token at the
    /// brace, six units wide, which is the width of the word `number`.
    /// The brace names no type, so the token goes and the names stay. A
    /// nested shorthand carries one such token per brace.
    #[test]
    fn a_type_token_that_opens_on_a_brace_goes() {
        const SRC: &str = concat!(
            "local function f(x: { number }) -> ()\nend\n",
            "local function g(y: { { number } }) -> ()\nend\n",
        );
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let kind = type_index(&types, "type").expect("the type");
        // The child's answer over the shadow: a token on each brace of a
        // shorthand and one on each name, all six units wide.
        let plain = doc.shadow.find("{ number }").expect("the type");
        let nested = doc.shadow.find("{ { number } }").expect("the nested type");
        let spots = [plain, plain + 2, nested, nested + 2, nested + 4];
        let mut data = Vec::new();
        let (mut pl, mut pc) = (0u32, 0u32);

        for at in spots {
            let (l, c) = position_of(&doc.shadow, at);
            let dl = l - pl;
            let dc = if dl > 0 { c } else { c - pc };
            data.extend_from_slice(&[u64::from(dl), u64::from(dc), 6, kind, 0]);
            (pl, pc) = (l, c);
        }

        let out = remap(&data, &doc, &types, &[]);
        let mut drawn: Vec<(&str, u32, u32)> = Vec::new();
        let (mut line, mut column) = (0u32, 0u32);

        for t in out.chunks_exact(5) {
            line += t[0] as u32;
            column = match t[0] > 0 {
                true => t[1] as u32,

                false => column + t[1] as u32,
            };

            if t[3] == kind {
                let at = offset_of(SRC, line, column).expect("the offset");
                drawn.push((&SRC[at..at + t[2] as usize], line, column));
            }
        }

        assert_eq!(drawn, [("number", 0, 22), ("number", 2, 24)], "{drawn:?}");
    }

    /// An `.alx` file with no markup maps like any other: the tokens
    /// keep their place. The proxy once answered every `.alx` request
    /// with an empty list, so even this file drew nothing.
    #[test]
    fn an_alx_file_with_no_markup_keeps_its_tokens() {
        let src = "local function add(a: number, b: number): number\n    return a + b\nend\n";
        let options = EmitOptions {
            file_name: "plain.alx".into(),
            ..EmitOptions::default()
        };
        let doc = Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        );
        let col = src.lines().next().unwrap().find("number").unwrap() as u64;
        // `number` on the first line, then the second `number` after it.
        let data = [0, col, 6, 1, 0, 0, 9, 6, 1, 0];
        assert_eq!(
            remap(&data, &doc, &[], &[]),
            vec![0, col, 6, 1, 0, 0, 9, 6, 1, 0]
        );
    }

    /// The map crosses the markup lowering byte for byte, so a token of
    /// the emitted call lands on the text the author wrote: the code of
    /// a hole, and the string an attribute is given.
    #[test]
    fn markup_tokens_land_on_the_text_the_author_wrote() {
        let src = "local function create(c) return function(p) return p end end\nlocal props = { name = \"a\" }\nlocal x = <Frame Name={props.name} Size=\"s\" />\nprint(x)\n";
        let options = EmitOptions {
            file_name: "ui.alx".into(),
            ..EmitOptions::default()
        };
        let jsx =
            alloy::luaux::Config::parse("[factory]\nbackend = \"table\"\ncreate = \"create\"\n")
                .unwrap();
        let doc = Doc::new(src.to_string(), 1, &options, &jsx, None);
        let shadow = doc.shadow.clone();
        assert!(shadow.contains("create(\"Frame\")"), "{shadow}");
        // In the shadow's third line: `props` inside the lowered call, and
        // the string `"s"` after it.
        let line = shadow.lines().nth(2).unwrap();
        let col = line.find("props").unwrap() as u64;
        let str_col = line.find("\"s\"").unwrap() as u64;
        let data = [1, 6, 5, 8, 0, 1, col, 5, 8, 0, 0, str_col - col, 3, 18, 0];
        let out = remap(&data, &doc, &[], &[]);
        // `props` on line 2 stays at 6, `props` on line 3 lands on the
        // hole's `props`, and the string on the `"s"` of the attribute.
        assert_eq!(out.len(), 15, "{out:?}");
        assert_eq!(&out[..5], &[1, 6, 5, 8, 0]);
        let third = src.lines().nth(2).unwrap();
        let props_col = third.find("props").unwrap() as u64;
        let quote_col = third.find("\"s\"").unwrap() as u64;
        assert_eq!(&out[5..10], &[1, props_col, 5, 8, 0]);
        assert_eq!(&out[10..], &[0, quote_col - props_col, 3, 18, 0]);
    }

    /// A macro argument takes the tokens of its code copy. `$assert(x)`
    /// lowers to `assert(x, "assertion failed: x")`, all generated, so
    /// `string.upper` and `t.k` went uncoloured. `$nameof` writes no
    /// copy, and a word in a message string is no copy either.
    #[test]
    fn a_macro_argument_takes_the_tokens_of_its_code_copy() {
        let src = "local t = { k = 'v' }\n$assert(string.upper(t.k) == 'V')\nprint($nameof(t.k))\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let shadow = doc.shadow.lines().nth(1).unwrap();
        let code = shadow.find("string.upper").unwrap() as u64;
        let message = shadow.rfind("string.upper").unwrap() as u64;
        let k = shadow.find("t.k").unwrap() as u64 + 2;
        // `string` in the copy, `k` in the copy, then `string` in the
        // message string.
        let data = [
            1,
            code,
            6,
            8,
            0,
            0,
            k - code,
            1,
            9,
            0,
            0,
            message - k,
            6,
            8,
            0,
        ];
        let out = remap(&data, &doc, &[], &[]);
        let line = src.lines().nth(1).unwrap();
        let at = line.find("string").unwrap() as u64;
        let at_k = line.find("t.k").unwrap() as u64 + 2;

        assert_eq!(out, vec![1, at, 6, 8, 0, 0, at_k - at, 1, 9, 0]);
    }

    /// A function of an enum's `impl` draws as a method, not a variant.
    /// `$nameof(Mode.describe)` has no code copy, so the proxy's own
    /// token is the one the editor shows there.
    #[test]
    fn an_enum_method_draws_as_a_method() {
        const SRC: &str = "enum Mode\n  Fast,\nend\n\nimpl Mode\n  function describe(self): string\n    return 'mode'\n  end\nend\n\nprint($nameof(Mode.describe), Mode.Fast)\n";
        let doc = Doc::new(
            SRC.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let types = legend();
        let drawn = alloy_tokens(&doc, &types, &[]);
        let kind = |needle: &str| {
            let (line, column) = position_of(SRC, SRC.rfind(needle).expect(needle));

            drawn
                .iter()
                .find(|t| t.0 == line && t.1 == column)
                .map(|t| types[t.3 as usize].as_str())
        };

        assert_eq!(kind("describe)"), Some("method"), "{drawn:?}");
        assert_eq!(kind("Fast)"), Some("enumMember"), "{drawn:?}");
    }
}
