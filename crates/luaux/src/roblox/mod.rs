//! Roblox class knowledge, from the API dump.
//!
//! Answers the questions the compiler asks of the Roblox API:
//!
//! * Is this tag an intrinsic or a component? (PLAN.md §3.1)
//! * Does this class have a `Text` property? Bare text children lower to it
//!   (PLAN.md §6.2), and it is an error on a class that has none.
//! * Is this attribute a real property, or an event?
//!
//! Tables are generated — see `generated.rs` and `scripts/generate_api_tables.py`.
//! They store each class's *own* members plus its superclass, and inheritance is
//! resolved by walking that chain here. Flattening at generation time would be
//! roughly ten times larger for no gain.

mod generated;

use generated::{ClassInfo, CLASSES, DEPRECATED};
use std::collections::HashMap;
use std::sync::OnceLock;

fn index() -> &'static HashMap<&'static str, &'static ClassInfo> {
    static INDEX: OnceLock<HashMap<&'static str, &'static ClassInfo>> = OnceLock::new();
    INDEX.get_or_init(|| CLASSES.iter().map(|class| (class.name, class)).collect())
}

fn lookup(name: &str) -> Option<&'static ClassInfo> {
    index().get(name).copied()
}

/// Walks a class and its ancestors.
fn ancestry(name: &str) -> impl Iterator<Item = &'static ClassInfo> {
    let mut current = lookup(name);

    std::iter::from_fn(move || {
        let class = current?;
        current = lookup(class.superclass);
        Some(class)
    })
}

/// Whether Roblox marks this member name deprecated.
///
/// Old spellings live alongside modern ones (`brickColor` beside `BrickColor`),
/// and they differ only by case — so a casing rename merges the pair and this
/// is what decides which survives (docs/adr/0001-casing-key.md).
pub fn is_deprecated(member: &str) -> bool {
    DEPRECATED.binary_search(&member).is_ok()
}

/// Whether the name is a class that `Instance.new` can create — i.e. usable as
/// an intrinsic element.
pub fn is_class(name: &str) -> bool {
    lookup(name).is_some_and(|class| class.creatable)
}

/// Whether the class, or anything it inherits from, declares a settable
/// property of this name.
pub fn has_property(class: &str, property: &str) -> bool {
    ancestry(class).any(|info| info.properties.contains(&property))
}

/// Whether the class, or anything it inherits from, declares an event of this
/// name. Vide connects a function on an event key rather than assigning it.
pub fn is_event(class: &str, name: &str) -> bool {
    ancestry(class).any(|info| info.events.contains(&name))
}

pub fn has_text_property(name: &str) -> bool {
    has_property(name, "Text")
}

/// Every class `Instance.new` can create, in name order.
///
/// The compiler only ever asks about one name at a time, so this exists for
/// tooling: a language server completing `<Fra` has to offer the whole list, and
/// there is no way to recover it from the by-name queries above.
pub fn creatable_classes() -> impl Iterator<Item = &'static str> {
    CLASSES
        .iter()
        .filter(|class| class.creatable)
        .map(|class| class.name)
}

/// Every settable property of a class, including inherited ones.
pub fn properties(class: &str) -> impl Iterator<Item = &'static str> {
    ancestry(class).flat_map(|info| info.properties.iter().copied())
}

/// Every event of a class, including inherited ones.
pub fn events(class: &str) -> impl Iterator<Item = &'static str> {
    ancestry(class).flat_map(|info| info.events.iter().copied())
}

/// The class this one inherits from, or `None` at the root and for names that
/// are not classes at all.
///
/// Only names the tables actually know are returned. `Instance` names `Object`
/// as its superclass and the dump carries no such class, so the chain ends there
/// — which is exactly where [`ancestry`] stops too.
pub fn superclass(class: &str) -> Option<&'static str> {
    let info = lookup(class)?;
    lookup(info.superclass).map(|parent| parent.name)
}

/// Whether any class declares a settable property or event of this name.
///
/// A global `[properties]` alias in `luaux.toml` is not tied to one class, so
/// validating its key means asking whether it exists anywhere.
pub fn is_member_name(name: &str) -> bool {
    CLASSES
        .iter()
        .any(|class| class.properties.contains(&name) || class.events.contains(&name))
}

/// Nearest member name across every class, for did-you-mean on global aliases.
pub fn closest_member_anywhere(name: &str) -> Option<&'static str> {
    let needle = name.to_lowercase();

    CLASSES
        .iter()
        .flat_map(|class| class.properties.iter().chain(class.events.iter()))
        .map(|member| (*member, edit_distance(&needle, &member.to_lowercase())))
        .filter(|(candidate, distance)| {
            let budget = (candidate.len().min(name.len()) / 4).clamp(1, 3);
            *distance <= budget
        })
        .min_by_key(|(candidate, distance)| (*distance, candidate.len(), *candidate))
        .map(|(candidate, _)| candidate)
}

/// Nearest creatable class name, for did-you-mean diagnostics.
///
/// With ~350 creatable classes a loose threshold suggests nonsense — plain
/// Levenshtein put `Receipt` within reach of `Script`. So the budget scales with
/// the shorter name and caps at 3, and comparison is case-insensitive so
/// `<textlabel>` still finds `TextLabel`.
pub fn closest_class(name: &str) -> Option<&'static str> {
    let needle = name.to_lowercase();

    CLASSES
        .iter()
        .filter(|class| class.creatable)
        .map(|class| {
            (
                class.name,
                edit_distance(&needle, &class.name.to_lowercase()),
            )
        })
        .filter(|(candidate, distance)| {
            let budget = (candidate.len().min(name.len()) / 4).clamp(1, 3);
            *distance <= budget
        })
        .min_by_key(|(candidate, distance)| (*distance, candidate.len()))
        .map(|(candidate, _)| candidate)
}

/// Nearest settable property or event on a class, for did-you-mean on
/// attributes.
///
/// Edit distance alone is not enough here. Roblox qualifies many property names
/// with a prefix — `Color3` is ten edits from `BackgroundColor3` — so a
/// containment match is tried first, which is what turns the common
/// `<Frame Color3={…}>` slip into a useful suggestion.
pub fn closest_members(class: &str, name: &str) -> Vec<&'static str> {
    const LIMIT: usize = 3;

    let needle = name.to_lowercase();
    let members =
        || ancestry(class).flat_map(|info| info.properties.iter().chain(info.events.iter()));

    let mut contained: Vec<&'static str> = members()
        .filter(|member| member.to_lowercase().contains(&needle))
        .copied()
        .collect();

    if !contained.is_empty() {
        // Several qualified names often contain the same stem — `Color3` sits
        // inside both `BorderColor3` and `BackgroundColor3` — and nothing here
        // can tell which was meant, so offer them rather than pick one.
        contained.sort_by_key(|member| (member.len(), *member));
        contained.dedup();
        contained.truncate(LIMIT);
        return contained;
    }

    let mut scored: Vec<(&'static str, usize)> = members()
        .map(|member| (*member, edit_distance(&needle, &member.to_lowercase())))
        .filter(|(candidate, distance)| {
            let budget = (candidate.len().min(name.len()) / 4).clamp(1, 3);
            *distance <= budget
        })
        .collect();

    scored.sort_by_key(|(candidate, distance)| (*distance, candidate.len(), *candidate));
    scored.truncate(LIMIT);
    scored.into_iter().map(|(candidate, _)| candidate).collect()
}

/// Optimal string alignment distance — Levenshtein plus transposition.
///
/// Transposition matters: `Frmae` for `Frame` is one slip, but plain
/// Levenshtein charges it as two substitutions, which pushes the most common
/// kind of typo out of a tight budget.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();

    let mut distance = vec![vec![0usize; b.len() + 1]; a.len() + 1];

    for (i, row) in distance.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in distance[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);

            distance[i][j] = (distance[i - 1][j] + 1)
                .min(distance[i][j - 1] + 1)
                .min(distance[i - 1][j - 1] + cost);

            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                distance[i][j] = distance[i][j].min(distance[i - 2][j - 2] + 1);
            }
        }
    }

    distance[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_creatable_classes() {
        assert!(is_class("Frame"));
        assert!(is_class("UICorner"));
        assert!(is_class("ScrollingFrame"));
        assert!(is_class("Folder"));
        assert!(!is_class("Receipt"));
    }

    #[test]
    fn services_and_abstract_classes_are_not_intrinsics() {
        // Both exist in the dump but cannot be constructed.
        assert!(!is_class("Players"));
        assert!(!is_class("GuiObject"));
        assert!(!is_class("Instance"));
    }

    #[test]
    fn resolves_properties_through_inheritance() {
        // Declared on the class itself.
        assert!(has_property("TextLabel", "Text"));
        // Declared on GuiObject, several levels up.
        assert!(has_property("TextLabel", "BackgroundColor3"));
        assert!(has_property("Frame", "BackgroundColor3"));
        // Declared on Instance, at the root.
        assert!(has_property("Frame", "Name"));
        assert!(!has_property("Frame", "Text"));
        assert!(!has_property("Frame", "Nonsense"));
    }

    #[test]
    fn read_only_properties_are_not_settable() {
        // ContentText is ReadOnly, so assigning it would fail at runtime.
        assert!(!has_property("TextLabel", "ContentText"));
    }

    #[test]
    fn resolves_events_through_inheritance() {
        // Activated is declared on GuiButton, which TextButton inherits.
        assert!(is_event("TextButton", "Activated"));
        assert!(is_event("TextButton", "MouseButton1Click"));
        assert!(!is_event("Frame", "Activated"));
        // An event is not a settable property, and vice versa.
        assert!(!has_property("TextButton", "Activated"));
        assert!(!is_event("TextLabel", "Text"));
    }

    #[test]
    fn enumerates_creatable_classes_only() {
        let classes: Vec<&str> = creatable_classes().collect();

        assert!(classes.contains(&"Frame"));
        assert!(classes.contains(&"TextLabel"));
        // Services and abstract classes cannot be elements, so they are not
        // offered as ones.
        assert!(!classes.contains(&"Players"));
        assert!(!classes.contains(&"GuiObject"));
    }

    #[test]
    fn enumerates_members_through_inheritance() {
        let properties: Vec<&str> = properties("TextLabel").collect();

        // Its own, and GuiObject's, and Instance's.
        assert!(properties.contains(&"Text"));
        assert!(properties.contains(&"BackgroundColor3"));
        assert!(properties.contains(&"Name"));
        // Read-only members are not settable, so they are not properties here.
        assert!(!properties.contains(&"ContentText"));

        let events: Vec<&str> = events("TextButton").collect();
        assert!(events.contains(&"Activated"));
        assert!(!events.contains(&"Text"));
    }

    #[test]
    fn enumeration_agrees_with_the_by_name_queries() {
        for property in properties("TextLabel") {
            assert!(has_property("TextLabel", property), "{property}");
        }
        for event in events("TextButton") {
            assert!(is_event("TextButton", event), "{event}");
        }
        for class in creatable_classes() {
            assert!(is_class(class), "{class}");
        }
    }

    #[test]
    fn walks_the_superclass_chain() {
        assert_eq!(superclass("TextLabel"), Some("GuiLabel"));
        assert_eq!(superclass("Instance"), Some("Object"));
        // The chain ends where the dump does, and names that are not classes
        // have no parent at all.
        assert_eq!(superclass("Object"), None);
        assert_eq!(superclass("NotAClass"), None);
    }

    #[test]
    fn knows_which_classes_carry_text() {
        assert!(has_text_property("TextLabel"));
        assert!(has_text_property("TextButton"));
        assert!(has_text_property("TextBox"));
        assert!(!has_text_property("Frame"));
        assert!(!has_text_property("ImageLabel"));
    }

    #[test]
    fn suggests_a_near_miss() {
        assert_eq!(closest_class("TextLabl"), Some("TextLabel"));
        assert_eq!(closest_class("Frmae"), Some("Frame"));
        assert_eq!(closest_class("ScrollingFrmae"), Some("ScrollingFrame"));
    }

    #[test]
    fn does_not_suggest_for_unrelated_names() {
        // `Receipt` used to reach `Script` under a looser threshold.
        assert_eq!(closest_class("Receipt"), None);
        assert_eq!(closest_class("Row"), None);
        assert_eq!(closest_class("Card"), None);
    }

    #[test]
    fn suggestions_are_case_insensitive() {
        assert_eq!(closest_class("textlabel"), Some("TextLabel"));
        assert_eq!(closest_class("frame"), Some("Frame"));
    }

    #[test]
    fn suggests_members_by_containment_first() {
        // `Color3` is ten edits from `BackgroundColor3`, so distance alone
        // finds nothing; containment finds the whole qualified family.
        let suggestions = closest_members("Frame", "Color3");
        assert!(suggestions.contains(&"BackgroundColor3"), "{suggestions:?}");
        assert!(suggestions.contains(&"BorderColor3"), "{suggestions:?}");
    }

    #[test]
    fn suggests_members_by_edit_distance() {
        assert_eq!(closest_members("TextLabel", "Txt").first(), Some(&"Text"));
        assert_eq!(closest_members("TextLabel", "Tetx").first(), Some(&"Text"));
        assert_eq!(
            closest_members("TextButton", "Activate").first(),
            Some(&"Activated")
        );
    }

    #[test]
    fn suggests_nothing_for_unrelated_attributes() {
        assert!(closest_members("Frame", "Zzzzq").is_empty());
    }

    #[test]
    fn suggestions_are_capped() {
        // `Text` is contained in a great many TextLabel members.
        assert!(closest_members("TextLabel", "Text").len() <= 3);
    }

    #[test]
    fn transpositions_count_as_one_slip() {
        // Plain Levenshtein charges 2 here, which a tight budget would reject.
        assert_eq!(edit_distance("frmae", "frame"), 1);
        assert_eq!(edit_distance("textlabl", "textlabel"), 1);
    }
}
