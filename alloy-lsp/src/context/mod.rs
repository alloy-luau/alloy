//! The completion contexts the proxy answers itself. The child sees the
//! emit, where an attribute, a macro call, a remote's side, or an import
//! no longer exists, so a completion there would list globals.

mod bodies;
mod declarations;
mod fields;
mod imports;
mod matches;
mod members;
mod scope;
mod strings;
mod types;

use bodies::Body;
use strings::{code_of, last_word};

// The proxy and the playground's wasm build (`alloy-web`, which
// `#[path]`-includes this file) each call a different subset of these,
// so a name unused in one of the two is still the module's public
// surface, not dead code.
#[allow(unused_imports)]
pub use bodies::impl_target;
#[allow(unused_imports)]
pub use declarations::declared;
#[allow(unused_imports)]
pub use fields::{Field, instance_class, record_entries};
#[allow(unused_imports)]
pub use matches::match_arms;
#[allow(unused_imports)]
pub use members::{
    Access, guarded_member_column, index_at, index_key_at, member_at, member_column,
};
#[allow(unused_imports)]
pub use scope::{Local, LocalKind, locals_in_scope};

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
    ImportHead {
        prefix: String,
        type_only: bool,
        /// The path after `from`, when the line already carries one.
        spec: Option<String>,
    },
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
    /// `import Name, |`: a default binding took the first slot, so the
    /// names in braces come next.
    ImportBrace,
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
    /// `impl`, a field's `:` in a struct body. Every slot takes the
    /// same list; `prefers` says which names rank first.
    TypeSlot { prefix: String, prefers: Prefers },
    /// `new |`: a struct, or a class the engine constructs.
    NewTarget { prefix: String },
    /// `destroy part |`: the `after` that puts the removal on a timer.
    DestroyAfter { prefix: String },
    /// `after 3 |`: the `do` that opens the block, and the `where` that
    /// puts a condition on it. `filtered` is true once a `where` is
    /// written, so only the `do` is left.
    AfterDo { prefix: String, filtered: bool },
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
    /// The member column of a `trait` body, which takes no visibility:
    /// every method a trait declares is public.
    TraitMemberStart { prefix: String },
    /// `new Stats { |`: a field of the struct the literal fills.
    StructField { prefix: String, target: String },
    /// The variant column of an `enum` body: a new name, and the `end`.
    VariantStart { prefix: String },
    /// Inside the string of `new Instance("|")`: an engine class name.
    /// The emit moves the call, so the child answers elsewhere.
    ClassName { prefix: String },
    /// `new Instance("Part") { |`: a property of the class the string
    /// names. The emit turns the table into assignments.
    InstanceField { prefix: String, class: String },
    /// `profile["|`, `profile[|`, `profile?[|`, `profile![|`: a string
    /// key of the receiver's type. `prefix` is what the author typed
    /// inside the bracket, and `quote` the opening quote when one
    /// stands there.
    IndexKey {
        prefix: String,
        receiver: String,
        quote: Option<char>,
    },
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

/// What a type slot ranks first. The list is the same for every slot:
/// a name the checker rejects is still a name the author may declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefers {
    /// `satisfies`, `is`, a field's `:`, a type argument: any type.
    Any,
    /// `extends` and the trait of `impl Trait for X`.
    Contract,
    /// `impl X` and the target of `impl Trait for X`.
    Concrete,
}

/// The word the cursor is at the end of.
fn trailing_word(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| strings::is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    &text[start..]
}

/// Whether the `:` a head ends with closes a ternary. The `?` of
/// `c ? a : b` carries a space in front; `x?.y` and `T?` do not.
pub fn ternary_else(head: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut prev = ' ';

    for c in head.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }

            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '?' if prev.is_whitespace() => return true,
                _ => {}
            },
        }

        prev = c;
    }

    false
}

/// Whether an expression may start at the caret: after `then`, `else`,
/// the `?` or the `:` of a ternary, `default`, `=`, `(`, `,`, `return`,
/// or an operator. luau-lsp answers nothing at those bytes inside an
/// `if` expression and right before a literal, so the proxy builds the
/// value scope for them itself.
pub fn expression_start(src: &str, offset: usize) -> bool {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let before = &src[line_start..offset];

    // A comment and a string take no expression.
    if inside_string(before) || code_of(before).len() < before.len() {
        return false;
    }

    let prefix = trailing_word(before);
    let head = &before[..before.len() - prefix.len()];
    let t = head.trim_end();

    // An empty head starts a statement, and a `.` opens a member.
    if t.is_empty() || t.ends_with(['.', '@', '$', ';']) {
        return false;
    }

    // A method call's `:` and an annotation's `:` take no expression;
    // the `?` in front is what makes the `:` an else.
    if t.ends_with(':') {
        return !t.ends_with("::") && ternary_else(t);
    }

    // `parent=>Name` waits for a child by name.
    if t.ends_with("=>") {
        return false;
    }

    if t.ends_with([
        '=', '(', ',', '[', '{', '?', '+', '-', '*', '/', '%', '^', '<', '>', '~', '|', '&',
    ]) {
        return true;
    }

    matches!(
        last_word(t),
        "then"
            | "else"
            | "default"
            | "return"
            | "and"
            | "or"
            | "not"
            | "in"
            | "await"
            | "try"
            | "if"
            | "elseif"
            | "while"
    )
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

/// Whether the caret sits inside a quoted string on its own line.
pub fn in_string(src: &str, offset: usize) -> bool {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);

    inside_string(&src[line_start..offset])
}

/// The word a token opens, past the brackets the source put in front
/// of it: `(new` names `new`, and `=` stays itself.
fn opening_word(token: &str) -> &str {
    let word = token.trim_start_matches(|c: char| !strings::is_word(c));

    match word.is_empty() {
        true => token,

        false => word,
    }
}

/// Whether the caret sits in the class string of `new Instance("`.
/// The emit turns the call into `Instance.new("Part")` and moves it, so
/// the position the child reads is no longer the string.
fn names_a_class(head: &str) -> bool {
    let Some(open) = head.rfind(['"', '\'']) else {
        return false;
    };

    if head[open + 1..].contains(['"', '\'']) {
        return false;
    }

    let before = head[..open].trim_end();

    before.ends_with("new Instance(") || before.ends_with("Instance.new(")
}

/// Whether the cursor names a parameter: inside the parenthesis of a
/// `function` head or of a `remote` declaration, not after a `:` of the
/// parameter. A remote's parameters are names the author is choosing,
/// the way a function's are.
fn names_a_parameter(head: &str) -> bool {
    let trimmed = head.trim_start();
    let statement = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let indent = head.len() - statement.len();
    // A `remote` and a `macro` name their parameters the way a
    // `function` does, and neither word reaches the emit.
    let declared = ["remote ", "macro "]
        .iter()
        .find(|word| statement.starts_with(*word))
        .map(|word| indent + word.trim_end().len());
    let Some(f) = head
        .rfind("function")
        .map(|f| f + "function".len())
        .or(declared)
    else {
        return false;
    };
    let after = &head[f..];
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

/// Whether the caret sits in the braces of a destructuring `local` or
/// `const`. The names there come from the value alone, so no std name
/// and no keyword belongs in the list.
pub fn in_destructure(src: &str, offset: usize) -> bool {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let head = src[line_start..offset].trim_start();
    let head = head.strip_prefix("export ").unwrap_or(head);
    let opened = head
        .strip_prefix("local ")
        .or_else(|| head.strip_prefix("const "));

    opened.is_some_and(|rest| {
        let rest = rest.trim_start();

        rest.starts_with('{') && !rest.contains('}') && !rest.contains('=')
    })
}

pub fn detect(src: &str, offset: usize) -> Option<Context> {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);

    if let Some(spec) = imports::import_spec(src, line_start, offset) {
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
            target: bodies::attribute_target(src, line_start, line_end, head),
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

    // A key inside an index that never closed. The receiver's own type
    // names the keys; the child sees the whole scope there instead,
    // and after `?[` or `![` it sees the guard the emit wrote.
    if let Some((receiver, typed, quote)) = members::index_key_at(src, offset) {
        return Some(Context::IndexKey {
            prefix: typed,
            receiver,
            quote,
        });
    }

    // A string that is no module path: the child answers alone, since it
    // knows the class names `Instance.new("` and `GetService("` take.
    if inside_string(head) {
        return names_a_class(head).then(|| Context::ClassName {
            prefix: prefix.to_string(),
        });
    }

    // `parent=>Name` waits for a child by name. No type carries the
    // children of an instance, so no list belongs here.
    if head.ends_with("=>") {
        return Some(Context::Nothing);
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
        // `(new Instance(...))` and `[ new Point {} ]` open the word with
        // a bracket, which is no part of it.
        let last = opening_word(last);
        let second = head_words
            .len()
            .checked_sub(2)
            .map(|i| opening_word(head_words[i]));
        let type_decl = head_words.first() == Some(&"type")
            || (matches!(head_words.first(), Some(&"export") | Some(&"global"))
                && head_words.get(1) == Some(&"type"));

        if matches!(last, "satisfies" | "is" | "extends" | "impl")
            || (last == "for" && head_words.first() == Some(&"impl"))
            || (last == "not" && second == Some("is"))
            || (last == "=" && type_decl && head_words.len() == 3)
        {
            // `impl |Trait for X` names a trait; `impl |X` and
            // `impl Trait for |X` name the type the methods go on.
            let prefers = match last {
                "extends" => Prefers::Contract,

                "impl" if line[before.len()..].contains(" for ") => Prefers::Contract,

                "impl" | "for" => Prefers::Concrete,

                _ => Prefers::Any,
            };

            return Some(Context::TypeSlot {
                prefix: prefix.to_string(),
                prefers,
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
                scrutinee: matches::match_scrutinee(src, offset),
            });
        }

        // `destroy part |`: the operand is written, so `after` is the
        // one word left.
        if head_words.first() == Some(&"destroy") && head_words.len() == 2 {
            return Some(Context::DestroyAfter {
                prefix: prefix.to_string(),
            });
        }

        // `after 3 |` and `after 3 where ready |`.
        if head_words.first() == Some(&"after")
            && head_words.len() >= 2
            && !head_words.contains(&"do")
        {
            return Some(Context::AfterDo {
                prefix: prefix.to_string(),
                filtered: head_words.contains(&"where"),
            });
        }
    }

    // `$matches(value, |Busy(_))` takes a pattern, the way an arm does.
    if let Some(value) = matches::matches_scrutinee(head) {
        return Some(Context::MatchCase {
            prefix: prefix.to_string(),
            scrutinee: Some(value),
        });
    }

    // `case [first, |`: the slots of an array pattern bind names.
    if matches::in_array_pattern(head) {
        return Some(Context::Nothing);
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

    // The field name of a struct literal. The child sees a table the
    // emit passes to a constructor, so it lists the globals instead.
    if let Some((target, is_class)) = fields::struct_literal_target(src, offset - prefix.len()) {
        // `new Instance("Part") { |` fills a class, not a struct.
        if is_class {
            return Some(Context::InstanceField {
                prefix: prefix.to_string(),
                class: target,
            });
        }

        return Some(Context::StructField {
            prefix: prefix.to_string(),
            target,
        });
    }

    // The body of a declaration. The opener's own line counts: the
    // body starts at the `as`, so `struct S as |` takes a field, not
    // every name in scope.
    let opener = bodies::opens_a_body(head);
    let body = opener.or_else(|| bodies::enclosing_body(src, line_start));
    // The member column: the line holds nothing but the caret's word,
    // or the caret sits right after the `as` that opened the body.
    let at_body_column = head.trim().is_empty() || opener.is_some();

    match body {
        Some(Body::Struct) => {
            // `struct S<T: Sized> as |` carries a colon of its own; the
            // caret is still at the first field.
            if let Some(colon) = head.find(':').filter(|_| opener.is_none()) {
                // The `=` ends the annotation: what follows is a value,
                // and the child reads it.
                if head[colon + 1..].contains('=') {
                    return None;
                }

                return Some(Context::TypeSlot {
                    prefix: prefix.to_string(),
                    prefers: Prefers::Any,
                });
            }

            if at_body_column {
                // The `end` the author just typed closes the body, so
                // the body's words no longer belong on the line.
                if prefix == "end" {
                    return Some(Context::Nothing);
                }

                return Some(Context::FieldStart {
                    prefix: prefix.to_string(),
                });
            }

            return Some(Context::Nothing);
        }

        Some(Body::Enum) => {
            if bodies::in_enum_payload(src, line_start, head) {
                return Some(Context::EnumPayload {
                    prefix: prefix.to_string(),
                });
            }

            if at_body_column && prefix != "end" {
                return Some(Context::VariantStart {
                    prefix: prefix.to_string(),
                });
            }

            return Some(Context::Nothing);
        }

        Some(Body::Impl) if at_body_column => {
            if prefix == "end" {
                return Some(Context::Nothing);
            }

            return Some(Context::MemberStart {
                prefix: prefix.to_string(),
            });
        }

        Some(Body::Trait) if at_body_column => {
            if prefix == "end" {
                return Some(Context::Nothing);
            }

            return Some(Context::TraitMemberStart {
                prefix: prefix.to_string(),
            });
        }

        Some(Body::Impl | Body::Trait) => {}

        None => {}
    }

    // A type annotation, a return type, or a type argument. Every one
    // of them takes the same list, and the child mixes values into it.
    if types::takes_a_type(head) {
        return Some(Context::TypeSlot {
            prefix: prefix.to_string(),
            prefers: Prefers::Any,
        });
    }

    if let Some(interface) = bodies::declaration_head(head) {
        return Some(Context::DeclarationAs {
            prefix: prefix.to_string(),
            interface,
        });
    }

    if bodies::in_enum_payload(src, line_start, head) {
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
        && (trimmed.len() == 6 || !strings::is_word(trimmed.as_bytes()[6] as char))
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
                spec: imports::import_path(line),
            });
        }

        if let Some(open) = rest.find('{') {
            if rest[open..].contains('}') {
                return Some(Context::ImportFrom);
            }

            let inside = &rest[open + 1..];
            // The entry the caret sits in: `type` opens a type-only
            // name, so the list holds the module's types alone.
            let entry = inside.rsplit(',').next().unwrap_or(inside);
            let entry_type_only = entry.trim_start().starts_with("type ") || entry.trim() == "type";
            let type_only = type_only || entry_type_only;
            let after_name = !entry_type_only
                && entry
                    .trim_end()
                    .chars()
                    .last()
                    .is_some_and(strings::is_word)
                && entry.ends_with(' ')
                && prefix.is_empty();
            let spec = imports::import_path(line);

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

        // `import Name, |`: the braces follow the default binding.
        if rest.trim_end().ends_with(',') {
            return Some(Context::ImportBrace);
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
    use super::*;

    fn at(src: &str) -> Option<Context> {
        let offset = src.find('|').unwrap();
        detect(&src.replace('|', ""), offset)
    }

    /// Where an expression may start, and where it may not.
    #[test]
    fn an_expression_position_reads_its_head() {
        let starts = |src: &str| {
            let offset = src.find('|').unwrap();

            expression_start(&src.replace('|', ""), offset)
        };
        assert!(starts("local c = if a then |1 else 2"));
        assert!(starts("local c = if a then 1 else |2"));
        assert!(starts("local t = c ? |\"a\" : \"b\""));
        assert!(starts("local t = c ? \"a\" : |\"b\""));
        assert!(starts("            default |\"over\""));
        assert!(starts("    return |"));
        assert!(starts("print(|"));
        assert!(starts("f(a, |"));
        assert!(starts("local n = a + |"));
        // A statement, a member, an annotation, a comment, a string.
        assert!(!starts("    |"));
        assert!(!starts("local n = value.|"));
        assert!(!starts("local n: |"));
        assert!(!starts("obj:|"));
        assert!(!starts("-- the |"));
        assert!(!starts("local s = \"a |"));
    }

    /// The `:` of a ternary takes a value; an annotation's `:` takes a
    /// type.
    #[test]
    fn a_ternary_else_is_no_type_slot() {
        assert_eq!(at("local t = c ? \"a\" : |"), None);
        assert_eq!(
            at("local t: |"),
            Some(Context::TypeSlot {
                prefix: String::new(),
                prefers: Prefers::Any,
            })
        );
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
        assert_eq!(at("for k, v in pa|"), None);
        assert_eq!(at("remote Test(nam|"), Some(Context::Nothing));
        assert_eq!(at("macro twice(val|"), Some(Context::Nothing));
    }

    /// The variant column of an `enum` body: the name is the author's,
    /// and `end` closes the body.
    #[test]
    fn an_enum_body_offers_the_end_alone() {
        let start = |p: &str| {
            Some(Context::VariantStart {
                prefix: p.to_string(),
            })
        };
        assert_eq!(at("enum Kind as\n    Al|"), start("Al"));
        assert_eq!(at("enum Kind as\n    |"), start(""));
        assert_eq!(at("enum Kind as\n    end|"), Some(Context::Nothing));
        assert_eq!(
            at("enum Kind as\n    Move(num|"),
            Some(Context::EnumPayload {
                prefix: "num".to_string()
            })
        );
    }

    /// A struct field's default is a value: the annotation ends at the
    /// `=`, and the child reads what follows.
    #[test]
    fn a_field_default_is_a_value_not_a_type() {
        assert_eq!(at("struct Box as\n    scope: Scope = Scope.|"), None);
        assert_eq!(
            at("struct Box as\n    scope: Sco|"),
            Some(Context::TypeSlot {
                prefix: "Sco".to_string(),
                prefers: Prefers::Any,
            })
        );
    }

    /// `new` opens a constructor whatever bracket sits in front of it.
    #[test]
    fn a_bracket_before_new_keeps_the_word() {
        let target = |p: &str| {
            Some(Context::NewTarget {
                prefix: p.to_string(),
            })
        };
        assert_eq!(at("local s = a ?? (new |"), target(""));
        assert_eq!(at("local xs = [ new Poi|"), target("Poi"));
        assert_eq!(at("local s = new |"), target(""));
    }

    /// The class string of `new Instance("` and the child-name operator.
    #[test]
    fn a_class_string_lists_classes_and_a_child_name_lists_nothing() {
        assert_eq!(
            at("local p = new Instance(\"Pa|"),
            Some(Context::ClassName {
                prefix: "Pa".to_string()
            })
        );
        assert_eq!(
            at("local p = Instance.new(\"|"),
            Some(Context::ClassName {
                prefix: String::new()
            })
        );
        assert_eq!(at("local s = game:GetService(\"Pl|"), None);
        assert_eq!(at("local part = workspace=>Ma|"), Some(Context::Nothing));
    }

    /// `import M, { | }` takes the names of the module, so the comma
    /// asks for the brace, not for `from`.
    #[test]
    fn a_default_import_then_a_comma_opens_the_brace() {
        assert_eq!(at("import Lib, |"), Some(Context::ImportBrace));
        assert_eq!(at("import Lib |"), Some(Context::ImportFrom));
        assert!(matches!(
            at("import Lib, { ma|"),
            Some(Context::ImportNames { .. })
        ));
    }

    /// `new Instance("Part") { |` fills a class, and the class comes
    /// from the string the call takes.
    #[test]
    fn an_object_initialiser_reads_its_class() {
        let field = |class: &str, p: &str| {
            Some(Context::InstanceField {
                prefix: p.to_string(),
                class: class.to_string(),
            })
        };
        assert_eq!(
            at("local p = new Instance(\"Part\") {\n    An|"),
            field("Part", "An")
        );
        assert_eq!(
            at("local p = new Instance(\"Part\") {\n    Name = \"a\",\n    |"),
            field("Part", "")
        );
        assert_eq!(
            at("local s = new Stats {\n    heal|"),
            Some(Context::StructField {
                prefix: "heal".to_string(),
                target: "Stats".to_string()
            })
        );
        assert_eq!(
            instance_class("new Instance(\"Part\")"),
            Some("Part".to_string())
        );
        assert_eq!(
            instance_class("Instance.new(\"TextLabel\")"),
            Some("TextLabel".to_string())
        );
        assert_eq!(instance_class("f(\"Part\")"), None);
    }

    /// `$matches(v, |Busy(_))` takes a pattern; the value it tests is
    /// the first argument.
    #[test]
    fn a_matches_call_takes_a_pattern() {
        let scrutinee = |src: &str| match at(src) {
            Some(Context::MatchCase { scrutinee, .. }) => scrutinee,

            other => panic!("{other:?}"),
        };
        assert_eq!(
            scrutinee("local b = $matches(state, |"),
            Some("state".to_string())
        );
        assert_eq!(
            scrutinee("local b = $matches(self.phase, Lob|"),
            Some("self.phase".to_string())
        );
        assert_eq!(at("local b = $matches(sta|"), None);
    }

    /// A destructuring `local` names the fields of the value alone.
    #[test]
    fn a_destructure_takes_the_values_fields() {
        let at_end = |src: &str| in_destructure(src, src.len());
        assert!(at_end("local { na"));
        assert!(at_end("    const { name, hp"));
        assert!(!at_end("local { name } = player"));
        assert!(!at_end("local t = { na"));
    }

    #[test]
    fn a_body_closed_on_its_own_line_opens_nothing_below() {
        assert_eq!(at("struct Test as end\n|"), None);
        assert_eq!(at("impl Test end\n|"), None);
        assert_eq!(at("struct Test as x: number end\nlocal a = |"), None);
        assert_eq!(
            at("struct Test as\n    |"),
            Some(Context::FieldStart {
                prefix: String::new()
            })
        );
    }

    /// `case [|`: an array pattern's slots bind names of the author's
    /// own, so the arm takes no list there.
    #[test]
    fn an_array_pattern_takes_no_list() {
        assert_eq!(at("    case [|"), Some(Context::Nothing));
        assert_eq!(at("    case [first, |"), Some(Context::Nothing));
        assert_eq!(at("    case [first, ...re|"), Some(Context::Nothing));

        // A closed bracket ends the pattern, and `case` with no bracket
        // still lists the variants.
        assert_ne!(at("    case [a] |"), Some(Context::Nothing));
        assert!(matches!(at("    case |"), Some(Context::MatchCase { .. })));
    }

    /// `struct S as |`: the body starts at the `as`, so the caret on
    /// the opener's line takes the body's words. It drew every name in
    /// scope before.
    #[test]
    fn the_opener_line_is_the_body_column() {
        assert_eq!(
            at("struct S as |"),
            Some(Context::FieldStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("export interface I as |"),
            Some(Context::FieldStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("enum E as |"),
            Some(Context::VariantStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("impl S as |"),
            Some(Context::MemberStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("trait T as |"),
            Some(Context::TraitMemberStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("struct Pair<A: Sized> as |"),
            Some(Context::FieldStart {
                prefix: String::new()
            })
        );

        // A one-liner closes the body, and a type slot still names a
        // type: neither is the member column.
        assert_eq!(at("struct S as end |"), None);
        assert_eq!(
            at("impl Drawable for |"),
            Some(Context::TypeSlot {
                prefix: String::new(),
                prefers: Prefers::Concrete
            })
        );
    }

    #[test]
    fn type_slots_and_bodies_have_their_own_lists() {
        let ty = |p: &str| {
            Some(Context::TypeSlot {
                prefix: p.to_string(),
                prefers: Prefers::Any,
            })
        };
        assert_eq!(at("type Alias = |"), ty(""));
        assert_eq!(at("local v = t satisfies Ha|"), ty("Ha"));
        assert_eq!(at("if key is |"), ty(""));
        assert_eq!(at("if key is not |"), ty(""));
        assert_eq!(at("struct Box as\n    inner: |"), ty(""));

        // The four slots share one list and rank it differently: a
        // contract for `extends` and for the trait of an `impl`, a
        // struct or an enum for the type the methods go on.
        let ranked = |p: &str, prefers: Prefers| {
            Some(Context::TypeSlot {
                prefix: p.to_string(),
                prefers,
            })
        };
        assert_eq!(
            at("interface Both extends |"),
            ranked("", Prefers::Contract)
        );
        assert_eq!(at("impl Dr|"), ranked("Dr", Prefers::Concrete));
        assert_eq!(at("impl |Drawable for Pt"), ranked("", Prefers::Contract));
        assert_eq!(at("impl Drawable for |"), ranked("", Prefers::Concrete));
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
            at("impl Box as\n    |"),
            Some(Context::MemberStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("impl Box as\n    function f(self)\n        local x = |"),
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

    /// Every slot that takes a type reads as one list.
    #[test]
    fn every_type_slot_reads_as_one() {
        let ty = |p: &str| {
            Some(Context::TypeSlot {
                prefix: p.to_string(),
                prefers: Prefers::Any,
            })
        };
        assert_eq!(at("local function f(a: |"), ty(""));
        assert_eq!(at("local function f(a: number): |"), ty(""));
        assert_eq!(at("export local function f() -> Res|"), ty("Res"));
        assert_eq!(at("local v: Result<|"), ty(""));
        assert_eq!(at("local v: HashMap<string, |"), ty(""));
        assert_eq!(at("local function k<T: |"), ty(""));
        assert_eq!(at("remote Damage(target: |"), ty(""));

        // A comparison is no type argument, and a cast is the child's.
        assert_eq!(at("if a < |"), None);
        assert_eq!(at("local v = x :: |"), None);
    }

    /// The fields of a struct literal, and the struct the caret fills.
    #[test]
    fn a_struct_literal_lists_the_fields_of_its_struct() {
        let field = |p: &str, t: &str| {
            Some(Context::StructField {
                prefix: p.to_string(),
                target: t.to_string(),
            })
        };
        assert_eq!(at("local s = new Stats { |"), field("", "Stats"));
        assert_eq!(at("local s = new Stats { heal|"), field("heal", "Stats"));
        assert_eq!(
            at("local s = new Stats { health = 1, |"),
            field("", "Stats")
        );
        assert_eq!(at("local l: Loadout = { |"), field("", "Loadout"));

        // Past the `=` the value is an expression, and a plain table
        // names no struct.
        assert_eq!(at("local s = new Stats { health = |"), None);
        assert_eq!(at("local t = { |"), None);
    }

    /// A trait body takes no visibility word.
    #[test]
    fn a_trait_body_and_a_closed_body_have_their_own_answers() {
        assert_eq!(
            at("trait Keyed as\n    |"),
            Some(Context::TraitMemberStart {
                prefix: String::new()
            })
        );
        // The `end` just typed closes the body; nothing follows it.
        assert_eq!(
            at("struct Box as\n    x: number\nend|"),
            Some(Context::Nothing)
        );
        assert_eq!(at("impl Box as\nend|"), Some(Context::Nothing));
    }

    /// An empty body on one line closes where it opens: the line after
    /// `impl T as end` is ordinary code, not the member column.
    #[test]
    fn a_one_line_body_is_closed() {
        for head in [
            "struct T as end",
            "enum E as end",
            "interface I as end",
            "trait U as end",
            "impl T as end",
            "impl U for T as end",
        ] {
            assert_eq!(at(&format!("{head}\nlocal x = |")), None, "{head}");
            assert_eq!(at(&format!("{head}\n|")), None, "{head}");
        }

        // The body a header opens is still open on the next line.
        assert_eq!(
            at("impl T as\n    |"),
            Some(Context::MemberStart {
                prefix: String::new()
            })
        );
    }

    #[test]
    fn a_string_is_no_place_for_a_name() {
        let src = "local m = map:get(\"rare\")\n";
        assert!(in_string(src, src.find("rare").unwrap() + 2));
        assert!(!in_string(src, src.find("map").unwrap() + 1));
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
        // Past the `as` the body begins, and the body's own words are
        // what the caret takes.
        assert_eq!(
            at("enum Test as |"),
            Some(Context::VariantStart {
                prefix: String::new()
            })
        );
        assert_eq!(at("enum |"), None);
    }

    /// `impl` and `trait` close their header with `as` too.
    #[test]
    fn an_impl_and_a_trait_header_want_as() {
        for head in [
            "impl Test |",
            "impl Box<T> |",
            "impl Shape for Test |",
            "impl Shape for Test<T> |",
            "trait Shape |",
            "export impl Shape for Test |",
        ] {
            assert_eq!(
                at(head),
                Some(Context::DeclarationAs {
                    prefix: String::new(),
                    interface: false
                }),
                "{head}"
            );
        }

        assert_eq!(
            at("impl Test a|"),
            Some(Context::DeclarationAs {
                prefix: "a".to_string(),
                interface: false
            })
        );
        assert_eq!(
            at("impl Test as |"),
            Some(Context::MemberStart {
                prefix: String::new()
            })
        );
        assert_eq!(
            at("trait Shape as |"),
            Some(Context::TraitMemberStart {
                prefix: String::new()
            })
        );
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
        assert_eq!(at("remote Test(|"), Some(Context::Nothing));
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

    /// A comment between the attribute and its declaration is no
    /// declaration. Both comment forms sit there, and a block comment
    /// runs over as many lines as it takes.
    #[test]
    fn a_comment_does_not_hide_the_attribute_target() {
        assert_eq!(
            target_of("@|\n-- why\nremote Ping() from server\n"),
            Some("remote")
        );
        assert_eq!(
            target_of("@|\n--[[ why\n   it is here ]]\nremote Ping() from server\n"),
            Some("remote")
        );
        assert_eq!(
            target_of("@|\n--[==[ why ]==]\nstruct V as\nend\n"),
            Some("struct")
        );
        assert_eq!(
            target_of("@|\n--[[ why ]] function go() end\n"),
            Some("function")
        );
        // A binding takes `@cfg`, which no other target does.
        assert_eq!(target_of("@|\nlocal count = 1\n"), Some("local"));
        assert_eq!(target_of("@|\nexport const CAP = 10\n"), Some("local"));
        assert_eq!(target_of("@|\ntype Handler = () -> ()\n"), Some("type"));
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
        assert_eq!(
            at("attribute icon(asset: |"),
            Some(Context::TypeSlot {
                prefix: String::new(),
                prefers: Prefers::Any,
            })
        );
    }

    #[test]
    fn imports() {
        assert_eq!(
            at("import |"),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: false,
                spec: None
            })
        );
        assert_eq!(
            at("import type |"),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: true,
                spec: None
            })
        );
        // The path is already written: the default of that module is
        // what belongs before `from`.
        assert_eq!(
            at("import | from \"./m\""),
            Some(Context::ImportHead {
                prefix: String::new(),
                type_only: false,
                spec: Some("./m".to_string())
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
