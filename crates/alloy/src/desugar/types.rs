//! Type lowering: type arguments, bounds, and the mapped-type shapes.

use alloy_syntax::ast::{Expr, TokSpan};

use super::*;

/// `HashMap<string, number>` as `("HashMap", "string, number")`, for the
/// std containers whose constructor takes the arguments. Any other
/// annotation is `None`: a `Signal<T...>` takes a pack, which explicit
/// arguments cannot name, and the annotation alone types it.
pub(crate) fn generic_head(ty: &str) -> Option<(String, String)> {
    // `T[]` is `Array<T>`, so the bracket form names the same head.
    let named = array_types(ty);
    let ty = named.trim();
    let open = ty.find('<')?;
    let base = ty[..open].trim();
    // `c.HashMap<K, V>` names the std type through `import * as c`.
    let last = base.rsplit('.').next().unwrap_or(base);

    // `Iter.from` is an overload set, which Luau cannot instantiate by
    // hand; it reads its element type off the source instead.
    if !matches!(
        last,
        "HashMap" | "Set" | "Array" | "Queue" | "Heap" | "Future"
    ) || !ty.ends_with('>')
    {
        return None;
    }

    let args = &ty[open + 1..ty.len() - 1];

    Some((base.to_string(), args.trim().to_string()))
}

/// The type of a literal, for a parameter that takes its value.
pub(crate) fn literal_type(e: &Expr) -> Option<String> {
    match e {
        Expr::Number(_) => Some("number".to_string()),

        Expr::String(_) | Expr::InterpString(_) | Expr::Interp { .. } => Some("string".to_string()),

        Expr::True(_) | Expr::False(_) => Some("boolean".to_string()),

        _ => None,
    }
}

/// Splits a generic list `<A, B>` at its top-level commas.
pub(crate) fn split_generics(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches('<').trim_end_matches('>');
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();

    for c in inner.chars() {
        match c {
            '<' | '(' | '{' => depth += 1,

            '>' | ')' | '}' => depth -= 1,

            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();

                continue;
            }

            _ => {}
        }

        cur.push(c);
    }

    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }

    parts
}

/// The bounds in a generic list: `<T: Shape, U>` gives `T -> Shape`.
pub(crate) fn generic_bounds(text: &str) -> Vec<(String, String)> {
    split_generics(text)
        .into_iter()
        .filter_map(|item| {
            let (name, bound) = item.split_once(':')?;

            Some((name.trim().to_string(), bound.trim().to_string()))
        })
        .collect()
}

/// A generic list without its bounds: `<T: Shape, U>` gives `<T, U>`.
pub(crate) fn strip_bounds(text: &str) -> String {
    format!("<{}>", generic_names(text).join(", "))
}

/// The names a generic list declares: `<T: Shape, U>` gives `T` and `U`.
pub(crate) fn generic_names(text: &str) -> Vec<String> {
    split_generics(text)
        .into_iter()
        .map(|item| match item.split_once(':') {
            Some((name, _)) => name.trim().to_string(),

            None => item,
        })
        .collect()
}

/// Rewrites each bounded generic name in a type to `(T & Bound)`.
pub(crate) fn apply_bounds(ty: &str, bounds: &[(String, String)]) -> String {
    let mut out = String::with_capacity(ty.len() + 16);
    let mut from = 0;

    for (start, end, bound) in bound_spots(ty, bounds) {
        out.push_str(&ty[from..start]);
        out.push_str(&format!("({} & {bound})", &ty[start..end]));
        from = end;
    }

    out.push_str(&ty[from..]);
    out
}

/// Each bounded generic name in a type that takes its bound: the byte
/// range of the name and the bound it takes.
pub(crate) fn bound_spots(ty: &str, bounds: &[(String, String)]) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let bytes = ty.as_bytes();
    let mut i = 0;
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    // The brackets open around the name. A bound reads through a generic
    // argument, `Box<T & Bound>`, and the caller's `Box<Num>` satisfies
    // it. Under a table or an `Array` it does not:
    // `bounded_array_params` casts the element reads instead.
    let mut open: Vec<u8> = Vec::new();

    while i < bytes.len() {
        match bytes[i] {
            // `Array<T>` is a table under another name, so it counts as
            // one: `bounded_array_params` casts the element reads.
            b'<' => open.push(match word_before(ty, i) {
                "Array" => b'{',

                _ => b'<',
            }),
            b'{' => open.push(b'{'),
            b'}' => drop(open.pop()),
            // `>` closes a generic; the one in `->` does not.
            b'>' if i > 0 && bytes[i - 1] != b'-' => drop(open.pop()),
            _ => {}
        }

        // A table is invariant, and so is `Array<T>`: `{ T & Bound }`
        // accepts no concrete argument. A name under one keeps its bound
        // off, wherever it sits.
        let bare = !open.contains(&b'{');

        if bare && is_word(bytes[i]) && (i == 0 || !is_word(bytes[i - 1])) {
            let start = i;

            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }

            let word = &ty[start..i];
            // A field name `T:` or a member `X.T` is not the generic.
            let is_field =
                bytes.get(i).is_some_and(|c| *c == b':') || (start > 0 && bytes[start - 1] == b'.');

            if let Some((_, bound)) = bounds.iter().find(|(n, _)| n == word)
                && !is_field
            {
                out.push((start, i, bound.clone()));
            }
        } else {
            i += 1;
        }
    }

    out
}

/// Rewrites each bare name in a type to the path `path_of` gives it:
/// `Pos[]` inside `namespace Combat` is `Combat.Pos[]`. A field name
/// `pos:` and a member `X.Pos` stay as the source wrote them.
pub(crate) fn qualify_names(ty: &str, path_of: &dyn Fn(&str) -> Option<String>) -> String {
    let bytes = ty.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = String::with_capacity(ty.len());
    let (mut from, mut i) = (0, 0);

    while i < bytes.len() {
        if !is_word(bytes[i]) {
            i += 1;

            continue;
        }

        let start = i;

        while i < bytes.len() && is_word(bytes[i]) {
            i += 1;
        }

        let member = start > 0 && bytes[start - 1] == b'.';
        let field = ty[i..].trim_start().starts_with(':');

        if !member
            && !field
            && let Some(path) = path_of(&ty[start..i])
        {
            out.push_str(&ty[from..start]);
            out.push_str(&path);
            from = i;
        }
    }

    out.push_str(&ty[from..]);

    out
}

/// The word that ends at this byte: the head in front of a `<`.
fn word_before(ty: &str, at: usize) -> &str {
    let bytes = ty.as_bytes();
    let mut start = at;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    &ty[start..at]
}

/// Whether a type text names `name` with arguments other than the ones
/// `whole` spells. A reference with the same arguments is the recursion
/// Luau allows.
pub(crate) fn names_other_args(text: &str, name: &str, whole: &str) -> bool {
    let head = format!("{name}<");
    let mut from = 0;

    while let Some(i) = text[from..].find(&head) {
        let at = from + i;
        let before = text.as_bytes().get(at.wrapping_sub(1));
        let word = at == 0
            || before.is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.'));

        if word && !text[at..].starts_with(whole) {
            return true;
        }

        from = at + head.len();
    }

    false
}

/// The element a parameter's array type names: `T[]` and `Array<T>`
/// both hold `T`. An optional array holds nothing: a nil element read
/// needs the guard, not a cast.
pub(crate) fn array_element(ty: &str) -> Option<&str> {
    let ty = ty.trim();

    if let Some(inner) = ty.strip_suffix("[]") {
        return Some(inner.trim());
    }

    if let Some(inner) = ty.strip_prefix("Array<").and_then(|t| t.strip_suffix('>')) {
        return Some(inner.trim());
    }

    // `{ T }` is Luau's array form. `{ x: number }` and `{ [K]: V }`
    // are records, and a comma makes a tuple of fields.
    let inner = ty.strip_prefix('{')?.strip_suffix('}')?.trim();

    (!inner.is_empty() && !inner.contains([':', ',', '['])).then_some(inner)
}

/// One type with `T[]` written as `Array<T>`, at every depth. The
/// bracket form is Alloy's own; Luau reads the named form alone.
pub(crate) fn array_types(text: &str) -> String {
    let text = text.trim();

    if let Some(inner) = text.strip_suffix('?') {
        return format!("{}?", array_types(inner));
    }

    if let Some(inner) = text.strip_suffix("[]") {
        return format!("Array<{}>", array_types(inner));
    }

    // `Name<A, B>`: each argument takes the same rewrite.
    if let Some(open) = text.find('<')
        && text.ends_with('>')
        && open > 0
    {
        let parts: Vec<String> = split_top_level(&text[open + 1..text.len() - 1], ',')
            .iter()
            .map(|p| array_types(p))
            .collect();

        return format!("{}<{}>", &text[..open], parts.join(", "));
    }

    text.to_string()
}

/// `<<A, B>>` as one type pack, `<<(A, B)>>`. Text already wrapped, or
/// empty, comes back unchanged.
pub(crate) fn pack_type_args(text: &str) -> String {
    let Some(inner) = text
        .strip_prefix("<<")
        .and_then(|t| t.strip_suffix(">>"))
        .map(str::trim)
    else {
        return text.to_string();
    };

    if inner.is_empty() || inner.starts_with('(') {
        return text.to_string();
    }

    format!("<<({inner})>>")
}

/// The type name of a literal argument, for an attribute's parameter
/// type. Anything else reads `None`: only a literal checks here.
pub(crate) fn literal_kind(e: &Expr) -> Option<&'static str> {
    match e {
        Expr::Number(_) => Some("number"),
        Expr::String(_) => Some("string"),
        Expr::True(_) | Expr::False(_) => Some("boolean"),
        _ => None,
    }
}

/// Why a parameter type cannot cross a remote, or `None` when it can.
/// Functions and threads never serialize; a `Future` or `Signal` holds
/// both.
/// The parts of a type text at a separator of depth zero: the members
/// of `{ a: T, b: { c: U } }` at its commas.
pub(crate) fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;

    for (i, c) in text.char_indices() {
        depth += depth_step(text, i, c);

        if c == sep && depth == 0 {
            parts.push(&text[from..i]);
            from = i + c.len_utf8();
        }
    }

    parts.push(&text[from..]);

    parts
}

/// The change of bracket depth at the char `c` at byte `i` of a type
/// text. The `>` of `->` closes nothing.
pub(crate) fn depth_step(text: &str, i: usize, c: char) -> i32 {
    match c {
        '{' | '(' | '<' | '[' => 1,

        '>' if text[..i].ends_with('-') => 0,

        '}' | ')' | '>' | ']' => -1,

        _ => 0,
    }
}

/// The byte offset of `needle` outside every bracket group, or `None`.
pub(crate) fn top_level_find(text: &str, needle: &str) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in text.char_indices() {
        depth += depth_step(text, i, c);

        if depth == 0 && text[i..].starts_with(needle) {
            return Some(i);
        }
    }

    None
}

/// The length of the bracket group that opens at the start of `text`,
/// when it closes inside the text.
pub(crate) fn group_len(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in text.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;

            if depth == 0 {
                return Some(i + c.len_utf8());
            }
        }
    }

    None
}

impl<'s> Desugar<'s> {
    /// The negations of the file, checked once before either artifact
    /// renders: a second `~`, and an operand Luau cannot negate. Luau's
    /// `types.negationof` fails on a table or a function type, and its
    /// report names the lowering, so the compiler says it first.
    pub(crate) fn check_negations(&mut self) {
        let negations: Vec<(TokSpan, TokSpan)> = self
            .type_edits
            .iter()
            .filter_map(|e| match e {
                alloy_syntax::ast::TypeEdit::Negation { tildes, operand } => {
                    Some((*tildes, *operand))
                }

                _ => None,
            })
            .collect();

        for (tildes, operand) in negations {
            let text = self.text_of(operand).trim().to_string();
            let count = tildes.end - tildes.start;

            if count > 1 {
                let written = format!("{}{text}", "~".repeat(count as usize));
                let simple = match count % 2 {
                    0 => text.clone(),

                    _ => format!("~{text}"),
                };
                let message = format!("`{written}` negates twice; write `{simple}`");
                self.diagnose(
                    TokSpan::new(tildes.start as usize, operand.end as usize),
                    &message,
                );

                continue;
            }

            if let Some(kind) = self.unnegatable(&text) {
                let message = format!(
                    "`~{text}` negates a {kind} type, which Luau cannot negate; negate a primitive, a singleton, a class, or a union of them"
                );
                self.diagnose(
                    TokSpan::new(tildes.start as usize, operand.end as usize),
                    &message,
                );
            }
        }
    }

    /// Whether a type span holds a `~`, or names an alias of this file
    /// that holds one.
    pub(crate) fn has_negation(&self, span: TokSpan) -> bool {
        self.type_edits.iter().any(|e| {
            matches!(e, alloy_syntax::ast::TypeEdit::Negation { tildes, operand }
                if tildes.start >= span.start && operand.end <= span.end)
        }) || self.names_negation(self.text_of(span), 0)
    }

    /// Whether a type text names an alias that negates, `depth` aliases
    /// down. The depth stops a cycle.
    fn names_negation(&self, text: &str, depth: u32) -> bool {
        depth < 8
            && text
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter_map(|w| self.alias_values.get(w))
                .any(|v| v.contains('~') || self.names_negation(v, depth + 1))
    }

    /// The kind of an operand that is plainly a table or a function type.
    pub(crate) fn unnegatable(&self, text: &str) -> Option<&'static str> {
        self.unnegatable_in(text, 0)
    }

    /// `unnegatable` through groups, unions, intersections, `?`, and the
    /// aliases of this file, `depth` aliases down.
    fn unnegatable_in(&self, text: &str, depth: u32) -> Option<&'static str> {
        let text = text.trim();

        if top_level_find(text, "->").is_some() {
            return Some("function");
        }

        if text.starts_with('(') && group_len(text, '(', ')') == Some(text.len()) {
            return self.unnegatable_in(&text[1..text.len() - 1], depth);
        }

        let members: Vec<&str> = split_top_level(text, '|')
            .into_iter()
            .flat_map(|m| split_top_level(m, '&'))
            .collect();

        if members.len() > 1 {
            return members.iter().find_map(|m| self.unnegatable_in(m, depth));
        }

        if let Some(t) = text.strip_suffix('?') {
            return self.unnegatable_in(t, depth);
        }

        // An alias names its value; the depth stops a cycle.
        if depth < 8
            && let Some(value) = self.alias_values.get(text)
        {
            return self.unnegatable_in(value, depth + 1);
        }

        let head = text.split('<').next().unwrap_or(text).trim();

        if text.starts_with('{')
            || text.ends_with("[]")
            || matches!(
                head,
                "Array" | "ReadArray" | "WriteArray" | "HashMap" | "Set" | "Future" | "Result"
            )
            || self.structs.contains(head)
        {
            return Some("table");
        }

        None
    }

    /// The std table, marking the file as needing the require.
    pub(crate) fn std(&mut self) -> &'static str {
        self.uses_std = true;

        "__alloy"
    }

    /// The prefix for an ambient type: `__alloy.` in code, nothing in a
    /// definitions file, where the std types are globals.
    pub(crate) fn type_std(&mut self) -> &'static str {
        if self.options.definitions {
            ""
        } else {
            self.uses_std = true;

            "__alloy."
        }
    }

    /// One `<<A, B>>` type-argument list in Luau's own spelling. The
    /// type edits rewrite an annotation, and a list never reaches them,
    /// so every list, written or inferred, comes through here instead.
    /// The ship artifact takes none: the runtime reads no `<<T>>` at
    /// a call, and the check artifact is the one the types serve.
    pub(crate) fn lower_type_args(&mut self, text: &str) -> String {
        if !self.options.check {
            return String::new();
        }

        let trimmed = text.trim();
        let Some(inner) = trimmed
            .strip_prefix("<<")
            .and_then(|t| t.strip_suffix(">>"))
        else {
            return text.to_string();
        };
        let parts: Vec<String> = split_top_level(inner, ',')
            .iter()
            .map(|p| (*p).to_string())
            .collect();
        let parts: Vec<String> = parts.iter().map(|p| self.lower_type(p)).collect();

        format!("<<{}>>", parts.join(", "))
    }

    /// One type as Luau spells it: `T[]` is `Array<T>`, and an ambient
    /// std name takes the runtime prefix. The rewrite reaches every
    /// depth, so a name inside a table, a union, or another list is
    /// written the same way.
    pub(crate) fn lower_type(&mut self, text: &str) -> String {
        let text = text.trim();

        if text.is_empty() {
            return String::new();
        }

        // A function type: the parameters and the result each lower on
        // their own, so `->` is the first split.
        if let Some(i) = top_level_find(text, "->") {
            let left = self.lower_type(&text[..i]);
            let right = self.lower_type(&text[i + 2..]);

            return format!("{left} -> {right}");
        }

        for sep in ['|', '&'] {
            let parts: Vec<String> = split_top_level(text, sep)
                .iter()
                .map(|p| (*p).to_string())
                .collect();

            if parts.len() > 1 {
                let parts: Vec<String> = parts.iter().map(|p| self.lower_type(p)).collect();

                return parts.join(&format!(" {sep} "));
            }
        }

        if let Some(inner) = text.strip_suffix('?') {
            return format!("{}?", self.lower_type(inner));
        }

        if let Some(inner) = text.strip_suffix("[]") {
            if self.options.definitions {
                return format!("{{ {} }}", self.lower_type(inner));
            }

            let std = self.type_std();

            return format!("{std}Array<{}>", self.lower_type(inner));
        }

        if text.starts_with('(')
            && text.ends_with(')')
            && group_len(text, '(', ')') == Some(text.len())
        {
            let parts: Vec<String> = split_top_level(&text[1..text.len() - 1], ',')
                .iter()
                .map(|p| (*p).to_string())
                .collect();
            let parts: Vec<String> = parts.iter().map(|p| self.lower_field(p)).collect();

            return format!("({})", parts.join(", "));
        }

        // A table type: each field keeps its name and lowers its type.
        if text.starts_with('{')
            && text.ends_with('}')
            && group_len(text, '{', '}') == Some(text.len())
        {
            let parts: Vec<String> = split_top_level(&text[1..text.len() - 1], ',')
                .iter()
                .map(|p| (*p).to_string())
                .collect();
            let parts: Vec<String> = parts.iter().map(|p| self.lower_field(p)).collect();

            return format!("{{ {} }}", parts.join(", "));
        }

        // `Name<A, B>`: the head takes the prefix, the arguments the
        // whole rewrite again.
        if let Some(open) = text.find('<')
            && text.ends_with('>')
            && open > 0
        {
            let head = self.lower_type_name(&text[..open]);
            let parts: Vec<String> = split_top_level(&text[open + 1..text.len() - 1], ',')
                .iter()
                .map(|p| (*p).to_string())
                .collect();
            let parts: Vec<String> = parts.iter().map(|p| self.lower_type(p)).collect();

            return format!("{head}<{}>", parts.join(", "));
        }

        if text.starts_with("typeof(") {
            let mut out = String::new();
            let mut at = 0;

            for (s, e, require) in self.typeof_imports(text) {
                out.push_str(&text[at..s as usize]);
                out.push_str(&require);
                at = e as usize;
            }

            out.push_str(&text[at..]);

            return out;
        }

        self.lower_type_name(text)
    }

    /// Each `import('./m')` in the text of a `typeof`, as its byte range
    /// and the value that an import in an expression writes. Luau has no
    /// `import`, and a `typeof` holds an expression.
    pub(crate) fn typeof_imports(&mut self, text: &str) -> Vec<(u32, u32, String)> {
        // A local named `import` is the file's own function.
        if self.is_local("import") {
            return Vec::new();
        }

        let Ok(lexed) = alloy_syntax::lexer::lex(text) else {
            return Vec::new();
        };
        let toks = &lexed.toks;
        let word = |i: usize| toks.get(i).map_or("", |t| t.text(text));
        let string = |i: usize| {
            toks.get(i)
                .is_some_and(|t| matches!(t.kind, alloy_syntax::lexer::TokKind::Str { .. }))
        };

        let found: Vec<(u32, u32, String)> = (0..toks.len())
            .filter(|&i| {
                word(i) == "import"
                    && !matches!(i.checked_sub(1).map(word), Some("." | ":"))
                    && word(i + 1) == "("
                    && string(i + 2)
                    && word(i + 3) == ")"
            })
            .map(|i| {
                let path = self.require_literal(word(i + 2));
                let value = match self.is_plain_module(word(i + 2)) {
                    true => format!("require({path})"),

                    false => format!("__module_value(require({path}))"),
                };

                (toks[i].start, toks[i + 3].end, value)
            })
            .collect();

        if found
            .iter()
            .any(|(_, _, v)| v.starts_with("__module_value"))
        {
            self.uses_module_value = true;
        }

        found
    }

    /// One `name: T` pair of a table type or a parameter list. Text with
    /// no name is a type on its own.
    pub(crate) fn lower_field(&mut self, text: &str) -> String {
        let text = text.trim();

        match top_level_find(text, ":") {
            Some(i) => {
                let ty = self.lower_type(&text[i + 1..]);

                format!("{}: {ty}", text[..i].trim())
            }

            None => self.lower_type(text),
        }
    }

    /// A bare type name, with the runtime prefix when it is an ambient
    /// std name this file does not shadow.
    pub(crate) fn lower_type_name(&mut self, text: &str) -> String {
        let name = text.trim();

        // A namespace's type renders under a name of its own. An
        // annotation reaches that name through the type edits. A
        // type-argument list is text, so the path resolves here.
        if let Some(rendered) = self
            .ns_member_name(name)
            .or_else(|| self.ns_path_name(name))
        {
            return rendered;
        }

        if !AMBIENT_TYPES.contains(&name)
            || self.is_local(name)
            || self.declared_types.contains(name)
        {
            return name.to_string();
        }

        let std = self.type_std();

        format!("{std}{name}")
    }

    /*
    A mapped type compiles to an inline type function call over the
    source's properties. Luau's `index` and `keyof` type functions cannot
    rebuild a table, so the loop runs inside a user-defined type function
    the alias declares once, named after the shape.
    */
    pub(crate) fn mapped_type(
        &mut self,
        key: TokSpan,
        source: TokSpan,
        modifier: Option<TokSpan>,
        optional: bool,
    ) -> String {
        let _ = key;
        let src = self.text_of(source).to_string();
        let kind = match (modifier.map(|m| self.text_of(m)), optional) {
            (Some("read"), _) => "read",

            (Some("write"), _) => "write",

            (None, true) => "optional",

            _ => "same",
        };

        if !self.mapped_used.contains(&kind) {
            self.mapped_used.push(kind);
        }

        format!("__mapped_{kind}<{src}>")
    }

    /// The type function behind one mapped shape, on one line.
    pub(crate) fn mapped_type_function(kind: &str) -> String {
        // A definitions file is checked in strict mode, so every local
        // carries a type.
        let value = match kind {
            "optional" => "types.unionof((v.read or v.write) :: any, types.singleton(nil))",

            _ => "(v.read or v.write) :: any",
        };
        let entry = match kind {
            "read" => "{ read = value }",

            "write" => "{ write = value }",

            _ => "{ read = value, write = value }",
        };

        format!(
            "type function __mapped_{kind}(T) local function collect(t): {{ [any]: any }} if t:is(\"intersection\") then local all = {{}} for _, c in t:components() do for k, v in collect(c) do all[k] = v end end return all end return t:properties() end local props: {{ [any]: any }} = {{}} for k, v in collect(T) do local value: any = {value} props[k] = {entry} end return types.newtable(props) end"
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_type_argument_list_writes_arrays_by_name() {
        // `T[]` is Alloy's spelling; a `<<...>>` list is read as Luau.
        let src = "local g: HashMap<string, number[]> = HashMap.new()\nlocal h: Array<number[][]> = Array.new()\nlocal i: Array<HashMap<string, number[]>> = Array.new()\nlocal j = new Array<<number[]>>()\nprint(g, h, i, j)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("HashMap.new<<string, __alloy.Array<number>>>()"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("Array.new<<__alloy.Array<__alloy.Array<number>>>>()"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("Array.new<<__alloy.HashMap<string, __alloy.Array<number>>>>()"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("Array.new<<__alloy.Array<number>>>()"),
            "{}",
            out.check
        );

        // The annotation may use the bracket form as well.
        let out =
            crate::compile("local b: HashMap<string, number>[] = Array.new()\nprint(b)\n").unwrap();
        assert!(
            out.check
                .contains("Array.new<<__alloy.HashMap<string, number>>>()"),
            "{}",
            out.check
        );
    }

    /// A bound reaches a `T` inside a generic argument: the check
    /// artifact types the parameter `Box<(T & Ord)>`, so the body reads
    /// the bound's members through the struct. An `Array` is invariant,
    /// so such a parameter keeps the bound off and each element read
    /// takes the cast.
    #[test]
    fn a_bound_reaches_a_generic_argument() {
        let head = "trait Ord as\n    function cmp(self, other: Num): number\nend\nstruct Num as\n    v: number\nend\nimpl Ord for Num as\n    function cmp(self, other: Num): number\n        return self.v - other.v\n    end\nend\nstruct Box<T> as\n    v: T\nend\nstruct Pair<T> as\n    a: T\nend\n";
        let src = format!(
            "{head}function maxb<T: Ord>(a: Box<T>, b: Box<T>): number\n    return a.v:cmp(b.v)\nend\nfunction deep<T: Ord>(p: Box<Pair<T>>): number\n    return p.v.a:cmp(p.v.a)\nend\nfunction many<T: Ord>(xs: Box<T>[]): number\n    return xs[1].v:cmp(xs[1].v)\nend\nfunction plain<T: Ord>(xs: T[]): number\n    return xs[1]:cmp(xs[1])\nend\nprint(maxb, deep, many, plain)\n"
        );
        let out = crate::compile(&src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("function maxb<T>(a: Box<(T & Ord)>, b: Box<(T & Ord)>)"),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("function deep<T>(p: Box<Pair<(T & Ord)>>)"),
            "{}",
            out.check
        );
        // The array parameter keeps its written type, and the element
        // read carries the bound.
        assert!(
            out.check
                .contains("function many<T>(xs: __alloy.Array<Box<T>>)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("(xs[1] :: Box<(T & Ord)>).v:cmp("),
            "{}",
            out.check
        );
        assert!(
            out.check
                .contains("function plain<T>(xs: __alloy.Array<T>)"),
            "{}",
            out.check
        );
        assert!(
            out.check.contains("(xs[1] :: (T & Ord)):cmp("),
            "{}",
            out.check
        );
    }

    #[test]
    fn a_written_type_argument_list_leaves_the_ship() {
        // The runtime reads no `<<T>>` at a call. The check artifact
        // keeps the list, and the ship calls without it.
        let src = "struct Box<T> as\n    value: T\nend\nimpl Box<T> as\n    function of<U>(v: U): Box<U>\n        return new Box<<U>> { value = v }\n    end\nend\nfunction ident<T>(x: T): T\n    return x\nend\nlocal M = { f = ident }\nlocal a = ident<<number>>(1)\nlocal b = M.f<<number>>(2)\nlocal c = M:f<<number>>(3)\nlocal d = Box.of<<number>>(9)\nlocal e = ident<<string>> \"x\"\nident<<number>>(7)\nprint(a, b, c, d, e)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(!out.ship.contains("<<"), "{}", out.ship);

        for call in [
            "ident(1)",
            "M.f(2)",
            "M:f(3)",
            "Box.of(9)",
            "ident\"x\"",
            "\nident(7)\n",
        ] {
            assert!(out.ship.contains(call), "{call}: {}", out.ship);
        }

        for call in [
            "ident<<number>>(1)",
            "M.f<<number>>(2)",
            "M:f<<number>>(3)",
            "Box.of<<number>>(9)",
            "ident<<string>>\"x\"",
            "\nident<<number>>(7)\n",
        ] {
            assert!(out.check.contains(call), "{call}: {}", out.check);
        }

        assert_eq!(out.ship.lines().count(), src.lines().count());
    }

    #[test]
    fn a_written_type_argument_list_lowers_like_an_annotation() {
        // A turbofish the author writes holds Alloy spellings, so it
        // takes the same lowering an annotation takes.
        let src = "struct Pair<A, B> as\n    first: A\n    second: B\nend\nlocal a = HashMap.new<<string, number[]>>()\nlocal b = HashMap.new<<string, Pair<number, string[]>[]>>()\nprint(a, b)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(
            out.check
                .contains("HashMap.new<<string, __alloy.Array<number>>>()"),
            "{}",
            out.check
        );
        // The nested list keeps its own arguments, each lowered.
        assert!(
            out.check.contains(
                "HashMap.new<<string, __alloy.Array<Pair<number, __alloy.Array<string>>>>>()"
            ),
            "{}",
            out.check
        );
        assert!(!out.check.contains("[]"), "{}", out.check);
    }

    #[test]
    fn a_namespace_type_lowers_inside_a_type_argument_list() {
        // A type-argument list is text, not an annotation, so the
        // namespace path resolves in the lowering, not in the edits.
        let src = "namespace Ns as\n    enum Kind as\n        A\n        B\n    end\nend\nfunction generic<T>(x: T): T\n    return x\nend\nprint(generic<<Ns.Kind>>(Ns.Kind.A))\n";
        let out = crate::compile(src).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert!(out.check.contains("generic<<Ns_Kind>>("), "{}", out.check);
        assert!(!out.check.contains("<<Ns.Kind>>"), "{}", out.check);
    }

    #[test]
    fn a_shadowed_std_name_keeps_its_own_spelling() {
        // A type the file declares wins over the ambient std name.
        let src = "type Iter = { at: number }\nlocal xs = Array.new<<Iter>>()\nprint(xs)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.check.contains("Array.new<<Iter>>()"), "{}", out.check);
    }
}
