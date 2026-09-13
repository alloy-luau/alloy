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

    let before = head[..open].trim_end();
    let name: String = {
        let start = before.len() - before.trim_end_matches(is_word).len();

        before[before.len() - start..].to_string()
    };

    if !name.is_empty() && name.starts_with(|c: char| c.is_uppercase()) {
        return Some((name, false));
    }

    // `new Instance("Part") { |`: the class comes from the string.
    if let Some(class) = instance_class(before) {
        return Some((class, true));
    }

    // `local l: Loadout = { |`: the annotation of the binding names it.
    let assigned = before.strip_suffix('=')?;
    let line = assigned.rsplit('\n').next()?;
    let colon = line.rfind(':')?;
    let declared = type_text(&line[colon + 1..]);

    (!declared.is_empty() && declared.starts_with(|c: char| c.is_uppercase()))
        .then_some((declared, false))
}

#[cfg(test)]
mod tests {
    use super::*;

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
