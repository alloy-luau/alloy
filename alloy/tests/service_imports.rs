//! The Roblox services as imports: `import Players from "@game/Players"`
//! and `import { Players } from "@game"`. The emit, the message each
//! wrong form reads, and the old spellings, which still work.

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
    let source = "import Players from '@game/Players'\nimport { ReplicatedStorage, RunService as Run } from '@game'\n\nprint(Players, ReplicatedStorage, Run)\n";
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
fn the_old_spellings_lower_the_same_way() {
    let old = "import Players from 'game:Players'\nimport { ReplicatedStorage, RunService as Run } from 'game'\n";
    let new = "import Players from '@game/Players'\nimport { ReplicatedStorage, RunService as Run } from '@game'\n";

    assert_eq!(compile(old).ship, compile(new).ship);
    assert!(problems(old).is_empty(), "{:?}", problems(old));
}

#[test]
fn a_bare_name_from_the_alias_asks_for_braces() {
    for source in [
        "import Players from '@game'\n",
        "import Players from 'game'\n",
    ] {
        let found = problems(source);

        assert_eq!(found.len(), 1, "{source}: {found:?}");
        assert_eq!(found[0].kind, "ImportError");
        assert_eq!(
            found[0].message,
            "`'@game'` names every service; write `import { Players } from '@game'` \
             or `import Players from '@game/Players'`"
        );
    }
}

#[test]
fn a_list_from_one_service_asks_for_a_bare_name() {
    for source in [
        "import { Players } from '@game/Players'\n",
        "import { Players } from 'game:Players'\n",
    ] {
        let found = problems(source);

        assert_eq!(found.len(), 1, "{source}: {found:?}");
        assert_eq!(found[0].kind, "ImportError");
        assert_eq!(
            found[0].message,
            "`'@game/Players'` names one service; write `import Players from '@game/Players'`"
        );
    }
}

#[test]
fn a_name_that_is_no_service_names_the_one_it_meant() {
    // The name sits in the path for one form and in the braces for the
    // other, and the report covers what the reader wrote either way.
    for (source, at) in [
        ("import Playerz from '@game/Playerz'\n", "'@game/Playerz'"),
        ("import { Playerz } from '@game'\n", "Playerz"),
        ("import Playerz from 'game:Playerz'\n", "'game:Playerz'"),
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
fn a_first_segment_that_is_no_service_reports_whatever_follows() {
    // `@game/Nope/x` names no service, and nothing under a name the
    // DataModel has no service for resolves either.
    let source = "import { a } from '@game/Playerz/thing'\n";
    let found = problems(source);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "ImportError");
    assert_eq!(
        found[0].message,
        "`Playerz` is not a Roblox service; did you mean `Players`?"
    );
}

#[test]
fn a_path_past_a_service_is_a_module_path() {
    // The ship artifact writes `@game/ReplicatedStorage/Shared/x`, so
    // the form stays a module path and the resolver answers for it.
    // `@game` is reserved, so the message never asks for the alias.
    let source = "import { a } from '@game/ReplicatedStorage/Shared/economy'\n";
    let found = problems(source);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "UnknownModule");
    assert!(
        !found[0].message.contains("Roblox service"),
        "{}",
        found[0].message
    );
    assert!(
        !found[0].message.contains("no alias game"),
        "{}",
        found[0].message
    );
    assert!(
        found[0].message.contains("a place in the tree"),
        "{}",
        found[0].message
    );
}

#[test]
fn a_service_imported_twice_is_a_duplicate() {
    let source = "import Players from '@game/Players'\nimport { Players } from '@game'\n";
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

#[test]
fn the_old_spelling_draws_the_deprecation_lint_and_its_fix() {
    let source = "import Players from 'game:Players'\nimport { RunService } from \"game\"\nimport { a } from './x'\n";
    let out = compile(source);
    let hits: Vec<&alloy::Lint> = out
        .lints
        .iter()
        .filter(|l| l.name == "game_alias")
        .collect();

    assert_eq!(hits.len(), 2, "{:?}", out.lints);
    assert_eq!(
        hits[0].message,
        "`'game:Players'` is the old service path; write `'@game/Players'`"
    );
    assert_eq!(
        hits[1].message,
        "`\"game\"` is the old service path; write `\"@game\"`"
    );

    let owned: Vec<alloy::Lint> = hits.into_iter().cloned().collect();

    assert_eq!(
        alloy::lint::apply_fixes(source, &owned).0,
        "import Players from '@game/Players'\nimport { RunService } from \"@game\"\nimport { a } from './x'\n"
    );
}

#[test]
fn the_alias_form_draws_no_lint() {
    let source = "import Players from '@game/Players'\nimport { RunService } from \"@game\"\n";

    assert!(
        !compile(source).lints.iter().any(|l| l.name == "game_alias"),
        "the alias form is the one to write"
    );
}
