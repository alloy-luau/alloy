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

pub use rules::{const_reassignments, run};

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
}

/// Applies the fixes of `lints` to `src`, last to first so the offsets
/// hold. Two fixes that overlap keep the first.
pub fn apply_fixes(src: &str, lints: &[Lint]) -> (String, usize) {
    let mut fixes: Vec<&Fix> = lints.iter().filter_map(|l| l.fix.as_ref()).collect();
    fixes.sort_by_key(|f| (f.start, f.end));
    let mut chosen: Vec<&Fix> = Vec::new();

    for f in fixes {
        if chosen.last().is_none_or(|c| c.end <= f.start) {
            chosen.push(f);
        }
    }

    let mut out = src.to_string();

    for f in chosen.iter().rev() {
        out.replace_range(f.start as usize..f.end as usize, &f.replacement);
    }

    (out, chosen.len())
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
    /// The case of names, off until `[lint.rules] naming = "warn"`.
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
            Group::Naming => "the case of names, off until `[lint.rules] naming = \"warn\"`",
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
        detail: "The extra values are evaluated and dropped, so a mistake in the argument order reads as working code. The lint counts only the functions the file declares by a plain name with a fixed parameter list; a vararg, a default, or a name declared twice makes the count a range and the lint stands down. The checker reports the other direction, a call with too few arguments.",
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
        name: "circular_import",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "two files that import each other",
        detail: "A cycle of `import` lines: Luau's `require` of a module that is still loading is an error at runtime, and the first file to load decides which one fails. Move the shared part into a third module that both import. `alloy flux` reports it; a single-file lint cannot see it.",
    },
    // --- suspicious ------------------------------------------------------------
    LintInfo {
        name: "deprecated_namespace",
        group: Group::Suspicious,
        default: Level::Warn,
        summary: "a use of a namespace declared `@deprecated`",
        detail: "`@deprecated` on a `function` passes through to Luau, which reports a call to it. A namespace has no Luau form, so this lint reports the use instead. The message the attribute carries prints after the name. Inside the namespace the members read each other by their own names, and nothing fires.",
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
        detail: "A `function`, a `local function`, an `async function`, or a local or const bound to a function value, whose name appears nowhere else in the file. An exported function, and a method of an `impl` or a `trait`, are for other files and do not fire. Prefix the name with `_` to keep it on purpose; `alloy flux --fix` does that.",
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
        name: "export_impl",
        group: Group::Style,
        default: Level::Allow,
        summary: "`export impl` on a foreign type, where `global impl` says it",
        detail: "An `impl` on a foreign type such as `BasePart` or `string` works project wide. `export` said that before `global` existed; `global impl` is the spelling now. Off by default while `export impl` is still accepted: `[lint.rules] export_impl = \"warn\"` turns it on, and `alloy flux --fix` rewrites the keyword.",
    },
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
        name: "legacy_iterator",
        group: Group::Style,
        default: Level::Warn,
        summary: "`pairs` or `ipairs` around a `for ... in` table",
        detail: "Flux. Luau iterates a table without a wrapper, arrays in order and then the rest, and honors `__iter`. `pairs` and `ipairs` add a call and hide the metamethod. `alloy flux --fix` removes them.",
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
        detail: "The local is read once, on the next line, by the `return`. `return v` says the same in one statement; a call goes in parentheses, `return (f())`, so the return keeps one value as the local did. `alloy flux --fix` rewrites it.",
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
        name: "shadowed_global",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "a `global` by the name of a std name",
        detail: "Pedantic. The std names `Signal`, `HashMap`, and the rest are ambient in every file. A `global` by one of those names wins over the std everywhere, and a reader who knows the std reads the wrong one. The project's name still works; the lint asks for a name of its own.",
    },
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
        summary: "a named function parameter with no type",
        detail: "Pedantic. A parameter of a named function with no annotation is `any` to the checker, and every use of it goes unchecked. Write the type. A callback passed as an argument is exempt: the checker infers its parameters from the callee.",
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
        detail: "Pedantic. A `print` left over from debugging writes to the output of every player. Remove it, or route it through a logger the project can turn off.",
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
        detail: "Pedantic. An `export` is the interface of the module. A comment line right above it, `--` or `---`, says what it is for; the language server shows it on hover.",
    },
    LintInfo {
        name: "import_order",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "an `import` under code that runs",
        detail: "Pedantic. The emit lifts every `require` to the top of the file, the way TypeScript hoists an import, so a module loads before the line above the import runs. The line reads as if the order were the other way. Move the imports to the top of the file.",
    },
    // --- naming ----------------------------------------------------------------
    LintInfo {
        name: "camel_case_name",
        group: Group::Naming,
        default: Level::Allow,
        summary: "a local, function, or parameter in camelCase",
        detail: "Naming. Alloy code is snake_case: `player_count`, not `playerCount`. Engine members stay PascalCase and Luau builtins lowercase, so the three read as three namespaces. A PascalCase local for a service or a module, `local Players`, is not camelCase and does not fire.",
    },
    LintInfo {
        name: "type_case",
        group: Group::Naming,
        default: Level::Allow,
        summary: "a struct, enum, trait, interface, or type not in PascalCase",
        detail: "Naming. A type name starts with a capital and has no underscore: `PlayerState`. The name of a type reads as one in a signature that way.",
    },
    LintInfo {
        name: "pascal_case_function",
        group: Group::Naming,
        default: Level::Allow,
        summary: "a `local function` in PascalCase",
        detail: "Naming. A local function is snake_case, `load_map`, so a call reads as a call and not as a constructor. A method of an engine protocol, `function Drop:Destroy`, keeps the host's case and does not fire.",
    },
];

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
    if let Some(level) = config.rules.get(key) {
        return Some(*level);
    }

    if config.deny.iter().any(|n| n == key) {
        Some(Level::Deny)
    } else if config.warn.iter().any(|n| n == key) {
        Some(Level::Warn)
    } else if config.allow.iter().any(|n| n == key) {
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
    if let Some((ingot, _)) = name.split_once('/') {
        let ext = external().into_iter().find(|l| l.name == name);

        return listed(config, name)
            .or_else(|| listed(config, ingot))
            .unwrap_or_else(|| match config.recommended {
                true => ext.map(|l| l.default).unwrap_or(Level::Warn),
                false => Level::Allow,
            });
    }

    let info = LINTS.iter().find(|l| l.name == name);
    let group = info.map(|l| l.group.name()).unwrap_or(LUAU_GROUP);

    if let Some(level) = listed(config, name).or_else(|| listed(config, group)) {
        return level;
    }

    match info {
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

/// The group of a lint by name; the type checker's lints are `luau`,
/// and an ingot's lints are the ingot's name.
pub fn group_name(name: &str) -> &'static str {
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
                    && !matches!(*n, "unused_variable" | "unused_function")
            })
            .collect()
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

    /// `export impl` on a foreign type still parses; the lint asks for
    /// `global impl`, and its rewrite is the one word. It is off by
    /// default while both spellings are accepted.
    #[test]
    fn export_impl_asks_for_global_impl() {
        let src = "export impl Vector3 as\n    function flat(self): Vector3\n        return self\n    end\nend\n";
        let out = crate::compile(src).unwrap();
        let hit = out
            .lints
            .iter()
            .find(|l| l.name == "export_impl")
            .expect("the lint fires");
        assert!(hit.message.contains("`global impl`"), "{}", hit.message);
        let fix = hit.fix.as_ref().expect("a rewrite");
        assert_eq!(fix.replacement, "global");
        assert_eq!(&src[fix.start as usize..fix.end as usize], "export");
        assert_eq!(
            level_of(&LintConfig::default(), "export_impl"),
            Level::Allow
        );

        // `global impl` says it already, and an `impl` on a struct of
        // this file is an export of the struct, not a project-wide one.
        let global = crate::compile(&src.replace("export impl", "global impl")).unwrap();
        assert!(!global.lints.iter().any(|l| l.name == "export_impl"));
        let own = crate::compile(
            "struct Vec2 as\n    x: number\nend\nexport impl Vec2 as\n    function len(self): number\n        return self.x\n    end\nend\n",
        )
        .unwrap();
        assert!(!own.lints.iter().any(|l| l.name == "export_impl"));
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
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");

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
}
