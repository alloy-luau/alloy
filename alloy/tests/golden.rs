//! Golden tests: every `.aly` under `tests/cases` compiles to the `.luau`
//! beside it, and every output has the line count of its source. A new
//! case is two files.

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
        let options = alloy::EmitOptions {
            file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
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

        count += 1;
    }

    assert!(count >= 4, "found {count} cases");
}
