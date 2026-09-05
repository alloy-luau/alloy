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

#[test]
fn excludes_and_diagnostics_are_reported() {
    let dir = temp_project("report");
    fs::write(dir.join("src/keep.aly"), "local = 1\n").unwrap();
    fs::write(dir.join("src/skip.spec.aly"), "local = 1\n").unwrap();

    let build = Build {
        exclude: vec!["**/*.spec.aly".to_string()],
        ..Build::default()
    };
    let report = alloy::build::run(&dir, &build, &Emit::default()).unwrap();

    assert_eq!(report.skipped, vec![PathBuf::from("skip.spec.aly")]);
    assert_eq!(report.written, vec![PathBuf::from("keep.luau")]);
    assert_eq!(report.diagnostics.len(), 1, "the broken line is reported");
    assert!(
        dir.join("build/keep.luau").is_file(),
        "output is written even with diagnostics"
    );

    let _ = fs::remove_dir_all(&dir);
}
