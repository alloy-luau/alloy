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
//! An Alloy flag leaves `FLAGS` when its feature becomes the default. A
//! configuration that still names it gets a warning, not an error, so
//! an old file keeps loading.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What the name of every flag of this compiler starts with.
pub const PREFIX: &str = "Alloy";

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
    pub fn on(&self, flag: &Flag) -> bool {
        self.0
            .get(flag.name)
            .is_some_and(|v| v.to_string() == "true")
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
            .filter(|(k, _)| !k.starts_with(PREFIX))
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
