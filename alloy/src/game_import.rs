//! The Roblox services as imports: `import Players from "game:Players"`
//! and `import { Players, ReplicatedStorage } from "game"`.
//!
//! Both forms lower to `local Players = game:GetService("Players")` on
//! the import's own line, so the analyzer types the binding as the
//! service class the Roblox definitions declare.

use crate::roblox_services::SERVICES;

/// What a `game` import path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GamePath {
    /// `"game"`: the names in braces are the services.
    Every,
    /// `"game:Players"`: one service, bound under the name written.
    One(String),
}

/// The `game` path a spec names, without its quotes. `None` for any
/// other spec, which is a module path.
pub fn game_path(spec: &str) -> Option<GamePath> {
    match spec.split_once(':') {
        None => (spec == "game").then_some(GamePath::Every),

        Some(("game", service)) => Some(GamePath::One(service.to_string())),

        Some(_) => None,
    }
}

/// Whether a spec names a service rather than a module.
pub fn is_game_spec(spec: &str) -> bool {
    game_path(spec).is_some()
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

/// `import X from "game"` names no service, because `"game"` is every
/// service. The message gives both forms that do name one.
pub fn braces_message(spec: &str, quote: char, name: &str) -> String {
    format!(
        "`{quote}{spec}{quote}` names every service; \
         write `import {{ {name} }} from {quote}{spec}{quote}` \
         or `import {name} from {quote}game:{name}{quote}`"
    )
}

/// `import { X } from "game:Players"` puts a list where one name goes.
pub fn single_message(spec: &str, quote: char, service: &str) -> String {
    format!(
        "`{quote}{spec}{quote}` names one service; \
         write `import {service} from {quote}{spec}{quote}`"
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
    fn a_game_path_splits_on_the_colon() {
        assert_eq!(game_path("game"), Some(GamePath::Every));
        assert_eq!(
            game_path("game:Players"),
            Some(GamePath::One("Players".into()))
        );
        assert_eq!(game_path("./game"), None);
        assert_eq!(game_path("@pkg/game"), None);
        assert_eq!(game_path("gamer"), None);
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
            braces_message("game", '\'', "X"),
            "`'game'` names every service; write `import { X } from 'game'` \
             or `import X from 'game:X'`"
        );
        assert_eq!(
            single_message("game:Players", '\'', "Players"),
            "`'game:Players'` names one service; write `import Players from 'game:Players'`"
        );
        assert_eq!(
            unknown_message("Playerz"),
            "`Playerz` is not a Roblox service; did you mean `Players`?"
        );
    }
}
