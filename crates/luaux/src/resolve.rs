//! Deciding whether a tag is a Roblox class or a user component (PLAN.md §3.1).
//!
//! Roblox class names are PascalCase and so are component names, so JSX's
//! `<div>` vs `<Button>` case rule cannot be reused. The resolution order is:
//!
//! 1. In the Roblox class list → **intrinsic**.
//! 2. Bound somewhere in the file → **component**.
//! 3. Neither → **error**, with a did-you-mean against the class list.
//! 4. Dotted (`<Foo.Bar/>`) → always a component.
//!
//! Step 2 is what stops `<Frmae/>` compiling into a call to an undefined global
//! that only fails at runtime.

use crate::backend::EmitError;
use crate::config::Config;
use crate::markup::ElementName;
use crate::roblox;
use full_moon::ast;
use full_moon::visitors::Visitor;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Emit the class name as a string.
    Intrinsic(String),
    /// Emit the name as written, as a function call.
    Component,
    /// Neither, and the error has already been recorded.
    ///
    /// Codegen carries on with the name as written so the rest of the file
    /// still compiles — one bad tag should cost its own diagnostic, not the
    /// whole file's type checking (PLAN.md §11.6). Attributes on such an
    /// element are **not** resolved: without a class there is nothing to check
    /// them against, and every one would report a second error caused entirely
    /// by the first.
    Unresolved(String),
}

pub struct Resolver {
    bound: HashSet<String>,
    config: Config,
}

impl Resolver {
    /// Collects every name bound in `source`.
    ///
    /// LuauX regions must already be blanked — see [`blank_luaux_regions`] — because
    /// `.luaux` is not parseable as Luau.
    pub fn new(blanked_source: &str, config: Config) -> Self {
        let mut collector = Bindings {
            names: HashSet::new(),
        };

        // parse_fallible so a file that is invalid for unrelated reasons still
        // yields whatever bindings it can. A missing binding degrades to a
        // "no such element" error, never to a silent miscompile.
        let parsed = full_moon::parse_fallible(blanked_source, full_moon::LuaVersion::luau());
        collector.visit_ast(parsed.ast());
        // Alloy patch: the caller's bindings join the parsed ones.
        collector.names.extend(config.extra_bound.iter().cloned());

        Self {
            bound: collector.names,
            config,
        }
    }

    /// Maps a written attribute name to the canonical Roblox property or event,
    /// applying `luaux.toml` aliases and rejecting names the class does not have.
    ///
    /// Only intrinsics reach here — a component's props are arbitrary, and a
    /// spread's keys are not known until runtime.
    pub fn resolve_attribute(
        &self,
        class: &str,
        written: &str,
        offset: usize,
    ) -> Result<String, EmitError> {
        let canonical = self
            .config
            .resolve_property(class, written)
            .map_err(|message| EmitError::new(message, offset, written.len()))?;

        if roblox::has_property(class, &canonical) || roblox::is_event(class, &canonical) {
            return Ok(canonical);
        }

        Err(EmitError::new(
            format!("{class} has no property or event named {written}"),
            offset,
            written.len(),
        )
        .maybe_help(suggestion(&roblox::closest_members(class, &canonical))))
    }

    /// Every name bound in the file, for checking the factory is reachable and
    /// whether a helper name is already taken.
    pub fn bound(&self) -> &HashSet<String> {
        &self.bound
    }

    /// Expression that constructs an element — `[factory] create`.
    pub fn create(&self) -> &str {
        &self.config.create
    }

    /// Table key an element's children go under — `[factory] children`.
    pub fn children(&self) -> Option<&str> {
        self.config.children.as_deref()
    }

    /// How an event name becomes a table key — `[factory] event`.
    pub fn event(&self) -> Option<&crate::config::EventKey> {
        self.config.event.as_ref()
    }

    /// Wrapper for interpolated text — `[factory] compute`.
    pub fn compute(&self) -> Option<&str> {
        self.config.compute.as_deref()
    }

    /// The reader's name inside `compute`'s callback — `[factory] use`.
    pub fn use_fn(&self) -> Option<&str> {
        self.config.use_fn.as_deref()
    }

    /// The component a fragment is constructed with — `[factory] fragment`.
    pub fn fragment(&self) -> Option<&str> {
        self.config.fragment.as_deref()
    }

    /// How interpolated text is encoded — `[factory] interpolate`.
    pub fn interpolate(&self) -> crate::config::Interpolate {
        self.config.interpolate
    }

    /// How spread groups combine — `[factory] merge`.
    pub fn merge(&self) -> Option<&str> {
        self.config.merge.as_deref()
    }

    pub fn resolve(&self, name: &ElementName, offset: usize) -> Result<Resolution, EmitError> {
        let simple = match name {
            // Dotted names are always components; a Roblox class name never has
            // a dot in it.
            ElementName::Member(_) => return Ok(Resolution::Component),
            ElementName::Simple(simple) => simple,
        };

        // Project aliases win: an explicit rename is a stronger signal than
        // either the class list or a same-named binding.
        match self.config.resolve_element(simple) {
            Ok(Some(class)) => return Ok(Resolution::Intrinsic(class.to_string())),
            Ok(None) => {}
            Err(message) => return Err(EmitError::new(message, offset, simple.len() + 1)),
        }

        if roblox::is_class(simple) {
            return Ok(Resolution::Intrinsic(simple.clone()));
        }

        if self.bound.contains(simple) {
            return Ok(Resolution::Component);
        }

        let help = match roblox::closest_class(simple) {
            Some(class) => format!("did you mean <{class}>?"),
            None => String::from("if it is a component, it has to be in scope"),
        };

        // `+ 1` covers the `<` so the underline starts at the angle bracket.
        Err(EmitError::new(
            format!("<{simple}> is not a Roblox class and is not defined"),
            offset,
            simple.len() + 1,
        )
        .with_help(help))
    }
}

/// Formats up to a few candidates as a single suggestion line.
fn suggestion(candidates: &[&'static str]) -> Option<String> {
    match candidates {
        [] => None,
        [one] => Some(format!("did you mean {one}?")),
        [rest @ .., last] => Some(format!("did you mean {} or {last}?", rest.join(", "))),
    }
}

/// Replaces every LuauX region with same-length filler so the result parses as
/// Luau while keeping byte offsets and line numbers intact.
///
/// `nil` stands in for the expression; the remaining bytes become spaces, except
/// newlines, which are kept so line numbers still line up.
pub fn blank_luaux_regions(source: &str, spans: &[(usize, usize)]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;

    for (start, end) in spans.iter().copied() {
        if start < cursor {
            continue;
        }

        out.push_str(&source[cursor..start]);

        let region = &source[start..end];
        let mut filler = String::with_capacity(region.len());

        for (index, character) in region.char_indices() {
            if character == '\n' {
                filler.push('\n');
            } else if index < 3 {
                filler.push(['n', 'i', 'l'][index]);
            } else {
                // Pad by byte length so offsets past the region are unchanged.
                for _ in 0..character.len_utf8() {
                    filler.push(' ');
                }
            }
        }

        // A region shorter than `nil` is impossible: `<a/>` is already 4 bytes.
        out.push_str(&filler);
        cursor = end;
    }

    out.push_str(&source[cursor..]);
    out
}

struct Bindings {
    names: HashSet<String>,
}

impl Bindings {
    fn insert(&mut self, token: &full_moon::tokenizer::TokenReference) {
        self.names.insert(token.token().to_string());
    }
}

impl Visitor for Bindings {
    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        for name in node.names() {
            self.insert(name);
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        self.insert(node.name());
    }

    // `const` binds exactly as `local` does, and missing it is not a quiet
    // degradation: the factory check reports `vide` is not in scope for a file
    // that imported it, and every component declared that way stops resolving,
    // so `<Card/>` is told it is not a Roblox class.
    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        for name in node.names() {
            self.insert(name);
        }
    }

    fn visit_const_function(&mut self, node: &ast::luau::ConstFunction) {
        self.insert(node.name());
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        // `function Receipt()` binds a global; `function a.b.c()` binds nothing
        // new, but recording the head is harmless.
        if let Some(first) = node.name().names().iter().next() {
            self.insert(first);
        }
    }

    fn visit_function_body(&mut self, node: &ast::FunctionBody) {
        for parameter in node.parameters() {
            if let ast::Parameter::Name(name) = parameter {
                self.insert(name);
            }
        }
    }

    fn visit_numeric_for(&mut self, node: &ast::NumericFor) {
        self.insert(node.index_variable());
    }

    fn visit_generic_for(&mut self, node: &ast::GenericFor) {
        for name in node.names() {
            self.insert(name);
        }
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        // Plain `Receipt = function() ... end` binds a global.
        for variable in node.variables() {
            if let ast::Var::Name(name) = variable {
                self.insert(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(source: &str) -> Resolver {
        Resolver::new(source, Config::default())
    }

    fn simple(name: &str) -> ElementName {
        ElementName::Simple(name.to_string())
    }

    #[test]
    fn classes_resolve_to_intrinsics() {
        let resolver = resolver("");
        assert_eq!(
            resolver.resolve(&simple("Frame"), 0),
            Ok(Resolution::Intrinsic("Frame".into()))
        );
        assert_eq!(
            resolver.resolve(&simple("UICorner"), 0),
            Ok(Resolution::Intrinsic("UICorner".into()))
        );
    }

    #[test]
    fn bound_names_resolve_to_components() {
        for source in [
            "local Receipt = require('./Receipt')",
            "local function Receipt() end",
            "function Receipt() end",
            "Receipt = function() end",
            "local Receipt",
            "const Receipt = require('./Receipt')",
            "const function Receipt() end",
        ] {
            assert_eq!(
                resolver(source).resolve(&simple("Receipt"), 0),
                Ok(Resolution::Component),
                "source: {source}"
            );
        }
    }

    #[test]
    fn parameters_and_loop_variables_count_as_bindings() {
        assert_eq!(
            resolver("local function f(Row) end").resolve(&simple("Row"), 0),
            Ok(Resolution::Component)
        );
        assert_eq!(
            resolver("for _, Row in items do end").resolve(&simple("Row"), 0),
            Ok(Resolution::Component)
        );
    }

    #[test]
    fn member_names_are_always_components() {
        assert_eq!(
            resolver("").resolve(&ElementName::Member(vec!["Foo".into(), "Bar".into()]), 0),
            Ok(Resolution::Component)
        );
    }

    #[test]
    fn unknown_names_are_rejected_with_a_suggestion() {
        let error = resolver("")
            .resolve(&simple("TextLabl"), 7)
            .expect_err("should fail");
        assert_eq!(error.help.as_deref(), Some("did you mean <TextLabel>?"));
        assert_eq!(error.offset, 7);
        // The underline covers `<TextLabl`.
        assert_eq!(error.length, "TextLabl".len() + 1);
    }

    #[test]
    fn unknown_names_with_no_near_miss_say_so() {
        let error = resolver("")
            .resolve(&simple("Receipt"), 0)
            .expect_err("should fail");
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains("has to be in scope")),
            "{:?}",
            error.help
        );
    }

    #[test]
    fn blanking_preserves_offsets_and_lines() {
        let source = "local a = <Frame>\n  <TextLabel/>\n</Frame>\nlocal b = 2";
        let start = source.find('<').expect("markup");
        let end = source.find("\nlocal b").expect("end");

        let blanked = blank_luaux_regions(source, &[(start, end)]);

        assert_eq!(blanked.len(), source.len());
        assert_eq!(blanked.lines().count(), source.lines().count());
        assert!(blanked.starts_with("local a = nil"));
        assert!(blanked.ends_with("local b = 2"));

        // And the result is parseable, which is the whole point.
        assert!(full_moon::parse(&blanked).is_ok(), "{blanked}");
    }
}
