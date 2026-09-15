//! Code generation backends.
//!
//! A backend owns one thing: the **arrangement** of the constructor call — how
//! many arguments it takes and where children sit among them. Everything that
//! varies *inside* a props table is a `[factory]` variable instead, and the two
//! seams are not interchangeable (backend-plan.md §2):
//!
//! > A factory variable changes what goes where inside one props table passed to
//! > a curried constructor. Anything that changes the arity or arrangement of the
//! > constructor call needs a backend.
//!
//! Two arrangements cover the Roblox UI libraries:
//!
//! * [`Table`] — `F(class)(props)`, children in the props table. Vide, Fusion.
//! * [`Element`] — `F(class, props, children)`, children positional. React.
//!
//! The seam exists from the start deliberately (PLAN.md §5.6), and the
//! raw-`Instance.new` backend in DEFER.md still cannot plug into it: it is
//! statement-oriented, and [`Backend::emit`] requires an expression.
//!
//! Roughly 75–80% of the compiler — lexer, LuauX parser, alias resolution,
//! validation, text rules — is target-independent and sits above this trait.

pub mod common;
pub mod context;
pub mod element;
pub mod table;
pub mod writer;

use crate::markup::Node;
use std::fmt;

pub use context::{EmitContext, Helpers};
pub use element::Element;
pub use table::Table;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    pub message: String,
    /// Byte offset into the original source.
    pub offset: usize,
    /// Length of the offending text, for underlining. Zero means "point here".
    pub length: usize,
    /// Suggestion shown separately from the message.
    pub help: Option<String>,
}

impl EmitError {
    pub fn new(message: impl Into<String>, offset: usize, length: usize) -> Self {
        Self {
            message: message.into(),
            offset,
            length,
            help: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn maybe_help(mut self, help: Option<String>) -> Self {
        self.help = help;
        self
    }
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for EmitError {}

pub trait Backend {
    fn name(&self) -> &'static str;

    /// Lowers one LuauX node to a Luau **expression**.
    ///
    /// It must be an expression, not a statement sequence, so that LuauX composes
    /// in every position it can appear — including short-circuit operands and
    /// call arguments, where statement hoisting would be illegal.
    ///
    /// The returned text replaces the node's source span exactly, and must
    /// contain the same number of newlines as the span it replaces so that line
    /// numbers survive (PLAN.md §5.5).
    fn emit(&self, node: &Node, context: &EmitContext<'_>) -> Result<String, EmitError>;
}
