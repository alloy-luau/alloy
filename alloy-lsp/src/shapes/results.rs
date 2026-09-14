//! A `Result`, in every form the checker prints one: a tagged table met
//! with its method table, one arm alone as `ResultOk` or `ResultErr`, a
//! mapped or a cut print, and the aliases the std spells the parts with.

use super::strings::{balanced_len, enclosing_brace, group_len, group_start, members, split_union};

/// `ResultMethods<T, E> & { ... }` is how a Result's method table meets
/// its data half. The methods are the same for every Result, so the
/// data half alone says what the value is.
pub(crate) fn drop_result_methods(text: &str) -> String {
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

        // A hint cuts the data half, `{ read _1: T, ... 4 more ... }`,
        // and the pair folds find no `tag` in it. The method table's
        // arguments still name the Result whole.
        if let Some(name) = cut_result_name(&out[at..end], &out[end..]) {
            let data = end + balanced_len(&out[end..]).unwrap_or(0);
            let group = out[..at].ends_with('(') && out[data..].starts_with(')');
            let (start, stop) = match group {
                true => (at - 1, data + 1),

                false => (at, data),
            };
            out.replace_range(start..stop, &name);
            from = start + name.len();

            continue;
        }

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

/// `Result<T, E>` for a `ResultMethods<T, E> & ` head whose data half
/// the child cut short; `None` when the data half is whole.
fn cut_result_name(head: &str, data: &str) -> Option<String> {
    let len = balanced_len(data)?;

    if !data.starts_with('{') || !data[..len].contains(" more ...") {
        return None;
    }

    let open = head.find('<')?;
    let args = &head[open..open + group_len(&head[open..], '<', '>')?];

    Some(format!("Result{args}"))
}

/// `Result<T, E> & { read expect: ..., read map: ... }`: the method
/// table printed in place, where the alias did not reach the print. It
/// is the same for every Result and names nothing.
pub(crate) fn fold_inline_result_methods(text: &mut String) {
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
            let close = open + len;
            // The group keeps one table now, and one type needs no
            // parentheses. Left in place they print the arms of a
            // Result as `({ ... }) | ({ ... })`, which the pair folds
            // do not read.
            let paren = open_of(&text[..at])
                .filter(|b| text[..*b].ends_with('(') && text[close..].starts_with(')'));

            match paren {
                Some(b) => {
                    text.replace_range(close..close + 1, "");
                    text.replace_range(at..close, "");
                    text.replace_range(b - 1..b, "");
                    from = b - 1;
                }

                None => {
                    text.replace_range(at..close, "");
                    from = at;
                }
            }

            continue;
        }

        from = open;
    }
}

/// A mapped result prints as one table with `tag: "Ok" | "Err"` and
/// `read _1: T | E`; it reads as `Result<T, E>`.
pub(crate) fn fold_lite_results(text: &mut String) {
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
pub(crate) fn fold_cut_results(text: &mut String) {
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
pub(crate) fn fold_result_aliases(text: &mut String) {
    for head in [
        "ResultOk<",
        "ResultErr<",
        "Result2<",
        "Result3<",
        "ResultMethods2<",
        "ResultMethods3<",
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

/// `{ read _1: T, ..., tag: "Ok", ... } | { read _1: E, ..., tag: "Err", ... }`
/// as `Result<T, E>`, in either order, innermost first when one nests
/// inside another's method.
pub(crate) fn fold_results(text: &mut String) {
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
pub(crate) fn fold_tagged_results(text: &mut String) {
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

    // A Result inside the alias arguments carries a ` & {` of its own,
    // and it stands before this group's. Step over the arguments so the
    // brace is the one this group meets its methods with.
    let past = rest
        .find('<')
        .filter(|open| *open < amp)
        .and_then(|open| group_len(&rest[open..], '<', '>').map(|len| open + len));

    let amp = match past {
        Some(skip) => skip + rest[skip..].find(" & {")?,

        None => amp,
    };

    Some(after_paren + amp + 3)
}

/// One group of a printed Result, given the `{` of its tagged table:
/// the group's start (its `(`), its end (past the `)`), the `__ok` and
/// `__err` types, and the tag. `None` when the shape is not a Result
/// member.
fn result_group(text: &str, brace: usize) -> Option<(usize, usize, String, String, String)> {
    let table_len = balanced_len(&text[brace..])?;
    let table_members = members(&text[brace..brace + table_len]);
    let tag = table_members
        .iter()
        .find(|(k, _)| k == "tag")
        .map(|(_, v)| v.trim_matches('"').to_string())?;

    if tag != "Ok" && tag != "Err" {
        return None;
    }

    let ok = table_members
        .iter()
        .find(|(k, _)| k == "__ok")
        .map(|(_, v)| v.clone())?;
    let err = table_members
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
        let mut name_at = alias_head(head, "ResultMethods")?;

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

/// The offset of `name` when a text ends with `name<...>`. The walk
/// starts at the closing `>`, so a `ResultMethods` inside the arguments
/// of another is no candidate.
fn alias_head(text: &str, name: &str) -> Option<usize> {
    if !text.ends_with('>') {
        return None;
    }

    let mut depth = 0i32;

    for (k, c) in text.char_indices().rev() {
        match c {
            '>' => depth += 1,
            '<' => {
                depth -= 1;

                if depth == 0 {
                    return text[..k].ends_with(name).then(|| k - name.len());
                }
            }
            _ => {}
        }
    }

    None
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

#[cfg(test)]
mod tests {
    use super::super::{Known, fold};

    #[test]
    fn a_function_returning_a_result_folds() {
        let text = "```luau\nfunction Result.pcall(f: (...any) -> (...any), ...: any): { read _1: any, expect: (self: any, message: string) -> any, is_err: (self: any) -> boolean, is_ok: (self: any) -> boolean, map: (self: any, f: (any) -> any) -> Result<any, string>, map_err: (self: any, f: (string) -> any) -> Result<any, any>, ok: (self: any) -> any?, tag: \"Ok\", trace: string?, unwrap: (self: any) -> any, unwrap_or: (self: any, default: any) -> any } | { read _1: string, expect: (self: any, message: string) -> any, is_err: (self: any) -> boolean, is_ok: (self: any) -> boolean, map: (self: any, f: (any) -> any) -> Result<any, string>, map_err: (self: any, f: (string) -> any) -> Result<any, any>, ok: (self: any) -> any?, tag: \"Err\", trace: string?, unwrap: (self: any) -> any, unwrap_or: (self: any, default: any) -> any }\n```";
        assert_eq!(
            fold(text, &Known::default()),
            "```luau\nfunction Result.pcall(f: (...any) -> (...any), ...: any): Result<any, string>\n```"
        );
    }

    // A Result whose value side is another Result, which is what
    // `Result<Result<number, any>, any>` prints as. The inner pair
    // carries a `ResultMethods` and a ` & {` of its own, and the outer
    // group read both of those as its own.
    #[test]
    fn a_result_inside_a_result_folds() {
        let inner = "(ResultMethods<number, any> & { read _1: any, read __err: any, read __ok: number, tag: \"Err\", read trace: string? }) | (ResultMethods<number, any> & { read _1: number, read __err: any, read __ok: number, tag: \"Ok\", read trace: string? })";
        let outer = format!(
            "(ResultMethods<{inner}, any> & {{ read _1: {inner}, read __err: any, read __ok: {inner}, tag: \"Ok\", read trace: string? }}) | (ResultMethods<{inner}, any> & {{ read _1: any, read __err: any, read __ok: {inner}, tag: \"Err\", read trace: string? }})"
        );

        assert_eq!(
            fold(&outer, &Known::default()),
            "Result<Result<number, any>, any>"
        );
    }

    // An inferred `local outer = nested()` where `nested` returns
    // `Result<Result<number, string>, string>`. The alias does not
    // reach that print: each arm meets its methods in place, inside
    // parentheses of its own.
    #[test]
    fn a_result_inside_a_result_folds_with_the_methods_in_place() {
        let methods = "{ read expect: any, read is_err: any, read is_ok: any, read map: any, read map_err: any, read ok: any, read unwrap: any, read unwrap_or: any }";
        let inner = format!(
            "({{ read _1: number, read __err: string, read __ok: number, tag: \"Ok\", read trace: string? }} & {methods}) | ({{ read _1: string, read __err: string, read __ok: number, tag: \"Err\", read trace: string? }} & {methods})"
        );
        let outer = format!(
            "local outer: ({{ read _1: {inner}, read __err: string, read __ok: {inner}, tag: \"Ok\", read trace: string? }} & {methods}) | ({{ read _1: string, read __err: string, read __ok: {inner}, tag: \"Err\", read trace: string? }} & {methods})"
        );
        let out = fold(&outer, &Known::default());

        assert_eq!(out, "local outer: Result<Result<number, string>, string>");

        for emit in ["_1", "__err", "__ok", "tag:", "trace"] {
            assert!(!out.contains(emit), "{emit} reached the hover: {out}");
        }
    }

    // The std splits a Result into three rungs so `r:map(f):map(g)`
    // keeps its value type. `Result3` and `ResultMethods3` are names no
    // source writes, and neither may reach a reader.
    #[test]
    fn the_third_result_rung_reads_by_name() {
        for text in [
            "local r: Result3<number, any>",
            "local r: (Result3<number, any>)",
            "local r: ResultMethods3<number, any>",
        ] {
            let out = fold(text, &Known::default());

            assert_eq!(out, "local r: Result<number, any>", "{text}");
        }
    }
}
