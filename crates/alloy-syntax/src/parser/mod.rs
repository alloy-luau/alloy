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

`reserved_keys` lets a table key be a reserved word, `{ in = "src" }`.
A `.config.aly` sets it: its keys are the keys of `alloy.toml`, and the
author writes them as that file does. Emit quotes each such key.
*/
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    pub definitions: bool,
    pub max_depth: u32,
    pub reserved_keys: bool,
}

/// The `max_depth` a default `ParseOptions` takes. A program whose
/// threads carry a large stack raises it once, with
/// [`run_with_deep_stack`], and every parse then accepts nesting as
/// deep as Luau's own parser does.
static DEFAULT_DEPTH: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(DEFAULT_MAX_DEPTH);

/// Luau's own parser stops at a recursion depth of 1000.
pub const LUAU_MAX_DEPTH: u32 = 1_000;

/// The stack of a thread that parses at [`LUAU_MAX_DEPTH`]. A debug
/// build spends up to 32 KiB of stack per level, so 1000 levels fit
/// with room to spare. The stack is reserved address space; only the
/// pages a parse touches are committed.
pub const DEEP_STACK: usize = 256 << 20;

/// Spawns a thread with [`DEEP_STACK`].
pub fn spawn_deep<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    std::thread::Builder::new()
        .stack_size(DEEP_STACK)
        .spawn(f)
        .expect("a thread")
}

/// Runs `main` on a thread with [`DEEP_STACK`] and raises the default
/// depth to Luau's. Every thread of the program that parses must then
/// come from [`spawn_deep`].
pub fn run_with_deep_stack<T: Send + 'static>(main: impl FnOnce() -> T + Send + 'static) -> T {
    DEFAULT_DEPTH.store(LUAU_MAX_DEPTH, std::sync::atomic::Ordering::Relaxed);

    spawn_deep(main)
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e))
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            definitions: false,
            max_depth: DEFAULT_DEPTH.load(std::sync::atomic::Ordering::Relaxed),
            reserved_keys: false,
        }
    }
}

impl ParseOptions {
    /// The options a file's name asks for: `.d.luau` and `.d.lua` are
    /// definitions, and `.config.aly` takes reserved words as keys.
    pub fn for_path(path: &std::path::Path) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        Self {
            definitions: name.ends_with(".d.luau")
                || name.ends_with(".d.lua")
                || name.ends_with(".d.aly"),
            reserved_keys: name == ".config.aly",
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
        type_names: Vec::new(),
        global_keywords: Vec::new(),
        reserved_keys: Vec::new(),
        no_method_call: 0,
        in_match_arm: 0,
        pattern_arg: 0,
        value_block: false,
        value_lines: 0,
    };

    let block = p.block()?;

    if !p.at_end() {
        return Err(p.err("unexpected token"));
    }

    Ok(Chunk {
        block,
        type_edits: p.type_edits,
        type_names: p.type_names,
        global_keywords: p.global_keywords,
        reserved_keys: p.reserved_keys,
    })
}

/// The most diagnostics one lenient parse reports. Past the cap the rest
/// of the file becomes one error node, so a pathological file costs a
/// bounded amount of work and memory.
pub const MAX_DIAGNOSTICS: usize = 200;

/// The tail of the report for an `impl` or a `trait` header without
/// `as`. The header is the only thing missing: the tree still covers
/// every token, so the formatter reads such a file and writes the `as`
/// in, and the server offers the same rewrite as a quick fix.
pub const NEEDS_AS: &str = "needs `as` before its body";

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
        type_names: Vec::new(),
        global_keywords: Vec::new(),
        reserved_keys: Vec::new(),
        no_method_call: 0,
        in_match_arm: 0,
        pattern_arg: 0,
        value_block: false,
        value_lines: 0,
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
        type_names: p.type_names,
        global_keywords: p.global_keywords,
        reserved_keys: p.reserved_keys,
    };

    (chunk, p.diagnostics)
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
    type_names: Vec<TokSpan>,
    /// The `global` keyword of every declaration that wrote one; see
    /// `Chunk::global_keywords`.
    global_keywords: Vec<TokSpan>,
    /// See `Chunk::reserved_keys`.
    reserved_keys: Vec<TokSpan>,
    /// Above zero inside the then-branch of a ternary, where `:` closes
    /// the branch instead of opening a method call.
    no_method_call: u32,
    /// Above zero inside a match arm, where `case` and `default` end a
    /// block the way `end` and `else` do.
    in_match_arm: u32,
    /// Inside the pattern argument of `$matches`, where an array literal
    /// reads as an array pattern and takes a `...rest`.
    pattern_arg: u32,
    /// Set for the next `block` call, when that block is the body of a
    /// value block: `try do ... end` or `async do ... end`. Such a block
    /// may end in an expression, whose value is the block's value.
    value_block: bool,
    /// Above zero inside Alloy's own value positions: an expression arm
    /// of a match and the body of a value block. There a string, a `{`,
    /// or a `[` that opens a line starts the next thing, not a call or an
    /// index of the line above; Luau code outside keeps Luau's reading.
    value_lines: u32,
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

    /// The token at the cursor is a word the language keeps, so no
    /// declaration can name itself with it.
    fn at_reserved(&self) -> bool {
        matches!(self.kind_at(0), Some(TokKind::Ident)) && is_reserved(self.text())
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

    /*
    The `end` that closes a block, reported against the keyword that
    opened it.

    A lenient parse closes the block where it stands instead of unwinding.
    Unwinding sends the recovery back to the start of the outer statement,
    which then reads the same tokens again and reports the same sentence
    once per level of nesting.
    */
    fn expect_end(&mut self, opener: usize) -> Result<usize, ParseError> {
        if self.at("end") {
            return Ok(self.bump());
        }

        let tok = self.toks[opener.min(self.toks.len().saturating_sub(1))];
        let word = &self.src[tok.start as usize..tok.end as usize];
        let line = self.src[..tok.start as usize].matches('\n').count() + 1;
        let message = format!("`{word}` on line {line} needs an `end`");
        let offset = tok.start as usize;

        if self.lenient {
            self.report_at(offset, &message);

            return Ok(self.pos);
        }

        Err(ParseError { offset, message })
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
    /*
    A Luau reserved word where a bound name goes: the word, then a
    token only a name can stand in front of there.

    `end` alone is the `end` of the block, and the old report names it
    that way. `end:`, `end,`, `end=`, `end)` and `end in` each follow a
    name, so the word is a binding the language cannot spell.
    */
    fn reserved_binding(&self) -> bool {
        self.at_reserved() && matches!(self.text_at(1), ":" | "," | "=" | ")" | "in")
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
            return "end of file".to_string();
        }

        // A piece of an interpolated string carries its own delimiters
        // and the literal text between them. The report quotes the
        // brace that closed the hole, so the string's closing backtick
        // stays out of the quotes.
        match self.kind_at(0) {
            Some(crate::lexer::TokKind::InterpMid | crate::lexer::TokKind::InterpTail) => {
                "`}`".to_string()
            }

            _ => format!("`{}`", self.text()),
        }
    }

    /// Reports if a newline sits between the token `n` ahead and the one
    /// after it. Expression-position words need their operand on the same
    /// line, or `local x = new` and `print(x)` on the next line would join.
    fn newline_after(&self, n: usize) -> bool {
        crate::contextual::newline_after(self.src, self.toks, self.pos + n)
    }

    /// Reports if the contextual word at the cursor is a prefix operator
    /// here: `new`, `await`, `try`, `async`, `delete`. The rule lives in
    /// [`crate::contextual`], so the formatter and the server read it too.
    fn prefix_word_here(&self) -> bool {
        crate::contextual::prefix_operand_follows(self.src, self.toks, self.pos)
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

        // `!=` reads as `~=` once `suffix_chain` reported it.
        if self.at("!") && self.text_at(1) == "=" && self.adjacent(0) {
            return binop_priority("~=").map(|p| (2, p));
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

    /// Records a diagnostic at a byte offset, up to the cap.
    fn report_at(&mut self, offset: usize, message: &str) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics.push(ParseError {
                offset,
                message: message.to_string(),
            });
        }
    }

    /// Records a diagnostic in lenient mode, up to the cap.
    fn report(&mut self, message: &str) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            let e = self.err(message);
            self.diagnostics.push(e);
        }
    }

    /// The one-based column a token starts at, in bytes.
    fn column_at(&self, i: usize) -> usize {
        let Some(t) = self.toks.get(i) else {
            return 0;
        };
        let before = &self.src[..t.start as usize];

        before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1
    }

    /// The source a token span covers.
    fn span_text(&self, span: TokSpan) -> &'a str {
        span.text(self.src, self.toks)
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

/*
The Alloy words a file may also use as a name.

Luau reserves none of them, and each one appears in ordinary Roblox code:
`Instance.new` lands in a local named `new`, a module table is named
`export`, `try` holds `pcall`, `match` holds a string helper, and `const`
is a Luau declaration Alloy passes through. Each word keeps its keyword
reading only where the token after it cannot follow a name. Luau does the
same with `export`: `export type T = number` and `local export = 1` live
in one file, because `type` is what makes the word a keyword.

The disambiguation lives at each dispatch, because the token that decides
differs per word:

- `new` is a constructor before a name, `Parser::prefix_word_here` plus a
  name. `new(x)`, `new.field`, `new = 1`, and `new "s"` are the name.
- `export` opens a declaration before `type`, `{`, `default`, or a
  declaration word. Anything else is the name.
- `try` is the operator before an operand on the same line,
  `Parser::prefix_word_here`. `try(f)`, `try = 1`, `try.x` are the name.
- `match` is the statement or the expression when
  `Parser::match_follows` holds. `match(x)`, `match = 1`, `match[k]`
  without a `with` are the name.
- `const` declares when a name, `function`, `@`, `[`, or `{` follows.
  `const = 1`, `const(x)`, `const.x` are the name.
- `async` is the modifier before `function` or `do` on the same line.
- `await` is the operator before an operand on the same line.
- `delete` and `destroy` take a name, a string, or a number, so
  `local function destroy(self)` and `self:destroy()` are the name.
- `after` takes a delay and then `do`, `Parser::after_delay_follows`.
  `after(x)`, `after = 1`, and `after[1] = 2` are the name.
- `enum`, `struct`, `interface`, `trait`, `impl`, `attribute`, `macro`,
  and `namespace` declare before a name on the same line, and `remote`
  before a name or `function`. `local enum = t`, `enum.Idle`, and
  `local remote = folder.Hit` are the name.
- `private` and `public` mark a member before a name or `function`.
  `local private = {}` is the name.
- `import` opens the statement before `*`, `{`, `type {`, or `Name from`.
  The call `import("m")` is the expression form and stays the keyword,
  because the emit turns it into a `require`; a file that binds a local
  named `import` must call it some other way.
*/
pub use crate::contextual::is_contextual;

fn is_reserved(word: &str) -> bool {
    crate::contextual::is_luau_reserved(word)
}

fn is_unary_op(s: &str) -> bool {
    matches!(s, "not" | "-" | "#")
}

pub use crate::contextual::is_compound_op;

pub use crate::contextual::binop_priority;
