use super::*;

/// The child's hover header with the source's declaring keywords: the
/// `local m: T` of a `const` becomes `const m: T`, and `function f(` of
/// an `async function` becomes `async function f(`. The fence switches
/// to the Alloy grammar, which highlights `const`, `async`, and
/// `export`; the Luau grammar drops the highlight after them. None when
/// the header names something else or the source used the same keyword.
pub(crate) fn restyle_hover(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let word = word_at(doc, line, character)?;
    let binding = doc.bindings.iter().find(|b| b.name == word)?;

    restyle_with(value, word, binding)
}

/// The same, for a name another file of the project declares `global`.
/// The declaration said `global local` or `global const`, and the type
/// the child printed is the one the declaring file infers. `owner` is
/// the file that wrote it.
///
/// The child sees one of two shapes. A `global const` is bound by copy
/// on the first line of the emit, so the child calls it a `local`. A
/// `global local` is one value for the project and every use reads its
/// slot off the declaring module, so the child answers with the type
/// alone and no declaration in front of it.
pub(crate) fn restyle_global_hover(
    value: &str,
    doc: &Doc,
    st: &State,
    uri: &str,
    line: u32,
    character: u32,
) -> Option<String> {
    let word = word_at(doc, line, character)?;

    // A name this file binds itself is this file's own; `restyle_hover`
    // already answered it.
    if doc.bindings.iter().any(|b| b.name == word) {
        return None;
    }

    let owner = super::global_owner(st, uri, word)?;
    let binding = owner
        .bindings
        .iter()
        .find(|b| b.name == word && b.prefix.split(' ').next() == Some("global"))?;

    let styled =
        restyle_with(value, word, binding).or_else(|| bare_type_hover(value, word, binding));

    // Under a mount the child reaches the declaring module by its
    // instance path and may not resolve it, so it prints `unknown`.
    // The declaration itself still says what the name holds.
    match styled.filter(|text| !says_nothing_of_the_type(text)) {
        Some(text) => Some(text),

        None => super::modules::const_hover(&owner.source, word),
    }
}

/// Whether a hover header carries a type that says nothing: the child
/// prints `unknown` for a module it could not read, and `*error-type*`
/// for one it read and could not check.
fn says_nothing_of_the_type(text: &str) -> bool {
    let Some(body) = text
        .strip_prefix("```alloy\n")
        .and_then(|r| r.split_once("\n```"))
        .map(|(body, _)| body)
    else {
        return false;
    };

    matches!(
        body.rsplit_once(": "),
        Some((_, "unknown")) | Some((_, "*error-type*"))
    )
}

/// The declaration in front of a type the child printed on its own.
/// `number` alone says nothing about where the name comes from or
/// whether a file may assign it.
fn bare_type_hover(
    value: &str,
    word: &str,
    binding: &alloy::declarations::Binding,
) -> Option<String> {
    // A function keeps its own header; only a value hovers as a type.
    if binding.prefix.ends_with("function") {
        return None;
    }

    let body = value.strip_prefix("```luau\n")?.strip_suffix("\n```")?;

    // A header the child wrote already names the binding, and this is
    // for the answers that do not.
    if body.starts_with("local ") || body.starts_with("function") || body.starts_with("type ") {
        return None;
    }

    let mut out = format!("```alloy\n{} {word}: {body}\n```", binding.prefix);

    if let Some(doc) = binding.doc.as_deref() {
        out.push_str("\n\n");
        out.push_str(doc);
    }

    Some(out)
}

/// The word the cursor sits on, in the source the author wrote.
fn word_at(doc: &Doc, line: u32, character: u32) -> Option<&str> {
    let offset = offset_of(&doc.source, line, character)?;

    if !keywords::is_word_at(&doc.source, offset) {
        return None;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);

    Some(&doc.source[start..end])
}

/// The child's header with one binding's keywords in front of it.
fn restyle_with(value: &str, word: &str, binding: &alloy::declarations::Binding) -> Option<String> {
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

        let from = text[..i].rfind(['\n', ' ', '(']).unwrap_or(0);

        if tests
            .iter()
            .any(|t| text[from..].starts_with(t) || text[i..].starts_with(t))
        {
            return true;
        }
    }

    false
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

/// Whether a hover invents a type for a name that is not one: the
/// child writes `type undefined_var = unknown` for a word it cannot
/// resolve, and the word is a value the file never declared.
pub(crate) fn invents_a_type(text: &str, doc: &Doc) -> bool {
    let Some((_, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(inner) = body.trim().strip_suffix("```") else {
        return false;
    };
    let Some(rest) = inner.trim().strip_prefix("type ") else {
        return false;
    };
    let Some((head, value)) = rest.split_once(" = ") else {
        return false;
    };
    let name = head.trim();

    if head.contains('\n') || !matches!(value.trim(), "unknown" | "any") {
        return false;
    }

    // The file may declare exactly that alias, and then the hover reads
    // what the author wrote.
    !doc.source.lines().any(|line| {
        let l = line.trim_start();
        let l = l.strip_prefix("export ").unwrap_or(l).trim_start();
        let l = l.strip_prefix("global ").unwrap_or(l).trim_start();

        l.strip_prefix("type ")
            .and_then(|r| r.trim_start().strip_prefix(name))
            .is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    })
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

/// Whether the hover is the closure a lowered block emits, asked for
/// somewhere that is not a name.
///
/// `async do ... end` and `try do ... end` both lower to a closure the
/// source never wrote. The child then answers about that closure at
/// every position the block covers that carries no binding: the `do`,
/// the `end`, and the blank columns between them. The answer names the
/// emit's own parameters, so `try do` reads
/// `function(__fail: (string, string?) -> (...unknown)): number`.
///
/// A name inside the block still answers for itself, and so does a
/// binding that really holds a function. Only the block's own furniture
/// goes quiet.
pub(crate) fn lowers_a_block(text: &str, doc: &Doc, line: u32, character: u32) -> bool {
    let Some((_, body)) = text.split_once('\n') else {
        return false;
    };
    let Some(inner) = body.trim().strip_suffix("```") else {
        return false;
    };

    let inner = inner.trim();

    // The whole answer is one anonymous closure type. A named one, a
    // `type function`, or anything with prose is somebody's real hover.
    if !inner.starts_with("function(") || inner.contains('\n') {
        return false;
    }

    let Some(offset) = offset_of(&doc.source, line, character) else {
        return false;
    };

    if !keywords::is_word_at(&doc.source, offset) {
        return true;
    }

    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];

    !doc.bindings.iter().any(|b| b.name == word)
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

/// Whether a return type already names a Future. `Future<T>` and `T`
/// mean the same answer on an `async function`, so only the second
/// takes a wrapper.
fn names_a_future(declared: &str) -> bool {
    let t = declared.trim();
    let bare = t.rsplit_once('.').map_or(t, |(_, last)| last);

    bare.strip_prefix("Future")
        .is_some_and(|rest| rest.starts_with('<'))
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
        .map(|(a, b)| {
            let text = source[a..b].trim();

            // A header that already names a Future names the answer
            // itself; wrapping it again builds a Future of a Future.
            match is_async && !names_a_future(text) {
                true => format!("Future<{text}>"),

                false => text.to_string(),
            }
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

    // A header may name the answer itself, `async function f(): Future<T>`,
    // or what it settles with, `async function f(): T`. Both mean one
    // thing, so a header that already names a Future is left alone.
    let ret = match is_async && !names_a_future(ret) {
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
