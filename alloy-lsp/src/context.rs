//! The completion contexts the proxy answers itself. The child sees the
//! emit, where an attribute, a macro call, a remote's side, or an import
//! no longer exists, so a completion there would list globals.

/// What the cursor sits in, from the text of its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// `@der|`: an attribute name. `sigil` is the byte offset of `@`, and
    /// `target` what the attribute would go on, when the position says.
    Attribute {
        prefix: String,
        sigil: usize,
        target: Option<&'static str>,
    },
    /// `@derive(Eq, De|`: a derive name.
    DeriveArg { prefix: String },
    /// `@cfg(ser|`: a condition, or a word that joins them.
    CfgArg { prefix: String },
    /// `$dg|`: an intrinsic or a macro. `sigil` is the byte offset of `$`.
    Macro { prefix: String, sigil: usize },
    /// `remote X(...) from cl|`: a side. `after` narrows to `or` or to
    /// the other side.
    RemoteSide {
        prefix: String,
        after: Option<String>,
    },
    /// `remote X(...) |`: the `from`.
    RemoteFrom { prefix: String },
    /// `struct Name |`, `enum Name |`, `interface Name |`: the `as` that
    /// opens the body, and `extends` for an interface. The child would
    /// offer `assert` here.
    DeclarationAs { prefix: String, interface: bool },
    /// `Move(num|` inside an `enum` body: a payload type. The emit turns
    /// the payload into a typed constructor, so the child sees no type
    /// slot at this position.
    EnumPayload { prefix: String },
    /// `import |` or `import type |`.
    ImportHead { prefix: String, type_only: bool },
    /// `import { a, b| } from "./m"`: names from the module.
    ImportNames {
        prefix: String,
        type_only: bool,
        spec: Option<String>,
        /// A name just ended, so `as` fits.
        after_name: bool,
    },
    /// `import * |`.
    ImportStar,
    /// `import * as M |` or `import { ... } |`: the `from`.
    ImportFrom,
    /// `attribute name(...) |`: the `on`.
    AttributeOn,
    /// `attribute name on fi|`: a target.
    AttributeTarget { prefix: String },
    /// Inside the string of `from "..."`, `require("...")`, or
    /// `import("...")`: a module path. `text` is what the string holds so
    /// far, and `start` the byte offset after the opening quote.
    ImportSpec { text: String, start: usize },
    /// Right after the closing quote of a module path, a new name the
    /// author is choosing, the inside of a string, a literal an
    /// attribute takes: the statement is done or the name is theirs, and
    /// no list belongs here.
    Nothing,
    /// A type goes here: after `type X =`, `satisfies`, `is`, `extends`,
    /// `impl`, a field's `:` in a struct body.
    TypeSlot { prefix: String },
    /// `new |`: a struct, or a class the engine constructs.
    NewTarget { prefix: String },
    /// `case |`: a variant of an enum, or `default`. `scrutinee` holds
    /// the text between the enclosing `match` and its `with`, when a
    /// `match` is open above the caret.
    MatchCase {
        prefix: String,
        scrutinee: Option<String>,
    },
    /// The modifier column of a struct or interface body: `read`,
    /// `write`, `private`, `public`, or the `end`.
    FieldStart { prefix: String },
    /// The member column of an `impl` or `trait` body: `function`,
    /// `async`, `private`, `public`, an attribute, or the `end`.
    MemberStart { prefix: String },
}

/// What the declaration of a name says about the name's type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared {
    /// The annotation it carries: `local m: Msg`, `const m: Msg`, or
    /// `m: Msg` as a parameter.
    Annotation(String),
    /// The expression it starts from: `local m = Msg.Join(p)`.
    Init(String),
}

/// The body the cursor sits in, when a declaration opened above it
/// and no `end` at the margin closed it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    Struct,
    Enum,
    Impl,
}

/// The declaration body around `line_start`: the nearest line at the
/// margin above that opens one, unless a margin `end` or another
/// margin statement sits between. Inside an `impl`, a method's own
/// block counts too: a cursor within one is in ordinary code.
fn enclosing_body(src: &str, line_start: usize) -> Option<Body> {
    let mut depth = 0i32;

    for line in src[..line_start].lines().rev() {
        let trimmed = line.trim_start();
        let at_margin = trimmed.len() == line.len();

        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }

        if !at_margin {
            // The blocks a method's body opens and closes, seen from
            // below: a closer first, then its opener.
            depth += block_closers(trimmed) - block_openers(trimmed);

            continue;
        }

        let decl = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let decl = decl.strip_prefix("@").map_or(decl, |_| "");

        return match decl.split_whitespace().next() {
            Some("struct" | "interface") if decl.contains(" as") || decl.ends_with("as") => {
                Some(Body::Struct)
            }
            Some("enum") if decl.contains(" as") => Some(Body::Enum),
            Some("impl" | "trait") if depth <= 0 => Some(Body::Impl),
            _ => None,
        };
    }

    None
}

fn block_openers(text: &str) -> i32 {
    text.split(|c: char| !is_word(c))
        .filter(|w| {
            matches!(
                *w,
                "function" | "if" | "for" | "while" | "do" | "match" | "repeat"
            )
        })
        .count() as i32
        - text
            .split(|c: char| !is_word(c))
            .filter(|w| matches!(*w, "do"))
            .count() as i32
            * i32::from(text.contains("while ") || text.contains("for "))
}

fn block_closers(text: &str) -> i32 {
    text.split(|c: char| !is_word(c))
        .filter(|w| matches!(*w, "end" | "until"))
        .count() as i32
}

/// Whether the cursor sits inside a quoted string on its line, with
/// escapes skipped; a long string is not counted.
fn inside_string(before: &str) -> bool {
    let mut open: Option<char> = None;
    let mut chars = before.chars();

    while let Some(c) = chars.next() {
        match open {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    open = None;
                }
            }

            None => {
                if c == '"' || c == '\'' || c == '`' {
                    open = Some(c);
                } else if c == '-' && chars.as_str().starts_with('-') {
                    return false;
                }
            }
        }
    }

    open.is_some()
}

/// Whether the cursor names a function's parameter: inside the
/// parenthesis of a `function` head, not after a `:` of the parameter.
fn names_a_parameter(head: &str) -> bool {
    let Some(f) = head.rfind("function") else {
        return false;
    };
    let after = &head[f + "function".len()..];
    let Some(open) = after.find('(') else {
        return false;
    };
    let params = &after[open + 1..];

    if params.matches('(').count() < params.matches(')').count() || params.contains(')') {
        return false;
    }

    let last = params.rsplit(',').next().unwrap_or(params);

    !last.contains(':') && !last.contains('=')
}

/// The base expression and the typed prefix of a member completion
/// right after `?.`: `bx?.na` answers `("bx", 2)`. `None` when the
/// cursor sits somewhere else.
pub fn optional_member_at(src: &str, offset: usize) -> Option<(String, usize)> {
    let head = src.get(..offset)?;
    let word = head.len() - head.trim_end_matches(is_word_byte).len();
    let before = head[..head.len() - word].strip_suffix("?.")?;
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word_byte(*c) || *c == '.')
        .last()
        .map(|(i, _)| i)?;
    let base = &before[start..];

    (!base.is_empty() && !base.ends_with('.')).then(|| (base.to_string(), word))
}

fn is_word_byte(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where the member of an optional access sits on the lowered line.
/// `a?.b` lowers to `(if a == nil then nil else a.b)`, all of it text
/// the compiler wrote, so the member has no position of its own; the
/// one the lowering wrote is where completion belongs.
pub fn optional_member_column(line: &str, base: &str, prefix: usize) -> Option<usize> {
    let needle = format!("{base}.");
    let at = line
        .match_indices(&needle)
        .filter(|(i, _)| {
            line[..*i]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_byte(c) && c != '.')
        })
        .map(|(i, _)| i)
        .last()?;
    let col = at + needle.len() + prefix;

    (col <= line.len()).then_some(col)
}

/// The string a module path is being typed in, when the cursor is inside
/// one after `from`, `require(`, or `import(`.
fn import_spec(src: &str, line_start: usize, offset: usize) -> Option<Context> {
    let before = &src[line_start..offset];
    let quote = before.rfind(['"', '\''])?;
    let text = &before[quote + 1..];

    // The cursor right after a closed path string: nothing to offer.
    if text.is_empty() {
        let quotes: Vec<usize> = before.match_indices(['"', '\'']).map(|(i, _)| i).collect();

        if quotes.len() >= 2 && quotes.len() % 2 == 0 {
            let open = quotes[quotes.len() - 2];
            let head = before[..open].trim_end();

            if head.ends_with("from") || head.ends_with("require(") || head.ends_with("import(") {
                return Some(Context::Nothing);
            }
        }
    }

    if text.contains(['"', '\'']) {
        return None;
    }

    let head = before[..quote].trim_end();

    if !(head.ends_with("from") || head.ends_with("require(") || head.ends_with("import(")) {
        return None;
    }

    Some(Context::ImportSpec {
        text: text.to_string(),
        start: line_start + quote + 1,
    })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether the cursor sits inside the parentheses of a variant, on a
/// line of an `enum` body: `Move(num|`. The head is the line up to the
/// word being typed.
fn in_enum_payload(src: &str, line_start: usize, head: &str) -> bool {
    // `Name(` with the parenthesis still open, attributes aside.
    let t = head.trim_start();
    let mut rest = t;

    while let Some(after) = rest.strip_prefix('@') {
        let end = after
            .find(|c: char| {
                !(is_word(c) || c == '(' || c == ')' || c == ',' || c == ' ' || c == '"')
            })
            .unwrap_or(after.len());
        rest = after[end..].trim_start();

        if rest == t {
            break;
        }
    }

    let name_len = rest.chars().take_while(|c| is_word(*c)).count();
    let after_name = rest[name_len..].trim_start();

    if name_len == 0 || !after_name.starts_with('(') {
        return false;
    }

    let opens = after_name.matches('(').count();
    let closes = after_name.matches(')').count();

    if opens <= closes {
        return false;
    }

    // The nearest declaration above is an `enum` that is still open.
    for line in src[..line_start].lines().rev() {
        let l = line.trim_start();
        let l = l.strip_prefix("export ").unwrap_or(l);

        if l.starts_with("enum ") {
            return true;
        }

        if l == "end"
            || l.starts_with("struct ")
            || l.starts_with("impl ")
            || l.starts_with("trait ")
            || l.starts_with("interface ")
            || l.starts_with("function ")
            || l.starts_with("local ")
        {
            return false;
        }
    }

    false
}

/// `struct Name `, `enum Name<T> `, `export interface Name `: the head
/// of a declaration whose body opener comes next. `Some(true)` for an
/// interface, which may take `extends` first.
fn declaration_head(head: &str) -> Option<bool> {
    let t = head.trim_start();
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let (rest, interface) = if let Some(r) = t.strip_prefix("struct ") {
        (r, false)
    } else if let Some(r) = t.strip_prefix("enum ") {
        (r, false)
    } else {
        (t.strip_prefix("interface ")?, true)
    };
    let rest = rest.trim_start();
    let name_len = rest.chars().take_while(|c| is_word(*c)).count();

    if name_len == 0 {
        return None;
    }

    let mut after = &rest[name_len..];

    if after.starts_with('<') {
        let close = after.find('>')?;
        after = &after[close + 1..];
    }

    (after.ends_with([' ', '\t']) && after.trim().is_empty()).then_some(interface)
}

/// The declaration keyword a line starts with, `export` aside.
fn declaration_word(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    let t = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
    let t = t
        .strip_prefix("local ")
        .or_else(|| t.strip_prefix("const "))
        .map(str::trim_start)
        .unwrap_or(t);
    let t = t.strip_prefix("async ").map(str::trim_start).unwrap_or(t);

    for (word, target) in [
        ("function ", "function"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("remote ", "remote"),
        ("interface ", "interface"),
        ("type ", "type"),
    ] {
        if t.starts_with(word) {
            return Some(target);
        }
    }

    None
}

/// What an attribute at this position would go on: a remote's parameter
/// inside its parentheses, a field or a variant inside a struct or an
/// enum, or the declaration the next non-attribute line starts.
fn attribute_target(
    src: &str,
    line_start: usize,
    line_end: usize,
    head: &str,
) -> Option<&'static str> {
    let opens = head.matches('(').count();
    let closes = head.matches(')').count();

    if opens > closes {
        return if head.trim_start().starts_with("remote ")
            || head.trim_start().starts_with("export remote ")
        {
            Some("param")
        } else {
            None
        };
    }

    // An indented line sits in a body: the nearest column-zero line above
    // names it. A column-zero `end` or another statement ends the search.
    let indented = head.starts_with(' ') || head.starts_with('\t');

    if indented {
        for line in src[..line_start].lines().rev() {
            if line.trim().is_empty() || !line.starts_with(|c: char| !c.is_whitespace()) {
                continue;
            }

            return match declaration_word(line) {
                Some("struct") | Some("interface") => Some("field"),
                Some("enum") => Some("variant"),
                _ => None,
            };
        }

        return None;
    }

    // At column zero the attribute precedes a declaration: skip the other
    // attribute lines, blanks, and comments to the first one.
    for line in src[line_end..].lines().skip(1) {
        let t = line.trim_start();

        if t.is_empty() || t.starts_with('@') || t.starts_with("--") {
            continue;
        }

        return declaration_word(line);
    }

    None
}

/// The word the cursor is at the end of.
fn trailing_word(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    &text[start..]
}

/// The last whole-word occurrence of `word` in `text`.
fn last_word_at(text: &str, word: &str) -> Option<usize> {
    let mut found = None;
    let mut from = 0;

    while let Some(i) = text[from..].find(word) {
        let start = from + i;
        let end = start + word.len();

        if !text[..start].chars().next_back().is_some_and(is_word)
            && !text[end..].chars().next().is_some_and(is_word)
        {
            found = Some(start);
        }

        from = start + 1;
    }

    found
}

/// The scrutinee of a `match` head: the text between `match` and the
/// `with` that ends the head. `local r = match x with` and `return
/// match x with` read the same as the statement form.
fn scrutinee_of(line: &str) -> Option<String> {
    let with = last_word_at(line, "with")?;
    let start = last_word_at(&line[..with], "match")? + "match".len();
    let text = line[start..with].trim();

    (!text.is_empty()).then(|| text.to_string())
}

/// The expression the `match` around the caret takes. The scan counts
/// the blocks upward, so the nearest open `match` wins and an inner one
/// shadows an outer.
fn match_scrutinee(src: &str, offset: usize) -> Option<String> {
    let mut depth = 0i32;

    for line in src[..offset.min(src.len())].lines().rev() {
        let t = line.trim();

        if t.is_empty() || t.starts_with("--") {
            continue;
        }

        depth += block_closers(t);

        let opens = block_openers(t);

        // More opened here than the scan closed below: this line opens
        // the block the caret sits in.
        if opens > depth {
            return scrutinee_of(t);
        }

        depth -= opens;
    }

    None
}

/// Whether the text before a name binds it: a `local` or a `const`, or
/// the comma of a list either one opened.
fn binds(before: &str) -> bool {
    let t = before.trim_end();

    for word in ["local", "const"] {
        if let Some(head) = t.strip_suffix(word) {
            return !head.ends_with(is_word);
        }
    }

    t.ends_with(',') && {
        let head = t.trim_start();

        head.starts_with("local ") || head.starts_with("const ")
    }
}

/// Whether a name sits in the parameter list of a `function` head.
fn in_parameters(before: &str) -> bool {
    before.contains("function") && before.matches('(').count() > before.matches(')').count()
}

/// The type an annotation names, up to the `,`, `)`, or `=` that ends
/// it at the top level.
fn type_text(rest: &str) -> String {
    let mut depth = 0i32;
    let mut end = rest.len();

    for (i, c) in rest.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,

            '>' | ')' | ']' | '}' => {
                if depth == 0 {
                    end = i;

                    break;
                }

                depth -= 1;
            }

            ',' | '=' if depth == 0 => {
                end = i;

                break;
            }

            _ => {}
        }
    }

    rest[..end].trim().to_string()
}

fn declared_in_line(line: &str, name: &str) -> Option<Declared> {
    let mut from = 0;

    while let Some(i) = line[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        from = start + 1;
        let before = &line[..start];

        if before.chars().next_back().is_some_and(is_word)
            || line[end..].chars().next().is_some_and(is_word)
            // A field or a member of something else.
            || before.trim_end().ends_with(['.', ':'])
        {
            continue;
        }

        let after = line[end..].trim_start();

        if let Some(rest) = after.strip_prefix(':')
            && !rest.starts_with(':')
            && (binds(before) || in_parameters(before))
        {
            return Some(Declared::Annotation(type_text(rest)));
        }

        if let Some(rest) = after.strip_prefix('=')
            && !rest.starts_with('=')
            && binds(before)
        {
            return Some(Declared::Init(rest.trim().to_string()));
        }
    }

    None
}

/// The type the `impl` block around the caret is for: the `X` of
/// `impl X` and of `impl Trait for X`. The first line at the margin
/// above the caret decides.
pub fn impl_target(src: &str, offset: usize) -> Option<String> {
    let line = src[..offset.min(src.len())]
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))?;
    let rest = line.trim().strip_prefix("impl ")?;
    let target = rest.rsplit(" for ").next().unwrap_or(rest).trim();
    let name: String = target.chars().take_while(|c| is_word(*c)).collect();

    (!name.is_empty()).then_some(name)
}

/// What the nearest declaration of `name` above the caret says. A use
/// of the name and a member access are not declarations.
pub fn declared(src: &str, offset: usize, name: &str) -> Option<Declared> {
    if name.is_empty() {
        return None;
    }

    src[..offset.min(src.len())]
        .lines()
        .rev()
        .find_map(|line| declared_in_line(line, name))
}

pub fn detect(src: &str, offset: usize) -> Option<Context> {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);

    if let Some(spec) = import_spec(src, line_start, offset) {
        return Some(spec);
    }

    let line_end = src[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(src.len());
    let before = &src[line_start..offset];
    let line = &src[line_start..line_end];
    let prefix = trailing_word(before);
    let head = &before[..before.len() - prefix.len()];

    // A sigil right before the word.
    if head.ends_with('@') {
        return Some(Context::Attribute {
            prefix: prefix.to_string(),
            sigil: line_start + head.len() - 1,
            target: attribute_target(src, line_start, line_end, head),
        });
    }

    if head.ends_with('$') {
        return Some(Context::Macro {
            prefix: prefix.to_string(),
            sigil: line_start + head.len() - 1,
        });
    }

    if let Some(i) = head.rfind("@derive(")
        && !head[i..].contains(')')
    {
        return Some(Context::DeriveArg {
            prefix: prefix.to_string(),
        });
    }

    // `any(` and `all(` nest, so the parenthesis is open while more
    // opened than closed.
    if let Some(i) = head.rfind("@cfg(")
        && head[i..].matches('(').count() > head[i..].matches(')').count()
    {
        return Some(Context::CfgArg {
            prefix: prefix.to_string(),
        });
    }

    let trimmed = before.trim_start();

    // A string that is no module path: the child answers alone, since it
    // knows the class names `Instance.new("` and `GetService("` take.
    if inside_string(head) {
        return None;
    }

    // The literal arguments of an attribute.
    for name in [
        "@ratelimit(",
        "@timeout(",
        "@rename(",
        "@u8(",
        "@deprecated(",
    ] {
        if let Some(i) = head.rfind(name)
            && !head[i..].contains(')')
        {
            return Some(Context::Nothing);
        }
    }

    // A word a type, a constructor, or a variant goes after.
    let head_words: Vec<&str> = head.split_whitespace().collect();

    if (head.ends_with(' ') || head.ends_with('=')) && !head_words.is_empty() {
        let last = head_words[head_words.len() - 1];
        let second = head_words.len().checked_sub(2).map(|i| head_words[i]);
        let type_decl = head_words.first() == Some(&"type")
            || (head_words.first() == Some(&"export") && head_words.get(1) == Some(&"type"));

        if matches!(last, "satisfies" | "is" | "extends" | "impl")
            || (last == "for" && head_words.first() == Some(&"impl"))
            || (last == "not" && second == Some("is"))
            || (last == "=" && type_decl && head_words.len() == 3)
        {
            return Some(Context::TypeSlot {
                prefix: prefix.to_string(),
            });
        }

        if last == "new" {
            return Some(Context::NewTarget {
                prefix: prefix.to_string(),
            });
        }

        if last == "case" {
            return Some(Context::MatchCase {
                prefix: prefix.to_string(),
                scrutinee: match_scrutinee(src, offset),
            });
        }
    }

    // A new name the author is choosing.
    if names_a_parameter(head) {
        return Some(Context::Nothing);
    }

    if let Some(w) = head_words.first()
        && *w == "for"
        && !head_words.contains(&"in")
    {
        return Some(Context::Nothing);
    }

    if let Some(i) = head.rfind("case ")
        && head[i..].contains('(')
        && !head[i..].contains(')')
    {
        return Some(Context::Nothing);
    }

    if (trimmed.starts_with("local [") || trimmed.starts_with("const [")) && !head.contains('=') {
        return Some(Context::Nothing);
    }

    // The body of a declaration.
    match enclosing_body(src, line_start) {
        Some(Body::Struct) => {
            let at_column = head.trim().is_empty();

            if head.contains(':') {
                return Some(Context::TypeSlot {
                    prefix: prefix.to_string(),
                });
            }

            if at_column {
                return Some(Context::FieldStart {
                    prefix: prefix.to_string(),
                });
            }

            return Some(Context::Nothing);
        }

        Some(Body::Enum) => {
            if in_enum_payload(src, line_start, head) {
                return Some(Context::EnumPayload {
                    prefix: prefix.to_string(),
                });
            }

            return Some(Context::Nothing);
        }

        Some(Body::Impl) => {
            let at_column = head.trim().is_empty();

            if at_column {
                return Some(Context::MemberStart {
                    prefix: prefix.to_string(),
                });
            }
        }

        None => {}
    }

    if let Some(interface) = declaration_head(head) {
        return Some(Context::DeclarationAs {
            prefix: prefix.to_string(),
            interface,
        });
    }

    if in_enum_payload(src, line_start, head) {
        return Some(Context::EnumPayload {
            prefix: prefix.to_string(),
        });
    }

    let attr_decl = trimmed
        .strip_prefix("export ")
        .unwrap_or(trimmed)
        .strip_prefix("attribute ");

    if let Some(rest) = attr_decl {
        let rest_head = &rest[..rest.len() - prefix.len()];

        if let Some(i) = rest_head.rfind(" on") {
            let after = &rest_head[i + " on".len()..];

            if after.is_empty() || after.starts_with(' ') {
                return Some(Context::AttributeTarget {
                    prefix: prefix.to_string(),
                });
            }
        }

        // The name, and the parameters when closed, then the cursor.
        let closed = rest_head.trim_end();
        let named = !closed.is_empty() && (!closed.contains('(') || closed.ends_with(')'));

        if named && rest_head.ends_with(' ') {
            return Some(Context::AttributeOn);
        }

        return None;
    }

    if trimmed.starts_with("remote ") || trimmed.starts_with("export remote ") {
        // The parameters closed and no `from` yet: the `from` comes next.
        let opens = head.matches('(').count();
        let closes = head.matches(')').count();

        if closes > 0 && opens == closes && !head.contains(" from") && head.ends_with(' ') {
            return Some(Context::RemoteFrom {
                prefix: prefix.to_string(),
            });
        }

        if let Some(i) = head.rfind(" from") {
            let tail = head[i + " from".len()..].trim();

            if tail.is_empty() {
                return Some(Context::RemoteSide {
                    prefix: prefix.to_string(),
                    after: None,
                });
            }

            let words: Vec<&str> = tail.split_whitespace().collect();

            match words.as_slice() {
                [side] if matches!(*side, "client" | "server") => {
                    return Some(Context::RemoteSide {
                        prefix: prefix.to_string(),
                        after: Some(format!("{side} ")),
                    });
                }

                [side, "or"] if matches!(*side, "client" | "server") => {
                    return Some(Context::RemoteSide {
                        prefix: prefix.to_string(),
                        after: Some(format!("{side} or")),
                    });
                }

                _ => {}
            }
        }

        return None;
    }

    if trimmed.starts_with("import")
        && (trimmed.len() == 6 || !is_word(trimmed.as_bytes()[6] as char))
    {
        let rest = head.trim_start()["import".len()..].trim_start();
        let type_only = rest.starts_with("type ") || rest == "type";
        let rest = rest
            .strip_prefix("type")
            .map(str::trim_start)
            .unwrap_or(rest);

        if rest.is_empty() {
            return Some(Context::ImportHead {
                prefix: prefix.to_string(),
                type_only,
            });
        }

        if let Some(open) = rest.find('{') {
            if rest[open..].contains('}') {
                return Some(Context::ImportFrom);
            }

            let inside = &rest[open + 1..];
            let after_name = inside.trim_end().chars().last().is_some_and(is_word)
                && inside.ends_with(' ')
                && prefix.is_empty();
            // The path in either quote.
            let spec = line
                .find("from")
                .and_then(|i| {
                    let rest = &line[i..];
                    let q = rest.find(['"', '\''])?;
                    let quote = rest.as_bytes()[q] as char;
                    let inner = &rest[q + 1..];
                    let end = inner.find(quote)?;

                    Some(&inner[..end])
                })
                .map(str::to_string);

            return Some(Context::ImportNames {
                prefix: prefix.to_string(),
                type_only,
                spec,
                after_name,
            });
        }

        if let Some(after_star) = rest.strip_prefix('*') {
            let after_star = after_star.trim_start();

            if after_star.is_empty() {
                return Some(Context::ImportStar);
            }

            if let Some(named) = after_star.strip_prefix("as")
                && named.split_whitespace().count() == 1
                && named.ends_with(' ')
            {
                return Some(Context::ImportFrom);
            }

            return None;
        }

        // `import Name |`: a default import wants `from`.
        if rest.split_whitespace().count() == 1 && rest.ends_with(' ') {
            return Some(Context::ImportFrom);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    /// The base and the typed prefix of a completion after `?.`, and
    /// where the member sits on the lowered line.
    #[test]
    fn an_optional_member_reads_its_base_and_its_prefix() {
        let src = "local deep = bx?.na";
        assert_eq!(
            super::optional_member_at(src, src.len()),
            Some(("bx".to_string(), 2))
        );
        let dangling = "local deep = bx?.";
        assert_eq!(
            super::optional_member_at(dangling, dangling.len()),
            Some(("bx".to_string(), 0))
        );
        assert_eq!(super::optional_member_at("local x = bx.na", 15), None);

        let line = "local deep = (if bx == nil then nil else bx.name)";
        assert_eq!(
            super::optional_member_column(line, "bx", 0),
            Some(line.find("else bx.").unwrap() + 8)
        );
        // A base that only ends another name does not match.
        assert_eq!(
            super::optional_member_column("local q = abx.name", "bx", 0),
            None
        );
    }

    use super::*;

    fn at(src: &str) -> Option<Context> {
        let offset = src.find('|').unwrap();
        detect(&src.replace('|', ""), offset)
    }

    #[test]
    fn a_new_name_and_a_finished_token_answer_nothing() {
        assert_eq!(at("function f(alpha: number, bet|"), Some(Context::Nothing));
        assert_eq!(at("local function f(|"), Some(Context::Nothing));
        assert_eq!(at("for _, ite| in items do"), Some(Context::Nothing));
        assert_eq!(
            at("match m with\n    case Join(pi|"),
            Some(Context::Nothing)
        );
        assert_eq!(at("local [ fir| ] = xs"), Some(Context::Nothing));
        assert_eq!(at("local s = \"hel|lo\""), None);
        assert_eq!(at("@ratelimit(2|"), Some(Context::Nothing));
        assert_eq!(at("struct Holder as\n    read na|"), Some(Context::Nothing));
        assert_eq!(at("enum Kind as\n    Al|"), Some(Context::Nothing));
        assert_eq!(at("function f(a: |"), None);
        assert_eq!(at("for k, v in pa|"), None);
    }

    #[test]
    fn type_slots_and_bodies_have_their_own_lists() {
        let ty = |p: &str| {
            Some(Context::TypeSlot {
                prefix: p.to_string(),
            })
        };
        assert_eq!(at("type Alias = |"), ty(""));
        assert_eq!(at("local v = t satisfies Ha|"), ty("Ha"));
        assert_eq!(at("if key is |"), ty(""));
        assert_eq!(at("if key is not |"), ty(""));
        assert_eq!(at("interface Both extends |"), ty(""));
        assert_eq!(at("impl Dr|"), ty("Dr"));
        assert_eq!(at("impl Drawable for |"), ty(""));
        assert_eq!(at("struct Box as\n    inner: |"), ty(""));
        assert_eq!(
            at("struct Box as\n    |"),
            Some(Context::FieldStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("struct Box as\n    pri|"),
            Some(Context::FieldStart {
                prefix: "pri".to_string()
            })
        );
        assert_eq!(
            at("impl Box\n    |"),
            Some(Context::MemberStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("impl Box\n    function f(self)\n        local x = |"),
            None
        );
        assert_eq!(at("struct Box as\nend\nlocal x = |"), None);
        assert_eq!(
            at("local made = new |"),
            Some(Context::NewTarget {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("match m with\n    case |"),
            Some(Context::MatchCase {
                prefix: String::new(),
                scrutinee: Some("m".to_string()),
            })
        );
    }

    fn scrutinee(src: &str) -> Option<String> {
        match at(src) {
            Some(Context::MatchCase { scrutinee, .. }) => scrutinee,

            other => panic!("not a case list: {other:?}"),
        }
    }

    #[test]
    fn a_case_reads_the_expression_its_match_takes() {
        let some = |s: &str| Some(s.to_string());

        // The statement form and the two expression forms.
        assert_eq!(scrutinee("match msg with\n    case |"), some("msg"));
        assert_eq!(
            scrutinee("local r = match msg with\n    case |"),
            some("msg")
        );
        assert_eq!(
            scrutinee("    return match msg with\n    case |"),
            some("msg")
        );
        assert_eq!(
            scrutinee("match year % 4, year % 100 with\n    case |"),
            some("year % 4, year % 100")
        );

        // The head on the caret's own line.
        assert_eq!(scrutinee("match msg with case |"), some("msg"));

        // An inner match wins; once it closes the outer one is back.
        let nested = "match a with\n    case X then\n        match b with\n            case |";
        assert_eq!(scrutinee(nested), some("b"));

        let closed = concat!(
            "match a with\n",
            "    case X then\n",
            "        match b with\n",
            "            case Y then f()\n",
            "        end\n",
            "    case |"
        );
        assert_eq!(scrutinee(closed), some("a"));

        // An `if` inside an arm opens and closes on one line.
        let guarded = concat!(
            "match a with\n",
            "    case X then\n",
            "        if p then q() end\n",
            "    case |"
        );
        assert_eq!(scrutinee(guarded), some("a"));

        // No match above: the proxy keeps its full list.
        assert_eq!(scrutinee("local x = 1\ncase |"), None);
    }

    #[test]
    fn a_declaration_gives_its_annotation_or_its_first_value() {
        let src = concat!(
            "local function handle(msg: Msg, tries: number)\n",
            "    local parsed: Result<number, string> = Ok(1)\n",
            "    const start = Msg.Join(p)\n",
            "    local names: string[] = {}\n",
            "    local t = start\n"
        );
        let end = src.len();
        let ann = |n: &str| declared(src, end, n);

        assert_eq!(ann("msg"), Some(Declared::Annotation("Msg".to_string())));
        assert_eq!(
            ann("tries"),
            Some(Declared::Annotation("number".to_string()))
        );
        assert_eq!(
            ann("parsed"),
            Some(Declared::Annotation("Result<number, string>".to_string()))
        );
        assert_eq!(
            ann("start"),
            Some(Declared::Init("Msg.Join(p)".to_string()))
        );
        assert_eq!(
            ann("names"),
            Some(Declared::Annotation("string[]".to_string()))
        );
        assert_eq!(ann("t"), Some(Declared::Init("start".to_string())));
        assert_eq!(ann("p"), None);
    }

    #[test]
    fn self_takes_the_type_the_impl_is_for() {
        let one = "impl Msg\n    function tag(self)\n        match self with\n";
        assert_eq!(impl_target(one, one.len()), Some("Msg".to_string()));

        let two = "impl Shape for Circle\n    function area(self)\n";
        assert_eq!(impl_target(two, two.len()), Some("Circle".to_string()));

        let none = "local function f()\n    match self with\n";
        assert_eq!(impl_target(none, none.len()), None);
    }

    #[test]
    fn a_variant_payload_is_a_type_slot() {
        assert_eq!(
            at("enum Msg as\n    Move(num|"),
            Some(Context::EnumPayload {
                prefix: "num".to_string()
            })
        );
        assert_eq!(
            at("enum Msg as\n    Move(number, |"),
            Some(Context::EnumPayload {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("enum Msg as\n    Move(number) |"),
            Some(Context::Nothing)
        );
        assert_eq!(at("enum Msg as\nend\nlocal x = f(num|"), None);
        assert_eq!(at("local x = f(num|"), None);
    }

    #[test]
    fn import_names_read_the_path_in_either_quote() {
        match at("import { | } from '@pkg/jecs'") {
            Some(Context::ImportNames { spec, .. }) => {
                assert_eq!(spec.as_deref(), Some("@pkg/jecs"))
            }
            other => panic!("{other:?}"),
        }
        match at("import { | } from \"./lib\"") {
            Some(Context::ImportNames { spec, .. }) => assert_eq!(spec.as_deref(), Some("./lib")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_declaration_name_wants_as() {
        assert_eq!(
            at("enum Test |"),
            Some(Context::DeclarationAs {
                prefix: String::new(),
                interface: false
            })
        );
        assert_eq!(
            at("export struct Vec2<T> as|"),
            Some(Context::DeclarationAs {
                prefix: "as".to_string(),
                interface: false
            })
        );
        assert_eq!(
            at("interface Entity ex|"),
            Some(Context::DeclarationAs {
                prefix: "ex".to_string(),
                interface: true
            })
        );
        assert_eq!(at("enum Test as |"), None);
        assert_eq!(at("enum |"), None);
    }

    #[test]
    fn sigils() {
        assert_eq!(
            at("@der|"),
            Some(Context::Attribute {
                prefix: "der".to_string(),
                sigil: 0,
                target: None
            })
        );
        assert_eq!(
            at("local x = $d|"),
            Some(Context::Macro {
                prefix: "d".to_string(),
                sigil: 10
            })
        );
        assert_eq!(
            at("@derive(Eq, De|"),
            Some(Context::DeriveArg {
                prefix: "De".to_string()
            })
        );
        assert_eq!(at("@derive(Eq) |"), None);
        assert_eq!(
            at("@cfg(any(server, cl|"),
            Some(Context::CfgArg {
                prefix: "cl".to_string()
            })
        );
        assert_eq!(at("@cfg(server) |"), None);
    }

    #[test]
    fn remote_sides() {
        assert_eq!(
            at("remote Ping(n: number) from |"),
            Some(Context::RemoteSide {
                prefix: String::new(),
                after: None
            })
        );
        assert_eq!(
            at("export remote Ping(n: number) from client or s|"),
            Some(Context::RemoteSide {
                prefix: "s".to_string(),
                after: Some("client or".to_string())
            })
        );
        assert_eq!(
            at("export remote Test() |"),
            Some(Context::RemoteFrom {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("remote function Get(id: number): Profile fr|"),
            Some(Context::RemoteFrom {
                prefix: "fr".to_string()
            })
        );
        assert_eq!(at("remote Test(|"), None);
        assert_eq!(at("local from = 1 |"), None);
    }

    #[test]
    fn import_specs() {
        assert_eq!(
            at("import { a } from \"./mod|\""),
            Some(Context::ImportSpec {
                text: "./mod".to_string(),
                start: 19
            })
        );
        assert_eq!(
            at("local m = require('@pack|"),
            Some(Context::ImportSpec {
                text: "@pack".to_string(),
                start: 19
            })
        );
        assert_eq!(at("local s = \"from |\""), None);
        assert_eq!(at("import { a } from '@pkg/jecs'|"), Some(Context::Nothing));
        assert_eq!(at("local m = require(\"./x\"|)"), Some(Context::Nothing));
    }

    fn target_of(src: &str) -> Option<&'static str> {
        match at(src) {
            Some(Context::Attribute { target, .. }) => target,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn attribute_targets_follow_the_position() {
        assert_eq!(target_of("@|\nstruct V as\nend\n"), Some("struct"));
        assert_eq!(
            target_of("@|\n@derive(Eq)\n-- note\nexport enum E as A end\n"),
            Some("enum")
        );
        assert_eq!(
            target_of("@|\nlocal async function f() end\n"),
            Some("function")
        );
        assert_eq!(
            target_of("@|\nexport remote Ping(n: number) from client\n"),
            Some("remote")
        );
        assert_eq!(target_of("remote Ping(@|"), Some("param"));
        assert_eq!(
            target_of("struct V as\n    @|\n    x: number\nend\n"),
            Some("field")
        );
        assert_eq!(
            target_of("enum E as\n    @|\n    A\nend\n"),
            Some("variant")
        );
        assert_eq!(target_of("struct V as\nend\n    @|\n"), None);
        assert_eq!(target_of("@|\n"), None);
    }

    #[test]
    fn attribute_declarations() {
        assert_eq!(
            at("attribute icon(asset: string) |"),
            Some(Context::AttributeOn)
        );
        assert_eq!(at("export attribute skip |"), Some(Context::AttributeOn));
        assert_eq!(at("attribute skip o|"), Some(Context::AttributeOn));
        assert_eq!(
            at("attribute icon(asset: string) on |"),
            Some(Context::AttributeTarget {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("attribute icon(asset: string) on struct, en|"),
            Some(Context::AttributeTarget {
                prefix: "en".to_string()
            })
        );
        assert_eq!(at("attribute icon(asset: |"), None);
    }

    #[test]
    fn imports() {
        assert_eq!(
            at("import |"),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: false
            })
        );
        assert_eq!(
            at("import type |"),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: true
            })
        );
        assert_eq!(
            at("import { a, b| } from \"./m\""),
            Some(Context::ImportNames {
                prefix: "b".to_string(),
                type_only: false,
                spec: Some("./m".to_string()),
                after_name: false
            })
        );
        assert_eq!(
            at("import { a |"),
            Some(Context::ImportNames {
                prefix: String::new(),
                type_only: false,
                spec: None,
                after_name: true
            })
        );
        assert_eq!(at("import * |"), Some(Context::ImportStar));
        assert_eq!(at("import * as M |"), Some(Context::ImportFrom));
        assert_eq!(at("import { a } |"), Some(Context::ImportFrom));
        assert_eq!(at("import Panel |"), Some(Context::ImportFrom));
    }
}
