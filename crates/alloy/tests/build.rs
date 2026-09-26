//! `alloy build` over a project folder: the tree is mirrored, unchanged
//! outputs are left alone, and `clean` removes what no source produces.

use std::fs;
use std::path::PathBuf;

use alloy::config::{Artifact, Build, Config, Emit};

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
    // A declaration file feeds the type check and writes nothing.
    assert!(!dir.join("dist/types.d.luau").exists());
    assert!(
        !dir.join("dist/stale.luau").exists(),
        "clean removed the stale output"
    );
    assert_eq!(report.removed, vec![PathBuf::from("stale.luau")]);

    let _ = fs::remove_dir_all(&dir);
}

/// With `out` equal to `in`, the second build read the first one's
/// `a.luau` as a plain source and reported that two files build it.
/// The build now refuses the folder with a message, before it writes.
#[test]
fn a_build_refuses_out_equal_to_in() {
    let dir = temp_project("in-place");
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"src\"\n",
    )
    .unwrap();
    fs::write(dir.join("src/a.aly"), "return 1\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let err = alloy::build::run_project(&dir, &config).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("[build] out is the folder in names, `src`"),
        "{err}"
    );
    assert!(!dir.join("src/a.luau").exists());

    // A folder under `in` still builds, and the walk skips it.
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"src/out\"\n",
    )
    .unwrap();
    let config = Config::load(&dir.join("alloy.toml")).unwrap();

    for _ in 0..2 {
        let report = alloy::build::run_project(&dir, &config).unwrap();
        assert!(report.diagnostics.is_empty(), "{report:?}");
    }

    assert!(dir.join("src/out/a.luau").exists());

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

/// With `clean` off, a renamed script left its old output, and Rojo ran
/// both scripts. The build keeps a manifest of what it wrote and removes
/// an output no source makes now. A file the build never wrote stays,
/// and so does the last output of a file with an error.
#[test]
fn a_renamed_source_takes_its_old_output_with_it() {
    let dir = temp_project("rename");
    fs::write(dir.join("alloy.toml"), "[build]\nclean = false\n").unwrap();
    fs::write(dir.join("src/nested/main.server.aly"), "print(1)\n").unwrap();
    fs::write(dir.join("src/bad.aly"), "return 1\n").unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let build = || alloy::build::run_project(&dir, &config).unwrap();
    build();
    fs::write(dir.join("build/mine.luau"), "-- the author's\n").unwrap();

    fs::rename(dir.join("src/nested"), dir.join("src/moved")).unwrap();
    fs::write(dir.join("src/bad.aly"), "local = 1\n").unwrap();
    let report = build();

    assert_eq!(
        report.removed,
        vec![PathBuf::from("nested/main.server.luau")]
    );
    assert!(!dir.join("build/nested").exists(), "the empty folder goes");
    assert!(dir.join("build/moved/main.server.luau").is_file());
    assert!(
        dir.join("build/mine.luau").is_file(),
        "the build never wrote it"
    );
    assert!(
        dir.join("build/bad.luau").is_file(),
        "the last good output stays"
    );

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

/*
An author's relative import follows the same Luau rule as the runtime
require. `import { g } from "./other"` in `src/init.aly` emitted
`require("./other")`, which names a file beside the output folder, so
`alloy flux` reported that the path names no module.

The emit writes the path from the folder Luau resolves it from. Only an
`init` module moves; every other file keeps the path the source wrote.
A file inside the `init`'s own folder is `@self/...`: the folder's name
on disk can differ from its instance name, so `./build/other` fails in
Roblox and under a sourcemap.
*/
#[test]
fn an_init_module_requires_a_sibling_from_the_folder_above_it() {
    let dir = temp_project("init-import");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"build\"\n").unwrap();
    fs::create_dir_all(dir.join("src/deep")).unwrap();
    fs::write(
        dir.join("src/other.aly"),
        "export function g(): number\n    return 1\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/widget.alx"),
        "export function w(): number\n    return 2\nend\n",
    )
    .unwrap();
    fs::write(dir.join("src/legacy.luau"), "return { n = 3 }\n").unwrap();
    fs::write(
        dir.join("src/deep/sib.aly"),
        "export function s(): number\n    return 4\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/init.aly"),
        "import { g } from \"./other\"\nimport { w } from \"./widget\"\nimport legacy from \"./legacy\"\n\nprint(g(), w(), legacy.n)\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/deep/init.aly"),
        "import { s } from \"./sib\"\nimport { g } from \"../other\"\n\nprint(s(), g())\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/plain.aly"),
        "import { g } from \"./other\"\n\nprint(g())\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();

    assert!(report.is_clean(), "{report:?}");

    for (out, specs) in [
        (
            "build/init.luau",
            ["@self/other", "@self/widget", "@self/legacy"].as_slice(),
        ),
        ("build/deep/init.luau", ["@self/sib", "./other"].as_slice()),
        // A file that is no `init` keeps the path the source wrote.
        ("build/plain.luau", ["./other"].as_slice()),
    ] {
        let text = fs::read_to_string(dir.join(out)).unwrap();

        for spec in specs {
            assert!(
                text.contains(&format!("require(\"{spec}\")")),
                "{out}: {text}"
            );

            // The folder Luau starts the require from: the file's own,
            // and the one above it for an `init.luau`. `@self` is the
            // `init`'s own folder.
            let path = dir.join(out);
            let folder = path.parent().unwrap();
            let (folder, spec) = match (spec.strip_prefix("@self/"), out.ends_with("init.luau")) {
                (Some(inner), _) => (folder, inner),

                (None, true) => (folder.parent().unwrap(), *spec),

                (None, false) => (folder, *spec),
            };

            assert!(
                folder.join(spec).with_extension("luau").is_file(),
                "{out}: {spec} names no module"
            );
        }
    }

    let _ = fs::remove_dir_all(&dir);
}

/*
`import { X } from "../shared/net"` in `src/server/a.server.aly` crosses
from one mount into another. The ship wrote the `@game/...` place, but
the check artifact kept the relative path. luau-lsp reads that path
from the file's place in the sourcemap, where the two mounts are no
siblings, so `alloy flux` and the editor reported `UnknownModule`.

The check artifact now writes the place, as the ship does. A relative
path inside one mount stays.

`"../shared/net"` in `src/shared/c.aly` climbs out of its own mount and
back in. Both artifacts kept it, and `Shared` has no `shared` beside it,
so the require missed at run time. The path inside the mount replaces
it.
*/
#[test]
fn a_relative_import_across_mounts_writes_the_game_path_in_the_check() {
    let dir = temp_project("cross-mount");
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"build\"\nartifact = \"check\"\n\n[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("src/server")).unwrap();
    fs::create_dir_all(dir.join("src/shared/sub")).unwrap();
    fs::write(dir.join("src/shared/net.aly"), "export const X = 1\n").unwrap();
    fs::write(
        dir.join("src/server/a.server.aly"),
        "import { X } from \"../shared/net\"\nprint(X)\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared/sub/b.aly"),
        "import { X } from \"../net\"\nprint(X)\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared/c.aly"),
        "import { X } from \"../shared/net\"\nprint(X)\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared/sub/d.aly"),
        "import { X } from \"../../shared/net\"\nprint(X)\n",
    )
    .unwrap();

    let mut config = Config::load(&dir.join("alloy.toml")).unwrap();

    for artifact in [Artifact::Check, Artifact::Ship] {
        config.build.artifact = artifact;
        let report = alloy::build::run_project(&dir, &config).unwrap();

        assert!(report.is_clean(), "{report:?}");

        for (out, spec) in [
            (
                "build/server/a.server.luau",
                "@game/ReplicatedStorage/Shared/net",
            ),
            ("build/shared/sub/b.luau", "../net"),
            ("build/shared/c.luau", "./net"),
            ("build/shared/sub/d.luau", "../net"),
        ] {
            let text = fs::read_to_string(dir.join(out)).unwrap();

            assert!(
                text.contains(&format!("require(\"{spec}\")")),
                "{artifact:?} {out}: {text}"
            );
        }
    }

    let _ = fs::remove_dir_all(&dir);
}

/*
`src/server/mod.aly` has no `.server` in its name, so the remote check
read it as shared and let it fire a remote that goes from the client.
Under `ServerScriptService` it runs on the server alone. A module now
takes the side of its mount's place. One under `ReplicatedStorage`
stays shared.
*/
#[test]
fn a_module_takes_the_side_of_its_mount() {
    let dir = temp_project("mount-side");
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nout = \"build\"\n\n[mount]\nclient = [\"src/client\", \"@game/StarterPlayer/StarterPlayerScripts/Client\"]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n",
    )
    .unwrap();

    for folder in ["src/client", "src/server", "src/shared"] {
        fs::create_dir_all(dir.join(folder)).unwrap();
    }

    fs::write(
        dir.join("src/shared/net.aly"),
        "export remote Ping(n: number) from client\nexport remote Pong(n: number) from server\n",
    )
    .unwrap();

    for (file, spec, fires) in [
        ("src/server/mod.aly", "../shared/net", "Ping"),
        ("src/client/mod.aly", "../shared/net", "Pong"),
        ("src/shared/mod.aly", "./net", "Ping"),
    ] {
        fs::write(
            dir.join(file),
            format!("import {{ {fires} }} from \"{spec}\"\n{fires}.fire(1)\n"),
        )
        .unwrap();
    }

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    let mut found: Vec<_> = report
        .diagnostics
        .iter()
        .map(|(path, d)| format!("{}: {}", path.display(), d.message))
        .collect();
    found.sort();

    assert_eq!(
        found,
        [
            "client/mod.aly: `Pong` goes from the server; the client cannot fire it",
            "server/mod.aly: `Ping` goes from the client; the server cannot fire it",
        ],
        "{report:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
An `init.server.aly` or `init.client.aly` is the script of its folder,
as `init.aly` is the module of its folder. The emit kept `./util` in
such a script, and Roblox read it from the folder above, so the script
found no module or the wrong one. `@self/util` names the script's own
child. An author may write `@self` in an `init` file; in any other file
it is an error that says what to write.
*/
#[test]
fn an_init_script_requires_a_file_of_its_folder_by_self() {
    let dir = temp_project("init-script");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"build\"\n").unwrap();
    fs::create_dir_all(dir.join("src/server")).unwrap();
    fs::create_dir_all(dir.join("src/client/ui")).unwrap();
    fs::write(dir.join("src/server/util.aly"), "export const X = 1\n").unwrap();
    fs::write(dir.join("src/server/more.aly"), "export const Y = 2\n").unwrap();
    fs::write(
        dir.join("src/server/init.server.aly"),
        "import { X } from \"./util\"\nimport { Y } from \"@self/more\"\n\nprint(X, Y)\n",
    )
    .unwrap();
    fs::write(dir.join("src/client/panel.aly"), "export const P = 1\n").unwrap();
    fs::write(dir.join("src/client/ui/panel.aly"), "export const P = 2\n").unwrap();
    fs::write(
        dir.join("src/client/ui/init.client.aly"),
        "import { P } from \"./panel\"\n\nprint(P)\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();

    assert!(report.is_clean(), "{report:?}");

    for (out, spec) in [
        ("build/server/init.server.luau", "@self/util"),
        ("build/server/init.server.luau", "@self/more"),
        ("build/client/ui/init.client.luau", "@self/panel"),
    ] {
        let text = fs::read_to_string(dir.join(out)).unwrap();

        assert!(
            text.contains(&format!("require(\"{spec}\")")),
            "{out}: {text}"
        );
    }

    // A file that is no `init` has no folder for `@self` to name.
    fs::write(
        dir.join("src/client/other.client.aly"),
        "import { P } from \"@self/panel\"\n\nprint(P)\n",
    )
    .unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let messages: Vec<_> = report.diagnostics.iter().map(|(_, d)| &d.message).collect();

    assert!(
        messages.iter().any(|m| m.contains(
            "`@self` is the folder of an `init` file, and src/client/other.client.aly is no `init`, so write \"./panel\""
        )),
        "{messages:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
`import { E } from "./m"` beside `export { E } from "./m"` wrote
`type E` and `export type E`, and Luau reported a redefinition of `E`.
The import's alias now takes the `export` word, and the list writes no
second alias.
*/
#[test]
fn an_import_and_a_reexport_of_one_type_write_one_alias() {
    let dir = temp_project("reexport-import");
    fs::write(dir.join("alloy.toml"), "[build]\n").unwrap();
    fs::write(
        dir.join("src/m.aly"),
        "export enum E\n    A\nend\n\nexport type T = { n: number }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/r.aly"),
        "import { E, T } from \"./m\"\nexport { E, T } from \"./m\"\n\nlocal t: T = { n = 1 }\nprint(E.A, t)\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();

    assert!(report.is_clean(), "{report:?}");

    let text = fs::read_to_string(dir.join("build/r.luau")).unwrap();

    for name in ["E", "T"] {
        assert_eq!(text.matches(&format!("type {name} =")).count(), 1, "{text}");
        assert!(text.contains(&format!("export type {name} =")), "{text}");
    }

    assert!(text.contains("return { E = _m2.E }"), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

/*
A barrel wrote `import * as Leaf from "./leaf"` and `export { Leaf }`.
`B.Leaf.Box` through `import * as B` of the barrel emitted as it was
written, and Luau reads no type path two modules deep, so the output did
not parse. The barrel now sends each type of `Leaf` out under one flat
name, `Leaf_Box`, as a namespace does, and the path writes that name.
*/
#[test]
fn a_type_path_through_a_module_a_barrel_passes_on_writes_one_name() {
    let dir = temp_project("barrel-star");
    fs::write(dir.join("alloy.toml"), "[build]\n").unwrap();
    fs::write(
        dir.join("src/leaf.aly"),
        "export struct Box\n    n: number\nend\n\nexport struct Cell<T>\n    v: T\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/barrel.aly"),
        "import * as Leaf from \"./leaf\"\nexport { Leaf }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/use.aly"),
        "import * as B from \"./barrel\"\nimport { Leaf } from \"./barrel\"\n\nconst b: B.Leaf.Box = new B.Leaf.Box { n = 1 }\nconst c: Leaf.Cell<number> = new Leaf.Cell { v = 2 }\nprint(b.n, c.v)\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();

    assert!(report.is_clean(), "{report:?}");

    let barrel = fs::read_to_string(dir.join("build/barrel.luau")).unwrap();

    for alias in [
        "export type Leaf_Box = Leaf.Box",
        "export type Leaf_Cell<T> = Leaf.Cell<T>",
    ] {
        assert!(barrel.contains(alias), "{barrel}");
    }

    let text = fs::read_to_string(dir.join("build/use.luau")).unwrap();

    assert!(text.contains("const b: B.Leaf_Box ="), "{text}");
    assert!(text.contains("const c: Leaf_Cell<number> ="), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

/*
A remote's wire layout reads a type name through the imports of the file
that writes it. The layout took "the one project type of this name", so
a private `Inner` in a file nothing imports stripped the layout: the enum
slot went, the array item read `any`, a star path lost its wire, and the
declaring file stopped registering its table.

Every build compiles every file, so a change to a shape reaches the
remote file on the next build.
*/
#[test]
fn a_private_type_of_the_same_name_leaves_a_wire_layout_alone() {
    let dir = temp_project("wire-scope");
    fs::create_dir_all(dir.join("src/shared")).unwrap();
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(
        dir.join("src/shared/inner.aly"),
        "export struct Inner\n    n: number\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared/kind.aly"),
        "import { Inner } from \"./inner\"\nexport enum Kind\n    Big(Inner)\n    Small\nend\nexport struct Holder\n    k: Kind\n    list: { Inner }\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/net.aly"),
        "import * as K from \"./shared/kind\"\nimport { Holder } from \"./shared/kind\"\nimport * as S from \"./shared/inner\"\nexport remote R1(k: K.Kind) from client\nexport remote R2(h: Holder) from client\nexport remote R3(i: S.Inner) from client\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/other.aly"),
        "struct Inner\n    label: string\nend\nprint(new Inner { label = \"x\" })\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let build = || alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let report = build();
    assert!(report.is_clean(), "{report:?}");

    let read = |file: &str| fs::read_to_string(dir.join("dist").join(file)).unwrap();
    let net = read("net.luau");
    let inner = "{ fields = { { \"n\", \"f64\" } }, struct = \"shared/inner.aly:Inner\" }";

    for layout in [
        format!("slots = {{ Big = {{ {inner} }} }}"),
        format!("{{ \"list\", {{ item = {inner}, array = true }} }}"),
        "wire = { { fields = { { \"n\", \"f64\" } }, struct = S.Inner } }".to_string(),
    ] {
        assert!(net.contains(&layout), "{layout}\n{net}");
    }

    assert!(
        read("shared/inner.luau")
            .contains("__alloy.wire.types[\"shared/inner.aly:Inner\"] = Inner")
    );
    assert!(!read("other.luau").contains("wire.types"));

    // A field added to `Inner` reaches the layout on the next build.
    fs::write(
        dir.join("src/shared/inner.aly"),
        "export struct Inner\n    n: number\n    tag: string\nend\n",
    )
    .unwrap();
    let report = build();
    assert!(
        report.written.contains(&PathBuf::from("net.luau")),
        "{report:?}"
    );
    assert!(
        read("net.luau").contains("{ { \"n\", \"f64\" }, { \"tag\", \"str\" } }"),
        "{}",
        read("net.luau")
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
A `.server.aly` file fired a remote that goes from the client. The shared
module that declares it types both sides, so the call passed, and the
server called `FireClient` with the first argument at run time. A file
with a side runs there alone, so its wrong-side call is an error. A
shared file may run on either side and gets none.
*/
#[test]
fn a_side_file_cannot_use_a_remote_of_the_other_side() {
    let dir = temp_project("remote-side");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(
        dir.join("src/rem.aly"),
        "export remote Up(n: number) from client\nexport remote Down(n: number) from server\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/a.server.aly"),
        "import { Up as U, Down } from \"./rem\"\nimport * as R from \"./rem\"\nU.fire(1)\nR.Down.on(function(n) print(n) end)\nDown.fire_all(1)\nU.on(function(p, n) print(p, n) end)\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/b.client.aly"),
        "import { Up, Down } from \"./rem\"\nDown.fire(1)\nUp.fire_all(1)\nUp.fire(1)\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared.aly"),
        "import { Up, Down } from \"./rem\"\nlocal function go()\n    Up.fire(1)\n    Down.fire_all(2)\nend\nreturn go\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let got: Vec<(String, &str)> = report
        .diagnostics
        .iter()
        .map(|(file, d)| (file.to_string_lossy().into_owned(), d.message.as_str()))
        .collect();
    let at = |file: &str, message| (file.to_string(), message);

    assert_eq!(
        got,
        [
            at(
                "a.server.aly",
                "`U` goes from the client; the server cannot fire it"
            ),
            at(
                "a.server.aly",
                "`R.Down` goes from the server; the server cannot handle it"
            ),
            at(
                "b.client.aly",
                "`Down` goes from the server; the client cannot fire it"
            ),
            at(
                "b.client.aly",
                "`Up.fire_all` reaches the clients; only the server calls it"
            ),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
A parameter, a loop variable, or a local of a remote's name is some other
value, so a call through it is no fire of the remote. The side check read
the name alone and would report each of them. A use past their scopes is
the remote again and reports.
*/
#[test]
fn a_binding_that_shadows_a_remote_is_no_remote() {
    let dir = temp_project("remote-shadow");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(
        dir.join("src/rem.aly"),
        "export remote Up(n: number) from client\n",
    )
    .unwrap();
    let src = "import { Up } from \"./rem\"
local function relay(Up: { fire: (number) -> () })
    Up.fire(1)
end
for _, Up in { { fire = function(_n: number) end } } do
    Up.fire(2)
end
do
    local Up = { fire = function(_n: number) end }
    Up.fire(3)
end
Up.fire(4)
relay({ fire = function(_n: number) end })
";
    fs::write(dir.join("src/a.server.aly"), src).unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let got: Vec<(usize, &str)> = report
        .diagnostics
        .iter()
        .map(|(_, d)| {
            let line = src[..d.start as usize].matches('\n').count() + 1;

            (line, d.message.as_str())
        })
        .collect();

    assert_eq!(
        got,
        [(12, "`Up` goes from the client; the server cannot fire it")]
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
A remote in a namespace skipped the side check: the server fired
`Net.Up`, which goes from the client, and nothing reported it. The check
reads the remote by its path, in the file and through a named import, a
rename, or a star path. A parameter of the namespace's name is no remote.
*/
#[test]
fn a_remote_in_a_namespace_keeps_its_side() {
    let dir = temp_project("remote-namespace");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(
        dir.join("src/net.aly"),
        "export namespace Net\n    remote Up(id: string) from client\n    remote Down(n: number) from server\nend\n",
    )
    .unwrap();
    let src = "import { Net } from \"./net\"
import { Net as N } from \"./net\"
import * as S from \"./net\"
namespace Own
    namespace Inner
        remote Ping(n: number) from client
    end
end
Net.Up.fire(\"x\")
N.Down.on(function(n) print(n) end)
S.Net.Up.fire(\"y\")
Own.Inner.Ping.fire(1)
Net.Down.fire_all(1)
Own.Inner.Ping.on(function(p, n) print(p, n) end)
local function relay(Own: any)
    Own.Inner.Ping.fire(2)
end
relay(nil)
";
    fs::write(dir.join("src/a.server.aly"), src).unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let got: Vec<(usize, &str)> = report
        .diagnostics
        .iter()
        .map(|(_, d)| {
            let line = src[..d.start as usize].matches('\n').count() + 1;

            (line, d.message.as_str())
        })
        .collect();

    assert_eq!(
        got,
        [
            (
                9,
                "`Net.Up` goes from the client; the server cannot fire it"
            ),
            (
                10,
                "`N.Down` goes from the server; the server cannot handle it"
            ),
            (
                11,
                "`S.Net.Up` goes from the client; the server cannot fire it"
            ),
            (
                12,
                "`Own.Inner.Ping` goes from the client; the server cannot fire it"
            ),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
A barrel that passes a module of remotes on whole, `import * as Remotes`
then `export { Remotes }`, lost the remotes. The server fired a remote
that goes from the client, and nothing reported it. An `await` in a
plain handler of one reported that it needs an async context.
*/
#[test]
fn a_remote_through_a_star_barrel_keeps_its_side() {
    let dir = temp_project("remote-star-barrel");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(
        dir.join("src/rem.aly"),
        "export remote Up(n: number) from client\nexport remote function Buy(id: string): boolean from client\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/index.aly"),
        "import * as Remotes from \"./rem\"\nexport { Remotes }\n",
    )
    .unwrap();
    let src = "import { Remotes } from \"./index\"
import * as Idx from \"./index\"
async function load(id: string): boolean
    return id ~= \"\"
end
Remotes.Up.fire(1)
Idx.Remotes.Up.fire(2)
Remotes.Buy.on(function(player, id)
    return await load(id)
end)
";
    fs::write(dir.join("src/a.server.aly"), src).unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let got: Vec<(usize, &str)> = report
        .diagnostics
        .iter()
        .map(|(_, d)| {
            let line = src[..d.start as usize].matches('\n').count() + 1;

            (line, d.message.as_str())
        })
        .collect();

    assert_eq!(
        got,
        [
            (
                6,
                "`Remotes.Up` goes from the client; the server cannot fire it"
            ),
            (
                7,
                "`Idx.Remotes.Up` goes from the client; the server cannot fire it"
            ),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
A local that holds a remote skipped the side check: `const vote =
Net.Up` then `vote.fire("x")` passed on the server and called
`FireClient("x")`. An alias of a namespace, an alias of an alias, and a
local in a function body hold the remote too. A parameter of the alias's
name is some other value.
*/
#[test]
fn a_local_that_holds_a_remote_keeps_its_side() {
    let dir = temp_project("remote-alias");
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    fs::write(
        dir.join("src/net.aly"),
        "export namespace Net\n    remote Up(id: string) from client\n    remote Down(n: number) from server\nend\nexport remote Top(n: number) from client\n",
    )
    .unwrap();
    let src = "import { Net, Top } from \"./net\"
const vote = Net.Up
vote.fire(\"x\")
local top = Top
top.fire(1)
const n = Net
n.Up.fire(\"y\")
const again = n.Up
again.fire(\"z\")
vote.on(function(p, id) print(p, id) end)
local function relay(vote: any)
    vote.fire(\"fine\")
end
local function down()
    local d = Net.Down
    d.fire_all(1)
    d.on(function(k) print(k) end)
end
relay(nil)
down()
";
    fs::write(dir.join("src/a.server.aly"), src).unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    let got: Vec<(usize, &str)> = report
        .diagnostics
        .iter()
        .map(|(_, d)| {
            let line = src[..d.start as usize].matches('\n').count() + 1;

            (line, d.message.as_str())
        })
        .collect();

    assert_eq!(
        got,
        [
            (3, "`vote` goes from the client; the server cannot fire it"),
            (5, "`top` goes from the client; the server cannot fire it"),
            (7, "`n.Up` goes from the client; the server cannot fire it"),
            (9, "`again` goes from the client; the server cannot fire it"),
            (17, "`d` goes from the server; the server cannot handle it"),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
}

/*
An imported struct gives its derives to this file: a field of it clones
and serializes through it. The lookup took the first project struct of
the name. A private `Inner` in another file then decided it: a clone
shared the imported value, or called an `Inner.clone` that was nil.
*/
#[test]
fn a_private_struct_of_the_same_name_gives_no_derives() {
    let dir = temp_project("derive-scope");
    fs::create_dir_all(dir.join("src/shared")).unwrap();
    fs::write(dir.join("alloy.toml"), "[build]\nout = \"dist\"\n").unwrap();
    // `a.aly` sorts first, so its private structs came first.
    fs::write(
        dir.join("src/a.aly"),
        "struct Real\n    n: number\nend\n@derive(Clone)\nstruct Bare\n    n: number\nend\nprint(new Real { n = 0 }, new Bare { n = 0 })\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/shared/inner.aly"),
        "@derive(Clone)\nexport struct Real\n    n: number\nend\nexport struct Bare\n    n: number\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        "import { Real } from \"./shared/inner\"\nimport * as I from \"./shared/inner\"\n@derive(Clone)\nstruct Bag\n    real: Real\n    bare: I.Bare\nend\nprint(Bag)\n",
    )
    .unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run(&dir, &config.build, &config.emit).unwrap();
    assert!(report.is_clean(), "{report:?}");

    let main = fs::read_to_string(dir.join("dist/main.luau")).unwrap();
    assert!(main.contains("v.real = Real.clone(v.real)"), "{main}");
    assert!(!main.contains("I.Bare.clone"), "{main}");

    let _ = fs::remove_dir_all(&dir);
}

/*
The shapes of each module stay in memory between reads, keyed by the
text and by the file each import names. A module whose import names a
file that did not exist yet reads again once the file is there, and a
changed text reads again.
*/
#[test]
fn a_held_shape_reads_again_when_its_import_resolves() {
    let dir = temp_project("shape-cache");
    let base = dir.join("src");
    let main = base.join("main.aly");
    fs::write(
        &main,
        "import { Late } from \"./late\"\nexport struct Box\n    late: Late\nend\n",
    )
    .unwrap();

    let names = || {
        let (_, scopes) = alloy::build::struct_shapes(std::slice::from_ref(&main), &base, &[]);

        scopes[0].names.clone()
    };
    let bound = |local: &str| {
        vec![(
            local.to_string(),
            "late.aly".to_string(),
            "Late".to_string(),
        )]
    };
    assert!(names().is_empty());

    fs::write(
        base.join("late.aly"),
        "export struct Late\n    n: number\nend\n",
    )
    .unwrap();
    assert_eq!(names(), bound("Late"));

    fs::write(
        &main,
        "import { Late as L } from \"./late\"\nexport struct Box\n    late: L\nend\n",
    )
    .unwrap();
    assert_eq!(names(), bound("L"));

    let _ = fs::remove_dir_all(&dir);
}
