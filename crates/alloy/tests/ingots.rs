//! The ingot host end to end, over the `shout` example of the
//! alloy-ingot crate: the transform maps, the lints carry the ingot's
//! name, the output hook edits the ship artifact, the format hook runs
//! after Anneal, and the editor hooks answer.

use std::path::{Path, PathBuf};

use alloy::config::Config;
use alloy::ingot::Ingots;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The example dir, with its binary built once per test run.
fn shout_dir() -> PathBuf {
    let dir = workspace().join("crates/alloy-ingot/examples/shout");
    let workspace_target = workspace().join("target");
    let bin = workspace_target.join("debug/examples/shout");

    if !bin.is_file() {
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "alloy-ingot", "--example", "shout"])
            // The manifest names the workspace `target`, whatever the
            // caller's CARGO_TARGET_DIR says.
            .env("CARGO_TARGET_DIR", workspace_target)
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

/// A compile that fails points into the author's text. The transform
/// makes the line longer, so an offset into its text is past the tag.
#[test]
fn a_failed_compile_points_into_the_source() {
    let ingots = load("");
    let src = "local a = $shout(\"hi\")\nlocal e = <Frame></Framex>\n";
    let error = alloy::compile_file("a.alx", src, &Default::default(), None, Some(&ingots))
        .err()
        .expect("the tags do not match");

    assert_eq!(error.offset, src.find("</Framex").unwrap(), "{error:?}");
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

    let items = ingots.complete("a.aly", "local x = $", 11, Some("$")).items;
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
        .join("../../../../target/debug/examples/shout")
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
    // The manifest path reads as the toml wrote it, under the root.
    assert!(
        ingots.problems[0]
            .to_string()
            .starts_with("ingot `none`: cannot read nowhere/ingot.toml: "),
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
    // The failure names the root's alloy.toml, not one under `src`.
    assert_eq!(report.failures[0].0, dir.join("alloy.toml"));
    assert!(report.failures[0].1.contains("ingot `none`"));
    assert_eq!(report.written.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build fetches nothing: an ingot from a release that is not in the
/// store is a report that names the command which installs it.
#[test]
fn a_missing_release_ingot_names_the_install_command() {
    let root = std::env::temp_dir().join(format!("alloy-ingot-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    for (text, says) in [
        (
            "logger = { repo = \"someone/logger-ingot\" }",
            "latest release",
        ),
        (
            "logger = { repo = \"someone/logger-ingot\", version = \"1.2.0\" }",
            "1.2.0",
        ),
    ] {
        let config =
            Config::parse(&format!("[ingots]\n{text}\n"), Path::new("alloy.toml")).unwrap();
        let ingots = Ingots::load(&root, &config);
        assert!(ingots.is_empty(), "nothing loads and nothing is fetched");

        let problem = ingots.problems[0].to_string();
        assert!(problem.contains("ingot `logger`"), "{problem}");
        assert!(problem.contains("alloy ingot install logger"), "{problem}");
        assert!(problem.contains(says), "{problem}");
        assert!(
            !alloy::ingot::fetch::store_root(&root).exists(),
            "a build writes nothing to the store"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// A scriptable ingot for the host's failure paths: it answers each
/// hook from its options, can crash or print on stdout, and its hover
/// names the ingots its init listed.
#[cfg(unix)]
const PROBE: &str = r#"#!/usr/bin/env python3
import sys, json, struct
inp, out = sys.stdin.buffer, sys.stdout.buffer
init = {}
def wr(v):
    b = json.dumps(v).encode()
    out.write(struct.pack("<I", len(b)) + b)
    out.flush()
while True:
    h = inp.read(4)
    if len(h) < 4:
        break
    r = json.loads(inp.read(struct.unpack("<I", h)[0]))
    op, o = r["op"], init.get("options", {})
    if op == "init":
        init = r
        wr({"ok": True})
    elif op == o.get("crash_on") and "crash" in r["source"]:
        sys.exit(3)
    elif op == o.get("print_on"):
        out.write(b"debug: a line\n")
        out.flush()
    elif op == "transform":
        wr({"ok": True, "edits": json.loads(o["edits"])})
    elif op == "lint":
        wr({"ok": True, "findings": json.loads(o["findings"])})
    elif op == "hover":
        wr({"ok": True, "hover": {"contents": json.dumps(init["ingots"])}})
    elif op == "complete":
        wr({"ok": True, "items": [], "merge": True, "class": "TextLabel"})
    else:
        wr({"ok": True})
"#;

/// Writes the probe ingot `name` and loads it with `[ingots]` and
/// `[ingot.<name>]` lines of its own.
#[cfg(unix)]
fn probe(name: &str, source: &str, options: &str) -> Ingots {
    let dir = probe_dir(name);
    let text = format!(
        "[ingots]\n{name} = {}\n\n[ingot.{name}]\n{options}\n",
        source.replace("DIR", &format!("{:?}", dir.display().to_string()))
    );
    let config = Config::parse(&text, Path::new("alloy.toml")).unwrap();

    Ingots::load(&std::env::temp_dir(), &config)
}

/// The folder of the probe ingot `name`, written fresh.
#[cfg(unix)]
fn probe_dir(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("alloy-probe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("probe.py"), PROBE).unwrap();
    std::fs::set_permissions(dir.join("probe.py"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join("ingot.toml"),
        format!(
            "name = \"{name}\"\napi = 1\nbinary = \"probe.py\"\nhooks = [\"transform\", \"lint\", \"hover\", \"complete\"]\n\n[options]\nedits = \"[]\"\nfindings = \"[]\"\ncrash_on = \"\"\nprint_on = \"\"\n\n[lints.probe_lint]\ndefault = \"warn\"\nsummary = \"a probe lint\"\n"
        ),
    )
    .unwrap();

    dir
}

/// An ingot's offsets may fall inside a character. The reports move to
/// the character's start; they panicked the CLI and the editor.
#[cfg(unix)]
#[test]
fn a_span_inside_a_character_does_not_panic() {
    let src = "local t = \"\u{e9}\"\nprint(t)\n";
    let inside = src.find('\u{e9}').unwrap() + 1;
    let ingots = probe(
        "splitter",
        "DIR",
        &format!(
            "edits = '[[{inside}, {inside}, \"x\"]]'\nfindings = '[{{\"lint\": \"probe_lint\", \"span\": [{inside}, {}]}}]'",
            inside + 1
        ),
    );
    assert!(ingots.problems.is_empty(), "{:?}", ingots.problems);
    let out = alloy::compile_file("a.aly", src, &Default::default(), None, Some(&ingots)).unwrap();

    for (start, end) in out
        .diagnostics
        .iter()
        .map(|d| (d.start, d.end))
        .chain(out.lints.iter().map(|l| (l.start, l.end)))
    {
        assert!(src.is_char_boundary(start as usize), "{start}");
        assert!(src.is_char_boundary(end as usize), "{end}");
    }

    assert!(out.lints.iter().any(|l| l.name == "splitter/probe_lint"));
    assert!(!out.diagnostics.is_empty(), "the edit is refused");
}

/// `lints = { name = false }` silences the lint wherever a level is
/// read, and `--@alloy-ignore` silences an ingot's lint on its line.
#[cfg(unix)]
#[test]
fn an_ingot_lint_meets_the_switches_and_the_directives() {
    let ingots = probe(
        "quiet",
        "{ path = DIR, lints = { probe_lint = false } }",
        "",
    );
    assert!(ingots.problems.is_empty(), "{:?}", ingots.problems);
    assert_eq!(
        alloy::lint::level_of(&Default::default(), "quiet/probe_lint"),
        alloy::lint::Level::Allow
    );

    let src = "local a = 1\n--@alloy-ignore\nlocal b = 2\nprint(a, b)\n";
    let ignored = src.find("local b").unwrap();
    let ingots = probe(
        "ignored",
        "DIR",
        &format!(
            "findings = '[{{\"lint\": \"probe_lint\", \"span\": [0, 5]}}, {{\"lint\": \"probe_lint\", \"span\": [{ignored}, {}]}}]'",
            ignored + 5
        ),
    );
    let out = alloy::compile_file("a.aly", src, &Default::default(), None, Some(&ingots)).unwrap();
    let starts: Vec<u32> = out
        .lints
        .iter()
        .filter(|l| l.name == "ignored/probe_lint")
        .map(|l| l.start)
        .collect();

    assert_eq!(starts, vec![0], "{:?}", out.lints);

    // A switch the manifest does not declare is a typo.
    let typo = probe("typo", "{ path = DIR, lints = { nope = false } }", "");
    assert!(
        typo.problems[0].message.contains("`nope`"),
        "{:?}",
        typo.problems
    );
}

/// A crash costs the file it happened on: the next request starts the
/// ingot again. The report names the exit status, and text on stdout
/// reads as that at once, not as a 20 second wait.
#[cfg(unix)]
#[test]
fn a_crashed_ingot_restarts_and_says_why() {
    let ingots = probe("crasher", "DIR", "crash_on = \"transform\"");
    let options = Default::default();
    let crashed = alloy::compile_file("a.aly", "local crash = 1\n", &options, None, Some(&ingots));
    let crashed = crashed.unwrap();

    assert_eq!(crashed.diagnostics.len(), 1, "{:?}", crashed.diagnostics);
    assert!(
        crashed.diagnostics[0].message.contains("exit status: 3"),
        "{:?}",
        crashed.diagnostics
    );

    let next =
        alloy::compile_file("b.aly", "local b = 1\n", &options, None, Some(&ingots)).unwrap();
    assert!(next.diagnostics.is_empty(), "{:?}", next.diagnostics);

    let printer = probe("printer", "DIR", "print_on = \"transform\"");
    let started = std::time::Instant::now();
    let out =
        alloy::compile_file("a.aly", "local a = 1\n", &options, None, Some(&printer)).unwrap();

    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        out.diagnostics[0].message.contains("stdout"),
        "{:?}",
        out.diagnostics
    );
}

/// A completion reply with no items still asks for the host's list,
/// completed as the class it names.
#[cfg(unix)]
#[test]
fn an_empty_completion_still_asks_for_the_host_list() {
    let ingots = probe("completer", "DIR", "");
    let completed = ingots.complete("a.alx", "local e = <card />\n", 15, None);

    assert!(completed.items.is_empty());
    assert!(completed.merge);
    assert_eq!(completed.class.as_deref(), Some("TextLabel"));
}

/// Init names the ingots that loaded, not every key of `[ingots]`.
#[cfg(unix)]
#[test]
fn init_names_only_the_ingots_that_loaded() {
    let ingots = probe("lister", "DIR\nmissing = \"nowhere\"", "");

    assert_eq!(ingots.problems.len(), 1, "{:?}", ingots.problems);

    let hover = ingots.hover("a.aly", "local a = 1\n", 0).expect("a hover");
    assert_eq!(hover["contents"], "[\"lister\"]");
}

/// `alloy ingot run` reads the nearest project: its options and root
/// reach the ingot. A refused edit fails the run.
#[cfg(unix)]
#[test]
fn ingot_run_reads_the_project() {
    let ingot = probe_dir("runner");
    let root = std::env::temp_dir().join(format!("alloy-ingot-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.aly"), "local a = 1\nprint(a)\n").unwrap();
    let run = |edits: &str| {
        std::fs::write(
            root.join("alloy.toml"),
            format!(
                "[ingots]\nrunner = {:?}\n\n[ingot.runner]\nedits = '{edits}'\n",
                ingot.display().to_string()
            ),
        )
        .unwrap();

        std::process::Command::new(env!("CARGO_BIN_EXE_alloy"))
            .args(["ingot", "run", &ingot.display().to_string(), "src/main.aly"])
            .current_dir(&root)
            .output()
            .expect("alloy runs")
    };

    let out = run("[[6, 7, \"b\"]]");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "local b = 1\nprint(a)\n"
    );

    let out = run("[[0, 0, \"\\n\"]]");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("main.aly:1:1"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&root);
}
