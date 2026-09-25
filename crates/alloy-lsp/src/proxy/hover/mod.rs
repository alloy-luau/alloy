//! Hover: field, module, and declaration hovers, and the name a printed type reads by.

mod declarations;
mod fields;
mod impls;
mod members;
mod modules;
mod restyle;

pub(crate) use declarations::{
    binds_a_value, case_arm_of_binding, case_binding_span, case_binding_text, formatted_hover,
    import_alias_source, let_else_binding, split_top,
};
pub(crate) use fields::{
    declared_field_hover, declared_field_owner, declared_parameter_hover,
    declared_type_parameters_of, enclosing_brace, field_key, foreign_method_hover,
    function_name_of, literal_key, literal_key_path, name_solver_local, name_solver_struct,
    receiver_type, record_entry, remote_parameter_hover, used_field_hover, used_field_owner,
};
pub(crate) use members::{attach_std_member_docs, std_member_hover, std_receiver};
pub(crate) use modules::{import_spec, module_hover, remote_spec, service_hover};

#[cfg(test)]
#[cfg(test)]
pub(crate) use modules::{shadows_an_import, star_module_hover, std_import_hover};
pub(crate) use restyle::group_len;
pub(crate) use restyle::{
    close_empty_packs, close_item_packs, declared_annotation, declared_head, declared_signature,
    drop_bound_intersections, empty_parameter_names, fold_std_shapes, invents_a_type,
    is_byte_count, keep_annotation, lowers_a_block, member_doc, name_by_declaration,
    name_method_doc, name_method_receiver, name_self_receiver, name_solver_variable,
    name_trait_method, names_a_key, prefer_constructed_struct, restates_itself,
    restore_struct_arguments, restyle_hover, restyle_signatures, source_type, std_generic,
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

    /// Whether a signature-help caret sits in the parameter list of a
    /// declaration, where no call is open.
    pub(crate) fn in_declared_params(&self, uri: &str, message: &Value) -> bool {
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
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return false;
        };

        super::completion::open_paren_word(&doc.source, offset)
            .is_some_and(|(start, _, _)| super::completion::declares_params(&doc.source, start))
    }

    /// The shadow position of a signature-help caret inside a call in
    /// an intrinsic's argument, where the argument stands as code.
    /// `None` when no such call is open at the caret.
    pub(crate) fn signature_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let (shadow_line, _) = doc.to_shadow(line, character);

        intrinsic_code_home(&doc.source, &doc.shadow, line, shadow_line, character)
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

    /// The shadow position a completion after `->` or `=>` belongs at:
    /// inside the `FindFirstChild("` or `WaitForChild("` string the
    /// lookup lowers to, where the child lists the children and nothing
    /// else. `None` when the caret names no child, or the shadow holds
    /// no such call: a `->` of a function type is one.
    pub(crate) fn child_home(&self, uri: &str, message: &Value) -> Option<(u32, u32)> {
        if !is_alloy_uri(uri) {
            return None;
        }

        let (line, character) = position_of_message(message)?;
        let st = self.state.lock().expect("state");
        let doc = st.docs.get(uri)?;
        let offset = offset_of(&doc.source, line, character)?;
        let start = context::child_name_start(&doc.source, offset)?;
        let (shadow_line, text, name) = child_call(doc, start)?;
        let at = (name + offset - start).min(text.len());

        Some((shadow_line, text[..at].chars().count() as u32))
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

        // A child name: the child types the name that holds the lookup
        // from the sourcemap, and the answer becomes the child hover.
        if let Some((start, _, _)) = keywords::child_hover(&doc.source, offset, |_| None)
            && let Some(home) = child_value_home(doc, start)
        {
            drop(st);
            self.forward_request_at(message.clone(), Some("textDocument/hover"), home);

            return true;
        }

        // A std name the file binds itself, through an import or a
        // declaration, is the file's: the child answers for that one.
        let owned = keywords::attribute_argument_hover(&doc.source, offset)
            .or_else(|| keywords::child_hover(&doc.source, offset, |at| child_cast(doc, at)));
        let hit = keywords::hover(&doc.source, offset).filter(|(start, end, _)| {
            let word = &doc.source[*start..*end];
            let is_std = alloy::desugar::AMBIENT.contains(&word)
                || matches!(
                    word,
                    "SignalConnection"
                        | "Signalish"
                        | "Partial"
                        | "Readonly"
                        | "Sink"
                        | "R15Character"
                        | "R6Character"
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

        let hit = owned.or_else(|| hit.map(|(s, e, t)| (s, e, t.to_string())));
        let result = match hit {
            Some((start, end, text)) => {
                let (sl, sc) = position_of(&doc.source, start);
                let (el, ec) = position_of(&doc.source, end);
                // A std type: the overview, then the names a reader can
                // hover on their own. Only a type carries members, and
                // `type_markdown` reads the whole doc of the key again,
                // so a keyword cut to one meaning would grow back.
                let word = &doc.source[start..end];
                let text = match alloy::docs::member_names(word).is_empty() {
                    true => text.to_string(),

                    false => alloy::docs::type_markdown(word).unwrap_or_else(|| text.to_string()),
                };

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
            None if keywords::is_word_caret(&doc.source, offset) => {
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
                    // A dotted tag names a member, and the module an
                    // import brings the component from says where it
                    // is bound.
                    let (member, from) = match &spot {
                        markup::Spot::Tag { name } => {
                            let member = name
                                .contains('.')
                                .then(|| {
                                    let path: Vec<&str> = name.split('.').collect();

                                    components::member_at(&doc.source, &path, &load)
                                })
                                .flatten();

                            (member, markup::component_module(&doc.source, name, &load))
                        }

                        _ => (None, None),
                    };

                    markup::hover(&spot, &bound, member.as_ref(), from.as_deref())
                        .unwrap_or(Value::Null)
                }

                None => return false,
            },

            _ => match st.markup_completion(uri, offset, None) {
                Some(items) => Value::Array(items),

                None => return false,
            },
        };

        drop(st);
        self.respond(id, result);

        true
    }
}

impl State {
    /// The completion items inside `.alx` markup at a byte offset, or
    /// `None` off markup. `as_class` completes an attribute slot as that
    /// Roblox class, for a tag an ingot rewrites into it.
    pub(crate) fn markup_completion(
        &self,
        uri: &str,
        offset: usize,
        as_class: Option<&str>,
    ) -> Option<Vec<Value>> {
        let doc = self.docs.get(uri)?;

        // `</` names the element it closes, and nothing else.
        if let Some(name) = markup::closing_slot(&doc.source, offset) {
            return Some(vec![json!({
                "label": name,
                "kind": 7,
                "insertText": format!("{name}>"),
            })]);
        }

        let mut spot = markup::completion_spot(&doc.source, offset)?;
        let bound = markup_bound(&doc.source);
        let load = |spec: &str| self.module_source(uri, spec);

        if let (markup::Spot::AttributeSlot { class, .. }, Some(as_class)) = (&mut spot, as_class) {
            *class = as_class.to_string();
        }

        let props = self.ingot_props(uri);
        // A dotted tag reaches the members of the path in front of its
        // last `.`; a bare one reaches every name of the file that holds
        // a component.
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

        // The props of a component stand where it is declared, which an
        // import may bring from another module.
        let declared = match &spot {
            markup::Spot::AttributeSlot { class, .. } => {
                markup::component_source(&doc.source, class, &load)
            }

            _ => None,
        };

        Some(markup::completions(
            &spot,
            &bound,
            declared.as_deref().unwrap_or(&doc.source),
            &props,
            &reach,
        ))
    }

    /// The source of the module a spec names, with its file: an open
    /// document first, then the file on disk. A `.alx` in another
    /// folder is open only when the author has it in a tab, so the disk
    /// answers for the rest.
    pub(crate) fn module_source(&self, uri: &str, spec: &str) -> Option<(String, PathBuf)> {
        let target = imports::module_path(&self.resolve_spec(uri, spec)?);

        for (u, d) in &self.docs {
            if let Some(p) = uri_to_path(u)
                && imports::module_path(&p) == target
            {
                return Some((d.source.clone(), p));
            }
        }

        let file = imports::module_file(&target)?;

        Some((std::fs::read_to_string(&file).ok()?, file))
    }

    /// The documents of the modules a file imports. Each one holds the
    /// declarations of its own imports, and a remote or a function of
    /// it can hand the file a struct that the file never imports.
    pub(crate) fn imported_docs(&self, uri: &str) -> Vec<&Doc> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let targets: Vec<PathBuf> = alloy_syntax::scan::import_statements(&doc.source)
            .into_iter()
            .filter_map(|s| self.resolve_spec(uri, &s.spec))
            .map(|p| imports::module_path(&p))
            .collect();

        self.docs
            .iter()
            .filter(|(u, _)| {
                uri_to_path(u).is_some_and(|p| targets.contains(&imports::module_path(&p)))
            })
            .map(|(_, d)| d)
            .collect()
    }
}

/// The check artifact's call for the child lookup whose name starts at
/// `start`, the byte after its `->` or `=>` and any blank: the shadow
/// line, its text, and the byte of the name inside `FindFirstChild("`
/// or `WaitForChild("`. The emit keeps the line, so the call is the one
/// with the same method and name, counted from the left. A lookup with
/// no name yet has the repair's placeholder there.
pub(crate) fn child_call(doc: &Doc, start: usize) -> Option<(u32, &str, usize)> {
    let src = &doc.source;
    let head = src[..start].trim_end_matches([' ', '\t']);
    let (arrow, method) = if head.ends_with("->") {
        ("->", ":FindFirstChild(\"")
    } else if head.ends_with("=>") {
        ("=>", ":WaitForChild(\"")
    } else {
        return None;
    };
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let name_at = |from: usize| {
        let end = src[from..]
            .find(|c| !word(c))
            .map_or(src.len(), |i| from + i);

        &src[from..end]
    };
    // `x->` at the end of a line: the parser reads the word that opens
    // the next line as the name.
    let name = match name_at(start) {
        "" => name_at(src.len() - src[start..].trim_start().len()),

        name => name,
    };
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    // The earlier lookups of the same child on the line.
    let earlier = src[line_start..head.len() - 2]
        .match_indices(arrow)
        .filter(|(i, _)| {
            let rest = src[line_start + i + 2..].trim_start_matches([' ', '\t']);

            rest.starts_with(name) && !rest[name.len()..].starts_with(word)
        })
        .count();
    let written = match name.is_empty() {
        true => crate::doc::HOLE.trim_end_matches("()"),

        false => name,
    };
    let (line, character) = position_of(src, start);
    let (shadow_line, _) = doc.to_shadow(line, character);
    let text = doc.shadow.lines().nth(shadow_line as usize)?;
    let call = text
        .match_indices(&format!("{method}{written}\""))
        .nth(earlier)?
        .0;

    Some((shadow_line, text, call + method.len()))
}

/// The type the check artifact casts a child lookup to: the `T` of
/// `(x:FindFirstChild("a") :: T)`. The compiler decides it, from the
/// operator and the place of the lookup in its chain.
pub(crate) fn child_cast(doc: &Doc, start: usize) -> Option<String> {
    let (_, text, name) = child_call(doc, start)?;
    // The name is a string, so its closing quote ends it.
    let args = name + text[name..].find('"')?;
    let close = args + text[args..].find(')')?;

    // A lookup the compiler leaves uncast is the plain call, so Luau's
    // own signature types it: `FindFirstChild` and a `WaitForChild` with
    // a timeout give `Instance?`. A sourcemap can narrow it further.
    // A timed wait casts to its own type made optional, so it reads as
    // the plain call does.
    let Some(rest) = text[close + 1..]
        .strip_prefix(" :: ")
        .filter(|r| !r.starts_with("typeof("))
    else {
        let optional =
            text[..name].ends_with("FindFirstChild(\"") || text[args..close].contains(',');

        return Some(if optional { "Instance?" } else { "Instance" }.to_string());
    };

    Some(rest[..cast_end(rest)?].trim().to_string())
}

/// The byte of the `)` that closes the group of a cast, in the text
/// after its ` :: `.
fn cast_end(cast: &str) -> Option<usize> {
    let mut depth = 0i32;

    cast.find(|c: char| {
        match c {
            '(' | '{' | '<' | '[' => depth += 1,

            ')' | '}' | '>' | ']' => depth -= 1,

            _ => {}
        }

        depth < 0
    })
}

/// The shadow position where the child types the child lookup whose
/// name starts at `start`. A name that holds the whole value answers
/// first. Else the `)` that closes the value of the lookup: a link in
/// the middle of a chain has no name of its own, and a temp that the
/// block assigns again types as the union of its values.
pub(crate) fn child_value_home(doc: &Doc, start: usize) -> Option<(u32, u32)> {
    bound_home(doc, start).or_else(|| value_close(doc, start))
}

/// The shadow position of the `)` that closes the value of a child
/// lookup: the call, or the group of the cast that the compiler writes
/// around it, `(x:WaitForChild("a", 5) :: typeof(...)?)`. The child
/// types the expression that ends there.
fn value_close(doc: &Doc, start: usize) -> Option<(u32, u32)> {
    let (line, text, name) = child_call(doc, start)?;
    let args = name + text[name..].find('"')?;
    let close = args + text[args..].find(')')?;
    let end = match text[close + 1..].strip_prefix(" :: ") {
        Some(cast) => close + 1 + " :: ".len() + cast_end(cast)?,

        None => close,
    };

    Some((line, text[..end].chars().count() as u32))
}

/// The shadow position of the name that holds the whole value of the
/// child lookup whose name starts at `start`: a temp the emit hoists
/// it into, `local _1 = x:WaitForChild("a")`, or a `local` or a `const`
/// of the source with the lookup as its whole value. The child types
/// that name from the sourcemap, as it types the lookup. `None` when
/// the lookup is part of a larger value.
fn bound_home(doc: &Doc, start: usize) -> Option<(u32, u32)> {
    let (line, text, name) = child_call(doc, start)?;
    let call = text[..name].rfind(':')?;
    let args = name + text[name..].find('"')?;
    let close = args + text[args..].find(')')?;
    let head = &text[..call];
    let is_temp = |w: &str| {
        w.strip_prefix('_')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    // The last `name = ` before the call that starts a statement: a
    // `local` or a `const` of one name, or a temp the block assigns
    // again. An annotation types the name as the source wrote it, and a
    // list of names holds more than the lookup, so neither is one.
    let (bind, eq) = head.rmatch_indices(" = ").find_map(|(eq, _)| {
        let bind = head[..eq]
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |i| i + 1);
        let before = &head[..bind];
        let declared = before.ends_with("local ") || before.ends_with("const ");
        let reused = is_temp(&head[bind..eq]) && (before.is_empty() || before.ends_with(' '));

        (bind < eq && (declared || reused)).then_some((bind, eq))
    })?;
    let binding = &text[bind..eq];
    // A name assigned twice types as the union of its values.
    let assign = format!("{binding} = ");
    let assigned = doc
        .shadow
        .lines()
        .flat_map(|l| {
            l.match_indices(&assign)
                .filter(move |(i, _)| *i == 0 || l.as_bytes()[i - 1] == b' ')
        })
        .count();

    if assigned > 1 {
        return None;
    }

    let value = &text[eq + 3..call];
    let rest = &text[close + 1..];
    // The receiver alone, or behind the guard of an optional link.
    let guarded = value
        .strip_prefix("(if ")
        .and_then(|v| v.split_once(" == nil then nil else "))
        .is_some_and(|(a, b)| a == b && !a.contains(' '));
    let rest = match guarded {
        true => rest.strip_prefix(')')?,

        false if value.contains(' ') => return None,

        false => rest,
    };
    // A timed wait carries a cast to its own type made optional.
    let rest = match rest.strip_prefix(" :: typeof(") {
        Some(cast) => &cast[cast.find(")?)")? + 3..],

        None => rest,
    };
    // A temp holds a prefix of the chain and nothing more. A name of the
    // source holds the lookup alone when nothing follows it on its line.
    let ends = match is_temp(binding) {
        true => rest.is_empty() || rest.starts_with(' ') && !rest.starts_with(" ::"),

        false => {
            let after = &doc.source[start..];
            let word = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            let tail = after[word..].lines().next().unwrap_or("").trim();

            tail.is_empty() || tail.starts_with("--")
        }
    };

    ends.then(|| (line, text[..bind].chars().count() as u32))
}

/// The hover of a child name from the child's answer at the name that
/// holds the lookup, see `child_value_home`: the child hover with the
/// type that answer gives, and the range of the name.
pub(crate) fn child_lookup_hover(
    answer: &str,
    doc: &Doc,
    line: u32,
    character: u32,
) -> Option<(String, Value)> {
    let offset = offset_of(&doc.source, line, character)?;
    let (start, _, _) = keywords::child_hover(&doc.source, offset, |_| None)?;
    child_value_home(doc, start)?;
    // `local katana: Tool?` at a name, where the type follows the name,
    // or `Tool?` alone at the `)` that closes the lookup.
    let head = answer.lines().find(|l| !l.starts_with("```"))?;
    let ty = head
        .strip_prefix("local ")
        .map_or(Some(head), |h| h.split_once(": ").map(|(_, ty)| ty))?;
    let (start, end, text) =
        keywords::child_hover(&doc.source, offset, |_| Some(ty.trim().to_string()))?;
    let (sl, sc) = position_of(&doc.source, start);
    let (el, ec) = position_of(&doc.source, end);

    Some((
        text,
        json!({
            "start": { "line": sl, "character": sc },
            "end": { "line": el, "character": ec }
        }),
    ))
}

/// Maps positions and ranges in request params into the shadow.
/// The shadow position of `word` for a hover: the same line first, then
/// the nearest line above that holds it as a whole word, then any line
/// below. A local is bound above its use; the emit of an `enum` at the
/// top of the file binds `v` too, and the nearest wins over the first.
pub(crate) fn shadow_home(shadow: &str, line: u32, word: &str) -> Option<(u32, u32)> {
    let lines: Vec<&str> = shadow.lines().collect();
    let line = line as usize;
    let above = (0..=line.min(lines.len().saturating_sub(1))).rev();
    let below = line + 1..lines.len();

    above.chain(below).find_map(|i| {
        let text = lines[i];
        let byte = word_outside_strings(text, word)?;

        Some((i as u32, text[..byte].chars().count() as u32))
    })
}

/// Where a caret inside a call in an intrinsic's argument stands in
/// the shadow. The intrinsic lowers to one generated text, with the
/// argument as a string for its message and as code after it, so the
/// map holds no position for the caret and the child sees a call in
/// the code alone. The text from the intrinsic's `(` to the caret is
/// the key: its last match outside every string of the shadow line is
/// the code copy. `None` when no call of the argument is open at the
/// caret, where the intrinsic's own signature is the answer.
pub(crate) fn intrinsic_code_home(
    source: &str,
    shadow: &str,
    line: u32,
    shadow_line: u32,
    character: u32,
) -> Option<(u32, u32)> {
    let text = source.lines().nth(line as usize)?;
    let at = offset_of(text, 0, character)?;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    // The `(` of the intrinsic whose list holds the caret: the nearest
    // open one to the left that a `$name` stands in front of.
    let mut depth = 0i32;
    let mut open = None;

    for (i, c) in text[..at].char_indices().rev() {
        match c {
            ')' => depth += 1,

            '(' if depth > 0 => depth -= 1,

            '(' => {
                let head = &text[..i];
                let name = head.rfind(|c: char| !is_word(c)).map_or(0, |p| p + 1);

                if name > 0 && name < head.len() && head[..name].ends_with('$') {
                    open = Some(i);

                    break;
                }
            }

            _ => {}
        }
    }

    let prefix = &text[open? + 1..at];

    // A call still open in the prefix is the one to help with.
    if prefix.matches('(').count() <= prefix.matches(')').count() {
        return None;
    }

    let shadow_text = shadow.lines().nth(shadow_line as usize)?;
    let code = shadow_text
        .match_indices(prefix)
        .filter(|(i, _)| !context::in_string(shadow_text, *i))
        .last()?
        .0;
    let column = shadow_text[..code + prefix.len()].encode_utf16().count() as u32;

    Some((shadow_line, column))
}

/// The column of `word` as a whole word outside every string of the
/// line. An intrinsic writes its argument twice, `"p:len()"` as text
/// for the message and `p:len()` as code, and the code is the one
/// with a type behind it.
fn word_outside_strings(text: &str, word: &str) -> Option<usize> {
    let mut from = 0;

    loop {
        let at = from + keywords::find_word(&text[from..], word)?;

        if !context::in_string(text, at) {
            return Some(at);
        }

        from = at + word.len();
    }
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
    // The block the caret sits in: the last `impl` or `trait` head
    // above it that no `end` at the head's own indent has closed.
    let mut head: Option<(usize, String)> = None;

    for l in doc.source.lines().take(line as usize + 1) {
        let indent = l.len() - l.trim_start().len();
        let text = l.trim_start();

        if let Some((at, _)) = &head
            && indent == *at
            && (text == "end" || text.starts_with("end "))
        {
            head = None;

            continue;
        }

        // A foreign impl is exported, so it works project wide.
        let text = text.strip_prefix("export ").unwrap_or(text);
        let text = text.strip_prefix("global ").unwrap_or(text);

        // Inside a trait's own default method `self` is whichever
        // type implements the trait. The trait itself is what the
        // reader can name there.
        if let Some(rest) = text
            .strip_prefix("impl ")
            .or_else(|| text.strip_prefix("trait "))
        {
            head = Some((indent, rest.to_string()));
        }
    }

    let (_, head) = head?;
    // `impl Shape for Sq` names the struct after `for`; a namespace
    // member keeps its path, `impl Zoo.Lion`.
    let named = head.split(" for ").last().unwrap_or(&head).trim();
    let name: String = named
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    let name = name.trim_end_matches('.').to_string();
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
