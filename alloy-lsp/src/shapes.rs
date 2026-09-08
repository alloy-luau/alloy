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
                || s.contains(" | ")
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

        // Every name the head uses resolved: the rest of the clause,
        // parsed or not, is for those names alone.
        let clause_end = out[tail_start..]
            .find("\n```")
            .map_or(out.len(), |k| tail_start + k);
        out.replace_range(head_start..clause_end.max(tail_start + tail_end), &new_head);
    }

    fold_heads(&mut out, known);
    fold_tagged_results(&mut out);
    fold_results(&mut out);
    fold_lite_results(&mut out);
    fold_symbols(&mut out);

    // An enum inside another's payload folds first, and then the outer.
    for _ in 0..3 {
        let before = out.len();
        fold_enums(&mut out, known);

        if out.len() == before {
            break;
        }
    }
    out = fold_private_views(&out);
    fold_full_views(&mut out, known);
    fold_array_alias(&mut out);
    fold_narrowed_primitives(&mut out);
    fold_cut_array(&mut out);
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

    out
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

        if inner.contains('{') || inner.contains(' ') {
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

/// A hint the child cut short, `: t1 where t1 = { [number]: any, concat:
/// (read any[], t1) -> t1, ...`, still names an array by its head: the
/// indexer and the first method are enough.
fn fold_cut_array(text: &mut String) {
    let mut from = 0;

    while let Some(i) = text[from..].find(" where ") {
        let at = from + i;
        let (head_start, head) = head_of(text, at);
        let var = head.trim().trim_end_matches('?');
        let optional = head.trim().ends_with('?');
        let clause = &text[at + " where ".len()..];
        let Some(body) = clause.strip_prefix(&format!("{var} = {{ [number]: ")) else {
            from = at + 1;
            continue;
        };
        let Some(comma) = body.find(", ") else {
            from = at + 1;
            continue;
        };
        let elem = body[..comma].to_string();
        let rest = &body[comma + 2..];

        if !(rest.starts_with("concat:") || rest.starts_with("push:") || rest.starts_with("len:")) {
            from = at + 1;
            continue;
        }

        let end = text[at..].find('\n').map_or(text.len(), |n| at + n);
        let name = if elem.contains(' ') || elem.contains('|') {
            format!("({elem})[]")
        } else {
            format!("{elem}[]")
        };
        let name = if optional { format!("{name}?") } else { name };
        text.replace_range(head_start..end, &name);
        from = head_start + name.len();
    }
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

        match text[inner_start..].find('>') {
            Some(n)
                if !prefixed
                    && text[inner_start..inner_start + n].chars().all(|c| {
                        c.is_alphanumeric() || matches!(c, '_' | '.' | '?' | '[' | ']')
                    }) =>
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
    // A head printed over several lines ends in a bracket before the
    // `where`; the type starts on the line that opens it.
    let before = text[..where_at].trim_end();
    let group_start = before
        .chars()
        .next_back()
        .filter(|c| matches!(c, '}' | ')' | ']'))
        .and_then(|_| enclosing_open(text, before.len() - 1))
        .unwrap_or(where_at);
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
/// The members of a table body split at the commas of depth one, with
/// their modifiers.
fn member_parts(body: &str) -> Vec<&str> {
    let inner = body.trim();
    let inner = inner
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(inner);
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

    parts
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

    let m = members(trimmed);
    let has = |key: &str| m.iter().any(|(k, _)| k == key);
    let get = |key: &str| m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());

    // The object a `remote` declaration binds.
    if has("fire_all") && has("on_ratelimited") && has("wait") {
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
            for shape in &known.shapes {
                let Shape::Struct { name, fields } = shape else {
                    continue;
                };
                let all: Vec<&String> = fields.iter().map(|(f, _)| f).collect();
                let public: Vec<&String> =
                    fields.iter().filter(|(_, p)| !p).map(|(f, _)| f).collect();

                if same_set(&keys, &all) || same_set(&keys, &public) {
                    let head = if all_read { "Readonly" } else { "Partial" };

                    return Some(format!("{head}<{name}>"));
                }
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
        let one = "local r: ResultMethods<number, string> & { read _1: number, read __err: string, read __ok: number, tag: \"Ok\", read trace: string? }";
        assert_eq!(
            fold(one, &Known::default()),
            "local r: ResultOk<number, string>"
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
