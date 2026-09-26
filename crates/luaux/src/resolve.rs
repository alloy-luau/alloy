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
use alloy_syntax::lexer::{Tok, TokKind};
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
        Self {
            bound: bound_names(blanked_source),
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
        // React takes `ref` and `key` on every host element and hands
        // neither to the instance. A table factory declares no such
        // props, so there they stay unknown names.
        if self.config.backend == crate::config::BackendKind::Element
            && matches!(written, "ref" | "key")
        {
            return Ok(written.to_string());
        }

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

/// The names a file binds, by a token scan of the blanked source. The
/// scan reads Alloy syntax too: `import`, `const`, `struct`, and the
/// rest. A name that is not a binding but looks like one costs nothing:
/// it only lets `<Name>` resolve to a component.
pub fn bound_names(src: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(lexed) = alloy_syntax::lexer::lex(src) else {
        return names;
    };
    let toks = &lexed.toks;
    let text = |t: &Tok| t.text(src);
    let is_ident = |t: &Tok| t.kind == TokKind::Ident;
    let mut i = 0;

    while i < toks.len() {
        let word = text(&toks[i]);

        match word {
            "local" | "const" => {
                i += 1;

                if i < toks.len() && text(&toks[i]) == "function" {
                    if let Some(t) = toks.get(i + 1).filter(|t| is_ident(t)) {
                        names.insert(text(t).to_string());
                    }

                    continue;
                }

                // `local a, b`, `local { a, b = c }`, `local [ x, ...rest ]`.
                let mut depth = 0i32;

                while i < toks.len() {
                    let t = &toks[i];
                    let s = text(t);

                    match s {
                        "{" | "[" => depth += 1,

                        "}" | "]" => depth -= 1,

                        "=" if depth == 0 => break,

                        ":" if depth == 0 => break,

                        _ if is_ident(t) => {
                            // In a table destructure `a = b` binds `b`; the
                            // name before `=` is a key. Keeping both is safe.
                            names.insert(s.to_string());
                        }

                        _ => {}
                    }

                    if depth == 0
                        && s != ","
                        && !is_ident(t)
                        && !matches!(s, "{" | "[" | "}" | "]" | "...")
                    {
                        break;
                    }

                    i += 1;
                }

                continue;
            }

            "function" => {
                if let Some(t) = toks.get(i + 1).filter(|t| is_ident(t)) {
                    names.insert(text(t).to_string());
                }

                // The parameters: `function f<T>(a, b: T, ...)`. A name
                // right after `(` or `,` is one; a type or a default is not.
                let mut j = i + 1;

                while toks
                    .get(j)
                    .is_some_and(|t| is_ident(t) || matches!(text(t), "." | ":" | "<" | ">" | ","))
                {
                    j += 1;
                }

                let mut depth = 0i32;

                while toks.get(j).is_some_and(|t| text(t) == "(") || depth > 0 {
                    let Some(t) = toks.get(j) else {
                        break;
                    };

                    match text(t) {
                        "(" | "{" | "[" => depth += 1,

                        ")" | "}" | "]" => depth -= 1,

                        _ if depth == 1
                            && is_ident(t)
                            && matches!(text(&toks[j - 1]), "(" | ",") =>
                        {
                            names.insert(text(t).to_string());
                        }

                        _ => {}
                    }

                    j += 1;
                }
            }

            // `for i = 1, n` and `for k, v: T in t`.
            "for" => {
                let mut j = i + 1;

                while let Some(t) = toks
                    .get(j)
                    .filter(|t| !matches!(text(t), "in" | "=" | "do"))
                {
                    if is_ident(t) && matches!(text(&toks[j - 1]), "for" | ",") {
                        names.insert(text(t).to_string());
                    }

                    j += 1;
                }
            }

            // `Receipt = function() ... end` at the start of a statement
            // binds a global. A key of a table on its own line reads the
            // same, and costs nothing.
            _ if is_ident(&toks[i])
                && toks.get(i + 1).is_some_and(|t| text(t) == "=")
                && (i == 0
                    || text(&toks[i - 1]) == ";"
                    || src[toks[i - 1].end as usize..toks[i].start as usize].contains('\n')) =>
            {
                names.insert(word.to_string());
            }

            // A namespace holds components: `<Scope.card/>` names one.
            "struct" | "enum" | "trait" | "interface" | "remote" | "attribute" | "macro"
            | "class" | "namespace" => {
                if let Some(t) = toks.get(i + 1).filter(|t| is_ident(t)) {
                    names.insert(text(t).to_string());
                }
            }

            "import" => {
                // `import * as N`, `import D from`, `import { a as b, c }`.
                let mut j = i + 1;

                while j < toks.len() {
                    let t = &toks[j];
                    let s = text(t);

                    if s == "from" || matches!(t.kind, TokKind::Str { .. }) {
                        break;
                    }

                    // An alias `a as b` binds `b`; keeping `a` too is
                    // harmless, since a name only lets a tag resolve.
                    if is_ident(t) && s != "type" && s != "as" {
                        names.insert(s.to_string());
                    }

                    j += 1;
                }
            }

            _ => {}
        }

        i += 1;
    }

    names
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

    /// A negation inside a table type is Alloy syntax, and the scan
    /// reads past it.
    #[test]
    fn a_negation_in_a_table_type_keeps_the_bindings() {
        let resolver = resolver("type T = { t: ~nil }\nlocal Card = 1\nprint(Card ~= 2)");

        assert!(resolver.bound().contains("Card"));
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
        assert!(alloy_syntax::parse_one(&blanked).is_ok(), "{blanked}");
    }
}
