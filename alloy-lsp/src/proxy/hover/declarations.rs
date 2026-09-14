use super::*;

impl Server {
    pub(crate) fn declaration_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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
        // `local Point = 1` binds the name in this file. Another file
        // may declare a `Point` of its own, and that declaration says
        // nothing about the binding the caret sits on.
        let bound_here = binds_a_value(&doc.bindings, &key);
        // A name inside an import list belongs to the module the spec
        // names. Another open file may export the same name, and its
        // declaration says nothing about this entry.
        let spec_decls = st.import_line_decls(uri, &doc.source, start);
        let lookup = |name: &str| {
            if let Some(decls) = &spec_decls {
                return decls.iter().find(|d| d.name == name);
            }

            doc.decls.iter().find(|d| d.name == name).or_else(|| {
                (!bound_here).then(|| {
                    st.docs
                        .values()
                        .flat_map(|d| d.decls.iter())
                        .find(|d| d.name == name)
                        // The modules this file imports, for the moment
                        // the workspace pass has not opened them yet.
                        .or_else(|| doc.import_decls.iter().find(|d| d.name == name))
                })?
            })
        };
        let found = lookup(&key).or_else(|| sigils.iter().find_map(|k| lookup(k)));
        // `import { Thing as ThingAlias }`: the alias is this file's word
        // for the export, and the declaration sits under the name the
        // module wrote. Without this the child answers instead, and it
        // prints the constructor table of a solver variable.
        let found = found.or_else(|| {
            // A macro or an attribute carries its sigil in the name the
            // module wrote, so `$log` for `logit as log` reads `$logit`.
            let source = import_alias_source(&doc.source, key.trim_start_matches(['$', '@']))?;

            [source.clone(), format!("${source}"), format!("@{source}")]
                .iter()
                .find_map(|k| lookup(k))
        });
        // `import * as Dir from "./m"`: `Dir.Name` names the module's
        // own export, so the declaration sits under the bare name. A
        // value reads better through its module, `function Dir.make(...)`,
        // so only a type declaration takes this path.
        let module_decls = found.is_none().then(|| {
            let (file, name) = st.module_member_at(uri, &doc.source, start)?;

            Some((
                alloy::declarations::summaries(&st.module_text(&file)?, false),
                name,
            ))
        });
        let found = found.or_else(|| {
            let (decls, name) = module_decls.as_ref()?.as_ref()?;

            decls
                .iter()
                .find(|d| d.name == *name && declares_a_type(&d.hover))
        });

        let Some(decl) = found else {
            return false;
        };

        // An attribute contract with an `each` clause reads best where
        // it is used: the arguments of this use name the members, so the
        // hover writes one line per member instead of the clause.
        let hover = expand_each(&decl.hover, &doc.source, start);
        let hover = with_member_methods(&hover, doc, &key);
        let (sl, sc) = position_of(&doc.source, start);
        let (el, ec) = position_of(&doc.source, end);
        let result = json!({
            "contents": { "kind": "markdown", "value": hover },
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
}

/*
The hover of a namespace member, with the methods of its `impl` blocks
listed inside the block.

`summaries` keys its impl index by the bare name of a top level `impl`,
so a member of a namespace finds none: the block stands inside the
namespace body as `impl Vec2`, or outside it as `impl Geo.Vec2`. Both
routes read here, from the sources in reach.
*/
fn with_member_methods(hover: &str, doc: &Doc, path: &str) -> String {
    let Some((ns, name)) = path.rsplit_once('.') else {
        return hover.to_string();
    };
    let mut lines: Vec<&str> = hover.lines().collect();
    // The block the hover opens with: a struct or an enum takes methods,
    // and one that already lists them needs none.
    let opens = lines
        .get(1)
        .map(|l| l.trim_start().trim_start_matches("export "))
        .is_some_and(|l| l.starts_with("struct ") || l.starts_with("enum "));
    let Some(end) = lines.iter().position(|l| *l == "end") else {
        return hover.to_string();
    };

    if !opens || lines[..end].iter().any(|l| l.contains("function ")) {
        return hover.to_string();
    }

    let methods = member_methods(doc, ns, name, path);

    if methods.is_empty() {
        return hover.to_string();
    }

    let owned: Vec<String> = methods.iter().map(|m| format!("    {m}")).collect();
    lines.splice(end..end, owned.iter().map(String::as_str));

    lines.join("\n")
}

/// The public method lines of every `impl` of `path`, from the sources in
/// reach: a block that names the path outright, and one inside the
/// namespace body that names the member alone.
fn member_methods(doc: &Doc, ns: &str, name: &str, path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    for src in std::iter::once(&doc.source).chain(doc.import_sources.iter()) {
        let blocks = alloy::impl_blocks::impl_blocks(src);

        if blocks.is_empty() {
            continue;
        }

        let ranges = alloy::declarations::namespace_ranges(src);

        for block in blocks {
            let inside = || {
                ranges
                    .iter()
                    .any(|r| r.path == ns && block.start >= r.start && block.start <= r.end)
            };

            if block.target != path && !(block.target == name && inside()) {
                continue;
            }

            // The header, then one line per method, then `end`: the
            // hover of the block already writes them as an author would.
            for line in block.hover.lines().skip(2) {
                if line == "end" {
                    break;
                }

                let line = line.trim();

                if !line.is_empty() && !out.iter().any(|held| held == line) {
                    out.push(line.to_string());
                }
            }
        }
    }

    out
}

/// Whether a line opens the `default` arm. The body may stand on the
/// same line, so the word alone and the word with a body both count.
fn opens_the_default_arm(text: &str) -> bool {
    text == "default" || text.starts_with("default ")
}

/// The line of the `case` arm that holds `line`: the nearest `case`
/// above it at the depth of the line's own `match`. A block that closed
/// above the line is crossed whole, and so is the `match` of a
/// `default` arm: that arm binds nothing, so the name comes from the
/// arm the match itself sits in.
pub(crate) fn case_binding_line(lines: &[&str], line: usize) -> Option<usize> {
    let mut at = line.min(lines.len().saturating_sub(1));
    // The blocks the walk entered from below and has yet to leave.
    let mut inside = 0i32;

    loop {
        let text = lines.get(at)?.trim();

        if inside > 0 {
            inside = (inside + crate::context::block_closers(text)
                - crate::context::value_openers(text))
            .max(0);
        } else if text.starts_with("case ") {
            break Some(at);
        } else if opens_the_default_arm(text) {
            inside = 1;
        } else if text.ends_with(" with") {
            return None;
        } else {
            inside = crate::context::block_closers(text);
        }

        at = at.checked_sub(1)?;
    }
}

/// Where the `case` arm that holds `line` binds `word`: the byte range
/// of the name in the arm's own pattern. A match lowers to one
/// expression, so the binding has no local of its own and the child,
/// which reads the emit, has nothing to point at.
pub(crate) fn case_binding_span(
    doc: &Doc,
    line: usize,
    word: &str,
    known: &crate::shapes::Known,
) -> Option<(usize, usize)> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let case_line = case_binding_line(&lines, line)?;
    let pattern = case_pattern(lines[case_line])?;

    if !pattern_bindings(&pattern, known, || array_element(&lines, case_line))
        .iter()
        .any(|(name, _, _)| name == word)
    {
        return None;
    }

    let at = offset_of(&doc.source, case_line as u32, 0)? + whole_word(lines[case_line], word)?;

    Some((at, at + word.len()))
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
    let case_line = case_binding_line(&lines, line)?;
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

impl State {
    /// The declarations of the module an import entry names, when the
    /// caret sits on that entry. The open document first, then the
    /// disk. `None` off an import list.
    pub(crate) fn import_line_decls(
        &self,
        uri: &str,
        source: &str,
        offset: usize,
    ) -> Option<Vec<alloy::declarations::Declaration>> {
        let entry = self.import_entry_at(source, offset)?;
        let target = imports::module_path(&self.resolve_spec(uri, &entry.spec)?);
        let open = self
            .docs
            .iter()
            .find(|(u, _)| uri_to_path(u).is_some_and(|p| imports::module_path(&p) == target))
            .map(|(_, d)| d.source.clone());
        let text = open.or_else(|| std::fs::read_to_string(imports::module_file(&target)?).ok())?;

        Some(alloy::declarations::summaries(&text, false))
    }
}

/// Whether a declaration's hover writes a type block. A value keeps its
/// own hover, which names the module it came through.
fn declares_a_type(hover: &str) -> bool {
    let Some(line) = hover.lines().nth(1) else {
        return false;
    };
    let line = line.trim_start().trim_start_matches("export ");

    ["struct ", "enum ", "trait ", "interface ", "type "]
        .iter()
        .any(|k| line.starts_with(k))
}

/// The export name a local alias stands for:
/// `import { Thing as ThingAlias }` answers `Thing` for `ThingAlias`.
/// A name the file imports under its own spelling answers nothing, so a
/// caller keeps its own lookup.
pub(crate) fn import_alias_source(src: &str, bound: &str) -> Option<String> {
    import_entries(src)
        .into_iter()
        .find(|e| e.bound == bound && e.name != bound)
        .map(|e| e.name)
}

/// Whether the file binds a name as a value of its own, `local Point`
/// or `const Point`. A declaration of that name in another file says
/// nothing about the binding the caret sits on.
///
/// `export` and `global` stand in front of the keyword that binds, so
/// the test reads every word of the prefix and not the first alone.
pub(crate) fn binds_a_value(bindings: &[alloy::declarations::Binding], name: &str) -> bool {
    bindings.iter().any(|b| {
        b.name == name
            && b.prefix
                .split_whitespace()
                .any(|word| matches!(word, "local" | "const"))
    })
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

    /// The hover of a namespace member lists the methods of its `impl`,
    /// whether the block stands inside the namespace body or outside it
    /// under the member's path.
    #[test]
    fn a_namespace_member_lists_the_methods_of_its_impl() {
        const INSIDE: &str = "namespace Geo as\n    struct Vec2 as\n        x: number\n    end\n\n    impl Vec2 as\n        function new(x: number): Vec2\n            return new Vec2 { x = x }\n        end\n    end\nend\n";
        const OUTSIDE: &str = "namespace Geo as\n    struct Vec2 as\n        x: number\n    end\nend\n\nimpl Geo.Vec2 as\n    function new(x: number): Geo.Vec2\n        return new Geo.Vec2 { x = x }\n    end\nend\n";

        for src in [INSIDE, OUTSIDE] {
            let doc = doc_of(src);
            let decl = doc
                .decls
                .iter()
                .find(|d| d.name == "Geo.Vec2")
                .expect("the member");
            let hover = with_member_methods(&decl.hover, &doc, "Geo.Vec2");

            assert!(
                hover.contains("    public function new(x: number)"),
                "{hover}"
            );
        }

        // A top level struct already carries them, so nothing repeats.
        let doc = doc_of(
            "struct P as\n    x: number\nend\nimpl P as\n    function new(x: number): P\n        return new P { x = x }\n    end\nend\n",
        );
        let decl = doc.decls.iter().find(|d| d.name == "P").expect("P");

        assert_eq!(with_member_methods(&decl.hover, &doc, "P"), decl.hover);
    }

    /// `local Point = 1` hovered as another file's `struct Point`.
    #[test]
    fn a_local_is_not_another_file_s_declaration() {
        let src = "local Point = 1\nconst MAX = 2\nstruct Vec2 as\n    x: number\nend\nprint(Point, MAX, Vec2)\n";
        let bindings = alloy::declarations::bindings(src);
        assert!(binds_a_value(&bindings, "Point"));
        assert!(binds_a_value(&bindings, "MAX"));

        // A struct's name is a declaration, not a value binding, so the
        // workspace still answers for it.
        assert!(!binds_a_value(&bindings, "Vec2"));
        assert!(!binds_a_value(&bindings, "nothing"));

        // `export local Size = 42` binds `Size` here. Another file's
        // `export type Size` says nothing about it.
        let exported = alloy::declarations::bindings("export local Size = 42\n");
        assert!(binds_a_value(&exported, "Size"));
    }
}

/*
The hover of an attribute, with every `each <param>` clause expanded
against the arguments of the use at `at`.

A clause reads `- \`private function each lifecycles (self)\`` in the
declaration's hover. At a use the reader wants the members it asks for,
so the line becomes one per entry of that argument. A caret on the
declaration itself finds no argument list and keeps the clause.
*/
fn expand_each(hover: &str, source: &str, at: usize) -> String {
    if !hover.contains("each ") {
        return hover.to_string();
    }

    let mut out: Vec<String> = Vec::new();

    for line in hover.lines() {
        let Some((head, param, shape)) = each_clause(line) else {
            out.push(line.to_string());

            continue;
        };
        let names = each_arguments(source, at, &param);

        if names.is_empty() {
            out.push(line.to_string());

            continue;
        }

        for name in names {
            out.push(format!("- `{head}{name}{shape}`"));
        }
    }

    out.join("\n")
}

/// A hover line that holds an `each` clause, split into the words before
/// `each`, the parameter, and the shape after it.
fn each_clause(line: &str) -> Option<(String, String, String)> {
    let body = line.strip_prefix("- `")?.strip_suffix('`')?;
    let (head, rest) = body.split_once("each ")?;
    // The parameter runs to the shape: `(self)` for a function, `: T` for
    // a field. A clause with no shape is the parameter alone.
    let at = rest.find(['(', ':']).unwrap_or(rest.len());
    let (param, shape) = rest.split_at(at);

    Some((
        head.to_string(),
        param.trim().to_string(),
        shape.to_string(),
    ))
}

/*
The member names the argument `param` carries, read from the attribute
use the offset `at` sits in.

The argument is a literal, which is what lets the compiler read it too.
This reads the same text: the list between the brackets, one name per
entry, with a dotted path reduced to its last segment.
*/
fn each_arguments(source: &str, at: usize, param: &str) -> Vec<String> {
    let line_start = source[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = source[at..]
        .find('\n')
        .map(|i| at + i)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];

    if !line.trim_start().starts_with('@') {
        return Vec::new();
    }

    // The record form names the parameter; the positional form is the
    // only list on the line.
    let rest = match line.find(&format!("{param} =")) {
        Some(i) => &line[i..],

        None => line,
    };
    let Some(open) = rest.find('[') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find(']') else {
        return Vec::new();
    };

    rest[open + 1..open + close]
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim().trim_matches(['"', '\'']);
            let name = entry.rsplit('.').next().unwrap_or(entry).trim();

            (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| name.to_string())
        })
        .collect()
}

#[cfg(test)]
mod contract_tests {
    use super::{each_arguments, each_clause, expand_each};

    #[test]
    fn a_clause_line_splits_into_its_parts() {
        assert_eq!(
            each_clause("- `private function each lifecycles(self)`"),
            Some((
                "private function ".to_string(),
                "lifecycles".to_string(),
                "(self)".to_string()
            ))
        );
        assert_eq!(
            each_clause("- `field each keys: number`"),
            Some((
                "field ".to_string(),
                "keys".to_string(),
                ": number".to_string()
            ))
        );
        assert_eq!(each_clause("- `public function Start(self)`"), None);
        assert_eq!(each_clause("**Requires**"), None);
    }

    #[test]
    fn the_arguments_of_a_use_name_the_members() {
        let src =
            "@provider({ lifecycles = [ Lifecycle.Init, Lifecycle.Start ] })\nimpl S as\nend\n";
        let at = src.find("provider").expect("the name");
        assert_eq!(
            each_arguments(src, at, "lifecycles"),
            ["Init".to_string(), "Start".to_string()]
        );

        // The positional form holds the only list on the line.
        let src = "@provider([ \"Init\" ])\nimpl S as\nend\n";
        let at = src.find("provider").expect("the name");
        assert_eq!(each_arguments(src, at, "lifecycles"), ["Init".to_string()]);

        // A declaration is no use: nothing to read.
        let src = "attribute provider(lifecycles: Lifecycle[]) on impl as\nend\n";
        let at = src.find("provider").expect("the name");
        assert!(each_arguments(src, at, "lifecycles").is_empty());
    }

    /// At a use the clause becomes one line per member; at the
    /// declaration it stays the clause the author wrote.
    #[test]
    fn the_hover_expands_each_at_a_use_and_not_at_the_declaration() {
        let hover = "```alloy\n@provider(lifecycles: Lifecycle[])\n```\n\n**Requires**\n- `private function each lifecycles(self)`";
        let use_src =
            "@provider({ lifecycles = [ Lifecycle.Init, Lifecycle.Start ] })\nimpl S as\nend\n";
        let at = use_src.find("provider").expect("the name");
        assert!(
            expand_each(hover, use_src, at).ends_with(
                "**Requires**\n- `private function Init(self)`\n- `private function Start(self)`"
            ),
            "{}",
            expand_each(hover, use_src, at)
        );

        let decl = "attribute provider(lifecycles: Lifecycle[]) on impl as\nend\n";
        let at = decl.find("provider").expect("the name");
        assert_eq!(expand_each(hover, decl, at), hover);
    }
}
