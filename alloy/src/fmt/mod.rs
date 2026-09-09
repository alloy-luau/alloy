//! `alloy fmt`, Anneal: an objective formatter.
//!
//! The output depends on the tokens and the options, not on how the
//! author laid the code out, with the exceptions the options name: a
//! magic trailing comma keeps a bracket group expanded, and
//! `block_newline_gaps = "preserve"` keeps a blank line at the edge of a
//! block. Everything else is decided here: the spacing between tokens,
//! which bracket groups break and how, the quotes of a string, the
//! parentheses of a call, the leading zero of a number.
//!
//! The token stream of the output is the token stream of the input, save
//! for the quote and parenthesis rewrites the options ask for, so the
//! program is the same and a second run changes nothing.
//!
//! Statements keep their lines: a newline between two statements in the
//! source is a newline in the output, and at most one blank line stays
//! between them. Inside a bracket group the source's newlines mean
//! nothing; the group lays itself out from the width.

use alloy_syntax::lexer::{Lexed, Tok, TokKind, lex};

#[cfg(test)]
use crate::config::{CallChainStyle, CallParentheses, Collapse, IndentType, RequireGrouping};
use crate::config::{FmtConfig, LineEndings, QuoteStyle};

pub use self::structure::{Step, Structure, structure};

/// Spaces per indentation level, for callers that only reindent.
pub const INDENT: usize = 4;

/// One lexical item: a token or a comment, in source order.
#[derive(Debug, Clone)]
struct Item {
    text: String,
    kind: ItemKind,
    /// Newlines in the source between the item before and this one.
    newlines_before: usize,
    /// Whether the source had whitespace right before this item.
    space_before: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Tok(TokKind),
    LineComment,
    LongComment,
}

impl Item {
    fn is_comment(&self) -> bool {
        matches!(self.kind, ItemKind::LineComment | ItemKind::LongComment)
    }

    fn is(&self, text: &str) -> bool {
        !self.is_comment() && self.text == text
    }

    fn is_ident(&self) -> bool {
        matches!(self.kind, ItemKind::Tok(TokKind::Ident))
    }

    fn is_string(&self) -> bool {
        matches!(
            self.kind,
            ItemKind::Tok(TokKind::Str { .. } | TokKind::InterpStr)
        )
    }

    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// A bracket group or one item.
#[derive(Debug)]
enum Node {
    Item(usize),
    Group {
        open: usize,
        close: usize,
        /// The elements between the separators, each with the separator
        /// that followed it, when one did.
        elements: Vec<(Vec<Node>, Option<usize>)>,
        /// A separator before the closer, with the closer on its own line.
        magic_comma: bool,
    },
}

/// Formats a whole file with the default options.
pub fn format(src: &str) -> Result<String, String> {
    format_file(src, &FmtConfig::default())
}

/// The head of the error `format_file` returns for a source the parser
/// cannot read. A caller tells that case from a real failure by it.
pub const UNPARSED: &str = "does not parse";

/// The tail of the report for an `impl` or a `trait` header without
/// `as`, for a caller that reads a diagnostic and wants the rewrite.
pub const NEEDS_AS: &str = alloy_syntax::parser::NEEDS_AS;

/// The rewrites that write the `as` an `impl` or a `trait` header is
/// missing, in source order. `alloy flux --fix` and the server's quick
/// fix apply these, so a file written before `as` migrates in place;
/// `alloy fmt` writes the same text as part of a whole format.
pub fn header_as_fixes(src: &str) -> Vec<crate::lint::Fix> {
    let Ok(Lexed { toks, .. }) = lex(src) else {
        return Vec::new();
    };
    let text = |i: usize| toks[i].text(src);
    let mut out = Vec::new();
    let mut i = 0;

    while i < toks.len() {
        if !matches!(text(i), "impl" | "trait") || !opens_a_header(src, &toks, i) {
            i += 1;

            continue;
        }

        let mut j = i + 1;
        let mut angle = 0usize;

        while j < toks.len() {
            let t = text(j);

            if angle > 0 {
                angle += usize::from(t == "<");
                angle -= usize::from(t == ">");
                j += 1;
            } else if t == "<" {
                angle += 1;
                j += 1;
            } else if t == "." || t == "for" || (toks[j].kind == TokKind::Ident && !is_keyword(t)) {
                j += 1;
            } else {
                break;
            }
        }

        if angle == 0 && j > i + 1 && j < toks.len() && text(j) != "as" {
            let at = toks[j - 1].end;
            out.push(crate::lint::Fix {
                start: at,
                end: at,
                replacement: " as".to_string(),
            });
        }

        i = j.max(i + 1);
    }

    out
}

/// Whether the `impl` or `trait` token at `i` opens a declaration: it
/// starts a line, follows `export`, or opens the file. The same rule
/// the formatter's `starts_block` uses, over raw tokens.
fn opens_a_header(src: &str, toks: &[Tok], i: usize) -> bool {
    if i == 0 {
        return true;
    }

    let prev = &toks[i - 1];

    matches!(prev.text(src), "export" | "global")
        || src[prev.end as usize..toks[i].start as usize].contains('\n')
}

/// The parser's first complaint about a whole file, if it has one. A
/// `.d.aly` file writes `declare`, so the definition syntax is allowed.
pub fn parse_error(src: &str) -> Option<String> {
    let options = alloy_syntax::parser::ParseOptions {
        definitions: true,
        ..Default::default()
    };

    match alloy_syntax::parse_lenient(src, options) {
        Err(e) => Some(e.message),

        // An `impl` or a `trait` header without `as` is the one report
        // the formatter reads past: the tree still covers every token,
        // and `header_as` writes the `as` the file is missing.
        Ok(parsed) => parsed
            .diagnostics
            .iter()
            .find(|d| !d.message.ends_with(alloy_syntax::parser::NEEDS_AS))
            .map(|d| d.message.clone()),
    }
}

/// Formats a whole file. The layout moves a statement into the block the
/// parser gives it, so a source with a missing `end` would come out as
/// another program: such a file keeps its text, and the error says so.
/// `format_with` skips this check, for the fragments an `.alx` hole
/// holds.
pub fn format_file(src: &str, options: &FmtConfig) -> Result<String, String> {
    match parse_error(src) {
        Some(message) => Err(format!("{UNPARSED}: {message}")),

        None => format_with(src, options),
    }
}

/// Formats Alloy source. `Err` carries the lexer's message: a file that
/// does not lex stays as it is.
pub fn format_with(src: &str, options: &FmtConfig) -> Result<String, String> {
    let Lexed { toks, comments } = lex(src).map_err(|e| e.message)?;
    let items = items_of(src, &toks, &comments);

    if items.is_empty() {
        return Ok(String::new());
    }

    let mut f = Formatter {
        items,
        options,
        lines: Vec::new(),
        line: String::new(),
        line_level: 0,
        depths: Vec::new(),
        generic: Vec::new(),
        signature: Vec::new(),
    };
    f.rewrite_tokens();
    f.sort_requires();
    f.collapse_simple_statements();
    f.break_call_chains();
    f.depths = f.block_depths();
    f.generic = f.generic_brackets();
    let tree = f.tree();
    let hard = f.hard_breaks(&tree);
    f.render_nodes(&tree, &hard, 0);
    f.flush();
    let mut text = f.finish();

    // The source keeps its own endings unless an option names one: a
    // Windows checkout would otherwise rewrite every line of every file.
    let windows = match options.line_endings {
        LineEndings::Windows => true,

        LineEndings::Unix => false,

        LineEndings::Input => src.contains("\r\n"),
    };

    // A long comment and a long string carry the source's own `\r\n`
    // through the token stream; the file's endings are one decision, so
    // they normalize first and the option writes them back.
    if text.contains('\r') {
        text = text.replace("\r\n", "\n");
    }

    if windows {
        text = text.replace('\n', "\r\n");
    }

    Ok(text)
}

/// Tokens and comments as one ordered list, with the whitespace facts
/// the layout needs.
fn items_of(src: &str, toks: &[Tok], comments: &[(u32, u32)]) -> Vec<Item> {
    let mut all: Vec<(usize, usize, ItemKind)> = toks
        .iter()
        .map(|t| (t.start as usize, t.end as usize, ItemKind::Tok(t.kind)))
        .collect();

    for (a, b) in comments {
        let text = &src[*a as usize..*b as usize];
        let long = text.len() > 3 && text.starts_with("--[") && text[3..].starts_with(['[', '=']);
        all.push((
            *a as usize,
            *b as usize,
            if long {
                ItemKind::LongComment
            } else {
                ItemKind::LineComment
            },
        ));
    }

    all.sort_by_key(|(a, _, _)| *a);
    let mut out = Vec::with_capacity(all.len());
    let mut prev_end = 0;

    for (a, b, kind) in all {
        let between = &src[prev_end..a];
        out.push(Item {
            text: src[a..b].to_string(),
            kind,
            newlines_before: between.matches('\n').count(),
            space_before: !between.is_empty(),
        });
        prev_end = b;
    }

    merge_operators(out)
}

/// The lexer emits `?`, `<`, and `>` one character at a time. The
/// operators built from them are one item here: `?.`, `?:`, `?[`, `?(`,
/// `??`, `??=`, and the `<<` `>>` of explicit type arguments.
fn merge_operators(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    let mut open_shl = 0usize;

    for it in items {
        let joined = match out.last() {
            Some(last) if !it.space_before && !it.is_comment() && !last.is_comment() => {
                let (a, b) = (last.text.as_str(), it.text.as_str());

                (a == "?" && matches!(b, "." | ":" | "[" | "(" | "?"))
                    || (a == "??" && b == "=")
                    // Luau has no `<<` operator, so two `<` with nothing
                    // between them after a name are the type-argument
                    // bracket, whatever stands before the first one.
                    || (a == "<" && b == "<" && out.len() >= 2 && out[out.len() - 2].is_ident())
                    || (a == ">" && b == ">" && open_shl > 0)
            }

            _ => false,
        };

        if joined {
            let last = out.last_mut().unwrap();
            last.text.push_str(&it.text);
            last.kind = ItemKind::Tok(TokKind::Symbol);

            if last.text == "<<" {
                open_shl += 1;
            } else if last.text == ">>" {
                open_shl -= 1;
            }

            continue;
        }

        out.push(it);
    }

    out
}

struct Formatter<'s> {
    items: Vec<Item>,
    options: &'s FmtConfig,
    lines: Vec<String>,
    line: String,
    /// The indentation level of the line under construction.
    line_level: usize,
    /// The block depth of each item, brackets not counted.
    depths: Vec<usize>,
    /// The `<` and `>` of type parameters and arguments, which stay tight.
    generic: Vec<bool>,
    /// The `function` items inside a trait that have no body.
    signature: Vec<bool>,
}

/// Openers of bracket groups, as token text.
fn opens(text: &str) -> bool {
    matches!(text, "(" | "{" | "[" | "?(" | "?[" | "<<")
}

fn closes(text: &str) -> bool {
    matches!(text, ")" | "}" | "]" | ">>")
}

fn closer_of(open: &str) -> &'static str {
    match open {
        "(" | "?(" => ")",
        "[" | "?[" => "]",
        "<<" => ">>",
        _ => "}",
    }
}

mod layout;
mod rewrite;
mod spacing;

pub mod alx;
pub mod structure;

impl<'s> Formatter<'s> {
    fn prev_code(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|j| !self.items[*j].is_comment())
    }

    fn next_code(&self, i: usize) -> Option<usize> {
        (i + 1..self.items.len()).find(|j| !self.items[*j].is_comment())
    }
}

/// The byte offset of the `:` of a struct field line, or none.
fn field_colon(line: &str) -> Option<usize> {
    let t = line.trim_start();

    if t.starts_with("--") || t.starts_with('@') {
        return None;
    }

    let at = t.find(':')?;
    let name = t[..at].trim();
    let name = name
        .strip_prefix("read ")
        .or_else(|| name.strip_prefix("write "))
        .unwrap_or(name);

    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    Some(line.len() - t.len() + at)
}

fn synthetic(text: &str) -> Item {
    Item {
        text: text.to_string(),
        kind: ItemKind::Tok(if text == "(" {
            TokKind::LParen
        } else {
            TokKind::RParen
        }),
        newlines_before: 0,
        space_before: false,
    }
}

/// A string literal with the quotes the option asks for. The content
/// keeps its characters; under an `auto` style a string that holds the
/// other quote keeps the quotes it has.
pub(crate) fn requote(text: &str, style: QuoteStyle) -> String {
    let Some(first) = text.chars().next() else {
        return text.to_string();
    };

    if first != '"' && first != '\'' {
        return text.to_string();
    }

    let body = &text[1..text.len() - 1];
    let has_double = body.contains('"');
    let has_single = body.contains('\'');
    let want = match style {
        QuoteStyle::Preserve => return text.to_string(),
        QuoteStyle::AutoPreferDouble => {
            if has_double && !has_single {
                '\''
            } else {
                '"'
            }
        }
        QuoteStyle::AutoPreferSingle => {
            if has_single && !has_double {
                '"'
            } else {
                '\''
            }
        }
        QuoteStyle::ForceDouble => '"',
        QuoteStyle::ForceSingle => '\'',
    };

    if want == first {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 2);
    out.push(want);
    let mut chars = body.chars();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(q) if q == first => out.push(q),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else if c == want {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }

    out.push(want);
    out
}

/// Openers of a block that `end` closes, when they start a statement.
fn block_opener(text: &str) -> bool {
    matches!(
        text,
        "function"
            | "if"
            | "do"
            | "repeat"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "interface"
            | "macro"
            | "namespace"
            | "match"
            | "class"
            | "with"
    )
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "and"
            | "or"
            | "not"
            | "if"
            | "then"
            | "else"
            | "elseif"
            | "end"
            | "for"
            | "in"
            | "while"
            | "do"
            | "repeat"
            | "until"
            | "return"
            | "break"
            | "continue"
            | "local"
            | "function"
            | "nil"
            | "true"
            | "false"
            | "const"
            | "export"
            | "global"
            | "import"
            | "from"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "interface"
            | "match"
            | "case"
            | "default"
            | "with"
            | "new"
            | "delete"
            | "destroy"
            | "after"
            | "async"
            | "await"
            | "try"
            | "macro"
            | "namespace"
            | "attribute"
            | "remote"
            | "where"
            | "is"
            | "as"
            | "satisfies"
            | "declare"
            | "read"
            | "write"
            | "extends"
            | "on"
    )
}

/// Tokens after which an `if` or a `function` is an expression.
fn expression_context(prev: &str) -> bool {
    matches!(
        prev,
        "=" | "("
            | ","
            | "["
            | "{"
            | "return"
            | "and"
            | "or"
            | "not"
            | "+"
            | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "=="
            | "~="
            | "<"
            | ">"
            | "<="
            | ">="
            | "??"
            | "?"
            | ":"
            | "in"
            | "?("
            | "?["
    )
}

/// Tokens that continue the expression of the line before, when they
/// start a line.
fn continues(text: &str) -> bool {
    matches!(
        text,
        "+" | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "and"
            | "or"
            | "=="
            | "~="
            | "<"
            | ">"
            | "<="
            | ">="
            | "??"
            | "?"
            | ":"
            | "."
            | "->"
            | "=>"
            | "?."
            | "?:"
            | "?["
            | "?("
    )
}

/// Tokens after which the next line continues the expression.
fn leaves_open(text: &str) -> bool {
    matches!(
        text,
        "=" | "+"
            | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "^"
            | ".."
            | "and"
            | "or"
            | "not"
            | "=="
            | "~="
            | "<="
            | ">="
            | "??"
            | "->"
            | "=>"
    )
}

fn is_closer(text: &str) -> bool {
    matches!(text, "end" | "until" | ")" | "]" | "}" | "else" | "elseif")
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        format(s).unwrap()
    }

    #[test]
    fn reindents_blocks() {
        let src = "local function f(x)\nif x then\nreturn 1\nelseif x == 2 then\nreturn 2\nelse\nreturn 3\nend\nend\n";
        let want = "local function f(x)\n    if x then\n        return 1\n    elseif x == 2 then\n        return 2\n    else\n        return 3\n    end\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn spacing_is_canonical() {
        assert_eq!(fmt("local x=1+2*3\n"), "local x = 1 + 2 * 3\n");
        assert_eq!(fmt("f(a,b , c)\n"), "f(a, b, c)\n");
        assert_eq!(fmt("local t={a=1,b=2}\n"), "local t = { a = 1, b = 2 }\n");
        assert_eq!(fmt("local v = -x\n"), "local v = -x\n");
        assert_eq!(fmt("obj:method(1):other()\n"), "obj:method(1):other()\n");
        assert_eq!(
            fmt("local p = workspace->Map?.Part\n"),
            "local p = workspace->Map?.Part\n"
        );
        assert_eq!(fmt("local n: number? = nil\n"), "local n: number? = nil\n");
        assert_eq!(fmt("local s = c ? a : b\n"), "local s = c ? a : b\n");
        assert_eq!(fmt("local xs = [1,2]\n"), "local xs = [ 1, 2 ]\n");
        assert_eq!(fmt("print(t[1], #t)\n"), "print(t[1], #t)\n");
    }

    #[test]
    fn a_table_that_fits_stays_on_one_line_and_one_that_does_not_breaks() {
        assert_eq!(
            fmt("local t = {\n    a = 1,\n    b = 2 }\n"),
            "local t = { a = 1, b = 2 }\n"
        );
        let long = "local t = { alpha = 111111111111, beta = 222222222222, gamma = 333333333333, delta = 444444444444, epsilon = 5555 }\n";
        let want = "local t = {\n    alpha = 111111111111,\n    beta = 222222222222,\n    gamma = 333333333333,\n    delta = 444444444444,\n    epsilon = 5555,\n}\n";
        assert_eq!(fmt(long), want);
    }

    /// A service import keeps the form the reader wrote, and the
    /// import list keeps its order: the formatter sorts nothing.
    #[test]
    fn a_service_import_keeps_its_form_and_its_order() {
        let src = "import { RunService, Players } from \"game\"\nimport TweenService from \"game:TweenService\"\nimport { ReplicatedStorage as RS } from \"game\"\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn a_magic_trailing_comma_keeps_a_group_expanded() {
        let src = "local t = {\n    a = 1,\n    b = 2,\n}\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn a_callback_argument_indents_once() {
        assert_eq!(
            fmt("foo(function()\nbar()\nend)\n"),
            "foo(function()\n    bar()\nend)\n"
        );
    }

    #[test]
    fn quotes_follow_the_option() {
        assert_eq!(fmt("local s = 'a'\n"), "local s = \"a\"\n");
        assert_eq!(fmt("local s = 'say \"hi\"'\n"), "local s = 'say \"hi\"'\n");
        let mut o = FmtConfig::default();
        o.quote_style = QuoteStyle::ForceSingle;
        assert_eq!(
            format_with("local s = \"it's\"\n", &o).unwrap(),
            "local s = 'it\\'s'\n"
        );
    }

    #[test]
    fn call_parentheses_follow_the_option() {
        assert_eq!(
            fmt("print \"x\"\nf { a = 1 }\n"),
            "print(\"x\")\nf({ a = 1 })\n"
        );
        let mut o = FmtConfig::default();
        o.call_parentheses = CallParentheses::None;
        assert_eq!(format_with("print(\"x\")\n", &o).unwrap(), "print \"x\"\n");
    }

    #[test]
    fn a_struct_fields_form_is_not_a_call() {
        assert_eq!(
            fmt("local p = new P { x = 1 }\n"),
            "local p = new P { x = 1 }\n"
        );
    }

    #[test]
    fn explicit_type_arguments_after_a_dot_stay_tight() {
        let src = "local damaged = Signal.new<<Player, number>>()\n";
        assert_eq!(fmt(src), src);
        // A second pass changes nothing: the `<<` did not split.
        assert_eq!(fmt(&fmt(src)), src);
        assert_eq!(fmt("local c = a < b\n"), "local c = a < b\n");
        assert_eq!(fmt("local d = t.x < y\n"), "local d = t.x < y\n");
    }

    #[test]
    fn a_map_literal_keeps_its_pairs_tight() {
        let src = "local prices = $map[[\"sword\", 10], [\"pet\", 25]]\n";
        assert_eq!(fmt(src), src);
        assert_eq!(
            fmt("local s = $set[\"a\", \"b\"]\n"),
            "local s = $set[\"a\", \"b\"]\n"
        );
        // A plain array of arrays keeps the array spacing.
        assert_eq!(
            fmt("local g = [[1, 2], [3, 4]]\n"),
            "local g = [ [ 1, 2 ], [ 3, 4 ] ]\n"
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_left_alone() {
        let src = "local function alpha(n: number): number\n    if n > 0 then\n        return n\n    return 0\nend\n";
        let e = format_file(src, &FmtConfig::default()).unwrap_err();
        assert!(e.starts_with(UNPARSED), "{e}");
        // A `.d.aly` writes `declare`, and still parses.
        assert!(parse_error("declare plugin: Plugin\n").is_none());
    }

    #[test]
    fn leading_zeros_follow_the_option() {
        assert_eq!(fmt("local x = .5\n"), "local x = 0.5\n");
    }

    #[test]
    fn import_lists_expand_on_a_trailing_comma_or_when_asked() {
        let src = "import { world, pair, } from \"@pkg/jecs\"\nimport { x, y } from \"./m\"\nexport { x, y }\nprint(world, pair, x, y)\n";
        assert_eq!(
            format(src).unwrap(),
            "import {\n    world,\n    pair,\n} from \"@pkg/jecs\"\nimport { x, y } from \"./m\"\nexport { x, y }\nprint(world, pair, x, y)\n"
        );

        let mut o = FmtConfig::default();
        o.expand_imports = true;
        assert_eq!(
            format_with("import a, { x, y } from \"./m\"\nimport { one } from \"./o\"\nexport { x, y }\nprint(a, x, y, one)\n", &o).unwrap(),
            "import a, {\n    x,\n    y,\n} from \"./m\"\nimport { one } from \"./o\"\nexport {\n    x,\n    y,\n}\nprint(a, x, y, one)\n"
        );
    }

    #[test]
    fn imports_sort_when_asked() {
        let mut o = FmtConfig::default();
        o.sort_requires.enabled = true;
        o.sort_requires.grouping = RequireGrouping::ByKind;
        let src = "import { b } from \"./b\"\nimport { a } from \"@pkg/a\"\nprint(a, b)\n";
        assert_eq!(
            format_with(src, &o).unwrap(),
            "import { a } from \"@pkg/a\"\nimport { b } from \"./b\"\nprint(a, b)\n"
        );
    }

    #[test]
    fn blank_lines_collapse_and_the_file_ends_with_one_newline() {
        assert_eq!(
            fmt("\n\nlocal a = 1\n\n\n\nlocal b = 2\n\n\n"),
            "local a = 1\n\nlocal b = 2\n"
        );
    }

    #[test]
    fn a_blank_line_at_the_edge_of_a_block_goes() {
        assert_eq!(
            fmt("if x then\n\n    y()\n\nend\n"),
            "if x then\n    y()\nend\n"
        );
    }

    #[test]
    fn match_arms_indent_once_and_bodies_twice() {
        let src = "match m with\ncase Ok(v) then\nprint(v)\ncase Err(e) then print(e)\ndefault\nprint(0)\nend\n";
        let want = "match m with\n    case Ok(v) then\n        print(v)\n    case Err(e) then print(e)\n    default\n        print(0)\nend\n";
        assert_eq!(fmt(src), want);
    }

    #[test]
    fn long_strings_and_comments_keep_their_text() {
        let src = "local s = [=[\n  keep\n\tthis  \n]=]\nprint(s) -- note\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn tabs_when_asked() {
        let mut o = FmtConfig::default();
        o.indent_type = IndentType::Tabs;
        assert_eq!(
            format_with("if x then\ny()\nend\n", &o).unwrap(),
            "if x then\n\ty()\nend\n"
        );
    }

    #[test]
    fn simple_statements_collapse_when_asked() {
        let mut o = FmtConfig::default();
        o.collapse_simple_statement = Collapse::Always;
        assert_eq!(
            format_with("if x then\n    return 1\nend\n", &o).unwrap(),
            "if x then return 1 end\n"
        );
    }

    #[test]
    fn call_chains_break_when_asked() {
        let mut o = FmtConfig::default();
        o.call_chains.style = CallChainStyle::Method;
        o.call_chains.min_calls = 3;
        assert_eq!(
            format_with("local v = xs:map(f):filter(g):reduce(h, 0)\n", &o).unwrap(),
            "local v = xs:map(f)\n    :filter(g)\n    :reduce(h, 0)\n"
        );
    }

    /// A CRLF file keeps its endings, and a long comment gains no
    /// second `\r`. A Windows checkout formats without rewriting every
    /// line of every file.
    #[test]
    fn crlf_round_trips() {
        let src = "struct T as\r\n    x: number -- a note\r\nend\r\n\r\n--[[ long\r\ncomment ]]\r\nlocal t = new T { x = 1 }\r\nprint(t.x)\r\n";
        assert_eq!(format(src).unwrap(), src);
        assert!(!format(src).unwrap().contains("\r\r"));

        // An option that names an ending still writes it.
        let mut unix = FmtConfig::default();
        unix.line_endings = LineEndings::Unix;
        let flat = format_with(src, &unix).unwrap();
        assert!(!flat.contains('\r'), "{flat:?}");

        let mut windows = FmtConfig::default();
        windows.line_endings = LineEndings::Windows;
        let back = format_with(&flat, &windows).unwrap();
        assert_eq!(back, src);
    }

    /// The header of an `impl` and of a `trait` closes with `as`, the
    /// way a `struct` header closes.
    #[test]
    fn an_impl_and_a_trait_header_gain_as() {
        assert_eq!(
            format("impl Circle\n    function f(self) end\nend\n").unwrap(),
            "impl Circle as\n    function f(self) end\nend\n"
        );
        assert_eq!(
            format("impl Shape for Circle\n    function f(self) end\nend\n").unwrap(),
            "impl Shape for Circle as\n    function f(self) end\nend\n"
        );
        assert_eq!(
            format("impl Box<T>\n    function f(self) end\nend\n").unwrap(),
            "impl Box<T> as\n    function f(self) end\nend\n"
        );
        assert_eq!(format("trait Empty end\n").unwrap(), "trait Empty as end\n");
        assert_eq!(format("impl Empty end\n").unwrap(), "impl Empty as end\n");
    }

    /// The rewrite `alloy flux --fix` and the server's quick fix apply:
    /// one insertion per header, at the end of the header.
    #[test]
    fn the_header_rewrite_inserts_one_as_per_header() {
        let src = "impl Circle\n    function f(self) end\nend\ntrait Shape\n    function a(self): number\nend\nimpl Shape for Circle as\nend\n";
        let fixes = header_as_fixes(src);
        assert_eq!(fixes.len(), 2, "{fixes:?}");
        let (text, n) = crate::lint::apply_fixes(
            src,
            &fixes
                .iter()
                .map(|f| crate::lint::Lint {
                    name: "header_as",
                    start: f.start,
                    end: f.end,
                    message: String::new(),
                    fix: Some(f.clone()),
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(n, 2);
        assert_eq!(
            text,
            "impl Circle as\n    function f(self) end\nend\ntrait Shape as\n    function a(self): number\nend\nimpl Shape for Circle as\nend\n"
        );
        assert!(parse_error(&text).is_none(), "{text}");
        assert!(header_as_fixes(&text).is_empty());
    }

    /// A header that already reads `as` gains no second one.
    #[test]
    fn the_as_of_a_header_is_written_once() {
        let src = "impl Shape for Circle as\n    function f(self) end\nend\n";
        assert_eq!(format(src).unwrap(), src);
        let trait_src = "trait Shape as\n    function area(self): number\nend\n";
        assert_eq!(format(trait_src).unwrap(), trait_src);
    }

    #[test]
    fn struct_fields_align_when_asked() {
        let mut o = FmtConfig::default();
        o.align_struct_fields = true;
        assert_eq!(
            format_with("struct P as\n    x: number\n    name: string\nend\n", &o).unwrap(),
            "struct P as\n    x:    number\n    name: string\nend\n"
        );
    }

    /// Drops the `as` that closes an `impl` or a `trait` header, so a
    /// file written before that form compares with its formatted text.
    fn drop_header_as(toks: Vec<String>) -> Vec<String> {
        let mut out = Vec::with_capacity(toks.len());
        let mut header = false;

        for t in toks {
            if header {
                if t == "as" {
                    header = false;

                    continue;
                }

                header = !matches!(t.as_str(), "function" | "end" | "@");
            } else {
                header = matches!(t.as_str(), "impl" | "trait");
            }

            out.push(t);
        }

        out
    }

    #[test]
    fn formatting_is_idempotent_on_the_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");

        if !dir.is_dir() {
            eprintln!("skipped: no examples checkout at {}", dir.display());

            return;
        }

        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aly") {
                let src = std::fs::read_to_string(&path).unwrap();
                let once = format(&src).unwrap();
                let twice = format(&once).unwrap();
                assert_eq!(once, twice, "{}", path.display());
                // The token stream holds, save for the rewrites.
                let norm = |text: &str| -> Vec<String> {
                    let toks: Vec<String> = lex(text)
                        .unwrap()
                        .toks
                        .iter()
                        .map(|t| t.text(text).replace('\'', "\""))
                        .filter(|t| !matches!(t.as_str(), "(" | ")" | ","))
                        .collect();

                    drop_header_as(toks)
                };
                assert_eq!(norm(&src), norm(&once), "{}", path.display());
            }
        }
    }
}
