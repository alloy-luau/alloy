//! Supplying what generated code needs, without emitting a single `require`.
//!
//! Two kinds of name appear in output, and neither is a module reference:
//!
//! * **luaux's own helpers** — merging spread props, and reading a value that
//!   may be a source. Both are a line long and are **inlined** into files that
//!   use them. They are implementation details of a syntax feature, not
//!   something an author asked for, so making them install a package would be
//!   backwards.
//! * **`[factory]` entries** — whatever `create`, `children`, `event`, and
//!   `compute` name. luaux only checks that each one an emission actually
//!   referenced is in scope; the author imports their library in whatever style
//!   their project uses.
//!
//! That split is why luaux does not resolve module paths. A require is
//! *location-dependent* — `script.Parent.Packages.vide` means something
//! different from a file one directory deeper, and a relative path likewise — so
//! a single configured string cannot be correct for every file. An author's own
//! import always is. Resolving requires across a codebase is darklua's job, and
//! luaux composes with it rather than duplicating it.
//!
//! Injection is **line-preserving** (PLAN.md §5.5): helpers go on the same line
//! as the first statement. That is uglier than a clean preamble and it is the
//! right trade — a stack trace pointing at the wrong line costs more than tidy
//! generated output.

use crate::backend::Helpers;
use crate::compile::CompileError;
use crate::config::Config;
use crate::lexer::Lexer;
use std::collections::HashSet;

/// Merges prop groups for spread attributes.
///
/// String keys are last-wins so source order decides precedence; numeric keys
/// concatenate so a spread and literal children can coexist.
///
/// The return is annotated `any` deliberately. A merge of heterogeneous tables
/// has no type Luau can express, and left to inference the result comes out as
/// `{*error-type*}` — which makes every spread a type error at its call site:
///
/// ```text
/// TypeError: Expected this to be 'ButtonProps', but got '{*error-type*}'
/// ```
///
/// `any` costs the checking of a spread's *result* against the component's props
/// and buys back the checking of everything else in the file. That is the better
/// side of the trade while the alternative is an error on every use.
pub const MERGE_HELPER: &str = "__luaux_merge";

/// Reads a value that may be a Vide source.
///
/// Interpolated text builds a string, and a source is a function — so it has to
/// be called, while a plain value must pass through untouched.
///
/// The call is cast, for the same kind of reason the merge helper's return is
/// annotated: left to inference the `v()` inside constrains the parameter to
/// `() -> (a, b...)`, and the `type(v) == "function"` guard does not widen it
/// back. The helper then rejects every value that is *not* a source:
///
/// ```text
/// TypeError: Expected this to be '() -> (a, b...)', but got 'number'
/// ```
///
/// which is exactly backwards, since a plain value is the case that guard exists
/// to serve. The cast detaches the call from the parameter's type, which is
/// where the constraint came from.
///
/// The parameter is deliberately **not** annotated. `v: any` silences the error
/// too, but `any` subsumes `nil`, so the parameter becomes optional and a hole
/// holding a call that returns nothing — `{f()}` where `f` returns `()` — stops
/// being reported as an argument-count mismatch. Inference gives the better
/// signature here; the annotation only looks tidier.
pub const READ_HELPER: &str = "__luaux_read";

/// Both helpers on one line each, so injection stays line-preserving.
const MERGE_HELPER_SOURCE: &str = "local function __luaux_merge(...): any local m, n = {}, 0 \
for i = 1, select(\"#\", ...) do local g = select(i, ...) if g ~= nil then for k, v in g do \
if type(k) == \"number\" then n += 1 m[n] = v else m[k] = v end end end end return m end";

const READ_HELPER_SOURCE: &str =
    "local function __luaux_read(v) return if type(v) == \"function\" then (v :: () -> any)() else v end";

/// Prepends the helpers this output uses, and checks the factory is reachable.
pub fn inject(
    output: &str,
    helpers: Helpers,
    bound: &HashSet<String>,
    config: &Config,
) -> Result<String, CompileError> {
    // Every `[factory]` entry the emission actually referenced has to name
    // something the file can reach — and only those. A Vide project that never
    // interpolates text should not be told to import a `compute` wrapper it
    // does not use, which is the same rule the inlined helpers follow.
    let mut referenced: Vec<(&str, &str)> = Vec::new();

    if helpers.create {
        referenced.push(("create", &config.create));
    }

    if helpers.children {
        if let Some(children) = &config.children {
            referenced.push(("children", children));
        }
    }

    if helpers.event {
        if let Some(event) = &config.event {
            referenced.push(("event", event.expression()));
        }
    }

    if helpers.compute {
        if let Some(compute) = &config.compute {
            referenced.push(("compute", compute));
        }
    }

    if helpers.fragment {
        if let Some(fragment) = &config.fragment {
            referenced.push(("fragment", fragment));
        }
    }

    if helpers.merge {
        if let Some(merge) = &config.merge {
            referenced.push(("merge", merge));
        }
    }

    for (setting, expression) in referenced {
        let root = root_of(expression);

        if !bound.contains(root) {
            return Err(CompileError {
                message: format!("`{root}` is not in scope"),
                offset: 0,
                length: 0,
                help: Some(format!(
                    "import it, or point [factory] {setting} at something else \
                     (currently `{expression}`)"
                )),
            });
        }
    }

    let mut statements = Vec::new();

    if helpers.merge_props && !bound.contains(MERGE_HELPER) {
        statements.push(MERGE_HELPER_SOURCE);
    }

    if helpers.read && !bound.contains(READ_HELPER) {
        statements.push(READ_HELPER_SOURCE);
    }

    if statements.is_empty() {
        return Ok(output.to_string());
    }

    let preamble = format!("{}; ", statements.join("; "));

    Ok(match first_statement_offset(output) {
        Some(offset) => format!("{}{preamble}{}", &output[..offset], &output[offset..]),
        // Nothing but comments; there is no code to support anyway.
        None => output.to_string(),
    })
}

/// The binding a factory expression depends on.
///
/// Only the head of a name path can be one: for `vide.create` that is `vide`,
/// for the method form `scope:New` it is `scope`, and for `React.Event` it is
/// `React`. Splitting on `.` alone took `scope:New` for a single name, so a
/// file that had imported Fusion was told to import `scope:New`.
fn root_of(expression: &str) -> &str {
    expression
        .split(['.', ':'])
        .next()
        .unwrap_or(expression)
        .trim()
}

/// Offset of the first non-trivia token.
///
/// Injecting here rather than at byte zero keeps Luau's leading directives
/// working — `--!strict` and friends must precede all code, so putting a
/// `local` above them would silently disable strict mode.
fn first_statement_offset(source: &str) -> Option<usize> {
    let mut lexer = Lexer::new(source);

    while let Some(token) = lexer.next_token() {
        let token = token.ok()?;
        if !token.is_trivia() {
            return Some(token.start);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Left to inference the merged table comes out as `{*error-type*}`, which
    /// makes every spread a type error where its result is used — so a file with
    /// one spread loses the type checking of everything else in it.
    #[test]
    fn the_merge_helper_has_a_usable_return_type() {
        assert!(
            MERGE_HELPER_SOURCE.contains("__luaux_merge(...): any"),
            "{MERGE_HELPER_SOURCE}"
        );
    }

    /// Left to inference the `v()` inside constrains the parameter to a
    /// function, so interpolating anything else — a number, a string, a table —
    /// is a type error at the point the author wrote a perfectly ordinary value.
    ///
    /// The cast is what fixes that, and it is the only part that may not be
    /// simplified away. Annotating the parameter `any` silences the same error
    /// while making it optional, which costs the argument-count check on a hole
    /// holding a call that returns nothing.
    #[test]
    fn the_read_helper_accepts_a_value_that_is_not_a_source() {
        assert!(
            READ_HELPER_SOURCE.contains("(v :: () -> any)()"),
            "the cast is what detaches the call from the parameter's type: \
             {READ_HELPER_SOURCE}"
        );
        assert!(
            !READ_HELPER_SOURCE.contains("(v: any)"),
            "an `any` parameter is optional, and costs the arity check: \
             {READ_HELPER_SOURCE}"
        );
    }

    /// Both helpers are injected onto the line of the first statement, so a
    /// newline anywhere in either would shift every line below it.
    ///
    /// `lines()` will not do: it drops a trailing empty segment, so it reports
    /// one line for a literal that ends in the newline this is looking for.
    #[test]
    fn the_helpers_contain_no_newline() {
        assert!(!MERGE_HELPER_SOURCE.contains('\n'), "{MERGE_HELPER_SOURCE}");
        assert!(!READ_HELPER_SOURCE.contains('\n'), "{READ_HELPER_SOURCE}");
    }

    fn all() -> Helpers {
        Helpers {
            create: true,
            read: true,
            merge_props: true,
            ..Default::default()
        }
    }

    #[test]
    fn inlines_helpers_with_no_config_and_no_dependency() {
        let out = inject(
            "local x = 1",
            all(),
            &bound(&["create"]),
            &Config::with_create("create"),
        )
        .expect("inject");

        assert!(out.contains("local function __luaux_merge"), "{out}");
        assert!(out.contains("local function __luaux_read"), "{out}");
        assert!(!out.contains("require"), "no dependency: {out}");
    }

    #[test]
    fn inlines_only_what_is_used() {
        let helpers = Helpers {
            read: true,
            ..Default::default()
        };
        let out = inject(
            "local x = 1",
            helpers,
            &bound(&[]),
            &Config::with_create("create"),
        )
        .expect("inject");

        assert!(out.contains("__luaux_read"), "{out}");
        assert!(!out.contains("__luaux_merge"), "{out}");
    }

    #[test]
    fn respects_a_helper_the_author_already_defined() {
        let out = inject(
            "local x = 1",
            all(),
            &bound(&["create", MERGE_HELPER]),
            &Config::with_create("create"),
        )
        .expect("inject");
        assert!(!out.contains("local function __luaux_merge"), "{out}");
    }

    #[test]
    fn requires_the_factory_to_be_in_scope() {
        let helpers = Helpers {
            create: true,
            ..Default::default()
        };
        let error = inject(
            "local x = 1",
            helpers,
            &bound(&[]),
            &Config::with_create("create"),
        )
        .expect_err("should fail");

        assert!(
            error.message.contains("`create` is not in scope"),
            "{error:?}"
        );
    }

    #[test]
    fn checks_only_the_root_of_a_dotted_factory() {
        let helpers = Helpers {
            create: true,
            ..Default::default()
        };
        let config = Config::with_create("vide.create");

        // `vide` is the binding; `create` is a field on it.
        assert!(inject("local x = 1", helpers, &bound(&["vide"]), &config).is_ok());

        let error =
            inject("local x = 1", helpers, &bound(&["create"]), &config).expect_err("should fail");
        assert!(
            error.message.contains("`vide` is not in scope"),
            "{error:?}"
        );
    }

    /// The method form names its object, not itself. Splitting on `.` alone
    /// took `scope:New` for one name, so a file that had imported Fusion — or
    /// anything else reached through a method — was told to import `scope:New`.
    #[test]
    fn checks_the_object_a_method_factory_is_called_on() {
        let helpers = Helpers {
            create: true,
            ..Default::default()
        };
        let config = Config::with_create("scope:New");

        assert!(inject("local x = 1", helpers, &bound(&["scope"]), &config).is_ok());

        let error =
            inject("local x = 1", helpers, &bound(&["New"]), &config).expect_err("should fail");
        assert!(
            error.message.contains("`scope` is not in scope"),
            "{error:?}"
        );
    }

    #[test]
    fn injects_nothing_when_no_helper_is_used() {
        let source = "local x = 1";
        let out = inject(
            source,
            Helpers::default(),
            &bound(&[]),
            &Config::with_create("create"),
        )
        .expect("inject");
        assert_eq!(out, source);
    }

    #[test]
    fn preserves_the_line_count() {
        let source = "--!strict\nlocal x = 1\nreturn x";
        let out = inject(
            source,
            all(),
            &bound(&["create"]),
            &Config::with_create("create"),
        )
        .expect("inject");
        assert_eq!(out.lines().count(), source.lines().count(), "{out}");
    }

    #[test]
    fn goes_after_leading_directives_and_comments() {
        // `--!strict` must stay first, or strict mode silently turns off.
        let source = "--!strict\n-- a note\n\nlocal x = 1";
        let out = inject(
            source,
            all(),
            &bound(&["create"]),
            &Config::with_create("create"),
        )
        .expect("inject");

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "--!strict");
        assert_eq!(lines[1], "-- a note");
        assert!(lines[3].ends_with("local x = 1"), "{out}");
    }

    #[test]
    fn leaves_a_comment_only_file_alone() {
        let source = "-- nothing here\n";
        let out = inject(
            source,
            all(),
            &bound(&["create"]),
            &Config::with_create("create"),
        )
        .expect("inject");
        assert_eq!(out, source);
    }

    /// The in-scope check across every `[factory]` entry, not just `create`.
    mod factory {
        use super::*;

        fn fusion() -> Config {
            Config::parse(
                "[factory]\n\
                 backend = \"table\"\n\
                 create = \"scope:New\"\n\
                 children = \"Children\"\n\
                 event = \"OnEvent\"\n\
                 compute = \"scope:Computed\"\n",
            )
            .expect("config")
        }

        fn referencing(children: bool, event: bool, compute: bool) -> Helpers {
            Helpers {
                create: true,
                children,
                event,
                compute,
                ..Default::default()
            }
        }

        #[test]
        fn each_referenced_entry_has_to_be_bound() {
            for (helpers, missing) in [
                (referencing(true, false, false), "Children"),
                (referencing(false, true, false), "OnEvent"),
            ] {
                let error = inject("local x = 1", helpers, &bound(&["scope"]), &fusion())
                    .expect_err("should fail");

                assert!(
                    error
                        .message
                        .contains(&format!("`{missing}` is not in scope")),
                    "{error:?}"
                );
            }
        }

        /// The diagnostic has to name the setting that wanted the binding.
        /// "`Children` is not in scope" alone sends someone looking for a
        /// module; the help line is what points at their own config.
        #[test]
        fn the_diagnostic_names_the_setting() {
            let error = inject(
                "local x = 1",
                referencing(true, false, false),
                &bound(&["scope"]),
                &fusion(),
            )
            .expect_err("should fail");

            let help = error.help.expect("help");
            assert!(help.contains("[factory] children"), "{help}");
            assert!(help.contains("`Children`"), "{help}");
        }

        /// Same rule the inlined helpers follow: only what a file actually
        /// referenced is demanded. A Vide-shaped module with no spreads should
        /// not gain a merge helper, and a Fusion one that never interpolates
        /// text should not be told to import a wrapper it does not use.
        #[test]
        fn an_unreferenced_entry_is_not_demanded() {
            inject(
                "local x = 1",
                referencing(false, false, false),
                &bound(&["scope"]),
                &fusion(),
            )
            .expect("nothing but create was referenced");
        }

        /// `scope:New` and `scope:Computed` share a root, so one binding
        /// satisfies both — the check is about reachability, not about how many
        /// settings happen to name the same object.
        #[test]
        fn one_binding_can_satisfy_several_settings() {
            inject(
                "local x = 1",
                referencing(false, false, true),
                &bound(&["scope"]),
                &fusion(),
            )
            .expect("scope covers create and compute");
        }

        /// React reaches three settings through one import. A project that
        /// destructures `createElement` instead has to bind the others too, and
        /// the error should say which one it is short of.
        #[test]
        fn a_dotted_library_is_reached_through_its_root() {
            let react = Config::parse(
                "[factory]\nbackend = \"table\"\ncreate = \"React.createElement\"\nevent = \"React.Event.\"\n",
            )
            .expect("config");

            inject(
                "local x = 1",
                referencing(false, true, false),
                &bound(&["React"]),
                &react,
            )
            .expect("one React binding covers create and event");

            let error = inject(
                "local x = 1",
                referencing(false, true, false),
                &bound(&["createElement"]),
                &react,
            )
            .expect_err("should fail");

            assert!(
                error.message.contains("`React` is not in scope"),
                "{error:?}"
            );
        }

        /// Setting `compute` removes a dependency rather than adding one: the
        /// reader comes from the callback, so nothing is inlined.
        #[test]
        fn compute_does_not_inline_the_read_helper() {
            let out = inject(
                "local x = 1",
                referencing(false, false, true),
                &bound(&["scope"]),
                &fusion(),
            )
            .expect("inject");

            assert!(!out.contains("__luaux_read"), "{out}");
        }
    }
}
