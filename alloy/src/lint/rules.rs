//! The lint pass itself: `run` walks the tokens and the top-level
//! statements of one file and reports every hit. `directive_lints`,
//! `const_reassignments`, and `matching` are its private helpers.

use std::collections::{HashMap, HashSet};

use alloy_syntax::ast::{Chunk, ImportKind, Stmt};
use alloy_syntax::lexer::{Tok, TokKind};

use crate::fmt::structure;

use super::{Fix, Lint, Thresholds};

/// The lints about the directives themselves: an
/// `--@alloy-expect-error` with no reason after it.
fn directive_lints(src: &str) -> Vec<Lint> {
    let directives = crate::directives::scan(src);

    directives
        .missing_reason
        .iter()
        .map(|at| {
            let (line_start, end) = crate::directives::span_of_line(src, *at);
            // The directive may trail code; the lint is about the
            // directive, so the range starts at the comment.
            let start = src[line_start..end]
                .find(crate::directives::EXPECT)
                .map_or(line_start, |i| line_start + i);

            Lint {
                name: crate::directives::MISSING_REASON,
                start: start as u32,
                end: end as u32,
                message: format!(
                    "`{}` says nothing about why the line must fail; write the reason after it",
                    crate::directives::EXPECT
                ),
                fix: None,
            }
        })
        .collect()
}

/// `const N = 3` then `N = 4`: the byte range of each reassignment and
/// its message.
///
/// `alloy doc const` says a reassignment is a compile error. Luau
/// reports it as a syntax error in the emit, which only `alloy flux`
/// runs, and in the checker's words.
///
/// `globals` names the `global const` declarations of the project this
/// file reaches, each with the file that declares it. A `global` is in
/// scope everywhere, so an assignment to one here reads the same way.
pub fn const_reassignments(
    src: &str,
    toks: &[Tok],
    globals: &[(String, String)],
) -> Vec<(u32, u32, String)> {
    let text = |i: usize| toks.get(i).map(|t| t.text(src)).unwrap_or("");
    let lines = crate::fmt::structure::token_lines(src, toks);
    let starts = |i: usize| {
        i == 0
            || lines[i - 1] != lines[i]
            || matches!(text(i - 1), "then" | "do" | "else" | "end" | ";" | "repeat")
    };
    let mut names: Vec<&str> = Vec::new();

    for i in 0..toks.len() {
        if text(i) != "const"
            || !(starts(i) || matches!(text(i.wrapping_sub(1)), "export" | "global"))
        {
            continue;
        }

        let mut j = i + 1;

        while text(j) == "async" {
            j += 1;
        }

        if text(j) == "function" || toks.get(j).map(|t| t.kind) != Some(TokKind::Ident) {
            continue;
        }

        names.push(text(j));
    }

    if names.is_empty() && globals.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Ident
            || !starts(i)
            || !matches!(
                text(i + 1),
                "=" | "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "^=" | "..=" | "??="
            )
        {
            continue;
        }

        let name = text(i);
        // A `const` of this file wins: the file's own declaration is
        // the nearer one, and a global by that name never reaches here.
        let message = if names.contains(&name) {
            format!("`{name}` is a `const`; its value is set once and a reassignment is an error")
        } else if let Some((_, file)) = globals.iter().find(|(n, _)| n == name) {
            format!(
                "`{name}` is a `const` of {file}; its value is set once and a reassignment is an error"
            )
        } else {
            continue;
        };
        out.push((t.start, t.end, message));
    }

    out
}

/// One function in the token stream.
struct Fn {
    /// The `function` token.
    at: usize,
    /// The name path, empty for an anonymous function.
    path: Vec<usize>,
    /// The `(` of the parameter list.
    open: usize,
    /// The `)` of the parameter list.
    close: usize,
    /// The `end`, when the structure found it.
    end: Option<usize>,
    /// Parameters: name token, has a type, the type is optional.
    params: Vec<(usize, bool, bool)>,
    has_return_type: bool,
    optional_return: bool,
    /// The return annotation names `Result`.
    returns_result: bool,
    exported: bool,
    /// Inside an `impl` or a `trait`.
    in_impl: bool,
}

/// Runs the token and statement lints on one file.
/// `export_impl`: an `impl` on a foreign type wearing the old keyword.
/// `export` said "project wide" before `global` existed; `global impl`
/// says it now, and the rewrite is the one word.
fn export_impl(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    let text = |span: alloy_syntax::ast::TokSpan| -> &str {
        if span.end <= span.start || span.end as usize > toks.len() {
            return "";
        }

        &src[toks[span.start as usize].start as usize..toks[span.end as usize - 1].end as usize]
    };
    // A struct or an enum of this file is never foreign, whatever its name.
    let mut local: Vec<&str> = Vec::new();

    for stmt in &chunk.block.stmts {
        match stmt {
            Stmt::Struct(d) => local.push(text(d.name)),

            Stmt::Enum(d) => local.push(text(d.name)),

            _ => {}
        }
    }

    let mut out = Vec::new();

    for stmt in &chunk.block.stmts {
        let Stmt::Impl(i) = stmt else {
            continue;
        };
        let target = text(i.target);

        if !i.exported
            || i.global
            || local.contains(&target)
            || !crate::extensions::is_foreign(target)
        {
            continue;
        }

        let Some(word) = toks.get(i.span.start as usize) else {
            continue;
        };

        if word.text(src) != "export" {
            continue;
        }

        out.push(Lint {
            name: "export_impl",
            start: word.start,
            end: word.end,
            message: format!(
                "`impl {target}` works project wide; `global impl` is the word for that"
            ),
            fix: Some(crate::lint::Fix {
                start: word.start,
                end: word.end,
                replacement: "global".to_string(),
            }),
        });
    }

    out
}

/// A use of a namespace the file declares `@deprecated`. The attribute
/// has no Luau form on a namespace, so nothing else reports it.
fn deprecated_namespaces(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    let text = |span: alloy_syntax::ast::TokSpan| -> &str {
        match toks.get(span.start as usize) {
            Some(t) => t.text(src),

            None => "",
        }
    };
    // The name, the message the attribute carries, and the byte range
    // of the declaration. A member reads a sibling by its own name, so
    // a hit inside the body is the long way to write it, not a use.
    let mut marked: Vec<(&str, String, u32, u32)> = Vec::new();

    for stmt in &chunk.block.stmts {
        let Stmt::Namespace(ns) = stmt.under_default() else {
            continue;
        };
        let Some(attr) = ns
            .attributes
            .iter()
            .find(|a| a.name.map(text) == Some("deprecated"))
        else {
            continue;
        };
        let note = attr
            .args
            .first()
            .map(|e| {
                let s = &src[toks[e.span().start as usize].start as usize
                    ..toks[e.span().end as usize - 1].end as usize];

                format!("; {}", s.trim_matches(['"', '\'']))
            })
            .unwrap_or_default();
        let start = toks[ns.span.start as usize].start;
        let end = toks[ns.span.end as usize - 1].end;
        marked.push((text(ns.name), note, start, end));
    }

    if marked.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Ident {
            continue;
        }

        // A field of another value spelled the same names no namespace.
        if i > 0 && matches!(toks[i - 1].text(src), "." | ":" | "?." | "?:") {
            continue;
        }

        let Some((name, note, start, end)) = marked.iter().find(|(n, _, _, _)| *n == t.text(src))
        else {
            continue;
        };

        if t.start >= *start && t.start < *end {
            continue;
        }

        out.push(Lint {
            name: "deprecated_namespace",
            start: t.start,
            end: t.end,
            message: format!("`{name}` is deprecated{note}"),
            fix: None,
        });
    }

    out
}

/// An `import` under a statement that runs. The emit lifts every
/// `require` to the top of the file, so the line reads in an order the
/// run does not follow.
fn import_order(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    let mut out = Vec::new();
    let mut ran = false;
    // The line a statement opens, as the source wrote it. An ingot
    // rewrites the source before the lints read it, and a statement it
    // wrote lands on a line the reader did not write code on.
    let line_of = |stmt: &Stmt| -> &str {
        let Some(tok) = toks.get(stmt.span().start as usize) else {
            return "";
        };
        let at = tok.start as usize;
        let start = src[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = src[start..].find('\n').map_or(src.len(), |i| start + i);

        src[start..end].trim()
    };

    for stmt in &chunk.block.stmts {
        let Stmt::Import(i) = stmt else {
            // A declaration binds a name and a call runs; both stand
            // in front of the import in the source and behind it in the
            // emit.
            let text = line_of(stmt);
            ran |= !matches!(stmt, Stmt::Empty(_)) && !text.is_empty() && !text.starts_with("--");

            continue;
        };

        if !ran {
            continue;
        }

        let start = toks[i.span.start as usize].start;
        let end = toks[i.span.end as usize - 1].end;
        out.push(Lint {
            name: "import_order",
            start,
            end,
            message:
                "this `import` runs before the code above it; the imports go at the top of the file"
                    .to_string(),
            fix: None,
        });
    }

    out
}

/// `import Players from "game:Players"`: the service path of the
/// release before this one. The fix writes the alias form, `"@game"`
/// and `"@game/Players"`, and keeps the quote the line wrote.
fn game_alias(src: &str, toks: &[Tok], chunk: &Chunk) -> Vec<Lint> {
    let mut out = Vec::new();

    for stmt in &chunk.block.stmts {
        let Stmt::Import(i) = stmt else {
            continue;
        };
        let Some(first) = toks.get(i.path.start as usize) else {
            continue;
        };
        let end = toks[(i.path.end as usize)
            .saturating_sub(1)
            .max(i.path.start as usize)]
        .end;
        let written = &src[first.start as usize..end as usize];
        let quote = written.chars().next().unwrap_or('"');
        let spec = written.trim_matches(['"', '\'']);

        if !crate::game_import::is_old_spelling(spec) {
            continue;
        }

        let Some(alias) = crate::game_import::alias_form(spec) else {
            continue;
        };

        out.push(Lint {
            name: "game_alias",
            start: first.start,
            end,
            message: format!(
                "`{quote}{spec}{quote}` is the old service path; write `{quote}{alias}{quote}`"
            ),
            fix: Some(Fix {
                start: first.start,
                end,
                replacement: format!("{quote}{alias}{quote}"),
            }),
        });
    }

    out
}

pub fn run(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    definitions: bool,
    ingot_rewrite: bool,
    thresholds: &Thresholds,
    import_privates: &[(String, Vec<String>)],
) -> Vec<Lint> {
    let mut lints = Vec::new();

    if definitions {
        return lints;
    }

    lints.extend(directive_lints(src));
    lints.extend(export_impl(src, toks, chunk));
    lints.extend(deprecated_namespaces(src, toks, chunk));
    lints.extend(game_alias(src, toks, chunk));
    if !ingot_rewrite {
        lints.extend(import_order(src, toks, chunk));
    }

    let text = |i: usize| toks[i].text(src);
    let st = structure(src, toks);
    let line_of = |i: usize| st.lines[i];

    // The `impl` and `trait` blocks, as token ranges.
    let impl_ranges: Vec<(usize, usize)> = toks
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            matches!(t.text(src), "impl" | "trait")
                && !matches!(
                    i.checked_sub(1).map(text),
                    Some("." | ":" | "?." | "?:" | "function" | "local")
                )
        })
        .filter_map(|(i, _)| st.ends[i].map(|e| (i, e)))
        .collect();

    // Every function.
    let mut fns: Vec<Fn> = Vec::new();

    for (i, t) in toks.iter().enumerate() {
        if t.text(src) != "function" || matches!(i.checked_sub(1).map(text), Some("." | ":")) {
            continue;
        }

        let mut path = Vec::new();
        let mut j = i + 1;

        while j < toks.len()
            && matches!(toks[j].kind, TokKind::Ident | TokKind::Dot | TokKind::Colon)
        {
            if toks[j].kind == TokKind::Ident {
                path.push(j);
            }

            j += 1;
        }

        if j >= toks.len() || text(j) != "(" {
            continue;
        }

        let open = j;
        let Some(close) = matching(src, toks, open) else {
            continue;
        };
        let mut params = Vec::new();
        let mut k = open + 1;

        while k < close {
            // One parameter runs to the comma at depth zero.
            let mut depth = 0i32;
            let mut m = k;

            while m < close {
                let tt = text(m);

                // `<` opens a type argument list: `Result<T, E>` holds a
                // comma that separates no parameters.
                if tt.ends_with('(') || tt.ends_with('[') || tt.ends_with('{') || tt == "<" {
                    depth += 1;
                } else if matches!(tt, ")" | "]" | "}" | ">") {
                    depth -= 1;
                } else if tt == ">>" {
                    depth -= 2;
                } else if tt == "," && depth == 0 {
                    break;
                }

                m += 1;
            }

            if toks[k].kind == TokKind::Ident && text(k) != "self" {
                let typed = k + 1 < m && text(k + 1) == ":";
                let mut ty_end = m;

                for x in k + 2..m {
                    if text(x) == "=" {
                        ty_end = x;

                        break;
                    }
                }

                let optional = typed && ty_end > k + 2 && text(ty_end - 1) == "?";
                params.push((k, typed, optional));
            }

            k = m + 1;
        }

        let after = close + 1;
        let has_return_type = after < toks.len() && matches!(text(after), ":" | "->");
        let mut optional_return = false;
        let mut returns_result = false;

        if has_return_type {
            // The annotation runs to the end of the `)` line.
            let line = line_of(close);
            let mut last = after;

            while last + 1 < toks.len() && line_of(last + 1) == line {
                last += 1;
            }

            optional_return = text(last) == "?";
            returns_result = (after..=last).any(|k| text(k) == "Result");
        }

        let prev = i.checked_sub(1).map(text);
        let prev2 = i.checked_sub(2).map(text);
        let exported = matches!(prev, Some("export") | Some("global"))
            || (prev == Some("async") && matches!(prev2, Some("export") | Some("global")));
        let private =
            prev == Some("private") || (prev == Some("async") && prev2 == Some("private"));
        let in_impl = !private && impl_ranges.iter().any(|(a, b)| *a < i && i < *b);

        fns.push(Fn {
            at: i,
            path,
            open,
            close,
            end: st.ends[i],
            params,
            has_return_type,
            optional_return,
            returns_result,
            exported,
            in_impl,
        });
    }

    // Names the file declares, so a global of the same name is not one.
    let mut declared: HashSet<&str> = HashSet::new();

    for f in &fns {
        for (p, _, _) in &f.params {
            declared.insert(text(*p));
        }

        if let Some(&n) = f.path.first() {
            declared.insert(text(n));
        }
    }

    for (i, t) in toks.iter().enumerate() {
        match t.text(src) {
            "local" => {
                let mut j = i + 1;

                while j < toks.len() && toks[j].kind == TokKind::Ident {
                    declared.insert(text(j));
                    j += 1;

                    if j < toks.len() && text(j) == "," {
                        j += 1;
                    } else {
                        break;
                    }
                }
            }

            "for" => {
                let mut j = i + 1;

                while j < toks.len() && !matches!(text(j), "in" | "=" | "do") {
                    if toks[j].kind == TokKind::Ident {
                        declared.insert(text(j));
                    }

                    j += 1;
                }
            }

            _ => {}
        }
    }

    // implicit_any and missing_return_type.
    for f in &fns {
        let named = !f.path.is_empty();

        if named {
            for (p, typed, _) in &f.params {
                if !typed {
                    lints.push(Lint {
                        name: "implicit_any",
                        start: toks[*p].start,
                        end: toks[*p].end,
                        message: format!(
                            "parameter `{}` has no type, so it is `any`; write `{}: T`",
                            text(*p),
                            text(*p)
                        ),
                        fix: None,
                    });
                }
            }
        }

        if named && !f.has_return_type && (f.exported || f.in_impl) {
            let name_tok = *f.path.last().unwrap_or(&f.at);
            lints.push(Lint {
                name: "missing_return_type",
                start: toks[name_tok].start,
                end: toks[name_tok].end,
                message: format!(
                    "`{}` is public and has no return type; write `): T` after the parameters",
                    text(name_tok)
                ),
                fix: None,
            });
        }
    }

    // optional_access: a `T?` parameter indexed with nothing guarding it.
    for f in &fns {
        let Some(end) = f.end else { continue };

        for (p, _, optional) in &f.params {
            if !optional {
                continue;
            }

            let name = text(*p);
            let body = f.close + 1..end;
            let mut guarded = false;
            let mut first_access: Option<usize> = None;

            for i in body.clone() {
                if toks[i].kind != TokKind::Ident
                    || text(i) != name
                    || matches!(i.checked_sub(1).map(text), Some("." | ":" | "?." | "?:"))
                {
                    continue;
                }

                let prev = i.checked_sub(1).map(text);
                let prev2 = i.checked_sub(2).map(text);
                let next = toks.get(i + 1).map(|t| t.text(src));
                let guard_after = next.is_some_and(|n| {
                    matches!(n, "and" | "or" | "==" | "~=" | "=" | "??" | "!" | "," | ")")
                        || n.starts_with('?')
                });
                let guard_before = matches!(
                    prev,
                    Some(
                        "if" | "elseif"
                            | "not"
                            | "while"
                            | "until"
                            | "return"
                            | "="
                            | ","
                            | "("
                            | "{"
                    )
                ) && !(prev == Some("(")
                    && !matches!(prev2, Some("assert" | "typeof" | "type")))
                    || prev.is_none();
                // `return t` passes the optional on; `return t.x` reads
                // through it. The token after the name decides, so a
                // guard word before it never covers an access.
                let access = matches!(next, Some("." | ":" | "["));

                if !access && (guard_after || guard_before) {
                    guarded = true;

                    break;
                }

                if access && first_access.is_none() {
                    first_access = Some(i);
                }
            }

            if let (false, Some(i)) = (guarded, first_access) {
                lints.push(Lint {
                    name: "optional_access",
                    start: toks[i].start,
                    end: toks[i].end,
                    message: format!(
                        "`{name}` may be nil and nothing checks it; guard it with `if {name} then`, or index with `?.`"
                    ),
                    fix: None,
                });
            }
        }
    }

    // optional_access: a local annotated `T?` indexed with nothing
    // guarding it, the same shape the parameter rule reads.
    let mut optional_locals: Vec<(usize, usize)> = Vec::new();

    for i in 0..toks.len() {
        if text(i) != "local" && text(i) != "const" {
            continue;
        }

        let mut j = i + 1;

        while j + 1 < toks.len() && toks[j].kind == TokKind::Ident {
            let name_at = j;
            j += 1;

            // `local a: T?`: the type runs to the next `,` or `=` of
            // this statement.
            if j < toks.len() && text(j) == ":" {
                j += 1;
                let mut depth = 0i32;
                let mut last = j;

                while j < toks.len() {
                    let t = text(j);

                    if matches!(t, "(" | "[" | "{" | "<") {
                        depth += 1;
                    } else if matches!(t, ")" | "]" | "}" | ">") {
                        depth -= 1;
                    } else if depth == 0 && matches!(t, "," | "=") {
                        break;
                    }

                    last = j;
                    j += 1;
                }

                if last < toks.len() && text(last) == "?" {
                    optional_locals.push((name_at, j));
                }
            }

            if j >= toks.len() || text(j) != "," {
                break;
            }

            j += 1;
        }
    }

    for (name_at, from) in optional_locals {
        let name = text(name_at);
        let mut guarded = false;
        let mut first_access: Option<usize> = None;

        for i in from..toks.len() {
            if toks[i].kind != TokKind::Ident
                || text(i) != name
                || matches!(i.checked_sub(1).map(text), Some("." | ":" | "?." | "?:"))
            {
                continue;
            }

            let prev = i.checked_sub(1).map(text);
            let next = toks.get(i + 1).map(|t| t.text(src));
            let guard_after = next.is_some_and(|n| {
                matches!(n, "and" | "or" | "==" | "~=" | "=" | "??" | "!" | "," | ")")
                    || n.starts_with('?')
            });
            let guard_before = matches!(
                prev,
                Some("if" | "elseif" | "not" | "while" | "until" | "assert")
            );
            let access = matches!(next, Some("." | ":" | "["));

            if !access && (guard_after || guard_before) {
                guarded = true;

                break;
            }

            if access && first_access.is_none() {
                first_access = Some(i);
            }
        }

        if let (false, Some(i)) = (guarded, first_access) {
            lints.push(Lint {
                name: "optional_access",
                start: toks[i].start,
                end: toks[i].end,
                message: format!(
                    "`{name}` may be nil and nothing checks it; guard it with `if {name} then`, or index with `?.`"
                ),
                fix: None,
            });
        }
    }

    // optional_access: `f().x` where `f` returns `T?`.
    let optional_fns: HashSet<&str> = fns
        .iter()
        .filter(|f| f.optional_return && f.path.len() == 1)
        .map(|f| text(f.path[0]))
        .collect();

    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Ident
            || !optional_fns.contains(t.text(src))
            || matches!(
                i.checked_sub(1).map(text),
                Some("." | ":" | "function" | "local")
            )
            || toks.get(i + 1).map(|t| t.text(src)) != Some("(")
        {
            continue;
        }

        if let Some(close) = matching(src, toks, i + 1)
            && matches!(
                toks.get(close + 1).map(|t| t.text(src)),
                Some("." | ":" | "[")
            )
        {
            lints.push(Lint {
                name: "optional_access",
                start: t.start,
                end: toks[close].end,
                message: format!(
                    "`{}` returns a value that may be nil; guard the result before indexing it, or use `?.`",
                    t.text(src)
                ),
                fix: None,
            });
        }
    }

    // A token's text, or the empty string past the end.
    let word = |i: usize| toks.get(i).map(|t| t.text(src)).unwrap_or("");

    // dropped_result: a call statement of a function that answers with
    // a `Result`. The failure passes in silence.
    {
        let answers: Vec<&str> = fns
            .iter()
            .filter(|f| f.returns_result && f.path.len() == 1)
            .map(|f| text(f.path[0]))
            .collect();

        for i in 0..toks.len() {
            if toks[i].kind != TokKind::Ident
                || !answers.contains(&word(i))
                || word(i + 1) != "("
                || matches!(
                    i.checked_sub(1).map(text),
                    Some("." | ":" | "?." | "?:" | "function" | "local")
                )
                || (i > 0 && line_of(i - 1) == line_of(i))
            {
                continue;
            }

            let Some(close) = matching(src, toks, i + 1) else {
                continue;
            };

            // Anything after the call on the same line reads the value.
            if close + 1 < toks.len() && line_of(close + 1) == line_of(close) {
                continue;
            }

            let name = word(i);
            lints.push(Lint {
                name: "dropped_result",
                start: toks[i].start,
                end: toks[close].end,
                message: format!(
                    "`{name}` answers with a `Result` and nothing reads it; a failure passes in silence. Match it, write `if local Ok(v) = {name}(...)`, or take the value with a method"
                ),
                fix: None,
            });
        }
    }

    // static_call: `Wallet:new()` on a function that takes no `self`.
    // The colon passes the table as the first argument, which the
    // static never asked for.
    {
        let mut statics: Vec<(&str, &str)> = Vec::new();

        for (a, b) in &impl_ranges {
            if text(*a) != "impl" {
                continue;
            }

            let target = word(a + 1);

            for f in &fns {
                if f.path.len() != 1 || f.at < *a || f.at > *b || word(f.open + 1) == "self" {
                    continue;
                }

                statics.push((target, text(f.path[0])));
            }
        }

        for i in 0..toks.len() {
            if toks[i].kind != TokKind::Ident
                || word(i + 1) != ":"
                || toks.get(i + 2).map(|t| t.kind) != Some(TokKind::Ident)
                || word(i + 3) != "("
                || matches!(i.checked_sub(1).map(text), Some("." | ":" | "?." | "?:"))
            {
                continue;
            }

            let (owner, member) = (word(i), word(i + 2));

            if !statics.contains(&(owner, member)) {
                continue;
            }

            lints.push(Lint {
                name: "static_call",
                start: toks[i].start,
                end: toks[i + 2].end,
                message: format!(
                    "`{member}` is not a method; call it with `{owner}.{member}(...)`, not `{owner}:{member}(...)`"
                ),
                fix: Some(Fix {
                    start: toks[i + 1].start,
                    end: toks[i + 1].end,
                    replacement: ".".to_string(),
                }),
            });
        }
    }

    // argument_count: a call with more arguments than the function
    // takes. Luau's solver reports too few and misses too many, and the
    // extra values are dropped in silence.
    {
        // The functions the file declares once, by a plain name, with a
        // fixed parameter list: their arity is exact.
        let mut arity: HashMap<&str, Option<usize>> = HashMap::new();

        for f in &fns {
            if f.path.len() != 1 || f.in_impl {
                continue;
            }

            let name = text(f.path[0]);
            let mut depth = 0i32;
            let mut fixed = true;
            // One slot per comma at depth zero, so a destructured
            // parameter counts as the one argument it takes.
            let mut slots = usize::from(f.close > f.open + 1);

            for k in f.open + 1..f.close {
                let tt = text(k);

                if tt.ends_with('(') || tt.ends_with('[') || tt.ends_with('{') || tt == "<" {
                    depth += 1;
                } else if matches!(tt, ")" | "]" | "}" | ">") {
                    depth -= 1;
                } else if depth == 0 && tt == "," {
                    slots += 1;
                } else if depth == 0 && (tt == "..." || tt == "=") {
                    // A vararg or a default makes the count a range.
                    fixed = false;

                    break;
                }
            }

            if word(f.open + 1) == "self" {
                slots = slots.saturating_sub(1);
            }

            let takes = fixed.then_some(slots);

            arity.entry(name).and_modify(|e| *e = None).or_insert(takes);
        }

        for i in 0..toks.len() {
            if toks[i].kind != TokKind::Ident
                || matches!(
                    i.checked_sub(1).map(text),
                    Some("." | ":" | "?." | "?:" | "function" | "local")
                )
                || toks.get(i + 1).map(|t| t.text(src)) != Some("(")
            {
                continue;
            }

            let Some(Some(takes)) = arity.get(text(i)).copied() else {
                continue;
            };
            let Some(close) = matching(src, toks, i + 1) else {
                continue;
            };

            if close == i + 2 {
                continue;
            }

            let mut depth = 0i32;
            let mut given = 1usize;

            for k in i + 2..close {
                let tt = text(k);

                if tt.ends_with('(') || tt.ends_with('[') || tt.ends_with('{') {
                    depth += 1;
                } else if matches!(tt, ")" | "]" | "}") {
                    depth -= 1;
                } else if tt == "," && depth == 0 {
                    given += 1;
                }
            }

            if given <= takes {
                continue;
            }

            let name = text(i);
            let word = |n: usize| if n == 1 { "argument" } else { "arguments" };
            lints.push(Lint {
                name: "argument_count",
                start: toks[i].start,
                end: toks[close].end,
                message: format!(
                    "`{name}` takes {takes} {}; this call passes {given}",
                    word(takes)
                ),
                fix: None,
            });
        }
    }

    // deprecated_global.
    for (i, t) in toks.iter().enumerate() {
        let name = t.text(src);

        if t.kind != TokKind::Ident
            || !matches!(name, "wait" | "spawn" | "delay" | "unpack")
            || declared.contains(name)
            || matches!(
                i.checked_sub(1).map(text),
                Some("." | ":" | "?." | "?:" | "function" | "local")
            )
            || toks.get(i + 1).map(|t| t.text(src)) != Some("(")
        {
            continue;
        }

        let replacement = if name == "unpack" {
            "table.unpack".to_string()
        } else {
            format!("task.{name}")
        };
        lints.push(Lint {
            name: "deprecated_global",
            start: t.start,
            end: t.end,
            message: if name == "unpack" {
                "`unpack` is the legacy global; call `table.unpack` instead".to_string()
            } else {
                format!("`{name}` is the legacy scheduler; call `task.{name}` instead")
            },
            fix: Some(Fix {
                start: t.start,
                end: t.end,
                replacement,
            }),
        });
    }

    // unused_import.
    for stmt in &chunk.block.stmts {
        let Stmt::Import(im) = stmt else { continue };
        let after = toks[im.span.end as usize - 1].end;
        let bound: Vec<u32> = match &im.kind {
            ImportKind::Namespace(n) | ImportKind::Default(n) => vec![n.start],

            ImportKind::Both(n, specs) => std::iter::once(n.start)
                .chain(specs.iter().map(|s| s.alias.unwrap_or(s.name).start))
                .collect(),

            ImportKind::Named(specs) | ImportKind::TypeOnly(specs) => specs
                .iter()
                .map(|s| s.alias.unwrap_or(s.name).start)
                .collect(),
        };

        for tok_index in bound {
            let n = toks[tok_index as usize];
            let name = n.text(src);
            let used = toks
                .iter()
                .any(|t| t.start >= after && t.kind == TokKind::Ident && t.text(src) == name);

            if !used {
                lints.push(Lint {
                    name: "unused_import",
                    start: n.start,
                    end: n.end,
                    message: format!("`{name}` is imported and never used"),
                    fix: None,
                });
            }
        }
    }

    let scan = crate::flux::scan::Scan::new(src, toks, &st).with_privates(import_privates);
    lints.extend(crate::flux::run(&scan));
    lints.extend(crate::flux::correctness::run(&scan));
    lints.extend(crate::flux::complexity::run(&scan, thresholds));
    lints.extend(crate::flux::roblox::run(&scan));
    lints.sort_by_key(|l| l.start);
    lints
}

/// The index of the bracket that closes the one at `open`.
fn matching(src: &str, toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, t) in toks.iter().enumerate().skip(open) {
        let text = t.text(src);

        if text.ends_with('(') || text.ends_with('[') || text.ends_with('{') {
            depth += 1;
        } else if matches!(text, ")" | "]" | "}") {
            depth -= 1;

            if depth == 0 {
                return Some(i);
            }
        }
    }

    None
}
