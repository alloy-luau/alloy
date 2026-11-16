//! The feature flags of the compiler. A feature that is not final sits
//! behind a flag, off by default, so it can land on the development
//! branch before it is done. A project turns one on in `[fflags]` of
//! alloy.toml or `.config.aly`; the editor has its own list.
//!
//! A flag leaves this list when its feature becomes the default. A
//! configuration that still names it gets a warning, not an error, so
//! an old file keeps loading.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One flag: its name, as the configuration writes it, and what it
/// turns on.
pub struct Flag {
    pub name: &'static str,
    pub doc: &'static str,
}

pub const NATIVE_IF_LOCAL: Flag = Flag {
    name: "native_if_local",
    doc: "Keep `if local x = e then`, `elseif local`, and the `if` expression form as Luau's own syntax in the ship artifact. Luau reads the syntax only where its `if local` flag is on. A condition with `where`, `not`, a pattern, or more than one name still lowers, and so does every `while local`.",
};

pub const NATIVE_EXPORT: Flag = Flag {
    name: "native_export",
    doc: "Write the exports of a module as Luau's own `export const` statements in the ship artifact, in place of the returned table. Luau reads the syntax only where its export flag is on, and it freezes the table an importer gets. A module with `export default`, or with a `return` at its top level, keeps the returned table.",
};

/// Every flag this version of the compiler reads.
pub const FLAGS: &[&Flag] = &[&NATIVE_IF_LOCAL, &NATIVE_EXPORT];

/// The flags a compile runs under: name to state. A name with no entry
/// is off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct FFlags(pub BTreeMap<String, bool>);

impl FFlags {
    pub fn on(&self, flag: &Flag) -> bool {
        self.0.get(flag.name).copied().unwrap_or(false)
    }

    /// These flags with each entry of `over` on top. The editor's list
    /// goes over the project's this way.
    #[must_use]
    pub fn with(mut self, over: &Self) -> Self {
        self.0.extend(over.0.iter().map(|(k, v)| (k.clone(), *v)));

        self
    }

    /// The names no flag of this version has: a misspelt one, or one
    /// whose feature became the default.
    pub fn unknown(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(String::as_str)
            .filter(|n| FLAGS.iter().all(|f| f.name != *n))
            .collect()
    }

    /// The editor's list, a JSON object of name to `true` or `false`.
    /// A value of another kind is skipped.
    pub fn from_json(value: &serde_json::Value) -> Self {
        let entries = value.as_object().into_iter().flatten();

        Self(
            entries
                .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_off_until_a_list_turns_it_on_and_the_later_list_wins() {
        let project: FFlags = toml::from_str("native_if_local = true\nold_flag = true\n").unwrap();
        assert!(project.on(&NATIVE_IF_LOCAL));
        assert!(!project.on(&NATIVE_EXPORT));
        assert_eq!(project.unknown(), ["old_flag"]);

        let editor = FFlags::from_json(&serde_json::json!({
            "native_if_local": false,
            "native_export": true,
            "skipped": "true",
        }));
        let both = project.with(&editor);
        assert!(!both.on(&NATIVE_IF_LOCAL));
        assert!(both.on(&NATIVE_EXPORT));
    }
}
