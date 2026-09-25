//! The numbered sections of the book, and the lint section number.

/// The website of the book, where a diagnostic's code links.
pub const SITE: &str = "https://alloy-luau.github.io";

/// One numbered section of the book, as the website lays it out. A
/// diagnostic carries a section's number as its code, `Alloy(4.2)`, and
/// the number links to the section; `alloy doc 4.2` prints it.
pub struct Section {
    pub number: &'static str,
    /// The anchor on the book page.
    pub id: &'static str,
    pub title: &'static str,
    /// The doc entry that explains the section, when one does.
    pub key: Option<&'static str>,
}

pub const BOOK: &[Section] = &[
    Section {
        number: "1",
        id: "intro",
        title: "Introduction",
        key: None,
    },
    Section {
        number: "2",
        id: "getting-started",
        title: "Getting started",
        key: None,
    },
    Section {
        number: "2.1",
        id: "install",
        title: "Install",
        key: None,
    },
    Section {
        number: "2.2",
        id: "first-project",
        title: "A first project",
        key: Some("topic:build"),
    },
    Section {
        number: "2.3",
        id: "editor",
        title: "The editor",
        key: None,
    },
    Section {
        number: "3",
        id: "language",
        title: "The language",
        key: None,
    },
    Section {
        number: "3.1",
        id: "safe",
        title: "Safe access",
        key: Some("?."),
    },
    Section {
        number: "3.2",
        id: "modules",
        title: "Modules",
        key: Some("import"),
    },
    Section {
        number: "3.3",
        id: "async",
        title: "Async and Futures",
        key: Some("async"),
    },
    Section {
        number: "3.4",
        id: "enums",
        title: "Enums and match",
        key: Some("enum"),
    },
    Section {
        number: "3.5",
        id: "bindings",
        title: "Conditional bindings",
        key: Some("match"),
    },
    Section {
        number: "3.6",
        id: "structs",
        title: "Structs and traits",
        key: Some("struct"),
    },
    Section {
        number: "3.7",
        id: "interfaces",
        title: "Interfaces and types",
        key: Some("interface"),
    },
    Section {
        number: "3.8",
        id: "sugar",
        title: "Sugar",
        key: Some("?"),
    },
    Section {
        number: "3.9",
        id: "extensions",
        title: "Extensions",
        key: Some("impl"),
    },
    Section {
        number: "3.10",
        id: "macros",
        title: "Macros",
        key: Some("macro"),
    },
    Section {
        number: "3.11",
        id: "attributes",
        title: "Attributes",
        key: Some("attribute"),
    },
    Section {
        number: "3.12",
        id: "remotes",
        title: "Remotes",
        key: Some("remote"),
    },
    Section {
        number: "3.13",
        id: "markup",
        title: "Markup",
        key: Some("topic:markup"),
    },
    Section {
        number: "3.14",
        id: "tests",
        title: "Tests",
        key: Some("@test"),
    },
    Section {
        number: "4",
        id: "strict",
        title: "Strict by default",
        key: Some("topic:strict"),
    },
    Section {
        number: "4.1",
        id: "contracts",
        title: "The contracts",
        key: Some("topic:strict"),
    },
    Section {
        number: "4.2",
        id: "exhaustive",
        title: "Exhaustive match",
        key: Some("topic:exhaustive"),
    },
    Section {
        number: "4.3",
        id: "wire",
        title: "Wire types",
        key: Some("topic:wire"),
    },
    Section {
        number: "4.4",
        id: "directives",
        title: "Directives",
        key: Some("topic:directives"),
    },
    Section {
        number: "5",
        id: "tooling",
        title: "Tooling",
        key: None,
    },
    Section {
        number: "5.1",
        id: "build",
        title: "alloy build",
        key: Some("topic:build"),
    },
    Section {
        number: "5.2",
        id: "check",
        title: "alloy check",
        key: Some("topic:check"),
    },
    Section {
        number: "5.3",
        id: "lint",
        title: "alloy lint",
        key: Some("topic:lint"),
    },
    Section {
        number: "5.4",
        id: "flux",
        title: "alloy flux",
        key: Some("topic:flux"),
    },
    Section {
        number: "5.5",
        id: "fmt",
        title: "alloy fmt",
        key: Some("topic:fmt"),
    },
    Section {
        number: "5.6",
        id: "test",
        title: "alloy test",
        key: Some("topic:test"),
    },
    Section {
        number: "5.7",
        id: "doc",
        title: "alloy doc",
        key: None,
    },
    Section {
        number: "5.8",
        id: "config",
        title: "alloy.toml",
        key: Some("topic:config"),
    },
    Section {
        number: "5.9",
        id: "luaurc",
        title: ".luaurc and .config.luau",
        key: Some("topic:luaurc"),
    },
    Section {
        number: "5.10",
        id: "mount",
        title: "Project files and mounts",
        key: Some("topic:mount"),
    },
    Section {
        number: "5.11",
        id: "data",
        title: "JSON and TOML data",
        key: Some("topic:data"),
    },
    Section {
        number: "5.12",
        id: "ingots",
        title: "Ingots",
        key: Some("topic:ingots"),
    },
    Section {
        number: "6",
        id: "reference",
        title: "Reference",
        key: None,
    },
    Section {
        number: "6.1",
        id: "ref-keywords",
        title: "Keywords",
        key: None,
    },
    Section {
        number: "6.2",
        id: "ref-operators",
        title: "Operators",
        key: None,
    },
    Section {
        number: "6.3",
        id: "ref-intrinsics",
        title: "Intrinsics",
        key: None,
    },
    Section {
        number: "6.4",
        id: "ref-attributes",
        title: "Attributes",
        key: None,
    },
    Section {
        number: "6.5",
        id: "ref-derives",
        title: "Derives",
        key: None,
    },
    Section {
        number: "6.6",
        id: "ref-std",
        title: "Standard library",
        key: Some("topic:std"),
    },
    Section {
        number: "6.7",
        id: "lints",
        title: "Lints",
        key: Some("lints"),
    },
];

/// The section a number names.
pub fn section(number: &str) -> Option<&'static Section> {
    BOOK.iter().find(|s| s.number == number)
}

/// The link for a section number.
pub fn book_url(number: &str) -> Option<String> {
    section(number).map(|s| format!("{SITE}/docs/#{}", s.id))
}

/// The lints' section number: every lint's code.
pub const LINT_CODE: &str = "6.7";
