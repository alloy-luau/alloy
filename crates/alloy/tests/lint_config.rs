//! `[lint]`, `[lint.rules]`, and `[fmt] recommended`: what the modes
//! set, what the rules table names, and what the deprecated form still
//! reads.

use std::path::Path;

use alloy::config::{Config, IndentType, QuoteStyle};
use alloy::lint::{Level, level_of};

fn parse(text: &str) -> Config {
    Config::parse(text, Path::new("alloy.toml")).unwrap_or_else(|e| panic!("{e}"))
}

// --- 1. the rules table ----------------------------------------------------

#[test]
fn a_rule_names_a_lint_a_group_or_a_markup_lint() {
    let c = parse(
        "[lint.rules]\noptional_access = \"deny\"\nstyle = \"allow\"\nnaming = \"warn\"\nalx.static_conditional_child = \"deny\"\n",
    );

    assert_eq!(level_of(&c.lint, "optional_access"), Level::Deny);
    // A name beats its group, and a group covers every lint in it.
    assert_eq!(level_of(&c.lint, "manual_safe_access"), Level::Allow);
    assert_eq!(level_of(&c.lint, "naming_convention"), Level::Warn);
    assert_eq!(
        alloy::lint::alx_level_of(&c.lint, "static_conditional_child"),
        Level::Deny
    );
    // The compile reports the markup lint under its full name.
    assert_eq!(
        level_of(&c.lint, "alx.static_conditional_child"),
        Level::Deny
    );
    assert_eq!(
        alloy::lint::group_name("alx.static_conditional_child"),
        "alx"
    );

    let markup = c.markup(Path::new(".")).unwrap();
    assert_eq!(
        markup.static_conditional_child,
        alloy::luaux::config::LintLevel::Error
    );
}

#[test]
fn a_level_the_table_does_not_spell_is_an_error() {
    assert!(
        Config::parse(
            "[lint.rules]\noptional_access = \"loud\"\n",
            Path::new("alloy.toml")
        )
        .is_err()
    );
}

#[test]
fn an_unknown_rule_name_is_reported_and_not_an_error() {
    let c = parse("[lint.rules]\nno_such_lint = \"warn\"\noptional_access = \"deny\"\n");
    assert_eq!(alloy::lint::unknown_names(&c.lint), vec!["no_such_lint"]);
}

// --- 2. the deprecated form ------------------------------------------------

#[test]
fn the_old_lists_still_read_and_name_the_key_that_replaces_them() {
    let c = parse("[lint]\nstrict = true\ndeny = [\"correctness\"]\nwarn = [\"naming\"]\n");

    assert_eq!(level_of(&c.lint, "optional_access"), Level::Deny);
    assert_eq!(level_of(&c.lint, "naming_convention"), Level::Warn);
    assert_eq!(
        c.deprecations(),
        vec![
            "`[lint] deny` is deprecated; write `[lint.rules] correctness = \"deny\"`",
            "`[lint] warn` is deprecated; write `[lint.rules] naming = \"warn\"`",
        ]
    );
}

#[test]
fn a_rule_beats_the_old_list_for_the_same_name() {
    let c = parse(
        "[lint]\nallow = [\"optional_access\"]\n\n[lint.rules]\noptional_access = \"deny\"\n",
    );
    assert_eq!(level_of(&c.lint, "optional_access"), Level::Deny);
}

#[test]
fn the_old_markup_table_still_reads_and_names_its_replacement() {
    let c = parse(
        "[alx.factory]\nbackend = \"table\"\ncreate = \"create\"\n\n[alx.lints]\nstatic_conditional_child = \"error\"\n",
    );

    assert_eq!(
        c.markup(Path::new(".")).unwrap().static_conditional_child,
        alloy::luaux::config::LintLevel::Error
    );
    assert_eq!(
        c.deprecations(),
        vec![
            "`[alx.lints]` is deprecated; write `[lint.rules] alx.static_conditional_child = \"deny\"`"
        ]
    );

    // The new key wins over the old table.
    let both = parse(
        "[alx.factory]\nbackend = \"table\"\ncreate = \"create\"\n\n[alx.lints]\nstatic_conditional_child = \"error\"\n\n[lint.rules]\nalx.static_conditional_child = \"allow\"\n",
    );
    assert_eq!(
        both.markup(Path::new("."))
            .unwrap()
            .static_conditional_child,
        alloy::luaux::config::LintLevel::Off
    );
}

/// The three naming lints became `naming_convention`. An old name, and
/// rustc's name, still set its level in the table and in a file, and
/// the config names the new lint.
#[test]
fn an_old_naming_lint_name_sets_the_new_lint() {
    let c = parse("[lint.rules]\ncamel_case_name = \"deny\"\n");
    assert_eq!(level_of(&c.lint, "naming_convention"), Level::Deny);
    assert!(alloy::lint::unknown_names(&c.lint).is_empty());
    assert_eq!(
        c.deprecations(),
        vec![
            "`camel_case_name` is now `naming_convention`; write `[lint.rules] naming_convention`, and set the case of each kind of name in `[lint.naming]`"
        ]
    );

    // The new name wins over the old one.
    let both = parse("[lint.rules]\ntype_case = \"deny\"\nnaming_convention = \"warn\"\n");
    assert_eq!(level_of(&both.lint, "naming_convention"), Level::Warn);

    for directive in [
        "--@alloy-lint pascal_case_function=warn",
        "--@alloy-lint non_snake_case=warn",
    ] {
        let d = alloy::directives::scan(&format!("{directive}\nlocal x = 1\n"));
        assert!(d.problems().is_empty(), "{directive}");
        assert_eq!(
            alloy::lint::level_in(&Config::default().lint, &d, "naming_convention"),
            Level::Warn,
            "{directive}"
        );
    }
}

/// `@allow` with rustc's names, the old names, and the new one quiets
/// the lint on the item it sits on, and a region names it too.
#[test]
fn an_allow_with_a_rust_name_quiets_the_naming_lint() {
    let hits = |src: &str| -> usize {
        alloy::compile(src)
            .unwrap()
            .lints
            .iter()
            .filter(|l| l.name == "naming_convention")
            .count()
    };
    assert_eq!(hits("local playerCount = 1\nprint(playerCount)\n"), 1);

    for allow in [
        "@allow(non_snake_case)",
        "@allow(non_upper_case_globals)",
        "@allow(clippy.non_snake_case)",
        "@allow(camel_case_name)",
        "@allow(naming_convention)",
        "@allow(naming)",
    ] {
        let src = format!("{allow}\nlocal playerCount = 1\nprint(playerCount)\n");
        assert_eq!(hits(&src), 0, "{allow}");
    }

    assert_eq!(
        hits(
            "--@alloy-ignore-start non_camel_case_types\nstruct point_list as\n    x: number\nend\n--@alloy-ignore-end\n"
        ),
        0
    );
}

// --- 3. the modes ----------------------------------------------------------

#[test]
fn the_modes_are_on_with_no_file() {
    let c = Config::default();
    assert!(c.lint.recommended && c.lint.strict && c.fmt.recommended);
    // A pedantic lint rides on `strict`.
    assert_eq!(level_of(&c.lint, "implicit_any"), Level::Warn);
    assert_eq!(level_of(&c.lint, "optional_access"), Level::Warn);
}

#[test]
fn nothing_recommended_leaves_every_lint_silent() {
    let c = parse("[lint]\nrecommended = false\nstrict = false\n");

    // `optional_access` warns under the recommended set.
    assert_eq!(level_of(&c.lint, "optional_access"), Level::Allow);
    assert_eq!(level_of(&c.lint, "implicit_any"), Level::Allow);
    assert_eq!(level_of(&c.lint, "LocalUnused"), Level::Allow);
    assert_eq!(
        alloy::lint::alx_level_of(&c.lint, "static_conditional_child"),
        Level::Allow
    );

    // A lint the source draws stays out of the report.
    let src = "local function f(p: Player?)\n    return p.Name\nend\nprint(f)\n";
    let out = alloy::compile(src).unwrap();
    assert!(out.lints.iter().any(|l| l.name == "optional_access"));
    assert!(
        out.lints
            .iter()
            .all(|l| level_of(&c.lint, l.name) == Level::Allow),
        "{:?}",
        out.lints
    );

    // The rules table alone then says what runs, and `strict` still does.
    let one = parse("[lint]\nrecommended = false\n\n[lint.rules]\noptional_access = \"deny\"\n");
    assert_eq!(level_of(&one.lint, "optional_access"), Level::Deny);
    assert_eq!(level_of(&one.lint, "dropped_result"), Level::Allow);
    assert_eq!(level_of(&one.lint, "implicit_any"), Level::Warn);
}

#[test]
fn a_preserving_formatter_reflows_nothing_and_keeps_the_quotes() {
    let c = parse("[fmt]\nrecommended = false\n");
    let long = format!(
        "local t = {{ a = \"one\", b = \"two\", c = \"{}\" }}\nprint(t)\n",
        "x".repeat(120)
    );

    assert!(long.lines().next().unwrap().len() > 100);

    let options = c.fmt.for_source(&long);
    assert_eq!(options.quote_style, QuoteStyle::Preserve);

    let out = alloy::fmt::format_file(&long, &options).unwrap();
    assert_eq!(out, long, "the long line was reflowed");

    // The recommended layout breaks the same line and rewrites the quotes.
    let reflowed = alloy::fmt::format_file(&long, &Config::default().fmt).unwrap();
    assert!(reflowed.lines().count() > long.lines().count());
    assert!(reflowed.contains("'one'"));
}

#[test]
fn a_preserving_formatter_takes_the_indent_from_the_file() {
    let c = parse("[fmt]\nrecommended = false\n");
    let two = "local function f()\n  return 1\nend\nprint(f)\n";
    let tabs = "local function f()\n\treturn 1\nend\nprint(f)\n";

    assert_eq!(c.fmt.for_source(two).indent_width, 2);
    assert_eq!(c.fmt.for_source(two).indent_type, IndentType::Spaces);
    assert_eq!(c.fmt.for_source(tabs).indent_type, IndentType::Tabs);
    assert_eq!(
        alloy::fmt::format_file(two, &c.fmt.for_source(two)).unwrap(),
        two
    );
    assert_eq!(
        alloy::fmt::format_file(tabs, &c.fmt.for_source(tabs)).unwrap(),
        tabs
    );
}

#[test]
fn a_written_key_applies_over_the_preserving_layout() {
    let c = parse(
        "[fmt]\nrecommended = false\nquote_style = \"force-double\"\nindent_width = 8\n\n[fmt.alx]\nblank_lines = false\n",
    );

    assert_eq!(c.fmt.quote_style, QuoteStyle::ForceDouble);
    assert_eq!(c.fmt.indent_width, 8);
    // An indent the project names wins over the file's own.
    assert!(!c.fmt.detect_indent);
    assert!(!c.fmt.alx.blank_lines);
    // Everything else stays preserving.
    assert_eq!(c.fmt.column_width, alloy::config::NO_REFLOW_WIDTH);
    assert_eq!(c.fmt.leading_zero, alloy::config::LeadingZero::Preserve);
    assert_eq!(c.fmt.alx.text_wrap, alloy::config::TextWrap::Preserve);

    let src = "local s = 'one'\nprint(s)\n";
    assert_eq!(
        alloy::fmt::format_file(src, &c.fmt.for_source(src)).unwrap(),
        "local s = \"one\"\nprint(s)\n"
    );
}

/// `alloy fmt` renames a name that breaks its `[lint.naming]` style
/// while the lint is on, and `[fmt] fix_naming = false` stops it. The
/// CLI and the editor copy `[lint]` into the options.
#[test]
fn fmt_renames_while_the_naming_lint_is_on() {
    // The local takes a second write, so `prefer_const` leaves it be.
    let src = "local playerCount = 1\nplayerCount += 1\nprint(playerCount)\n";
    let renamed = "local player_count = 1\nplayer_count += 1\nprint(player_count)\n";
    let options = |toml: &str| {
        let c = parse(toml);
        let mut fmt = c.fmt.for_source(src);
        fmt.lint = c.lint;
        fmt
    };
    let fmt = |name: &str, text: &str, toml: &str| {
        alloy::fmt::format_named(name, text, &options(toml)).unwrap()
    };

    // The lint is on by default, so a project with no word on it takes
    // the Rust styles, and one that turns the group off keeps its names.
    assert_eq!(fmt("a.aly", src, ""), renamed);
    assert_eq!(fmt("a.aly", src, "[lint.rules]\nnaming = \"allow\"\n"), src);

    let on = "[lint.rules]\nnaming = \"warn\"\n";
    assert_eq!(fmt("a.aly", src, on), renamed);
    assert_eq!(
        fmt(
            "a.aly",
            src,
            "[lint.rules]\nnaming = \"warn\"\n\n[lint.naming]\nvariable = \"camelCase\"\n"
        ),
        src
    );
    assert_eq!(
        fmt(
            "a.aly",
            "local total_count = 1\ntotal_count += 1\nprint(total_count)\n",
            "[lint.rules]\nnaming = \"warn\"\n\n[lint.naming]\nvariable = [\"camelCase\", \"PascalCase\"]\n"
        ),
        "local totalCount = 1\ntotalCount += 1\nprint(totalCount)\n"
    );

    // `fix_naming = false`, a file that turns the lint off, and a
    // `.d.aly` keep the names.
    assert_eq!(
        fmt(
            "a.aly",
            src,
            "[fmt]\nfix_naming = false\n\n[lint.rules]\nnaming = \"warn\"\n"
        ),
        src
    );
    let quiet = format!("--@alloy-lint naming=allow\n{src}");
    assert_eq!(fmt("a.aly", &quiet, on), quiet);
    assert!(fmt("a.d.aly", "declare function loadMap(): ()\n", on).contains("loadMap"));
}

/// A markup lint fires under the table backend, so a project of one
/// `.alx` file shows what silences it. The result holds each error, then
/// each lint as `lint: <name>: <message>`.
fn markup_project(name: &str, toml: &str, source: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("alloy-alx-lint-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("alloy.toml"), toml).unwrap();
    std::fs::write(dir.join("src/ui.alx"), source).unwrap();

    let config = Config::load(&dir.join("alloy.toml")).unwrap();
    let report = alloy::build::check_project(&dir, &config).unwrap();
    let out = report
        .diagnostics
        .iter()
        .map(|(_, d)| d.message.clone())
        .chain(
            report
                .lints
                .iter()
                .map(|(_, l)| format!("lint: {}: {}", l.name, l.message)),
        )
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    out
}

#[test]
fn a_markup_lint_reads_the_rules_table_and_the_file_directive() {
    const FACTORY: &str = "[alx.factory]\nbackend = \"table\"\ncreate = \"create\"\n";
    const UI: &str = "import create from \"./create\"\n\nlocal function View(props: { open: boolean })\n    return (\n        <Frame>\n            {if props.open then <TextLabel /> else nil}\n        </Frame>\n    )\nend\n\nreturn View\n";

    // At its default level it is a warning: a lint, not an error that
    // fails the build.
    let loud = markup_project("loud", FACTORY, UI);
    assert!(
        loud.iter()
            .any(|m| m.starts_with("lint: alx.static_conditional_child: this child is built once")),
        "{loud:?}"
    );
    assert!(!loud.iter().any(|m| m.starts_with("markup:")), "{loud:?}");

    // The rules table silences it for the project.
    let quiet = markup_project(
        "quiet",
        &format!("{FACTORY}\n[lint.rules]\nalx.static_conditional_child = \"allow\"\n"),
        UI,
    );
    assert!(!quiet.iter().any(|m| m.contains("built once")), "{quiet:?}");

    // A directive silences it for one file.
    let one = markup_project(
        "one",
        FACTORY,
        &format!("--@alloy-lint alx.static_conditional_child=allow\n{UI}"),
    );
    assert!(!one.iter().any(|m| m.contains("built once")), "{one:?}");
}

// --- 4. the file `alloy init` writes ---------------------------------------

/// The template is the defaults plus one opinion: `wait_timeout`. A
/// second opinion that creeps into the file fails the equality below.
#[test]
fn the_template_holds_one_opinion_and_the_defaults() {
    let c = parse(alloy::config::TEMPLATE);
    let mut want = Config::default();

    assert_eq!(
        want.emit.wait_timeout, None,
        "the Rust default waits forever"
    );
    want.emit.wait_timeout = Some(5.0);

    assert_eq!(c, want);
    assert!(c.deprecations().is_empty());
    assert!(alloy::lint::unknown_names(&c.lint).is_empty());

    // The keys the template writes are the ones the docs entry shows.
    let entry = alloy::docs::lookup("topic:config").expect("the alloy.toml entry");

    for line in alloy::config::TEMPLATE.lines() {
        assert!(entry.contains(line), "the alloy.toml entry lacks `{line}`");
    }
}

// --- 5. the projects that already exist ------------------------------------

/// The alloy.toml of PowerTraining, copied verbatim.
const POWER_TRAINING: &str = r#"#:schema .alloy/alloy.schema.json

[build]
in = "src"
out = "build"
clean = true
artifact = "ship"

[emit]
wait_timeout = 10

[fmt]
column_width = 100
indent_type = "spaces"
indent_width = 4
quote_style = "auto-prefer-single"

[lint]
strict = true

[flux]
typecheck = true

[alx.factory]
backend = "table"
create = "fluid.create"
interpolate = "plain"

[test]
out = "tests"
suite = "alloy"
lest = true
shim = true

[project]
name = "PowerTraining"
runtime = "@game/ReplicatedStorage/Alloy"
sourcemap = true
source_of_truth = true
mount_aliases = true

[mount]
server = ["src/server", "@game/ServerScriptService/Server"]
client = ["src/client", "@game/ReplicatedStorage/Client"]
shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
pkg = ["packages/roblox", "@game/ReplicatedStorage/Packages"]

[ingots]
enamel = "../../Rust/Languages/Alloy/ingots/crates/enamel"
"#;

/// The alloy.toml of SaberSimulator, copied verbatim.
const SABER_SIMULATOR: &str = r#"#:schema .alloy/alloy.schema.json
# Saber Simulator: a proof of concept game in Alloy.
[build]
in = "src"
out = "build"
clean = true
artifact = "ship"

[emit]
# a WaitForChild that never resolves would hang a script forever
wait_timeout = 10

[fmt]
column_width = 100
indent_type = "spaces"
indent_width = 4

[lint]
strict = true
deny = ["correctness"]
warn = ["naming"]

[flux]
typecheck = true

[test]
out = "tests"
suite = "saber"

# The .alx files build Instances through the `create` factory in
# src/shared/ui/create.aly: one curried call, children in the table.
[alx.factory]
backend = "table"
create = "create"
interpolate = "plain"

# Enamel: Tailwind's class names on .alx elements, from the ingots
# repository checked out beside the Alloy sources.
[ingots]
enamel = "../../Rust/Languages/Alloy/ingots/crates/enamel"

[project]
name = "SaberSimulator"
runtime = "@game/ReplicatedStorage/Alloy"

[mount]
server = ["src/server", "@game/ServerScriptService/Server"]
client = ["src/client", "@game/StarterPlayer/StarterPlayerScripts/Client"]
shared = ["src/shared", "@game/ReplicatedStorage/Shared"]
"#;

#[test]
fn the_projects_that_exist_still_load() {
    let power = parse(POWER_TRAINING);
    assert_eq!(power.project.name, "PowerTraining");
    assert!(power.lint.strict && power.lint.recommended);
    assert_eq!(power.fmt.quote_style, QuoteStyle::AutoPreferSingle);
    assert!(power.deprecations().is_empty());

    let saber = parse(SABER_SIMULATOR);
    assert_eq!(saber.project.name, "SaberSimulator");
    assert_eq!(level_of(&saber.lint, "optional_access"), Level::Deny);
    assert_eq!(level_of(&saber.lint, "naming_convention"), Level::Warn);
    assert_eq!(level_of(&saber.lint, "implicit_any"), Level::Warn);
    assert_eq!(
        saber.deprecations(),
        vec![
            "`[lint] deny` is deprecated; write `[lint.rules] correctness = \"deny\"`",
            "`[lint] warn` is deprecated; write `[lint.rules] naming = \"warn\"`",
        ]
    );

    // The two files on this machine, when they are there.
    for path in [
        "/mnt/new_volume/Programming/Alloy/PowerTraining/alloy.toml",
        "/mnt/new_volume/Programming/Alloy/SaberSimulator/alloy.toml",
    ] {
        let path = Path::new(path);

        if let Ok(text) = std::fs::read_to_string(path) {
            Config::parse(&text, path).unwrap_or_else(|e| panic!("{e}"));
        }
    }
}
