use super::super::*;
use super::support::one_file;

/// The mirror's Luau configuration always names the runtime, so a
/// shadow resolves `@alloy` on disk with no sourcemap and no build.
#[test]
pub(crate) fn the_mirror_config_names_the_runtime_and_the_mounts() {
    let dir = std::env::temp_dir().join(format!("alloy-mirror-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join(".config.luau"),
        "return { luau = { aliases = { alloy = \"./build/alloy\", pkg = \"./vendor\" } } }\n",
    )
    .expect("config");
    let config = Config::parse(
            "[build]\nout = \"out\"\n\n[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"packages\", \"@game/ReplicatedStorage/Packages\"]\n",
            Path::new("alloy.toml"),
        )
        .expect("alloy.toml");
    let text = mirror_luau_text(&dir, Some(&config));
    let json: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(json["languageMode"], "strict");
    // The mirror holds no output tree, so the user's own `alloy`
    // alias is replaced by the place the mirror writes the runtime.
    assert_eq!(json["aliases"]["alloy"], "./out/alloy");
    // A name the Luau configuration declares wins over the mount.
    assert_eq!(json["aliases"]["pkg"], "./vendor");
    assert_eq!(json["aliases"]["shared"], "./src/shared");

    // With no alloy.toml the runtime sits at the root.
    let bare = mirror_luau_text(&dir, None);
    let bare: Value = serde_json::from_str(&bare).expect("json");
    assert_eq!(bare["aliases"]["alloy"], "./alloy");
    assert_eq!(bare["aliases"].get("shared"), None);

    let _ = std::fs::remove_dir_all(&dir);
}
/// After Enter on an opener the `end` arrives as one edit at the
/// caret, so the caret keeps the line the editor indented.
#[test]
pub(crate) fn the_newline_edit_writes_the_end_below_the_caret() {
    let (st, uri) = one_file("function f()\n    \n");
    let edit = st.end_edit(uri, 1, 4).expect("an edit");
    assert_eq!(edit[0]["newText"], json!("\nend"));
    assert_eq!(
        edit[0]["range"],
        json!({ "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 4 } })
    );

    // Text on the caret line, and a closed block, write nothing.
    let (st, uri) = one_file("function f()\n    local x = 1\n");
    assert_eq!(st.end_edit(uri, 1, 4), None);
    let (st, uri) = one_file("function f()\n\nend\n");
    assert_eq!(st.end_edit(uri, 1, 0), None);

    // An `end` already under the opener is the one the block has.
    assert!(end_follows("f()\n\nend\n", 4, ""));
    assert!(!end_follows("f()\n\n    end\n", 4, ""));
    assert!(!end_follows("f()\n\nendless()\n", 4, ""));
    assert!(!end_follows("f()\n", 4, ""));
}

/// An opener inside a namespace, an impl, a trait, or a struct's impl
/// writes its `end` at its own column. The body of the enclosing block
/// already ends below, which is where the count alone lost the opener.
#[test]
pub(crate) fn the_end_edit_follows_the_opener_inside_a_body() {
    for (src, line, character, want) in [
        (
            "namespace N as\n    function f()\n        \nend\n",
            2,
            8,
            "\n    end",
        ),
        (
            "impl V as\n    function V.new()\n        \nend\n",
            2,
            8,
            "\n    end",
        ),
        (
            "trait Show as\n    function show(self)\n        \nend\n",
            2,
            8,
            "\n    end",
        ),
        (
            "namespace A as\n    namespace B as\n        function f()\n            \n    end\nend\n",
            3,
            12,
            "\n        end",
        ),
        (
            "struct V as\n    n: number\nend\n\nimpl V as\n    function V.scale(self)\n        \nend\n",
            6,
            8,
            "\n    end",
        ),
        // A file that indents with tabs writes the tab back.
        (
            "namespace N as\n\tfunction f()\n\t\t\nend\n",
            2,
            2,
            "\n\tend",
        ),
    ] {
        let (st, uri) = one_file(src);
        let edit = st.end_edit(uri, line, character).expect("an edit");
        assert_eq!(edit[0]["newText"], json!(want), "{src:?}");
        assert_eq!(
            edit[0]["range"],
            json!({ "start": { "line": line, "character": character },
                    "end": { "line": line, "character": character } }),
            "{src:?}"
        );
    }

    // The method already closes, so the caret line takes nothing.
    let (st, uri) = one_file("impl V as\n    function V.new()\n        \n    end\nend\n");
    assert_eq!(st.end_edit(uri, 2, 8), None);
}
#[test]
pub(crate) fn a_hint_in_parts_folds_as_one_label() {
    let mut result = json!([{
        "position": { "line": 8, "character": 18 },
        "kind": 1,
        "label": [{ "value": ": " }, { "value": "Swinger", "location": {} }, { "value": " & " }, { "value": "Swinger__private" }, { "value": " & { last: number, scope: Scope }" }]
    }]);
    let joined = hint_label(&result[0]);
    result[0]["label"] = json!(joined);
    crate::shapes::fold_value(&mut result, &crate::shapes::Known::default());
    assert_eq!(result[0]["label"], ": Swinger");
}
#[test]
pub(crate) fn a_private_view_hint_folds_through_the_result_path() {
    let mut result = json!([{
        "position": { "line": 8, "character": 18 },
        "kind": 1,
        "label": ": Swinger & Swinger__private & { last: number, scope: Scope }",
        "textEdits": [{ "range": { "start": { "line": 8, "character": 18 }, "end": { "line": 8, "character": 18 } }, "newText": ": Swinger & Swinger__private & { last: number, scope: Scope }" }]
    }]);
    strip_std_prefix(&mut result);
    crate::shapes::fold_value(&mut result, &crate::shapes::Known::default());
    assert_eq!(result[0]["label"], ": Swinger");
}
#[test]
pub(crate) fn declaration_files_stay_out_of_the_child() {
    assert!(!child_sees("file:///w/globals.d.aly"));
    assert!(child_sees("file:///w/main.aly"));
}
#[test]
pub(crate) fn uris_move_into_the_mirror_and_back() {
    let st = State {
        root: Some(PathBuf::from("/w")),
        mirror: PathBuf::from("/m"),
        ..State::default()
    };
    assert_eq!(st.child_uri("file:///w/a/b.aly"), "file:///m/a/b.luau");
    assert_eq!(st.child_uri("file:///w/b.d.aly"), "file:///m/b.d.luau");
    assert_eq!(st.child_uri("file:///w/ui.alx"), "file:///m/ui.luau");
    assert_eq!(st.child_uri("file:///w/x.luau"), "file:///m/x.luau");
    assert_eq!(
        st.child_uri("file:///else/x.luau"),
        "file:///m/_outside/else/x.luau"
    );
    assert_eq!(
        st.editor_uri("file:///m/x.luau"),
        ("file:///w/x.luau".to_string(), false)
    );
    assert_eq!(
        st.editor_uri("file:///m/_outside/else/x.luau"),
        ("file:///else/x.luau".to_string(), false)
    );
    let p = PathBuf::from("/a b/c.aly");
    assert_eq!(path_to_uri(&p), "file:///a%20b/c.aly");
    assert_eq!(uri_to_path("file:///a%20b/c.aly"), Some(p));
}
/// A mount or an alias written by hand: `\` reads as `/`, and a
/// leading `~` is the home directory.
#[test]
pub(crate) fn a_configured_path_reads_backslashes_and_a_tilde() {
    let base = Path::new("/w");
    let home = Path::new("/home/t");
    let at = |text: &str| config_dir_from(base, text, Some(home));
    assert_eq!(at("packages\\roblox"), PathBuf::from("/w/packages/roblox"));
    assert_eq!(at("packages/roblox"), PathBuf::from("/w/packages/roblox"));
    assert_eq!(at("../shared"), PathBuf::from("/shared"));
    assert_eq!(at("~/pkg"), PathBuf::from("/home/t/pkg"));
    assert_eq!(at("~"), PathBuf::from("/home/t"));
    assert_eq!(at("~pkg"), PathBuf::from("/w/~pkg"));
    // No home: the path stays relative to the project.
    assert_eq!(
        config_dir_from(base, "~/pkg", None),
        PathBuf::from("/w/~/pkg")
    );
}
/// A Windows URI: the editor writes the drive as `c%3A` and puts a
/// slash before it. The path keeps the drive and loses the slash.
#[test]
pub(crate) fn a_windows_uri_keeps_its_drive() {
    assert_eq!(
        uri_to_path("file:///c%3A/Users/a/x.aly"),
        Some(PathBuf::from("c:/Users/a/x.aly"))
    );
    assert_eq!(
        uri_to_path("file:///C:/Users/a/x.aly"),
        Some(PathBuf::from("C:/Users/a/x.aly"))
    );
    assert_eq!(
        uri_to_path("file:///c%3A/Program%20Files/x.aly"),
        Some(PathBuf::from("c:/Program Files/x.aly"))
    );
    // A path whose second byte is a colon is a drive only when the
    // colon sits right after one letter.
    assert_eq!(
        uri_to_path("file:///ab:/x.aly"),
        Some(PathBuf::from("/ab:/x.aly"))
    );
    assert_eq!(
        path_to_uri(Path::new("c:/Users/a/x.aly")),
        "file:///c:/Users/a/x.aly"
    );
    // A path the editor wrote comes back as the same path.
    for uri in [
        "file:///c%3A/Users/a/x.aly",
        "file:///c%3A/a%20b/x.aly",
        "file:///home/a/x.aly",
    ] {
        let path = uri_to_path(uri).expect("a path");
        assert_eq!(uri_to_path(&path_to_uri(&path)), Some(path), "{uri}");
    }
}
#[test]
pub(crate) fn results_map_back_to_the_source() {
    let mut st = State {
        root: Some(PathBuf::from("/")),
        mirror: PathBuf::from("/m"),
        ..State::default()
    };
    let src = "local v = a ?? 0\nprint(v)\n";
    st.docs.insert(
        "file:///t.aly".to_string(),
        Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        ),
    );
    st.shadows
        .insert("file:///m/t.luau".to_string(), "file:///t.aly".to_string());

    let mut result = json!([{
        "uri": "file:///m/t.luau",
        "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
    }]);
    map_from_shadow(&mut result, None, &st);
    assert_eq!(result[0]["uri"], "file:///t.aly");
    assert_eq!(result[0]["range"]["end"]["character"], 5);

    // A range in a plain Luau file is left alone; its URI leaves the
    // mirror.
    let mut other = json!({ "uri": "file:///m/x.luau", "range": { "start": { "line": 9, "character": 9 }, "end": { "line": 9, "character": 9 } } });
    map_from_shadow(&mut other, None, &st);
    assert_eq!(other["range"]["start"]["line"], 9);
    assert_eq!(other["uri"], "file:///x.luau");
}
/// A project root in the temp folder, with the files each test
/// names.
pub(crate) fn alias_root(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-lsp-alias-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    for (rel, text) in files {
        let path = dir.join(rel);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the folder");
        }

        std::fs::write(path, text).expect("the file");
    }

    dir
}
/// The alias labels an empty import path offers, `@name/` each.
pub(crate) fn alias_labels(dir: &Path, root: &Path) -> Vec<String> {
    module_entries(dir, Some(root), "", "sourcemap.json", None)
        .into_iter()
        .map(|(label, _, _)| label)
        .filter(|l| l.starts_with('@') && l != "@self/" && l != "@game" && l != "@game/")
        .collect()
}
/// The list of a directory leaves out the file being edited and
/// every `.server` or `.client` script: a module never imports
/// itself, and Roblox runs a script on its own.
#[test]
pub(crate) fn an_import_path_lists_neither_the_file_itself_nor_a_script() {
    let dir = alias_root(
        "self",
        &[
            ("alloy.toml", "[build]\nin = \"src\"\n"),
            ("src/main.aly", ""),
            ("src/helper.aly", ""),
            ("src/boot.server.aly", ""),
            ("src/hud.client.luau", ""),
            ("src/plain.luau", ""),
            ("src/sub/leaf.aly", ""),
        ],
    );
    let src = dir.join("src");
    let own = src.join("main.aly");
    let labels = |head: &str| -> Vec<String> {
        module_entries(&src, Some(&dir), head, "sourcemap.json", Some(&own))
            .into_iter()
            .map(|(label, _, _)| label)
            .filter(|l| !l.starts_with('@'))
            .collect()
    };

    // `@game` and `@game/` name the Roblox services, which no file
    // holds, and the filter above drops both with the aliases.
    assert_eq!(
        labels(""),
        vec![
            "../".to_string(),
            "helper".to_string(),
            "plain".to_string(),
            "sub/".to_string()
        ]
    );
    // `./` lists the same directory, and drops the same names.
    assert_eq!(
        labels("./"),
        vec![
            "helper".to_string(),
            "plain".to_string(),
            "sub/".to_string()
        ]
    );
    // The neighbour's own directory listing keeps `main`.
    let other = src.join("helper.aly");
    let from_other: Vec<String> =
        module_entries(&src, Some(&dir), "./", "sourcemap.json", Some(&other))
            .into_iter()
            .map(|(label, _, _)| label)
            .filter(|l| !l.starts_with('@'))
            .collect();
    assert_eq!(
        from_other,
        vec!["main".to_string(), "plain".to_string(), "sub/".to_string()],
        "the neighbour's list keeps `main`"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
pub(crate) fn an_import_path_offers_the_luau_config_and_the_mounts() {
    let toml = "[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n";
    let dir = alias_root(
        "merge",
        &[
            ("alloy.toml", toml),
            (
                ".config.luau",
                "return { luau = { aliases = { pkg = \"Packages\" } } }\n",
            ),
            ("src/server/main.aly", ""),
        ],
    );
    let src = dir.join("src/server");

    // The Luau configuration and the table both name aliases.
    assert_eq!(
        alias_labels(&src, &dir),
        vec![
            "@pkg/".to_string(),
            "@server/".to_string(),
            "@shared/".to_string()
        ]
    );

    // The same set resolves a path, so `@shared/` lists its files.
    let shared = project_aliases(&src, Some(&dir))
        .into_iter()
        .find(|(a, _)| a == "shared")
        .map(|(_, p)| p);
    assert_eq!(shared, Some(dir.join("src/shared")));

    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
pub(crate) fn a_luaurc_alias_wins_over_a_mount_of_the_same_name() {
    let toml = "[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n";
    let dir = alias_root(
        "clash",
        &[
            ("alloy.toml", toml),
            (
                ".luaurc",
                "{ \"aliases\": { \"shared\": \"vendor/shared\" } }\n",
            ),
            ("src/a.aly", ""),
        ],
    );
    let src = dir.join("src");
    let aliases = project_aliases(&src, Some(&dir));

    assert_eq!(
        aliases,
        vec![
            ("pkg".to_string(), dir.join("Packages")),
            ("shared".to_string(), dir.join("vendor/shared")),
        ]
    );
    assert_eq!(
        alias_labels(&src, &dir),
        vec!["@pkg/".to_string(), "@shared/".to_string()]
    );

    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
pub(crate) fn mount_aliases_off_leaves_the_luau_config_alone() {
    let toml = "[project]\nmount_aliases = false\n\n[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\n";
    let dir = alias_root(
        "off",
        &[
            ("alloy.toml", toml),
            (".luaurc", "{ \"aliases\": { \"pkg\": \"Packages\" } }\n"),
            ("src/a.aly", ""),
        ],
    );
    let src = dir.join("src");

    assert_eq!(alias_labels(&src, &dir), vec!["@pkg/".to_string()]);
    assert!(
        project_aliases(&src, Some(&dir))
            .iter()
            .all(|(a, _)| a != "server")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
pub(crate) fn a_mirrored_sourcemap_points_at_luau() {
    let text = r#"{"name":"game","className":"DataModel","children":[{"name":"Shared","className":"ModuleScript","filePaths":["src/shared/init.aly"],"children":[{"name":"Alloy","className":"ModuleScript","filePaths":["build/alloy.luau"]}]}]}"#;
    let root = Path::new("/w");
    let out = mirrored_sourcemap(text, &root.join("src"), Some(&root.join("build")), root);
    let json: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["children"][0]["filePaths"][0], "src/shared/init.luau");
    assert_eq!(
        json["children"][0]["children"][0]["filePaths"][0],
        "src/alloy.luau"
    );
}
#[test]
pub(crate) fn capabilities_lose_formatting_and_gain_renames() {
    let mut m = json!({ "result": { "capabilities": {
            "documentFormattingProvider": true,
            "semanticTokensProvider": { "legend": {}, "full": { "delta": true }, "range": true }
        } } });
    edit_capabilities(&mut m);
    let caps = &m["result"]["capabilities"];
    assert_eq!(caps["documentFormattingProvider"], true);
    assert_eq!(caps["semanticTokensProvider"]["full"], true);
    assert!(caps["semanticTokensProvider"].get("range").is_none());
    assert!(caps["workspace"]["fileOperations"]["didRename"].is_object());
}
#[test]
pub(crate) fn an_emit_slot_is_no_parameter_hint() {
    assert!(emit_slot_hint(&json!({ "label": "_1:" })));
    assert!(emit_slot_hint(&json!({ "label": "_12:" })));
    assert!(!emit_slot_hint(&json!({ "label": "amount:" })));
    assert!(!emit_slot_hint(&json!({ "label": "_:" })));
}
#[test]
pub(crate) fn a_type_the_source_cannot_write() {
    assert!(writable_type("number[]"));
    assert!(!writable_type("(@checked (string) -> string)?"));
    assert!(!writable_type("t1 where t1 = { }"));
    assert!(!writable_type("*error-type*"));
}
