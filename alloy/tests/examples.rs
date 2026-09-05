//! Every example compiles with no diagnostic and keeps its line count.
//!
//! The examples are the feature corpus; `scripts/check-build.sh` then
//! proves the output is Luau the engine and the analyzer accept.

use std::path::{Path, PathBuf};

use alloy::EmitOptions;

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();

        if path.is_dir() {
            sources(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("aly" | "alx")
        ) {
            out.push(path);
        }
    }
}

#[test]
fn every_example_compiles_clean() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();
    assert!(files.len() > 20, "found {} examples", files.len());

    let mut failures = Vec::new();

    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let options = EmitOptions {
            file_name: name.clone(),
            definitions: name.ends_with(".d.aly"),
            ..EmitOptions::default()
        };

        let out = if name.ends_with(".alx") {
            alloy::compile_alx(&src, &options, alloy::luaux::Config::default()).map(|a| a.output)
        } else {
            alloy::compile_with(&src, &options)
        };

        match out {
            Ok(out) => {
                for d in &out.diagnostics {
                    failures.push(format!("{name}: byte {}: {}", d.start, d.message));
                }

                let want = src.matches('\n').count();
                let got = out.ship.matches('\n').count();

                if want != got {
                    failures.push(format!("{name}: {got} lines, source has {want}"));
                }
            }

            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The output is plain Luau, and plain Luau is Alloy that compiles to
/// itself: a second compile changes nothing and reports nothing.
#[test]
fn every_output_compiles_to_itself() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut files = Vec::new();
    sources(&root, &mut files);
    let mut failures = Vec::new();

    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let options = EmitOptions {
            file_name: name.clone(),
            definitions: name.ends_with(".d.aly"),
            ..EmitOptions::default()
        };
        let first = if name.ends_with(".alx") {
            alloy::compile_alx(&src, &options, alloy::luaux::Config::default()).map(|a| a.output)
        } else {
            alloy::compile_with(&src, &options)
        };
        let Ok(first) = first else { continue };
        let again = alloy::compile_with(&first.ship, &options).unwrap();

        if again.ship != first.ship {
            let at = again
                .ship
                .bytes()
                .zip(first.ship.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            let line = first.ship[..at].matches('\n').count() + 1;
            failures.push(format!("{name}: second compile differs at line {line}"));
        }

        for d in &again.diagnostics {
            let line = first.ship[..d.start as usize].matches('\n').count() + 1;
            failures.push(format!(
                "{name}: second compile reports line {line}: {}",
                d.message
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A damaged example never panics the compiler: bytes deleted, tokens
/// dropped, and the file cut short at many points.
#[test]
fn damaged_examples_never_panic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut files = Vec::new();
    sources(&root, &mut files);
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;

        seed
    };

    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();

        if name.ends_with(".alx") {
            continue;
        }

        let options = EmitOptions {
            file_name: name.clone(),
            definitions: name.ends_with(".d.aly"),
            ..EmitOptions::default()
        };

        for round in 0..40 {
            let mut text = src.clone();

            match round % 3 {
                0 => {
                    // Delete a run of bytes on a character boundary.
                    let at = (next() as usize) % text.len();
                    let start = (0..=at)
                        .rev()
                        .find(|i| text.is_char_boundary(*i))
                        .unwrap_or(0);
                    let len = (next() as usize) % 12 + 1;
                    let end = (start + len..=text.len())
                        .find(|i| text.is_char_boundary(*i))
                        .unwrap_or(text.len());
                    text.replace_range(start..end, "");
                }

                1 => {
                    let at = (next() as usize) % text.len();
                    let cut = (0..=at)
                        .rev()
                        .find(|i| text.is_char_boundary(*i))
                        .unwrap_or(0);
                    text.truncate(cut);
                }

                _ => {
                    let at = (next() as usize) % text.len();
                    let cut = (0..=at)
                        .rev()
                        .find(|i| text.is_char_boundary(*i))
                        .unwrap_or(0);
                    let pieces = ["(", "{", "match ", "?.", "$", "`", "->", "end ", "as", "?"];
                    text.insert_str(cut, pieces[(next() as usize) % pieces.len()]);
                }
            }

            let _ = alloy::compile_with(&text, &options);
        }
    }
}
