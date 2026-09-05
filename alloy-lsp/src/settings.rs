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
        },
        "completion": {
            "autocompleteEnd": true,
        },
    })
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
    // extension's server for the same one.
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
    }

    out
}

/// The command line flags for the child from the editor's `fflags`
/// section: `--flag:Name=value` for each override, and the new solver
/// unless `enableNewSolver` is false.
pub fn child_flags(options: &Value) -> Vec<String> {
    let section = options
        .pointer("/luauLsp/fflags")
        .or_else(|| options.get("fflags"));
    let mut flags = Vec::new();

    let new_solver = section
        .and_then(|f| f.get("enableNewSolver"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if new_solver {
        flags.push("--flag:LuauSolverV2=true".to_string());
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
        assert_eq!(flags, ["--flag:LuauSolverV2=true", "--flag:LuauX=true"]);
        let flags = child_flags(&json!({ "luauLsp": { "fflags": { "enableNewSolver": false } } }));
        assert!(flags.is_empty());
        assert_eq!(child_flags(&json!({})), ["--flag:LuauSolverV2=true"]);
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
