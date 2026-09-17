//! `alloy build` over a project folder: the tree is mirrored, unchanged
//! outputs are left alone, and `clean` removes what no source produces.

use std::fs;
use std::path::PathBuf;

use alloy::config::{Build, Config, Emit};

fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-build-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src/nested")).unwrap();

    dir
}

#[test]
fn a_project_builds_into_its_out_tree() {
    let dir = temp_project("tree");
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"dist\"\nclean = true\n",
    )
    .unwrap();
    fs::write(dir.join("src/main.aly"), "local v = a ?? 1\n").unwrap();
    fs::write(dir.join("src/nested/util.aly"), "return 1\n").unwrap();
    fs::write(dir.join("src/types.d.aly"), "export type T = number\n").unwrap();
    fs::create_dir_all(dir.join("dist")).unwrap();
    fs::write(dir.join("dist/stale.luau"), "-- old\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();

    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        fs::read_to_string(dir.join("dist/main.luau")).unwrap(),
        "local v = (if a == nil then 1 else a)\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("dist/nested/util.luau")).unwrap(),
        "return 1\n"
    );
    assert!(dir.join("dist/types.d.luau").is_file());
    assert!(
        !dir.join("dist/stale.luau").exists(),
        "clean removed the stale output"
    );
    assert_eq!(report.removed, vec![PathBuf::from("stale.luau")]);

    let _ = fs::remove_dir_all(&dir);
}

/*
The build skips the write when the output already holds the bytes the
compile produced, so rojo does not resync. `written` counted every
output it walked, so a second build reported files it never touched.

`written` now counts a file the build creates or changes, and
`up_to_date` counts the rest.
*/
#[test]
fn a_second_build_counts_an_unchanged_output_as_up_to_date() {
    let dir = temp_project("uptodate");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(dir.join("src/one.aly"), "print(1)\n").unwrap();
    fs::write(dir.join("src/two.aly"), "print(2)\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let build = || alloy::build::run(&dir, &config.build, &config.emit).unwrap();

    // The first build creates both outputs.
    let first = build();

    assert_eq!(first.written.len(), 2, "{first:?}");
    assert!(first.up_to_date.is_empty(), "{first:?}");

    // Nothing changed: both outputs stand as they are.
    let second = build();

    assert!(second.written.is_empty(), "{second:?}");
    assert_eq!(second.up_to_date.len(), 2, "{second:?}");

    // One source changes: one output is written, the other stands.
    fs::write(dir.join("src/one.aly"), "print(3)\n").unwrap();
    let third = build();

    assert_eq!(third.written, vec![PathBuf::from("one.luau")]);
    assert_eq!(third.up_to_date, vec![PathBuf::from("two.luau")]);

    // The check path writes nothing, so it counts every output it
    // would write.
    let dry = alloy::build::check(&dir, &config.build, &config.emit).unwrap();

    assert_eq!(dry.written.len(), 2, "{dry:?}");
    assert!(dry.up_to_date.is_empty(), "{dry:?}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn excludes_and_diagnostics_are_reported() {
    let dir = temp_project("report");
    // The file parses, so the emit is Luau; the diagnostic is about
    // what the code means.
    fs::write(dir.join("src/keep.aly"), "const A = 1\nA = 2\nprint(A)\n").unwrap();
    fs::write(dir.join("src/skip.spec.aly"), "local = 1\n").unwrap();

    let build = Build {
        exclude: vec!["**/*.spec.aly".to_string()],
        ..Build::default()
    };
    let report = alloy::build::run(&dir, &build, &Emit::default()).unwrap();

    assert_eq!(
        report.skipped,
        vec![PathBuf::from("keep.aly"), PathBuf::from("skip.spec.aly")]
    );
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert_eq!(report.diagnostics.len(), 1, "the reassignment is reported");
    assert!(
        !dir.join("build/keep.luau").exists(),
        "an error keeps the output unwritten"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A file with a compile error writes no output: the emit ships the
/// construct the error names. The output a run before left stays, so
/// the game keeps running the last good build.
#[test]
fn a_file_with_an_error_keeps_its_last_output() {
    let dir = temp_project("error");
    fs::write(dir.join("alloy.toml"), "[build]\n").unwrap();
    fs::write(
        dir.join("src/bad.aly"),
        "struct Pt as\n    x: number\nend\n\nlocal p = new Pt { x = 1, z = 9 }\n",
    )
    .unwrap();
    fs::write(dir.join("src/fine.aly"), "return 1\n").unwrap();
    fs::create_dir_all(dir.join("build")).unwrap();
    fs::write(dir.join("build/bad.luau"), "-- an older run\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();

    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert_eq!(report.written, vec![PathBuf::from("fine.luau")]);
    assert_eq!(report.skipped, vec![PathBuf::from("bad.aly")]);
    assert_eq!(
        fs::read_to_string(dir.join("build/bad.luau")).unwrap(),
        "-- an older run\n",
        "the last output stays"
    );
    assert!(!report.is_clean());

    let _ = fs::remove_dir_all(&dir);
}

/// A file the parser could not read whole writes no output: past the
/// first error the emit copies the source through, and a `.luau` of
/// Alloy text is what the next tool would load. `clean` takes the one a
/// run before this left.
#[test]
fn a_file_that_does_not_parse_writes_no_output() {
    let dir = temp_project("broken");
    fs::write(dir.join("alloy.toml"), "[build]\nclean = true\n").unwrap();
    fs::write(dir.join("src/broken.aly"), "this is not alloy !!! @#$\n").unwrap();
    fs::write(dir.join("src/fine.aly"), "return 1\n").unwrap();
    fs::create_dir_all(dir.join("build")).unwrap();
    fs::write(dir.join("build/broken.luau"), "-- an older run\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();

    assert!(!report.diagnostics.is_empty(), "the file reports");
    assert_eq!(report.written, vec![PathBuf::from("fine.luau")]);
    assert_eq!(report.skipped, vec![PathBuf::from("broken.aly")]);
    assert!(
        !dir.join("build/broken.luau").exists(),
        "the stale output is gone"
    );
    assert_eq!(report.removed, vec![PathBuf::from("broken.luau")]);
    assert!(dir.join("build/fine.luau").is_file());

    let _ = fs::remove_dir_all(&dir);
}

/// `reg.aly` and `reg.alx` both build `reg.luau`: the second write
/// overwrites the first, and `require("./reg")` could not say which one
/// it meant. The build names both files instead of writing one silently.
#[test]
fn two_sources_that_build_one_module_are_a_diagnostic() {
    let dir = temp_project("twin-source");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"out\"\n").unwrap();
    fs::write(dir.join("src/reg.aly"), "return 1\n").unwrap();
    fs::write(
        dir.join("src/reg.alx"),
        "function view()\n    return <frame />\nend\n\nreturn view\n",
    )
    .unwrap();
    // A name that differs in more than the extension is fine, and so is
    // a definitions file beside the module it describes.
    fs::write(dir.join("src/other.aly"), "return 2\n").unwrap();
    fs::write(dir.join("src/other.d.aly"), "export type T = number\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|(p, d)| format!("{}: {}", p.display(), d.message))
        .collect();

    assert_eq!(
        messages,
        ["reg.aly: src/reg.aly and src/reg.alx both build src/reg.luau; rename one"],
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Two `.d.aly` files that declare one name: the second declaration
/// wins at run time, so the build reports it once and names the file
/// the first declaration sits in.
#[test]
fn one_ambient_name_declared_twice_is_an_error() {
    let dir = temp_project("ambient");
    fs::write(
        dir.join("src/globals.d.aly"),
        "declare function helperFn(x: number): number\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/other.d.aly"),
        "declare function helperFn(y: string): string\n",
    )
    .unwrap();

    let report = alloy::build::run(&dir, &Build::default(), &Emit::default()).unwrap();
    let clashes: Vec<&(PathBuf, alloy::Diagnostic)> = report
        .diagnostics
        .iter()
        .filter(|(_, d)| d.message.contains("already declared"))
        .collect();

    assert_eq!(clashes.len(), 1, "{report:?}");
    assert_eq!(clashes[0].0, PathBuf::from("other.d.aly"));
    assert!(
        clashes[0].1.message.contains("globals.d.aly"),
        "{}",
        clashes[0].1.message
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `[emit] erase_type_imports` drops the `require` of a line that binds
/// types alone. The build never read the key, so no shape was blanked;
/// and only `import type { }` was blanked, not a `{ type X }` list.
#[test]
fn erase_type_imports_drops_a_type_only_require() {
    let dir = temp_project("erase");
    fs::write(
        dir.join("alloy.toml"),
        "[emit]\nerase_type_imports = true\n",
    )
    .unwrap();
    fs::write(dir.join("src/types.aly"), "export type Meters = number\n").unwrap();
    fs::write(
        dir.join("src/util.aly"),
        "export type Feet = number\n\nexport function scale(x: number): number\n    return x * 2\nend\n",
    )
    .unwrap();
    // The whole list is types: `import type { }`, a `type` spec, and a
    // name the module exports as a type alone.
    fs::write(
        dir.join("src/whole.aly"),
        "import type { Meters } from \"./types\"\n\nexport function a(x: Meters): Meters\n    return x\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/spec.aly"),
        "import { type Meters } from \"./types\"\n\nexport function b(x: Meters): Meters\n    return x\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/exported.aly"),
        "import { Meters } from \"./types\"\n\nexport function c(x: Meters): Meters\n    return x\nend\n",
    )
    .unwrap();
    // One value in the list: the require stays and nothing is dropped.
    fs::write(
        dir.join("src/mixed.aly"),
        "import { type Feet, scale } from \"./util\"\n\nexport function d(x: Feet): Feet\n    return scale(x)\nend\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    assert!(report.is_clean(), "{report:?}");

    for name in ["whole", "spec", "exported"] {
        let out = fs::read_to_string(dir.join(format!("build/{name}.luau"))).unwrap();
        assert!(!out.contains("require("), "{name}: {out}");
        assert!(!out.contains("type Meters"), "{name}: {out}");
    }

    let mixed = fs::read_to_string(dir.join("build/mixed.luau")).unwrap();
    assert!(mixed.contains("require(\"./util\")"), "{mixed}");
    assert!(mixed.contains("local scale = "), "{mixed}");
    assert!(mixed.contains("type Feet = "), "{mixed}");

    // Off by default: the require stays in every shape.
    let plain = Config {
        build: Build::default(),
        emit: Emit::default(),
        ..config
    };
    alloy::build::run(&dir, &plain.build, &plain.emit).unwrap();
    let out = fs::read_to_string(dir.join("build/spec.luau")).unwrap();
    assert!(out.contains("require(\"./types\")"), "{out}");

    let _ = fs::remove_dir_all(&dir);
}

/*
Luau reads `x/init.luau` as the module `x`, so a `./y` in it names a
file beside `x`, not one inside it. The build wrote the runtime require
of an `init.aly` from the file's own folder, one folder too deep, and
`alloy flux` then reported every std type the emit names as unknown.

The require now starts at the folder Luau resolves it from.
*/
#[test]
fn an_init_module_requires_the_runtime_from_the_folder_above_it() {
    let dir = temp_project("init-runtime");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"build\"\n").unwrap();
    fs::create_dir_all(dir.join("src/deep/deeper")).unwrap();
    let source = "attribute tag on function\n\n@tag\nexport local function f() end\n";

    for name in [
        "src/init.aly",
        "src/deep/init.aly",
        "src/deep/deeper/init.aly",
        "src/deep/plain.aly",
    ] {
        fs::write(dir.join(name), source).unwrap();
    }

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();

    assert!(report.is_clean(), "{report:?}");

    for (out, spec) in [
        ("build/init.luau", "./build/alloy"),
        ("build/deep/init.luau", "./alloy"),
        ("build/deep/deeper/init.luau", "../alloy"),
        ("build/deep/plain.luau", "../alloy"),
    ] {
        let text = fs::read_to_string(dir.join(out)).unwrap();
        assert!(
            text.contains(&format!("require(\"{spec}\")")),
            "{out}: {text}"
        );

        // The folder Luau starts the require from: the file's own, and
        // the one above it for an `init.luau`.
        let folder = dir.join(out);
        let folder = folder.parent().unwrap();
        let folder = match out.ends_with("init.luau") {
            true => folder.parent().unwrap(),

            false => folder,
        };

        assert!(
            folder.join(spec).with_extension("luau").is_file(),
            "{out}: {spec} names no runtime"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}
