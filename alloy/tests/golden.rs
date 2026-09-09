//! Golden tests: every `.aly` under `tests/cases` compiles to the `.luau`
//! beside it, and every output has the line count of its source. A new
//! case is two files. A `<name>.check.luau` beside them, where one
//! exists, is the check artifact of the same source. A case whose name
//! starts with `global_` compiles as one file of a project, since
//! `global` needs one.

use std::fs;
use std::path::Path;

#[test]
fn every_case_matches_its_expected_output() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut count = 0;

    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();

        if path.extension().and_then(|e| e.to_str()) != Some("aly") {
            continue;
        }

        let src = fs::read_to_string(&path).unwrap();
        let expected = fs::read_to_string(path.with_extension("luau")).unwrap();
        // The file name reaches `$dbg` messages, so the goldens are
        // generated from inside the cases directory with the bare name.
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // `global` needs a project, so a case named for it compiles as
        // one file of one.
        let in_project = name.starts_with("global_");
        let options = alloy::EmitOptions {
            file_name: name,
            in_project,
            // A module beside the case that returns a value has no
            // export table, and the import forms read differently for
            // one. The resolve runs against the cases directory.
            plain_modules: alloy::modules::plain_modules(&src, &path, &[]),
            ..alloy::EmitOptions::default()
        };
        let out = alloy::compile_with(&src, &options).unwrap();

        assert!(
            out.diagnostics.is_empty(),
            "{}: diagnostics {:?}",
            path.display(),
            out.diagnostics
        );
        assert_eq!(
            out.ship,
            expected,
            "{} differs from its .luau",
            path.display()
        );
        assert_eq!(
            out.ship.lines().count(),
            src.lines().count(),
            "{}: line count changed",
            path.display()
        );

        // The check artifact, where the case pins it too.
        let check_path = path.with_extension("check.luau");

        if let Ok(want) = fs::read_to_string(&check_path) {
            assert_eq!(
                out.check,
                want,
                "{} differs from its .check.luau",
                path.display()
            );
            assert_eq!(
                out.check.lines().count(),
                src.lines().count(),
                "{}: check line count changed",
                path.display()
            );
        }

        count += 1;
    }

    assert!(count >= 4, "found {count} cases");
}
