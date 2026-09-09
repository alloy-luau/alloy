//! The Roblox services as imports: `import Players from "game:Players"`
//! and `import { Players } from "game"`. The emit, and the message each
//! wrong form reads.

use std::path::Path;

fn problems(source: &str) -> Vec<alloy::modules::ImportProblem> {
    alloy::modules::import_problems(
        source,
        Path::new("main.aly"),
        Path::new("/nowhere/main.aly"),
        &[],
    )
}

fn compile(source: &str) -> alloy::Output {
    alloy::compile_with(source, &alloy::EmitOptions::default()).unwrap()
}

#[test]
fn both_forms_bind_get_service_on_the_import_line() {
    let source = "import Players from 'game:Players'\nimport { ReplicatedStorage, RunService as Run } from 'game'\n\nprint(Players, ReplicatedStorage, Run)\n";
    let out = compile(source);

    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        out.ship,
        "local Players = game:GetService(\"Players\")\nlocal ReplicatedStorage = game:GetService(\"ReplicatedStorage\") local Run = game:GetService(\"RunService\")\n\nprint(Players, ReplicatedStorage, Run)\n"
    );
    // The check artifact is what the analyzer reads, and it is the
    // same text: `game:GetService` is what ships.
    assert_eq!(out.check, out.ship);
    assert_eq!(out.ship.lines().count(), source.lines().count());
    assert!(problems(source).is_empty());
}

#[test]
fn a_bare_name_from_game_asks_for_braces() {
    let source = "import Players from 'game'\n";
    let found = problems(source);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "ImportError");
    assert_eq!(
        found[0].message,
        "`'game'` names every service; write `import { Players } from 'game'` \
         or `import Players from 'game:Players'`"
    );
}

#[test]
fn a_list_from_one_service_asks_for_a_bare_name() {
    let source = "import { Players } from 'game:Players'\n";
    let found = problems(source);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "ImportError");
    assert_eq!(
        found[0].message,
        "`'game:Players'` names one service; write `import Players from 'game:Players'`"
    );
}

#[test]
fn a_name_that_is_no_service_names_the_one_it_meant() {
    // The name sits in the path for one form and in the braces for the
    // other, and the report covers what the reader wrote either way.
    for (source, at) in [
        ("import Playerz from 'game:Playerz'\n", "'game:Playerz'"),
        ("import { Playerz } from 'game'\n", "Playerz"),
    ] {
        let found = problems(source);

        assert_eq!(found.len(), 1, "{source}: {found:?}");
        assert_eq!(
            found[0].message,
            "`Playerz` is not a Roblox service; did you mean `Players`?"
        );
        assert_eq!(&source[found[0].start as usize..found[0].end as usize], at);
    }
}

#[test]
fn a_service_imported_twice_is_a_duplicate() {
    let source = "import Players from 'game:Players'\nimport { Players } from 'game'\n";
    let found = problems(source);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].message,
        "`Players` is already imported in this file"
    );
}

#[test]
fn a_module_path_that_starts_with_game_is_still_a_module() {
    // `gamer` and `./game` name files, so the resolver reports them.
    let source = "import { a } from './game'\n";
    let found = problems(source);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "UnknownModule");
}
