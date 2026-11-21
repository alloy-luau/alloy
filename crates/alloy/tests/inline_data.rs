//! `import inline`: the document goes into the file that imports it,
//! and no module is required.

use alloy::EmitOptions;

const FLAG: &str = "AlloyExperimentalInlineDataSyntax = true\n";

/// Compiles `src` as `<dir>/src/main.aly`, with `game.json` and
/// `limits.toml` one folder up, outside the source folder.
fn compiled(name: &str, src: &str, fflags: &str) -> alloy::Output {
    let dir = std::env::temp_dir().join(format!("alloy-inline-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("game.json"),
        r#"{"coins": 5, "pets": [{"name": "cat"}], "max-players": 12}"#,
    )
    .unwrap();
    std::fs::write(dir.join("limits.toml"), "max = 12\n[spawn]\nx = 1.5\n").unwrap();
    std::fs::write(dir.join("broken.json"), "{").unwrap();

    let options = EmitOptions {
        fflags: toml::from_str(fflags).unwrap(),
        ..EmitOptions::default()
    }
    .imports(src, &dir.join("src/main.aly"), &[]);
    let out = alloy::compile_with(src, &options).unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    out
}

fn messages(out: &alloy::Output) -> Vec<&str> {
    out.diagnostics.iter().map(|d| d.message.as_str()).collect()
}

#[test]
fn an_inline_import_is_a_local_with_the_document() {
    let src = "import inline limits from \"../limits.toml\"\nimport inline { coins, pets as animals } from \"../game.json\"\nprint(limits, coins, animals)\n";
    let out = compiled("whole", src, FLAG);
    assert_eq!(messages(&out), [] as [&str; 0]);

    for want in [
        "local limits = { max = 12, spawn = { x = 1.5 } }",
        "local coins, animals = 5, { { name = \"cat\" } }",
    ] {
        assert!(out.ship.contains(want), "{want}\n{}", out.ship);
    }

    // No module ships for the file: no require, no import, no data ref.
    assert!(!out.ship.contains("require"), "{}", out.ship);
    assert!(out.imports.is_empty() && out.data_refs.is_empty());
    assert_eq!(out.ship.lines().count(), src.lines().count());
}

#[test]
fn an_inline_import_reports_what_it_cannot_read() {
    let src = "import inline { nope } from \"../game.json\"\nimport inline a from \"../gone.json\"\nimport inline b from \"../broken.json\"\nimport inline c from \"./m\"\nimport inline * as d from \"../game.json\"\nprint(nope, a, b, c, d)\n";
    let out = compiled("errors", src, FLAG);
    let got = messages(&out);

    for want in [
        "`../game.json` has no key `nope`",
        "no data file at `../gone.json`",
        "`import inline` reads a `.json` or `.toml` file",
        "`import inline` takes one name or one list in braces",
    ] {
        assert!(got.contains(&want), "{want}\n{got:?}");
    }

    assert!(
        got.iter()
            .any(|m| m.starts_with("data file `../broken.json` does not parse as JSON")),
        "{got:?}"
    );
}

#[test]
fn an_inline_import_needs_its_flag() {
    let out = compiled(
        "flag",
        "import inline a from \"../game.json\"\nprint(a)\n",
        "",
    );
    assert_eq!(
        messages(&out),
        [
            "`import inline` is not final; set `AlloyExperimentalInlineDataSyntax = true` under `[fflags]` in alloy.toml to use it"
        ]
    );
}

/// `inline` before `from` is a name, as it was before the modifier.
#[test]
fn a_module_under_the_name_inline_is_a_plain_import() {
    let out = compiled("name", "import inline from \"./m\"\nprint(inline)\n", "");
    assert!(out.ship.contains("require(\"./m\")"), "{}", out.ship);
}
