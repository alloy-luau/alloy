//! `global` left the language.
//!
//! A declaration that writes the word reports at the word, and the
//! report names the `export` declaration and the `import` line that
//! replace it. The word stays free as a name. See the globals-removal
//! RFC.

fn compile_as(src: &str, file: &str) -> alloy::Output {
    let options = alloy::EmitOptions {
        file_name: file.to_string(),
        ..alloy::EmitOptions::default()
    };

    alloy::compile_with(src, &options).unwrap()
}

/// Every message of one file, in order.
fn messages(src: &str, file: &str) -> Vec<String> {
    compile_as(src, file)
        .diagnostics
        .into_iter()
        .map(|d| d.message)
        .collect()
}

/// The report names the declaration kind the author wrote and the
/// module every reader imports from.
#[test]
fn a_global_declaration_names_the_export_and_the_import() {
    let cases = [
        (
            "global local counter = 0\n",
            "`global` is removed; declare `counter` with `export local` and write `import { counter } from \"./a\"` where it is read",
        ),
        (
            "global const LIMIT = 5\n",
            "`global` is removed; declare `LIMIT` with `export const` and write `import { LIMIT } from \"./a\"` where it is read",
        ),
        (
            "global function bump()\nend\n",
            "`global` is removed; declare `bump` with `export function` and write `import { bump } from \"./a\"` where it is read",
        ),
        (
            "global struct Vec2 as\n    x: number\nend\n",
            "`global` is removed; declare `Vec2` with `export struct` and write `import { Vec2 } from \"./a\"` where it is read",
        ),
        (
            "global type Id = number\n",
            "`global` is removed; declare `Id` with `export type` and write `import { Id } from \"./a\"` where it is read",
        ),
        (
            "global namespace Math as\n    public const PI = 3.14\nend\n",
            "`global` is removed; declare `Math` with `export namespace` and write `import { Math } from \"./a\"` where it is read",
        ),
        (
            // An attribute is written `@tag` wherever it is applied,
            // and the import list writes it the same way.
            "global attribute tag(name: string) on struct\n",
            "`global` is removed; declare `tag` with `export attribute` and write `import { @tag } from \"./a\"` where it is read",
        ),
        (
            "global local function helper()\nend\n",
            "`global` is removed; declare `helper` with `export local function` and write `import { helper } from \"./a\"` where it is read",
        ),
    ];

    for (src, message) in cases {
        let said = messages(src, "src/a.aly");
        assert_eq!(said, vec![message.to_string()], "{src}");
        // The family is the one the `import` belongs to.
        assert_eq!(alloy::docs::kind_for(message), "ImportError");
        assert_eq!(alloy::docs::code_for(message), Some("3.2"));
    }
}

/// The report sits on the word, so the code action can replace it.
#[test]
fn the_report_covers_the_keyword() {
    let src = "global const LIMIT = 5\n";
    let out = compile_as(src, "src/a.aly");
    let d = &out.diagnostics[0];

    assert_eq!(&src[d.start as usize..d.end as usize], "global");
}

/// An `impl` binds no name to import, and no import carries a macro.
#[test]
fn the_kinds_with_no_import_say_what_they_have() {
    let impls = messages("global impl Vector3 as\nend\n", "src/a.aly");
    assert_eq!(
        impls,
        vec![
            "`global` is removed; `export impl` reaches every file that imports this module"
                .to_string()
        ]
    );

    let macros = messages("global macro twice(x)\n    x + x\nend\n", "src/a.aly");
    assert_eq!(
        macros,
        vec!["`global` is removed; a macro is in scope in the file that declares it".to_string()]
    );
}

/// `export global` and `global export` each report once, on the word.
#[test]
fn the_stacked_forms_report_once() {
    for src in [
        "export global function f()\nend\n",
        "global export function f()\nend\n",
    ] {
        let said = messages(src, "src/a.aly");
        assert_eq!(said.len(), 1, "{src}: {said:?}");
        assert!(said[0].starts_with("`global` is removed;"), "{said:?}");
    }
}

/// The word is still a name: `local global = 1` compiles.
#[test]
fn global_is_free_as_a_name() {
    assert!(messages("local global = 1\nprint(global)\n", "src/a.aly").is_empty());
    assert!(messages("local t = { global = 1 }\nprint(t.global)\n", "src/a.aly").is_empty());
}

/// The emit writes `export` in the keyword's place, so the artifact the
/// checker reads is Luau even on a file that reports.
#[test]
fn the_artifact_holds_no_global_keyword() {
    for src in [
        "global type Id = number\n",
        "global const LIMIT = 5\n",
        "global function bump()\nend\n",
        "global struct Vec2 as\n    x: number\nend\n",
    ] {
        let out = compile_as(src, "src/a.aly");

        assert!(!out.check.contains("global"), "{src}: {}", out.check);
    }
}

/// What replaces a `global local`: an `export local` in one module.
///
/// The module holds the one slot, so a write through a function it
/// exports is the value every file reads back. A reader's `import`
/// binds the value at require time, the way Luau binds a `local`, so a
/// bare imported name is not a window on later writes.
///
/// The test runs the emitted tree with `luau`, and skips where luau is
/// not installed.
#[test]
fn an_export_local_is_one_slot_on_its_module() {
    use std::fs;
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("alloy-export-local-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/a.aly"),
        concat!(
            "--- The count every file shares.\n",
            "export local counter = 0\n\n",
            "--- Raises it by one.\n",
            "export function bump(): ()\n    counter += 1\nend\n\n",
            "--- Reads it.\n",
            "export function read(): number\n    return counter\nend\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/b.aly"),
        concat!(
            "import { bump, read, counter } from \"./a\"\n\n",
            "--- Writes the shared slot.\n",
            "export function go(): ()\n    bump()\n    bump()\nend\n\n",
            "--- What this file sees.\n",
            "export function seen(): (number, number)\n    return read(), counter\nend\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/main.aly"),
        concat!(
            "import { go, seen } from \"./b\"\n",
            "import { read } from \"./a\"\n\n",
            "go()\n",
            "local through, bare = seen()\n",
            "print(`{through} {bare} {read()}`)\n"
        ),
    )
    .unwrap();

    let config = alloy::config::Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::run_project(&dir, &config).unwrap();
    assert!(report.is_clean(), "{:?}", report.diagnostics);

    let run = Command::new("luau")
        .arg("main.luau")
        .current_dir(dir.join("build"))
        .output();

    let Ok(run) = run else {
        eprintln!("skipped: luau is not installed");

        return;
    };
    let out =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);

    // Two writes through the module's function reach every file that
    // calls back into it; the copy the import bound stays at 0.
    assert_eq!(out.trim(), "2 0 2", "{out}");

    let _ = fs::remove_dir_all(&dir);
}
