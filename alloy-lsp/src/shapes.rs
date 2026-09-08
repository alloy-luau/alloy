//! The checker prints a struct value as the table it is at runtime,
//! `t2 where t1 = { __index: t1, ... } ; t2 = { @metatable t1, { x: number } }`,
//! an array as its method table, and a unit enum as a union of strings.
//! A reader wrote `Vec2`, `number[]`, and `Color`; this folds the print
//! back to those names, from the shapes the workspace declares.

use alloy::declarations::Shape;
use serde_json::Value;

/// The shapes a fold may name, over every open document.
#[derive(Default)]
pub struct Known {
    pub shapes: Vec<Shape>,
    pub interfaces: Vec<Interface>,
}

/// An interface a source declares: the interfaces it extends and the
/// fields it adds. The checker prints the two met with `&`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub bases: Vec<String>,
    pub fields: Vec<String>,
    /// Whether a `type` alias over a record declared it, and not an
    /// `interface` block. The alias marks no field `read` or `write`,
    /// so a printed record that marks one is another type.
    pub alias: bool,
}

/// The interfaces a source declares, with their bases and fields, and
/// the `type` aliases over a record. Both print as the table they hold,
/// and both have a name the source wrote.
pub fn interfaces(source: &str) -> Vec<Interface> {
    let mut out = declared_interfaces(source);
    out.extend(record_aliases(source));

    out
}

/// `export type Profile = { name: string, level: number }`: the alias
/// with the keys of its record.
fn record_aliases(source: &str) -> Vec<Interface> {
    let mut out = Vec::new();
    let mut rest = source;

    while let Some(i) = rest.find("type ") {
        let head = &rest[i..];
        // The word opens a declaration when nothing but `export` or
        // `local` stands before it on its line. Trimming the text back
        // to the previous line's last byte reads a `type` in the middle
        // of one as a declaration.
        let raw = &rest[..i];
        let lead = raw[raw.rfind('\n').map_or(0, |k| k + 1)..].trim();
        let opens = matches!(lead, "" | "export" | "local");
        rest = &rest[i + "type ".len()..];

        if !opens {
            continue;
        }

        let after = &head["type ".len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name.is_empty() {
            continue;
        }

        let Some(body) = after[name.len()..].trim_start().strip_prefix("= ") else {
            continue;
        };
        let body = body.trim_start();

        if !body.starts_with('{') {
            continue;
        }

        let Some(len) = group_len(body, '{', '}') else {
            continue;
        };
        let fields: Vec<String> = members(&body[..len])
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .collect();

        if !fields.is_empty() {
            out.push(Interface {
                name,
                bases: Vec::new(),
                fields,
                alias: true,
            });
        }
    }

    out
}

fn declared_interfaces(source: &str) -> Vec<Interface> {
    let mut out: Vec<Interface> = Vec::new();
    let mut open: Option<Interface> = None;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text
            .strip_prefix("interface ")
            .or_else(|| text.strip_prefix("export interface "))
        {
            let head = rest.trim_end().trim_end_matches(" as").trim();
            let (name, bases) = match head.split_once(" extends ") {
                Some((n, b)) => (
                    n.trim(),
                    b.split(',').map(|p| p.trim().to_string()).collect(),
                ),

                None => (head, Vec::new()),
            };

            if let Some(previous) = open.take() {
                out.push(previous);
            }

            open = Some(Interface {
                name: name.to_string(),
                bases,
                fields: Vec::new(),
                alias: false,
            });

            continue;
        }

        let Some(current) = open.as_mut() else {
            continue;
        };

        if text == "end" {
            out.push(open.take().expect("open interface"));

            continue;
        }

        let head = text
            .trim_start_matches("read ")
            .trim_start_matches("write ");

        if let Some((name, _)) = head.split_once(':')
            && !name.trim().is_empty()
            && name.trim().chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            current.fields.push(name.trim().to_string());
        }
    }

    if let Some(previous) = open {
        out.push(previous);
    }

    out
}

impl Interface {
    /// Whether a printed type is this interface: the bases it extends,
    /// met with a table of the fields it adds, or one table holding
    /// every field, the bases' included.
    fn matches(&self, text: &str, all: &[Interface]) -> bool {
        let mut bases: Vec<String> = Vec::new();
        let mut keys: Vec<String> = Vec::new();

        // `Readonly<Ent>` prints `{ read id: number, read name: string }`.
        // The alias `Ent` marks no field, so the mapped form is not it,
        // and `fold_aliases` names it after this fold declines.
        if self.alias && marks_a_member(text) {
            return false;
        }

        for part in split_intersection(text) {
            match part.starts_with('{') {
                true => keys.extend(members(part).into_iter().map(|(k, _)| k)),

                // The print qualifies a base with the module it came
                // from, `Core.Named` or `_m1.Named`; the name is the
                // tail, and the source wrote that alone.
                false => bases.push(part.rsplit('.').next().unwrap_or(part).to_string()),
            }
        }

        let mut wanted: Vec<String> = self.bases.clone();
        wanted.sort();
        bases.sort();
        keys.sort();

        if self.name.is_empty() || keys.is_empty() {
            return false;
        }

        let mut own = self.fields.clone();
        own.sort();

        if bases == wanted && keys == own {
            return true;
        }

        let mut every = self.inherited(all);
        every.sort();

        bases.is_empty() && keys == every
    }

    /// Every field the interface carries: the bases' fields and its own.
    fn inherited(&self, all: &[Interface]) -> Vec<String> {
        let mut out: Vec<String> = self
            .bases
            .iter()
            .filter_map(|b| all.iter().find(|i| i.name == *b))
            .flat_map(|i| i.inherited(all))
            .chain(self.fields.iter().cloned())
            .collect();
        out.sort();
        out.dedup();

        out
    }
}

/// Whether a printed record marks a member `read` or `write`.
fn marks_a_member(text: &str) -> bool {
    split_intersection(text)
        .into_iter()
        .filter(|p| p.starts_with('{'))
        .flat_map(|p| member_parts(p))
        .any(|m| {
            let text = m.trim();

            text.starts_with("read ") || text.starts_with("write ")
        })
}

/// The members of an intersection at depth zero.
fn split_intersection(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in text.char_indices() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            '&' if depth == 0 && text[..k].ends_with(' ') && text[k + 1..].starts_with(' ') => {
                out.push(text[start..k].trim());
                start = k + 1;
            }
            _ => {}
        }
    }

    out.push(text[start..].trim());

    out
}

/// Folds every string of a JSON value, in place.
pub fn fold_value(value: &mut Value, known: &Known) {
    match value {
        Value::String(s) => {
            if s.contains(" & ")
                || s.contains(" where ")
                || s.contains("tag: \"")
                || s.contains("\" | \"")
                || s.contains("__private")
                || s.contains("Array<")
                || s.contains("{read ")
                || s.contains("Awaitable<")
                || s.contains("ResultMethods")
                || s.contains(" | ")
                || s.contains("intersect<")
                || s.contains("@metatable")
                || s.contains('~')
                || s.contains("ResultOk")
                || s.contains("ResultErr")
                || s.contains("Result2<")
            {
                *s = fold(s, known);
            }
        }

        Value::Array(items) => items.iter_mut().for_each(|i| fold_value(i, known)),

        Value::Object(map) => map.values_mut().for_each(|v| fold_value(v, known)),

        _ => {}
    }
}

/// Whether a message names something the reader never wrote, so it says
/// nothing they can act on.
///
/// `%error-id%` is the checker's stand-in for a name a half-typed member
/// access has yet to give. The parser already reports the missing name.
pub fn names_only_the_emit(message: &str) -> bool {
    // The checker names the two emitted files of an import cycle;
    // `circular_import` names the two the author wrote.
    // The require binding as the subject of a report: the reader never
    // wrote the name, and the mistake reads on the annotation instead.
    message.contains("%error-id%")
        || message.contains("Cyclic module dependency")
        || message.contains("'__alloy'")
}

/// Whether a report is about a key the emit writes and the source line
/// does not: an enum's `tag`, and the `_1`, `_2` its payload goes in.
/// A reader who never wrote the name has nothing to fix.
pub fn names_the_emit_key(message: &str, line: &str) -> bool {
    const OPENERS: [&str; 3] = ["does not have key '", "Key '", "Cannot add property '"];

    OPENERS
        .iter()
        .filter_map(|opener| {
            let at = message.find(opener)? + opener.len();

            message[at..].find('\'').map(|end| &message[at..at + end])
        })
        .any(|key| {
            let emitted = key == "tag"
                || key
                    .strip_prefix('_')
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));

            emitted && !holds_word(line, key)
        })
}

/// Whether a duplicate-field report is about a table the emit built:
/// the source line writes the key once, or not at all. A markup
/// attribute that expands to several properties makes these.
pub fn duplicate_only_in_the_emit(message: &str, line: &str) -> bool {
    const OPENER: &str = "Table field '";

    let Some(at) = message.find(OPENER).map(|i| i + OPENER.len()) else {
        return false;
    };
    let Some(end) = message[at..].find('\'') else {
        return false;
    };

    if !message.contains("is a duplicate") {
        return false;
    }

    let key = &message[at..at + end];

    line.match_indices(key)
        .filter(|(at, _)| {
            !line[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                && !line[at + key.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        })
        .count()
        < 2
}

/// Whether a line holds a name as a whole word.
fn holds_word(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(at, _)| {
        !line[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_')
            && !line[at + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
    })
}

/// A `{ ... }` where an Array belongs, as the mistake reads. The checker
/// answers with the nineteen methods the table lacks; the reader wrote
/// the wrong bracket.
pub fn plain_table_hint(message: &str) -> Option<String> {
    const HEAD: &str = "Table type '";
    const MIDDLE: &str = "' not compatible with type '";

    if !message.contains("missing fields") {
        return None;
    }

    let at = message.find(HEAD)? + HEAD.len();
    let mid = message[at..].find(MIDDLE)? + at;
    let after = mid + MIDDLE.len();
    let end = message[after..].find('\'')? + after;
    let want = &message[after..end];
    let named = want.trim_end_matches('?');

    if !(named.ends_with("[]") || named.starts_with("Array<")) {
        return None;
    }

    Some(format!(
        "a `{{ ... }}` is a plain table, not a `{want}`; an Array literal is `[ ... ]`"
    ))
}

/// A checker message as a reader should get it: a failed bound reads as
/// a bound, and the tail that walks the emitted shape goes.
pub fn friendly_text(message: &str) -> String {
    let text = bound_failure(message)
        .or_else(|| pack_mismatch(message))
        .or_else(|| unsolved_generic(message))
        .or_else(|| solver_gave_up(message))
        .unwrap_or_else(|| cut_explanation(message));

    table_beside_array(&text).unwrap_or(text)
}

/// The checker's own step limit, worded as an order to the reader. It
/// says nothing is wrong with the code, only that the checker stopped.
fn solver_gave_up(message: &str) -> Option<String> {
    const CLAUSE: &str = "Code is too complex to typecheck!";

    let at = message.find(CLAUSE)?;

    Some(format!(
        "{}the checker reached its limit on this expression; it says nothing about the code. Name a step in a local, or annotate the result",
        &message[..at]
    ))
}

/// A generic the checker could not solve. It answers with the bounds it
/// collected, which name no place and no fix; the reader wants to know
/// that the values do not agree.
fn unsolved_generic(message: &str) -> Option<String> {
    const CLAUSE: &str = "No valid instantiation could be inferred for generic type parameter ";

    let at = message.find(CLAUSE)? + CLAUSE.len();
    let name: String = message[at..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let head = match message.split_once(": ") {
        Some((kind, _)) if !kind.contains(' ') => format!("{kind}: "),

        _ => String::new(),
    };

    (!name.is_empty()).then(|| {
        format!(
            "{head}these values give `{name}` no one type; make them agree, or write `{name}` out"
        )
    })
}

/// `{T}` is a plain Luau table and `T[]` is an Array with its methods.
/// A message that holds both reads as one type printed two ways, so it
/// says which is which.
pub fn table_beside_array(message: &str) -> Option<String> {
    let name = message.match_indices('{').find_map(|(at, _)| {
        let rest = message.get(at + 1..)?.trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name.is_empty() || !rest[name.len()..].trim_start().starts_with('}') {
            return None;
        }

        message.contains(&format!("{name}[]")).then_some(name)
    })?;

    Some(format!(
        "{message}; a `{{ {name} }}` is a plain table, and `{name}[]` is an Array"
    ))
}

/// A callback whose parameters do not line up. The checker explains it
/// by walking the type pack, which reads as a broken sentence and calls
/// the two types "former" and "latter". The parameter, what the source
/// wrote, and what the callee asks for say it.
fn pack_mismatch(message: &str) -> Option<String> {
    const CLAUSE: &str = "entry in the type pack is ";

    let flat = flatten(message);
    let at = flat.find(CLAUSE)?;
    let ordinal = flat[..at].split_whitespace().next_back()?.to_string();

    if !ordinal.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }

    let rest = &flat[at + CLAUSE.len()..];
    let first = quoted_from(rest)?;
    let after = rest.get(first.len() + 2..)?;
    let second = quoted_from(after.split_once("type and ")?.1)?;
    // "the latter type" is the type the source wrote; "the former" is
    // the one the callee asks for.
    let (got, want) = match after.trim_start().starts_with("in the latter") {
        true => (first, second),

        false => (second, first),
    };
    let head = flat[..at].split_once("; it ").map(|(h, _)| h)?.trim_end();

    // A pack of parameters belongs to a function on both sides; any
    // other pack keeps the head alone.
    if head.matches("->").count() < 2 {
        return Some(head.to_string());
    }

    Some(format!(
        "{head}: its {ordinal} parameter is `{got}` where `{want}` is wanted"
    ))
}

/// One line of a message the checker laid out over several, with its
/// tabs and its runs of spaces closed up.
fn flatten(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `where T: Shape` emits as an intersection, so a bound the argument
/// misses reads as an intersection it is not part of.
fn bound_failure(message: &str) -> Option<String> {
    const CLAUSE: &str = "component of the intersection is ";

    let at = message.find(CLAUSE)? + CLAUSE.len();
    let bound = quoted_from(&message[at..])?;
    let got_at = message.find("but got ")? + "but got ".len();
    let got = quoted_from(&message[got_at..])?;
    // The message keeps the kind it came with; a caller that adds one
    // would print it twice.
    let head = match message.split_once(": ") {
        Some((kind, _)) if !kind.contains(' ') => format!("{kind}: "),

        _ => String::new(),
    };

    Some(format!(
        "{head}`{got}` does not satisfy the bound `{bound}`"
    ))
}

/// The text the quote at the start of a message fragment opens; the
/// checker writes either a quote or a backtick.
fn quoted_from(text: &str) -> Option<&str> {
    let quote = text.chars().next().filter(|c| matches!(c, '\'' | '`'))?;
    let end = text[1..].find(quote)?;

    Some(&text[1..1 + end])
}

/// The checker explains a mismatch by walking the shape it printed, so
/// the tail names the emit: `_1`, `__index`, and the type pack. The
/// head already says what the reader needs.
fn cut_explanation(message: &str) -> String {
    const MARKERS: [&str; 2] = ["this is because", "in the metatable portion"];

    let Some(at) = MARKERS.iter().filter_map(|m| message.find(m)).min() else {
        return message.to_string();
    };
    let head = message[..at].trim_end();

    head.strip_suffix(';')
        .unwrap_or(head)
        .trim_end()
        .to_string()
}

/// One `tN = { ... }` binding of a `where` clause.
struct Binding {
    var: String,
    body: String,
}

/// Folds one text: every `where` clause whose bindings all name known
/// shapes goes, and the head reads by name; then the enum unions and
/// the Result unions.
pub fn fold(text: &str, known: &Known) -> String {
    // `Table type 'X' not compatible with type 'Y'` contrasts a value
    // the reader wrote in place against the type it is going into. A
    // `type` alias of the same shape, declared in another module, is
    // no name they wrote, so the record stays a record here.
    let narrowed;
    let known = match text.contains("not compatible with type") {
        true => {
            narrowed = Known {
                shapes: known.shapes.clone(),
                interfaces: known
                    .interfaces
                    .iter()
                    .filter(|i| !i.alias)
                    .cloned()
                    .collect(),
            };

            &narrowed
        }

        false => known,
    };
    // The std spells the operand of `await` `Awaitable<T>`; the source
    // writes `Future<T>`, and the two are one type.
    let mut out = text
        .replace("Awaitable<", "Future<")
        // A method hovered on a Future names the alias `await` takes;
        // the reader wrote `Future`.
        .replace("function Awaitable:", "function Future:")
        .replace("function Awaitable.", "function Future.");

    // A hover may hold several types, one per line; each `where` is
    // handled in turn, from the last so the offsets before it hold.
    while let Some(i) = out.rfind(" where ") {
        let (head_start, head) = head_of(&out, i);
        let tail_start = i + " where ".len();
        let (bindings, tail_end) = parse_bindings(&out[tail_start..]);

        if bindings.is_empty() {
            break;
        }

        let resolved = resolve(&bindings, known);
        let mut new_head = head.to_string();
        let mut all = true;

        for b in &bindings {
            if !mentions(&new_head, &b.var) {
                continue;
            }

            match resolved.iter().find(|(v, _)| *v == b.var) {
                Some((_, name)) => new_head = replace_var(&new_head, &b.var, name),

                None => all = false,
            }
        }

        if !all {
            // The head keeps the clause; the bindings it needs still
            // read by name where they can.
            let mut clause = String::new();

            for (k, b) in bindings.iter().enumerate() {
                if k > 0 {
                    clause.push_str(" ; ");
                }

                let mut body = b.body.clone();

                for (v, name) in &resolved {
                    body = replace_var(&body, v, name);
                }

                clause.push_str(&format!("{} = {body}", b.var));
            }

            out.replace_range(
                head_start..tail_start + tail_end,
                &format!("{new_head} where {clause}"),
            );

            break;
        }

        // Every name the head uses resolved: the rest of the clause,
        // parsed or not, is for those names alone. A fenced hover holds
        // nothing after it, so the cut runs to the fence. A diagnostic
        // carries the sentence on after the clause, a closing quote
        // among it, so there the cut stops where the bindings end.
        let parsed_end = tail_start + tail_end;
        let clause_end = out[tail_start..]
            .find("\n```")
            .map_or(parsed_end, |k| (tail_start + k).max(parsed_end));
        out.replace_range(head_start..clause_end, &new_head);
    }

    fold_heads(&mut out, known);
    fold_metatable_groups(&mut out, known);
    fold_tagged_results(&mut out);
    fold_results(&mut out);
    fold_lite_results(&mut out);
    // Whatever the Result folds could not pair keeps its data half; the
    // method table is the same for every Result and names nothing.
    out = drop_result_methods(&out);
    fold_inline_result_methods(&mut out);
    fold_results(&mut out);
    fold_symbols(&mut out);

    // An enum inside another's payload folds first, and then the outer.
    for _ in 0..3 {
        let before = out.len();
        fold_enums(&mut out, known);

        if out.len() == before {
            break;
        }
    }
    fold_variant_tables(&mut out, known);
    fold_enum_unions(&mut out, known);
    out = fold_private_views(&out);
    fold_full_views(&mut out, known);
    fold_interfaces(&mut out, known);
    fold_name_parens(&mut out);
    fold_signalish(&mut out);
    fold_array_alias(&mut out);
    fold_narrowed_primitives(&mut out);
    fold_cut_array(&mut out);
    fold_cut_results(&mut out);
    fold_result_aliases(&mut out);
    fold_refinements(&mut out);
    fold_negated_members(&mut out);
    out = fold_temp_receiver(&out);
    fold_aliases(&mut out, known);
    // An async body that returns nothing types as `Future<nil>`, since
    // `()` is no type argument; it reads as `Future<()>`.
    out = out.replace("Future<nil>", "Future<()>");
    fold_union_dupes(&mut out);
    fold_array_parens(&mut out);
    // `Array<number[] | number[]>` is one array once the union folds.
    fold_array_alias(&mut out);
    fold_read_arrays(&mut out);
    fold_deletable(&mut out);
    fold_iter_shapes(&mut out);
    out = fold_call_receivers(&out);
    fold_quoted_types(&mut out, known);

    out
}

/// An `Iter` two maps deep prints as its shape, `{ next: (self: any) ->
/// T? }`, since the std spells it under a second name. It reads as
/// `Iter<T>`.
fn fold_iter_shapes(text: &mut String) {
    const HEAD: &str = "{ next: (self: any) -> ";
    let mut from = 0;

    while let Some(i) = text[from..].find(HEAD) {
        let at = from + i;
        let Some(len) = group_len(&text[at..], '{', '}') else {
            break;
        };
        let inner = &text[at + HEAD.len()..at + len - 1];
        let Some(element) = inner.trim_end().strip_suffix('?') else {
            from = at + 1;

            continue;
        };
        // The shape holds `next` alone when the child cut the print; a
        // second member means another table with a `next` field.
        if element.contains(',') || element.contains('(') {
            from = at + 1;

            continue;
        }

        let name = format!("Iter<{}>", element.trim());
        text.replace_range(at..at + len, &name);
        from = at + name.len();
    }
}

/// A method hovered at the end of a call chain names the chain,
/// `function Iter.from(xs):map(f):collect(self: Iter<number>)`. The
/// receiver's type stands in for the chain. A chain wraps, so the head
/// runs over as many lines as the source did; the whole of it goes.
fn fold_call_receivers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;

    while let Some(i) = text[from..].find(SIGNATURE) {
        let at = from + i;
        let head = at + SIGNATURE.len();

        // A signature opens a line; `-> function` names a type instead.
        match (at == 0 || text[..at].ends_with('\n'))
            .then(|| fold_call_receiver_at(&text[at..]))
            .flatten()
        {
            Some((len, folded)) => {
                out.push_str(&text[from..at]);
                out.push_str(&folded);
                from = at + len;
            }

            None => {
                out.push_str(&text[from..head]);
                from = head;
            }
        }
    }

    out.push_str(&text[from..]);

    out
}

const SIGNATURE: &str = "function ";

/// The head `function <receiver>:<name>(self: T` at the start of the
/// text: how far it runs, and the same head with the receiver replaced
/// by the name of `T`. `None` when the receiver is already a name, or
/// when the self type has none.
fn fold_call_receiver_at(text: &str) -> Option<(usize, String)> {
    // The head stops at the fence or the blank line that closes the
    // signature; a bracket left open past either is not part of it.
    let limit = [text.find("\n```"), text.find("\n\n")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text.len());
    let span = &text[..limit];
    let mut depth = 0i32;
    let mut colon = None;
    let mut open = None;

    for (k, c) in span[SIGNATURE.len()..].char_indices() {
        let k = k + SIGNATURE.len();

        match c {
            '(' if depth == 0 && span[k + 1..].starts_with("self: ") => {
                open = Some(k);

                break;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 => colon = Some(k),
            _ => {}
        }
    }

    let open = open?;
    let colon = colon?;
    let receiver = &span[SIGNATURE.len()..colon];
    let after = &span[open + "(self: ".len()..];
    let name_len = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .count();

    if name_len == 0 {
        return None;
    }

    let name = &after[..name_len];

    // A receiver that is a name already, `Iter:filter`, is the type.
    // `self:markup` and `x:describe` name the variable the call went
    // through; the type is what the reader wants there, and only a
    // type name stands in for it. `read number[]` and `any` are not
    // names, and `self: read T[]` would read as `read`.
    if !receiver.contains('(') && !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }

    Some((open, format!("{SIGNATURE}{name}{}", &span[colon..open])))
}

/// A message prints a type between quotes, `not found in table '{ ... }'`.
/// It reads by name there the way a hover does.
fn fold_quoted_types(text: &mut String, known: &Known) {
    for quote in ['\''] {
        let mut from = 0;

        while let Some(i) = text[from..].find(quote) {
            let open = from + i + 1;
            // The child cuts a long print, so the closing quote may be
            // gone; what is left still names the shape.
            let cut = text[open..].find(quote).is_none();
            let close = text[open..].find(quote).unwrap_or(text.len() - open);
            let body = text[open..open + close].to_string();

            if cut && !body.trim_start().starts_with('{') {
                break;
            }

            let folded = { dedupe_type(&body) };

            if folded != body {
                text.replace_range(open..open + close, &folded);
                from = open + folded.len() + 1;

                continue;
            }

            match name_of_body(&body, known) {
                Some(name) if name != body => {
                    let name = match cut {
                        true => format!("{name}{quote}"),

                        false => name,
                    };
                    text.replace_range(open..open + close, &name);
                    from = open + name.len() + 1;
                }

                _ => from = open + close + 1,
            }
        }
    }
}

/// `delete` takes a value with a `Destroy`, a `Disconnect`, or their
/// lower-case pair. The union of the four reads as the name the doc
/// gives it.
fn fold_deletable(text: &mut String) {
    const UNION: &str = "{ read Destroy: (any) -> () } | { read Disconnect: (any) -> () } | { read destroy: (any) -> () } | { read disconnect: (any) -> () }";

    while let Some(at) = text.find(UNION) {
        text.replace_range(at..at + UNION.len(), "Deletable");
    }
}

/// `(number[])[]`, a union that folded to one member under an array,
/// reads as `number[][]`.
fn fold_array_parens(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find(")[]") {
        let close = from + i;
        let Some(open) = enclosing_open(text, close) else {
            from = close + 3;
            continue;
        };
        let inner = &text[open + 1..close];

        if inner.contains(" | ")
            || inner.contains(" & ")
            || inner.contains("->")
            || inner.contains(' ')
        {
            from = close + 3;
            continue;
        }

        text.replace_range(close..close + 1, "");
        text.replace_range(open..open + 1, "");
        from = close - 1;
    }
}

/// An Array method's receiver, `{ read [number]: T }`, prints as
/// `{read T}`; the sugar is `read T[]`.
fn fold_read_arrays(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find("{read ") {
        let at = from + i;
        let Some(len) = balanced_len(&text[at..]) else {
            break;
        };
        let inner = text[at + 6..at + len - 1].trim().to_string();

        // One element type, `Result<Profile, any>` included; a space
        // outside the type arguments means the group holds more.
        if inner.contains('{') || outside_angles(&inner, ' ') {
            from = at + 6;
            continue;
        }

        text.replace_range(at..at + len, &format!("read {inner}[]"));
        from = at;
    }
}

/// `T | D` of an `unwrap_or` prints twice when both are the same:
/// `number | number` reads as `number`, and two instantiations of one
/// alias print as two members, `Array<number> | Array<number>`. Each
/// type after a `: ` on a line loses its repeated members, at any
/// depth.
fn fold_union_dupes(text: &mut String) {
    if !text.contains(" | ") {
        return;
    }

    let mut out = String::with_capacity(text.len());

    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }

        let Some(colon) = line.find(": ") else {
            out.push_str(line);
            continue;
        };
        let (head, ty) = line.split_at(colon + 2);
        out.push_str(head);
        out.push_str(&dedupe_type(ty));
    }

    *text = out;
}

/// The type text with every union's repeated members dropped.
fn dedupe_type(text: &str) -> String {
    let members = split_union(text);

    if members.len() > 1 {
        let mut kept: Vec<String> = Vec::new();

        for m in members {
            let m = dedupe_type(m.trim());

            if !kept.contains(&m) {
                kept.push(m);
            }
        }

        return kept.join(" | ");
    }

    // No union at this depth: each bracket group gets its own pass.
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < text.len() {
        let c = text[i..].chars().next().unwrap_or(' ');
        let close = match c {
            '<' => Some('>'),
            '(' => Some(')'),
            '{' => Some('}'),
            '[' => Some(']'),
            _ => None,
        };

        if let Some(close) = close
            && let Some(len) = group_len(&text[i..], c, close)
        {
            out.push(c);
            out.push_str(&dedupe_list(&text[i + 1..i + len - 1]));
            out.push(close);
            i += len;

            continue;
        }

        out.push(c);
        i += c.len_utf8();
    }

    out
}

/// The inside of a group: parts at the commas of depth zero, each a
/// type or a `name: type`, with its own spacing kept.
fn dedupe_list(text: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;
    let bytes = text.as_bytes();

    for i in 0..bytes.len() {
        match bytes[i] {
            b'<' | b'(' | b'{' | b'[' => depth += 1,
            b'>' if i > 0 && bytes[i - 1] == b'-' => {}
            b'>' | b')' | b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&text[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }

    parts.push(&text[from..]);

    let mut out = Vec::with_capacity(parts.len());

    for part in parts {
        let lead = part.len() - part.trim_start().len();
        let trail = part.len() - part.trim_end().len();
        let body = part.trim();
        let deduped = match label_end(body) {
            Some(at) => format!("{}{}", &body[..at], dedupe_type(&body[at..])),

            None => dedupe_type(body),
        };
        out.push(format!(
            "{}{deduped}{}",
            &part[..lead],
            &part[part.len() - trail..]
        ));
    }

    out.join(",")
}

/// The end of a `name: ` or `read name: ` label at depth zero, when the
/// text starts with one.
fn label_end(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = text.as_bytes();

    for i in 0..bytes.len() {
        match bytes[i] {
            b'<' | b'(' | b'{' | b'[' => depth += 1,
            b'>' | b')' | b'}' | b']' => depth -= 1,
            b'|' | b'&' if depth == 0 => return None,
            b':' if depth == 0
                && bytes.get(i + 1) == Some(&b' ')
                && bytes.get(i + 2) != Some(&b':') =>
            {
                return Some(i + 2);
            }
            _ => {}
        }
    }

    None
}

/// The members of a union at depth zero of `text`; one member when the
/// text holds no such union. An arrow's `>` closes no bracket.
fn split_union(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;
    let bytes = text.as_bytes();

    for i in 0..bytes.len() {
        match bytes[i] {
            b'<' | b'(' | b'{' | b'[' => depth += 1,
            b'>' if i > 0 && bytes[i - 1] == b'-' => {}
            b'>' | b')' | b'}' | b']' => depth -= 1,
            b'|' if depth == 0
                && i > 0
                && bytes[i - 1] == b' '
                && bytes.get(i + 1) == Some(&b' ') =>
            {
                out.push(&text[from..i - 1]);
                from = i + 2;
            }
            _ => {}
        }
    }

    out.push(&text[from..]);

    out
}

/// The length of the group `open ... close` that starts the text.
fn group_len(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = text.as_bytes();

    for i in 0..bytes.len() {
        if bytes[i] == open as u8 {
            depth += 1;
        } else if bytes[i] == close as u8 && !(close == '>' && i > 0 && bytes[i - 1] == b'-') {
            depth -= 1;

            if depth == 0 {
                return Some(i + 1);
            }
        }
    }

    None
}

/// `type Snapshot = Readonly<Profile>`: the mapped form reads as the
/// alias the source named.
fn fold_aliases(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Alias { name, target } = shape else {
            continue;
        };

        *text = replace_var(text, target, name);
    }
}

/// An `is table` or `is function` test meets the value with a shape the
/// checker can index or call; the reader wants the primitive's name.
fn fold_narrowed_primitives(text: &mut String) {
    // A long union prints one member per line.
    while let Some(i) = text.find("*error-type*\n") {
        let after = i + "*error-type*\n".len();
        let rest = &text[after..];
        let pad = rest.len() - rest.trim_start().len();

        if rest[pad..].starts_with("| ") {
            text.replace_range(i..after + pad + 2, "");
        } else {
            break;
        }
    }

    let lines: Vec<&str> = text.lines().collect();

    if lines.iter().any(|l| l.trim() == "| *error-type*") {
        let kept: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.trim() != "| *error-type*")
            .collect();
        *text = kept.join("\n");
    }

    for (from, to) in [
        (
            "((...any) -> ()) & ((...any) -> (...any)) & function",
            "function",
        ),
        (
            "((...any) -> (...any)) & ((...any) -> ()) & function",
            "function",
        ),
        (
            "function & ((...any) -> ()) & ((...any) -> (...any))",
            "function",
        ),
        (
            "function & ((...any) -> (...any)) & ((...any) -> ())",
            "function",
        ),
        ("{ [any]: any } & table", "table"),
        ("table & { [any]: any }", "table"),
        // The new solver types the value a `for` reads from an `any`
        // indexer as this; the value is any.
        ("*error-type* | ~nil", "any"),
        // A refinement of that value keeps the error as a member; the
        // other members are the type.
        ("*error-type* | ", ""),
        (" | *error-type*", ""),
        ("(table) & { [any]: any }", "table"),
    ] {
        *text = text.replace(from, to);
    }

    strip_lone_parens(text);
}

/// `x: (string & ~"a")` after a member left the group reads without
/// the parens; a function type, `(a) -> b`, keeps its own.
fn strip_lone_parens(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find(": (") {
        let open = from + i + 2;
        let Some(len) = balanced_len(&text[open..]) else {
            break;
        };
        let close = open + len;
        let after = text[close..].chars().next();

        let inner = &text[open + 1..close - 1];
        let group = inner.contains(" & ") || inner.contains(" | ");

        if after.is_none_or(|c| c == '\n') && group && !inner.contains("->") {
            text.replace_range(close - 1..close, "");
            text.replace_range(open..open + 1, "");
            from = close - 2;
        } else {
            from = close;
        }
    }
}

/// A type printed in place, `local r: { fire: ..., on: ... }`, with no
/// clause, reads by name the way a binding does.
fn fold_heads(text: &mut String, known: &Known) {
    let mut from = 0;

    while let Some(i) = text[from..].find(": {") {
        let open = from + i + 2;
        let Some(len) = balanced_len(&text[open..]) else {
            break;
        };

        match name_of_body(&text[open..open + len], known) {
            Some(name) => {
                text.replace_range(open..open + len, &name);
                from = open + name.len();
            }

            None => from = open + 1,
        }
    }
}

/// `ResultMethods<T, E> & { ... }` is how a Result's method table meets
/// its data half. The methods are the same for every Result, so the
/// data half alone says what the value is.
pub fn drop_result_methods(text: &str) -> String {
    let mut out = text.to_string();
    let mut from = 0;

    while let Some(i) = out[from..].find("ResultMethods") {
        // The runtime's table may spell it `__alloy.ResultMethods`.
        let at = from + i;
        let at = out[..at]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .map_or(0, |k| k + 1);

        let Some(rel) = out[at..].find(" & ") else {
            break;
        };
        let end = at + rel + " & ".len();

        // `(ResultMethods<T, E> & { ... })` loses its parentheses with
        // the member, since one type needs none.
        if out[..at].ends_with('(') {
            let open = at - 1;

            let Some(len) = balanced_len(&out[open..]) else {
                break;
            };
            let close = open + len - 1;
            out.replace_range(close..close + 1, "");
            out.replace_range(open..end, "");
            from = open;

            continue;
        }

        out.replace_range(at..end, "");
        from = at;
    }

    out
}

/// `Result<T, E> & { read expect: ..., read map: ... }`: the method
/// table printed in place, where the alias did not reach the print. It
/// is the same for every Result and names nothing.
fn fold_inline_result_methods(text: &mut String) {
    const KEYS: [&str; 8] = [
        "expect",
        "is_err",
        "is_ok",
        "map",
        "map_err",
        "ok",
        "unwrap",
        "unwrap_or",
    ];
    let mut from = 0;

    while let Some(i) = text[from..].find(" & {") {
        let at = from + i;
        let open = at + " & ".len();

        let Some(len) = balanced_len(&text[open..]) else {
            break;
        };
        let mut keys: Vec<String> = members(&text[open..open + len])
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        keys.dedup();

        if keys == KEYS {
            text.replace_range(at..open + len, "");
            from = at;

            continue;
        }

        from = open;
    }
}

/// A struct value printed in place, `{ @metatable t1, { x: number } }`,
/// reads by the struct's name wherever it stands, not only after a `: `.
fn fold_metatable_groups(text: &mut String, known: &Known) {
    let mut from = 0;

    while let Some(i) = text[from..].find("{ @metatable ") {
        let open = from + i;
        let Some(len) = balanced_len(&text[open..]) else {
            break;
        };
        let body = text[open..open + len].to_string();
        let rest = &body["{ @metatable ".len()..];
        let head_len = type_len(rest);
        let head = rest[..head_len].trim().to_string();

        let replacement = match known.shapes.iter().find(|s| s.name() == head) {
            // Every variant of a payload enum carries the enum's own
            // metatable, so the metatable names none of them. The union
            // of the data tables is what reads as the enum.
            Some(Shape::Enum { .. }) => rest
                .get(head_len + 1..)
                .map(str::trim)
                .and_then(|t| t.strip_suffix('}'))
                .map(|t| t.trim().to_string()),

            // A struct's metatable prints by the struct's name.
            Some(Shape::Struct { name, .. }) => Some(name.clone()),

            _ => {
                let data = rest
                    .get(head_len + 1..)
                    .map(str::trim)
                    .and_then(|t| t.strip_suffix('}'))
                    .map(|t| t.trim().to_string());

                // The metatable is a solver variable the clause never
                // named. A tagged table under it is a variant, and the
                // union of the variants names the enum.
                match data.as_deref().is_some_and(is_tagged_variant) {
                    true => data,

                    false => name_of_body(&body, known),
                }
            }
        };

        match replacement {
            Some(name) => {
                text.replace_range(open..open + len, &name);
                from = open + name.len();
            }

            None => from = open + 1,
        }
    }
}

/// A tagged table the union fold could not pair with its siblings still
/// names one variant; the enum is what the reader wrote.
fn fold_variant_tables(text: &mut String, known: &Known) {
    let mut from = 0;

    while let Some(i) = text[from..].find("tag: \"") {
        let at = from + i;

        let group = enclosing_brace(text, at)
            .and_then(|open| balanced_len(&text[open..]).map(|len| (open, len)));

        let Some((open, len)) = group else {
            from = at + 1;

            continue;
        };
        let body = text[open..open + len].to_string();

        if is_tagged_variant(&body)
            && let Some(name) = enum_of_variant(&body, known)
        {
            text.replace_range(open..open + len, &name);
            from = open + name.len();

            continue;
        }

        from = at + 1;
    }
}

/// The enum a tagged table belongs to: its `tag` literal names one of
/// the enum's variants.
fn enum_of_variant(table: &str, known: &Known) -> Option<String> {
    let m = members(table);
    let (_, tag) = m.iter().find(|(k, _)| k == "tag")?;
    let variant = tag.trim().trim_matches('"');

    known.shapes.iter().find_map(|s| match s {
        Shape::Enum { name, variants } if variants.iter().any(|(v, _)| v == variant) => {
            Some(name.clone())
        }

        _ => None,
    })
}

/// A payload enum whose variants folded to its name still prints its
/// unit variants as strings: `Shape | "Empty"` is `Shape`.
fn fold_enum_unions(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum { name, variants } = shape else {
            continue;
        };
        let units: Vec<String> = variants
            .iter()
            .filter(|(_, p)| p.is_empty())
            .map(|(v, _)| format!("\"{v}\""))
            .collect();

        if units.is_empty() {
            continue;
        }

        let mut from = 0;

        while let Some(at) = word_at_or_after(text, name, from) {
            let mut start = at;
            let mut end = at + name.len();
            let mut grew = false;

            loop {
                let rest = text[end..].trim_start();
                let pad = text[end..].len() - rest.len();

                let Some(after) = rest.strip_prefix("| ") else {
                    break;
                };
                let gap = after.len() - after.trim_start().len();
                let candidate = after.trim_start();

                let Some(unit) = units.iter().find(|u| candidate.starts_with(u.as_str())) else {
                    break;
                };
                end += pad + 2 + gap + unit.len();
                grew = true;
            }

            loop {
                let before = text[..start].trim_end();

                let Some(head) = before.strip_suffix('|') else {
                    break;
                };
                let head = head.trim_end();

                let Some(unit) = units.iter().find(|u| head.ends_with(u.as_str())) else {
                    break;
                };
                start = head.len() - unit.len();
                grew = true;
            }

            match grew {
                true => {
                    text.replace_range(start..end, name);
                    from = start + name.len();
                }

                false => from = at + name.len(),
            }
        }
    }
}

/// The offset of `name` as a whole word at or after `from`.
fn word_at_or_after(text: &str, name: &str, from: usize) -> Option<usize> {
    text[from.min(text.len())..]
        .match_indices(name)
        .map(|(i, _)| from + i)
        .find(|at| {
            let before = text[..*at].chars().next_back();
            let after = text[at + name.len()..].chars().next();

            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Whether a table body is one variant of a payload enum: a `tag`
/// literal with the emit's `_1` and `_2` slots beside it.
fn is_tagged_variant(body: &str) -> bool {
    let m = members(body);

    m.iter().any(|(k, v)| k == "tag" && v.starts_with('"'))
        && m.iter().any(|(k, _)| {
            k.len() > 1 && k.starts_with('_') && k[1..].chars().all(|c| c.is_ascii_digit())
        })
}

/// A mapped result prints as one table with `tag: "Ok" | "Err"` and
/// `read _1: T | E`; it reads as `Result<T, E>`.
fn fold_lite_results(text: &mut String) {
    while let Some(at) = text
        .find("tag: \"Ok\" | \"Err\"")
        .or_else(|| text.find("tag: \"Err\" | \"Ok\""))
    {
        let Some(open) = enclosing_brace(text, at) else {
            return;
        };
        let Some(len) = balanced_len(&text[open..]) else {
            return;
        };
        let body = text[open..open + len].to_string();
        let m = members(&body);
        let get = |key: &str| m.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        // `__ok` and `__err` keep the order the source wrote; the `_1`
        // union prints its members alphabetically.
        let pair = match (get("__ok"), get("__err")) {
            (Some(t), Some(e)) => Some((t, e)),

            _ => get("_1").and_then(|both| {
                both.split_once(" | ")
                    .map(|(t, e)| (t.to_string(), e.to_string()))
            }),
        };

        let Some((t, e)) = pair else {
            return;
        };
        let name = format!("Result<{t}, {e}>");
        text.replace_range(open..open + len, &name);
    }
}

/// A Result the child printed with its members cut, `{ read _1: T,
/// read __err: E, ... 3 more ... }`: the folds that pair the two arms
/// need `tag`, which the cut dropped. `__err` carries the error side of
/// both arms, and the value side is the `_1` that differs from it.
fn fold_cut_results(text: &mut String) {
    const MARK: &str = "read __err: ";
    let mut from = 0;

    while let Some(i) = text[from..].find(MARK) {
        let at = from + i;
        let Some(open) = enclosing_brace(text, at) else {
            return;
        };
        let (start, end) = union_run(text, open);
        let arms = split_union(&text[start..end]);
        let mut error: Option<String> = None;
        let mut value: Option<String> = None;
        let mut cut = false;
        let mut all = !arms.is_empty();

        for arm in &arms {
            let body = arm.trim();

            if !body.starts_with('{') || !body.ends_with('}') {
                all = false;

                break;
            }

            cut = cut || body.contains(" more ...");
            let m = members(body);
            let get = |key: &str| m.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            let Some(e) = get("__err") else {
                all = false;

                break;
            };

            if error.get_or_insert(e.clone()) != &e {
                all = false;

                break;
            }

            if let Some(t) = get("__ok") {
                value = Some(t);
            } else if let Some(t) = get("_1").filter(|t| *t != e) {
                value.get_or_insert(t);
            }
        }

        // A print that carries `tag` is whole; `fold_results` reads it.
        if !all || !cut {
            from = at + MARK.len();

            continue;
        }

        let (Some(t), Some(e)) = (value.or_else(|| error.clone()), error) else {
            from = at + MARK.len();

            continue;
        };
        let name = format!("Result<{t}, {e}>");
        text.replace_range(start..end, &name);
        from = start + name.len();
    }
}

/// The byte range of the ` | ` joined run of brace groups that holds
/// the group opening at `open`.
fn union_run(text: &str, open: usize) -> (usize, usize) {
    let mut start = open;
    let mut end = open + balanced_len(&text[open..]).unwrap_or(text.len() - open);

    loop {
        let head = text[..start].trim_end();
        let Some(prev) = head.strip_suffix('|').map(str::trim_end) else {
            break;
        };

        if !prev.ends_with('}') {
            break;
        }

        let Some(at) = open_of(prev) else {
            break;
        };
        start = at;
    }

    loop {
        let tail = text[end..].trim_start();
        let Some(next) = tail.strip_prefix('|').map(str::trim_start) else {
            break;
        };

        if !next.starts_with('{') {
            break;
        }

        let at = text.len() - next.len();
        let Some(len) = balanced_len(&text[at..]) else {
            break;
        };
        end = at + len;
    }

    (start, end)
}

/// The offset of the `{` that opens the group a text ends with.
fn open_of(text: &str) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text.char_indices().rev() {
        match c {
            '}' => depth += 1,
            '{' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }

    None
}

/// `ResultOk`, `ResultErr`, `Result2` and the method tables are how
/// the std spells the parts of a Result. None of the four is a name a
/// source may write, and each stands for `Result<T, E>`.
fn fold_result_aliases(text: &mut String) {
    for head in [
        "ResultOk<",
        "ResultErr<",
        "Result2<",
        "ResultMethods2<",
        "ResultMethods<",
    ] {
        let mut from = 0;

        while let Some(i) = text[from..].find(head) {
            let at = from + i;
            let Some(len) = group_len(&text[at + head.len() - 1..], '<', '>') else {
                from = at + head.len();

                continue;
            };
            let end = at + head.len() - 1 + len;

            // `ResultMethods<T, E> & { ... }` is the method table met
            // with the data half; the fold above it reads the pair.
            if text[end..].trim_start().starts_with('&') {
                from = at + head.len();

                continue;
            }

            let args = &text[at + head.len()..end - 1];
            let name = format!("Result<{args}>");
            text.replace_range(at..end, &name);
            from = at + name.len();
        }
    }
}

/// `intersect<T, ~nil>` is how the checker writes a value a loop or a
/// test proved is not nil. Alloy has no negation to write, and the
/// name the source gave the value is `T`.
fn fold_refinements(text: &mut String) {
    const HEAD: &str = "intersect<";
    let mut from = 0;

    while let Some(i) = text[from..].find(HEAD) {
        let at = from + i;
        let Some(len) = group_len(&text[at + HEAD.len() - 1..], '<', '>') else {
            from = at + HEAD.len();

            continue;
        };
        let inner = &text[at + HEAD.len()..at + HEAD.len() - 1 + len - 1];
        let kept: Vec<&str> = split_list(inner)
            .into_iter()
            .filter(|p| !p.trim().starts_with('~'))
            .collect();

        if kept.is_empty() {
            from = at + HEAD.len();

            continue;
        }

        let name = kept.join(" & ");
        text.replace_range(at..at + HEAD.len() - 1 + len, &name);
        from = at + name.len();
    }
}

/// `a & ~nil` is a value a test proved is not nil. Alloy writes no
/// negation, and the name the source gave the value is the other side.
fn fold_negated_members(text: &mut String) {
    loop {
        let Some(at) = text.find('~') else {
            return;
        };
        let name_len = text[at + 1..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count();

        if name_len == 0 {
            return;
        }

        let end = at + 1 + name_len;
        let before = text[..at].trim_end();

        // ` & ~nil` goes with the `&` that joined it, and `~nil & ` with
        // the one that follows.
        if let Some(head) = before.strip_suffix('&') {
            text.replace_range(head.trim_end().len()..end, "");

            continue;
        }

        let after = text[end..].trim_start();

        match after.strip_prefix('&') {
            Some(rest) => {
                let keep = text.len() - rest.trim_start().len();
                text.replace_range(at..keep, "");
            }

            None => return,
        }
    }
}

/// The parts of a comma separated list at depth zero.
fn split_list(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in text.char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[start..k].trim());
                start = k + 1;
            }
            _ => {}
        }
    }

    out.push(text[start..].trim());

    out
}

/// `function _1:unwrap(self: any): T`: the receiver is a temp the emit
/// made; the method reads without it.
fn fold_temp_receiver(text: &str) -> String {
    let mut out = text.to_string();

    while let Some(i) = out.find("function _") {
        let rest = &out[i + "function _".len()..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();

        if digits == 0 || !matches!(rest[digits..].chars().next(), Some(':' | '.')) {
            break;
        }

        out.replace_range(
            i + "function ".len()..i + "function _".len() + digits + 1,
            "",
        );
    }

    out
}

/// A `where` clause the child cut short still names an array: the head
/// is a solver variable whose binding opens with `[number]: T` and an
/// Array method. The pair reads as `T[]`, wherever the cut landed.
fn fold_cut_array(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find(" where ") {
        let at = from + i;
        let (head_start, head) = head_of(text, at);
        let Some((var_start, var, optional, quoted)) = solver_head(head_start, head) else {
            from = at + 1;
            continue;
        };
        let clause_start = at + " where ".len();
        let Some(mut name) = cut_name(&text[clause_start..], &var) else {
            from = at + 1;
            continue;
        };
        let end = clause_end(text, clause_start);

        if optional {
            name = format!("{name}?");
        }

        // The cut may have taken the closing quote with it; the
        // replacement writes the pair.
        if quoted && !text[end..].starts_with('\'') {
            name.push('\'');
        }

        text.replace_range(var_start..end, &name);
        from = var_start + name.len();
    }
}

/// A `where` clause head that is one solver variable: its offset, the
/// variable, whether it is optional, and whether a quote opens it.
/// `None` when the head is anything else.
fn solver_head(head_start: usize, head: &str) -> Option<(usize, String, bool, bool)> {
    let lead = head.len() - head.trim_start().len();
    let mut at = head_start + lead;
    let mut var = head.trim();
    let quoted = var.starts_with('\'');

    if quoted {
        at += 1;
        var = &var[1..];
    }

    let var = var.trim_end();
    let optional = var.ends_with('?');
    let var = var.trim_end_matches('?');

    if var.len() < 2 || !var.starts_with('t') || !var[1..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    Some((at, var.to_string(), optional, quoted))
}

/// The end of a `where` clause: the parsed length when the bindings are
/// whole, else the end of the fenced block or of the text, since a cut
/// clause runs to the end of what the child sent.
fn clause_end(text: &str, clause_start: usize) -> usize {
    let (bindings, len) = parse_bindings(&text[clause_start..]);

    if !bindings.is_empty() {
        return clause_start + len;
    }

    text[clause_start..]
        .find("\n```")
        .map_or(text.len(), |k| clause_start + k)
}

/// The name the binding `var` stands for, read from the head of its
/// body so a cut clause still answers: an Array by its indexer and one
/// method, a struct by the return of the `__new` its metatable carries.
/// A binding that only aliases another variable is followed.
fn cut_name(clause: &str, var: &str) -> Option<String> {
    let mut var = var.to_string();

    for _ in 0..4 {
        // The cut may have taken the head's own binding; one Array in
        // the clause is still what the head stands for.
        let body = match binding_body(clause, &var) {
            Some(body) => body,

            None => lone_array_binding(clause)?,
        };
        let trimmed = body.trim_start();

        if let Some(inner) = trimmed.strip_prefix('{') {
            let inner = inner.trim_start();

            if let Some(rest) = inner.strip_prefix("@metatable ") {
                return constructed_name(rest);
            }

            let rest = inner.strip_prefix("[number]: ")?;
            let len = type_len(rest);
            let elem = rest[..len].trim().to_string();
            let next = rest[len..].strip_prefix(',')?.trim_start();
            let key = &next[..next.find(':')?];

            if !matches!(
                key,
                "concat"
                    | "contains"
                    | "filter"
                    | "find"
                    | "for_each"
                    | "is_empty"
                    | "join"
                    | "len"
                    | "map"
                    | "pop"
                    | "push"
            ) {
                return None;
            }

            let compound = elem.contains(" | ")
                || elem.contains(" & ")
                || elem.contains("->")
                || elem.ends_with('?');

            return Some(match compound {
                true => format!("({elem})[]"),

                false => format!("{elem}[]"),
            });
        }

        // `t2 = t1`: the head stands for another binding.
        let next: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();

        if next.len() < 2 || !next.starts_with('t') || next == var {
            return None;
        }

        var = next;
    }

    None
}

/// The one binding of a clause that opens an Array, when exactly one
/// does. A cut clause names the head's binding nowhere else.
fn lone_array_binding(clause: &str) -> Option<&str> {
    let mut found = None;

    for (i, _) in clause.match_indices("= { [number]: ") {
        if found.is_some() {
            return None;
        }

        found = Some(&clause[i + 2..]);
    }

    found
}

/// The struct a metatable builds: `__new: ({ ... }) -> Node` names it.
/// The body may be cut after the constructor; that much is enough.
fn constructed_name(body: &str) -> Option<String> {
    let at = body.find("__new: ").or_else(|| body.find("new: "))?;
    let rest = &body[at..];
    let arrow = rest.find(") -> ")? + ") -> ".len();
    let tail = &rest[arrow..];
    let name = tail[..type_len(tail)].trim().to_string();

    (!name.is_empty() && name.chars().next().is_some_and(char::is_alphabetic)).then_some(name)
}

/// The body of the binding `var` names, from its `= ` to the end of the
/// clause. `None` when the clause binds no such variable.
fn binding_body<'a>(clause: &'a str, var: &str) -> Option<&'a str> {
    let needle = format!("{var} = ");
    let mut from = 0;

    while let Some(i) = clause[from..].find(&needle) {
        let at = from + i;
        let starts = at == 0 || clause[..at].ends_with(" ; ");

        if starts {
            return Some(&clause[at + needle.len()..]);
        }

        from = at + 1;
    }

    None
}

/// `Array<T>` reads as the sugar the source has, `T[]`, when `T` is a
/// name, a dotted path, or an array of one, `Array<number[]>`; a
/// compound argument keeps the alias.
fn fold_array_alias(text: &mut String) {
    // The inner alias of `Array<Array<number>>` folds on the first
    // pass and the outer on the next.
    for _ in 0..4 {
        let before = text.len();
        fold_array_alias_once(text);

        if text.len() == before {
            break;
        }
    }
}

fn fold_array_alias_once(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find("Array<") {
        let start = from + i;
        let inner_start = start + "Array<".len();
        // Not the tail of another name, `ReadArray<`.
        let prefixed = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');

        // The element carries its own arguments, `Result<Profile, any>`;
        // a union or an intersection under the array keeps the form it
        // came with, since `A | B[]` reads the other way.
        match group_len(&text[inner_start - 1..], '<', '>') {
            Some(len)
                if !prefixed && {
                    let elem = &text[inner_start..inner_start + len - 2];

                    !elem.is_empty()
                        && !elem.contains('{')
                        && !outside_angles(elem, '|')
                        && !outside_angles(elem, '&')
                        && !outside_angles(elem, ',')
                } =>
            {
                let elem = text[inner_start..inner_start + len - 2].to_string();
                text.replace_range(start..inner_start + len - 1, &format!("{elem}[]"));
                from = start + elem.len() + 2;
            }

            _ => from = inner_start,
        }
    }
}

/// The type text before ` where `: from the start of its line, past a
/// `local x: ` or `x: ` head, so a replacement keeps the label.
fn head_of(text: &str, where_at: usize) -> (usize, &str) {
    // A head printed over several lines ends in a bracket before the
    // `where`; the type starts on the line that opens it. A union walks
    // group by group, past every `|` between them.
    let mut group_start = where_at;

    loop {
        let before = text[..group_start].trim_end();

        match before.chars().next_back() {
            Some('}' | ')' | ']') => match enclosing_open(text, before.len() - 1) {
                Some(open) => group_start = open,

                None => break,
            },

            Some('|' | '&' | '?') => group_start = before.len() - 1,

            _ => break,
        }
    }

    let line_start = text[..group_start].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let line = &text[line_start..group_start];
    // The type starts after the last `: ` outside brackets on the line,
    // or at the line start when the line is the type alone.
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in line.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ':' if depth == 0 && line[k + 1..].starts_with(' ') => start = k + 2,
            _ => {}
        }
    }

    (line_start + start, &text[line_start + start..where_at])
}

/// The opener that matches the closing bracket at `close`.
fn enclosing_open(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text[..=close].char_indices().rev() {
        match c {
            '}' | ')' | ']' => depth += 1,
            '{' | '(' | '[' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }

    None
}

fn parse_bindings(text: &str) -> (Vec<Binding>, usize) {
    let mut out = Vec::new();
    let mut at = 0;

    loop {
        let rest = &text[at..];
        let trimmed = rest.trim_start();
        at += rest.len() - trimmed.len();
        let var: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();

        if !var.starts_with('t') || var.len() < 2 || !trimmed[var.len()..].starts_with(" = ") {
            break;
        }

        let body_start = at + var.len() + 3;
        let Some(len) = binding_len(&text[body_start..]) else {
            break;
        };
        out.push(Binding {
            var,
            body: text[body_start..body_start + len].to_string(),
        });
        at = body_start + len;

        // The separator belongs to the next binding: a clause that stops
        // after it, at a truncated body, keeps the separator in place.
        if text[at..].starts_with(" ; ") && next_binding(&text[at + 3..]) {
            at += 3;
        } else {
            break;
        }
    }

    (out, at)
}

/// Whether a `tN = ` binding with a balanced body starts the text.
fn next_binding(text: &str) -> bool {
    let var: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();

    var.starts_with('t')
        && var.len() >= 2
        && text[var.len()..].starts_with(" = ")
        && binding_len(&text[var.len() + 3..]).is_some()
}

/// The length of a type that starts at the text's start: a balanced
/// bracket group, or a run up to the next separator.
fn balanced_len(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;

    for (k, c) in text.char_indices() {
        if in_string {
            if c == '"' {
                in_string = false;
            }

            continue;
        }

        // `->` carries a `>`, so angle brackets do not count.
        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k + 1);
                }
            }
            '\n' | ';' if depth == 0 => return Some(k),
            _ => {}
        }
    }

    (depth == 0).then_some(text.len())
}

/// The length of a binding's body: a type that may go on past a closed
/// group, as `{ ... } & { ... }`, `( ... ) | nil`, `{ ... }?`, or
/// `( ... ) -> ()`.
fn binding_len(text: &str) -> Option<usize> {
    let mut at = balanced_len(text)?;

    loop {
        let rest = &text[at..];
        let next = rest.trim_start_matches(' ');
        let pad = rest.len() - next.len();

        if next.starts_with('?') {
            at += pad + 1;
        } else if let Some(ret) = next.strip_prefix("-> ") {
            let more = balanced_len(ret)?;
            at += pad + 3 + more;
        } else if next.starts_with('&') || next.starts_with('|') {
            let after = &next[1..];
            let gap = after.len() - after.trim_start_matches(' ').len();
            let more = balanced_len(&after[gap..])?;
            at += pad + 1 + gap + more;
        } else {
            return Some(at);
        }
    }
}

fn mentions(text: &str, var: &str) -> bool {
    text.match_indices(var).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + var.len()..].chars().next();

        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn replace_var(text: &str, var: &str, name: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;

    while let Some(i) = text[from..].find(var) {
        let at = from + i;
        let before = text[..at].chars().next_back();
        let after = text[at + var.len()..].chars().next();
        let whole = !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        out.push_str(&text[from..at]);
        out.push_str(if whole { name } else { var });
        from = at + var.len();
    }

    out.push_str(&text[from..]);

    out
}

/// The name each binding stands for, by its shape, to a fixpoint so an
/// array of structs reads `Vec2[]`.
fn resolve(bindings: &[Binding], known: &Known) -> Vec<(String, String)> {
    let mut resolved: Vec<(String, String)> = Vec::new();

    // A name that still holds another binding's variable waits for it.
    // `t1 = { Connect: (self: t1, handler: (t4) -> ()) -> t6 }` names
    // itself `Signal<t4>` on sight; `t4` is an enum, and the reader
    // wants `Signal<Effect>`. The second round takes what is left.
    for strict in [true, false] {
        loop {
            let mut changed = false;

            for b in bindings {
                if resolved.iter().any(|(v, _)| *v == b.var) {
                    continue;
                }

                let mut body = b.body.clone();

                for (v, name) in &resolved {
                    body = replace_var(&body, v, name);
                }

                let Some(name) = name_of_body(&body, known) else {
                    continue;
                };
                let waits = bindings.iter().any(|o| {
                    o.var != b.var
                        && !resolved.iter().any(|(v, _)| *v == o.var)
                        && mentions(&name, &o.var)
                });

                if strict && waits {
                    continue;
                }

                resolved.push((b.var.clone(), name));
                changed = true;
            }

            if !changed {
                break;
            }
        }
    }

    resolved
}

/// The members of a table body split at the commas of depth one, with
/// their modifiers. A type argument list holds its own commas, so
/// `[number]: Result<number, any>` stays one member.
fn member_parts(body: &str) -> Vec<&str> {
    let inner = body.trim();
    let inner = inner
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(inner);
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();

    while start < inner.len() {
        let len = type_len(&inner[start..]);

        if inner[start + len..].starts_with(',') {
            parts.push(&inner[start..start + len]);
            start += len + 1;
        } else {
            break;
        }
    }

    parts.push(&inner[start..]);

    parts
}

/// The length of a type at the start of the text, up to a `,` or a
/// closing bracket at depth zero. Angle brackets count, so
/// `Result<number, any>` stays whole; the `>` of an arrow closes none.
fn type_len(text: &str) -> usize {
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut in_string = false;
    let mut prev = ' ';

    for (k, c) in text.char_indices() {
        if in_string {
            if c == '"' {
                in_string = false;
            }

            prev = c;

            continue;
        }

        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' if depth == 0 => return k,
            '}' | ')' | ']' => depth -= 1,
            '<' if prev.is_ascii_alphanumeric() || prev == '_' => angle += 1,
            '>' if prev != '-' && angle > 0 => angle -= 1,
            ',' if depth == 0 && angle == 0 => return k,
            _ => {}
        }

        prev = c;
    }

    text.len()
}

fn members(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();

    for part in member_parts(body) {
        let part = part.trim();

        if part.is_empty() {
            continue;
        }

        let Some(colon) = find_key_colon(part) else {
            continue;
        };
        let key = part[..colon].trim();
        let key = key
            .strip_prefix("read ")
            .or_else(|| key.strip_prefix("write "))
            .unwrap_or(key);
        out.push((key.to_string(), part[colon + 1..].trim().to_string()));
    }

    out
}

/// The colon after a member's key, not one inside a function type.
fn find_key_colon(part: &str) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in part.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ':' if depth == 0 => return Some(k),
            _ => {}
        }
    }

    None
}

fn name_of_body(body: &str, known: &Known) -> Option<String> {
    let trimmed = body.trim();

    // A struct instance: `{ @metatable tN, { fields } }`. The metatable
    // may print in place, so its own commas are not the separator.
    if let Some(rest) = trimmed.strip_prefix("{ @metatable ") {
        let comma = type_len(rest);
        let table = rest.get(comma + 1..)?.trim();
        let table = table.strip_suffix('}')?.trim();
        // A guard on a field prints it twice, `read dirty: true, write
        // dirty: boolean`; the field set is what names the struct.
        let mut printed: Vec<String> = members(table)
            .into_iter()
            .map(|(k, _)| {
                k.trim_start_matches("read ")
                    .trim_start_matches("write ")
                    .to_string()
            })
            .collect();
        printed.sort();
        printed.dedup();

        // A tagged table under a metatable is one variant of a payload
        // enum; the enum is the type the source wrote.
        if is_tagged_variant(table)
            && let Some(name) = enum_of_variant(table, known)
        {
            return Some(name);
        }

        for shape in &known.shapes {
            if let Shape::Struct { name, fields } = shape {
                let all: Vec<&String> = fields.iter().map(|(f, _)| f).collect();
                let public: Vec<&String> =
                    fields.iter().filter(|(_, p)| !p).map(|(f, _)| f).collect();

                // A refinement flattens the methods in with the fields;
                // every field present still names the struct.
                let holds_all = !all.is_empty() && all.iter().all(|f| printed.contains(f));

                if !printed.is_empty()
                    && (same_set(&printed, &all) || same_set(&printed, &public) || holds_all)
                {
                    return Some(name.clone());
                }
            }
        }

        return None;
    }

    // A struct's full view: the name met with its private tables,
    // `Saber & { ... } & { ... }`.
    if let Some((head, rest)) = trimmed.split_once(" & ")
        && known
            .shapes
            .iter()
            .any(|s| matches!(s, Shape::Struct { name, .. } if name == head))
        && rest.split(" & ").all(|part| {
            let part = part.trim();

            part.starts_with('{') && balanced_len(part) == Some(part.len())
        })
    {
        return Some(head.to_string());
    }

    // A body an earlier pass already named, `Future<number>`, `number[]`:
    // the name is the body.
    if !trimmed.starts_with('{')
        && !trimmed.starts_with('(')
        && !trimmed.contains("->")
        && !trimmed.contains(" & ")
        && !trimmed.contains(" | ")
        && trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return Some(trimmed.to_string());
    }

    // A payload enum prints as the union of its variants, and a unit
    // variant prints as its own name in quotes. One name covers it.
    if let Some(name) = union_name(trimmed, known) {
        return Some(name);
    }

    let m = members(trimmed);
    let has = |key: &str| m.iter().any(|(k, _)| k == key);
    let get = |key: &str| m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());

    // The object a `remote` declaration binds.
    // The surface a side sees is a subset, so no one member is always
    // there; the pair of `instance` and `spec` is.
    if has("instance")
        && has("spec")
        && ["call", "fire", "fire_all", "on", "once", "wait"]
            .iter()
            .any(|k| has(k))
    {
        return Some("Remote".to_string());
    }

    // A mapped type over a struct: every field read-only, or every
    // field optional, over the struct's field set.
    if !m.is_empty() && trimmed.starts_with('{') {
        let parts: Vec<&str> = member_parts(trimmed)
            .into_iter()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let all_read = parts.iter().all(|p| p.starts_with("read "));
        let all_optional = parts.iter().all(|p| !p.starts_with("read "))
            && m.iter().all(|(_, v)| v.ends_with('?'));
        let keys: Vec<String> = m.iter().map(|(k, _)| k.clone()).collect();

        if all_read || all_optional {
            let head = if all_read { "Readonly" } else { "Partial" };

            for shape in &known.shapes {
                let Shape::Struct { name, fields } = shape else {
                    continue;
                };
                let all: Vec<&String> = fields.iter().map(|(f, _)| f).collect();
                let public: Vec<&String> =
                    fields.iter().filter(|(_, p)| !p).map(|(f, _)| f).collect();

                if same_set(&keys, &all) || same_set(&keys, &public) {
                    return Some(format!("{head}<{name}>"));
                }
            }

            // An interface carries the fields of what it extends too.
            for iface in &known.interfaces {
                let every = iface.inherited(&known.interfaces);
                let all: Vec<&String> = every.iter().collect();

                if !all.is_empty() && same_set(&keys, &all) {
                    return Some(format!("{head}<{}>", iface.name));
                }
            }
        }
    }

    // An interface prints as what it extends, met with a table of the
    // fields it adds; the source wrote one name.
    if let Some(iface) = known
        .interfaces
        .iter()
        .find(|i| i.matches(trimmed, &known.interfaces))
    {
        return Some(iface.name.clone());
    }

    // The std containers, by the methods that name their arguments. Two
    // arrays of one element type are two types to the checker, so the
    // element may be a union: it keeps its parentheses under the `[]`
    // until the names inside it resolve and the union folds.
    if let Some(elem) = get("[number]")
        && has("concat")
        && has("push")
    {
        return Some(if elem.contains(" | ") {
            format!("({elem})[]")
        } else {
            format!("{elem}[]")
        });
    }

    if let Some(sig) = get("get")
        && has("set")
        && has("entries")
        && let Some((k, v)) = map_args(sig)
    {
        return Some(format!("HashMap<{k}, {v}>"));
    }

    if let Some(sig) = get("add")
        && has("has")
        && has("union")
        && let Some(t) = set_arg(sig)
    {
        return Some(format!("Set<{t}>"));
    }

    // A struct's metatable: `{ __index: t1, __new: (f: { ... }) -> Node,
    // __tostring: (s: Node) -> string }`. The constructor's return names
    // the struct the metatable belongs to.
    if has("__index")
        && let Some(sig) = get("__new").or_else(|| get("new"))
        && let Some(arrow) = sig.rfind("-> ")
    {
        let name = sig[arrow + 3..].trim();

        if known
            .shapes
            .iter()
            .any(|s| matches!(s, Shape::Struct { name: n, .. } if n == name))
        {
            return Some(name.to_string());
        }
    }

    // The rest of the std, by a member only it has.
    if let Some(sig) = get("Connect")
        && has("Fire")
        && has("DisconnectAll")
    {
        // `Connect: (self: t1, handler: (A, B) -> ()) -> t2`: the
        // handler's parameters are the signal's arguments.
        let args = sig
            .find("handler: (")
            .map(|i| i + "handler: (".len())
            .or_else(|| sig.find("f: (").map(|i| i + "f: (".len()))
            .and_then(|from| {
                balanced_len(&sig[from - 1..]).map(|len| sig[from..from - 1 + len - 1].to_string())
            })
            .unwrap_or_default();

        return Some(format!("Signal<{args}>"));
    }

    if has("Connected") && has("Disconnect") && m.len() <= 4 {
        return Some("SignalConnection".to_string());
    }

    // The engine's signal alias, expanded: the callback names the arguments.
    if let Some(sig) = get("Connect")
        && has("ConnectParallel")
        && has("Once")
    {
        let args = sig
            .find("callback: (")
            .map(|i| i + "callback: (".len())
            .and_then(|from| {
                balanced_len(&sig[from - 1..]).map(|len| sig[from..from - 1 + len - 1].to_string())
            })
            .unwrap_or_default();

        return Some(format!("RBXScriptSignal<{args}>"));
    }

    // A Future carries its value type as `__value`.
    if let Some(v) = get("__value")
        && has("andThen")
    {
        let t = if v == "nil" { "()" } else { v };

        return Some(format!("Future<{t}>"));
    }

    if let Some(sig) = get("andThen")
        && has("is_settled")
    {
        let t = sig
            .find("on_resolve: ((")
            .and_then(|i| {
                let from = i + "on_resolve: ((".len();
                sig[from..]
                    .find(") -> ())?")
                    .map(|len| sig[from..from + len].to_string())
            })
            .unwrap_or_else(|| "any".to_string());

        let t = if t == "nil" { "()".to_string() } else { t };

        return Some(format!("Future<{t}>"));
    }

    if has("add") && has("clean") && has("extend") && has("Destroy") {
        return Some("Scope".to_string());
    }

    if let Some(sig) = get("push")
        && has("pop")
        && has("peek")
        && !has("concat")
    {
        let t = sig
            .find("value: ")
            .and_then(|i| {
                sig[i + 7..]
                    .find(')')
                    .map(|e| sig[i + 7..i + 7 + e].to_string())
            })
            .unwrap_or_else(|| "any".to_string());
        // A Heap keeps the comparison it was built with; a Queue has
        // no such member. The std spells it `__less`.
        let kind = if has("__less") || has("less") || has("sift_up") {
            "Heap"
        } else {
            "Queue"
        };

        return Some(format!("{kind}<{t}>"));
    }

    if has("next") && has("take_while") && has("collect") {
        let t = get("next")
            .and_then(return_type)
            .map(|t| t.trim_end_matches('?').to_string())
            .unwrap_or_else(|| "any".to_string());

        return Some(format!("Iter<{t}>"));
    }

    None
}

/// The one name a union carries: every member names the same shape, or
/// is the string a unit variant of it prints as.
fn union_name(body: &str, known: &Known) -> Option<String> {
    let parts = union_parts(body)?;
    let mut name: Option<String> = None;

    for part in parts {
        let part = part.trim();
        let found = match part.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
            Some(unit) => enum_of_unit(unit, known)?,

            None => name_of_body(part, known)?,
        };

        match &name {
            Some(had) if *had != found => return None,

            _ => name = Some(found),
        }
    }

    name
}

/// The members of a union at depth zero, or `None` when the text holds
/// no `|` of its own.
fn union_parts(body: &str) -> Option<Vec<&str>> {
    let bytes = body.as_bytes();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut in_string = false;
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();

    for (k, c) in body.char_indices() {
        if in_string {
            if c == '"' {
                in_string = false;
            }

            continue;
        }

        match c {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            '<' => angle += 1,
            '>' if k > 0 && bytes[k - 1] != b'-' => angle -= 1,
            '|' if depth == 0 && angle == 0 => {
                parts.push(&body[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        return None;
    }

    parts.push(&body[start..]);

    Some(parts)
}

/// The enum a unit variant belongs to, by the name it prints as.
fn enum_of_unit(unit: &str, known: &Known) -> Option<String> {
    known.shapes.iter().find_map(|s| match s {
        Shape::Enum { name, variants }
            if variants.iter().any(|(v, p)| v == unit && p.is_empty()) =>
        {
            Some(name.clone())
        }

        _ => None,
    })
}

/// An interface prints as its bases met with a table of the fields it
/// adds. The name the source wrote reads everywhere the print does, not
/// only after a `: `.
fn fold_interfaces(text: &mut String, known: &Known) {
    if known.interfaces.is_empty() {
        return;
    }

    let mut from = 0;

    while let Some(i) = text[from..].find('{') {
        let open = from + i;
        let start = base_before(text, open).unwrap_or(open);

        // A member inside a longer intersection folds with its head.
        let inner =
            text[..start].trim_end().ends_with('&') || intersection_len(&text[start..]).is_none();

        if inner {
            from = open + 1;

            continue;
        }

        let len = intersection_len(&text[start..]).expect("intersection");
        let body = text[start..start + len].to_string();

        match known
            .interfaces
            .iter()
            .find(|f| f.matches(&body, &known.interfaces))
        {
            Some(iface) => {
                let name = iface.name.clone();
                text.replace_range(start..start + len, &name);
                from = start + name.len();
            }

            None => from = open + 1,
        }
    }
}

/// The start of the name that opens `Named & { ... }`, when the table
/// at `open` follows one.
fn base_before(text: &str, open: usize) -> Option<usize> {
    let head = text[..open].strip_suffix("& ")?.trim_end();
    let len = head
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .count();

    (len > 0).then(|| head.len() - len)
}

/// How far an intersection runs from the start of the text: one member,
/// then every ` & ` member after it.
fn intersection_len(text: &str) -> Option<usize> {
    let mut at = member_len(text)?;

    loop {
        let rest = &text[at..];
        let pad = rest.len() - rest.trim_start().len();

        let Some(after) = rest.trim_start().strip_prefix("& ") else {
            return Some(at);
        };
        let gap = after.len() - after.trim_start().len();
        let next = at + pad + 2 + gap;

        let Some(len) = member_len(&text[next..]) else {
            return Some(at);
        };

        at = next + len;
    }
}

/// Whether a character stands in the text outside every bracket.
fn outside_angles(text: &str, want: char) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 0i32;

    for (k, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if k > 0 && bytes[k - 1] == b'-' => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            _ if c == want && depth == 0 => return true,
            _ => {}
        }
    }

    false
}

/// `(Slot)?` is `Slot?`: the parentheses held an intersection that now
/// reads as one name.
fn fold_name_parens(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find('(') {
        let open = from + i;

        let Some(len) = group_len(&text[open..], '(', ')') else {
            break;
        };
        let inner = text[open + 1..open + len - 1].to_string();
        // One named type, its arguments included. A union, an
        // intersection, an arrow, or a list needs the parentheses.
        let plain = inner.chars().next().is_some_and(char::is_alphabetic)
            && !inner.contains("->")
            && !outside_angles(&inner, ',')
            && !outside_angles(&inner, '|')
            && !outside_angles(&inner, '&');
        // A `?` or a `[]` after the group is what the parentheses were
        // for; anywhere else they may be a parameter list.
        let suffix = matches!(text[open + len..].chars().next(), Some('?') | Some('['));

        if plain && suffix {
            text.replace_range(open..open + len, &inner);
            from = open + inner.len();

            continue;
        }

        from = open + 1;
    }
}

/// `Signalish<T...>` prints as the four sources it takes. The doc names
/// the union, so the reader gets the name.
fn fold_signalish(text: &mut String) {
    const HEAD: &str = "RBXScriptSignal<";
    let mut from = 0;

    while let Some(i) = text[from..].find(HEAD) {
        let at = from + i;
        let Some(len) = group_len(&text[at + HEAD.len() - 1..], '<', '>') else {
            break;
        };
        let arg = text[at + HEAD.len()..at + HEAD.len() - 1 + len - 1].to_string();
        let tail = format!(
            " | Signal<{arg}> | {{ Connect: (self: any, handler: ({arg}) -> ()) -> any }} | {{ connect: (self: any, handler: ({arg}) -> ()) -> any }}"
        );
        let end = at + HEAD.len() - 1 + len;

        if !text[end..].starts_with(&tail) {
            from = at + HEAD.len();

            continue;
        }

        let name = format!("Signalish<{arg}>");
        text.replace_range(at..end + tail.len(), &name);
        from = at + name.len();
    }
}

/// The return type of a function type: what follows the first arrow
/// that stands outside every bracket. `Iter`'s own `next` takes an
/// `Iter` as its `self`, so the first arrow in the text is the nested
/// one and reading it cuts the type in half.
fn return_type(sig: &str) -> Option<&str> {
    let bytes = sig.as_bytes();
    let mut depth = 0i32;

    for (k, c) in sig.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '-' if depth == 0 && bytes.get(k + 1) == Some(&b'>') => {
                return Some(sig[k + 2..].trim());
            }
            _ => {}
        }
    }

    None
}

/// `Swinger & { last: number, scope: Scope }` is a struct's full view
/// once its private table folded away; the reader knows it as `Swinger`.
fn fold_full_views(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Struct { name, .. } = shape else {
            continue;
        };
        let head = format!("{name} & {{");
        let mut from = 0;

        while let Some(i) = text[from..].find(&head) {
            let at = from + i;
            let before = text[..at].chars().next_back();

            if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                from = at + head.len();
                continue;
            }

            let brace = at + name.len() + 3;

            match balanced_len(&text[brace..]) {
                Some(len) => {
                    text.replace_range(at..brace + len, name);
                    from = at + name.len();
                }

                None => from = at + head.len(),
            }
        }
    }
}

/// `Session & Session__private & { dirty: boolean }` is the full view of
/// a struct inside its own impl; the reader knows it as `Session`. The
/// private table's own name goes too: `Session__private.mark` is
/// `Session.mark`.
pub fn fold_private_views(text: &str) -> String {
    let mut out = text.to_string();

    while let Some(i) = out.find("__private & ") {
        let name_start = out[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map(|n| n + 1)
            .unwrap_or(0);
        let name = out[name_start..i].to_string();
        let brace = i + "__private & ".len();

        if !out[brace..].starts_with('{') {
            break;
        }

        let Some(len) = balanced_len(&out[brace..]) else {
            break;
        };
        // `Name & Name__private & { ... }`: the head `Name & ` goes too.
        let head = format!("{name} & ");
        let start = if out[..name_start].ends_with(&head) {
            name_start - head.len()
        } else {
            name_start
        };
        out.replace_range(start..brace + len, &name);
    }

    out.replace("__private.", ".")
}

fn same_set(a: &[String], b: &[&String]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(&x))
}

/// `(self: t1, key: K) -> V?` as `(K, V)`.
fn map_args(sig: &str) -> Option<(String, String)> {
    let key_at = sig.find("key: ")? + 5;
    let key_end = sig[key_at..].find(')')? + key_at;
    let arrow = sig[key_end..].find("-> ")? + key_end + 3;
    let value = sig[arrow..].trim().trim_end_matches('?');

    Some((sig[key_at..key_end].trim().to_string(), value.to_string()))
}

/// `(self: t1, value: T) -> boolean` as `T`.
fn set_arg(sig: &str) -> Option<String> {
    let at = sig.find("value: ")? + 7;
    let end = sig[at..].find(')')? + at;

    Some(sig[at..end].trim().to_string())
}

/// `{ read _1: T, ..., tag: "Ok", ... } | { read _1: E, ..., tag: "Err", ... }`
/// as `Result<T, E>`, in either order, innermost first when one nests
/// inside another's method.
fn fold_results(text: &mut String) {
    let mut limit = text.len();

    while let Some(at) = text[..limit].rfind("tag: \"Ok\"") {
        let Some(open) = enclosing_brace(text, at) else {
            limit = at;
            continue;
        };
        let Some(len) = balanced_len(&text[open..]) else {
            limit = at;
            continue;
        };
        let ok_body = text[open..open + len].to_string();
        let ok_members = members(&ok_body);
        let own_tag = ok_members.iter().any(|(k, v)| k == "tag" && v == "\"Ok\"");
        let t = ok_members
            .iter()
            .find(|(k, _)| k == "_1")
            .map(|(_, v)| v.clone());

        let (Some(t), true) = (t, own_tag) else {
            limit = at;
            continue;
        };

        // The Err table sits right after, or right before.
        let after = &text[open + len..];
        let sibling = if let Some(rest) = after.strip_prefix(" | {") {
            balanced_len(&text[open + len + 3..])
                .map(|blen| {
                    let b_open = open + len + 3;

                    (b_open, b_open + blen, open, b_open + blen)
                })
                .filter(|_| !rest.is_empty())
        } else if text[..open].ends_with("} | ") {
            let b_close = open - " | ".len();
            enclosing_brace(text, b_close - 1).map(|b_open| (b_open, b_close, b_open, open + len))
        } else {
            None
        };

        let Some((b_open, b_close, whole_start, whole_end)) = sibling else {
            limit = at;
            continue;
        };
        let err_body = text[b_open..b_close].to_string();
        let err_members = members(&err_body);
        let err_tag = err_members
            .iter()
            .any(|(k, v)| k == "tag" && v == "\"Err\"");
        let e = err_members
            .iter()
            .find(|(k, _)| k == "_1")
            .map(|(_, v)| v.clone());

        let (Some(e), true) = (e, err_tag) else {
            limit = at;
            continue;
        };

        text.replace_range(whole_start..whole_end, &format!("Result<{t}, {e}>"));
        limit = text.len();
    }
}

/// A Result prints as two groups, each a tagged table met with the
/// methods table: `({ read _1: E, tag: "Err", ... } & { ... }) | ({ read
/// _1: T, tag: "Ok", ... } & { ... })`, or with the methods first as
/// `(ResultMethods<T, E> & { ... })` in a signature. The pair reads as
/// `Result<T, E>`; one group alone, a narrowed side, as `ResultOk<T, E>`
/// or `ResultErr<T, E>`. `__ok` and `__err` carry the arguments. The
/// innermost pair folds first, so a `map` inside reads too.
fn fold_tagged_results(text: &mut String) {
    let mut limit = text.len();

    while let Some(at) = text[..limit].rfind("tag: \"") {
        limit = at;

        let Some(open) = enclosing_brace(text, at) else {
            continue;
        };
        let Some(group) = result_group(text, open) else {
            continue;
        };
        let (start, end, ok, err, tag) = group;

        // The other side sits before, or after; a long union breaks the
        // line before its `|`.
        let after = text[end..].trim_start();
        let before = text[..start].trim_end();
        let pair = if after.starts_with("| (") {
            let brace_at = end + (text[end..].len() - after.len()) + "| (".len();

            group_brace(text, brace_at)
                .and_then(|brace| result_group(text, brace))
                .filter(|g| g.4 != tag)
                .map(|g| (start, g.1))
        } else if before.ends_with('|') && before[..before.len() - 1].trim_end().ends_with(')') {
            let close = before[..before.len() - 1].trim_end().len() - 1;

            group_start(text, close)
                .and_then(|s_start| group_brace(text, s_start + 1))
                .and_then(|brace| result_group(text, brace))
                .filter(|g| g.4 != tag)
                .map(|g| (g.0, end))
        } else {
            None
        };

        let (whole_start, whole_end, name) = match pair {
            Some((ws, we)) => (ws, we, format!("Result<{ok}, {err}>")),

            None => (start, end, format!("Result{tag}<{ok}, {err}>")),
        };

        text.replace_range(whole_start..whole_end, &name);
        limit = whole_start;
    }
}

/// The `{` of the tagged table of a group that opens at `paren`: the
/// first brace of the group's first part when that part is a table, else
/// the brace after the `& `.
fn group_brace(text: &str, after_paren: usize) -> Option<usize> {
    let rest = &text[after_paren..];

    if rest.starts_with('{') {
        return Some(after_paren);
    }

    let amp = rest.find(" & {")?;

    Some(after_paren + amp + 3)
}

/// One group of a printed Result, given the `{` of its tagged table:
/// the group's start (its `(`), its end (past the `)`), the `__ok` and
/// `__err` types, and the tag. `None` when the shape is not a Result
/// member.
fn result_group(text: &str, brace: usize) -> Option<(usize, usize, String, String, String)> {
    let table_len = balanced_len(&text[brace..])?;
    let members = members(&text[brace..brace + table_len]);
    let tag = members
        .iter()
        .find(|(k, _)| k == "tag")
        .map(|(_, v)| v.trim_matches('"').to_string())?;

    if tag != "Ok" && tag != "Err" {
        return None;
    }

    let ok = members
        .iter()
        .find(|(k, _)| k == "__ok")
        .map(|(_, v)| v.clone())?;
    let err = members
        .iter()
        .find(|(k, _)| k == "__err")
        .map(|(_, v)| v.clone())?;
    let after = brace + table_len;

    let before = text[..brace].trim_end();

    // Methods first: `ResultMethods<T, E> & { ... }`, in parens or not.
    // The hover names the runtime's table before the strip: `__alloy.`.
    if before.ends_with('&') {
        let head_end = before.len() - 1;
        let head = text[..head_end].trim_end();
        let mut name_at = head.rfind("ResultMethods")?;

        if head[name_at..].contains(' ') && !head[name_at..].contains(", ") {
            return None;
        }

        if head[..name_at].ends_with("__alloy.") {
            name_at -= "__alloy.".len();
        }

        let paren = name_at > 0 && text[..name_at].trim_end().ends_with('(');
        let (start, end) = if paren && text[after..].starts_with(')') {
            (text[..name_at].trim_end().len() - 1, after + 1)
        } else {
            (name_at, after)
        };

        return Some((start, end, ok, err, tag));
    }

    // Tagged table first: `{ ... } & { methods }`, in parens or not.
    let rest = &text[after..];
    let pad = rest.len() - rest.trim_start().len();
    let methods_at = after + pad;

    if !text[methods_at..].starts_with("& ") {
        return None;
    }

    let m_open = methods_at + 2;
    let m_len = other_len(&text[m_open..])?;
    let close = m_open + m_len;
    let paren = text[..brace].ends_with('(');

    if paren && text[close..].starts_with(')') {
        return Some((brace - 1, close + 1, ok, err, tag));
    }

    Some((brace, close, ok, err, tag))
}

/// The length of the methods part: a table, or an alias with arguments.
fn other_len(text: &str) -> Option<usize> {
    if text.starts_with('{') {
        return balanced_len(text);
    }

    let open = text.find('<')?;
    let mut depth = 0i32;

    for (k, c) in text[open..].char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;

                if depth == 0 {
                    return Some(open + k + 1);
                }
            }
            _ => {}
        }
    }

    None
}

/// The `(` that opens the group a `)` at `close` ends.
fn group_start(text: &str, close: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text[..=close].char_indices().rev() {
        match c {
            ')' | '}' | ']' => depth += 1,
            '(' | '{' | '[' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }

    None
}

/// The `{` that opens the table a byte sits in.
fn enclosing_brace(text: &str, at: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text[..at].char_indices().rev() {
        match c {
            '}' => depth += 1,
            '{' if depth == 0 => return Some(k),
            '{' => depth -= 1,
            _ => {}
        }
    }

    None
}

/// A Symbol prints as an empty table under a metatable with a printer;
/// it reads as `Symbol`.
fn fold_symbols(text: &mut String) {
    // The child prints the empty half as `{ }` or as `{  }`.
    *text = text.replace("{  }", "{ }");
    let pattern = "{ @metatable { __tostring: (...any) -> string }, { } }";
    let mut from = 0;

    while let Some(i) = text[from..].find("{ @metatable {") {
        let at = from + i;

        match match_loose(&text[at..], pattern) {
            Some(len) => {
                text.replace_range(at..at + len, "Symbol");
                from = at + "Symbol".len();
            }

            None => from = at + 1,
        }
    }
}

/// An enum prints as the union of its members: `"Name"` for a unit,
/// `{ _1: T, tag: "V" }` for a payload. The members come in the order
/// of their text, and a payload of another enum may already read by
/// name, so the match is a set: a union whose members are the enum's,
/// in any order, reads as the enum.
fn fold_enums(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum { name, variants } = shape else {
            continue;
        };

        if variants.len() < 2 {
            continue;
        }

        let members: Vec<String> = variants
            .iter()
            .map(|(v, p)| {
                if p.is_empty() {
                    format!("\"{v}\"")
                } else {
                    let fields: Vec<String> = p
                        .iter()
                        .enumerate()
                        .map(|(i, t)| format!("_{}: {t}", i + 1))
                        .collect();

                    format!("{{ {}, tag: \"{v}\" }}", fields.join(", "))
                }
            })
            .collect();
        let first = members[0].split(',').next().unwrap_or("").to_string();
        let mut from = 0;

        while let Some(i) = text[from..].find(&first) {
            let at = from + i;
            let Some((start, end, found)) = union_around(text, at) else {
                from = at + 1;
                continue;
            };
            let all = members.len() == found.len()
                && members.iter().all(|m| {
                    found
                        .iter()
                        .any(|f| match_loose(f, m).is_some_and(|len| len == f.len()))
                });

            if all {
                text.replace_range(start..end, name);
                from = start + name.len();
            } else {
                from = at + 1;
            }
        }
    }
}

/// The union a byte sits in: its start, its end, and its members, with
/// any whitespace around each `|`. A member is a quoted string, a
/// braced table, or a name with its arguments.
fn union_around(text: &str, at: usize) -> Option<(usize, usize, Vec<&str>)> {
    // Back up to the start of the member that holds `at`.
    let mut start = text[..=at].rfind(['{', '"'])?;

    if text[start..].starts_with('"')
        && !text[..start].ends_with([' ', '(', ':', '|', '\n', '<', ','])
    {
        return None;
    }

    // Extend left over `| member` pairs.
    loop {
        let before = text[..start].trim_end();

        if !before.ends_with('|') {
            break;
        }

        let prev_end = before.len() - 1;
        let prev = text[..prev_end].trim_end();
        let prev_start = member_start(text, prev.len())?;
        start = prev_start;
    }

    // Walk right over members and separators.
    let mut members = Vec::new();
    let mut pos = start;

    loop {
        let len = member_len(&text[pos..])?;
        members.push(&text[pos..pos + len]);
        let mut next = pos + len;
        let rest = text[next..].trim_start();

        if !rest.starts_with('|') {
            return Some((start, next, members));
        }

        next += text[next..].len() - rest.len() + 1;
        let gap = text[next..].len() - text[next..].trim_start().len();
        pos = next + gap;
    }
}

/// The length of one union member at the start of `text`.
fn member_len(text: &str) -> Option<usize> {
    if text.starts_with('{') {
        return balanced_len(text);
    }

    if let Some(rest) = text.strip_prefix('"') {
        return rest.find('"').map(|i| i + 2);
    }

    let name: usize = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .map(char::len_utf8)
        .sum();

    if name == 0 {
        return None;
    }

    if text[name..].starts_with('<') {
        let mut depth = 0i32;

        for (k, c) in text[name..].char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;

                    if depth == 0 {
                        return Some(name + k + 1);
                    }
                }
                _ => {}
            }
        }

        return None;
    }

    Some(name)
}

/// The start of the union member that ends at `end`.
fn member_start(text: &str, end: usize) -> Option<usize> {
    let last = text[..end].chars().next_back()?;

    match last {
        '}' => group_start(text, end - 1),

        '"' => text[..end - 1].rfind('"'),

        '>' => {
            let mut depth = 0i32;

            for (k, c) in text[..end].char_indices().rev() {
                match c {
                    '>' => depth += 1,
                    '<' => {
                        depth -= 1;

                        if depth == 0 {
                            let head = &text[..k];
                            let name_len = head
                                .chars()
                                .rev()
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                                .map(char::len_utf8)
                                .sum::<usize>();

                            return Some(k - name_len);
                        }
                    }
                    _ => {}
                }
            }

            None
        }

        _ => {
            let name_len = text[..end]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                .map(char::len_utf8)
                .sum::<usize>();

            (name_len > 0).then_some(end - name_len)
        }
    }
}

/// The length of `pattern` at the start of `text`, when the two agree
/// up to whitespace: a run of it on either side matches any run, or
/// none, on the other.
fn match_loose(text: &str, pattern: &str) -> Option<usize> {
    let t: Vec<(usize, char)> = text.char_indices().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut i, mut j) = (0, 0);

    while j < p.len() {
        if p[j].is_whitespace() {
            while j < p.len() && p[j].is_whitespace() {
                j += 1;
            }

            while i < t.len() && t[i].1.is_whitespace() {
                i += 1;
            }

            continue;
        }

        while i < t.len() && t[i].1.is_whitespace() {
            i += 1;
        }

        if i >= t.len() || t[i].1 != p[j] {
            return None;
        }

        i += 1;
        j += 1;
    }

    Some(t.get(i).map_or(text.len(), |(k, _)| *k))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_chain_method_hover_names_the_receiver_type() {
        let known = Known::default();
        let text = "function Iter.from(xs):filter(function(n) return n > 1 end):map(function(n) return n * 2 end):collect(self: { next: (self: any) -> number? }): number[]";

        assert_eq!(
            fold(text, &known),
            "function Iter:collect(self: Iter<number>): number[]"
        );
        assert_eq!(
            fold("function xs:map(self: read number[]): number[]", &known),
            "function xs:map(self: read number[]): number[]"
        );
    }

    #[test]
    fn a_wrapped_chain_hover_names_the_receiver_type() {
        let known = Known::default();
        let text = "```luau\nfunction Iter.from(self.items:values())\n            :filter(function(i: Item) return i.count > 0 end)\n            :map(function(i: Item) return i.name end):collect(self: { next: (self: any) -> string? }): string[]\n```";

        assert_eq!(
            fold(text, &known),
            "```luau\nfunction Iter:collect(self: Iter<string>): string[]\n```"
        );
        // A self type with no name of its own keeps the head it had.
        assert_eq!(
            fold("function f(x):go(self: { a: number }): ()", &known),
            "function f(x):go(self: { a: number }): ()"
        );
    }

    #[test]
    fn an_interface_and_a_record_alias_read_by_name() {
        let source = "export interface Named as\n    name: string\nend\n\nexport interface Slot extends Named as\n    read id: number\n    count: number\nend\n\nexport type Profile = { name: string, level: number }\n";
        let known = Known {
            shapes: Vec::new(),
            interfaces: interfaces(source),
        };

        assert_eq!(
            fold(
                "local s: Named & { count: number, read id: number }",
                &known
            ),
            "local s: Slot"
        );
        // A module prefix on the base is still the base.
        assert_eq!(
            fold(
                "local s: _m1.Named & { count: number, read id: number }",
                &known
            ),
            "local s: Slot"
        );
        // One table with every field, under an array, keeps the array.
        assert_eq!(
            fold(
                "{ count: number, read id: number } & { name: string }[]",
                &known
            ),
            "Slot[]"
        );
        assert_eq!(
            fold("local all: { level: number, name: string }[]", &known),
            "local all: Profile[]"
        );
    }

    #[test]
    fn a_result_printed_with_its_methods_reads_as_the_result() {
        let known = Known::default();
        let text = "local r: (Result<Profile, any> & { read expect: (self: any, message: string) -> Profile, read is_err: (self: any) -> boolean, read is_ok: (self: any) -> boolean, read map: (self: any) -> any, read map_err: (self: any) -> any, read ok: (self: any) -> Profile?, read unwrap: (self: any) -> Profile, read unwrap_or: (self: any) -> any })[]";

        assert_eq!(fold(text, &known), "local r: Result<Profile, any>[]");
    }

    #[test]
    fn one_array_receiver_spelling() {
        let known = Known::default();

        assert_eq!(
            fold(
                "function Array:len(self: {read Result<Profile, any>}): number",
                &known
            ),
            "function Array:len(self: read Result<Profile, any>[]): number"
        );
    }

    #[test]
    fn a_signal_source_reads_as_signalish() {
        let known = Known::default();
        let text = "function Signal.collect<T...>(source: RBXScriptSignal<T...> | Signal<T...> | { Connect: (self: any, handler: (T...) -> ()) -> any } | { connect: (self: any, handler: (T...) -> ()) -> any }): ((...any) -> T..., SignalConnection)";

        assert_eq!(
            fold(text, &known),
            "function Signal.collect<T...>(source: Signalish<T...>): ((...any) -> T..., SignalConnection)"
        );
    }

    #[test]
    fn a_method_through_a_variable_names_the_type() {
        let known = Known::default();

        assert_eq!(
            fold("function self:markup(self: Item): number", &known),
            "function Item:markup(self: Item): number"
        );
        // No name to put there: the print stays as it came.
        assert_eq!(
            fold("function a:price(self: any): number", &known),
            "function a:price(self: any): number"
        );
    }

    #[test]
    fn an_iter_return_reads_past_the_nested_arrow() {
        let known = Known::default();
        let body = "{ collect: (self: { next: (self: any) -> U? }) -> Array<U>, next: (self: { next: (self: any) -> U? }) -> U?, take_while: (self: { next: (self: any) -> U? }, f: (U) -> boolean) -> any }";

        assert_eq!(name_of_body(body, &known), Some("Iter<U>".to_string()));
    }

    #[test]
    fn a_heap_is_not_a_queue() {
        let known = Known::default();
        let heap = "{ read __less: (number, number) -> boolean, clear: (self: t2) -> (), is_empty: (self: t2) -> boolean, len: (self: t2) -> number, peek: (self: t2) -> number?, pop: (self: t2) -> number?, push: (self: t2, value: number) -> (), to_array: (self: t2) -> t1 }";
        let queue = "{ clear: (self: t2) -> (), is_empty: (self: t2) -> boolean, len: (self: t2) -> number, peek: (self: t2) -> number?, pop: (self: t2) -> number?, push: (self: t2, value: number) -> () }";

        assert_eq!(name_of_body(heap, &known), Some("Heap<number>".to_string()));
        assert_eq!(
            name_of_body(queue, &known),
            Some("Queue<number>".to_string())
        );
    }

    #[test]
    fn a_mapped_record_keeps_the_mapped_name() {
        let known = Known {
            interfaces: interfaces("type Ent = { id: number, name: string }\n"),
            shapes: Vec::new(),
        };

        // The alias marks no field, so the `Readonly` print is not it,
        // and the mapped fold names what the source wrote.
        assert_eq!(
            fold(
                "Property id of table '{ read id: number, read name: string }' is read-only",
                &known
            ),
            "Property id of table 'Readonly<Ent>' is read-only"
        );
        // A literal contrasted against another type keeps its record.
        assert_eq!(
            fold(
                "Table type '{ id: number, name: string }' not compatible with type 'Entity'",
                &known
            ),
            "Table type '{ id: number, name: string }' not compatible with type 'Entity'"
        );
        // Elsewhere the alias still names the shape.
        assert_eq!(
            fold("local e: { id: number, name: string }", &known),
            "local e: Ent"
        );
    }

    #[test]
    fn a_future_method_names_the_future() {
        let known = Known::default();

        assert_eq!(
            fold(
                "function Awaitable:cancel(self: Awaitable<number>): ()",
                &known
            ),
            "function Future:cancel(self: Future<number>): ()"
        );
    }

    #[test]
    fn a_record_alias_reads_by_name_with_no_export() {
        let known = Known {
            interfaces: interfaces("type Profile = { name: string, level: number }\n"),
            shapes: Vec::new(),
        };
        let printed = "local all: {\n        level: number,\n        name: string\n    }[]";

        assert_eq!(fold(printed, &known), "local all: Profile[]");
    }

    #[test]
    fn a_cut_result_print_reads_as_a_result() {
        let known = Known::default();
        let arms = "{ read _1: Record, read __err: string, ... 3 more ... } \
                    | { read _1: string, read __err: string, ... 3 more ... }";

        assert_eq!(fold(arms, &known), "Result<Record, string>");
        assert_eq!(
            fold(&format!("Array<{arms}>"), &known),
            "Result<Record, string>[]"
        );
        // A whole print keeps the tag, and the pairing fold reads it.
        assert_eq!(
            fold("{ read _1: number, read __err: string }", &known),
            "{ read _1: number, read __err: string }"
        );
    }

    #[test]
    fn the_two_arms_of_a_result_read_as_the_result() {
        let known = Known::default();

        assert_eq!(
            fold(
                "local head: ResultOk<number, string> | Result<number, string> | ResultErr<number, string>",
                &known
            ),
            "local head: Result<number, string>"
        );
        assert_eq!(
            fold("ResultErr<Item, string>", &known),
            "Result<Item, string>"
        );
    }

    #[test]
    fn a_not_nil_refinement_reads_as_the_type_it_refines() {
        let known = Known::default();

        assert_eq!(fold("intersect<T, ~nil>", &known), "T");
        assert_eq!(fold("(a & ~nil) | { }", &known), "(a) | { }");
        assert_eq!(fold("Item & ~nil", &known), "Item");
        assert_eq!(fold("intersect<A, ~nil>[]", &known), "A[]");
        assert_eq!(fold("intersect<Item, Named>", &known), "Item & Named");
    }

    use super::*;

    fn known() -> Known {
        Known {
            interfaces: Vec::new(),
            shapes: vec![
                Shape::Struct {
                    name: "Saber".into(),
                    fields: vec![
                        ("id".into(), false),
                        ("cost".into(), false),
                        ("color".into(), false),
                    ],
                },
                Shape::Enum {
                    name: "Rarity".into(),
                    variants: vec![("Common".into(), vec![]), ("Rare".into(), vec![])],
                },
                Shape::Enum {
                    name: "Boost".into(),
                    variants: vec![
                        ("None".into(), vec![]),
                        ("Coins".into(), vec!["number".into()]),
                    ],
                },
            ],
        }
    }

    #[test]
    fn a_struct_value_reads_by_name() {
        let text = "```luau\nlocal found: t3? where t1 = {\n    [number]: number,\n    concat: (self: t1, other: t1) -> t1,\n    push: (self: t1, ...number) -> ()\n} ; t2 = {\n    __index: t2,\n    __new: (f: { color: t1, cost: number, id: string }) -> t3\n} ; t3 = { @metatable t2, {\n    read color: t1,\n    read cost: number,\n    read id: string\n} }\n```";
        assert_eq!(fold(text, &known()), "```luau\nlocal found: Saber?\n```");
    }

    #[test]
    fn arrays_and_maps_read_by_name() {
        let text = "local xs: {t1} where t1 = { [number]: string, concat: (self: t1) -> t1, push: (self: t1) -> () }";
        assert_eq!(fold(text, &known()), "local xs: {string[]}");
        let text = "local m: t1 where t1 = { get: (self: t1, key: string) -> number?, set: (self: t1, key: string, value: number) -> (), entries: (self: t1) -> () }";
        assert_eq!(fold(text, &known()), "local m: HashMap<string, number>");
        let text = "local s: t1 where t1 = { add: (self: t1, value: string) -> boolean, has: (self: t1, value: string) -> boolean, union: (self: t1, other: t1) -> t1 }";
        assert_eq!(fold(text, &known()), "local s: Set<string>");
    }

    #[test]
    fn an_array_alias_of_an_array_folds_to_the_sugar() {
        let mut text = ": Array<number[]>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": number[][]");
        let mut text = ": Array<Array<number>>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": number[][]");
        assert_eq!(
            fold(": Array<Array<number> | Array<number>>", &Known::default()),
            ": number[][]"
        );
        let mut text = ": Array<number | string>".to_string();
        fold_array_alias(&mut text);
        assert_eq!(text, ": Array<number | string>");
    }

    #[test]
    fn a_private_view_hint_reads_as_the_struct() {
        let text = ": Swinger & Swinger__private & { last: number, scope: Scope }";
        assert_eq!(fold(text, &Known::default()), ": Swinger");
        let known = Known {
            interfaces: Vec::new(),
            shapes: alloy::declarations::shapes(
                "export struct Swinger as\n    read requested: Signal<> = Signal.new()\n    private last: number = 0\n    private scope: Scope = Scope.new()\nend\n",
            ),
        };
        assert_eq!(fold(text, &known), ": Swinger");
    }

    #[test]
    fn a_nested_array_of_two_array_types_reads_once() {
        let text = "local g: t1 where t1 = {\n    [number]: t2 | t3,\n    concat: (self: {read t2 | t3}, other: t1) -> t1,\n    push: (self: t1, value: t2 | t3) -> ()\n} ; t2 = {\n    [number]: number,\n    concat: (self: {read number}, other: t2) -> t2,\n    push: (self: t2, value: number) -> ()\n} ; t3 = {\n    [number]: number,\n    concat: (self: {read number}, other: t3) -> t3,\n    push: (self: t3, value: number) -> ()\n}";
        assert_eq!(fold(text, &known()), "local g: number[][]");
        let text = "local g: t1 where t1 = { [number]: number | string, concat: (self: t1) -> t1, push: (self: t1) -> () }";
        assert_eq!(fold(text, &known()), "local g: (number | string)[]");
    }

    #[test]
    fn enums_and_results_read_by_name() {
        let text = "function saber(id: string): { read _1: Saber, tag: \"Ok\", unwrap: (self: any) -> Saber } | { read _1: string, tag: \"Err\", unwrap: (self: any) -> Saber }";
        assert_eq!(
            fold(text, &known()),
            "function saber(id: string): Result<Saber, string>"
        );
        let text = "local r: \"Common\" | \"Rare\"";
        assert_eq!(fold(text, &known()), "local r: Rarity");
        let text = "local b: \"None\" | { _1: number, tag: \"Coins\" }";
        assert_eq!(fold(text, &known()), "local b: Boost");
    }

    #[test]
    fn a_full_view_binding_reads_as_the_struct() {
        let text = "```luau\nlocal self: t1 where t1 = t2 & {\n    hit: (self: t1) -> ()\n} & {\n    last: number\n} ; t2 = { @metatable t3, {\n    read id: string,\n    read cost: number,\n    read color: t4\n} } ; t3 = {\n    __index: t3,\n    __new: (f: { id: string }) -> t2\n} ; t4 = {\n    [number]: number,\n    concat: (self: t4, other: t4) -> t4,\n    push: (self: t4, ...number) -> ()\n}\n```";
        assert_eq!(fold(text, &known()), "```luau\nlocal self: Saber\n```");
    }

    #[test]
    fn a_guarded_field_and_a_method_binding_still_name_the_struct() {
        let text = "```luau\nlocal self: t2 where t1 = (self: t2) -> () ; t2 = { @metatable t3, {\n    read color: t4,\n    read cost: number,\n    read id: true,\n    write id: string,\n    mark: t1\n} } ; t3 = {\n    __index: t3\n} ; t4 = {\n    [number]: number,\n    concat: (self: t4, other: t4) -> t4,\n    push: (self: t4, ...number) -> ()\n}\n```";
        assert_eq!(fold(text, &known()), "```luau\nlocal self: Saber\n```");
    }

    #[test]
    fn error_members_leave_a_union() {
        let mut text =
            "local v: (userdata & ~Instance)\n    | *error-type*\n    | boolean".to_string();
        fold_narrowed_primitives(&mut text);
        assert_eq!(text, "local v: userdata & ~Instance\n    | boolean");
    }

    #[test]
    fn a_mapped_type_over_a_struct_reads_as_its_alias() {
        let mut known = known();
        known.shapes.push(Shape::Alias {
            name: "Snapshot".into(),
            target: "Readonly<Saber>".into(),
        });
        let text = "```luau\nlocal first: {\n    read color: t1,\n    read cost: number,\n    read id: string\n} where t1 = {\n    [number]: number,\n    concat: (self: t1, other: t1) -> t1,\n    push: (self: t1, ...number) -> ()\n}\n```";
        assert_eq!(fold(text, &known), "```luau\nlocal first: Snapshot\n```");
        let partial = "local part: {\n    color: t1?,\n    cost: number?,\n    id: string?\n} where t1 = {\n    [number]: number,\n    concat: (self: t1, other: t1) -> t1,\n    push: (self: t1, ...number) -> ()\n}";
        assert_eq!(fold(partial, &known), "local part: Partial<Saber>");
    }

    #[test]
    fn a_resolved_head_drops_a_clause_it_cannot_parse() {
        let text = "```luau\nlocal self: t1 where t1 = { @metatable t2, {\n    read color: t3,\n    read cost: number,\n    read id: string\n} } ; t3 = {\n    [number]: number,\n    concat: (self: t3, other: t3) -> t3,\n    push: (self: t3, ...number) -> ()\n} ; t2 = <T>(x: T) -> T ; t4 = {\n    __new: (f: {}) -> t1\n}\n```";
        assert_eq!(fold(text, &known()), "```luau\nlocal self: Saber\n```");
    }

    #[test]
    fn a_remote_reads_by_name_in_place() {
        let text = "```luau\nlocal BuySaber: {\n    call: (id: string) -> Future<any>,\n    fire: (id: string) -> (),\n    fire_all: (id: string) -> (),\n    fire_except: (except: Player, id: string) -> (),\n    instance: Instance?,\n    on: (handler: (sender: Player, id: string) -> ()) -> RBXScriptConnection,\n    on_ratelimited: (handler: (player: Player) -> ()) -> (),\n    once: (handler: (sender: Player, id: string) -> ()) -> RBXScriptConnection,\n    spec: any,\n    wait: () -> Future<any>\n}\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nlocal BuySaber: Remote\n```"
        );
    }

    #[test]
    fn a_result_of_two_groups_reads_by_name() {
        let text = "local config: ({\n    read _1: string,\n    read __err: string,\n    read __ok: number,\n    tag: \"Err\",\n    read trace: string?\n} & {\n    read expect: (self: {\n        read tag: string\n    }, message: string) -> number,\n    read map: <U>(self: { read tag: string }, f: (number) -> U) -> ({ read _1: U, read __err: string, read __ok: U, tag: \"Ok\", read trace: string? } & { read ok: (self: any) -> U? }) | ({ read _1: string, read __err: string, read __ok: U, tag: \"Err\", read trace: string? } & { read ok: (self: any) -> U? })\n}) | ({\n    read _1: number,\n    read __err: string,\n    read __ok: number,\n    tag: \"Ok\",\n    read trace: string?\n} & {\n    read expect: (self: {\n        read tag: string\n    }, message: string) -> number\n})";
        assert_eq!(
            fold(text, &Known::default()),
            "local config: Result<number, string>"
        );
        let sig = "function open(player: Player): Future<(ResultMethods<Session, string> & { read _1: Session, read __err: string, read __ok: Session, tag: \"Ok\", read trace: string? }) | (ResultMethods<Session, string> & { read _1: string, read __err: string, read __ok: Session, tag: \"Err\", read trace: string? })>";
        assert_eq!(
            fold(sig, &Known::default()),
            "function open(player: Player): Future<Result<Session, string>>"
        );
        let multi = "```luau\nfunction open(player: Player): Future<(ResultMethods<Session, string> & {\n    read _1: Session,\n    read __err: string,\n    read __ok: Session,\n    tag: \"Ok\",\n    read trace: string?\n}) | (ResultMethods<Session, string> & {\n    read _1: string,\n    read __err: string,\n    read __ok: Session,\n    tag: \"Err\",\n    read trace: string?\n})>\n```";
        assert_eq!(
            fold(multi, &Known::default()),
            "```luau\nfunction open(player: Player): Future<Result<Session, string>>\n```"
        );
        let prefixed = "function open(player: Player): __alloy.Future<(__alloy.ResultMethods<Session, string> & { read _1: Session, read __err: string, read __ok: Session, tag: \"Ok\", read trace: string? }) | (__alloy.ResultMethods<Session, string> & { read _1: string, read __err: string, read __ok: Session, tag: \"Err\", read trace: string? })>";
        assert_eq!(
            fold(prefixed, &Known::default()),
            "function open(player: Player): __alloy.Future<Result<Session, string>>"
        );
        // `ResultOk` is the arm the emit names; a source writes
        // `Result`, so one arm alone reads as the Result it belongs to.
        let one = "local r: ResultMethods<number, string> & { read _1: number, read __err: string, read __ok: number, tag: \"Ok\", read trace: string? }";
        assert_eq!(
            fold(one, &Known::default()),
            "local r: Result<number, string>"
        );
    }

    #[test]
    fn a_broken_line_union_of_results_folds() {
        let text = "```luau\nlocal function report(result: (ResultMethods<any, string> & { read _1: any, read __err: string, read __ok: any, tag: \"Ok\", read trace: string? })\n    | (ResultMethods<any, string> & { read _1: string, read __err: string, read __ok: any, tag: \"Err\", read trace: string? })): ()\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nlocal function report(result: Result<any, string>): ()\n```"
        );
    }

    #[test]
    fn a_symbol_and_a_nested_enum_read_by_name() {
        let known = Known {
            interfaces: Vec::new(),
            shapes: vec![
                Shape::Enum {
                    name: "Shape".into(),
                    variants: vec![
                        ("Circle".into(), vec!["number".into()]),
                        ("Rect".into(), vec!["number".into(), "number".into()]),
                    ],
                },
                Shape::Enum {
                    name: "Event".into(),
                    variants: vec![
                        ("Spawn".into(), vec!["Player".into(), "Vector3".into()]),
                        ("Hit".into(), vec!["Player".into(), "Shape".into()]),
                        ("Leave".into(), vec!["Player".into()]),
                    ],
                },
            ],
        };
        let text = "local function describe(event: { _1: Player, _2: Vector3, tag: \"Spawn\" } | { _1: Player, _2: { _1: number, _2: number, tag: \"Rect\" } | { _1: number, tag: \"Circle\" }, tag: \"Hit\" } | { _1: Player, tag: \"Leave\" }): string";
        assert_eq!(
            fold(text, &known),
            "local function describe(event: Event): string"
        );
        let symbol = "local CHILDREN: { @metatable {\n        __tostring: (...any) -> string\n    },\n    {  } }";
        assert_eq!(fold(symbol, &Known::default()), "local CHILDREN: Symbol");
    }

    #[test]
    fn an_enum_inside_a_type_argument_reads_by_name() {
        let boost = Known {
            interfaces: Vec::new(),
            shapes: vec![Shape::Enum {
                name: "Boost".into(),
                variants: vec![
                    ("None".into(), vec![]),
                    ("Coins".into(), vec!["number".into()]),
                    ("Strength".into(), vec!["number".into()]),
                ],
            }],
        };
        let inside = "function buy(id: string): Result<\"None\" | { _1: number, tag: \"Coins\" } | { _1: number, tag: \"Strength\" }, string>";
        assert_eq!(
            fold(inside, &boost),
            "function buy(id: string): Result<Boost, string>"
        );
    }

    #[test]
    fn a_union_loses_its_repeated_members() {
        let text = "```luau\nlocal value: number | number\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nlocal value: number\n```"
        );
        let text = "```luau\nlocal v: string | number\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nlocal v: string | number\n```"
        );
    }

    #[test]
    fn a_cut_hint_still_names_an_array() {
        let text = ": t1 where t1 = { [number]: any, concat: (read any[], t1) -> t1, ...";
        assert_eq!(fold(text, &Known::default()), ": any[]");
        let text = "local xs: t1? where t1 = { [number]: number | string, len: (read (number | string)[]) -> number, ...";
        assert_eq!(
            fold(text, &Known::default()),
            "local xs: (number | string)[]?"
        );
    }

    #[test]
    fn a_hint_of_a_mapped_array_reads_as_an_array() {
        let text = ": t1 where t1 = { [number]: any, concat: (read any[], t1) -> t1, contains: (read any[], any) -> boolean, filter: (read any[], (any, number) -> boolean) -> t1, find: (read any[], (any, number) -> boolean) -> any?, find_index: (read any[], (any, number) -> boolean) -> number?, first: (read any[]) -> any?, for_each: (read any[], (any, number) -> ()) -> (), index_of: (read any[], any) -> number?, is_empty: (read any[]) -> boolean, join: (read any[], string?) -> string, last: (read any[]) -> any?, len: (read any[]) -> number, map: <U>(read any[], (any, number) -> U) -> any, pop: (read any[]) -> any?, push: (read any[], ...any) -> (), reduce: <U>(read any[], (U, any, number) -> U, U) -> U, reverse: (read any[]) -> t1, slice: (read any[], number, number?) -> t1, sort_by: (read any[], (any, any) -> boolean) -> t1 }";
        assert_eq!(fold(text, &Known::default()), ": any[]");
    }

    #[test]
    fn a_union_of_one_type_reads_once() {
        let k = Known::default();
        assert_eq!(fold(": Array<number[] | number[]>", &k), ": number[][]");
        assert_eq!(fold("local n: number | number", &k), "local n: number");
        assert_eq!(
            fold("f: (a: number | string) -> (number | number)", &k),
            "f: (a: number | string) -> (number)"
        );
        assert_eq!(
            fold("x: { a: number | number } | nil", &k),
            "x: { a: number } | nil"
        );
    }

    #[test]
    fn a_future_of_nothing_reads_as_unit() {
        let text = "```luau\nfunction tick(): Future<nil>\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nfunction tick(): Future<()>\n```"
        );
    }

    #[test]
    fn the_private_view_reads_as_the_struct() {
        let text = "function Session__private.mark(self: Session & Session__private & { dirty: boolean, scope: Scope }): ()";
        assert_eq!(
            fold(text, &Known::default()),
            "function Session.mark(self: Session): ()"
        );
    }

    #[test]
    fn nested_results_fold_from_the_inside() {
        let text = "f: { read _1: any, map: (self: any) -> { read _1: any, tag: \"Ok\" } | { read _1: string, tag: \"Err\" }, tag: \"Ok\" } | { read _1: string, tag: \"Err\" }";
        assert_eq!(fold(text, &Known::default()), "f: Result<any, string>");
    }

    #[test]
    fn mapped_results_and_temp_receivers_read_plainly() {
        let text = "local v: { read _1: number | string, tag: \"Ok\" | \"Err\", unwrap: (self: any) -> number }";
        assert_eq!(
            fold(text, &Known::default()),
            "local v: Result<number, string>"
        );
        assert_eq!(
            fold("function _1:unwrap(self: any): Saber", &Known::default()),
            "function unwrap(self: any): Saber"
        );
    }

    #[test]
    fn an_unknown_shape_keeps_its_clause() {
        let text = "local q: t1 where t1 = { weird: (self: t1) -> number }";
        assert_eq!(fold(text, &known()), text);
    }
}

#[cfg(test)]
mod pcall_tests {
    use super::*;

    #[test]
    fn a_function_returning_a_result_folds() {
        let text = "```luau\nfunction Result.pcall(f: (...any) -> (...any), ...: any): { read _1: any, expect: (self: any, message: string) -> any, is_err: (self: any) -> boolean, is_ok: (self: any) -> boolean, map: (self: any, f: (any) -> any) -> Result<any, string>, map_err: (self: any, f: (string) -> any) -> Result<any, any>, ok: (self: any) -> any?, tag: \"Ok\", trace: string?, unwrap: (self: any) -> any, unwrap_or: (self: any, default: any) -> any } | { read _1: string, expect: (self: any, message: string) -> any, is_err: (self: any) -> boolean, is_ok: (self: any) -> boolean, map: (self: any, f: (any) -> any) -> Result<any, string>, map_err: (self: any, f: (string) -> any) -> Result<any, any>, ok: (self: any) -> any?, tag: \"Err\", trace: string?, unwrap: (self: any) -> any, unwrap_or: (self: any, default: any) -> any }\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nfunction Result.pcall(f: (...any) -> (...any), ...: any): Result<any, string>\n```"
        );
    }
}

#[cfg(test)]
mod array_clause_tests {
    use super::*;

    #[test]
    fn a_cut_array_clause_reads_by_its_element() {
        let text = "Expected this to be\n\t'string[]'\nbut got\n\t't1 where t1 = { [number]: number, concat: (read number[], t1) -> t1, contains: (read number[], number) -> b";
        assert_eq!(
            fold(text, &Known::default()),
            "Expected this to be\n\t'string[]'\nbut got\n\t'number[]'"
        );
    }

    #[test]
    fn a_whole_clause_in_a_diagnostic_keeps_the_closing_quote() {
        let text = "Expected this to be 'string[]' but got 't2 where t1 = { [number]: number, concat: (read number[], t1) -> t1, push: (read number[], ...number) -> () } ; t2 = t1'";
        assert_eq!(
            fold(text, &Known::default()),
            "Expected this to be 'string[]' but got 'number[]'"
        );
    }

    #[test]
    fn a_clause_over_several_lines_folds_too() {
        let text = "```luau\nlocal rs: t1 where t1 = {\n    [number]: Result<number, any>,\n    concat: (self: {read Result<number, any>}, other: t1) -> t1,\n    len: (self: {read ResultErr<number, any>... *TRUNCATED*\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nlocal rs: Result<number, any>[]\n```"
        );
    }

    #[test]
    fn a_type_argument_list_keeps_its_comma() {
        assert_eq!(
            member_parts("{ [number]: Result<number, any>, len: (self: t1) -> number }"),
            vec![
                " [number]: Result<number, any>",
                " len: (self: t1) -> number "
            ]
        );
    }

    #[test]
    fn an_optional_head_keeps_its_question_mark() {
        let text =
            "local xs: t1? where t1 = { [number]: string, push: (self: t1, value: string) -> ()";
        assert_eq!(fold(text, &Known::default()), "local xs: string[]?");
    }

    #[test]
    fn a_callback_mismatch_names_the_parameter_and_not_the_type_pack() {
        let text = "Expected this to be '(number, number) -> string' but got '(string) -> string'; it takes the 1st entry in the type pack is `string` in the latter type and `number` in the former type, and `string` is not a supertype of `number`";
        assert_eq!(
            friendly_text(text),
            "Expected this to be '(number, number) -> string' but got '(string) -> string': its 1st parameter is `string` where `number` is wanted"
        );
    }

    #[test]
    fn the_same_sentence_reads_alike_over_several_lines() {
        let text = "Expected this to be\n\t'(Player, number) -> ()'\nbut got\n\t'(Player, string) -> ()'; \nit takes the 2nd entry in the type pack is `string` in the latter type and `number` in the former type";
        assert_eq!(
            friendly_text(text),
            "Expected this to be '(Player, number) -> ()' but got '(Player, string) -> ()': its 2nd parameter is `string` where `number` is wanted"
        );
    }

    #[test]
    fn a_report_about_a_key_the_emit_writes_is_dropped() {
        let line = "local Build(first_name) = Job.Build(\"tower\")";
        assert!(names_the_emit_key(
            "Type 'nil' does not have key 'tag'",
            line
        ));
        assert!(names_the_emit_key(
            "Key '_1' not found in table 'Job'",
            line
        ));
        // The source that writes the name keeps its report.
        assert!(!names_the_emit_key(
            "Type 'Row' does not have key 'tag'",
            "print(row.tag)"
        ));
        assert!(!names_the_emit_key(
            "Type 'Row' does not have key 'name'",
            line
        ));
    }

    #[test]
    fn a_duplicate_the_lowering_made_is_dropped() {
        let expanded = "            <Frame Name=\"Stats\" ClassName=\"panel bg-orange-700\">";
        assert!(duplicate_only_in_the_emit(
            "Table field 'BackgroundColor3' is a duplicate; previously defined at line 65",
            expanded
        ));
        assert!(!duplicate_only_in_the_emit(
            "Table field 'hp' is a duplicate; previously defined at line 4",
            "local t = { hp = 1, hp = 2 }"
        ));
    }

    #[test]
    fn a_table_beside_an_array_says_which_is_which() {
        assert_eq!(
            friendly_text("Expected this to be 'Future<{Profile}>', but got 'Future<Profile[]>'"),
            "Expected this to be 'Future<{Profile}>', but got 'Future<Profile[]>'; a `{ Profile }` is a plain table, and `Profile[]` is an Array"
        );
        assert_eq!(
            friendly_text("Expected this to be 'number', but got 'string'"),
            "Expected this to be 'number', but got 'string'"
        );
    }

    #[test]
    fn a_generic_with_no_solution_reads_as_the_values_that_disagree() {
        assert_eq!(
            friendly_text(
                "TypeError: No valid instantiation could be inferred for generic type parameter T. It was expected to be at least: number | nil and at most: number & nil but these types are not compatible with one another."
            ),
            "TypeError: these values give `T` no one type; make them agree, or write `T` out"
        );
    }

    #[test]
    fn the_checkers_step_limit_says_it_is_the_checker() {
        assert_eq!(
            friendly_text(
                "TypeError: Code is too complex to typecheck! Consider simplifying the code around this area"
            ),
            "TypeError: the checker reached its limit on this expression; it says nothing about the code. Name a step in a local, or annotate the result"
        );
    }

    #[test]
    fn a_pack_that_is_no_callback_keeps_the_head_alone() {
        let text = "Expected this to be 'number' but got 'string'; it takes the 1st entry in the type pack is `string` in the latter type and `number` in the former type";
        assert_eq!(
            friendly_text(text),
            "Expected this to be 'number' but got 'string'"
        );
    }
}

#[cfg(test)]
mod dbg2 {
    use super::*;

    #[test]
    fn debug_enum_payload() {
        let known = Known {
            shapes: vec![
                Shape::Struct {
                    name: "Scope2".into(),
                    fields: vec![("tag".into(), false)],
                },
                Shape::Enum {
                    name: "Shape".into(),
                    variants: vec![
                        ("Circle".into(), vec!["number".into()]),
                        ("Rect".into(), vec!["number".into(), "number".into()]),
                        ("Empty".into(), vec![]),
                    ],
                },
            ],
            interfaces: Vec::new(),
        };
        let text = "Key 'area' is missing from 'string' in the type '\"Empty\" | { _1: number, tag: \"Circle\" } | { _1: number, _2: number, tag: \"Rect\" }'";
        println!("OUT: {:?}", fold(text, &known));
        println!(
            "NOB: {:?}",
            name_of_body("{ _1: number, tag: \"Circle\" }", &known)
        );
    }
}
