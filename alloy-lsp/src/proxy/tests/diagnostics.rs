use super::super::*;
use super::support::one_file;

#[test]
pub(crate) fn the_quoted_path_of_an_import_line() {
    let src = "import * as M from \"./inventory\"\nlocal x = 1\n";
    assert_eq!(quoted_span_on_line(src, 0), Some((19, 32)));
    assert_eq!(quoted_span_on_line(src, 1), None);
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
    let source = "--@alloy-expect-error a negative count is refused\nlocal a = 1\nprint(a)\n";
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
pub(crate) fn the_key_a_message_says_is_missing() {
    assert_eq!(
        missing_key("TypeError: Type 'Wallet' does not have key 'balance'"),
        Some("balance")
    );
    assert_eq!(missing_key("TypeError: something else"), None);
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
    assert_eq!(alias_key_line(luaurc, "game"), Some((3, 10)));

    let toml = "[build]\nin = \"src\"\n\n[mount]\nshared = [\"src/shared\", \"@game/x\"]\ngame = [\"src/g\", \"@game/y\"]\n";
    assert_eq!(alias_key_line(toml, "game"), Some((5, 4)));

    let config_luau = "return {\n    luau = {\n        aliases = {\n            game = \"src/shared\",\n        },\n    },\n}\n";
    assert_eq!(alias_key_line(config_luau, "game"), Some((3, 16)));

    // A name that is only part of another key is no declaration.
    assert_eq!(alias_key_line("gamer = \"x\"\n", "game"), None);
    assert_eq!(alias_key_line(luaurc, "alloy"), None);
}
