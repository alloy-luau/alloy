//! The ingot host end to end, over the `shout` example of the
//! alloy-ingot crate: the transform maps, the lints carry the ingot's
//! name, the output hook edits the ship artifact, the format hook runs
//! after Anneal, and the editor hooks answer.

use std::path::{Path, PathBuf};

use alloy::config::Config;
use alloy::ingot::Ingots;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The example dir, with its binary built once per test run.
fn shout_dir() -> PathBuf {
    let dir = workspace().join("alloy-ingot/examples/shout");
    let bin = workspace().join("target/debug/examples/shout");

    if !bin.is_file() {
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "alloy-ingot", "--example", "shout"])
            .current_dir(workspace())
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the shout example builds");
    }

    dir.canonicalize().unwrap()
}

fn config(extra: &str) -> Config {
    let text = format!(
        "[ingots]\nshout = \"{}\"\n{extra}",
        shout_dir().display().to_string().replace('\\', "/")
    );

    Config::parse(&text, Path::new("alloy.toml")).unwrap()
}

fn load(extra: &str) -> Ingots {
    let ingots = Ingots::load(&std::env::temp_dir(), &config(extra));
    assert!(ingots.problems.is_empty(), "{:?}", ingots.problems);

    ingots
}

const SRC: &str = "local a = $shout(\"hi\")\n-- HELLO THERE\nlocal b = 1   \nprint(a, b)\n";

#[test]
fn the_transform_maps_and_the_hooks_run() {
    let ingots = load("");
    let options = alloy::EmitOptions::default();
    let out = alloy::compile_file("src/a.aly", SRC, &options, None, Some(&ingots)).unwrap();

    assert!(
        out.ship
            .starts_with("local a = string.upper(\"hi\") -- shouted\n"),
        "{}",
        out.ship
    );
    assert!(
        out.check.starts_with("local a = string.upper(\"hi\")\n"),
        "the check artifact keeps its layout: {}",
        out.check
    );
    assert_eq!(out.check.lines().count(), SRC.lines().count());
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

    // `$shout` (bytes 10..16) became `string.upper` in the middle text;
    // the map returns to the source.
    let dollar = out.check.find("string.upper").unwrap() as u32;
    assert_eq!(out.map.to_source(dollar), 10);
    assert!(out.map.is_generated(dollar + 3));
    let paren = out.check.find("(\"hi\")").unwrap() as u32;
    assert_eq!(
        out.map.to_source(paren),
        16,
        "the `(` after the sigil is the author's"
    );
    assert_eq!(out.map.to_output(16), Some(paren));
    assert_eq!(
        out.map.to_output(10),
        None,
        "a replaced byte has no output position"
    );

    let loud: Vec<_> = out
        .lints
        .iter()
        .filter(|l| l.name == "shout/loud_comment")
        .collect();
    assert_eq!(loud.len(), 1, "{:?}", out.lints);
    assert_eq!(loud[0].start, SRC.find("-- HELLO").unwrap() as u32);
    let fix = loud[0].fix.as_ref().unwrap();
    assert_eq!(fix.replacement, " hello there");
    assert_eq!(alloy::lint::group_name("shout/loud_comment"), "shout");
    assert_eq!(
        alloy::lint::level_of(&Default::default(), "shout/loud_comment"),
        alloy::lint::Level::Warn
    );
}

#[test]
fn options_and_lint_levels_reach_the_ingot() {
    let ingots = load("[ingot.shout]\nword = \"yell\"\n\n[lint]\nallow = [\"shout\"]\n");
    let src = "local a = $yell(\"x\")\n-- LOUD ONE\n";
    let out = alloy::compile_file("a.aly", src, &Default::default(), None, Some(&ingots)).unwrap();

    assert!(out.ship.contains("string.upper(\"x\")"), "{}", out.ship);
    assert!(
        out.lints.iter().all(|l| l.name != "shout/loud_comment"),
        "an allowed lint is not run: {:?}",
        out.lints
    );

    let bad = Ingots::load(
        &std::env::temp_dir(),
        &config("[ingot.shout]\nvolume = 11\n"),
    );
    assert_eq!(bad.problems.len(), 1);
    assert!(
        bad.problems[0].message.contains("`volume`"),
        "{}",
        bad.problems[0]
    );
}

#[test]
fn the_format_hook_runs_after_anneal_and_the_editor_hooks_answer() {
    let ingots = load("");
    let (text, problems) = ingots.format("a.aly", "local b = 1   \nprint(b)  \n");
    assert!(problems.is_empty());
    assert_eq!(text, "local b = 1\nprint(b)\n");

    let hover = ingots.hover("a.aly", SRC, 12).expect("a hover on $shout");
    assert!(hover["contents"].as_str().unwrap().contains("string.upper"));
    assert_eq!(hover["span"], serde_json::json!([10, 16]));
    assert!(
        ingots.hover("a.aly", SRC, 2).is_none(),
        "no hover on `local`"
    );

    let (items, _) = ingots.complete("a.aly", "local x = $", 11, Some("$"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["label"], "$shout");
    assert_eq!(items[0]["snippet"], true);

    let actions = ingots.actions("a.aly", SRC, (6, 7), &[]);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["title"], "Shout `a`");
    assert_eq!(
        actions[0]["edits"][0],
        serde_json::json!([6, 7, "$shout(a)"])
    );
}

/// A prop such as `ClassName` is on no Roblox class and in no component
/// type, so the editor takes the name and the words for it from the
/// ingot that reads it.
#[test]
fn an_ingot_declares_the_props_it_reads_on_a_tag() {
    let dir = std::env::temp_dir().join("alloy-ingot-props-test");
    std::fs::create_dir_all(&dir).unwrap();
    let binary = shout_dir()
        .join("../../../target/debug/examples/shout")
        .display()
        .to_string()
        .replace('\\', "/");
    std::fs::write(
        dir.join("ingot.toml"),
        format!(
            "name = \"styler\"\napi = 1\nbinary = \"{binary}\"\nkinds = [\"alx\"]\nhooks = [\"complete\"]\n\n[props]\nClassName = {{ doc = \"the utility list\", insert = \"ClassName=\\\"$1\\\"\" }}\n"
        ),
    )
    .unwrap();
    let text = format!(
        "[ingots]\nstyler = \"{}\"\n",
        dir.display().to_string().replace('\\', "/")
    );
    let config = Config::parse(&text, Path::new("alloy.toml")).unwrap();
    let ingots = Ingots::load(&std::env::temp_dir(), &config);
    assert!(ingots.problems.is_empty(), "{:?}", ingots.problems);

    assert_eq!(
        ingots.props("src/a.alx"),
        vec![(
            "ClassName",
            "the utility list",
            "styler",
            "ClassName=\"$1\""
        )]
    );
    // The ingot wants `.alx` only, so no other file lists the prop.
    assert!(ingots.props("src/a.aly").is_empty());
}

#[test]
fn a_missing_ingot_is_a_problem_not_a_panic() {
    let c = Config::parse("[ingots]\nnone = \"nowhere\"\n", Path::new("alloy.toml")).unwrap();
    let ingots = Ingots::load(&std::env::temp_dir(), &c);
    assert!(ingots.is_empty());
    assert_eq!(ingots.problems.len(), 1);
    assert!(
        ingots.problems[0].message.contains("cannot read"),
        "{}",
        ingots.problems[0]
    );

    // A build with a broken ingot reports it against alloy.toml and
    // still compiles the sources.
    let dir = std::env::temp_dir().join(format!("alloy-ingot-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.aly"), "local x = 1\nprint(x)\n").unwrap();
    let report = alloy::build::run_project(&dir, &c).unwrap();
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].1.contains("ingot `none`"));
    assert_eq!(report.written.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
