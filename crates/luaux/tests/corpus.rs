//! Phase 0 acceptance check (PLAN.md §5.2).
//!
//! Runs the lexer and LuauX detector over a corpus of *plain Luau* — code that
//! contains no LuauX at all — and requires zero detections. Every hit is a false
//! positive, and a false positive means luaux would try to compile valid Luau as
//! LuauX.
//!
//! The corpus lives outside the repository, so this test is opt-in:
//!
//! ```sh
//! git clone --depth 1 https://github.com/luau-lang/luau
//! git clone --depth 1 https://github.com/centau/vide
//! LUAUX_CORPUS=luau:vide cargo test -p luaux --test corpus -- --nocapture
//! ```
//!
//! Without `LUAUX_CORPUS` the test reports that it was skipped and passes, so
//! `cargo test` stays green on a bare checkout.

use std::path::{Path, PathBuf};

#[test]
fn corpus_contains_no_luaux() {
    let Ok(corpus) = std::env::var("LUAUX_CORPUS") else {
        eprintln!("skipped: set LUAUX_CORPUS to a ':'-separated list of paths");
        return;
    };

    let mut files = Vec::new();
    for root in corpus.split(':').filter(|path| !path.is_empty()) {
        collect(Path::new(root), &mut files).expect("read corpus");
    }
    files.sort();

    assert!(
        !files.is_empty(),
        "LUAUX_CORPUS matched no .luau/.lua files"
    );

    let mut scanned = 0usize;
    let mut skipped = 0usize;
    let mut findings = Vec::new();

    for file in &files {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };

        // luaux requires UTF-8 source; some Lua corpora embed raw bytes.
        let Ok(source) = String::from_utf8(bytes) else {
            skipped += 1;
            continue;
        };

        let tokens = match luaux::tokenize(&source) {
            Ok(tokens) => tokens,
            Err(error) => {
                // Corpora carry fixtures that are *meant* to fail lexing.
                if is_negative_fixture(file) {
                    skipped += 1;
                    continue;
                }
                panic!("{}: unexpected lex error: {error}", file.display());
            }
        };

        for site in luaux::find_luaux_sites(&source, &tokens) {
            findings.push(format!(
                "{}:{}: {}",
                file.display(),
                line_of(&source, site.offset),
                line_text(&source, site.offset)
            ));
        }

        scanned += 1;
    }

    eprintln!("corpus: {scanned} scanned, {skipped} skipped");

    assert!(
        findings.is_empty(),
        "{} false positive(s) — valid Luau read as LuauX:\n{}",
        findings.len(),
        findings.join("\n")
    );
}

/// Fixtures under a `fail/` directory are invalid on purpose.
fn is_negative_fixture(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "fail")
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }

    for entry in std::fs::read_dir(path)? {
        let entry = entry?.path();

        if entry.is_dir() {
            collect(&entry, out)?;
        } else if matches!(
            entry.extension().and_then(|ext| ext.to_str()),
            Some("luau") | Some("lua")
        ) {
            out.push(entry);
        }
    }

    Ok(())
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

fn line_text(source: &str, offset: usize) -> &str {
    let start = source[..offset].rfind('\n').map_or(0, |at| at + 1);
    let end = source[offset..]
        .find('\n')
        .map_or(source.len(), |at| offset + at);
    source[start..end].trim()
}
