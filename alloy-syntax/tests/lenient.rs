/*!
The lenient parser's contract, held against broken input. Every parse must
cover every token with no holes, print back byte for byte, and finish. The
mutation sweeps are the fuzz that the recovery invariant answers to.
*/

use alloy_syntax::ast::Stmt;
use alloy_syntax::parser::{self, ParseOptions};
use alloy_syntax::{lexer, printer};

/// Parses leniently and checks the three properties.
#[track_caller]
fn lenient(src: &str) -> (usize, usize) {
    let Ok(lexed) = lexer::lex(src) else {
        return (0, 0);
    };

    let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    let holes = printer::coverage_errors(&chunk);
    assert!(holes.is_empty(), "coverage holes {holes:?}\nsource:\n{src}");
    assert_eq!(
        printer::print_chunk(src, &lexed.toks, &chunk),
        src,
        "round trip differed\nsource:\n{src}"
    );
    assert!(
        diagnostics.len() <= parser::MAX_DIAGNOSTICS,
        "diagnostics stay under the cap"
    );

    let errors = chunk
        .block
        .stmts
        .iter()
        .filter(|s| matches!(s, Stmt::Error(_)))
        .count();

    (errors, diagnostics.len())
}

#[test]
fn valid_source_has_no_error_nodes() {
    let (errors, diagnostics) = lenient("local x = 1\nprint(x)\n");
    assert_eq!((errors, diagnostics), (0, 0));
}

#[test]
fn a_broken_statement_becomes_one_error_node() {
    let src = "local x = 1\nlocal = 2\nprint(x)\n";
    let (errors, diagnostics) = lenient(src);
    assert_eq!(errors, 1);
    assert_eq!(diagnostics, 1);

    // The statements around it still parse as themselves.
    let lexed = lexer::lex(src).unwrap();
    let (chunk, _) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    assert!(matches!(chunk.block.stmts[0], Stmt::Local(_)));
    assert!(matches!(chunk.block.stmts[1], Stmt::Error(_)));
    assert!(matches!(chunk.block.stmts[2], Stmt::Call(..)));
}

#[test]
fn a_stray_end_at_the_top_level_is_an_error_node() {
    let (errors, diagnostics) = lenient("local x = 1\nend\nlocal y = 2\n");
    assert_eq!(errors, 1);
    assert_eq!(diagnostics, 1);
}

#[test]
fn an_unfinished_expression_stops_at_the_next_line() {
    let src = "local x =\nlocal y = 2\n";
    let (errors, _) = lenient(src);
    assert_eq!(errors, 1);

    let lexed = lexer::lex(src).unwrap();
    let (chunk, _) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    assert!(
        matches!(chunk.block.stmts[1], Stmt::Local(_)),
        "the next line parses"
    );
}

#[test]
fn nested_errors_keep_the_enclosing_block() {
    let src = "if x then\n\tlocal = 1\n\tprint(1)\nend\nprint(2)\n";
    let lexed = lexer::lex(src).unwrap();
    let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(
        matches!(chunk.block.stmts[0], Stmt::If(_)),
        "the if survives"
    );
    assert!(matches!(chunk.block.stmts[1], Stmt::Call(..)));
    assert!(printer::coverage_errors(&chunk).is_empty());
}

#[test]
fn the_diagnostic_cap_holds() {
    let src = "local = 1\n".repeat(parser::MAX_DIAGNOSTICS + 50);
    let (_, diagnostics) = lenient(&src);
    assert_eq!(diagnostics, parser::MAX_DIAGNOSTICS);
}

const CORPUS: &[&str] = &[
    "local x = 1\nprint(x)\n",
    "enum Phase as\n\tLobby\n\tPlaying(round: number)\nend\n",
    "if a then b() elseif c then d() else e() end\n",
    "for i = 1, 10 do t[i] = i * 2 end\n",
    "local function f(a: number, b: string?): boolean return a > #b end\n",
    "local t = { a = 1, [k] = 2, 3, f'x', g{}, }\n",
    "while true do if x then break end continue end\n",
    "return (a or b)(c):d(1)[2] .. 'e' ?? f\n",
    "x ??= y\nt[f()] ??= 1\n",
    "local s = `a {b} c` .. [[d]]\n",
    "type T = { read x: number?, y: (a: string) -> () }\n",
];

/// Byte-level mutations must never panic, must tile, and must round trip.
#[test]
fn mutations_hold_the_contract() {
    let interesting = b"\"'`[]{}()\\\n-=?!$:.,";
    let mut checked = 0usize;

    for src in CORPUS {
        let bytes = src.as_bytes();

        for pos in 0..bytes.len() {
            for &b in interesting {
                let mut m = bytes.to_vec();
                m[pos] = b;

                if let Ok(text) = String::from_utf8(m) {
                    lenient(&text);
                    checked += 1;
                }
            }
        }
    }

    assert!(checked > 1000, "mutation count {checked}");
}

/// Every prefix of the corpus holds the contract too.
#[test]
fn truncations_hold_the_contract() {
    for src in CORPUS {
        for cut in 0..src.len() {
            if src.is_char_boundary(cut) {
                lenient(&src[..cut]);
            }
        }
    }
}

/// Deletions of one token's worth of text, which is what a keystroke does.
#[test]
fn deletions_hold_the_contract() {
    for src in CORPUS {
        for cut in 0..src.len() {
            for len in 1..=3 {
                if cut + len <= src.len()
                    && src.is_char_boundary(cut)
                    && src.is_char_boundary(cut + len)
                {
                    let text = format!("{}{}", &src[..cut], &src[cut + len..]);
                    lenient(&text);
                }
            }
        }
    }
}

/// `as` closes an `impl` and a `trait` header. A header without it
/// reports on the header, and the body still parses, so the editor
/// keeps working while the author migrates the file.
#[test]
fn a_header_without_as_reports_and_still_parses() {
    for (src, head) in [
        ("impl Test\n\tfunction f(self) end\nend\n", "impl Test"),
        (
            "impl Shape for Test\n\tfunction f(self) end\nend\n",
            "impl Shape for Test",
        ),
        (
            "trait Shape\n\tfunction area(self): number\nend\n",
            "trait Shape",
        ),
        ("impl Box<T>\n\tfunction f(self) end\nend\n", "impl Box<T>"),
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(
            diagnostics.len(),
            1,
            "one report for {src:?}, got {diagnostics:?}"
        );
        assert_eq!(
            diagnostics[0].message,
            format!("`{head}` needs `as` before its body")
        );
        assert_eq!(diagnostics[0].offset, 0, "the report sits on the header");
        assert!(
            !chunk
                .block
                .stmts
                .iter()
                .any(|s| matches!(s, Stmt::Error(_))),
            "the body still parses for {src:?}"
        );
    }
}

/// A trait is one shape, so its header takes no type parameters. The
/// report names that, and the body still parses.
#[test]
fn a_generic_trait_header_reports_the_type_parameters() {
    let src = "trait Container<T> as\n\tfunction get(self): number\nend\n";
    let lexed = lexer::lex(src).unwrap();
    let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    assert_eq!(diagnostics.len(), 1, "one report, got {diagnostics:?}");
    assert_eq!(
        diagnostics[0].message,
        "a trait takes no type parameters; put `<T>` on the method"
    );
    assert_eq!(diagnostics[0].offset, 15, "the report sits on the `<`");
    assert!(
        !chunk
            .block
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::Error(_))),
        "the body still parses"
    );
}

/// A body with no `end` stops at the next statement, so the file after
/// it still parses. The editor needs `later` for hover and completion.
#[test]
fn a_body_with_no_end_keeps_the_statements_after_it() {
    let tail = "\nlocal function later(a: number, b: number): number\n    return a + b\nend\n\nprint(later(1, 2))\n";

    for head in [
        "struct S as",
        "interface I as",
        "enum E as",
        "trait T as",
        "impl S as",
        "namespace N as",
    ] {
        let src = format!("{head}{tail}");
        let (_, diagnostics) = lenient(&src);
        assert_eq!(diagnostics, 1, "one report for {head:?}");

        let lexed = lexer::lex(&src).unwrap();
        let (chunk, _) = parser::parse_lenient(&src, &lexed.toks, ParseOptions::default());
        assert!(
            chunk
                .block
                .stmts
                .iter()
                .any(|s| matches!(s, Stmt::LocalFunction(_))),
            "`later` still reads after {head:?}"
        );
    }
}

/// A body that holds members and no `end` reports once, against the
/// keyword that opened it. The members stay and the file after the body
/// still parses, so the editor reads `later`.
#[test]
fn a_body_with_members_and_no_end_reports_once() {
    let tail = "\nfunction later(): number\n    return 1\nend\n";

    for (head, opener) in [
        ("struct S as\n    a: number", "struct"),
        ("interface I as\n    a: number", "interface"),
        ("enum E as\n    A\n    B", "enum"),
        ("trait T as\n    function m(self): number", "trait"),
        ("impl S as\n    function f(self) end", "impl"),
        ("namespace N as\n    local q = 1", "namespace"),
    ] {
        let src = format!("{head}{tail}");
        let (_, count) = lenient(&src);
        assert_eq!(count, 1, "one report for {opener}");

        let lexed = lexer::lex(&src).unwrap();
        let (chunk, diagnostics) =
            parser::parse_lenient(&src, &lexed.toks, ParseOptions::default());
        assert!(
            chunk
                .block
                .stmts
                .iter()
                .any(|s| matches!(s, Stmt::Function(_))),
            "`later` still reads after {opener}"
        );

        // `impl` and `namespace` hold statements, so their bodies recover
        // through the statement path and report at the end of the file.
        if matches!(opener, "impl" | "namespace") {
            continue;
        }

        assert_eq!(
            diagnostics[0].message,
            format!("`{opener}` on line 1 needs an `end`"),
            "the report names the missing `end`"
        );
        assert_eq!(diagnostics[0].offset, 0, "the report sits on {opener}");
    }

    // The members the body held stay in the tree.
    let src = format!("enum E as\n    A\n    B{tail}");
    let lexed = lexer::lex(&src).unwrap();
    let (chunk, _) = parser::parse_lenient(&src, &lexed.toks, ParseOptions::default());
    let Some(Stmt::Enum(e)) = chunk.block.stmts.first() else {
        panic!("the enum still reads");
    };
    assert_eq!(e.variants.len(), 2, "`A` and `B` stay");

    let src = format!("trait T as\n    function m(self): number{tail}");
    let lexed = lexer::lex(&src).unwrap();
    let (chunk, _) = parser::parse_lenient(&src, &lexed.toks, ParseOptions::default());
    let Some(Stmt::Trait(t)) = chunk.block.stmts.first() else {
        panic!("the trait still reads");
    };
    assert_eq!(t.methods.len(), 1, "the signature stays");
}

/// An indented statement after the last member is the file going on, not
/// a member: the body with no `end` reports once and the tail still parses.
#[test]
fn an_indented_tail_after_a_body_with_no_end_reports_once() {
    for head in [
        "enum E as\n    A\n    B\n",
        "struct S as\n    a: number\n",
        "interface I as\n    a: number\n",
        "trait T as\n    function m(self): number\n",
    ] {
        let src = format!("{head}\n    local x = 1\n    print(x)\n");
        assert_eq!(lenient(&src), (0, 1), "one report for {head:?}");

        let lexed = lexer::lex(&src).unwrap();
        let (chunk, _) = parser::parse_lenient(&src, &lexed.toks, ParseOptions::default());
        assert!(
            chunk
                .block
                .stmts
                .iter()
                .any(|s| matches!(s, Stmt::Local(_))),
            "`x` still reads after {head:?}"
        );
    }
}

/// The `as` form reports nothing.
#[test]
fn a_header_with_as_is_clean() {
    for src in [
        "impl Test as\n\tfunction f(self) end\nend\n",
        "impl Shape for Test as\n\tfunction f(self) end\nend\n",
        "trait Shape as\n\tfunction area(self): number\nend\n",
        // An empty body: the header opens and the same line closes it.
        "impl Test as end\n",
        "impl Shape for Test as end\n",
        "export impl Shape for Test as end\n",
        "trait Test as end\n",
        "struct Test as end\n",
        "enum Test as end\n",
        "interface Test as end\n",
    ] {
        assert_eq!(lenient(src), (0, 0), "{src:?}");
    }
}

/*
`declare struct`, and the four other declarations Luau's definition
syntax has no form for, report once. Recovery used to read the `declare`
alone, so the body reported again, once per member.
*/
#[test]
fn a_declaration_declare_does_not_take_reports_once() {
    let options = ParseOptions {
        definitions: true,
        ..ParseOptions::default()
    };

    for (word, noun) in [
        ("struct", "a struct"),
        ("enum", "an enum"),
        ("trait", "a trait"),
        ("interface", "an interface"),
        ("namespace", "a namespace"),
    ] {
        for body in [
            "    name: string\n",
            "    function get(n: number): number\n",
        ] {
            let src = format!("declare {word} Old\n{body}end\n");
            let lexed = lexer::lex(&src).unwrap();
            let (chunk, diagnostics) = parser::parse_lenient(&src, &lexed.toks, options);
            assert_eq!(
                diagnostics.len(),
                1,
                "one report for {src:?}, got {diagnostics:?}"
            );
            assert_eq!(
                diagnostics[0].message,
                format!(
                    "`declare` takes a function, a name with a type, an extern type, or a class; {noun} is not declared"
                )
            );
            assert_eq!(diagnostics[0].offset, 8, "the report sits on `{word}`");
            assert!(
                printer::coverage_errors(&chunk).is_empty(),
                "the declaration covers its tokens"
            );
            assert_eq!(printer::print_chunk(&src, &lexed.toks, &chunk), src);
        }
    }
}

/*
A `declare` in a file the parser reads as code. `declare` is a plain
name there, so `declare function f(): number` read as an expression and
the header reported twice: the expression, and the missing `end`.

The word reports once, and the declaration reader moves past the
declaration.
*/
#[test]
fn a_declare_in_code_reports_once() {
    for src in [
        "declare function f(): number\n\nprint(1)\n",
        "declare n: number\n\nprint(1)\n",
        "declare class C\n    x: number\nend\n\nprint(1)\n",
        "declare extern type E with\n    x: number\nend\n\nprint(1)\n",
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());

        assert_eq!(
            diagnostics.len(),
            1,
            "one report for {src:?}, got {diagnostics:?}"
        );
        assert_eq!(
            diagnostics[0].message,
            "`declare` belongs in a `.d.aly` file; move this declaration there"
        );
        assert_eq!(diagnostics[0].offset, 0, "the report sits on `declare`");
        assert_eq!(lenient(src), (1, 1), "{src:?}");
    }

    // `declare` is still a name. Neither line is a declaration.
    assert_eq!(
        lenient("local declare = 1\ndeclare = 2\nprint(declare)\n"),
        (0, 0)
    );

    // A definitions file reads every form with no report.
    let options = ParseOptions {
        definitions: true,
        ..ParseOptions::default()
    };
    let src = "declare function f(): number\ndeclare n: number\n";
    let lexed = lexer::lex(src).unwrap();
    let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, options);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/*
`enum E {}` writes another language's body. Each of the six keywords
names the Alloy form, on the `{`, and the brace group goes with the
report. Only `struct` did, so the other five fell through to a generic
parse error.
*/
#[test]
fn a_body_in_braces_names_the_as_form() {
    for (src, message) in [
        (
            "struct S {}\n",
            "a struct body is `as ... end`: `struct S as`",
        ),
        ("enum E {}\n", "an enum body is `as ... end`: `enum E as`"),
        ("trait T {}\n", "a trait body is `as ... end`: `trait T as`"),
        (
            "interface I {}\n",
            "an interface body is `as ... end`: `interface I as`",
        ),
        (
            "namespace N {}\n",
            "a namespace body is `as ... end`: `namespace N as`",
        ),
        ("impl S {}\n", "an impl body is `as ... end`: `impl S as`"),
        (
            "enum E {\n    A,\n    B,\n}\n",
            "an enum body is `as ... end`: `enum E as`",
        ),
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (chunk, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(
            diagnostics.len(),
            1,
            "one report for {src:?}, got {diagnostics:?}"
        );
        assert_eq!(diagnostics[0].message, message);
        assert_eq!(
            src.as_bytes()[diagnostics[0].offset],
            b'{',
            "the report sits on the `{{` for {src:?}"
        );
        assert!(printer::coverage_errors(&chunk).is_empty());
        assert_eq!(printer::print_chunk(src, &lexed.toks, &chunk), src);
    }
}
