//! The feature flags. A feature that is not final sits behind a flag,
//! off by default, so it can land on the development branch before it
//! is done. A project sets flags in `[fflags]` of alloy.toml or
//! `.config.aly`, and the editor has a list of its own.
//!
//! One list holds two kinds of flag, told apart by the name. A name
//! that starts with `Alloy` is a flag of this compiler. Any other name
//! is a Luau flag, and goes to luau-lsp as it is.
//!
//! A feature that adds syntax takes two flags. The `Syntax` flag turns
//! on the parse and the emit. The `Analysis` flag turns on the types
//! the check artifact carries for it, and what the editor says about
//! it. With `Syntax` alone the feature runs and the checker reads its
//! values as `any`, so a wrong report points at one layer. `Analysis`
//! alone does nothing. A feature with no syntax of its own takes one
//! flag.
//!
//! `OnByDefault = true` in the list turns on each `AlloyExperimental`
//! flag that has no entry of its own. luau-lsp turns on each Luau flag
//! with no help, so the one key gives a project every flag.
//!
//! An Alloy flag leaves `FLAGS` when its feature becomes the default. A
//! configuration that still names it gets a warning, not an error, so
//! an old file keeps loading.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What the name of every flag of this compiler starts with.
pub const PREFIX: &str = "Alloy";

/// The key that turns on each flag the list does not name. It is
/// neither a flag of Alloy nor one of Luau, so it never goes to luau-lsp.
pub const ON_BY_DEFAULT: &str = "OnByDefault";

/// What the name of a flag starts with when `OnByDefault` reaches it. An
/// `AlloyNative` flag changes the Luau of the ship artifact to syntax
/// that only some runtimes read, so a project names each one itself.
const EXPERIMENTAL: &str = "AlloyExperimental";

/// One flag of the compiler: its name, as the configuration writes it,
/// and what it turns on.
pub struct Flag {
    pub name: &'static str,
    pub doc: &'static str,
}

pub const NATIVE_IF_LOCAL: Flag = Flag {
    name: "AlloyNativeIfLocal",
    doc: "Keep `if local x = e then`, `elseif local`, and the `if` expression form as Luau's own syntax in the ship artifact. Luau reads the syntax only where its `if local` flag is on. A condition with `where`, `not`, a pattern, or more than one name still lowers, and so does every `while local`.",
};

pub const NATIVE_EXPORT: Flag = Flag {
    name: "AlloyNativeExport",
    doc: "Write the exports of a module as Luau's own `export const` statements in the ship artifact, in place of the returned table. Luau reads the syntax only where its export flag is on, and it freezes the table an importer gets. A module with `export default`, or with a `return` at its top level, keeps the returned table.",
};

pub const ENUM_EACH: Flag = Flag {
    name: "AlloyExperimentalEnumEach",
    doc: "The `$each(Enum)` intrinsic: an Array of the items of an enum, in the order the enum declares them. A unit variant is its value, and a variant with a payload is its constructor. The RFC is `syntax-enum-each` in alloy-luau/rfcs.",
};

pub const SUPERTYPES: Flag = Flag {
    name: "AlloyExperimentalSupertypes",
    doc: "The supertypes of Alloy's own declarations. `x is struct`, `x is enum`, and `x is object` test for a struct, an enum, and a value a struct made. As types, `struct<T>` is the struct whose values are `T`, `struct` is any struct, and `enum` is any enum. The RFC is `types-supertypes` in alloy-luau/rfcs.",
};

pub const PIPE_OPERATOR: Flag = Flag {
    name: "AlloyExperimentalPipeOperator",
    doc: "The pipe operator. `a |> f(_)` evaluates `a` once, binds it to `_`, and evaluates the right side with it. The right side must read `_`. The RFC is `syntax-pipe-operator` in alloy-luau/rfcs.",
};

pub const WALRUS_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalWalrusSyntax",
    doc: "The append operator. `xs := v` adds `v` to the end of `xs`. A value with a `push` method takes the call `xs:push(v)`, one with an `add` method takes `xs:add(v)`, and any other table takes `table.insert(xs, v)`. So a Luau table, an Array, a Set, a Queue, and a Heap all append the same way.",
};

pub const WALRUS_ANALYSIS: Flag = Flag {
    name: "AlloyExperimentalWalrusAnalysis",
    doc: "The types of `:=`. The checker reports a value that does not fit the items of the target, and a target that holds no items. With this flag off, `:=` takes any target and any value. It reads `AlloyExperimentalWalrusSyntax`, and does nothing without it.",
};

pub const TABLE_STD_INHERITANCE: Flag = Flag {
    name: "AlloyExperimentalTableStdInheritance",
    doc: "A table has the functions of the `table` library as methods: `t:insert(v)` is `table.insert(t, v)`, and so on for `remove`, `find`, `sort`, `concat`, `clear`, `clone`, `freeze`, `isfrozen`, `unpack`, `move`, and `maxn`. A method the value has under the same name comes first, so an Array keeps its own `remove`, `find`, and `concat`.",
};

pub const COMPREHENSION_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalComprehensionSyntax",
    doc: "The comprehension. `[ x * 2 for _, x in xs if x > 0 ]` is an Array of the value in front of `for`, one for each pass of the loop that the condition keeps. The loop header means what it means in a `for` loop. `[ i * i for i = 1, 10 ]` counts. `where` means the same as `if`.",
};

pub const COMPREHENSION_ANALYSIS: Flag = Flag {
    name: "AlloyExperimentalComprehensionAnalysis",
    doc: "The types of a comprehension. The checker reads the type of the value in front of `for` as the type of the items. With this flag off, the items are `any`. It reads `AlloyExperimentalComprehensionSyntax`, and does nothing without it.",
};

pub const CHAINED_COMPARISON_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalChainedComparisonSyntax",
    doc: "The chained comparison. `a < b <= c` is `a < b and b <= c`, and each operand runs one time. `<` chains with `<=`, and `>` with `>=`. The emit is plain Luau, so the feature has no analysis flag.",
};

pub const LOGICAL_ASSIGN_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalLogicalAssignSyntax",
    doc: "The logical assignments. `a or= b` is `a = a or b`, and `a and= b` is `a = a and b`. An index on the left runs one time. The emit is plain Luau, so the feature has no analysis flag.",
};

pub const INLINE_DATA_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalInlineDataSyntax",
    doc: "The inline data import. `import inline data from \"./data.json\"` writes the document as a table in place of a `require`, so the build writes no module for the file and the file needs no mount. `import inline { a, b } from` writes the values of the keys it names. The emit is plain Luau, so the feature has no analysis flag.",
};

pub const STRING_UNION_MATCH: Flag = Flag {
    name: "AlloyExperimentalStringUnionMatch",
    doc: "The exhaustive match over a union of string literals. When the annotation of the match head is such a union, written inline or as a `type` alias of the same file, a match with an arm for each literal needs no `default` arm, and a match that misses one names it. With this flag off, a match over literals needs a `default` arm.",
};

pub const STD_ADDITIONS: Flag = Flag {
    name: "AlloyExperimentalStdAdditions",
    doc: "The functions of the std that are not final: `Array.range`, `Result.all`, `Future.timeout`, `Future.retry`, `Future.from_signal`, `Future.debounce`, and `Future.throttle`. With this flag off, a call of one reports. The new methods, such as `xs:zip(ys)` and `result:map_or(default, f)`, are not behind the flag: a method call names no module, so the compiler cannot tell one from a method of the project.",
};

pub const STRING_METHODS: Flag = Flag {
    name: "AlloyExperimentalStringMethods",
    doc: "More methods on a string: `s:trim()`, `s:trim_start()`, `s:trim_end()`, `s:starts_with(prefix)`, `s:ends_with(suffix)`, `s:contains(part)`, `s:pad_start(width, fill)`, and `s:pad_end(width, fill)`. A method the value has under the same name comes first.",
};

pub const GENERATOR_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalGeneratorSyntax",
    doc: "Generators. `function* ids(n)` declares a function whose call returns a lazy `Iter` of the values its body yields. `yield v` hands one value to the reader and stops the body until the reader asks for the next. A `return` alone ends the generator.",
};

pub const GENERATOR_ANALYSIS: Flag = Flag {
    name: "AlloyExperimentalGeneratorAnalysis",
    doc: "The types of a generator. The checker reads the item type from the first `yield`, or from a declared return type such as `Iter<number>`, and reports a `yield` that does not fit it. With this flag off, a call of a generator reads as `any`. It reads `AlloyExperimentalGeneratorSyntax`, and does nothing without it.",
};

pub const WIDTH_TYPES: Flag = Flag {
    name: "AlloyExperimentalWidthTypes",
    doc: "The width types: `u8`, `u16`, `u32`, `i8`, `i16`, `i32`, `f32`, and `f64` as type names. Each one is a `number` to the checker, so arithmetic needs no call. On a remote parameter or a struct field the name sets the wire width, as the `@u16` attribute does. A type of the file with the same name comes first. No value wraps or rounds outside a buffer: the `Width` module of the std, behind this flag too, has `wrap`, `saturate`, `fits`, `size`, `min`, and `max` for that.",
};

pub const NET_LIBRARY: Flag = Flag {
    name: "AlloyExperimentalNetLibrary",
    doc: "The `Net` module of the std: HTTP requests over HttpService. `Net.get(url)` starts a request, a chain of calls fills it, and `send` returns a Future that holds a Result, so a failure of the network is a value and raises nothing. With this flag off, a call of `Net` reports.",
};

pub const LAMBDA_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalLambdaSyntax",
    doc: "The short function. `|x| x * 2` is `function(x) return x * 2 end`, and `|| value` takes no parameter. The body is one expression. A parameter takes a type, `|x: number| x * 2`; a union there needs parens.",
};

pub const DEFER_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalDeferSyntax",
    doc: "The `defer` statement. `defer conn:Disconnect()` runs the call when the function it stands in ends: at a `return`, at the last statement, or at an error. `defer do ... end` takes a block. The last `defer` runs first.",
};

pub const BAIL_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalBailSyntax",
    doc: "A way out after `??`. `local x = find(id) ?? return 0` leaves the function when the value is nil, and `x` is not nil on the lines after it. `break` and `continue` stand there too. The form is the whole value of a `local` or a `const` with one name.",
};

pub const RANGE_PATTERN_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalRangePatternSyntax",
    doc: "A range as a pattern. `case 1..10 then` takes a number from 1 up to 10, without 10, and `case 1..=10 then` takes 10 too. Each side is a number. A value that is no number does not match.",
};

pub const USING_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalUsingSyntax",
    doc: "The `using` statement. `using lock = locks:take(id)` binds a name that does not change, and drops its value when the function ends: at a `return`, at the last statement, or at an error. The drop calls the first of `drop`, `Destroy`, `Disconnect`, `close`, and `cancel` the value has. The last `using` drops first.",
};

pub const MUST_USE: Flag = Flag {
    name: "AlloyExperimentalMustUse",
    doc: "The `@must_use` attribute. A call of a function under it that stands alone as a statement drops the result, and the `unused_must_use` lint reports it. `@must_use(\"why\")` adds the reason to the report. The lint reads the file that declares the function; a call from another file is not reported.",
};

pub const NEWTYPE_SYNTAX: Flag = Flag {
    name: "AlloyExperimentalNewtypeSyntax",
    doc: "A struct over one type. `struct UserId(number)` declares a type the checker keeps apart from `number` and from each other struct of this form, so a `PlaceId` does not pass for a `UserId`. `new UserId(5)` makes one and `UserId.raw(id)` gives the number back. The value is the plain number when the code runs.",
};

/// Every flag this version of the compiler reads.
pub const FLAGS: &[&Flag] = &[
    &NATIVE_IF_LOCAL,
    &NATIVE_EXPORT,
    &ENUM_EACH,
    &SUPERTYPES,
    &PIPE_OPERATOR,
    &WALRUS_SYNTAX,
    &WALRUS_ANALYSIS,
    &TABLE_STD_INHERITANCE,
    &COMPREHENSION_SYNTAX,
    &COMPREHENSION_ANALYSIS,
    &CHAINED_COMPARISON_SYNTAX,
    &LOGICAL_ASSIGN_SYNTAX,
    &INLINE_DATA_SYNTAX,
    &STRING_METHODS,
    &STD_ADDITIONS,
    &STRING_UNION_MATCH,
    &GENERATOR_SYNTAX,
    &GENERATOR_ANALYSIS,
    &WIDTH_TYPES,
    &NET_LIBRARY,
    &LAMBDA_SYNTAX,
    &DEFER_SYNTAX,
    &BAIL_SYNTAX,
    &RANGE_PATTERN_SYNTAX,
    &USING_SYNTAX,
    &MUST_USE,
    &NEWTYPE_SYNTAX,
];

/// The value of one flag. An Alloy flag is `true` or `false`; a Luau
/// flag can be a number too. The editor sends each value as text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FlagValue {
    Bool(bool),
    Int(i64),
    Text(String),
}

impl std::fmt::Display for FlagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),

            Self::Int(i) => write!(f, "{i}"),

            Self::Text(s) => f.write_str(s),
        }
    }
}

/// The flags a compile runs under: name to value. A name with no entry
/// is off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct FFlags(pub BTreeMap<String, FlagValue>);

impl FFlags {
    /// Whether a flag is on. An entry of the flag decides. With no
    /// entry, `OnByDefault = true` turns on each experimental flag.
    pub fn on(&self, flag: &Flag) -> bool {
        let is_true = |name: &str| self.0.get(name).is_some_and(|v| v.to_string() == "true");

        match self.0.contains_key(flag.name) {
            true => is_true(flag.name),

            false => flag.name.starts_with(EXPERIMENTAL) && is_true(ON_BY_DEFAULT),
        }
    }

    /// These flags with each entry of `over` on top. The editor's list
    /// goes over the project's this way.
    #[must_use]
    pub fn with(mut self, over: &Self) -> Self {
        self.0
            .extend(over.0.iter().map(|(k, v)| (k.clone(), v.clone())));

        self
    }

    /// The `Alloy` names no flag of this version has: a misspelt one,
    /// or one whose feature became the default.
    pub fn unknown(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(String::as_str)
            .filter(|n| n.starts_with(PREFIX) && FLAGS.iter().all(|f| f.name != *n))
            .collect()
    }

    /// The Luau flags of the list, each as the `--flag:Name=value`
    /// luau-lsp takes.
    pub fn luau_args(&self) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| !k.starts_with(PREFIX) && *k != ON_BY_DEFAULT)
            .map(|(k, v)| format!("--flag:{k}={v}"))
            .collect()
    }

    /// The editor's list, a JSON object of name to value. A value that
    /// is no boolean, whole number, or text is skipped.
    pub fn from_json(value: &serde_json::Value) -> Self {
        let entries = value.as_object().into_iter().flatten();

        Self(
            entries
                .filter_map(|(k, v)| {
                    serde_json::from_value(v.clone())
                        .ok()
                        .map(|v| (k.clone(), v))
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_by_default_turns_on_each_experimental_flag_with_no_entry() {
        let project: FFlags = toml::from_str(
            "OnByDefault = true\nAlloyExperimentalDeferSyntax = false\nLuauTypeLength = 200\n",
        )
        .unwrap();
        assert!(project.on(&LAMBDA_SYNTAX));
        // An entry of the flag wins.
        assert!(!project.on(&DEFER_SYNTAX));
        // A native flag changes what ships, so it stays off.
        assert!(!project.on(&NATIVE_EXPORT));
        // The key is no flag of Luau and no unknown flag of Alloy.
        assert_eq!(project.luau_args(), ["--flag:LuauTypeLength=200"]);
        assert!(project.unknown().is_empty());

        let off: FFlags = toml::from_str("OnByDefault = false\n").unwrap();
        assert!(!off.on(&LAMBDA_SYNTAX));
        assert!(off.luau_args().is_empty());
    }

    #[test]
    fn one_list_holds_the_flags_of_alloy_and_of_luau() {
        let project: FFlags = toml::from_str(
            "AlloyNativeIfLocal = true\nAlloyOldFlag = true\nLuauSolverV2 = false\nLuauTypeLength = 200\n",
        )
        .unwrap();
        assert!(project.on(&NATIVE_IF_LOCAL));
        assert!(!project.on(&NATIVE_EXPORT));
        // A Luau name is never an unknown flag of Alloy.
        assert_eq!(project.unknown(), ["AlloyOldFlag"]);
        assert_eq!(
            project.luau_args(),
            ["--flag:LuauSolverV2=false", "--flag:LuauTypeLength=200"]
        );

        // The editor sends text, and its list wins.
        let editor = FFlags::from_json(&serde_json::json!({
            "AlloyNativeIfLocal": "false",
            "AlloyNativeExport": "true",
            "skipped": [1],
        }));
        let both = project.with(&editor);
        assert!(!both.on(&NATIVE_IF_LOCAL));
        assert!(both.on(&NATIVE_EXPORT));
    }
}
