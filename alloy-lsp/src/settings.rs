//! The child's settings. luau-lsp asks for its `luau-lsp` section over
//! `workspace/configuration`; the proxy answers from this object, so the
//! child's behavior does not depend on what the editor has installed.

use serde_json::{Value, json};

/// What the child sees when the editor says nothing: type hints on
/// variables, loop variables, parameters, and returns, each insertable
/// on a double click.
pub fn defaults() -> Value {
    json!({
        "inlayHints": {
            "variableTypes": true,
            "parameterTypes": true,
            "functionReturnTypes": true,
            "parameterNames": "literals",
            "makeInsertable": true,
            "hideHintsForDuplicateParameterNames": true,
            // The child cuts a long type with `*TRUNCATED*`; the proxy
            // folds the type to its name first and shortens what is left.
            "typeHintMaxLength": 4000,
        },
        "completion": {
            // The proxy writes the `end` of an open block itself, as an
            // on-type edit from the Alloy source: the child sees the
            // shadow, where `struct`, `trait`, and `match` are already
            // gone, and its own item would open a popup on every Enter.
            "autocompleteEnd": false,
        },
    })
}

/// The proxy's own editor options. The two helpers are on until the
/// editor turns one off; the two deprecation filters are off until it
/// turns one on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Editor {
    /// Name the element the editor closes after the `>` that ends an
    /// opening tag.
    pub auto_close_tags: bool,
    /// Write the `end` of an open block after Enter.
    pub auto_end: bool,
    /// Leave a member the Roblox API marks deprecated out of every
    /// completion list.
    pub hide_roblox_deprecated: bool,
    /// Leave out what the source marks `@deprecated` as well.
    pub hide_all_deprecated: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            auto_close_tags: true,
            auto_end: true,
            hide_roblox_deprecated: false,
            hide_all_deprecated: false,
        }
    }
}

/// The proxy's own options from the editor's settings object. A key the
/// editor leaves out keeps the value it has, so a settings change that
/// names one feature does not reset the other.
pub fn editor(options: &Value, current: Editor) -> Editor {
    let flag = |name: &str, now: bool| options.get(name).and_then(Value::as_bool).unwrap_or(now);

    Editor {
        auto_close_tags: flag("autoCloseTags", current.auto_close_tags),
        auto_end: flag("autoEnd", current.auto_end),
        hide_roblox_deprecated: flag("hideRobloxDeprecated", current.hide_roblox_deprecated),
        hide_all_deprecated: flag("hideAllDeprecated", current.hide_all_deprecated),
    }
}

/// Deep-merges `over` into `base`: objects merge key by key, anything
/// else replaces.
pub fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge(existing, v),

                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        (b, o) => *b = o.clone(),
    }
}

/// The editor's `initializationOptions` or `didChangeConfiguration`
/// settings, in the extension's shape: `luauLsp` holds a whole luau-lsp
/// section and `inlayHints` an overlay. A bare luau-lsp section works
/// too.
pub fn from_editor(options: &Value) -> Value {
    let mut out = json!({});

    if let Some(section) = options.get("luauLsp") {
        merge(&mut out, section);
    }

    if let Some(hints) = options.get("inlayHints") {
        merge(&mut out, &json!({ "inlayHints": hints }));
    }

    // The Alloy extension decides the Studio plugin: off unless its own
    // setting says so, on its own port, so it never fights the luau-lsp
    // extension's server for the same one. luau-lsp reads the legacy
    // `plugin` key as well as `studioPlugin`, so a user's global
    // `luau-lsp.plugin.enabled` rode through the section above and bound
    // the port anyway; that key goes.
    if let Some(o) = out.as_object_mut() {
        o.remove("plugin");
    }

    if let Some(plugin) = options.get("studioPlugin") {
        merge(&mut out, &json!({ "studioPlugin": plugin }));
    }

    // The sourcemap file, relative to the root; the mirror keeps the
    // layout, so the child finds it at the same relative path.
    if let Some(file) = options.pointer("/sourcemap/file").and_then(Value::as_str) {
        merge(
            &mut out,
            &json!({ "sourcemap": { "enabled": true, "sourcemapFile": file } }),
        );
    }

    if options.get("luauLsp").is_none() && options.get("inlayHints").is_none() {
        merge(&mut out, options);
    }

    // `fflags` belongs to the luau-lsp editor extension, which turns it
    // into command line flags. The server rejects the whole settings
    // object when the section is present, and then keeps its own
    // defaults, where every inlay hint is off. `main.rs` reads the
    // section from the first message and passes the flags itself.
    if let Some(o) = out.as_object_mut() {
        o.remove("fflags");

        // The proxy's own options; the child knows none of the names.
        o.remove("autoCloseTags");
        o.remove("autoEnd");
        o.remove("hideRobloxDeprecated");
        o.remove("hideAllDeprecated");
    }

    out
}

/// The command line flags for the child from the editor's `fflags`
/// section: `--no-flags-enabled` when `enableByDefault` is false,
/// `--flag:Name=value` for each override, and the new solver unless
/// `enableNewSolver` is false. The Alloy extension sends its own section
/// under `fflags`, over the user's luau-lsp one.
pub fn child_flags(options: &Value) -> Vec<String> {
    let section = options
        .get("fflags")
        .or_else(|| options.pointer("/luauLsp/fflags"));
    let mut flags = Vec::new();

    let by_default = section
        .and_then(|f| f.get("enableByDefault"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if !by_default {
        flags.push("--no-flags-enabled".to_string());
    }

    let new_solver = section
        .and_then(|f| f.get("enableNewSolver"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if new_solver {
        flags.push("--flag:LuauSolverV2=true".to_string());
    }

    // A printed type must arrive whole: the proxy folds a struct's
    // table back to its name from the complete text, and the default
    // limit cuts a struct with a few methods to `*TRUNCATED*`.
    let overrides = section
        .and_then(|f| f.get("override"))
        .and_then(Value::as_object);

    for name in [
        "LuauTypeMaximumStringifierLength",
        "LuauTableTypeMaximumStringifierLength",
    ] {
        if !overrides.is_some_and(|o| o.contains_key(name)) {
            flags.push(format!("--flag:{name}=200000"));
        }
    }

    if let Some(overrides) = section
        .and_then(|f| f.get("override"))
        .and_then(Value::as_object)
    {
        for (name, value) in overrides {
            let value = match value {
                Value::String(s) => s.clone(),

                other => other.to_string(),
            };
            flags.push(format!("--flag:{name}={value}"));
        }
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_section_turns_into_child_arguments() {
        let flags = child_flags(&json!({
            "fflags": { "enableByDefault": false, "override": { "LuauTableTypeMaximumStringifierLength": "100", "LuauX": true } }
        }));
        assert_eq!(
            flags,
            vec![
                "--no-flags-enabled",
                "--flag:LuauSolverV2=true",
                "--flag:LuauTypeMaximumStringifierLength=200000",
                "--flag:LuauTableTypeMaximumStringifierLength=100",
                "--flag:LuauX=true"
            ]
        );
        assert_eq!(
            child_flags(&json!({})),
            vec![
                "--flag:LuauSolverV2=true",
                "--flag:LuauTypeMaximumStringifierLength=200000",
                "--flag:LuauTableTypeMaximumStringifierLength=200000"
            ]
        );
        assert_eq!(
            child_flags(&json!({ "fflags": { "enableNewSolver": false } })),
            vec![
                "--flag:LuauTypeMaximumStringifierLength=200000",
                "--flag:LuauTableTypeMaximumStringifierLength=200000"
            ]
        );
    }

    #[test]
    fn fflags_never_reach_the_child_settings() {
        let s = from_editor(
            &json!({ "luauLsp": { "fflags": { "enableNewSolver": true }, "completion": { "autocompleteEnd": false } } }),
        );
        assert!(s.get("fflags").is_none());
        assert_eq!(s["completion"]["autocompleteEnd"], false);
    }

    #[test]
    fn fflags_become_command_line_flags() {
        let flags =
            child_flags(&json!({ "luauLsp": { "fflags": { "override": { "LuauX": "true" } } } }));
        assert_eq!(flags[0], "--flag:LuauSolverV2=true");
        assert_eq!(flags.last().unwrap(), "--flag:LuauX=true");
        let flags = child_flags(&json!({ "luauLsp": { "fflags": { "enableNewSolver": false } } }));
        assert!(flags.iter().all(|f| f.contains("Stringifier")));
        assert_eq!(child_flags(&json!({}))[0], "--flag:LuauSolverV2=true");
    }

    #[test]
    fn the_proxys_own_options_stay_out_of_the_child_settings() {
        let s = from_editor(
            &json!({ "luauLsp": {}, "autoCloseTags": false, "autoEnd": false,
                                     "hideRobloxDeprecated": true, "hideAllDeprecated": true }),
        );
        assert!(s.get("autoCloseTags").is_none());
        assert!(s.get("autoEnd").is_none());
        assert!(s.get("hideRobloxDeprecated").is_none());
        assert!(s.get("hideAllDeprecated").is_none());
    }

    /// Both filters start off, and a settings change that names one
    /// leaves the other where it was.
    #[test]
    fn the_deprecation_filters_start_off() {
        let start = Editor::default();
        assert!(!start.hide_roblox_deprecated && !start.hide_all_deprecated);
        let roblox = editor(&json!({ "hideRobloxDeprecated": true }), start);
        assert!(roblox.hide_roblox_deprecated);
        assert!(!roblox.hide_all_deprecated);
        let both = editor(&json!({ "hideAllDeprecated": true }), roblox);
        assert!(both.hide_roblox_deprecated && both.hide_all_deprecated);
        assert_eq!(editor(&json!({ "inlayHints": {} }), both), both);
        assert!(!editor(&json!({ "hideRobloxDeprecated": false }), both).hide_roblox_deprecated);
    }

    #[test]
    fn the_editor_options_default_on_and_keep_what_they_have() {
        let both = Editor::default();
        assert!(both.auto_close_tags && both.auto_end);
        let off = editor(&json!({ "autoCloseTags": false }), both);
        assert!(!off.auto_close_tags);
        assert!(off.auto_end);
        // A later settings change that names neither key changes nothing.
        assert_eq!(editor(&json!({ "inlayHints": {} }), off), off);
        assert!(editor(&json!({ "autoCloseTags": true }), off).auto_close_tags);
    }

    #[test]
    fn the_sourcemap_file_reaches_the_child() {
        let s = from_editor(&json!({ "sourcemap": { "file": "build/sourcemap.json" } }));
        assert_eq!(s["sourcemap"]["sourcemapFile"], "build/sourcemap.json");
        assert_eq!(s["sourcemap"]["enabled"], true);
    }

    #[test]
    fn the_extension_owns_the_studio_plugin() {
        let s = from_editor(
            &json!({ "luauLsp": { "studioPlugin": { "enabled": true, "port": 3667 } }, "studioPlugin": { "enabled": false, "port": 3668 } }),
        );
        assert_eq!(s["studioPlugin"]["enabled"], false);
        assert_eq!(s["studioPlugin"]["port"], 3668);
    }

    /// `luau-lsp.plugin.enabled` is the legacy spelling luau-lsp still
    /// reads. Left in the passthrough, it bound the Studio port under a
    /// second editor and the child died with "already in use".
    #[test]
    fn the_legacy_plugin_key_never_reaches_the_child() {
        let s = from_editor(&json!({ "luauLsp": { "plugin": { "enabled": true, "port": 3667 } } }));
        assert!(s.get("plugin").is_none(), "{s}");
        let s = from_editor(
            &json!({ "luauLsp": { "plugin": { "enabled": true } }, "studioPlugin": { "enabled": false, "port": 3668 } }),
        );
        assert!(s.get("plugin").is_none(), "{s}");
        assert_eq!(s["studioPlugin"]["enabled"], false);
    }

    #[test]
    fn merge_is_deep_and_editor_wins() {
        let mut s = defaults();
        merge(
            &mut s,
            &from_editor(&json!({ "inlayHints": { "parameterNames": "none" } })),
        );
        assert_eq!(s["inlayHints"]["parameterNames"], "none");
        assert_eq!(s["inlayHints"]["variableTypes"], true);
    }
}
