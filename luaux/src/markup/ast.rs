//! LuauX syntax tree.
//!
//! Luau expressions are held as **raw source slices**, never parsed. LuauX lowers
//! to an ordinary Luau expression and everything else passes through unchanged,
//! so the compiler never needs to understand what is inside `{...}` — only where
//! it ends (PLAN.md §5.1).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Fragment(Fragment),
}

impl Node {
    pub fn span(&self) -> Span {
        match self {
            Node::Element(element) => element.span,
            Node::Fragment(fragment) => fragment.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: ElementName,
    pub attributes: Vec<Attribute>,
    pub children: Vec<Child>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub children: Vec<Child>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementName {
    /// `<Frame>` — resolved to an intrinsic or a component later.
    Simple(String),
    /// `<Foo.Bar>` — always a component.
    Member(Vec<String>),
}

impl ElementName {
    pub fn as_written(&self) -> String {
        match self {
            ElementName::Simple(name) => name.clone(),
            ElementName::Member(parts) => parts.join("."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribute {
    Named {
        name: String,
        value: AttributeValue,
        span: Span,
    },
    /// `{props}` in attribute position.
    Spread { expression: String, span: Span },
    /// `={props.Text}` — the property is inferred from the expression.
    ///
    /// Held unresolved rather than turned into a [`Attribute::Named`] at parse
    /// time, because an expression that names nothing is a mistake in one
    /// attribute. Parse errors stop the file; attribute errors are recovered
    /// from, and this belongs with the latter.
    Inferred { expression: String, span: Span },
}

/// The property an `={expr}` shorthand names.
///
/// The expression has to *be* a name: an identifier, or a dotted path of them,
/// whose last segment is the property. `={Text}` and `={props.Text}` both name
/// `Text`.
///
/// Deliberately strict. `={getProps().Text}` ends in a name too, and accepting
/// it would make the rule "whatever follows the last dot" — a rule about
/// punctuation rather than about names, and one nobody could apply without
/// trying it first. A shorthand that works exactly where you predict it will is
/// worth more than one that usually works.
///
/// The name is returned as written, so `luaux.toml` aliases and casing apply to
/// it exactly as they would to a name typed out in full.
pub fn infer_name(expression: &str) -> Option<&str> {
    // These lex as identifiers and are values, not names. Left to the property
    // check they would come back as "Frame has no property named nil", which
    // describes the symptom rather than the mistake.
    const VALUES: &[&str] = &["nil", "true", "false"];

    let mut last = None;

    for segment in expression.split('.') {
        let segment = segment.trim();
        let mut characters = segment.chars();

        let named = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && characters.all(|character| character.is_ascii_alphanumeric() || character == '_');

        if !named || VALUES.contains(&segment) {
            return None;
        }

        last = Some(segment);
    }

    last
}

#[cfg(test)]
mod infer_tests {
    use super::infer_name;

    #[test]
    fn takes_the_last_segment_of_a_name() {
        assert_eq!(infer_name("Text"), Some("Text"));
        assert_eq!(infer_name("props.Text"), Some("Text"));
        assert_eq!(
            infer_name("self.props.BackgroundColor3"),
            Some("BackgroundColor3")
        );
        assert_eq!(infer_name("_private"), Some("_private"));
    }

    /// Whitespace inside the hole is the author's, and `{ props.Text }` means
    /// what it looks like.
    #[test]
    fn ignores_surrounding_whitespace() {
        assert_eq!(infer_name(" props.Text "), Some("Text"));
        assert_eq!(infer_name("props . Text"), Some("Text"));
    }

    /// Anything that is not a name has no name to take.
    #[test]
    fn refuses_what_is_not_a_name() {
        for expression in [
            "getProps().Text",
            "props[1]",
            "props:Text()",
            "a + b",
            "\"Text\"",
            "props.Text or other",
            "",
            "   ",
            "1",
            "props.",
            ".Text",
        ] {
            assert_eq!(infer_name(expression), None, "{expression}");
        }
    }

    /// A value is not a name, even though it lexes like one.
    #[test]
    fn refuses_a_literal_that_lexes_as_a_name() {
        assert_eq!(infer_name("nil"), None);
        assert_eq!(infer_name("true"), None);
        assert_eq!(infer_name("false"), None);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeValue {
    /// `Size={expr}` — raw Luau, verbatim.
    Expression(String),
    /// `Name="literal"` — the raw literal *including* its quotes.
    ///
    /// Attribute strings are ordinary Luau strings, so they are captured and
    /// re-emitted byte for byte. Decoding luaux's `\{` escapes here would
    /// corrupt Luau escapes like `\n`; those escapes apply to text *children*,
    /// which are a different lexical mode.
    StringLiteral(String),
    /// Shorthand `Visible`, meaning `true`.
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Child {
    Node(Node),
    /// `{expr}` — raw Luau, verbatim.
    Expression {
        expression: String,
        span: Span,
    },
    /// Literal text with escapes already decoded and whitespace normalised.
    Text {
        text: String,
        span: Span,
    },
    /// A comment, stored as **Luau source ready to emit**.
    ///
    /// `<!-- … -->` is wrapped into a block comment here rather than at emit
    /// time; a hole like `{--[[ … ]]}` is already Luau and is kept verbatim.
    /// Wrapping twice would produce `--[[ --[[ … ]] ]]`, and Lua block comments
    /// do not nest — the inner `]]` closes the outer one.
    Comment {
        luau: String,
        span: Span,
    },
}

impl Child {
    /// Where this child began in the source. Codegen positions each emitted
    /// entry on its original line (PLAN.md §5.5).
    pub fn span(&self) -> Span {
        match self {
            Child::Node(node) => node.span(),
            Child::Expression { span, .. }
            | Child::Text { span, .. }
            | Child::Comment { span, .. } => *span,
        }
    }
}
