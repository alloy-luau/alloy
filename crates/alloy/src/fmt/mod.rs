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
    /// The byte the item starts at in the source.
    start: usize,
    /// Newlines in the source between the item before and this one.
    newlines_before: usize,
    /// Whether the source had whitespace right before this item.
    space_before: bool,
    /// A contextual Alloy word that reads as a plain name here, ex: the
    /// `new` of `local new = Instance.new`. The layout and the spacing
    /// treat it as the identifier it is.
    name_here: bool,
    /// A `-` that opens a line of a value arm: the arm's value, `-v`,
    /// not a subtraction from the line above.
    value_start: bool,
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

    /// A keyword of the language at this place. A contextual word used as
    /// a name is not one, so `new(x)` spaces like any other call.
    fn is_keyword_here(&self) -> bool {
        is_keyword(&self.text) && !self.name_here
    }

    /// An opener of a block that `end` closes, at this place.
    fn opens_block_here(&self) -> bool {
        block_opener(&self.text) && !self.name_here
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

/// The report for output the parser cannot read back. The formatter
/// writes nothing in that case.
pub const OUTPUT_UNPARSED: &str = "fmt: the output would not parse; the file is unchanged";

/// The tail of the report for an `impl` or a `trait` header without
/// `as`, for a caller that reads a diagnostic and wants the rewrite.
pub const NEEDS_AS: &str = alloy_syntax::parser::NEEDS_AS;

/// The rewrites that write the `as` a declaration header is missing
/// before a body on its own line, in source order. `alloy flux
/// --fix` and the server's quick fix apply these. A body on the next
/// line needs no `as`, so it takes no rewrite.
pub fn header_as_fixes(src: &str) -> Vec<crate::lint::Fix> {
    let Ok(Lexed { toks, .. }) = lex(src) else {
        return Vec::new();
    };
    let text = |i: usize| toks[i].text(src);
    let mut out = Vec::new();
    let mut i = 0;

    while i < toks.len() {
        let opener = text(i);

        if !matches!(
            opener,
            "impl" | "trait" | "struct" | "enum" | "interface" | "namespace"
        ) || !opens_a_header(src, &toks, i)
        {
            i += 1;

            continue;
        }

        // An `impl` header runs over names, `.`, and `for`; the others end
        // at their one name, past `<...>` and an interface's `extends`.
        // A name follows the opener or a joining word. The first word of
        // the body is a name too: `public` in `impl Svc` then `public
        // function`, or `area` in `interface Shape` then `area: number`.
        let mut j = i + 1;
        let mut angle = 0usize;

        while j < toks.len() {
            let t = text(j);
            let name = toks[j].kind == TokKind::Ident && !is_keyword(t);

            if angle > 0 {
                angle += usize::from(t == "<");
                angle -= usize::from(t == ">");
                j += 1;
            } else if t == "<"
                || (opener == "impl" && (t == "." || t == "for"))
                || (opener == "interface" && (t == "extends" || t == ","))
                || (name && (j == i + 1 || matches!(text(j - 1), "." | "for" | "extends" | ",")))
            {
                angle += usize::from(t == "<");
                j += 1;
            } else {
                break;
            }
        }

        let same_line =
            j < toks.len() && !src[toks[j - 1].end as usize..toks[j].start as usize].contains('\n');

        if angle == 0 && j > i + 1 && same_line && !matches!(text(j), "as" | "end") {
            let at = toks[j - 1].end;
            out.push(crate::lint::Fix::new(src, at, at, " as"));
        }

        i = j.max(i + 1);
    }

    out
}

/// Whether the `impl` or `trait` token at `i` opens a declaration: it
/// starts a line, follows `export` or a visibility word, or opens the
/// file. The same rule the formatter's `starts_block` uses, over raw
/// tokens.
fn opens_a_header(src: &str, toks: &[Tok], i: usize) -> bool {
    if i == 0 {
        return true;
    }

    let prev = &toks[i - 1];

    matches!(prev.text(src), "export" | "global" | "public" | "private")
        || src[prev.end as usize..toks[i].start as usize].contains('\n')
}

/// How the formatter parses: it lays out every file, so it takes the
/// syntax each kind of file allows.
pub(crate) fn parse_options() -> alloy_syntax::parser::ParseOptions {
    alloy_syntax::parser::ParseOptions {
        definitions: true,
        reserved_keys: true,
        ..Default::default()
    }
}

/// The parser's first complaint about a whole file, if it has one. A
/// `.d.aly` file writes `declare` and a `.config.aly` writes `in` as a
/// key, so the parse takes both.
pub fn parse_error(src: &str) -> Option<String> {
    let options = parse_options();

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

/// Formats a file by its name: markup takes the `.alx` pass, and a
/// `.config.aly` keeps its config tables as written. A plain source
/// first takes the renames of the `naming_convention` lint. A `.d.aly`
/// declares names another program owns, so it keeps them.
pub fn format_named(name: &str, src: &str, options: &FmtConfig) -> Result<String, String> {
    if name.ends_with(".alx") {
        alx::format_alx_file(src, options)
    } else if name.rsplit(['/', '\\']).next() == Some(crate::config_aly::FILE_NAME) {
        crate::config_aly::format(src, options)
    } else if name.ends_with(".d.aly") {
        format_file(src, options)
    } else {
        // `prefer_const` goes first. A local it makes a `const` takes
        // the const style, so a second run renames nothing.
        let src = with_consts(src, options);
        let renamed = crate::naming::renamed(&src, options);

        format_file(renamed.as_deref().unwrap_or(&src), options)
    }
}

/// The source with the `prefer_const` rewrites: a `local` that nothing
/// assigns again reads as `const`, unless `[fmt] prefer_const = false`.
fn with_consts<'a>(src: &'a str, options: &FmtConfig) -> std::borrow::Cow<'a, str> {
    match options.prefer_const {
        true => crate::std_names::apply(src, &crate::flux::prefer_const_fixes(src)).into(),

        false => src.into(),
    }
}

/// Formats a whole file. The layout moves a statement into the block the
/// parser gives it, so a source with a missing `end` would come out as
/// another program: such a file keeps its text, and the error says so.
/// `format_with` skips this check, for the fragments an `.alx` hole
/// holds.
pub fn format_file(src: &str, options: &FmtConfig) -> Result<String, String> {
    if let Some(message) = parse_error(src) {
        return Err(format!("{UNPARSED}: {message}"));
    }

    let text = format_with(&with_consts(src, options), options)?;

    // A formatter never writes a file it cannot read back: the input
    // parsed, so output that does not is a bug here, and the caller
    // keeps the file it has.
    match parse_error(&text) {
        Some(_) => Err(OUTPUT_UNPARSED.to_string()),

        None => Ok(text),
    }
}

/// Formats Alloy source. `Err` carries the lexer's message: a file that
/// does not lex stays as it is.
pub fn format_with(src: &str, options: &FmtConfig) -> Result<String, String> {
    let text = format_tokens(src, options)?;

    // The lexer steps over a leading byte order mark, so the rebuilt
    // text drops it and `fmt --check` reports every tidy file a Windows
    // editor wrote. The mark belongs to the source, the way its line
    // endings do, so it goes back in front and a second pass writes the
    // same bytes.
    match src.starts_with(crate::directives::BOM) {
        true => Ok(format!("{}{text}", crate::directives::BOM)),

        false => Ok(text),
    }
}

fn format_tokens(src: &str, options: &FmtConfig) -> Result<String, String> {
    let Lexed { toks, comments } = lex(src).map_err(|e| e.message)?;
    let items = items_of(src, &toks, &comments);

    if items.is_empty() {
        return Ok(String::new());
    }

    // `parse_error` has read the file already, so the tree is whole.
    let (chunk, _) = alloy_syntax::parser::parse_lenient(src, &toks, parse_options());
    let annotation = colons::annotation_colons(src, &toks, &chunk);
    let expr_ifs = colons::expr_ifs(src, &toks, &chunk);

    let mut f = Formatter {
        items,
        options,
        lines: Vec::new(),
        line: String::new(),
        line_level: 0,
        depths: Vec::new(),
        generic: Vec::new(),
        annotation,
        expr_ifs,
        signature: Vec::new(),
        forced: Vec::new(),
        at_line: Vec::new(),
        hole: Vec::new(),
        held: Vec::new(),
    };
    f.rewrite_tokens();
    f.sort_requires();
    f.collapse_simple_statements();
    f.break_call_chains();
    // The rewrites are done, so the item count is final.
    f.forced = vec![false; f.items.len()];
    f.at_line = vec![0; f.items.len()];
    f.hole = f.holes();
    f.measure_lines();

    // An `if` expression and a `match` are no bracket group, so the
    // width alone cannot break them. The render says which ones came out
    // past the column, the next pass breaks those, and a pass that
    // breaks nothing is the last. Each pass breaks one level of a nest,
    // and the last pass renders whatever the passes before it forced.
    for pass in 1..=8 {
        let tree = f.tree();
        let hard = f.hard_breaks(&tree);
        f.held = f.held_items(&hard);
        f.render_nodes(&tree, &hard, 0);
        f.flush();

        if pass == 8 {
            break;
        }

        let broke_ifs = f.force_long_expr_ifs();
        let broke_matches = f.force_long_matches();

        if !broke_ifs && !broke_matches {
            break;
        }

        f.lines.clear();
        f.line.clear();
        f.line_level = 0;
        f.measure_lines();
    }

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
    // A contextual word is a name wherever the parser reads one, so the
    // layout and the spacing ask the same module the parser asks.
    let names: std::collections::HashSet<usize> = toks
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            alloy_syntax::contextual::is_contextual(t.text(src))
                && !alloy_syntax::contextual::keyword_at(src, toks, *i)
        })
        .map(|(_, t)| t.start as usize)
        .collect();
    let arms = structure::structure(src, toks).value_arm;
    let value_starts: std::collections::HashSet<usize> = toks
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            t.text(src) == "-"
                && arms[*i]
                && *i > 0
                && src[toks[i - 1].end as usize..t.start as usize].contains('\n')
        })
        .map(|(_, t)| t.start as usize)
        .collect();
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
            start: a,
            newlines_before: between.matches('\n').count(),
            space_before: !between.is_empty(),
            name_here: names.contains(&a),
            value_start: value_starts.contains(&a),
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
    /// The byte offsets of the `:` items that open a type; see `colons`.
    annotation: std::collections::HashSet<usize>,
    /// The byte offsets of the `if` items that open an expression; see
    /// `colons`.
    expr_ifs: std::collections::HashSet<usize>,
    /// The `function` items inside a trait that have no body.
    signature: Vec<bool>,
    /// Items the layout breaks before whatever the source wrote: the
    /// branches of an `if` expression that ran past the column.
    forced: Vec<bool>,
    /// The output line each item landed on in the last render.
    at_line: Vec<usize>,
    /// The items inside an interpolation hole, the `}` that closes it
    /// included. A hole keeps its line.
    hole: Vec<bool>,
    /// The items of an `if` expression that has not broken yet. A
    /// bracket group among them keeps its line, so a long `if` breaks at
    /// its keywords first.
    held: Vec<bool>,
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

mod colons;
mod layout;
mod rewrite;
mod spacing;

pub mod alx;
pub mod structure;

impl<'s> Formatter<'s> {
    /// The block depths and the type brackets, both read from the
    /// newlines the items carry. A forced break changes them, so they
    /// are read again before every render.
    fn measure_lines(&mut self) {
        self.depths = self.block_depths();
        self.generic = self.generic_brackets();
    }

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
        // No source byte: the item is no annotation colon.
        start: usize::MAX,
        newlines_before: 0,
        space_before: false,
        name_here: false,
        value_start: false,
    }
}

/// A string literal with the quotes the option asks for. The content
/// keeps its characters; under an `auto` style a string that holds the
/// other quote keeps the quotes it has.
pub fn requote(text: &str, style: QuoteStyle) -> String {
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
            | "attribute"
    )
}

pub(crate) fn is_keyword(text: &str) -> bool {
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

/// Tokens after which an `if` or a `function` is an expression. The
/// head of an interpolated string ends with the `{` of its hole, so a
/// token that ends with `{` opens an expression like a plain `{` does.
fn expression_context(prev: &str) -> bool {
    prev.ends_with('{')
        || matches!(
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

    /// These tests read the layout; `prefer_const` has tests of its own.
    fn format(src: &str) -> Result<String, String> {
        format_file(
            src,
            &FmtConfig {
                prefer_const: false,
                ..FmtConfig::default()
            },
        )
    }

    fn fmt(s: &str) -> String {
        format(s).unwrap()
    }

    /// A `local` that nothing assigns again becomes a `const`; one that
    /// takes a later write, or whose value a line writes into, stays.
    /// A project that keeps its files as written keeps its locals.
    #[test]
    fn a_local_nothing_assigns_again_becomes_const() {
        let src = "local a = 1\nlocal b = 2\nb = 3\nlocal t = { n = 1 }\nt.n = 2\nlocal c\nc = 1\nprint(a, b, t, c)\n";

        assert_eq!(
            format_file(src, &FmtConfig::default()).unwrap(),
            "const a = 1\nlocal b = 2\nb = 3\nlocal t = { n = 1 }\nt.n = 2\nlocal c\nc = 1\nprint(a, b, t, c)\n"
        );
        assert_eq!(format_file(src, &FmtConfig::preserving()).unwrap(), src);
    }

    /// `export default struct` opened no block, so its fields lost
    /// their indent. `default` after `export` now opens it, as `export`
    /// alone does.
    #[test]
    fn a_default_export_indents_its_body() {
        let src = "@derive(Debug)\nexport default struct Bag\nn: number\nend\n";

        assert_eq!(
            fmt(src),
            "@derive(Debug)\nexport default struct Bag\n  n: number\nend\n"
        );
    }

    #[test]
    fn a_negation_holds_its_operand() {
        assert_eq!(
            format("local b: ~ nil|~string = 1\ntype N = ~(number | string)\nif a ~= b then end\n")
                .unwrap(),
            "local b: ~nil | ~string = 1\ntype N = ~(number | string)\nif a ~= b then end\n"
        );
    }

    #[test]
    fn preserving_keeps_open_groups_and_calls_take_their_space() {
        assert_eq!(
            format("local function f(...) return ... end\n").unwrap(),
            "local function f(...) return ... end\n"
        );

        let src = "print(\n    1,\n    2\n)\n";
        assert_eq!(
            format_with(src, &FmtConfig::preserving().for_source(src)).unwrap(),
            src
        );

        let mut calls = FmtConfig::default();
        calls.space_after_function_names = crate::config::FunctionNameSpace::Calls;
        assert_eq!(
            format_with("local function f(x) return x end\nf(1)\n", &calls).unwrap(),
            "local function f(x) return x end\nf (1)\n"
        );
    }

    #[test]
    fn reindents_blocks() {
        let src = "local function f(x)\nif x then\nreturn 1\nelseif x == 2 then\nreturn 2\nelse\nreturn 3\nend\nend\n";
        let want = "local function f(x)\n  if x then\n    return 1\n  elseif x == 2 then\n    return 2\n  else\n    return 3\n  end\nend\n";
        assert_eq!(fmt(src), want);
    }

    /// An annotation colon is tight before and breathes after, wherever
    /// the source put its spaces. A method colon and a ternary's `:`
    /// keep what the source wrote.
    #[test]
    fn an_annotation_colon_takes_one_space_after() {
        let src = "local x:number = 1\n\nfunction f(a:number, b :number):number\n  return a + b\nend\n\nstruct S as\n  y:number?\n  z : string\nend\n\nlocal function g<T : Show>(a : T) : T\n  return a\nend\n\nlocal m: { [string] : number } = {}\nlocal h : typeof(m) = m\nlocal o: number? = x > 1 ? 1 : 2\nfor i : number = 1, 2 do\n  print(i)\nend\nlocal cb = m:get\nprint(m:get(1), m:get \"s\", o, h, cb)\n";
        let want = "local x: number = 1\n\nfunction f(a: number, b: number): number\n  return a + b\nend\n\nstruct S\n  y: number?\n  z: string\nend\n\nlocal function g<T: Show>(a: T): T\n  return a\nend\n\nlocal m: { [string]: number } = {}\nlocal h: typeof(m) = m\nlocal o: number? = x > 1 ? 1 : 2\nfor i: number = 1, 2 do\n  print(i)\nend\nlocal cb = m:get\nprint(m:get(1), m:get('s'), o, h, cb)\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// A `public struct` inside a namespace opens a body the way an
    /// exported one does. The formatter opened one only behind `export`,
    /// so the fields and every `end` fell to column 0, and a second
    /// pass called that stable.
    #[test]
    fn a_visibility_word_opens_a_declaration_body() {
        let decls = [
            "public struct T\n    x: number\n  end",
            "private struct T\n    x: number\n  end",
            "public enum E\n    A\n    B\n  end",
            "public function f()\n    return 1\n  end",
            "private const K = 1",
        ];

        for decl in decls {
            let want = format!("export namespace Ns\n  {decl}\nend\n");
            let flat: String = want
                .lines()
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";

            assert_eq!(fmt(&want), want);
            assert_eq!(fmt(&flat), want);
        }
    }

    /// A multi-line `if` expression indents every line that continues
    /// it: the branch bodies and the `else`.
    #[test]
    fn an_if_expression_indents_its_branches() {
        let src = "local x = if a > 0 then\n'big'\nelse\n'small'\n";
        let want = "local x = if a > 0 then\n  'big'\n  else\n  'small'\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// An `if` expression past `column_width` breaks the way StyLua
    /// breaks one: the first `then` and the `else` open a line, each with
    /// its value. The rule reaches the three places one sits in, and a
    /// second run changes nothing.
    #[test]
    fn a_long_if_expression_breaks_its_branches() {
        let a = "1111111111111111111111111111111111111111111111111";
        let b = "2222222222222222222222222222222222222222222222222222";
        let branches = format!("if flag\n    then {a}\n    else {b}");

        let cases = [
            (
                format!("local function g(): number\n  return if flag then {a} else {b}\nend\n"),
                format!("local function g(): number\n  return {branches}\nend\n"),
            ),
            (
                format!("local t = {{ value = if flag then {a} else {b} }}\n"),
                format!("local t = {{\n  value = {branches},\n}}\n"),
            ),
            (
                format!("f(if flag then {a} else {b})\n"),
                format!("f(\n  {branches}\n)\n"),
            ),
        ];

        for (src, want) in cases {
            assert_eq!(fmt(&src), want);
            assert_eq!(fmt(&want), want);
        }

        // One that fits keeps its line.
        let short = "local x = if flag then 1 else 2\n";
        assert_eq!(fmt(short), short);
    }

    /// The `)` of a call in a branch ended the `if` expression, so the
    /// `else` of a broken `local` or `const` fell to column 0. A closer
    /// now ends only an `if` that opened inside its group. A hand-broken
    /// `if` keeps its shape.
    #[test]
    fn a_call_in_a_branch_keeps_the_else_in_the_if() {
        let config = FmtConfig {
            prefer_const: false,
            indent_width: 4,
            ..FmtConfig::default()
        };
        let value = "if props.unlocked then Difficulty.color(props.stage.difficulty) else Color3.fromRGB(90, 90, 90)";

        for word in ["local", "const"] {
            let src = format!(
                "local function view(props: any)\n    {word} color = {value}\n    print(color)\nend\n"
            );
            let want = format!(
                "local function view(props: any)\n    {word} color = if props.unlocked\n        then Difficulty.color(props.stage.difficulty)\n        else Color3.fromRGB(90, 90, 90)\n    print(color)\nend\n"
            );
            let by_hand = format!(
                "local function view(props: any)\n    {word} color = if props.unlocked then\n        Difficulty.color(props.stage.difficulty)\n        else\n        Color3.fromRGB(90, 90, 90)\n    print(color)\nend\n"
            );

            assert_eq!(format_file(&src, &config).unwrap(), want);
            assert_eq!(format_file(&want, &config).unwrap(), want);
            assert_eq!(format_file(&by_hand, &config).unwrap(), by_hand);
        }

        // A closer still ends an `if` that opened inside its group.
        let inner = "print(f(if a then g(1) else h(2)), { k = if a then g(1) else 2 })\n";
        assert_eq!(format_file(inner, &config).unwrap(), inner);
    }

    /// An `if` inside an interpolation hole is an expression, so it
    /// opens no block and the `end` of the function stays at column 0.
    /// A long one keeps its line, since a hole never breaks, and one an
    /// older run broke joins again.
    #[test]
    fn an_if_expression_in_an_interpolation_hole_opens_no_block() {
        let short =
            "function f(flag: boolean): string\n  return `x {if flag then 'a' else 'b'} y`\nend\n";
        assert_eq!(fmt(short), short);
        // The `end` of a file the old rule indented comes back.
        assert_eq!(fmt(&short.replace("\nend\n", "\n  end\n")), short);

        let t = "'longvalueherefortrueandthenmore'";
        let f = "'longvaluehereforfalsealternativevalue'";
        let long = format!(
            "function g(flag: boolean): string\n  return `prefix {{if flag then {t} else {f}}} suffix`\nend\n"
        );
        let broken = format!(
            "function g(flag: boolean): string\n  return `prefix {{if flag then\n    {t}\n    else\n    {f}}} suffix`\nend\n"
        );
        assert!(long.lines().any(|l| l.chars().count() > 100));
        assert_eq!(fmt(&long), long);
        assert_eq!(fmt(&broken), long);
    }

    /// A call in an interpolation hole kept its line only while it fit,
    /// so a long one broke inside the hole. A hole never breaks now, and
    /// one an older run broke joins again.
    #[test]
    fn a_call_in_an_interpolation_hole_keeps_its_line() {
        let name = "Enemy.displayNameForTheEnemyThatTheWaveSpawnedJustNow";
        let long = format!(
            "local s = `hit {{{name}(enemy, wave)}} for {{damage}} damage, {{hits}} left`\n"
        );
        assert!(long.chars().count() > 100);
        assert_eq!(fmt(&long), long);

        let broken = format!(
            "local s = `hit {{{name}(\n  enemy,\n  wave\n)}} for {{damage}} damage, {{hits}} left`\n"
        );
        assert_eq!(fmt(&broken), long);
    }

    /// The tail of a macro is an expression, so an `if` there opens no
    /// block. The `end` of the macro and every line after it kept one
    /// level of indent too many.
    #[test]
    fn an_if_expression_as_a_macro_tail_opens_no_block() {
        let src = "macro pick(ok, a, b)\n    if ok then a else b\nend\n\nmacro twice(x)\n    x * 2\nend\n\nprint($pick(true, 1, 2), $twice(3))\n";
        let want = "macro pick(ok, a, b)\n  if ok then a else b\nend\n\nmacro twice(x)\n  x * 2\nend\n\nprint($pick(true, 1, 2), $twice(3))\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// An `end` after an `if` expression on one line closes the block
    /// of that line. It closed nothing, so every line after it moved in.
    #[test]
    fn an_end_after_an_if_expression_closes_its_block() {
        let src = "local function f(c: boolean, a: boolean)\n  local y = 0\n  if c then y = if a then 1 else 2 end\n  print(y)\nend\n\nf(true, false)\n";
        assert_eq!(fmt(src), src);
    }

    /// A long `if` expression breaks at its keywords before a bracket
    /// group inside it breaks, the way StyLua 2.5.2 lays it out. An `if`
    /// in an `else` breaks only when its own line is too long.
    #[test]
    fn a_long_if_expression_breaks_at_its_keywords_first() {
        let src = "local function label(name: string, wave: number): string\n    return if wave > 20 then `elite {string.upper(name)} of wave {wave}` else if wave > 10 then `veteran {name}` else name\nend\n\nlocal function color(flying: boolean, boss: boolean, wave: number): Color3\n    local c = if flying then Color3.fromRGB(80, 80, 255) elseif boss and wave > 10 then Color3.fromRGB(255, 0, 0) else Color3.fromRGB(200, 200, 200)\n    return c\nend\n";
        let want = "local function label(name: string, wave: number): string\n  return if wave > 20\n    then `elite {string.upper(name)} of wave {wave}`\n    else if wave > 10 then `veteran {name}` else name\nend\n\nlocal function color(flying: boolean, boss: boolean, wave: number): Color3\n  local c = if flying\n    then Color3.fromRGB(80, 80, 255)\n    elseif boss and wave > 10 then Color3.fromRGB(255, 0, 0)\n    else Color3.fromRGB(200, 200, 200)\n  return c\nend\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);

        // The group an older run broke inside a branch joins again.
        let old = "local c = if flying then Color3.fromRGB(80, 80, 255) elseif boss and wave > 10 then Color3.fromRGB(\n  255,\n  0,\n  0\n) else Color3.fromRGB(200, 200, 200)\n";
        let fixed = "local c = if flying\n  then Color3.fromRGB(80, 80, 255)\n  elseif boss and wave > 10 then Color3.fromRGB(255, 0, 0)\n  else Color3.fromRGB(200, 200, 200)\n";
        assert_eq!(fmt(old), fixed);
    }

    /// A nest of long `if` expressions breaks one level per pass, and
    /// each level indents under the one around it. The render loop
    /// stopped after three passes with its lines cleared, so a deeper
    /// nest could come out empty.
    #[test]
    fn nested_if_expressions_break_one_level_at_a_time() {
        let [a, b, c, d, e] = ["a", "b", "c", "d", "e"].map(|x| x.repeat(60));
        let src = format!(
            "local v = if {a} then 1 else if {b} then 2 else if {c} then 3 else if {d} then 4 else if {e} then 5 else 6\n"
        );
        let want = format!(
            "local v = if {a}\n  then 1\n  else if {b}\n    then 2\n    else if {c}\n      then 3\n      else if {d}\n        then 4\n        else if {e} then 5 else 6\n"
        );
        assert_eq!(fmt(&src), want);
        assert_eq!(fmt(&want), want);

        // An `if` in a `then` gives the outer `else` back its level.
        let (x, y) = ("1".repeat(56), "2".repeat(46));
        let src = format!("local n = if flag then if other then {x} else {y} else 3\n");
        let want =
            format!("local n = if flag\n  then if other\n    then {x}\n    else {y}\n  else 3\n");
        assert_eq!(fmt(&src), want);
        assert_eq!(fmt(&want), want);
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
            fmt("local t = {\n  a = 1,\n  b = 2 }\n"),
            "local t = { a = 1, b = 2 }\n"
        );
        let long = "local t = { alpha = 111111111111, beta = 222222222222, gamma = 333333333333, delta = 444444444444, epsilon = 5555 }\n";
        let want = "local t = {\n  alpha = 111111111111,\n  beta = 222222222222,\n  gamma = 333333333333,\n  delta = 444444444444,\n  epsilon = 5555,\n}\n";
        assert_eq!(fmt(long), want);
    }

    /// The width check counted the source's spaces and one more after
    /// each comma. A 99-column call broke under a 100-column width, and
    /// the second run broke it in a different shape. The check now
    /// counts the spaces the render writes.
    #[test]
    fn a_group_measures_the_spaces_it_renders() {
        let head = "local function f(p: { plots: { PlotSave } })\n";
        let fits = format!(
            "{head}  table.insert(p.plots, new PlotSave {{ slot = 1, state = CropState.Growing(CropKind.Carrot, 0.5) }})\nend\n"
        );
        assert_eq!(fits.lines().nth(1).unwrap().chars().count(), 99);
        assert_eq!(fmt(&fits), fits);
        // Tight source spacing gives the same result.
        assert_eq!(fmt(&fits.replace(", ", ",")), fits);

        // Two columns more break the call once, and the output holds.
        let long = fits.replace("0.5", "0.525");
        let want = format!(
            "{head}  table.insert(\n    p.plots,\n    new PlotSave {{ slot = 1, state = CropState.Growing(CropKind.Carrot, 0.525) }}\n  )\nend\n"
        );
        assert_eq!(fmt(&long), want);
        assert_eq!(fmt(&want), want);

        // A magic trailing comma keeps the table open, and the call
        // around it stays on its line.
        let hug = format!(
            "{head}  table.insert(p.plots, new PlotSave {{\n    slot = 1,\n    state = CropState.Growing(CropKind.Carrot, 0.5),\n  }})\nend\n"
        );
        assert_eq!(fmt(&hug), hug);
    }

    /// The width check stopped at the closer of the group, so `: Part`
    /// after a parameter list ran past the column. It now counts the
    /// line up to the next place the line can break.
    #[test]
    fn a_group_counts_the_text_after_its_closer() {
        let src = "local function part(name: string, size: Vector3, position: Vector3, color: Rgb, parent: Instance): Part\nend\n";
        let want = "local function part(\n  name: string,\n  size: Vector3,\n  position: Vector3,\n  color: Rgb,\n  parent: Instance\n): Part\nend\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// An index that breaks takes no trailing comma: `t[k,]` does not
    /// parse. The child index `w->[k]` and the index after an assert,
    /// `t![k]`, take none either.
    #[test]
    fn a_broken_index_takes_no_trailing_comma() {
        let key = "a_very_long_key_name_that_runs_on_and_on_and_on_past_the_column_width_of_the_whole_file";

        for (head, tail) in [
            ("local event = (instance :: any)[", "] :: unknown"),
            ("local j = w->Map->[", "]"),
            ("local k = map![", "]"),
        ] {
            let src = format!("{head}{key}{tail}\n");
            let want = format!("{head}\n  {key}\n{tail}\n");
            assert_eq!(fmt(&src), want);
            assert_eq!(fmt(&want), want);
        }
    }

    /// A comma inside type arguments split the parameter list around
    /// them, as `Result<any,` and `string>` on two lines. The second run
    /// then read `<` as a comparison. The comma now stays inside.
    #[test]
    fn a_broken_group_keeps_its_type_arguments_whole() {
        let src = "local function report(player: Player, result: Result<any, string>, success: string, extra: number, more: number)\nend\n";
        let want = "local function report(\n  player: Player,\n  result: Result<any, string>,\n  success: string,\n  extra: number,\n  more: number\n)\nend\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);

        // Explicit type arguments that break keep their tight brackets.
        let src = "local damaged = Signal.new<<Player, number, string, boolean, Instance, Vector3, CFrame, Color3, Vector2>>()\n";
        let want = "local damaged = Signal.new<<\n  Player,\n  number,\n  string,\n  boolean,\n  Instance,\n  Vector3,\n  CFrame,\n  Color3,\n  Vector2\n>>()\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// A header whose group broke put a newline before its `do`, or
    /// before the next line of a trait signature. The layout read a
    /// second block there, and the second run indented every line after
    /// it one level more.
    #[test]
    fn a_broken_header_opens_one_block() {
        let src = "for name, r in Attributes.fields(struct_type_with_a_long_name, range_with_a_long_name, more_args, extra) do\n  print(name, r)\nend\nprint(1)\n";
        let want = "for name, r in Attributes.fields(\n  struct_type_with_a_long_name,\n  range_with_a_long_name,\n  more_args,\n  extra\n) do\n  print(name, r)\nend\nprint(1)\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);

        let src = "trait Codec\n  function decode(self, raw_input_string: string, options: DecodeOptions, fallback: SaveData): SaveData\nend\nprint(1)\n";
        let want = "trait Codec\n  function decode(\n    self,\n    raw_input_string: string,\n    options: DecodeOptions,\n    fallback: SaveData\n  ): SaveData\nend\nprint(1)\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// A service import keeps the form the reader wrote, and the
    /// import list keeps its order: the formatter sorts nothing. Both
    /// spellings of the path stay as written; the `game_alias` lint is
    /// what asks for the alias form.
    #[test]
    fn a_service_import_keeps_its_form_and_its_order() {
        let src = "import { RunService, Players } from '@game'\nimport TweenService from '@game/TweenService'\nimport { ReplicatedStorage as RS } from '@game'\n";
        assert_eq!(fmt(src), src);

        let old = "import { RunService, Players } from 'game'\nimport TweenService from 'game:TweenService'\n";
        assert_eq!(fmt(old), old);
    }

    #[test]
    fn a_trailing_comma_goes_before_the_last_comment() {
        let src = "local t = {\n  a = 1,\n  b = 2 -- two\n}\nlocal u = {\n  a = 1,\n  -- note\n}\nlocal v = {\n  -- only\n}\n";
        let once = format(src).unwrap();
        assert_eq!(
            once,
            "local t = {\n  a = 1,\n  b = 2, -- two\n}\nlocal u = {\n  a = 1,\n  -- note\n}\nlocal v = {\n  -- only\n}\n"
        );
        assert_eq!(format(&once).unwrap(), once);
    }

    #[test]
    fn a_magic_trailing_comma_keeps_a_group_expanded() {
        let src = "local t = {\n  a = 1,\n  b = 2,\n}\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn a_callback_argument_indents_once() {
        assert_eq!(
            fmt("foo(function()\nbar()\nend)\n"),
            "foo(function()\n  bar()\nend)\n"
        );
    }

    #[test]
    fn quotes_follow_the_option() {
        // The default forces single quotes, and escapes a single
        // quote the string holds.
        assert_eq!(fmt("local s = \"a\"\n"), "local s = 'a'\n");
        assert_eq!(fmt("local s = \"it's\"\n"), "local s = 'it\\'s'\n");
        let mut o = FmtConfig::default();
        o.quote_style = QuoteStyle::AutoPreferDouble;
        assert_eq!(
            format_with("local s = 'a'\n", &o).unwrap(),
            "local s = \"a\"\n"
        );
        // An `auto` style keeps the other quote instead of escaping.
        assert_eq!(
            format_with("local s = 'say \"hi\"'\n", &o).unwrap(),
            "local s = 'say \"hi\"'\n"
        );
    }

    #[test]
    fn call_parentheses_follow_the_option() {
        assert_eq!(
            fmt("print \"x\"\nf { a = 1 }\n"),
            "print('x')\nf({ a = 1 })\n"
        );
        let mut o = FmtConfig::default();
        o.call_parentheses = CallParentheses::None;
        assert_eq!(format_with("print(\"x\")\n", &o).unwrap(), "print 'x'\n");
        // A macro call keeps them: `$say 'hi'` does not parse.
        assert_eq!(
            format_with("$dbg(\"x\")\n$M.say({ 1 })\n", &o).unwrap(),
            "$dbg('x')\n$M.say({ 1 })\n"
        );
    }

    /// A macro body that opens with a group keeps its space: glued to
    /// the parameters it reads as a call.
    #[test]
    fn a_macro_body_keeps_its_space_after_the_parameters() {
        let want = "macro dbl(x) (x) * 2 end\n";
        assert_eq!(fmt("macro dbl(x) (x)*2 end\n"), want);
        assert_eq!(fmt(want), want);
    }

    #[test]
    fn a_struct_fields_form_is_not_a_call() {
        assert_eq!(
            fmt("local p = new P { x = 1 }\n"),
            "local p = new P { x = 1 }\n"
        );
    }

    /// A struct pattern in a `local` is no call either: `Pt({ x })`
    /// would be a variant pattern. A call in the value still takes its
    /// parentheses.
    #[test]
    fn a_struct_pattern_in_a_local_is_not_a_call() {
        let src = "if local Seg { a = Pt { x } } = s then\n  print(x)\nend\nlocal Pt { x = y } = f { 1 }\nconst N.P { x = z } = p\n";
        let want = "if local Seg { a = Pt { x } } = s then\n  print(x)\nend\nlocal Pt { x = y } = f({ 1 })\nconst N.P { x = z } = p\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// `enum Opt<T>` keeps its parameter list, and the body indents
    /// the way a struct's does. A second pass changes nothing.
    #[test]
    fn a_generic_enum_formats_like_a_struct() {
        let src = "enum Either<L, R = string> as\n  Left(L)\n  Right(R)\nend\n";
        let want = "enum Either<L, R = string>\n  Left(L)\n  Right(R)\nend\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
        assert_eq!(
            fmt("enum Opt<T> as Some(T), Nil end\n"),
            "enum Opt<T> as Some(T), Nil end\n"
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
        let src = "local prices = $map[['sword', 10], ['pet', 25]]\n";
        assert_eq!(fmt(src), src);
        assert_eq!(
            fmt("local s = $set['a', 'b']\n"),
            "local s = $set['a', 'b']\n"
        );
        // A plain array of arrays keeps the array spacing, so it never
        // prints the `[[` of a long string.
        assert_eq!(
            fmt("local g = [ [1, 2], [3, 4]]\n"),
            "local g = [ [ 1, 2 ], [ 3, 4 ] ]\n"
        );
        let long = "local s = [[1, 2], [3, 4]]\n";
        assert_eq!(fmt(long), long);
    }

    #[test]
    fn a_file_that_does_not_parse_is_left_alone() {
        let src = "local function alpha(n: number): number\n  if n > 0 then\n    return n\n  return 0\nend\n";
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
        let src = "import { world, pair, } from '@pkg/jecs'\nimport { x, y } from './m'\nexport { x, y }\nprint(world, pair, x, y)\n";
        assert_eq!(
            format(src).unwrap(),
            "import {\n  world,\n  pair,\n} from '@pkg/jecs'\nimport { x, y } from './m'\nexport { x, y }\nprint(world, pair, x, y)\n"
        );

        let mut o = FmtConfig::default();
        o.expand_imports = true;
        assert_eq!(
            format_with("import a, { x, y } from './m'\nimport { one } from './o'\nexport { x, y }\nprint(a, x, y, one)\n", &o).unwrap(),
            "import a, {\n  x,\n  y,\n} from './m'\nimport { one } from './o'\nexport {\n  x,\n  y,\n}\nprint(a, x, y, one)\n"
        );
    }

    #[test]
    fn imports_sort_when_asked() {
        let mut o = FmtConfig::default();
        o.sort_requires.enabled = true;
        o.sort_requires.grouping = RequireGrouping::ByKind;
        let src = "import { b } from './b'\nimport { a } from '@pkg/a'\nprint(a, b)\n";
        assert_eq!(
            format_with(src, &o).unwrap(),
            "import { a } from '@pkg/a'\nimport { b } from './b'\nprint(a, b)\n"
        );

        // A name list over several lines moves whole.
        let src = "import { b } from './b'\nimport {\n  a, -- the a\n  c,\n} from './a'\nprint(a, b, c)\n";
        let once = format_with(src, &o).unwrap();
        assert_eq!(
            once,
            "import {\n  a, -- the a\n  c,\n} from './a'\nimport { b } from './b'\nprint(a, b, c)\n"
        );
        assert_eq!(format_with(&once, &o).unwrap(), once);
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
            fmt("if x then\n\n  y()\n\nend\n"),
            "if x then\n  y()\nend\n"
        );
    }

    /// A `class` block parses and stops the compile. The layout read
    /// only `declare class` as a block, so the body lost its indent.
    #[test]
    fn a_class_body_indents_as_a_block() {
        let src = "class Critter\npublic hp: number\nfunction heal(self) end\nend\n";
        let want = "class Critter\n  public hp: number\n  function heal(self) end\nend\n";
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
        // The word is still a name where the parser reads one.
        assert_eq!(
            fmt("local class = 1\nprint(class)\n"),
            "local class = 1\nprint(class)\n"
        );
    }

    /// A method of a declared class has no body. After a field, the
    /// layout read one and indented the `end` and every line below.
    #[test]
    fn a_declared_class_method_after_a_field_opens_nothing() {
        let want = "declare class A\n  n: number\n  function f(self): number\n  m: string\nend\n\ndeclare x: number\nfunction g()\n  return 1\nend\n";
        assert_eq!(fmt(want), want);
        assert_eq!(
            fmt("declare extern type P with\n  n: number\n  function f(self): number\nend\n"),
            "declare extern type P with\n  n: number\n  function f(self): number\nend\n"
        );
    }

    #[test]
    fn match_arms_indent_once_and_bodies_twice() {
        let src = "match m with\ncase Ok(v) then\nprint(v)\ncase Err(e) then print(e)\ndefault\nprint(0)\nend\n";
        let want = "match m with\n  case Ok(v) then\n    print(v)\n  case Err(e) then print(e)\n  default\n    print(0)\nend\n";
        assert_eq!(fmt(src), want);
        // The hand-broken form keeps its lines.
        assert_eq!(fmt(want), want);
    }

    /// A `match` is no bracket group, so the width alone cannot break it.
    /// One written on a line that runs past `column_width` takes the
    /// shape a hand-broken one has: each arm one level in, its body one
    /// level deeper, and `end` back at the `match`. A second run changes
    /// nothing.
    #[test]
    fn a_long_one_line_match_breaks_its_arms() {
        let a = "'1111111111111111111111111111111111111111111'";
        let b = "'2222222222222222222222222222222222222222222'";
        let src = format!(
            "match n with case 0 then return {a} case 1 then return {b} default return 0 end\n"
        );
        let want = format!(
            "match n with\n  case 0 then\n    return {a}\n  case 1 then\n    return {b}\n  default\n    return 0\nend\n"
        );
        assert!(src.lines().any(|l| l.chars().count() > 100));
        assert_eq!(fmt(&src), want);
        assert_eq!(fmt(&want), want);

        // Two arms on a line break even under the width.
        let two = "match n with case 1 then f() case 2 then g() end\n";
        let broken = "match n with\n  case 1 then\n    f()\n  case 2 then\n    g()\nend\n";
        assert_eq!(fmt(two), broken);

        // One arm that fits keeps its line.
        let one = "local x = match n with case 1 then f() end\n";
        assert_eq!(fmt(one), one);
    }

    #[test]
    fn long_strings_and_comments_keep_their_text() {
        let src = "local s = [=[\n  keep\n\tthis  \n]=]\nprint(s) -- note\n";
        assert_eq!(fmt(src), src);
    }

    /// A trailing comment inside a bracket group stays behind the
    /// element it follows, and the next element opens a new line. The
    /// tokens after the comment once landed inside it, so the file lost
    /// them; the output guard now rejects such a run as well.
    #[test]
    fn a_comment_inside_an_argument_list_keeps_its_line() {
        let src = "print(\n  1, -- first\n  2, -- second\n  -- own line\n  3 -- last\n)\n";
        assert_eq!(fmt(src), src);
        // The same list written flat breaks the same way.
        let flat = "print(1, -- first\n2, -- second\n-- own line\n3 -- last\n)\n";
        assert_eq!(fmt(flat), src);
    }

    /// The formatter reads its own output back before a caller writes
    /// it, and the report names the file as unchanged.
    #[test]
    fn output_that_does_not_parse_is_refused() {
        assert!(OUTPUT_UNPARSED.contains("the file is unchanged"));
        assert!(parse_error("print(\n  1, -- first\n  2\n)\n").is_none());
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
            format_with("if x then\n  return 1\nend\n", &o).unwrap(),
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
            "local v = xs:map(f)\n  :filter(g)\n  :reduce(h, 0)\n"
        );
    }

    /// A CRLF file keeps its endings, and a long comment gains no
    /// second `\r`. A Windows checkout formats without rewriting every
    /// line of every file.
    #[test]
    fn crlf_round_trips() {
        let src = "struct T\r\n  x: number -- a note\r\nend\r\n\r\n--[[ long\r\ncomment ]]\r\nlocal t = new T { x = 1 }\r\nprint(t.x)\r\n";
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

    /// A Windows editor writes a byte order mark in front of the file.
    /// The lexer steps over it, so the rebuilt text dropped it and
    /// `fmt --check` reported every tidy file as one that would change.
    #[test]
    fn a_byte_order_mark_round_trips() {
        let src = "\u{feff}local x = 1\nlocal y = 2\n";
        assert_eq!(format(src).unwrap(), src);

        // A pass over the formatted text writes the same bytes.
        let once = format("\u{feff}local   x=1\n").unwrap();
        assert_eq!(once, "\u{feff}local x = 1\n");
        assert_eq!(format(&once).unwrap(), once);

        // A file without the mark never gains one.
        assert!(!format("local x = 1\n").unwrap().starts_with('\u{feff}'));
    }

    /// The header of an `impl` and of a `trait` closes with `as`, the
    /// way a `struct` header closes.
    /// Luau's attribute list keeps Luau's form: no padding inside the
    /// brackets and no parentheses around a table argument.
    #[test]
    fn a_luau_attribute_list_keeps_its_form() {
        for src in [
            "@[native]\nlocal function a() end\n",
            "@[native, deprecated]\nlocal function c() end\n",
            "@[deprecated { use = 'a', reason = 'old' }]\nlocal function b() end\n",
        ] {
            assert_eq!(format(src).unwrap(), src);
        }
    }

    #[test]
    fn a_header_drops_as_over_a_body_below() {
        for src in [
            "impl Circle\n  function f(self) end\nend\n",
            "impl Shape for Circle\n  function f(self) end\nend\n",
            "impl Box<T>\n  function f(self) end\nend\n",
            "struct P\n  x: number\nend\n",
            "interface Sized extends Named\n  size: number\nend\n",
        ] {
            assert_eq!(format(src).unwrap(), src);
            let with_as = src.replacen("\n", " as\n", 1);
            assert_eq!(format(&with_as).unwrap(), src, "{with_as:?}");
        }
        for one_line in ["trait Empty end\n", "enum Color as Red, Green end\n"] {
            assert_eq!(format(one_line).unwrap(), one_line);
        }
        // A comment after the `as` stays on the header line.
        assert_eq!(
            format("namespace Geo as -- shapes\n  const X = 1\nend\n").unwrap(),
            "namespace Geo -- shapes\n  const X = 1\nend\n"
        );
    }

    /// The rewrite `alloy flux --fix` and the server's quick fix apply:
    /// one insertion per header, at the end of the header.
    #[test]
    fn the_header_rewrite_inserts_one_as_per_header() {
        let src = "impl Circle function f(self) end\nend\ntrait Shape function a(self): number\nend\nimpl Shape for Circle\nend\n";
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
            "impl Circle as function f(self) end\nend\ntrait Shape as function a(self): number\nend\nimpl Shape for Circle\nend\n"
        );
        assert!(parse_error(&text).is_none(), "{text}");
        assert!(header_as_fixes(&text).is_empty());
    }

    /// A header on its own line takes no rewrite. The first word of the
    /// body read as one more name of the header, and the fix wrote
    /// `public as function` and `area as: number`.
    #[test]
    fn the_header_rewrite_stops_at_the_header() {
        for src in [
            "impl Svc\n  public function boot(self)\n  end\nend\n",
            "impl Shape for Svc\n  private function area(self): number\n    return 1\n  end\nend\n",
            "interface Shape extends Base, Named\n  area: number\nend\n",
            "impl Box<T>\n  public function get(self): T\n  end\nend\n",
        ] {
            assert!(header_as_fixes(src).is_empty(), "{src}");
        }
        let fixes = header_as_fixes("interface Shape extends Base area: number end\n");
        assert_eq!(fixes.len(), 1, "{fixes:?}");
    }

    /// The `as name` of a match head stays on the head line, with one
    /// space on each side of the word.
    #[test]
    fn a_match_alias_stays_on_the_head_line() {
        let src = "match s as state with\n  case Loading then print(state)\n  case Ready(n) then print(n, state)\nend\n";
        assert_eq!(format(src).unwrap(), src);
        assert_eq!(format("match s   as    state with\ncase Loading then print(state)\ncase Ready(n) then print(n, state)\nend\n").unwrap(), src);
        let two = "match a as left, b as right with\n  case Loading, Loading then print(left, right)\n  default print(left, right)\nend\n";
        assert_eq!(format(two).unwrap(), two);
        let expr =
            "local v = match s as st with\n  case Loading then 0\n  default #tostring(st)\nend\n";
        assert_eq!(format(expr).unwrap(), expr);
    }

    /// A header that already reads `as` gains no second one.
    #[test]
    fn the_as_of_a_one_line_header_stays() {
        let src = "impl Shape for Circle as function f(self) end end\n";
        assert_eq!(format(src).unwrap().matches(" as ").count(), 1);
    }

    #[test]
    fn struct_fields_align_when_asked() {
        let mut o = FmtConfig::default();
        o.align_struct_fields = true;
        assert_eq!(
            format_with("struct P as\n  x: number\n  name: string\nend\n", &o).unwrap(),
            "struct P\n  x:    number\n  name: string\nend\n"
        );
    }

    /// `src` formats to `want`, and `want` formats to itself.
    fn stable(src: &str, want: &str) {
        assert_eq!(fmt(src), want);
        assert_eq!(fmt(want), want);
    }

    /// A child lookup by expression and the indexer of a table type key
    /// a value, as `t[k]` does, so their brackets stay tight. The spacing
    /// of an array literal wrote `part->[ name ]` and `{ read [ number ]: string }`.
    #[test]
    fn a_child_lookup_and_a_type_indexer_keep_tight_brackets() {
        stable(
            "local c = part->[name]\nlocal w = part=>[name]\nlocal v = map![name]\ntype R = { read [number]: string, write [string]: number }\n",
            "local c = part->[name]\nlocal w = part=>[name]\nlocal v = map![name]\ntype R = { read [number]: string, write [string]: number }\n",
        );
        stable("local xs = [1, 2]\n", "local xs = [ 1, 2 ]\n");
    }

    /// A return type is no place to break. Past the column, fmt broke a
    /// `{ T }` or `(A, B)` return type and kept the parameters on the
    /// line; it breaks the parameters first, as before `Result<A, B>`.
    #[test]
    fn a_long_header_breaks_its_parameters_before_its_return_type() {
        let params = "start: number, stop: number, step: number, extra: number, more: number";
        let broken = "(\n  start: number,\n  stop: number,\n  step: number,\n  extra: number,\n  more: number\n)";

        for ret in ["{ number }", "(number, number?)", "{ [string]: number }?"] {
            stable(
                &format!("export function walk({params}): {ret}\n  return nil\nend\n"),
                &format!("export function walk{broken}: {ret}\n  return nil\nend\n"),
            );
        }

        // A short header keeps its line.
        stable(
            "local function f(a: number): { number }\n  return { a }\nend\n",
            "local function f(a: number): { number }\n  return { a }\nend\n",
        );
    }

    /// A lone table argument hugs its parentheses. fmt wrote `copy(`,
    /// then `{` on a line of its own and the fields one level deeper:
    /// three lines of brackets for one argument.
    #[test]
    fn a_lone_table_argument_hugs_its_parentheses() {
        let fields = "first_long_key_name = 1, second_long_key_name = 2, third_long_key_name = 3, fourth = 4";
        let hugged = "  local t = copy({\n    first_long_key_name = 1,\n    second_long_key_name = 2,\n    third_long_key_name = 3,\n    fourth = 4,\n  })\n";

        for call in [
            format!("copy({{ {fields} }})"),
            format!("copy {{ {fields} }}"),
        ] {
            stable(
                &format!("do\n  local t = {call}\nend\n"),
                &format!("do\n{hugged}end\n"),
            );
        }

        // A table that fits stays on the line, and a second argument
        // breaks the list as before.
        stable("copy({ a = 1 })\n", "copy({ a = 1 })\n");
        stable(
            &format!("copy({{ {fields} }}, second_argument_here)\n"),
            &format!("copy(\n  {{ {fields} }},\n  second_argument_here\n)\n"),
        );
    }

    #[test]
    fn formatting_is_idempotent_on_the_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");

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
                // The token stream holds, save for the rewrites: quotes,
                // call parentheses, and the `as` of a header.
                let norm = |text: &str| -> Vec<String> {
                    lex(text)
                        .unwrap()
                        .toks
                        .iter()
                        .map(|t| t.text(text).replace('\'', "\""))
                        .filter(|t| !matches!(t.as_str(), "(" | ")" | "," | "as"))
                        .collect()
                };
                assert_eq!(norm(&src), norm(&once), "{}", path.display());
            }
        }
    }
}
