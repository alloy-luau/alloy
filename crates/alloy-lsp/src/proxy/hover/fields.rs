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

    let value = format!("```alloy\nfunction {owner}.{word}{rest}\n```");

    Some(name_method_doc(&value, doc).unwrap_or(value))
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

    // `attribute icon(asset: string) on struct`: the emit writes the
    // parameter list, so the child answers about generated text. The
    // declaration answers for itself, the way a function's does.
    let head_text = lead.trim();
    let head_text = head_text.strip_prefix("export ").unwrap_or(head_text);
    let owner = match head_text.strip_prefix("attribute ") {
        Some(name) => format!("attribute {}", name.trim()),

        None => function_name_of(lead)?,
    };
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
    // A namespace member answers under two names at one offset: the
    // path the source writes, `Ns.T`, and the name the emit gives it,
    // `Ns_T`. The reader wrote the path.
    let owner = doc
        .decls
        .iter()
        .filter(|d| d.offset < start && declared_field_owner(d).is_some())
        .max_by_key(|d| (d.offset, d.name.contains('.')))?;
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
pub(crate) fn used_field_owner(st: &State, doc: &Doc, start: usize) -> Option<String> {
    // The word sits after a `.`; a `:` names a method and `..` is the
    // concatenation operator.
    let head = doc.source[..start].trim_end();

    if !head.ends_with('.') || head.ends_with("..") {
        return None;
    }

    receiver_type(st, doc, head.len() - 1)
}

/// The declared return type of a call `f(...)`, for a name the result
/// of a function binds. `None` for a call of a method, of a field, or
/// of a function the file has no header for.
fn call_return(doc: &Doc, init: &str) -> Option<String> {
    let (name, list) = init.trim().split_once('(')?;

    if !list.ends_with(')')
        || name.is_empty()
        || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }

    let (head, _) = super::restyle::declaration_head(doc, name)?;
    let (a, b) = super::restyle::head_spans(head, name.len())?.ret?;

    alloy::docs::type_head(head[a..b].trim())
}

/// The type a receiver holds, for the separator at `at`: the type of the
/// `impl` block around a `self`, else what the receiver's own
/// declaration says. `p.x` and `p:m()` read the same receiver.
pub(crate) fn receiver_type(st: &State, doc: &Doc, at: usize) -> Option<String> {
    // `p?.x` reads the same receiver as `p.x`.
    let receiver_head = doc.source[..at].trim_end();
    let receiver_head = receiver_head.strip_suffix('?').unwrap_or(receiver_head);

    // `s.backpack[1].x` and `list[i].x`: the element of what the index
    // reads. The child answers here too, but not inside an intrinsic's
    // argument, where the emit writes the code a second time.
    if let Some(open) = receiver_head
        .strip_suffix(']')
        .and_then(|inner| inner.rfind('['))
    {
        let base = doc.source[..open].trim_end();
        let base = base.strip_suffix(['?', '!']).unwrap_or(base);

        if !base.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
            return None;
        }

        let (bs, be) = keywords::word_range(&doc.source, base.len() - 1);
        let word = &doc.source[bs..be];
        let declared = match used_field_owner(st, doc, bs) {
            Some(hop) => declared_field_line(st, doc, &hop, word)?
                .1
                .split_once(':')?
                .1
                .trim()
                .to_string(),

            None => match context::declared(&doc.source, bs, word)? {
                context::Declared::Annotation(t) => t,

                context::Declared::Init(_) => return None,
            },
        };
        let element = element_of(&declared)?;
        let named = alloy::docs::type_head(&element)?;

        return Some(named.rsplit('.').next().unwrap_or(&named).to_string());
    }

    if !receiver_head.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }

    let (rs, re) = keywords::word_range(&doc.source, receiver_head.len() - 1);
    let receiver = &doc.source[rs..re];

    // `a.b.c`: the receiver is itself a field of the hop before it, so
    // no binding of the file names it. The hop that holds it says what
    // type it carries, and the walk reads one link at a time.
    //
    // `self` reads the type of the `impl` block around it; any other
    // name reads its annotation or what it starts from.
    let owner = match receiver {
        _ if let Some(hop) = used_field_owner(st, doc, rs) => field_type(st, doc, &hop, receiver)?,

        "self" => {
            let (line, _) = position_of(&doc.source, rs);
            impl_self_type(doc, line)?
        }

        _ => match context::declared(&doc.source, rs, receiver)? {
            context::Declared::Annotation(t) => alloy::docs::type_head(&t)?,

            // `local s = new S { ... }`: the constructor names the type.
            // A namespace member reads `new Ns.S { ... }`, where the
            // type is the last word of the path. `S.new(...)` names it
            // the same way, under a star alias or not: `M.Gadget.new(5)`
            // is a `Gadget`.
            context::Declared::Init(v) => match v.trim().strip_prefix("new ") {
                Some(rest) => rest
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                    .collect(),

                None => match v.trim().split_once(".new(") {
                    Some((path, _))
                        if !path.is_empty()
                            && path
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '.') =>
                    {
                        path.to_string()
                    }

                    // `local c = make_gadget()`: the declared return
                    // of the function names the type.
                    _ => match call_return(doc, &v) {
                        Some(ret) => ret,

                        None => alloy::docs::value_head(&v)?,
                    },
                },
            },
        },
    };
    // `Counter {}` with no `new`: the file does not compile, and the
    // reader still means the struct.
    let owner = owner.split(['<', ' ', '{']).next().unwrap_or(&owner);

    Some(owner.rsplit('.').next().unwrap_or(owner).to_string())
}

/// What indexing a value of a declared type reads: `Item` of `Item[]`,
/// `{ Item }` and `Array<Item>`, and the value type of `HashMap<K, V>`.
pub(crate) fn element_of(ty: &str) -> Option<String> {
    let ty = ty.trim().trim_end_matches(',').trim().trim_end_matches('?');

    if let Some(inner) = ty.strip_suffix("[]") {
        return Some(inner.trim().to_string());
    }

    if let Some(inner) = ty.strip_prefix('{').and_then(|t| t.strip_suffix('}'))
        && !inner.contains(':')
    {
        return Some(inner.trim().to_string());
    }

    let (head, args) = ty.split_once('<')?;
    let args = alloy::shapes::top_level_parts(args.strip_suffix('>')?);

    match head.trim() {
        "Array" => args.first().map(|a| a.trim().to_string()),

        "HashMap" => args.get(1).map(|a| a.trim().to_string()),

        _ => None,
    }
}

/// The line a struct body writes for one field, with the keyword of
/// the declaration that holds it: this file's own, else a module of
/// the workspace.
fn declared_field_line(
    st: &State,
    doc: &Doc,
    owner: &str,
    field: &str,
) -> Option<(&'static str, String, String)> {
    // A namespace member is keyed by the path the source writes,
    // `Ns.T`, and a receiver carries the last word of it alone. A
    // struct of that spelling is the one the reader means, so the walk
    // takes the path only when no name matches whole. The path walk
    // reads the file and its imports alone: two files can each hold a
    // `T` in a namespace, and only an imported one is in reach.
    let read = |exact: bool| {
        let reach = doc.decls.iter().chain(doc.import_decls.iter());
        let workspace = st.docs.values().flat_map(|d| d.decls.iter());

        reach
            .chain(workspace.filter(|_| exact))
            .filter(|d| match exact {
                true => d.name == owner,

                false => d.name.contains('.') && d.name.rsplit('.').next() == Some(owner),
            })
            .find_map(|d| {
                let keyword = ["struct", "interface", "class"]
                    .into_iter()
                    .find(|k| d.hover.contains(&format!("{k} ")))?;
                let line = d
                    .hover
                    .lines()
                    .find(|l| field_key(l) == Some(field))
                    .map(|l| l.trim().trim_end_matches(',').trim_end().to_string())?;

                Some((keyword, line, d.name.clone()))
            })
    };

    read(true).or_else(|| read(false))
}

/// The type one field of a struct holds, as the name a next hop reads
/// off it: `b: B?` gives `B`.
fn field_type(st: &State, doc: &Doc, owner: &str, field: &str) -> Option<String> {
    let (_, line, _) = declared_field_line(st, doc, owner, field)?;
    let named = alloy::docs::type_head(line.split_once(':')?.1.trim())?;

    Some(named.split('<').next().unwrap_or(&named).to_string())
}

/// A field where it is read, `self.secret` or `p.x`. The child answers
/// with the type alone; the declaration carries `private`, the
/// modifiers, and the struct that owns it.
pub(crate) fn used_field_hover(st: &State, doc: &Doc, start: usize, end: usize) -> Option<String> {
    let word = &doc.source[start..end];
    let owner = used_field_owner(st, doc, start)?;
    let (keyword, line, owner) = declared_field_line(st, doc, &owner, word)?;

    let mut out = format!("```alloy\n{line}\n```\nA field of `{keyword} {owner}`.");

    // The comment above the declaration, from the source that holds it:
    // a field of an imported struct says the same at its use.
    if let Some(text) = member_doc(doc, &owner, word) {
        out.push_str("\n\n");
        out.push_str(&text);
    }

    Some(out)
}

/// A local whose hover prints a solver variable, `local b: t2 where t1 =
/// ...`, names nothing a reader wrote. The checker prints a type that
/// holds an imported struct that way. The binding's first value says
/// what it holds: the type its `new` constructs, or the declared type of
/// the field it reads.
pub(crate) fn name_solver_local(
    st: &State,
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let (head, printed) = body.split_once(": ")?;
    let printed = printed.split(" where ").next()?.lines().next()?;

    if head.contains('\n') || !super::restyle::holds_solver_variable(printed) {
        return None;
    }

    let offset = offset_of(&doc.source, line, character)?;
    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    if !head.ends_with(word) {
        return None;
    }

    // The walk reads the whole line, so the cursor may sit on the
    // declaration itself.
    let line_end = doc.source[end..]
        .find('\n')
        .map_or(doc.source.len(), |i| end + i);
    let (line_start, declared) = context::declared_at(&doc.source, line_end, word)?;
    let context::Declared::Init(init) = declared else {
        return None;
    };
    let named = match init.strip_prefix("new ") {
        Some(after) => super::restyle::constructed_type(doc, after)?,

        None => read_field_type(st, doc, line_start, &init)
            .or_else(|| plain_table_alias(doc, &init))?,
    };
    let named = match printed.ends_with('?') && !named.ends_with('?') {
        true => format!("{named}?"),

        false => named,
    };

    Some(format!("{fence}\n{head}: {named}\n```{tail}"))
}

/// `local alias = Provider`, where `Provider` is a plain table of this
/// file. The value is the table itself, so the name is `typeof(Provider)`.
/// The shape folds name a plain table for `self` alone.
fn plain_table_alias(doc: &Doc, init: &str) -> Option<String> {
    doc.tables
        .iter()
        .any(|(name, _)| name == init)
        .then(|| format!("typeof({init})"))
}

/*
A print that names a struct only by its shape: `t2 where t1 = { new: ...
} ; t2 = { @metatable t1, { x: number } }`. The checker writes a struct
that way when its name is out of its reach, a member of a namespace or
one an import brought. The field list names the struct: the one struct
in reach that declares those fields and no others. The file's own
declarations answer before the ones an import brings, and those before
`reach`, the ones the imported modules import. A remote of `net` that
sends `{ Stack }` hands the file a `Stack` that only `net` imports.
*/
pub(crate) fn name_solver_struct(
    value: &str,
    doc: &Doc,
    reach: &[&alloy::declarations::Declaration],
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let (head, printed) = match body.split_once(": ") {
        Some((head, printed)) if !head.contains(['\n', '{']) => (Some(head), printed),

        _ => (None, body),
    };
    let first = printed.split(" where ").next()?.trim();

    if !super::restyle::holds_solver_variable(first) {
        return None;
    }

    // `{ @metatable t1,\n{ x: number } }`: the record after the comma.
    let struct_named = |meta: usize| {
        let comma = meta + printed[meta..].find(',')? + 1;
        let open = comma + printed[comma..].find(|c: char| !c.is_whitespace())?;

        if !printed[open..].starts_with('{') {
            return None;
        }

        let len = super::restyle::group_len(&printed[open..], '{', '}')?;
        let mut fields: Vec<String> = context::record_entries(&printed[open..open + len])
            .into_iter()
            .map(|f| f.name)
            .collect();
        fields.sort();

        let matches = |d: &&alloy::declarations::Declaration| {
            let mut own: Vec<String> = context::record_entries(&d.hover)
                .into_iter()
                .map(|f| f.name)
                .collect();
            own.sort();

            d.hover.contains("struct ") && own == fields
        };

        // A namespace member is indexed twice, under its path and under
        // the flat name the emit gives it; the two share one hover.
        [
            doc.decls.iter().collect(),
            doc.import_decls.iter().collect(),
            reach.to_vec(),
        ]
        .into_iter()
        .find_map(|decls: Vec<&alloy::declarations::Declaration>| {
            let hits: Vec<&alloy::declarations::Declaration> =
                decls.into_iter().filter(matches).collect();
            let first = hits.first()?;
            let path = hits.iter().find(|d| d.name.contains('.')).unwrap_or(first);

            hits.iter()
                .all(|d| d.hover == first.hover)
                .then(|| path.name.clone())
        })
    };
    // The variable a metatable pair binds, `t2` of `t2 = { @metatable
    // t1, ... }`, stands for the struct wherever the print reads it:
    // `{t2}?` is `{Part}?`. A clause can bind several: an enum field of
    // the struct binds its own variable before the struct's.
    let mut named_first = first.to_string();

    for (meta, _) in printed.match_indices("@metatable ") {
        let Some(bound) = printed[..meta]
            .trim_end()
            .strip_suffix('{')
            .and_then(|b| b.trim_end().strip_suffix('='))
            .map(str::trim_end)
        else {
            continue;
        };
        let var = &bound[bound
            .rfind(|c: char| !c.is_alphanumeric())
            .map_or(0, |i| i + 1)..];

        if var.is_empty() || keywords::find_word(&named_first, var).is_none() {
            continue;
        }

        let Some(named) = struct_named(meta) else {
            continue;
        };
        let mut out = String::new();
        let mut rest = named_first.as_str();

        while let Some(at) = keywords::find_word(rest, var) {
            out.push_str(&rest[..at]);
            out.push_str(&named);
            rest = &rest[at + var.len()..];
        }

        out.push_str(rest);
        named_first = out;
    }

    if super::restyle::holds_solver_variable(&named_first) {
        return None;
    }

    Some(match head {
        Some(head) => format!("{fence}\n{head}: {named_first}\n```{tail}"),

        None => format!("{fence}\n{named_first}\n```{tail}"),
    })
}

/// The declared type of the field a chain reads, `save?.inventory.slots`
/// on the line that starts at `line_start`. A `?.` makes it optional.
fn read_field_type(st: &State, doc: &Doc, line_start: usize, chain: &str) -> Option<String> {
    if !chain
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '?'))
    {
        return None;
    }

    let line_end = doc.source[line_start..]
        .find('\n')
        .map_or(doc.source.len(), |i| line_start + i);
    let at = line_start + doc.source[line_start..line_end].rfind(chain)?;
    let last = at + chain.rfind('.')? + 1;
    let owner = used_field_owner(st, doc, last)?;
    let field = &doc.source[last..at + chain.len()];
    let (_, declaration, _) = declared_field_line(st, doc, &owner, field)?;
    let ty = declaration.split_once(':')?.1.trim();

    Some(match chain.contains("?.") && !ty.ends_with('?') {
        true => format!("{ty}?"),

        false => ty.to_string(),
    })
}

impl Server {
    /// The shadow position of the binding a key of a table literal
    /// belongs to. The child reads the key as a string and answers its
    /// byte count; at the binding it prints the whole record, and the
    /// response takes the key's entry out of it.
    pub(crate) fn literal_key_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let Caret { offset, .. } = Caret::at(&doc.source, line, character)?;
        let (at, _) = literal_key(&doc.source, offset)?;
        let (l, c) = position_of(&doc.source, at);

        Some(doc.to_shadow(l, c))
    }
}

/// The binding a key of a table literal belongs to, and the keys down
/// to it: `strength` in `const S = { stats = { strength = 1 } }` gives
/// the offset of `S` and `["stats", "strength"]`. The literal is the
/// value of a top-level `local` or `const`; a key inside a call's
/// argument or a nested computed key names no entry of the binding.
pub(crate) fn literal_key(source: &str, offset: usize) -> Option<(usize, Vec<String>)> {
    use alloy_syntax::ast::{Expr, Stmt, TableField, TokSpan};

    let parsed = alloy_syntax::parse_lenient(source, Default::default()).ok()?;
    let toks = &parsed.lexed.toks;
    let bytes = |span: TokSpan| {
        let first = toks.get(span.start as usize)?;
        let last = toks.get(span.end.checked_sub(1)? as usize)?;

        Some(first.start as usize..last.end as usize)
    };

    for stmt in &parsed.chunk.block.stmts {
        let Stmt::Local(l) = stmt.under_default() else {
            continue;
        };
        let (Some(binding), Some(Expr::Table { fields, span }), 1) =
            (l.names.first(), l.values.first(), l.names.len())
        else {
            continue;
        };

        if !bytes(*span)?.contains(&offset) {
            continue;
        }

        let at = bytes(binding.name)?.start;
        let mut path = Vec::new();
        let mut fields = fields;

        loop {
            let mut next = None;

            for field in fields {
                let TableField::Named { name, value } = field else {
                    continue;
                };
                let key = source[bytes(*name)?].to_string();

                if bytes(*name)?.contains(&offset) {
                    path.push(key);

                    return Some((at, path));
                }

                if let Expr::Table { fields, span } = value
                    && bytes(*span)?.contains(&offset)
                {
                    next = Some((key, fields));
                }
            }

            let (key, inner) = next?;
            path.push(key);
            fields = inner;
        }
    }

    None
}

/// The keys down to the literal key under the caret, or nothing when
/// the caret is on no such key.
pub(crate) fn literal_key_path(doc: &Doc, line: u32, character: u32) -> Option<Vec<String>> {
    let Caret { offset, .. } = Caret::at(&doc.source, line, character)?;

    literal_key(&doc.source, offset).map(|(_, path)| path)
}

/// One entry of the record a hover prints, by its path of keys:
/// `strength: { default: number, kind: "int" }` out of the print of
/// the whole table. A nested record keeps the child's lines, moved
/// left to the key's own column.
pub(crate) fn record_entry(text: &str, path: &[String]) -> Option<String> {
    let (fence, rest) = text.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let mut record = body.to_string();
    let mut ty = String::new();

    for key in path {
        let field = crate::context::record_entries(&record)
            .into_iter()
            .find(|f| f.name == *key)?;
        ty = field.ty;
        record = ty.clone();
    }

    // The closing brace sits at the key's column: its indent is the
    // one every line after the first moves left by.
    let indent = ty
        .lines()
        .last()
        .map_or(0, |l| l.len() - l.trim_start().len());
    let mut lines = ty.lines();
    let mut out = format!("{}: {}", path.last()?, lines.next()?);

    for line in lines {
        out.push('\n');
        out.push_str(line.get(indent..).unwrap_or(line.trim_start()));
    }

    Some(format!("{fence}\n{out}\n```{tail}"))
}
