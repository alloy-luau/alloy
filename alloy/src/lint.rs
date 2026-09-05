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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Silent.
    Allow,
    /// Printed; the exit code stays zero.
    Warn,
    /// Printed; the exit code is one.
    Deny,
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
    /// Strict rules, off until `[lint] strict = true` or `warn = ["pedantic"]`.
    Pedantic,
    /// The case of names, off until `warn = ["naming"]`.
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
            Group::Pedantic => "strict rules, off until `[lint] strict = true`",
            Group::Naming => "the case of names, off until `[lint] warn = [\"naming\"]`",
        }
    }
}

/// The group of the type checker's own lints, `LocalUnused` and the
/// rest, which `alloy flux` reports beside these. `[lint]` sets their
/// level by this name.
pub const LUAU_GROUP: &str = "luau";

/// The description of one lint, for `alloy doc lints` and `--list`.
pub struct LintInfo {
    pub name: &'static str,
    pub group: Group,
    /// The level with no `[lint]` table. `Allow` marks a pedantic
    /// lint, which `strict = true` raises to `Warn`.
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
        name: "circular_import",
        group: Group::Correctness,
        default: Level::Warn,
        summary: "two files that import each other",
        detail: "A cycle of `import` lines: Luau's `require` of a module that is still loading is an error at runtime, and the first file to load decides which one fails. Move the shared part into a third module that both import. `alloy flux` reports it; a single-file lint cannot see it.",
    },
    // --- suspicious ------------------------------------------------------------
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
        detail: "A `local`, a `local function`, or a loop variable that appears nowhere after its declaration. A leftover, or a typo in the name that reads it. Prefix it with `_` to say it is unused on purpose; `alloy flux --fix` does that. The type checker reports the scoped cases this lint cannot see.",
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
        detail: "The lowercase members are the pre-2014 names, kept for old places and gone from the docs. `Connect`, `Wait`, `Destroy`, `Clone`, `GetChildren`, `FindFirstChild`, and `IsA` are the current ones, and the checker knows only those. A method of the same name that the file declares does not fire. `alloy flux --fix` rewrites them.",
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
        name: "missing_doc",
        group: Group::Pedantic,
        default: Level::Allow,
        summary: "an exported declaration with no comment above it",
        detail: "Pedantic. An `export` is the interface of the module. A comment line right above it, `--` or `---`, says what it is for; the language server shows it on hover.",
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

/// The level a lint runs at under a config: its own name in a list
/// first, then its group's name, then its default. A name the table
/// lacks is a lint of the type checker, under the `luau` group.
pub fn level_of(config: &LintConfig, name: &str) -> Level {
    let info = LINTS.iter().find(|l| l.name == name);
    let group = info.map(|l| l.group.name()).unwrap_or(LUAU_GROUP);
    let listed = |key: &str| {
        if config.allow.iter().any(|n| n == key) {
            Some(Level::Allow)
        } else if config.deny.iter().any(|n| n == key) {
            Some(Level::Deny)
        } else if config.warn.iter().any(|n| n == key) {
            Some(Level::Warn)
        } else {
            None
        }
    };

    if let Some(level) = listed(name).or_else(|| listed(group)) {
        return level;
    }

    match info {
        Some(l) if l.group == Group::Pedantic && config.strict => Level::Warn,
        Some(l) => l.default,
        None => Level::Warn,
    }
}

/// The group of a lint by name; the type checker's lints are `luau`.
pub fn group_name(name: &str) -> &'static str {
    LINTS
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.group.name())
        .unwrap_or(LUAU_GROUP)
}

/// A `[lint]` name that is neither a lint nor a group.
pub fn unknown_names(config: &LintConfig) -> Vec<String> {
    config
        .allow
        .iter()
        .chain(&config.warn)
        .chain(&config.deny)
        .filter(|n| {
            !LINTS.iter().any(|l| l.name == n.as_str())
                && Group::from_name(n).is_none()
                && n.as_str() != LUAU_GROUP
        })
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
pub fn run(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    definitions: bool,
    thresholds: &Thresholds,
) -> Vec<Lint> {
    let mut lints = Vec::new();

    if definitions {
        return lints;
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
                    fix: None,
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
                    fix: None,
                });
            }
        }
    }

    let scan = crate::flux_scan::Scan::new(src, toks, &st);
    lints.extend(crate::flux::run(&scan));
    lints.extend(crate::flux_correctness::run(&scan));
    lints.extend(crate::flux_complexity::run(&scan, thresholds));
    lints.extend(crate::flux_roblox::run(&scan));
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

    /// The lints at their default level: the pedantic ones stay out.
    fn names(src: &str) -> Vec<&'static str> {
        let out = crate::compile(src).unwrap();
        let config = LintConfig::default();

        out.lints
            .iter()
            .map(|l| l.name)
            .filter(|n| level_of(&config, n) != Level::Allow && *n != "unused_variable")
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
        let src = "-- Doc.\nexport function f(x)\n    return x\nend\n";
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
