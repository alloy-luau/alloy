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

/// A macro body the statement parser recovers inside reports once:
/// an if-expression is the tail, and a body that is neither a statement
/// nor a tail is one error node up to the macro's `end`.
#[test]
fn a_macro_body_reports_once() {
    let (errors, diagnostics) =
        lenient("export macro pick(c, a, b)\n    if c then a else b\nend\n");
    assert_eq!((errors, diagnostics), (0, 0));

    let (errors, diagnostics) =
        lenient("macro bad(n)\n    if n < 0 then 0 else n\n    print(n)\nend\n\nlocal x = 1\n");
    assert_eq!((errors, diagnostics), (1, 1));

    let (errors, diagnostics) =
        lenient("macro bad2(n)\n    if n < 0 then 0 end\nend\n\nlocal x = 1\n");
    assert_eq!((errors, diagnostics), (1, 1));
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

/// `as` splits a header from a body on the same line. A body there
/// without it reports on the header, and the body still parses. A body
/// on the next line needs no `as`, and nothing reports.
#[test]
fn a_header_without_as_reports_and_still_parses() {
    for (src, head) in [
        ("impl Test function f(self) end\nend\n", "impl Test"),
        (
            "impl Shape for Test function f(self) end\nend\n",
            "impl Shape for Test",
        ),
        (
            "trait Shape function area(self): number\nend\n",
            "trait Shape",
        ),
        ("impl Box<T> function f(self) end\nend\n", "impl Box<T>"),
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

    for src in [
        "impl Test\n\tfunction f(self) end\nend\n",
        "trait Shape\n\tfunction area(self): number\nend\n",
        "struct P\n\tx: number\nend\n",
        "enum E\n\tA\n\tB\nend\n",
        "namespace N\n\tconst X = 1\nend\n",
        "interface I extends J\n\tx: number\nend\n",
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert!(diagnostics.is_empty(), "{src:?}: {diagnostics:?}");
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
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());

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
            "a struct body goes on the lines below the header and closes with `end`, not braces: `struct S`",
        ),
        (
            "enum E {}\n",
            "an enum body goes on the lines below the header and closes with `end`, not braces: `enum E`",
        ),
        (
            "trait T {}\n",
            "a trait body goes on the lines below the header and closes with `end`, not braces: `trait T`",
        ),
        (
            "interface I {}\n",
            "an interface body goes on the lines below the header and closes with `end`, not braces: `interface I`",
        ),
        (
            "namespace N {}\n",
            "a namespace body goes on the lines below the header and closes with `end`, not braces: `namespace N`",
        ),
        (
            "impl S {}\n",
            "an impl body goes on the lines below the header and closes with `end`, not braces: `impl S`",
        ),
        (
            "enum E {\n    A,\n    B,\n}\n",
            "an enum body goes on the lines below the header and closes with `end`, not braces: `enum E`",
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

/*
A remote reads to the end of its line. A header without `from` asks for
the word once. The recovery used to read the `function` of
`remote function f()` as a plain function, which asked that header for an
`end` a remote never has.
*/
#[test]
fn a_remote_without_a_side_reports_once() {
    for (src, message) in [
        ("remote test()\n", "expected `from`, found end of file"),
        (
            "remote function test()\n",
            "expected `from`, found end of file",
        ),
        (
            "remote function test(): number\n",
            "expected `from`, found end of file",
        ),
        (
            "remote function test(\n",
            "expected a name, found end of file",
        ),
        (
            "remote function test() from\n",
            "expected `client` or `server` after `from`",
        ),
        (
            "remote function test() from bogus\n",
            "expected `client` or `server` after `from`",
        ),
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(
            diagnostics.len(),
            1,
            "one report for {src:?}, got {diagnostics:?}"
        );
        assert_eq!(diagnostics[0].message, message, "for {src:?}");
    }

    // The error stops at the end of the line, so the file after it parses.
    let (errors, diagnostics) = lenient("remote function test()\nlocal x = 1\nprint(x)\n");
    assert_eq!((errors, diagnostics), (1, 1));

    // A remote in a body leaves the `end` that closes the body.
    let (errors, diagnostics) = lenient("namespace N as\n    remote function test()\nend\n");
    assert_eq!((errors, diagnostics), (0, 1));
}

/*
An unfinished `requires` clause reports once, on its own line.

The clause used to unwind the whole `attribute` statement. The recovery
then read `impl as` as an impl declaration and reported the header, each
line of the body, and the `end`: five reports for one missing word.
*/
#[test]
fn an_unfinished_contract_clause_reports_once() {
    for (clause, message) in [
        (
            "requires",
            "a `requires` clause asks for a `function` or a `field`, found end of line",
        ),
        (
            "requires public",
            "a `requires` clause asks for a `function` or a `field`, found end of line",
        ),
        (
            "requires private ",
            "a `requires` clause asks for a `function` or a `field`, found end of line",
        ),
        (
            "requires bogus thing",
            "a `requires` clause asks for a `function` or a `field`, found `bogus`",
        ),
        (
            "requires public function",
            "expected a name, found end of line",
        ),
        (
            "requires function each",
            "expected a name, found end of line",
        ),
    ] {
        let src = format!("attribute provider on impl as\n  {clause}\nend\n");
        let (errors, diagnostics) = lenient(&src);
        assert_eq!((errors, diagnostics), (0, 1), "one report for {clause:?}");

        let lexed = lexer::lex(&src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(&src, &lexed.toks, ParseOptions::default());
        assert_eq!(diagnostics[0].message, message, "for {clause:?}");

        let line = src[..diagnostics[0].offset].matches('\n').count() + 1;
        assert_eq!(line, 2, "the report sits on the clause for {clause:?}");
    }

    // A clause with no shape asks for the member alone, so it is whole.
    let (errors, diagnostics) =
        lenient("attribute provider on impl as\n  requires private field state\nend\n");
    assert_eq!((errors, diagnostics), (0, 0));

    // One broken clause leaves the clauses around it.
    let (errors, diagnostics) = lenient(
        "attribute provider on impl as\n  requires public function Start(self)\n  requires private\n  requires field state: number\nend\n",
    );
    assert_eq!((errors, diagnostics), (0, 1));
}

/*
A match head whose `as` names nothing reports once.

A head that a `with` still closes reads on without the alias, so the
arms, the `end` of the match, and the `end` of the function around it
all parse. A head no `with` closes is the error node of its line, and
the file after it parses.
*/
#[test]
fn a_match_head_without_an_alias_name_reports_once() {
    for src in [
        "match e as with\n    case 1 then print(1)\n    default print(2)\nend\n",
        "match e as 5 with\n    case 1 then print(1)\n    default print(2)\nend\n",
        "export local function f(e)\n    match e as with\n        case 1 then print(1)\n    end\nend\n",
        "match e as\n",
        "local function g(e)\n    match e as\nend\nprint(1)\n",
        "match a as x, b as with\n    case 1, 2 then print(x)\n    default print(x)\nend\n",
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(
            diagnostics.len(),
            1,
            "one report for {src:?}, got {diagnostics:?}"
        );
        assert_eq!(
            diagnostics[0].message, "expected a name after `as`; write `match e as name with`",
            "for {src:?}"
        );
    }

    // The head still closes, so the match keeps its arms and the file
    // after it parses.
    let (errors, diagnostics) =
        lenient("match e as with\n    case 1 then print(1)\n    default print(2)\nend\nprint(3)\n");
    assert_eq!((errors, diagnostics), (0, 1));

    // Nothing closes the head, so the statement is the error node and
    // the `end` of the function around it still closes.
    let (errors, diagnostics) = lenient("local function g(e)\n    match e as\nend\nprint(1)\n");
    assert_eq!((errors, diagnostics), (0, 1));
}

/// An attribute's argument list the author is still typing reports on
/// the bracket left open, not on the declaration below it. A list that
/// closes keeps the report of what is wrong inside it.
#[test]
fn an_unclosed_attribute_names_its_bracket() {
    for (src, message, at) in [
        (
            "@deprecated({\nfunction old()\nend\n",
            "`@deprecated` opens `{` and never closes it; write `})` after its arguments",
            "{\n",
        ),
        (
            "@deprecated(\nfunction old()\nend\n",
            "`@deprecated` opens `(` and never closes it; write `)` after its arguments",
            "(\n",
        ),
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(diagnostics.len(), 1, "for {src:?}: {diagnostics:?}");
        assert_eq!(diagnostics[0].message, message);
        assert!(src[diagnostics[0].offset..].starts_with(at), "for {src:?}");
    }

    assert_eq!(
        lenient("@deprecated({ use = \"f\" })\nfunction old()\nend\n"),
        (0, 0)
    );

    let src = "@deprecated(1 +)\nfunction old()\nend\n";
    let lexed = lexer::lex(src).unwrap();
    let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    assert!(
        !diagnostics[0].message.contains("never closes"),
        "{diagnostics:?}"
    );
}

/// A `case` the author is still typing reports once, on the `case`. The
/// arms around it, the `end` of the match, and the `end` of the function
/// all parse, in the value form and in the statement form.
#[test]
fn a_bare_case_reports_once_on_the_case() {
    for src in [
        "local function f(x: number): string\n    local label = match x with\n        case 0 then \"zero\"\n        case\n    end\n    return label\nend\n",
        "match x with\n    case\n    case 1 then print(1)\n    default print(2)\nend\nprint(3)\n",
    ] {
        let (errors, diagnostics) = lenient(src);
        assert_eq!((errors, diagnostics), (0, 1), "for {src:?}");

        let lexed = lexer::lex(src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(diagnostics[0].message, "expected a pattern after `case`");
        assert!(
            src[diagnostics[0].offset..].starts_with("case\n"),
            "for {src:?}"
        );
    }
}

/// Two values of one head under one name: the second would shadow the
/// first. The report lands on the second name, once.
#[test]
fn two_values_of_a_head_cannot_share_a_name() {
    let src = "match a as x, b as x with\n    case 1, 2 then print(x)\n    default print(x)\nend\n";
    let lexed = lexer::lex(src).unwrap();
    let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0].message,
        "the alias `x` is already the alias of another value of this match; give each value its own name"
    );
    assert_eq!(&src[diagnostics[0].offset..diagnostics[0].offset + 1], "x");
    assert_eq!(src[..diagnostics[0].offset].matches('x').count(), 1);
}

/*
An arm that holds the other form of a match reports once.

A match is a statement or an expression by where it stands. A value in
a statement arm read as a broken statement and asked for a name; two
statements in an expression arm asked for `case`, `default`, or `end`.
Each now names the form the arm takes, and the `end` of the match and
of the function around it still close.
*/
#[test]
fn an_arm_of_the_wrong_form_reports_once() {
    for (src, message, at) in [
        (
            "match s with\n    case \"a\" then 5\n    default print(\"d\")\nend\n",
            "a statement arm takes a statement; write `local x = match ... with` to read the arms as values",
            "5",
        ),
        (
            "function f()\n    match s with\n        case \"a\" then 5\n    end\n    print(\"after\")\nend\n",
            "a statement arm takes a statement; write `local x = match ... with` to read the arms as values",
            "5",
        ),
        (
            "match s with\n    case \"a\" then print(\"a\")\n    default 5\nend\n",
            "a statement arm takes a statement; write `local x = match ... with` to read the arms as values",
            "5",
        ),
        (
            "local v = match s with\n    case \"a\" then\n        print(\"a\")\n        local y = 1\n    default 0\nend\n",
            "this arm gives no value: end it with the value, or leave with `return`",
            "default",
        ),
    ] {
        let lexed = lexer::lex(src).unwrap();
        let (_, diagnostics) = parser::parse_lenient(src, &lexed.toks, ParseOptions::default());
        assert_eq!(
            diagnostics.len(),
            1,
            "one report for {src:?}, got {diagnostics:?}"
        );
        assert_eq!(diagnostics[0].message, message, "for {src:?}");
        assert!(
            src[diagnostics[0].offset..].starts_with(at),
            "the report sits on {at} for {src:?}"
        );

        let (errors, count) = lenient(src);
        assert_eq!((errors, count), (0, 1), "for {src:?}");
    }

    // An arm that runs statements ends in its value, as a value block
    // does; both parse clean.
    for src in [
        "local v = match s with\n    case \"a\" then\n        print(\"a\")\n        2\n    default 0\nend\n",
        "local v = match s with\n    case \"a\" then 1\n    default\n        print(\"d\")\n        0\nend\n",
    ] {
        assert_eq!(lenient(src), (0, 0), "for {src:?}");
    }

    // The arm a nested `end` closes goes with the report, so the match
    // keeps its `default` and the file after it parses.
    let (errors, diagnostics) = lenient(
        "local v = match s with\n    case \"a\" then\n        print(\"a\")\n        if c then\n            print(\"b\")\n        end\n    default 0\nend\nprint(v)\n",
    );
    assert_eq!((errors, diagnostics), (0, 1));

    // Both forms still parse clean.
    let (errors, diagnostics) = lenient(
        "match s with\n    case \"a\" then\n        print(\"a\")\n    default\n        print(\"d\")\nend\nlocal w = match s with\n    case \"a\" then 1\n    default 2\nend\nprint(w)\n",
    );
    assert_eq!((errors, diagnostics), (0, 0));
}
