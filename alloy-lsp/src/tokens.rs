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

pub fn remap(data: &[u64], doc: &Doc, types: &[String]) -> Vec<u64> {
    let Some(out) = doc.mapping() else {
        return data.to_vec();
    };

    let mut line = 0u64;
    let mut start = 0u64;
    let mut tokens: Vec<Token> = Vec::new();

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

        if out.map.is_generated(first as u32) || out.map.is_generated(last as u32) {
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

        let (sl, sc) = doc.to_source(l, s);
        tokens.push((sl, sc, len, kind, mods));
    }

    // The child's tokens stand first, so a word both of them describe
    // keeps the child's reading.
    tokens.extend(alloy_tokens(doc, types));
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
    doc.decls
        .iter()
        .chain(doc.import_decls.iter())
        .filter(|d| d.name == path)
        .find_map(|d| {
            let mut line = d.hover.lines().nth(1)?.trim_start();

            for word in ["export ", "global ", "public ", "private "] {
                line = line.strip_prefix(word).unwrap_or(line);
            }

            ["namespace", "enum"]
                .into_iter()
                .find(|k| line.starts_with(&format!("{k} ")))
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
fn alloy_tokens(doc: &Doc, types: &[String]) -> Vec<Token> {
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
    let mut push = |start: u32, end: u32, name: &str| {
        let Some(kind) = type_index(types, name) else {
            return;
        };
        let (line, column) = position_of(src, start as usize);
        let width = src[start as usize..end as usize].encode_utf16().count() as u64;
        out.push((line, column, width, kind, 0));
    };
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

        // `@Contracted` and `$triple`: the sigil and the name are one
        // word to the reader.
        if tok.kind == TokKind::Symbol && matches!(text, "@" | "$") {
            let name = match toks.get(i + 1) {
                Some(n) if n.kind == TokKind::Ident && n.start == tok.end => *n,

                _ => {
                    i += 1;

                    continue;
                }
            };
            push(
                tok.start,
                name.end,
                if text == "@" { "decorator" } else { "macro" },
            );
            i += 2;

            continue;
        }

        if tok.kind != TokKind::Ident {
            i += 1;

            continue;
        }

        // The name a hole reads. The child sees the same bytes, and it
        // draws nothing inside a string.
        if in_hole && matches!(toks[i - 1].kind, TokKind::InterpHead | TokKind::InterpMid) {
            push(tok.start, tok.end, "variable");
            i += 1;

            continue;
        }

        // The head of a `macro` or an `attribute` declaration. The emit
        // keeps neither, so the child draws nothing on the line: the
        // name reads as its call site does, and the list holds
        // parameters.
        if matches!(text, "macro" | "attribute")
            && toks.get(i + 1).is_some_and(|n| n.kind == TokKind::Ident)
        {
            let name = toks[i + 1];
            push(
                name.start,
                name.end,
                match text {
                    "macro" => "macro",

                    _ => "decorator",
                },
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

                        TokKind::Ident if depth == 1 && opens => push(t.start, t.end, "parameter"),

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

                            push(t.start, t.end, name);
                        }

                        TokKind::Ident if depth > 0 && (after("(") || after(",")) => {
                            push(t.start, t.end, "parameter");
                        }

                        TokKind::Ident if matches!(word, "requires" | "each") => {
                            push(t.start, t.end, "keyword");
                        }

                        TokKind::Ident if matches!(word, "public" | "private") => {
                            push(t.start, t.end, visibility);
                        }

                        TokKind::Ident if matches!(word, "function" | "field") => {
                            push(t.start, t.end, "keyword");
                        }

                        _ => {}
                    }
                }

                i = past;

                continue;
            }

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
                // enum in front of it says what the segment is.
                None if declared_kind(doc, &parent) == Some("enum") => "enumMember",

                None => continue,
            };
            push(segment.start, segment.end, name);
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
        let drawn = alloy_tokens(&doc, &types);
        let named = |name: &str| {
            let kind = type_index(&types, name).expect("the type");

            drawn
                .iter()
                .filter(|t| t.3 == kind)
                .map(|t| (t.0, t.1, t.2))
                .collect::<Vec<_>>()
        };
        let line_of = |needle: &str| position_of(SRC, SRC.find(needle).expect(needle));

        // `@Contracted` and `$triple(2)`, sigil and name as one word.
        assert!(named("decorator").contains(&{
            let (l, c) = line_of("@Contracted");

            (l, c, 11)
        }));
        assert!(named("macro").contains(&{
            let (l, c) = line_of("$triple(2)");

            (l, c, 7)
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
        let out = remap(&[], &doc, &types);
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
        let drawn = alloy_tokens(&doc, &types);
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

        // `local macro = 1` declares nothing: the word is a name there,
        // and the child reads it.
        let (line, _) = position_of(SRC, SRC.find("local macro").expect("the local"));

        assert!(!drawn.iter().any(|t| t.0 == line), "{drawn:?}");
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
        let drawn = alloy_tokens(&doc, &types);
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
        let out = remap(&data, &doc, &[]);
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
        let out = remap(&data, &doc, &[]);
        assert_eq!(out.len(), 5, "{out:?}");
        let source_line = src.lines().nth(5).unwrap();
        let x_col = source_line.find(".x").unwrap() as u64 + 1;
        assert_eq!(out, vec![5, x_col, 1, 9, 0]);
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
            remap(&data, &doc, &[]),
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
        let out = remap(&data, &doc, &[]);
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
}
