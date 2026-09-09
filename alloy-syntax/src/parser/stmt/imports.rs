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

            ImportKind::Namespace(self.expect_name()?)
        } else if self.at("type") && self.text_at(1) == "{" {
            self.bump();
            let mut specs = self.import_specs()?;

            for s in &mut specs {
                s.is_type = true;
            }

            ImportKind::TypeOnly(specs)
        } else if self.at("{") {
            ImportKind::Named(self.import_specs()?)
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

            let name = self.expect_name()?;
            let alias = if self.eat("as") {
                Some(self.expect_name()?)
            } else {
                None
            };
            specs.push(ImportSpec {
                name,
                alias,
                is_type,
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
