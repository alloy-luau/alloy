use super::*;

/// The child's hover header with the source's declaring keywords: the
/// `local m: T` of a `const` becomes `const m: T`, and `function f(` of
/// an `async function` becomes `async function f(`. The fence switches
/// to the Alloy grammar, which highlights `const`, `async`, and
/// `export`; the Luau grammar drops the highlight after them. None when
/// the header names something else or the source used the same keyword.
pub(crate) fn restyle_hover(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let word = word_at(doc, line, character)?;
    // A use reads the keywords of the declaration in scope, so a `const
    // bag` and a `local bag` in two functions each keep their own.
    let line = Caret::at(&doc.source, line, character)
        .and_then(|c| alloy::flux::binding_of(&doc.source, c.start))
        .map_or(line, |at| position_of(&doc.source, at).0);
    let binding = declaring_binding(doc, line, word)?;

    restyle_with(value, word, binding)
}

/*
The binding a hover on `word` is about.

One file may bind a name twice with different keywords: `impl Thing as
function new` and `local new = Instance.new` both write `new`, and the
first one found used to restyle the second. The line the cursor sits on
decides when that line declares the name itself. Otherwise the keyword
has to read the same at every binding of the name, or the hover keeps
what the child printed, which is right for one reading and wrong about
neither.
*/
fn declaring_binding<'a>(
    doc: &'a Doc,
    line: u32,
    word: &str,
) -> Option<&'a alloy::declarations::Binding> {
    // The keywords this line writes, when it declares the name at all.
    let here = doc.source.lines().nth(line as usize).and_then(|text| {
        alloy::declarations::bindings(text)
            .into_iter()
            .find(|b| b.name == word)
            .map(|b| b.prefix)
    });
    let mut found: Option<&alloy::declarations::Binding> = None;

    for b in doc.bindings.iter().filter(|b| b.name == word) {
        if let Some(prefix) = &here {
            if &b.prefix == prefix {
                return Some(b);
            }

            continue;
        }

        match found {
            Some(other) if other.prefix != b.prefix => return None,

            _ => found = Some(b),
        }
    }

    found
}

/// The word the cursor sits on, in the source the author wrote.
fn word_at(doc: &Doc, line: u32, character: u32) -> Option<&str> {
    let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;

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
    let declares = |name: &str| declares_a_struct(doc, name);

    // `function get(self)` inside `impl Box`: the receiver is the struct.
    let before: String = text.chars().take(character as usize).collect();

    if before.trim_end().ends_with("self") && text.contains("function ") {
        return impl_self_type(doc, line).filter(|t| declares(t.split('<').next().unwrap_or(t)));
    }

    // `local root = new Node { ... }`, `local b = new Box<<number>> { }`,
    // and `new Ns.T { }` for a member of a namespace.
    if let Some(i) = text.find("new ")
        && let Some(named) = constructed_type(doc, &text[i + "new ".len()..])
    {
        return Some(named);
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
            .and_then(explicit_arguments)
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
            matches!(s, alloy::declarations::Shape::Enum { name: n, variants, .. }
                if *n == name && variants.iter().any(|(v, _)| *v == variant))
        });

        if holds {
            return Some(name);
        }
    }

    // `local mapped = a:map(double)`: the method's declared return
    // under the receiver's arguments and the call's own.
    if let Some(offset) = offset_of(&doc.source, line, character)
        && let Some(named) = method_call_type(doc, rest, offset)
    {
        return Some(named);
    }

    // `local p = Point.new(1, 2)`, and `local v = Geo.Vec2.new(5)` for a
    // namespace member: the path in front of `.new` names the struct.
    let path: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    let target = path.strip_suffix(".new")?;

    (rest[path.len()..].starts_with('(') && declares(target)).then(|| target.to_string())
}

/// The declared return of `recv:name(args)` with its parameters bound.
/// The receiver's declared type binds the impl's, `T` of `Box<T>` to
/// `number`, and each argument's declared type binds the method's own:
/// `double: (number) -> number` against `f: (T) -> U` binds `U`, so
/// `Box<U>` reads `Box<number>`. None while the return names a
/// parameter nothing bound.
fn method_call_type(doc: &Doc, call: &str, offset: usize) -> Option<String> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let (receiver, after) = call.split_once(':')?;
    let receiver = receiver.trim();
    let name: String = after.chars().take_while(|c| is_word(*c)).collect();
    let list = after[name.len()..].trim_start();

    if receiver.is_empty() || !receiver.chars().all(is_word) || !list.starts_with('(') {
        return None;
    }

    let len = group_len(list, '(', ')')?;
    let named = match crate::context::declared(&doc.source, offset, receiver)? {
        crate::context::Declared::Annotation(t) => t,

        crate::context::Declared::Init(init) => constructed_type(doc, init.strip_prefix("new ")?)?,
    };
    let (owner, owner_args) = match named.split_once('<') {
        Some((o, a)) => (o, alloy::shapes::top_level_parts(a.strip_suffix('>')?)),

        None => (named.as_str(), Vec::new()),
    };
    let declared = std::iter::once(&doc.source)
        .chain(doc.import_sources.iter())
        .map(|src| struct_generics(src, owner))
        .find(|g| !g.is_empty())
        .unwrap_or_default();
    let impl_params: Vec<&str> = declared
        .trim_matches(['<', '>'])
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();

    if impl_params.len() != owner_args.len() {
        return None;
    }

    let head = impl_method_head(doc, owner, &name)?;
    let spans = head_spans(head, name.len())?;
    let (a, b) = spans.ret?;
    let own: Vec<String> = spans
        .generics
        .map(|(g, h)| {
            head[g..h]
                .trim_matches(['<', '>'])
                .split(',')
                .map(|p| p.split(':').next().unwrap_or("").trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut bindings: Vec<(String, String)> = impl_params
        .iter()
        .zip(&owner_args)
        .map(|(p, a)| (p.to_string(), a.trim().to_string()))
        .collect();
    let params = alloy::shapes::top_level_parts(&head[spans.params.0 + 1..spans.params.1 - 1]);
    let args = alloy::shapes::top_level_parts(&list[1..len - 1]);
    let typed = params
        .iter()
        .map(|p| p.trim())
        .filter(|p| *p != "self" && !p.starts_with("self:"))
        .zip(&args);

    for (param, arg) in typed {
        let Some((_, pattern)) = param.split_once(':') else {
            continue;
        };
        let mut pattern = pattern.trim().to_string();

        for (g, bound) in &bindings {
            pattern = alloy::shapes::replace_var(&pattern, g, bound);
        }

        if let Some(actual) = argument_type(doc, arg.trim(), offset) {
            unify(&pattern, &actual, &own, &mut bindings);
        }
    }

    let mut out = head[a..b].trim().to_string();

    for (g, bound) in &bindings {
        out = alloy::shapes::replace_var(&out, g, bound);
    }

    let unbound = impl_params
        .iter()
        .copied()
        .chain(own.iter().map(String::as_str))
        .any(|g| mentions_word(&out, g));

    (!unbound).then_some(out)
}

/// The type an argument is written with: a literal's, a name's
/// annotation or `new`, a declared function's head as `(A) -> R`, or
/// the declared return of a call of one, `g(1)`.
// ponytail: literals, annotated names, function heads, and one call
// deep; a field or a nested chain reads nothing, and the return stays
// unbound.
fn argument_type(doc: &Doc, arg: &str, offset: usize) -> Option<String> {
    if arg.parse::<f64>().is_ok() {
        return Some("number".to_string());
    }

    if arg.starts_with(['"', '\'', '`']) {
        return Some("string".to_string());
    }

    if matches!(arg, "true" | "false") {
        return Some("boolean".to_string());
    }

    // `g(1)`: the declared return of the function, unless it names one
    // of the function's own parameters, which the call alone does not
    // bind.
    if let Some((name, list)) = arg.split_once('(')
        && list.ends_with(')')
        && !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        let (head, _) = declaration_head(doc, name)?;
        let spans = head_spans(head, name.len())?;
        let (a, b) = spans.ret?;
        let ret = head[a..b].trim();
        let own = spans
            .generics
            .map(|(g, h)| head[g..h].trim_matches(['<', '>']).to_string())
            .unwrap_or_default();
        let unbound = own
            .split(',')
            .filter_map(|p| p.split(':').next())
            .map(str::trim)
            .any(|g| !g.is_empty() && mentions_word(ret, g));

        return (!unbound).then(|| ret.to_string());
    }

    if arg.is_empty() || !arg.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    if let Some((head, _)) = declaration_head(doc, arg) {
        let spans = head_spans(head, arg.len())?;
        let params: Vec<&str> =
            alloy::shapes::top_level_parts(&head[spans.params.0 + 1..spans.params.1 - 1])
                .into_iter()
                .map(|p| p.split_once(':').map_or(p.trim(), |(_, t)| t.trim()))
                .collect();
        let ret = spans.ret.map_or("()", |(a, b)| head[a..b].trim());

        return Some(format!("({}) -> {ret}", params.join(", ")));
    }

    match crate::context::declared(&doc.source, offset, arg)? {
        crate::context::Declared::Annotation(t) => Some(t),

        crate::context::Declared::Init(init) => constructed_type(doc, init.strip_prefix("new ")?),
    }
}

/// Binds each parameter of `own` in `pattern` to what stands at the
/// same place in `actual`: `(T) -> U` against `(number) -> string`
/// binds both. The first binding of a parameter holds.
fn unify(pattern: &str, actual: &str, own: &[String], bindings: &mut Vec<(String, String)>) {
    let (pattern, actual) = (pattern.trim(), actual.trim());

    if own.iter().any(|g| g == pattern) {
        if !bindings.iter().any(|(g, _)| g == pattern) {
            bindings.push((pattern.to_string(), actual.to_string()));
        }

        return;
    }

    // A function type: the parameters pairwise, then the return.
    if pattern.starts_with('(')
        && actual.starts_with('(')
        && let Some(pl) = group_len(pattern, '(', ')')
        && let Some(al) = group_len(actual, '(', ')')
    {
        let ps = alloy::shapes::top_level_parts(&pattern[1..pl - 1]);
        let r#as = alloy::shapes::top_level_parts(&actual[1..al - 1]);

        for (p, a) in ps.iter().zip(&r#as) {
            unify(p, a, own, bindings);
        }

        if let Some(pr) = pattern[pl..].trim().strip_prefix("->")
            && let Some(ar) = actual[al..].trim().strip_prefix("->")
        {
            unify(pr, ar, own, bindings);
        }

        return;
    }

    // One generic type under another: `Box<T>` against `Box<number>`.
    if let Some((pn, pa)) = pattern.split_once('<')
        && let Some((an, aa)) = actual.split_once('<')
        && pn == an
        && let Some(pa) = pa.strip_suffix('>')
        && let Some(aa) = aa.strip_suffix('>')
    {
        let ps = alloy::shapes::top_level_parts(pa);
        let r#as = alloy::shapes::top_level_parts(aa);

        for (p, a) in ps.iter().zip(&r#as) {
            unify(p, a, own, bindings);
        }
    }
}

/// The struct a `new` builds, from the text after the keyword: the
/// name or the path, with the arguments the source wrote. `None` when
/// no struct in reach has the name.
pub(crate) fn constructed_type(doc: &Doc, after_new: &str) -> Option<String> {
    let rest = after_new.trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    // `new HashMap<<string, number>>()`: a std collection prints by its
    // metatable too, so the `new` is the only place its arguments stand.
    if !declares_a_struct(doc, &name) && !std_generic(&name) {
        return None;
    }

    let args = rest[name.len()..]
        .strip_prefix("<<")
        .and_then(explicit_arguments);
    let printed = match args {
        Some(a) => format!("{name}<{a}>"),

        None => name,
    };
    // `new Pair<<number>>` of `struct Pair<A, B = string>`: the source
    // names the arguments it has to, and the type carries the rest.
    let shapes: Vec<alloy::declarations::Shape> = doc
        .shapes
        .iter()
        .chain(&doc.import_shapes)
        .cloned()
        .collect();

    Some(alloy::shapes::fill_generic_defaults(&printed, &shapes))
}

/// The arguments of `<<...>>`, from the text after the `<<`. The `>>`
/// that closes the list stands at bracket depth zero, so a nested
/// argument keeps its own `>`: `string, Array<number>>>()` gives
/// `string, Array<number>`.
fn explicit_arguments(after: &str) -> Option<String> {
    let bytes = after.as_bytes();
    let mut depth = 0i32;

    for (i, c) in after.char_indices() {
        match c {
            '<' | '(' | '{' | '[' => depth += 1,

            // The `>` of an arrow closes no bracket.
            '>' if i > 0 && bytes[i - 1] == b'-' => {}

            '>' if depth == 0 => {
                return after[i..].starts_with(">>").then(|| after[..i].to_string());
            }

            '>' | ')' | '}' | ']' => depth -= 1,

            _ => {}
        }
    }

    None
}

/// Whether a name in reach declares a struct. `shapes` reads the top
/// level alone, so a member of a namespace, `Geo.Vec2`, comes from the
/// declaration index, where it stands under its path.
pub(crate) fn declares_a_struct(doc: &Doc, name: &str) -> bool {
    let struct_hover = |hover: &str| {
        hover
            .lines()
            .nth(1)
            .map(|l| l.trim_start().trim_start_matches("export "))
            .is_some_and(|l| l.starts_with("struct "))
    };
    // `new M.Ns.T { }` through `import * as M`: the module declares
    // the path under `M`. `new A.T { }` through `import { Ns as A }`:
    // the module declares it under `Ns`.
    let aliased: String;
    let name = match name.split_once('.') {
        Some((module, rest))
            if crate::proxy::navigation::module_bindings(&doc.source)
                .iter()
                .any(|(m, _)| m == module) =>
        {
            rest
        }

        Some((alias, rest)) if let Some(source) = import_alias_source(&doc.source, alias) => {
            aliased = format!("{source}.{rest}");

            &aliased
        }

        _ => name,
    };

    doc.shapes
        .iter()
        .chain(doc.import_shapes.iter())
        .any(|s| matches!(s, alloy::declarations::Shape::Struct { name: n, .. } if n == name))
        || doc
            .decls
            .iter()
            .chain(doc.import_decls.iter())
            .any(|d| d.name == name && struct_hover(&d.hover))
}

/// Whether a std type takes an argument that has no default:
/// `HashMap<K, V>` does, `Clone<T = any>` does not. The source cannot
/// write such a type bare.
pub(crate) fn std_generic(name: &str) -> bool {
    alloy::std_names::is_std_name(name)
        && alloy::RUNTIME
            .split_once(&format!("export type {name}<"))
            .and_then(|(_, rest)| rest.split_once('>'))
            .is_some_and(|(params, _)| params.split(',').any(|p| !p.contains('=')))
}

/// The child prints a std value's type as its whole shape. The shapes the
/// runtime builds read as their names instead: the Future table becomes
/// `Future<T>`, the Array metatable pair becomes `T[]`, and `Array<T>`
/// with a plain element becomes `T[]` too.
pub(crate) fn fold_std_shapes(value: &str) -> String {
    let mut out = value.to_string();

    // Iter: `{ all: (self: Iter<number>, ...) -> boolean, ... 16 more ... }`.
    // The type has no metatable name to print, and its first method
    // takes the type itself as `self`.
    let mut from = 0;

    while let Some(i) = out[from..].find("{ ") {
        let open = from + i;
        let Some(len) = group_len(&out[open..], '{', '}') else {
            break;
        };
        let body = &out[open + 2..open + len];
        let receiver = body
            .split_once(": (self: ")
            .filter(|(key, _)| key.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .and_then(|(_, ty)| {
                let head = ty.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
                let args = group_len(&ty[head..], '<', '>')?;

                (ty[head..].starts_with('<') && alloy::std_names::is_std_name(&ty[..head]))
                    .then(|| ty[..head + args].to_string())
            });

        match receiver {
            Some(named) => {
                out.replace_range(open..open + len, &named);
                from = open + named.len();
            }

            None => from = open + 1,
        }
    }

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
/// The struct the line constructs is the one the reader means, and a
/// use of the binding below reads the same `new`: Luau names a generic
/// struct by its metatable, which carries no argument.
pub(crate) fn prefer_constructed_struct(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, body) = value.split_once('\n')?;
    let inner = body.trim().strip_suffix("```")?.trim();
    let (head, printed) = inner.split_once(": ")?;
    let offset = offset_of(&doc.source, line, character)?;
    let (start, end) = keywords::word_range(&doc.source, offset);
    let word = &doc.source[start..end];
    let named = source_type(doc, line, character).or_else(|| {
        match crate::context::declared(&doc.source, start, word)? {
            crate::context::Declared::Init(init) => {
                constructed_type(doc, init.strip_prefix("new ")?)
            }

            crate::context::Declared::Annotation(_) => None,
        }
    })?;

    fn bare(n: &str) -> &str {
        n.split('<').next().unwrap_or(n)
    }

    // `local plain: Box<number> = new Box { }`: the annotation names
    // the arguments and the `new` does not. The print keeps them.
    if printed == named || (bare(printed) == bare(&named) && printed.contains('<')) {
        return None;
    }

    // `new Pair<<number, string>>` names the struct with its arguments,
    // and the declaration stands under the name alone.
    let is_struct = |n: &str| declares_a_struct(doc, bare(n));
    // A struct the compiler lists nowhere, a member of a namespace,
    // prints as a solver variable with or without its clause: `t1`,
    // or `t2 where t1 = { ... }`. The print names nothing.
    let unnamed = printed
        .split_whitespace()
        .next()
        .is_some_and(|w| is_solver_variable(w.trim_end_matches('?')));

    // The cursor is on the binding the line declares.
    (head.ends_with(word)
        && (unnamed || (!printed.contains(' ') && is_struct(printed)))
        && is_struct(&named))
    .then(|| format!("{fence}\n{head}: {named}\n```"))
}

/// Whether a word is a solver variable, `t1`: a name the checker made
/// and no source can write.
fn is_solver_variable(word: &str) -> bool {
    word.len() > 1 && word.starts_with('t') && word[1..].chars().all(|c| c.is_ascii_digit())
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

    if !is_solver_variable(inner.trim_end_matches('?')) {
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
/// from the binding the caret reaches, and from no other scope.
pub(crate) fn keep_annotation(value: &str, doc: &Doc, line: u32, character: u32) -> Option<String> {
    let Caret { offset, start, end } = Caret::at(&doc.source, line, character)?;
    let word = &doc.source[start..end];
    let (decl_at, annotation) = declared_annotation(&doc.source, word, offset)?;

    // The binding the caret reaches decides. A parameter of another
    // function is out of scope here, and a `local` the source gave no
    // type keeps the child's own answer.
    if let Some(local) = crate::context::binding_in_scope(&doc.source, offset, word)
        && local.annotation.as_deref() != Some(annotation.as_str())
    {
        return None;
    }

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

    if !keywords::is_word_caret(&doc.source, offset) {
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
    let Some(Caret { start, .. }) = Caret::at(&doc.source, line, character) else {
        return false;
    };
    let line_start = doc.source[..start].rfind('\n').map_or(0, |i| i + 1);
    let head = &doc.source[line_start..start];

    head.matches('"').count() % 2 == 0 && head.matches('\'').count() % 2 == 0
}

/// The three parts of a function head after its name: the type
/// parameter list, the parameter list, and the return type. The first
/// two carry their brackets; the return is the type alone.
pub(crate) struct Head {
    pub(crate) generics: Option<(usize, usize)>,
    pub(crate) params: (usize, usize),
    pub(crate) ret: Option<(usize, usize)>,
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

    let mut last = ' ';

    for (k, c) in inner.char_indices() {
        match c {
            '(' | '{' | '[' | '<' => depth += 1,
            // The `>` of an arrow closes no bracket: `(f: () -> (), n)`
            // binds two names.
            '>' if last == '-' => {}
            ')' | '}' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&inner[start..k]);
                start = k + 1;
            }
            _ => {}
        }

        last = c;
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

            // A blank line has no margin and closes no body.
            if !text.is_empty() && !line.starts_with([' ', '\t']) && text != "end" {
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
    let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;
    let word = doc.source[start..end].to_string();
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;

    if body.contains('\n') {
        return None;
    }

    if !body.contains("function ") {
        return forward_declaration(fence, body, tail, doc, &word);
    }

    // The declaration of a hoisted function assigns the forward
    // `local`, so the child prints that local's optional type:
    // `function isOdd: ((n: number) -> boolean)?`. The header under
    // the caret says what the function takes and returns.
    let Some(name_end) = name_end_in(body, &word) else {
        let typed_binding = body
            .strip_prefix("function ")
            .or_else(|| body.strip_prefix("local "))
            .and_then(|rest| rest.strip_prefix(&word))
            .is_some_and(|after| after.starts_with(':'));

        if !typed_binding || !heads_the_line(doc, line, &word) {
            return None;
        }

        return declared_head(doc, &word).map(|out| format!("{fence}\n{out}\n```{tail}"));
    };
    let child = head_spans(body, name_end)?;
    let owner = printed_owner(&body[..name_end], &word);
    // The file may bind the name as a value too: `local new =
    // Instance.new` beside an impl's `function new` writes the same
    // word. The print then belongs to the local, and a declaration of
    // that name says nothing about it.
    let shadowed = doc
        .bindings
        .iter()
        .any(|b| b.name == word && !b.prefix.ends_with("function"));
    let (source, is_async) = match owner
        .as_deref()
        .and_then(|o| impl_method_head(doc, o, &word))
    {
        Some(head) => (head, false),

        None if shadowed => return None,

        None => declaration_head(doc, &word)?,
    };
    let src = head_spans(source, word.len())?;
    let mut out = body[..name_end].to_string();

    // The source's own parameters go in when every one of them carries
    // a type and the names line up: a print with more names is another
    // function of the same name.
    let src_params = parameter_names(&source[src.params.0..src.params.1]);
    let child_params = parameter_names(&body[child.params.0..child.params.1]);
    // The solver names one type parameter per untyped parameter and
    // gives the letters no meaning: `add<a, b>(a: a, b: b): add<a, b>`
    // says no more than the header the source wrote, and it reads as if
    // the function returned a call to itself. Those letters annotate
    // nothing, so the print drops them with the types they stand in for.
    let scope = declared_type_parameters(&doc.source);
    let invented = child.generics.is_some_and(|(a, b)| {
        let names: Vec<&str> = body[a..b]
            .trim_start_matches('<')
            .trim_end_matches('>')
            .split(',')
            .map(str::trim)
            .collect();

        !names.is_empty() && names.iter().all(|n| n.len() <= 2 && !scope.contains(*n))
    });
    let same = src_params.len() == child_params.len()
        && src_params
            .iter()
            .zip(&child_params)
            .all(|(a, b)| a.0 == b.0 && (a.1 || invented));
    let generics = match (same, src.generics, child.generics) {
        (true, Some((a, b)), _) => Some(&source[a..b]),

        (_, _, Some((a, b))) if !invented => Some(&body[a..b]),

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
            match is_async && !alloy::desugar::names_a_future(text) {
                true => format!("Future<{text}>"),

                false => text.to_string(),
            }
        })
        .or_else(|| match invented {
            true => None,

            false => child.ret.map(|(a, b)| body[a..b].trim().to_string()),
        });

    if let Some(ret) = ret {
        out.push_str(": ");
        out.push_str(&ret);
    }

    (out != body).then(|| format!("{fence}\n{out}\n```{tail}"))
}

/// The head the source declares for a name the checker never bound.
///
/// `local x = later()` above `function later()` reads a global the emit
/// writes further down, so the checker has no type there and prints
/// `type later = *error-type*`. The declaration below says what the
/// name is, and the reader wrote the call to it.
fn forward_declaration(
    fence: &str,
    body: &str,
    tail: &str,
    doc: &Doc,
    word: &str,
) -> Option<String> {
    let rest = body.strip_prefix("type ")?.strip_prefix(word)?;

    if !matches!(rest.trim(), "= *error-type*" | "= unknown" | "= any") {
        return None;
    }

    // A `const` declared below its use: no function head names it, and
    // the line the author wrote says what the name is.
    let out = declared_head(doc, word).or_else(|| forward_constant(doc, word))?;

    Some(format!("{fence}\n{out}\n```{tail}"))
}

/// The signature the source declares for a function name, as the hover
/// prints one: `function later(n: number): boolean`. An async function
/// answers a Future.
pub(crate) fn declared_head(doc: &Doc, word: &str) -> Option<String> {
    let (source, is_async) = declaration_head(doc, word)?;
    let spans = head_spans(source, word.len())?;
    let mut out = format!("function {}", &source[..spans.params.1]);

    if let Some((a, b)) = spans.ret {
        let ret = source[a..b].trim();

        out.push_str(": ");
        out.push_str(&match is_async && !alloy::desugar::names_a_future(ret) {
            true => format!("Future<{ret}>"),

            false => ret.to_string(),
        });
    }

    Some(out)
}

/// Whether the source line under the caret is the header of the
/// function `word`, with any of `export`, `local`, and `async` in front.
fn heads_the_line(doc: &Doc, line: u32, word: &str) -> bool {
    let Some(text) = doc.source.lines().nth(line as usize) else {
        return false;
    };
    let mut head = text.trim();

    for keyword in ["export ", "local ", "async "] {
        head = head.strip_prefix(keyword).unwrap_or(head);
    }

    head.strip_prefix("function ")
        .and_then(|rest| rest.strip_prefix(word))
        .is_some_and(|after| after.starts_with(['(', '<']))
}

/// The `const` a name below its use declares: the keyword with the type
/// the annotation or the literal names, `const limit: number`. A value no
/// literal names reads as the line the author wrote.
///
/// A plain `local` is left out: a name used above its `local` line is the
/// global of that name, so the local below says nothing about it.
fn forward_constant(doc: &Doc, word: &str) -> Option<String> {
    let mut found: Option<(&str, &str)> = None;

    for line in doc.source.lines() {
        let text = line.trim();
        let bare = text.strip_prefix("export ").unwrap_or(text);
        let bare = bare.strip_prefix("global ").unwrap_or(bare);
        let Some(after) = bare
            .strip_prefix("const ")
            .and_then(|r| r.strip_prefix(word))
        else {
            continue;
        };

        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            continue;
        }

        // One name declared twice says nothing about which one this is.
        if found.is_some() {
            return None;
        }

        found = Some((text, after));
    }

    let (text, after) = found?;
    let keyword = text[..text.find(word)?].trim();
    let after = after.trim_start();

    // `const limit: number = 100` names the type outright.
    if let Some(annotated) = after.strip_prefix(':') {
        let ty = annotated.split('=').next().unwrap_or(annotated).trim();

        return Some(format!("{keyword} {word}: {ty}"));
    }

    match after.strip_prefix('=').and_then(|v| literal_type(v)) {
        Some(ty) => Some(format!("{keyword} {word}: {ty}")),

        None => Some(text.to_string()),
    }
}

/// The type a literal names outright: a string, a boolean, or a number.
fn literal_type(value: &str) -> Option<&'static str> {
    let value = value.trim();

    if value.starts_with(['"', '\'']) || value.starts_with("[[") {
        return Some("string");
    }

    if matches!(value, "true" | "false") {
        return Some("boolean");
    }

    let number = value.strip_prefix('-').unwrap_or(value).trim_start();

    (number.starts_with(|c: char| c.is_ascii_digit())
        && number
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_'))
    .then_some("number")
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

        // A parameter list has to follow, or the word names something
        // else: `add<a, b>` in the return of `add<a, b>(a: a, b: b)`
        // matches the name and opens no header.
        if bounded && head_spans(body, end).is_some() {
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

/// A method's head under its receiver's arguments. `a:map(` on
/// `local a: Box<number>` binds the `T` of `struct Box<T>` to `number`:
/// `map<U>(self: Box<number>, f: (number) -> U): Box<U>`. The method's
/// own parameters stay in the list.
pub(crate) fn bind_receiver_arguments(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let head = body.strip_prefix("function ")?;
    let open = head.find('(')?;
    let (owner, name) = head[..open].split_once(':')?;
    let name = name.split('<').next()?;
    // The receiver: the word before `:name(` on the cursor's line.
    let text = doc.source.lines().nth(line as usize)?;
    let before: String = text.chars().take(character as usize).collect();
    let call = before.rfind(&format!(":{name}("))?;
    let receiver = before[..call].trim_end();
    let start = receiver
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    let receiver = &receiver[start..];
    let offset = offset_of(&doc.source, line, character)?;
    let named = match crate::context::declared(&doc.source, offset, receiver)? {
        crate::context::Declared::Annotation(t) => t,

        crate::context::Declared::Init(init) => constructed_type(doc, init.strip_prefix("new ")?)?,
    };
    let (bare, args) = named.split_once('<')?;

    if bare != owner {
        return None;
    }

    let args = alloy::shapes::top_level_parts(args.strip_suffix('>')?);
    let declared = std::iter::once(&doc.source)
        .chain(doc.import_sources.iter())
        .map(|src| struct_generics(src, owner))
        .find(|g| !g.is_empty())?;
    let params: Vec<&str> = declared
        .trim_matches(['<', '>'])
        .split(',')
        .map(str::trim)
        .collect();

    if params.len() != args.len() {
        return None;
    }

    let own: Vec<&str> = head[..open]
        .split_once('<')
        .map(|(_, g)| {
            g.trim_end_matches('>')
                .split(',')
                .map(str::trim)
                .filter(|g| !params.contains(g))
                .collect()
        })
        .unwrap_or_default();
    let mut out = format!("function {owner}:{name}");

    if !own.is_empty() {
        out.push_str(&format!("<{}>", own.join(", ")));
    }

    let mut list = head[open..].to_string();

    for (p, a) in params.iter().zip(&args) {
        list = alloy::shapes::replace_var(&list, p, a.trim());
    }

    out.push_str(&list);

    (out != body).then(|| format!("{fence}\n{out}\n```{tail}"))
}

/// The column of the callee's name on the cursor's line, for a
/// signature label `function Owner:name(` or `function name(`.
fn callee_column(doc: &Doc, label: &str, line: u32, character: u32) -> Option<u32> {
    let head = label.strip_prefix("function ")?;
    let open = head.find('(')?;
    let name = head[..open].rsplit([':', '.']).next()?.split('<').next()?;
    let text = doc.source.lines().nth(line as usize)?;
    let before: String = text.chars().take(character as usize).collect();
    let call = before.rfind(&format!("{name}("))?;

    Some(call as u32)
}

/// The name of the call open at the caret, when a `case` pattern binds
/// it. An expression match calls the payload path in its place, so the
/// child names the call after the path, `Item._1`, or not at all.
fn case_bound_callee(doc: &Doc, line: u32, character: u32) -> Option<&str> {
    let offset = offset_of(&doc.source, line, character)?;
    let (start, end, _) = crate::proxy::completion::open_paren_word(&doc.source, offset)?;

    // `t.f(` and `x:m(` call a member, which no pattern binds.
    if doc.source[..start].ends_with(['.', ':']) {
        return None;
    }

    let word = &doc.source[start..end];
    case_arm_of_binding(doc, line as usize, word)?;

    Some(word)
}

/// Signature help through the hover's restyle: the source's own head
/// replaces the print, a struct name gets its declared parameters
/// back, and the receiver binds the impl's own. The parameters follow
/// the label, one per top-level comma.
pub(crate) fn restyle_signatures(result: &mut Value, doc: &Doc, line: u32, character: u32) {
    let Some(signatures) = result.get_mut("signatures").and_then(Value::as_array_mut) else {
        return;
    };

    for sig in signatures.iter_mut() {
        let Some(label) = sig["label"].as_str() else {
            continue;
        };
        let mut text = format!("```luau\n{label}\n```");

        // The hover's passes read the word under the cursor; here the
        // cursor sits in the call, so the callee's own column stands in.
        if let Some(at) = callee_column(doc, label, line, character)
            && let Some(written) = declared_signature(&text, doc, line, at)
        {
            text = written;
        }

        if let Some(named) = restore_struct_arguments(&text, doc, line) {
            text = named;
        }

        if let Some(bound) = bind_receiver_arguments(&text, doc, line, character) {
            text = bound;
        }

        let Some(rebuilt) = text
            .strip_prefix("```luau\n")
            .and_then(|t| t.strip_suffix("\n```"))
        else {
            continue;
        };
        // A caller passes one value for a pattern, so its type stands in
        // the list, in place of the pattern or of the temp the emit wrote.
        let typed = alloy::desugar::signature_with_pattern_types(rebuilt);
        let rebuilt = typed.as_deref().unwrap_or(rebuilt);
        let plain = crate::proxy::patterns::without_pattern_temps(rebuilt, &doc.source);
        let rebuilt = plain.as_deref().unwrap_or(rebuilt);
        let named = case_bound_callee(doc, line, character).and_then(|word| {
            let rest = rebuilt.strip_prefix("function")?;
            let open = rest.find('(')?;
            let name_end = rest[..open].find('<').unwrap_or(open);

            Some(format!("function {word}{}", &rest[name_end..]))
        });
        let rebuilt = named.as_deref().unwrap_or(rebuilt);

        if rebuilt == label {
            continue;
        }

        let parameters: Vec<Value> = crate::proxy::completion::payload_types(rebuilt)
            .into_iter()
            .map(|p| json!({ "label": p }))
            .collect();
        sig["label"] = json!(rebuilt);
        sig["parameters"] = json!(parameters);
    }
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
    let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;
    let word = &doc.source[start..end];
    let owner = trait_of_method(doc, word)?;
    let rebuilt =
        format!("function {owner}.{word}({head}").replace("(self: any", &format!("(self: {owner}"));

    Some(format!("{fence}\n{rebuilt}\n```{tail}"))
}

/*
The doc comment above a member of a declaration in reach: a field of a
`struct` or an `interface`, or a method of an `impl` or a `trait`.

`doc_before` reads the block above one offset, and only the hover of a
declaration ever held that offset. A use of the member reads the same
text here, from the source that declares it, so `p.hp` in one file says
what `hp` says in the other.
*/
pub(crate) fn member_doc(doc: &Doc, owner: &str, member: &str) -> Option<String> {
    let heads = [
        format!("struct {owner}"),
        format!("interface {owner}"),
        format!("trait {owner}"),
        format!("impl {owner}"),
    ];

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        let mut at = 0;
        let mut inside = false;

        for line in src.lines() {
            let line_start = at;
            at += line.len() + 1;
            let text = line.trim();
            let head = text.strip_prefix("export ").unwrap_or(text);
            let head = head.strip_prefix("global ").unwrap_or(head);

            // A block opens at the margin, and the line at the margin
            // after it closes the one before.
            if !line.starts_with([' ', '\t']) && !text.is_empty() {
                // `impl Trait for Owner` names the owner after `for`.
                inside = heads.iter().any(|h| {
                    head.starts_with(h.as_str())
                        && head[h.len()..].starts_with(|c: char| c.is_whitespace() || c == '<')
                }) || head.starts_with("impl ")
                    && head
                        .split(" for ")
                        .nth(1)
                        .is_some_and(|rest| names_the_owner(rest, owner));

                continue;
            }

            if !inside {
                continue;
            }

            let names_it = field_key(text) == Some(member)
                || text
                    .trim_start_matches("private ")
                    .trim_start_matches("public ")
                    .strip_prefix("function ")
                    .is_some_and(|rest| {
                        rest.starts_with(member) && rest[member.len()..].starts_with(['(', '<'])
                    });

            if names_it && let Some(text) = alloy::declarations::doc_before(src, line_start) {
                return Some(text);
            }
        }
    }

    None
}

/*
A method hover with the doc comment of its declaration appended.

Several passes write the head, `function Owner:m(...)`, so the comment
goes on once they are all done. The `impl` of the receiver carries it,
and a method that takes its text from the trait it implements reads the
trait's.
*/
pub(crate) fn name_method_doc(value: &str, doc: &Doc) -> Option<String> {
    let (block, tail) = value.split_once("\n```")?;

    if !tail.trim().is_empty() {
        return None;
    }

    let body = block.split_once('\n')?.1;
    let head = body.strip_prefix("function ")?;
    let (owner, rest) = head.split_once([':', '.'])?;
    let member: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if member.is_empty() || !rest[member.len()..].starts_with(['(', '<']) {
        return None;
    }

    let text = member_doc(doc, owner, &member)
        .or_else(|| member_doc(doc, &trait_of_method(doc, &member)?, &member))?;

    Some(format!("{block}\n```{tail}\n\n{text}"))
}

/// Whether the text after `for` in an `impl` header names `owner`.
fn names_the_owner(rest: &str, owner: &str) -> bool {
    let named: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();

    named == owner
}

/// A parameter the child prints as a `local`. The two differ in what a
/// reader may do to them, and the source says which this is. The hover
/// names the function the parameter belongs to, the way the hover at
/// its declaration does; the comment the child appends there documents
/// that function, not the parameter.
pub(crate) fn unlocal_parameter(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, _) = rest.split_once("\n```")?;
    // A parameter the source gave a function type prints as a
    // declaration of its own: the body stands, the comment under it
    // does not.
    let local = body.strip_prefix("local ");
    let named = local.or_else(|| body.strip_prefix("function "))?;
    let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;
    let word = &doc.source[start..end];

    if !named.starts_with(word) || !named[word.len()..].starts_with([':', '(']) {
        return None;
    }

    // A name the file declares with a keyword is that declaration,
    // unless a parameter of the same name is the one in scope here.
    let parameter = context::binding_in_scope(&doc.source, start, word)
        .is_some_and(|l| l.kind == context::LocalKind::Parameter);

    if !parameter && doc.bindings.iter().any(|b| b.name == *word) {
        return None;
    }

    // The child prints a solver variable where the source wrote no
    // type: `b: a` names nothing a reader can write, and the letter does
    // not even agree with the one the signature gives `b`. The parameter
    // then reads as the source wrote it.
    let scope = declared_type_parameters(&doc.source);
    let text = match local.and_then(|named| named.split_once(": ")) {
        Some((name, ty)) if undeclared_variable(ty, &scope) => name,

        _ => local.unwrap_or(body),
    };

    // The head the parameter belongs to is the nearest one above the
    // caret that writes the name: a lower function of its own may take
    // one by the same name. A line may hold two heads,
    // `Connect(function(player)`, and the last one is the nearest.
    let line_end = doc.source[end..]
        .find('\n')
        .map_or(doc.source.len(), |i| end + i);
    let owner = doc.source[..line_end].lines().rev().find_map(|l| {
        l.rmatch_indices("function").find_map(|(at, _)| {
            let after = l[at + "function".len()..].chars().next();

            if (at > 0 && keywords::is_word_at(l, at - 1))
                || !matches!(after, Some(' ' | '(' | '<'))
            {
                return None;
            }

            let open = at + l[at..].find('(')?;
            // A list that runs past the line keeps the rest of it.
            let len = group_len(&l[open..], '(', ')').unwrap_or(l.len() - open);
            let list = &l[open..open + len];

            parameter_names(list)
                .iter()
                .any(|(n, _)| n == word)
                .then(|| function_name_of(&l[at..open]))
        })
    })?;
    let owner = match owner {
        Some(name) => format!("`function {name}`"),

        None => "an anonymous function".to_string(),
    };

    Some(format!("{fence}\n{text}\n```\nA parameter of {owner}."))
}

/// `self` inside an `impl`, by the name the `impl` head writes.
///
/// The checker reads the receiver off the body, so one file printed
/// three answers for one name: a read-only view of the fields at
/// `self.x`, a solver variable at `self:m()`, and nothing at all where
/// the body says too little. The `impl` names the type once.
pub(crate) fn name_self_receiver(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;

    if &doc.source[start..end] != "self" {
        return None;
    }

    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let head = body.strip_prefix("local ").unwrap_or(body);

    // The receiver's own print, and not a signature that holds it.
    if head != "self" && !head.starts_with("self:") {
        return None;
    }

    let out = format!("self: {}", impl_self_type(doc, line)?);

    (out != body).then(|| format!("{fence}\n{out}\n```{tail}"))
}

/// `local rows = checked(ids)`: the child prints a solver variable for
/// the binding, or a generic struct by the name of its metatable,
/// `Box` for `Box<number>`. The function the line calls declares what
/// it gives back, and that is the name the reader wrote.
pub(crate) fn name_by_declaration(
    value: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let (fence, rest) = value.split_once('\n')?;
    let (body, tail) = rest.split_once("\n```")?;
    let (head, printed) = body.rsplit_once(": ")?;
    let bare_generic_struct = || {
        std::iter::once(&doc.source)
            .chain(doc.import_sources.iter())
            .any(|src| !struct_generics(src, printed).is_empty())
    };

    if !holds_solver_variable(printed) && !bare_generic_struct() {
        return None;
    }

    let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;
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
    let ret = match is_async && !alloy::desugar::names_a_future(ret) {
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

/// The variadic tail Luau's solver gives a function it infers from a
/// definition with no parameters. The printed pack is `(...any)`, which
/// reads as "takes anything" beside the `(x: number)` of a function
/// with one. The names below get `()` back.
const PRINTED_PACK: &str = "(...any) ->";

/// Every name a source in reach declares as a function with an empty
/// parameter list. A name two sources declare both ways is left out:
/// the print may belong to either one.
///
/// `export default function make()` binds `default` in the module's
/// table, so the key the reader sees goes in beside the name.
pub(crate) fn empty_parameter_names(doc: &Doc) -> HashSet<String> {
    let mut empty: HashSet<String> = HashSet::new();
    let mut takes: HashSet<String> = HashSet::new();

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        for line in src.lines() {
            let Some((name, is_default, empty_list)) = declared_parameter_list(line.trim()) else {
                continue;
            };
            let target = match empty_list {
                true => &mut empty,

                false => &mut takes,
            };

            if is_default {
                target.insert("default".to_string());
            }

            target.insert(name);
        }
    }

    empty.retain(|name| !takes.contains(name));

    empty
}

/// What one line says about a function it declares: the name, whether
/// `default` binds it too, and whether its parameter list is empty.
fn declared_parameter_list(text: &str) -> Option<(String, bool, bool)> {
    // The keywords that may stand in front of `function`, in any order
    // a grammar allows. `remote` is not one: every member of a remote
    // surface really does take anything.
    const AHEAD: [&str; 7] = [
        "export ", "global ", "local ", "public ", "private ", "async ", "default ",
    ];
    let mut head = text;
    let mut is_default = false;

    while let Some((keyword, rest)) = AHEAD.iter().find_map(|k| Some((*k, head.strip_prefix(k)?))) {
        is_default = is_default || keyword == "default ";
        head = rest;
    }

    let rest = head.strip_prefix("function ")?;
    let path: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))
        .collect();
    let after = &rest[path.len()..];
    let after = match after.starts_with('<') {
        true => &after[angle_len(after)?..],

        false => after,
    };
    let list = after.strip_prefix('(')?;
    // `function M.zero()` in a plain Luau module writes the member as
    // a path; the table key is its last segment.
    let name = path.rsplit(['.', ':']).next()?.to_string();

    (!name.is_empty()).then_some((name, is_default, list.trim_start().starts_with(')')))
}

/// `zero: (...any) -> T` back to `zero: () -> T` for every key the
/// sources declare with an empty parameter list.
pub(crate) fn close_empty_packs(text: &str, empty: &HashSet<String>) -> String {
    let mut out = text.to_string();
    let mut from = 0;

    while let Some(at) = out[from..].find(PRINTED_PACK).map(|i| from + i) {
        from = at + PRINTED_PACK.len();

        let key = {
            let Some(head) = out[..at].strip_suffix(": ") else {
                continue;
            };
            let mut key: Vec<char> = head
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            key.reverse();

            key.into_iter().collect::<String>()
        };

        if !empty.contains(&key) {
            continue;
        }

        out.replace_range(at.."(...any)".len() + at, "()");
        from = at + "()".len();
    }

    out
}

/// The same for a completion list, where the label names the member and
/// the detail carries the type with nothing in front of it.
pub(crate) fn close_item_packs(value: &mut Value, empty: &HashSet<String>) {
    match value {
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| close_item_packs(item, empty)),

        Value::Object(map) => {
            let closed = map
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| empty.contains(*label))
                .and_then(|_| map.get("detail")?.as_str()?.strip_prefix("(...any)"))
                .map(|rest| format!("(){rest}"));

            if let Some(detail) = closed {
                map.insert("detail".to_string(), Value::String(detail));
            }

            map.values_mut()
                .for_each(|item| close_item_packs(item, empty));
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_of(src: &str) -> Doc {
        Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        )
    }

    /// Luau names a generic struct by its metatable, `Box`. The
    /// annotation keeps its arguments, nested ones too, a `new` with
    /// a turbofish names them, and a call names the declared return.
    #[test]
    fn a_generic_struct_local_keeps_its_arguments() {
        let src = "export struct Box<T> as\n    v: T\nend\nlocal plain: Box<number> = new Box { v = 42 }\nlocal bb: Box<Box<number>> = new Box { v = plain }\nlocal b = new Box<<number>> { v = 1 }\nfunction useBox(b: Box<number>): Box<number>\n    return b\nend\nlocal x = useBox(plain)\n";
        let doc = doc_of(src);

        assert_eq!(
            prefer_constructed_struct("```luau\nlocal plain: Box<number>\n```", &doc, 3, 6),
            None
        );
        assert_eq!(
            prefer_constructed_struct("```luau\nlocal bb: Box<Box<number>>\n```", &doc, 4, 6),
            None
        );
        assert_eq!(
            prefer_constructed_struct("```luau\nlocal b: Box\n```", &doc, 5, 6).as_deref(),
            Some("```luau\nlocal b: Box<number>\n```")
        );
        assert_eq!(
            name_by_declaration("```luau\nlocal x: Box\n```", &doc, 9, 6).as_deref(),
            Some("```luau\nlocal x: Box<number>\n```")
        );
    }
}
