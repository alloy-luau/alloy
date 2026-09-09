//! Hover: field, module, and declaration hovers, and the name a printed type reads by.

use super::completion::{lands_on_member, member_position, sep_of};
use super::diagnostics::names_word;
use super::documents::project_aliases;
use super::*;

impl Server {
    /// Answers a hover on bytes the desugar replaced: an Alloy keyword or
    /// operator gets its own text, other punctuation gets nothing. A word
    /// with no entry, such as a hoisted name, still goes to the child.
    /// Returns false when the child should answer.
    /// A hover on the name of a struct, interface, enum, or trait shows
    /// the declaration as the source wrote it. The child would show a
    /// table or a type alias. This file's declarations win; then any
    /// open file's, since an import brings the name in unchanged.
    /// The shadow position of the word a hover asks about, when the
    /// mapped position lands off it. `None` when the map is already
    /// right, or the cursor is on no word.
    pub(crate) fn hover_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = message
            .pointer("/params/position")
            .and_then(position_of_value)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;

        if !keywords::is_word_at(&doc.source, offset) {
            return None;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];
        let (sl, sc) = doc.to_shadow(line, character);
        let landed = doc
            .shadow
            .lines()
            .nth(sl as usize)
            .and_then(|l| offset_of(l, 0, sc).map(|b| (l, b)))
            .is_some_and(|(l, b)| {
                keywords::is_word_at(l, b) && {
                    let (ws, we) = keywords::word_range(l, b);

                    &l[ws..we] == word
                }
            });

        match landed {
            true => None,

            false => shadow_home(&doc.shadow, line, word),
        }
    }

    /// The shadow position a member completion belongs at. `a?.b` and
    /// `a!.b` lower to text the compiler wrote, and `await X.m()` moves
    /// the receiver into a call, so the member the author is typing maps
    /// nowhere; the member the lowering wrote is the one with a type
    /// behind it. A plain access the child already reads keeps its own
    /// position.
    pub(crate) fn member_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = message
            .pointer("/params/position")
            .and_then(position_of_value)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;

        // A `.alx` lowering moves columns of its own; leave it alone.
        if doc.is_alx {
            return None;
        }

        let offset = offset_of(&doc.source, line, character)?;
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let source_line = doc.source.lines().nth(line as usize)?;
        // A field default moves to the line the struct's header takes,
        // so the member's line is the one the map reports, not this one.
        let (shadow_line_no, _) = doc.to_shadow(line, character);
        let shadow_line = doc.shadow.lines().nth(shadow_line_no as usize)?;
        let Some((base, access, sep, prefix)) = context::member_at(&doc.source, offset) else {
            // A call before the guard, `f(x)?:m()`: the lowering binds
            // the value to a name of its own, so no receiver of the
            // source is on the lowered line.
            let column = context::guarded_member_column(
                &doc.source[line_start..offset],
                shadow_line,
                sep_of(&doc.source, offset)?,
            )?;

            return Some((shadow_line_no, shadow_line[..column].chars().count() as u32));
        };

        if access == context::Access::Plain
            && lands_on_member(doc, line, character, &base, sep, prefix)
        {
            return None;
        }

        let column = context::member_column(
            source_line,
            shadow_line,
            &base,
            access,
            sep,
            prefix,
            offset - line_start,
        )?;

        Some((shadow_line_no, shadow_line[..column].chars().count() as u32))
    }

    /// The shadow position an expression completion belongs at. The emit
    /// qualifies a std name, `Ok(v)` to `__alloy.Ok(v)`, so a caret at
    /// that name maps past a `.` the source never wrote and the child
    /// lists the runtime's own table. The scope answers where the
    /// qualifier starts.
    pub(crate) fn expression_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = message
            .pointer("/params/position")
            .and_then(position_of_value)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;

        if doc.is_alx || member_position(doc, line, character).is_some() {
            return None;
        }

        let (sl, sc) = doc.to_shadow(line, character);
        let text = doc.shadow.lines().nth(sl as usize)?;
        let at = offset_of(text, 0, sc)?;
        // The map lands on the qualified name, at either end of it.
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let head = text[..at.min(text.len())].trim_end_matches(word);
        let qualifier = head.strip_suffix('.')?;
        let start = qualifier.trim_end_matches(word).len();

        // Only the runtime's own table: a temporary the desugar bound,
        // `_1`, stands for an expression the source did write.
        qualifier[start..]
            .starts_with("__alloy")
            .then(|| (sl, text[..start].chars().count() as u32))
    }

    /// The hover of a name the source declares and the child sees only
    /// as a table: a `remote`, and the namespace an `import * as` binds.
    pub(crate) fn source_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = doc.source[start..end].to_string();
        let path = uri_to_path(uri);
        // A `remote` or an exported `const` the file imported is
        // declared somewhere else; the child reads the emitted local
        // and calls it a `local`.
        let imported = || {
            doc.import_sources
                .iter()
                .find_map(|text| remote_hover(text, &word).or_else(|| const_hover(text, &word)))
        };
        let answer = remote_hover(&doc.source, &word)
            .or_else(imported)
            .or_else(|| {
                let dir = path
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new("."))
                    .to_path_buf();
                let aliases = project_aliases(&dir, st.root.as_deref());
                let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
                let before = &doc.source[line_start..start];
                let in_spec =
                    before.matches('"').count() % 2 == 1 || before.matches('\'').count() % 2 == 1;

                module_hover(&doc.source, &word, path.as_deref(), &aliases, in_spec)
            });

        let Some(answer) = answer else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": answer },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    pub(crate) fn declaration_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];

        // A sigil names a macro or an attribute of this project. After a
        // dot the name is the receiver's: `Msg.Join` finds the variant
        // through the enum's name; any other field is not ours.
        let before = doc.source[..start].trim_end();
        let key = if doc.source[..start].ends_with('$') || before.ends_with("macro") {
            format!("${word}")
        } else if doc.source[..start].ends_with('@') || before.ends_with("attribute") {
            format!("@{word}")
        } else if let Some(head) = before.strip_suffix('.') {
            let at = head.len().saturating_sub(1);

            if head.is_empty() || !keywords::is_word_at(&doc.source, at) {
                return false;
            }

            let (hs, he) = keywords::word_range(&doc.source, at);

            format!("{}.{word}", &doc.source[hs..he])
        } else if doc.source[..start].ends_with(':') {
            // `obj:method`, not the `x: T` of an annotation.
            return false;
        } else {
            word.to_string()
        };

        // An attribute or a macro is keyed by its sigil; a bare name that
        // an import bound finds it that way.
        let sigils = [format!("@{key}"), format!("${key}")];
        let lookup = |name: &str| {
            doc.decls.iter().find(|d| d.name == name).or_else(|| {
                st.docs
                    .values()
                    .flat_map(|d| d.decls.iter())
                    .find(|d| d.name == name)
            })
        };
        let found = lookup(&key).or_else(|| sigils.iter().find_map(|k| lookup(k)));

        let Some(decl) = found else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": decl.hover },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// A `case` pattern's binding hovers as the payload it names. A
    /// match lowers to one expression, so the binding has no local of
    /// its own and the child answers the arm's result instead.
    pub(crate) fn case_binding_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = doc.source[start..end].to_string();
        let known = st.known_shapes_at(Some(uri));

        let Some(answer) = case_binding_text(doc, line as usize, start, &word, &known) else {
            return false;
        };
        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": answer },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// A key in a struct's raw constructor, `Menu { button = ... }`, hovers
    /// as the struct's field. The child sees a table key and answers with
    /// the key's string type.
    pub(crate) fn field_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        if !keywords::is_word_at(&doc.source, offset) {
            return false;
        }

        let (start, end) = keywords::word_range(&doc.source, offset);
        let word = &doc.source[start..end];

        // A field where it is declared, `read hp: number = 1` in a struct
        // body: the child sees the constructor's table, where a default
        // makes the field optional, so the declaration answers itself.
        if let Some(answer) = declared_field_hover(doc, start, end)
            .or_else(|| remote_parameter_hover(doc, start, end))
            .or_else(|| declared_parameter_hover(doc, start, end))
            .or_else(|| foreign_method_hover(doc, start, end))
            .or_else(|| type_parameter_hover(doc, start, end))
        {
            let (sl, sc) = position_of(&doc.source, start);
            let (el, ec) = position_of(&doc.source, end);
            let result = json!({
                "contents": { "kind": "markdown", "value": answer },
                "range": {
                    "start": { "line": sl, "character": sc },
                    "end": { "line": el, "character": ec }
                }
            });
            drop(st);
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return true;
        }

        // The key sits before `=` inside the braces of `Name { ... }`.
        if !doc.source[end..].trim_start().starts_with('=')
            || doc.source[end..].trim_start().starts_with("==")
        {
            return false;
        }

        let Some(open) = enclosing_brace(&doc.source, start) else {
            return false;
        };
        // `new Slotted<<T>> { value = ... }`: the arguments stand between
        // the name and the brace.
        let head = doc.source[..open].trim_end();
        let head = head.strip_suffix(">>").map_or(head, |h| {
            h.rfind("<<").map_or(head, |i| doc.source[..i].trim_end())
        });

        if !head.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
            return false;
        }

        let (hs, he) = keywords::word_range(&doc.source, head.len() - 1);
        let struct_name = &doc.source[hs..he];
        let field_line = doc
            .decls
            .iter()
            .chain(st.docs.values().flat_map(|d| d.decls.iter()))
            .filter(|d| d.name == struct_name && d.hover.contains("struct "))
            .find_map(|d| {
                d.hover
                    .lines()
                    .find(|l| field_key(l) == Some(word))
                    .map(|l| l.trim().to_string())
            });

        let Some(field_line) = field_line else {
            return false;
        };

        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": {
                "kind": "markdown",
                "value": format!("```alloy\n{field_line}\n```\nA field of `struct {struct_name}`."),
            },
            "range": {
                "start": { "line": sl, "character": sc },
                "end": { "line": el, "character": ec }
            }
        });
        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    pub(crate) fn keyword_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(out) = &doc.output else {
            return false;
        };

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        // A copied byte has a shadow position; the child answers there.
        if out.map.to_output(offset as u32).is_some() {
            return false;
        }

        // A std name the file binds itself, through an import or a
        // declaration, is the file's: the child answers for that one.
        let hit = keywords::hover(&doc.source, offset).filter(|(start, end, _)| {
            let word = &doc.source[*start..*end];
            let is_std = alloy::desugar::AMBIENT.contains(&word)
                || matches!(
                    word,
                    "SignalConnection" | "Signalish" | "Partial" | "Readonly" | "Sink"
                );

            // A `:` opens a type far more often than it closes a
            // ternary; the `?` is what makes it the else.
            if word == ":" && !ternary_else_at(&doc.source, *start) {
                return false;
            }

            !(is_std && doc_binds(doc, word))
        });

        let result = match hit {
            Some((start, end, text)) => {
                let (sl, sc) = position_of(&doc.source, start);
                let (el, ec) = position_of(&doc.source, end);
                // A std type: the overview, then the names a reader can
                // hover on their own.
                let text = alloy::docs::type_markdown(&doc.source[start..end])
                    .unwrap_or_else(|| text.to_string());

                json!({
                    "contents": { "kind": "markdown", "value": text },
                    "range": {
                        "start": { "line": sl, "character": sc },
                        "end": { "line": el, "character": ec },
                    },
                })
            }

            // A replaced word such as a struct name has a home in the
            // shadow: on the same line when the emit kept it there, else
            // where the declaration landed. The child answers there.
            None if keywords::is_word_at(&doc.source, offset) => {
                let (start, end) = keywords::word_range(&doc.source, offset);
                let word = &doc.source[start..end];

                let Some(target) = shadow_home(&doc.shadow, line, word) else {
                    return false;
                };

                drop(st);
                self.forward_request_at(message.clone(), Some("textDocument/hover"), target);

                return true;
            }

            None => Value::Null,
        };

        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// Answers a hover or completion inside `.alx` markup. Returns false
    /// when the cursor is not on markup, so the child answers.
    pub(crate) fn markup_answer(
        &self,
        method: &str,
        uri: &str,
        message: &Value,
        id: &Value,
    ) -> bool {
        let Some((line, character)) = message
            .pointer("/params/position")
            .and_then(position_of_value)
        else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };
        let bound = markup_bound(&doc.source);

        let result = match method {
            "textDocument/hover" => match markup::hover_spot(&doc.source, offset) {
                Some(spot) => markup::hover(&spot, &bound).unwrap_or(Value::Null),

                None => return false,
            },

            _ => match markup::completion_spot(&doc.source, offset) {
                Some(spot) => {
                    let props = st.ingot_props(uri);

                    Value::Array(markup::completions(&spot, &bound, &doc.source, &props))
                }

                None => return false,
            },
        };

        drop(st);
        self.respond(id, result);

        true
    }
}

/// Maps positions and ranges in request params into the shadow.
/// The shadow position of `word` for a hover: the same line first, then
/// the first line that holds it as a whole word.
pub(crate) fn shadow_home(shadow: &str, line: u32, word: &str) -> Option<(u32, u32)> {
    let lines: Vec<&str> = shadow.lines().collect();
    let same = lines.get(line as usize).copied().unwrap_or("");

    let (l, text, byte) = keywords::find_word(same, word)
        .map(|c| (line, same, c))
        .or_else(|| {
            lines
                .iter()
                .enumerate()
                .find_map(|(i, l)| keywords::find_word(l, word).map(|c| (i as u32, *l, c)))
        })?;

    Some((l, text[..byte].chars().count() as u32))
}

/// The child's hover header with the source's declaring keywords: the
/// `local m: T` of a `const` becomes `const m: T`, and `function f(` of
/// an `async function` becomes `async function f(`. The fence switches
/// to the Alloy grammar, which highlights `const`, `async`, and
/// `export`; the Luau grammar drops the highlight after them. None when
/// the header names something else or the source used the same keyword.
pub(crate) fn restyle_hover(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let binding = doc.bindings.iter().find(|b| b.name == word)?;
    let rest = value.strip_prefix("```luau\n")?;

    // The child reads a `---` comment the shadow keeps, and the hover
    // carries it behind a rule. That doc is the binding's own: adding
    // it again shows it twice.
    let doc_text = binding
        .doc
        .as_deref()
        .filter(|_| !rest.contains("\n```\n----------\n"));

    // A type function hovers as `function<a>(t): type`, nameless: the
    // name goes back in, behind `type function`.
    if (rest.starts_with("function<") || rest.starts_with("function("))
        && binding.prefix.ends_with("function")
    {
        let mut out = format!(
            "```alloy\n{} {word}{}",
            binding.prefix,
            &rest["function".len()..]
        );

        if let Some(doc) = doc_text {
            out.push_str("\n\n");
            out.push_str(doc);
        }

        return Some(out);
    }

    let (head, tail) = match rest {
        r if r.starts_with(&format!("local function {word}")) => {
            ("local function", &r["local function".len()..])
        }

        r if r.starts_with(&format!("local {word}")) => ("local", &r["local".len()..]),

        r if r.starts_with(&format!("function {word}")) => ("function", &r["function".len()..]),

        _ => return None,
    };

    if head == binding.prefix && doc_text.is_none() {
        return None;
    }

    let mut out = format!("```alloy\n{}{tail}", binding.prefix);

    if let Some(doc) = doc_text {
        out.push_str("\n\n");
        out.push_str(doc);
    }

    Some(out)
}

/// Whether the file binds the name itself: a declaration, a local, a
/// function, or an import. A std name so bound belongs to the file.
pub(crate) fn doc_binds(doc: &Doc, name: &str) -> bool {
    doc.decls.iter().any(|d| d.name == name)
        || doc.bindings.iter().any(|b| b.name == name)
        || imports::bound_names(&doc.source).iter().any(|n| n == name)
}

/// The type the source names at a position outright: the struct a
/// `new Name` or a `Name.new(` builds, and the struct a method's `self`
/// belongs to inside an `impl`.
pub(crate) fn source_type(doc: &Doc, line: u32, character: u32) -> Option<String> {
    let text = doc.source.lines().nth(line as usize)?;
    let declares = |name: &str| {
        doc.shapes
            .iter()
            .chain(doc.import_shapes.iter())
            .any(|s| matches!(s, alloy::declarations::Shape::Struct { name: n, .. } if n == name))
    };

    // `function get(self)` inside `impl Box`: the receiver is the struct.
    let before: String = text.chars().take(character as usize).collect();

    if before.trim_end().ends_with("self") && text.contains("function ") {
        return impl_self_type(doc, line).filter(|t| declares(t.split('<').next().unwrap_or(t)));
    }

    // `local root = new Node { ... }`, `local b = new Box<<number>> { }`.
    if let Some(i) = text.find("new ") {
        let rest = text[i + "new ".len()..].trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if declares(&name) {
            let args = rest[name.len()..]
                .strip_prefix("<<")
                .and_then(|a| a.find(">>").map(|e| a[..e].to_string()));

            return Some(match args {
                Some(a) => format!("{name}<{a}>"),

                None => name,
            });
        }
    }

    // `local p = Point.new(1, 2)`.
    let rest = text.split_once("= ")?.1.trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    // `Signal.new<<Effect>>()`: a std constructor with the arguments
    // the source wrote. Nothing else names them.
    if name.starts_with(|c: char| c.is_ascii_uppercase())
        && let Some(args) = rest[name.len()..]
            .strip_prefix(".new<<")
            .and_then(|a| a.find(">>").map(|e| a[..e].to_string()))
        && !args.is_empty()
    {
        return Some(format!("{name}<{args}>"));
    }

    // `local burn = Effect.Damage(20, Element.Fire)`: a variant with a
    // payload is a value of its enum, and the child prints the tagged
    // table it lowers to.
    if let Some(after) = rest[name.len()..].strip_prefix('.') {
        let variant: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let holds = doc.shapes.iter().chain(doc.import_shapes.iter()).any(|s| {
            matches!(s, alloy::declarations::Shape::Enum { name: n, variants }
                if *n == name && variants.iter().any(|(v, _)| *v == variant))
        });

        if holds {
            return Some(name);
        }
    }

    (declares(&name) && rest[name.len()..].starts_with(".new(")).then_some(name)
}

/// The struct a method's `self` belongs to: the nearest `impl` above
/// the line, with the type parameters the struct declares.
pub(crate) fn impl_self_type(doc: &Doc, line: u32) -> Option<String> {
    let head = doc
        .source
        .lines()
        .take(line as usize + 1)
        .filter_map(|l| {
            let text = l.trim_start();
            // A foreign impl is exported, so it works project wide.
            let text = text.strip_prefix("export ").unwrap_or(text);

            text.strip_prefix("impl ").map(str::to_string)
        })
        .last()?;
    // `impl Shape for Sq` names the struct after `for`.
    let named = head.split(" for ").last().unwrap_or(&head).trim();
    let name: String = named
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let generics = struct_generics(&doc.source, &name);

    (!name.is_empty()).then(|| format!("{name}{generics}"))
}

/// The type parameters of `struct Name<T, U>`, as the source writes
/// them; an empty text when the struct takes none.
pub(crate) fn struct_generics(source: &str, name: &str) -> String {
    let needle = format!("struct {name}<");
    let Some(i) = source.find(&needle) else {
        return String::new();
    };
    let rest = &source[i + needle.len()..];
    let Some(end) = rest.find('>') else {
        return String::new();
    };
    let params: Vec<&str> = rest[..end]
        .split(',')
        .map(|p| p.split(':').next().unwrap_or("").trim())
        .filter(|p| !p.is_empty())
        .collect();

    match params.is_empty() {
        true => String::new(),

        false => format!("<{}>", params.join(", ")),
    }
}

/// The byte offset of the `{` that encloses `at`, at depth zero, when
/// the brace is on the same line or an earlier one within the statement.
pub(crate) fn enclosing_brace(source: &str, at: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in source[..at].char_indices().rev() {
        match c {
            '}' => depth += 1,

            '{' if depth == 0 => return Some(i),

            '{' => depth -= 1,

            _ => {}
        }
    }

    None
}

/// The child prints a std value's type as its whole shape. The shapes the
/// runtime builds read as their names instead: the Future table becomes
/// `Future<T>`, the Array metatable pair becomes `T[]`, and `Array<T>`
/// with a plain element becomes `T[]` too.
pub(crate) fn fold_std_shapes(value: &str) -> String {
    let mut out = value.to_string();

    // Future: `{ andThen: (self: any, on_resolve: ((T) -> ())?, ... is_settled: (self: any) -> boolean }`.
    // A Future that carries `__value` names itself in `shapes::fold`,
    // where the value type may hold braces of its own.
    while !out.contains("__value: ")
        && let Some(i) = out.find("andThen: (self: any, on_resolve: ((")
    {
        let Some(open) = out[..i].rfind('{') else {
            break;
        };
        let inner_start = i + "andThen: (self: any, on_resolve: ((".len();
        let Some(inner_len) = out[inner_start..].find(") -> ())?") else {
            break;
        };
        let inner = out[inner_start..inner_start + inner_len].to_string();
        let Some(settled) = out[i..].find("is_settled: (self: any) -> boolean") else {
            break;
        };
        let Some(close_rel) = out[i + settled..].find('}') else {
            break;
        };
        let close = i + settled + close_rel;
        out.replace_range(open..=close, &format!("Future<{inner}>"));
    }

    // A plain `Array<T>` reads as the sugar the source has.
    let mut from = 0;

    while let Some(i) = out[from..].find("Array<") {
        let start = from + i;
        let inner_start = start + "Array<".len();

        match out[inner_start..].find('>') {
            Some(n)
                if out[inner_start..inner_start + n]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '?') =>
            {
                let elem = out[inner_start..inner_start + n].to_string();
                out.replace_range(start..inner_start + n + 1, &format!("{elem}[]"));
                from = start + elem.len() + 2;
            }

            _ => from = inner_start,
        }
    }

    out
}

/// Two structs of one shape print alike, so the child may name either.
/// The struct the line constructs is the one the reader means.
pub(crate) fn prefer_constructed_struct(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
    known: &crate::shapes::Known,
) -> Option<String> {
    let (fence, body) = value.split_once('\n')?;
    let inner = body.trim().strip_suffix("```")?.trim();
    let (head, printed) = inner.rsplit_once(": ")?;
    let named = source_type(doc, line, character)?;

    if printed == named || printed.contains(' ') {
        return None;
    }

    let is_struct = |n: &str| {
        known
            .shapes
            .iter()
            .any(|s| matches!(s, alloy::declarations::Shape::Struct { name, .. } if name == n))
    };
    let offset = offset_of(&doc.source, line, character)?;
    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    // The cursor is on the binding the line declares.
    (head.ends_with(word) && is_struct(printed) && is_struct(&named))
        .then(|| format!("{fence}\n{head}: {named}\n```"))
}

/// The hover of a `remote`: the declaration as the source wrote it,
/// with the comment above it.
pub(crate) fn remote_hover(source: &str, word: &str) -> Option<String> {
    let mut at = 0;

    for line in source.lines() {
        let text = line.trim();
        let head = text.strip_prefix("export ").unwrap_or(text);

        if let Some(rest) = head.strip_prefix("remote ") {
            let rest = rest.strip_prefix("function ").unwrap_or(rest);
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if name == word {
                let doc_text = alloy::declarations::doc_before(source, at)
                    .map(|d| format!("\n\n{d}"))
                    .unwrap_or_default();

                return Some(format!("```alloy\n{text}\n```{doc_text}"));
            }
        }

        at += line.len() + 1;
    }

    None
}

/// The declaration line of an exported `const`, with the comment above
/// it. A `const` cannot be reassigned, and `local` says the opposite.
pub(crate) fn const_hover(source: &str, word: &str) -> Option<String> {
    let mut at = 0;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text.strip_prefix("export const ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if name == word {
                // The value is the module's, not the reader's.
                let head = text.split_once(" = ").map_or(text, |(h, _)| h);
                let doc_text = alloy::declarations::doc_before(source, at)
                    .map(|d| format!("\n\n{d}"))
                    .unwrap_or_default();

                return Some(format!("```alloy\n{head}\n```{doc_text}"));
            }
        }

        at += line.len() + 1;
    }

    None
}

/// What a `remote` declaration says: whether it answers, which sides
/// fire it, and whether it carries `@ratelimit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteSpec {
    pub(crate) answers: bool,
    pub(crate) from_client: bool,
    pub(crate) from_server: bool,
    pub(crate) ratelimited: bool,
}

impl RemoteSpec {
    /// Whether a file on `side` may fire the remote. A file with no
    /// side of its own sees both surfaces.
    pub(crate) fn fires(&self, side: Option<alloy::directives::Side>) -> bool {
        match side {
            Some(alloy::directives::Side::Client) => self.from_client,
            Some(alloy::directives::Side::Server) => self.from_server,
            None => true,
        }
    }

    /// Whether a file on `side` may handle the remote.
    pub(crate) fn handles(&self, side: Option<alloy::directives::Side>) -> bool {
        match side {
            Some(alloy::directives::Side::Client) => self.from_server,
            Some(alloy::directives::Side::Server) => self.from_client,
            None => true,
        }
    }

    /// Whether the surface holds a member. The emit types every member
    /// on every remote, so the declaration and the file's side are what
    /// tell them apart.
    pub(crate) fn holds(&self, member: &str, side: Option<alloy::directives::Side>) -> bool {
        match member {
            "spec" | "instance" => true,
            "fire" => self.fires(side),
            "call" => self.answers && self.fires(side),
            "fire_all" | "fire_except" => self.from_server && self.fires(side),
            "on" | "once" | "wait" => self.handles(side),
            "on_ratelimited" => self.ratelimited && self.handles(side),
            _ => true,
        }
    }
}

/// The `remote` declaration a source writes for `name`, with the
/// attributes above it.
pub(crate) fn remote_spec(source: &str, name: &str) -> Option<RemoteSpec> {
    let lines: Vec<&str> = source.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let text = line.trim();
        let head = text.strip_prefix("export ").unwrap_or(text);
        let Some(rest) = head.strip_prefix("remote ") else {
            continue;
        };
        let answers = rest.starts_with("function ");
        let rest = rest.strip_prefix("function ").unwrap_or(rest);
        let word: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if word != name {
            continue;
        }

        let tail = text.rfind(" from ").map_or("", |at| &text[at..]);
        let mut ratelimited = false;

        // The attribute lines the declaration carries sit right above
        // it, comments aside; a blank line ends them.
        for above in lines[..i].iter().rev() {
            let t = above.trim();

            if t.starts_with('@') {
                ratelimited = ratelimited || t.starts_with("@ratelimit");

                continue;
            }

            if t.starts_with("--") {
                continue;
            }

            break;
        }

        return Some(RemoteSpec {
            answers,
            from_client: names_word(tail, "client"),
            from_server: names_word(tail, "server"),
            ratelimited,
        });
    }

    None
}

/// The hover of a name an import binds to a whole module: `import * as
/// Lib`, and the default binding of `import fluid from "@pkg/fluid"`.
///
/// The child reads the emitted `require` and prints the module's table,
/// a `__SCHEDULER_INTERFACE` field and dozens of lines with it. The
/// import line and the names the module exports say what the reader
/// asked. A `.luau` module answers the same way.
pub(crate) fn module_hover(
    source: &str,
    word: &str,
    from: Option<&Path>,
    aliases: &[(String, PathBuf)],
    in_spec: bool,
) -> Option<String> {
    // On the binding the child's answer stands: the module's table, as
    // it prints. On the path the answer is the file the path names.
    if !in_spec {
        return None;
    }

    let line = source.lines().find(|l| {
        let l = l.trim();

        l.starts_with("import ") && import_spec(l).is_some_and(|spec| spec_names(&spec, word))
    })?;
    let spec = import_spec(line)?;

    // A module the server cannot find is the child's to answer.
    module_target(&spec, from, aliases)?;

    // No link: the editor's document links already offer to follow the
    // path, on the same characters.
    Some(format!("```alloy\n{}\n```", line.trim()))
}

/// Whether an import path holds `word` as one of its segments, the
/// alias included: `@pkg/fluid` names `pkg` and `fluid`.
pub(crate) fn spec_names(spec: &str, word: &str) -> bool {
    spec.trim_start_matches('@')
        .split('/')
        .any(|segment| segment == word || segment.trim_end_matches(".luau") == word)
}

/// The spec of an import line, whichever quote it uses.
pub(crate) fn import_spec(line: &str) -> Option<String> {
    let at = line.rfind(" from ")? + " from ".len();
    let rest = line[at..].trim();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &rest[quote.len_utf8()..];
    let end = body.find(quote)?;

    Some(body[..end].to_string())
}

/// The file a spec names: an `@alias/tail` through the project's
/// aliases, anything else relative to the importing file.
pub(crate) fn module_target(
    spec: &str,
    from: Option<&Path>,
    aliases: &[(String, PathBuf)],
) -> Option<PathBuf> {
    let dir = from.and_then(Path::parent);
    let target = match spec.strip_prefix('@') {
        Some(rest) => {
            let (name, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let base = aliases
                .iter()
                .find(|(a, _)| a == name)
                .map(|(_, p)| p.clone())?;

            match tail.is_empty() {
                true => base,

                false => imports::lexical(&base, tail),
            }
        }

        None => imports::lexical(dir?, spec),
    };

    imports::module_file(&target)
}

/// A hover that prints one solver variable, `t3?`, names nothing. The
/// field the cursor reads names its declared type instead.
pub(crate) fn name_solver_variable(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, body) = value.split_once('\n')?;
    let inner = body.trim().strip_suffix("```")?.trim();
    let optional = inner.ends_with('?');
    let var = inner.trim_end_matches('?');

    if var.len() < 2 || !var.starts_with('t') || !var[1..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let offset = offset_of(&doc.source, line, character)?;
    let (start, end) = keywords::word_range(&doc.source, offset);
    let declared = declared_field_type(&doc.source, &doc.source[start..end])?;
    let declared = match optional && !declared.ends_with('?') {
        true => format!("{declared}?"),

        false => declared,
    };

    Some(format!("{fence}\n{declared}\n```"))
}

/// The type a struct body declares for a field, when one struct alone
/// declares it: `child: Node?` in `struct Node`.
pub(crate) fn declared_field_type(source: &str, field: &str) -> Option<String> {
    let mut found: Option<String> = None;
    let mut in_struct = false;

    for line in source.lines() {
        let text = line.trim();

        if text.starts_with("struct ") || text.starts_with("export struct ") {
            in_struct = true;

            continue;
        }

        if text == "end" {
            in_struct = false;

            continue;
        }

        if !in_struct {
            continue;
        }

        let head = text
            .trim_start_matches("private ")
            .trim_start_matches("public ")
            .trim_start_matches("read ")
            .trim_start_matches("write ");
        let Some((name, rest)) = head.split_once(':') else {
            continue;
        };

        if name.trim() != field {
            continue;
        }

        let declared = rest.split(" = ").next().unwrap_or(rest).trim().to_string();

        if declared.is_empty() {
            continue;
        }

        match &found {
            Some(other) if *other != declared => return None,

            _ => found = Some(declared),
        }
    }

    found
}

/// A hover header keeps the type the source wrote: `items: Item[]`
/// instead of the child's expansion of the array. The annotation comes
/// from the first `name: T` in the source.
pub(crate) fn keep_annotation(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let (decl_at, annotation) = declared_annotation(&doc.source, word, offset)?;

    // A test on the name between its declaration and the hover narrows
    // it: the child's type is the narrowed one, and it stays.
    if decl_at < offset && narrowed_between(&doc.source[decl_at..offset], word) {
        return None;
    }

    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let header_end = body.find('\n').unwrap_or(body.len());
    let header = &body[..header_end];
    let colon = header.find(&format!("{word}: "))? + word.len();
    let head = &header[..colon];

    // The header names the word as `local x: T`, `x: T`, or `const x: T`.
    if head != word && !head.trim_end_matches(word).ends_with(' ') {
        return None;
    }

    Some(format!("{fence}\n{head}: {annotation}\n```{tail}"))
}

/// The type text after the `name:` nearest before `at`: up to a `,`, a
/// `)`, an `=`, or the line's end at bracket depth zero. The result
/// carries the declaration's offset. A use never comes before its
/// declaration, so a later `name:` belongs to another scope.
pub(crate) fn declared_annotation(source: &str, name: &str, at: usize) -> Option<(usize, String)> {
    let mut from = 0;
    let mut found: Option<(usize, String)> = None;

    while let Some(i) = source[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let bounded = start
            .checked_sub(1)
            .is_none_or(|b| !keywords::is_word_at(source, b))
            && !keywords::is_word_at(source, end);
        let after = source[end..].trim_start();

        // `v: T` annotates; `v:m()` calls and `v :: T` casts.
        // A binding: `local v: T`, `const v: T`, or a parameter. A
        // field of a table type or a struct names another thing.
        let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
        let before = source[line_start..start].trim_end();
        let is_binding = before.ends_with("local")
            || before.ends_with("const")
            || before.ends_with('(')
            || before.ends_with(',');

        if bounded && is_binding && after.starts_with(": ") {
            let text = after[1..].trim_start();
            let mut depth = 0i32;
            let mut stop = text.len();

            for (j, c) in text.char_indices() {
                match c {
                    '(' | '{' | '[' | '<' => depth += 1,

                    ')' | '}' | ']' | '>' if depth > 0 => depth -= 1,

                    // A comma inside `Signal<Player, number>` is the type's.
                    ',' | '=' | '\n' if depth > 0 => {}

                    ')' | ',' | '=' | '\n' => {
                        stop = j;

                        break;
                    }

                    _ => {}
                }
            }

            let annotation = text[..stop].trim();

            if start > at {
                break;
            }

            if !annotation.is_empty() {
                found = Some((start, annotation.to_string()));
            }
        }

        from = end;
    }

    found
}

/// Whether a stretch of source tests `name`, so a use after it may be
/// narrowed: an `is`, a `typeof` or `type` call, an `IsA`, a nil
/// comparison, or a truthiness test.
pub(crate) fn narrowed_between(text: &str, name: &str) -> bool {
    let tests = [
        format!("{name} is "),
        format!("typeof({name})"),
        format!("type({name})"),
        format!("{name}:IsA("),
        format!("{name} == nil"),
        format!("{name} ~= nil"),
        format!("if {name} then"),
        format!("if not {name} then"),
        format!(" and {name} then"),
        format!("{name} and "),
        format!("{name} or "),
        format!("local {name} = "),
    ];

    for (i, _) in text.match_indices(name) {
        let bounded = i
            .checked_sub(1)
            .is_none_or(|b| !keywords::is_word_at(text, b));

        if !bounded {
            continue;
        }

        let from = text[..i].rfind(['\n', ' ', '(']).map_or(0, |k| k);

        if tests
            .iter()
            .any(|t| text[from..].starts_with(t) || text[i..].starts_with(t))
        {
            return true;
        }
    }

    false
}

/// The hover of a `case` pattern's binding at `line`: the name with the
/// type the pattern gives it. `None` when the line is in no arm, or the
/// word is no binding of it.
pub(crate) fn case_binding_text(
    doc: &Doc,
    line: usize,
    start: usize,
    word: &str,
    known: &crate::shapes::Known,
) -> Option<String> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let mut at = line.min(lines.len().saturating_sub(1));

    // The arm the line belongs to: the nearest `case` above it, and no
    // `end` or `match` head between.
    let case_line = loop {
        let text = lines.get(at)?.trim();

        if text.starts_with("case ") {
            break at;
        }

        if text == "end" || text.ends_with(" with") {
            return None;
        }

        at = at.checked_sub(1)?;
    };
    let pattern = case_pattern(lines[case_line])?;
    let bindings = pattern_bindings(&pattern, known, || array_element(&lines, case_line));

    // `b.amount` reads a field of what `case Buff(b)` bound; the child
    // sees the payload slot and answers `any`.
    if let Some(head) = doc.source[..start].strip_suffix('.')
        && keywords::is_word_at(&doc.source, head.len().checked_sub(1)?)
    {
        let (rs, re) = keywords::word_range(&doc.source, head.len() - 1);
        let receiver = &doc.source[rs..re];
        let (_, ty, _) = bindings.iter().find(|(n, _, _)| n == receiver)?;

        return field_of_struct(doc, ty, word);
    }

    let (_, ty, owner) = bindings.into_iter().find(|(n, _, _)| n == word)?;

    Some(format!(
        "```alloy\n{word}: {ty}\n```\nA binding of {owner}."
    ))
}

/// The hover of one field of a named struct, from the declaration index.
pub(crate) fn field_of_struct(doc: &Doc, name: &str, field: &str) -> Option<String> {
    let line = doc
        .decls
        .iter()
        .filter(|d| d.name == name && d.hover.contains("struct "))
        .find_map(|d| {
            d.hover
                .lines()
                .find(|l| field_key(l) == Some(field))
                .map(|l| l.trim().to_string())
        })?;

    Some(format!(
        "```alloy\n{line}\n```\nA field of `struct {name}`."
    ))
}

/// The pattern of a `case` line: what stands between `case` and the
/// arm's `then`, or the guard's `and`.
pub(crate) fn case_pattern(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("case ")?;
    let end = [rest.find(" then"), rest.find(" and ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());

    Some(rest[..end].trim().to_string())
}

/// The names a pattern binds, each with its type and what it comes
/// from. A payload reads its type off the enum's declaration; an array
/// pattern reads the element type of what the match runs over.
pub(crate) fn pattern_bindings(
    pattern: &str,
    known: &crate::shapes::Known,
    element: impl Fn() -> Option<String>,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();

    if let Some(inner) = pattern.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
        let Some(elem) = element() else {
            return out;
        };

        for item in inner.split(',') {
            let item = item.trim();

            match item.strip_prefix("...") {
                Some(rest) if is_binding(rest) => {
                    out.push((
                        rest.to_string(),
                        format!("{elem}[]"),
                        "the array pattern".into(),
                    ));
                }

                _ if is_binding(item) => {
                    out.push((item.to_string(), elem.clone(), "the array pattern".into()));
                }

                _ => {}
            }
        }

        return out;
    }

    let Some(open) = pattern.find('(') else {
        return out;
    };
    let head = pattern[..open].trim();
    let variant = head.rsplit('.').next().unwrap_or(head);
    let args = pattern[open + 1..].trim_end().trim_end_matches(')');

    let found = known.shapes.iter().find_map(|s| match s {
        alloy::declarations::Shape::Enum { name, variants } => variants
            .iter()
            .find(|(v, _)| v == variant)
            .map(|(_, payload)| (name.clone(), payload.clone())),

        _ => None,
    });

    let Some((enum_name, payload)) = found else {
        return out;
    };

    for (k, item) in split_top(args).into_iter().enumerate() {
        let item = item.trim();

        if !is_binding(item) {
            continue;
        }

        let Some(ty) = payload.get(k) else {
            continue;
        };
        out.push((
            item.to_string(),
            ty.clone(),
            format!("`{enum_name}.{variant}`"),
        ));
    }

    out
}

/// Whether a pattern item is a name the arm binds, and not `_` or a
/// literal.
pub(crate) fn is_binding(text: &str) -> bool {
    !text.is_empty()
        && text != "_"
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The members of a pattern's argument list, split at the commas that
/// stand outside every bracket.
pub(crate) fn split_top(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    for (k, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&text[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    out.push(&text[start..]);

    out
}

/// The element type the match runs over, for an array pattern: the
/// annotation of the name the `match` head reads.
pub(crate) fn array_element(lines: &[&str], case_line: usize) -> Option<String> {
    let head = lines[..case_line]
        .iter()
        .rev()
        .find(|l| l.trim_end().ends_with(" with"))?;
    let at = head.find("match ")? + "match ".len();
    let name = head[at..].trim_end().trim_end_matches("with").trim();

    if !is_binding(name) {
        return None;
    }

    let needle = format!("{name}: ");

    for line in lines[..case_line].iter().rev() {
        let Some(i) = line.find(&needle) else {
            continue;
        };
        let rest = &line[i + needle.len()..];
        let end = rest.find([',', ')']).unwrap_or(rest.len());
        let ty = rest[..end].trim();

        return ty.strip_suffix("[]").map(str::to_string);
    }

    None
}

/// The std type a receiver word names, with whether the word is the
/// type itself. A value resolves through its annotation, or through
/// what it starts from.
pub(crate) fn std_receiver(source: &str, sigil: usize, word: &str) -> Option<(&'static str, bool)> {
    if word.is_empty() {
        return None;
    }

    if let Some(key) = alloy::docs::member_owner(word) {
        return Some((key, true));
    }

    let base = match context::declared(source, sigil, word)? {
        context::Declared::Annotation(t) => alloy::docs::type_head(&t),
        context::Declared::Init(v) => alloy::docs::value_head(&v),
    }?;

    alloy::docs::member_owner(&base).map(|key| (key, false))
}

/// The std member the byte sits on: the word after a `.` or a `:` whose
/// receiver resolves to a std type that documents it.
pub(crate) fn std_member_at(
    source: &str,
    offset: usize,
) -> Option<(&'static str, &'static alloy::docs::Member)> {
    let (name, sigil, receiver) = alloy::docs::member_spot(source, offset)?;
    let (key, on_type) = std_receiver(source, sigil, receiver)?;
    let m = alloy::docs::member(key, name)?;

    alloy::docs::member_fits(m.kind, on_type).then_some((key, m))
}

/// The std member a hover sits on, as the doc and the example the
/// checker's type cannot carry. The source resolves the receiver where
/// it can; otherwise the type the child printed names it.
pub(crate) fn std_member_hover(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;
    let hit = std_member_at(&doc.source, offset).or_else(|| {
        let (name, _, _) = alloy::docs::member_spot(&doc.source, offset)?;

        alloy::docs::MEMBERS
            .iter()
            .filter(|(key, _)| names_type(value, key))
            .find_map(|(key, _)| alloy::docs::member(key, name).map(|m| (*key, m)))
    })?;

    Some(alloy::docs::member_hover(hit.0, hit.1))
}

/// Whether a printed type names `key` as a whole word.
pub(crate) fn names_type(text: &str, key: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(i) = text[from..].find(key) {
        let start = from + i;
        let end = start + key.len();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

        if !(start > 0 && word(bytes[start - 1])) && !(end < bytes.len() && word(bytes[end])) {
            return true;
        }

        from = start + 1;
    }

    false
}

/// The child's member list, with the std's doc on the items a std type
/// declares. A completion item carries the type; the doc says what the
/// member does.
pub(crate) fn attach_std_member_docs(result: &mut Value, doc: &Doc, line: u32, character: u32) {
    let Some(offset) = offset_of(&doc.source, line, character) else {
        return;
    };
    let head = doc.source[..offset].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');

    if !head.ends_with(['.', ':']) {
        return;
    }

    let sigil = head.len() - 1;
    let from = head[..sigil]
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let Some((key, on_type)) = std_receiver(&doc.source, sigil, &head[from..sigil]) else {
        return;
    };
    let items = match result.get_mut("items").and_then(Value::as_array_mut) {
        Some(items) => items,

        None => match result.as_array_mut() {
            Some(items) => items,

            None => return,
        },
    };

    for item in items {
        let Some(label) = item.get("label").and_then(Value::as_str) else {
            continue;
        };
        let Some(m) = alloy::docs::member(key, label) else {
            continue;
        };
        if !alloy::docs::member_fits(m.kind, on_type) {
            continue;
        }

        item["detail"] = json!(m.signature);
        item["documentation"] = json!({
            "kind": "markdown",
            "value": alloy::docs::member_hover(key, m),
        });
    }
}

/// Whether a hover is a type alias to itself, `type Player = Player`.
/// The child writes one for a name it has no definition for.
pub(crate) fn restates_itself(text: &str) -> bool {
    let Some((_, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(inner) = body.trim().strip_suffix("```") else {
        return false;
    };
    let Some(rest) = inner.trim().strip_prefix("type ") else {
        return false;
    };

    match rest.split_once(" = ") {
        Some((head, value)) => head.trim() == value.trim() && !head.contains('\n'),

        None => false,
    }
}

/// The field a struct body's line declares: the name before the colon,
/// past its visibility and its modifier. `None` when the line declares
/// no field.
pub(crate) fn field_key(line: &str) -> Option<&str> {
    let mut text = line.trim();

    for word in ["public ", "private ", "read ", "write "] {
        text = text.strip_prefix(word).unwrap_or(text);
    }

    let name = text.split_once(':')?.0.trim();

    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

/// A parameter of a `remote` declaration. The emit writes the name as a
/// string key of the wire table, and the child answers with its length.
pub(crate) fn remote_parameter_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let after = doc.source[end..].trim_start();

    if !after.starts_with(':') || after.starts_with("::") {
        return None;
    }

    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let line = doc.source[line_start..line_end].trim();
    let head = line.strip_prefix("export ").unwrap_or(line);
    let rest = head.strip_prefix("remote ")?;
    let rest = rest.strip_prefix("function ").unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    // The parameter as the line writes it, its wire attribute included.
    let from = doc.source[line_start..start].rfind(['(', ','])? + line_start + 1;
    let to = parameter_end(&doc.source, end, line_end);
    let param = doc.source[from..to].trim();

    Some(format!(
        "```alloy\n{param}\n```\nA parameter of `remote {name}`."
    ))
}

/// Whether a hover is the byte length of a string, `string (5 bytes)`.
pub(crate) fn is_byte_count(text: &str) -> bool {
    let Some((_, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(inner) = body.trim().strip_suffix("```") else {
        return false;
    };
    let Some(rest) = inner.trim().strip_prefix("string (") else {
        return false;
    };

    match rest
        .strip_suffix(" bytes)")
        .or_else(|| rest.strip_suffix(" byte)"))
    {
        Some(count) => !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()),

        None => false,
    }
}

/// Whether the position sits on a name outside every string literal of
/// its line. The emit turns such a name into a key, and the child then
/// answers about the key's own text.
pub(crate) fn names_a_key(doc: &Doc, line: u32, character: u32) -> bool {
    let Some(offset) = offset_of(&doc.source, line, character) else {
        return false;
    };

    if !keywords::is_word_at(&doc.source, offset) {
        return false;
    }

    let (start, _) = keywords::word_range(&doc.source, offset);
    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let head = &doc.source[line_start..start];

    head.matches('"').count() % 2 == 0 && head.matches('\'').count() % 2 == 0
}

/// The hover of a struct field at its declaration: the line as written,
/// under the struct it belongs to, with the comment above it. `None`
/// when the word is not a field of a struct body.
/// Whether the `:` at an offset closes a ternary: a `?` stands before
/// it on the same line, outside every string. `x: T` writes a type
/// with the same byte.
pub(crate) fn ternary_else_at(source: &str, at: usize) -> bool {
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut quote: Option<char> = None;

    for c in source[line_start..at].chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,

            (Some(_), _) => {}

            (None, '"' | '\'' | '`') => quote = Some(c),

            (None, '?') => return true,

            _ => {}
        }
    }

    false
}

/// The three parts of a function head after its name: the type
/// parameter list, the parameter list, and the return type. The first
/// two carry their brackets; the return is the type alone.
pub(crate) struct Head {
    generics: Option<(usize, usize)>,
    params: (usize, usize),
    ret: Option<(usize, usize)>,
}

/// Reads `<A, B>(p: T): R` from `at`, the byte just past a function's
/// name. `None` when no parameter list follows.
pub(crate) fn head_spans(text: &str, at: usize) -> Option<Head> {
    let mut i = at + text[at..].len() - text[at..].trim_start().len();
    let generics = match text[i..].starts_with('<') {
        true => {
            let len = angle_len(&text[i..])?;
            let span = (i, i + len);
            i += len;

            Some(span)
        }

        false => None,
    };
    i += text[i..].len() - text[i..].trim_start().len();

    if !text[i..].starts_with('(') {
        return None;
    }

    let len = group_len(&text[i..], '(', ')')?;
    let params = (i, i + len);
    i += len;
    let after = text[i..].trim_start();
    let skip = text[i..].len() - after.len();
    let ret = match (after.strip_prefix("->"), after.strip_prefix(':')) {
        (Some(r), _) | (_, Some(r)) => {
            let mark = after.len() - r.len();
            let body = r.trim_start();
            let start = i + skip + mark + (r.len() - body.len());
            let end = body.find('\n').map_or(text.len(), |k| start + k);

            (end > start).then_some((start, end))
        }

        _ => None,
    };

    Some(Head {
        generics,
        params,
        ret,
    })
}

/// The length of the `<...>` a text opens with. An arrow's `>` closes
/// no bracket.
pub(crate) fn angle_len(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut last = ' ';

    for (k, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' if last != '-' => {
                depth -= 1;

                if depth == 0 {
                    return Some(k + 1);
                }
            }
            '\n' => return None,
            _ => {}
        }

        last = c;
    }

    None
}

/// The length of the `open ... close` group a text starts with.
pub(crate) fn group_len(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;

    for (k, c) in text.char_indices() {
        match c {
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;

                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }

    None
}

/// The names a parameter list binds, in order, and whether each one
/// carries a type. `(a: T, b)` gives `[("a", true), ("b", false)]`.
pub(crate) fn parameter_names(list: &str) -> Vec<(String, bool)> {
    let inner = list.trim().trim_start_matches('(').trim_end_matches(')');
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();

    for (k, c) in inner.char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&inner[start..k]);
                start = k + 1;
            }
            _ => {}
        }
    }

    parts.push(&inner[start..]);

    for part in parts {
        let text = part.trim();

        if text.is_empty() {
            continue;
        }

        // A wire attribute stands in front of the name.
        let text = match text.starts_with('@') {
            true => text.split_once(' ').map_or("", |(_, r)| r).trim(),

            false => text,
        };
        let name: String = text
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        out.push((name, text[..].contains(':')));
    }

    out
}

/// The head of the declaration of `name`, from the name onward, and
/// whether it is `async`. `None` when no source in reach declares the
/// name exactly once: two declarations name two things.
pub(crate) fn declaration_head<'a>(doc: &'a Doc, name: &str) -> Option<(&'a str, bool)> {
    let mut found: Option<(&'a str, bool)> = None;

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        for line in src.lines() {
            let text = line.trim();
            let head = text.strip_prefix("export ").unwrap_or(text);
            let head = head.strip_prefix("local ").unwrap_or(head);
            let (head, is_async) = match head.strip_prefix("async ") {
                Some(rest) => (rest, true),

                None => (head, false),
            };
            let Some(rest) = head.strip_prefix("function ") else {
                continue;
            };

            if !rest.starts_with(name) {
                continue;
            }

            let after = &rest[name.len()..];

            if !after.starts_with('<') && !after.starts_with('(') {
                continue;
            }

            if found.is_some() {
                return None;
            }

            found = Some((rest, is_async));
        }
    }

    found
}

/// The head of `function name` inside `impl Owner`, from the name
/// onward. A method name repeats across impls, so the owner picks one.
pub(crate) fn impl_method_head<'a>(doc: &'a Doc, owner: &str, name: &str) -> Option<&'a str> {
    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        let mut inside = false;

        for line in src.lines() {
            let text = line.trim();

            if let Some(rest) = text.strip_prefix("impl ") {
                let named = rest.split(" for ").last().unwrap_or(rest).trim();
                inside = named
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .eq(owner.chars());

                continue;
            }

            if !line.starts_with([' ', '\t']) && text != "end" {
                inside = false;
            }

            if !inside {
                continue;
            }

            let head = text.strip_prefix("private ").unwrap_or(text);
            let Some(rest) = head.strip_prefix("function ") else {
                continue;
            };

            if rest.starts_with(name)
                && matches!(rest[name.len()..].chars().next(), Some('(' | '<'))
            {
                return Some(rest);
            }
        }
    }

    None
}

/// The signature the source wrote, in place of the one the checker
/// printed. A bound leaves the type parameter list and joins every use
/// of the parameter as an intersection, and a union is reordered, so
/// the print says less than the line the reader is looking at.
pub(crate) fn declared_signature(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = doc.source[start..end].to_string();
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;

    if body.contains('\n') || !body.contains("function ") {
        return None;
    }

    let name_end = name_end_in(body, &word)?;
    let child = head_spans(body, name_end)?;
    let owner = printed_owner(&body[..name_end], &word);
    let (source, is_async) = match owner
        .as_deref()
        .and_then(|o| impl_method_head(doc, o, &word))
    {
        Some(head) => (head, false),

        None => declaration_head(doc, &word)?,
    };
    let src = head_spans(source, word.len())?;
    let mut out = body[..name_end].to_string();

    // The source's own parameters go in when every one of them carries
    // a type and the names line up: a print with more names is another
    // function of the same name.
    let src_params = parameter_names(&source[src.params.0..src.params.1]);
    let child_params = parameter_names(&body[child.params.0..child.params.1]);
    let same = src_params.len() == child_params.len()
        && src_params
            .iter()
            .zip(&child_params)
            .all(|(a, b)| a.0 == b.0 && a.1);
    let generics = match (same, src.generics, child.generics) {
        (true, Some((a, b)), _) => Some(&source[a..b]),

        (_, _, Some((a, b))) => Some(&body[a..b]),

        _ => None,
    };

    if let Some(g) = generics {
        out.push_str(g);
    }

    out.push_str(match same {
        true => &source[src.params.0..src.params.1],

        false => &body[child.params.0..child.params.1],
    });

    let ret = src
        .ret
        .map(|(a, b)| match is_async {
            true => format!("Future<{}>", source[a..b].trim()),

            false => source[a..b].trim().to_string(),
        })
        .or_else(|| child.ret.map(|(a, b)| body[a..b].trim().to_string()));

    if let Some(ret) = ret {
        out.push_str(": ");
        out.push_str(&ret);
    }

    (out != body).then(|| format!("{fence}\n{out}\n```{tail}"))
}

/// The byte past the last occurrence of a function's name in a printed
/// head: the one a `<` or a `(` follows.
pub(crate) fn name_end_in(body: &str, word: &str) -> Option<usize> {
    let mut found = None;
    let mut from = 0;

    while let Some(i) = body[from..].find(word) {
        let at = from + i;
        let end = at + word.len();
        let bounded = at == 0 || !keywords::is_word_at(body, at - 1);

        if bounded && matches!(body[end..].chars().next(), Some('<' | '(')) {
            found = Some(end);
        }

        from = end;
    }

    found
}

/// The type a printed head hangs a method off: `function Item:add`
/// gives `Item`. `None` when the head names the function alone.
pub(crate) fn printed_owner(head: &str, word: &str) -> Option<String> {
    let rest = head.strip_suffix(word)?;
    let rest = rest.strip_suffix(['.', ':'])?;
    let name: String = rest
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    (!name.is_empty()).then(|| name.chars().rev().collect())
}

/// Luau prints a struct's value type by the name of its metatable, so a
/// generic struct loses the arguments it was given: `Slotted<T>` reads
/// `Slotted`, which no source can write. A signature that declares the
/// same parameters names them, and so does the `impl` above a `self`.
pub(crate) fn restore_struct_arguments(value: &str, doc: &Doc, line: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;

    if body.contains('\n') {
        return None;
    }

    // `local self: Slotted` inside `impl Slotted<T>`. A foreign impl,
    // `impl string`, types its untyped `self` from the body, so the
    // print is `any`; the `impl` head says what it is.
    if let Some((head, printed)) = body.rsplit_once(": ")
        && head.trim_end().ends_with("self")
        && let Some(named) = impl_self_type(doc, line)
        && (matches!(printed, "any" | "any?" | "unknown" | "unknown?")
            || (named.starts_with(printed) && named.len() > printed.len()))
    {
        return Some(format!("{fence}\n{head}: {named}\n```{tail}"));
    }

    let open = body.find('(')?;
    let scope = declared_type_parameters(&body[..open]);
    let rebuilt = with_struct_arguments(&body[open..], doc, &scope);

    (rebuilt != body[open..]).then(|| format!("{fence}\n{}{rebuilt}\n```{tail}", &body[..open]))
}

/// The text with every bare generic struct name given the parameters
/// the struct declares, when the scope holds all of them.
pub(crate) fn with_struct_arguments(text: &str, doc: &Doc, scope: &HashSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at] as char).is_alphanumeric() && bytes[at] != b'_' {
            out.push(bytes[at] as char);
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && ((bytes[at] as char).is_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }

        let word = &text[start..at];
        out.push_str(word);

        if text[at..].starts_with('<') {
            continue;
        }

        let arguments = std::iter::once(&doc.source)
            .chain(doc.import_sources.iter())
            .find_map(|src| {
                let text = struct_generics(src, word);

                (!text.is_empty()).then_some(text)
            });

        if let Some(arguments) = arguments
            && arguments
                .trim_matches(['<', '>'])
                .split(',')
                .all(|p| scope.contains(p.trim()))
        {
            out.push_str(&arguments);
        }
    }

    out
}

/// A method's receiver, when the print names the variable the call
/// went through and types it `any`. The trait or the `impl` that
/// declares the method names the type, and a std method on a primitive
/// carries it in its own first parameter.
pub(crate) fn name_method_receiver(value: &str, doc: &Doc, line: u32) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let head = body.strip_prefix("function ")?;
    let open = head.find('(')?;
    let (recv, name) = head[..open].rsplit_once([':', '.'])?;
    let plain = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_');

    if !plain(recv) || !plain(name) {
        return None;
    }

    let inner = &head[open + 1..];
    let first = inner.split([',', ')']).next().unwrap_or("").trim();
    let declared = first.strip_prefix("self:").unwrap_or(first).trim();
    // A receiver that starts upper case is a type already.
    let is_type = recv.starts_with(|c: char| c.is_ascii_uppercase());
    let owner = match (is_type, declared) {
        // `function Bag:is_empty(self: any)`: the head has the type.
        (true, "any") => recv.to_string(),

        (true, _) => return None,

        // `function v:upper(string)`: the parameter carries it.
        (false, d) if !d.is_empty() && d != "any" && plain(d) => d.to_string(),

        (false, _) => trait_of_method(doc, name).or_else(|| {
            (recv == "self")
                .then(|| impl_self_type(doc, line))
                .flatten()
        })?,
    };
    let rebuilt = format!(
        "function {owner}{}(self: {owner}{}",
        &head[recv.len()..open],
        &inner[first.len()..]
    );

    (rebuilt != body).then(|| format!("{fence}\n{rebuilt}\n```{tail}"))
}

/// The trait that declares a method, when one in reach does and no
/// other does.
pub(crate) fn trait_of_method(doc: &Doc, method: &str) -> Option<String> {
    let mut found: Option<String> = None;

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        let mut owner: Option<String> = None;

        for line in src.lines() {
            let text = line.trim();
            let head = text.strip_prefix("export ").unwrap_or(text);

            if let Some(rest) = head.strip_prefix("trait ") {
                owner = Some(
                    rest.chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect(),
                );

                continue;
            }

            if !line.starts_with([' ', '\t']) && text != "end" {
                owner = None;
            }

            let Some(name) = owner.as_ref() else {
                continue;
            };
            let Some(rest) = text.strip_prefix("function ") else {
                continue;
            };

            if rest.starts_with(method) && rest[method.len()..].starts_with('(') {
                match &found {
                    Some(other) if other != name => return None,

                    _ => found = Some(name.clone()),
                }
            }
        }
    }

    found
}

/// A trait's required method has no body, so the emit binds it as a
/// value and the print has no name for it. The trait names it.
pub(crate) fn name_trait_method(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let head = body.strip_prefix("function (")?;
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let owner = trait_of_method(doc, word)?;
    let rebuilt =
        format!("function {owner}.{word}({head}").replace("(self: any", &format!("(self: {owner}"));

    Some(format!("{fence}\n{rebuilt}\n```{tail}"))
}

/// A parameter the child prints as a `local`. The two differ in what a
/// reader may do to them, and the source says which this is.
pub(crate) fn unlocal_parameter(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let named = body.strip_prefix("local ")?;
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    if !named.starts_with(word) || !named[word.len()..].starts_with(':') {
        return None;
    }

    // A name the file declares with a keyword is that declaration.
    if doc.bindings.iter().any(|b| b.name == *word) {
        return None;
    }

    doc.source
        .lines()
        .filter_map(|l| {
            let open = l.find('(')?;

            function_name_of(&l[..open]).map(|_| l[open..].to_string())
        })
        .any(|list| parameter_names(&list).iter().any(|(n, _)| n == word))
        .then(|| format!("{fence}\n{named}\n```{tail}"))
}

/// `local rows = checked(ids)`: the child prints a solver variable for
/// the binding. The function the line calls declares what it gives
/// back, and that is the name the reader wrote.
pub(crate) fn name_by_declaration(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let (head, printed) = body.rsplit_once(": ")?;

    if !holds_solver_variable(printed) {
        return None;
    }

    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    if !head.ends_with(word) {
        return None;
    }

    let text = doc.source.lines().nth(line as usize)?;
    let call = text.split_once("= ")?.1.trim_start();
    let path: String = call
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    if !call[path.len()..].starts_with('(') {
        return None;
    }

    let name = path.rsplit('.').next()?;
    let owner = path.strip_suffix(name)?.strip_suffix('.');
    let source = match owner {
        Some(owner) => impl_method_head(doc, owner, name)?,

        None => declaration_head(doc, name)?.0,
    };
    let is_async = owner.is_none() && declaration_head(doc, name).is_some_and(|(_, a)| a);
    let spans = head_spans(source, name.len())?;
    let (a, b) = spans.ret?;
    let ret = source[a..b].trim();
    let scope = spans
        .generics
        .map(|(g, h)| declared_type_parameters(&source[g..h]))
        .unwrap_or_default();

    // A return that names the function's own parameters says nothing
    // about the value the call gave back.
    if scope.iter().any(|p| mentions_word(ret, p)) {
        return None;
    }

    let ret = match is_async {
        true => format!("Future<{ret}>"),

        false => ret.to_string(),
    };

    Some(format!("{fence}\n{head}: {ret}\n```{tail}"))
}

/// Whether a printed type holds a solver variable, `t1`: a name the
/// checker made and no source can write.
pub(crate) fn holds_solver_variable(text: &str) -> bool {
    let bytes = text.as_bytes();

    text.match_indices('t').any(|(i, _)| {
        let before = i == 0 || !keywords::is_word_at(text, i - 1);
        let digits = text[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        let after = i + 1 + digits;

        before && digits > 0 && (after >= bytes.len() || !(bytes[after] as char).is_alphanumeric())
    })
}

/// Whether a type text names a word on its own.
pub(crate) fn mentions_word(text: &str, word: &str) -> bool {
    let mut from = 0;

    while let Some(i) = text[from..].find(word) {
        let at = from + i;
        let end = at + word.len();
        let before = at == 0 || !keywords::is_word_at(text, at - 1);

        if before && !keywords::is_word_at(text, end.saturating_sub(1).max(end)) {
            let next = text[end..].chars().next();

            if next.is_none_or(|c| !c.is_alphanumeric() && c != '_') {
                return true;
            }
        }

        from = end;
    }

    false
}

/// A bound leaves the type parameter list at emit and joins every use
/// of the parameter as an intersection. The reader wrote neither, so
/// `Priced & T` reads as `T` when the enclosing head bounds `T`.
pub(crate) fn drop_bound_intersections(value: &str, doc: &Doc) -> Option<String> {
    let mut out = value.to_string();

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        for line in src.lines() {
            let Some((_, _, params)) = declared_type_parameters_of(line) else {
                continue;
            };

            for param in params {
                let Some((name, bound)) = param.split_once(':') else {
                    continue;
                };
                let (name, bound) = (name.trim(), bound.trim());

                if name.is_empty() || bound.is_empty() {
                    continue;
                }

                out = out
                    .replace(&format!("({bound} & {name})"), name)
                    .replace(&format!("({name} & {bound})"), name)
                    .replace(&format!("{bound} & {name}"), name)
                    .replace(&format!("{name} & {bound}"), name);
            }
        }
    }

    (out != value).then_some(out)
}

/// The hover of a method an `impl` writes on a foreign type. The emit
/// hangs it off a helper table the child has no name for, so the child
/// answers `any`; the source says what it is.
pub(crate) fn foreign_method_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let word = &doc.source[start..end];
    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let lead = doc.source[line_start..start].trim();
    let lead = lead.strip_prefix("private ").unwrap_or(lead);

    if lead != "function" || !doc.source[end..].starts_with(['(', '<']) {
        return None;
    }

    let (line, _) = position_of(&doc.source, start);
    let owner = impl_self_type(doc, line)?;
    let base = owner.split('<').next().unwrap_or(&owner);
    let declared = |name: &str| {
        doc.shapes
            .iter()
            .chain(doc.import_shapes.iter())
            .any(|s| match s {
                alloy::declarations::Shape::Struct { name: n, .. }
                | alloy::declarations::Shape::Enum { name: n, .. } => n == name,

                _ => false,
            })
    };

    // A struct or an enum the file declares reads through the child,
    // which types its methods from the class table.
    if declared(base) {
        return None;
    }

    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let rest = doc.source[end..line_end].trim_end();
    // An untyped `self` is the type the `impl` names.
    let rest = match rest.starts_with("(self)") || rest.starts_with("(self,") {
        true => rest.replacen("(self", &format!("(self: {owner}"), 1),

        false => rest.to_string(),
    };
    let rest = rest.replacen(" -> ", ": ", 1);

    Some(format!("```alloy\nfunction {owner}.{word}{rest}\n```"))
}

/// The hover of a function parameter at its declaration: the parameter
/// as the line writes it, under the function it belongs to. A parameter
/// is not a `local`, and the child has no other word for it.
pub(crate) fn declared_parameter_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let after = doc.source[end..].trim_start();

    // `function trim(self)` inside `impl string`: the receiver carries
    // no type, and the child reads one off the body.
    if &doc.source[start..end] == "self"
        && doc.source[..start].trim_end().ends_with('(')
        && (after.starts_with(')') || after.starts_with(','))
    {
        let (line, _) = position_of(&doc.source, start);
        let named = impl_self_type(doc, line)?;
        let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
        let owner = function_name_of(
            doc.source[line_start..start]
                .trim_end()
                .trim_end_matches('('),
        )?;

        return Some(format!(
            "```alloy\nself: {named}\n```\nA parameter of `function {owner}`."
        ));
    }

    if !after.starts_with(':') || after.starts_with("::") {
        return None;
    }

    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let line = doc.source[line_start..line_end].trim();
    // The name stands right after the `(` or the `,` of a parameter list.
    let open = doc.source[line_start..start].rfind(['(', ','])? + line_start;
    let lead = doc.source[line_start..open].trim_end();

    if !doc.source[open + 1..start]
        .trim()
        .trim_start_matches(|c: char| c == '@' || c.is_alphanumeric() || c == '_')
        .is_empty()
    {
        return None;
    }

    let owner = function_name_of(lead)?;
    let to = parameter_end(&doc.source, end, line_end);
    let param = doc.source[open + 1..to].trim();
    let head = match line.starts_with("function ") || line.contains(" function ") {
        true => format!("function {owner}"),

        false => owner,
    };

    Some(format!("```alloy\n{param}\n```\nA parameter of `{head}`."))
}

/// Where a parameter ends: the `,` or `)` that closes it at bracket
/// depth zero. A record type, `{ a: number, b: string }`, holds commas
/// of its own.
pub(crate) fn parameter_end(source: &str, from: usize, line_end: usize) -> usize {
    let mut depth = 0i32;
    let mut last = ' ';

    for (i, c) in source[from..line_end].char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            // The `>` of an arrow closes no bracket.
            '}' | ']' => depth -= 1,
            '>' if last != '-' => depth -= 1,
            ')' | ',' if depth == 0 => return from + i,
            ')' => depth -= 1,
            _ => {}
        }

        last = c;
    }

    line_end
}

/// The name a function declaration head names, for the text that runs
/// up to its `(`: `local function key_for` gives `key_for`. `None` when
/// the text is no function head.
pub(crate) fn function_name_of(lead: &str) -> Option<String> {
    let head = lead.trim();
    let head = head.strip_prefix("export ").unwrap_or(head);
    let head = head.strip_prefix("local ").unwrap_or(head);
    let head = head.strip_prefix("async ").unwrap_or(head);
    let rest = head.strip_prefix("function ")?;
    // `function get<T>` and `function M.f` both name the function.
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == ':')
        .collect();

    (!name.is_empty()).then_some(name)
}

/// The hover of a type parameter, at its declaration and at every use
/// inside the declaration that binds it. The child has no binding for
/// one, so it answers with the constructor beside it or with nothing.
pub(crate) fn type_parameter_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let word = &doc.source[start..end];

    if word.len() > 2 && !word.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
        return None;
    }

    // A name the file declares is that declaration, not a parameter.
    if doc.decls.iter().any(|d| d.name == word) {
        return None;
    }

    let mut at = 0;
    let mut found = None;

    // The nearest head above the cursor binds the name; an outer head
    // of the same spelling is another parameter.
    for line in doc.source.lines() {
        let line_start = at;
        at += line.len() + 1;

        if line_start > start {
            break;
        }

        let Some((owner, kind, params)) = declared_type_parameters_of(line) else {
            continue;
        };
        let Some(spelling) = params
            .iter()
            .find(|p| p.split(':').next().map(str::trim) == Some(word))
        else {
            continue;
        };

        found = Some(format!(
            "```alloy\n{spelling}\n```\nA type parameter of `{kind} {owner}`."
        ));
    }

    found
}

/// The `<A, B: Bound>` a declaration head writes: what it declares, its
/// name, and the parameters with their bounds.
pub(crate) fn declared_type_parameters_of(
    line: &str,
) -> Option<(String, &'static str, Vec<String>)> {
    let text = line.trim();
    let text = text.strip_prefix("export ").unwrap_or(text);
    let text = text.strip_prefix("local ").unwrap_or(text);
    let text = text.strip_prefix("async ").unwrap_or(text);
    let kinds = [
        "struct",
        "enum",
        "trait",
        "interface",
        "function",
        "impl",
        "type",
    ];
    let kind = kinds
        .into_iter()
        .find(|k| text.strip_prefix(k).is_some_and(|r| r.starts_with(' ')))?;
    let rest = text[kind.len()..].trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let open = rest[name.len()..].strip_prefix('<')?;
    let close = open.find('>')?;
    let params: Vec<String> = open[..close]
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();

    (!name.is_empty() && !params.is_empty()).then_some((name, kind, params))
}

/// The keyword of a declaration that carries fields: `struct`,
/// `interface`, or a `type` whose body is a record. Anything else has no
/// field to name.
pub(crate) fn declared_field_owner(
    decl: &alloy::declarations::Declaration,
) -> Option<&'static str> {
    let line = decl.hover.lines().nth(1)?.trim_start_matches("export ");

    for keyword in ["struct ", "interface ", "type "] {
        if line.starts_with(keyword) {
            let keyword = keyword.trim_end();

            // `type Name = number` names no field.
            if keyword == "type" && !line.contains('{') {
                return None;
            }

            return Some(keyword);
        }
    }

    None
}

pub(crate) fn declared_field_hover(doc: &Doc, start: usize, end: usize) -> Option<String> {
    let after = doc.source[end..].trim_start();

    if !after.starts_with(':') || after.starts_with("::") {
        return None;
    }

    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let lead = doc.source[line_start..start].trim();

    // A wire attribute, `@u16 slot: number`, stands in front of the
    // modifiers; it is part of the field the source wrote.
    if !lead
        .split_whitespace()
        .all(|w| matches!(w, "read" | "write" | "private" | "public") || w.starts_with('@'))
    {
        return None;
    }

    // The nearest record above, still open: a `struct` or an `interface`
    // with no `end` at the margin yet, or a `type` whose braces have not
    // closed. A `type` body carries fields the same way a struct does.
    let owner = doc
        .decls
        .iter()
        .filter(|d| d.offset < start && declared_field_owner(d).is_some())
        .max_by_key(|d| d.offset)?;
    let keyword = declared_field_owner(owner)?;

    if keyword == "type" {
        // The field is inside the alias body, so a brace is still open.
        let body = &doc.source[owner.offset..start];
        let depth = body.matches('{').count() as i64 - body.matches('}').count() as i64;

        if depth <= 0 {
            return None;
        }
    } else if doc.source[owner.offset..start].lines().any(|l| l == "end") {
        return None;
    }

    let line_end = doc.source[start..]
        .find('\n')
        .map_or(doc.source.len(), |i| start + i);
    let field_line = doc.source[line_start..line_end]
        .trim()
        .trim_end_matches(',')
        .trim_end();
    let mut out = format!(
        "```alloy\n{field_line}\n```\nA field of `{keyword} {}`.",
        owner.name
    );

    if let Some(comment) = alloy::declarations::doc_before(&doc.source, line_start) {
        out.push_str("\n\n");
        out.push_str(&comment);
    }

    Some(out)
}

/// Whether `offset` sits in a name a declaring keyword introduces: the
/// word before the one at the cursor is `enum`, `struct`, `function`,
/// `local`, and the rest. The name is the author's, so no list belongs
/// there, at the first column of the name as much as mid-word.
///
/// `impl` and `class` take a type, not a new name, and an `import`
/// names nothing of its own but the alias of `* as M`.
pub(crate) fn declares_a_name_at(source: &str, offset: usize) -> bool {
    const DECLARERS: &[&str] = &[
        "enum",
        "struct",
        "trait",
        "interface",
        "type",
        "function",
        "local",
        "const",
        "macro",
        "attribute",
        "remote",
    ];
    let offset = offset.min(source.len());
    let bytes = source.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = offset;

    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }

    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let statement = source[line_start..offset].trim_start();
    let import = statement
        .strip_prefix("import")
        .is_some_and(|rest| !rest.starts_with(|c: char| is_word(c as u8)));

    // Every name in an `import` comes from the module; the alias of
    // `* as M` and a default binding are the author's own.
    if import {
        let head = source[line_start..start].trim_end();

        if head
            .strip_suffix("as")
            .is_some_and(|h| h.trim_end().ends_with('*'))
        {
            return true;
        }

        let mut word_end = start;

        while word_end < bytes.len() && is_word(bytes[word_end]) {
            word_end += 1;
        }

        let after_keyword = head
            .trim_start()
            .strip_prefix("import")
            .map(str::trim)
            .unwrap_or("-");

        return after_keyword.is_empty()
            && word_end > start
            && source[word_end..].trim_start().starts_with("from");
    }

    let mut end = start;

    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }

    if end == start {
        return false;
    }

    let mut word_start = end;

    while word_start > 0 && is_word(bytes[word_start - 1]) {
        word_start -= 1;
    }

    DECLARERS.contains(&&source[word_start..end])
}

/// What a built-in attribute goes on.
pub(crate) fn builtin_attribute_targets(key: &str) -> &'static [&'static str] {
    match key {
        "@derive" => &["struct", "enum"],

        "@cfg" => &["function", "local"],

        "@test" | "@native" | "@checked" | "@deprecated" | "@inline" | "@noinline" => &["function"],

        "@unreliable" | "@ratelimit" | "@timeout" | "@validate" => &["remote"],

        "@u8" | "@u16" | "@u32" | "@i8" | "@i16" | "@i32" | "@f32" => &["param", "field"],

        "@rename" | "@skip" => &["field"],

        _ => &[
            "function",
            "struct",
            "enum",
            "variant",
            "field",
            "param",
            "remote",
            "interface",
            "type",
        ],
    }
}

/// The targets a declared attribute's hover names:
/// `**Applies to** \`field\` · \`struct\``.
pub(crate) fn declared_attribute_targets(hover: &str) -> Vec<&str> {
    hover
        .lines()
        .find_map(|l| l.strip_prefix("**Applies to** "))
        .map(|list| {
            list.split(" · ")
                .map(|t| t.trim().trim_matches('`'))
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The width of a line, in UTF-16 units.
/// The zero-based UTF-16 offset a range carries as a one-based byte
/// column, which is what the compiler's rewrites read.
pub(crate) fn byte_column(doc: &Doc, line: u32, offset: u32) -> usize {
    let Some(text) = doc.source.lines().nth(line as usize) else {
        return offset as usize + 1;
    };
    let mut units = 0;

    for (at, c) in text.char_indices() {
        if units >= offset {
            return at + 1;
        }

        units += c.len_utf16() as u32;
    }

    text.len() + 1
}

/// A one-based byte column of a line as the zero-based UTF-16 offset a
/// range carries.
pub(crate) fn utf16_column(doc: &Doc, line: u32, col: usize) -> u32 {
    doc.source
        .lines()
        .nth(line as usize)
        .and_then(|l| l.get(..col.saturating_sub(1)))
        .map(|head| head.encode_utf16().count() as u32)
        .unwrap_or(col.saturating_sub(1) as u32)
}

/// The width of the name that starts at a UTF-16 offset, in UTF-16
/// units; a range with no name under it stays one unit wide.
pub(crate) fn word_width(doc: &Doc, line: u32, start: u32) -> u32 {
    let width = doc
        .source
        .lines()
        .nth(line as usize)
        .map(|l| {
            l.chars()
                .skip(start as usize)
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .map(|c| c.len_utf16() as u32)
                .sum::<u32>()
        })
        .unwrap_or(0);

    width.max(1)
}

pub(crate) fn impl_width(doc: &Doc, line: u32) -> u32 {
    doc.source
        .lines()
        .nth(line as usize)
        .map(|l| l.trim_end().encode_utf16().count() as u32)
        .unwrap_or(1)
}

/// The struct whose `impl` writes a method, when one alone writes it.
pub(crate) fn method_owner(source: &str, method: &str) -> Option<String> {
    let mut owner: Option<String> = None;
    let mut found: Option<String> = None;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text.strip_prefix("impl ") {
            let named = rest.split(" for ").last().unwrap_or(rest).trim();
            owner = Some(
                named
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect(),
            );

            continue;
        }

        // An impl body is indented; a line at the margin closes it.
        if !line.starts_with([' ', '\t']) && text != "end" {
            owner = None;
        }

        let Some(head) = owner.as_ref() else {
            continue;
        };
        let Some(rest) = text.strip_prefix("function ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name == method && rest[name.len()..].starts_with("(self") {
            match &found {
                Some(other) if other != head => return None,

                _ => found = Some(head.clone()),
            }
        }
    }

    found
}

/// An arity message with the receiver taken out of both counts.
pub(crate) fn without_self(message: &str) -> Option<String> {
    const HEAD: &str = "Function expects ";

    let at = message.find(HEAD)?;
    let rest = &message[at + HEAD.len()..];
    let expects: u32 = rest[..rest.find(' ')?].parse().ok()?;
    let but = message.find("but ")? + "but ".len();
    let tail = message[but..]
        .strip_prefix("only ")
        .unwrap_or(&message[but..]);
    let given: u32 = match tail.starts_with("none") {
        true => 0,

        false => tail[..tail.find(' ')?].parse().ok()?,
    };
    let expects = expects.checked_sub(1)?;
    let given = given.checked_sub(1)?;
    let plural = |n: u32| if n == 1 { "argument" } else { "arguments" };
    let only = if given < expects { "only " } else { "" };
    let verb = if given == 1 { "is" } else { "are" };

    Some(format!(
        "{}{HEAD}{expects} {}, but {only}{given} {verb} specified",
        &message[..at],
        plural(expects),
    ))
}
