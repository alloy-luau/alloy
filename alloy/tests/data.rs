//! `alloy build` over a project that imports JSON and TOML files: each
//! named data file becomes a `.luau` module in the output, the emitted
//! requires drop the extension, and a bad data file is a diagnostic on
//! the import.

use std::fs;
use std::path::{Path, PathBuf};

use alloy::config::Config;

/// A copy of the fixture in the temp directory, so the build writes
/// nothing into the repository.
fn fixture(name: &str) -> PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/data");
    let dir = std::env::temp_dir().join(format!("alloy-data-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy_tree(&from, &dir);

    dir
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();

    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());

        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn build(dir: &Path) -> alloy::build::Report {
    let config = Config::load(&dir.join("alloy.toml")).unwrap();

    alloy::build::run_project(dir, &config).unwrap()
}

#[test]
fn named_data_files_build_as_modules() {
    let dir = fixture("modules");
    fs::create_dir_all(dir.join("build")).unwrap();
    fs::write(dir.join("build/stale.luau"), "-- old\n").unwrap();

    let report = build(&dir);
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let mut data = report.data.clone();
    data.sort();
    assert_eq!(
        data,
        vec![
            PathBuf::from("cfg.luau"),
            PathBuf::from("config.luau"),
            PathBuf::from("data.luau"),
        ]
    );
    assert!(
        !dir.join("build/unused.luau").exists(),
        "a data file nothing names is not converted"
    );
    assert!(
        !dir.join("build/default.project.luau").exists(),
        "a project file is not converted"
    );
    assert!(!dir.join("build/stale.luau").exists(), "clean ran");

    assert_eq!(
        fs::read_to_string(dir.join("build/data.luau")).unwrap(),
        "return {\n    name = \"game\",\n    [\"max-players\"] = 12,\n    [\"end\"] = true,\n    tags = {\n        \"a\",\n        \"b\",\n    },\n    pets = {\n        {\n            name = \"cat\",\n            legs = 4,\n        },\n        {\n            name = \"snake\",\n            legs = 0,\n        },\n    },\n    nothing = nil,\n    ratio = 0.5,\n    whole = 2,\n}\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("build/config.luau")).unwrap(),
        "return {\n    title = \"Alloy\",\n    coins = 100,\n    limits = {\n        max = 10,\n        min = 1,\n    },\n    pets = {\n        {\n            name = \"cat\",\n        },\n        {\n            name = \"dog\",\n        },\n    },\n}\n"
    );

    let main = fs::read_to_string(dir.join("build/main.luau")).unwrap();
    let source = fs::read_to_string(dir.join("src/main.aly")).unwrap();
    assert_eq!(main.lines().count(), source.lines().count(), "{main}");
    assert!(main.contains("local data = require(\"./data\")"), "{main}");
    assert!(main.contains("require(\"./config\")"), "{main}");
    assert!(!main.contains(".json") && !main.contains(".toml"), "{main}");
    assert!(
        main.contains("local x = require(\"./data\")") || main.contains("= require(\"./data\")\n"),
        "{main}"
    );
    assert!(main.contains("local y = require(\"./config\")"), "{main}");

    let deep = fs::read_to_string(dir.join("build/nested/deep.luau")).unwrap();
    assert!(
        deep.starts_with("local cfg = require(\"../cfg\") local few = cfg.coins"),
        "{deep}"
    );

    // A second build keeps the module: `clean` leaves what a source names.
    let report = build(&dir);
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert!(dir.join("build/data.luau").is_file());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_data_file_is_a_diagnostic_on_the_import() {
    let dir = fixture("errors");
    fs::write(dir.join("src/broken.json"), "{ \"a\": 1, }\n").unwrap();
    fs::write(dir.join("src/twin.toml"), "a = 1\n").unwrap();
    fs::write(dir.join("src/twin.aly"), "return 1\n").unwrap();
    fs::write(
        dir.join("src/errors.aly"),
        "import a from \"./missing.json\"\nimport b from \"./broken.json\"\nimport c from \"./twin.toml\"\nimport d from \"../outside.json\"\nprint(a, b, c, d)\n",
    )
    .unwrap();

    let report = build(&dir);
    let messages: Vec<(String, String)> = report
        .diagnostics
        .iter()
        .map(|(p, d)| (p.to_string_lossy().into_owned(), d.message.clone()))
        .collect();
    assert_eq!(messages.len(), 4, "{messages:?}");
    assert!(
        messages.iter().all(|(p, _)| p == "errors.aly"),
        "{messages:?}"
    );
    assert_eq!(
        messages[0].1,
        "\"./missing.json\" names no module; no JSON file at src/missing.json"
    );
    assert!(
        messages[1]
            .1
            .starts_with("data file src/broken.json does not parse as JSON: ")
            && messages[1].1.contains("line 1"),
        "{}",
        messages[1].1
    );
    assert_eq!(
        messages[2].1,
        "data file src/twin.toml and src/twin.aly both build src/twin.luau; rename one"
    );
    assert!(
        messages[3]
            .1
            .starts_with("data file \"../outside.json\" lies outside [build] in"),
        "{}",
        messages[3].1
    );

    // Each diagnostic sits on its path literal.
    let source = fs::read_to_string(dir.join("src/errors.aly")).unwrap();
    let first = &report.diagnostics[0].1;
    assert_eq!(
        &source[first.start as usize..first.end as usize],
        "\"./missing.json\""
    );

    assert!(!dir.join("build/broken.luau").exists());
    assert_eq!(
        fs::read_to_string(dir.join("build/twin.luau")).unwrap(),
        "return 1\n",
        "the module wins the name"
    );

    let _ = fs::remove_dir_all(&dir);
}
