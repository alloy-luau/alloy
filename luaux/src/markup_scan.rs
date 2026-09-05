//! Deciding whether a `<` opens LuauX or is a comparison operator.
//!
//! The base rule is the previous significant token — the same trick JS lexers
//! use for regex-vs-divide. Luau has no prefix `<` operator, so a `<` after a
//! token that cannot end an expression can only be LuauX.
//!
//! That rule alone is not enough. Luau *does* have a turbofish, spelled
//! `identity<<number>>(1)`, and generic function types can appear in positions
//! the base rule reads as expressions. Phase 0 ran this over 1,056 files of real
//! Luau and found four false-positive classes, each guarded here and pinned by
//! tests: `<<` instantiation, anonymous generic function expressions,
//! parenthesised generic function types, and `type` declarations following a
//! block `end`. See PLAN.md §5.2.
//!
//! The dangerous direction is the opposite one — type context swallowing real
//! LuauX, which nothing reports. That is why [`Scanner::enters_type_context`]
//! treats a bare `:` so narrowly.

use crate::lexer::{Lexer, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LuauxSite {
    /// Index into the token slice of the opening `<`.
    pub token_index: usize,
    /// Byte offset of the opening `<`.
    pub offset: usize,
}

/// Keywords that can legally end an expression, and so make a following `<` a
/// comparison. `end` counts because `function() end` is an expression.
const EXPRESSION_ENDING_KEYWORDS: &[&str] = &["end", "true", "false", "nil"];

/// Reserved words that cannot appear inside a type expression. Seeing one means
/// any in-progress `type X = ...` declaration is over.
const STATEMENT_KEYWORDS: &[&str] = &[
    "local", "function", "return", "if", "while", "for", "do", "end", "else", "elseif", "repeat",
    "until", "then", "in", "break", "continue",
];

/// Symbols after which a `<` introduces a *type*, not LuauX.
///
/// - `:`  — `local f: <T>(T) -> T`
/// - `::` — `x :: <T>(T) -> T`
/// - `->` — `type F = () -> <T>(T) -> T`
/// - `|`, `&` — union/intersection members; Luau has no such expression
///   operators, so these only ever occur in type position
const TYPE_INTRODUCING_SYMBOLS: &[&str] = &[":", "::", "->", "|", "&"];

/// Keywords after which a `<` opens a generic parameter list.
///
/// Only the anonymous form needs this: `function<T>() end` is a generic function
/// *expression*. Named forms (`local function f<T>()`, `function t:m<T>()`) put
/// an identifier before the `<`, which the base rule already reads as a
/// comparison.
const TYPE_PARAMETER_KEYWORDS: &[&str] = &["function"];

/// Incremental LuauX-entry detector.
///
/// The compiler drives this token by token so that lexing can stop the moment a
/// LuauX region opens (see [`crate::lexer::Lexer`] on why whole-file tokenizing
/// cannot work). All the cross-token state — bracket depth, type context, and
/// whether a `type` declaration is in progress — lives here so it survives the
/// hand-off to the LuauX parser and back.
pub struct Scanner<'a> {
    src: &'a str,
    /// Bracket nesting.
    depth: i32,
    /// Depth at which the current type expression began, if any.
    type_context: Option<i32>,
    in_type_declaration: bool,
    previous: Option<Token>,
    /// A consumed LuauX region acts as a previous token that ends an expression.
    previous_was_luaux: bool,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            depth: 0,
            type_context: None,
            in_type_declaration: false,
            previous: None,
            previous_was_luaux: false,
        }
    }

    /// Feeds one token. Returns `true` if it is a `<` that opens LuauX, in which
    /// case the caller should hand off to the LuauX parser and *not* treat this
    /// token as consumed context.
    ///
    /// `lookahead` must be positioned just past `token`; it is only consulted
    /// for `type` and `:`, both of which are always inside Luau.
    pub fn feed(&mut self, token: Token, lookahead: &Lexer<'a>) -> bool {
        if token.is_trivia() {
            return false;
        }

        let text = token.text(self.src);

        if matches!(text, ")" | "]" | "}") {
            self.depth -= 1;
        }

        if let Some(entry) = self.type_context {
            if self.depth < entry || (self.depth == entry && ends_type_expression(&token, text)) {
                self.type_context = None;
            }
        }

        if token.kind == TokenKind::Name && STATEMENT_KEYWORDS.contains(&text) {
            self.in_type_declaration = false;
            self.type_context = None;
        }

        if token.kind == TokenKind::Name
            && text == "type"
            && self.starts_type_declaration(&token, lookahead)
        {
            self.in_type_declaration = true;
        }

        if token.kind == TokenKind::Symbol && text == "<" && self.opens_luaux() {
            return true;
        }

        if self.enters_type_context(&token, text, lookahead) {
            self.type_context = Some(self.depth);
            if text == "=" {
                self.in_type_declaration = false;
            }
        }

        if matches!(text, "(" | "[" | "{") {
            self.depth += 1;
        }

        self.previous = Some(token);
        self.previous_was_luaux = false;
        false
    }

    /// Records that a LuauX region was parsed and consumed.
    pub fn note_luaux_region(&mut self) {
        self.previous = None;
        self.previous_was_luaux = true;
    }

    fn opens_luaux(&self) -> bool {
        if self.type_context.is_some() {
            return false;
        }

        // A LuauX region is an expression, so a `<` right after one is a
        // comparison.
        if self.previous_was_luaux {
            return false;
        }

        let Some(previous) = &self.previous else {
            // Nothing before it. In a whole file that would not be valid Luau,
            // but the compiler also runs over captured sub-expressions, where a
            // leading `<` is exactly how `<Frame>{<TextLabel/>}</Frame>` looks.
            // Treating it as LuauX is right there and harmless at file level,
            // where the alternative is an equally loud error either way.
            return true;
        };

        let text = previous.text(self.src);

        if previous.kind == TokenKind::Symbol && TYPE_INTRODUCING_SYMBOLS.contains(&text) {
            return false;
        }

        if previous.kind == TokenKind::Name && TYPE_PARAMETER_KEYWORDS.contains(&text) {
            return false;
        }

        !can_end_expression(previous, text)
    }

    /// `type` is contextual in Luau: `type(x)` is the builtin and `local type =
    /// 1` is a variable. It only introduces a declaration at statement position,
    /// when followed by a name and then `=` or a generic parameter list.
    ///
    /// Statement position deliberately does *not* reuse [`can_end_expression`].
    /// `end` can end an expression (`function() end`) yet almost always closes a
    /// block, and `type Create = ...` directly after an `end` is exactly the
    /// shape that appears in Vide's source. A newline between the previous token
    /// and `type` is the reliable signal, and it correctly rejects a same-line
    /// `local f = type`.
    fn starts_type_declaration(&self, token: &Token, lookahead: &Lexer<'a>) -> bool {
        let at_statement_start = match &self.previous {
            None => true,
            Some(previous) => {
                let text = previous.text(self.src);
                // `export type Foo = ...`
                (previous.kind == TokenKind::Name && text == "export")
                    || text == ";"
                    || self.src[previous.end..token.start].contains('\n')
            }
        };

        if !at_statement_start {
            return false;
        }

        let mut after = lookahead.clone();

        let named = after
            .peek_significant()
            .is_some_and(|next| next.kind == TokenKind::Name && !is_keyword(next.text(self.src)));

        if !named {
            return false;
        }

        // Advance past the name, then require `=` or `<`.
        let Some(name) = after.peek_significant() else {
            return false;
        };
        after.seek(name.end);

        after
            .peek_significant()
            .is_some_and(|next| matches!(next.text(self.src), "=" | "<"))
    }

    /// Tokens after which the following tokens are a type expression.
    ///
    /// `::`, `->`, `|` and `&` are unambiguous — none are Luau expression
    /// operators. A bare `:` is not: `obj:method(<Frame/>)` and `x: Type` share
    /// a prefix, and treating the method call as a type would silently swallow
    /// real LuauX. So `:` only enters when followed directly by `(`, which is the
    /// generic function type form and cannot be a method call.
    fn enters_type_context(&self, token: &Token, text: &str, lookahead: &Lexer<'a>) -> bool {
        if token.kind != TokenKind::Symbol {
            return false;
        }

        if matches!(text, "::" | "->" | "|" | "&") {
            return true;
        }

        if text == "=" && self.in_type_declaration {
            return true;
        }

        text == ":"
            && lookahead
                .peek_significant()
                .is_some_and(|next| next.kind == TokenKind::Symbol && next.text(self.src) == "(")
    }
}

/// Reports every `<` the rules treat as opening LuauX, over an already-tokenized
/// source.
///
/// This is for the Phase 0 acceptance check, which runs over sources containing
/// no LuauX at all — so whole-file tokenizing is safe there and regions never need
/// consuming. The compiler itself drives [`Scanner`] incrementally instead.
pub fn find_luaux_sites(src: &str, tokens: &[Token]) -> Vec<LuauxSite> {
    let mut scanner = Scanner::new(src);
    let mut sites = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        let lookahead = Lexer::at(src, token.end);

        if scanner.feed(*token, &lookahead) {
            sites.push(LuauxSite {
                token_index: index,
                offset: token.start,
            });
            // Keep scanning as if the `<` were an ordinary token so that a
            // corpus run reports every candidate rather than stopping.
            scanner.note_luaux_region();
        }
    }

    sites
}

/// Whether this token, at the depth the type expression started, ends it.
/// Whether a token can be the last token of an expression, which makes a
/// following `<` a comparison rather than the start of LuauX.
fn can_end_expression(token: &Token, text: &str) -> bool {
    match token.kind {
        TokenKind::Number | TokenKind::Str | TokenKind::InterpStr => true,
        TokenKind::Name => !is_keyword(text) || EXPRESSION_ENDING_KEYWORDS.contains(&text),
        // Alloy patch: `x!` is a non-nil assert, so `x! < y` compares.
        TokenKind::Symbol => matches!(text, ")" | "]" | "}" | "..." | "!"),
        TokenKind::Comment | TokenKind::Whitespace => {
            debug_assert!(false, "trivia should be filtered before classification");
            false
        }
    }
}

/// Whether this token, at the depth the type expression started, ends it.
fn ends_type_expression(token: &Token, text: &str) -> bool {
    if token.kind == TokenKind::Symbol {
        return matches!(text, "=" | "," | ";");
    }

    token.kind == TokenKind::Name && STATEMENT_KEYWORDS.contains(&text)
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn count(src: &str) -> usize {
        let tokens = tokenize(src).expect("lex");
        find_luaux_sites(src, &tokens).len()
    }

    // --- positives: `<` in expression position ---

    #[test]
    fn detects_after_assignment() {
        assert_eq!(count("local x = <Frame/>"), 1);
    }

    #[test]
    fn detects_after_return_and_paren() {
        assert_eq!(count("return (<Frame/>)"), 1);
        assert_eq!(count("f(<Frame/>)"), 1);
    }

    #[test]
    fn detects_in_call_arguments_and_tables() {
        assert_eq!(count("table.insert(t, (<Frame/>))"), 1);
        assert_eq!(count("local t = { <Frame/>, <Frame/> }"), 2);
    }

    #[test]
    fn detects_after_logical_operators() {
        assert_eq!(count("local x = cond and <Frame/> or nil"), 1);
    }

    #[test]
    fn detects_fragments() {
        // Two hits, not one: the scanner reports candidates and does not consume
        // a region, so the `<` of the closing `</>` is counted as well. The LuauX
        // parser takes over that job in Phase 1.
        assert_eq!(count("local x = (<></>)"), 2);
    }

    #[test]
    fn reports_the_opening_tag_first() {
        let src = "local x = (<></>)";
        let tokens = tokenize(src).expect("lex");
        let sites = find_luaux_sites(src, &tokens);
        assert_eq!(sites[0].offset, src.find('<').unwrap());
    }

    // --- negatives: comparisons ---

    #[test]
    fn ignores_comparison_after_identifier() {
        assert_eq!(count("if a < b then end"), 0);
        assert_eq!(count("while i < #list do end"), 0);
    }

    #[test]
    fn ignores_comparison_after_literals_and_closers() {
        assert_eq!(count("if 1 < 2 then end"), 0);
        assert_eq!(count("if f() < 2 then end"), 0);
        assert_eq!(count("if t[1] < 2 then end"), 0);
        assert_eq!(count("if {} < 2 then end"), 0);
        assert_eq!(count("if 'a' < 'b' then end"), 0);
        assert_eq!(count("if `a` < `b` then end"), 0);
    }

    #[test]
    fn ignores_chained_comparison() {
        assert_eq!(count("local c = a < b < c"), 0);
    }

    #[test]
    fn ignores_less_than_or_equal() {
        assert_eq!(count("if a <= b then end"), 0);
    }

    // --- negatives: type position ---

    #[test]
    fn ignores_generic_type_arguments() {
        // `<` follows the type name, an identifier, so the base rule covers it.
        assert_eq!(count("local m: Map<string, Frame> = f()"), 0);
        assert_eq!(count("type A = Array<number>"), 0);
        assert_eq!(count("local x = y :: Map<string, number>"), 0);
    }

    #[test]
    fn ignores_generic_function_declarations() {
        assert_eq!(count("local function f<T>(v: T): T return v end"), 0);
        assert_eq!(count("type Fn<T> = (T) -> T"), 0);
    }

    #[test]
    fn ignores_generic_function_types() {
        assert_eq!(count("local f: <T>(T) -> T = g"), 0);
        assert_eq!(count("local x = y :: <T>(T) -> T"), 0);
        assert_eq!(count("type F = <T>(T) -> T"), 0);
        assert_eq!(count("export type F = <T>(T) -> T"), 0);
        assert_eq!(count("type F = () -> <T>(T) -> T"), 0);
        assert_eq!(count("type F = number | <T>(T) -> T"), 0);
    }

    // Regressions found by the Phase 0 corpus run over the Luau repository.

    #[test]
    fn ignores_explicit_type_instantiation() {
        // Luau *does* have a turbofish, spelled `<<...>>`.
        // tests/conformance/explicit_type_instantiations.luau
        assert_eq!(count("assert(identity<<number>>(1) == 1)"), 0);
        assert_eq!(
            count("local a, b = typePacks<<(string, number)>>(1, 'a')"),
            0
        );
        assert_eq!(
            count("local a, b = t:methodTypePacks<<(string, number)>>(1, 'a')"),
            0
        );
    }

    #[test]
    fn ignores_generic_function_expressions() {
        // tests/conformance/interrupt.luau and native.luau
        assert_eq!(count("repeat continue until function<t0>() end"), 0);
        assert_eq!(
            count("for l0 in pcall, function<A...>(...): any end do end"),
            0
        );
    }

    #[test]
    fn ignores_parenthesised_generic_function_types() {
        // All eight false positives from the Vide and react-lua corpus run.
        assert_eq!(
            count("return source :: (<T>(initial_value: T) -> Source<T>) & (<T>() -> Source<T>)"),
            0
        );
        assert_eq!(
            count("export type Context<T> = (() -> T) & (<U>(T, () -> U) -> U)"),
            0
        );
        assert_eq!(
            count("local t = { useDeferredValue: (<T>(value: T) -> T)? }"),
            0
        );
        assert_eq!(
            count("return untrack :: ( <T>(fn: () -> T) -> T ) & ( (fn: () -> ()) -> () )"),
            0
        );
    }

    #[test]
    fn method_calls_are_not_type_context() {
        // `:` is shared between method calls and annotations. Entering type
        // context on a bare `:` would silently swallow this LuauX.
        assert_eq!(count("obj:method(<Frame/>)"), 1);
        assert_eq!(count("local x = t:render(<Frame/>, <Frame/>)"), 2);
    }

    #[test]
    fn type_context_ends_at_assignment_and_separators() {
        assert_eq!(count("local x: Frame = <Frame/>"), 1);
        assert_eq!(
            count("local x: () -> Frame = function() return <Frame/> end"),
            1
        );
        assert_eq!(count("f(a :: T, <Frame/>)"), 1);
        assert_eq!(count("local t = { a = 1 :: number, b = <Frame/> }"), 1);
    }

    #[test]
    fn type_context_ends_at_statement_keywords() {
        assert_eq!(
            count("function Component(props: Props): Frame return <Frame/> end"),
            1
        );
    }

    #[test]
    fn type_keyword_is_contextual() {
        // `type(x)` is the builtin, not a declaration, so the `=` that follows
        // must still be treated as expression position.
        assert_eq!(count("local k = type(x)\nlocal e = <Frame/>"), 1);
        assert_eq!(count("local type = 1\nlocal e = <Frame/>"), 1);
    }

    #[test]
    fn type_declaration_does_not_leak_into_later_statements() {
        assert_eq!(count("type F = <T>(T) -> T\nlocal e = <Frame/>"), 1);
    }

    #[test]
    fn type_declaration_recognised_after_a_block_end() {
        // vide/src/create.luau:105 — `end` closes the preceding function, and
        // `end` also legitimately ends an expression, so statement-start
        // detection has to key on the newline instead.
        assert_eq!(
            count("local function f() end\ntype Create = <Name>(Name) -> Name"),
            0
        );
        assert_eq!(count("local x = 1\ntype F = <T>(T) -> T"), 0);
    }

    #[test]
    fn type_builtin_on_the_same_line_is_not_a_declaration() {
        assert_eq!(count("local f = type\nfoo = <Frame/>"), 1);
    }

    // --- negatives: trivia and strings must not be misread ---

    #[test]
    fn ignores_angle_brackets_in_strings_and_comments() {
        assert_eq!(count("local s = '<Frame/>'"), 0);
        assert_eq!(count("-- local x = <Frame/>"), 0);
        assert_eq!(count("--[[ local x = <Frame/> ]]"), 0);
        assert_eq!(count("local s = [[ <Frame/> ]]"), 0);
        assert_eq!(count("local s = `<Frame/>`"), 0);
    }

    #[test]
    fn sees_through_trivia_to_the_previous_token() {
        assert_eq!(count("local x = -- note\n  <Frame/>"), 1);
        assert_eq!(count("if a --[[ c ]] < b then end"), 0);
    }
}
