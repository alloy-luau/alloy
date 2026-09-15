//! The Roblox services as imports: `import Players from "@game/Players"`
//! and `import { Players, ReplicatedStorage } from "@game"`.
//!
//! Both forms lower to `local Players = game:GetService("Players")` on
//! the import's own line, so the analyzer types the binding as the
//! service class the Roblox definitions declare.
//!
//! `@game` is one namespace with two readings, and the segment after
//! the service decides which. `"@game/ReplicatedStorage"` is the
//! service. `"@game/ReplicatedStorage/Shared/economy"` is an instance
//! path, which resolves as a module does.
//!
//! `"game"` and `"game:Players"` are the spellings of the release
//! before this one. They parse and lower the same way, and the
//! `game_alias` lint asks for the alias form.

use crate::roblox_services::SERVICES;

/// The alias every service import starts from.
pub const ALIAS: &str = "@game";

/// What a `@game` import path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GamePath {
    /// `"@game"`: the names in braces are the services.
    Every,
    /// `"@game/Players"`: one service, bound under the name written.
    One(String),
}

/// The `@game` path a spec names, without its quotes. `None` for any
/// other spec, which is a module path.
///
/// A path with more than one segment after `@game` names an instance,
/// not a service, so it is a module path. The one exception is a first
/// segment that names no service: nothing under it can resolve either,
/// so it comes back as `One` and the caller reports the name.
pub fn game_path(spec: &str) -> Option<GamePath> {
    if let Some(rest) = spec.strip_prefix(ALIAS) {
        if rest.is_empty() {
            return Some(GamePath::Every);
        }

        let rest = rest.strip_prefix('/')?;
        let (first, tail) = rest.split_once('/').unwrap_or((rest, ""));

        if first.is_empty() {
            return None;
        }

        if !tail.is_empty() && is_service(first) {
            return None;
        }

        return Some(GamePath::One(first.to_string()));
    }

    // The old spellings, kept for one release.
    match spec.split_once(':') {
        None => (spec == "game").then_some(GamePath::Every),

        Some(("game", service)) => Some(GamePath::One(service.to_string())),

        Some(_) => None,
    }
}

/// Whether a spec is one of the spellings the release before this one
/// used: `"game"` or `"game:Players"`. The `game_alias` lint reports
/// these, and `alias_form` gives the spelling to write instead.
pub fn is_old_spelling(spec: &str) -> bool {
    spec == "game" || spec.starts_with("game:")
}

/// The alias spelling of an old spec: `"game"` becomes `"@game"` and
/// `"game:Players"` becomes `"@game/Players"`. `None` for a spec that
/// is already the alias form, or no service path at all.
pub fn alias_form(spec: &str) -> Option<String> {
    match spec {
        "game" => Some(ALIAS.to_string()),

        _ => spec
            .strip_prefix("game:")
            .map(|service| format!("{ALIAS}/{service}")),
    }
}

/// Whether a name is one of the services the definitions declare.
pub fn is_service(name: &str) -> bool {
    SERVICES.binary_search(&name).is_ok()
}

/// The service a misspelling is nearest to, for a "did you mean".
pub fn nearest_service(name: &str) -> Option<&'static str> {
    SERVICES
        .iter()
        .map(|s| (edit_distance(s, name), *s))
        .filter(|(d, _)| *d <= 2 && *d < name.len())
        .min()
        .map(|(_, s)| s)
}

/// `import X from "@game"` names no service, because `"@game"` is
/// every service. The message gives both forms that do name one. It
/// quotes the alias form whichever spelling the file wrote.
pub fn braces_message(quote: char, name: &str) -> String {
    format!(
        "`{quote}{ALIAS}{quote}` names every service; \
         write `import {{ {name} }} from {quote}{ALIAS}{quote}` \
         or `import {name} from {quote}{ALIAS}/{name}{quote}`"
    )
}

/// `import { X } from "@game/Players"` puts a list where one name goes.
/// The message quotes the alias form whichever spelling the file wrote.
pub fn single_message(quote: char, service: &str) -> String {
    format!(
        "`{quote}{ALIAS}/{service}{quote}` names one service; \
         write `import {service} from {quote}{ALIAS}/{service}{quote}`"
    )
}

/// A name that is no service, with the nearest one when there is one.
pub fn unknown_message(name: &str) -> String {
    match nearest_service(name) {
        Some(near) => format!("`{name}` is not a Roblox service; did you mean `{near}`?"),

        None => format!("`{name}` is not a Roblox service"),
    }
}

/// The line one service binding lowers to.
pub fn get_service(local: &str, service: &str) -> String {
    format!("local {local} = game:GetService(\"{service}\")")
}

/// One line about a service, for a completion item and a hover. The
/// vendored definitions carry no text of their own, so the line names
/// the class and what it extends.
pub fn service_summary(name: &str) -> String {
    let parent = crate::roblox_props::CLASSES
        .binary_search_by_key(&name, |c| c.name)
        .ok()
        .map(|at| crate::roblox_props::CLASSES[at].parent)
        .filter(|p| !p.is_empty());

    match parent {
        Some(parent) => format!("`{name}`: a Roblox service. The class extends `{parent}`."),

        None => format!("`{name}`: a Roblox service."),
    }
}

/// The edit distance of two names, for a "did you mean".
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;

        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(previous + cost);
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }

    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_alias_names_every_service_and_one() {
        assert_eq!(game_path("@game"), Some(GamePath::Every));
        assert_eq!(
            game_path("@game/Players"),
            Some(GamePath::One("Players".into()))
        );
        assert_eq!(game_path("./game"), None);
        assert_eq!(game_path("@pkg/game"), None);
        assert_eq!(game_path("gamer"), None);
        assert_eq!(game_path("@gamer/x"), None);
        assert_eq!(game_path("@game/"), None);
    }

    #[test]
    fn a_path_past_a_service_is_an_instance_path() {
        // The ship artifact writes this form, and it resolves the way
        // a module path does, so it is no service import.
        assert_eq!(game_path("@game/ReplicatedStorage/Shared/economy"), None);
        assert_eq!(game_path("@game/ServerScriptService/Server"), None);
        // A first segment that names no service resolves nowhere, so
        // the caller reports it whatever follows.
        assert_eq!(game_path("@game/Nope"), Some(GamePath::One("Nope".into())));
        assert_eq!(
            game_path("@game/Nope/x"),
            Some(GamePath::One("Nope".into()))
        );
    }

    #[test]
    fn the_old_spellings_still_parse() {
        assert_eq!(game_path("game"), Some(GamePath::Every));
        assert_eq!(
            game_path("game:Players"),
            Some(GamePath::One("Players".into()))
        );
        assert!(is_old_spelling("game"));
        assert!(is_old_spelling("game:Players"));
        assert!(!is_old_spelling("@game"));
        assert_eq!(alias_form("game").as_deref(), Some("@game"));
        assert_eq!(alias_form("game:Players").as_deref(), Some("@game/Players"));
        assert_eq!(alias_form("@game"), None);
    }

    #[test]
    fn the_service_list_is_sorted_and_holds_the_common_names() {
        assert!(SERVICES.windows(2).all(|w| w[0] < w[1]));

        for name in ["Players", "ReplicatedStorage", "Workspace", "TweenService"] {
            assert!(is_service(name), "{name}");
        }

        assert!(!is_service("Part"));
    }

    #[test]
    fn a_misspelling_names_the_service_it_meant() {
        assert_eq!(nearest_service("Playerz"), Some("Players"));
        assert_eq!(nearest_service("TweenServce"), Some("TweenService"));
        assert_eq!(nearest_service("Zzzzzzzzzzzz"), None);
    }

    #[test]
    fn each_message_reads_as_written() {
        assert_eq!(
            braces_message('\'', "X"),
            "`'@game'` names every service; write `import { X } from '@game'` \
             or `import X from '@game/X'`"
        );
        assert_eq!(
            single_message('\'', "Players"),
            "`'@game/Players'` names one service; write `import Players from '@game/Players'`"
        );
        assert_eq!(
            unknown_message("Playerz"),
            "`Playerz` is not a Roblox service; did you mean `Players`?"
        );
    }
}
