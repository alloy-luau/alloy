//! Hover: field, module, and declaration hovers, and the name a printed type reads by.

mod declarations;
mod fields;
mod impls;
mod members;
mod modules;
mod restyle;

pub(crate) use declarations::case_binding_text;
pub(crate) use fields::{
    declared_field_hover, declared_parameter_hover, declared_type_parameters_of, field_key,
    foreign_method_hover, function_name_of, remote_parameter_hover, used_field_hover,
};
pub(crate) use members::{attach_std_member_docs, std_member_hover, std_receiver};
pub(crate) use modules::{
    global_declaration_hover, global_owner, import_spec, module_hover, remote_spec, service_hover,
};

#[cfg(test)]
pub(crate) use modules::const_hover as const_hover_of;
#[cfg(test)]
pub(crate) use modules::shadows_an_import;
pub(crate) use restyle::{
    close_empty_packs, close_item_packs, declared_annotation, declared_signature,
    drop_bound_intersections, empty_parameter_names, fold_std_shapes, invents_a_type,
    is_byte_count, keep_annotation, lowers_a_block, name_by_declaration, name_method_receiver,
    name_solver_variable, name_trait_method, names_a_key, prefer_constructed_struct,
    restates_itself, restore_struct_arguments, restyle_global_hover, restyle_hover, source_type,
    unlocal_parameter,
};

use super::completion::{lands_on_member, member_position, sep_of};
use super::diagnostics::names_word;
use super::documents::project_aliases;
use super::*;
pub(crate) use crate::names::{builtin_attribute_targets, declares_a_name_at};

impl Server {
    /// Answers a hover on bytes the desugar replaced: an Alloy keyword or
    /// operator gets its own text, other punctuation gets nothing. A word
    /// with no entry, such as a hoisted name, still goes to the child.
    /// Returns false when the child should answer.
    /// A hover on the name of a struct, interface, enum, or trait shows
    /// the declaration as the source wrote it. The child would show a
    /// table or a type alias. This file's declarations win; then any
    /// open file's, since an import brings the name in unchanged.
    /// The shadow position of the word a hover asks about, when the
    /// mapped position lands off it. `None` when the map is already
    /// right, or the cursor is on no word.
    pub(crate) fn hover_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let Caret { start, end, .. } = Caret::at(&doc.source, line, character)?;
        let word = &doc.source[start..end];
        let (sl, sc) = doc.to_shadow(line, character);
        let landed = doc
            .shadow
            .lines()
            .nth(sl as usize)
            .and_then(|l| offset_of(l, 0, sc).map(|b| (l, b)))
            .is_some_and(|(l, b)| {
                keywords::is_word_at(l, b) && {
                    let (ws, we) = keywords::word_range(l, b);

                    &l[ws..we] == word
                }
            });

        match landed {
            true => None,

            false => shadow_home(&doc.shadow, line, word),
        }
    }

    /// The shadow position a member completion belongs at. `a?.b` and
    /// `a!.b` lower to text the compiler wrote, and `await X.m()` moves
    /// the receiver into a call, so the member the author is typing maps
    /// nowhere; the member the lowering wrote is the one with a type
    /// behind it. A plain access the child already reads keeps its own
    /// position.
    pub(crate) fn member_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let source_line = doc.source.lines().nth(line as usize)?;
        // A field default moves to the line the struct's header takes,
        // so the member's line is the one the map reports, not this one.
        let (shadow_line_no, _) = doc.to_shadow(line, character);
        let shadow_line = doc.shadow.lines().nth(shadow_line_no as usize)?;
        let Some((base, access, sep, prefix)) = context::member_at(&doc.source, offset) else {
            // A call before the guard, `f(x)?:m()`: the lowering binds
            // the value to a name of its own, so no receiver of the
            // source is on the lowered line.
            let column = context::guarded_member_column(
                &doc.source[line_start..offset],
                shadow_line,
                sep_of(&doc.source, offset)?,
            )?;

            return Some((shadow_line_no, shadow_line[..column].chars().count() as u32));
        };

        if access == context::Access::Plain
            && lands_on_member(doc, line, character, &base, sep, prefix)
        {
            return None;
        }

        // An index before the guard, `xs[1]?.m`, binds to a name of the
        // lowering's own the way a call does, so the receiver of the
        // source is not on the lowered line either.
        let column = context::member_column(
            source_line,
            shadow_line,
            &base,
            access,
            sep,
            prefix,
            offset - line_start,
        )
        .or_else(|| {
            context::guarded_member_column(&doc.source[line_start..offset], shadow_line, sep)
        })?;

        Some((shadow_line_no, shadow_line[..column].chars().count() as u32))
    }

    /// The shadow position a hover on a guarded index belongs at.
    /// `mo?[k]` lowers to `(if mo == nil then nil else mo[k])` and
    /// `mo![k]` to `(if mo == nil then error(..) else mo)[k]`, so the
    /// bracket the author wrote has no position of its own; the
    /// bracket the lowering wrote reads the element.
    pub(crate) fn index_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;
        let line_start = doc.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let (base, access, at) = context::index_at(&doc.source, offset)?;
        let source_line = doc.source.lines().nth(line as usize)?;
        let (shadow_line_no, _) = doc.to_shadow(line, character);
        let shadow_line = doc.shadow.lines().nth(shadow_line_no as usize)?;
        // The column right after the lowered `[`; the caret belongs on
        // the bracket itself, which is the byte before it.
        let column = context::member_column(
            source_line,
            shadow_line,
            &base,
            access,
            '[',
            0,
            at + 1 - line_start,
        )?;

        Some((
            shadow_line_no,
            shadow_line[..column - 1].chars().count() as u32,
        ))
    }

    /// The shadow position an expression completion belongs at. The emit
    /// qualifies a std name, `Ok(v)` to `__alloy.Ok(v)`, so a caret at
    /// that name maps past a `.` the source never wrote and the child
    /// lists the runtime's own table. The scope answers where the
    /// qualifier starts.
    pub(crate) fn expression_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;

        if member_position(doc, line, character).is_some() {
            return None;
        }

        let (sl, sc) = doc.to_shadow(line, character);
        let text = doc.shadow.lines().nth(sl as usize)?;
        let at = offset_of(text, 0, sc)?;
        // The map lands on the qualified name, at either end of it.
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let head = text[..at.min(text.len())].trim_end_matches(word);
        let qualifier = head.strip_suffix('.')?;
        let start = qualifier.trim_end_matches(word).len();

        // Only the runtime's own table: a temporary the desugar bound,
        // `_1`, stands for an expression the source did write.
        qualifier[start..]
            .starts_with("__alloy")
            .then(|| (sl, text[..start].chars().count() as u32))
    }

    pub(crate) fn keyword_hover(&self, uri: &str, message: &Value, id: &Value) -> bool {
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

        if doc.mapping().is_none() {
            return false;
        }

        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        // A copied byte has a shadow position; the child answers there.
        if doc.maps_to_shadow(offset) {
            return false;
        }

        // A std name the file binds itself, through an import or a
        // declaration, is the file's: the child answers for that one.
        let hit = keywords::hover(&doc.source, offset).filter(|(start, end, _)| {
            let word = &doc.source[*start..*end];
            let is_std = alloy::desugar::AMBIENT.contains(&word)
                || matches!(
                    word,
                    "SignalConnection" | "Signalish" | "Partial" | "Readonly" | "Sink"
                );

            // A `:` opens a type far more often than it closes a
            // ternary; the `?` is what makes it the else.
            if word == ":" && !ternary_else_at(&doc.source, *start) {
                return false;
            }

            // A bracket that indexes a value opens no array literal,
            // and the caret on it asks for the element the index
            // answers. The `?` or `!` in front still reads as itself.
            if matches!(word, "[" | "?[")
                && offset == *end - 1
                && indexes_a_value(&doc.source, *end - 1)
            {
                return false;
            }

            !(is_std && doc_binds(doc, word))
        });

        let result = match hit {
            Some((start, end, text)) => {
                let (sl, sc) = position_of(&doc.source, start);
                let (el, ec) = position_of(&doc.source, end);
                // A std type: the overview, then the names a reader can
                // hover on their own.
                let text = alloy::docs::type_markdown(&doc.source[start..end])
                    .unwrap_or_else(|| text.to_string());

                json!({
                    "contents": { "kind": "markdown", "value": text },
                    "range": {
                        "start": { "line": sl, "character": sc },
                        "end": { "line": el, "character": ec },
                    },
                })
            }

            // A replaced word such as a struct name has a home in the
            // shadow: on the same line when the emit kept it there, else
            // where the declaration landed. The child answers there.
            None if keywords::is_word_at(&doc.source, offset) => {
                let (start, end) = keywords::word_range(&doc.source, offset);
                let word = &doc.source[start..end];

                let Some(target) = shadow_home(&doc.shadow, line, word) else {
                    return false;
                };

                drop(st);
                self.forward_request_at(message.clone(), Some("textDocument/hover"), target);

                return true;
            }

            None => Value::Null,
        };

        drop(st);
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

        true
    }

    /// Whether the caret sits in markup the shadow blanked, which
    /// happens when the markup could not lower.
    pub(crate) fn stands_in_blanked_markup(&self, uri: &str, message: &Value) -> bool {
        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let st = self.state.lock().expect("state");

        st.docs.get(uri).is_some_and(|doc| {
            offset_of(&doc.source, line, character).is_some_and(|at| doc.in_blanked_markup(at))
        })
    }

    /// Answers a hover or completion inside `.alx` markup. Returns false
    /// when the cursor is not on markup, so the child answers.
    pub(crate) fn markup_answer(
        &self,
        method: &str,
        uri: &str,
        message: &Value,
        id: &Value,
    ) -> bool {
        let Some((line, character)) = position_of_message(message) else {
            return false;
        };
        let st = self.state.lock().expect("state");
        let Some(doc) = st.docs.get(uri) else {
            return false;
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };
        let bound = markup_bound(&doc.source);
        let load = |spec: &str| st.module_source(uri, spec);

        let result = match method {
            "textDocument/hover" => match markup::hover_spot(&doc.source, offset) {
                Some(spot) => {
                    let member = match &spot {
                        markup::Spot::Tag { name } if name.contains('.') => {
                            let path: Vec<&str> = name.split('.').collect();

                            components::member_at(&doc.source, &path, &load)
                        }

                        _ => None,
                    };

                    markup::hover(&spot, &bound, member.as_ref()).unwrap_or(Value::Null)
                }

                None => return false,
            },

            _ => match markup::completion_spot(&doc.source, offset) {
                Some(spot) => {
                    let props = st.ingot_props(uri);
                    // A dotted tag reaches the members of the path in
                    // front of its last `.`; a bare one reaches every
                    // name of the file that holds a component.
                    let reach = match &spot {
                        markup::Spot::TagSlot { prefix } => match prefix.rsplit_once('.') {
                            Some((holder, _)) => {
                                let path: Vec<&str> = holder.split('.').collect();

                                components::members(&doc.source, &path, &load)
                            }

                            None => components::containers(&doc.source),
                        },

                        _ => Vec::new(),
                    };

                    Value::Array(markup::completions(
                        &spot,
                        &bound,
                        &doc.source,
                        &props,
                        &reach,
                    ))
                }

                None => return false,
            },
        };

        drop(st);
        self.respond(id, result);

        true
    }
}

impl State {
    /// The source of the module a spec names: an open document first,
    /// then the file on disk. A `.alx` in another folder is open only
    /// when the author has it in a tab, so the disk answers for the
    /// rest.
    pub(crate) fn module_source(&self, uri: &str, spec: &str) -> Option<String> {
        let target = imports::module_path(&self.resolve_spec(uri, spec)?);

        for (u, d) in &self.docs {
            if uri_to_path(u).is_some_and(|p| imports::module_path(&p) == target) {
                return Some(d.source.clone());
            }
        }

        std::fs::read_to_string(imports::module_file(&target)?).ok()
    }
}

/// Maps positions and ranges in request params into the shadow.
/// The shadow position of `word` for a hover: the same line first, then
/// the first line that holds it as a whole word.
pub(crate) fn shadow_home(shadow: &str, line: u32, word: &str) -> Option<(u32, u32)> {
    let lines: Vec<&str> = shadow.lines().collect();
    let same = lines.get(line as usize).copied().unwrap_or("");

    let (l, text, byte) = keywords::find_word(same, word)
        .map(|c| (line, same, c))
        .or_else(|| {
            lines
                .iter()
                .enumerate()
                .find_map(|(i, l)| keywords::find_word(l, word).map(|c| (i as u32, *l, c)))
        })?;

    Some((l, text[..byte].chars().count() as u32))
}

/// Whether the file binds the name itself: a declaration, a local, a
/// function, or an import. A std name so bound belongs to the file.
pub(crate) fn doc_binds(doc: &Doc, name: &str) -> bool {
    doc.decls.iter().any(|d| d.name == name)
        || doc.bindings.iter().any(|b| b.name == name)
        || imports::bound_names(&doc.source).iter().any(|n| n == name)
}

/// The struct a method's `self` belongs to: the nearest `impl` above
/// the line, with the type parameters the struct declares.
pub(crate) fn impl_self_type(doc: &Doc, line: u32) -> Option<String> {
    let head = doc
        .source
        .lines()
        .take(line as usize + 1)
        .filter_map(|l| {
            let text = l.trim_start();
            // A foreign impl is exported, so it works project wide.
            let text = text.strip_prefix("export ").unwrap_or(text);

            text.strip_prefix("impl ").map(str::to_string)
        })
        .last()?;
    // `impl Shape for Sq` names the struct after `for`.
    let named = head.split(" for ").last().unwrap_or(&head).trim();
    let name: String = named
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let generics = struct_generics(&doc.source, &name);

    (!name.is_empty()).then(|| format!("{name}{generics}"))
}

/// The type parameters of `struct Name<T, U>`, as the source writes
/// them; an empty text when the struct takes none.
pub(crate) fn struct_generics(source: &str, name: &str) -> String {
    let needle = format!("struct {name}<");
    let Some(i) = source.find(&needle) else {
        return String::new();
    };
    let rest = &source[i + needle.len()..];
    let Some(end) = rest.find('>') else {
        return String::new();
    };
    let params: Vec<&str> = rest[..end]
        .split(',')
        .map(|p| p.split(':').next().unwrap_or("").trim())
        .filter(|p| !p.is_empty())
        .collect();

    match params.is_empty() {
        true => String::new(),

        false => format!("<{}>", params.join(", ")),
    }
}

/// The hover of a struct field at its declaration: the line as written,
/// under the struct it belongs to, with the comment above it. `None`
/// when the word is not a field of a struct body.
/// Whether the `:` at an offset closes a ternary: a `?` stands before
/// it on the same line, outside every string. `x: T` writes a type
/// with the same byte.
pub(crate) fn ternary_else_at(source: &str, at: usize) -> bool {
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut quote: Option<char> = None;

    for c in source[line_start..at].chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,

            (Some(_), _) => {}

            (None, '"' | '\'' | '`') => quote = Some(c),

            (None, '?') => return true,

            _ => {}
        }
    }

    false
}

/// The type of `x?[k]` reads `T?`: the guard answers nil, and the
/// child sees only the index inside it. An asserted index needs
/// nothing, since `!` already dropped the `?`.
pub(crate) fn optional_index_hover(
    text: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<String> {
    let offset = offset_of(&doc.source, line, character)?;

    if context::index_at(&doc.source, offset)?.1 != context::Access::Optional {
        return None;
    }

    let body = text.strip_prefix("```alloy\n")?.strip_suffix("\n```")?;

    (!body.contains('\n') && !body.ends_with('?')).then(|| format!("```alloy\n{body}?\n```"))
}

/// Whether the `[` at `at` indexes a value: something stands before
/// it that an index reads. A `[` that opens an array literal follows
/// an operator, a comma, or nothing at all.
pub(crate) fn indexes_a_value(source: &str, at: usize) -> bool {
    let head = source[..at]
        .strip_suffix(['?', '!'])
        .unwrap_or(&source[..at]);

    head.chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ')' | ']' | '"' | '\'' | '`'))
}

/// The built-in attributes that read with nothing under the caret.
/// Luau takes these before the reader writes the function, and each
/// one says something on its own. Every other built-in names a target:
/// a wire width, a remote attribute, `@derive`, `@sealed`, `@test`, and
/// `@cfg` all need the declaration in front of them to mean anything.
pub(crate) const OPEN_ATTRIBUTES: &[&str] = &["@native", "@checked", "@deprecated"];

/// The targets a declared attribute's hover names:
/// `**Applies to** \`field\` · \`struct\``.
pub(crate) fn declared_attribute_targets(hover: &str) -> Vec<&str> {
    hover
        .lines()
        .find_map(|l| l.strip_prefix("**Applies to** "))
        .map(|list| {
            list.split(" · ")
                .map(|t| t.trim().trim_matches('`'))
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The width of a line, in UTF-16 units.
/// The zero-based UTF-16 offset a range carries as a one-based byte
/// column, which is what the compiler's rewrites read.
pub(crate) fn byte_column(doc: &Doc, line: u32, offset: u32) -> usize {
    let Some(text) = doc.source.lines().nth(line as usize) else {
        return offset as usize + 1;
    };
    let mut units = 0;

    for (at, c) in text.char_indices() {
        if units >= offset {
            return at + 1;
        }

        units += c.len_utf16() as u32;
    }

    text.len() + 1
}

/// A one-based byte column of a line as the zero-based UTF-16 offset a
/// range carries.
pub(crate) fn utf16_column(doc: &Doc, line: u32, col: usize) -> u32 {
    doc.source
        .lines()
        .nth(line as usize)
        .and_then(|l| l.get(..col.saturating_sub(1)))
        .map(|head| head.encode_utf16().count() as u32)
        .unwrap_or(col.saturating_sub(1) as u32)
}

/// The width of the name that starts at a UTF-16 offset, in UTF-16
/// units; a range with no name under it stays one unit wide.
pub(crate) fn word_width(doc: &Doc, line: u32, start: u32) -> u32 {
    let width = doc
        .source
        .lines()
        .nth(line as usize)
        .map(|l| {
            l.chars()
                .skip(start as usize)
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .map(|c| c.len_utf16() as u32)
                .sum::<u32>()
        })
        .unwrap_or(0);

    width.max(1)
}

pub(crate) fn impl_width(doc: &Doc, line: u32) -> u32 {
    doc.source
        .lines()
        .nth(line as usize)
        .map(|l| l.trim_end().encode_utf16().count() as u32)
        .unwrap_or(1)
}

/// The struct whose `impl` writes a method, when one alone writes it.
pub(crate) fn method_owner(source: &str, method: &str) -> Option<String> {
    let mut owner: Option<String> = None;
    let mut found: Option<String> = None;

    for line in source.lines() {
        let text = line.trim();

        if let Some(rest) = text.strip_prefix("impl ") {
            let named = rest.split(" for ").last().unwrap_or(rest).trim();
            owner = Some(
                named
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect(),
            );

            continue;
        }

        // An impl body is indented; a line at the margin closes it. A
        // blank line has no margin and closes nothing.
        if !text.is_empty() && !line.starts_with([' ', '\t']) && text != "end" {
            owner = None;
        }

        let Some(head) = owner.as_ref() else {
            continue;
        };
        let Some(rest) = text.strip_prefix("function ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();

        if name == method && rest[name.len()..].starts_with("(self") {
            match &found {
                Some(other) if other != head => return None,

                _ => found = Some(head.clone()),
            }
        }
    }

    found
}

/// An arity message with the receiver taken out of both counts.
pub(crate) fn without_self(message: &str) -> Option<String> {
    const HEAD: &str = "Function expects ";

    let at = message.find(HEAD)?;
    let rest = &message[at + HEAD.len()..];
    let expects: u32 = rest[..rest.find(' ')?].parse().ok()?;
    let but = message.find("but ")? + "but ".len();
    let tail = message[but..]
        .strip_prefix("only ")
        .unwrap_or(&message[but..]);
    let given: u32 = match tail.starts_with("none") {
        true => 0,

        false => tail[..tail.find(' ')?].parse().ok()?,
    };
    let expects = expects.checked_sub(1)?;
    let given = given.checked_sub(1)?;
    let plural = |n: u32| if n == 1 { "argument" } else { "arguments" };
    let only = if given < expects { "only " } else { "" };
    let verb = if given == 1 { "is" } else { "are" };

    Some(format!(
        "{}{HEAD}{expects} {}, but {only}{given} {verb} specified",
        &message[..at],
        plural(expects),
    ))
}
