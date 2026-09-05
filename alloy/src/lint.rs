//! `alloy lint`: the checks that are not errors.
//!
//! A diagnostic from the compiler stops a build. A lint is advice: the
//! program runs, and the lint names a habit that costs bugs. Each lint
//! has a name, a default level, and a switch in `[lint]` of `alloy.toml`.
//!
//! The lints here read tokens and the top-level statements. The ones
//! that need the enum table, `unreachable_default` and `empty_default`,
//! run inside the desugar and land in the same list.

use std::collections::HashSet;

use alloy_syntax::ast::{Chunk, ImportKind, Stmt};
use alloy_syntax::lexer::{Tok, TokKind};

use crate::config::LintConfig;
use crate::fmt::structure;

/// One lint hit: a byte range in the source and the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lint {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub message: String,
}

/// What a lint does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Silent.
    Allow,
    /// Printed; the exit code stays zero.
    Warn,
    /// Printed; the exit code is one.
    Deny,
}

/// The description of one lint, for `alloy doc lints` and `--list`.
pub struct LintInfo {
    pub name: &'static str,
    /// The level with no `[lint]` table. `Allow` marks a strict-only
    /// lint, which `strict = true` raises to `Warn`.
    pub default: Level,
    pub summary: &'static str,
    pub detail: &'static str,
}

/// Every lint, in the order the docs list them.
pub const LINTS: &[LintInfo] = &[
    LintInfo {
        name: "optional_access",
        default: Level::Warn,
        summary: "a value that may be nil is indexed without a guard",
        detail: "A parameter typed `T?`, or the result of a function that returns `T?`, is indexed with `.` or called with `:` while nothing in the function checks it for nil. Guard it with `if x then`, `x and`, `assert(x)`, or use `?.` and `?:`, which stop the chain at nil.",
    },
    LintInfo {
        name: "unreachable_default",
        default: Level::Warn,
        summary: "a `default` arm after arms that cover every variant",
        detail: "The arms of this `match` already cover every variant of the enum, so the `default` arm never runs. Delete it: with no `default`, the compiler reports a new variant as a missing arm instead of routing it here in silence.",
    },
    LintInfo {
        name: "empty_default",
        default: Level::Warn,
        summary: "a `default` arm with an empty body",
        detail: "An empty `default` swallows every variant the arms do not name, including the ones added later. Name the variants, or write the fallback the default stands for.",
    },
    LintInfo {
        name: "deprecated_global",
        default: Level::Warn,
        summary: "a call to `wait`, `spawn`, or `delay`",
        detail: "The legacy scheduler globals run on the 30 Hz legacy pipeline and their timing drifts. `task.wait`, `task.spawn`, `task.delay`, and `task.defer` are the replacements.",
    },
    LintInfo {
        name: "unused_import",
        default: Level::Warn,
        summary: "an imported name the file never uses",
        detail: "The name an `import` binds appears nowhere after the import. The require still runs, so the module loads for nothing. Remove the name, or the import.",
    },
    LintInfo {
        name: "implicit_any",
        default: Level::Allow,
        summary: "a named function parameter with no type",
        detail: "Strict only. A parameter of a named function with no annotation is `any` to the checker, and every use of it goes unchecked. Write the type. A callback passed as an argument is exempt: the checker infers its parameters from the callee.",
    },
    LintInfo {
        name: "missing_return_type",
        default: Level::Allow,
        summary: "a public function with no return type",
        detail: "Strict only. An exported function, or a method in an `impl`, is an interface others call; without a return annotation, a change to its body changes its type in silence. Write the return type.",
    },
];

/// The level a lint runs at under a config.
pub fn level_of(config: &LintConfig, name: &str) -> Level {
    if config.allow.iter().any(|n| n == name) {
        return Level::Allow;
    }

    if config.deny.iter().any(|n| n == name) {
        return Level::Deny;
    }

    if config.warn.iter().any(|n| n == name) {
        return Level::Warn;
    }

    match LINTS.iter().find(|l| l.name == name) {
        Some(l) if l.default == Level::Allow && config.strict => Level::Warn,
        Some(l) => l.default,
        None => Level::Warn,
    }
}

/// A `[lint]` name that no lint carries.
pub fn unknown_names(config: &LintConfig) -> Vec<String> {
    config
        .allow
        .iter()
        .chain(&config.warn)
        .chain(&config.deny)
        .filter(|n| !LINTS.iter().any(|l| l.name == n.as_str()))
        .cloned()
        .collect()
}

/// One function in the token stream.
struct Fn {
    /// The `function` token.
    at: usize,
    /// The name path, empty for an anonymous function.
    path: Vec<usize>,
    /// The `)` of the parameter list.
    close: usize,
    /// The `end`, when the structure found it.
    end: Option<usize>,
    /// Parameters: name token, has a type, the type is optional.
    params: Vec<(usize, bool, bool)>,
    has_return_type: bool,
    optional_return: bool,
    exported: bool,
    /// Inside an `impl` or a `trait`.
    in_impl: bool,
}

/// Runs the token and statement lints on one file.
pub fn run(src: &str, toks: &[Tok], chunk: &Chunk, definitions: bool) -> Vec<Lint> {
    let mut lints = Vec::new();

    if definitions {
        return lints;
    }

    let text = |i: usize| toks[i].text(src);
    let st = structure(src, toks);
    let line_of = |i: usize| src[..toks[i].start as usize].matches('\n').count();

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

                if tt.ends_with('(') || tt.ends_with('[') || tt.ends_with('{') {
                    depth += 1;
                } else if matches!(tt, ")" | "]" | "}") {
                    depth -= 1;
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

        if has_return_type {
            // The annotation runs to the end of the `)` line.
            let line = line_of(close);
            let mut last = after;

            while last + 1 < toks.len() && line_of(last + 1) == line {
                last += 1;
            }

            optional_return = text(last) == "?";
        }

        let prev = i.checked_sub(1).map(text);
        let prev2 = i.checked_sub(2).map(text);
        let exported = prev == Some("export") || (prev == Some("async") && prev2 == Some("export"));
        let in_impl = impl_ranges.iter().any(|(a, b)| *a < i && i < *b);

        fns.push(Fn {
            at: i,
            path,
            close,
            end: st.ends[i],
            params,
            has_return_type,
            optional_return,
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

                if guard_after || guard_before {
                    guarded = true;

                    break;
                }

                if matches!(next, Some("." | ":" | "[")) && first_access.is_none() {
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
                });
            }
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
            });
        }
    }

    // deprecated_global.
    for (i, t) in toks.iter().enumerate() {
        let name = t.text(src);

        if t.kind != TokKind::Ident
            || !matches!(name, "wait" | "spawn" | "delay")
            || declared.contains(name)
            || matches!(
                i.checked_sub(1).map(text),
                Some("." | ":" | "?." | "?:" | "function" | "local")
            )
            || toks.get(i + 1).map(|t| t.text(src)) != Some("(")
        {
            continue;
        }

        lints.push(Lint {
            name: "deprecated_global",
            start: t.start,
            end: t.end,
            message: format!("`{name}` is the legacy scheduler; call `task.{name}` instead"),
        });
    }

    // unused_import.
    for stmt in &chunk.block.stmts {
        let Stmt::Import(im) = stmt else { continue };
        let after = toks[im.span.end as usize - 1].end;
        let bound: Vec<u32> = match &im.kind {
            ImportKind::Namespace(n) => vec![n.start],

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
                });
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<&'static str> {
        let out = crate::compile(src).unwrap();

        out.lints.iter().map(|l| l.name).collect()
    }

    #[test]
    fn an_unguarded_optional_parameter_is_a_lint() {
        assert_eq!(
            names("local function f(p: Player?)\n    print(p.Name)\nend\n"),
            vec!["optional_access"]
        );
        assert_eq!(
            names("local function f(p: Player?)\n    if p then print(p.Name) end\nend\n"),
            Vec::<&str>::new()
        );
        assert_eq!(
            names("local function f(p: Player?)\n    print(p?.Name)\nend\n"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn indexing_an_optional_result_is_a_lint() {
        let src = "local function find(): Player?\n    return nil\nend\nprint(find().Name)\n";
        assert_eq!(names(src), vec!["optional_access"]);
    }

    #[test]
    fn a_default_that_cannot_run_is_a_lint() {
        let src = "enum C as A, B end\nlocal c: C = C.A\nmatch c with\n    case A then print(1)\n    case B then print(2)\n    default print(3)\nend\n";
        assert_eq!(names(src), vec!["unreachable_default"]);
    }

    #[test]
    fn an_empty_default_is_a_lint() {
        let src = "enum C as A, B end\nlocal c: C = C.A\nmatch c with\n    case A then print(1)\n    default\nend\n";
        assert_eq!(names(src), vec!["empty_default"]);
    }

    #[test]
    fn the_legacy_scheduler_is_a_lint_unless_declared() {
        assert_eq!(names("wait(1)\n"), vec!["deprecated_global"]);
        assert_eq!(names("local wait = 1\nprint(wait)\n"), Vec::<&str>::new());
        assert_eq!(names("task.wait(1)\n"), Vec::<&str>::new());
    }

    #[test]
    fn an_unused_import_is_a_lint() {
        assert_eq!(
            names("import { a, b } from \"./m\"\nprint(a)\n"),
            vec!["unused_import"]
        );
    }

    #[test]
    fn strict_lints_are_off_until_the_config_turns_them_on() {
        let src = "export function f(x)\n    return x\nend\n";
        let out = crate::compile(src).unwrap();
        let mut hits: Vec<&str> = out.lints.iter().map(|l| l.name).collect();
        hits.sort();
        assert_eq!(hits, vec!["implicit_any", "missing_return_type"]);

        let lax = LintConfig::default();
        assert_eq!(level_of(&lax, "implicit_any"), Level::Allow);

        let strict = LintConfig {
            strict: true,
            ..LintConfig::default()
        };
        assert_eq!(level_of(&strict, "implicit_any"), Level::Warn);
        assert_eq!(level_of(&strict, "optional_access"), Level::Warn);

        let denied = LintConfig {
            deny: vec!["optional_access".to_string()],
            ..LintConfig::default()
        };
        assert_eq!(level_of(&denied, "optional_access"), Level::Deny);
    }

    #[test]
    fn the_examples_carry_no_default_lints() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let config = LintConfig::default();

        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "aly") {
                let src = std::fs::read_to_string(&path).unwrap();
                let options = crate::EmitOptions {
                    definitions: path.to_string_lossy().ends_with(".d.aly"),
                    ..Default::default()
                };
                let out = crate::compile_with(&src, &options).unwrap();
                let live: Vec<&Lint> = out
                    .lints
                    .iter()
                    .filter(|l| level_of(&config, l.name) != Level::Allow)
                    .collect();
                assert!(live.is_empty(), "{}: {live:?}", path.display());
            }
        }
    }
}
