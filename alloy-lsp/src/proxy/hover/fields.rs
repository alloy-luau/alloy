use super::*;

impl Server {
    /// A key in a struct's raw constructor, `Menu { button = ... }`, hovers
    /// as the struct's field. The child sees a table key and answers with
    /// the key's string type.
    pub(crate) fn field_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
        if !is_alloy_uri(uri) {
            return false;
        }

        let Some((line, character)) = position_of_message(message) else {
            return false;
        };

        let st = self.state.lock().expect("state");

        let Some(doc) = st.docs.get(uri) else {
            return false;
        };

        let Some(Caret { start, end, .. }) = Caret::at(&doc.source, line, character) else {
            return false;
        };
        let word = &doc.source[start..end];

        // A field where it is declared, `read hp: number = 1` in a struct
        // body: the child sees the constructor's table, where a default
        // makes the field optional, so the declaration answers itself.
        if let Some(answer) = declared_field_hover(doc, start, end)
            .or_else(|| remote_parameter_hover(doc, start, end))
            .or_else(|| declared_parameter_hover(doc, start, end))
            .or_else(|| foreign_method_hover(doc, start, end))
            .or_else(|| type_parameter_hover(doc, start, end))
            .or_else(|| used_field_hover(&st, doc, start, end))
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
        doc.shapes.iter().any(|s| match s {
            alloy::declarations::Shape::Struct { name: n, .. }
            | alloy::declarations::Shape::Enum { name: n, .. } => n == name,

            _ => false,
        })
    };

    // A struct or an enum this file declares reads through the child,
    // which types its methods from the class table it builds here. An
    // imported one carries the module's type, and the methods this file
    // writes are not in it: the child answers `unknown` there.
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

/// The type that owns a field where it is read, `self.secret` or `p.x`:
/// the `impl` block around a `self`, else what the receiver's own
/// declaration says. None when the word sits after no `.`.
pub(crate) fn used_field_owner(doc: &Doc, start: usize) -> Option<String> {
    // The word sits after a `.`; a `:` names a method and `..` is the
    // concatenation operator.
    let head = doc.source[..start].trim_end();

    if !head.ends_with('.') || head.ends_with("..") {
        return None;
    }

    let receiver_end = head.len() - 1;
    let receiver_head = doc.source[..receiver_end].trim_end();

    if !receiver_head.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }

    let (rs, re) = keywords::word_range(&doc.source, receiver_head.len() - 1);
    let receiver = &doc.source[rs..re];
    // `self` reads the type of the `impl` block around it; any other
    // name reads its annotation or what it starts from.
    let owner = match receiver {
        "self" => {
            let (line, _) = position_of(&doc.source, start);
            impl_self_type(doc, line)?
        }

        _ => match context::declared(&doc.source, rs, receiver)? {
            context::Declared::Annotation(t) => alloy::docs::type_head(&t)?,

            // `local s = new S { ... }`: the constructor names the type.
            context::Declared::Init(v) => match v.trim().strip_prefix("new ") {
                Some(rest) => rest
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect(),

                None => alloy::docs::value_head(&v)?,
            },
        },
    };

    Some(owner.split('<').next().unwrap_or(&owner).to_string())
}

/// A field where it is read, `self.secret` or `p.x`. The child answers
/// with the type alone; the declaration carries `private`, the
/// modifiers, and the struct that owns it.
pub(crate) fn used_field_hover(st: &State, doc: &Doc, start: usize, end: usize) -> Option<String> {
    let word = &doc.source[start..end];
    let owner = used_field_owner(doc, start)?;
    // The declaration of the struct, here or in a module this file
    // imports, and the line inside it that declares the field.
    let (keyword, line) = doc
        .decls
        .iter()
        .chain(st.docs.values().flat_map(|d| d.decls.iter()))
        .filter(|d| d.name == owner)
        .find_map(|d| {
            let keyword = ["struct", "interface", "class"]
                .into_iter()
                .find(|k| d.hover.contains(&format!("{k} ")))?;
            let line = d
                .hover
                .lines()
                .find(|l| field_key(l) == Some(word))
                .map(|l| l.trim().trim_end_matches(',').trim_end().to_string())?;

            Some((keyword, line))
        })?;

    Some(format!(
        "```alloy\n{line}\n```\nA field of `{keyword} {owner}`."
    ))
}
