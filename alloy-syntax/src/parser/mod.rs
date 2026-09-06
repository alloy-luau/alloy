/*!
A recursive descent parser for the full Luau grammar, modeled on the
official Parser.cpp. It produces the token span tree in
[`crate::ast`].

The parser reads types for their extent, but it does not interpret them.
That is intentional. A rule that needs type structure can parse the span
later. The recursion has a depth guard. So pathological nesting is a clean
error and never a crash.
*/

use crate::ast::*;
use crate::lexer::{Tok, TokKind};

mod expr;
mod stmt;
mod types;

#[derive(Debug)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

/// The default nesting limit. It is deep enough for real code, and shallow
/// enough to protect the stack on any thread that parses. Alloy's grammar
/// adds frames per level, so the limit sits below eclipse_luau's 180.
pub const DEFAULT_MAX_DEPTH: u32 = 120;
const UNARY_PRIORITY: u8 = 12;

/*
How a parse reads the source.

`definitions` allows the `declare` statements of a `.d.luau` file. It is
off for ordinary source, because Luau itself accepts a declaration only
in a definitions file, and a parser that quietly accepted one in a
normal module would bless code the compiler refuses.

`max_depth` bounds the nesting of the recursive descent. Pathological
nesting is then a clean error and never a crash, which is the guarantee
this crate makes. The default holds that guarantee on every thread. A
consumer with deeper generated code can raise the limit, and the stack
budget then becomes that consumer's: a debug build spends several
kilobytes of stack per level, so a raised limit belongs on a thread
built with a stack to match, ex: `std::thread::Builder::stack_size`.
A limit of zero refuses everything; there is no "unlimited", because an
unbounded recursion is the crash this field exists to prevent.
*/
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    pub definitions: bool,
    pub max_depth: u32,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            definitions: false,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

impl ParseOptions {
    /// The options a file's name asks for: `.d.luau` and `.d.lua` are definitions
    pub fn for_path(path: &std::path::Path) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        Self {
            definitions: name.ends_with(".d.luau")
                || name.ends_with(".d.lua")
                || name.ends_with(".d.aly"),
            ..Self::default()
        }
    }
}

pub fn parse(src: &str, toks: &[Tok]) -> Result<Chunk, ParseError> {
    parse_with(src, toks, ParseOptions::default())
}

pub fn parse_with(src: &str, toks: &[Tok], options: ParseOptions) -> Result<Chunk, ParseError> {
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        depth: 0,
        options,
        lenient: false,
        diagnostics: Vec::new(),
        method_context: 0,
        type_edits: Vec::new(),
        no_method_call: 0,
        in_match_arm: 0,
    };

    let block = p.block()?;

    if !p.at_end() {
        return Err(p.err("unexpected token"));
    }

    Ok(Chunk {
        block,
        type_edits: p.type_edits,
    })
}

/// The most diagnostics one lenient parse reports. Past the cap the rest
/// of the file becomes one error node, so a pathological file costs a
/// bounded amount of work and memory.
pub const MAX_DIAGNOSTICS: usize = 200;

/*
Parses with recovery. The tree always covers every token: a stretch the
parser cannot read becomes a `Stmt::Error` that tiles the block like any
other statement, and the reason lands in the diagnostics.

Recovery always advances. On an error the parser returns to the start of
the statement, then skips at least one token and stops at the next place a
statement can begin. A recovery that consumed nothing would loop forever,
so the skip is the invariant this function keeps, and the fuzz tests hold
it against mutated input.
*/
pub fn parse_lenient(src: &str, toks: &[Tok], options: ParseOptions) -> (Chunk, Vec<ParseError>) {
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        depth: 0,
        options,
        lenient: true,
        diagnostics: Vec::new(),
        method_context: 0,
        type_edits: Vec::new(),
        no_method_call: 0,
        in_match_arm: 0,
    };

    let mut stmts = Vec::new();

    while !p.at_end() {
        match p.block() {
            Ok(block) => stmts.extend(block.stmts),

            // A block only fails in lenient mode when the depth guard trips.
            Err(e) => {
                p.diagnostics.push(e);
                let start = p.pos;
                p.pos = p.toks.len();
                stmts.push(Stmt::Error(TokSpan::new(start, p.pos)));
                break;
            }
        }

        // `block` stops at a block-end keyword. At the top level that
        // keyword has no opener, so it is an error node of its own.
        if !p.at_end() {
            let start = p.pos;
            p.report(&format!("unexpected `{}`", p.text()));
            p.bump();
            stmts.push(Stmt::Error(TokSpan::new(start, p.pos)));
        }
    }

    let block = Block {
        stmts,
        span: TokSpan::new(0, toks.len()),
    };

    let chunk = Chunk {
        block,
        type_edits: p.type_edits,
    };

    (chunk, p.diagnostics)
}

/// Parses one expression that covers the whole token stream. Use this for a
/// source slice that another caller cut, for example the `host` span of a
/// worm that holds an attribute value.
pub fn parse_expr(src: &str, toks: &[Tok]) -> Result<Expr, ParseError> {
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        depth: 0,
        options: ParseOptions::default(),
        lenient: false,
        diagnostics: Vec::new(),
        method_context: 0,
        type_edits: Vec::new(),
        no_method_call: 0,
        in_match_arm: 0,
    };

    let expr = p.expr()?;

    if !p.at_end() {
        return Err(p.err("unexpected token after the expression"));
    }

    Ok(expr)
}

struct Parser<'a> {
    options: ParseOptions,
    src: &'a str,
    toks: &'a [Tok],
    pos: usize,
    depth: u32,
    /// Recover from statement errors instead of failing the parse.
    lenient: bool,
    /// The errors a lenient parse recovered from, in source order.
    diagnostics: Vec<ParseError>,
    /// Above zero inside an `impl` or `trait` body, where a method named
    /// `new` is the constructor and no reserved word applies.
    method_context: u32,
    /// Alloy syntax inside type spans, for emit.
    type_edits: Vec<TypeEdit>,
    /// Above zero inside the then-branch of a ternary, where `:` closes
    /// the branch instead of opening a method call.
    no_method_call: u32,
    /// Above zero inside a match arm, where `case` and `default` end a
    /// block the way `end` and `else` do.
    in_match_arm: u32,
}

impl<'a> Parser<'a> {
    // --- token access ------------------------------------------------------

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn text_at(&self, n: usize) -> &'a str {
        match self.toks.get(self.pos + n) {
            Some(t) => t.text(self.src),

            None => "",
        }
    }

    fn text(&self) -> &'a str {
        self.text_at(0)
    }

    fn kind_at(&self, n: usize) -> Option<TokKind> {
        self.toks.get(self.pos + n).map(|t| t.kind)
    }

    fn at(&self, s: &str) -> bool {
        self.text() == s
    }

    fn at_name(&self) -> bool {
        matches!(self.kind_at(0), Some(TokKind::Ident)) && !is_reserved(self.text())
    }

    /// Reports if the token `n` ahead is a name and not a reserved word.
    fn name_at(&self, n: usize) -> bool {
        matches!(self.kind_at(n), Some(TokKind::Ident)) && !is_reserved(self.text_at(n))
    }

    fn bump(&mut self) -> usize {
        let i = self.pos;
        self.pos += 1;

        i
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.at(s) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> Result<usize, ParseError> {
        if self.at(s) {
            Ok(self.bump())
        } else {
            Err(self.err(&format!("expected `{s}`, found {}", self.found())))
        }
    }

    /// Records a diagnostic when a name is an Alloy reserved word. The
    /// parse goes on with the name, so the rest of the file still reports.
    fn reject_reserved(&mut self, name: TokSpan) {
        let tok = self.toks[name.start as usize];
        let word = &self.src[tok.start as usize..tok.end as usize];

        if is_alloy_reserved(word) {
            self.diagnostics.push(ParseError {
                offset: tok.start as usize,
                message: format!("`{word}` is a reserved word and cannot be a name"),
            });
        }
    }

    fn expect_name(&mut self) -> Result<TokSpan, ParseError> {
        if self.at_name() {
            let i = self.bump();
            Ok(TokSpan::new(i, i + 1))
        } else {
            Err(self.err(&format!("expected a name, found {}", self.found())))
        }
    }

    fn found(&self) -> String {
        if self.at_end() {
            "end of file".to_string()
        } else {
            format!("`{}`", self.text())
        }
    }

    /// Reports if a newline sits between the token `n` ahead and the one
    /// after it. Expression-position words need their operand on the same
    /// line, or `local x = new` and `print(x)` on the next line would join.
    fn newline_after(&self, n: usize) -> bool {
        let (Some(here), Some(next)) =
            (self.toks.get(self.pos + n), self.toks.get(self.pos + n + 1))
        else {
            return true;
        };

        let (lo, hi) = (here.end as usize, next.start as usize);

        lo < hi && self.src[lo..hi].contains('\n')
    }

    /*
    Reports if the contextual word at the cursor is a prefix operator here:
    `new`, `await`, `try`, `async`, `delete`. The operand must start on the
    same line, and a word followed by `(`, `{`, a string, `=`, or a binary
    operator is a plain identifier, so `new(x)`, `await = 1`, and
    `try .. "s"` keep their Luau meaning.
    */
    fn prefix_word_here(&self) -> bool {
        if self.newline_after(0) {
            return false;
        }

        match self.kind_at(1) {
            Some(TokKind::LParen)
            | Some(TokKind::Str { .. })
            | Some(TokKind::InterpStr | TokKind::InterpHead) => false,

            Some(TokKind::Ident) => true,

            Some(TokKind::Number) => true,

            _ => {
                let next = self.text_at(1);

                !(next == "{"
                    || next == "="
                    || next == ","
                    || next == "."
                    || next == ":"
                    || next == "["
                    || next == "]"
                    || next == ")"
                    || next == "}"
                    || next == ";"
                    || is_compound_op(next)
                    || binop_priority(next).is_some())
            }
        }
    }

    /// Reports if the contextual word at the cursor is an infix operator:
    /// `is`, `satisfies`, `where`, and the bitwise words. Same-line rule.
    fn infix_word_here(&self) -> bool {
        !self.newline_before_pos() && !self.newline_after(0)
    }

    /// Reports if a newline sits between the previous token and the current one
    fn newline_before_pos(&self) -> bool {
        let (Some(prev), Some(here)) = (
            self.pos.checked_sub(1).and_then(|i| self.toks.get(i)),
            self.toks.get(self.pos),
        ) else {
            return false;
        };

        let (lo, hi) = (prev.end as usize, here.start as usize);

        lo < hi && self.src[lo..hi].contains('\n')
    }

    /// Reports if the token `n` ahead touches the token after it, with no
    /// trivia between. Alloy fuses `?` `?` into `??` only when they touch.
    fn adjacent(&self, n: usize) -> bool {
        match (self.toks.get(self.pos + n), self.toks.get(self.pos + n + 1)) {
            (Some(a), Some(b)) => a.end == b.start,

            _ => false,
        }
    }

    /*
    The binary operator at the cursor: its token count and binding power.

    `??` is two `?` tokens that touch, and `??=` is three tokens that the
    assignment parser reads, so `??` followed by an adjacent `=` is not an
    operator here. The lexer keeps `?` single because the type parser needs
    `number?` to end at the `?`, and `x :: number?/2` must stay a division.
    */
    fn binop_at(&self) -> Option<(usize, (u8, u8))> {
        if self.at("?") && self.text_at(1) == "?" && self.adjacent(0) {
            if self.text_at(2) == "=" && self.adjacent(1) {
                return None;
            }

            return Some((2, (3, 3)));
        }

        let word = self.text();

        // A word operator obeys the same-line rule; `in` is reserved and
        // cannot start a statement, so it needs no rule.
        if matches!(word, "bor" | "bxor" | "band" | "shl" | "shr") && !self.infix_word_here() {
            return None;
        }

        binop_priority(word).map(|p| (1, p))
    }

    /// The compound assignment operator at the cursor, as a token count.
    fn compound_op_at(&self) -> Option<usize> {
        if is_compound_op(self.text()) {
            return Some(1);
        }

        if self.at("?")
            && self.text_at(1) == "?"
            && self.text_at(2) == "="
            && self.adjacent(0)
            && self.adjacent(1)
        {
            return Some(3);
        }

        None
    }

    /// Records a diagnostic in lenient mode, up to the cap.
    fn report(&mut self, message: &str) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            let e = self.err(message);
            self.diagnostics.push(e);
        }
    }

    fn err(&self, message: &str) -> ParseError {
        let offset = match self.toks.get(self.pos) {
            Some(t) => t.start as usize,

            None => self.src.len(),
        };

        ParseError {
            offset,
            message: message.to_string(),
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;

        if self.depth > self.options.max_depth {
            return Err(self.err("expression or statement nests too deeply"));
        }

        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }
}

/// The words that start an Alloy statement or expression. A name cannot
/// be one of them: not a local, a parameter, a plain function, or a bare
/// expression. After `.` or `:` each is a field, so `Instance.new` and
/// an `impl`'s `function new` stay valid. Words with a meaning only
/// inside a construct, `client`, `from`, `as`, `case`, and so on, are
/// free names.
pub fn is_alloy_reserved(word: &str) -> bool {
    matches!(
        word,
        "struct"
            | "enum"
            | "trait"
            | "impl"
            | "interface"
            | "remote"
            | "macro"
            | "attribute"
            | "match"
            | "const"
            | "async"
            | "await"
            | "try"
            | "new"
            | "delete"
            | "import"
            | "export"
    )
}

fn is_reserved(word: &str) -> bool {
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

fn is_unary_op(s: &str) -> bool {
    matches!(s, "not" | "-" | "#")
}

fn is_compound_op(s: &str) -> bool {
    matches!(s, "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "..=" | "//=")
}

/// The left and right binding power. A right value lower than the left value
/// means the operator is right associative.
fn binop_priority(s: &str) -> Option<(u8, u8)> {
    Some(match s {
        "or" => (1, 1),

        "and" => (2, 2),

        // `??` sits here, at 3: above `and` and `or`, below comparison, so
        // `a ?? b == c` reads `a ?? (b == c)` as in C#. See `binop_at`.
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
