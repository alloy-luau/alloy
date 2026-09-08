//! `alloy check` reports what an import got wrong: a module that names
//! no file, a name the module does not export, and a name imported
//! twice. Agent A2's cases; the file name keeps them apart from the
//! golden suite.

use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alloy-a2-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    dir
}

#[test]
fn a_module_lists_what_it_exports() {
    let source = "export struct Point as\n    x: number\nend\n\nexport enum Kind as A, B end\n\nexport function make(): number\n    return 1\nend\n\nexport type Alias = number\n\nlocal hidden = 2\nexport { hidden }\n\nlocal private_one = 3\n";
    assert_eq!(
        alloy::modules::exported_names(source),
        vec!["Point", "Kind", "make", "Alias", "hidden"]
    );
}

#[test]
fn an_import_names_the_module_and_what_it_exports() {
    let dir = scratch("imports");
    std::fs::write(
        dir.join("lib.aly"),
        "export function used_one(n: number): number\n    return n\nend\n",
    )
    .unwrap();
    let source = "import { missing_name } from \"./lib\"\nimport { used_one } from \"./nowhere\"\nimport { used_one } from \"./lib\"\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    let problems = alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]);
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();

    assert_eq!(problems.len(), 3, "{messages:?}");
    assert_eq!(problems[0].kind, "ImportError");
    assert_eq!(
        problems[0].message,
        "\"./lib\" does not export `missing_name`; it exports `used_one`"
    );
    // The range covers the name, not the whole statement.
    assert_eq!(
        &source[problems[0].start as usize..problems[0].end as usize],
        "missing_name"
    );
    assert_eq!(problems[1].kind, "UnknownModule");
    assert!(
        problems[1]
            .message
            .starts_with("\"./nowhere\" names no module")
    );
    assert_eq!(problems[2].kind, "ImportError");
    assert_eq!(
        problems[2].message,
        "`used_one` is already imported in this file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_luau_module_is_not_checked_for_names() {
    let dir = scratch("plain");
    std::fs::write(dir.join("util.luau"), "return { f = function() end }\n").unwrap();
    let source = "import { f } from \"./util\"\n";
    let main = dir.join("main.aly");
    std::fs::write(&main, source).unwrap();

    assert!(alloy::modules::import_problems(source, Path::new("main.aly"), &main, &[]).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
