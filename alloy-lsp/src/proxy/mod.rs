//! The proxy: shadow documents for the child, position and URI mapping
//! for every message that crosses, and the features the child cannot
//! give an Alloy file: its settings, auto-imports, rename follow-up, and
//! markup intellisense.
//!
//! An Alloy buffer never reaches the child as itself. The server keeps
//! the source, compiles the check artifact, and gives the child that text
//! as a shadow `.luau` document. The child resolves a `require` only to a
//! file on disk, so the shadows live in a mirror of the workspace under
//! the temp directory, beside a copy of every plain Luau file; the child's
//! root is the mirror. Every URI and position in a message crossing
//! either way is mapped, so the editor only ever sees its own files.

mod capabilities;
mod completion;
mod diagnostics;
mod dispatch;
mod documents;
mod hints;
mod hover;
mod ingots_bridge;
mod navigation;
mod state;
mod typing;

pub use dispatch::Server;
pub use documents::root_key;
pub(crate) use state::{Asked, Pending, State};

// Several names below serve dispatch.rs and the tests under tests/, not
// this file's own code; `cfg(test)` code drops out of the plain build,
// and dispatch.rs pulls each name in through this file's glob import.
use capabilities::edit_capabilities;
#[allow(unused_imports)]
use completion::{
    MatchKind, call_snippet, clean_completion, complete_std_members, drop_internal_items,
    drop_receiver, hide_private, hide_record, import_temps, lands_on_member, member_position,
    module_entries, payload_types, plain_snippet, strip_import_temps, strip_std_prefix,
};
#[allow(unused_imports)]
use diagnostics::{
    alias_key_line, collapse_diagnostics, consumed_by_intrinsic, friendly_message, keep_diagnostic,
    missing_key, quoted_span_on_line, snap_ranges, unmet_expectations, unused_name,
};
#[allow(unused_imports)]
use documents::{
    RUNTIME_ALIAS, UPDATE_IMPORTS, config_dir_from, export_surface, map_from_shadow,
    map_into_shadow, map_uris_into_mirror, mirror_dir, mirror_luau_text, mirrored_sourcemap,
    mount_alias_settings, normalize, project_aliases,
};
#[allow(unused_imports)]
use hints::{clean_hints, emit_slot_hint, hint_label, unwrap_future_hint, writable_type};
#[allow(unused_imports)]
use hover::{
    OPEN_ATTRIBUTES, attach_std_member_docs, case_binding_text, declared_annotation,
    declared_attribute_targets, declared_field_hover, declared_parameter_hover, declared_signature,
    declares_a_name_at, drop_bound_intersections, field_key, fold_std_shapes, foreign_method_hover,
    invents_a_type, is_byte_count, keep_annotation, method_owner, module_hover,
    name_by_declaration, name_method_receiver, name_solver_variable, name_trait_method,
    names_a_key, optional_index_hover, prefer_constructed_struct, remote_parameter_hover,
    remote_spec, restates_itself, restore_struct_arguments, restyle_hover, service_hover,
    std_member_hover, unlocal_parameter, used_field_hover, without_self,
};
#[allow(unused_imports)]
use navigation::{data_module_of, service_definition};
#[allow(unused_imports)]
use typing::{append_initializer, end_follows};

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alloy::EmitOptions;
use alloy::config::Config;
use serde_json::{Map, Value, json};

use crate::doc::{Doc, offset_of, position_of};
use crate::imports::{self, Rename};
use crate::{block_end, context, keywords, log, markup, settings, tokens};

/// The names bound in a file, with markup blanked for `.alx`.
pub(crate) fn markup_bound(src: &str) -> HashSet<String> {
    let blanked = alloy::luaux::compile::markup_spans(src)
        .map(|spans| alloy::luaux::resolve::blank_luaux_regions(src, &spans))
        .unwrap_or_else(|_| src.to_string());

    alloy::alx::bound_names(&blanked)
}

/// The type parameters a file declares: the names inside every
/// `Name<...>` the source writes.
pub(crate) fn declared_type_parameters(source: &str) -> HashSet<String> {
    let mut out = HashSet::new();

    for (i, _) in source.match_indices('<') {
        let before = source[..i].chars().next_back();

        if !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }

        let Some(end) = source[i..].find('>') else {
            continue;
        };

        for part in source[i + 1..i + end].split(',') {
            let name = part.split(':').next().unwrap_or("").trim();

            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.insert(name.to_string());
            }
        }
    }

    out
}

/// Whether the child gets a document's shadow. A `.d.aly` compiles to
/// `declare` syntax, which the child reads only as a definitions file;
/// as a document it would report every line. The declarations reach it
/// through `--definitions` instead.
pub(crate) fn child_sees(uri: &str) -> bool {
    !uri.ends_with(".d.aly")
}

pub(crate) fn position_of_value(v: &Value) -> Option<(u32, u32)> {
    let line = v.get("line")?.as_u64()? as u32;
    let character = v.get("character")?.as_u64()? as u32;

    Some((line, character))
}

pub fn range_of(v: &Value) -> Option<((u32, u32), (u32, u32))> {
    Some((
        position_of_value(v.get("start")?)?,
        position_of_value(v.get("end")?)?,
    ))
}

pub(crate) fn range_value(start: (u32, u32), end: (u32, u32)) -> Value {
    json!({
        "start": { "line": start.0, "character": start.1 },
        "end": { "line": end.0, "character": end.1 }
    })
}

pub(crate) fn text_document_uri(message: &Value) -> Option<String> {
    message
        .pointer("/params/textDocument/uri")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(crate) fn id_key(id: &Value) -> String {
    id.to_string()
}

pub fn is_alloy_uri(uri: &str) -> bool {
    uri.ends_with(".aly") || uri.ends_with(".alx")
}

/// The path of a `file:` URI, percent-decoded.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;

                continue;
            }
        }

        out.push(bytes[i]);
        i += 1;
    }

    let text = String::from_utf8(out).ok()?;

    // Windows: `file:///C:/x` and `file:///c%3A/x` carry a leading
    // slash before the drive. One letter then a colon is a drive; a
    // longer first segment with a colon is a file name on Unix.
    let bytes = text.as_bytes();
    let drive = bytes.len() > 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/' || bytes[3] == b'\\');
    let text = if drive { text[1..].to_string() } else { text };

    Some(PathBuf::from(text))
}

/// The `file:` URI of a path, with the characters editors escape.
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");

    if !text.starts_with('/') {
        out.push('/');
    }

    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }

            _ => out.push_str(&format!("%{b:02X}")),
        }
    }

    out
}

#[cfg(test)]
mod tests;
