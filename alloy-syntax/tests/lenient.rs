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
