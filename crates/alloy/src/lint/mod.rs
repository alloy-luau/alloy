//! `alloy lint`: the checks that are not errors.
//!
//! A diagnostic from the compiler stops a build. A lint is advice: the
//! program runs, and the lint names a habit that costs bugs. Each lint
//! has a name, a default level, and a switch in `[lint.rules]` of
//! `alloy.toml`.
//!
//! The lints here read tokens and the top-level statements. The ones
//! that need the enum table, `unreachable_default` and `empty_default`,
//! run inside the desugar and land in the same list.

mod rules;

use std::collections::HashSet;

use crate::config::LintConfig;

pub use rules::{const_reassignments, namespace_consts, run};

/// One lint hit: a byte range in the source, the message, and the
/// rewrite when the lint has one that keeps the program the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lint {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub message: String,
    pub fix: Option<Fix>,
}

/// A rewrite `alloy lint --fix` applies: the bytes from `start` to
/// `end` become `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub start: u32,
    pub end: u32,
    pub replacement: String,
    /// The bytes the lint read at that range. The applier compares them
    /// with the file it writes, so a range that came from another text
    /// never lands on the author's source.
    pub saw: String,
    /// The rewrites that land with this one, all or none. A rename
    /// writes the new name at each read; the others leave it empty.
    pub more: Vec<Fix>,
}

impl Fix {
    /// A rewrite of `src[start..end]`, which keeps those bytes.
    pub fn new(src: &str, start: u32, end: u32, replacement: impl Into<String>) -> Fix {
        Fix {
            start,
            end,
            replacement: replacement.into(),
            saw: src
                .get(start as usize..end as usize)
                .unwrap_or_default()
                .to_string(),
            more: Vec::new(),
        }
    }

    /// This rewrite and the ones that land with it.
    pub fn edits(&self) -> impl Iterator<Item = &Fix> {
        std::iter::once(self).chain(&self.more)
    }

    /// The rewrite as one range, from its first byte to its last, for a
    /// reader that takes one range per fix.
    pub fn as_one(&self, src: &str) -> Fix {
        let start = self.edits().map(|e| e.start).min().unwrap_or(self.start);
        let end = self.edits().map(|e| e.end).max().unwrap_or(self.end);
        let mut edits: Vec<&Fix> = self.edits().collect();
        edits.sort_by_key(|e| e.start);
        let mut text = src
            .get(start as usize..end as usize)
            .unwrap_or_default()
            .to_string();

        for e in edits.iter().rev() {
            text.replace_range(
                (e.start - start) as usize..(e.end - start) as usize,
                &e.replacement,
            );
        }

        Fix::new(src, start, end, text)
    }
}

/// Whether a rewrite still covers the bytes the lint read.
///
/// A `.alx` file and a file an ingot rewrote reach the lints as another
/// text. A rewrite whose offsets come from that text points somewhere
/// else in the author's file, and writing it corrupts the file.
pub fn fix_applies(src: &str, fix: &Fix) -> bool {
    fix.edits()
        .all(|e| src.get(e.start as usize..e.end as usize) == Some(e.saw.as_str()))
}

/// Moves the lints of a pass's output back to the text the author
/// wrote. `map` is that pass's map and `src` is the author's text.
///
/// A rewrite keeps a range of its own, so it maps on its own. It
/// survives only when the source reads what the lint read; a rewrite
/// over text the pass generated has nothing to write back.
pub fn to_source(lints: &mut [Lint], src: &str, map: &crate::render::SpanMap) {
    for l in lints {
        l.start = map.to_source(l.start);
        l.end = map.to_source(l.end).max(l.start);

        if let Some(f) = &mut l.fix {
            let back = |e: &mut Fix| {
                e.start = map.to_source(e.start);
                e.end = map.to_source(e.end).max(e.start);
            };
            back(f);
            f.more.iter_mut().for_each(back);

            if !fix_applies(src, f) {
                l.fix = None;
            }
        }
    }
}

/// Applies the fixes of `lints` to `src`. Two fixes that overlap keep
/// the first, a fix lands with all its edits or none, and a fix that
/// breaks the parse does not land. See `sound`.
pub fn apply_fixes(src: &str, lints: &[Lint]) -> (String, usize) {
    let chosen = compatible(src, lints.iter().filter_map(|l| l.fix.as_ref()));
    let (kept, _) = sound(src, chosen);

    (apply(src, &kept), kept.len())
}

/// The text of `src` with the edits of `fixes`, last to first so the
/// offsets hold. The fixes must not overlap: see `compatible`.
pub fn apply(src: &str, fixes: &[&Fix]) -> String {
    let mut edits: Vec<&Fix> = fixes.iter().flat_map(|f| f.edits()).collect();
    edits.sort_by_key(|e| (e.start, e.end));
    let mut out = src.to_string();

    for e in edits.iter().rev() {
        out.replace_range(e.start as usize..e.end as usize, &e.replacement);
    }

    out
}

/// Splits `fixes` into the ones that keep `src` parsing and the ones
/// that break it. A lint that misreads its code can write text that
/// does not parse, as `if return f() then` once did. `--fix` and the
/// editor refuse that rewrite and keep the file. A sound set costs one
/// parse. A source that does not parse refuses nothing.
pub fn sound<'a>(src: &str, fixes: Vec<&'a Fix>) -> (Vec<&'a Fix>, Vec<&'a Fix>) {
    let parses = |fixes: &[&Fix]| alloy_syntax::parse_one(&apply(src, fixes)).is_ok();

    if fixes.is_empty() || parses(&fixes) || alloy_syntax::parse_one(src).is_err() {
        return (fixes, Vec::new());
    }

    let (kept, broken): (Vec<&Fix>, Vec<&Fix>) = fixes.iter().copied().partition(|f| parses(&[*f]));

    // Two fixes that parse alone can still break the parse together.
    if parses(&kept) {
        (kept, broken)
    } else {
        (Vec::new(), fixes)
    }
}

/// The fixes that land together, in source order: each still reads
/// what its lint read, and none overlaps one before it. The editor's
/// fix-all takes the same set as `--fix`.
pub fn compatible<'a>(src: &str, fixes: impl IntoIterator<Item = &'a Fix>) -> Vec<&'a Fix> {
    let mut fixes: Vec<&Fix> = fixes.into_iter().filter(|f| fix_applies(src, f)).collect();
    fixes.sort_by_key(|f| (f.start, f.end));
    let mut chosen: Vec<&Fix> = Vec::new();

    for f in fixes {
        // An edit clears one it follows, and one it inserts before.
        let clear = f.edits().all(|e| {
            chosen
                .iter()
                .flat_map(|c| c.edits())
                .all(|c| c.end <= e.start || (e.end <= c.start && e.start < c.start))
        });

        if clear {
            chosen.push(f);
        }
    }

    chosen
}

/// Swaps the lints of one file for the lints of its rewritten text.
///
/// `--fix` writes the file, so every lint the run collected before the
/// write is stale: a lint whose code the rewrite deleted still names a
/// line, and each offset behind the rewrite has moved. The caller reads
/// the file again, lints it, and hands the result here.
pub fn after_fix(
    remaining: &mut Vec<(std::path::PathBuf, Lint)>,
    rel: &std::path::Path,
    fresh: Vec<Lint>,
) {
    remaining.retain(|(r, _)| r.as_path() != rel);
    remaining.extend(fresh.into_iter().map(|l| (rel.to_path_buf(), l)));
}

/// What a lint does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Silent.
    Allow,
    /// Printed; the exit code stays zero.
    Warn,
    /// Printed; the exit code is one.
    Deny,
}

impl Level {
    /// The level a name spells, for `[lint.rules]` and `--@alloy-lint`.
    pub fn from_name(name: &str) -> Option<Level> {
        match name {
            "allow" => Some(Level::Allow),
            "warn" => Some(Level::Warn),
            "deny" => Some(Level::Deny),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Allow => "allow",
            Level::Warn => "warn",
            Level::Deny => "deny",
        }
    }
}

/// The group a lint belongs to, after clippy's: `[lint]` sets a level
/// for a whole group by its name, and `--list` sorts by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Code that is wrong, or cannot run.
    Correctness,
    /// Code that is probably not what the author meant.
    Suspicious,
    /// A Luau habit with an Alloy form.
    Style,
    /// Code that does a simple thing in a hard way.
    Complexity,
    /// Code that runs slower than the plain form.
    Perf,
    /// Roblox APIs that are deprecated or misused.
    Roblox,
    /// Strict rules, on while `[lint] strict = true`.
    Pedantic,
    /// The case of names, by `[lint.naming]`. It warns by default.
    Naming,
}

impl Group {
    pub const ALL: &[Group] = &[
        Group::Correctness,
        Group::Suspicious,
        Group::Style,
        Group::Complexity,
        Group::Perf,
        Group::Roblox,
        Group::Pedantic,
        Group::Naming,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Group::Correctness => "correctness",
            Group::Suspicious => "suspicious",
            Group::Style => "style",
            Group::Complexity => "complexity",
            Group::Perf => "perf",
            Group::Roblox => "roblox",
            Group::Pedantic => "pedantic",
            Group::Naming => "naming",
        }
    }

    pub fn from_name(name: &str) -> Option<Group> {
        Group::ALL.iter().copied().find(|g| g.name() == name)
    }

    pub fn summary(self) -> &'static str {
        match self {
            Group::Correctness => "code that is wrong, or cannot run",
            Group::Suspicious => "code that is probably not what the author meant",
            Group::Style => "a Luau habit with an Alloy form",
            Group::Complexity => "a simple thing done in a hard way",
            Group::Perf => "code that runs slower than the plain form",
            Group::Roblox => "a Roblox API that is deprecated or misused",
            Group::Pedantic => "strict rules, on while `[lint] strict = true`",
            Group::Naming => {
                "the case of names, by `[lint.naming]`; `[lint.rules] naming = \"allow\"` turns it off"
            }
        }
    }
}

/// The group of the type checker's own lints, `LocalUnused` and the
/// rest, which `alloy flux` reports beside these. `[lint.rules]` sets
/// their level by this name.
pub const LUAU_GROUP: &str = "luau";

/// The prefix of a markup lint in `[lint.rules]`:
/// `alx.static_conditional_child = "warn"`. The markup compiler owns
/// these, so a level here reaches it through `Config::markup`.
pub const ALX_PREFIX: &str = "alx.";

/// One markup lint. The list is short because the markup compiler
/// reports everything else as an error.
pub struct AlxLintInfo {
    /// The name without the `alx.` prefix.
    pub name: &'static str,
    pub default: Level,
    pub summary: &'static str,
}

/// Every markup lint, as `[lint.rules]` names them under `alx.`.
pub const ALX_LINTS: &[AlxLintInfo] = &[AlxLintInfo {
    name: "static_conditional_child",
    default: Level::Warn,
    summary: "markup in a child expression that no function encloses: it is built once, not on each render",
}];

/// The description of one lint, for `alloy doc lints` and `--list`.
pub struct LintInfo {
    pub name: &'static str,
    pub group: Group,
    /// The level the recommended set gives it. `Allow` marks a
    /// pedantic lint, which `strict = true` raises to `Warn`.
    pub default: Level,
    pub summary: &'static str,
    pub detail: &'static str,
}

/// The limits of the complexity lints, from `[flux]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thresholds {
    pub too_many_arguments: usize,
    pub too_many_lines: usize,
    pub max_nesting: usize,
    pub cognitive_complexity: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            too_many_arguments: 7,
            too_many_lines: 100,
            max_nesting: 5,
            cognitive_complexity: 25,
        }
    }
}

/// Every lint, by group, in the order the docs list them.
pub const LINTS: &[LintInfo] = &[
    // --- correctness -----------------------------------------------------------
    LintInfo {
        name: "optional_access",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a value that may be nil is indexed without a guard",
        detail: "A parameter typed `T?`, or the result of a function that returns `T?`, is indexed with `.` or called with `:` while nothing in the function checks it for nil. Guard it with `if x then`, `x and`, `assert(x)`, or use `?.` and `?:`, which stop the chain at nil.",
    },
    LintInfo {
        name: "needless_assert",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a `!` on a value the file types as never nil",
        detail: "`x!` throws when `x` is nil and hands on the type without the `?`. On a local, a `const`, or a parameter the file annotates with no `?`, neither half does anything: the check never fires and the type is already the one it would give. The `!` reads as a warning to whoever comes next, so drop it. `alloy flux --fix` removes it.",
    },
    LintInfo {
        name: "dropped_result",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a call of a `Result` function whose value nothing reads",
        detail: "A function that can fail answers with a `Result`, and the caller reads it with a `match`, an `if local Ok(v) = r`, or a method. A call statement that drops it loses the failure: the program goes on as if the call worked. Bind the value, or say the failure is expected by naming what to do with it.",
    },
    LintInfo {
        name: "static_call",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a static of an `impl` called with `:`",
        detail: "A function in an `impl` that does not take `self` is a static: `Wallet.new()`. Called with `:`, the colon passes the table as the first argument, which the static never asked for, and the values shift by one. `alloy flux --fix` writes the dot.",
    },
    LintInfo {
        name: "argument_count",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a call passes more arguments than the function takes",
        detail: "The extra values are evaluated and dropped, so a mistake in the argument order reads as working code. The lint counts a call of a function with a fixed parameter list that the file or an imported module declares: a plain name, `M.f` through `import * as M`, a static, a method on a value the file types with an annotation or a `new`, and a function in a table a local holds. A vararg, a default, or a name declared twice makes the count a range and the lint stands down. The checker reports the other direction, a call with too few arguments.",
    },
    LintInfo {
        name: "unreachable_default",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a `default` arm after arms that cover every variant",
        detail: "The arms of this `match` already cover every variant of the enum, so the `default` arm never runs. Delete it: with no `default`, the compiler reports a new variant as a missing arm instead of routing it here in silence.",
    },
    LintInfo {
        name: "empty_default",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a `default` arm with an empty body",
        detail: "An empty `default` swallows every variant the arms do not name, including the ones added later. Name the variants, or write the fallback the default stands for.",
    },
    LintInfo {
        name: "unused_import",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "an imported name the file never uses",
        detail: "The name an `import` binds appears nowhere after the import. The require still runs, so the module loads for nothing. Remove the name, or the import.",
    },
    LintInfo {
        name: "self_assignment",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "`x = x`, an assignment that changes nothing",
        detail: "The target and the value are the same name or the same path, so the statement does nothing. One side is a typo: the value was meant to come from somewhere else, or the target was meant to be a different field.",
    },
    LintInfo {
        name: "unreachable_code",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a statement after `return`, `break`, or `continue`",
        detail: "Nothing runs after `return`, `break`, or `continue` in the same block, so the statement is dead. Luau rejects most of these as syntax errors; Alloy reports the rest here. Delete the code, or move the jump.",
    },
    LintInfo {
        name: "constant_condition",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "`if true`, `if false`, or `if nil`",
        detail: "The condition is a literal, so one branch always runs and the other never does. A leftover from debugging, or a flag that should be a named constant. `while true do` is the idiom for a loop that breaks from inside, so it does not fire.",
    },
    LintInfo {
        name: "duplicate_key",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a table constructor that sets one key twice",
        detail: "The second `key = value` in a constructor overwrites the first, in silence. One of the two is a typo for another key, or the first value was meant to be gone.",
    },
    LintInfo {
        name: "misplaced_not",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "`not a == b`, which compares `not a` to `b`",
        detail: "`not` binds tighter than `==`, so `not a == b` is `(not a) == b`, a boolean compared to `b`. The test the author meant is `a ~= b`. `alloy flux --fix` rewrites it; `not a ~= b` becomes `a == b`.",
    },
    LintInfo {
        name: "identical_branches",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "an `if` whose `then` and `else` bodies are the same",
        detail: "Both branches hold the same statements, so the condition decides nothing. One branch was meant to differ, or the `if` is a leftover. The ternary form `c ? a : a` fires too.",
    },
    LintInfo {
        name: "private_access",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "a private field or method read outside its struct's impl",
        detail: "A member marked `private` belongs to the struct's own methods. This access sits outside every `impl` of that struct, in the same file; in the editor and under `alloy flux` the type checker reports it as an error, since the public type of the struct has no such member. The lint reads names, so a plain table with a field of the same name fires it too; `--@alloy-ignore` silences that line.",
    },
    LintInfo {
        name: "duplicate_function",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "one name given a `function` body twice",
        detail: "The second body replaces the first, so the first never runs. Either the two were meant to have different names, or one is a leftover from an edit. A `@cfg` pair is exempt: only one of the two reaches a build.",
    },
    LintInfo {
        name: "identity_compare",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "`==` or `~=` with a new payload variant or a new struct of a type that derives no `Eq`",
        detail: "A payload variant, `Item.Tool(\"a\", 1)`, and a `new` struct are a new table. `==` on two tables compares identity, so a value built in the comparison equals no other: `x == Item.Tool(\"a\", 1)` is always false, and `~=` is always true. `@derive(Eq)` on the type writes an `__eq` that compares the payload or the fields. A unit variant is a string and compares by its text, so it does not fire. The lint reads the derives of a type the file declares or imports from the project.",
    },
    LintInfo {
        name: "circular_import",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "two files that import each other",
        detail: "A cycle of `import` lines: Luau's `require` of a module that is still loading is an error at runtime, and the first file to load decides which one fails. Move the shared part into a third module that both import. `alloy flux` reports it; a single-file lint cannot see it.",
    },
    // --- suspicious ------------------------------------------------------------
    LintInfo {
        name: "single_angle_call",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a call that writes its type arguments in one `<...>`",
        detail: "A call takes its type arguments in `<<...>>`: `id<<number>>(5)`. Luau reads `id<number>(5)` as two comparisons, `(id < number) > 5`, and so does Alloy, because every valid Luau file compiles. Where the tokens read as no Luau, `Signal.new<string>()`, the compiler reports an error instead. The editor offers the `<<...>>` form as a quick fix; `alloy flux --fix` leaves the line, because the rewrite changes what the program does.",
    },
    LintInfo {
        name: "deprecated_namespace",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a use of a namespace declared `@deprecated`",
        detail: "`@deprecated` on a `function` passes through to Luau, which reports a call to it. A namespace has no Luau form, so this lint reports the use instead. The message the attribute carries prints after the name. Inside the namespace the members read each other by their own names, and nothing fires.",
    },
    LintInfo {
        name: "stale_export",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a write to an exported `local` inside a function",
        detail: "A module returns its exports as a table built when it loads, and an import copies each value into a local of the importer. `export local count = 0` sends out the number 0, not the variable. A write at the top level runs before the module returns, so the table takes it. A write inside a function runs later and never reaches an importer, which keeps the value it read when it loaded. Export a function that returns the value, or keep the value in a table: `export const state = { count = 0 }` and `state.count += 1`. An importer holds that table, so it reads every write. A `local` member of a namespace stays live through accessors and draws no report.",
    },
    LintInfo {
        name: "deprecated_call",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a `:` call of an impl method declared `@deprecated`",
        detail: "Flux. Luau reports `Box.value(b)` on a method marked `@deprecated`, but its lint does not follow `b:value()` through the metatable. This lint reports the method call when the file types the receiver as the struct: an annotation, `b: Box`, or the struct a `new Box { }` builds. The impl may sit in this file or in a module the file imports. A receiver of no known type stays quiet. The message the attribute carries prints after the name.",
    },
    LintInfo {
        name: "and_or_ternary",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "`c and a or b` in place of a ternary",
        detail: "Flux. The `and ... or` idiom yields `b` when `a` is false or nil, whatever `c` was; that is the classic Lua trap. `c ? a : b` picks by `c` alone. When `a` is a literal that is never false, the two are the same and `alloy flux --fix` rewrites it; otherwise the lint shows the ternary and leaves the change to the author.",
    },
    LintInfo {
        name: "unused_variable",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a local that nothing reads",
        detail: "A `local` or a loop variable that appears nowhere after its declaration. A leftover, or a typo in the name that reads it. Prefix it with `_` to say it is unused on purpose; `alloy flux --fix` does that. The type checker reports the scoped cases this lint cannot see, and `unused_function` covers a function.",
    },
    LintInfo {
        name: "unused_function",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a function that nothing calls",
        detail: "A `function`, a `local function`, an `async function`, or a local or const bound to a function value, whose name appears nowhere else in the file. An exported function, and a method of an `impl` or a `trait`, are for other files and do not fire. A function with an attribute above it does not fire either: `@test` and a declared attribute mark a function the runtime reaches by its attribute, and `@deprecated`, `@cfg`, and Luau's `@[...]` list keep that rule. Prefix the name with `_` to keep it on purpose; `alloy flux --fix` does that.",
    },
    LintInfo {
        name: "empty_block",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "an `if`, `else`, or loop body with nothing in it",
        detail: "The block runs nothing. An `if` with an empty body was meant to hold something, or its condition was meant to be inverted; an empty `else` is a leftover. A block that holds only a comment does not fire: the comment says why it is empty.",
    },
    LintInfo {
        name: "bool_comparison",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "`x == true` or `x == false`",
        detail: "For a boolean `x`, `x == true` is `x` and `x == false` is `not x`. For any other value the comparison is always false, which is rarely the intent. No automatic rewrite: the checker knows the type, the lint does not.",
    },
    LintInfo {
        name: "needless_bool",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "`if c then return true else return false end`",
        detail: "The `if` converts a condition to a boolean by hand. `return c` is the statement when `c` is a comparison, and `alloy flux --fix` rewrites that case; for a plain value, `return c == true` or `return not not c` keeps the boolean type.",
    },
    // --- style -----------------------------------------------------------------
    LintInfo {
        name: "prefer_destroy",
        group: Group::Style,
        default: Level::Allow,
        summary: "`delete part` on a plain Instance, where `destroy part` says it",
        detail: "Flux. `delete` covers every kind of cleanup: a connection, a thread, a scope, a signal, an Instance. On a plain Instance it runs the Destroy and nothing else, so the word promises more than the code does. `destroy part` names the one thing that happens, and `destroy part after n` puts it on a timer. The lint fires when the file shows the operand is an Instance and no scope holds anything for it. Off by default, since `delete` on an Instance is right: `[lint.rules] prefer_destroy = \"warn\"` turns it on, and `alloy flux --fix` rewrites the word.",
    },
    LintInfo {
        name: "manual_safe_access",
        group: Group::Style,
        default: Level::Warn,
        summary: "`a and a.b`, the guard written by hand",
        detail: "Flux. `a and a.b` reads `a` twice to guard one index. `a?.b` is the guard: it stops at nil and yields nil. With `or` after it, `a?.b ?? x` is the same when `b` is never false; the lint leaves that rewrite to the author. `alloy flux --fix` rewrites the plain form.",
    },
    LintInfo {
        name: "manual_coalesce",
        group: Group::Style,
        default: Level::Warn,
        summary: "`if x == nil then x = v end`, a coalescing assignment by hand",
        detail: "Flux. The three-line nil check assigns when `x` is nil and nothing else. `x ??= v` is that statement, and it reads `x` once. `alloy flux --fix` rewrites it.",
    },
    LintInfo {
        name: "nil_check_call",
        group: Group::Style,
        default: Level::Warn,
        summary: "`if f then f(...) end`, an optional call by hand",
        detail: "Flux. A block that tests a function and calls it is `f?(...)`: the call runs when `f` is set and yields nil when it is not. `alloy flux --fix` rewrites it.",
    },
    LintInfo {
        name: "manual_type_test",
        group: Group::Style,
        default: Level::Warn,
        summary: "`typeof(x) == \"T\"` in place of `x is T`",
        detail: "Flux. `x is T` compiles to the right test for the name, `type`, `typeof`, or `IsA`, and the checker narrows `x` in the branch. A string comparison narrows nothing. `alloy flux --fix` rewrites primitives, `Instance`, and the Roblox datatypes.",
    },
    LintInfo {
        name: "array_long_string",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a long string whose text reads like a nested array",
        detail: "`[[1, 2], [3, 4]]` is Luau's long string, the text `1, 2], [3, 4`. Alloy once read it as a nested array; a nested array now puts a space between the brackets, `[ [1, 2], [3, 4] ]`. `alloy flux --fix` writes that form.",
    },
    LintInfo {
        name: "pattern_shadows_local",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a match pattern that binds the name of a local in scope",
        detail: "`case target then`, with `target` a local or a parameter, reads as a comparison and is a binding: the arm takes every value, and a `default` or a later arm never runs. Compare in a guard, `case v where v == target`, or bind a name no local holds. A const or a SCREAMING_CASE name is an error for the same reason.",
    },
    LintInfo {
        name: "prefer_const",
        group: Group::Style,
        default: Level::Warn,
        summary: "a `local` that nothing assigns again",
        detail: "`local x = v` with no later `x = ...` holds one value, and `const x = v` says so: a write added later is a compile error instead of a quiet change. A local whose value the file writes into, with `t.x = 1` or with a call such as `t:push(v)` in any expression, stays `local`, so `prefer_const` and `const_mutation` never disagree. `alloy flux --fix` writes `const`, and so does `alloy fmt` unless `[fmt] prefer_const = false`.",
    },
    LintInfo {
        name: "redundant_as",
        group: Group::Style,
        default: Level::Warn,
        summary: "`as` before a declaration body on the next line",
        detail: "`as` joins a header to a body on the same line, `enum Dir as Up, Down end`. A body on the next line needs none, as Luau's block headers read, so `struct P as` over its fields writes `struct P`. `alloy fmt` and `alloy flux --fix` drop the word.",
    },
    LintInfo {
        name: "match_guard_and",
        group: Group::Style,
        default: Level::Warn,
        summary: "a match guard written with `and`",
        detail: "`case n and n > 5` reads as one condition joined to the pattern. `case n where n > 5` is the guard, the word a `for` filter takes too. `alloy flux --fix` swaps the word.",
    },
    LintInfo {
        name: "legacy_iterator",
        group: Group::Style,
        default: Level::Warn,
        summary: "`pairs` or `ipairs` around a `for ... in` table",
        detail: "Flux. Luau iterates a table without a wrapper, arrays in order and then the rest, and honors `__iter`. `pairs` and `ipairs` add a call and hide the metamethod. `alloy flux --fix` removes `pairs`. It leaves `ipairs`, which stops at the first nil and skips the hash part, so the rewrite is the author's choice.",
    },
    LintInfo {
        name: "manual_floor_div",
        group: Group::Style,
        default: Level::Warn,
        summary: "`math.floor(a / b)` in place of `a // b`",
        detail: "Flux. Floor division is an operator: `a // b`. The lint fires when the argument is one division with no other operator around it, so the rewrite is the same value. `alloy flux --fix` rewrites it, in parentheses where a neighbour binds tighter.",
    },
    LintInfo {
        name: "manual_push",
        group: Group::Style,
        default: Level::Warn,
        summary: "`table.insert` or `table.remove` on a value typed as an Array",
        detail: "Flux. A value declared `T[]`, `Array<T>`, or with an array literal carries methods: `xs:push(v)` and `xs:pop()`. The `table` functions work on it too, but the method names the intent and keeps the type. `alloy flux --fix` rewrites the two-argument insert and the one-argument remove.",
    },
    LintInfo {
        name: "concat_interpolation",
        group: Group::Style,
        default: Level::Warn,
        summary: "a `..` chain that joins literals and values",
        detail: "Flux. A chain such as `\"Hello \" .. name .. \"!\"` is one interpolated string: `` `Hello {name}!` ``. The backtick form calls `tostring` on each hole, so a `tostring(x)` in the chain becomes `{x}`. The lint skips a chain whose literals hold a backtick, a brace, or an escape. `alloy flux --fix` rewrites the rest.",
    },
    LintInfo {
        name: "raw_pcall",
        group: Group::Style,
        default: Level::Warn,
        summary: "a `pcall` or `xpcall`",
        detail: "Flux. `pcall` yields a flag and a value the caller has to test by hand, and the error loses its traceback. `Result.pcall(f, ...)` yields a `Result` with the traceback on the `Err`, and `try` unwraps it or returns it. No automatic rewrite: the surrounding code changes with it.",
    },
    LintInfo {
        name: "raw_require",
        group: Group::Style,
        default: Level::Warn,
        summary: "a `require` where an `import` would do",
        detail: "Flux. `import` resolves the path at build time, binds only the names the file uses, and carries the types; the checker follows it and `unused_import` watches it. `require` binds the whole module at runtime. `alloy flux --fix` rewrites `local X = require(\"./x\")` to `import X from \"./x\"`; an instance path stays for the author.",
    },
    LintInfo {
        name: "game_alias",
        group: Group::Style,
        default: Level::Warn,
        summary: "`\"game\"` or `\"game:X\"`, the service path before the alias",
        detail: "The Roblox services sit under the `@game` alias: `import { Players } from \"@game\"` takes a list, and `import Players from \"@game/Players\"` takes one. `\"game\"` and `\"game:Players\"` were the spellings before, and they still compile; this release is the last that reads them. `alloy flux --fix` rewrites the path, and the formatter leaves both as written.",
    },
    LintInfo {
        name: "manual_class",
        group: Group::Style,
        default: Level::Warn,
        summary: "`X.__index = X`, the class idiom by hand",
        detail: "Flux. The metatable idiom writes the constructor, the `__index`, and the method table by hand, and the checker sees plain tables. `struct X as ... end` with `impl X` emits the same tables with types, `new X(...)` for construction, and traits for shared behaviour. No automatic rewrite.",
    },
    LintInfo {
        name: "manual_ternary_return",
        group: Group::Style,
        default: Level::Warn,
        summary: "`if c then return a else return b end`",
        detail: "Flux. Two returns that differ only in the value are one: `return c ? a : b`. The ternary picks by `c` alone, so the rewrite is the same program. `alloy flux --fix` rewrites it.",
    },
    LintInfo {
        name: "redundant_return",
        group: Group::Style,
        default: Level::Warn,
        summary: "a bare `return` at the end of a function",
        detail: "A `return` with no value right before the function's `end` does what falling off the end does. Delete it. `alloy flux --fix` removes it.",
    },
    LintInfo {
        name: "local_then_return",
        group: Group::Style,
        default: Level::Warn,
        summary: "`local x = v` followed by `return x`",
        detail: "The local is read once, on the next line, by the `return`. `return v` says the same in one statement; a call goes in parentheses, `return (f())`, so the return keeps one value as the local did. A `const x = v` reads the same. `alloy flux --fix` rewrites it.",
    },
    LintInfo {
        name: "numeric_for_index",
        group: Group::Style,
        default: Level::Warn,
        summary: "`for i = 1, #t do local v = t[i]`",
        detail: "Flux. The numeric loop indexes the table by hand on its first line. `for i, v in t do` binds both, in order, and reads as what it is. `alloy flux --fix` rewrites the header and drops the index line.",
    },
    LintInfo {
        name: "missing_reason",
        group: Group::Style,
        default: Level::Warn,
        summary: "an `--@alloy-expect-error` with no reason",
        detail: "The directive says a line must hold an error. Write why after the name, `--@alloy-expect-error the contract rejects a negative count`, and the reason comes back in the message when the line goes clean, so a stale directive is easy to place. `--@alloy-ignore` takes a reason the same way and never draws this lint. No automatic rewrite: only the author knows the reason.",
    },
    // --- complexity ------------------------------------------------------------
    LintInfo {
        name: "too_many_arguments",
        group: Group::Complexity,
        default: Level::Warn,
        summary: "a function with more parameters than `[flux] too_many_arguments`",
        detail: "A long parameter list is hard to call in the right order. Group the parameters into a struct or a table, or split the function. The limit is `too_many_arguments` in `[flux]`, seven by default; `self` does not count.",
    },
    LintInfo {
        name: "too_many_lines",
        group: Group::Complexity,
        default: Level::Warn,
        summary: "a function longer than `[flux] too_many_lines`",
        detail: "A function this long does several things. Name the parts and call them. The limit is `too_many_lines` in `[flux]`, one hundred by default, counted between the header and the `end`.",
    },
    LintInfo {
        name: "deep_nesting",
        group: Group::Complexity,
        default: Level::Warn,
        summary: "blocks nested deeper than `[flux] max_nesting`",
        detail: "Each `if`, loop, `match`, and function inside another adds a level the reader has to hold. Return early, invert the condition, or move the inner block into its own function. The limit is `max_nesting` in `[flux]`, five by default.",
    },
    LintInfo {
        name: "cognitive_complexity",
        group: Group::Complexity,
        default: Level::Warn,
        summary: "a function whose branches score past `[flux] cognitive_complexity`",
        detail: "Every `if`, `elseif`, `else`, loop, `match`, ternary, `and`, and `or` adds one, and a branch inside another adds its depth on top. A score past the limit means the function is hard to follow; split it where the deepest branches begin. The limit is `cognitive_complexity` in `[flux]`, twenty-five by default.",
    },
    LintInfo {
        name: "collapsible_if",
        group: Group::Complexity,
        default: Level::Warn,
        summary: "`if a then if b then ... end end`",
        detail: "An `if` whose only statement is another `if`, with no `else` on either, is one `if a and b then`. `alloy flux --fix` rewrites it; `alloy fmt` then fixes the indent of the body.",
    },
    LintInfo {
        name: "collapsible_else_if",
        group: Group::Complexity,
        default: Level::Warn,
        summary: "`else if ... end end` in place of `elseif`",
        detail: "An `else` whose only statement is an `if` is an `elseif`, one `end` shorter and one level shallower. `alloy flux --fix` rewrites it; `alloy fmt` then fixes the indent.",
    },
    // --- perf ------------------------------------------------------------------
    LintInfo {
        name: "concat_in_loop",
        group: Group::Perf,
        default: Level::Warn,
        summary: "`s = s .. x` inside a loop",
        detail: "Each `..` copies the whole string so far, so the loop is quadratic in the output. Push the pieces into a table and `table.concat` it once after the loop.",
    },
    LintInfo {
        name: "service_in_loop",
        group: Group::Perf,
        default: Level::Warn,
        summary: "`game:GetService` inside a loop",
        detail: "The service never changes, so the lookup repeats for nothing. Bind it once above the loop, or at the top of the file, as the Roblox style guide does.",
    },
    LintInfo {
        name: "table_insert_position",
        group: Group::Perf,
        default: Level::Warn,
        summary: "`table.insert(t, #t + 1, v)`",
        detail: "The three-argument insert shifts elements, and computes a length that the two-argument form computes itself. `table.insert(t, v)` appends. `alloy flux --fix` rewrites it.",
    },
    // --- roblox ----------------------------------------------------------------
    LintInfo {
        name: "deprecated_global",
        group: Group::Roblox,
        default: Level::Warn,
        summary: "a call to `wait`, `spawn`, `delay`, or `unpack`",
        detail: "The legacy scheduler globals run on the 30 Hz legacy pipeline and their timing drifts. `task.wait`, `task.spawn`, `task.delay`, and `task.defer` are the replacements; `unpack` is `table.unpack`. `alloy flux --fix` rewrites them.",
    },
    LintInfo {
        name: "manual_child_lookup",
        group: Group::Roblox,
        default: Level::Warn,
        summary: "`:FindFirstChild(\"X\")` or `:WaitForChild(\"X\")` with a literal name",
        detail: "Flux. `parent->X` is `FindFirstChild(\"X\")` and `parent=>X` is `WaitForChild(\"X\")`, typed from the sourcemap and shorter to read. A call with a second argument stays as it is. `alloy flux --fix` rewrites the one-argument form.",
    },
    LintInfo {
        name: "deprecated_method",
        group: Group::Roblox,
        default: Level::Warn,
        summary: "a lowercase Roblox method: `:connect`, `:wait`, `:remove`, `:clone`",
        detail: "The lowercase members are the pre-2014 names, kept for old places and gone from the docs. `Connect`, `Wait`, `Destroy`, `Clone`, `GetChildren`, `FindFirstChild`, and `IsA` are the current ones, and the checker knows only those. A method of the same name that the file declares does not fire, and `:remove` and `:clone` fire only on a call with no arguments, since a `HashMap` has a `remove` of its own. The std spells `connect`, `disconnect`, `wait`, and `clone` the same way, so those four ask for a Roblox receiver: an event such as `.Touched` or `:GetPropertyChangedSignal(...)`, an instance such as `workspace.Ball` or `script`, or a name the file annotates with a Roblox class. Over a `Signal`, a `SignalConnection`, a `T: Clone`, or a plain local, nothing fires. `alloy flux --fix` rewrites them.",
    },
    LintInfo {
        name: "instance_new_parent",
        group: Group::Roblox,
        default: Level::Warn,
        summary: "`Instance.new(class, parent)`, the parent as an argument",
        detail: "With the parent set first, every property written after it replicates and fires a change on its own. Create the instance, set its properties, then set `Parent` last. No automatic rewrite: the assignments move.",
    },
    LintInfo {
        name: "deprecated_body_mover",
        group: Group::Roblox,
        default: Level::Warn,
        summary: "`BodyVelocity`, `BodyPosition`, `BodyGyro`, and the other body movers",
        detail: "The body movers are deprecated. `LinearVelocity` replaces `BodyVelocity`, `AlignPosition` replaces `BodyPosition`, `AlignOrientation` replaces `BodyGyro`, `VectorForce` replaces `BodyForce` and `BodyThrust`, `AngularVelocity` replaces `BodyAngularVelocity`, and `LineForce` with `AlignOrientation` replaces `RocketPropulsion`. Each needs an `Attachment`; no automatic rewrite.",
    },
    // --- pedantic --------------------------------------------------------------
    LintInfo {
        name: "explicit_any",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "an annotation of `any`",
        detail: "Pedantic. Flux. `any` turns the checker off for the value and everything read from it. `unknown` keeps the checker on, and `x is T` narrows it where the code needs a shape. The lint skips the `any_cast` the compiler writes.",
    },
    LintInfo {
        name: "implicit_any",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "a named function parameter, or an empty array literal, with no type",
        detail: "Pedantic. A parameter of a named function with no annotation is `any` to the checker, and every use of it goes unchecked. Write the type. A callback passed as an argument is exempt: the checker infers its parameters from the callee.\n\nA binding of an empty `[ ]` is the same case: `local xs = []` names no element type, so the checker reads `any[]`. Write `local xs: T[] = []`. A position that carries the type stays silent: an annotated binding, an argument of a typed parameter, a `return` of a declared return type, and a struct field default.",
    },
    LintInfo {
        name: "missing_return_type",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "a public function with no return type",
        detail: "Pedantic. An exported function, or a method in an `impl`, is an interface others call; without a return annotation, a change to its body changes its type in silence. Write the return type.",
    },
    LintInfo {
        name: "todo_comment",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "a `TODO`, `FIXME`, `XXX`, or `HACK` comment",
        detail: "Pedantic. The comment marks work that is not done. The lint lists them so a release can hold until they are, or until they become tickets.",
    },
    LintInfo {
        name: "print_debug",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "a `print` call",
        detail: "Pedantic, and `strict` leaves it alone: `print` is ordinary in Luau, so a project that wants it gone writes `[lint.rules] print_debug = \"warn\"`. A `print` left over from debugging writes to the output of every player. Remove it, or route it through a logger the project can turn off.",
    },
    LintInfo {
        name: "const_mutation",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "a write into the value a `const` holds",
        detail: "Pedantic. `const` freezes the binding, not the value: `const LIMITS = { hp = 100 }` still allows `LIMITS.hp = 1`, and `NAMES:push(x)` still grows the array. The lint reports a field assignment, an index assignment, and a call of a method that changes the value, so a name written as a constant reads as one.",
    },
    LintInfo {
        name: "missing_doc",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "an exported declaration with no comment above it",
        detail: "Pedantic. An `export` is the interface of the module. A comment line right above it, `--` or `---`, says what it is for; the language server shows it on hover. An exported `type` alias and an exported `interface` answer to `missing_doc_type` instead, which is off.",
    },
    LintInfo {
        name: "missing_doc_type",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "an exported `type` alias or `interface` with no comment above it",
        detail: "Pedantic, and `strict` leaves it alone: a type alias and an interface say what they are in their own shape, while a function or a const does not, so `missing_doc` exempts the two and this lint covers them. A project that documents every export as well writes `[lint.rules] missing_doc_type = \"warn\"`. A struct, an enum, a function, a const, and a remote stay with `missing_doc`.",
    },
    // --- naming ----------------------------------------------------------------
    LintInfo {
        name: "naming_convention",
        group: Group::Naming,
        default: Level::Warn,
        summary: "a name in a case other than the one `[lint.naming]` sets for its kind",
        detail: "Naming. `[lint.naming]` gives each kind of name a case style, or a list of styles, and a name passes when it fits one of them. The defaults follow Rust. A local, a function, a method, a parameter, a field, an attribute, and a macro take snake_case. A struct, an enum, a variant, a trait, an interface, a type, a namespace, and a remote take PascalCase. A `const` takes snake_case or SCREAMING_SNAKE_CASE, because Alloy marks any binding that the file never assigns again as `const`. A `local` that nothing assigns again takes the const style too, while `prefer_const` or `[fmt] prefer_const` is on, since either one makes it a `const`.\n\nA name that starts with `_` does not fire. A local bound to `require(...)` or `:GetService(...)` takes the case of its module or service, so `local Players` does not fire. In an `.alx` file a function that returns markup, or that a tag names, is a component and takes the `component` style, PascalCase by default. A method in the `impl` of a trait takes its name from the trait.\n\n`alloy flux --fix` and `alloy fmt` rename the name to the first style of its list, at each place the file reads it. The rename stays in the scope of a local or a parameter, and skips a scope that shadows it. It writes nothing when another file or saved data reads the name, so an export, a field, a variant, a method, a remote, an attribute, a macro, and a namespace member keep the lint alone. So does a declaration under an attribute other than `@allow`, such as `@test`, because the runtime may read its name. It also writes nothing when the new name is already in the file, or is a keyword or a Luau global. `[fmt] fix_naming = false` stops the renames of `alloy fmt`.\n\nThe lint answers to its old names, `camel_case_name`, `type_case`, and `pascal_case_function`, and to rustc's `non_snake_case`, `non_camel_case_types`, and `non_upper_case_globals`. So `@allow(non_snake_case)` quiets it. `alloy doc naming-conventions` lists the keys of `[lint.naming]`.",
    },
];

/// The three naming lints that `naming_convention` replaced. An old
/// name still sets the level of the new lint.
pub const OLD_NAMING: &[&str] = &["camel_case_name", "type_case", "pascal_case_function"];

/// The lint a name stands for. A rustc or Clippy name reads as the
/// Alloy lint, so `@allow(dead_code)` reads the way a Rust developer
/// writes it. An old lint name reads as the lint that replaced it.
pub fn canonical_name(name: &str) -> &str {
    match name {
        "unused_variables" => "unused_variable",

        "unused_imports" => "unused_import",

        "dead_code" => "unused_function",

        "needless_return" => "redundant_return",

        "let_and_return" => "local_then_return",

        "print_stdout" | "dbg_macro" => "print_debug",

        "todo" => "todo_comment",

        "non_snake_case" | "non_camel_case_types" | "non_upper_case_globals" => crate::naming::LINT,

        old if OLD_NAMING.contains(&old) => crate::naming::LINT,

        other => other,
    }
}

/// A lint an ingot declares, registered when the ingot loads. Its name
/// is `<ingot>/<lint>` and its group is the ingot's name, so `[lint]`
/// sets a level for one lint or for the whole ingot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalLint {
    pub name: &'static str,
    pub group: &'static str,
    pub default: Level,
    pub summary: String,
    pub detail: String,
}

static EXTERNAL: std::sync::OnceLock<std::sync::Mutex<Vec<ExternalLint>>> =
    std::sync::OnceLock::new();

/// Registers the lints of one ingot, replacing an earlier registration
/// of the same group. A loaded ingot calls this once.
pub fn register_external(group: &str, lints: Vec<ExternalLint>) {
    let mut all = EXTERNAL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    all.retain(|l| l.group != group);
    all.extend(lints);
    all.sort_by_key(|l| l.name);
}

/// Every registered ingot lint.
pub fn external() -> Vec<ExternalLint> {
    EXTERNAL
        .get_or_init(Default::default)
        .lock()
        .map(|l| l.clone())
        .unwrap_or_default()
}

/// A lint name leaked once, so an ingot's lint carries a `&'static str`
/// like the built-in ones. The set is bounded by the manifests loaded.
pub fn intern(name: &str) -> &'static str {
    static NAMES: std::sync::OnceLock<std::sync::Mutex<HashSet<&'static str>>> =
        std::sync::OnceLock::new();
    let mut set = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    if let Some(n) = set.get(name) {
        return n;
    }

    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    set.insert(leaked);

    leaked
}

/// The level `[lint]` sets for one key: `[lint.rules]` first, then the
/// deprecated `deny`, `warn`, and `allow` lists.
pub fn listed(config: &LintConfig, key: &str) -> Option<Level> {
    // The lint's own name wins over a name that stands for it.
    let alias = || {
        config
            .rules
            .iter()
            .find(|(k, _)| canonical_name(k) == key)
            .map(|(_, l)| l)
    };

    if let Some(level) = config.rules.get(key).or_else(alias) {
        return Some(*level);
    }

    let names = |list: &[String]| list.iter().any(|n| canonical_name(n) == key);

    if names(&config.deny) {
        Some(Level::Deny)
    } else if names(&config.warn) {
        Some(Level::Warn)
    } else if names(&config.allow) {
        Some(Level::Allow)
    } else {
        None
    }
}

/// The level a lint runs at under a config: its own name in
/// `[lint.rules]` first, then its group's name, then the level the
/// modes give it. A name the table lacks is a lint of the type
/// checker, under the `luau` group. A name with a `/` is an ingot's,
/// under the ingot's name.
///
/// The modes are two: `strict` raises the pedantic group to `warn`,
/// and `recommended` decides the floor, which is each lint's own
/// default when it is on and `allow` when it is off.
pub fn level_of(config: &LintConfig, name: &str) -> Level {
    if let Some(markup) = name.strip_prefix(ALX_PREFIX) {
        return alx_level_of(config, markup);
    }

    if let Some((ingot, _)) = name.split_once('/') {
        let ext = external().into_iter().find(|l| l.name == name);

        return listed(config, name)
            .or_else(|| listed(config, ingot))
            .unwrap_or_else(|| match config.recommended {
                true => ext.map(|l| l.default).unwrap_or(Level::Warn),
                false => Level::Allow,
            });
    }

    let name = canonical_name(name);
    let info = LINTS.iter().find(|l| l.name == name);
    let group = info.map(|l| l.group.name()).unwrap_or(LUAU_GROUP);

    if let Some(level) = listed(config, name).or_else(|| listed(config, group)) {
        return level;
    }

    match info {
        // `print` is ordinary in Luau, and a type alias says what it is
        // in its own shape: a project that wants either reported says so
        // by name. Strict carries the rest of the group.
        Some(l) if matches!(l.name, "print_debug" | "missing_doc_type") => l.default,
        Some(l) if l.group == Group::Pedantic && config.strict => Level::Warn,
        _ if !config.recommended => Level::Allow,
        Some(l) => l.default,
        None => Level::Warn,
    }
}

/// The level of one markup lint, named without the `alx.` prefix.
/// `Config::markup` passes it to the markup compiler.
pub fn alx_level_of(config: &LintConfig, name: &str) -> Level {
    let info = ALX_LINTS.iter().find(|l| l.name == name);

    listed(config, &format!("{ALX_PREFIX}{name}")).unwrap_or(match config.recommended {
        true => info.map(|l| l.default).unwrap_or(Level::Warn),
        false => Level::Allow,
    })
}

/// The lints of luau-lsp, `luau.LocalShadow` under `@allow`: Luau's
/// `kWarningNames` in `LinterConfig.h`, less its `Unknown`.
pub const LUAU_LINTS: &[&str] = &[
    "UnknownGlobal",
    "DeprecatedGlobal",
    "GlobalUsedAsLocal",
    "LocalShadow",
    "SameLineStatement",
    "MultiLineStatement",
    "LocalUnused",
    "FunctionUnused",
    "ImportUnused",
    "BuiltinGlobalWrite",
    "PlaceholderRead",
    "UnreachableCode",
    "UnknownType",
    "ForRange",
    "UnbalancedAssignment",
    "ImplicitReturn",
    "DuplicateLocal",
    "FormatString",
    "TableLiteral",
    "UninitializedLocal",
    "DuplicateFunction",
    "DeprecatedApi",
    "TableOperations",
    "DuplicateCondition",
    "MisleadingAndOr",
    "CommentDirective",
    "IntegerParsing",
    "ComparisonPrecedence",
    "RedundantNativeAttribute",
];

/// The group of a lint by name; the type checker's lints are `luau`,
/// and an ingot's lints are the ingot's name.
pub fn group_name(name: &str) -> &'static str {
    if name.starts_with(ALX_PREFIX) {
        return "alx";
    }

    if let Some((ingot, _)) = name.split_once('/') {
        return intern(ingot);
    }

    LINTS
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.group.name())
        .unwrap_or(LUAU_GROUP)
}

/// The level a lint runs at in one file: the `--@alloy-lint` directives
/// of that file first, then the `[lint]` table of alloy.toml. A file
/// says the last word about its own lints.
pub fn level_in(
    config: &LintConfig,
    directives: &crate::directives::Directives,
    name: &str,
) -> Level {
    directives
        .level_override(name)
        .unwrap_or_else(|| level_of(config, name))
}

/// Whether a name is a lint, a group, an ingot's lint, or one of the
/// type checker's. `[lint]` and `--@alloy-lint` accept the same names.
/// A checker lint has no list here, so a capitalised name passes, the
/// way `level_of` reads one under the `luau` group.
pub fn is_known_name(name: &str) -> bool {
    let name = canonical_name(name);
    let checker_lint = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric());

    if let Some(markup) = name.strip_prefix(ALX_PREFIX) {
        return ALX_LINTS.iter().any(|l| l.name == markup);
    }

    !name.is_empty()
        && (LINTS.iter().any(|l| l.name == name)
            || Group::from_name(name).is_some()
            || name == LUAU_GROUP
            || checker_lint
            || external().iter().any(|l| l.name == name || l.group == name))
}

/// A `[lint]` name that is neither a lint nor a group. The deprecated
/// lists and `[lint.rules]` name the same things, so both are read.
pub fn unknown_names(config: &LintConfig) -> Vec<String> {
    config
        .allow
        .iter()
        .chain(&config.warn)
        .chain(&config.deny)
        .chain(config.rules.keys())
        .filter(|n| !is_known_name(n))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lints of the recommended set, with the pedantic group off,
    /// which is what `strict = false` leaves.
    fn names(src: &str) -> Vec<&'static str> {
        let out = crate::compile(src).unwrap();
        let config = LintConfig::default().without_strict();

        out.lints
            .iter()
            .map(|l| l.name)
            .filter(|n| {
                level_of(&config, n) != Level::Allow
                    && !matches!(
                        *n,
                        "unused_variable" | "unused_function" | "redundant_as" | "prefer_const"
                    )
            })
            .collect()
    }

    /// `--fix` deletes the code a lint named, so the lints of the old
    /// text cannot be printed: the rewritten file answers instead.
    #[test]
    fn the_lints_of_a_fixed_file_come_from_its_new_text() {
        let src = "function bad_bool(): boolean\n    if true then\n        return true\n    else\n        return false\n    end\nend\n";
        let before = crate::compile(src).unwrap().lints;
        assert!(before.iter().any(|l| l.name == "constant_condition"));
        let (after, _) = apply_fixes(src, &before);
        let rel = std::path::PathBuf::from("src/x.aly");
        let mut remaining: Vec<(std::path::PathBuf, Lint)> = before
            .iter()
            .filter(|l| l.fix.is_none())
            .map(|l| (rel.clone(), l.clone()))
            .collect();
        assert!(!remaining.is_empty());
        after_fix(&mut remaining, &rel, crate::compile(&after).unwrap().lints);
        assert!(
            !remaining
                .iter()
                .any(|(_, l)| l.name == "constant_condition"),
            "{remaining:?}"
        );
    }

    /// An import copies the value an exported `local` held when the
    /// module loaded. A write inside a function comes later and never
    /// reaches the importer; a write at the top level does.
    #[test]
    fn a_write_to_an_exported_local_in_a_function_is_stale() {
        let stale = |body: &str| {
            let src = format!(
                "export local count = 0\nexport local other = 0\nlocal hits = 0\nexport {{ hits }}\n\nexport function bump(): ()\n{body}\nend\n"
            );
            names(&src)
                .into_iter()
                .filter(|n| *n == "stale_export")
                .count()
        };

        assert_eq!(stale("    count += 1"), 1);
        assert_eq!(stale("    count = 5"), 1);
        assert_eq!(stale("    count ??= 1"), 1);
        assert_eq!(stale("    other, count = 1, 2"), 2);
        assert_eq!(stale("    hits += 1"), 1);
        assert_eq!(
            stale("    task.defer(function()\n        count += 1\n    end)"),
            1
        );

        // A nearer binding of the name, or a field of the value, is not
        // the export.
        assert_eq!(stale("    local count = 1\n    count += 1"), 0);
        assert_eq!(
            stale("    for count = 1, 2 do\n        count += 1\n    end"),
            0
        );

        // The top level runs before the module returns its table.
        let top = "export local count = 0\ncount += 1\nif count > 0 then\n    count = 2\nend\nexport const state = { count = 0 }\n\nexport function bump(): ()\n    state.count += 1\nend\n";
        assert!(!names(top).contains(&"stale_export"), "{:?}", names(top));

        let got = crate::compile(
            "export local count = 0\n\nexport function bump(): ()\n    count += 1\nend\n",
        )
        .unwrap()
        .lints;
        let one = got
            .iter()
            .find(|l| l.name == "stale_export")
            .expect("the lint");
        assert_eq!(
            one.message,
            "`count` is an exported `local`; importers keep the value they read when they loaded, so they never see this write. Export a function that returns it, or hold it in a table, such as `export const state = { count = ... }`"
        );
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

    /// A guard word before the name never covers an access through it:
    /// `return t` passes the optional on, `return t.x` reads through it.
    /// A static called with `:` gets the table as its first argument.
    #[test]
    fn a_static_called_with_a_colon_fires() {
        let src = "struct W as\n    n: number\nend\n\nimpl W as\n    function new(): W\n        return new W { n = 0 }\n    end\n\n    function bump(self): number\n        return self.n\n    end\nend\n\nlocal a = W:new()\nlocal b = a:bump()\nprint(a, b)\n";
        assert_eq!(names(src), vec!["static_call"]);
        assert!(
            apply_fixes(src, &crate::compile(src).unwrap().lints)
                .0
                .contains("W.new()")
        );
    }

    /// Luau's solver reports a call with too few arguments and misses
    /// one with too many, so the extra values are dropped in silence.
    #[test]
    fn a_call_with_too_many_arguments_fires() {
        let src = "local function heal(who: string, amount: number): number\n    return amount\nend\nprint(heal(\"a\", 10, true))\n";
        assert_eq!(names(src), vec!["argument_count"]);
        assert_eq!(
            names(
                "local function heal(who: string, amount: number): number\n    return amount\nend\nprint(heal(\"a\", 10))\n"
            ),
            Vec::<&str>::new()
        );
        // The `{` of an interpolated string closes with its tail.
        assert_eq!(
            names(
                "local function make(name: string, props: {}): {}\n    return props\nend\nlocal n = 1\nprint({ make(\"a\", { Text = `n: {n}` }), 1 })\n"
            ),
            Vec::<&str>::new()
        );
        // The commas of `<<K, V>>` split no argument.
        assert_eq!(
            names(
                "local function pick<K, V>(k: K, v: V): V\n    return v\nend\nlocal function count(n: number, extra: number): number\n    return n + extra\nend\nprint(count(pick<<string, number>>(\"a\", 1), 1))\n"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            names(
                "local function pick<K, V>(k: K, v: V): V\n    return v\nend\nlocal function count(n: number, extra: number): number\n    return n + extra\nend\nprint(count(pick<<string, number>>(\"a\", 1), 1, 2))\n"
            ),
            vec!["argument_count"]
        );
        // A vararg and a default make the count a range.
        assert_eq!(
            names(
                "local function log(fmt: string, ...)\n    print(fmt, ...)\nend\nlog(\"a\", 1, 2)\n"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            names(
                "local function step(n: number, by: number = 1): number\n    return n + by\nend\nprint(step(1, 2))\n"
            ),
            Vec::<&str>::new()
        );
    }

    /// A method on a value the file types, a static, and a function in
    /// a local table count their arguments too.
    #[test]
    fn a_method_a_static_and_a_table_field_count_their_arguments() {
        let head = "struct W as\n    d: number\nend\n\nimpl W as\n    function make(d: number): W\n        return new W { d = d }\n    end\n\n    function dps(self, rate: number): number\n        return self.d * rate\n    end\nend\n\nconst w = W.make(1)\nconst v = new W { d = 2 }\nconst t = { f = function(x: number): number return x end }\n";

        for (call, fires) in [
            ("print(v:dps(1, 2))", true),
            ("print(v:dps(1))", false),
            ("print(W.make(1, 2))", true),
            ("print(W.dps(v, 1, 2))", true),
            ("print(W.dps(v, 1))", false),
            ("print(t.f(1, 2))", true),
            ("print(t.f(1))", false),
            // `w` has no type the file writes, so the count stands down.
            ("print(w:dps(1, 2))", false),
        ] {
            let src = format!("{head}{call}\nprint(w)\n");
            let want: Vec<&str> = if fires {
                vec!["argument_count"]
            } else {
                vec![]
            };
            assert_eq!(names(&src), want, "{call}");
        }

        // A parameter of the table's name is some other value, and a
        // later write puts another function in the field.
        assert_eq!(
            names(
                "const t = { f = function(x: number): number return x end }\nlocal function g(t: { f: (number, number) -> number }): number\n    return t.f(1, 2)\nend\nprint(g, t)\n"
            ),
            Vec::<&str>::new()
        );
        assert_eq!(
            names(
                "local t = { f = function(x: number): number return x end }\nt.f = function(a: number, b: number): number return a + b end\nprint(t.f(1, 2))\n"
            ),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn an_access_after_a_keyword_still_fires() {
        for src in [
            "local function f(t: { x: number }?): number\n    return t.x\nend\n",
            "local function f(s: string?): string\n    return s:upper()\nend\n",
            "local function f(p: Player?): string\n    local n = p.Name\n    print(n)\n    return n\nend\n",
        ] {
            assert_eq!(names(src), vec!["optional_access"], "{src}");
        }

        // The name passed on, not read through, still stands down.
        assert_eq!(
            names("local function f(t: Player?): Player?\n    return t\nend\n"),
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
        assert_eq!(
            names("import m, { a } from \"./m\"\nprint(a)\n"),
            vec!["unused_import"]
        );
        assert_eq!(
            names("import m, { a } from \"./m\"\nprint(a, m)\n"),
            Vec::<&str>::new()
        );
    }

    /// `--fix` cuts the dead name from the list, and the whole statement
    /// when every name it binds is dead. The text it leaves parses, and
    /// a second pass finds nothing to cut.
    #[test]
    fn an_unused_import_carries_its_own_cut() {
        let cut = |src: &str| -> String {
            let lints = crate::compile(src).unwrap().lints;
            let mine: Vec<Lint> = lints
                .into_iter()
                .filter(|l| l.name == "unused_import")
                .collect();
            assert!(mine.iter().all(|l| l.fix.is_some()), "{mine:?}");
            let (after, n) = apply_fixes(src, &mine);
            assert!(n > 0, "{mine:?}");
            // The text still parses, and nothing is left to cut.
            let again = crate::compile(&after).unwrap().lints;
            assert!(
                !again.iter().any(|l| l.name == "unused_import"),
                "{again:?}"
            );

            after
        };

        // One dead name of three: the entry goes with its comma.
        assert_eq!(
            cut("import { a, b, c } from \"./m\"\nprint(a, c)\n"),
            "import { a, c } from \"./m\"\nprint(a, c)\n"
        );
        // The last entry goes with the comma before it.
        assert_eq!(
            cut("import { a, b, c } from \"./m\"\nprint(a, b)\n"),
            "import { a, b } from \"./m\"\nprint(a, b)\n"
        );
        // Every name dead: the statement goes.
        assert_eq!(
            cut("import { a, b } from \"./m\"\nprint(\"hi\")\n"),
            "print(\"hi\")\n"
        );
        assert_eq!(
            cut("import * as m from \"./m\"\nprint(\"hi\")\n"),
            "print(\"hi\")\n"
        );
        // A dead head under a live list, and a dead list under a live
        // head: each leaves the other half.
        assert_eq!(
            cut("import * as m, { a } from \"./m\"\nprint(a)\n"),
            "import { a } from \"./m\"\nprint(a)\n"
        );
        assert_eq!(
            cut("import * as m, { a } from \"./m\"\nprint(m.z)\n"),
            "import * as m from \"./m\"\nprint(m.z)\n"
        );
        // A list over several lines: a dead entry on a line of its own
        // goes with that line, and the comment above it stays.
        assert_eq!(
            cut("import {\n    a, -- the a\n    b,\n    c,\n} from \"./m\"\nprint(a, c)\n"),
            "import {\n    a, -- the a\n    c,\n} from \"./m\"\nprint(a, c)\n"
        );
        assert_eq!(
            cut("import {\n    a, -- the a\n    b\n} from \"./m\"\nprint(a)\n"),
            "import {\n    a, -- the a\n} from \"./m\"\nprint(a)\n"
        );
    }

    /// `local xs = []` names no element type, so `implicit_any` fires
    /// on the binding. A position that carries the type is silent.
    #[test]
    fn an_empty_array_with_no_type_is_an_implicit_any() {
        let src = concat!(
            "struct Hub as\n",
            "    systems: number[] = []\n",
            "end\n",
            "function take(xs: number[])\n",
            "    print(xs)\n",
            "end\n",
            "function make(): number[]\n",
            "    return []\n",
            "end\n",
            "local a = []\n",
            "local b: number[] = []\n",
            "local filled = [1, 2]\n",
            "local nested = [ [], [] ]\n",
            "take([])\n",
            "print(a, b, filled, nested, make(), Hub)\n",
        );
        let out = crate::compile(src).unwrap();
        let hits: Vec<(&str, &str)> = out
            .lints
            .iter()
            .filter(|l| l.name == "implicit_any")
            .map(|l| (l.name, &src[l.start as usize..l.end as usize]))
            .collect();

        // `a` and `nested` only: the annotation, the typed parameter,
        // the declared return, and the struct field all name the type.
        assert_eq!(
            hits,
            vec![("implicit_any", "a"), ("implicit_any", "nested")],
            "{:?}",
            out.lints
        );

        // Pedantic, so a project without `strict` never sees it.
        assert_eq!(
            level_of(&LintConfig::default().without_strict(), "implicit_any"),
            Level::Allow
        );
    }

    #[test]
    fn strict_carries_the_pedantic_group_and_is_on_by_default() {
        let src = "-- Doc.\nexport function f(x)\n    return x\nend\n";
        let out = crate::compile(src).unwrap();
        let mut hits: Vec<&str> = out.lints.iter().map(|l| l.name).collect();
        hits.sort();
        assert_eq!(hits, vec!["implicit_any", "missing_return_type"]);

        let strict = LintConfig::default();
        assert!(strict.strict);
        assert_eq!(level_of(&strict, "implicit_any"), Level::Warn);
        assert_eq!(level_of(&strict, "optional_access"), Level::Warn);

        let lax = strict.without_strict();
        assert_eq!(level_of(&lax, "implicit_any"), Level::Allow);
        assert_eq!(level_of(&lax, "optional_access"), Level::Warn);

        // `print` is ordinary in Luau, so strict leaves it alone and a
        // project that wants it gone names it. A type alias reads as
        // its own documentation, so `missing_doc_type` goes the same
        // way while `missing_doc` rides with strict.
        assert_eq!(level_of(&strict, "print_debug"), Level::Allow);
        assert_eq!(level_of(&strict, "missing_doc"), Level::Warn);
        assert_eq!(level_of(&strict, "missing_doc_type"), Level::Allow);

        let documents_types = LintConfig {
            rules: [("missing_doc_type".to_string(), Level::Warn)]
                .into_iter()
                .collect(),
            ..LintConfig::default()
        };
        assert_eq!(level_of(&documents_types, "missing_doc_type"), Level::Warn);

        let asked = LintConfig {
            rules: [("print_debug".to_string(), Level::Warn)]
                .into_iter()
                .collect(),
            ..LintConfig::default()
        };
        assert_eq!(level_of(&asked, "print_debug"), Level::Warn);

        let by_group = LintConfig {
            rules: [("pedantic".to_string(), Level::Warn)]
                .into_iter()
                .collect(),
            ..LintConfig::default()
        };
        assert_eq!(level_of(&by_group, "print_debug"), Level::Warn);

        let denied = LintConfig {
            rules: [("optional_access".to_string(), Level::Deny)]
                .into_iter()
                .collect(),
            ..LintConfig::default()
        };
        assert_eq!(level_of(&denied, "optional_access"), Level::Deny);
    }

    #[test]
    fn a_group_name_sets_every_lint_in_it_and_a_name_beats_it() {
        let config = LintConfig {
            warn: vec!["pedantic".to_string()],
            allow: vec!["style".to_string(), "explicit_any".to_string()],
            deny: vec!["manual_floor_div".to_string(), "luau".to_string()],
            ..LintConfig::default()
        };
        assert_eq!(level_of(&config, "implicit_any"), Level::Warn);
        assert_eq!(level_of(&config, "explicit_any"), Level::Allow);
        assert_eq!(level_of(&config, "manual_safe_access"), Level::Allow);
        assert_eq!(level_of(&config, "manual_floor_div"), Level::Deny);
        assert_eq!(level_of(&config, "LocalUnused"), Level::Deny);
        assert_eq!(level_of(&LintConfig::default(), "LocalUnused"), Level::Warn);
        assert!(unknown_names(&config).is_empty());
        assert_eq!(group_name("optional_access"), "correctness");
        assert_eq!(group_name("LocalShadow"), "luau");
    }

    #[test]
    fn the_examples_carry_no_default_lints() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples");

        // The examples are their own repository beside this one; a
        // checkout without it skips the test and says so.
        if !dir.is_dir() {
            eprintln!("skipped: no examples checkout at {}", dir.display());

            return;
        }
        let config = LintConfig::default().without_strict();

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

    /// `alloy doc <lint>` answers for every name `alloy lint --list`
    /// prints. A `LINTS` entry answers from its own `detail`; a markup
    /// lint carries no detail, so its page is a table entry under
    /// `alx.`.
    #[test]
    fn every_lint_has_a_doc_entry() {
        for l in LINTS {
            assert!(!l.detail.is_empty(), "no doc for lint `{}`", l.name);
        }

        for l in ALX_LINTS {
            let key = format!("{ALX_PREFIX}{}", l.name);

            assert!(
                crate::docs::lookup(&key).is_some(),
                "no doc for lint `{key}`"
            );
        }
    }
}
