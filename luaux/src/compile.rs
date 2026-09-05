//! Compiling a `.luaux` source to `.luau`.
//!
//! The transform is local (PLAN.md §5.1): LuauX regions are replaced and every
//! other byte passes through untouched. Expressions captured inside LuauX are
//! themselves compiled, so `{cond and <X/> or nil}` works at any depth.

use crate::backend::{Backend, EmitContext, EmitError};
use crate::config::{Config, LintLevel};
use crate::lexer::{LexError, Lexer};
use crate::lint;
use crate::markup::{self, Attribute, AttributeValue, Child, Element, Fragment, Node};
use crate::markup_scan::Scanner;
use crate::resolve::{blank_luaux_regions, Resolver};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub message: String,
    pub offset: usize,
    /// Length of the offending text, for underlining. Zero means "point here".
    pub length: usize,
    /// Suggestion shown separately from the message.
    pub help: Option<String>,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for CompileError {}

impl From<LexError> for CompileError {
    fn from(error: LexError) -> Self {
        Self {
            message: error.message,
            offset: error.offset,
            length: 0,
            help: None,
        }
    }
}

impl From<markup::ParseError> for CompileError {
    fn from(error: markup::ParseError) -> Self {
        // An empty hole is usually a deletion accident; a value-less one in an
        // attribute is someone trying to comment where a value is required.
        let help = if error.message.contains("is empty") {
            Some("remove it, or put an expression in it".to_string())
        } else if error.message.contains("contains no value") {
            Some("a comment only stands in for a child, not a prop".to_string())
        } else {
            None
        };

        Self {
            message: error.message,
            offset: error.offset,
            length: 0,
            help,
        }
    }
}

impl From<EmitError> for CompileError {
    fn from(error: EmitError) -> Self {
        Self {
            message: error.message,
            offset: error.offset,
            length: error.length,
            help: error.help,
        }
    }
}

/// A non-fatal diagnostic. Same shape as an error so the CLI renders both the
/// same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub message: String,
    pub offset: usize,
    pub length: usize,
    pub help: Option<String>,
}

/// Compiles with the default config for the arrangement `backend` emits.
///
/// The config has to match the backend, and `Config::default()` alone no longer
/// does: it is React's, so pairing it with the one-table backend emitted React's
/// names around a curried constructor — `React.createElement("Frame")({ … })` —
/// which is neither library's shape. A caller with a `luaux.toml` wants
/// [`compile_configured`]; this is the no-config entry point, and no-config has
/// to mean the default *for what you asked for*.
pub fn compile(source: &str, backend: &dyn Backend) -> Result<String, CompileError> {
    let config = match backend.name() {
        "element" => Config::default(),
        _ => Config::bare(),
    };

    Ok(compile_configured(source, backend, config)?.0)
}

/// A compile that produced output, and everything it has to say about it.
///
/// `errors` is not a contradiction: a resolution error — an unknown element, an
/// attribute a class does not have — is **recovered** from, so the output is
/// complete and still Luau while being wrong at those positions. That split is
/// what lets a language server report every mistake at once and still hand the
/// generated file to a type checker, instead of one mistake costing the whole
/// file's analysis.
///
/// A caller that writes the output to disk must treat a non-empty `errors` as
/// failure — [`compile_configured`] does exactly that.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub output: String,
    pub warnings: Vec<Warning>,
    /// Resolution errors, in source order. Recovered from, never guessed at.
    pub errors: Vec<CompileError>,
}

/// Compiles with project aliases, recovering from resolution errors.
///
/// Parse errors still stop the compile and come back as `Err`: recovering from
/// `<Frame` with no `>` means deciding what the author meant, and a wrong guess
/// buries the real mistake under invented ones. Resolution errors need no such
/// guess — the tree is already built and each name is checked on its own — so
/// they are collected and the file carries on.
pub fn compile_recovering(
    source: &str,
    backend: &dyn Backend,
    config: Config,
) -> Result<Compiled, CompileError> {
    compile_inner(source, backend, config)
}

/// Compiles with project aliases from `luaux.toml` (PLAN.md §8).
///
/// Fails on the first resolution error, so output that reaches disk is never
/// knowingly wrong. Tooling that wants every error at once — and the output
/// alongside them — wants [`compile_recovering`].
pub fn compile_configured(
    source: &str,
    backend: &dyn Backend,
    config: Config,
) -> Result<(String, Vec<Warning>), CompileError> {
    let compiled = compile_inner(source, backend, config)?;

    match compiled.errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok((compiled.output, compiled.warnings)),
    }
}

fn compile_inner(
    source: &str,
    backend: &dyn Backend,
    config: Config,
) -> Result<Compiled, CompileError> {
    // Name resolution needs the whole file, so it runs once up front: blank the
    // LuauX regions (`.luaux` is not parseable as Luau), collect every binding,
    // and thread the result through — including into nested expressions, which
    // are compiled separately but must still see the file's bindings.
    let spans = luaux_spans(source)?;
    let blanked = blank_luaux_regions(source, &spans);
    let level = config.static_conditional_child;
    let imports = config.clone();
    let resolver = Resolver::new(&blanked, config);

    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut helpers = crate::backend::Helpers::default();
    let output = compile_with(
        source,
        backend,
        &resolver,
        level,
        &mut warnings,
        &mut errors,
        &mut helpers,
    )?;

    // Only the outermost call injects: nested expressions are compiled on their
    // own but are spliced back into this file, so their helper usage belongs to
    // this preamble.
    let output = crate::imports::inject(&output, helpers, resolver.bound(), &imports)?;

    // Source order, because a list of diagnostics is read top to bottom and
    // nested regions are compiled out of order.
    errors.sort_by_key(|error| error.offset);

    Ok(Compiled {
        output,
        warnings,
        errors,
    })
}

/// The byte spans of every markup region, outermost only. Alloy patch: the
/// caller blanks them to collect bindings with its own parser.
pub fn markup_spans(source: &str) -> Result<Vec<(usize, usize)>, CompileError> {
    luaux_spans(source)
}

#[doc(hidden)]
pub fn luaux_spans_for_test(source: &str) -> Vec<(usize, usize)> {
    luaux_spans(source).unwrap_or_default()
}

/// Byte ranges of the outermost LuauX regions. Nested LuauX lies inside these, so
/// blanking the outer ranges is enough to make the file parseable.
fn luaux_spans(source: &str) -> Result<Vec<(usize, usize)>, CompileError> {
    let mut lexer = Lexer::new(source);
    let mut scanner = Scanner::new(source);
    let mut spans = Vec::new();

    while let Some(token) = lexer.next_token() {
        let token = token?;
        let lookahead = Lexer::at(source, token.end);

        if !scanner.feed(token, &lookahead) {
            continue;
        }

        let (_, end) = markup::parse_node(source, token.start)?;
        spans.push((token.start, end));

        lexer.seek(end);
        scanner.note_luaux_region();
    }

    Ok(spans)
}

#[allow(clippy::too_many_arguments)]
fn compile_with(
    source: &str,
    backend: &dyn Backend,
    resolver: &Resolver,
    level: LintLevel,
    warnings: &mut Vec<Warning>,
    errors: &mut Vec<CompileError>,
    helpers: &mut crate::backend::Helpers,
) -> Result<String, CompileError> {
    let mut lexer = Lexer::new(source);
    let mut scanner = Scanner::new(source);
    let context = EmitContext::new(source, resolver);

    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;

    // Lexing and LuauX parsing interleave. A `.luaux` file is not Luau end to end
    // — LuauX text is a different lexical mode where `don't`, a backtick, or `--`
    // has no Luau meaning — so the lexer runs only until a LuauX region opens,
    // then resumes past it.
    loop {
        let token = match lexer.next_token() {
            None => break,
            Some(token) => token?,
        };

        let lookahead = Lexer::at(source, token.end);

        if !scanner.feed(token, &lookahead) {
            continue;
        }

        let (mut node, end) = markup::parse_node(source, token.start)?;
        compile_embedded(
            &mut node, backend, resolver, level, warnings, errors, helpers,
        )?;

        out.push_str(&source[cursor..token.start]);

        // Whatever happens next, the errors already recorded are real and come
        // first. Losing them to a later fatal one would report the downstream
        // mistake and hide the upstream one.
        let emitted = backend.emit(&node, &context);
        errors.extend(context.take_errors().into_iter().map(CompileError::from));

        out.push_str(&emitted?);
        cursor = end;

        *helpers |= context.helpers();

        lexer.seek(end);
        scanner.note_luaux_region();
    }

    out.push_str(&source[cursor..]);
    Ok(out)
}

/// Compiles and then re-parses the output as Luau (PLAN.md §5.4, step 8).
///
/// A codegen bug that emits invalid Luau should surface here, not on the user's
/// next rojo sync. One parse turns a confusing downstream failure into a clear
/// internal error.
pub fn compile_verified(
    source: &str,
    backend: &dyn Backend,
    config: &Config,
) -> Result<(String, Vec<Warning>), CompileError> {
    let (output, warnings) = compile_configured(source, backend, config.clone())?;

    if let Err(errors) =
        full_moon::parse_fallible(&output, full_moon::LuaVersion::luau()).into_result()
    {
        let detail = errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");

        return Err(CompileError {
            message: format!(
                "internal error: the {} backend emitted invalid Luau — {detail}",
                backend.name()
            ),
            offset: 0,
            length: 0,
            help: Some("this is a luaux bug; please report it".into()),
        });
    }

    Ok((output, warnings))
}

/// Compiles LuauX appearing inside captured Luau expressions.
///
/// Expressions are held verbatim, so nested LuauX is still source text at this
/// point. Recursing here is what makes `{items:map(function() return <X/> end)}`
/// work.
#[allow(clippy::too_many_arguments)]
fn compile_embedded(
    node: &mut Node,
    backend: &dyn Backend,
    resolver: &Resolver,
    level: LintLevel,
    warnings: &mut Vec<Warning>,
    errors: &mut Vec<CompileError>,
    helpers: &mut crate::backend::Helpers,
) -> Result<(), CompileError> {
    match node {
        Node::Element(element) => {
            compile_element(element, backend, resolver, level, warnings, errors, helpers)
        }
        Node::Fragment(fragment) => compile_fragment(
            fragment, backend, resolver, level, warnings, errors, helpers,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_element(
    element: &mut Element,
    backend: &dyn Backend,
    resolver: &Resolver,
    level: LintLevel,
    warnings: &mut Vec<Warning>,
    errors: &mut Vec<CompileError>,
    helpers: &mut crate::backend::Helpers,
) -> Result<(), CompileError> {
    for attribute in &mut element.attributes {
        match attribute {
            Attribute::Spread { expression, .. } => {
                *expression = compile_with(
                    expression, backend, resolver, level, warnings, errors, helpers,
                )?;
            }
            Attribute::Named { value, .. } => {
                if let AttributeValue::Expression(expression) = value {
                    *expression = compile_with(
                        expression, backend, resolver, level, warnings, errors, helpers,
                    )?;
                }
            }
            // An inferred name comes from a dotted path, which cannot contain
            // markup — but the walk stays exhaustive rather than assuming that,
            // so relaxing the inference rule later cannot quietly skip a region.
            Attribute::Inferred { expression, .. } => {
                *expression = compile_with(
                    expression, backend, resolver, level, warnings, errors, helpers,
                )?;
            }
        }
    }

    compile_children(
        &mut element.children,
        backend,
        resolver,
        level,
        warnings,
        errors,
        helpers,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_fragment(
    fragment: &mut Fragment,
    backend: &dyn Backend,
    resolver: &Resolver,
    level: LintLevel,
    warnings: &mut Vec<Warning>,
    errors: &mut Vec<CompileError>,
    helpers: &mut crate::backend::Helpers,
) -> Result<(), CompileError> {
    // Text in an element becomes its `Text` property. A fragment has no element
    // to carry one, under either arrangement — it is a plain table in the first
    // and a component with no props in the second — so the text has nowhere to
    // go, and the backend used to drop it without a word.
    if let Some(Child::Text { text, span }) = fragment
        .children
        .iter()
        .find(|child| matches!(child, Child::Text { .. }))
    {
        return Err(CompileError {
            message: "a fragment cannot hold text".to_string(),
            offset: span.start,
            length: span.end.saturating_sub(span.start),
            help: Some(format!(
                "text becomes an element's Text property, and a fragment is not \
                 an element; wrap it in one, as <TextLabel>{text}</TextLabel>"
            )),
        });
    }

    compile_children(
        &mut fragment.children,
        backend,
        resolver,
        level,
        warnings,
        errors,
        helpers,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_children(
    children: &mut [Child],
    backend: &dyn Backend,
    resolver: &Resolver,
    level: LintLevel,
    warnings: &mut Vec<Warning>,
    errors: &mut Vec<CompileError>,
    helpers: &mut crate::backend::Helpers,
) -> Result<(), CompileError> {
    for child in children {
        match child {
            Child::Node(node) => {
                compile_embedded(node, backend, resolver, level, warnings, errors, helpers)?
            }
            Child::Expression { expression, span } => {
                // §11.1 runs on the original text, where LuauX is still `<...>`.
                if level != LintLevel::Off {
                    let spans = luaux_spans(expression).unwrap_or_default();

                    if lint::has_unwrapped_luaux(expression, &spans) {
                        let warning = lint::static_conditional_child(
                            span.start,
                            expression.len(),
                            resolver.compute(),
                        );

                        if level == LintLevel::Error {
                            return Err(CompileError {
                                message: warning.message,
                                offset: warning.offset,
                                length: warning.length,
                                help: warning.help,
                            });
                        }

                        warnings.push(warning);
                    }
                }

                *expression = compile_with(
                    expression, backend, resolver, level, warnings, errors, helpers,
                )?;
                // Rule 2 — checked after compiling, so nested LuauX has already
                // become ordinary Luau and the expression parses.
                lint::check_conditional_child(expression, span.start)?;
            }
            Child::Text { .. } | Child::Comment { .. } => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Table;

    /// Fixtures do not import Vide, so the factory-in-scope check would fire.
    /// That check is covered directly by `imports::tests`.
    fn test_config() -> Config {
        Config::with_create("create")
    }

    /// Bound on line 1 so the factory-in-scope check passes without changing
    /// any fixture's line count.
    const BINDING: &str = "local create = _G.create; ";

    fn try_build(source: &str) -> Result<String, CompileError> {
        compile_configured(&format!("{BINDING}{source}"), &Table, test_config())
            .map(|(output, _)| output)
    }

    fn build(source: &str) -> String {
        strip_preamble(&try_build(source).expect("compile"))
    }

    /// Removes injected helper `require`s so a test can assert on the code it
    /// is actually about. Covered directly by `imports::tests`.
    fn strip_preamble(output: &str) -> String {
        let mut rest = output;

        while let Some(end) = rest.find("; ") {
            let head = &rest[..end];

            let injected = head.starts_with("local function __luaux_")
                || head == BINDING.trim_end_matches("; ");

            if !injected {
                break;
            }

            rest = &rest[end + 2..];
        }

        rest.to_string()
    }

    /// Message and help joined, so assertions can check either half.
    fn build_err(source: &str) -> String {
        let error = try_build(source).expect_err("should fail");
        match error.help {
            Some(help) => format!("{} — {help}", error.message),
            None => error.message,
        }
    }

    #[test]
    fn passes_through_sources_without_luaux() {
        let source = "local x = 1\nreturn x < 2\n";
        assert_eq!(build(source), source);
    }

    #[test]
    fn preserves_surrounding_source_exactly() {
        assert_eq!(
            build("local a = 1\nlocal e = <Frame/>\nreturn a"),
            "local a = 1\nlocal e = create(\"Frame\")({})\nreturn a"
        );
    }

    #[test]
    fn emits_attributes() {
        assert_eq!(
            build(r#"local e = <Frame Name='a' Size={UDim2.new(1, 0)} Visible />"#),
            "local e = create(\"Frame\")({ Name = 'a', Size = UDim2.new(1, 0), Visible = true })"
        );
    }

    #[test]
    fn emits_nested_children() {
        assert_eq!(
            build("local e = <Frame><TextLabel/></Frame>"),
            "local e = create(\"Frame\")({ create(\"TextLabel\")({}) })"
        );
    }

    #[test]
    fn emits_text_as_the_text_property() {
        assert_eq!(
            build("local e = <TextLabel>Hello</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"Hello\" })"
        );
    }

    #[test]
    fn interpolated_text_is_reactive() {
        // The headline case: `count` is a Vide source, so the text has to be a
        // thunk that reads it, not a string that stringifies the function.
        assert_eq!(
            build("local e = <TextLabel>Clicked {count} times</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = function() return `Clicked {__luaux_read(count)} times` end })"
        );
        assert_eq!(
            build("local e = <TextLabel>Name: {name}</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = function() return `Name: {__luaux_read(name)}` end })"
        );
    }

    #[test]
    fn a_lone_expression_needs_no_thunk() {
        // Vide already treats a function on a property key as a source, so
        // `Text = label` is right whether label is a source or a plain string.
        assert_eq!(
            build("local e = <TextButton>{label}</TextButton>"),
            "local e = create(\"TextButton\")({ Text = label })"
        );
    }

    #[test]
    fn literal_only_text_stays_a_plain_string() {
        assert_eq!(
            build("local e = <TextLabel>Hello</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"Hello\" })"
        );
    }

    #[test]
    fn text_children_override_a_text_attribute() {
        // PROPOSAL.md Rule 5.
        assert_eq!(
            build(r#"local e = <TextLabel Text="A">B</TextLabel>"#),
            "local e = create(\"TextLabel\")({ Text = \"B\" })"
        );
    }

    #[test]
    fn escapes_braces_and_backticks_in_text() {
        // PROPOSAL.md Examples 10 and 13.
        assert_eq!(
            build(r"local e = <TextLabel>literal \{text}</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"literal {text}\" })"
        );
        assert_eq!(
            build(r"local e = <TextLabel>slash \\{text}</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = function() return `slash \\\\{__luaux_read(text)}` end })"
        );
        assert_eq!(
            build("local e = <TextLabel>a ` b</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"a ` b\" })"
        );
    }

    #[test]
    fn expression_children_are_emitted_verbatim() {
        // Deliberately *not* wrapped in a one-element table: Vide iterates
        // children with generalised `for k, v in t`, which skips an absent key
        // rather than stopping at it, so a nil child cannot truncate its
        // siblings. Verified against real Vide in tests/runtime.
        assert_eq!(
            build("local e = <Frame>{cond and child or nil}</Frame>"),
            "local e = create(\"Frame\")({ cond and child or nil })"
        );
    }

    #[test]
    fn a_fragment_rejects_text_rather_than_dropping_it() {
        // A fragment has no element to carry a `Text` property, so text in one
        // has nowhere to go. It used to vanish silently.
        for source in [
            "local a = <>hello</>",
            "local a = <>Count: {n} items</>",
            "local a = <Frame><>text</></Frame>",
        ] {
            let error = build_err(source);
            assert!(
                error.contains("fragment cannot hold text"),
                "{source}: {error}"
            );
        }

        // An expression child is fine — a table slot can hold one.
        assert!(build("local a = <>{n}</>").contains("{ n }"));
    }

    #[test]
    fn emits_fragments_as_plain_tables() {
        // Vide recurses tables in numeric position, so a fragment needs no
        // runtime representation of its own.
        assert_eq!(
            build("local e = (<><Frame/><TextLabel/></>)"),
            "local e = ({ create(\"Frame\")({}), create(\"TextLabel\")({}) })"
        );
    }

    #[test]
    fn handles_luaux_text_the_luau_lexer_could_not() {
        // Apostrophes, backticks and `--` are meaningless in LuauX text but would
        // each derail a whole-file Luau tokenizer.
        assert_eq!(
            build("local e = <TextLabel>don't</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"don't\" })"
        );
        assert_eq!(
            build("local e = <TextLabel>a -- b</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"a -- b\" })"
        );
        assert_eq!(
            build("local e = <TextLabel>[[x]]</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"[[x]]\" })"
        );
    }

    #[test]
    fn resumes_lexing_correctly_after_a_region() {
        // The scanner treats a consumed region as an expression, so the `<`
        // that follows is a comparison, not a second region.
        assert_eq!(
            build("local ok = count < 2\nlocal e = <Frame/>\nlocal also = count < 3"),
            "local ok = count < 2\nlocal e = create(\"Frame\")({})\nlocal also = count < 3"
        );
    }

    #[test]
    fn bound_names_are_components() {
        assert_eq!(
            build("local Receipt = require('./Receipt')\nlocal e = <Receipt Name={n} />"),
            "local Receipt = require('./Receipt')\nlocal e = Receipt({ Name = n })"
        );
        // Dotted names never need a binding — no Roblox class contains a dot.
        assert_eq!(build("local e = <Foo.Bar/>"), "local e = Foo.Bar({})");
    }

    fn build_with(source: &str, config: &str) -> String {
        strip_preamble(
            &compile_configured(
                &format!("{BINDING}{source}"),
                &Table,
                crate::Config::parse(&format!("[factory]\nbackend = \"table\"\n{config}"))
                    .expect("config"),
            )
            .expect("compile")
            .0,
        )
    }

    /// Warnings raised while compiling with the default config.
    fn warnings_for(source: &str) -> Vec<Warning> {
        compile_configured(&format!("{BINDING}{source}"), &Table, test_config())
            .expect("compile")
            .1
    }

    fn build_with_err(source: &str, config: &str) -> String {
        let error = compile_configured(
            &format!("{BINDING}{source}"),
            &Table,
            crate::Config::parse(&format!("[factory]\nbackend = \"table\"\n{config}"))
                .expect("config"),
        )
        .expect_err("should fail");
        match error.help {
            Some(help) => format!("{} — {help}", error.message),
            None => error.message,
        }
    }

    #[test]
    fn text_children_are_double_quoted_and_escape_double_quotes() {
        // An apostrophe is common in UI copy and now needs no escape.
        assert_eq!(
            build("local e = <TextLabel>don't</TextLabel>"),
            "local e = create(\"TextLabel\")({ Text = \"don't\" })"
        );

        // A double quote does, and the result must still parse.
        let out = compile_verified(
            &format!("{BINDING}local e = <TextLabel>say \"hi\"</TextLabel>"),
            &Table,
            &test_config(),
        );
        let out = out.expect("valid Luau").0;
        assert!(out.contains(r#"Text = "say \"hi\"""#), "{out}");
    }

    #[test]
    fn element_aliases_resolve_to_canonical_classes() {
        // The attribute keeps its single quotes: a string literal is the
        // author's own Luau, re-emitted byte for byte. Only strings luaux
        // *builds* — text children — are double-quoted.
        assert_eq!(
            build_with(
                "local e = <text Text='hi'/>",
                "[elements]\nTextLabel = \"text\"\n"
            ),
            "local e = create(\"TextLabel\")({ Text = 'hi' })"
        );
    }

    #[test]
    fn property_aliases_emit_canonical_names() {
        // Emitted code is identical regardless of the project's aliases.
        assert_eq!(
            build_with(
                "local e = <Frame bgColor={c}/>",
                "[properties.Frame]\nBackgroundColor3 = \"bgColor\"\n"
            ),
            "local e = create(\"Frame\")({ BackgroundColor3 = c })"
        );
    }

    #[test]
    fn an_alias_retires_the_original_spelling() {
        let message = build_with_err(
            "local e = <TextLabel/>",
            "[elements]\nTextLabel = \"text\"\n",
        );
        assert!(message.contains("use <text>"), "{message}");

        let message = build_with_err(
            "local e = <Frame BackgroundColor3={c}/>",
            "[properties.Frame]\nBackgroundColor3 = \"bgColor\"\n",
        );
        assert!(message.contains("use bgColor"), "{message}");
    }

    #[test]
    fn unknown_attributes_are_rejected() {
        // PROPOSAL.md's own examples carried this bug: a Frame has
        // BackgroundColor3, not Color3.
        let message = build_err("local e = <Frame Color3={c}/>");
        assert!(
            message.contains("no property or event named Color3"),
            "{message}"
        );
        assert!(message.contains("did you mean"), "{message}");

        // Inherited properties and events are both accepted.
        assert!(try_build("local e = <Frame BackgroundColor3={c}/>").is_ok());
        assert!(try_build("local e = <Frame Name='a'/>").is_ok());
        assert!(try_build("local e = <TextButton Activated={f}/>").is_ok());

        // Components take arbitrary props, so nothing is checked there.
        assert!(try_build("local Row = f()\nlocal e = <Row Whatever={1}/>").is_ok());
    }

    #[test]
    fn read_only_properties_are_rejected() {
        // ContentText exists on TextLabel but cannot be assigned.
        let message = build_err("local e = <TextLabel ContentText='x'/>");
        assert!(
            message.contains("no property or event named ContentText"),
            "{message}"
        );
    }

    #[test]
    fn unbound_names_are_rejected() {
        // The correctness gap this closes: without resolution, `<Frmae/>`
        // compiled to a call to an undefined global and only failed at runtime.
        let message = build_err("local e = <Frmae/>");
        assert!(message.contains("did you mean <Frame>"), "{message}");

        let message = build_err("local e = <Receipt/>");
        assert!(message.contains("not defined"), "{message}");
    }

    #[test]
    fn resolution_sees_bindings_from_the_whole_file() {
        // A component used inside a nested expression still resolves, even
        // though that expression is compiled on its own.
        assert!(try_build(
            "local Row = require('./Row')\nlocal e = <Frame>{cond and <Row/> or nil}</Frame>"
        )
        .is_ok());

        // And one declared *after* its use.
        assert!(try_build("local e = <Frame>{Row}</Frame>\nlocal function Row() end").is_ok());
    }

    #[test]
    fn compiles_luaux_nested_inside_expressions() {
        assert_eq!(
            build("local e = <Frame>{cond and <TextLabel/> or nil}</Frame>"),
            "local e = create(\"Frame\")({ cond and create(\"TextLabel\")({}) or nil })"
        );
    }

    #[test]
    fn compiles_luaux_inside_attribute_expressions() {
        assert_eq!(
            build("local e = <Frame Size={f(<TextLabel/>)} />"),
            "local e = create(\"Frame\")({ Size = f(create(\"TextLabel\")({})) })"
        );
    }

    #[test]
    fn emits_merge_props_for_spreads() {
        assert_eq!(
            build("local e = <Frame {props} Name={n} />"),
            "local e = create(\"Frame\")(__luaux_merge(props, { Name = n }))"
        );
    }

    #[test]
    fn compiles_multiple_sites() {
        assert_eq!(
            build("local a = <Frame/>\nlocal b = <TextLabel/>"),
            "local a = create(\"Frame\")({})\nlocal b = create(\"TextLabel\")({})"
        );
    }

    #[test]
    fn rejects_text_on_a_class_without_a_text_property() {
        assert!(build_err("local e = <Frame>hello</Frame>").contains("no Text property"));
    }

    /// Every resolution error at once, and output alongside them.
    ///
    /// Stopping at the first costs the author every other diagnostic *and* the
    /// generated Luau, so nothing downstream — a type checker, a language
    /// server — can say anything about the file either. One mistake should cost
    /// one diagnostic.
    fn recovered(source: &str) -> Compiled {
        compile_recovering(&format!("{BINDING}{source}"), &Table, test_config())
            .expect("a resolution error is not fatal")
    }

    #[test]
    fn every_unknown_element_is_reported_not_just_the_first() {
        let compiled = recovered("local e = <Frmae><Recieve/><Buton/></Frmae>");

        let messages: Vec<&str> = compiled
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .collect();
        assert_eq!(messages.len(), 3, "{messages:#?}");

        assert!(messages[0].contains("Frmae"), "{messages:#?}");
        assert!(messages[1].contains("Recieve"), "{messages:#?}");
        assert!(messages[2].contains("Buton"), "{messages:#?}");
    }

    #[test]
    fn errors_are_reported_in_source_order() {
        let compiled = recovered("local e = <Frame><Buton/><Aaa/></Frame>");
        let offsets: Vec<usize> = compiled.errors.iter().map(|error| error.offset).collect();

        assert_eq!(offsets.len(), 2);
        assert!(offsets[0] < offsets[1], "{offsets:?}");
    }

    #[test]
    fn every_unknown_attribute_is_reported() {
        let compiled = recovered("local e = <Frame Nonsense={1} Rubbish={2}/>");
        assert_eq!(compiled.errors.len(), 2, "{:#?}", compiled.errors);
    }

    /// The output is still Luau, so whatever reads it next keeps working. That
    /// is the whole point of recovering rather than collecting.
    #[test]
    fn a_file_with_a_bad_tag_still_produces_luau() {
        let compiled = recovered("local e = <Frmae Size={1}/>\nlocal after = 1");

        assert!(compiled.output.contains("Frmae"), "{}", compiled.output);
        assert!(compiled.output.contains("after"), "{}", compiled.output);
        full_moon::parse_fallible(&compiled.output, full_moon::LuaVersion::luau())
            .into_result()
            .expect("recovered output is still Luau");
    }

    /// An unknown tag has an unknown class, so its attributes cannot be checked
    /// against anything — reporting them would be one mistake reported four
    /// times.
    #[test]
    fn attributes_of_an_unknown_element_do_not_cascade() {
        let compiled = recovered("local e = <Frmae Text='a' Size={1} Visible/>");
        assert_eq!(compiled.errors.len(), 1, "{:#?}", compiled.errors);
    }

    #[test]
    fn text_children_of_an_unknown_element_do_not_cascade() {
        let compiled = recovered("local e = <Frmae>hello</Frmae>");
        assert_eq!(compiled.errors.len(), 1, "{:#?}", compiled.errors);
    }

    /// Recovery is for resolution, not for parsing. Guessing what an unclosed
    /// tag meant buries the real mistake under invented ones.
    #[test]
    fn a_parse_error_is_still_fatal() {
        assert!(
            compile_recovering(&format!("{BINDING}local e = <Frame"), &Table, test_config())
                .is_err()
        );
    }

    /// Output that reaches disk is never knowingly wrong, so the entry point the
    /// CLI uses still fails on the first error.
    #[test]
    fn the_writing_entry_point_still_fails() {
        assert!(try_build("local e = <Frmae/>").is_err());
    }

    #[test]
    fn rejects_bare_text_on_a_component() {
        assert!(build_err("local e = <Receipt>hello</Receipt>").contains("component"));
    }

    #[test]
    fn helpers_are_inlined_not_required() {
        // luaux emits no `require` at all: helpers are inlined and the factory
        // is the author's own import.
        let out = try_build("local e = <Frame>{--[[c]]}</Frame>").expect("compile");
        assert!(!out.contains("require"), "{out}");
    }

    #[test]
    fn inlines_the_merge_helper_only_when_a_spread_is_used() {
        let plain = try_build("local e = <Frame/>").expect("compile");
        assert!(!plain.contains("__luaux_merge"), "{plain}");

        let spread = try_build("local p = {}\nlocal e = <Frame {p}/>").expect("compile");
        assert!(spread.contains("local function __luaux_merge"), "{spread}");
    }

    #[test]
    fn inlines_read_only_when_text_interpolates() {
        let plain = try_build("local e = <TextLabel>Hi</TextLabel>").expect("compile");
        assert!(!plain.contains("__luaux_read"), "{plain}");

        let interpolated =
            try_build("local e = <TextLabel>Hi {name}</TextLabel>").expect("compile");
        assert!(
            interpolated.contains("local function __luaux_read"),
            "{interpolated}"
        );
    }

    #[test]
    fn helpers_used_only_inside_nested_expressions_are_still_inlined() {
        let out = try_build(
            "local cond = true\nlocal e = <Frame>{cond and <TextLabel>a {b}</TextLabel> or nil}</Frame>",
        )
        .expect("compile");
        assert!(out.contains("local function __luaux_read"), "{out}");
    }

    #[test]
    fn the_factory_must_be_in_scope() {
        let error = compile_configured("local e = <Frame/>", &Table, Config::with_create("create"))
            .expect_err("should fail");
        assert!(
            error.message.contains("`create` is not in scope"),
            "{error:?}"
        );

        // And a dotted factory checks its root.
        let ok = compile_configured(
            "local vide = require('./vide')\nlocal e = <Frame/>",
            &Table,
            Config::with_create("vide.create"),
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// A factory reached through a method call has to *emit* as well as
    /// resolve. `scope:New("Frame")({})` is legal Luau — a method call is a
    /// function call, so calling its result is too — and the spread form has to
    /// hold up the same way. Neither was covered by the in-scope test alone,
    /// which never reaches codegen.
    #[test]
    fn a_method_factory_emits_and_reparses() {
        // The spread case below overflows the harness's own 2 MB inside
        // full_moon — the very thing the CLI's `STACK` exists for.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let config = Config::with_create("scope:New");

                let (plain, _) = compile_verified(
                    "local scope = _G.s\nlocal e = <Frame Size={1}/>",
                    &Table,
                    &config,
                )
                .expect("valid Luau");
                assert!(
                    plain.contains("scope:New(\"Frame\")({ Size = 1 })"),
                    "{plain}"
                );

                let (spread, _) = compile_verified(
                    "local scope = _G.s\nlocal p = {}\nlocal e = <Frame {p} Size={1}/>",
                    &Table,
                    &config,
                )
                .expect("valid Luau");
                assert!(
                    spread.contains("scope:New(\"Frame\")(__luaux_merge(p, { Size = 1 }))"),
                    "{spread}"
                );
            })
            .expect("spawn")
            .join()
            .expect("method factory thread");
    }

    /// `const` is Luau, and the import it declares binds like any other.
    ///
    /// Collecting bindings missed it, which failed in the least useful way
    /// available: a file that imported Vide on its first line was told Vide was
    /// not in scope, with a help line suggesting it import Vide.
    #[test]
    fn a_const_import_satisfies_the_factory() {
        let ok = compile_configured(
            "const vide = require('./vide')\nlocal e = <Frame/>",
            &Table,
            Config::with_create("vide.create"),
        );
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn warns_about_conditional_children_that_never_update() {
        // §11.1 — the LuauX is built once, so the condition looks live but is not.
        let warnings = warnings_for("local e = <Frame>{cond() and <TextLabel/> or nil}</Frame>");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].message.contains("built once"), "{warnings:?}");

        // Wrapped in a function, Vide tracks it — no warning.
        assert!(warnings_for(
            "local e = <Frame>{function() return cond() and <TextLabel/> or nil end}</Frame>"
        )
        .is_empty());

        // The idiomatic map form must not nag.
        assert!(warnings_for(
            "local e = <Frame>{items:map(function(i) return <TextLabel/> end)}</Frame>"
        )
        .is_empty());

        // No LuauX in the expression, nothing to say.
        assert!(warnings_for("local e = <Frame>{items}</Frame>").is_empty());
    }

    #[test]
    fn the_lint_level_is_configurable() {
        let source = "local e = <Frame>{cond() and <TextLabel/> or nil}</Frame>";

        let off = compile_configured(
            &format!("{BINDING}{source}"),
            &Table,
            crate::Config::parse(
                "[factory]\nbackend = \"table\"\n[lints]\nstatic_conditional_child = \"off\"\n",
            )
            .expect("config"),
        )
        .expect("compile");
        assert!(off.1.is_empty());

        let escalated = compile_configured(
            &format!("{BINDING}{source}"),
            &Table,
            crate::Config::parse(
                "[factory]\nbackend = \"table\"\n[lints]\nstatic_conditional_child = \"error\"\n",
            )
            .expect("config"),
        );
        assert!(escalated.is_err());
    }

    #[test]
    fn comments_are_dropped_by_default_and_kept_on_request() {
        let source = "local e = <Frame><!-- why -->{--[[ how ]]}<TextLabel/></Frame>";

        // Default: no trace of them.
        // Emitted as Luau block comments, and — crucially — without a comma,
        // since a comment is not a table field.
        let kept = build(source);
        assert!(kept.contains("--[[ why ]]"), "{kept}");
        assert!(kept.contains("--[[ how ]]"), "{kept}");
        assert!(
            !kept.contains("]],"),
            "a comment must not take a comma: {kept}"
        );
    }

    #[test]
    fn a_multi_line_markup_comment_keeps_its_shape() {
        let source = format!(
            "{BINDING}local e = (\n  <Frame>\n    <!--\n      first line\n      second line\n    -->\n    <TextLabel>x</TextLabel>\n  </Frame>\n)"
        );
        let out = build(&source);

        // The opening bracket must not drag the first line up with it.
        assert!(out.contains("--[[\n      first line"), "{out}");
        assert!(out.contains("second line\n    ]]"), "{out}");

        // And the emission still spans exactly the lines the source did.
        assert_eq!(out.lines().count(), source.lines().count(), "{out}");
    }

    #[test]
    fn a_comment_ending_in_a_bracket_does_not_close_early() {
        // `]` meeting the closing `]]` would form `]]]` and terminate a byte
        // early, leaving a stray bracket as code.
        let out = compile_verified(
            &format!("{BINDING}local e = <Frame><!-- see list[1] --></Frame>"),
            &Table,
            &test_config(),
        );
        assert!(out.is_ok(), "{out:?}");

        let nested = compile_verified(
            &format!("{BINDING}local e = <Frame><!-- a ]] b --></Frame>"),
            &Table,
            &test_config(),
        );
        assert!(nested.is_ok(), "{nested:?}");
        assert!(
            nested.unwrap().0.contains("--[=["),
            "bracket level must rise"
        );
    }

    #[test]
    fn a_kept_comment_beside_no_value_still_emits_valid_luau() {
        // `{ --[[c]] }` — a table holding only a comment.
        let out = compile_verified(
            &format!("{BINDING}local e = <Frame><!-- only --></Frame>"),
            &Table,
            &test_config(),
        );
        assert!(out.is_ok(), "{out:?}");
    }

    #[test]
    fn rejects_nil_on_the_left_of_and() {
        // PROPOSAL.md Rule 2. `cond and nil or x` is always `x` in Lua, so the
        // condition silently does nothing.
        let message = build_err("local e = <Frame>{cond and nil or child}</Frame>");
        assert!(message.contains("no effect"), "{message}");
        assert!(message.contains("if cond then"), "{message}");

        // The faithful shape is fine — an element is always truthy.
        assert!(try_build("local e = <Frame>{cond and child or nil}</Frame>").is_ok());
        assert!(try_build("local e = <Frame>{if cond then a else b}</Frame>").is_ok());
    }

    #[test]
    fn rejects_ambiguous_expression_beside_element_children() {
        // On a Text-bearing class, `{label}` could be the text or a child, and
        // guessing emits code that fails inside Vide at runtime.
        let message = build_err("local e = <TextButton>{label}<UICorner/></TextButton>");
        assert!(message.contains("unclear"), "unexpected: {message}");
        assert!(message.contains("Text={...}"), "unexpected: {message}");

        // Unambiguous either side of it.
        assert!(try_build("local e = <TextButton>Click<UICorner/></TextButton>").is_ok());
        assert!(try_build("local e = <TextButton>{label}</TextButton>").is_ok());
        assert!(try_build("local e = <Frame>{child}<TextLabel/></Frame>").is_ok());
    }

    /// Multi-line fixtures, shared by the line-preservation and validity tests.
    const MULTILINE_FIXTURES: &[&str] = &[
        "local e = (\n  <Frame\n    Size={size}\n    Visible\n  >\n    <TextLabel>Hi</TextLabel>\n  </Frame>\n)\n",
        "local e = (\n  <Frame>\n    <TextLabel>\n      Clicked {count} times\n    </TextLabel>\n    <UICorner/>\n  </Frame>\n)\n",
        "local e = (\n  <>\n    <Frame/>\n    <TextLabel/>\n  </>\n)\n",
        "local Button = f()\nlocal e = (\n  <Button\n    OnClick={function()\n      count(count() + 1)\n    end}\n  />\n)\n",
        "local e = (\n  <Frame>\n    {cond and (\n      <TextLabel/>\n    ) or nil}\n  </Frame>\n)\n",
        "local e = (\n  <Frame\n    {props}\n    Name={n}\n  />\n)\n",
        // Nothing between the tags. Every fixture above has an attribute or a
        // child, so none of them reached the empty-table path — which is how it
        // came to lose lines unnoticed.
        "local e = (\n  <Frame>\n  </Frame>\n)\n",
        "local e = (\n  <Frame>\n\n  </Frame>\n)\n",
        "local e = (\n  <>\n  </>\n)\n",
        "local e = (\n  <Frame>\n    <TextLabel>\n    </TextLabel>\n  </Frame>\n)\n",
        // A spread *after* a named attribute. This is the shape that splits
        // into two groups, and the group that is not last must not carry the
        // emission to the closing tag — handed the close, the first group burnt
        // every line the element had left, so the spread landed on the closing
        // tag with blank lines above it. Every spread fixture here used to put
        // the spread first or use spreads only, which is exactly why nothing
        // caught it.
        "local e = (\n  <Frame\n    Name={n}\n    {props}\n  />\n)\n",
        "local e = (\n  <Frame\n    Name={n}\n    {props}\n  >\n    <TextLabel/>\n  </Frame>\n)\n",
        // The same, with the spread holding an expression of its own lines. The
        // one above loses *placement*; this one lost the line **count**, because
        // a verbatim push from the closing line adds newlines the source never
        // had.
        "local e = (\n  <Frame\n    Name={n}\n    {f(\n      base\n    )}\n  />\n)\n",
        // Nothing *but* spreads. These emit no table, so the closing brace that
        // usually carries the emission down to the closing tag is never written
        // — the fixture above with a `Name` beside its spread hides that, since
        // the named attribute is enough to bring the table back.
        "local e = (\n  <Frame\n    {props}\n  />\n)\n",
        "local e = (\n  <Frame\n    {props}\n  >\n  </Frame>\n)\n",
        "local e = (\n  <Frame\n    {props}\n    {props}\n  />\n)\n",
    ];

    /// The generated `.luau` must have the same number of lines as the `.luaux`
    /// it came from (PLAN.md §5.5). Luau has no runtime source maps, so matching
    /// line numbers are the only thing making a stack trace or a luau-lsp
    /// diagnostic point at the right place.
    #[test]
    fn output_preserves_line_count() {
        for fixture in MULTILINE_FIXTURES {
            let compiled = build(fixture);
            assert_eq!(
                compiled.lines().count(),
                fixture.lines().count(),
                "line count changed\n--- in ---\n{fixture}\n--- out ---\n{compiled}"
            );
        }
    }

    /// An element with nothing in it is still as tall as it was written.
    ///
    /// The closing brace follows the closing tag on every other path; the empty
    /// one used to emit `{}` and stop, which shortened the file by however many
    /// lines the element spanned. That is invisible in the output — it is
    /// correct Luau, just in the wrong place — and it costs the whole file its
    /// luau-lsp answers, because a map built on matching line numbers then lines
    /// nothing up.
    #[test]
    fn an_empty_element_spanning_lines_keeps_them() {
        let source = "local e = (\n  <Frame>\n\n  </Frame>\n)\nreturn e\n";
        let compiled = build(source);

        assert_eq!(
            compiled.lines().count(),
            source.lines().count(),
            "{compiled}"
        );

        // And the statement after it is still on its own line, which is the
        // property that actually matters to a stack trace.
        let lines: Vec<&str> = compiled.lines().collect();
        assert!(lines[1].contains("create(\"Frame\")"), "{compiled}");
        assert!(lines[5].contains("return e"), "{compiled}");
    }

    /// One line in, one line out: the fix must not start breaking tables that
    /// were never multi-line to begin with.
    #[test]
    fn an_empty_element_on_one_line_stays_on_one_line() {
        let compiled = build("local e = <Frame></Frame>\n");

        assert_eq!(compiled.lines().count(), 1, "{compiled}");
        assert!(compiled.contains("create(\"Frame\")({})"), "{compiled}");
    }

    /// A spread lands on its own line even when it is the first thing in the
    /// element.
    ///
    /// Every group after the first is positioned before it is written; the first
    /// was not, which a table survives — it positions its own entries — and a
    /// spread does not. The spread came out on the opening tag's line and the
    /// line it was written on came out blank, so hovering it asked luau-lsp
    /// about an empty line.
    #[test]
    fn a_leading_spread_lands_on_its_source_line() {
        let source = "local e = (\n  <Frame\n    {props}\n    Name={n}\n  />\n)\n";
        let compiled = build(source);
        let lines: Vec<&str> = compiled.lines().collect();

        assert_eq!(
            compiled.lines().count(),
            source.lines().count(),
            "{compiled}"
        );
        assert!(lines[2].contains("props"), "{compiled}");
        assert!(lines[3].contains("Name = n"), "{compiled}");
    }

    #[test]
    fn entries_land_on_their_source_lines() {
        let source = "local e = (\n  <Frame\n    Size={size}\n    Visible\n  >\n    <TextLabel>Hi</TextLabel>\n  </Frame>\n)\n";
        let compiled = build(source);
        let lines: Vec<&str> = compiled.lines().collect();

        assert!(lines[1].contains("create(\"Frame\")"), "{compiled}");
        assert!(lines[2].contains("Size = size"), "{compiled}");
        assert!(lines[3].contains("Visible = true"), "{compiled}");
        assert!(lines[5].contains("create(\"TextLabel\")"), "{compiled}");
    }

    /// Every compiled fixture must re-parse as Luau. This is the guard that
    /// stops a codegen bug reaching the user's rojo sync.
    #[test]
    fn generated_output_is_valid_luau() {
        let fixtures = [
            "local e = <Frame/>",
            r#"local e = <Frame Name='a' Size={UDim2.new(1, 0)} Visible />"#,
            "local e = <Frame><TextLabel>Hi</TextLabel></Frame>",
            "local e = <TextLabel>Name: {name}</TextLabel>",
            "local e = (<><Frame/><TextLabel/></>)",
            "local e = <Frame>{cond and <TextLabel/> or nil}</Frame>",
            "local e = <Frame {props} Name={n} />",
            "local Receipt = f()\nlocal e = <Receipt Name={n}><TextLabel>Debit</TextLabel></Receipt>",
            "local e = <TextLabel>don't</TextLabel>",
            r"local e = <TextLabel>slash \\{text}</TextLabel>",
            "return function(props) return <Frame>{props.children}</Frame> end",
        ];

        // full_moon's recursive-descent parser has large stack frames in debug
        // builds — enough to exhaust a test thread's 2 MB on the inlined merge
        // helper. Give it room rather than shrinking the fixtures to suit the
        // harness. The CLI runs on a larger stack for the same reason; see
        // `STACK` in luaux-cli's main.rs.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                for fixture in fixtures.iter().chain(MULTILINE_FIXTURES.iter()) {
                    let bound = format!("{BINDING}{fixture}");
                    compile_verified(&bound, &Table, &test_config())
                        .unwrap_or_else(|error| panic!("{fixture}\n  -> {error}"));
                }
            })
            .expect("spawn")
            .join()
            .expect("verification thread");
    }

    /// The `[factory]` variables (factory-plan.md), exercised through the shape
    /// they exist to serve. Fusion is the same *arrangement* as Vide — one
    /// curried constructor taking one table — with four things inside it moved,
    /// which is exactly the claim these keys make.
    mod factory {
        use super::*;

        /// Parsed rather than built by hand, so every test here also covers the
        /// config validation that stands between a typo and luaux's own
        /// "please report it" internal error.
        fn fusion_config() -> Config {
            Config::parse(
                "[factory]\nbackend = \"table\"\n\
                 create = \"scope:New\"\n\
                 children = \"Children\"\n\
                 event = \"OnEvent\"\n\
                 compute = \"scope:Computed\"\n",
            )
            .expect("config")
        }

        /// Bound on line 1, like the outer `BINDING`, so no fixture's line
        /// count moves.
        const BINDING: &str = "local scope, Children, OnEvent = _G.a, _G.b, _G.c; ";

        pub(super) fn build(source: &str) -> String {
            let output = compile_configured(&format!("{BINDING}{source}"), &Table, fusion_config())
                .map(|(output, _)| output)
                .expect("compile");

            strip_preamble(&output)
                .strip_prefix(BINDING)
                .expect("binding")
                .to_string()
        }

        #[test]
        fn children_go_under_the_configured_key() {
            assert_eq!(
                build("local e = <Frame Size={s}><UICorner/></Frame>"),
                "local e = scope:New(\"Frame\")({ Size = s, [Children] = { scope:New(\"UICorner\")({}) } })"
            );
        }

        /// `<Frame/>` must not gain `[Children] = {}`. An empty entry is not
        /// wrong so much as noise in every leaf of a tree, and leaves are most
        /// of a tree (factory-plan.md §3.1).
        #[test]
        fn an_element_with_no_children_gains_no_entry() {
            assert_eq!(
                build("local e = <Frame/>"),
                "local e = scope:New(\"Frame\")({})"
            );
        }

        /// The README promises components and intrinsics are interchangeable at
        /// the call site. Handing one children under a key and the other in the
        /// array part would break that over a guess about what a component
        /// expects of its own props.
        #[test]
        fn children_apply_to_components_too() {
            assert_eq!(
                build("local Card = f()\nlocal e = <Card Color={c}><TextLabel/></Card>"),
                "local Card = f()\nlocal e = Card({ Color = c, [Children] = { scope:New(\"TextLabel\")({}) } })"
            );
        }

        #[test]
        fn an_event_becomes_a_wrapped_key() {
            assert_eq!(
                build("local e = <TextButton Activated={fire}>Fire</TextButton>"),
                "local e = scope:New(\"TextButton\")({ [OnEvent(\"Activated\")] = fire, Text = \"Fire\" })"
            );
        }

        /// A property is not an event, and wrapping one would assign the
        /// handler to a key the library never reads.
        #[test]
        fn a_property_is_left_alone() {
            assert_eq!(
                build("local e = <Frame Size={s}/>"),
                "local e = scope:New(\"Frame\")({ Size = s })"
            );
        }

        /// `is_event` can only answer for an intrinsic. A component's props are
        /// arbitrary, so wrapping a name that happens to be an event on *some*
        /// class would be a guess — and a wrong guess here is silent, because
        /// the component simply never sees the prop it was passed.
        #[test]
        fn a_component_prop_is_never_wrapped_as_an_event() {
            assert_eq!(
                build("local Card = f()\nlocal e = <Card Activated={fire}/>"),
                "local Card = f()\nlocal e = Card({ Activated = fire })"
            );
        }

        /// The headline of `compute`: interpolated text is the one place LuauX
        /// generates reactivity, and it is the one place the two libraries
        /// disagree about how.
        #[test]
        fn interpolated_text_uses_the_configured_wrapper() {
            assert_eq!(
                build("local e = <TextLabel>HP {health} / {max}</TextLabel>"),
                "local e = scope:New(\"TextLabel\")({ Text = scope:Computed(function(use) return `HP {use(health)} / {use(max)}` end) })"
            );
        }

        /// Setting `compute` *removes* a dependency: a project configured this
        /// way never sees `__luaux_read` in its output, because the reader comes
        /// from the callback instead.
        #[test]
        fn the_read_helper_is_not_inlined_under_compute() {
            let output = compile_configured(
                &format!("{BINDING}local e = <TextLabel>HP {{health}}</TextLabel>"),
                &Table,
                fusion_config(),
            )
            .map(|(output, _)| output)
            .expect("compile");

            assert!(!output.contains("__luaux_read"), "{output}");
        }

        /// Unchanged in every mode. Vide accepts a source on a property key and
        /// Fusion accepts a state object, so neither needs a wrapper for the
        /// value it was already handed.
        #[test]
        fn a_single_expression_stays_bare() {
            assert_eq!(
                build("local e = <TextLabel>{label}</TextLabel>"),
                "local e = scope:New(\"TextLabel\")({ Text = label })"
            );
        }

        /// Nothing reactive about a literal, so no wrapper and no reader.
        #[test]
        fn literal_text_is_untouched() {
            assert_eq!(
                build("local e = <TextLabel>Hi</TextLabel>"),
                "local e = scope:New(\"TextLabel\")({ Text = \"Hi\" })"
            );
        }

        /// A spread still splits the props into groups, and the children entry
        /// joins the last of them rather than escaping the merge.
        #[test]
        fn a_spread_and_children_coexist() {
            assert_eq!(
                build("local e = <Frame {props} Size={s}><UICorner/></Frame>"),
                "local e = scope:New(\"Frame\")(__luaux_merge(props, { Size = s, [Children] = { scope:New(\"UICorner\")({}) } }))"
            );
        }

        /// A fragment is a plain table under both libraries — Fusion's `Child`
        /// recurses arrays to any depth — so `children` deliberately does not
        /// reach it (factory-plan.md §1.2).
        #[test]
        fn a_fragment_needs_no_children_key() {
            assert_eq!(
                build("local e = <><Frame/></>"),
                "local e = { scope:New(\"Frame\")({}) }"
            );
        }

        /// The `[Children] = {` wrapper adds a nesting level the writer has to
        /// carry without adding a newline of its own. It is the change most
        /// likely to break the line-preservation invariant, so every multiline
        /// fixture runs under this config too.
        #[test]
        fn output_preserves_line_count() {
            for fixture in MULTILINE_FIXTURES {
                let compiled = build(fixture);
                assert_eq!(
                    compiled.lines().count(),
                    fixture.lines().count(),
                    "line count changed\n--- in ---\n{fixture}\n--- out ---\n{compiled}"
                );
            }
        }

        /// A bare function is what Vide tracks; under Fusion it is just a
        /// value, and the reactive form is the configured wrapper. Suggesting
        /// the wrong one sends someone to write code that silently does
        /// nothing, which is the failure this lint exists to catch.
        #[test]
        fn the_lint_suggests_the_configured_wrapper() {
            let (_, warnings) = compile_configured(
                &format!("{BINDING}local e = <Frame>{{cond and <TextLabel/> or nil}}</Frame>"),
                &Table,
                fusion_config(),
            )
            .expect("compile");

            let help = warnings
                .first()
                .expect("a warning")
                .help
                .as_deref()
                .expect("help");

            assert!(
                help.contains("scope:Computed(function() return ... end)"),
                "{help}"
            );
        }

        /// Both closing braces target the closing tag's line, and `writer.to` is
        /// a no-op the second time, so they land together rather than the inner
        /// one stranding a line.
        #[test]
        fn children_close_on_the_closing_tag() {
            let compiled = build("local e = (\n  <Frame>\n    <UICorner/>\n  </Frame>\n)\n");

            assert_eq!(
                compiled,
                "local e = (\n  scope:New(\"Frame\")({ [Children] = {\n    scope:New(\"UICorner\")({}),\n  } })\n)\n"
            );
        }
    }

    /// The element backend (backend-plan.md §3) — `F(class, props, children)`.
    ///
    /// React is the shape these are written against, and the differences from
    /// the one-table arrangement are the ones a backend exists to carry:
    /// children in a third argument, components through the factory, and a
    /// fragment that is an element rather than a plain table.
    mod element {
        use super::*;
        use crate::backend::Element;

        fn react_config() -> Config {
            Config::parse(
                "[factory]\n\
                 backend = \"element\"\n\
                 create = \"React.createElement\"\n\
                 event = \"React.Event.\"\n\
                 fragment = \"React.Fragment\"\n\
                 interpolate = \"plain\"\n",
            )
            .expect("config")
        }

        /// Bound on line 1, so no fixture's line count moves. One binding
        /// covers `createElement`, `Event`, and `Fragment` — they share a root.
        const BINDING: &str = "local React = _G.react; ";

        pub(super) fn build(source: &str) -> String {
            let output =
                compile_configured(&format!("{BINDING}{source}"), &Element, react_config())
                    .map(|(output, _)| output)
                    .expect("compile");

            strip_preamble(&output)
                .strip_prefix(BINDING)
                .expect("binding")
                .to_string()
        }

        #[test]
        fn children_are_the_third_argument() {
            assert_eq!(
                build("local e = <Frame Size={s}><UICorner/></Frame>"),
                "local e = React.createElement(\"Frame\", { Size = s }, { React.createElement(\"UICorner\", {}) })"
            );
        }

        /// A leaf takes no third argument at all, rather than an empty table
        /// nothing reads.
        #[test]
        fn a_leaf_has_no_children_argument() {
            assert_eq!(
                build("local e = <Frame/>"),
                "local e = React.createElement(\"Frame\", {})"
            );
        }

        /// The arrangement's own difference: a component is the factory's first
        /// argument, not a function to call. Under the one-table backend the
        /// same source emits `Card({...})`.
        #[test]
        fn a_component_goes_through_the_factory() {
            assert_eq!(
                build("local Card = f()\nlocal e = <Card Color={c}><TextLabel/></Card>"),
                "local Card = f()\nlocal e = React.createElement(Card, { Color = c }, { React.createElement(\"TextLabel\", {}) })"
            );
        }

        /// `React.Event.Activated` is a field access, which the call form
        /// `[E(\"Activated\")]` cannot express — the gap backend-plan.md §5.3
        /// closes with the trailing dot.
        #[test]
        fn an_event_is_indexed_rather_than_called() {
            assert_eq!(
                build("local e = <TextButton Activated={fire}>Fire</TextButton>"),
                "local e = React.createElement(\"TextButton\", { [React.Event.Activated] = fire, Text = \"Fire\" })"
            );
        }

        /// A plain table is a fragment under a one-table library and is not one
        /// here, so it needs a component of its own.
        #[test]
        fn a_fragment_is_an_element() {
            assert_eq!(
                build("local e = <><Frame/><TextLabel/></>"),
                "local e = React.createElement(React.Fragment, nil, { React.createElement(\"Frame\", {}), React.createElement(\"TextLabel\", {}) })"
            );
        }

        /// No wrapper, no reader, no helper. Under a library with no per-prop
        /// reactivity a hole is already a value, and reading it would mean
        /// calling something the author meant to interpolate.
        #[test]
        fn interpolated_text_is_a_plain_string() {
            assert_eq!(
                build("local e = <TextLabel>HP {health} / {max}</TextLabel>"),
                "local e = React.createElement(\"TextLabel\", { Text = `HP {health} / {max}` })"
            );
        }

        #[test]
        fn the_read_helper_is_not_inlined_under_plain_interpolation() {
            let output = compile_configured(
                &format!("{BINDING}local e = <TextLabel>HP {{health}}</TextLabel>"),
                &Element,
                react_config(),
            )
            .map(|(output, _)| output)
            .expect("compile");

            assert!(!output.contains("__luaux_read"), "{output}");
        }

        #[test]
        fn a_spread_merges_into_the_props_argument() {
            assert_eq!(
                build("local e = <Frame {props} Size={s}><UICorner/></Frame>"),
                "local e = React.createElement(\"Frame\", __luaux_merge(props, { Size = s }), { React.createElement(\"UICorner\", {}) })"
            );
        }

        /// §12.1, answered against real react-lua under Lune rather than by
        /// reading the source. A nil child leaves a hole in the table, and the
        /// hole is *correct*: React renders nothing for it and every sibling
        /// keeps its index, so a conditional child that toggles does not remount
        /// the children after it.
        ///
        /// An earlier version of this backend compacted the list through an
        /// inlined helper, on the theory that `#` on a holed table could
        /// truncate. It does not — a table constructor presizes its array part —
        /// and compacting would have *moved* later children, which under React's
        /// implicit keys is the remount it was meant to prevent.
        #[test]
        fn an_expression_child_keeps_its_position() {
            assert_eq!(
                build("local e = <Frame>{items}</Frame>"),
                "local e = React.createElement(\"Frame\", {}, { items })"
            );
        }

        /// No helper, for any shape of children. The element backend inlines
        /// nothing of its own.
        #[test]
        fn children_never_cost_a_helper() {
            for source in [
                "local e = <Frame>{items}</Frame>",
                "local e = <Frame><UICorner/>{items}</Frame>",
                "local e = <><Frame/>{items}</>",
            ] {
                let output =
                    compile_configured(&format!("{BINDING}{source}"), &Element, react_config())
                        .map(|(output, _)| output)
                        .expect("compile");

                assert!(!output.contains("__luaux_children"), "{source}: {output}");
            }
        }

        /// Element children can never be nil, so they cost no helper.
        /// The whole point of tracking a helper is inlining it. Every other
        /// test here strips the preamble, so a helper that was referenced and
        /// never emitted looked exactly like one that worked — until the
        /// generated file called an undefined global at runtime. That is a bug
        /// this suite actually shipped, caught by building a file end to end.
        #[test]
        fn a_referenced_helper_is_actually_inlined() {
            let output = compile_configured(
                &format!("{BINDING}local e = <Frame {{props}} Size={{s}}/>"),
                &Element,
                react_config(),
            )
            .map(|(output, _)| output)
            .expect("compile");

            assert!(output.contains("__luaux_merge(props,"), "{output}");
            assert!(
                output.contains("local function __luaux_merge"),
                "referenced but never inlined:\n{output}"
            );
        }

        /// The in-scope check has to reach a real compile, not just
        ///  called directly.  is only ever referenced
        /// from inside an emission, so this is the path that proves it.
        #[test]
        fn an_unbound_fragment_is_caught_in_a_real_compile() {
            let error = compile_configured(
                "local createElement = _G.c; local e = <><Frame/></>",
                &Element,
                Config::parse(
                    "[factory]
backend = \"element\"
create = \"createElement\"
fragment = \"Frag\"
",
                )
                .expect("config"),
            )
            .expect_err("should fail");

            assert!(
                error.message.contains("`Frag` is not in scope"),
                "{error:?}"
            );
        }

        #[test]
        fn element_children_need_no_helper() {
            let output = compile_configured(
                &format!("{BINDING}local e = <Frame><UICorner/></Frame>"),
                &Element,
                react_config(),
            )
            .map(|(output, _)| output)
            .expect("compile");

            assert!(!output.contains("__luaux_children"), "{output}");
        }

        /// The props table is *not* the last argument here, so it must not span
        /// to the closing tag — those lines belong to the children. Getting this
        /// wrong collapses the children onto the closing tag's line while the
        /// line count still checks out, which is why it has its own test.
        #[test]
        fn the_children_argument_owns_the_closing_line() {
            assert_eq!(
                build("local e = (\n  <Frame>\n    <UICorner/>\n  </Frame>\n)\n"),
                "local e = (\n  React.createElement(\"Frame\", {}, {\n    React.createElement(\"UICorner\", {}),\n  })\n)\n"
            );
        }

        #[test]
        fn output_preserves_line_count() {
            for fixture in MULTILINE_FIXTURES {
                let compiled = build(fixture);
                assert_eq!(
                    compiled.lines().count(),
                    fixture.lines().count(),
                    "line count changed\n--- in ---\n{fixture}\n--- out ---\n{compiled}"
                );
            }
        }

        /// Every fixture has to re-parse as Luau. The compaction helper's
        /// argument list is the shape most likely to get this wrong, since a
        /// call takes no trailing comma where a table does.
        #[test]
        fn every_fixture_reparses() {
            // full_moon's recursive-descent parser has large stack frames in
            // debug builds — enough to exhaust a test thread's 2 MB. Same reason
            // the one-table suite spawns, and the same reason the CLI sets
            // `STACK` in luaux-cli's main.rs.
            std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(|| {
                    for fixture in MULTILINE_FIXTURES {
                        let source = format!("{BINDING}{fixture}");
                        compile_verified(&source, &Element, &react_config())
                            .unwrap_or_else(|error| panic!("{fixture}\n  -> {error}"));
                    }
                })
                .expect("spawn")
                .join()
                .expect("verification thread");
        }
    }

    /// `={expr}` — the property is inferred from the expression.
    ///
    /// The spelling is `=` rather than a bare `{Text}` because a bare hole in
    /// attribute position already means a spread, and one syntax cannot mean two
    /// things. Everything after the name is decided stays shared with a written
    /// attribute: aliases, the property check, event wrapping, and Rule 5.
    mod inferred {
        use super::*;

        #[test]
        fn a_bare_name_names_itself() {
            assert_eq!(
                build("local e = <TextLabel ={Text}/>"),
                "local e = create(\"TextLabel\")({ Text = Text })"
            );
        }

        #[test]
        fn a_dotted_path_names_its_last_segment() {
            assert_eq!(
                build("local e = <TextLabel ={props.Text}/>"),
                "local e = create(\"TextLabel\")({ Text = props.Text })"
            );
            assert_eq!(
                build("local e = <Frame ={self.props.BackgroundColor3}/>"),
                "local e = create(\"Frame\")({ BackgroundColor3 = self.props.BackgroundColor3 })"
            );
        }

        #[test]
        fn several_can_sit_beside_written_attributes() {
            assert_eq!(
                build("local e = <Frame Name=\"n\" ={props.Size} ={props.Visible}/>"),
                "local e = create(\"Frame\")({ Name = \"n\", Size = props.Size, Visible = props.Visible })"
            );
        }

        /// The inferred name is a name like any other, so a class that has no
        /// such property says so — with the underline on the shorthand.
        #[test]
        fn an_unknown_property_is_still_rejected() {
            let error = try_build("local e = <Frame ={props.Nonsense}/>").expect_err("should fail");
            assert!(
                error
                    .message
                    .contains("no property or event named Nonsense"),
                "{error:?}"
            );
        }

        /// An expression that is not a name has no name to take. Recovered from
        /// rather than fatal, because it is a mistake in one attribute — the
        /// same reason an unknown property is.
        #[test]
        fn an_expression_that_names_nothing_is_reported() {
            let error = try_build("local e = <Frame ={getProps().Size}/>").expect_err("fail");

            assert!(
                error
                    .message
                    .contains("cannot tell which property this names"),
                "{error:?}"
            );
            assert!(
                error.help.expect("help").contains("={props.Text}"),
                "the help has to show the shapes that do work"
            );
        }

        /// One unusable shorthand costs its own diagnostic, not the file's.
        #[test]
        fn the_rest_of_the_file_still_compiles() {
            let compiled = compile_recovering(
                &format!("{BINDING}local a = <Frame ={{f()}}/>\nlocal b = <Frame Name=\"ok\"/>"),
                &Table,
                test_config(),
            )
            .expect("compile");

            assert_eq!(compiled.errors.len(), 1, "{:?}", compiled.errors);
            assert!(
                compiled.output.contains("Name = \"ok\""),
                "{}",
                compiled.output
            );
        }

        /// `={...}` decides *which* name, and nothing else. Once it has one, the
        /// attribute takes the same path a written one does.
        #[test]
        fn the_inferred_name_goes_through_aliases() {
            let compiled = build_with(
                "local e = <Frame ={props.bgColor}/>",
                "[properties.Frame]\nBackgroundColor3 = \"bgColor\"\n",
            );

            assert_eq!(
                compiled,
                "local e = create(\"Frame\")({ BackgroundColor3 = props.bgColor })"
            );
        }

        /// Likewise for events, which are wrapped by the same rule.
        #[test]
        fn an_inferred_event_is_wrapped() {
            assert_eq!(
                factory::build("local e = <TextButton ={props.Activated}/>"),
                "local e = scope:New(\"TextButton\")({ [OnEvent(\"Activated\")] = props.Activated })"
            );
        }

        /// Rule 5 applies to an inferred `Text` exactly as to a written one.
        #[test]
        fn text_between_the_tags_still_wins() {
            assert_eq!(
                build("local e = <TextLabel ={props.Text}>Body</TextLabel>"),
                "local e = create(\"TextLabel\")({ Text = \"Body\" })"
            );
        }

        /// Resolving before the skip also means a *retired* spelling is now
        /// rejected here, where Rule 5 used to drop it unexamined. That is the
        /// README's exclusive-rename rule applied consistently: once a property
        /// is renamed the old spelling is an error everywhere, and the presence
        /// of text children is no reason to exempt it.
        #[test]
        fn a_retired_text_spelling_is_still_an_error() {
            let error = compile_configured(
                &format!("{BINDING}local e = <TextLabel Text=\"A\">Body</TextLabel>"),
                &Table,
                Config::parse(
                    "[factory]\nbackend = \"table\"\n[properties]\nall = \"camelCase\"\n",
                )
                .expect("config"),
            )
            .expect_err("should fail");

            assert!(error.message.contains("use text"), "{error:?}");
        }

        /// Rule 5 compares the **canonical** name. Compared against what was
        /// written, any rename slipped past it and the property was emitted
        /// twice in one table — Luau takes the last, so the tags won by accident
        /// rather than by rule. Both spellings are checked because the written
        /// form had the bug too; the shorthand only made it easy to hit.
        #[test]
        fn a_renamed_text_property_does_not_emit_twice() {
            for source in [
                "local e = <TextLabel text={props.x}>Body</TextLabel>",
                "local e = <TextLabel ={props.text}>Body</TextLabel>",
            ] {
                let compiled = build_with(source, "[properties]\nall = \"camelCase\"\n");

                assert_eq!(
                    compiled, "local e = create(\"TextLabel\")({ Text = \"Body\" })",
                    "{source}"
                );
            }
        }

        /// A shorthand is as tall as it was written, like everything else.
        #[test]
        fn output_preserves_line_count() {
            let fixture =
                "local e = (\n  <Frame\n    ={props.Size}\n    ={props.Visible}\n  />\n)\n";
            let compiled = build(fixture);

            assert_eq!(
                compiled.lines().count(),
                fixture.lines().count(),
                "--- out ---\n{compiled}"
            );
        }
    }
}
