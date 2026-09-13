//! `namespace Name as members end`.

use super::super::*;

impl<'a> Parser<'a> {
    /// `namespace Name as ... end`, from the token at `start`. The
    /// caller has already eaten `export` or `global`, so the span
    /// covers the modifier and the emit replaces the whole statement.
    pub(super) fn namespace_decl(
        &mut self,
        start: usize,
        attributes: Vec<Attr>,
        exported: bool,
    ) -> Result<Stmt, ParseError> {
        let open = self.pos;
        self.expect("namespace")?;
        let name = self.expect_name()?;
        self.expect("as")?;
        let members = self.namespace_members()?;
        self.expect_end(open)?;

        Ok(Stmt::Namespace(NamespaceDecl {
            attributes,
            exported,
            name,
            members,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// The declarations of a namespace body, up to `end`.
    fn namespace_members(&mut self) -> Result<Vec<NamespaceMember>, ParseError> {
        let mut members = Vec::new();

        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("unterminated namespace, expected `end`"));
            }

            let m_start = self.pos;
            // The attributes come first, so `@cfg(server) private
            // function f()` reads the way the field of a struct does.
            let attrs = if self.at("@") {
                self.attrs()?
            } else {
                Vec::new()
            };
            let visibility = self.member_visibility();
            let before = self.pos;
            let stmt = match attrs.is_empty() {
                true => self.stmt()?,

                false => self.attributed_stmt(m_start, attrs)?,
            };

            // A statement that reads no token would spin the loop.
            if self.pos == before && self.pos == m_start {
                return Err(self.err("expected a declaration or `end`"));
            }

            members.push(NamespaceMember {
                visibility,
                stmt,
                span: TokSpan::new(m_start, self.pos),
            });

            let _ = self.eat(";");
        }

        Ok(members)
    }

    /// `private` or `public` before a member's declaration. The word is
    /// a name everywhere else, so a declaration has to follow it.
    fn member_visibility(&mut self) -> Option<TokSpan> {
        if !matches!(self.text(), "private" | "public") || !self.member_decl_at(1) {
            return None;
        }

        let i = self.bump();

        Some(TokSpan::new(i, i + 1))
    }

    /// Whether the token `n` ahead opens a declaration a namespace
    /// takes as a member.
    fn member_decl_at(&self, n: usize) -> bool {
        matches!(
            self.text_at(n),
            "function"
                | "local"
                | "const"
                | "struct"
                | "enum"
                | "trait"
                | "interface"
                | "type"
                | "impl"
                | "namespace"
                | "remote"
                | "attribute"
                | "macro"
                | "class"
                | "export"
                | "@"
        ) || (self.text_at(n) == "async" && self.text_at(n + 1) == "function")
            || (self.text_at(n) == "open" && self.text_at(n + 1) == "class")
    }

    /// Whether `namespace` at the cursor opens a declaration. The word
    /// stays contextual, the way `export` and `global` do.
    pub(super) fn namespace_follows(&self) -> bool {
        self.name_at(1) && self.text_at(2) == "as"
    }
}
