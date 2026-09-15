//! The value scope: the names a `local`, a `const`, a parameter, a
//! `for` variable, or a `case` binding puts within reach of the caret.

use super::strings::{block_closers, code_of, is_word, last_word, type_text};

/// What a name in the value scope is, for the item's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// A `local`, a `const`, a `for` variable, or a `case` binding.
    Variable,
    /// A parameter of a function or of a lambda.
    Parameter,
    /// A `local function` or a named `function`.
    Function,
}

/// A name an expression at the caret may write, with the annotation
/// its declaration carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub name: String,
    pub annotation: Option<String>,
    pub kind: LocalKind,
}

/// Where a binding a line makes lives: the block the line sits in, the
/// block the line opens, or the `case` arm the line opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bind {
    Outer,
    Inner,
    Arm,
}

/// Whether the `if` after `before` opens an expression rather than a
/// block. An expression `if` closes with its `else`, so it opens no
/// block and takes no `end`.
fn expression_if(before: &str) -> bool {
    let t = before.trim_end();

    if t.is_empty() {
        return false;
    }

    if t.ends_with(is_word) {
        return matches!(last_word(t), "return" | "and" | "or" | "not");
    }

    t.ends_with([
        '=', '(', ',', '[', '{', '+', '-', '*', '/', '%', '^', '<', '>', '~', '?', ':',
    ])
}

/// The words of a line with the byte each one starts at.
fn words_at(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;

    for (i, c) in text.char_indices() {
        match (is_word(c), start) {
            (true, None) => start = Some(i),

            (false, Some(s)) => {
                out.push((s, &text[s..i]));
                start = None;
            }

            _ => {}
        }
    }

    if let Some(s) = start {
        out.push((s, &text[s..]));
    }

    out
}

/// The blocks a line opens, for the walk that tracks which names are
/// still in scope. An `if` expression is left out: it closes with its
/// `else`, not with an `end`.
pub fn value_openers(text: &str) -> i32 {
    let mut count = 0;
    // A `for` or a `while` head owns the `do` that ends it.
    let mut head_open = false;

    for (at, word) in words_at(text) {
        match word {
            // A local named match opens nothing; `match x with` does.
            "match" if !alloy_syntax::contextual::keyword_at_byte(text, at) => {}

            "function" | "match" | "repeat" | "struct" | "enum" | "interface" | "impl"
            | "trait" | "macro" | "namespace" => count += 1,

            "for" | "while" => {
                count += 1;
                head_open = true;
            }

            "do" => match head_open {
                true => head_open = false,

                false => count += 1,
            },

            "if" if !expression_if(&text[..at]) => count += 1,

            _ => {}
        }
    }

    count
}

/// Splits at the commas of the top level. A bracket, an angle bracket,
/// and a string keep their own commas.
fn split_top(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    let mut start = 0;

    for (i, c) in text.char_indices() {
        match quote {
            Some(q) => {
                if c == q && prev != '\\' {
                    quote = None;
                }
            }

            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                '<' => angle += 1,
                '>' if prev != '-' && angle > 0 => angle -= 1,

                ',' if depth == 0 && angle == 0 => {
                    out.push(&text[start..i]);
                    start = i + 1;
                }

                _ => {}
            },
        }

        prev = c;
    }

    out.push(&text[start..]);

    out
}

/// One entry of a binding list: `x`, `x: T`, `x: T = d`, `...rest`. A
/// `_` binds nothing, and a literal names nothing.
fn binding_entry(part: &str) -> Option<Local> {
    let t = part
        .trim()
        .trim_start_matches(['[', '{', '(', '.', ' '])
        .trim_start();
    let name: String = t.chars().take_while(|c| is_word(*c)).collect();

    if name.is_empty() || name == "_" || name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }

    let rest = t[name.len()..].trim_start();
    let annotation = rest
        .strip_prefix(':')
        .filter(|r| !r.starts_with(':'))
        .map(type_text)
        .filter(|a| !a.is_empty());

    Some(Local {
        name,
        annotation,
        kind: LocalKind::Variable,
    })
}

/// The names a `case` pattern binds: what its payload brackets hold.
/// A bare variant and a literal bind nothing.
pub fn pattern_names(rest: &str) -> Vec<Local> {
    let text = rest.split(" then").next().unwrap_or(rest);
    let Some(open) = text.find(['(', '[', '{']) else {
        return Vec::new();
    };
    let inner = &text[open + 1..];
    let end = inner.rfind([')', ']', '}']).unwrap_or(inner.len());

    split_top(&inner[..end])
        .into_iter()
        .filter_map(binding_entry)
        .collect()
}

/// The byte the group opened before `text` closes at, or the length of
/// `text` when the line has no closing bracket yet.
fn group_end(text: &str) -> usize {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;

    for (i, c) in text.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }

            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,

                ')' | ']' | '}' => {
                    if depth == 0 {
                        return i;
                    }

                    depth -= 1;
                }

                _ => {}
            },
        }
    }

    text.len()
}

/// The `=` that ends the name list of a binding, at the top level. A
/// `==` compares and a `=>` names a child.
fn top_assign(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut quote: Option<u8> = None;
    let mut prev = b' ';

    for (i, c) in bytes.iter().enumerate() {
        let c = *c;

        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }

            None => match c {
                b'"' | b'\'' | b'`' => quote = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'<' => angle += 1,
                b'>' if prev != b'-' && angle > 0 => angle -= 1,

                b'=' if depth == 0
                    && angle == 0
                    && bytes.get(i + 1) != Some(&b'=')
                    && bytes.get(i + 1) != Some(&b'>')
                    && !matches!(prev, b'=' | b'~' | b'<' | b'>') =>
                {
                    return Some(i);
                }

                _ => {}
            },
        }

        prev = c;
    }

    None
}

/// The names one line binds, each with the block it belongs to.
fn bindings_of(line: &str) -> Vec<(Local, Bind)> {
    let mut out: Vec<(Local, Bind)> = Vec::new();
    let trimmed = line.trim();
    let head = trimmed.strip_prefix("export ").unwrap_or(trimmed);

    // An arm binds what its pattern names, and only for that arm.
    if let Some(rest) = head.strip_prefix("case ") {
        return pattern_names(rest)
            .into_iter()
            .map(|l| (l, Bind::Arm))
            .collect();
    }

    if head.starts_with("default") {
        return out;
    }

    // `for k, v in rows do` and `for i = 1, n do`.
    if let Some(rest) = head.strip_prefix("for ") {
        let names = rest.split(" in ").next().unwrap_or(rest);
        let names = names.split('=').next().unwrap_or(names);

        return split_top(names)
            .into_iter()
            .filter_map(binding_entry)
            .map(|l| (l, Bind::Inner))
            .collect();
    }

    // Every `function` on the line: the name it declares and the
    // parameters its list holds.
    for (at, word) in words_at(line) {
        if word != "function" {
            continue;
        }

        let after = &line[at + word.len()..];
        let skip = after.len() - after.trim_start().len();
        let named = after.trim_start();
        let name: String = named.chars().take_while(|c| is_word(*c)).collect();

        // `function Type.method` and `function Type:method` add no
        // name to the scope; the type owns the method.
        if !name.is_empty() && !named[name.len()..].starts_with(['.', ':']) {
            out.push((
                Local {
                    name,
                    annotation: None,
                    kind: LocalKind::Function,
                },
                Bind::Outer,
            ));
        }

        let rest = &after[skip..];

        if let Some(open) = rest.find('(') {
            let inside = &rest[open + 1..];
            let end = group_end(inside);

            for part in split_top(&inside[..end]) {
                if let Some(mut local) = binding_entry(part) {
                    local.kind = LocalKind::Parameter;
                    out.push((local, Bind::Inner));
                }
            }
        }
    }

    // `local a, b = f()`, `const n: number = 1`, and the `if local c =
    // ... then` whose binding lives in the branch.
    for (at, word) in words_at(line) {
        if !matches!(word, "local" | "const") {
            continue;
        }

        let before = line[..at].trim();
        let rest = line[at + word.len()..].trim_start();

        // `local function f()` named its function above.
        if rest.starts_with("function") {
            continue;
        }

        let names = match top_assign(rest) {
            Some(i) => &rest[..i],

            None => rest,
        };
        let bind = match before.is_empty() || matches!(before, "export" | "global") {
            true => Bind::Outer,

            false => Bind::Inner,
        };

        for part in split_top(names) {
            // `local { a, b } = p` binds each name the pattern holds.
            let inner = part
                .trim()
                .strip_prefix(['{', '['])
                .and_then(|t| t.strip_suffix(['}', ']']));

            for name in inner.map_or_else(|| vec![part], split_top) {
                if let Some(local) = binding_entry(name) {
                    out.push((local, bind));
                }
            }
        }
    }

    out
}

/// Whether a line opens the body of a declaration, whose members
/// belong to the type rather than to the scope around it.
fn opens_a_declaration(trimmed: &str) -> bool {
    let head = trimmed.strip_prefix("export ").unwrap_or(trimmed);

    matches!(
        head.split_whitespace().next(),
        Some("struct" | "enum" | "interface" | "impl" | "trait" | "class" | "declare")
    )
}

/// The names in scope at the caret: the locals and the constants, the
/// parameters of the enclosing functions, the `for` variables, the
/// `case` bindings, and the `if local` bindings. A name a closed block
/// declared is gone, and a name below the caret was never there.
pub fn locals_in_scope(src: &str, offset: usize) -> Vec<Local> {
    let offset = offset.min(src.len());
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let mut scope: Vec<(i32, Bind, Local)> = Vec::new();
    // The depths a `struct`, an `impl`, or a `trait` body holds. A
    // method named there belongs to its type, not to the scope.
    let mut bodies: Vec<i32> = Vec::new();
    let mut depth = 0i32;

    for raw in src[..line_start].lines() {
        let text = code_of(raw);
        let trimmed = text.trim();

        if trimmed.is_empty() {
            continue;
        }

        depth = (depth - block_closers(trimmed)).max(0);
        scope.retain(|(d, _, _)| *d <= depth);
        bodies.retain(|d| *d <= depth);

        // A branch and an arm end where the next one starts.
        if trimmed.starts_with("else") {
            scope.retain(|(d, _, _)| *d < depth);
        }

        if trimmed.starts_with("case ") || trimmed.starts_with("default") {
            scope.retain(|(d, bind, _)| *bind != Bind::Arm || *d < depth);
        }

        let inner = depth + value_openers(trimmed);
        let in_body = bodies.contains(&depth);

        for (local, bind) in bindings_of(text) {
            // A method of an `impl` or a `trait` reads as `self:name`;
            // its bare name is no local.
            if bind == Bind::Outer && local.kind == LocalKind::Function && in_body {
                continue;
            }

            let at = match bind {
                Bind::Outer => depth,

                _ => inner,
            };
            // A name of an enclosing block stays: the inner binding
            // shadows it, and it is back in scope once the block ends.
            scope.retain(|(d, _, l)| l.name != local.name || *d < at);
            scope.push((at, bind, local));
        }

        if opens_a_declaration(trimmed) {
            bodies.push(inner);
        }

        depth = inner;
    }

    // The caret's own line binds too: `case Ok(v) then |` sees `v`, and
    // `for _, p in rows where |` sees `p`. A `local x = |` does not:
    // the caret sits in the value `x` takes.
    for (local, bind) in bindings_of(code_of(&src[line_start..offset])) {
        if bind == Bind::Outer {
            continue;
        }

        scope.retain(|(_, _, l)| l.name != local.name);
        scope.push((depth, bind, local));
    }

    // One entry per name, the deepest of them: a binding an inner block
    // made shadows the one around it.
    let mut out: Vec<Local> = Vec::new();

    for (_, _, local) in scope.into_iter().rev() {
        if !out.iter().any(|l| l.name == local.name) {
            out.push(local);
        }
    }

    out.reverse();

    out
}

/// The binding `name` has at the caret: the one the enclosing blocks
/// declare, or the one the caret's own line makes. A declaration in
/// another function never reaches the caret.
pub fn binding_in_scope(src: &str, offset: usize, name: &str) -> Option<Local> {
    let offset = offset.min(src.len());

    if let Some(local) = locals_in_scope(src, offset)
        .into_iter()
        .find(|l| l.name == name)
    {
        return Some(local);
    }

    // The caret sits on the declaration itself. `local rows: Row[] = f()`
    // binds below its own line, so the walk above leaves it out.
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[line_start..]
        .find('\n')
        .map_or(src.len(), |i| line_start + i);

    bindings_of(code_of(&src[line_start..line_end]))
        .into_iter()
        .find(|(l, _)| l.name == name)
        .map(|(l, _)| l)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names `locals_in_scope` finds at the `|`, sorted.
    fn scope_at(src: &str) -> Vec<String> {
        let offset = src.find('|').unwrap();
        let mut names: Vec<String> = locals_in_scope(&src.replace('|', ""), offset)
            .into_iter()
            .map(|l| l.name)
            .collect();
        names.sort();

        names
    }

    /// An expression takes the locals above it, the parameters of the
    /// functions around it, and nothing a closed block declared.
    #[test]
    fn the_value_scope_holds_what_the_caret_can_name() {
        let src = concat!(
            "local total = 0\n",
            "export function tally(rows: number[], seed: number): number\n",
            "    local acc = seed\n",
            "    for _, row in rows do\n",
            "        local doubled = row * 2\n",
            "        acc += doubled\n",
            "    end\n",
            "    local kind = if acc > 0 then |1 else 2\n",
            "    return acc\n",
            "end\n",
        );
        let names = scope_at(src);
        assert!(names.contains(&"total".to_string()), "{names:?}");
        assert!(names.contains(&"tally".to_string()), "{names:?}");
        assert!(names.contains(&"rows".to_string()), "{names:?}");
        assert!(names.contains(&"seed".to_string()), "{names:?}");
        assert!(names.contains(&"acc".to_string()), "{names:?}");
        // The `for` block closed above the caret.
        assert!(!names.contains(&"row".to_string()), "{names:?}");
        assert!(!names.contains(&"doubled".to_string()), "{names:?}");
        // A name the caret's own line declares is not bound yet.
        assert!(!names.contains(&"kind".to_string()), "{names:?}");
    }

    /// A pattern local binds every name its brackets hold.
    #[test]
    fn a_pattern_local_binds_each_of_its_names() {
        let src = "local { a, b } = p\nlocal [x, ...rest] = arr\nprint(|)\n";
        let names = scope_at(src);
        assert_eq!(names, ["a", "b", "rest", "x"], "{names:?}");
    }

    /// An arm binds its payload for that arm alone, and a method of an
    /// `impl` is no local.
    #[test]
    fn an_arm_binding_and_a_method_name_take_their_place() {
        let src = concat!(
            "impl Round as\n",
            "    function step(self, msg: Msg): string\n",
            "        match msg with\n",
            "            case Join(pid) then\n",
            "                return \"in\"\n",
            "            case Leave(who) then\n",
            "                return |\"out\"\n",
            "        end\n",
            "    end\n",
            "end\n",
        );
        let names = scope_at(src);
        assert!(names.contains(&"self".to_string()), "{names:?}");
        assert!(names.contains(&"msg".to_string()), "{names:?}");
        assert!(names.contains(&"who".to_string()), "{names:?}");
        // The arm above closed with its own binding.
        assert!(!names.contains(&"pid".to_string()), "{names:?}");
        // `step` is a method: it reads as `self:step`.
        assert!(!names.contains(&"step".to_string()), "{names:?}");
    }

    /// A lambda's parameters, an `if local`, and a `for` head bind on
    /// the caret's own line.
    #[test]
    fn a_lambda_an_if_local_and_a_for_head_bind_at_the_caret() {
        assert!(
            scope_at("rows:for_each(function(row)\n    print(|)\nend)\n")
                .contains(&"row".to_string())
        );
        assert!(
            scope_at("if local hit = find() then\n    print(|)\nend\n")
                .contains(&"hit".to_string())
        );
        assert!(scope_at("for _, p in players where p > | do\nend\n").contains(&"p".to_string()));
    }
}
