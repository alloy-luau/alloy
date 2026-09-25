use super::super::*;
use super::support::one_file;

#[test]
pub(crate) fn the_quoted_path_of_an_import_line() {
    let src = "import * as M from \"./inventory\"\nlocal x = 1\n";
    assert_eq!(quoted_span_on_line(src, 0), Some(((0, 19), (0, 32))));
    assert_eq!(quoted_span_on_line(src, 1), None);

    // The emit writes the `require` of a list over several lines on the
    // line of `import`; the path sits on the last line.
    let src = "import {\n    a, -- \"x\"\n} from \"./inventory\"\n";
    assert_eq!(quoted_span_on_line(src, 0), Some(((2, 7), (2, 20))));
}
#[test]
pub(crate) fn a_private_view_in_a_message_reads_as_the_struct() {
    let st = State::default();
    let doc = Doc::new(
        "struct Swinger as\n    private last: number\nend\n".to_string(),
        1,
        &EmitOptions::default(),
        &alloy::luaux::Config::default(),
        None,
    );
    let mut d = json!({
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
        "message": "TypeError: Type 'Swinger & Swinger__private & { last: number, scope: Scope }' does not have key 'self'",
    });
    friendly_message(&mut d, &doc, &st);
    assert_eq!(
        d["message"],
        "TypeError: Type 'Swinger' does not have key 'self'"
    );
}
#[test]
pub(crate) fn unused_lints_name_the_variable() {
    assert_eq!(
        unused_name("LocalUnused: Variable 'RunService' is never used; prefix with '_' to silence"),
        Some("RunService")
    );
    assert_eq!(
        unused_name("FunctionUnused: Function 'f' is never used"),
        Some("f")
    );
    assert_eq!(unused_name("DeprecatedApi: Member 'x'"), None);
}
#[test]
pub(crate) fn intrinsic_arguments_count_as_uses() {
    let src = "local RunService = game
local f = $nameof(RunService.Heartbeat)
";
    assert!(consumed_by_intrinsic(src, "RunService"));
    assert!(!consumed_by_intrinsic(src, "game"));
    assert!(consumed_by_intrinsic("$stringify(a + b)", "b"));
    assert!(!consumed_by_intrinsic("$stringify(ab)", "b"));
}
// --- the comment directives ------------------------------------------------

/// One child diagnostic, as the checker sends it.
pub(crate) fn child(line: u32, message: &str, severity: u64) -> Value {
    json!({
        "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 4 } },
        "severity": severity,
        "message": message,
    })
}
#[test]
pub(crate) fn a_set_the_editor_already_holds_is_not_published_again() {
    // Opening 60 files published four sets each: the workspace pass,
    // the editor's own open, and the child's answer to both.
    let mut st = State::default();
    let uri = "file:///t.aly";
    let empty: Vec<Value> = Vec::new();
    let one = vec![child(1, "TypeError: Unknown global", 1)];

    assert!(!st.already_published(uri, &empty));
    assert!(st.already_published(uri, &empty));
    assert!(!st.already_published(uri, &one));
    assert!(st.already_published(uri, &one));
    // Another file answers for itself.
    assert!(!st.already_published("file:///other.aly", &one));
}
#[test]
pub(crate) fn a_private_method_call_reads_as_private() {
    // The checker reads a call of a private method from another file
    // as a member the struct has not got; `alloy flux` prints the
    // privacy sentence, and the editor now prints the same one.
    let dir = std::env::temp_dir().join(format!("alloy-private-method-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .expect("alloy.toml");
    std::fs::write(
        dir.join("src/lib.aly"),
        "export struct Counter as\n    read name: string\nend\n\nimpl Counter as\n    private function reset(self)\n    end\nend\n",
    )
    .expect("lib.aly");
    let source =
        "import { Counter } from \"./lib\"\n\nfunction use(c: Counter)\n    c.reset()\nend\n";
    std::fs::write(dir.join("src/other.aly"), source).expect("other.aly");

    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        ..State::default()
    };
    let uri = path_to_uri(&dir.join("src/other.aly"));
    let (options, jsx) = st.options_for(&uri);
    st.docs.insert(
        uri.clone(),
        Doc::new(source.to_string(), 1, &options, &jsx, None),
    );

    let doc = st.docs.get(&uri).expect("doc");
    let mut d = json!({
        "range": { "start": { "line": 3, "character": 6 }, "end": { "line": 3, "character": 11 } },
        "severity": 1,
        "message": "TypeError: Type 'Counter' does not have key 'reset'",
    });
    friendly_message(&mut d, doc, &st);

    assert_eq!(
        d["message"],
        "StructError: `reset` is private to `Counter`; only its impl reaches it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
pub(crate) fn a_private_read_keeps_the_error_beside_the_lint() {
    // `alloy flux` reports the read as an error and lints it, so the
    // editor shows both. A report that names a member the struct does
    // have answers to the lint alone, after the wording pass writes it.
    let source = concat!(
        "struct Box as\n",
        "    value: number,\n",
        "    private secret: number,\n",
        "end\n",
        "\n",
        "impl Box as\n",
        "    private function hide(self)\n",
        "        self.secret = 0\n",
        "    end\n",
        "end\n",
        "\n",
        "local b = new Box { value = 1, secret = 0 }\n",
        "print(b.secret)\n",
        "b:hide()\n",
    );
    let (st, uri) = one_file(source);
    let doc = st.docs.get(uri).unwrap();
    let config = alloy::config::LintConfig::default();
    let lints: Vec<usize> = doc
        .output
        .as_ref()
        .map(|o| {
            o.lints
                .iter()
                .filter(|l| l.name == "private_access")
                .map(|l| alloy::directives::line_of(&doc.source, l.start as usize))
                .collect()
        })
        .unwrap_or_default();

    assert_eq!(lints, vec![12, 13], "the fixture must lint both accesses");
    assert!(keep_diagnostic(
        &child(12, "TypeError: Type 'Box' does not have key 'secret'", 1),
        doc,
        None,
        &config
    ));
    assert!(!keep_diagnostic(
        &child(13, "TypeError: Key 'hide' not found in table 'Box'", 1),
        doc,
        None,
        &config
    ));
    assert!(answers_to_the_private_lint(
        &child(13, "StructError: `Box` has no method `hide`", 1),
        doc,
        &config
    ));
    assert!(!answers_to_the_private_lint(
        &child(
            12,
            "StructError: `secret` is private to `Box`; only its impl reaches it",
            1
        ),
        doc,
        &config
    ));
}
#[test]
pub(crate) fn a_region_silences_the_checkers_reports_between_its_pair() {
    let source = concat!(
        "--@alloy-ignore-start\n",
        "local a = undefined_one\n",
        "--@alloy-ignore-end\n",
        "local b = undefined_two\n",
    );
    let (st, uri) = one_file(source);
    let doc = st.docs.get(uri).unwrap();
    let config = alloy::config::LintConfig::default();

    assert!(!keep_diagnostic(
        &child(1, "TypeError: Unknown global", 1),
        doc,
        None,
        &config
    ));
    assert!(keep_diagnostic(
        &child(3, "TypeError: Unknown global", 1),
        doc,
        None,
        &config
    ));
}
#[test]
pub(crate) fn a_named_region_silences_that_kind_alone() {
    let source = concat!(
        "--@alloy-ignore-start LocalUnused\n",
        "local a = 1\n",
        "local b = 2\n",
        "--@alloy-ignore-end\n",
    );
    let (st, uri) = one_file(source);
    let doc = st.docs.get(uri).unwrap();
    let config = alloy::config::LintConfig::default();

    assert!(!keep_diagnostic(
        &child(1, "LocalUnused: Variable 'a' is never used", 2),
        doc,
        None,
        &config
    ));
    assert!(keep_diagnostic(
        &child(2, "LocalShadow: Variable 'b' shadows", 2),
        doc,
        None,
        &config
    ));
}
#[test]
pub(crate) fn a_region_reads_the_kind_the_author_sees() {
    // The child says `Unknown require`; the editor shows
    // `UnknownModule`, and the region names that.
    let source = concat!(
        "--@alloy-ignore-start UnknownModule\n",
        "local a = require(\"./gone\")\n",
        "--@alloy-ignore-end\n",
    );
    let (st, uri) = one_file(source);
    let doc = st.docs.get(uri).unwrap();
    let config = alloy::config::LintConfig::default();

    assert!(!keep_diagnostic(
        &child(1, "TypeError: Unknown require: \"./gone\"", 1),
        doc,
        None,
        &config
    ));
}
#[test]
pub(crate) fn an_unmet_expectation_carries_its_reason() {
    // The covered line must come clean, so it holds no lint of
    // its own: an unused local would meet the expectation.
    let source = "--@alloy-expect-error a negative count is refused\nconst a = 1\nprint(a)\n";
    let (st, uri) = one_file(source);
    let doc = st.docs.get(uri).unwrap();
    let items = unmet_expectations(doc, &[]);
    assert_eq!(items.len(), 1);
    let message = items[0]["message"].as_str().unwrap_or_default();
    assert!(message.contains("a negative count is refused"), "{message}");
    // The report sits on the directive's own line.
    assert_eq!(items[0]["range"]["start"]["line"], 0);

    // A directive over a line the checker reported on says nothing.
    assert!(unmet_expectations(doc, &[child(1, "TypeError: no", 1)]).is_empty());
}
#[test]
pub(crate) fn a_lint_directive_re_levels_one_document() {
    let source = "--@alloy-lint raw_require=deny\nlocal m = require(\"./m\")\n";
    let (st, uri) = one_file(source);
    let items = st.alloy_diagnostics(uri);
    let raw = items
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("raw_require"))
        })
        .expect("the lint reports");
    // Denied, so the editor shows it as an error.
    assert_eq!(raw["severity"], 1);

    let silent = "--@alloy-lint raw_require=allow\nlocal m = require(\"./m\")\n";
    let (st, uri) = one_file(silent);
    assert!(
        !st.alloy_diagnostics(uri).iter().any(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("raw_require"))
        }),
        "an allowed lint still reports"
    );
}
/// An alias nothing reads draws the `unused_variable` lint, whose fix
/// drops the ` as name`. The fix replaces with nothing, so the title
/// read `Rewrite as ``` and said what goes away nowhere.
#[test]
pub(crate) fn a_fix_that_deletes_names_what_it_removes() {
    let source = concat!(
        "local function f(p: number): number\n",
        "  match p as state with\n",
        "    case 0 then\n",
        "      return 1\n",
        "    default\n",
        "      return 0\n",
        "  end\n",
        "end\n",
    );
    let (st, uri) = one_file(source);
    let action = st
        .lint_actions(uri, ((1, 0), (1, 20)))
        .into_iter()
        .find(|a| {
            a["title"]
                .as_str()
                .is_some_and(|t| t.contains("unused_variable"))
        })
        .expect("the unused alias offers a fix");

    assert_eq!(
        action["title"],
        json!("Remove `as state` (unused_variable)")
    );

    // The edit takes the ` as state` out, and leaves `match p with`.
    let edit = &action["edit"]["changes"][uri][0];
    assert_eq!(edit["newText"], json!(""));

    let line = source.lines().nth(1).expect("the head");
    let from = edit["range"]["start"]["character"].as_u64().unwrap() as usize;
    let to = edit["range"]["end"]["character"].as_u64().unwrap() as usize;

    assert_eq!(edit["range"]["start"]["line"], json!(1));
    assert_eq!(edit["range"]["end"]["line"], json!(1));
    assert_eq!(
        format!("{}{}", &line[..from], &line[to..]),
        "  match p with"
    );
}
/// The fix-all takes `prefer_const` first, as `alloy fmt` does: a local
/// it makes a `const` takes the const style, not the variable style.
#[test]
pub(crate) fn the_fix_all_names_a_new_const_in_the_const_style() {
    let config = alloy::config::Config::parse(
        "[lint.naming]\nvariable = \"camelCase\"\nconst = \"SCREAMING_SNAKE_CASE\"\n",
        std::path::Path::new("alloy.toml"),
    )
    .unwrap();
    let source = "local max_hp = 100\nprint(max_hp)\n";
    let uri = "file:///t.aly";
    let options = EmitOptions {
        naming: config.lint.naming.clone(),
        ..EmitOptions::default()
    };
    let mut st = State {
        root: Some(PathBuf::from("/")),
        mirror: PathBuf::from("/m"),
        ..State::default()
    };
    st.docs.insert(
        uri.to_string(),
        Doc::new(
            source.to_string(),
            1,
            &options,
            &alloy::luaux::Config::default(),
            None,
        ),
    );
    st.configs.borrow_mut().insert(
        PathBuf::from("/"),
        Some(std::sync::Arc::new((PathBuf::from("/alloy.toml"), config))),
    );
    let actions = st.lint_actions(uri, ((0, 0), (2, 0)));
    let all = actions
        .iter()
        .find(|a| a["kind"] == "source.fixAll")
        .expect("the fix-all");
    let texts: Vec<&str> = all["edit"]["changes"][uri]
        .as_array()
        .expect("the edits")
        .iter()
        .map(|e| e["newText"].as_str().unwrap())
        .collect();

    assert_eq!(texts, vec!["const", "MAX_HP", "MAX_HP"]);
}

/// A rename writes the new name at each read in one quick fix, and the
/// fix-all takes every edit of it beside the other rewrites.
#[test]
pub(crate) fn a_rename_fix_edits_every_read() {
    let source = "--@alloy-lint naming=warn\nconst playerCount = 1\nprint(playerCount)\nprint(p and p.Name)\n";
    let (st, uri) = one_file(source);
    let actions = st.lint_actions(uri, ((0, 0), (4, 0)));
    let rename = actions
        .iter()
        .find(|a| {
            a["title"]
                .as_str()
                .is_some_and(|t| t.contains("naming_convention"))
        })
        .expect("the name offers a rename");

    assert_eq!(
        rename["title"],
        json!("Rewrite as `player_count` (naming_convention)")
    );
    let lines: Vec<u64> = rename["edit"]["changes"][uri]
        .as_array()
        .expect("the edits")
        .iter()
        .map(|e| e["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![1, 2]);

    let all = actions
        .iter()
        .find(|a| a["kind"] == "source.fixAll")
        .expect("the fix-all");
    assert_eq!(
        all["edit"]["changes"][uri].as_array().map(Vec::len),
        Some(3)
    );
}
#[test]
pub(crate) fn preserve_keeps_the_quick_fix_off_a_line() {
    let plain = "local n = p and p.Name\n";
    let (st, uri) = one_file(plain);
    let range = ((0, 0), (1, 0));
    assert!(
        st.lint_actions(uri, range)
            .iter()
            .any(|a| a["kind"] == "quickfix"),
        "the rewrite is offered"
    );

    let kept = "--@alloy-preserve the two names read better apart\nlocal n = p and p.Name\n";
    let (st, uri) = one_file(kept);
    let range = ((0, 0), (2, 0));
    assert!(
        st.lint_actions(uri, range).is_empty(),
        "a preserved line still offers a rewrite"
    );

    // The lint still reports, and says the line is preserved.
    let message = st
        .alloy_diagnostics(uri)
        .into_iter()
        .find_map(|d| {
            d["message"]
                .as_str()
                .filter(|m| m.starts_with("manual_safe_access"))
                .map(str::to_string)
        })
        .expect("the lint reports");
    assert!(message.contains("--@alloy-preserve"), "{message}");
}
#[test]
pub(crate) fn the_directive_list_holds_every_directive() {
    let source = "--@\n";
    let (st, uri) = one_file(source);
    let items = st.directive_completions(uri, 0, 3);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

    for name in alloy::directives::NAMES {
        assert!(labels.contains(name), "`{name}` is not offered");
    }

    // `--@` asks for Alloy's own; the Luau hot comments stay out.
    assert!(!labels.iter().any(|l| l.starts_with("--!")));
}
#[test]
pub(crate) fn a_range_on_whitespace_moves_to_the_next_token() {
    let source = "impl Shape for Alias as\n    function area(self): number\n";
    let mut items = vec![json!({
        "range": { "start": { "line": 1, "character": 12 }, "end": { "line": 1, "character": 13 } },
        "message": "x",
    })];
    snap_ranges(&mut items, source);
    assert_eq!(range_of(&items[0]["range"]), Some(((1, 13), (1, 17))));
}
#[test]
pub(crate) fn one_report_per_problem() {
    let one = json!({
        "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 5 } },
        "message": "expected `end`",
    });
    let wide = json!({
        "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 3, "character": 9 } },
        "message": "expected `end`",
    });
    let mut items = vec![one.clone(), one.clone(), wide];
    collapse_diagnostics(&mut items);
    assert_eq!(items, vec![one]);
}

/// `import { Ha } from "@alloy/std/collections"`: the name the module
/// does not export draws one report, not a second that calls it unused.
#[test]
fn a_missing_import_is_not_also_unused() {
    let at = |line: u32, character: u32, message: &str| {
        json!({
            "range": {
                "start": { "line": line, "character": character },
                "end": { "line": line, "character": character + 2 },
            },
            "message": message,
        })
    };
    let missing = at(0, 9, "ImportError: the std has no `Ha`");
    let unused = at(1, 9, "unused_import: `Hb` is imported and never used");
    let mut items = vec![
        missing.clone(),
        at(0, 9, "unused_import: `Ha` is imported and never used"),
        unused.clone(),
    ];
    collapse_diagnostics(&mut items);
    assert_eq!(items, vec![missing, unused]);

    // `Nope as N`: the alias stands four columns past the name.
    let aliased = at(2, 9, "ImportError: \"./x\" does not export `Nope`");
    let mut items = vec![
        json!({ "range": range_value((2, 9), (2, 13)), "message": aliased["message"] }),
        at(2, 17, "unused_import: `N` is imported and never used"),
    ];
    collapse_diagnostics(&mut items);
    assert_eq!(items.len(), 1, "{items:?}");
}

/// Two open files: a module and the file that imports it. The import
/// checks read the module from disk, so a module that gains an
/// `export default` clears the report of the importer, and the export
/// surface of the document is the signal that the importers must hear
/// the change again.
#[test]
pub(crate) fn a_module_that_gains_a_default_clears_its_importer() {
    let dir = std::env::temp_dir().join(format!("alloy-stale-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("temp dir");
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"out\"\n",
    )
    .expect("toml");

    let module = dir.join("src/m.aly");
    let main = dir.join("src/main.aly");
    let before = "export function helper(): number\n    return 2\nend\n";
    let after = "export function helper(): number\n    return 2\nend\n\nexport default function make(): number\n    return 1\nend\n";
    std::fs::write(&module, before).expect("module");
    std::fs::write(&main, "import X from \"./m\"\n\nlocal n = X\n").expect("main");

    let doc_of = |text: &str| {
        Doc::new(
            text.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        )
    };
    let mut st = State {
        root: Some(dir.clone()),
        mirror: dir.join("mirror"),
        ..State::default()
    };
    let module_uri = format!("file://{}", module.display());
    let main_uri = format!("file://{}", main.display());
    st.docs.insert(module_uri.clone(), doc_of(before));
    st.docs.insert(
        main_uri.clone(),
        doc_of("import X from \"./m\"\n\nlocal n = X\n"),
    );

    let says_no_default = |st: &State| {
        st.alloy_diagnostics(&main_uri).iter().any(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("has no default export"))
        })
    };
    assert!(says_no_default(&st));

    // The editor types the fix. The buffer holds it and the disk does
    // not, so the importer's report stands.
    let typed = doc_of(after);
    assert_ne!(
        export_surface(&st.docs[&module_uri]),
        export_surface(&typed),
        "the export surface says the importers must hear this"
    );
    st.docs.insert(module_uri.clone(), typed);
    assert!(says_no_default(&st));

    // The editor saves. Nothing is cached: the next read of the
    // importer's diagnostics is clean.
    std::fs::write(&module, after).expect("module");
    assert!(!says_no_default(&st));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The reserved alias report points at the key that declares it, in
/// each of the three files that can.
#[test]
pub(crate) fn the_key_of_a_reserved_alias_is_found_in_every_file() {
    let luaurc =
        "{\n  \"aliases\": {\n    \"pkg\": \"Packages\",\n    \"game\": \"src/shared\"\n  }\n}\n";
    assert_eq!(alias_key_line(luaurc, "game"), Some((3, 4, 10)));

    let toml = "[build]\nin = \"src\"\n\n[mount]\nshared = [\"src/shared\", \"@game/x\"]\ngame = [\"src/g\", \"@game/y\"]\n";
    assert_eq!(alias_key_line(toml, "game"), Some((5, 0, 4)));

    let config_luau = "return {\n    luau = {\n        aliases = {\n            game = \"src/shared\",\n        },\n    },\n}\n";
    assert_eq!(alias_key_line(config_luau, "game"), Some((3, 12, 16)));

    // A name that is only part of another key is no declaration.
    assert_eq!(alias_key_line("gamer = \"x\"\n", "game"), None);
    assert_eq!(alias_key_line(luaurc, "alloy"), None);
}

/// "Write the members `@service` requires": one action per attribute and
/// insertion point, with the text the language puts there.
#[test]
pub(crate) fn a_contract_draws_an_action_that_writes_its_members() {
    let src = concat!(
        "attribute service on impl as\n",
        "    requires public function Start(self)\n",
        "    requires private field state: number\n",
        "end\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\nend\n\nprint(S)\n"
    );
    let (st, uri) = one_file(src);
    let line = src[..src.find("@service").expect("the use")]
        .matches('\n')
        .count() as u32;
    let actions = st.contract_actions(uri, ((line, 0), (line, 8)));
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert_eq!(
        titles,
        [
            "Write the member `@service` requires: Start",
            "Write the member `@service` requires: state"
        ],
        "{actions:#?}"
    );

    let edit_of = |action: &Value| -> (u32, String) {
        let edit = &action["edit"]["changes"][uri][0];
        (
            edit["range"]["start"]["line"].as_u64().unwrap_or(0) as u32,
            edit["newText"].as_str().unwrap_or_default().to_string(),
        )
    };
    // The method goes in the `impl`, on the line of its `end`.
    let (at, text) = edit_of(&actions[0]);
    assert_eq!(text, "  public function Start(self)\n  end\n");
    assert_eq!(src.lines().nth(at as usize), Some("end"));

    // The field goes in the struct, which is where the language keeps it.
    let (at, text) = edit_of(&actions[1]);
    assert_eq!(text, "  private state: number\n");
    assert_eq!(src.lines().nth(at as usize), Some("end"));

    // A range that holds no attribute draws nothing.
    assert!(st.contract_actions(uri, ((0, 0), (0, 4))).is_empty());
}

/// A clause that declares a return type gets a stub that returns. An
/// empty body would leave `Not all codepaths in this function return
/// 'boolean'` where the action just wrote the member.
#[test]
pub(crate) fn a_stub_with_a_return_type_returns() {
    let src = concat!(
        "attribute service on impl as\n",
        "    requires function Stop(self): boolean\n",
        "    requires private function Tick(self, dt: number)\n",
        // The `:` that counts is the one after the parameter list. A
        // parameter's own function type carries parentheses of its own.
        "    requires function On(self, cb: (number) -> ())\n",
        "end\n\n",
        "struct S as\n    x: number\nend\n\n",
        "@service\nimpl S as\nend\n\nprint(S)\n"
    );
    let (st, uri) = one_file(src);
    let line = src[..src.find("@service").expect("the use")]
        .matches('\n')
        .count() as u32;
    let actions = st.contract_actions(uri, ((line, 0), (line, 8)));
    let text = actions[0]["edit"]["changes"][uri][0]["newText"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        text,
        concat!(
            "  function Stop(self): boolean\n",
            "    error(\"todo\")\n",
            "  end\n",
            "  private function Tick(self, dt: number)\n",
            "  end\n",
            "  function On(self, cb: (number) -> ())\n",
            "  end\n",
        ),
        "{actions:#?}"
    );
}

/// `reg.aly` beside `reg.alx` build one module, so the shadow of one
/// takes the other's place and every answer about the first is about
/// someone else's code. The file says so on its first line.
#[test]
pub(crate) fn two_sources_that_build_one_module_say_so() {
    let st = super::support::files(&[
        (
            "file:///src/reg.aly",
            "local v = 1
print(v)
",
        ),
        (
            "file:///src/reg.alx",
            "return function() end
",
        ),
        (
            "file:///src/other.aly",
            "local w = 2
print(w)
",
        ),
    ]);

    assert_eq!(
        st.twin_module("file:///src/reg.aly").as_deref(),
        Some("file:///src/reg.alx")
    );
    assert_eq!(st.twin_module("file:///src/other.aly"), None);

    let said = |uri: &str| -> Vec<String> {
        st.alloy_diagnostics(uri)
            .iter()
            .filter_map(|d| d["message"].as_str().map(str::to_string))
            .filter(|m| m.contains("both build"))
            .collect()
    };

    assert_eq!(
        said("file:///src/reg.aly"),
        ["reg.aly and reg.alx both build reg.luau; rename one"]
    );
    assert!(said("file:///src/other.aly").is_empty());

    // The first line carries it, and it covers text the reader sees.
    let item = st
        .alloy_diagnostics("file:///src/reg.aly")
        .into_iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("both build"))
        })
        .expect("the report");

    assert_eq!(item["range"]["start"], json!({ "line": 0, "character": 0 }));
    assert_eq!(item["range"]["end"], json!({ "line": 0, "character": 11 }));
    assert_eq!(item["severity"], json!(1));
}

/// The checker gave up on the line: what else it says there comes from
/// a solve it did not finish, and it stays out.
#[test]
pub(crate) fn the_checkers_limit_stands_alone_on_its_line() {
    let limit = json!({
        "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 24 } },
        "message": "TypeError: the checker reached its limit on this expression; it says nothing about the code. Name a step in a local, or annotate the result",
    });
    let partial = json!({
        "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 24 } },
        "message": "TypeError: Expected this to be 'Result<{ read _1: number }>'",
    });
    let lint = json!({
        "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 24 } },
        "message": "unused_variable: `r` is never read",
    });
    let other = json!({
        "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 3 } },
        "message": "TypeError: Expected this to be 'number'",
    });
    let mut items = vec![limit.clone(), partial, lint.clone(), other.clone()];
    collapse_diagnostics(&mut items);
    assert_eq!(items, vec![limit, lint, other]);
}

/// `import { Widget, makeWidget, HELPER }` with only `makeWidget` used:
/// the child cut the whole line, which takes the name still in use.
#[test]
pub(crate) fn a_partly_unused_import_list_loses_the_dead_names_alone() {
    let (st, uri) = one_file(
        "import { Widget, makeWidget, HELPER } from \"./mod\"\n\nlocal w = makeWidget(2)\nprint(w)\n",
    );
    let whole_line = json!({
        "title": "Remove all unused code",
        "edit": { "changes": { uri: [{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 0 } },
            "newText": "",
        }] } },
    });
    let mut actions = vec![whole_line];
    st.unused_import_actions(uri, ((0, 0), (0, 0)), &mut actions);

    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["title"], "Remove 2 unused imports");
    assert_eq!(
        actions[0]["edit"]["changes"][uri],
        json!([
            {
                "range": { "start": { "line": 0, "character": 9 }, "end": { "line": 0, "character": 17 } },
                "newText": "",
            },
            {
                "range": { "start": { "line": 0, "character": 27 }, "end": { "line": 0, "character": 35 } },
                "newText": "",
            },
        ])
    );
}

/// `import * as M, { a, b }`: the star half and the list each go on
/// their own, and the half that lives stays whole.
#[test]
pub(crate) fn a_mixed_import_cuts_the_half_that_is_dead() {
    let cut = |src: &str| {
        let (st, uri) = one_file(src);
        let mut actions = Vec::new();
        st.unused_import_actions(uri, ((0, 0), (0, 0)), &mut actions);

        assert_eq!(actions.len(), 1, "{actions:?}");
        actions[0]["edit"]["changes"][uri].clone()
    };

    // `M` alone is dead: the cut runs from the `*` to the `{`.
    assert_eq!(
        cut("import * as M, { a, b } from \"./mod\"\n\nprint(a, b)\n"),
        json!([{
            "range": { "start": { "line": 0, "character": 7 }, "end": { "line": 0, "character": 15 } },
            "newText": "",
        }])
    );
    // The list is dead and `M` lives: the cut leaves `import * as M`.
    assert_eq!(
        cut("import * as M, { a, b } from \"./mod\"\n\nprint(M)\n"),
        json!([{
            "range": { "start": { "line": 0, "character": 13 }, "end": { "line": 0, "character": 23 } },
            "newText": "",
        }])
    );
    // Every name is dead: the statement goes whole.
    assert_eq!(
        cut("import * as M, { a, b } from \"./mod\"\n\nprint(1)\n"),
        json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 0 } },
            "newText": "",
        }])
    );
    // A default binding beside a list cuts the same way.
    assert_eq!(
        cut("import D, { a } from \"./mod\"\n\nprint(a)\n"),
        json!([{
            "range": { "start": { "line": 0, "character": 7 }, "end": { "line": 0, "character": 10 } },
            "newText": "",
        }])
    );
}

/// `import type { Widget }` unused got no action at all: the child reads
/// the emit, where a type-only import writes no `require`.
#[test]
pub(crate) fn an_unused_type_import_loses_its_whole_line() {
    let (st, uri) = one_file("import type { Widget } from \"./mod\"\n\nprint(\"hello\")\n");
    let mut actions = Vec::new();
    st.unused_import_actions(uri, ((0, 0), (0, 0)), &mut actions);

    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["title"], "Remove unused import");
    assert_eq!(
        actions[0]["edit"]["changes"][uri],
        json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 0 } },
            "newText": "",
        }])
    );
}
/// A name list over several lines: a dead entry on a line of its own
/// goes with that line, and the comment of the entry above it stays. A
/// comma in that comment is no entry.
#[test]
pub(crate) fn a_dead_entry_on_its_own_line_goes_with_the_line() {
    let src = "import {\n    a, -- the a, not b\n    b,\n} from \"./mod\"\n\nprint(a)\n";
    let (st, uri) = one_file(src);
    let mut actions = Vec::new();
    st.unused_import_actions(uri, ((0, 0), (0, 0)), &mut actions);

    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(
        actions[0]["edit"]["changes"][uri],
        json!([{
            "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 3, "character": 0 } },
            "newText": "",
        }])
    );
}
/// An `import` whose keyword stands alone on its line: the statement
/// runs over three lines, and the child's cut of it takes the `import`
/// with one name and leaves the rest without a keyword.
#[test]
pub(crate) fn a_three_line_import_cuts_the_dead_name_alone() {
    let cut = |src: &str| {
        let (st, uri) = one_file(src);
        let child = json!({
            "title": "Remove all unused code",
            "edit": { "changes": { uri: [{
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 6 } },
                "newText": "",
            }] } },
        });
        let mut actions = vec![child];
        st.unused_import_actions(uri, ((0, 0), (0, 0)), &mut actions);

        assert_eq!(actions.len(), 1, "{actions:?}");
        actions[0]["edit"]["changes"][uri].clone()
    };
    const HEAD: &str = "import\n    D,\n    { a } from \"./mod8\"\n\n";

    // The default is dead: the cut leaves `import` with the list.
    assert_eq!(
        cut(&format!("{HEAD}print(a)\n")),
        json!([{
            "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 2, "character": 4 } },
            "newText": "",
        }])
    );
    // `a` is dead: the cut leaves `import` with the default.
    assert_eq!(
        cut(&format!("{HEAD}print(D)\n")),
        json!([{
            "range": { "start": { "line": 1, "character": 5 }, "end": { "line": 2, "character": 9 } },
            "newText": "",
        }])
    );
    // Both are dead: the statement goes whole.
    assert_eq!(
        cut(&format!("{HEAD}print(1)\n")),
        json!([{
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 3, "character": 0 } },
            "newText": "",
        }])
    );
}
/// A pulled report used to carry the child's items alone. The pull
/// path now reads the list the push path publishes: the compile error
/// and the lint first, then the checker's own, in that order.
#[test]
pub(crate) fn a_pull_lists_what_a_push_publishes() {
    let (st, uri) = one_file(
        "struct Pt\n    x: number\nend\nconst p = new Pt { x = 1, y = 2 }\nconst n: number = \"s\"\n",
    );
    let checker = json!({
        "message": "TypeError: Expected this to be 'number', but got 'string'",
        "range": { "start": { "line": 4, "character": 18 }, "end": { "line": 4, "character": 21 } },
        "severity": 1,
    });
    let heads: Vec<String> = st
        .full_diagnostics(uri, vec![checker])
        .iter()
        .map(|d| {
            d["message"]
                .as_str()
                .unwrap_or_default()
                .split(':')
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    assert_eq!(
        heads,
        [
            "StructError",
            "unused_variable",
            "unused_variable",
            "TypeError"
        ]
    );
}

#[test]
pub(crate) fn a_bad_character_reports_one_character_wide() {
    // Two bytes, one UTF-16 unit: the range covers `é` and stops.
    let (st, uri) = one_file("local café = 1\n");
    let items = st.alloy_diagnostics(uri);
    let range = &items[0]["range"];
    assert!(
        items[0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("unexpected character")),
        "{items:?}"
    );
    assert_eq!(range["start"], json!({ "line": 0, "character": 9 }));
    assert_eq!(range["end"], json!({ "line": 0, "character": 10 }));

    // Four bytes, two UTF-16 units: one character still.
    let (st, uri) = one_file("local x😀y = 1\n");
    let items = st.alloy_diagnostics(uri);
    let range = &items[0]["range"];
    assert_eq!(range["start"], json!({ "line": 0, "character": 7 }));
    assert_eq!(range["end"], json!({ "line": 0, "character": 9 }));
}
#[test]
pub(crate) fn the_compiler_errors_carry_their_quick_fixes() {
    let whole = ((0, 0), (99, 0));
    let title = |actions: &[Value]| {
        actions
            .first()
            .and_then(|a| a["title"].as_str())
            .unwrap_or("none")
            .to_string()
    };
    let edit = |actions: &[Value], uri: &str| actions[0]["edit"]["changes"][uri].clone();

    // A construction with no `new`: the word goes in front of the name.
    let (st, uri) =
        one_file("struct P as\n    x: number,\nend\n\nlocal p = P { x = 1 }\nprint(p.x)\n");
    let actions = st.compiler_actions(uri, whole);
    assert_eq!(title(&actions), "Add `new`");
    assert_eq!(
        edit(&actions, uri),
        json!([{
            "range": { "start": { "line": 4, "character": 10 }, "end": { "line": 4, "character": 10 } },
            "newText": "new ",
        }])
    );

    // A double negation says the type the long way.
    let (st, uri) = one_file("local n: ~~number = 1\nprint(n)\n");
    let actions = st.compiler_actions(uri, whole);
    assert_eq!(title(&actions), "Write `number`");
    assert_eq!(
        edit(&actions, uri),
        json!([{
            "range": { "start": { "line": 0, "character": 9 }, "end": { "line": 0, "character": 17 } },
            "newText": "number",
        }])
    );

    // A match with no arm for `Move` and `Quit`: one arm each, above
    // the `end`, with a hole per payload value.
    let src = "enum M as\n    Join(string)\n    Move(number, number)\n    Quit\nend\n\nlocal function show(m: M)\n    match m with\n        case M.Join(n) then print(n)\n    end\nend\n\nshow(M.Quit)\n";
    let (st, uri) = one_file(src);
    let actions = st.compiler_actions(uri, whole);
    assert_eq!(title(&actions), "Add the missing arms");
    assert_eq!(
        edit(&actions, uri),
        json!([{
            "range": { "start": { "line": 9, "character": 0 }, "end": { "line": 9, "character": 0 } },
            "newText": "        case M.Move(_, _) then\n            \n        case M.Quit then\n            \n",
        }])
    );

    // A variant one edit away from a name the enum has.
    let (st, uri) = one_file(
        "enum Color as\n    Red\n    Green\n    Blue\nend\n\nlocal c = Color.Gren\nprint(c)\n",
    );
    let actions = st.compiler_actions(uri, whole);
    assert_eq!(title(&actions), "Rename to `Green`");

    // No name stands near `Purple`, so the report gets no rewrite.
    let (st, uri) = one_file(
        "enum Color as\n    Red\n    Green\n    Blue\nend\n\nlocal c = Color.Purple\nprint(c)\n",
    );
    assert!(st.compiler_actions(uri, whole).is_empty());

    // `!=` is `~=`: the edit takes both characters.
    let (st, uri) = one_file("local a = 1\nif a != 2 then\n    print(a)\nend\n");
    let actions = st.compiler_actions(uri, whole);
    assert_eq!(title(&actions), "Write `~=`");
    assert_eq!(
        edit(&actions, uri),
        json!([{
            "range": { "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 7 } },
            "newText": "~=",
        }])
    );
}

/// The checker's report on a `.` call of a method, in the compiler's
/// words, carries the edit that writes the `:`.
#[test]
fn a_dot_call_of_a_method_takes_the_colon() {
    let (mut st, uri) = one_file("local bag = { n = 0 }\n    bag.add(3)\n");
    let d = json!({
        "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 11 } },
        "severity": 1,
        "message": "TypeError: `add` is a method; call it with `bag:add(...)`, not `bag.add(...)`",
    });
    st.child_diagnostics.insert(uri.to_string(), vec![d]);
    let actions = st.compiler_actions(uri, ((0, 0), (99, 0)));

    assert_eq!(actions[0]["title"], "Write `bag:add`");
    assert_eq!(
        actions[0]["edit"]["changes"][uri],
        json!([{
            "range": { "start": { "line": 1, "character": 7 }, "end": { "line": 1, "character": 8 } },
            "newText": ":",
        }])
    );
}

/// A missing member of a remote names the remote the source declared.
/// The report sits on the `$` of the macro around the call, so the
/// name has to come off the source line, not off the reported span.
#[test]
pub(crate) fn a_missing_remote_member_names_the_remote() {
    let src = concat!(
        "export remote Damage(target: string, amount: number) from client\n",
        "\n",
        "@test\n",
        "function fires_damage()\n",
        "    $assert_eq(#Damage.nope, 1)\n",
        "end\n",
    );
    let (st, uri) = one_file(src);
    let mut d = json!({
        "range": { "start": { "line": 4, "character": 4 }, "end": { "line": 4, "character": 5 } },
        "message": "TypeError: Key 'nope' not found in table 'Remote'",
    });
    let raw = d["message"].as_str().unwrap().to_string();
    alloy_wording(&mut d, &st.docs[uri], None, &[], &raw);

    assert_eq!(d["message"], "TypeError: remote `Damage` has no `nope`");
}

/// A member one edit away from a verb the remote has: the fold cuts
/// the surface to `Remote`, so the suggestion reads the raw report,
/// and the quick fix renames the member alone.
#[test]
pub(crate) fn a_remote_typo_keeps_its_suggestion_and_its_fix() {
    let src = concat!(
        "remote Damage(target: number, amount: number) from client\n",
        "\n",
        "function main()\n",
        "    Damage.fier(1, 2)\n",
        "end\n",
        "\n",
        "main()\n",
    );
    let (mut st, uri) = one_file(src);
    let raw = "TypeError: Key 'fier' not found in table '{ calls: RemoteCalls, fire: (number, number) -> (), instance: RemoteEvent?, on: ((Player, number, number) -> ()) -> RBXScriptConnection, spec: RemoteSpec, wait: () -> Future<Player> }'";
    let mut d = json!({
        "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 15 } },
        "severity": 1,
        "message": "TypeError: Key 'fier' not found in table 'Remote'",
    });
    alloy_wording(&mut d, &st.docs[uri], None, &[], raw);

    assert_eq!(
        d["message"],
        "TypeError: remote `Damage` has no `fier`; did you mean `fire`?"
    );

    st.child_diagnostics.insert(uri.to_string(), vec![d]);
    let actions = st.compiler_actions(uri, ((0, 0), (99, 0)));

    assert_eq!(actions[0]["title"], "Rename to `fire`");
    assert_eq!(
        actions[0]["edit"]["changes"][uri],
        json!([{
            "range": { "start": { "line": 3, "character": 11 }, "end": { "line": 3, "character": 15 } },
            "newText": "fire",
        }])
    );
}

/// A markup error stops the compile, and the stop carried no code:
/// the editor showed the message with no link to the book section.
#[test]
fn a_compile_stop_carries_its_book_code() {
    let src = "struct RowProps as\n    item: string\nend\n\nlocal function Row(props: RowProps)\n    return (\n        <Row />\n    )\nend\n";
    let st = super::support::files(&[("file:///t.alx", src)]);
    let items = st.alloy_diagnostics("file:///t.alx");
    let item = items
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("MarkupError"))
        })
        .expect("the markup error");
    assert_eq!(item["code"], "3.13");
    assert!(
        item["codeDescription"]["href"]
            .as_str()
            .is_some_and(|h| !h.is_empty())
    );
}

/// A `.config.aly` reports the keys the schema does not take, then what
/// its load says, and no export there asks for a comment.
#[test]
fn a_config_file_reports_its_keys_and_its_load() {
    let messages = |src: &str| -> Vec<String> {
        let st = super::support::files(&[("file:///p/.config.aly", src)]);

        st.full_diagnostics("file:///p/.config.aly", Vec::new())
            .iter()
            .filter_map(|d| d["message"].as_str().map(str::to_string))
            .collect()
    };

    assert_eq!(
        messages("export const build = { outt = \"dist\" }\n"),
        ["`outt` is no key of `build`; did you mean `out`?"]
    );
    assert_eq!(
        messages("local n = nil\nexport default { fmt = { indent_width = n + 1 } }\n")
            .iter()
            .map(|m| m.split(':').next().unwrap_or("").to_string())
            .collect::<Vec<_>>(),
        ["the config does not load"]
    );
    assert!(messages("export const build = { out = \"dist\" }\n").is_empty());

    // A reserved word is a bare key there, and the child reads it quoted.
    let src = "export const build = { in = \"src\" }\n";
    assert!(messages(src).is_empty());
    let st = super::support::files(&[("file:///p/.config.aly", src)]);
    let shadow = &st.docs["file:///p/.config.aly"].shadow;
    assert!(shadow.contains("[\"in\"] = \"src\""), "{shadow}");
}

/// The child computes a refactor on the lowered Luau. An edit over text
/// the lowering wrote would put that Luau into the source, so the
/// action goes; an edit over the author's own text stays.
#[test]
pub(crate) fn a_child_edit_over_generated_text_is_dropped() {
    let src = "local function describe(n: number): string\n  return match n with\n    case 1 then \"one\"\n    default \"many\"\n  end\nend\nlocal a = describe(2)\n";
    let (mut st, uri) = one_file(src);
    let shadow_uri = "file:///m/t.luau";
    st.shadows.insert(shadow_uri.to_string(), uri.to_string());
    let shadow = st.docs[uri].shadow.clone();
    let range_of_text = |text: &str| {
        let at = shadow.find(text).expect(text);

        range_value(
            position_of(&shadow, at),
            position_of(&shadow, at + text.len()),
        )
    };
    let edit =
        |range: Value| json!({ "changes": { shadow_uri: [{ "range": range, "newText": "x" }] } });
    let call = edit(range_of_text("describe(2)"));
    let lowered = edit(range_of_text("n == 1"));

    assert!(st.writes_source_only(&call), "{shadow}");
    assert!(!st.writes_source_only(&lowered), "{shadow}");
    assert!(st.keeps_child_action(&json!({ "edit": call }), uri, None));
    assert!(!st.keeps_child_action(&json!({ "edit": lowered }), uri, None));
    // Nothing to change is noise.
    assert!(!st.keeps_child_action(
        &json!({ "edit": { "changes": { shadow_uri: [] } } }),
        uri,
        None
    ));

    // A lazy refactor reads the selection: the `match` lowered, the
    // call did not.
    let lazy = json!({ "kind": "refactor.extract", "data": {} });

    assert!(!st.keeps_child_action(&lazy, uri, Some(((1, 9), (4, 5)))));
    assert!(st.keeps_child_action(&lazy, uri, Some(((6, 10), (6, 21)))));
}

/// The child reports a definitions file it cannot load on the file it
/// read: the merged copy under the temp folder that no editor shows. A
/// mistyped name in a `.d.aly` was silent. The report now lands on the
/// `.d.aly` its line came from, on that file's own line, and the popup
/// names the `.d.aly` files.
#[test]
fn a_definitions_report_lands_on_the_declaration_file() {
    use super::documents::Recorder;

    let log = Arc::new(Mutex::new(Vec::new()));
    let server = Server::new(
        Box::new(std::io::sink()),
        Box::new(Recorder(Arc::clone(&log))),
        Vec::new(),
        None,
    );
    let read = PathBuf::from("/tmp/defs/declared.d.luau");
    let g = PathBuf::from("/w/src/g.d.aly");
    let h = PathBuf::from("/w/src/h.d.aly");

    {
        let mut st = server.state.lock().expect("state");

        for (source, first_line) in [(&g, 0), (&h, 2)] {
            st.definition_sources.push((
                read.clone(),
                alloy::declarations::Segment {
                    source: source.clone(),
                    first_line,
                },
            ));
        }
    }

    server.handle_child(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": path_to_uri(&read),
            "diagnostics": [{
                "range": { "start": { "line": 3, "character": 29 }, "end": { "line": 3, "character": 34 } },
                "message": "TypeError: Unknown type 'strin'",
            }],
        },
    }));
    server.handle_child(json!({
        "jsonrpc": "2.0",
        "method": "window/showMessage",
        "params": { "type": 1, "message": format!("Failed to read definitions file {}.", read.display()) },
    }));

    let sent = String::from_utf8_lossy(&log.lock().expect("the log").clone()).into_owned();
    let published: Vec<Value> = sent
        .split("Content-Length")
        .filter_map(|m| m.find('{').map(|at| &m[at..]))
        .filter_map(|m| serde_json::from_str(m).ok())
        .filter(|m: &Value| m["method"] == "textDocument/publishDiagnostics")
        .collect();
    let on = |path: &Path| {
        published
            .iter()
            .find(|m| m["params"]["uri"] == json!(path_to_uri(path)))
            .map(|m| m["params"]["diagnostics"].clone())
    };

    assert_eq!(on(&g), Some(json!([])), "{sent}");
    assert_eq!(
        on(&h).and_then(|d| d[0]["range"]["start"]["line"].as_u64()),
        Some(1),
        "{sent}"
    );
    assert!(
        sent.contains("Failed to read definitions file /w/src/g.d.aly, /w/src/h.d.aly."),
        "{sent}"
    );
    assert!(!sent.contains("declared.d.luau"), "{sent}");
}

/// The schema takes any string as a lint name, so `unused_varible` was
/// silent, and `export default const config` drew `unused_variable`. A
/// load that failed at run time sat on the `export` line, with the whole
/// path and a traceback in it.
#[test]
fn a_config_file_names_a_wrong_lint_and_the_line_that_failed() {
    let reports = |src: &str| -> Vec<(u64, String)> {
        let st = super::support::files(&[("file:///p/.config.aly", src)]);

        st.full_diagnostics("file:///p/.config.aly", Vec::new())
            .iter()
            .map(|d| {
                (
                    d["range"]["start"]["line"].as_u64().unwrap_or(99),
                    d["message"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    };

    assert_eq!(
        reports(
            "export default const config = {\n    lint = { rules = { unused_varible = \"deny\" } },\n}\n"
        ),
        [(
            1,
            "`unused_varible` is not a lint; did you mean `unused_variable`?".to_string()
        )]
    );
    assert_eq!(
        reports("local t = nil\nexport default {\n    build = { out = t.x },\n}\n"),
        [(
            2,
            "the config does not load: attempt to index nil with 'x'".to_string()
        )]
    );
}

/// A std name the load refuses sits on its string. The message names no
/// line, and the report once covered the whole `export` line.
#[test]
fn a_config_std_name_typo_sits_on_its_string() {
    let src = "export default { std = { globals = { \"HashMap\", \"Sgnal\" } } }\n";
    let st = super::support::files(&[("file:///p/.config.aly", src)]);
    let reports = st.full_diagnostics("file:///p/.config.aly", Vec::new());
    let at = src.find("Sgnal").unwrap() as u64;

    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0]["range"],
        json!({
            "start": { "line": 0, "character": at },
            "end": { "line": 0, "character": at + 5 },
        })
    );
    assert!(
        reports[0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("`Sgnal` is no std name")),
        "{reports:?}"
    );
}

/// An `[alx.factory]` the markup compiler refuses: the build skips the
/// file and names the table, and the editor compiled it with the default
/// backend and named `React`. A fix on disk left that report in place
/// until the next keystroke. The report sits on the key in alloy.toml,
/// not on the first line of each `.alx` file.
#[test]
fn a_broken_markup_config_is_named_and_a_fix_on_disk_clears_it() {
    use super::documents::{Recorder, alias_root};

    let dir = alias_root(
        "broken-factory",
        &[
            (
                "alloy.toml",
                "[build]\nin = \"src\"\nout = \"build\"\n\n[alx.factory]\ncreate = \"create\"\n",
            ),
            (
                "src/ui.alx",
                "local function create(name: string, props: any): any\n    return props\nend\n\nreturn <Frame />\n",
            ),
        ],
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let server = Arc::new(Server::new(
        Box::new(std::io::sink()),
        Box::new(Recorder(Arc::clone(&log))),
        Vec::new(),
        None,
    ));

    {
        let mut st = server.state.lock().expect("state");
        st.root = Some(dir.clone());
        st.mirror = dir.join("mirror");
    }

    let uri = path_to_uri(&dir.join("src/ui.alx"));
    let text = std::fs::read_to_string(dir.join("src/ui.alx")).expect("source");
    server.open_doc(&uri, text, 1, true);
    server.publish(&uri);

    let sent = String::from_utf8_lossy(&log.lock().expect("the log").clone()).into_owned();
    assert!(!sent.contains("needs a backend"), "{sent}");
    assert!(!sent.contains("`React` is not in scope"), "{sent}");

    log.lock().expect("the log").clear();
    server.publish_alias_problems();

    let sent = String::from_utf8_lossy(&log.lock().expect("the log").clone()).into_owned();
    let toml = sent
        .split("Content-Length")
        .find(|m| m.contains("alloy.toml"))
        .expect("a report on alloy.toml");
    assert!(
        toml.contains("MarkupError: [alx.factory] needs a backend"),
        "{toml}"
    );
    // The report names no key the table writes, so it sits on the
    // table's header.
    assert!(
        toml.contains(r#""start":{"line":4,"character":0}"#),
        "{toml}"
    );

    log.lock().expect("the log").clear();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[alx.factory]\nbackend = \"element\"\ncreate = \"create\"\n",
    )
    .expect("the fix");
    server.handle_client(json!({
        "jsonrpc": "2.0",
        "method": "workspace/didChangeWatchedFiles",
        "params": { "changes": [{ "uri": path_to_uri(&dir.join("alloy.toml")), "type": 2 }] },
    }));

    let sent = String::from_utf8_lossy(&log.lock().expect("the log").clone()).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(sent.contains("publishDiagnostics"), "{sent}");
    assert!(!sent.contains("needs a backend"), "{sent}");
}
