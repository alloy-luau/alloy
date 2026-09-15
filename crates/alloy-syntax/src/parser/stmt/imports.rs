//! `import` and the `export { ... }` list.

use crate::lexer::TokKind;

use super::super::*;

impl<'a> Parser<'a> {
    // --- modules -----------------------------------------------------------

    /// `import` is a keyword before `*`, `{`, `type {`, or `Name from`.
    pub(super) fn import_follows(&self) -> bool {
        match self.text_at(1) {
            "*" | "{" => true,

            "type" => self.text_at(2) == "{",

            // `import M from` and `import M, { a } from`.
            _ => {
                self.name_at(1)
                    && (self.text_at(2) == "from"
                        || (self.text_at(2) == "," && self.text_at(3) == "{"))
            }
        }
    }

    pub(super) fn import_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump();

        let kind = if self.eat("*") {
            self.expect("as")?;
            let module = self.expect_name()?;

            // `import * as M, { a } from`: the whole module and names
            // from it, the shape `import M, { a }` already reads.
            if self.at(",") && self.text_at(1) == "{" {
                self.bump();

                ImportKind::Namespace(module, self.import_specs()?)
            } else {
                ImportKind::Namespace(module, Vec::new())
            }
        } else if self.at("type") && self.text_at(1) == "{" {
            self.bump();
            let mut specs = self.import_specs()?;

            for s in &mut specs {
                s.is_type = true;
            }

            ImportKind::TypeOnly(specs)
        } else if self.at("{") {
            let specs = self.import_specs()?;

            // `import { a }, * as M`: the module-wide name comes first,
            // so one written order holds for both forms.
            if self.at(",") {
                return Err(self.err(
                    "the name for the whole module comes first; write `import * as M, { ... } from`",
                ));
            }

            ImportKind::Named(specs)
        } else {
            let module = self.expect_name()?;

            // `import M, { a } from`: the module and names from it.
            if self.at(",") && self.text_at(1) == "{" {
                self.bump();

                ImportKind::Both(module, self.import_specs()?)
            } else {
                ImportKind::Default(module)
            }
        };

        self.expect("from")?;
        let path = self.string_token()?;

        Ok(Stmt::Import(Import {
            kind,
            path,
            span: TokSpan::new(start, self.pos),
        }))
    }

    fn import_specs(&mut self) -> Result<Vec<ImportSpec>, ParseError> {
        self.expect("{")?;
        let mut specs = Vec::new();

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated import list"));
            }

            let is_type = self.at("type") && self.name_at(1);

            if is_type {
                self.bump();
            }

            // `import { @tagged }`: an attribute reads with the `@`
            // it is applied with. The sigil sits on the exported name;
            // the local name after `as` is written plain.
            let is_attribute = self.eat("@");
            let start = self.pos;
            let mut name = self.expect_name()?;

            // `export type { Geom.Point as Position }`: a member of a
            // namespace reads by its path, so the name spans the dots.
            while self.at(".") && self.name_at(1) {
                self.bump();
                self.bump();
                name = TokSpan::new(start, self.pos);
            }

            let alias = if self.eat("as") {
                if self.at("@") {
                    let written = self.span_text(name);

                    return Err(self.err(&format!(
                        "an alias in an import list takes no `@`; write `@{written} as {}`",
                        self.text_at(1)
                    )));
                }

                Some(self.expect_name()?)
            } else {
                None
            };
            specs.push(ImportSpec {
                name,
                alias,
                is_type,
                is_attribute,
            });

            if !self.eat(",") {
                break;
            }
        }

        self.expect("}")?;

        Ok(specs)
    }

    fn string_token(&mut self) -> Result<TokSpan, ParseError> {
        if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
            let i = self.bump();

            return Ok(TokSpan::new(i, i + 1));
        }

        Err(self.err(&format!(
            "expected a module path string, found {}",
            self.found()
        )))
    }

    pub(super) fn export_list(
        &mut self,
        start: usize,
        type_only: bool,
    ) -> Result<Stmt, ParseError> {
        self.bump();
        let specs = self.import_specs()?;
        let from = if self.eat("from") {
            Some(self.string_token()?)
        } else {
            None
        };

        Ok(Stmt::ExportList(ExportList {
            specs,
            from,
            type_only,
            span: TokSpan::new(start, self.pos),
        }))
    }
}
