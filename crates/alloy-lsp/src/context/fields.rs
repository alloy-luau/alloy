//! The fields of a struct: the record a hover or an inline annotation
//! writes, and the struct or the engine class a literal at the caret
//! fills.

use super::strings::{is_word, type_text};

/// One entry of a record type or of a struct body: the field name, the
/// type it declares, and whether the body keeps it private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: String,
    pub private: bool,
}

/// Whether a `>` closes a bracket or carries an arrow.
fn closes_bracket(c: char, prev: char) -> bool {
    matches!(c, ')' | ']' | '}') || (c == '>' && prev != '-')
}

/// The fields a struct body or a record type declares, from the text a
/// declaration hover or an inline annotation holds. A struct body reads
/// line by line, a record type comma by comma; both end each entry at
/// the top level, so a nested `{ }` or a `->` stays inside its field.
pub fn record_entries(text: &str) -> Vec<Field> {
    let body = text.trim();
    let body = body
        .strip_prefix("```alloy")
        .and_then(|rest| rest.split_once("```").map(|(head, _)| head))
        .unwrap_or(body);
    // A record annotation, or the body a `type X = { ... }` names.
    let body = match (body.find('{'), body.rfind('}')) {
        (Some(open), Some(close)) if close > open => &body[open + 1..close],
        _ => body,
    };
    let mut out = Vec::new();
    let mut entry = String::new();
    let mut depth = 0i32;
    let mut prev = ' ';

    for c in body.chars().chain(std::iter::once('\n')) {
        if c == '<' || c == '(' || c == '[' || c == '{' {
            depth += 1;
        } else if closes_bracket(c, prev) {
            depth -= 1;
        }

        prev = c;

        if (c == ',' || c == '\n') && depth <= 0 {
            if let Some(field) = field_entry(&entry) {
                out.push(field);
            }

            entry.clear();

            continue;
        }

        entry.push(c);
    }

    out
}

/// One `name: Type` of a record or a struct body, its modifiers read.
fn field_entry(entry: &str) -> Option<Field> {
    let mut t = entry.trim();
    let mut private = false;

    loop {
        let mut cut = None;

        for modifier in ["export ", "public ", "read ", "write ", "private "] {
            if let Some(rest) = t.strip_prefix(modifier) {
                private = private || modifier == "private ";
                cut = Some(rest.trim_start());

                break;
            }
        }

        // `@f32 x: number` and `@rename("n") x: string`: an attribute
        // stands in front of the name.
        if cut.is_none()
            && let Some(rest) = t.strip_prefix('@')
        {
            let rest = rest.trim_start_matches(is_word);
            let rest = match rest.strip_prefix('(') {
                Some(args) => args.split_once(')').map_or("", |(_, after)| after),

                None => rest,
            };
            cut = Some(rest.trim_start());
        }

        match cut {
            Some(rest) => t = rest,

            None => break,
        }
    }

    let (name, rest) = t.split_once(':')?;
    let name = name.trim();

    if name.is_empty() || !name.chars().all(is_word) {
        return None;
    }

    let ty = type_text(rest).trim_end_matches(',').trim().to_string();

    (!ty.is_empty()).then_some(Field {
        name: name.to_string(),
        ty,
        private,
    })
}

/// The class of `new Instance("Part")`, from the text that ends with
/// its closing parenthesis. An object initialiser follows it.
pub fn instance_class(before: &str) -> Option<String> {
    let head = before.trim_end().strip_suffix(')')?;
    let open = head.rfind('(')?;
    let name = head[..open].trim_end();
    let called = name.ends_with("Instance") && {
        let before_name = &name[..name.len() - "Instance".len()];

        before_name.trim_end().ends_with("new") || before_name.is_empty()
    };

    if !called && !name.ends_with("Instance.new") {
        return None;
    }

    let inner = head[open + 1..].trim();
    let quote = inner.chars().next()?;

    if !matches!(quote, '"' | '\'') || !inner.ends_with(quote) || inner.len() < 2 {
        return None;
    }

    let class = &inner[1..inner.len() - 1];

    (!class.is_empty() && class.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| class.to_string())
}

/// The struct a literal at the caret fills: the name before the `{`
/// that is still open, as `new Stats { |` writes it, or the type the
/// binding a bare `{ |` initialises declares.
pub fn struct_literal_target(src: &str, offset: usize) -> Option<(String, bool)> {
    let head = &src[..offset];
    let mut opens: Vec<usize> = Vec::new();
    let mut quote: Option<char> = None;
    let mut chars = head.char_indices();

    while let Some((i, c)) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                }
            }

            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '-' if head[i..].starts_with("--") => {
                    let end = head[i..].find('\n').map_or(head.len(), |n| i + n);

                    while chars.as_str().len() > head.len() - end {
                        chars.next();
                    }
                }
                '{' => opens.push(i),
                '}' => {
                    opens.pop();
                }
                _ => {}
            },
        }
    }

    let open = *opens.last()?;
    // A field slot takes a name until its `=`.
    let entry = head[open + 1..].rsplit([',', '\n']).next()?;

    if entry.contains('=') {
        return None;
    }

    // `new Pair<<number>> { |`: the type arguments stand between the
    // head and the table, and the fields are the head's.
    let before = without_type_arguments(head[..open].trim_end());
    // `new Ns.T { |`: the path in front of the name is part of it. The
    // declaration index keys a namespace member by its path, the way
    // `impl Ns.T` writes it.
    let name: String = {
        let start = before.len()
            - before
                .trim_end_matches(|c: char| is_word(c) || c == '.')
                .len();

        before[before.len() - start..].trim_matches('.').to_string()
    };

    // The last step of the path is the struct; the rest is where it
    // lives, and a lowercase step there is still a namespace.
    if name
        .rsplit('.')
        .next()
        .is_some_and(|last| last.starts_with(|c: char| c.is_uppercase()))
    {
        return Some((name, false));
    }

    // `new Instance("Part") { |`: the class comes from the string.
    if let Some(class) = instance_class(before) {
        return Some((class, true));
    }

    // `local l: Loadout = { |`: the annotation of the binding names it.
    // `return { |` reads the function's declared return, and `f({ |`
    // the parameter the argument fills.
    let declared = match before.strip_suffix('=') {
        Some(assigned) => {
            let line = assigned.rsplit('\n').next()?;
            let colon = line.rfind(':')?;

            type_text(&line[colon + 1..])
        }

        None if before.ends_with("return")
            && !before[..before.len() - "return".len()].ends_with(is_word) =>
        {
            enclosing_return(src, open)?
        }

        None => argument_type(src, head, open)?,
    };

    (!declared.is_empty() && declared.starts_with(|c: char| c.is_uppercase()))
        .then_some((declared, false))
}

/// The return type the function around `at` declares: `Pet` of
/// `function make(): Pet` or of `function make() -> Pet`.
fn enclosing_return(src: &str, at: usize) -> Option<String> {
    let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = |line: &str| line.len() - line.trim_start().len();
    let own = indent(&src[line_start..]);

    src[..line_start].lines().rev().find_map(|line| {
        let head = line.trim_start();

        if indent(line) >= own || !head.contains("function") {
            return None;
        }

        let open = head.find('(')?;
        let len = super::scope::group_end(&head[open + 1..]) + 1;
        let rest = head[open + 1 + len..].trim_start();
        let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix("->"))?;

        Some(type_text(rest))
    })
}

/// The declared type of the parameter a call argument at `open` fills:
/// `Pet` for the `{` of `feed({ ... })` under `function feed(p: Pet)`.
fn argument_type(src: &str, head: &str, open: usize) -> Option<String> {
    // The `(` of the call that holds the argument, and the argument's
    // index: the commas at depth zero between the two.
    let mut depth = 0i32;
    let mut index = 0usize;
    let mut call = None;

    for (i, c) in head[..open].char_indices().rev() {
        match c {
            ')' | ']' | '}' => depth += 1,

            '(' if depth == 0 => {
                call = Some(i);

                break;
            }

            '(' | '[' | '{' => depth -= 1,

            ',' if depth == 0 => index += 1,

            '\n' if depth == 0 && head[i..open].trim().is_empty() => {}

            _ => {}
        }
    }

    let call = call?;
    let name_end = head[..call].trim_end();
    let name_start = name_end
        .rfind(|c: char| !(is_word(c) || c == '.'))
        .map_or(0, |i| i + 1);
    let name = &name_end[name_start..];

    if name.is_empty() {
        return None;
    }

    let bare = name.rsplit('.').next()?;
    let header = format!("function {name}(");
    let at = src
        .find(&header)
        .or_else(|| src.find(&format!("function {bare}(")))?;
    let params_at = at + src[at..].find('(')? + 1;
    let params = &src[params_at..params_at + super::scope::group_end(&src[params_at..])];
    let param = super::scope::split_top(params).into_iter().nth(index)?;
    let (_, ty) = param.split_once(':')?;

    Some(type_text(ty))
}

/// The text with a trailing `<<...>>` group cut off, so
/// `new Pair<<number>>` reads as `new Pair`. The group may hold a `>`
/// of its own, so the walk counts the brackets.
fn without_type_arguments(before: &str) -> &str {
    if !before.ends_with('>') {
        return before;
    }

    let mut depth = 0usize;

    for (i, c) in before.char_indices().rev() {
        match c {
            '>' => depth += 1,

            '<' => match depth.checked_sub(1) {
                Some(0) => return before[..i].trim_end(),

                Some(left) => depth = left,

                None => return before,
            },

            _ => {}
        }
    }

    before
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table argument fills the parameter its call declares, and a
    /// returned table the function's declared return.
    #[test]
    fn a_literal_argument_and_a_returned_literal_take_their_declared_type() {
        let src = "local function feed(n: number, p: Pet): Pet\n    return p\nend\nfeed(1, { \nlocal function make(): Pet\n    return { \nend\n";
        let arg = src.find("1, { ").unwrap() + 5;
        let ret = src.rfind("return { ").unwrap() + 9;

        assert_eq!(
            struct_literal_target(src, arg),
            Some(("Pet".to_string(), false))
        );
        assert_eq!(
            struct_literal_target(src, ret),
            Some(("Pet".to_string(), false))
        );
        // A table for a parameter of no named type fills nothing.
        let plain = "local function f(t: { any })\nend\nf({ ";
        assert_eq!(struct_literal_target(plain, plain.len()), None);
    }

    /// A field's attributes stand in front of its name; the field is
    /// still one a literal fills.
    #[test]
    fn a_field_with_attributes_is_still_a_field() {
        let hover = "```alloy\nstruct Shot\n  @f32 x: number\n  @u8 power: number\n  @skip note: string\n  @rename(\"t\") tag: string\n  label: string\nend\n```";
        let names: Vec<String> = record_entries(hover).into_iter().map(|f| f.name).collect();

        assert_eq!(names, ["x", "power", "note", "tag", "label"]);
    }

    /// `new Pair<<number>> { |`: the head names the struct, and the
    /// type arguments stand between it and the table.
    #[test]
    fn a_generic_struct_literal_keys_by_its_head() {
        let src = "local p = new Pair<<number>> { fir\n";
        let at = src.find("fir").expect("the prefix");

        assert_eq!(
            struct_literal_target(src, at),
            Some(("Pair".to_string(), false))
        );
        assert_eq!(
            without_type_arguments("new Map<<string, Pair<<number>>>>"),
            "new Map"
        );
        // A comparison is no type argument.
        assert_eq!(without_type_arguments("a > b"), "a > b");
        assert_eq!(without_type_arguments("new Box"), "new Box");
    }

    /// `new Ns.T { |` and `new Outer.Inner.Deep.T { |`: the head keeps
    /// the namespace path, which is the name the index holds.
    #[test]
    fn a_namespaced_struct_literal_keeps_its_path() {
        let src = "local v = new Ns.T { \n";
        let at = src.find("{ ").expect("the brace") + 2;

        assert_eq!(
            struct_literal_target(src, at),
            Some(("Ns.T".to_string(), false))
        );

        let deep = "local v = new Outer.Inner.Deep.T { \n";
        let at = deep.find("{ ").expect("the brace") + 2;

        assert_eq!(
            struct_literal_target(deep, at),
            Some(("Outer.Inner.Deep.T".to_string(), false))
        );

        // A lowercase last step names no struct.
        let lower = "local v = ns.thing { \n";
        let at = lower.find("{ ").expect("the brace") + 2;

        assert_eq!(struct_literal_target(lower, at), None);
    }

    /// The fields a struct body or a record type declares.
    #[test]
    fn record_entries_read_a_body_and_a_record() {
        let hover = concat!(
            "```alloy\n",
            "export struct Profile as\n",
            "    public read id: ProfileId\n",
            "    public stats: Stats\n",
            "    private coins: number = 0\n",
            "end\n",
            "```"
        );
        let fields = record_entries(hover);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "stats", "coins"]);
        assert_eq!(fields[0].ty, "ProfileId");
        assert!(!fields[1].private);
        assert!(fields[2].private);

        // A record type, whose `->` closes no bracket.
        let record = "type RowProps = { entry: Entry, rank: number, on_pick: ((Entry) -> ())? }";
        let fields = record_entries(record);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["entry", "rank", "on_pick"]);
        assert_eq!(fields[2].ty, "((Entry) -> ())?");
    }
}
