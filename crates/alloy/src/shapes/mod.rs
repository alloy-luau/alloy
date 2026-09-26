//! The checker prints a struct value as the table it is at runtime,
//! `t2 where t1 = { __index: t1, ... } ; t2 = { @metatable t1, { x: number } }`,
//! an array as its method table, and a unit enum as a union of strings.
//! A reader wrote `Vec2`, `number[]`, and `Color`; this folds the print
//! back to those names, from the shapes the workspace declares.

mod arrays;
mod enums;
mod messages;
mod metatables;
mod naming;
mod receivers;
mod results;
mod strings;

use crate::declarations::Shape;
use serde_json::Value;

use arrays::{
    fold_array_alias, fold_array_parens, fold_cut_array, fold_generic_arity, fold_iter_shapes,
    fold_read_arrays,
};
use enums::{fold_enum_unions, fold_enums, fold_variant_tables};
use metatables::{fold_empty_metatables, fold_metatable_groups};
use naming::name_of_body;
use receivers::{fold_call_receivers, fold_temp_receiver};
use results::{
    drop_result_methods, fold_cut_results, fold_inline_result_methods, fold_lite_results,
    fold_result_aliases, fold_results, fold_tagged_results,
};
use strings::{
    balanced_len, group_len, head_of, match_loose, member_len, member_parts, members,
    outside_angles, split_at_depth, split_union,
};

// The diagnostics rewrite calls these, in `crate::typecheck` and in
// `alloy-lsp`.
pub use messages::{
    duplicate_only_in_the_emit, friendly_text, names_only_the_emit, names_the_emit_key,
    plain_table_hint, table_beside_array,
};

/// The shapes a fold may name, over every open document.
#[derive(Default)]
pub struct Known {
    pub shapes: Vec<Shape>,
    pub interfaces: Vec<Interface>,
    /// Every namespace member: the name the emit writes and the path
    /// the source wrote. `Math_Vec2` reads as `Math.Vec2`.
    pub namespaces: Vec<(String, String)>,
    /// The plain `local X = { }` tables with their members. The
    /// analyzer has no name for one, so a print of the whole shape as
    /// `self` reads back as `typeof(X)`.
    pub tables: Vec<(String, Vec<String>)>,
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
    /// Whether a `trait` declared it. The emit marks every method of a
    /// trait `read`, so the printed record reads as a mapped type over
    /// the name; the source wrote the name alone.
    pub is_trait: bool,
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
        // The word opens a declaration when nothing but `export`,
        // `global`, or `local` stands before it on its line. Trimming
        // the text back to the previous line's last byte reads a `type`
        // in the middle of one as a declaration.
        let raw = &rest[..i];
        let lead = raw[raw.rfind('\n').map_or(0, |k| k + 1)..].trim();
        let opens = matches!(lead, "" | "export" | "global" | "local");
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
                is_trait: false,
            });
        }
    }

    out
}

/// The interfaces and the traits a source declares. A trait is a named
/// record too: the emit writes `type Ord = { read cmp: ... }`, and the
/// checker prints that record wherever a bound on a type parameter
/// reaches it.
fn declared_interfaces(source: &str) -> Vec<Interface> {
    let mut out: Vec<Interface> = Vec::new();
    let mut open: Option<Interface> = None;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text
            .strip_prefix("trait ")
            .or_else(|| text.strip_prefix("export trait "))
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if let Some(previous) = open.take() {
                out.push(previous);
            }

            if !name.is_empty() {
                open = Some(Interface {
                    name,
                    bases: Vec::new(),
                    fields: Vec::new(),
                    alias: false,
                    is_trait: true,
                });
            }

            continue;
        }

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
                is_trait: false,
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

        // A trait writes its members as `function name(...)`, and the
        // emit gives each one a key of that name.
        if let Some(rest) = head
            .trim_start_matches("private ")
            .trim_start_matches("public ")
            .strip_prefix("function ")
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if !name.is_empty() {
                current.fields.push(name);
            }

            continue;
        }

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
        .flat_map(member_parts)
        .any(|m| {
            let text = m.trim();

            text.starts_with("read ") || text.starts_with("write ")
        })
}

/// Every backquoted type with an intersection of one repeated member
/// collapsed to that member.
///
/// `Result2` reads as its two tag tables in a union, intersected with
/// the methods outside it. When both sides of a comparison fold to the
/// same text, the checker prints `(Result<T, E>) & Result<T, E>`, which
/// says the same thing twice and reads as if the two differ.
fn fold_intersection_dupes(text: &mut String) {
    if !text.contains(" & ") {
        return;
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();

    // The checker quotes a type with a backtick in one sentence and an
    // apostrophe in another, so both carry a type to collapse.
    while let Some(open) = rest.find(['`', '\'']) {
        let quote = rest[open..].chars().next().unwrap_or('`');
        let after = &rest[open + quote.len_utf8()..];
        let Some(close) = after.find(quote) else {
            break;
        };

        out.push_str(&rest[..=open]);
        out.push_str(&collapse_intersection(&after[..close]));
        out.push(quote);
        rest = &after[close + quote.len_utf8()..];
    }

    out.push_str(rest);
    *text = out;
}

/// One member when every member of an intersection is the same type,
/// whatever parentheses the checker wrapped them in. The text itself
/// otherwise.
fn collapse_intersection(text: &str) -> String {
    let parts = split_intersection(text);

    if parts.len() < 2 {
        return text.to_string();
    }

    let bare = |p: &str| {
        let p = p.trim();

        match p.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            // Only a whole group, never `(a) & (b)` read as one.
            Some(inner) if split_intersection(inner).len() == 1 => inner.trim().to_string(),
            _ => p.to_string(),
        }
    };

    let first = bare(parts[0]);

    match parts.iter().all(|p| bare(p) == first) {
        true => first,
        false => text.to_string(),
    }
}

#[cfg(test)]
mod intersection_tests {
    use super::collapse_intersection;

    #[test]
    fn one_repeated_member_collapses() {
        assert_eq!(
            collapse_intersection("(Result<string, any>) & Result<string, any>"),
            "Result<string, any>"
        );
        assert_eq!(collapse_intersection("A & A"), "A");
    }

    #[test]
    fn members_that_differ_stay() {
        assert_eq!(collapse_intersection("A & B"), "A & B");
        assert_eq!(
            collapse_intersection("Saber & { hp: number }"),
            "Saber & { hp: number }"
        );
    }

    #[test]
    fn a_lone_type_is_itself() {
        assert_eq!(collapse_intersection("number"), "number");
    }

    #[test]
    fn an_inner_ampersand_is_no_split() {
        // The `&` sits inside the braces, so this is one member.
        assert_eq!(
            collapse_intersection("{ f: (A & B) -> () }"),
            "{ f: (A & B) -> () }"
        );
    }
}

/// The members of an intersection at depth zero.
fn split_intersection(text: &str) -> Vec<&str> {
    split_at_depth(text, " & ", false)
        .into_iter()
        .map(str::trim)
        .collect()
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
                || s.contains("Result3<")
                || known
                    .namespaces
                    .iter()
                    .any(|(emitted, _)| s.contains(emitted.as_str()))
            {
                *s = fold(s, known);
            }
        }

        Value::Array(items) => items.iter_mut().for_each(|i| fold_value(i, known)),

        Value::Object(map) => map.values_mut().for_each(|v| fold_value(v, known)),

        _ => {}
    }
}

/// A print with the defaults its declaration gives filled in:
/// `Pair<number>` of `struct Pair<A, B = string>` reads
/// `Pair<number, string>`, and an enum the same way. The emit writes
/// the arguments the source wrote and Luau fills the rest from the
/// alias, so the print names fewer arguments than the type carries.
pub fn fill_generic_defaults(text: &str, shapes: &[Shape]) -> String {
    let mut out = String::new();
    let mut at = 0;

    while let Some(rel) = text[at..].find('<') {
        let open = at + rel;
        let name_start = text[..open]
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |p| p + 1);
        let name = &text[name_start..open];
        // `Pair<<number>>` is the source's own form, where the emit has
        // yet to run; its arguments are already the ones written.
        let nested = text[open + 1..].starts_with('<');
        let params = shapes.iter().find_map(|s| match s {
            Shape::Struct {
                name: held,
                generics,
                ..
            }
            | Shape::Enum {
                name: held,
                generics,
                ..
            } if held == name && !nested => Some(generics),

            _ => None,
        });
        let (Some(params), Some(close)) = (params, closing_angle(text, open)) else {
            out.push_str(&text[at..open + 1]);
            at = open + 1;

            continue;
        };
        let written = top_level_parts(&text[open + 1..close]);
        let mut filled: Vec<String> = written.iter().map(|p| p.trim().to_string()).collect();

        for param in params.iter().skip(filled.len()) {
            match param.split_once('=') {
                Some((_, default)) => filled.push(default.trim().to_string()),

                None => break,
            }
        }

        out.push_str(&text[at..open]);
        out.push('<');
        out.push_str(&filled.join(", "));
        out.push('>');
        at = close + 1;
    }

    out.push_str(&text[at..]);
    out
}

/// The `>` that closes the `<` at `open`, counting the pairs between.
fn closing_angle(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;

    for (i, c) in text[open..].char_indices() {
        match c {
            '<' => depth += 1,

            '>' => {
                depth -= 1;

                if depth == 0 {
                    return Some(open + i);
                }
            }

            // A type argument list holds no line of its own.
            '\n' => return None,

            _ => {}
        }
    }

    None
}

/// The comma-separated parts of an argument list, with the commas
/// inside a nested list left alone. The language server restyles a
/// hover with it.
pub fn top_level_parts(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;

    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '{' => depth += 1,

            '>' | ')' | '}' => depth = depth.saturating_sub(1),

            ',' if depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }

            _ => {}
        }
    }

    if !text[start..].trim().is_empty() {
        out.push(&text[start..]);
    }

    out
}

/// One `tN = { ... }` binding of a `where` clause.
struct Binding {
    var: String,
    body: String,
}

/// The name a hover declares, in `local X: T` or `X: T`. A type that
/// reads back as the name of what it declares tells the reader
/// nothing, so the fold leaves that one print alone. The restyle runs
/// first and writes the source's keywords, so `export const X: T` is
/// the same subject.
fn subject_of(text: &str) -> Option<String> {
    let mut line = text.lines().nth(1)?.trim_start();

    for keyword in ["export ", "local ", "const "] {
        line = line.strip_prefix(keyword).unwrap_or(line);
    }

    let (name, _) = line.split_once(": ")?;
    let name = name.trim();

    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| name.to_string())
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
                tables: known.tables.clone(),
                shapes: known.shapes.clone(),
                interfaces: known
                    .interfaces
                    .iter()
                    .filter(|i| !i.alias)
                    .cloned()
                    .collect(),
                namespaces: known.namespaces.clone(),
            };

            &narrowed
        }

        false => known,
    };
    // A plain table has no type name, so the fold matches its keys. Two
    // tables with the same keys are two values: `local b = { walk = 5 }`
    // is not `speeds` of another file. Only `self` in a method of the
    // table is sure to be the table, so every other print keeps the
    // shape. `local Provider: typeof(Provider)` also says nothing.
    let without_tables;
    let known = match subject_of(text).as_deref() {
        Some("self") => known,

        _ if known.tables.is_empty() => known,

        _ => {
            without_tables = Known {
                tables: Vec::new(),
                shapes: known.shapes.clone(),
                interfaces: known.interfaces.clone(),
                namespaces: known.namespaces.clone(),
            };

            &without_tables
        }
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

        // A member of the head may be a table that names a shape of
        // its own, and the variable then stands inside what the name
        // replaced whole. The clause is for that variable alone, so it
        // goes with it.
        if !all {
            fold_heads(&mut new_head, known);

            all = !bindings.iter().any(|b| mentions(&new_head, &b.var));
        }

        // A binding no fold could name still must not reach the reader:
        // `tN` is the checker's own spelling and no source can write
        // it. The body prints in place of the variable and the clause
        // goes. A body that names its own variable is the one print the
        // clause exists for, so that one keeps it.
        if !all && let Some(inlined) = inline_bindings(&new_head, &bindings, &resolved) {
            let parsed_end = tail_start + tail_end;
            let clause_end = out[tail_start..]
                .find("\n```")
                .map_or(parsed_end, |k| (tail_start + k).max(parsed_end));
            out.replace_range(head_start..clause_end, &inlined);

            continue;
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
    fold_namespace_names(&mut out, known);
    fold_full_views(&mut out, known);
    fold_rig_characters(&mut out);
    fold_interfaces(&mut out, known);
    fold_bound_records(&mut out, known);
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
    fold_repeated_members(&mut out);
    fold_intersection_dupes(&mut out);
    fold_array_parens(&mut out);
    // `Array<number[] | number[]>` is one array once the union folds.
    fold_array_alias(&mut out);
    fold_read_arrays(&mut out);
    fold_named_unions(&mut out);
    fold_iter_shapes(&mut out);
    out = fold_call_receivers(&out);
    fold_hidden_fields(&mut out);
    fold_empty_metatables(&mut out);
    fold_quoted_types(&mut out, known);
    fold_generic_arity(&mut out);
    drop_free_clauses(&mut out);
    // The two arms of a cut Result read alike once the clause goes.
    fold_repeated_members(&mut out);

    // `local p: Pair<number>` of `struct Pair<A, B = string>`: the
    // print names the arguments the source wrote, and the type carries
    // the default too.
    fill_generic_defaults(&out, &known.shapes)
}

/// Drops a `where` clause the head no longer needs. A fold below the
/// loop may name what the head printed in place: the metatable group of
/// a struct reads by its name, and the variable the clause bound stands
/// nowhere in the head after that. The clause then tells the reader
/// nothing, and every pass that reads the text after the fold had to
/// parse around it.
fn drop_free_clauses(text: &mut String) {
    let mut limit = text.len();

    // A message may hold several clauses, and the checker cuts the last
    // one short; a clause that stays does not stop the ones before it.
    while let Some(i) = text[..limit].rfind(" where ") {
        let (head_start, head) = head_of(text, i);
        let head = head.to_string();
        let tail_start = i + " where ".len();
        let (bindings, tail_end) = parse_bindings(&text[tail_start..]);

        if bindings.is_empty() || bindings.iter().any(|b| mentions(&head, &b.var)) {
            limit = i;

            continue;
        }

        let parsed_end = tail_start + tail_end;
        let clause_end = text[tail_start..]
            .find("\n```")
            .map_or(parsed_end, |k| (tail_start + k).max(parsed_end));
        // A message quotes the type, and a return type at the end of
        // the clause reads the closing quote as its own; the quote
        // stays with the message.
        let clause_end = clause_end - usize::from(text[..clause_end].ends_with(['\'', '`']));
        text.replace_range(head_start..clause_end, &head);
        limit = head_start;
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

/// An `is table` test meets the value with a shape the checker can
/// index; the reader wants the primitive's name.
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

/// The unions the doc gives a name. `delete` takes a value with a
/// `Destroy`, a `Disconnect`, or their lower-case pair; `destroy` takes
/// an Instance or a value with a destroy method.
const NAMED_UNIONS: &[(&str, &str)] = &[
    (
        "{ read Destroy: (any) -> () } | { read Disconnect: (any) -> () } | { read destroy: (any) -> () } | { read disconnect: (any) -> () }",
        "Deletable",
    ),
    (
        "Instance | { read Destroy: (any) -> () } | { read destroy: (any) -> () }",
        "Destroyable",
    ),
    // The operand of `in`, which the std spells `Container`. The
    // typecheck reads that name and writes the sentence the terminal
    // prints; a report the sentence does not cover names the type. The
    // child prints an open table as `{  }`, and a later pass closes the
    // gap, so both spellings are here.
    ("string | {}", "Container"),
];

/// Each named union reads as its name. The `contains` guard keeps a
/// hover that holds no union from a copy of the text.
fn fold_named_unions(text: &mut String) {
    // The child prints an open table as `{ }` or `{  }`, and a pass
    // below closes the gap. The keys hold the closed spelling, so the
    // gap closes here first.
    if text.contains("{ ") {
        *text = text.replace("{  }", "{}").replace("{ }", "{}");
    }

    for (union, name) in NAMED_UNIONS {
        if text.contains(union) {
            *text = text.replace(union, name);
        }
    }
}

/// `T | D` of an `unwrap_or` prints twice when both are the same:
/// `number | number` reads as `number`, and two instantiations of one
/// alias print as two members, `Array<number> | Array<number>`. An
/// intersection repeats a member the same way: the check artifact
/// writes the bound of a type parameter on the array's element and on
/// the read of one, so `T & Ord & Ord` reaches the reader. Each type
/// after a `: ` on a line loses its repeated members, at any depth.
fn fold_repeated_members(text: &mut String) {
    if !text.contains(" | ") && !text.contains(" & ") {
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
    let parts = split_union(text);

    if parts.len() > 1 {
        let mut kept: Vec<String> = Vec::new();

        for m in parts {
            let m = dedupe_type(m.trim());

            if !kept.contains(&m) {
                kept.push(m);
            }
        }

        return kept.join(" | ");
    }

    let parts = split_at_depth(text, " & ", true);

    if parts.len() > 1 {
        let mut kept: Vec<String> = Vec::new();

        for m in parts {
            let m = dedupe_type(m.trim());

            if !kept.contains(&m) {
                kept.push(m);
            }
        }

        return kept.join(" & ");
    }

    // No union and no intersection at this depth: each bracket group
    // gets its own pass.
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
    let parts = split_at_depth(text, ",", true);
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
    split_at_depth(text, ",", true)
        .into_iter()
        .map(str::trim)
        .collect()
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

/// The head with every unnamed binding's body written in place of its
/// variable. `None` when a body names its own variable, or a variable
/// the clause never bound, since then no inline print is faithful.
fn inline_bindings(
    head: &str,
    bindings: &[Binding],
    resolved: &[(String, String)],
) -> Option<String> {
    let mut out = head.to_string();

    for _ in 0..bindings.len() {
        let Some(b) = bindings
            .iter()
            .find(|b| !resolved.iter().any(|(v, _)| *v == b.var) && mentions(&out, &b.var))
        else {
            break;
        };
        let mut body = b.body.clone();

        for (v, name) in resolved {
            body = replace_var(&body, v, name);
        }

        if mentions(&body, &b.var) {
            return None;
        }

        out = replace_var(&out, &b.var, &body);
    }

    (!holds_a_solver_variable(&out)).then_some(out)
}

/// Whether the text still names a solver variable, `t1`.
fn holds_a_solver_variable(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w.len() >= 2 && w.starts_with('t') && w[1..].chars().all(|c| c.is_ascii_digit()))
}

fn mentions(text: &str, var: &str) -> bool {
    text.match_indices(var).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + var.len()..].chars().next();

        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The text with every whole-word `var` read as `name`.
pub fn replace_var(text: &str, var: &str, name: &str) -> String {
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

/// A trait's record inside a longer intersection. A bound leaves the
/// type parameter list at emit and joins every use of the parameter,
/// `T & { read cmp: ... }`. `fold_interfaces` reads a whole
/// intersection, so no name reaches one member of one; the trait names
/// its own record here, and the bound then drops with the rest.
fn fold_bound_records(text: &mut String, known: &Known) {
    if !known.interfaces.iter().any(|i| i.is_trait) {
        return;
    }

    let mut from = 0;

    while let Some(i) = text[from..].find('{') {
        let open = from + i;

        let Some(len) = balanced_len(&text[open..]) else {
            break;
        };
        let body = text[open..open + len].to_string();

        match known
            .interfaces
            .iter()
            .find(|f| f.is_trait && f.matches(&body, &known.interfaces))
        {
            Some(iface) => {
                let name = iface.name.clone();
                text.replace_range(open..open + len, &name);
                from = open + name.len();
            }

            None => from = open + 1,
        }
    }
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
        // A group right after a `: ` that holds one name and stands in
        // front of no arrow is a type and nothing else. The Result
        // folds leave `local r: (Result<number, any>)`, where the
        // parentheses held the two arms the pair folded. A parameter
        // list, `(n: number) -> ()`, keeps both of its marks.
        let annotated = text[..open].ends_with(": ")
            && !outside_angles(&inner, ':')
            && !text[open + len..].trim_start().starts_with("->");

        if plain && (suffix || annotated) {
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

/// `Model & { HumanoidRootPart: Part?, ... }` is a rig's character
/// type; the child prints the intersection behind the alias, and the
/// reader knows it as `R15Character` or `R6Character`.
fn fold_rig_characters(text: &mut String) {
    const HEAD: &str = "Model & {";
    let mut from = 0;

    while let Some(i) = text[from..].find(HEAD) {
        let at = from + i;
        let before = text[..at].chars().next_back();
        let brace = at + HEAD.len() - 1;

        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            from = brace;
            continue;
        }

        let Some(len) = balanced_len(&text[brace..]) else {
            from = brace;
            continue;
        };
        let keys: Vec<String> = members(&text[brace + 1..brace + len - 1])
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let has = |k: &str| keys.iter().any(|x| x == k);
        let name = if has("HumanoidRootPart") && has("UpperTorso") {
            "R15Character"
        } else if has("HumanoidRootPart") && has("Torso") {
            "R6Character"
        } else {
            from = brace;
            continue;
        };

        text.replace_range(at..brace + len, name);
        from = at + name.len();
    }
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

    // `Cat & Cat__private` with no table after it is the view an impl
    // in another file reads, and `Cat__all` is the emit's name for it.
    for suffix in ["__private", "__all"] {
        let mut from = 0;

        while let Some(at) = out[from..].find(suffix).map(|i| from + i) {
            let end = at + suffix.len();
            let joined = out[end..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');

            if joined || out[end..].starts_with('.') {
                from = end;

                continue;
            }

            let name_start = out[..at]
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map(|n| n + 1)
                .unwrap_or(0);
            let name = out[name_start..at].to_string();
            let head = format!("{name} & ");
            let start = match out[..name_start].ends_with(&head) {
                true => name_start - head.len(),

                false => name_start,
            };
            out.replace_range(start..end, &name);
            from = start + name.len();
        }
    }

    out.replace("__private.", ".")
}

/// `Math_Vec2` is the name the emit gives a type of `namespace Math`;
/// the reader knows it as `Math.Vec2`. The fold takes whole words, so
/// a name that only starts with one stays.
fn fold_namespace_names(text: &mut String, known: &Known) {
    if known.namespaces.is_empty() {
        return;
    }

    // The longest name first: `A_B_C` must not fold as `A_B` and a tail.
    let mut pairs: Vec<&(String, String)> = known.namespaces.iter().collect();
    pairs.sort_by_key(|(rendered, _)| std::cmp::Reverse(rendered.len()));

    for (rendered, shown) in pairs {
        let mut from = 0;

        while let Some(i) = text[from..].find(rendered.as_str()) {
            let at = from + i;
            let end = at + rendered.len();
            let before = text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
            let after = text[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');

            if before || after {
                from = end;

                continue;
            }

            text.replace_range(at..end, shown);
            from = at + shown.len();
        }
    }
}

/// The `__` fields Alloy's own emit writes, plus the Luau metamethods.
/// A hover hides these; every other `__` name is the author's, or a
/// package's, and it stays. jecs marks an entity with `__T`, and
/// hiding it printed the type as an empty table.
const HIDDEN_FIELDS: &[&str] = &[
    // The emit's own bookkeeping.
    "__alloy",
    "__attrs",
    "__err",
    "__impl",
    "__less",
    "__new",
    "__ok",
    "__private",
    "__value",
    "__values",
    // The Luau metamethods.
    "__add",
    "__call",
    "__concat",
    "__div",
    "__eq",
    "__idiv",
    "__index",
    "__iter",
    "__le",
    "__len",
    "__lt",
    "__metatable",
    "__mod",
    "__mode",
    "__mul",
    "__namecall",
    "__newindex",
    "__pow",
    "__sub",
    "__tostring",
    "__type",
    "__unm",
];

/// A `__` field of Alloy's own emit is bookkeeping, hidden from
/// completion; a hover hides it the same way. The metatable folds ran
/// before this, so an `__index` a fold reads is gone by now.
fn fold_hidden_fields(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find("__") {
        let at = from + i;
        let opens_field = text[..at].ends_with("{ ")
            || text[..at].ends_with(", ")
            || text[..at]
                .rfind('\n')
                .is_some_and(|nl| text[nl + 1..at].trim().is_empty() && at > nl + 1);
        let name_len = text[at..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count();
        let is_field = name_len > 2
            && text[at + name_len..].starts_with(':')
            && text
                .get(at..at + name_len)
                .is_some_and(|name| HIDDEN_FIELDS.contains(&name));

        if !opens_field || !is_field {
            from = at + 2;

            continue;
        }

        // The field ends at a comma or a closing brace at its own depth.
        let mut depth = 0i32;
        let mut end = None;

        for (k, c) in text[at..].char_indices() {
            match c {
                '{' | '(' | '[' | '<' => depth += 1,
                '}' | ')' | ']' => {
                    if depth == 0 {
                        end = Some(at + k);
                        break;
                    }

                    depth -= 1;
                }
                '>' if !text[..at + k].ends_with('-') => {
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                ',' if depth == 0 => {
                    end = Some(at + k + 1);
                    break;
                }
                _ => {}
            }
        }

        let Some(end) = end else {
            from = at + 2;

            continue;
        };
        // The cut takes the trailing whitespace of a comma, or the
        // separator before a last field.
        let mut cut_end = end;

        if text[at..end].ends_with(',') {
            while text[cut_end..].starts_with([' ', '\n']) {
                cut_end += 1;
            }
        }

        let mut cut_start = at;

        if !text[at..end].ends_with(',') {
            // A last field: the separator before it goes, and the
            // layout before the closing brace stays.
            while cut_start > 0 && text[..cut_start].ends_with([' ', '\n']) {
                cut_start -= 1;
            }

            if text[..cut_start].ends_with(',') {
                cut_start -= 1;
            }

            while cut_end > at && text[..cut_end].ends_with([' ', '\n']) {
                cut_end -= 1;
            }
        }

        text.replace_range(cut_start..cut_end, "");
        from = cut_start;
    }

    fold_empty_tables(text);
}

/// A table the folds emptied keeps the layout of what it held: the
/// child broke it over lines, so `{` and `}` sit on two. One pair of
/// braces reads as `{}`.
fn fold_empty_tables(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find('{') {
        let at = from + i;
        let Some(close) = text[at + 1..].find('}').map(|k| at + 1 + k) else {
            break;
        };

        if text[at + 1..close].trim().is_empty() {
            text.replace_range(at..close + 1, "{}");
            from = at + 2;

            continue;
        }

        from = at + 1;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_metatable_leaves_a_hover() {
        let known = Known::default();

        assert_eq!(
            fold(
                "local a: (n: number) -> { @metatable {  },\n{\n    callback: () -> (),\n    priority: number\n} }",
                &known
            ),
            "local a: (n: number) -> {\n    callback: () -> (),\n    priority: number\n}"
        );
    }

    #[test]
    fn a_hidden_field_leaves_a_hover() {
        let known = Known::default();

        assert_eq!(
            fold(
                "local m: {\n    __value: number,\n    open: string\n}",
                &known
            ),
            "local m: {\n    open: string\n}"
        );
        assert_eq!(
            fold(
                "local m: { open: string, __tostring: (a: number) -> () }",
                &known
            ),
            "local m: { open: string }"
        );
    }

    /// Only the names Alloy's own emit writes are bookkeeping. Another
    /// library's `__` field is data the reader wants: jecs marks an
    /// entity with `__T`, and a table of one such field is no empty
    /// table.
    #[test]
    fn a_foreign_hidden_field_stays_in_a_hover() {
        let known = Known::default();

        assert_eq!(
            fold(
                "local fluid: { __SCHEDULER_INTERFACE: { tick: () -> () }, create: (string) -> Frame, mount: number }",
                &known
            ),
            "local fluid: { __SCHEDULER_INTERFACE: { tick: () -> () }, create: (string) -> Frame, mount: number }"
        );
        assert_eq!(
            fold("local Position: {\n    __T: T\n}", &known),
            "local Position: {\n    __T: T\n}"
        );
    }

    /// A Future of two values carries both in `__values`, and the name
    /// the hover prints holds both.
    #[test]
    fn a_future_of_two_values_names_both() {
        let known = Known::default();

        assert_eq!(
            fold(
                "local f: { read __value: number, read __values: (number, string) -> (), read andThen: (self: any) -> any }",
                &known
            ),
            "local f: Future<number, string>"
        );
        assert_eq!(
            fold(
                "local f: { read __value: number, read __values: (number) -> (), read andThen: (self: any) -> any }",
                &known
            ),
            "local f: Future<number>"
        );
    }

    /// A table the folds emptied reads as `{}`, on one line, whatever
    /// layout the child gave what it held.
    #[test]
    fn an_emptied_table_reads_on_one_line() {
        let known = Known::default();

        assert_eq!(
            fold("local m: {\n    __value: number\n}", &known),
            "local m: {}"
        );
        assert_eq!(fold("local m: { __ok: number }", &known), "local m: {}");
    }

    /// A method of a namespace member prints `M_Gadget:spin(self:
    /// M_Gadget)`; the receiver is the whole path once the name folds.
    #[test]
    fn a_namespace_member_method_keeps_its_path_as_the_receiver() {
        let known = Known {
            namespaces: vec![("M_Gadget".to_string(), "M.Gadget".to_string())],
            ..Known::default()
        };
        assert_eq!(
            fold("function M_Gadget:spin(self: M_Gadget): number", &known),
            "function M.Gadget:spin(self: M.Gadget): number"
        );
    }

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

    /// The std's `Iter` type function builds its methods with no
    /// parameter names, and a step of the chain prints in place with its
    /// metatable and the cycles cut. The hover reads as the method.
    #[test]
    fn a_map_hover_on_an_iter_step_names_the_types() {
        let known = Known::default();
        let step = "{ @metatable { __iter: (any) -> () -> U? }, { collect: <X>(self: { next: (self: any) -> X? }) -> { [number]: X, concat: (self: {read X}, other: *CYCLE*) -> *CYCLE*, push: (self: {read X}, ...X) -> *CYCLE* }, filter: ({ next: (any) -> U? }, (U) -> boolean) -> { @metatable { __iter: (any) -> () -> U? }, *CYCLE* }, next: ({ next: (any) -> U? }) -> U?, take_while: ({ next: (any) -> U? }, (U) -> boolean) -> { @metatable { __iter: (any) -> () -> U? }, *CYCLE* } } }";
        let text = format!(
            "```luau\nfunction xs:map<U>({{ next: (any) -> number? }}, (number) -> U): {step}\n```"
        );

        assert_eq!(
            fold(&text, &known),
            "```luau\nfunction Iter:map<U>(self: Iter<number>, (number) -> U): Iter<U>\n```"
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
            namespaces: Vec::new(),
            tables: Vec::new(),
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
    fn a_mapped_record_keeps_the_mapped_name() {
        let known = Known {
            interfaces: interfaces("type Ent = { id: number, name: string }\n"),
            shapes: Vec::new(),
            namespaces: Vec::new(),
            tables: Vec::new(),
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
            namespaces: Vec::new(),
            tables: Vec::new(),
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

    /// `new Pair<<number>>` of `struct Pair<A, B = string>`: Luau
    /// fills the second argument from the alias, and the print stops
    /// short of it.
    #[test]
    fn a_generic_print_carries_the_defaults_of_its_declaration() {
        let shapes = vec![
            Shape::Struct {
                name: "Pair".into(),
                fields: vec![("first".into(), false), ("second".into(), false)],
                generics: vec!["A".into(), "B = string".into()],
                types: vec!["A".into(), "B".into()],
            },
            Shape::Enum {
                name: "Either".into(),
                generics: vec!["L".into(), "R = string".into()],
                variants: vec![
                    ("Left".into(), vec!["L".into()]),
                    ("Right".into(), vec!["R".into()]),
                ],
            },
        ];

        assert_eq!(
            fill_generic_defaults("local p: Pair<number>", &shapes),
            "local p: Pair<number, string>"
        );
        // An enum with a default fills the same way, and the hover of
        // an annotated value reads the whole type through the fold.
        assert_eq!(
            fill_generic_defaults("local e: Either<number>", &shapes),
            "local e: Either<number, string>"
        );
        let known = Known {
            shapes: shapes.clone(),
            ..Default::default()
        };
        assert_eq!(
            fold("```luau\nlocal p: Pair<number>\n```", &known),
            "```luau\nlocal p: Pair<number, string>\n```"
        );
        assert_eq!(
            fold("```luau\nlocal e: Either<number>\n```", &known),
            "```luau\nlocal e: Either<number, string>\n```"
        );
        // A list the source wrote whole stays as it is, and so does
        // the declaration's own line.
        assert_eq!(
            fill_generic_defaults("Pair<number, boolean>", &shapes),
            "Pair<number, boolean>"
        );
        assert_eq!(
            fill_generic_defaults("struct Pair<A, B = string> as", &shapes),
            "struct Pair<A, B = string> as"
        );
        // The source's own `<<...>>` names its arguments already.
        assert_eq!(
            fill_generic_defaults("new Pair<<number>>", &shapes),
            "new Pair<<number>>"
        );
        // A parameter with no default leaves the list as it stands.
        assert_eq!(
            fill_generic_defaults("Other<number>", &shapes),
            "Other<number>"
        );
    }

    #[test]
    fn a_not_nil_refinement_reads_as_the_type_it_refines() {
        let known = Known::default();

        assert_eq!(fold("intersect<T, ~nil>", &known), "T");
        assert_eq!(fold("(a & ~nil) | { }", &known), "(a) | {}");
        assert_eq!(fold("Item & ~nil", &known), "Item");
        assert_eq!(fold("intersect<A, ~nil>[]", &known), "A[]");
        assert_eq!(fold("intersect<Item, Named>", &known), "Item & Named");
    }

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
                    generics: vec![],
                    types: vec!["string".into(), "number".into(), "number[]".into()],
                },
                Shape::Enum {
                    name: "Rarity".into(),
                    generics: vec![],
                    variants: vec![("Common".into(), vec![]), ("Rare".into(), vec![])],
                },
                Shape::Enum {
                    name: "Boost".into(),
                    generics: vec![],
                    variants: vec![
                        ("None".into(), vec![]),
                        ("Coins".into(), vec!["number".into()]),
                    ],
                },
            ],
            namespaces: Vec::new(),
            tables: Vec::new(),
        }
    }

    #[test]
    fn a_struct_value_reads_by_name() {
        let text = "```luau\nlocal found: t3? where t1 = {\n    [number]: number,\n    concat: (self: t1, other: t1) -> t1,\n    push: (self: t1, ...number) -> ()\n} ; t2 = {\n    __index: t2,\n    __new: (f: { color: t1, cost: number, id: string }) -> t3\n} ; t3 = { @metatable t2, {\n    read color: t1,\n    read cost: number,\n    read id: string\n} }\n```";
        assert_eq!(fold(text, &known()), "```luau\nlocal found: Saber?\n```");
    }

    /// `import * as M from "./m"`: the module's export table reaches the
    /// struct's own table through one field, and the print carries the
    /// constructor alone. It read `{ Box: t1 } where t1 = { new: ... }`.
    #[test]
    fn a_namespace_import_reads_as_the_export_list() {
        let text = "```luau\nlocal M: {\n    Box: t1\n} where t1 = {\n    new: (f: {\n        id: string,\n        cost: number,\n        color: number[]\n    }) -> Saber\n}\n```";
        assert_eq!(
            fold(text, &known()),
            "```luau\nlocal M: {\n    Box: Saber\n}\n```"
        );
    }

    /// `import * as M from "./geo"` on a module that exports a
    /// namespace: the group lowers to a table of its members, so the
    /// checker prints that table. The group's own name reads there, and
    /// the clause of the member it held goes with it.
    #[test]
    fn a_namespace_table_reads_as_the_group() {
        let known = Known {
            namespaces: vec![("Ns_T".into(), "Ns.T".into())],
            ..known()
        };
        let text = "```luau\nlocal M: {\n    Ns: {\n        T: t1\n    }\n} where t1 = {\n    new: (f: { value: number }) -> { @metatable t1, { value: number } }\n}\n```";

        assert_eq!(
            fold(text, &known),
            "```luau\nlocal M: {\n    Ns: Ns\n}\n```"
        );
    }

    /// `import * as Dir from "./m"` on a module that exports an enum:
    /// the export table reaches the enum's own table through a field,
    /// and the variants with the `is` guard name the enum. A binding no
    /// fold can name prints its body in place of `tN`.
    #[test]
    fn an_enum_table_reads_as_the_enum() {
        let text = "```luau\nlocal Dir: {\n    Rarity: t1\n} where t1 = {\n    Common: Rarity,\n    Rare: Rarity,\n    is: (v: unknown) -> boolean\n}\n```";
        assert_eq!(
            fold(text, &known()),
            "```luau\nlocal Dir: {\n    Rarity: Rarity\n}\n```"
        );

        let text = "```luau\nlocal U: {\n    Util: t1\n} where t1 = {\n    double: (x: number) -> number\n}\n```";
        assert_eq!(
            fold(text, &known()),
            "```luau\nlocal U: {\n    Util: {\n    double: (x: number) -> number\n}\n}\n```"
        );
    }

    #[test]
    fn a_generic_struct_reads_with_its_argument() {
        let known = Known {
            interfaces: Vec::new(),
            shapes: vec![Shape::Struct {
                name: "Slotted".into(),
                fields: vec![("value".into(), false), ("count".into(), false)],
                generics: vec!["T".into()],
                types: vec!["T".into(), "number".into()],
            }],
            namespaces: Vec::new(),
            tables: Vec::new(),
        };
        let text = "local held: t1 where t1 = {\n    read bump: (self: t1, n: number) -> number,\n    count: number,\n    read get: (self: t1) -> number,\n    value: number\n}";
        assert_eq!(fold(text, &known), "local held: Slotted<number>");
    }

    /// A generic struct whose `impl` writes only `new` prints no method,
    /// and the parameter may sit under a `?`. The fields still name the
    /// struct, and the `?` still carries the argument.
    #[test]
    fn a_generic_struct_with_no_method_reads_by_its_fields() {
        let known = Known {
            interfaces: Vec::new(),
            shapes: vec![Shape::Struct {
                name: "Scheduler".into(),
                fields: vec![("phase".into(), false), ("data".into(), false)],
                generics: vec!["T".into()],
                types: vec!["number".into(), "T?".into()],
            }],
            namespaces: Vec::new(),
            tables: Vec::new(),
        };
        let text = "local g: {\n    data: unknown?,\n    phase: number\n}";
        assert_eq!(fold(text, &known), "local g: Scheduler<unknown>");

        // A call that pins the parameter reads the type it pins.
        let text = "local g: {\n    data: string?,\n    phase: number\n}";
        assert_eq!(fold(text, &known), "local g: Scheduler<string>");
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
    fn a_private_view_hint_reads_as_the_struct() {
        let text = ": Swinger & Swinger__private & { last: number, scope: Scope }";
        assert_eq!(fold(text, &Known::default()), ": Swinger");
        let known = Known {
            interfaces: Vec::new(),
            shapes: crate::declarations::shapes(
                "export struct Swinger as\n    read requested: Signal<> = Signal.new()\n    private last: number = 0\n    private scope: Scope = Scope.new()\nend\n",
            ),
            namespaces: Vec::new(),
            tables: Vec::new(),
        };
        assert_eq!(fold(text, &known), ": Swinger");

        // The view an impl in another file reads carries no table.
        let text = "function Cat:helper(self: Cat & Cat__private): string";
        assert_eq!(
            fold(text, &Known::default()),
            "function Cat:helper(self: Cat): string"
        );
        assert_eq!(fold("local c: Cat__all", &Known::default()), "local c: Cat");
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

    /// The child prints the rig's character as the intersection behind
    /// the alias; the reader knows it by the name the std declares.
    #[test]
    fn a_rig_character_reads_by_name() {
        let r15 = "```luau\nlocal c: (Model & {\n    Animate: LocalScript?,\n    BodyColors: BodyColors?,\n    Head: MeshPart?,\n    Health: Script?,\n    Humanoid: (Humanoid & {\n        Animator: Animator?,\n        HumanoidDescription: HumanoidDescription?\n    })?,\n    HumanoidRootPart: Part?,\n    LeftFoot: MeshPart?,\n    UpperTorso: MeshPart?\n})?\n```";
        assert_eq!(
            fold(r15, &Known::default()),
            "```luau\nlocal c: R15Character?\n```"
        );

        let r6 = "local c: (Model & { [\"Left Arm\"]: Part?, Head: Part?, HumanoidRootPart: Part?, Torso: Part? })?";
        assert_eq!(fold(r6, &Known::default()), "local c: R6Character?");

        // Any other model keeps its shape.
        let other = "local m: Model & { Root: Part? }";
        assert_eq!(fold(other, &Known::default()), other);
    }

    #[test]
    fn a_remote_reads_by_name_in_place() {
        let text = "```luau\nlocal BuySaber: {\n    call: (id: string) -> Future<any>,\n    fire: (id: string) -> (),\n    fire_all: (id: string) -> (),\n    fire_except: (except: Player, id: string) -> (),\n    instance: RemoteFunction?,\n    on: (handler: (sender: Player, id: string) -> ()) -> RBXScriptConnection,\n    on_ratelimited: (handler: (player: Player) -> ()) -> (),\n    once: (handler: (sender: Player, id: string) -> ()) -> RBXScriptConnection,\n    spec: any,\n    wait: () -> Future<any>\n}\n```";
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

    /// `alloy flux` prints a report on one line, with the type inside
    /// quotes. The head of a clause is the quoted type, not the line,
    /// so the method's clause goes, and the sentence after it stays.
    #[test]
    fn a_one_line_report_drops_the_method_clause_of_a_generic_enum() {
        let known = Known {
            shapes: vec![Shape::Enum {
                name: "Opt".into(),
                generics: vec!["T".into()],
                variants: vec![("Some".into(), vec!["T".into()]), ("Nil".into(), vec![])],
            }],
            ..Default::default()
        };
        let clause = "where t1 = <U>({ read _1: *error-type*, read map: t1, read tag: \"Some\" } | { read map: t1, read tag: \"Nil\" }, (any) -> U) -> { read _1: U, read map: t1, read tag: \"Some\" } | { read map: t1, read tag: \"Nil\" }";
        let text = format!(
            "Expected this to be '{{ read _1: string, read map: t1, read tag: \"Some\" }} | {{ read map: t1, read tag: \"Nil\" }} {clause}' but got '{{ read _1: number, read map: t1, read tag: \"Some\" }} | {{ read map: t1, read tag: \"Nil\" }} {clause}'; the 1st component of the union is `{{ read _1: number, read map: t1, read tag: \"Some\" }} {clause}`, which is not a subtype of `{{ read _1: string, read map: t1, read tag: \"Some\" }} | {{ read map: t1, read tag: \"Nil\" }} {clause}`"
        );
        assert_eq!(
            fold(&text, &known),
            "Expected this to be 'Opt<string>' but got 'Opt<number>'; the 1st component of the union is `Opt<number>`, which is not a subtype of `Opt<string>`"
        );
    }

    /// A generic enum's alias is a plain union: a payload carries its
    /// argument in a slot, and a unit prints as a table with its tag
    /// alone beside the methods. The union reads as the enum with the
    /// argument, and the method's clause goes with it.
    #[test]
    fn a_generic_enum_reads_with_its_argument() {
        let known = Known {
            shapes: vec![Shape::Enum {
                name: "Opt".into(),
                generics: vec!["T".into()],
                variants: vec![("Some".into(), vec!["T".into()]), ("Nil".into(), vec![])],
            }],
            ..Default::default()
        };
        let text = "```luau\nlocal a: {\n    read _1: number,\n    read map: t1,\n    read tag: \"Some\"\n} | {\n    read map: t1,\n    read tag: \"Nil\"\n} where t1 = <T>(self: {\n    read _1: T,\n    read map: t1,\n    read tag: \"Some\"\n} | {\n    read map: t1,\n    read tag: \"Nil\"\n}, f: any) -> any\n```";
        assert_eq!(fold(text, &known), "```luau\nlocal a: Opt<number>\n```");

        let message = "Type '{ read _1: number, read map: t1, read tag: \"Some\" } | { read map: t1, read tag: \"Nil\" }' could not be converted into 'Opt<string>'";
        assert_eq!(
            fold(message, &known),
            "Type 'Opt<number>' could not be converted into 'Opt<string>'"
        );

        // The checker cuts the last clause of a long message short; the
        // whole clauses before it still go.
        let cut = "Expected this to be\n\t'{ read _1: string, read map: t1, read tag: \"Some\" } | { read map: t1, read tag: \"Nil\" } where t1 = <T>(Opt, any) -> any'\nbut got\n\t'{ read _1: number, read map: t1, read tag: \"Some\" } | { read map: t1, read tag: \"Nil\" } where t1 = <T>(Opt, any) -> any'; \nthe 1st component of the union is `Opt<number> where t1 = <T>(Opt`";
        assert_eq!(
            fold(cut, &known),
            "Expected this to be\n\t'Opt<string>'\nbut got\n\t'Opt<number>'; \nthe 1st component of the union is `Opt<number> where t1 = <T>(Opt`"
        );

        // A unit table alone, and a payload table alone, each name the
        // enum; the payload keeps its argument.
        assert_eq!(
            fold("local n: { read map: t1, read tag: \"Nil\" }", &known),
            "local n: Opt"
        );
        assert_eq!(
            fold(
                "local s: { read _1: string, read map: t1, read tag: \"Some\" }",
                &known
            ),
            "local s: Opt<string>"
        );
    }

    /// The enum's parameter list names the slot to read: a payload
    /// spelled as a parameter carries the argument, and any other
    /// payload, an alias the fold does not know, binds nothing.
    /// A generic struct value prints as its table, and the field that
    /// holds the parameter names the argument: `Box<number>`, nested
    /// too.
    #[test]
    fn a_generic_struct_value_reads_its_arguments_from_its_fields() {
        let known = Known {
            shapes: vec![Shape::Struct {
                name: "Box".into(),
                fields: vec![("inner".into(), false)],
                generics: vec!["T".into()],
                types: vec!["T".into()],
            }],
            ..Default::default()
        };
        let printed = "local mapped: { @metatable t1, { inner: number } } where t1 = { __index: t1, map: <T, U>(self: { @metatable t1, { inner: T } }, f: (T) -> U) -> { @metatable t1, { inner: U } } }";
        assert_eq!(fold(printed, &known), "local mapped: Box<number>");
        assert_eq!(
            fold(
                "{ @metatable t1, { inner: { @metatable t1, { inner: string } } } }",
                &known
            ),
            "Box<Box<string>>"
        );
    }

    #[test]
    fn a_generic_enum_reads_its_arguments_from_its_parameters() {
        let known = Known {
            shapes: vec![
                Shape::Enum {
                    name: "Opt".into(),
                    generics: vec!["T".into()],
                    variants: vec![("Some".into(), vec!["T".into()]), ("Nil".into(), vec![])],
                },
                Shape::Enum {
                    name: "Tagged".into(),
                    generics: vec!["T".into()],
                    variants: vec![
                        ("Both".into(), vec!["Alias".into(), "T".into()]),
                        ("None".into(), vec![]),
                    ],
                },
                Shape::Enum {
                    name: "Either".into(),
                    generics: vec!["L".into(), "R".into()],
                    variants: vec![
                        ("Left".into(), vec!["L".into()]),
                        ("Right".into(), vec!["R".into()]),
                    ],
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            fold(
                "local a: { read tag: \"Some\", read _1: number, read map: t1 } | { read tag: \"Nil\", read map: t1 }",
                &known
            ),
            "local a: Opt<number>"
        );
        // `Alias` resolves to `number` in the print; the slot is not
        // an argument, and `T` alone is.
        assert_eq!(
            fold(
                "local a: { read _1: number, read _2: string, read tag: \"Both\" } | { read tag: \"None\" }",
                &known
            ),
            "local a: Tagged<string>"
        );
        // The checker prints `Either.Right("x")` with `any` in the
        // `Left` slot, so the fold reads both arguments; the `Right`
        // table alone has no slot for `L`, and the name stands bare.
        assert_eq!(
            fold(
                "local b: { read _1: any, read tag: \"Left\" } | { read _1: string, read tag: \"Right\" }",
                &known
            ),
            "local b: Either<any, string>"
        );
        assert_eq!(
            fold("local r: { read _1: string, read tag: \"Right\" }", &known),
            "local r: Either"
        );
    }

    #[test]
    fn a_symbol_and_a_nested_enum_read_by_name() {
        let known = Known {
            interfaces: Vec::new(),
            shapes: vec![
                Shape::Enum {
                    name: "Shape".into(),
                    generics: vec![],
                    variants: vec![
                        ("Circle".into(), vec!["number".into()]),
                        ("Rect".into(), vec!["number".into(), "number".into()]),
                    ],
                },
                Shape::Enum {
                    name: "Event".into(),
                    generics: vec![],
                    variants: vec![
                        ("Spawn".into(), vec!["Player".into(), "Vector3".into()]),
                        ("Hit".into(), vec!["Player".into(), "Shape".into()]),
                        ("Leave".into(), vec!["Player".into()]),
                    ],
                },
            ],
            namespaces: Vec::new(),
            tables: Vec::new(),
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
                generics: vec![],
                variants: vec![
                    ("None".into(), vec![]),
                    ("Coins".into(), vec!["number".into()]),
                    ("Strength".into(), vec!["number".into()]),
                ],
            }],
            namespaces: Vec::new(),
            tables: Vec::new(),
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
    fn a_future_of_nothing_keeps_its_nil() {
        // `Future<()>` names a type nobody can write: `()` is a type
        // pack, not a type argument. The emit writes `Future<nil>`, so
        // that is what a reader sees and can write back.
        let text = "```luau\nfunction tick(): Future<nil>\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nfunction tick(): Future<nil>\n```"
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

    /// The operand of `in` is `string | {}`, which the std names
    /// `Container`. The typecheck reads that name and writes the
    /// sentence the terminal prints, so the editor's report agrees with
    /// the terminal's; a report the sentence does not cover names the
    /// type instead of the union.
    #[test]
    fn the_container_union_reads_by_name() {
        for printed in ["string | {}", "string | { }", "string | {  }"] {
            assert_eq!(
                fold(
                    &format!("Expected this to be '{printed}', but got 'number'"),
                    &Known::default()
                ),
                "Expected this to be 'Container', but got 'number'"
            );
        }
    }

    /// A table shape read as `typeof(X)` by its keys named a local of
    /// another file for any table with the same keys. Only `self` in a
    /// method of the table is sure to be it.
    #[test]
    fn a_table_shape_names_a_plain_table_for_self_alone() {
        let known = Known {
            tables: vec![(
                "speeds".to_string(),
                vec!["walk".to_string(), "run".to_string()],
            )],
            ..Known::default()
        };
        let shape = "{\n    run: number,\n    walk: number\n}";

        for head in ["local other", "local speeds", "local mine"] {
            let text = format!("```luau\n{head}: {shape}\n```");
            assert_eq!(fold(&text, &known), text);
        }
        assert_eq!(
            fold(&format!("```luau\nlocal self: {shape}\n```"), &known),
            "```luau\nlocal self: typeof(speeds)\n```"
        );
    }
}
