//! Luau lexer.
//!
//! Produces a flat token stream over the whole source. This is deliberately not
//! a parser: LuauX only appears in expression position and lowers to an ordinary
//! Luau expression, so the compiler needs to know *where* expression position is
//! and nothing more (see PLAN.md §5.1).
//!
//! The tokenizer must still be exactly right about the lexical forms that can
//! swallow a `<`: strings, long strings, comments, and interpolated strings.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Identifier or keyword. Distinguished by text, since only a handful of
    /// keywords matter to LuauX detection.
    Name,
    Number,
    /// Quoted (`'`/`"`) or long (`[[ ]]`) string.
    Str,
    /// Backtick-interpolated string.
    InterpStr,
    Symbol,
    Comment,
    Whitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }

    /// Whitespace and comments carry no meaning for LuauX detection.
    pub fn is_trivia(&self) -> bool {
        matches!(self.kind, TokenKind::Whitespace | TokenKind::Comment)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub offset: usize,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for LexError {}

/// Multi-byte symbols, longest first so that greedy matching is correct.
/// `<<` is Luau's explicit type instantiation opener (`identity<<number>>(1)`).
/// Lexing it as one token is what keeps the second `<` from looking like the
/// start of LuauX. The closing `>>` is deliberately left as two `>` tokens so that
/// nested generics (`Map<string, Array<number>>`) still close correctly.
const SYMBOLS: &[&str] = &[
    "...", "..=", "//=", //
    "..", "==", "~=", "<=", ">=", "::", "->", "+=", "-=", "*=", "/=", "%=", "^=", "//", "<<",
];

pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(src).run()
}

/// Given the byte offset of a `{`, returns the offset of the matching `}`.
///
/// Brace matching has to respect strings, long strings, comments, and nested
/// interpolation — `{ "}" }` closes at the second brace, not the one inside the
/// string. The LuauX parser needs this to delimit `{expr}` attribute values and
/// children without parsing the Luau inside them (PLAN.md §5.1).
pub fn find_matching_brace(src: &str, open: usize) -> Result<usize, LexError> {
    debug_assert_eq!(src.as_bytes().get(open), Some(&b'{'));

    let mut lexer = Lexer::new(src);
    lexer.pos = open + 1;
    lexer.scan_balanced_braces(open)?;

    // scan_balanced_braces lands just past the closing brace.
    Ok(lexer.pos - 1)
}

/// A resumable Luau lexer.
///
/// Resumability is not a convenience — it is required. A `.luaux` file is not
/// Luau end to end: LuauX text is a different lexical mode, where an apostrophe
/// (`don't`), a backtick, or `--` carries no Luau meaning. Tokenizing the whole
/// file up front would fail on any of them. So the compiler lexes until a LuauX
/// region opens, hands off to the LuauX parser, and resumes past it
/// (PLAN.md §5.4).
#[derive(Clone)]
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self::at(src, 0)
    }

    pub fn at(src: &'a str, pos: usize) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos,
        }
    }

    /// Restarts lexing at `pos`, used to skip past a parsed LuauX region.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn offset(&self) -> usize {
        self.pos
    }

    pub fn next_token(&mut self) -> Option<Result<Token, LexError>> {
        if self.pos >= self.bytes.len() {
            return None;
        }

        let start = self.pos;

        Some(match self.scan_one() {
            Ok(kind) => {
                debug_assert!(self.pos > start, "lexer failed to advance");
                Ok(Token {
                    kind,
                    start,
                    end: self.pos,
                })
            }
            Err(error) => Err(error),
        })
    }

    /// Next non-trivia token, without disturbing this lexer.
    pub fn peek_significant(&self) -> Option<Token> {
        let mut lookahead = self.clone();

        loop {
            match lookahead.next_token()? {
                Ok(token) if token.is_trivia() => continue,
                Ok(token) => return Some(token),
                Err(_) => return None,
            }
        }
    }

    fn run(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();

        while let Some(token) = self.next_token() {
            tokens.push(token?);
        }

        Ok(tokens)
    }

    fn byte(&self) -> u8 {
        self.bytes[self.pos]
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn err<T>(&self, message: impl Into<String>, offset: usize) -> Result<T, LexError> {
        Err(LexError {
            message: message.into(),
            offset,
        })
    }

    fn scan_one(&mut self) -> Result<TokenKind, LexError> {
        let c = self.byte();

        if c.is_ascii_whitespace() {
            while self.pos < self.bytes.len() && self.byte().is_ascii_whitespace() {
                self.pos += 1;
            }
            return Ok(TokenKind::Whitespace);
        }

        // Comments must be checked before the `-` symbol.
        if c == b'-' && self.peek(1) == Some(b'-') {
            self.scan_comment()?;
            return Ok(TokenKind::Comment);
        }

        if c == b'[' {
            if let Some(level) = self.long_bracket_level() {
                self.scan_long_bracket(level)?;
                return Ok(TokenKind::Str);
            }
        }

        if c == b'\'' || c == b'"' {
            self.scan_quoted(c)?;
            return Ok(TokenKind::Str);
        }

        if c == b'`' {
            self.scan_interp_string()?;
            return Ok(TokenKind::InterpStr);
        }

        if c.is_ascii_digit() {
            self.scan_number();
            return Ok(TokenKind::Number);
        }

        // `.5` is a number; `..`/`...` are symbols; a lone `.` is a symbol.
        if c == b'.' && matches!(self.peek(1), Some(d) if d.is_ascii_digit()) {
            self.scan_number();
            return Ok(TokenKind::Number);
        }

        if is_name_start(c) {
            self.pos += 1;
            while self.pos < self.bytes.len() && is_name_continue(self.byte()) {
                self.pos += 1;
            }
            return Ok(TokenKind::Name);
        }

        for symbol in SYMBOLS {
            if self.src[self.pos..].starts_with(symbol) {
                self.pos += symbol.len();
                return Ok(TokenKind::Symbol);
            }
        }

        // Any other single byte is a symbol. Non-ASCII outside a string or
        // comment is not valid Luau, but the lexer is not the right place to
        // reject it — advance so callers still get a usable token stream.
        self.pos += 1;
        while self.pos < self.bytes.len() && !self.src.is_char_boundary(self.pos) {
            self.pos += 1;
        }

        Ok(TokenKind::Symbol)
    }

    fn scan_comment(&mut self) -> Result<(), LexError> {
        self.pos += 2; // `--`

        if self.pos < self.bytes.len() && self.byte() == b'[' {
            if let Some(level) = self.long_bracket_level() {
                return self.scan_long_bracket(level);
            }
        }

        while self.pos < self.bytes.len() && self.byte() != b'\n' {
            self.pos += 1;
        }

        Ok(())
    }

    /// If the cursor sits on a long-bracket opener (`[[`, `[=[`, `[==[`, ...),
    /// returns the number of `=` signs.
    fn long_bracket_level(&self) -> Option<usize> {
        debug_assert_eq!(self.byte(), b'[');

        let mut level = 0;
        loop {
            match self.peek(1 + level) {
                Some(b'=') => level += 1,
                Some(b'[') => return Some(level),
                _ => return None,
            }
        }
    }

    fn scan_long_bracket(&mut self, level: usize) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 2 + level; // `[` + `=`*level + `[`

        while self.pos < self.bytes.len() {
            if self.byte() == b']' {
                let mut matched = 0;
                while self.peek(1 + matched) == Some(b'=') {
                    matched += 1;
                }

                if matched == level && self.peek(1 + matched) == Some(b']') {
                    self.pos += 2 + level;
                    return Ok(());
                }
            }

            self.pos += 1;
        }

        self.err("unterminated long bracket", start)
    }

    fn scan_quoted(&mut self, quote: u8) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 1;

        while self.pos < self.bytes.len() {
            match self.byte() {
                b'\\' => self.scan_escape(),
                c if c == quote => {
                    self.pos += 1;
                    return Ok(());
                }
                b'\n' => return self.err("unterminated string", start),
                _ => self.pos += 1,
            }
        }

        self.err("unterminated string", start)
    }

    /// Consumes a backslash escape.
    ///
    /// `\z` is special: it skips *all* following whitespace, newlines included,
    /// so a string legally continues onto the next line. Everything else
    /// consumes the backslash plus one character — enough to find the end of the
    /// string, and correct for `\n`, `\\`, `\"`, `\u{...}` and `\<newline>`.
    fn scan_escape(&mut self) {
        debug_assert_eq!(self.byte(), b'\\');

        if self.peek(1) == Some(b'z') {
            self.pos = (self.pos + 2).min(self.bytes.len());
            while self.pos < self.bytes.len() && self.byte().is_ascii_whitespace() {
                self.pos += 1;
            }
            return;
        }

        self.pos += 1;
        self.advance_char();
    }

    /// Advances one UTF-8 character, so an escaped multi-byte character never
    /// leaves the cursor mid-codepoint (slicing there would panic).
    fn advance_char(&mut self) {
        if self.pos >= self.bytes.len() {
            return;
        }

        self.pos += 1;
        while self.pos < self.bytes.len() && !self.src.is_char_boundary(self.pos) {
            self.pos += 1;
        }
    }

    /// Backtick strings contain `{ ... }` holes holding arbitrary Luau, which
    /// may themselves contain strings, comments, and further interpolation.
    fn scan_interp_string(&mut self) -> Result<(), LexError> {
        let start = self.pos;
        self.pos += 1;

        while self.pos < self.bytes.len() {
            match self.byte() {
                b'\\' => self.scan_escape(),
                b'`' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'{' => {
                    self.pos += 1;
                    self.scan_balanced_braces(start)?;
                }
                _ => self.pos += 1,
            }
        }

        self.err("unterminated interpolated string", start)
    }

    /// Cursor is just past a `{`. Consumes through the matching `}`.
    fn scan_balanced_braces(&mut self, origin: usize) -> Result<(), LexError> {
        let mut depth = 1usize;

        while self.pos < self.bytes.len() {
            match self.byte() {
                b'{' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' => {
                    depth -= 1;
                    self.pos += 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                c @ (b'\'' | b'"') => self.scan_quoted(c)?,
                b'`' => self.scan_interp_string()?,
                b'-' if self.peek(1) == Some(b'-') => self.scan_comment()?,
                b'[' if self.long_bracket_level().is_some() => {
                    let level = self.long_bracket_level().unwrap();
                    self.scan_long_bracket(level)?;
                }
                _ => self.pos += 1,
            }
        }

        self.err("unterminated interpolation", origin)
    }

    fn scan_number(&mut self) {
        // Hex and binary literals.
        if self.byte() == b'0' {
            if let Some(marker) = self.peek(1) {
                if marker == b'x' || marker == b'X' || marker == b'b' || marker == b'B' {
                    self.pos += 2;
                    while self.pos < self.bytes.len()
                        && (self.byte().is_ascii_alphanumeric() || self.byte() == b'_')
                    {
                        self.pos += 1;
                    }
                    return;
                }
            }
        }

        while self.pos < self.bytes.len() {
            let c = self.byte();

            if c.is_ascii_digit() || c == b'_' || c == b'.' {
                self.pos += 1;
            } else if c == b'e' || c == b'E' {
                self.pos += 1;
                if matches!(self.peek(0), Some(b'+') | Some(b'-')) {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }
}

fn is_name_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_name_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        tokenize(src)
            .expect("lex")
            .into_iter()
            .filter(|t| !t.is_trivia())
            .map(|t| (t.kind, t.text(src)))
            .collect()
    }

    #[test]
    fn lexes_basic_statement() {
        assert_eq!(
            kinds("local x = 1"),
            vec![
                (TokenKind::Name, "local"),
                (TokenKind::Name, "x"),
                (TokenKind::Symbol, "="),
                (TokenKind::Number, "1"),
            ]
        );
    }

    #[test]
    fn lexes_long_strings_and_comments() {
        assert_eq!(
            kinds("--[==[ <Frame/> ]==]\n[[ raw ]]"),
            vec![(TokenKind::Str, "[[ raw ]]")]
        );
    }

    #[test]
    fn line_comment_swallows_angle_brackets() {
        assert!(kinds("-- <Frame/>").is_empty());
    }

    #[test]
    fn lexes_interpolation_with_nested_braces() {
        assert_eq!(
            kinds("`a {({b = 1}).b} c`"),
            vec![(TokenKind::InterpStr, "`a {({b = 1}).b} c`")]
        );
    }

    #[test]
    fn lexes_nested_interpolation() {
        assert_eq!(
            kinds("`outer {`inner {x}`} end`"),
            vec![(TokenKind::InterpStr, "`outer {`inner {x}`} end`")]
        );
    }

    #[test]
    fn interpolation_ignores_braces_in_strings() {
        assert_eq!(
            kinds(r#"`a {"}"} b`"#),
            vec![(TokenKind::InterpStr, r#"`a {"}"} b`"#)]
        );
    }

    #[test]
    fn escaped_backtick_does_not_end_string() {
        assert_eq!(
            kinds(r#"`a \` b`"#),
            vec![(TokenKind::InterpStr, r#"`a \` b`"#)]
        );
    }

    #[test]
    fn lexes_compound_and_multichar_symbols() {
        assert_eq!(
            kinds("a ..= b :: T -> U ... //="),
            vec![
                (TokenKind::Name, "a"),
                (TokenKind::Symbol, "..="),
                (TokenKind::Name, "b"),
                (TokenKind::Symbol, "::"),
                (TokenKind::Name, "T"),
                (TokenKind::Symbol, "->"),
                (TokenKind::Name, "U"),
                (TokenKind::Symbol, "..."),
                (TokenKind::Symbol, "//="),
            ]
        );
    }

    #[test]
    fn lexes_numbers() {
        assert_eq!(
            kinds("0xFF 0b1010 1_000 1.5e-3 .5"),
            vec![
                (TokenKind::Number, "0xFF"),
                (TokenKind::Number, "0b1010"),
                (TokenKind::Number, "1_000"),
                (TokenKind::Number, "1.5e-3"),
                (TokenKind::Number, ".5"),
            ]
        );
    }

    #[test]
    fn handles_non_ascii_in_strings() {
        assert_eq!(
            kinds("local s = 'héllo — ok'"),
            vec![
                (TokenKind::Name, "local"),
                (TokenKind::Name, "s"),
                (TokenKind::Symbol, "="),
                (TokenKind::Str, "'héllo — ok'"),
            ]
        );
    }

    #[test]
    fn z_escape_continues_a_string_across_lines() {
        // tests/conformance/tpack.luau and utf8.luau
        let src = "local s = \"first \\z\n   second\"";
        assert_eq!(
            kinds(src),
            vec![
                (TokenKind::Name, "local"),
                (TokenKind::Name, "s"),
                (TokenKind::Symbol, "="),
                (TokenKind::Str, "\"first \\z\n   second\""),
            ]
        );
    }

    #[test]
    fn lexes_explicit_type_instantiation() {
        assert_eq!(
            kinds("identity<<number>>(1)"),
            vec![
                (TokenKind::Name, "identity"),
                (TokenKind::Symbol, "<<"),
                (TokenKind::Name, "number"),
                (TokenKind::Symbol, ">"),
                (TokenKind::Symbol, ">"),
                (TokenKind::Symbol, "("),
                (TokenKind::Number, "1"),
                (TokenKind::Symbol, ")"),
            ]
        );
    }

    #[test]
    fn escaped_multibyte_character_keeps_char_boundaries() {
        assert_eq!(
            kinds("local s = '\\é'"),
            vec![
                (TokenKind::Name, "local"),
                (TokenKind::Name, "s"),
                (TokenKind::Symbol, "="),
                (TokenKind::Str, "'\\é'"),
            ]
        );
    }

    #[test]
    fn rejects_unterminated_forms() {
        assert!(tokenize("'abc").is_err());
        assert!(tokenize("`abc").is_err());
        assert!(tokenize("[[abc").is_err());
    }
}
