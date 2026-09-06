//! A small ingot that exercises every hook, for the host's tests and as
//! a worked example. `$shout("x")` in Alloy source becomes
//! `string.upper("x")`; the lint `loud_comment` flags a comment in
//! capitals and offers to lower it; hover on `$shout` explains it;
//! completion offers `$shout` after `$`; a code action wraps the word
//! under the cursor in `$shout( )`; the output hook stamps a header
//! comment onto the first line; the format hook trims trailing spaces.

use alloy_ingot::{
    CodeAction, CompletionItem, DiagnosticRef, Edit, File, Finding, Handler, Hover, ItemKind,
    Settings, serve,
};

#[derive(Default)]
struct Shout {
    /// `[ingot.shout] word`, the intrinsic's name; `shout` by default.
    word: String,
    lint_on: bool,
}

impl Shout {
    fn sigil(&self) -> String {
        format!("${}", self.word)
    }
}

impl Handler for Shout {
    fn init(&mut self, settings: &Settings) -> Result<(), String> {
        self.word = settings.options["word"]
            .as_str()
            .unwrap_or("shout")
            .to_string();
        self.lint_on = settings
            .lints
            .get("loud_comment")
            .is_some_and(|l| l != "allow");

        Ok(())
    }

    fn transform(&mut self, file: &File) -> Result<Vec<Edit>, String> {
        let sigil = self.sigil();

        Ok(file
            .source
            .match_indices(&sigil)
            .map(|(at, _)| Edit::replace(at as u32, (at + sigil.len()) as u32, "string.upper"))
            .collect())
    }

    fn output(&mut self, file: &File) -> Result<Vec<Edit>, String> {
        let first = file.source.find('\n').unwrap_or(file.source.len()) as u32;

        Ok(vec![Edit::insert(first, " -- shouted")])
    }

    fn lint(&mut self, file: &File) -> Result<Vec<Finding>, String> {
        if !self.lint_on {
            return Ok(Vec::new());
        }

        let mut findings = Vec::new();

        for (at, _) in file.source.match_indices("--") {
            let (_, end) = file.line_span(at as u32);
            let text = &file.source[at + 2..end as usize];
            let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();

            if letters.len() >= 3 && letters.iter().all(|c| c.is_uppercase()) {
                findings.push(
                    Finding::new("loud_comment", (at as u32, end), "a comment in capitals")
                        .with_fix(Edit::replace(at as u32 + 2, end, text.to_lowercase())),
                );
            }
        }

        Ok(findings)
    }

    fn format(&mut self, file: &File) -> Result<Vec<Edit>, String> {
        let mut edits = Vec::new();
        let mut at = 0usize;

        for line in file.source.split_inclusive('\n') {
            let body = line.trim_end_matches('\n');
            let trimmed = body.trim_end_matches(' ');

            if trimmed.len() != body.len() {
                edits.push(Edit::remove(
                    (at + trimmed.len()) as u32,
                    (at + body.len()) as u32,
                ));
            }

            at += line.len();
        }

        Ok(edits)
    }

    fn hover(&mut self, file: &File, offset: u32) -> Result<Option<Hover>, String> {
        let sigil = self.sigil();
        let (start, end) = file.line_span(offset);
        let line = &file.source[start as usize..end as usize];

        for (i, _) in line.match_indices(&sigil) {
            let s = start + i as u32;
            let e = s + sigil.len() as u32;

            if offset >= s && offset <= e {
                return Ok(Some(
                    Hover::new(format!(
                        "```alloy\n{sigil}(s: string): string\n```\nThe shout ingot: `{sigil}(x)` compiles to `string.upper(x)`."
                    ))
                    .over((s, e)),
                ));
            }
        }

        Ok(None)
    }

    fn complete(
        &mut self,
        file: &File,
        offset: u32,
        _trigger: Option<&str>,
    ) -> Result<Vec<CompletionItem>, String> {
        let before = &file.source[..(offset as usize).min(file.source.len())];

        if !before.ends_with('$') {
            return Ok(Vec::new());
        }

        Ok(vec![
            CompletionItem::new(self.sigil())
                .kind(ItemKind::Function)
                .detail("shout ingot")
                .documentation("Upper-cases a string at run time.")
                .snippet(format!("{}(${{1:s}})", self.sigil()))
                .over((offset - 1, offset)),
        ])
    }

    fn actions(
        &mut self,
        file: &File,
        span: (u32, u32),
        _diagnostics: &[DiagnosticRef],
    ) -> Result<Vec<CodeAction>, String> {
        let Some((word, (s, e))) = file.word_at(span.0) else {
            return Ok(Vec::new());
        };

        Ok(vec![
            CodeAction::new(
                format!("Shout `{word}`"),
                vec![Edit::replace(s, e, format!("{}({word})", self.sigil()))],
            )
            .kind("refactor.rewrite"),
        ])
    }

    fn manifest(&self) -> Option<&'static str> {
        Some(include_str!("shout/ingot.toml"))
    }
}

fn main() {
    serve(Shout::default())
}
