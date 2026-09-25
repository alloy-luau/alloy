//! The name a printed table, union, or signature resolves to: a
//! struct's fields, an interface's, a generic struct's, a payload
//! enum's union, or one of the std containers by the members only it
//! has.

use crate::declarations::Shape;

use super::Known;
use super::strings::{balanced_len, member_parts, members, type_len};

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

/// Whether a table body is one variant of a payload enum: a `tag`
/// literal with the emit's `_1` and `_2` slots beside it.
pub(crate) fn is_tagged_variant(body: &str) -> bool {
    let m = members(body);

    m.iter().any(|(k, v)| k == "tag" && v.starts_with('"'))
        && m.iter().any(|(k, _)| {
            k.len() > 1 && k.starts_with('_') && k[1..].chars().all(|c| c.is_ascii_digit())
        })
}

/// The enum a tagged table belongs to: its `tag` literal names one of
/// the enum's variants.
pub(crate) fn enum_of_variant(table: &str, known: &Known) -> Option<String> {
    let m = members(table);
    let (_, tag) = m.iter().find(|(k, _)| k == "tag")?;
    let variant = tag.trim().trim_matches('"');

    known.shapes.iter().find_map(|s| match s {
        Shape::Enum { name, variants, .. } if variants.iter().any(|(v, _)| v == variant) => {
            Some(name.clone())
        }

        _ => None,
    })
}

/// The enum a printed table declares: `{ Up: Direction, Down: Direction,
/// is: (v: unknown) -> boolean }`. The keys, less the `is` guard the
/// emit adds, are the enum's variants.
fn enum_table_name(m: &[(String, String)], known: &Known) -> Option<String> {
    let keys: Vec<&String> = m.iter().map(|(k, _)| k).filter(|k| *k != "is").collect();

    if keys.is_empty() {
        return None;
    }

    known.shapes.iter().find_map(|s| match s {
        Shape::Enum { name, variants, .. } => {
            let names: Vec<String> = variants.iter().map(|(v, _)| v.clone()).collect();

            same_set(&names, &keys).then(|| name.clone())
        }

        _ => None,
    })
}

/// The namespace a printed table declares: one member per member of
/// the group. The emit lowers a group to a table of what it holds, so
/// the checker prints that table wherever the source wrote the group.
fn namespace_table_name(m: &[(String, String)], known: &Known) -> Option<String> {
    if m.is_empty() {
        return None;
    }

    let keys: Vec<&String> = m.iter().map(|(k, _)| k).collect();
    let mut groups: Vec<&str> = known
        .namespaces
        .iter()
        .filter_map(|(_, path)| path.rsplit_once('.').map(|(group, _)| group))
        .collect();
    groups.sort();
    groups.dedup();

    groups.into_iter().find_map(|group| {
        let members: Vec<String> = known
            .namespaces
            .iter()
            .filter_map(|(_, path)| path.rsplit_once('.'))
            .filter(|(g, _)| *g == group)
            .map(|(_, name)| name.to_string())
            .collect();

        // The reader writes the group's last word; an inner group
        // stands under its own name.
        same_set(&members, &keys).then(|| group.rsplit('.').next().unwrap_or(group).to_string())
    })
}

/// The enum a unit variant belongs to, by the name it prints as.
fn enum_of_unit(unit: &str, known: &Known) -> Option<String> {
    known.shapes.iter().find_map(|s| match s {
        Shape::Enum { name, variants, .. }
            if variants.iter().any(|(v, p)| v == unit && p.is_empty()) =>
        {
            Some(name.clone())
        }

        _ => None,
    })
}

/// A generic struct printed as its plain-table alias: the fields, then
/// one `read name: (self: ...) -> ...` per method. The field set names
/// the struct, and a field declared as a bare parameter names that
/// parameter's argument. `Slotted` with no argument beats the table
/// when a field leaves one unbound.
///
/// A struct whose `impl` writes only `new` has no method to print, so
/// the fields stand alone.
fn generic_struct_of_body(body: &str, known: &Known) -> Option<String> {
    if !body.starts_with('{') || balanced_len(body) != Some(body.len()) {
        return None;
    }

    let inner = body.get(1..body.len() - 1)?.trim();
    let fields: Vec<(String, String)> = members(inner)
        .into_iter()
        .filter(|(_, value)| !value.trim_start().starts_with("(self:"))
        .collect();

    if fields.is_empty() {
        return None;
    }

    let keys: Vec<String> = fields.iter().map(|(k, _)| k.clone()).collect();

    for shape in &known.shapes {
        let Shape::Struct {
            fields: decl,
            generics,
            types,
            ..
        } = shape
        else {
            continue;
        };

        if generics.is_empty() || decl.len() != types.len() {
            continue;
        }

        let all: Vec<&String> = decl.iter().map(|(f, _)| f).collect();

        if !same_set(&keys, &all) {
            continue;
        }

        return struct_with_arguments(shape, &fields, known);
    }

    None
}

/// The struct's name with the arguments its printed fields carry: a
/// field declared as a parameter holds that argument, `inner: T` under
/// `inner: number` reads `Box<number>`. The bare name when a parameter
/// has no field to read.
fn struct_with_arguments(
    shape: &Shape,
    printed: &[(String, String)],
    known: &Known,
) -> Option<String> {
    let Shape::Struct {
        name,
        fields: decl,
        generics,
        types,
    } = shape
    else {
        return None;
    };

    if generics.is_empty() || decl.len() != types.len() {
        return Some(name.clone());
    }

    let args: Vec<String> = generics
        .iter()
        .map(|g| {
            decl.iter()
                .zip(types)
                .find_map(|((f, _), ty)| {
                    let ty = ty.trim();
                    // `inner: T` reads the argument straight. `data: T?`
                    // reads it through the `?` the print carries too.
                    let through_option = ty.strip_suffix('?') == Some(g.as_str());

                    if !through_option && ty != g.as_str() {
                        return None;
                    }

                    let v = printed.iter().find(|(k, _)| k == f)?.1.trim();
                    let v = match through_option {
                        true => v.strip_suffix('?')?,

                        false => v,
                    };

                    Some(name_of_body(v, known).unwrap_or_else(|| v.to_string()))
                })
                .unwrap_or_default()
        })
        .collect();

    if args.iter().any(String::is_empty) {
        return Some(name.clone());
    }

    Some(format!("{name}<{}>", args.join(", ")))
}

pub(crate) fn name_of_body(body: &str, known: &Known) -> Option<String> {
    let trimmed = body.trim();

    // A struct instance: `{ @metatable tN, { fields } }`. The metatable
    // may print in place, so its own commas are not the separator.
    if let Some(rest) = trimmed.strip_prefix("{ @metatable ") {
        let comma = type_len(rest);
        let table = rest.get(comma + 1..)?.trim();
        let table = table.strip_suffix('}')?.trim();
        // A guard on a field prints it twice, `read dirty: true, write
        // dirty: boolean`; the field set is what names the struct.
        let typed: Vec<(String, String)> = members(table)
            .into_iter()
            .map(|(k, v)| {
                (
                    k.trim_start_matches("read ")
                        .trim_start_matches("write ")
                        .to_string(),
                    v,
                )
            })
            .collect();
        let mut printed: Vec<String> = typed.iter().map(|(k, _)| k.clone()).collect();
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
            if let Shape::Struct { fields, .. } = shape {
                let all: Vec<&String> = fields.iter().map(|(f, _)| f).collect();
                let public: Vec<&String> =
                    fields.iter().filter(|(_, p)| !p).map(|(f, _)| f).collect();

                // A refinement flattens the methods in with the fields;
                // every field present still names the struct.
                let holds_all = !all.is_empty() && all.iter().all(|f| printed.contains(f));

                if !printed.is_empty()
                    && (same_set(&printed, &all) || same_set(&printed, &public) || holds_all)
                {
                    return struct_with_arguments(shape, &typed, known);
                }
            }
        }

        // A step of an `Iter` chain carries `__iter` in its metatable,
        // and the members name it.
        return iter_name(&typed);
    }

    // A generic struct: the check artifact writes its alias as a plain
    // table of the fields and the methods, since Luau prints no type
    // argument for a `typeof(setmetatable(...))` alias. The arguments
    // read back off the fields that hold them.
    if let Some(name) = generic_struct_of_body(trimmed, known) {
        return Some(name);
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

    // An enum's own table, reached through a module's export table: one
    // member per variant and the `is` guard the emit adds.
    if let Some(name) = enum_table_name(&m, known) {
        return Some(name);
    }

    // A namespace's own table: one member per member of the group.
    if let Some(name) = namespace_table_name(&m, known) {
        return Some(name);
    }

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
                let Shape::Struct { name, fields, .. } = shape else {
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
            // A trait is no mapped type: the emit marks every method of
            // one `read`, so `Readonly<Ord>` names what the source
            // spelled `Ord`.
            for iface in known.interfaces.iter().filter(|i| !i.is_trait) {
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

    // A plain `local X = { }` table. No type names it, so the analyzer
    // prints the whole shape; the source named the value, and the type
    // of it is `typeof(X)`.
    if !known.tables.is_empty() && !m.is_empty() && trimmed.starts_with('{') {
        let mut keys: Vec<String> = m
            .iter()
            .map(|(k, _)| {
                k.trim_start_matches("read ")
                    .trim_start_matches("write ")
                    .to_string()
            })
            .collect();
        keys.sort();
        keys.dedup();

        for (name, fields) in &known.tables {
            let all: Vec<&String> = fields.iter().collect();

            if same_set(&keys, &all) {
                return Some(format!("typeof({name})"));
            }
        }
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

    // A BitSet has the members of a Set, and its `add` takes an index
    // where a Set's takes a value.
    if get("add").is_some_and(|sig| sig.contains("index: number")) && has("has") && has("union") {
        return Some("BitSet".to_string());
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
    //
    // A module's export table reaches the same table through a field,
    // `{ Box: t1 }`, and the print there carries the constructor alone:
    // `Box.__index = Box` is a cycle the checker leaves out. One member
    // that builds a struct is that struct's own table.
    if (has("__index") || m.len() == 1)
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
        // `nil` is what the emit writes and what a reader can write
        // back. `()` is a type pack, so `Future<()>` names a type
        // nobody can spell; it read as a different type from the one
        // the source declared.
        // `__values: (A, B) -> ()` holds every value, so a Future of
        // two reads `Future<A, B>`.
        let all = get("__values")
            .and_then(|r| r.trim().strip_suffix("-> ()"))
            .and_then(|r| r.trim().strip_prefix('(')?.strip_suffix(')'))
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(v);

        return Some(format!("Future<{all}>"));
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

    iter_name(&m)
}

/// An `Iter`, by three members only it has together. The element is
/// what `next` returns.
fn iter_name(m: &[(String, String)]) -> Option<String> {
    let has = |key: &str| m.iter().any(|(k, _)| k == key);

    if !(has("next") && has("take_while") && has("collect")) {
        return None;
    }

    let t = m
        .iter()
        .find(|(k, _)| k == "next")
        .and_then(|(_, v)| return_type(v))
        .map(|t| t.trim_end_matches('?').to_string())
        .unwrap_or_else(|| "any".to_string());

    Some(format!("Iter<{t}>"))
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

#[cfg(test)]
mod tests {
    use super::super::Known;
    use super::*;

    #[test]
    fn an_iter_return_reads_past_the_nested_arrow() {
        let known = Known::default();
        let body = "{ collect: (self: { next: (self: any) -> U? }) -> Array<U>, next: (self: { next: (self: any) -> U? }) -> U?, take_while: (self: { next: (self: any) -> U? }, f: (U) -> boolean) -> any }";

        assert_eq!(name_of_body(body, &known), Some("Iter<U>".to_string()));
    }

    /// A step of an `Iter` chain prints with its `__iter` metatable in
    /// place, and names no struct.
    #[test]
    fn an_iter_step_under_its_metatable_reads_as_iter() {
        let known = Known::default();
        let body = "{ @metatable { __iter: (any) -> () -> string? }, { collect: <X>(self: { next: (self: any) -> X? }) -> X[], next: ({ next: (any) -> string? }) -> string?, take_while: ({ next: (any) -> string? }, (string) -> boolean) -> *CYCLE* } }";

        assert_eq!(name_of_body(body, &known), Some("Iter<string>".to_string()));
    }

    /// A bound on a type parameter prints as the trait's own record.
    /// The trait is a name the source wrote, and the emit marks every
    /// method of one `read`, so the record is no mapped type.
    #[test]
    fn a_trait_s_record_reads_as_the_trait() {
        let known = Known {
            interfaces: crate::shapes::interfaces(
                "export trait Ord as\n    function cmp(self, other: Ord): number\nend\n",
            ),
            ..Known::default()
        };
        let printed = "{ read cmp: (self: any, other: t1) -> number }";

        assert_eq!(name_of_body(printed, &known), Some("Ord".to_string()));
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
    fn a_bitset_is_not_a_set() {
        let known = Known::default();
        let bits = "{ add: (self: t1, index: number) -> t1, has: (self: t1, index: number) -> boolean, len: (self: t1) -> number, to_array: (self: t1) -> number[], union: (self: t1, other: t1) -> t1 }";
        let set = "{ add: (self: t1, value: string) -> t1, has: (self: t1, value: string) -> boolean, to_table: (self: t1) -> { [string]: boolean }, union: (self: t1, other: t1) -> t1 }";

        assert_eq!(name_of_body(bits, &known), Some("BitSet".to_string()));
        assert_eq!(name_of_body(set, &known), Some("Set<string>".to_string()));
    }
}
