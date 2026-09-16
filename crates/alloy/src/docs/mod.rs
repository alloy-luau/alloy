//! The documentation of every Alloy construct, in one table.
//!
//! The server answers a hover and a completion from it, and `alloy doc`
//! prints it. A key is the word as written, with its sigil: `struct`,
//! `??=`, `$dbg`, `@derive`, `derive:Eq`, and `topic:strict` for the
//! articles that no token names.

mod book;
mod keywords;
mod members;
mod table;

pub use book::{BOOK, LINT_CODE, SITE, Section, book_url, section};
pub use keywords::ALLOY_KEYWORDS;
pub use members::{
    MEMBER_KINDS, MEMBERS, Member, MemberKind, TYPE_SIGNATURES, member, member_fits, member_hover,
    member_markdown, member_names, member_owner, member_spot, members, split_member, type_head,
    type_markdown, type_signature, value_head,
};
pub use table::{TABLE, keys_with_prefix, lookup};

/// The kinds a diagnostic prints, each with the book section that
/// explains it. `alloy doc <kind>` opens the section, so every kind
/// `kind_for` and the checker name has a row here.
pub const KINDS: &[(&str, &str)] = &[
    ("AlloyError", "4.1"),
    ("AsyncError", "3.3"),
    ("AttributeContract", "3.11"),
    ("AttributeError", "3.11"),
    ("BoundError", "3.6"),
    ("ConstError", "6.1"),
    ("ConstructorError", "3.6"),
    ("DataError", "5.11"),
    ("DeclareError", "6.1"),
    ("DirectiveError", "4.4"),
    ("DuplicateError", "6.1"),
    ("EnumError", "3.4"),
    ("ExhaustiveMatch", "4.2"),
    ("ImportError", "3.2"),
    ("IngotError", "5.12"),
    ("InternalError", "4.1"),
    ("MacroError", "3.10"),
    ("MarkupError", "3.13"),
    ("ReservedWord", "6.1"),
    ("ResultError", "4.1"),
    ("StructError", "3.6"),
    ("SyntaxError", "6.1"),
    ("TestError", "3.14"),
    ("TraitContract", "3.6"),
    ("TypeError", "5.4"),
    ("UnknownModule", "3.2"),
    ("WireType", "4.3"),
];

/// The section a kind name opens, in any case: `alloy doc structerror`
/// opens what `alloy doc StructError` does.
pub fn kind_section(name: &str) -> Option<&'static Section> {
    KINDS
        .iter()
        .find(|(kind, _)| kind.eq_ignore_ascii_case(name))
        .and_then(|(_, number)| section(number))
}

/// The kinds a section explains, for its page.
pub fn section_kinds(number: &str) -> Vec<&'static str> {
    KINDS
        .iter()
        .filter(|(_, n)| *n == number)
        .map(|(kind, _)| *kind)
        .collect()
}

/// The words of a diagnostic, lower case, and the kind they name. The
/// first match wins, from the most specific wording to the least.
const KIND_RULES: &[(&[&str], &str)] = &[
    (&["internal:"], "InternalError"),
    // The removal report names a declaration kind, which the rules
    // below would read as the kind's own family.
    (&["`global` is removed"], "ImportError"),
    (&["needs `as` before its body"], "SyntaxError"),
    // The forms a Luau user writes from another language. Each names
    // the Alloy form, and the rules below would read the declaration
    // word in the sentence as that declaration's own family.
    (&["body is `as ... end`"], "SyntaxError"),
    (&["one name holds one declaration"], "DuplicateError"),
    (&["a comment starts with"], "SyntaxError"),
    (&["interpolation hole is empty"], "SyntaxError"),
    (&["has no `++`"], "SyntaxError"),
    // A pattern that binds the name the match head aliased. No rule
    // below reads the sentence, and the default kind is not the one
    // the parser reports it as.
    (&["is the alias of the match"], "SyntaxError"),
    (&["`declare` takes", "`declare` belongs"], "DeclareError"),
    // A header the parser cannot read. The `trait` rule below would
    // read the word as a contract report.
    (&["takes no type parameters"], "SyntaxError"),
    // A turbofish inside a type argument list; the parser reports it.
    (&["a type is written"], "SyntaxError"),
    (&["markup:"], "MarkupError"),
    (&["names no module"], "UnknownModule"),
    (&["is a script, not a module"], "UnknownModule"),
    (&["returns nothing to import"], "UnknownModule"),
    (&["ingot `"], "IngotError"),
    (&["reserved word"], "ReservedWord"),
    (&["is already declared in"], "DeclareError"),
    // A namespace used above its declaration. The struct, enum, and
    // function forms name their own kind in the sentence; a namespace
    // has no section, so the report is about the declaration.
    (&["move the namespace above it"], "DeclareError"),
    (
        &["in macro expansion", "returns from the function"],
        "MacroError",
    ),
    (&["not exhaustive", "no arm for"], "ExhaustiveMatch"),
    // The `impl` header names a struct and an enum in one sentence;
    // the enum rule below would take it.
    (&["an `impl` targets"], "StructError"),
    (&["remote"], "WireType"),
    (&["directive"], "DirectiveError"),
    (&["result"], "ResultError"),
    // After `result`, so `try await` on a Result keeps that kind.
    (&["await", "async"], "AsyncError"),
    (&["a `const`"], "ConstError"),
    // Before the test rule: `test ` also matches a struct the user
    // named `Test`, and a message about constructing one says
    // `new Test { ... }`. The `new ` marker is the narrower of the
    // two, so it answers first.
    (&["`new ", "constructor", "construct"], "ConstructorError"),
    // Before the `@test` rule: `@test` on a method is about where
    // the attribute goes, not about a test.
    (&["goes on a function, not a method"], "AttributeError"),
    (&["@test", "test "], "TestError"),
    (&["@cfg"], "AttributeError"),
    // An intrinsic names itself with its sigil: `$nameof`, `$map`.
    (&["macro", "`$"], "MacroError"),
    // An attribute contract: the report names the attribute and the
    // clause it broke. Above the import rule, because the word
    // `requires` holds the word `require`.
    (
        &["` requires ", "`each ", "`requires`"],
        "AttributeContract",
    ),
    (&["attribute", "derive"], "AttributeError"),
    (&["data file"], "DataError"),
    // A module the importer names that the parser cannot read. The
    // words of the report name neither `import` nor `module`.
    (&["does not parse"], "ImportError"),
    (&["import", "export", "require", "module"], "ImportError"),
    // A bound on a generic. Before the trait rule: the report names
    // the trait the bound asks for, and it is about the argument.
    (&["does not implement"], "BoundError"),
    (
        &["does not write", "parameters in", "trait"],
        "TraitContract",
    ),
    (&["field", "sealed", "struct"], "StructError"),
    (&["variant", "enum"], "EnumError"),
    (
        &[
            "expected",
            "unexpected",
            "unterminated",
            "needs a",
            "found",
            "cannot follow",
        ],
        "SyntaxError",
    ),
];

/// The kind of a compiler diagnostic, from its text: the word before
/// the colon in `ReservedWord: ...`, the way the checker names its own.
pub fn kind_for(message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();

    KIND_RULES
        .iter()
        .find(|(words, _)| words.iter().any(|w| m.contains(w)))
        .map(|(_, kind)| *kind)
        .unwrap_or("AlloyError")
}

/// A compiler diagnostic as the editor and the CLI show it: its kind,
/// a colon, its text.
pub fn labeled(message: &str) -> String {
    // `markup:` names the kind, and the kind prints in front of the
    // text; printing both says it twice.
    let text = message.strip_prefix("markup: ").unwrap_or(message);

    format!("{}: {text}", kind_for(message))
}

/// The book section a compiler diagnostic prints as its code: the
/// section of its kind. `alloy doc 6.1` then lists `DuplicateError`,
/// the kind the line names, and `alloy doc DuplicateError` opens 6.1.
pub fn code_for(message: &str) -> Option<&'static str> {
    kind_section(kind_for(message)).map(|s| s.number)
}

#[cfg(test)]
mod tests {
    /// `alloy doc <kind>` opens a section for every kind a diagnostic
    /// prints: the ones `kind_for` names, its fallback, and the ones
    /// the type checker names.
    #[test]
    fn every_diagnostic_kind_names_a_section() {
        let named = super::KIND_RULES
            .iter()
            .map(|(_, kind)| *kind)
            .chain(["AlloyError", "TypeError"]);

        for kind in named {
            assert!(
                super::kind_section(kind).is_some(),
                "`{kind}` opens no section"
            );
        }

        for (kind, number) in super::KINDS {
            assert!(
                super::section(number).is_some(),
                "{kind}: no section {number}"
            );
            assert_eq!(
                super::kind_section(&kind.to_ascii_lowercase()).map(|s| s.number),
                Some(*number)
            );
        }
    }

    /// The code a diagnostic prints is the section of its kind, so the
    /// page that code opens lists the kind in its `Reports:` line.
    #[test]
    fn every_kind_is_listed_by_the_section_its_code_opens() {
        for (kind, _) in super::KINDS {
            let code = super::kind_section(kind).map(|s| s.number).unwrap();

            assert!(
                super::section_kinds(code).contains(kind),
                "{kind}: section {code} does not list it"
            );
        }
    }

    /// The `Future` entry documents every member the std declares. The
    /// two drift apart the moment the std grows a method, and the doc
    /// is the only place a reader looks.
    #[test]
    fn the_future_topic_names_every_member_of_the_std() {
        let documented = super::member_names("Future");
        let mut names: Vec<&str> = Vec::new();

        // The members of the type: `read name: (...) -> ...`.
        let at = crate::RUNTIME
            .find("export type Future<T> = {")
            .expect("Future type");
        let body = &crate::RUNTIME[at..];
        let end = body.find("\n}").expect("end of the type");

        for line in body[..end].lines() {
            if let Some(rest) = line.trim().strip_prefix("read ")
                && let Some(name) = rest.split(':').next()
                && !name.starts_with("__")
            {
                names.push(name);
            }
        }

        // The statics: `function Future.name<T>(...)`.
        for line in crate::RUNTIME.lines() {
            if let Some(rest) = line.strip_prefix("function Future.")
                && let Some(name) = rest.split(['<', '(']).next()
                && !name.starts_with("__")
            {
                names.push(name);
            }
        }

        for name in names {
            assert!(
                documented.contains(&name),
                "the std has `Future.{name}`; the members do not"
            );
        }
    }

    /// Every member's example is Alloy the compiler accepts. A doc
    /// example that does not compile is worse than none: a reader
    /// copies it.
    #[test]
    fn every_member_example_compiles() {
        for (owner, members) in super::MEMBERS {
            for m in *members {
                let out = crate::compile(m.example)
                    .unwrap_or_else(|e| panic!("{owner}.{}: {}", m.name, e.located(m.example)));

                assert!(
                    out.diagnostics.is_empty(),
                    "{owner}.{}: {}",
                    m.name,
                    out.diagnostics
                        .iter()
                        .map(|d| d.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }
    }

    /// A member's signature opens with its own name: a static and a
    /// method carry the owner and the sigil the call takes, and a plain
    /// member may name itself alone.
    #[test]
    fn every_signature_names_its_owner_and_member() {
        for (owner, members) in super::MEMBERS {
            for m in *members {
                let heads = match m.kind {
                    super::MemberKind::Static => vec![format!("{owner}.{}", m.name)],
                    super::MemberKind::Method => vec![format!("{owner}:{}", m.name)],
                    _ => vec![format!("{owner}.{}", m.name), m.name.to_string()],
                };

                assert!(
                    heads.iter().any(|h| m.signature.starts_with(h)),
                    "{owner}.{} signs as `{}`, which opens with none of {heads:?}",
                    m.name,
                    m.signature
                );
                assert!(!m.doc.is_empty(), "{owner}.{} has no doc", m.name);
                assert!(!m.example.is_empty(), "{owner}.{} has no example", m.name);
            }
        }
    }

    /// Every entry with members is a Std entry, and none of the Std
    /// entries keeps a pipe table: a member is a section now.
    #[test]
    fn no_std_entry_holds_a_table() {
        for (key, _) in super::MEMBERS {
            let text = super::lookup(key).unwrap_or_else(|| panic!("no entry for `{key}`"));

            assert!(!text.contains("|---|"), "`{key}` still holds a table");
        }

        // `Traits` groups the shapes a bound names; it is the one std
        // entry that is not a type of its own.
        for (key, _) in super::TYPE_SIGNATURES {
            assert!(super::lookup(key).is_some(), "no entry for `{key}`");
        }

        for (key, _) in super::MEMBERS {
            assert!(
                *key == "Traits" || super::type_signature(key).is_some(),
                "`{key}` documents members and no signature"
            );
        }
    }

    #[test]
    fn a_dotted_topic_names_one_member() {
        assert_eq!(
            super::split_member("HashMap:get").map(|(k, m)| (k, m.name)),
            Some(("HashMap", "get"))
        );
        assert_eq!(
            super::split_member("HashMap.get").map(|(k, m)| (k, m.name)),
            Some(("HashMap", "get"))
        );
        assert_eq!(
            super::split_member("Signal.new").map(|(k, m)| (k, m.name)),
            Some(("Signal", "new"))
        );
        assert!(super::split_member("HashMap.nope").is_none());
        assert!(super::split_member("Nope.get").is_none());
        assert!(super::split_member("HashMap").is_none());
    }
}
