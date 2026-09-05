//! Recursive-descent LuauX parser.
//!
//! Runs directly over the source rather than the Luau token stream, because LuauX
//! has lexical modes Luau does not: text between tags is raw (`don't` would open
//! a string), and `{...}` holes return to Luau expression mode. Expressions are
//! captured verbatim by brace matching and never parsed.

use super::ast::*;
use crate::lexer::find_matching_brace;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

/// Parses one LuauX node starting at `start`, which must be a `<`.
/// Returns the node and the offset just past it.
pub fn parse_node(src: &str, start: usize) -> Result<(Node, usize), ParseError> {
    let mut parser = Parser { src, pos: start };
    let node = parser.parse_node()?;
    Ok((node, parser.pos))
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn at(&self, prefix: &str) -> bool {
        self.rest().starts_with(prefix)
    }

    fn byte(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            message: message.into(),
            offset: self.pos,
        })
    }

    fn error_at<T>(&self, message: impl Into<String>, offset: usize) -> Result<T, ParseError> {
        Err(ParseError {
            message: message.into(),
            offset,
        })
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.byte(), Some(c) if c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, prefix: &str, what: &str) -> Result<(), ParseError> {
        if self.at(prefix) {
            self.pos += prefix.len();
            Ok(())
        } else {
            self.err(format!("expected {what}"))
        }
    }

    fn parse_node(&mut self) -> Result<Node, ParseError> {
        let start = self.pos;
        self.expect("<", "`<`")?;
        self.skip_whitespace();

        if self.at(">") {
            self.pos += 1;
            let children = self.parse_children()?;
            self.expect("</", "closing `</>` for fragment")?;
            self.skip_whitespace();
            self.expect(">", "`>` to close fragment")?;

            return Ok(Node::Fragment(Fragment {
                children,
                span: Span::new(start, self.pos),
            }));
        }

        let name = self.parse_element_name()?;
        let attributes = self.parse_attributes()?;

        if self.at("/>") {
            self.pos += 2;
            return Ok(Node::Element(Element {
                name,
                attributes,
                children: Vec::new(),
                span: Span::new(start, self.pos),
            }));
        }

        self.expect(">", "`>` or `/>`")?;
        let children = self.parse_children()?;

        let closing = self.pos;
        self.expect("</", "closing tag")?;
        self.skip_whitespace();
        let closing_name = self.parse_element_name()?;

        if closing_name != name {
            return self.error_at(
                format!(
                    "closing </{}> does not match opening <{}>",
                    closing_name.as_written(),
                    name.as_written()
                ),
                closing,
            );
        }

        self.skip_whitespace();
        self.expect(">", "`>` to close tag")?;

        Ok(Node::Element(Element {
            name,
            attributes,
            children,
            span: Span::new(start, self.pos),
        }))
    }

    fn parse_element_name(&mut self) -> Result<ElementName, ParseError> {
        let mut parts = vec![self.parse_identifier()?];

        while self.at(".") {
            self.pos += 1;
            parts.push(self.parse_identifier()?);
        }

        if parts.len() == 1 {
            Ok(ElementName::Simple(parts.pop().expect("one part")))
        } else {
            Ok(ElementName::Member(parts))
        }
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        let start = self.pos;

        if !matches!(self.byte(), Some(c) if c == b'_' || c.is_ascii_alphabetic()) {
            return self.err("expected an element or attribute name");
        }

        while matches!(self.byte(), Some(c) if c == b'_' || c.is_ascii_alphanumeric()) {
            self.pos += 1;
        }

        Ok(self.src[start..self.pos].to_string())
    }

    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attributes = Vec::new();

        loop {
            self.skip_whitespace();

            if self.at("/>") || self.at(">") || self.byte().is_none() {
                return Ok(attributes);
            }

            let start = self.pos;

            // `{props}` — spread.
            if self.at("{") {
                let expression = self.parse_braced_expression()?;
                attributes.push(Attribute::Spread {
                    expression,
                    span: Span::new(start, self.pos),
                });
                continue;
            }

            // `={props.Text}` — the name is the expression's last segment.
            //
            // Unambiguous in this position: an attribute is a name, a name with
            // a value, or a spread, and none of them can start with `=`. The
            // spelling is `=` rather than a bare `{Text}` because a bare hole
            // already means a spread here, and one syntax cannot mean two
            // things.
            if self.at("=") {
                self.pos += 1;
                self.skip_whitespace();

                if !self.at("{") {
                    return self.err("expected `{expression}` after `=`");
                }

                let expression = self.parse_braced_expression()?;
                attributes.push(Attribute::Inferred {
                    expression,
                    span: Span::new(start, self.pos),
                });
                continue;
            }

            let name = self.parse_identifier()?;
            let after_name = self.pos;
            self.skip_whitespace();
            let spaced = self.pos != after_name;

            let value = if self.at("=") {
                // `Visible ={props.Size}` has two readings: a value for
                // `Visible`, or `Visible` as a boolean shorthand beside an
                // inferred attribute. Refuse rather than pick — picking emitted
                // `Visible = props.Size`, which sets a property the author did
                // not name, leaves the one they did name unset, and resolves
                // cleanly because `Visible` is a real property. No diagnostic,
                // wrong UI.
                //
                // Only a hole is ambiguous. `Name = "a"` cannot be the inferred
                // form, which always takes one.
                if spaced && self.src[self.pos + 1..].trim_start().starts_with('{') {
                    return self.error_at(
                        format!(
                            "`{name} ={{…}}` could be a value for {name}, or {name} on its own \
                             beside an inferred attribute"
                        ),
                        after_name,
                    );
                }

                self.pos += 1;
                self.skip_whitespace();
                self.parse_attribute_value()?
            } else {
                // Shorthand `Visible`. Rewind so the span stops at the name
                // rather than swallowing the whitespace before the next
                // attribute; the loop re-skips it.
                self.pos = after_name;
                AttributeValue::Boolean
            };

            attributes.push(Attribute::Named {
                name,
                value,
                span: Span::new(start, self.pos),
            });
        }
    }

    fn parse_attribute_value(&mut self) -> Result<AttributeValue, ParseError> {
        match self.byte() {
            Some(b'{') => Ok(AttributeValue::Expression(self.parse_braced_expression()?)),
            Some(quote @ (b'"' | b'\'')) => {
                let start = self.pos;
                self.pos += 1;

                loop {
                    match self.byte() {
                        None | Some(b'\n') => {
                            return self.error_at("unterminated attribute string", start)
                        }
                        // Skip the escape without interpreting it; Luau will.
                        // Step over the backslash, then over one whole
                        // character, so an escaped multi-byte character does
                        // not leave the cursor mid-codepoint.
                        Some(b'\\') => {
                            self.pos += 1;
                            self.pos = self.next_char_end();
                        }
                        Some(c) if c == quote => {
                            self.pos += 1;
                            return Ok(AttributeValue::StringLiteral(
                                self.src[start..self.pos].to_string(),
                            ));
                        }
                        Some(_) => self.pos = self.next_char_end(),
                    }
                }
            }
            _ => self.err("expected `{expression}` or a quoted string after `=`"),
        }
    }

    /// Captures a `{ ... }` region verbatim, requiring it to hold a value.
    ///
    /// Attribute and spread positions need one; children do not, since a
    /// comment-only hole is a comment (see [`Parser::parse_hole`]).
    fn parse_braced_expression(&mut self) -> Result<String, ParseError> {
        let open = self.pos;

        match self.parse_hole()? {
            Hole::Value(expression) => Ok(expression),
            Hole::Comment => self.error_at("this `{}` contains no value", open),
        }
    }

    /// Captures a `{ ... }` region, distinguishing a value from a comment.
    fn parse_hole(&mut self) -> Result<Hole, ParseError> {
        let open = self.pos;

        let close = find_matching_brace(self.src, open).map_err(|error| ParseError {
            message: error.message,
            offset: error.offset,
        })?;

        let expression = self.src[open + 1..close].trim().to_string();
        self.pos = close + 1;

        // `{}` says nothing about intent and is almost always a deletion
        // accident, so it stays an error wherever it appears.
        if expression.is_empty() {
            return self.error_at("this `{}` is empty", open);
        }

        // A hole holding only comments *is* a comment — the same shape LuauX uses
        // for `{/* ... */}`. It cannot be emitted as a child: on its own it
        // would compile to a harmless empty table, but alongside siblings it
        // becomes `{ a, --[[ note ]], b }`, and Lua rejects a comma with no
        // field before it. So callers drop it rather than emit it.
        Ok(if holds_a_value(&expression) {
            Hole::Value(expression)
        } else {
            Hole::Comment
        })
    }

    fn parse_children(&mut self) -> Result<Vec<Child>, ParseError> {
        let mut children = Vec::new();

        loop {
            if self.byte().is_none() {
                return self.err("unclosed element");
            }

            if self.at("<!--") {
                let start = self.pos;
                let text = self.parse_markup_comment()?;
                children.push(Child::Comment {
                    luau: block_comment(&text),
                    span: Span::new(start, self.pos),
                });
                continue;
            }

            if self.at("</") {
                return Ok(children);
            }

            if self.at("<") {
                children.push(Child::Node(self.parse_node()?));
                continue;
            }

            if self.at("{") {
                let start = self.pos;

                // A comment-only hole is a comment, matching JSX's `{/* … */}`.
                match self.parse_hole()? {
                    Hole::Value(expression) => children.push(Child::Expression {
                        expression,
                        span: Span::new(start, self.pos),
                    }),
                    // Already Luau — emit it exactly as written.
                    Hole::Comment => children.push(Child::Comment {
                        luau: self.src[start + 1..self.pos - 1].trim().to_string(),
                        span: Span::new(start, self.pos),
                    }),
                }

                continue;
            }

            let start = self.pos;
            let text = self.parse_text();
            if !text.is_empty() {
                children.push(Child::Text {
                    text,
                    span: Span::new(start, self.pos),
                });
            }
        }
    }

    /// Consumes `<!-- … -->`, returning the text between the delimiters.
    fn parse_markup_comment(&mut self) -> Result<String, ParseError> {
        let start = self.pos;

        match self.rest().find("-->") {
            Some(at) => {
                // Raw, not trimmed: a comment written across several lines has
                // to keep its own line structure, and `block_comment` decides
                // how to present it.
                let text = self.src[self.pos + 4..self.pos + at].to_string();
                self.pos += at + 3;
                Ok(text)
            }
            None => self.error_at("unterminated `<!--` comment", start),
        }
    }

    /// Reads a text run up to the next `<` or `{`, decoding escapes and applying
    /// the whitespace rules.
    fn parse_text(&mut self) -> String {
        let mut raw = String::new();

        loop {
            match self.byte() {
                None | Some(b'<') | Some(b'{') => break,
                Some(b'\\') => {
                    self.pos += 1;
                    self.push_escaped(&mut raw);
                }
                Some(_) => {
                    let next = self.next_char_end();
                    raw.push_str(&self.src[self.pos..next]);
                    self.pos = next;
                }
            }
        }

        normalise_text(&raw)
    }

    /// Consumes the character after a backslash.
    ///
    /// `\{`, `\}`, `` \` `` and `\\` are luaux escapes and yield the bare
    /// character. Anything else keeps the backslash, so Luau escapes written in
    /// attribute strings (`\n`, `\t`) survive to the output untouched.
    fn push_escaped(&mut self, out: &mut String) {
        match self.byte() {
            None => out.push('\\'),
            Some(c @ (b'{' | b'}' | b'`' | b'\\')) => {
                out.push(c as char);
                self.pos += 1;
            }
            Some(_) => {
                out.push('\\');
                let next = self.next_char_end();
                out.push_str(&self.src[self.pos..next]);
                self.pos = next;
            }
        }
    }

    fn next_char_end(&self) -> usize {
        let mut end = self.pos + 1;
        while end < self.src.len() && !self.src.is_char_boundary(end) {
            end += 1;
        }
        end
    }
}

/// What a `{ ... }` hole turned out to hold.
enum Hole {
    Value(String),
    /// Only comments — a comment child, not an expression.
    Comment,
}

/// Wraps prose in a Luau block comment, at a bracket level the text cannot close.
///
/// `--[[ … ]]` is the usual form, but text containing `]]` would end the comment
/// early, so the level grows until it is unambiguous — the same trick Lua's own
/// long strings use.
/// Wraps markup comment text as a Luau block comment.
///
/// A comment spanning several lines keeps its own newlines and indentation, so
/// the emission covers the same lines the source did (PLAN.md §5.5) and reads
/// as the author wrote it. Collapsing it would pull the first line up against
/// `--[[` and leave the rest hanging at their original indent.
///
/// A single-line comment is padded instead, because `--[[text]]` reads worse
/// than `--[[ text ]]`.
fn block_comment(text: &str) -> String {
    let mut body = if text.contains('\n') {
        text.to_string()
    } else {
        format!(" {} ", text.trim())
    };

    // A body ending in `]` would otherwise meet the closing bracket and form
    // `]]]`, which closes the comment one byte early.
    if body.ends_with(']') {
        body.push(' ');
    }

    // Long brackets do not nest, so pick a level the body cannot terminate.
    let mut level = 0;

    while body.contains(&format!("]{}]", "=".repeat(level))) {
        level += 1;
    }

    let equals = "=".repeat(level);
    format!("--[{equals}[{body}]{equals}]")
}

/// Whether a captured expression contains anything but whitespace and comments.
fn holds_a_value(expression: &str) -> bool {
    match crate::lexer::tokenize(expression) {
        Ok(tokens) => tokens.iter().any(|token| !token.is_trivia()),
        // Unlexable is not empty; let a later pass produce the better message.
        Err(_) => true,
    }
}

/// LuauX whitespace rules, matching PROPOSAL.md Rule 4.
///
/// Whitespace adjacent to a newline is structural indentation and is dropped;
/// whitespace that is not is meaningful and kept. So `\n  Name: ` becomes
/// `Name: ` — the trailing space survives, which is what makes
/// `<TextLabel>\n  Name: {x}\n</TextLabel>` render as `Name: {x}`.
fn normalise_text(raw: &str) -> String {
    if !raw.contains('\n') {
        return raw.to_string();
    }

    let lines: Vec<&str> = raw.split('\n').collect();
    let last = lines.len() - 1;
    let mut parts = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let mut line = *line;

        if index > 0 {
            line = line.trim_start();
        }
        if index < last {
            line = line.trim_end();
        }

        if !line.is_empty() {
            parts.push(line);
        }
    }

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Node {
        let (node, end) = parse_node(src, 0).expect("parse");
        assert_eq!(end, src.len(), "parser did not consume the whole input");
        node
    }

    fn element(src: &str) -> Element {
        match parse(src) {
            Node::Element(element) => element,
            other => panic!("expected an element, got {other:?}"),
        }
    }

    /// Children reduced to kind+content, so tests don't assert brittle spans.
    fn summary(children: &[Child]) -> Vec<String> {
        children
            .iter()
            .map(|child| match child {
                Child::Text { text, .. } => format!("text:{text}"),
                Child::Expression { expression, .. } => format!("expr:{expression}"),
                Child::Node(_) => "node".to_string(),
                Child::Comment { luau, .. } => format!("comment:{luau}"),
            })
            .collect()
    }

    #[test]
    fn parses_self_closing_element() {
        let element = element("<Frame/>");
        assert_eq!(element.name, ElementName::Simple("Frame".into()));
        assert!(element.attributes.is_empty());
        assert!(element.children.is_empty());
    }

    #[test]
    fn parses_attributes() {
        let element = element(r#"<Frame Name='a' Size={UDim2.new(1, 0)} Visible />"#);
        assert_eq!(
            element.attributes,
            vec![
                Attribute::Named {
                    name: "Name".into(),
                    value: AttributeValue::StringLiteral("'a'".into()),
                    span: Span::new(7, 15),
                },
                Attribute::Named {
                    name: "Size".into(),
                    value: AttributeValue::Expression("UDim2.new(1, 0)".into()),
                    span: Span::new(16, 38),
                },
                Attribute::Named {
                    name: "Visible".into(),
                    value: AttributeValue::Boolean,
                    span: Span::new(39, 46),
                },
            ]
        );
    }

    /// `={expr}` is unambiguous in attribute position: an attribute is a name, a
    /// name with a value, or a spread, and none of them can begin with `=`.
    #[test]
    fn parses_an_inferred_attribute() {
        let element = element(r#"<Frame ={props.Size} Name="a" ={Visible} />"#);
        assert_eq!(
            element.attributes,
            vec![
                Attribute::Inferred {
                    expression: "props.Size".into(),
                    span: Span::new(7, 20),
                },
                Attribute::Named {
                    name: "Name".into(),
                    value: AttributeValue::StringLiteral("\"a\"".into()),
                    span: Span::new(21, 29),
                },
                Attribute::Inferred {
                    expression: "Visible".into(),
                    span: Span::new(30, 40),
                },
            ]
        );
    }

    /// The parser only takes the expression; whether it *names* anything is
    /// decided later, where the mistake can be recovered from.
    #[test]
    fn an_inferred_attribute_takes_any_expression() {
        let element = element(r#"<Frame ={f().x} />"#);
        assert_eq!(
            element.attributes[0],
            Attribute::Inferred {
                expression: "f().x".into(),
                span: Span::new(7, 15),
            }
        );
    }

    /// `Visible ={x}` reads two ways, and picking one silently set a property
    /// the author never named while leaving the one they did name unset — with
    /// no diagnostic, because `Visible` resolves perfectly well.
    #[test]
    fn a_shorthand_beside_an_inferred_attribute_is_refused() {
        for source in [
            "<Frame Visible ={props.Size}/>",
            "<Frame\n  Visible\n  ={props.Size}\n/>",
        ] {
            let error = parse_node(source, 0).expect_err("should fail");
            assert!(
                error.message.contains("could be a value for"),
                "{source}: {error:?}"
            );
        }
    }

    /// Only a hole is ambiguous — the inferred form always takes one — so a
    /// spaced string or a spaced hole-less value is untouched.
    #[test]
    fn spacing_around_a_value_is_otherwise_fine() {
        let element = element(r#"<Frame Name = "a" />"#);
        assert_eq!(
            element.attributes[0],
            Attribute::Named {
                name: "Name".into(),
                value: AttributeValue::StringLiteral("\"a\"".into()),
                span: Span::new(7, 17),
            }
        );
    }

    /// A shorthand with no hole after it is a parse error, not a name.
    #[test]
    fn an_inferred_attribute_needs_a_hole() {
        assert!(parse_node("<Frame = />", 0).is_err());
        assert!(parse_node("<Frame =\"a\" />", 0).is_err());
        assert!(parse_node("<Frame =Size />", 0).is_err());
    }

    #[test]
    fn attribute_expression_tolerates_braces_in_strings() {
        let element = element(r#"<Frame A={f("}")} />"#);
        assert_eq!(
            element.attributes[0],
            Attribute::Named {
                name: "A".into(),
                value: AttributeValue::Expression(r#"f("}")"#.into()),
                span: Span::new(7, 17),
            }
        );
    }

    #[test]
    fn parses_spread_attribute() {
        let element = element("<Frame {props} />");
        assert!(matches!(
            element.attributes[0],
            Attribute::Spread { ref expression, .. } if expression == "props"
        ));
    }

    #[test]
    fn parses_member_names() {
        let element = element("<Foo.Bar.Baz />");
        assert_eq!(
            element.name,
            ElementName::Member(vec!["Foo".into(), "Bar".into(), "Baz".into()])
        );
    }

    #[test]
    fn parses_nested_children() {
        let element = element("<Frame><TextLabel/><TextButton/></Frame>");
        assert_eq!(element.children.len(), 2);
    }

    #[test]
    fn parses_fragments() {
        match parse("<><Frame/><Frame/></>") {
            Node::Fragment(fragment) => assert_eq!(fragment.children.len(), 2),
            other => panic!("expected a fragment, got {other:?}"),
        }
    }

    #[test]
    fn parses_text_and_expression_children() {
        let element = element("<TextLabel>Name: {name}</TextLabel>");
        assert_eq!(summary(&element.children), ["text:Name: ", "expr:name"]);
    }

    #[test]
    fn drops_indentation_but_keeps_meaningful_spaces() {
        let element = element("<TextLabel>\n  Name: {name}\n</TextLabel>");
        assert_eq!(summary(&element.children), ["text:Name: ", "expr:name"]);
    }

    #[test]
    fn joins_lines_with_a_single_space() {
        let element = element("<TextLabel>\n  Hi\n  Hello\n</TextLabel>");
        assert_eq!(summary(&element.children), ["text:Hi Hello"]);
    }

    #[test]
    fn drops_whitespace_only_runs_between_elements() {
        let element = element("<Frame>\n  <TextLabel/>\n</Frame>");
        assert_eq!(element.children.len(), 1);
    }

    #[test]
    fn retains_comments_for_the_backend_to_decide_on() {
        // Kept in the tree, and always carried into the output.
        let element = element("<Frame>\n<!-- note -->\n<TextLabel/>\n</Frame>");
        assert_eq!(summary(&element.children), ["comment:--[[ note ]]", "node"]);
    }

    #[test]
    fn decodes_escapes_in_text() {
        let element = element(r"<TextLabel>a \{b} c \\ d</TextLabel>");
        assert_eq!(summary(&element.children), [r"text:a {b} c \ d"]);
    }

    #[test]
    fn keeps_luau_escapes_in_attribute_strings() {
        let element = element(r#"<TextLabel Text="a\nb" />"#);
        assert_eq!(
            element.attributes[0],
            Attribute::Named {
                name: "Text".into(),
                value: AttributeValue::StringLiteral(r#""a\nb""#.into()),
                span: Span::new(11, 22),
            }
        );
    }

    #[test]
    fn reports_mismatched_closing_tag() {
        let error = parse_node("<Frame></TextLabel>", 0).expect_err("should fail");
        assert!(
            error.message.contains("does not match"),
            "unexpected message: {}",
            error.message
        );
    }

    #[test]
    fn reports_unclosed_element() {
        assert!(parse_node("<Frame>", 0).is_err());
        assert!(parse_node("<Frame", 0).is_err());
    }

    #[test]
    fn a_comment_only_hole_is_a_comment_child() {
        // JSX writes comments as `{/* ... */}`; the Luau analogue is a hole
        // holding only a comment. It contributes no child, because
        // `{ a, --[[c]], b }` is a syntax error — a comment is not a field.
        for source in [
            "<Frame>{--[[ note ]]}</Frame>",
            "<Frame>{-- note\n}</Frame>",
            "<Frame>{\n  --[[ multi\n       line ]]\n}</Frame>",
        ] {
            let element = element(source);
            assert!(
                matches!(element.children.as_slice(), [Child::Comment { .. }]),
                "{source} -> {:?}",
                element.children
            );
        }

        // Siblings survive around it, which is the case that used to break.
        let siblings = element("<Frame><TextLabel/>{--[[ note ]]}<TextLabel/></Frame>");
        assert_eq!(
            summary(&siblings.children),
            ["node", "comment:--[[ note ]]", "node"]
        );

        // A trailing comment beside a real value is still a value.
        let valued = element("<Frame>{x --[[ note ]]}</Frame>");
        assert_eq!(summary(&valued.children), ["expr:x --[[ note ]]"]);
    }

    #[test]
    fn a_markup_comment_containing_brackets_still_closes_correctly() {
        // `]]` in the prose would end a plain `--[[ … ]]` early.
        let element = element("<Frame><!-- a ]] b --></Frame>");
        assert_eq!(summary(&element.children), ["comment:--[=[ a ]] b ]=]"]);
    }

    #[test]
    fn an_empty_hole_is_rejected() {
        // `{}` says nothing about intent, so it stays an error everywhere.
        for source in ["<Frame A={} />", "<Frame A={   } />", "<Frame>{}</Frame>"] {
            let error = parse_node(source, 0).expect_err("should fail");
            assert!(
                error.message.contains("is empty"),
                "{source}: {}",
                error.message
            );
        }
    }

    #[test]
    fn attribute_positions_still_require_a_value() {
        // A comment cannot stand in for a prop or a spread.
        for source in ["<Frame A={--[[ note ]]} />", "<Frame {--[[ note ]]} />"] {
            let error = parse_node(source, 0).expect_err("should fail");
            assert!(
                error.message.contains("no value"),
                "{source}: {}",
                error.message
            );
        }
    }

    #[test]
    fn stops_at_the_end_of_the_node() {
        let (_, end) = parse_node("local x = <Frame/> + 1", 10).expect("parse");
        assert_eq!(end, 18);
    }
}
