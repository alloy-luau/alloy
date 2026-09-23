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
/// Luau reads `src/init.luau` as the module `src`, so a relative
/// require in it names a file beside `src`. The shadow of an `init.aly`
/// writes the path from there. Without it the child resolves no module
/// and every imported name reads as `unknown`.
#[test]
pub(crate) fn an_init_shadow_requires_a_sibling_through_its_folder() {
    let dir = std::env::temp_dir().join(format!("alloy-init-require-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .expect("alloy.toml");
    std::fs::write(
        dir.join("src/scheduler.aly"),
        "export struct Plain as\n    phase: number,\nend\n",
    )
    .expect("scheduler.aly");

    let st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        ..State::default()
    };
    let source = "import { Plain } from \"./scheduler\"\n\nlocal p = Plain\n";
    let init = |name: &str| {
        let (options, jsx) = st.options_for(&path_to_uri(&dir.join(name)));

        Doc::new(source.to_string(), 1, &options, &jsx, None).shadow
    };
    let shadow = init("src/init.aly");

    assert!(shadow.contains("require(\"./src/scheduler\")"), "{shadow}");

    // A file that is no `init` keeps the path the source wrote.
    let shadow = init("src/other.aly");

    assert!(shadow.contains("require(\"./scheduler\")"), "{shadow}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// An `impl` on a struct another file declares reaches the declaring
/// file's check artifact, the way the project build feeds it. Without
/// the index the child reports `Cannot add property` on the impl.
#[test]
pub(crate) fn the_project_impl_index_reaches_the_declaring_file() {
    let dir = std::env::temp_dir().join(format!("alloy-project-impls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .expect("alloy.toml");
    let declaring = "export struct Box as\n    value: number,\n    private secret: number,\nend\n";
    std::fs::write(dir.join("src/mod_a.aly"), declaring).expect("mod_a.aly");
    std::fs::write(
        dir.join("src/main.aly"),
        "import { Box } from \"./mod_a\"\n\nimpl Box as\n    function getSecret(self): number\n        return self.secret\n    end\nend\n",
    )
    .expect("main.aly");

    let st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        ..State::default()
    };
    let index = st.project_impls();

    assert!(
        index
            .methods
            .iter()
            .any(|m| m.target == "Box" && m.name == "getSecret"),
        "{:?}",
        index.methods
    );

    // The declaring file's shadow declares the method, so the impl in
    // the other file has a place to land.
    let (options, jsx) = st.options_for(&path_to_uri(&dir.join("src/mod_a.aly")));
    let doc = Doc::new(declaring.to_string(), 1, &options, &jsx, None);

    assert!(doc.shadow.contains("getSecret"), "{}", doc.shadow);

    // The index is remembered until the project changes.
    st.forget_disk();
    std::fs::write(dir.join("src/main.aly"), "local x = 1\n").expect("main.aly");

    assert!(st.project_impls().methods.is_empty());

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
    alloy::shapes::fold_value(&mut result, &alloy::shapes::Known::default());
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
    alloy::shapes::fold_value(&mut result, &alloy::shapes::Known::default());
    assert_eq!(result[0]["label"], ": Swinger");
}
/// The gutter on `local xs = []` reads `any[]`, not `any`: the check
/// artifact casts the empty literal to `Array<any>`, and the fold
/// writes the sugar the source has.
#[test]
pub(crate) fn an_empty_array_hint_reads_as_an_array() {
    let (st, uri) = one_file("local xs = []\nprint(xs)\n");
    let doc = st.docs.get(uri).expect("doc");

    assert!(
        doc.shadow.contains(":: __alloy.Array<any>"),
        "{}",
        doc.shadow
    );

    let mut result = json!([{
        "position": { "line": 0, "character": 8 },
        "kind": 1,
        "label": ": __alloy.Array<any>",
        "textEdits": [{ "range": { "start": { "line": 0, "character": 8 }, "end": { "line": 0, "character": 8 } }, "newText": ": __alloy.Array<any>" }]
    }]);
    strip_std_prefix(&mut result);
    alloy::shapes::fold_value(&mut result, &alloy::shapes::Known::default());

    assert_eq!(result[0]["label"], ": any[]");
    assert_eq!(result[0]["textEdits"][0]["newText"], ": any[]");
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
            "documentRangeFormattingProvider": true,
            "diagnosticProvider": { "interFileDependencies": true, "workspaceDiagnostics": false },
            "semanticTokensProvider": { "legend": {}, "full": { "delta": true }, "range": true }
        } } });
    edit_capabilities(&mut m);
    let caps = &m["result"]["capabilities"];
    assert_eq!(caps["documentFormattingProvider"], true);

    // The proxy publishes; a client that also pulled saw each report
    // twice.
    assert!(caps.get("diagnosticProvider").is_none());

    // `alloy fmt` reads a whole file, so the editor offers no
    // "Format Selection" over a range it cannot hold.
    assert!(caps.get("documentRangeFormattingProvider").is_none());
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
    assert!(writable_type("~number"));
    assert!(!writable_type("(@checked (string) -> string)?"));
    assert!(!writable_type("t1 where t1 = { }"));
    assert!(!writable_type("*error-type*"));
}

/// A writer the tests read back: what the server sent the editor.
#[derive(Clone)]
pub(crate) struct Recorder(pub(crate) Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("the log").extend_from_slice(buf);

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A deleted module refreshes the files that import it. The editor
/// sends the watched file event and nothing else, so the importer must
/// hear its diagnostics again on the delete alone.
#[test]
pub(crate) fn a_deleted_module_refreshes_its_importers() {
    let dir = alias_root(
        "deleted-import",
        &[
            ("alloy.toml", "[build]\nin = \"src\"\nout = \"build\"\n"),
            (
                "src/shape.aly",
                "export struct Circle as\n    radius: number\nend\n",
            ),
            (
                "src/main.aly",
                "import { Circle } from \"./shape\"\n\nlocal c = new Circle { radius = 5 }\nprint(c.radius)\n",
            ),
        ],
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(Recorder(Arc::clone(&log))),
        Vec::new(),
        None,
    );

    {
        let mut st = server.state.lock().expect("state");
        st.root = Some(dir.clone());
        st.mirror = dir.join("mirror");
    }

    let shape_uri = path_to_uri(&dir.join("src/shape.aly"));
    let main_uri = path_to_uri(&dir.join("src/main.aly"));

    for uri in [&shape_uri, &main_uri] {
        let text = std::fs::read_to_string(uri_to_path(uri).expect("path")).expect("source");
        server.open_doc(uri, text, 1, true);
    }

    log.lock().expect("the log").clear();
    std::fs::remove_file(dir.join("src/shape.aly")).expect("the delete");
    server.close_shadow(&shape_uri);

    let sent = String::from_utf8_lossy(&log.lock().expect("the log").clone()).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(sent.contains("UnknownModule"), "{sent}");
}

/// An `end` past the one that closes an `impl` is an error node the
/// emit copies through, so the child stopped there and nothing below
/// it answered. The repair blanks the word, and the shadow parses.
#[test]
fn a_stray_end_leaves_the_rest_of_the_file_to_the_child() {
    let src = "impl Something as\n    function m(self)\n    end\nend\nend\nfunction helper3(): number\n    return 44\nend\n";
    let (st, uri) = one_file(src);
    let doc = &st.docs[uri];
    let repair = doc.repair.as_ref().expect("the repair");

    assert!(repair.spots.is_empty());
    assert_eq!(repair.source.len(), src.len());
    assert!(repair.output.parsed_clean);
    assert_eq!(doc.shadow.lines().nth(4), Some("   "), "{}", doc.shadow);
    assert!(doc.shadow.contains("function helper3(): number"));
    // The author's own compile still reports the stray `end`.
    let out = doc.output.as_ref().expect("output");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message == "unexpected `end`"),
        "{:?}",
        out.diagnostics
    );
}

/// An import into another project: the emit writes the require relative
/// to the source, so the dependency's shadow sits where that path names
/// it, above the mirror's root, and the real path reads back from it.
/// A path too far out still goes under `_outside`.
#[test]
fn a_dependency_shadow_sits_where_the_require_names_it() {
    let dir = std::env::temp_dir().join(format!("alloy-dep-mirror-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let app = dir.join("ws/app");
    let shared = dir.join("ws/shared");
    std::fs::create_dir_all(shared.join("src")).expect("shared");
    std::fs::write(
        shared.join("src/util.aly"),
        "export function double(n: number): number\n    return n * 2\nend\n",
    )
    .expect("util");

    let st = State {
        root: Some(app.clone()),
        mirror: mirror_dir(Some(&app), mirror_above(Some(&app))),
        ..State::default()
    };
    let main = st.mirror_path(&app.join("src/main.aly"));
    let util = shared.join("src/util.aly");
    let lands = normalize(
        &main
            .parent()
            .expect("dir")
            .join("../../shared/src/util.luau"),
    );

    assert_eq!(lands, st.mirror_path(&util));
    assert!(lands.starts_with(mirror_base(&st.mirror)), "{lands:?}");
    assert_eq!(
        st.real_path(&lands),
        Some(normalize(&util.with_extension("luau")))
    );
    assert_eq!(
        st.real_path(&st.mirror_path(&app.join("src/main.aly"))),
        Some(normalize(&app.join("src/main.luau")))
    );

    // Five folders up is too far: a root that deep keeps such a file
    // under `_outside`.
    let deep = State {
        root: Some(dir.join("a/b/c/d/e/app")),
        mirror: mirror_dir(Some(&dir.join("a/b/c/d/e/app")), mirror_above(None)),
        ..State::default()
    };
    let far = dir.join("x.aly");
    assert!(
        deep.mirror_path(&far)
            .starts_with(deep.mirror.join("_outside"))
    );
    assert_eq!(
        deep.real_path(&deep.mirror_path(&far)),
        Some(normalize(&far.with_extension("luau")))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The mirror keeps as many folders above the root as the deepest
/// dependency needs: a project five folders up, past the fixed four,
/// still lands inside the mirror, and its real path reads back.
#[test]
fn the_mirror_depth_follows_the_deepest_dependency() {
    let dir = alias_root(
        "mirror-depth",
        &[
            (
                "a/b/c/d/app/alloy.toml",
                "[build]\nin = \"src\"\nout = \"build\"\n",
            ),
            (
                "a/b/c/d/app/src/main.aly",
                "import { double } from \"../../../../../../shared/src/util\"\nprint(double(2))\n",
            ),
            (
                "shared/alloy.toml",
                "[build]\nin = \"src\"\nout = \"build\"\n",
            ),
            (
                "shared/src/util.aly",
                "export function double(n: number): number\n    return n * 2\nend\n",
            ),
        ],
    );
    let app = dir.join("a/b/c/d/app");
    let above = mirror_above(Some(&app));
    let st = State {
        root: Some(app.clone()),
        mirror: mirror_dir(Some(&app), above),
        ..State::default()
    };
    let util = dir.join("shared/src/util.aly");
    let lands = st.mirror_path(&util);
    let back = st.real_path(&lands);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(above, 5);
    assert!(lands.starts_with(mirror_base(&st.mirror)), "{lands:?}");
    assert!(!lands.starts_with(st.mirror.join("_outside")), "{lands:?}");
    assert_eq!(back, Some(normalize(&util.with_extension("luau"))));
    // No dependency: the fixed four hold.
    assert_eq!(mirror_above(Some(&dir)), 4);
}

/// The file poll watches the `[build] in` of every project the root's
/// sources import into, so a dependency saved outside the editor
/// reaches the next tick.
#[test]
fn the_poll_watches_the_folder_of_a_dependency() {
    let dir = alias_root(
        "poll-dependency",
        &[
            ("app/alloy.toml", "[build]\nin = \"src\"\nout = \"build\"\n"),
            (
                "app/src/main.aly",
                "import { double } from \"../../shared/src/util\"\nprint(double(2))\n",
            ),
            (
                "shared/alloy.toml",
                "[build]\nin = \"src\"\nout = \"build\"\n",
            ),
            (
                "shared/src/util.aly",
                "export function double(n: number): number\n    return n * 2\nend\n",
            ),
        ],
    );
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(std::io::sink()),
        Vec::new(),
        None,
    );
    server.state.lock().expect("state").root = Some(dir.join("app"));

    let roots = server.poll_roots();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        roots.contains(&normalize(&dir.join("shared/src"))),
        "{roots:?}"
    );
}

/// A dependency's declarations join the workspace symbols, under the
/// path they live at, so the reader sees where the name lives.
#[test]
fn a_dependency_declaration_is_a_workspace_symbol_under_its_path() {
    let dir = std::env::temp_dir().join(format!("alloy-dep-symbol-{}", std::process::id()));
    let app = dir.join("ws/app");
    let util = dir.join("ws/shared/src/util.aly");
    let util_uri = format!("file://{}", util.display());
    let mut st = State {
        root: Some(app.clone()),
        mirror: mirror_dir(Some(&app), mirror_above(Some(&app))),
        ..State::default()
    };
    let options = EmitOptions {
        file_name: util.to_string_lossy().into_owned(),
        ..EmitOptions::default()
    };
    st.docs.insert(
        util_uri.clone(),
        Doc::new(
            "export function double(n: number): number\n    return n * 2\nend\n".to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        ),
    );
    let mut out = Vec::new();
    crate::proxy::outline::source_symbols(&st, Some("double"), &mut out);

    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0]["name"], json!("double"));
    assert_eq!(out[0]["containerName"], json!("../shared/src/util.aly"));
    assert_eq!(out[0]["location"]["uri"], json!(util_uri));
}

/// `new Vec2 { |` for a struct a dependency project declares: the
/// fields come from the dependency's source, not the global list.
#[test]
fn a_struct_of_a_dependency_completes_its_fields() {
    let dir = std::env::temp_dir().join(format!("alloy-dep-fields-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let app = dir.join("ws/app");
    let shared = dir.join("ws/shared");
    std::fs::create_dir_all(app.join("src")).expect("app");
    std::fs::create_dir_all(shared.join("src")).expect("shared");
    let toml = "[build]\nin = \"src\"\nout = \"build\"\n";
    std::fs::write(app.join("alloy.toml"), toml).expect("toml");
    std::fs::write(shared.join("alloy.toml"), toml).expect("toml");
    std::fs::write(
        shared.join("src/util.aly"),
        "export struct Vec2 as\n    x: number,\n    y: number\nend\n",
    )
    .expect("util");
    let main = app.join("src/main.aly");
    let src = "import { Vec2 } from \"../../shared/src/util\"\n\nlocal v = new Vec2 { \n";
    std::fs::write(&main, src).expect("main");

    let uri = format!("file://{}", main.display());
    let mut st = State {
        root: Some(app.clone()),
        mirror: mirror_dir(Some(&app), mirror_above(Some(&app))),
        ..State::default()
    };
    let options = EmitOptions {
        file_name: main.to_string_lossy().into_owned(),
        ..EmitOptions::default()
    };
    st.docs.insert(
        uri.clone(),
        Doc::new(
            src.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        ),
    );
    let names: Vec<String> = st
        .struct_fields(&uri, "Vec2", false)
        .into_iter()
        .map(|f| f.name)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(names, ["x", "y"]);
}

/// A method of an imported enum: the emit writes the header
/// `function Status.describe(self)` as generated text anchored at the
/// name, so the child's range on it came back one character wide.
/// The source spells the name at the anchor, and the range keeps it.
#[test]
pub(crate) fn a_generated_name_keeps_its_width() {
    let src = concat!(
        "export enum Status as\n",
        "    Idle\n",
        "    Busy\n",
        "end\n",
        "\n",
        "impl Status as\n",
        "    function describe(self): string\n",
        "        return \"x\"\n",
        "    end\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let doc = st.docs.get(uri).unwrap();
    let line = doc.shadow.lines().nth(6).expect("the header");
    let at = line.find("describe").expect("the name") as u32;
    let mut range = range_value((6, at), (6, at + 8));
    super::super::capabilities::map_range_value(&mut range, doc);

    assert_eq!(range, range_value((6, 13), (6, 21)), "{line}");
}

/// A mirror another root left behind a week ago goes at initialize; a
/// fresh one and the session's own stay.
#[test]
pub(crate) fn stale_mirrors_of_other_roots_are_purged() {
    let base = std::env::temp_dir().join(format!("alloy-lsp-purge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let own = base.join("own").join("root");
    let old = base.join("old");
    let fresh = base.join("fresh");
    std::fs::create_dir_all(&own).expect("own");
    std::fs::create_dir_all(&old).expect("old");
    std::fs::create_dir_all(&fresh).expect("fresh");
    let week_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 60 * 60);
    std::fs::File::open(&old)
        .expect("old dir")
        .set_modified(week_ago)
        .expect("mtime");

    purge_stale_mirrors(&own);

    assert!(own.exists());
    assert!(fresh.exists());
    assert!(!old.exists());
    let _ = std::fs::remove_dir_all(&base);
}
