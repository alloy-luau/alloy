//! `global`: a name every file of a project reaches without an import.
//!
//! The build resolves it at compile time. A file that names a global
//! gets the `require` of the declaring module and the binding on its
//! first line, so the line count holds and the type checker reads the
//! module the run reads. These tests build small projects and read the
//! emitted text and the diagnostics.

use std::fs;
use std::path::{Path, PathBuf};

use alloy::config::Config;

fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-globals-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"out\"\n").unwrap();

    dir
}

/// Builds the project and gives the report back.
fn build(dir: &Path) -> alloy::build::Report {
    let config = Config::load(&dir.join("alloy.toml")).unwrap();

    alloy::build::run_project(dir, &config).unwrap()
}

/// Every diagnostic message of a build, as one list.
fn messages(report: &alloy::build::Report) -> Vec<String> {
    report
        .diagnostics
        .iter()
        .map(|(p, d)| format!("{}: {}", p.display(), d.message))
        .collect()
}

fn output(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join("out").join(rel)).unwrap()
}

/// One line of a message list that holds `needle`.
#[track_caller]
fn one_saying(messages: &[String], needle: &str) -> String {
    let hits: Vec<&String> = messages.iter().filter(|m| m.contains(needle)).collect();
    assert_eq!(hits.len(), 1, "expected one `{needle}` in {messages:?}");

    hits[0].clone()
}

// --- 1. what a global emits --------------------------------------------------

/// A function, a const, a struct, an enum, and a type alias, each
/// global: the declaring module exports them, and a file that names
/// them requires that module on its first line.
#[test]
fn a_file_reaches_every_kind_of_global() {
    let dir = temp_project("kinds");
    fs::create_dir_all(dir.join("src/shared")).unwrap();
    fs::write(
        dir.join("src/shared/log.aly"),
        "--- Writes a line.\nglobal function log(msg: string)\n    print(msg)\nend\n\nglobal struct Vec2 as\n    x: number\n    y: number\nend\n\nglobal enum State as\n    Idle\n    Busy\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared/ids.aly"),
        "global type Id = number\n\nglobal const MAX = 10\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "local n: Id = MAX\nlog(`{n}`)\nlocal v: Vec2 = nil :: any\nlocal s: State = State.Idle\nprint(v, s)\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));

    let main = output(&dir, "main.luau");
    // One require per module, the bindings after it, all on line one.
    assert!(main.contains("require(\"./shared/ids\")"), "{main}");
    assert!(main.contains("require(\"./shared/log\")"), "{main}");
    assert!(main.contains("type Id = "), "{main}");
    assert!(main.contains("type Vec2 = "), "{main}");
    assert!(main.lines().next().unwrap().contains("local n: Id = MAX"));
    assert_eq!(
        main.lines().count(),
        fs::read_to_string(dir.join("src/main.aly"))
            .unwrap()
            .lines()
            .count(),
        "the line count holds\n{main}"
    );

    // The declaring module still exports the names.
    let log = output(&dir, "shared/log.luau");
    assert!(log.contains("log = log"), "{log}");
    assert!(log.contains("Vec2 = Vec2"), "{log}");

    let _ = fs::remove_dir_all(&dir);
}

/// A file that declares a global uses it with no require of its own.
#[test]
fn a_global_works_inside_its_own_file() {
    let dir = temp_project("own-file");
    fs::write(
        dir.join("src/here.aly"),
        "global function twice(n: number): number\n    return n * 2\nend\n\nprint(twice(2))\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));

    let here = output(&dir, "here.luau");
    assert!(!here.contains("require("), "no require of itself\n{here}");

    let _ = fs::remove_dir_all(&dir);
}

/// `global impl` on a foreign type is the project-wide extension: it
/// declares no name, so no file requires anything for it.
#[test]
fn a_global_impl_declares_no_binding() {
    let dir = temp_project("impl");
    fs::write(
        dir.join("src/ext.aly"),
        "global impl Vector3 as\n    function flat(self): Vector3\n        return self\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/use.aly"),
        "local v = Vector3.new(1, 2, 3)\nprint(v:flat())\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));
    let text = output(&dir, "use.luau");
    assert!(!text.contains("_g1"), "no binding to inject\n{text}");

    let _ = fs::remove_dir_all(&dir);
}

/// A global macro expands where it is written, and a global attribute
/// reads there: neither travels as a require.
#[test]
fn a_global_macro_and_attribute_reach_every_file() {
    let dir = temp_project("compile-time");
    fs::write(
        dir.join("src/decl.aly"),
        "global macro twice(x)\n    x + x\nend\n\nglobal attribute tag(name: string) on struct\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/use.aly"),
        "@tag(\"hi\")\nstruct S as\n    x: number\nend\n\nprint($twice(2), S)\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));

    let text = output(&dir, "use.luau");
    assert!(text.contains("2 + 2"), "the macro expanded\n{text}");
    assert!(text.contains("tag = { \"hi\" }"), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

// --- 2. the errors -----------------------------------------------------------

/// Outside a project there is no set of files to reach.
#[test]
fn a_global_outside_a_project_reports() {
    let out = alloy::compile("global const MAX = 10\n").unwrap();
    let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("needs a project"), "{messages:?}");
}

/// Two files that declare one global name: neither names the other, so
/// the build is the only place that can see it.
#[test]
fn one_name_in_two_files_reports() {
    let dir = temp_project("duplicate");
    fs::write(dir.join("src/a.aly"), "global function log()\nend\n").unwrap();
    fs::write(dir.join("src/b.aly"), "global const log = 1\n").unwrap();

    let report = build(&dir);
    let messages = messages(&report);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .all(|m| m.contains("`log` is global in both")),
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// The compiler names the file it renders as the caller wrote it, and
/// the index names it relative to `[build] in`. One file wrote the
/// global, so nothing reports.
#[test]
fn one_file_never_reports_against_itself() {
    let dir = temp_project("self_duplicate");
    fs::write(
        dir.join("src/main.server.aly"),
        "global local testing12 = 1
print(testing12)
",
    )
    .unwrap();

    let report = build(&dir);
    let messages = messages(&report);
    assert!(
        !messages.iter().any(|m| m.contains("is global in both")),
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// The declaring module leads back to a file that names the global, so
/// the injected require would close a loop.
#[test]
fn a_global_whose_module_leads_back_reports() {
    let dir = temp_project("cycle");
    fs::write(
        dir.join("src/log.aly"),
        "import { fmt } from \"./fmt\"\n\nglobal function log(m: string)\n    print(fmt(m))\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/fmt.aly"),
        "export function fmt(m: string): string\n    log(m)\n    return m\nend\n",
    )
    .unwrap();

    let report = build(&dir);
    let cycle = one_saying(&messages(&report), "form a cycle");
    assert!(cycle.contains("`log`"), "{cycle}");
    assert!(cycle.contains("log.aly"), "{cycle}");
    assert!(cycle.contains("fmt.aly"), "{cycle}");

    let _ = fs::remove_dir_all(&dir);
}

/// A file the project excludes gets no require injected, so a global in
/// it reaches nothing and a use of one from it never resolves.
#[test]
fn an_excluded_file_reports_both_ways() {
    let dir = temp_project("excluded");
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"out\"\nexclude = [\"**/*.spec.aly\"]\n",
    )
    .unwrap();
    fs::write(dir.join("src/log.aly"), "global function log()\nend\n").unwrap();
    fs::write(
        dir.join("src/a.spec.aly"),
        "global const HIDDEN = 1\n\nlog()\n",
    )
    .unwrap();

    let report = build(&dir);
    let messages = messages(&report);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`HIDDEN` is global, and `[build] exclude` drops this file")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`log` is global, and `[build] exclude` drops this file")),
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A name Luau already owns cannot be a global.
#[test]
fn a_global_by_a_luau_name_reports() {
    let dir = temp_project("luau-name");
    fs::write(dir.join("src/a.aly"), "global function print()\nend\n").unwrap();

    let report = build(&dir);
    let message = one_saying(&messages(&report), "is a Luau global");
    assert!(message.contains("`print`"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}

/// A name the std owns is the author's to take; the pedantic lint says
/// what it costs.
#[test]
fn a_global_by_a_std_name_is_a_lint() {
    let dir = temp_project("std-name");
    fs::write(
        dir.join("src/a.aly"),
        "--- The project's own.\nglobal struct Signal as\n    n: number\nend\n",
    )
    .unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));
    let names: Vec<&str> = report.lints.iter().map(|(_, l)| l.name).collect();
    assert!(names.contains(&"shadowed_global"), "{names:?}");

    let _ = fs::remove_dir_all(&dir);
}

/// The name is already here, so the import hides where it comes from.
#[test]
fn an_import_of_a_global_reports() {
    let dir = temp_project("import");
    fs::write(dir.join("src/log.aly"), "global function log()\nend\n").unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import { log } from \"./log\"\n\nlog()\n",
    )
    .unwrap();

    let report = build(&dir);
    let message = one_saying(
        &messages(&report),
        "is global; it is in scope without an import",
    );
    assert!(message.contains("main.aly"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}

/// `export global` says the same thing twice.
#[test]
fn export_global_reports() {
    let out = alloy::compile("export global function f()\nend\n").unwrap();
    let messages: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("drop `export`")),
        "{messages:?}"
    );
}

// --- 3. definitions files ----------------------------------------------------

/// A `.d.aly` declares a name for the checker with no module behind it.
#[test]
fn a_global_in_a_definitions_file_reports() {
    let dir = temp_project("in-definitions");
    fs::write(dir.join("src/a.d.aly"), "global const MAX = 10\n").unwrap();

    let report = build(&dir);
    let message = one_saying(&messages(&report), "a declaration file declares");
    assert!(message.contains("a.d.aly"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}

/// A global by a name a definitions file already declares gives the
/// name two declarations and no way to pick.
#[test]
fn a_global_that_a_definitions_file_declares_reports() {
    let dir = temp_project("ambient-clash");
    fs::write(
        dir.join("src/globals.d.aly"),
        "declare function log(message: string): ()\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/log.aly"),
        "global function log(m: string)\n    print(m)\nend\n",
    )
    .unwrap();

    let report = build(&dir);
    let message = one_saying(&messages(&report), "is declared in");
    assert!(message.contains("globals.d.aly"), "{message}");
    assert!(message.contains("log.aly"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}

/// A definitions file beside a module that declares an unrelated global
/// reports nothing: the two never meet.
#[test]
fn a_definitions_file_and_an_unrelated_global_are_clean() {
    let dir = temp_project("ambient-clean");
    fs::write(
        dir.join("src/globals.d.aly"),
        "declare function shout(message: string): ()\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/log.aly"),
        "--- Writes a line.\nglobal function log(m: string)\n    print(m)\nend\n",
    )
    .unwrap();
    fs::write(dir.join("src/main.aly"), "log(\"hi\")\nshout(\"hi\")\n").unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));

    // The `.d.aly` name needs no require; the global gets one.
    let main = output(&dir, "main.luau");
    assert!(main.contains("require(\"./log\")"), "{main}");
    assert_eq!(main.matches("require(").count(), 1, "{main}");

    let _ = fs::remove_dir_all(&dir);
}

// --- 4. the sides -----------------------------------------------------------

/// Nine combinations: a client, server, and shared global, each read
/// from a client, server, and shared file.
#[test]
fn a_global_reaches_its_own_side() {
    let dir = temp_project("sides");
    fs::write(dir.join("src/c.client.aly"), "global const C = 1\n").unwrap();
    fs::write(dir.join("src/s.server.aly"), "global const S = 2\n").unwrap();
    fs::write(dir.join("src/h.aly"), "global const H = 3\n").unwrap();

    for (name, side) in [
        ("uc.client.aly", "client"),
        ("us.server.aly", "server"),
        ("uh.aly", "shared"),
    ] {
        fs::write(dir.join("src").join(name), "print(C, S, H)\n").unwrap();
        let _ = side;
    }

    let report = build(&dir);
    let messages = messages(&report);
    let side_errors: Vec<&String> = messages
        .iter()
        .filter(|m| m.contains("is global on"))
        .collect();
    assert_eq!(side_errors.len(), 4, "{messages:?}");
    assert!(
        side_errors
            .iter()
            .any(|m| m.contains("uc.client.aly") && m.contains("`S` is global on the server only")),
        "{side_errors:?}"
    );
    assert!(
        side_errors
            .iter()
            .any(|m| m.contains("us.server.aly") && m.contains("`C` is global on the client only")),
        "{side_errors:?}"
    );
    assert!(
        side_errors.iter().any(|m| m.contains("uh.aly")
            && m.contains("`C` is global on the client; this file is shared")),
        "{side_errors:?}"
    );
    assert!(
        side_errors.iter().any(|m| m.contains("uh.aly")
            && m.contains("`S` is global on the server; this file is shared")),
        "{side_errors:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Every pair of sides for one name. A server global and a client
/// global never run together, so both may hold the name; a shared
/// global reserves it on every side, and two globals of one side
/// report.
#[test]
fn one_name_holds_on_two_sides_that_never_meet() {
    let dir = temp_project("side-pairs");
    let client = "--@alloy-file-side client\nglobal const LIMIT = 1\n";
    let server = "--@alloy-file-side server\nglobal const LIMIT = 2\n";
    let shared = "global const LIMIT = 3\n";
    fs::write(dir.join("src/uc.client.aly"), "print(LIMIT)\n").unwrap();
    fs::write(dir.join("src/us.server.aly"), "print(LIMIT)\n").unwrap();
    // The pairs and how many files report the clash: none when the two
    // sides never meet, one report per declaring file otherwise.
    let clashes = |a: &str, b: &str| -> usize {
        fs::write(dir.join("src/a.aly"), a).unwrap();
        fs::write(dir.join("src/b.aly"), b).unwrap();

        messages(&build(&dir))
            .iter()
            .filter(|m| m.contains("`LIMIT` is global in both"))
            .count()
    };

    assert_eq!(clashes(client, server), 0);
    assert_eq!(clashes(server, client), 0);
    assert_eq!(clashes(client, shared), 2);
    assert_eq!(clashes(shared, client), 2);
    assert_eq!(clashes(server, shared), 2);
    assert_eq!(clashes(shared, server), 2);
    assert_eq!(clashes(client, client), 2);
    assert_eq!(clashes(server, server), 2);
    assert_eq!(clashes(shared, shared), 2);

    // With one name on each side, each file reaches the one of its own.
    fs::write(dir.join("src/a.aly"), client).unwrap();
    fs::write(dir.join("src/b.aly"), server).unwrap();
    let report = build(&dir);
    assert!(messages(&report).is_empty(), "{:?}", messages(&report));
    assert!(output(&dir, "uc.client.luau").contains("require(\"./a\")"));
    assert!(output(&dir, "us.server.luau").contains("require(\"./b\")"));

    let _ = fs::remove_dir_all(&dir);
}

/// A global a script declares moves into a module of the build's own
/// making. A message names the script the author wrote.
#[test]
fn a_message_names_the_script_that_declares_a_global() {
    let dir = temp_project("script-name");
    fs::write(
        dir.join("src/main.server.aly"),
        "global const LIMIT = 1
",
    )
    .unwrap();
    fs::write(
        dir.join("src/other.server.aly"),
        "global const LIMIT = 2
",
    )
    .unwrap();

    let report = build(&dir);
    let messages = messages(&report);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages.iter().all(|m| !m.contains(".globals.aly")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`LIMIT` is global in both main.server.aly and other.server.aly")),
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `--@alloy-side` over one global beats every rule the file follows.
#[test]
fn a_side_directive_over_a_global_wins() {
    let dir = temp_project("side-directive");
    fs::write(
        dir.join("src/mix.aly"),
        "--@alloy-side client\nglobal const THEME = \"dark\"\n\nglobal const SHARED = 1\n",
    )
    .unwrap();
    fs::write(dir.join("src/ui.client.aly"), "print(THEME, SHARED)\n").unwrap();
    fs::write(dir.join("src/back.server.aly"), "print(SHARED)\n").unwrap();
    fs::write(dir.join("src/oops.server.aly"), "print(THEME)\n").unwrap();

    let report = build(&dir);
    let messages = messages(&report);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("oops.server.aly"), "{messages:?}");
    assert!(
        messages[0].contains("`THEME` is global on the client only"),
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `[contexts]` names the folders of each side, for a project that
/// keeps its client code under a replicated service.
#[test]
fn the_contexts_table_names_the_side_of_a_folder() {
    let dir = temp_project("contexts");
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"out\"\n\n[contexts]\nclient = [\"ui\"]\nserver = [\"back\"]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("src/ui")).unwrap();
    fs::create_dir_all(dir.join("src/back")).unwrap();
    fs::write(dir.join("src/ui/theme.aly"), "global const THEME = 1\n").unwrap();
    fs::write(dir.join("src/ui/panel.aly"), "print(THEME)\n").unwrap();
    fs::write(dir.join("src/back/rules.aly"), "print(THEME)\n").unwrap();

    let report = build(&dir);
    let message = one_saying(&messages(&report), "is global on the client only");
    assert!(message.contains("rules.aly"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}

// --- 5. a global in a script -------------------------------------------------

/// A script cannot be required, so the build writes a module beside it
/// and every file, the script included, requires that.
#[test]
fn a_script_hoists_its_globals_into_a_module() {
    let dir = temp_project("hoist");
    fs::write(
        dir.join("src/util.aly"),
        "--- Formats a version.\nexport function fmt(v: string): string\n    return `v{v}`\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/boot.server.aly"),
        "import { fmt } from \"./util\"\n\nglobal const VERSION = \"1.0\"\n\nglobal function banner(): string\n    return fmt(VERSION)\nend\n\nprint(banner())\n",
    )
    .unwrap();
    fs::write(dir.join("src/other.server.aly"), "print(banner())\n").unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", messages(&report));

    // The module holds the globals; the script keeps its line count and
    // requires them back.
    let module = output(&dir, "boot.server.globals.luau");
    assert!(module.contains("banner = banner"), "{module}");
    assert!(module.contains("VERSION = VERSION"), "{module}");

    let script = output(&dir, "boot.server.luau");
    assert!(
        script.contains("require(\"./boot.server.globals\")"),
        "{script}"
    );
    assert_eq!(
        script.lines().count(),
        fs::read_to_string(dir.join("src/boot.server.aly"))
            .unwrap()
            .lines()
            .count(),
        "{script}"
    );

    let other = output(&dir, "other.server.luau");
    assert!(
        other.contains("require(\"./boot.server.globals\")"),
        "{other}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A global in a script has to stand on its own: it moves into a module
/// of its own, where the script's other names do not exist.
#[test]
fn a_global_in_a_script_that_reads_a_local_reports() {
    let dir = temp_project("hoist-leak");
    fs::write(
        dir.join("src/boot.server.aly"),
        "local secret = 42\n\nglobal function peek(): number\n    return secret\nend\n",
    )
    .unwrap();

    let report = build(&dir);
    let message = one_saying(&messages(&report), "may use only imports");
    assert!(message.contains("`secret`"), "{message}");

    let _ = fs::remove_dir_all(&dir);
}

/// A `global const` is set once wherever it is read. The `const` check
/// used to read the declaring file alone, so an assignment in another
/// file said nothing.
#[test]
fn an_assignment_to_a_global_const_reports_in_every_file() {
    let dir = temp_project("const");
    fs::write(dir.join("src/a.aly"), "global const MAX = 100\n").unwrap();
    fs::write(dir.join("src/b.aly"), "MAX = 200\n\nprint(MAX)\n").unwrap();
    let report = build(&dir);
    let hit = one_saying(&messages(&report), "is a `const`");
    assert!(hit.contains("`MAX` is a `const` of a.aly"), "{hit}");
    assert!(hit.starts_with("b.aly"), "{hit}");
}

/// A local of the file wins: the global never reaches a name the file
/// binds itself.
#[test]
fn a_local_of_the_same_name_takes_no_const_report() {
    let dir = temp_project("const-shadow");
    fs::write(dir.join("src/a.aly"), "global const MAX = 100\n").unwrap();
    fs::write(
        dir.join("src/b.aly"),
        "local MAX = 1\nMAX = 200\n\nprint(MAX)\n",
    )
    .unwrap();
    let report = build(&dir);
    let hits: Vec<String> = messages(&report)
        .into_iter()
        .filter(|m| m.contains("is a `const`"))
        .collect();
    assert!(hits.is_empty(), "{hits:?}");
}

/// `impl Vec as` in one file on a `global struct` another declares: the
/// methods reach the global's table at run time, and the declaring
/// file's check artifact declares them, so every file types them.
#[test]
fn an_impl_in_another_file_attaches_to_a_global_struct() {
    let dir = temp_project("foreign-impl");
    fs::write(
        dir.join("src/a.aly"),
        "global struct Vec as\n    x: number\n    y: number\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/b.aly"),
        "impl Vec as\n    function length(self): number\n        return self.x + self.y\n    end\n\n    function origin(): Vec\n        return new Vec { x = 0, y = 0 }\n    end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/c.aly"),
        "local v = Vec.origin()\n\nprint(v:length())\n",
    )
    .unwrap();
    let report = build(&dir);
    let hits: Vec<String> = messages(&report);
    assert!(hits.is_empty(), "{hits:?}");

    // The check artifact declares both; the ship artifact writes no key
    // the source has not got.
    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let flux = alloy::build::flux_project(&dir, &config).unwrap();
    let check = flux
        .checks
        .iter()
        .find(|c| c.rel.to_string_lossy() == "a.aly")
        .map(|c| c.check.clone())
        .unwrap_or_default();
    assert!(
        check.contains("Vec.length = (nil :: any) :: (self: Vec) -> number"),
        "{check}"
    );
    assert!(
        check.contains("Vec.origin = (nil :: any) :: () -> Vec"),
        "{check}"
    );
    assert!(!output(&dir, "a.luau").contains("nil :: any"), "ship");
}
