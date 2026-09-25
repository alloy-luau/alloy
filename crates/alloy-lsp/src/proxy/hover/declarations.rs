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
        let Some(key) = declaration_key(&doc.source, start, end) else {
            return false;
        };

        // An attribute or a macro is keyed by its sigil; a bare name that
        // an import bound finds it that way.
        let sigils = [format!("@{key}"), format!("${key}")];
        // `local Point = 1` binds the name in this file, and
        // `public function Test()` declares one. Another file may hold a
        // `Point` or a `Test` of its own, and that declaration says
        // nothing about the name the caret sits on.
        let bound_here =
            binds_a_value(&doc.bindings, &key) || declares_a_name_at(&doc.source, start);
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
        // The index keys a namespace member by its whole path, and a
        // reader writes the path under the words its own file binds:
        // `Outer.Inner.T` here, `M.Ns.T` under a module binding. The
        // longest path answers first.
        let found = member_keys(&key, &doc.namespace_ranges, start)
            .iter()
            .find_map(|k| lookup(k))
            .or_else(|| sigils.iter().find_map(|k| lookup(k)));
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

            // `@M.tag` and `M.tag` read the module's attribute too.
            decls
                .iter()
                .find(|d| d.name == *name && declares_a_type(&d.hover))
                .or_else(|| decls.iter().find(|d| d.name == format!("@{name}")))
        });

        // `Light.Active` under `import { Status as Light }`: the path
        // names the enum, and the module keys its variant under the
        // enum's own name. Another module may declare a `Status` too, so
        // the path answers before the name does.
        let variant_decls = st.variant_home(uri, start).and_then(|(file, owner)| {
            let variant = key.rsplit('.').next()?;

            Some((
                alloy::declarations::summaries(&st.module_text(&file)?, false),
                format!("{owner}.{variant}"),
            ))
        });
        let found = variant_decls
            .as_ref()
            .and_then(|(decls, name)| decls.iter().find(|d| d.name == *name))
            .or(found);

        let Some(decl) = found else {
            return false;
        };

        // An attribute contract with an `each` clause reads best where
        // it is used: the arguments of this use name the members, so the
        // hover writes one line per member instead of the clause.
        let hover = expand_each(&decl.hover, &doc.source, start);
        let hover = with_member_methods(&hover, doc, &key);
        let hover = formatted_hover(&hover, &st.fmt_config(uri));
        // The source the declaration sits in: this file, another open
        // one, or a module an import reads.
        let home = std::iter::once(&doc.source)
            .chain(st.docs.values().map(|d| &d.source))
            .chain(doc.import_sources.iter())
            .find(|text| {
                let bare = decl.name.rsplit('.').next().unwrap_or(&decl.name);

                text.get(decl.offset..)
                    .is_some_and(|rest| rest.starts_with(bare))
                    && text[..decl.offset].ends_with(' ')
            });
        let hover = match home {
            Some(text) => with_derives(&hover, text, decl.offset),

            None => hover,
        };
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

        let Some(answer) = case_binding_text(doc, line as usize, start, &word, &known)
            .or_else(|| expression_binding_text(&doc.source, line as usize, &word))
        else {
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
in a block under the declaration.

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
    // and one that already carries an `impl` block needs none.
    let opens = lines
        .get(1)
        .map(|l| l.trim_start().trim_start_matches("export "))
        .is_some_and(|l| l.starts_with("struct ") || l.starts_with("enum "));
    let Some(end) = lines.iter().position(|l| *l == "end") else {
        return hover.to_string();
    };

    if !opens || lines.iter().any(|l| l.starts_with("impl ")) {
        return hover.to_string();
    }

    let methods = member_methods(doc, ns, name, path);

    if methods.is_empty() {
        return hover.to_string();
    }

    let mut block = vec![String::new(), format!("impl {path} as")];
    block.extend(methods.iter().map(|m| format!("    {m}")));
    block.push("end".to_string());
    lines.splice(end + 1..end + 1, block.iter().map(String::as_str));

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

/*
The name a caret asks the declaration index about: a macro or an
attribute under its sigil, a member under `Receiver.name`, and every
other word as itself. `None` where no declaration answers: a method
call, `obj:m`, or a `.` with no name in front of it.

The receiver stands on the caret's own line. A doc comment that ends in a
full stop sits right above a declaration, and a read across the line
break made `--- A round shape.` the receiver of the variant below it.
*/
fn declaration_key(source: &str, start: usize, end: usize) -> Option<String> {
    let word = &source[start..end];
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let before = source[line_start..start].trim_end();

    if source[..start].ends_with('$') || before.ends_with("macro") {
        return Some(format!("${word}"));
    }

    if source[..start].ends_with('@') || before.ends_with("attribute") {
        return Some(format!("@{word}"));
    }

    if let Some(head) = before.strip_suffix('.') {
        let at = line_start + head.len().saturating_sub(1);

        if head.is_empty() || !keywords::is_word_at(source, at) {
            return None;
        }

        let (mut hs, he) = keywords::word_range(source, at);

        // The whole path in front of the word: `Outer.Inner.T` names a
        // member two groups deep, and the index keys it by its path.
        while let Some(head) = source[line_start..hs].trim_end().strip_suffix('.') {
            let at = line_start + head.len().saturating_sub(1);

            if head.is_empty() || !keywords::is_word_at(source, at) {
                break;
            }

            hs = keywords::word_range(source, at).0;
        }

        return Some(format!("{}.{word}", &source[hs..he]));
    }

    // `obj:method`, not the `x: T` of an annotation.
    match source[..start].ends_with(':') {
        true => None,

        false => Some(word.to_string()),
    }
}

/// The keys a name at `at` answers to, longest first. `M.Ns.T` is the
/// member `Ns.T` of a module the file binds to `M`, and `Outer.Inner.T`
/// is keyed whole. The bare word is no key of a member: it would answer
/// a field read with any declaration of that spelling. A word inside a
/// namespace body is the group's own member, which the group's file
/// writes bare and the index keys under the path.
fn member_keys(key: &str, groups: &[alloy::declarations::NamespaceSpan], at: usize) -> Vec<String> {
    let mut out = vec![key.to_string()];
    let mut rest = key;

    while let Some((_, tail)) = rest.split_once('.')
        && tail.contains('.')
    {
        out.push(tail.to_string());
        rest = tail;
    }

    for ns in groups {
        if (ns.start..=ns.end).contains(&at) && ns.members.iter().any(|(m, _)| *m == key) {
            out.push(format!("{}.{key}", ns.path));
        }
    }

    out
}

/// Whether a line opens the `default` arm. The body may stand on the
/// same line, so the word alone and the word with a body both count.
fn opens_the_default_arm(text: &str) -> bool {
    text == "default" || text.starts_with("default ")
}

/// The binding an `if local` expression on the caret's line makes for
/// `word`, as the reader wrote it: `local b = g(5)`. The expression
/// form hoists its temp and substitutes the name, so the emit holds no
/// local the child could read. A statement-form `if local` opens the
/// line, and the child answers that one from its own local.
// ponytail: the source text alone; the temp's type needs a hover
// through the child under another name.
pub(crate) fn expression_binding_text(source: &str, line: usize, word: &str) -> Option<String> {
    let text = source.lines().nth(line)?;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;

    while let Some(i) = text[from..].find("local ") {
        let at = from + i;
        from = at + 1;
        let head = text[..at].trim_end();
        let head = head.strip_suffix("not").map_or(head, str::trim_end);
        let Some(before) = head
            .strip_suffix("if")
            .or_else(|| head.strip_suffix("elseif"))
        else {
            continue;
        };

        // The statement form, or a word that ends in `if`.
        if before.trim().is_empty() || before.ends_with(is_word) {
            continue;
        }

        let rest = &text[at + "local ".len()..];
        let name: String = rest.chars().take_while(|c| is_word(*c)).collect();

        if name != word {
            continue;
        }

        let (head, value) = rest.split_once('=')?;
        let value = value.split(" then").next()?.trim();

        return Some(format!("```alloy\nlocal {} = {value}\n```", head.trim()));
    }

    None
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
    known: &alloy::shapes::Known,
) -> Option<(usize, usize)> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let case_line = case_binding_line(&lines, line)?;
    let pattern = case_pattern(lines[case_line])?;

    // `case n then` binds the whole value, and no payload types it.
    if !pattern_bindings(
        &pattern,
        known,
        || array_element(&lines, case_line),
        &|_| Vec::new(),
    )
    .iter()
    .any(|(name, _, _)| name == word)
        && !crate::context::pattern_names(&pattern)
            .iter()
            .any(|l| l.name == word)
    {
        return None;
    }

    let at = offset_of(&doc.source, case_line as u32, 0)? + whole_word(lines[case_line], word)?;

    Some((at, at + word.len()))
}

/// The `match ... with` line the `case` at `case_line` belongs to. The
/// walk crosses a block that closed above whole, so a nested match of
/// an arm above the line is not the head.
fn match_head_line(lines: &[&str], case_line: usize) -> Option<usize> {
    let mut at = case_line.checked_sub(1)?;
    // The blocks the walk entered from below and has yet to leave.
    let mut inside = 0i32;

    loop {
        let text = lines.get(at)?.trim();

        if inside > 0 {
            inside = (inside + crate::context::block_closers(text)
                - crate::context::value_openers(text))
            .max(0);
        } else if text.ends_with(" with") {
            return Some(at);
        } else {
            inside = crate::context::block_closers(text);
        }

        at = at.checked_sub(1)?;
    }
}

/// The byte range of the `case` arm that opens at `case_line`: from that
/// line to the last one before the next `case`, the `default`, or the
/// `end` of the match. A block the arm opens is crossed whole, so a
/// nested match stays inside the arm.
fn case_arm_span(doc: &Doc, case_line: usize, lines: &[&str]) -> Option<(usize, usize)> {
    let depth_of =
        |text: &str| crate::context::value_openers(text) - crate::context::block_closers(text);
    let mut depth = depth_of(lines[case_line]).max(0);
    let mut last = case_line;

    for (at, raw) in lines.iter().enumerate().skip(case_line + 1) {
        let text = raw.trim();
        let leaves = text.starts_with("case ")
            || opens_the_default_arm(text)
            || crate::context::block_closers(text) > 0;

        if depth == 0 && leaves {
            break;
        }

        depth = (depth + depth_of(text)).max(0);
        last = at;
    }

    let start = offset_of(&doc.source, case_line as u32, 0)?;
    let end = offset_of(&doc.source, last as u32, 0)? + lines[last].len();

    Some((start, end))
}

/// The pattern of a let-else line: `local Build(model) = j else` gives
/// `Build(model) = j`. A `local x = if c then a else b` is no let-else.
fn let_else_pattern(text: &str) -> Option<&str> {
    let head = text.strip_prefix("export ").unwrap_or(text);
    let rest = head
        .strip_prefix("local ")
        .or_else(|| head.strip_prefix("const "))?;
    let pattern = rest
        .strip_suffix(" else")
        .or_else(|| rest.split_once(" else ").map(|(p, _)| p))?;

    (!pattern.contains(" then ")).then_some(pattern)
}

/// The let-else that binds `word` at `line`: the byte range of the name
/// in its pattern, and the byte range of the name's scope, from the
/// pattern line to the last line of the block that holds it. The emit
/// writes the declaration after the `end` of the else block, on another
/// line, so the child has no name of the reader's to point at.
pub(crate) fn let_else_binding(
    doc: &Doc,
    line: usize,
    word: &str,
) -> Option<((usize, usize), (usize, usize))> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let mut at = line.min(lines.len().checked_sub(1)?);
    // The blocks the walk entered from below and has yet to leave.
    let mut inside = 0i32;

    let decl = loop {
        let text = crate::context::code_of(lines.get(at)?).trim();
        let pattern = let_else_pattern(text);
        // The `else` of a let-else opens a block `value_openers` does
        // not count.
        let opens = crate::context::value_openers(text) + i32::from(pattern.is_some());
        inside = (inside + crate::context::block_closers(text) - opens).max(0);

        if inside == 0
            && let Some(pattern) = pattern
            && crate::context::pattern_names(pattern)
                .iter()
                .any(|l| l.name == word)
        {
            break at;
        }

        at = at.checked_sub(1)?;
    };

    let depth_of =
        |text: &str| crate::context::value_openers(text) - crate::context::block_closers(text);
    // The else block the declaration opens.
    let mut depth = 1 + depth_of(lines[decl]);
    let mut last = decl;

    for (at, raw) in lines.iter().enumerate().skip(decl + 1) {
        let text = crate::context::code_of(raw).trim();
        let sibling = text.starts_with("else")
            || text.starts_with("until")
            || text.starts_with("case ")
            || opens_the_default_arm(text);

        if depth + depth_of(text) < 0 || (depth == 0 && sibling) {
            break;
        }

        depth = (depth + depth_of(text)).max(0);
        last = at;
    }

    let name = offset_of(&doc.source, decl as u32, 0)? + whole_word(lines[decl], word)?;
    let start = offset_of(&doc.source, decl as u32, 0)?;
    let end = offset_of(&doc.source, last as u32, 0)? + lines[last].len();

    Some(((name, name + word.len()), (start, end)))
}

/// The arm that binds `word` at `line`, as a byte range: the arm the
/// line sits in, or one around it, up to the outermost match. The arm is
/// the whole scope of the name, so a rename and a reference list stop
/// there. The scope walk reads the same pattern, so the three answers
/// name one set of bindings.
pub(crate) fn case_arm_of_binding(doc: &Doc, line: usize, word: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let mut at = line;

    loop {
        let case_line = case_binding_line(&lines, at)?;
        let pattern = lines[case_line].trim().strip_prefix("case ")?;

        if crate::context::pattern_names(pattern)
            .iter()
            .any(|l| l.name == word)
        {
            return case_arm_span(doc, case_line, &lines);
        }

        // An inner arm binds something else. The name may still come
        // from the arm the inner match itself stands in.
        at = match_head_line(&lines, case_line)?.checked_sub(1)?;
    }
}

/// The hover of a `case` pattern's binding at `line`: the name with the
/// type the pattern gives it. `None` when the line is in no arm, or the
/// word is no binding of it.
pub(crate) fn case_binding_text(
    doc: &Doc,
    line: usize,
    start: usize,
    word: &str,
    known: &alloy::shapes::Known,
) -> Option<String> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let case_line = case_binding_line(&lines, line)?;
    let pattern = case_pattern(lines[case_line])?;
    // A struct's or a record's fields, from a declaration in reach or
    // from the record text itself.
    let fields = |ty: &str| {
        let ty = ty.trim().trim_end_matches('?');

        match ty.starts_with('{') {
            true => crate::context::record_entries(ty),

            false => doc
                .decls
                .iter()
                .chain(&doc.import_decls)
                .find(|d| d.name == ty)
                .map(|d| crate::context::record_entries(&d.hover))
                .unwrap_or_default(),
        }
    };
    let bindings = pattern_bindings(
        &pattern,
        known,
        || {
            array_element(&lines, case_line)
                .or_else(|| super::fields::element_of(&scrutinee_type(doc, case_line)?))
        },
        &fields,
    );

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

    if let Some((_, ty, owner)) = bindings.into_iter().find(|(n, _, _)| n == word) {
        return Some(format!(
            "```alloy\n{word}: {ty}\n```\nA binding of {owner}."
        ));
    }

    let head = lines[match_head_line(&lines, case_line)?].trim();

    // A name deeper in the pattern whose type the text does not say. An
    // expression match writes the path in its place, so the child would
    // answer for the text after it. The statement form keeps a local the
    // child types.
    if pattern != word {
        let bound = crate::context::pattern_names(&pattern)
            .iter()
            .any(|l| l.name == word);

        return (bound && !head.starts_with("match "))
            .then(|| format!("```alloy\n{word}\n```\nA binding of `case {pattern}`."));
    }

    // `case k where k > 5 then`: a bare name binds the whole value. An
    // expression match lowers to one expression with no local for the
    // name, so the child answers with the type of the arm's result.
    let scrutinee = match_value(head)?;
    let text = match scrutinee_type(doc, case_line) {
        Some(ty) => format!("{word}: {ty}"),

        // The statement form keeps a local the child types.
        None if head.starts_with("match ") => return None,

        None => word.to_string(),
    };

    Some(format!(
        "```alloy\n{text}\n```\nA binding of `match {scrutinee}`."
    ))
}

/// The value a `match` head reads: `n` of `local r = match n with`.
fn match_value(head: &str) -> Option<&str> {
    let at = head.find("match ")? + "match ".len();
    let value = head[at..].trim_end().strip_suffix("with")?.trim();

    Some(value.split(" as ").next().unwrap_or(value).trim())
}

/// The type of the value the `match` around `case_line` reads, from the
/// declaration of the name: its annotation, or the literal or the `new`
/// it starts from. An arm `case nil` above the line takes the nil out.
fn scrutinee_type(doc: &Doc, case_line: usize) -> Option<String> {
    let lines: Vec<&str> = doc.source.lines().collect();
    let head_line = match_head_line(&lines, case_line)?;
    let name = match_value(lines[head_line].trim())?;

    if !is_binding(name) {
        return None;
    }

    let at = offset_of(&doc.source, head_line as u32, 0)?;
    let ty = match crate::context::declared(&doc.source, at, name)? {
        crate::context::Declared::Annotation(t) => t,

        crate::context::Declared::Init(v) => literal_type(doc, &v)?,
    };
    let nil_arm = lines[head_line + 1..case_line]
        .iter()
        .any(|l| l.trim().starts_with("case nil"));

    Some(match nil_arm {
        true => ty.trim_end_matches('?').to_string(),

        false => ty,
    })
}

/// The type of a value a declaration starts from, when the text alone
/// says it: a number, a string, a boolean, a `new`, or an array of one
/// of those.
fn literal_type(doc: &Doc, value: &str) -> Option<String> {
    let v = value.trim();

    if let Some(rest) = v.strip_prefix("new ") {
        return super::restyle::constructed_type(doc, rest);
    }

    let first = v.chars().next()?;

    if first.is_ascii_digit() || (first == '-' && v[1..].starts_with(|c: char| c.is_ascii_digit()))
    {
        return Some("number".to_string());
    }

    if matches!(first, '"' | '\'' | '`') {
        return Some("string".to_string());
    }

    if matches!(v, "true" | "false") {
        return Some("boolean".to_string());
    }

    let inner = v
        .strip_prefix('{')
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| v.strip_prefix('[').and_then(|r| r.strip_suffix(']')))?;
    let item = split_top(inner).into_iter().next()?.trim();

    (!item.is_empty() && !item.contains('='))
        .then(|| literal_type(doc, item))
        .flatten()
        .map(|t| format!("{t}[]"))
}

/// A declaration hover with the `@derive(...)` lines the source writes
/// above it. The derives say what the type can do, `clone` and
/// `default`, and the index leaves the attribute lines out.
pub(crate) fn with_derives(hover: &str, source: &str, offset: usize) -> String {
    let line_start = source[..offset.min(source.len())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let mut derives: Vec<&str> = source[..line_start]
        .lines()
        .rev()
        .map(str::trim)
        .take_while(|l| l.starts_with('@') || l.starts_with("--"))
        .filter(|l| l.starts_with("@derive("))
        .collect();
    derives.reverse();

    match (derives.is_empty(), hover.split_once('\n')) {
        (false, Some((fence, body))) if fence.starts_with("```") => {
            format!("{fence}\n{}\n{body}", derives.join("\n"))
        }

        _ => hover.to_string(),
    }
}

/// A declaration hover laid out the way `alloy fmt` writes the source.
/// The index writes every header with `as` and indents by four spaces;
/// fmt drops the `as` of a header whose body stands below it and
/// indents by the project's unit. A one-line body keeps its `as`.
pub(crate) fn formatted_hover(hover: &str, fmt: &alloy::config::FmtConfig) -> String {
    let unit = match fmt.indent_type {
        alloy::config::IndentType::Tabs => "\t".to_string(),

        alloy::config::IndentType::Spaces => " ".repeat(fmt.indent_width),
    };
    let lines: Vec<&str> = hover.split('\n').collect();
    // The index may write two spaces or four; its smallest indent is
    // one level.
    let step = lines
        .iter()
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .filter(|n| *n > 0)
        .min()
        .unwrap_or(4);
    let mut code = false;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("```") {
            code = !code;
            out.push(line.to_string());

            continue;
        }

        if !code {
            out.push(line.to_string());

            continue;
        }

        let spaces = line.len() - line.trim_start_matches(' ').len();
        let text = &line[spaces - spaces % step..];
        let body_below = lines
            .get(i + 1)
            .is_some_and(|next| !next.starts_with("```"));
        let text = match text.strip_suffix(" as") {
            Some(head) if body_below => head,

            _ => text,
        };

        out.push(format!("{}{text}", unit.repeat(spaces / step)));
    }

    out.join("\n")
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
/// arm's `then`, or the guard's `where` or `and`.
pub(crate) fn case_pattern(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("case ")?;
    let end = [rest.find(" then"), rest.find(" where "), rest.find(" and ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());

    Some(rest[..end].trim().to_string())
}

/// The names a pattern binds, each with its type and what it comes
/// from, at any depth. A payload reads its type off the enum's
/// declaration, a field off what `fields` reads for a struct or a
/// record, and an array pattern the element type of what it runs over.
pub(crate) fn pattern_bindings(
    pattern: &str,
    known: &alloy::shapes::Known,
    element: impl Fn() -> Option<String>,
    fields: &dyn Fn(&str) -> Vec<crate::context::Field>,
) -> Vec<(String, String, String)> {
    // The top of an array pattern reads the element of what the match
    // runs over.
    let top = match pattern.trim_start().starts_with('[') {
        true => element().map(|e| format!("{e}[]")),

        false => None,
    };
    let mut out = Vec::new();
    typed_names(pattern, top.as_deref(), "", known, fields, &mut out);

    out
}

/// The names of one pattern that `expected` types, pushed with what
/// each comes from. `owner` names what a bare name at this level reads.
fn typed_names(
    text: &str,
    expected: Option<&str>,
    owner: &str,
    known: &alloy::shapes::Known,
    fields: &dyn Fn(&str) -> Vec<crate::context::Field>,
    out: &mut Vec<(String, String, String)>,
) {
    let sides = crate::context::alternatives(text.trim());

    // `Sword(n) or Wand(n)`: each side types the name, and the union of
    // the sides is its type.
    if let [first, rest @ ..] = sides.as_slice()
        && !rest.is_empty()
    {
        let mut merged: Vec<(String, String, String)> = Vec::new();
        typed_names(first, expected, owner, known, fields, &mut merged);

        for side in rest {
            let mut more = Vec::new();
            typed_names(side, expected, owner, known, fields, &mut more);

            for (name, ty, from) in more {
                let Some((_, have, owners)) = merged.iter_mut().find(|(n, ..)| *n == name) else {
                    continue;
                };

                if !have.split(" | ").any(|t| t == ty) {
                    *have = format!("{have} | {ty}");
                }

                if !owners.split(" or ").any(|o| o == from) {
                    *owners = format!("{owners} or {from}");
                }
            }
        }

        out.extend(merged);

        return;
    }

    let t = sides[0];

    if is_binding(t) {
        if let Some(ty) = expected {
            out.push((t.to_string(), ty.trim().to_string(), owner.to_string()));
        }

        return;
    }

    let (Some(open), Some(close)) = (t.find(['(', '[', '{']), t.rfind([')', ']', '}'])) else {
        return;
    };

    if close <= open {
        return;
    }

    let head = t[..open].trim();
    let parts = split_top(&t[open + 1..close]);

    match t.as_bytes()[open] {
        b'(' => {
            let Some((enum_name, variant, payload)) = variant_payload(head, expected, known) else {
                return;
            };
            let owner = format!("`{enum_name}.{variant}`");

            for (k, part) in parts.into_iter().enumerate() {
                typed_names(
                    part,
                    payload.get(k).map(String::as_str),
                    &owner,
                    known,
                    fields,
                    out,
                );
            }
        }

        b'[' => {
            let elem = expected.and_then(super::fields::element_of);
            let owner = "the array pattern";

            for part in parts {
                match part.trim().strip_prefix("...") {
                    Some(rest) if is_binding(rest) => {
                        if let Some(elem) = &elem {
                            out.push((rest.to_string(), format!("{elem}[]"), owner.into()));
                        }
                    }

                    _ => typed_names(part, elem.as_deref(), owner, known, fields, out),
                }
            }
        }

        _ => {
            let ty = match head.is_empty() {
                true => expected.unwrap_or_default().trim().trim_end_matches('?'),

                false => head,
            };
            let declared = fields(ty);

            for part in parts {
                let (field, sub) = match part.split_once('=') {
                    Some((f, sub)) => (f.trim(), sub),

                    None => (part.trim(), part),
                };
                let field_ty = declared
                    .iter()
                    .find(|f| f.name == field)
                    .map(|f| f.ty.as_str());
                let owner = format!("field `{field}` of `{ty}`");
                typed_names(sub, field_ty, &owner, known, fields, out);
            }
        }
    }
}

/// The enum, the variant, and the payload types a variant pattern's head
/// names. `Item.Sword` picks the enum by its name, and a bare `Sword` or
/// an alias the first enum with that variant. A generic payload reads
/// the arguments of `expected`: `Some(v)` against `Opt<Item>` types `v`
/// as `Item`.
fn variant_payload(
    head: &str,
    expected: Option<&str>,
    known: &alloy::shapes::Known,
) -> Option<(String, String, Vec<String>)> {
    let (path, variant) = head.rsplit_once('.').unwrap_or(("", head));
    let lookup = |named: bool| {
        known.shapes.iter().find_map(|s| {
            let alloy::declarations::Shape::Enum {
                name,
                generics,
                variants,
            } = s
            else {
                return None;
            };

            if named && name != path && !name.ends_with(&format!(".{path}")) {
                return None;
            }

            let (_, payload) = variants.iter().find(|(v, _)| v == variant)?;

            Some((name, generics, payload))
        })
    };
    let (name, generics, payload) = (!path.is_empty())
        .then(|| lookup(true))
        .flatten()
        .or_else(|| lookup(false))?;

    // `Opt<Item>` gives each parameter of `enum Opt<T>` its argument.
    let args = expected
        .and_then(|e| {
            e.trim()
                .strip_prefix(name.rsplit('.').next().unwrap_or(name))
        })
        .and_then(|rest| rest.trim().strip_prefix('<')?.strip_suffix('>'))
        .map(alloy::shapes::top_level_parts)
        .unwrap_or_default();
    let payload = payload
        .iter()
        .map(|ty| {
            generics.iter().zip(&args).fold(ty.clone(), |ty, (g, arg)| {
                let g = g.split('=').next().unwrap_or(g).trim();

                replace_word(&ty, g, arg.trim())
            })
        })
        .collect();

    Some((name.clone(), variant.to_string(), payload))
}

/// `text` with every whole word `word` replaced by `with`.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut last = 0;

    for (at, _) in text.match_indices(word) {
        if !text[..at].ends_with(is_word) && !text[at + word.len()..].starts_with(is_word) {
            out.push_str(&text[last..at]);
            out.push_str(with);
            last = at + word.len();
        }
    }

    out.push_str(&text[last..]);

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
///
/// A namespace is a group of types, and the emit flattens it, so the
/// child prints the table of tables the group lowers to. The group's
/// own block is what the source wrote.
fn declares_a_type(hover: &str) -> bool {
    let Some(line) = hover.lines().nth(1) else {
        return false;
    };
    let line = line.trim_start().trim_start_matches("export ");

    [
        "struct ",
        "enum ",
        "trait ",
        "interface ",
        "type ",
        "namespace ",
    ]
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
    /// A name an `if local` expression binds has no local in the emit,
    /// so the hover reads the binding from the source. The statement
    /// form opens its line, and the child answers that one.
    #[test]
    fn an_if_local_expression_binding_hovers_as_written() {
        let src = "if local a = f2(if local b: number = g(5) then b else 0) then\nlocal x = if not local c = h() then 0 else c\n";

        assert_eq!(
            super::expression_binding_text(src, 0, "b").as_deref(),
            Some("```alloy\nlocal b: number = g(5)\n```")
        );
        assert_eq!(super::expression_binding_text(src, 0, "a"), None);
        assert_eq!(
            super::expression_binding_text(src, 1, "c").as_deref(),
            Some("```alloy\nlocal c = h()\n```")
        );
        assert_eq!(super::expression_binding_text(src, 1, "x"), None);
    }

    /// A declaration hover reads the way `alloy fmt` writes it: no `as`
    /// over a body below, the project's indent, and a one-line body kept.
    #[test]
    fn a_declaration_hover_takes_the_fmt_layout() {
        let fmt = alloy::config::FmtConfig::default();
        let hover = "```alloy\nexport enum Phase as\n    Lobby\n    Countdown(number)\nend\n\nimpl Contestant as\n    public function add(self, n: number)\nend\n```\n\nA doc that says as";

        assert_eq!(
            super::formatted_hover(hover, &fmt),
            "```alloy\nexport enum Phase\n  Lobby\n  Countdown(number)\nend\n\nimpl Contestant\n  public function add(self, n: number)\nend\n```\n\nA doc that says as"
        );
        assert_eq!(
            super::formatted_hover("```alloy\nenum Team as Red, Blue end\n```", &fmt),
            "```alloy\nenum Team as Red, Blue end\n```"
        );

        // An index that writes two spaces reads at the project's width.
        let four = alloy::config::FmtConfig {
            indent_width: 4,
            ..fmt
        };
        assert_eq!(
            super::formatted_hover("```alloy\nstruct P\n  x: number\nend\n```", &four),
            "```alloy\nstruct P\n    x: number\nend\n```"
        );
    }

    /// A struct's hover carries the derives the source writes above it.
    #[test]
    fn a_struct_hover_names_its_derives() {
        let src = "-- Stats.\n@derive(Default, Clone)\n@rename_all(\"camelCase\")\nstruct Stats\n  level: number\nend\n";
        let at = src.find("Stats\n").unwrap();

        assert_eq!(
            super::with_derives("```alloy\nstruct Stats\n  level: number\nend\n```", src, at),
            "```alloy\n@derive(Default, Clone)\nstruct Stats\n  level: number\nend\n```"
        );
        assert_eq!(
            super::with_derives("```alloy\nstruct P\nend\n```", "struct P\nend\n", 7),
            "```alloy\nstruct P\nend\n```"
        );
    }

    /// `where` is the guard word, as `and` is, so it ends the pattern and
    /// the payload binding hovers with its type.
    #[test]
    fn a_where_guard_ends_the_case_pattern() {
        let line = "    case Phase.Countdown(n) where n > 0 then Phase.Countdown(n - 1)";

        assert_eq!(
            super::case_pattern(line).as_deref(),
            Some("Phase.Countdown(n)")
        );
        assert_eq!(
            super::case_pattern("case n and n > 0 then 1").as_deref(),
            Some("n")
        );
    }

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

    /// A doc comment that ends in a full stop stands right above the
    /// name the caret is on. The dot of the sentence is no member
    /// access, so the variant below it still reads as its own.
    #[test]
    fn a_doc_comment_s_full_stop_is_no_receiver() {
        const SRC: &str = "--- A shape.\nenum Shape as\n    --- A round shape.\n    Circle\nend\n";
        let at = SRC.find("    Circle").expect("the variant") + 4;
        // The key was `shape.Circle`, the last word of the sentence met
        // with the name, and no declaration stands under it.
        assert_eq!(
            declaration_key(SRC, at, at + "Circle".len()),
            Some("Circle".to_string())
        );

        // A receiver on the caret's own line still names the member, so
        // a variant in a `case` pattern reads as the variant and not as
        // the enum in front of it.
        for src in ["print(Shape.Circle)\n", "    case Shape.Circle then\n"] {
            let at = src.find("Circle").expect("the variant");
            assert_eq!(
                declaration_key(src, at, at + "Circle".len()),
                Some("Shape.Circle".to_string())
            );
        }
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
    use super::{
        declaration_key, declares_a_type, each_arguments, each_clause, expand_each, member_keys,
    };

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

    /// The emit flattens a namespace into a table of its members, so
    /// the child prints that table for `M.Ns`. The group's own block is
    /// the declaration the source wrote.
    #[test]
    fn a_namespace_hover_declares_a_type() {
        assert!(declares_a_type(
            "```alloy\nexport namespace Ns as\n    public struct T\nend\n```"
        ));
        assert!(!declares_a_type("```alloy\nfunction make(): number\n```"));
    }

    /// The index keys a namespace member by its whole path. A reader
    /// writes that path under the words its own file binds, so the
    /// caret's key holds every word in front of the name.
    #[test]
    fn a_member_two_groups_deep_keys_by_its_path() {
        let src = "local z: Outer.Inner.T = new Outer.Inner.T { value = 9 }\n";
        let at = src.find("Inner.T").expect("the path") + "Inner.".len();

        assert_eq!(
            declaration_key(src, at, at + 1).as_deref(),
            Some("Outer.Inner.T")
        );
        assert_eq!(
            member_keys("Outer.Inner.T", &[], 0),
            ["Outer.Inner.T", "Inner.T"].map(String::from)
        );
        // `M.Ns.T` under a module binding: the index holds `Ns.T`.
        assert_eq!(
            member_keys("M.Ns.T", &[], 0),
            ["M.Ns.T", "Ns.T"].map(String::from)
        );
        // A field read stops at its own key: the bare word would
        // answer with any declaration of that spelling.
        assert_eq!(
            member_keys("p.value", &[], 0),
            ["p.value"].map(String::from)
        );
    }

    /// The group's own file writes a member bare, and the index keys it
    /// under the path, so the caret inside the body asks for both.
    #[test]
    fn a_member_inside_its_own_group_asks_under_the_path() {
        let src = "export namespace Ns as\n    struct T as\n        value: number,\n    end\n\n    function make(): T\n        return new T { value = 0 }\n    end\nend\n\nlocal T = 1\n";
        let groups = alloy::declarations::namespace_ranges(src);
        let inside = src.find(": T").expect("the return type") + 2;
        let outside = src.rfind('T').expect("the local");

        assert_eq!(
            member_keys("T", &groups, inside),
            ["T", "Ns.T"].map(String::from)
        );
        assert_eq!(member_keys("T", &groups, outside), ["T"].map(String::from));
        // A word the group never declares keeps its own key.
        assert_eq!(
            member_keys("other", &groups, inside),
            ["other"].map(String::from)
        );
    }
}
