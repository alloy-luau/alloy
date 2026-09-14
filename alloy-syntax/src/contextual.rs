/*!
The Alloy words a file may also use as a name, and where each one still
reads as a keyword.

Luau reserves none of these words, and every one of them appears in
ordinary Roblox code: `Instance.new` lands in a local named `new`, a
module table is named `export`, `try` holds `pcall`, `match` holds a
string helper, `const` is a Luau declaration Alloy passes through, and
`local function destroy(self)` is the cleanup method half of Roblox
writes. A package a project imports brings its own names, and `async`,
`await`, `after`, and `enum` are among them.
Luau itself treats `export` this way. `export type T = number` and
`local export = 1` live in one file, because `type` is what makes the
word a keyword.

One shape stays reserved inside a contextual word: a call spelled
`import(...)` or `import<<T>>(...)` is always the module import, because
that call is the expression form of the statement and the emit turns it
into a `require`. Every other reading of `import` is the name.

The parser decides this per dispatch, and every other reader of the file
needs the same answer: the formatter spaces `new(x)` as a call, the
highlighter paints `local new = 1` as a name, and the server answers a
hover on it from the child. They all call [`keyword_at`], so one rule
holds across the tools.
*/

use crate::lexer::{Tok, TokKind};

/// The words that read as a keyword in some places and a name in others.
pub fn is_contextual(word: &str) -> bool {
    matches!(
        word,
        "new"
            | "export"
            | "try"
            | "match"
            | "const"
            | "async"
            | "await"
            | "delete"
            | "destroy"
            | "after"
            | "enum"
            | "struct"
            | "interface"
            | "import"
            | "requires"
            | "each"
    )
}

/// The words that make `export` a declaration instead of a name.
const EXPORT_DECL: &[&str] = &[
    "type",
    "default",
    "local",
    "const",
    "function",
    "class",
    "open",
    "async",
    "global",
    "enum",
    "struct",
    "trait",
    "interface",
    "remote",
    "attribute",
    "macro",
    "namespace",
    "impl",
];

/*
Reports if the word at token `i` reads as a keyword there.

A word that is not contextual answers true, so a caller can route every
word through this one call. A contextual word answers false wherever the
token after it cannot follow a keyword, and the caller then treats it as
the plain name it is.

The token before decides first: after `.`, `:`, `?.`, or `function` every
word is a name, because `Instance.new` and an `impl`'s `function new` are
the names Alloy itself writes.
*/
pub fn keyword_at(src: &str, toks: &[Tok], i: usize) -> bool {
    let Some(tok) = toks.get(i) else {
        return false;
    };
    let word = tok.text(src);

    if !is_contextual(word) {
        return true;
    }

    // `each` sits after the `function` of its clause, so it answers
    // before the rule that reads every word after a `function` as a name.
    if word == "each" {
        return each_member_follows(src, toks, i);
    }

    if i > 0 && matches!(text(src, toks, i - 1), "." | ":" | "?." | "function") {
        return false;
    }

    match word {
        // `new Thing()` needs a name after it. `new(x)`, `new.f`, and
        // `new "Part"` are the Luau readings of a local named new.
        "new" => prefix_operand_follows(src, toks, i) && name_at(src, toks, i + 1),

        // `try f()` and `try do ... end`.
        "try" => {
            (text(src, toks, i + 1) == "do" && !newline_after(src, toks, i))
                || prefix_operand_follows(src, toks, i)
        }

        "export" => EXPORT_DECL.contains(&text(src, toks, i + 1)) || text(src, toks, i + 1) == "{",

        "match" => match_follows(src, toks, i),

        "const" => const_decl_follows(src, toks, i),

        // `async function f()` and `async do ... end`. Nothing else takes
        // the word, so `local async = true` keeps its name.
        "async" => {
            matches!(text(src, toks, i + 1), "function" | "do") && !newline_after(src, toks, i)
        }

        // `await f()`. `await(p)` and `await = 1` are the name.
        "await" => prefix_operand_follows(src, toks, i),

        // `delete t[k]` and `destroy part`. `local function destroy(self)`
        // and `self:destroy()` are the name.
        "delete" | "destroy" => delete_operand_follows(src, toks, i),

        "after" => after_delay_follows(src, toks, i),

        // A declaration opens with its own name: `enum State as`,
        // `struct Vec2 as`, `interface Named as`.
        "enum" | "struct" | "interface" => {
            !newline_after(src, toks, i) && name_at(src, toks, i + 1)
        }

        "import" => import_follows(src, toks, i),

        "requires" => requires_clause_follows(src, toks, i),

        _ => true,
    }
}

/*
Reports if a contract clause follows the `requires` at token `i`.

The word opens a clause of an `attribute ... as ... end` body:
`requires public function Start(self)`. The clause head decides, because
it is a shape no expression carries: a kind word, `function` or `field`,
with at most a visibility word in front of it, and a member name after.
`requires = 1`, `requires(f)`, and `requires.list` are the Luau readings
of a local named requires.
*/
pub fn requires_clause_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    if newline_after(src, toks, i) {
        return false;
    }

    let mut n = i + 1;

    if matches!(text(src, toks, n), "public" | "private") {
        n += 1;
    }

    if !matches!(text(src, toks, n), "function" | "field") {
        return false;
    }

    name_at(src, toks, n + 1) || (text(src, toks, n + 1) == "each" && name_at(src, toks, n + 2))
}

/*
Reports if the `each` at token `i` expands a clause over a parameter.

`each` is a keyword only between a clause's kind word and the parameter
it reads, so the tokens behind it decide: `requires`, an optional
visibility, and `function` or `field`. `local each = 1` and
`t.each(f)` keep their Luau reading.
*/
pub fn each_member_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    if !name_at(src, toks, i + 1) {
        return false;
    }

    let Some(kind) = i.checked_sub(1) else {
        return false;
    };

    if !matches!(text(src, toks, kind), "function" | "field") {
        return false;
    }

    let mut n = kind;

    if matches!(
        n.checked_sub(1).map(|k| text(src, toks, k)),
        Some("public" | "private")
    ) {
        n -= 1;
    }

    n.checked_sub(1).map(|k| text(src, toks, k)) == Some("requires")
}

/*
Reports if the contextual word that starts at byte `at` in `src` is a
keyword there.

This is [`keyword_at`] for a caller that holds text and no tokens, ex: the
server walking one line of a file the author is still typing. A source the
lexer refuses answers true, and so does a byte that starts no token, so
such a caller keeps the reading it had before.
*/
pub fn keyword_at_byte(src: &str, at: usize) -> bool {
    let Ok(lexed) = crate::lexer::lex(src) else {
        return true;
    };

    match lexed.toks.iter().position(|t| t.start as usize == at) {
        Some(i) => keyword_at(src, &lexed.toks, i),

        None => true,
    }
}

/// The source of token `i`, or the empty string past the end.
pub fn text<'s>(src: &'s str, toks: &[Tok], i: usize) -> &'s str {
    match toks.get(i) {
        Some(t) => t.text(src),

        None => "",
    }
}

/// Reports if a newline sits between token `i` and the one after it.
/// The end of the file counts, so a word at the end takes no operand.
pub fn newline_after(src: &str, toks: &[Tok], i: usize) -> bool {
    let (Some(here), Some(next)) = (toks.get(i), toks.get(i + 1)) else {
        return true;
    };
    let (lo, hi) = (here.end as usize, next.start as usize);

    lo < hi && src[lo..hi].contains('\n')
}

/// Reports if token `i` is a name: an identifier that Luau does not reserve.
pub fn name_at(src: &str, toks: &[Tok], i: usize) -> bool {
    matches!(toks.get(i).map(|t| t.kind), Some(TokKind::Ident))
        && !is_luau_reserved(text(src, toks, i))
}

/*
Reports if an operand for a prefix word follows token `i`.

The operand must start on the same line, or `local x = new` and
`print(x)` on the next line would join. A word followed by `(`, `{`, a
string, `=`, or a binary operator is a plain name, so `new(x)`,
`try = 1`, and `try .. "s"` keep their Luau meaning.
*/
pub fn prefix_operand_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    if newline_after(src, toks, i) {
        return false;
    }

    match toks.get(i + 1).map(|t| t.kind) {
        // `await(x)` is a call of a Luau function of that name;
        // `await (x)` with a space is the word over a parenthesised
        // operand.
        Some(TokKind::LParen) => toks[i].end != toks[i + 1].start,

        Some(TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpHead) => false,

        // `return try end` closes a block: the word after is no operand.
        Some(TokKind::Ident) => starts_an_expression(text(src, toks, i + 1)),

        Some(TokKind::Number) => true,

        _ => {
            let next = text(src, toks, i + 1);

            !(matches!(
                next,
                "{" | "=" | "," | "." | ":" | "[" | "]" | ")" | "}" | ";"
            ) || is_compound_op(next)
                || binop_priority(next).is_some())
        }
    }
}

/// Reports if a word can begin an expression. Luau reserves the rest, and
/// each of those closes a block or opens a statement instead.
fn starts_an_expression(word: &str) -> bool {
    !is_luau_reserved(word) || matches!(word, "nil" | "true" | "false" | "not" | "function" | "if")
}

/*
Reports if the `match` at token `i` opens a match.

The word is a keyword when an expression that is not a call shape follows
on the same line: `match(s, p)` and `match "x"` stay calls. A bracket
reads both ways, `match [k] with` against `match[k] = 1`, and the `with`
decides.
*/
pub fn match_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    if newline_after(src, toks, i) {
        return false;
    }

    match toks.get(i + 1).map(|t| t.kind) {
        Some(TokKind::LParen | TokKind::Str { .. } | TokKind::InterpStr | TokKind::InterpHead) => {
            false
        }

        Some(TokKind::Ident) => starts_an_expression(text(src, toks, i + 1)),

        Some(TokKind::Number) => true,

        _ if matches!(text(src, toks, i + 1), "{" | "[") => with_closes_scrutinees(src, toks, i),

        _ => matches!(text(src, toks, i + 1), "-" | "#"),
    }
}

/// The tokens [`with_closes_scrutinees`] reads before it gives up. A
/// scrutinee list is short; the cap keeps a file with no `with` from
/// costing a pass per `match` in it.
const SCRUTINEE_SCAN: usize = 256;

/*
Reports if a `with` closes the scrutinee list that opens after token `i`.

The scan balances brackets, so a `with` inside a nested table does not
count. At depth zero it stops at a token no scrutinee list holds, ex: the
`=` of `match[k] = 1`, so the decision never reads into the next
statement.
*/
fn with_closes_scrutinees(src: &str, toks: &[Tok], i: usize) -> bool {
    let mut depth = 0usize;

    for n in 1..=SCRUTINEE_SCAN {
        let Some(t) = toks.get(i + n) else {
            return false;
        };

        match t.text(src) {
            "(" | "[" | "{" => depth += 1,

            ")" | "]" | "}" => match depth.checked_sub(1) {
                Some(d) => depth = d,

                None => return false,
            },

            _ if depth > 0 => {}

            "with" => return true,

            "=" | ";" | "end" | "then" | "do" | "else" | "elseif" | "until" | "return"
            | "local" | "while" | "for" | "function" => return false,

            _ => {}
        }
    }

    false
}

/*
Reports if an operand for `delete` or `destroy` follows token `i`.

The operand is a name, a string, or a number. A string and a number are
not things a file can delete, and they are here on purpose: the statement
takes them and the analyzer names what the user wrote, instead of the
parser reporting a call it never saw. `delete(x)` and `destroy = f` keep
their Luau reading, and so does `local function destroy(self)`.
*/
pub fn delete_operand_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    if newline_after(src, toks, i) {
        return false;
    }

    name_at(src, toks, i + 1)
        || matches!(
            toks.get(i + 1).map(|t| t.kind),
            Some(TokKind::Str { .. } | TokKind::Number | TokKind::InterpStr)
        )
}

/*
Reports if a delay follows the `after` at token `i`.

`after 2 do ... end` takes a delay and then a block, so a number or a
name after the word makes it the keyword. `after(x)`, `after = 1`,
`after[1] = 2`, `after.f = 2`, and `after += 1` are the Luau readings of
a local named after.

A parenthesized delay, `after (a + b) do`, carries the tokens of a call.
The `do` that closes the delay decides, and the scan stops at the end of
the line, so `after(x)` on one line and `do ... end` on the next stay a
call and a block.
*/
pub fn after_delay_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    prefix_operand_follows(src, toks, i)
        || (text(src, toks, i + 1) == "(" && do_closes_delay(src, toks, i))
}

/*
Reports if a `do` or a `where` closes the delay that opens after token
`i`, with no newline before it.

The scan balances brackets, so the `do` of a nested `for` inside the
delay does not count. At depth zero it gives up at a token no delay
holds, ex: the `=` of `after(x) = 1`, an assignment a file could write
to the field of a table named after.
*/
fn do_closes_delay(src: &str, toks: &[Tok], i: usize) -> bool {
    let mut depth = 0usize;

    for n in 1..=SCRUTINEE_SCAN {
        if newline_after(src, toks, i + n - 1) {
            return false;
        }

        let Some(t) = toks.get(i + n) else {
            return false;
        };

        match t.text(src) {
            "(" | "[" | "{" => depth += 1,

            ")" | "]" | "}" => match depth.checked_sub(1) {
                Some(d) => depth = d,

                None => return false,
            },

            _ if depth > 0 => {}

            "do" | "where" => return true,

            "=" | ";" | "end" | "then" | "else" | "elseif" | "until" | "return" | "local"
            | "while" | "for" | "function" => return false,

            _ => {}
        }
    }

    false
}

/*
Reports if an import list follows the `import` at token `i`.

`import { a } from "m"`, `import * as m from "m"`, `import type { T }
from "m"`, and `import M from "m"` are the statement. Everything else is
the name, so `local import = 1` and `import.cache` keep their Luau
reading.

The call `import("m")` is absent from this rule on purpose. It is the
expression form of the same statement, and the emit turns it into a
`require`, so the parser holds that one shape for the keyword. The word
is still spelled like a name there, which is what lets the call parse at
all.
*/
pub fn import_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    match text(src, toks, i + 1) {
        "*" | "{" => true,

        "type" => text(src, toks, i + 2) == "{",

        _ => {
            name_at(src, toks, i + 1)
                && (text(src, toks, i + 2) == "from"
                    || (text(src, toks, i + 2) == "," && text(src, toks, i + 3) == "{"))
        }
    }
}

/*
Reports if a declaration follows the `const` at token `i`.

A name, `function`, `async function`, an `@attr` line, or a destructuring
bracket makes it the keyword. A `=`, a `(`, a `.`, a `:`, or a `,` after
it makes it the name, so `const = 1` and `const(x)` keep their Luau
reading.
*/
pub fn const_decl_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    name_at(src, toks, i + 1)
        || matches!(text(src, toks, i + 1), "function" | "@")
        || (text(src, toks, i + 1) == "async" && text(src, toks, i + 2) == "function")
        || destructure_follows(src, toks, i)
}

/*
Reports if a destructuring pattern follows token `i`.

`const [a, b] = t` takes the pattern apart; `const[1] = 2` indexes a table
named const. Both open with the same two tokens, so the contents decide: a
pattern holds names, commas, `...`, and the `=` of a rename, and the
bracket it closes is followed by `=`. A key that is a number, a string, or
an expression makes it an index.

A single bare name, `const[k] = v`, reads both ways on the same tokens.
The pattern wins, because a destructuring declaration is what the `const`
spelling is for.
*/
fn destructure_follows(src: &str, toks: &[Tok], i: usize) -> bool {
    let open = text(src, toks, i + 1);

    if !matches!(open, "[" | "{") {
        return false;
    }

    let close = if open == "[" { "]" } else { "}" };

    for n in (i + 2)..toks.len() {
        let word = text(src, toks, n);

        if word == close {
            return text(src, toks, n + 1) == "=";
        }

        if !(name_at(src, toks, n) || matches!(word, "," | "..." | "=")) {
            return false;
        }
    }

    false
}

/*
Reports if the contextual `word` reads as a name, judged from `after`:
the rest of the line behind it.

This is [`keyword_at`] for the terminal highlighter, which paints one
line at a time and holds no tokens. A punctuation mark that cannot
follow a keyword makes the word a name, so `new(x)`, `try = pcall`, and
`t.destroy` paint as the names they are.

`import(` and `import<<T>>(` are the exception the tokens make too: that
call is the module import, so the word keeps its color there.
*/
pub fn name_before(word: &str, after: &str) -> bool {
    if !is_contextual(word) {
        return false;
    }

    let rest = after.trim_start();

    if word == "import" && rest.starts_with(['(', '<']) {
        return false;
    }

    rest.starts_with(['(', '=', '.', ':', '[', ',', ')', '}', ';'])
}

/// The words Luau itself reserves, plus the two Alloy adds to a body.
pub fn is_luau_reserved(word: &str) -> bool {
    matches!(
        word,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
            | "private"
            | "public"
    )
}

/// The compound assignment operators, ex: `x += 1`.
pub fn is_compound_op(s: &str) -> bool {
    matches!(s, "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "..=" | "//=")
}

/*
The left and right binding power of a binary operator. A right value lower
than the left value means the operator is right associative.

`??` is absent: the lexer keeps `?` single, because the type parser needs
`number?` to end at the `?`. The parser fuses the pair itself and gives it
priority 3.
*/
pub fn binop_priority(s: &str) -> Option<(u8, u8)> {
    Some(match s {
        "or" => (1, 1),

        "and" => (2, 2),

        "<" | ">" | "<=" | ">=" | "~=" | "==" | "in" => (4, 4),

        // The bitwise words: below arithmetic, above comparison, C order.
        "bor" => (5, 5),

        "bxor" => (6, 6),

        "band" => (7, 7),

        "shl" | "shr" => (8, 8),

        ".." => (9, 8),

        "+" | "-" => (10, 10),

        "*" | "/" | "//" | "%" => (11, 11),

        "^" => (14, 13),

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    /// The index of the first token whose text is `word`.
    fn at(src: &str, word: &str) -> (Vec<Tok>, usize) {
        let toks = lex(src).unwrap().toks;
        let i = toks
            .iter()
            .position(|t| t.text(src) == word)
            .unwrap_or_else(|| panic!("no `{word}` in {src:?}"));

        (toks, i)
    }

    fn is_kw(src: &str, word: &str) -> bool {
        let (toks, i) = at(src, word);

        keyword_at(src, &toks, i)
    }

    #[test]
    fn a_bracket_key_is_an_index_and_a_pattern_is_a_pattern() {
        assert!(!is_kw("const[1] = 2\n", "const"));
        assert!(!is_kw("const[\"a\"] = 2\n", "const"));
        assert!(!is_kw("const[i + 1] = 2\n", "const"));
        assert!(!is_kw("const { n = 1 }\n", "const"));
        assert!(is_kw("const [a, b] = t\n", "const"));
        assert!(is_kw("const { a } = t\n", "const"));
        assert!(is_kw("const [...rest] = t\n", "const"));
    }

    /// A word that closes a block is no operand: `return try end` returns
    /// the local named try.
    #[test]
    fn a_block_word_is_no_operand() {
        assert!(!is_kw("local f = function() return try end\n", "try"));
        assert!(!is_kw("if c then return new end\n", "new"));
        assert!(is_kw("local v = try nil\n", "try"));
    }

    #[test]
    fn a_word_before_a_call_shape_is_a_name() {
        assert!(!is_kw("local p = new(\"Part\")\n", "new"));
        assert!(!is_kw("new = Instance.new\n", "new"));
        assert!(!is_kw("print(new)\n", "new"));
        assert!(!is_kw("print(new.f)\n", "new"));
        assert!(!is_kw("print(new[1])\n", "new"));
        assert!(!is_kw("print(new \"Part\")\n", "new"));
        assert!(!is_kw("print(new { n = 1 })\n", "new"));
        assert!(!is_kw("local x = try(f)\n", "try"));
        assert!(!is_kw("try = pcall\n", "try"));
        assert!(!is_kw("print(try.f)\n", "try"));
        assert!(!is_kw("print(match(\"a\", \"b\"))\n", "match"));
        assert!(!is_kw("match[\"a\"] = 2\n", "match"));
        assert!(!is_kw("print(match.a)\n", "match"));
        assert!(!is_kw("const = 2\n", "const"));
        assert!(!is_kw("const()\n", "const"));
        assert!(!is_kw("print(const.f)\n", "const"));
        assert!(!is_kw("export = export\n", "export"));
        assert!(!is_kw("export.f = 1\n", "export"));
        assert!(!is_kw("export()\n", "export"));
    }

    #[test]
    fn a_word_before_its_own_syntax_is_the_keyword() {
        assert!(is_kw("local t = new Thing()\n", "new"));
        assert!(is_kw("local v = try f()\n", "try"));
        assert!(is_kw("local v = try do return 1 end\n", "try"));
        assert!(is_kw("match x with\ncase 1 then print(1)\nend\n", "match"));
        assert!(is_kw(
            "match [a, b] with\ncase [1, 2] then 3\nend\n",
            "match"
        ));
        assert!(is_kw("const LIMIT = 5\n", "const"));
        assert!(is_kw("const function f() end\n", "const"));
        assert!(is_kw("export type T = number\n", "export"));
        assert!(is_kw("export local x = 1\n", "export"));
        assert!(is_kw("export { a, b }\n", "export"));
    }

    /// After a `.`, a `:`, or `function` the word is the member or the
    /// definition name that Alloy's own code writes.
    #[test]
    fn a_member_is_never_the_keyword() {
        let src = "local p = Instance.new(\"Part\")\n";
        let toks = lex(src).unwrap().toks;
        let i = toks.iter().position(|t| t.text(src) == "new").unwrap();
        assert!(!keyword_at(src, &toks, i));

        assert!(!is_kw("function new(n: number) end\n", "new"));
        assert!(!is_kw("local s = t:match(\"a\")\n", "match"));
    }

    /// A word with no keyword reading of its own always answers true, so
    /// a caller routes every word through the one call.
    #[test]
    fn a_plain_keyword_answers_true() {
        assert!(is_kw("struct V as\nend\n", "struct"));
        assert!(is_kw("local x = 1\n", "local"));
    }

    /// `after` reads both ways on the tokens a call and an index share.
    #[test]
    fn after_takes_a_delay_and_nothing_else() {
        assert!(is_kw("after 2 do\nend\n", "after"));
        assert!(is_kw("after n do\nend\n", "after"));
        assert!(is_kw("after delay() do\nend\n", "after"));
        assert!(is_kw("after 3 where ready do\nend\n", "after"));
        assert!(is_kw("after (n + 1) do\nend\n", "after"));
        assert!(is_kw("destroy part after 2\n", "after"));

        assert!(!is_kw("after = 1\n", "after"));
        assert!(!is_kw("after(x)\n", "after"));
        assert!(!is_kw("after[1] = 2\n", "after"));
        assert!(!is_kw("after.f = 2\n", "after"));
        assert!(!is_kw("after:m()\n", "after"));
        assert!(!is_kw("after += 1\n", "after"));
        assert!(!is_kw("print(after)\n", "after"));

        // A call on one line and a block on the next stay two statements.
        assert!(!is_kw("after(x)\ndo print(1) end\n", "after"));
    }

    /// The modifier, the operator, and the two cleanup words.
    #[test]
    fn the_prefix_words_need_what_follows_them() {
        assert!(is_kw("async function f()\nend\n", "async"));
        assert!(is_kw("async do print(1) end\n", "async"));
        assert!(!is_kw("local async = false\n", "async"));
        assert!(!is_kw("async(1)\n", "async"));
        assert!(!is_kw("if async then print(1) end\n", "async"));

        assert!(is_kw("local v = await f()\n", "await"));
        assert!(!is_kw("local await = pcall\n", "await"));
        assert!(!is_kw("await(1)\n", "await"));

        assert!(is_kw("delete t\n", "delete"));
        assert!(is_kw("destroy part\n", "destroy"));
        assert!(!is_kw("delete(1)\n", "delete"));
        assert!(!is_kw("local function destroy(self)\nend\n", "destroy"));
        assert!(!is_kw("self:destroy()\n", "destroy"));
        assert!(!is_kw("t.destroy = print\n", "destroy"));
    }

    /// A declaration opens with its own name; a table named for the word
    /// does not.
    #[test]
    fn a_declaration_word_needs_a_name_after_it() {
        assert!(is_kw("enum State as\nend\n", "enum"));
        assert!(is_kw("struct Vec2 as\nend\n", "struct"));
        assert!(is_kw("interface Named as\nend\n", "interface"));

        assert!(!is_kw("local enum = { Idle = 1 }\n", "enum"));
        assert!(!is_kw("print(enum.Idle)\n", "enum"));
        assert!(!is_kw("struct.x = 1\n", "struct"));
        assert!(!is_kw("print(interface)\n", "interface"));
        assert!(!is_kw("local x = enum\nState = 1\n", "enum"));
    }

    /// `import` opens the statement, and the call form keeps the keyword
    /// because the emit turns it into a `require`.
    #[test]
    fn import_is_a_name_outside_its_statement() {
        assert!(is_kw("import { a } from \"m\"\n", "import"));
        assert!(is_kw("import * as m from \"m\"\n", "import"));
        assert!(is_kw("import M from \"m\"\n", "import"));
        assert!(is_kw("import M, { a } from \"m\"\n", "import"));
        assert!(is_kw("import type { T } from \"m\"\n", "import"));

        assert!(!is_kw("local import = {}\n", "import"));
        assert!(!is_kw("import.cache = 1\n", "import"));
        assert!(!is_kw("print(import)\n", "import"));
    }

    /// The one-line reading the terminal highlighter uses.
    #[test]
    fn the_line_reading_matches_the_token_reading() {
        assert!(name_before("new", "(\"Part\")"));
        assert!(name_before("destroy", ".x = 1"));
        assert!(name_before("after", " = 1"));
        assert!(!name_before("after", " 2 do"));
        assert!(!name_before("struct", " Vec2 as"));

        // The import call keeps its color: the emit reads it as a require.
        assert!(!name_before("import", "(\"m\")"));
        assert!(!name_before("import", "<<Config>>(name)"));
        assert!(name_before("import", ".cache = 1"));

        // A word with no name reading is never a name.
        assert!(!name_before("impl", " = 1"));
    }

    /// The operand of a prefix word starts on the same line.
    #[test]
    fn a_newline_ends_a_prefix_word() {
        assert!(!is_kw("local x = new\nprint(x)\n", "new"));
        assert!(!is_kw("local x = try\nprint(x)\n", "try"));
        assert!(!is_kw("local x = match\nprint(x)\n", "match"));
    }
}
