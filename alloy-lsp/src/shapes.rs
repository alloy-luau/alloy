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
}

/// Folds every string of a JSON value, in place.
pub fn fold_value(value: &mut Value, known: &Known) {
    match value {
        Value::String(s) => {
            if s.contains(" where ")
                || s.contains("tag: \"")
                || s.contains("\" | \"")
                || s.contains("__private")
                || s.contains("Array<")
            {
                *s = fold(s, known);
            }
        }

        Value::Array(items) => items.iter_mut().for_each(|i| fold_value(i, known)),

        Value::Object(map) => map.values_mut().for_each(|v| fold_value(v, known)),

        _ => {}
    }
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
    let mut out = text.to_string();

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

        out.replace_range(head_start..tail_start + tail_end, &new_head);
    }

    fold_results(&mut out);
    fold_lite_results(&mut out);
    fold_enums(&mut out, known);
    out = fold_private_views(&out);
    fold_full_views(&mut out, known);
    fold_array_alias(&mut out);
    fold_narrowed_primitives(&mut out);
    out = fold_temp_receiver(&out);

    out
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
        let Some((_, both)) = members(&body).into_iter().find(|(k, _)| k == "_1") else {
            return;
        };
        let Some((t, e)) = both.split_once(" | ") else {
            return;
        };
        let name = format!("Result<{t}, {e}>");
        text.replace_range(open..open + len, &name);
    }
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

/// `Array<T>` reads as the sugar the source has, `T[]`, when `T` is a
/// name or a dotted path; a compound argument keeps the alias.
fn fold_array_alias(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find("Array<") {
        let start = from + i;
        let inner_start = start + "Array<".len();
        // Not the tail of another name, `ReadArray<`.
        let prefixed = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');

        match text[inner_start..].find('>') {
            Some(n)
                if !prefixed
                    && text[inner_start..inner_start + n]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '?') =>
            {
                let elem = text[inner_start..inner_start + n].to_string();
                text.replace_range(start..inner_start + n + 1, &format!("{elem}[]"));
                from = start + elem.len() + 2;
            }

            _ => from = inner_start,
        }
    }
}

/// The type text before ` where `: from the start of its line, past a
/// `local x: ` or `x: ` head, so a replacement keeps the label.
fn head_of(text: &str, where_at: usize) -> (usize, &str) {
    let line_start = text[..where_at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let line = &text[line_start..where_at];
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

    (line_start + start, &line[start..])
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

            if let Some(name) = name_of_body(&body, known) {
                resolved.push((b.var.clone(), name));
                changed = true;
            }
        }

        if !changed {
            return resolved;
        }
    }
}

/// The members of `{ a: T, b: U }` as `(key, type)` pairs, split at
/// the commas of depth one.
fn members(body: &str) -> Vec<(String, String)> {
    let inner = body.trim();
    let inner = inner
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(inner);
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();

    for (k, c) in inner.char_indices() {
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
            ',' if depth == 0 => {
                parts.push(&inner[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    parts.push(&inner[start..]);

    for part in parts {
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

    // A struct instance: `{ @metatable tN, { fields } }`.
    if let Some(rest) = trimmed.strip_prefix("{ @metatable ") {
        let comma = rest.find(',')?;
        let table = rest[comma + 1..].trim();
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

    let m = members(trimmed);
    let has = |key: &str| m.iter().any(|(k, _)| k == key);
    let get = |key: &str| m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());

    // The std containers, by the methods that name their arguments.
    if let Some(elem) = get("[number]")
        && has("concat")
        && has("push")
    {
        return Some(format!("{elem}[]"));
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

    // The rest of the std, by a member only it has.
    if let Some(sig) = get("Connect")
        && has("Fire")
        && has("DisconnectAll")
    {
        // `Connect: (self: t1, f: (A, B) -> ()) -> t2`
        let args = sig
            .find("f: (")
            .and_then(|i| {
                let from = i + 4;
                balanced_len(&sig[from - 1..]).map(|len| sig[from..from - 1 + len - 1].to_string())
            })
            .unwrap_or_default();

        return Some(format!("Signal<{args}>"));
    }

    if has("Connected") && has("Disconnect") && m.len() <= 4 {
        return Some("SignalConnection".to_string());
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
        let kind = if has("less") || has("sift_up") {
            "Heap"
        } else {
            "Queue"
        };

        return Some(format!("{kind}<{t}>"));
    }

    if has("next") && has("take_while") && has("collect") {
        let t = get("next")
            .and_then(|sig| {
                sig.find("-> ")
                    .map(|i| sig[i + 3..].trim_end_matches('?').to_string())
            })
            .unwrap_or_else(|| "any".to_string());

        return Some(format!("Iter<{t}>"));
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

/// A unit enum prints as the union of its names in alphabetical order;
/// a payload enum as that union with `{ _1: T, tag: "V" }` members.
fn fold_enums(text: &mut String, known: &Known) {
    for shape in &known.shapes {
        let Shape::Enum { name, variants } = shape else {
            continue;
        };

        if variants.len() < 2 {
            continue;
        }

        let mut units: Vec<String> = variants
            .iter()
            .filter(|(_, p)| p.is_empty())
            .map(|(v, _)| format!("\"{v}\""))
            .collect();
        units.sort();
        let payloads: Vec<String> = variants
            .iter()
            .filter(|(_, p)| !p.is_empty())
            .map(|(v, p)| {
                let fields: Vec<String> = p
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("_{}: {t}", i + 1))
                    .collect();

                format!("{{ {}, tag: \"{v}\" }}", fields.join(", "))
            })
            .collect();
        let mut members = units;
        members.extend(payloads);
        let printed = members.join(" | ");
        let mut from = 0;

        // A long union prints over several lines: the match reads past
        // any whitespace.
        while let Some(i) = text[from..].find(&members[0]) {
            let at = from + i;

            match match_loose(&text[at..], &printed) {
                Some(len) => {
                    text.replace_range(at..at + len, name);
                    from = at + name.len();
                }

                None => from = at + members[0].len(),
            }
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
    use super::*;

    fn known() -> Known {
        Known {
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
