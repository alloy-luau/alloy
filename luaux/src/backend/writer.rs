//! Line-preserving output buffer.
//!
//! Maintains one invariant: **the number of newlines written so far equals
//! `line - start_line`.** Keeping that true is what makes the generated `.luau`
//! line up with the `.luaux` it came from (PLAN.md §5.5).
//!
//! Two things can disturb it, and each is handled:
//!
//! * Verbatim source — a captured expression spanning lines carries its own
//!   newlines, so [`Writer::push`] counts them.
//! * Collapsed source — a multi-line text run becomes one line, leaving the
//!   writer *behind* its source position. [`Writer::to`] pads the difference
//!   before the next entry, and the final `to` at the closing tag makes the
//!   totals match.

use super::EmitContext;

pub struct Writer<'a> {
    context: &'a EmitContext<'a>,
    out: String,
    /// Source line the output's current position corresponds to.
    line: usize,
}

impl<'a> Writer<'a> {
    pub fn new(context: &'a EmitContext<'a>, start: usize) -> Self {
        Self {
            context,
            out: String::new(),
            line: context.line_of(start),
        }
    }

    pub fn finish(self) -> String {
        self.out
    }

    /// Writes text, accounting for any newlines it carries.
    pub fn push(&mut self, text: &str) {
        self.line += text.matches('\n').count();
        self.out.push_str(text);
    }

    /// Moves to the line containing `offset`, padding with newlines and the
    /// source's own indentation. Returns whether a line break was written.
    ///
    /// Never moves backwards: if the output has already reached or passed that
    /// line, the caller separates with a space instead.
    pub fn to(&mut self, offset: usize) -> bool {
        let target = self.context.line_of(offset);

        if target <= self.line {
            return false;
        }

        while self.line < target {
            self.out.push('\n');
            self.line += 1;
        }

        self.out.push_str(self.context.indent_of(target));
        true
    }

    /// Separator before an entry: a line break if the entry starts on a later
    /// line, otherwise a single space.
    pub fn break_or_space(&mut self, offset: usize) {
        if !self.to(offset) {
            self.out.push(' ');
        }
    }

    /// Whether [`Writer::to`] would break for `offset`.
    ///
    /// Needed so a trailing comma can be written *before* the newline that
    /// precedes a closing brace.
    pub fn will_break(&self, offset: usize) -> bool {
        self.context.line_of(offset) > self.line
    }
}
