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

    if !matches!(
        base,
        "HashMap" | "Set" | "Array" | "Queue" | "Heap" | "Iter" | "Future"
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
    let names: Vec<String> = split_generics(text)
        .into_iter()
        .map(|item| match item.split_once(':') {
            Some((name, _)) => name.trim().to_string(),

            None => item,
        })
        .collect();

    format!("<{}>", names.join(", "))
}

/// Rewrites each bounded generic name in a type to `(T & Bound)`.
pub(crate) fn apply_bounds(ty: &str, bounds: &[(String, String)]) -> String {
    let mut out = String::with_capacity(ty.len() + 16);
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

            match bounds.iter().find(|(n, _)| n == word) {
                Some((_, bound)) if !is_field => out.push_str(&format!("({word} & {bound})")),

                _ => out.push_str(word),
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

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
        match c {
            '{' | '(' | '<' | '[' => depth += 1,
            '}' | ')' | '>' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&text[from..i]);
                from = i + c.len_utf8();
            }
            _ => {}
        }
    }

    parts.push(&text[from..]);

    parts
}

/// The byte offset of `needle` outside every bracket group, or `None`.
pub(crate) fn top_level_find(text: &str, needle: &str) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in text.char_indices() {
        match c {
            '{' | '(' | '<' | '[' => depth += 1,
            '}' | ')' | '>' | ']' => depth -= 1,
            _ => {}
        }

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
    pub(crate) fn lower_type_args(&mut self, text: &str) -> String {
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

        self.lower_type_name(text)
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
        let head = "trait Ord as\n    function cmp(self, other: Num): number\nend\nstruct Num as\n    v: number\nend\nimpl Ord for Num as\n    function cmp(self, other: Num): number\n        return self.v - other.v\n    end\nend\nstruct Box<T: Ord> as\n    v: T\nend\nstruct Pair<T: Ord> as\n    a: T\nend\n";
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
    fn a_shadowed_std_name_keeps_its_own_spelling() {
        // A type the file declares wins over the ambient std name.
        let src = "type Iter = { at: number }\nlocal xs = Array.new<<Iter>>()\nprint(xs)\n";
        let out = crate::compile(src).unwrap();
        assert!(out.check.contains("Array.new<<Iter>>()"), "{}", out.check);
    }
}
