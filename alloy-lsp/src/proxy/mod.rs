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
mod documents;
mod hints;
mod hover;
mod ingots_bridge;
mod navigation;
mod typing;

pub use documents::root_key;

// Several names below serve only the tests at the end of this file:
// `cfg(test)` code drops out of the plain build, so a name only a test
// calls reads as unused there even though the "bin" test target uses it.
use capabilities::edit_capabilities;
#[allow(unused_imports)]
use completion::{
    MatchKind, call_snippet, clean_completion, complete_std_members, drop_internal_items,
    drop_receiver, hide_private, hide_record, import_temps, lands_on_member, member_position,
    module_entries, payload_types, plain_snippet, strip_import_temps, strip_std_prefix,
};
#[allow(unused_imports)]
use diagnostics::{
    collapse_diagnostics, consumed_by_intrinsic, friendly_message, keep_diagnostic, missing_key,
    quoted_span_on_line, snap_ranges, unmet_expectations, unused_name,
};
#[allow(unused_imports)]
use documents::{
    RUNTIME_ALIAS, UPDATE_IMPORTS, config_dir_from, map_from_shadow, map_into_shadow,
    map_uris_into_mirror, mirror_dir, mirror_luau_text, mirrored_sourcemap, mount_alias_settings,
    normalize, project_aliases,
};
#[allow(unused_imports)]
use hints::{clean_hints, emit_slot_hint, hint_label, unwrap_future_hint, writable_type};
#[allow(unused_imports)]
use hover::{
    attach_std_member_docs, case_binding_text, declared_annotation, declared_attribute_targets,
    declared_field_hover, declared_parameter_hover, declared_signature, declares_a_name_at,
    drop_bound_intersections, field_key, fold_std_shapes, foreign_method_hover, is_byte_count,
    keep_annotation, method_owner, module_hover, name_by_declaration, name_method_receiver,
    name_solver_variable, name_trait_method, names_a_key, prefer_constructed_struct,
    remote_parameter_hover, remote_spec, restates_itself, restore_struct_arguments, restyle_hover,
    std_member_hover, unlocal_parameter, without_self,
};
use navigation::data_module_of;
#[allow(unused_imports)]
use typing::{append_initializer, end_follows};

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alloy::EmitOptions;
use alloy::config::Config;
use serde_json::{Map, Value, json};

use crate::doc::{Doc, offset_of, position_of};
use crate::imports::{self, Rename};
use crate::{block_end, context, keywords, log, markup, settings, tokens};

pub struct Server {
    pub(crate) state: Mutex<State>,
    pub(crate) child_in: Mutex<Box<dyn Write + Send>>,
    pub(crate) client_out: Mutex<Box<dyn Write + Send>>,
    /// Held for the length of a pass over the workspace. The file poll
    /// runs on its own thread, and two passes at once would open a
    /// document twice.
    pub(crate) scan: Mutex<()>,
}

/// A request the editor sent, waiting for the child's answer.
pub(crate) struct Pending {
    pub(crate) method: String,
    /// The source URI the request was about, if an Alloy file.
    pub(crate) ctx: Option<String>,
    /// The source position of the request, for completion.
    pub(crate) position: Option<(u32, u32)>,
    /// The character that triggered a completion, when one did.
    pub(crate) trigger: Option<String>,
    /// The source range of the request, for code actions.
    pub(crate) range: Option<((u32, u32), (u32, u32))>,
}

/// A question the server asked the editor.
pub(crate) enum Asked {
    /// Apply this workspace edit if the answer is the update action.
    Rename(Value),
    /// The watcher registration; the answer says nothing to act on.
    Watch,
}

#[derive(Default)]
pub(crate) struct State {
    /// Source URI -> document.
    pub(crate) docs: HashMap<String, Doc>,
    /// Shadow URI -> source URI.
    pub(crate) shadows: HashMap<String, String>,
    /// Source URIs the editor holds open; the rest mirror the disk.
    pub(crate) editor_open: HashSet<String>,
    pub(crate) pending: HashMap<String, Pending>,
    /// The id of the `initialize` request, whose result is edited.
    pub(crate) initialize_id: Option<String>,
    /// Latest child diagnostics per source URI, already mapped.
    pub(crate) child_diagnostics: HashMap<String, Vec<Value>>,
    pub(crate) root: Option<PathBuf>,
    /// Extensions declared anywhere under the root, read at startup.
    pub(crate) extensions: Vec<alloy::extensions::Extension>,
    /// The ingots of the root's alloy.toml, started at workspace open
    /// and again when the file changes.
    pub(crate) ingots: Option<std::sync::Arc<alloy::ingot::Ingots>>,
    /// The mirror workspace the child works in.
    pub(crate) mirror: PathBuf,
    /// Plain Luau documents the editor holds open, by real URI: their
    /// text keeps the mirror copy current.
    pub(crate) plain: HashMap<String, String>,
    /// The mirror URI of the runtime module; its diagnostics stay inside.
    pub(crate) runtime_uri: Option<String>,
    /// The places the runtime was written to, one per project input.
    pub(crate) runtimes: std::cell::RefCell<std::collections::HashSet<PathBuf>>,
    /// The child's settings, answered on `workspace/configuration`.
    pub(crate) settings: Value,
    /// The proxy's own editor options, from the same settings object.
    pub(crate) editor: settings::Editor,
    /// Questions in flight, by request id.
    pub(crate) asked: HashMap<String, Asked>,
    pub(crate) next_id: u64,
    /// Whether the editor takes snippet text in a completion item.
    pub(crate) snippets: bool,
    /// Whether the editor takes a watcher registration. Without one the
    /// server polls the project's folders itself.
    pub(crate) watch_registration: bool,
}

impl State {
    /// The structs and enums of every open document, for the folds.
    pub(crate) fn known_shapes(&self) -> crate::shapes::Known {
        self.known_shapes_at(None)
    }

    /// The shapes of the workspace, with one document's own first: two
    /// structs of a shape print alike, and the file's own is the one
    /// its reader means.
    pub(crate) fn known_shapes_at(&self, uri: Option<&str>) -> crate::shapes::Known {
        let here = uri.and_then(|u| self.docs.get(u));
        let rest = self.docs.iter().filter(|(u, _)| Some(u.as_str()) != uri);

        crate::shapes::Known {
            shapes: here
                .into_iter()
                .chain(rest.clone().map(|(_, d)| d))
                .flat_map(|d| d.shapes.iter().chain(&d.import_shapes).cloned())
                .collect(),
            interfaces: here
                .into_iter()
                .chain(rest.map(|(_, d)| d))
                .flat_map(|d| d.interfaces.iter().chain(&d.import_interfaces).cloned())
                .collect(),
        }
    }

    /// The `[lint]` table of the workspace's alloy.toml, or the defaults.
    pub(crate) fn lint_config(&self) -> alloy::config::LintConfig {
        self.root
            .as_deref()
            .and_then(|r| alloy::config::Config::find_within(r, r))
            .and_then(|p| alloy::config::Config::load(&p).ok())
            .map(|c| c.lint)
            .unwrap_or_default()
    }

    /// The emit options and markup config for a file, from the nearest
    /// `alloy.toml`: its `[alx]` table, or a `luaux.toml` beside it.
    pub(crate) fn options_for(&self, uri: &str) -> (EmitOptions, alloy::luaux::Config) {
        let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        // The climb stops at the workspace root: a sibling project
        // under the same parent must not lend its configuration.
        let found = match &self.root {
            Some(root) => Config::find_within(&dir, root),

            None => Config::find(&dir),
        };
        let config = found.and_then(|p| Config::load(&p).ok().map(|c| (p, c)));
        let file_name = path.to_string_lossy().into_owned();
        let definitions = file_name.ends_with(".d.aly");
        let config_dir = config
            .as_ref()
            .and_then(|(p, _)| p.parent().map(Path::to_path_buf))
            .or_else(|| self.root.clone())
            .unwrap_or_else(|| dir.clone());
        let jsx = config
            .as_ref()
            .map(|(_, c)| c.markup(&config_dir))
            .unwrap_or_else(|| alloy::luaux::Config::load(&config_dir).map_err(|e| e.message))
            .unwrap_or_default();

        let options = match config {
            Some((config_path, config)) => {
                let root = normalize(config_path.parent().unwrap_or(Path::new(".")));
                // The shadow requires the runtime by an alias, and the
                // mirror's Luau configuration names the place the runtime
                // is written. A relative path would not do: the analyzer
                // reads `../alloy` in a file the sourcemap holds as an
                // instance path, so the runtime would resolve only in a
                // project that has built one. The ship artifact still
                // writes the instance path.
                self.ensure_runtime(&normalize(&root.join(&config.build.out)));
                self.write_mirror(
                    &root.join(".luaurc"),
                    &mirror_luau_text(&root, Some(&config)),
                );

                EmitOptions {
                    wait_timeout: config.emit.wait_timeout,
                    file_name,
                    std_require: RUNTIME_ALIAS.to_string(),
                    definitions,
                    erase_type_imports: config.emit.erase_type_imports,
                    extensions: self.extensions.clone(),
                    ..EmitOptions::default()
                }
            }

            None => {
                self.ensure_runtime(&normalize(&dir));

                EmitOptions {
                    file_name,
                    std_require: "./alloy".to_string(),
                    definitions,
                    extensions: self.extensions.clone(),
                    ..EmitOptions::default()
                }
            }
        };

        (options, jsx)
    }
}

impl Server {
    pub fn new(
        child_in: Box<dyn Write + Send>,
        client_out: Box<dyn Write + Send>,
        extensions: Vec<alloy::extensions::Extension>,
    ) -> Self {
        let state = State {
            settings: settings::defaults(),
            extensions,
            ..State::default()
        };

        Self {
            state: Mutex::new(state),
            child_in: Mutex::new(child_in),
            client_out: Mutex::new(client_out),
            scan: Mutex::new(()),
        }
    }

    pub(crate) fn to_child(&self, message: &Value) {
        let mut w = self.child_in.lock().expect("child stdin");

        if let Err(e) = crate::rpc::write_message(&mut *w, message) {
            log::error(&format!("write to child failed: {e}"));
        }
    }

    pub(crate) fn to_client(&self, message: &Value) {
        let mut w = self.client_out.lock().expect("client stdout");

        if let Err(e) = crate::rpc::write_message(&mut *w, message) {
            log::error(&format!("write to client failed: {e}"));
        }
    }

    pub(crate) fn respond(&self, id: &Value, result: Value) {
        self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Handles one message from the editor. Returns false on `exit`.
    pub fn handle_client(&self, mut message: Value) -> bool {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        log::trace(&format!(
            "client -> {} id={}",
            method.as_deref().unwrap_or("(response)"),
            message.get("id").map(id_key).unwrap_or_default()
        ));

        match method.as_deref() {
            Some("initialize") => {
                let root = message
                    .pointer("/params/rootUri")
                    .and_then(Value::as_str)
                    .and_then(uri_to_path)
                    .or_else(|| {
                        message
                            .pointer("/params/workspaceFolders/0/uri")
                            .and_then(Value::as_str)
                            .and_then(uri_to_path)
                    });
                let mut st = self.state.lock().expect("state");
                st.mirror = mirror_dir(root.as_deref());
                let _ = std::fs::remove_dir_all(&st.mirror);
                let _ = std::fs::create_dir_all(&st.mirror);
                st.root = root;
                st.initialize_id = message.get("id").map(id_key);
                st.snippets = message
                    .pointer(
                        "/params/capabilities/textDocument/completion/completionItem/snippetSupport",
                    )
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                st.watch_registration = message
                    .pointer(
                        "/params/capabilities/workspace/didChangeWatchedFiles/dynamicRegistration",
                    )
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mirror_uri = path_to_uri(&st.mirror);
                let mirror_path = st.mirror.to_string_lossy().into_owned();

                if let Some(options) = message.pointer("/params/initializationOptions") {
                    st.editor = settings::editor(options, st.editor);
                    let over = settings::from_editor(options);
                    settings::merge(&mut st.settings, &over);
                }

                let mounts = mount_alias_settings(st.root.as_deref());
                settings::merge(&mut st.settings, &mounts);

                // The child reads its own shape from here, and asks for
                // it over `workspace/configuration`, which the proxy
                // answers: the editor need not support that request.
                let child_settings = st.settings.clone();
                drop(st);

                if let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) {
                    params.insert("initializationOptions".to_string(), child_settings);

                    // The child's workspace is the mirror.
                    if params.contains_key("rootUri") {
                        params.insert("rootUri".to_string(), Value::String(mirror_uri.clone()));
                    }

                    if params.contains_key("rootPath") {
                        params.insert("rootPath".to_string(), Value::String(mirror_path));
                    }

                    // Every folder of the editor maps into the one mirror,
                    // so the child gets one folder. With the same URI
                    // listed twice it never finishes configuring its
                    // workspaces, and every request waits forever.
                    if let Some(folders) = params
                        .get_mut("workspaceFolders")
                        .and_then(Value::as_array_mut)
                    {
                        let name = folders
                            .first()
                            .and_then(|f| f.get("name"))
                            .cloned()
                            .unwrap_or_else(|| Value::String("workspace".to_string()));
                        folders.clear();
                        folders.push(json!({ "uri": mirror_uri.clone(), "name": name }));
                    }

                    let caps = params.entry("capabilities").or_insert_with(|| json!({}));

                    if let Some(caps) = caps.as_object_mut() {
                        let ws = caps.entry("workspace").or_insert_with(|| json!({}));

                        if let Some(ws) = ws.as_object_mut() {
                            ws.insert("configuration".to_string(), Value::Bool(true));
                        }
                    }
                }

                self.to_child(&message);
            }

            Some("initialized") => {
                self.to_child(&message);
                self.open_workspace();
                self.watch_project_files();
            }

            Some("exit") => {
                self.to_child(&message);

                return false;
            }

            Some("workspace/didChangeConfiguration") => {
                let mut st = self.state.lock().expect("state");

                if let Some(settings) = message.pointer("/params/settings") {
                    st.editor = settings::editor(settings, st.editor);
                    let over = settings::from_editor(settings);
                    settings::merge(&mut st.settings, &over);
                }

                let mounts = mount_alias_settings(st.root.as_deref());
                settings::merge(&mut st.settings, &mounts);

                let child_settings = st.settings.clone();
                drop(st);
                self.to_child(&json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": { "settings": child_settings }
                }));
            }

            Some("textDocument/didOpen") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri) {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let version = message
                        .pointer("/params/textDocument/version")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.open_doc(&uri, text, version, true);
                } else {
                    let text = message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.plain_changed(&uri, Some(text), &[]);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didChange") => {
                let uri = text_document_uri(&message).unwrap_or_default();
                let changes = message
                    .pointer("/params/contentChanges")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                if is_alloy_uri(&uri) {
                    let version = message
                        .pointer("/params/textDocument/version")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.change_doc(&uri, version, &changes);
                } else {
                    self.plain_changed(&uri, None, &changes);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didClose") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri) {
                    // The shadow stays open so other files still resolve
                    // it; its text goes back to the disk version.
                    let mut st = self.state.lock().expect("state");
                    st.editor_open.remove(&uri);
                    drop(st);

                    match uri_to_path(&uri).and_then(|p| std::fs::read_to_string(p).ok()) {
                        Some(text) => self.change_doc(&uri, 0, &[json!({ "text": text })]),

                        None => self.close_shadow(&uri),
                    }
                } else {
                    // Back to the disk version in the mirror.
                    let mut st = self.state.lock().expect("state");
                    st.plain.remove(&uri);

                    if let Some(path) = uri_to_path(&uri) {
                        match std::fs::read_to_string(&path) {
                            Ok(text) => st.write_mirror(&path, &text),

                            Err(_) => st.remove_mirror(&path),
                        }
                    }

                    drop(st);
                    self.forward_plain(message);
                }
            }

            Some("textDocument/didSave") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if is_alloy_uri(&uri)
                    && let Some(params) = message.get_mut("params").and_then(Value::as_object_mut)
                {
                    params.remove("text");
                }

                self.forward_plain(message);
            }

            Some("workspace/didChangeWatchedFiles") => {
                let changes = message
                    .pointer("/params/changes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // The child re-reads a `.luau` it hears about; a data
                // file's module joins the list under the module's name.
                let mut modules: Vec<Value> = Vec::new();
                let mut data_changed: Vec<PathBuf> = Vec::new();

                for change in &changes {
                    let uri = change
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let kind = change.get("type").and_then(Value::as_i64).unwrap_or(2);

                    if uri.ends_with("/alloy.toml") {
                        self.load_ingots();
                    }

                    if !is_alloy_uri(&uri) {
                        // A plain file: the mirror copy follows the disk
                        // unless the editor holds the file open.
                        let st = self.state.lock().expect("state");

                        if let Some(path) = uri_to_path(&uri)
                            && !st.plain.contains_key(&uri)
                        {
                            match (kind, std::fs::read_to_string(&path)) {
                                (3, _) | (_, Err(_)) => st.remove_mirror(&path),

                                (_, Ok(text)) => st.write_mirror(&path, &text),
                            }

                            if let Some(module) = data_module_of(&path) {
                                modules.push(json!({ "uri": path_to_uri(&module), "type": kind }));
                                data_changed.push(path);
                            }
                        }

                        drop(st);

                        continue;
                    }

                    let open = self.state.lock().expect("state").editor_open.contains(&uri);

                    match kind {
                        3 => self.close_shadow(&uri),

                        _ if !open => {
                            if let Some(text) =
                                uri_to_path(&uri).and_then(|p| std::fs::read_to_string(p).ok())
                            {
                                self.open_doc(&uri, text, 0, false);
                            }
                        }

                        _ => {}
                    }
                }

                if let Some(list) = message
                    .pointer_mut("/params/changes")
                    .and_then(Value::as_array_mut)
                {
                    list.extend(modules);
                }

                self.forward_plain(message);

                for path in &data_changed {
                    self.refresh_dependents(path);
                }
            }

            // The mirror is the child's one folder, whatever the editor
            // adds or removes on its side.
            Some("workspace/didChangeWorkspaceFolders") => {}

            Some("workspace/didRenameFiles") => {
                let files = message
                    .pointer("/params/files")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self.renamed(&files);
            }

            Some("textDocument/formatting") => {
                if let Some(id) = message.get("id").cloned() {
                    let uri = text_document_uri(&message).unwrap_or_default();
                    self.format_document(&uri, &id);
                }
            }

            Some("textDocument/onTypeFormatting") => {
                if let Some(id) = message.get("id").cloned() {
                    let uri = text_document_uri(&message).unwrap_or_default();
                    self.end_after_opener(&uri, &message, &id);
                }
            }

            Some("alloy/closeTag") => {
                if let Some(id) = message.get("id").cloned() {
                    self.close_tag_name(&message, &id);
                }
            }

            // A picked color: an ingot that colored the range names it;
            // else the child's `Color3` forms.
            Some("textDocument/colorPresentation") => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.ingot_presentation(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, method.as_deref());
            }

            Some(m @ ("textDocument/hover" | "textDocument/completion")) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                // An ingot's hover comes first: a class in a `.alx` string
                // is the ingot's, not the markup's. Its completion items
                // likewise, where it has any.
                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && self.ingot_hover(&uri, &message, &id)
                {
                    return true;
                }

                // A closing quote asks for nothing: the editor sends the
                // quote as a trigger either way, and a list that pops up
                // there takes the next Enter.
                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && (self.closes_a_string(&uri, &message)
                        || self.names_a_declaration(&uri, &message))
                {
                    self.respond(&id, json!([]));

                    return true;
                }

                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && self.ingot_completion(&uri, &message, &id)
                {
                    return true;
                }

                if uri.ends_with(".alx")
                    && let Some(id) = message.get("id").cloned()
                    && self.markup_answer(m, &uri, &message, &id)
                {
                    return true;
                }

                if m == "textDocument/hover"
                    && let Some(id) = message.get("id").cloned()
                    && (self.case_binding_hover(&uri, &message, &id)
                        || self.field_hover(&uri, &message, &id)
                        || self.source_binding_hover(&uri, &message, &id)
                        || self.declaration_hover(&uri, &message, &id)
                        || self.keyword_hover(&uri, &message, &id))
                {
                    return true;
                }

                if m == "textDocument/completion"
                    && let Some(id) = message.get("id").cloned()
                    && self.context_completion(&uri, &message, &id)
                {
                    return true;
                }

                // `bx?.`, `p!.`, `await X.`: the lowering owns the member.
                if m == "textDocument/completion"
                    && let Some(home) = self.member_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // `Ok(v)` lowers to `__alloy.Ok(v)`: the caret maps past
                // a `.` the source never wrote.
                if m == "textDocument/completion"
                    && let Some(home) = self.expression_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                // A binding the desugar moved, `if local c = ...`, maps
                // to a byte the child knows nothing about. The name has
                // a home in the shadow; the child answers there.
                if m == "textDocument/hover"
                    && let Some(home) = self.hover_home(&uri, &message)
                {
                    self.forward_request_at(message, method.as_deref(), home);

                    return true;
                }

                self.forward_request(message, method.as_deref());
            }

            Some("alloy/blockEnd") => {
                // The editor asks after Enter: the line before the new one.
                let uri = text_document_uri(&message).unwrap_or_default();
                let line = message
                    .pointer("/params/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let indent = self
                    .state
                    .lock()
                    .expect("state")
                    .docs
                    .get(&uri)
                    .and_then(|d| block_end::needs_end(&d.source, line));

                if let Some(id) = message.get("id") {
                    let result = match indent {
                        Some(indent) => json!({ "indent": indent }),

                        None => Value::Null,
                    };
                    self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }

            Some(
                m @ ("textDocument/definition"
                | "textDocument/declaration"
                | "textDocument/typeDefinition"),
            ) => {
                let uri = text_document_uri(&message).unwrap_or_default();

                if let Some(id) = message.get("id").cloned()
                    && self.definition_answer(&uri, &message, &id)
                {
                    return true;
                }

                self.forward_request(message, Some(m));
            }

            Some(_) => self.forward_request(message, method.as_deref()),

            None => {
                // A response from the editor: to a question of ours, or to
                // the child's, whose URIs then move into the mirror.
                let key = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let asked = self.state.lock().expect("state").asked.remove(&key);

                match asked {
                    Some(Asked::Watch) => {}

                    Some(Asked::Rename(edit)) => {
                        let chosen = message
                            .pointer("/result/title")
                            .and_then(Value::as_str)
                            .unwrap_or_default();

                        if chosen == UPDATE_IMPORTS {
                            let id = self.state.lock().expect("state").fresh_id();
                            self.to_client(&json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "method": "workspace/applyEdit",
                                "params": { "label": "Update imports", "edit": edit }
                            }));
                        }
                    }

                    None => self.forward_plain(message),
                }
            }
        }

        true
    }

    /// Maps a request about an Alloy document into its shadow and
    /// forwards it, remembering what it was about.
    pub(crate) fn forward_request(&self, message: Value, method: Option<&str>) {
        self.forward_request_with(message, method, None);
    }

    /// Forwards a request whose shadow position is already known.
    pub(crate) fn forward_request_at(
        &self,
        message: Value,
        method: Option<&str>,
        shadow: (u32, u32),
    ) {
        self.forward_request_with(message, method, Some(shadow));
    }

    pub(crate) fn forward_request_with(
        &self,
        mut message: Value,
        method: Option<&str>,
        shadow: Option<(u32, u32)>,
    ) {
        let uri = text_document_uri(&message);

        // Semantic tokens for markup come from the lowered code, whose
        // columns are not the source's; the grammar colors `.alx`.
        if let Some(u) = &uri
            && u.ends_with(".alx")
            && method.is_some_and(|m| m.starts_with("textDocument/semanticTokens"))
            && let Some(id) = message.get("id").cloned()
        {
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": { "data": [] } }));

            return;
        }

        // The child never sees a `.d.aly` document, so a request on one
        // gets an empty answer here instead of the child's error.
        if let Some(u) = &uri
            && !child_sees(u)
            && let Some(id) = message.get("id").cloned()
        {
            let result = match method {
                Some("textDocument/diagnostic") => json!({ "kind": "full", "items": [] }),

                Some("textDocument/semanticTokens/full") => json!({ "data": [] }),

                _ => Value::Null,
            };
            self.to_client(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));

            return;
        }
        let ctx = uri.filter(|u| is_alloy_uri(u));
        let position = message
            .pointer("/params/position")
            .and_then(position_of_value);
        let trigger = message
            .pointer("/params/context/triggerCharacter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let range = message.pointer("/params/range").and_then(range_of);

        if let Some(id) = message.get("id") {
            let key = id_key(id);
            self.state.lock().expect("state").pending.insert(
                key,
                Pending {
                    method: method.unwrap_or_default().to_string(),
                    ctx: ctx.clone(),
                    position,
                    trigger,
                    range,
                },
            );
        }

        let st = self.state.lock().expect("state");

        if let Some(ctx) = &ctx
            && let Some(doc) = st.docs.get(ctx)
            && let Some(params) = message.get_mut("params")
        {
            // A markup hole that opens with a keyword lowers to the
            // keyword itself, where the child completes nothing. The
            // expression after it is the same scope, and the editor
            // still inserts where the caret is.
            if method == Some("textDocument/completion")
                && ctx.ends_with(".alx")
                && let Some((l, c)) = position
                && let Some(at) = offset_of(&doc.source, l, c)
                && let Some(moved) = markup::hole_expression_start(&doc.source, at)
            {
                let (ml, mc) = position_of(&doc.source, moved);
                params["position"] = json!({ "line": ml, "character": mc });
            }

            map_into_shadow(params, doc);

            if let Some((line, character)) = shadow {
                params["position"] = json!({ "line": line, "character": character });
            }
        }

        map_uris_into_mirror(&mut message, &st);
        drop(st);
        self.to_child(&message);
    }

    /// Handles one message from the child.
    pub fn handle_child(&self, mut message: Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        log::trace(&format!(
            "child -> {} id={}",
            method.as_deref().unwrap_or("(response)"),
            message.get("id").map(id_key).unwrap_or_default()
        ));

        match method.as_deref() {
            Some("textDocument/publishDiagnostics") => {
                let uri = message
                    .pointer("/params/uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let st = self.state.lock().expect("state");

                if st.runtime_uri.as_deref() == Some(uri.as_str()) {
                    return;
                }

                let (source, is_alloy) = st.editor_uri(&uri);
                drop(st);

                match is_alloy {
                    true => {
                        let diagnostics = message
                            .pointer("/params/diagnostics")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let mut st = self.state.lock().expect("state");
                        let doc_path = uri_to_path(&source);
                        let mapped: Vec<Value> = match st.docs.get(&source) {
                            Some(doc) => {
                                let mut out: Vec<Value> = Vec::new();

                                let lint_config = st.lint_config();
                                let unmet = unmet_expectations(doc, &diagnostics);

                                for mut d in diagnostics {
                                    if !keep_diagnostic(&d, doc, doc_path.as_deref(), &lint_config)
                                    {
                                        continue;
                                    }

                                    map_from_shadow(&mut d, Some(&source), &st);
                                    friendly_message(&mut d, doc, &st);

                                    // Two references in one desugar map to
                                    // one source token: report it once.
                                    if !out.iter().any(|o| {
                                        o["range"] == d["range"] && o["message"] == d["message"]
                                    }) {
                                        out.push(d);
                                    }
                                }

                                out.extend(unmet);
                                collapse_diagnostics(&mut out);

                                out
                            }

                            None => Vec::new(),
                        };
                        st.child_diagnostics.insert(source.clone(), mapped);
                        drop(st);
                        self.publish(&source);
                    }

                    false => {
                        if let Some(p) = message.pointer_mut("/params/uri") {
                            *p = Value::String(source);
                        }

                        self.to_client(&message);
                    }
                }
            }

            Some("workspace/configuration") => {
                // The child's settings are ours to answer.
                let count = message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map_or(1, Vec::len);
                let settings = self.state.lock().expect("state").settings.clone();
                let result: Vec<Value> = (0..count).map(|_| settings.clone()).collect();

                if let Some(id) = message.get("id") {
                    self.to_child(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                }
            }

            Some("client/registerCapability") => {
                // Dynamic semantic token registration would bypass the
                // static one the proxy edits; the static one stays.
                if let Some(list) = message
                    .pointer_mut("/params/registrations")
                    .and_then(Value::as_array_mut)
                {
                    list.retain(|r| {
                        !r.get("method")
                            .and_then(Value::as_str)
                            .is_some_and(|m| m.starts_with("textDocument/semanticTokens"))
                    });
                }

                self.to_client(&message);
            }

            Some(_) => {
                // A server request or notification: map any locations.
                let st = self.state.lock().expect("state");

                if let Some(params) = message.get_mut("params") {
                    map_from_shadow(params, None, &st);
                }

                drop(st);
                self.to_client(&message);
            }

            None => self.child_response(message),
        }
    }

    pub(crate) fn child_response(&self, mut message: Value) {
        let key = message.get("id").map(id_key);
        let mut st = self.state.lock().expect("state");
        let pending = key.as_ref().and_then(|k| st.pending.remove(k));
        let is_init = key.is_some() && key == st.initialize_id;

        if is_init {
            st.initialize_id = None;
            edit_capabilities(&mut message);
        }

        let (method, ctx, position, trigger, range) = match pending {
            Some(p) => (p.method, p.ctx, p.position, p.trigger, p.range),

            None => (String::new(), None, None, None, None),
        };

        // The child answers null when it has no action; the Alloy
        // rewrites need a list to join.
        if method == "textDocument/codeAction"
            && ctx.is_some()
            && message.get("result").is_some_and(Value::is_null)
        {
            message["result"] = json!([]);
        }

        if let Some(result) = message.get_mut("result") {
            // Hints and tokens in generated text describe temps: gone
            // before the mapping moves what is left.
            if let Some(uri) = &ctx
                && let Some(doc) = st.docs.get(uri)
            {
                match method.as_str() {
                    // The child says `local` or `function`; the source may
                    // have said `const`, `export`, or `async`.
                    "textDocument/hover" => {
                        if let Some((line, character)) = position
                            && let Some(value) = result
                                .pointer("/contents/value")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        {
                            let mut text = value.clone();

                            if let Some(rewritten) = restyle_hover(&text, doc, line, character) {
                                text = rewritten;
                            }

                            if let Some(kept) = keep_annotation(&text, doc, line, character) {
                                text = kept;
                            }

                            text = fold_std_shapes(&text);
                            text = crate::shapes::fold(&text, &st.known_shapes_at(ctx.as_deref()));

                            if let Some(written) = declared_signature(&text, doc, line, character) {
                                text = written;
                            }

                            if let Some(named) = name_trait_method(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = name_method_receiver(&text, doc, line) {
                                text = named;
                            }

                            if let Some(named) = restore_struct_arguments(&text, doc, line) {
                                text = named;
                            }

                            if let Some(dropped) = drop_bound_intersections(&text, doc) {
                                text = dropped;
                            }

                            if let Some(named) = name_by_declaration(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = unlocal_parameter(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = name_solver_variable(&text, doc, line, character) {
                                text = named;
                            }

                            if let Some(named) = prefer_constructed_struct(
                                &text,
                                doc,
                                line,
                                character,
                                &st.known_shapes(),
                            ) {
                                text = named;
                            }

                            if let Some(with_init) = append_initializer(&text, doc, line, character)
                            {
                                text = with_init;
                            }

                            // A component's factory returns whatever the
                            // configured `create` gives; the checker has
                            // no type for it and prints its own marker.
                            text = text.replace("): *error-type*", ")");
                            text = text.replace("*error-type*", "unknown");

                            // One grammar over a file: the Alloy fence
                            // highlights `const`, `async`, and `export`,
                            // which the Luau one drops.
                            text = text.replace("```luau", "```alloy");

                            // `type Player = Player` restates the token
                            // under the cursor and says nothing, and
                            // `string (5 bytes)` measures the key the
                            // emit wrote, not the name the source has.
                            let says_nothing = restates_itself(&text)
                                || (is_byte_count(&text) && names_a_key(doc, line, character));

                            // A std member: the type above, then what
                            // the member does and an example, which no
                            // type carries.
                            let member = std_member_hover(&text, doc, line, character);

                            if let Some(section) = &member {
                                text.push_str("\n\n");
                                text.push_str(section);
                            }

                            if says_nothing && member.is_none() {
                                *result = Value::Null;
                            } else if text != value {
                                result["contents"]["value"] = json!(text);
                            }
                        }
                    }

                    // A pulled report gets the filter the push path has. The
                    // messages wait for the mapping below: a range set here
                    // in source terms would map once more, to one byte.
                    "textDocument/diagnostic" => {
                        if let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) {
                            let lint_config = st.lint_config();
                            let doc_path = uri_to_path(uri);
                            let unmet = unmet_expectations(doc, items);
                            items.retain(|d| {
                                keep_diagnostic(d, doc, doc_path.as_deref(), &lint_config)
                            });
                            items.extend(unmet);
                            collapse_diagnostics(items);
                        }
                    }

                    // A `Color3` the desugar or an ingot wrote has no place in
                    // the author's text to put a swatch on.
                    "textDocument/documentColor" => {
                        if let Some(colors) = result.as_array_mut() {
                            colors.retain(|c| {
                                c.pointer("/range/start")
                                    .and_then(position_of_value)
                                    .is_none_or(|(l, ch)| !doc.generated_at(l, ch))
                            });
                        }
                    }

                    "textDocument/inlayHint" => {
                        if let Some(hints) = result.as_array_mut() {
                            // A hint attaches to the byte before it, so
                            // that byte decides: the `)` of a generated
                            // inner function is generated even when the
                            // copied newline follows. A hint that names an
                            // error type describes the emit, not the source.
                            hints.retain(|h| {
                                let error_type = hint_label(h).contains("*error-type*");
                                let offset = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, c)| offset_of(&doc.shadow, l, c));
                                let generated = offset
                                    .is_some_and(|o| doc.generated_offset(o.saturating_sub(1)));
                                // A parameter hint on a call the lowering
                                // wrote, `create("TextLabel")` behind a tag,
                                // lands on the tag: the byte before it is
                                // not the author's.
                                let lowered_call = h.get("kind").and_then(Value::as_u64) == Some(2)
                                    && offset.is_some_and(|o| doc.lowering_differs_before(o));

                                !error_type && !generated && !lowered_call
                            });

                            // A label the child sends in parts folds as
                            // one text, the way its edit does; the parts'
                            // locations point into the emit anyway.
                            for h in hints.iter_mut() {
                                if h.get("label").is_some_and(Value::is_array) {
                                    h["label"] = json!(hint_label(h));
                                }
                            }

                            // An async function declares the inner type;
                            // the child infers the Future around it.
                            for h in hints.iter_mut() {
                                let async_line = h
                                    .get("position")
                                    .and_then(position_of_value)
                                    .and_then(|(l, _)| doc.shadow.lines().nth(l as usize))
                                    .is_some_and(|line| line.contains(".future(function"));

                                if async_line {
                                    unwrap_future_hint(h);
                                }
                            }
                        }
                    }

                    "textDocument/semanticTokens/full" => {
                        if let Some(data) = result.get("data").and_then(Value::as_array) {
                            let raw: Vec<u64> = data.iter().filter_map(Value::as_u64).collect();
                            let mapped = tokens::remap(&raw, doc);
                            log::debug(&format!(
                                "semantic tokens: {} in, {} out",
                                raw.len() / 5,
                                mapped.len() / 5
                            ));
                            result["data"] = json!(mapped);
                        }

                        drop(st);
                        self.to_client(&message);

                        return;
                    }

                    _ => {}
                }
            }

            if method == "workspace/diagnostic"
                && let Some(reports) = result.get_mut("items").and_then(Value::as_array_mut)
            {
                for report in reports {
                    let source = report
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(|shadow| st.shadows.get(shadow))
                        .cloned();
                    let doc = source.as_ref().and_then(|source| st.docs.get(source));

                    if let Some(doc) = doc
                        && let Some(items) = report.get_mut("items").and_then(Value::as_array_mut)
                    {
                        let lint_config = st.lint_config();
                        let doc_path = source.as_deref().and_then(uri_to_path);
                        items
                            .retain(|d| keep_diagnostic(d, doc, doc_path.as_deref(), &lint_config));
                    }
                }
            }

            map_from_shadow(result, ctx.as_deref(), &st);

            if method == "textDocument/diagnostic"
                && let Some(uri) = ctx.as_deref()
                && let Some(doc) = st.docs.get(uri)
                && let Some(items) = result.get_mut("items").and_then(Value::as_array_mut)
            {
                for d in items.iter_mut() {
                    friendly_message(d, doc, &st);
                }

                // Alloy's own reports travel by push alone: a push
                // overwrites the set an earlier server left on the file,
                // and a pull that repeated them would show each twice.

                // A rewrite may move two reports onto one line, the
                // `impl` an alias names among them; they collapse after
                // it, not before.
                collapse_diagnostics(items);
                snap_ranges(items, &doc.source);
            }

            // The ingots' colors join the child's `Color3` swatches; they
            // speak source positions already, so they join after the map.
            if method == "textDocument/documentColor"
                && let Some(uri) = &ctx
            {
                let mut extra = st.ingot_colors(uri);

                // A `Color3` call the child already colors gets one square.
                if let Value::Array(items) = result {
                    extra.retain(|e| !items.iter().any(|i| i.get("range") == e.get("range")));
                }

                if !extra.is_empty() {
                    match result {
                        Value::Array(items) => items.extend(extra),

                        Value::Null => *result = Value::Array(extra),

                        _ => {}
                    }
                }
            }

            // The editor never sees the runtime's table: `__alloy.Future<T>`
            // reads `Future<T>`, and `__alloy_string.trim` reads `string.trim`.
            if ctx.is_some()
                && matches!(
                    method.as_str(),
                    "textDocument/hover"
                        | "textDocument/inlayHint"
                        | "textDocument/completion"
                        | "completionItem/resolve"
                        | "textDocument/signatureHelp"
                )
            {
                // Before the rewrite: `__alloy_string` reads `oy_string`
                // once `__all` goes, and no list should carry it at all.
                if method == "textDocument/completion" {
                    drop_internal_items(result);
                }

                strip_std_prefix(result);
                // The checker prints a struct as its runtime table and a
                // unit enum as a union of strings; the names go back.
                crate::shapes::fold_value(result, &st.known_shapes_at(ctx.as_deref()));

                // A type hint inserts its edit on a click: the label shows
                // that text, so the two never differ.
                if method == "textDocument/inlayHint"
                    && let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u))
                    && let Some(hints) = result.as_array_mut()
                {
                    clean_hints(hints, doc);
                }

                if let Some(doc) = ctx.as_ref().and_then(|u| st.docs.get(u)) {
                    strip_import_temps(result, &doc.shadow);
                }
            }

            // The Alloy diagnostics travel on the push channel alone, in
            // `publish`; a pulled report that carried them too showed
            // each lint twice in an editor that reads both.
            match method.as_str() {
                // The rewrites of the lints in the range, as quick fixes,
                // and one action that applies every rewrite of the file.
                "textDocument/codeAction" => {
                    if let Some(uri) = &ctx
                        && let Some(range) = range
                        && let Some(actions) = result.as_array_mut()
                    {
                        actions.extend(st.header_as_actions(uri, range));
                        actions.extend(st.lint_actions(uri, range));
                        actions.extend(st.ingot_actions(uri, range));
                    }
                }

                "textDocument/signatureHelp" => {
                    if let Some(uri) = &ctx {
                        st.rewrite_variant_signatures(uri, result);
                    }
                }

                // A link sits on the require the emit wrote, which maps to
                // the start of the import; it moves to the quoted path of
                // that source line, and its target leaves the mirror. One
                // shadow line can hold two requires, the runtime among
                // them, so the reader gets one link per import path.
                "textDocument/documentLink" => {
                    if let Some(uri) = &ctx
                        && let Some(doc) = st.docs.get(uri)
                        && let Some(links) = result.as_array_mut()
                    {
                        let mut seen: HashSet<(u32, u32)> = HashSet::new();

                        links.retain_mut(|link| {
                            let Some(target) = link.get("target").and_then(Value::as_str) else {
                                return false;
                            };
                            let real = uri_to_path(target).map(|p| normalize(&p));

                            // Alloy writes the runtime require; the reader
                            // wrote no import of it, so it gets no link.
                            if real
                                .as_ref()
                                .and_then(|p| st.real_path(p))
                                .is_some_and(|p| st.runtimes.borrow().contains(&normalize(&p)))
                            {
                                return false;
                            }

                            // A link out of the mirror names the file the
                            // reader edits, not its copy.
                            let (target, _) = st.editor_uri(target);

                            if uri_to_path(&target).is_none_or(|p| !p.exists()) {
                                return false;
                            }

                            let Some(((line, _), _)) = link.get("range").and_then(range_of) else {
                                return false;
                            };
                            let Some((s, e)) = quoted_span_on_line(&doc.source, line) else {
                                return false;
                            };

                            if !seen.insert((line, s)) {
                                return false;
                            }

                            link["target"] = json!(target);
                            link["range"] = range_value((line, s), (line, e));

                            true
                        });
                    }
                }

                "workspace/diagnostic" => {
                    if let Some(reports) = result.get_mut("items").and_then(Value::as_array_mut) {
                        for report in reports {
                            let uri = report
                                .get("uri")
                                .and_then(Value::as_str)
                                .map(str::to_string);

                            // Alloy's own reports travel by push alone.
                            if let Some(uri) = uri
                                && st.docs.contains_key(&uri)
                                && let Some(items) =
                                    report.get_mut("items").and_then(Value::as_array_mut)
                            {
                                collapse_diagnostics(items);
                            }
                        }
                    }
                }

                "textDocument/completion" => {
                    // Enter opens no list. The `end` of an open block
                    // arrives as an on-type edit, and any other name here
                    // pops a popup the reader has to dismiss.
                    if trigger.as_deref() == Some("\n") {
                        *result = json!([]);
                    }

                    if let Some(uri) = &ctx
                        && let Some((line, character)) = position
                        && trigger.as_deref() != Some("\n")
                    {
                        st.mark_enum_members(uri, line, character, result);
                        st.mark_declarations(result);

                        if let Some(doc) = st.docs.get(uri) {
                            complete_std_members(doc, line, character, st.snippets, result);
                            attach_std_member_docs(result, doc, line, character);
                        }

                        let member = st
                            .docs
                            .get(uri)
                            .is_some_and(|d| member_position(d, line, character).is_some());
                        // Inside a string the child answers alone: it
                        // knows the class names `Instance.new("` and
                        // `GetService("` take, and nothing else belongs
                        // between quotes.
                        let quoted = st
                            .docs
                            .get(uri)
                            .and_then(|d| offset_of(&d.source, line, character))
                            .zip(st.docs.get(uri))
                            .is_some_and(|(at, d)| context::in_string(&d.source, at));
                        // A member list names what the value has; an
                        // auto-import is a new name, which cannot follow
                        // a `.` or a `:`.
                        let mut extra = match member || quoted {
                            true => Vec::new(),

                            false => st.auto_imports(uri, line, character),
                        };

                        if !quoted {
                            extra.extend(st.primitive_completions(uri, line, character, result));
                            extra.extend(st.std_completions(uri, line, character, result));
                            extra.extend(st.value_scope(uri, line, character, result));
                            extra.extend(st.directive_completions(uri, line, character));
                        }

                        let (from_ingots, incomplete) =
                            st.ingot_items(uri, line, character, trigger.as_deref());
                        extra.extend(from_ingots);

                        if !extra.is_empty() {
                            match result {
                                Value::Array(items) => items.extend(extra),

                                Value::Object(obj) => {
                                    if let Some(items) =
                                        obj.get_mut("items").and_then(Value::as_array_mut)
                                    {
                                        items.extend(extra);
                                    }
                                }

                                Value::Null => *result = Value::Array(extra),

                                _ => {}
                            }
                        }

                        // An ingot's list is made from the word being
                        // typed, so the child's answer stops being the
                        // whole answer.
                        if incomplete {
                            let list = match result {
                                Value::Array(items) => Some(std::mem::take(items)),

                                Value::Object(obj) => {
                                    obj.insert("isIncomplete".into(), json!(true));

                                    None
                                }

                                _ => None,
                            };

                            if let Some(items) = list {
                                *result = json!({ "isIncomplete": true, "items": items });
                            }
                        }

                        if let Some(doc) = st.docs.get(uri) {
                            clean_completion(result, doc, line, character, st.snippets);
                        }

                        st.filter_remote_members(uri, line, character, result);
                        // After the clean: the spec `@pkg/react` is the
                        // detail, and the clean drops a detail that
                        // spells no type.
                        st.rewrite_child_auto_imports(uri, result);
                        st.keyword_first(uri, line, character, result);
                    }
                }

                _ => {}
            }
        }

        drop(st);
        self.to_client(&message);
    }
}

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
mod tests {
    #[test]
    pub(crate) fn a_declared_annotation_keeps_its_type_arguments() {
        let src = "local damaged: Signal<Player, number> = Signal.new()\nlocal n: number = 1\nlocal function f(a: { x: number, y: number }, b: string) end\n";
        assert_eq!(
            super::declared_annotation(src, "damaged", 6).map(|(_, a)| a),
            Some("Signal<Player, number>".to_string())
        );
        assert_eq!(
            super::declared_annotation(src, "n", 60).map(|(_, a)| a),
            Some("number".to_string())
        );
        assert_eq!(
            super::declared_annotation(src, "a", 90).map(|(_, a)| a),
            Some("{ x: number, y: number }".to_string())
        );
    }

    use super::*;

    /// A state with one open document, so the declarations are there.
    pub(crate) fn hover_of(src: &str, line: u32, character: u32, printed: &str) -> String {
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let mut text = format!("```luau\n{printed}\n```");

        for step in [
            declared_signature as fn(&str, &Doc, u32, u32) -> Option<String>,
            name_trait_method,
            name_by_declaration,
            unlocal_parameter,
        ] {
            if let Some(next) = step(&text, doc, line, character) {
                text = next;
            }
        }

        if let Some(next) = name_method_receiver(&text, doc, line) {
            text = next;
        }

        if let Some(next) = restore_struct_arguments(&text, doc, line) {
            text = next;
        }

        if let Some(next) = drop_bound_intersections(&text, doc) {
            text = next;
        }

        text.trim_start_matches("```luau\n")
            .trim_end_matches("\n```")
            .to_string()
    }

    #[test]
    pub(crate) fn a_bound_reads_where_the_source_wrote_it() {
        let src = concat!(
            "export trait Priced as\n",
            "    function price(self): number\n",
            "end\n",
            "\n",
            "export function cheapest<T: Priced>(a: T, b: T): T\n",
            "    return a\n",
            "end\n",
        );

        assert_eq!(
            hover_of(
                src,
                4,
                17,
                "export function cheapest<T>(a: Priced & T, b: Priced & T): T"
            ),
            "export function cheapest<T: Priced>(a: T, b: T): T"
        );
    }

    #[test]
    pub(crate) fn a_union_keeps_the_order_the_source_wrote() {
        let src = concat!(
            "export function describe_any(v: string | number | boolean): string\n",
            "    return \"x\"\n",
            "end\n",
        );

        assert_eq!(
            hover_of(
                src,
                0,
                17,
                "export function describe_any(v: boolean | number | string): string"
            ),
            "export function describe_any(v: string | number | boolean): string"
        );
    }

    #[test]
    pub(crate) fn a_generic_struct_keeps_its_arguments() {
        let src = concat!(
            "export struct Slotted<T> as\n",
            "    value: T\n",
            "end\n",
            "\n",
            "impl Slotted<T> as\n",
            "    function get(self): T\n",
            "        return self.value\n",
            "    end\n",
            "end\n",
        );

        assert_eq!(
            hover_of(src, 5, 14, "function Slotted.get<T>(self: Slotted): T"),
            "function Slotted.get<T>(self: Slotted<T>): T"
        );
        assert_eq!(
            hover_of(src, 5, 18, "local self: Slotted"),
            "self: Slotted<T>"
        );
    }

    #[test]
    pub(crate) fn a_trait_method_reads_with_its_name_and_its_receiver() {
        let src = concat!(
            "export trait Describable as\n",
            "    function label(self): string\n",
            "end\n",
        );

        assert_eq!(
            hover_of(src, 1, 14, "function (self: any): string"),
            "function Describable.label(self: Describable): string"
        );
        assert_eq!(
            hover_of(src, 1, 14, "function x:label(self: any): string"),
            "function Describable:label(self: Describable): string"
        );
    }

    #[test]
    pub(crate) fn a_parameter_hover_keeps_a_record_type_whole() {
        let src = concat!(
            "local function Stat(props: { label: string, name: string }): number\n",
            "    return 1\n",
            "end\n",
        );
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let start = src.find("props").expect("props");
        let answer = declared_parameter_hover(doc, start, start + "props".len()).expect("hover");

        assert!(
            answer.starts_with("```alloy\nprops: { label: string, name: string }\n```"),
            "{answer}"
        );
        assert!(
            answer.ends_with("A parameter of `function Stat`."),
            "{answer}"
        );

        // The `>` of an arrow closes no bracket.
        let arrows = "local function Button(props: { on_click: () -> () })\n    return 1\nend\n";
        let (st, uri) = one_file(arrows);
        let doc = st.docs.get(uri).expect("doc");
        let start = arrows.find("props").expect("props");
        let answer = declared_parameter_hover(doc, start, start + "props".len()).expect("hover");

        assert!(
            answer.starts_with("```alloy\nprops: { on_click: () -> () }\n```"),
            "{answer}"
        );
    }

    #[test]
    pub(crate) fn a_foreign_impl_names_its_type() {
        let src = concat!(
            "export impl string as\n",
            "    function trim(self): string\n",
            "        return self\n",
            "    end\n",
            "end\n",
        );
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let at = src.find("trim").expect("trim");

        assert_eq!(
            foreign_method_hover(doc, at, at + "trim".len()),
            Some("```alloy\nfunction string.trim(self: string): string\n```".to_string())
        );

        // A struct's own impl reads through the child.
        let own = concat!(
            "struct Item as\n",
            "    id: number\n",
            "end\n",
            "impl Item as\n",
            "    function room(self): number\n",
            "        return self.id\n",
            "    end\n",
            "end\n",
        );
        let (st, uri) = one_file(own);
        let doc = st.docs.get(uri).expect("doc");
        let at = own.find("room").expect("room");

        assert_eq!(foreign_method_hover(doc, at, at + "room".len()), None);
    }

    #[test]
    pub(crate) fn a_parameter_is_not_a_local() {
        let src = "export function room(count: number): number\n    return count\nend\n";

        assert_eq!(hover_of(src, 1, 12, "local count: number"), "count: number");
        // A name the file declares with a keyword keeps its keyword.
        let bound = "local total = 1\nprint(total)\n";
        assert_eq!(
            hover_of(bound, 1, 7, "local total: number"),
            "local total: number"
        );
    }

    #[test]
    pub(crate) fn a_binding_reads_the_type_its_call_declares() {
        let src = concat!(
            "export function checked(n: number): Result<number, string>[]\n",
            "    return []\n",
            "end\n",
            "\n",
            "local rows = checked(1)\n",
        );

        assert_eq!(
            hover_of(src, 4, 7, "local rows: t3"),
            "local rows: Result<number, string>[]"
        );
    }

    pub(crate) fn one_file(src: &str) -> (State, &'static str) {
        let uri = "file:///t.aly";
        let mut st = State {
            root: Some(PathBuf::from("/")),
            mirror: PathBuf::from("/m"),
            snippets: true,
            ..State::default()
        };
        st.docs.insert(
            uri.to_string(),
            Doc::new(
                src.to_string(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            ),
        );

        (st, uri)
    }

    /// The mirror's Luau configuration always names the runtime, so a
    /// shadow resolves `@alloy` on disk with no sourcemap and no build.
    #[test]
    pub(crate) fn the_mirror_config_names_the_runtime_and_the_mounts() {
        let dir = std::env::temp_dir().join(format!("alloy-mirror-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(
            dir.join(".config.luau"),
            "return { luau = { aliases = { alloy = \"./build/alloy\", pkg = \"./vendor\" } } }\n",
        )
        .expect("config");
        let config = Config::parse(
            "[build]\nout = \"out\"\n\n[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"packages\", \"@game/ReplicatedStorage/Packages\"]\n",
            Path::new("alloy.toml"),
        )
        .expect("alloy.toml");
        let text = mirror_luau_text(&dir, Some(&config));
        let json: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(json["languageMode"], "strict");
        // The mirror holds no output tree, so the user's own `alloy`
        // alias is replaced by the place the mirror writes the runtime.
        assert_eq!(json["aliases"]["alloy"], "./out/alloy");
        // A name the Luau configuration declares wins over the mount.
        assert_eq!(json["aliases"]["pkg"], "./vendor");
        assert_eq!(json["aliases"]["shared"], "./src/shared");

        // With no alloy.toml the runtime sits at the root.
        let bare = mirror_luau_text(&dir, None);
        let bare: Value = serde_json::from_str(&bare).expect("json");
        assert_eq!(bare["aliases"]["alloy"], "./alloy");
        assert_eq!(bare["aliases"].get("shared"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A default import and an `import * as` hover as the module: the
    /// import line and the public names, not the module's table.
    #[test]
    pub(crate) fn a_module_import_hovers_as_the_module() {
        let dir = std::env::temp_dir().join(format!("alloy-module-hover-{}", std::process::id()));
        let pkg = dir.join("packages");
        std::fs::create_dir_all(&pkg).expect("temp dir");
        std::fs::write(
            pkg.join("fluid.luau"),
            "local m = {}\nm.__SCHEDULER_INTERFACE = {}\nfunction m.create(x) return x end\nm.mount = 1\nreturn m\n",
        )
        .expect("module");
        let src = "import fluid from \"@pkg/fluid\"\nimport { create } from \"@pkg/fluid\"\nimport * as f2 from \"./packages/fluid\"\nprint(fluid, create, f2)\n";
        let from = dir.join("main.aly");
        let aliases = vec![("pkg".to_string(), pkg.clone())];
        // On the binding the child's table stands.
        assert_eq!(
            module_hover(src, "fluid", Some(&from), &aliases, false),
            None
        );

        // On the path the answer is the import line and nothing else:
        // the document link on the same characters offers to follow it.
        let hover = module_hover(src, "fluid", Some(&from), &aliases, true).expect("a path hover");
        assert_eq!(hover, "```alloy\nimport fluid from \"@pkg/fluid\"\n```");

        // The alias segment answers the same import line.
        let by_alias =
            module_hover(src, "pkg", Some(&from), &aliases, true).expect("an alias hover");
        assert_eq!(by_alias, hover);

        // A path that names no file is the child's to answer.
        assert_eq!(module_hover(src, "gone", Some(&from), &aliases, true), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A word that begins a keyword drops the child's auto-imports.
    /// `end` in a guard clause and in a one-line `impl` drew
    /// `EncodingService`, which the editor sorted first.
    #[test]
    pub(crate) fn the_keyword_wins_over_an_auto_import() {
        let src = "impl T as end\nlocal function f(x: number?): number\n    if x == nil then return 0 end\n    return x\nend\n";
        let (st, uri) = one_file(src);
        let child = || {
            json!([
                {
                    "label": "EncodingService",
                    "kind": 7,
                    "detail": "Auto-import",
                    "sortText": "7",
                    "additionalTextEdits": [{
                        "newText": "local EncodingService = game:GetService(\"EncodingService\")\n",
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                    }],
                },
                { "label": "endsWith", "kind": 3, "detail": "Auto-import", "sortText": "7" },
                { "label": "elseif", "kind": 14, "sortText": "0" },
                { "label": "print", "kind": 3, "sortText": "4" },
            ])
        };
        let labels = |result: &Value| -> Vec<String> {
            result
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["label"].as_str().unwrap_or("").to_string())
                .collect()
        };

        // `impl T as end`, the caret past `end`. The word is the whole
        // keyword and nothing else survives, so the list is empty and
        // the popup closes.
        let mut result = child();
        st.keyword_first(uri, 0, 13, &mut result);
        assert!(labels(&result).is_empty(), "{result}");

        // `if x == nil then return 0 end`, the caret past `end`.
        let mut result = child();
        st.keyword_first(uri, 2, 33, &mut result);
        assert!(labels(&result).is_empty(), "{result}");

        // Half a keyword keeps the keywords it begins, and no module.
        let mut result = child();
        st.keyword_first(uri, 2, 32, &mut result);
        let mut got = labels(&result);
        got.sort();
        assert_eq!(got, ["end", "enum"]);

        // A word that begins no keyword leaves the list alone.
        let mut result = child();
        st.keyword_first(uri, 3, 12, &mut result);
        assert_eq!(labels(&result).len(), 4);
    }

    /// A whole keyword with more names behind it keeps the list, and
    /// takes the first row. `else` is `elseif` as far as the letters go,
    /// so the reader still needs to see both.
    #[test]
    pub(crate) fn a_whole_keyword_with_company_stays_in_the_list() {
        let src = "local elsewhere = 1\nif elsewhere == 1 then\nelse\n";
        let (st, uri) = one_file(src);
        let mut result = json!([
            { "label": "elsewhere", "kind": 6, "sortText": "4" },
            { "label": "print", "kind": 3, "sortText": "4" },
        ]);
        st.keyword_first(uri, 2, 4, &mut result);
        let mut labels: Vec<String> = result
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect();
        labels.sort();
        assert_eq!(labels, ["else", "elseif", "elsewhere"]);

        let exact = result
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["label"] == json!("else"))
            .expect("the keyword");
        assert_eq!(exact["preselect"], json!(true));
        assert_eq!(exact["sortText"], json!("!else"));
    }

    /// After Enter on an opener the `end` arrives as one edit at the
    /// caret, so the caret keeps the line the editor indented.
    #[test]
    pub(crate) fn the_newline_edit_writes_the_end_below_the_caret() {
        let (st, uri) = one_file("function f()\n    \n");
        let edit = st.end_edit(uri, 1, 4).expect("an edit");
        assert_eq!(edit[0]["newText"], json!("\nend"));
        assert_eq!(
            edit[0]["range"],
            json!({ "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 4 } })
        );

        // Text on the caret line, and a closed block, write nothing.
        let (st, uri) = one_file("function f()\n    local x = 1\n");
        assert_eq!(st.end_edit(uri, 1, 4), None);
        let (st, uri) = one_file("function f()\n\nend\n");
        assert_eq!(st.end_edit(uri, 1, 0), None);

        // An `end` already under the opener is the one the block has.
        assert!(end_follows("f()\n\nend\n", 4, ""));
        assert!(!end_follows("f()\n\n    end\n", 4, ""));
        assert!(!end_follows("f()\n\nendless()\n", 4, ""));
        assert!(!end_follows("f()\n", 4, ""));
    }

    /// A hover on a std member reads the member's own section, not the
    /// type's whole page. The receiver resolves from the source: an
    /// annotation, an initializer, or the type name itself.
    #[test]
    pub(crate) fn a_hover_on_a_std_member_names_the_member() {
        let src = concat!(
            "local prices: HashMap<string, number> = HashMap.new()\n",
            "local price = prices:get(\"sword\")\n",
            "local xs = [ 1, 2, 3 ]\n",
            "local n = xs:len()\n",
        );
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");

        let at_new = std_member_hover("```luau\n(...)\n```", doc, 0, 49).expect("HashMap.new");
        assert!(at_new.starts_with("**HashMap.new**"), "{at_new}");

        let at_get = std_member_hover("```luau\n(...)\n```", doc, 1, 22).expect("HashMap:get");
        assert!(at_get.starts_with("**HashMap:get**"), "{at_get}");
        assert!(
            at_get.contains("```alloy"),
            "the section carries an example"
        );

        let at_len = std_member_hover("```luau\n(...)\n```", doc, 3, 14).expect("Array:len");
        assert!(at_len.starts_with("**Array:len**"), "{at_len}");
    }

    /// With no annotation the type the child printed names the receiver.
    #[test]
    pub(crate) fn a_printed_type_names_the_member_the_source_cannot() {
        let src = "local n = whatever:pop()\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let printed = "```luau\n(self: Queue<string>) -> string?\n```";
        let hover = std_member_hover(printed, doc, 0, 20).expect("Queue:pop");

        assert!(hover.starts_with("**Queue:pop**"), "{hover}");
    }

    /// A hover on the type name keeps the overview and lists the names.
    #[test]
    pub(crate) fn a_hover_on_a_std_type_lists_its_members() {
        let text = alloy::docs::type_markdown("HashMap").expect("HashMap");

        assert!(text.contains("A map with methods"), "the overview stays");
        assert!(text.contains("Members: `new`, `from`, `get`"), "{text}");
        assert!(!text.contains("|---|"), "no table is left");
    }

    /// Every ambient std name reaches an expression, and a package
    /// module that carries one of those names arrives as an auto-import,
    /// which is another row: `Signal` in `packages/` left the std
    /// `Signal` out of the list.
    #[test]
    pub(crate) fn an_auto_import_does_not_hide_a_std_name() {
        let src = "local x = \n";
        let (st, uri) = one_file(src);
        let child = json!([
            { "label": "print", "kind": 3 },
            {
                "label": "Signal",
                "kind": 9,
                "detail": "Auto-import",
                "additionalTextEdits": [{ "newText": "local Signal = require(script.Signal)\n" }],
            },
        ]);
        let items = st.std_completions(uri, 0, 10, &child);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in alloy::desugar::AMBIENT {
            assert!(labels.contains(name), "`{name}` is missing: {labels:?}");
        }

        // A name the child already answered stays the child's.
        let child = json!([{ "label": "Signal", "kind": 7 }]);
        let items = st.std_completions(uri, 0, 10, &child);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        assert!(!labels.contains(&"Signal"), "{labels:?}");
    }

    /// A type slot lists the std types, the type-only ones included.
    #[test]
    pub(crate) fn a_type_slot_lists_every_ambient_std_type() {
        let src = "local t: \n";
        let (st, uri) = one_file(src);
        let items = st.std_completions(uri, 0, 9, &json!([{ "label": "string" }]));
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in alloy::desugar::AMBIENT_TYPES {
            assert!(labels.contains(name), "`{name}` is missing: {labels:?}");
        }
    }

    /// A `.` with no name after it stops the parse, so the child sees
    /// the Alloy source and answers nothing. The std table holds the
    /// members, and the type table offers its statics alone.
    #[test]
    pub(crate) fn a_dot_after_a_std_name_lists_the_std_members() {
        let src = "local h = HashMap.\nlocal f = Future.\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let mut result = Value::Null;
        complete_std_members(doc, 0, 18, true, &mut result);
        let labels: Vec<&str> = result
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|i| i["label"].as_str())
            .collect();

        assert_eq!(labels, ["new", "from"], "the statics of HashMap");
        assert_eq!(
            result[0]["detail"],
            json!("HashMap.new<K, V>(): HashMap<K, V>")
        );
        assert_eq!(result[0]["insertText"], json!("new()"));
        assert!(
            result[0]["documentation"]["value"]
                .as_str()
                .is_some_and(|v| v.starts_with("**HashMap.new**")),
            "{result}"
        );

        let mut result = Value::Null;
        complete_std_members(doc, 1, 17, true, &mut result);
        let labels: Vec<&str> = result
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|i| i["label"].as_str())
            .collect();

        for name in [
            "resolve",
            "reject",
            "delay",
            "all",
            "race",
            "any",
            "all_settled",
        ] {
            assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
        }

        assert!(!labels.contains(&"cancel"), "a method is no static");
    }

    /// A method takes a receiver, so the type table does not offer it;
    /// a value does, and a static is no member of one.
    #[test]
    pub(crate) fn a_std_member_list_keeps_what_the_sigil_can_call() {
        let src = "local prices: HashMap<string, number> = HashMap.new()\nprices.\nHashMap.\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let child = json!([{ "label": "get" }, { "label": "new" }, { "label": "Fire" }]);
        let mut result = child.clone();
        complete_std_members(doc, 1, 7, true, &mut result);
        let labels: Vec<&str> = result
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|i| i["label"].as_str())
            .collect();

        assert!(labels.contains(&"get"), "{labels:?}");
        assert!(!labels.contains(&"new"), "a static is no member of a map");
        // A name the std table does not document stays the child's.
        assert!(labels.contains(&"Fire"), "{labels:?}");

        let mut result = child.clone();
        complete_std_members(doc, 2, 8, true, &mut result);
        let labels: Vec<&str> = result
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|i| i["label"].as_str())
            .collect();

        assert!(labels.contains(&"new"), "{labels:?}");
        assert!(!labels.contains(&"get"), "a method takes a receiver");
    }

    /// The child's member list gains the std's doc and signature.
    #[test]
    pub(crate) fn a_std_member_completion_carries_its_doc() {
        let src = "local prices: HashMap<string, number> = HashMap.new()\nprices:g\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).expect("doc");
        let mut result = json!([{ "label": "get" }, { "label": "nothing" }]);
        attach_std_member_docs(&mut result, doc, 1, 8);

        assert_eq!(result[0]["detail"], json!("HashMap:get(key: K): V?"));
        assert!(
            result[0]["documentation"]["value"]
                .as_str()
                .is_some_and(|v| v.starts_with("**HashMap:get**")),
            "{result}"
        );
        assert!(
            result[1].get("detail").is_none(),
            "an unknown label is left"
        );
    }

    /// An `if` expression arm, a ternary, and a `default` get the
    /// locals, the parameters, the file's own declarations, and the std
    /// names. The child's own list wins wherever it answered.
    #[test]
    pub(crate) fn an_expression_position_the_child_leaves_empty_gets_the_scope() {
        let src = concat!(
            "struct Round as\n",
            "    seconds: number\n",
            "end\n",
            "\n",
            "export function pick(acc: number): string\n",
            "    local many = \"many\"\n",
            "    return if acc > 0 then \"a\" else \"b\"\n",
            "end\n",
        );
        let (st, uri) = one_file(src);
        let at = src.find("then \"a\"").unwrap() + "then ".len();
        let (line, character) = position_of(src, at);
        let items = st.value_scope(uri, line, character, &json!([]));
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in ["acc", "many", "pick", "Round", "Ok", "print", "if", "not"] {
            assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
        }

        // A field of a struct is no name the caret can write bare.
        assert!(!labels.contains(&"seconds"), "{labels:?}");
        // The child answered: its list already holds the scope.
        assert!(
            st.value_scope(uri, line, character, &json!([{ "label": "print" }]))
                .is_empty()
        );
    }

    /// A scrutinee the proxy cannot resolve keeps to the variants the
    /// file declares or imports; another file's stay out.
    #[test]
    pub(crate) fn an_unresolved_scrutinee_offers_only_the_names_the_file_sees() {
        let src = concat!(
            "enum Phase as\n",
            "    Lobby\n",
            "    Playing\n",
            "end\n",
            "\n",
            "export function run(input: InputObject)\n",
            "    match input.KeyCode with\n",
            "        case \n",
            "    end\n",
            "end\n",
        );
        let (mut st, uri) = one_file(src);
        st.docs.insert(
            "file:///other.aly".to_string(),
            Doc::new(
                "export enum Coin as\n    Gold\n    Silver\nend\n".to_string(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            ),
        );

        let at = src.find("case \n").unwrap() + "case ".len();
        let ctx = context::detect(src, at).expect("a case context");
        let items = st.context_items(uri, at, &ctx);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in ["Lobby", "Playing", "Ok", "Err", "Enum", "default"] {
            assert!(labels.contains(&name), "`{name}` is missing: {labels:?}");
        }

        // `Coin` is another file's, and this one imports nothing.
        for name in ["Gold", "Silver"] {
            assert!(!labels.contains(&name), "`{name}` leaked: {labels:?}");
        }
    }

    /// A statement line inside a block takes `end`, and the member
    /// column of an `impl` is the only place its member words belong.
    #[test]
    pub(crate) fn a_statement_line_in_a_block_takes_end() {
        let src = "export function f(n: number): number\n    local x = n\n    \nend\n";
        let (st, uri) = one_file(src);
        let at = src.find("\n    \n").unwrap() + 1 + 4;
        let (line, character) = position_of(src, at);
        let items = st.primitive_completions(uri, line, character, &json!([{ "label": "print" }]));
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"end"), "{labels:?}");
    }

    const MATCH_FILE: &str = concat!(
        "enum Msg as\n",
        "    Quit\n",
        "    Join(Player)\n",
        "end\n",
        "enum Color as Red, Green end\n",
        "type Answer = Result<number, string>\n",
        "local function handle(msg: Msg, tally: number, names: string[])\n",
        "    local parsed: Result<number, string> = Ok(1)\n",
        "    local seed = Msg.Join(p)\n",
        "    local reply: Answer = Ok(2)\n",
        "    local made = Array<Msg>()\n",
        "    match msg with\n",
        "        case \n",
        "    end\n",
        "end\n"
    );

    #[test]
    pub(crate) fn a_scrutinee_resolves_to_a_type() {
        let (st, uri) = one_file(MATCH_FILE);
        let at = |name: &str| st.match_kind(uri, MATCH_FILE, MATCH_FILE.len(), name);
        let msg = MatchKind::Enum("Msg".to_string());

        // The annotation of a parameter, of a local, and of a const.
        assert_eq!(at("msg"), msg);
        assert_eq!(at("parsed"), MatchKind::Result);
        assert_eq!(at("names"), MatchKind::Array);
        assert_eq!(at("tally"), MatchKind::Literal);

        // The variant a local starts at.
        assert_eq!(at("seed"), msg);

        // A hover that names an enum or a `Result`.
        assert_eq!(at("Msg"), msg);
        assert_eq!(at("reply"), MatchKind::Result);

        // Anything else keeps the full list.
        assert_eq!(at("made"), MatchKind::Unknown);
        assert_eq!(at("p"), MatchKind::Unknown);
        assert_eq!(at("year % 4, year % 100"), MatchKind::Unknown);
    }

    /// The attribute list a `@` opens, for the declaration under it.
    fn attribute_labels(src: &str) -> Vec<String> {
        let (st, uri) = one_file(src);
        let offset = src.find('@').expect("a sigil") + 1;
        let ctx = context::detect(src, offset).expect("an attribute list");

        st.context_items(uri, offset, &ctx)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    }

    /// An attribute belongs to what it sits above. The list offers the
    /// ones that go there and no others, and a comment between the two
    /// does not hide the declaration.
    #[test]
    pub(crate) fn an_attribute_list_follows_the_declaration_under_it() {
        let remote = attribute_labels("@\nremote Ping() from server\n");

        for name in ["@ratelimit", "@timeout", "@validate", "@unreliable"] {
            assert!(remote.contains(&name.to_string()), "{remote:?}");
        }

        for name in ["@derive", "@test", "@cfg", "@u8", "@rename", "@sealed"] {
            assert!(!remote.contains(&name.to_string()), "{remote:?}");
        }

        // The same list past a comment of either form.
        assert_eq!(
            attribute_labels("@\n-- why\nremote Ping() from server\n"),
            remote
        );
        assert_eq!(
            attribute_labels("@\n--[[ why\n   it stays ]]\nremote Ping() from server\n"),
            remote
        );

        let structure = attribute_labels("@\nstruct V as\n    x: number\nend\n");

        assert!(structure.contains(&"@derive".to_string()), "{structure:?}");
        assert!(structure.contains(&"@sealed".to_string()), "{structure:?}");
        assert!(
            !structure.contains(&"@ratelimit".to_string()),
            "{structure:?}"
        );

        let function = attribute_labels("@\nfunction go()\nend\n");

        for name in ["@native", "@checked", "@deprecated", "@test", "@cfg"] {
            assert!(function.contains(&name.to_string()), "{function:?}");
        }

        assert!(!function.contains(&"@derive".to_string()), "{function:?}");

        // A binding takes `@cfg` alone.
        assert_eq!(attribute_labels("@\nlocal count = 1\n"), ["@cfg"]);

        // The wire sizes go on a remote's parameter and a struct field.
        let param = attribute_labels("remote Hit(@ target: Player) from client\n");

        assert!(param.contains(&"@u8".to_string()), "{param:?}");
        assert!(!param.contains(&"@ratelimit".to_string()), "{param:?}");

        let field = attribute_labels("struct V as\n    @\n    x: number\nend\n");

        assert!(field.contains(&"@u8".to_string()), "{field:?}");
        assert!(field.contains(&"@rename".to_string()), "{field:?}");
        assert!(!field.contains(&"@derive".to_string()), "{field:?}");
    }

    /// A declared attribute reaches the targets it names, and nothing
    /// else.
    #[test]
    pub(crate) fn a_declared_attribute_reaches_its_own_targets() {
        let src = concat!(
            "attribute audited(reason: string) on remote, function\n",
            "\n",
            "@\n",
            "remote Ping() from server\n",
        );
        let (st, uri) = one_file(src);
        let offset = src.rfind('@').expect("a sigil") + 1;
        let ctx = context::detect(src, offset).expect("an attribute list");
        let labels: Vec<String> = st
            .context_items(uri, offset, &ctx)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect();

        assert!(labels.contains(&"@audited".to_string()), "{labels:?}");

        let structure = concat!(
            "attribute audited(reason: string) on remote, function\n",
            "\n",
            "@\n",
            "struct V as\n    x: number\nend\n",
        );
        let (st, uri) = one_file(structure);
        let offset = structure.rfind('@').expect("a sigil") + 1;
        let ctx = context::detect(structure, offset).expect("an attribute list");
        let labels: Vec<String> = st
            .context_items(uri, offset, &ctx)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect();

        assert!(!labels.contains(&"@audited".to_string()), "{labels:?}");
    }

    pub(crate) fn case_items(st: &State, uri: &str, src: &str) -> Vec<Value> {
        let offset = src.rfind("case ").unwrap() + "case ".len();
        let ctx = context::detect(src, offset).expect("a case list");

        st.context_items(uri, offset, &ctx)
    }

    #[test]
    pub(crate) fn a_case_list_holds_the_arms_of_its_own_match() {
        let (st, uri) = one_file(MATCH_FILE);
        let items = case_items(&st, uri, MATCH_FILE);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();

        // The variants of `Msg` alone, then `default`. The other enum's
        // variants, `Ok`, `Err`, and `_` stay out.
        assert_eq!(labels, ["Quit", "Join", "default"]);
        assert_eq!(items[0]["textEdit"]["newText"], "Quit");
        assert_eq!(items[1]["textEdit"]["newText"], "Join($1)");
        assert_eq!(items[1]["insertTextFormat"], 2);
        assert_eq!(items[1]["detail"], "Msg.Join(Player)");
        assert!(
            items[1]["documentation"]["value"]
                .as_str()
                .unwrap()
                .contains("A variant of `enum Msg`")
        );
    }

    #[test]
    pub(crate) fn a_result_a_literal_and_an_array_take_their_own_arms() {
        let result = "local r: Result<number, string> = Ok(1)\nmatch r with\n    case \nend\n";
        let (st, uri) = one_file(result);
        let items = case_items(&st, uri, result);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["Ok", "Err", "default"]);
        assert_eq!(items[0]["textEdit"]["newText"], "Ok(${1:v})");
        assert_eq!(items[1]["textEdit"]["newText"], "Err(${1:e})");

        let text = "local s: string = \"a\"\nmatch s with\n    case \nend\n";
        let (st, uri) = one_file(text);
        let items = case_items(&st, uri, text);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["label"], "default");

        let array = "local xs: string[] = {}\nmatch xs with\n    case \nend\n";
        let (st, uri) = one_file(array);
        let items = case_items(&st, uri, array);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["[ first, ...rest ]", "[ ]", "default"]);
        assert_eq!(
            items[0]["textEdit"]["newText"],
            "[ ${1:first}, ...${2:rest} ]"
        );
    }

    /// `await X.m()` moves the receiver into a call the emit wrote, so
    /// the child's own mapping lands past the member.
    #[test]
    pub(crate) fn an_awaited_receiver_keeps_its_member_list() {
        let src = "local function f(p: Future<number>)\n    local s = await Future.all(p)\nend\n";
        let (st, uri) = one_file(src);
        let doc = st.docs.get(uri).unwrap();
        let line = 1u32;
        let column = "    local s = await Future.".len() as u32;

        // The emit wrote `__alloy.await(__alloy.Future.all(p))`, and the
        // child's mapping no longer sits after `Future.`.
        assert!(!lands_on_member(doc, line, column, "Future", '.', 0));

        let shadow_line = doc.shadow.lines().nth(line as usize).unwrap();
        let at = context::member_column(
            doc.source.lines().nth(line as usize).unwrap(),
            shadow_line,
            "Future",
            context::Access::Plain,
            '.',
            0,
            column as usize,
        )
        .expect("a member column");
        assert!(shadow_line[..at].ends_with("Future."));

        // A plain access the emit copied keeps its own position.
        let plain = "local t = { a = 1 }\nlocal v = t.a\n";
        let (st, uri) = one_file(plain);
        let doc = st.docs.get(uri).unwrap();
        assert!(lands_on_member(
            doc,
            1,
            "local v = t.".len() as u32,
            "t",
            '.',
            0
        ));
    }

    /// A remote's surface follows the file's side and the declaration.
    #[test]
    pub(crate) fn a_remote_offers_the_members_its_side_reaches() {
        use alloy::directives::Side;

        let src = concat!(
            "@ratelimit(10, 1)\n",
            "export remote Chat(text: string) from client\n",
            "export remote Toast(message: string) from server\n",
            "export remote function Fetch(id: number) -> number from client\n"
        );
        let chat = remote_spec(src, "Chat").expect("Chat");
        let toast = remote_spec(src, "Toast").expect("Toast");
        let fetch = remote_spec(src, "Fetch").expect("Fetch");

        assert!(chat.ratelimited);
        assert!(!toast.ratelimited);
        assert!(fetch.answers);
        assert!(!chat.answers);

        // The client fires `Chat`; the server handles it.
        assert!(chat.holds("fire", Some(Side::Client)));
        assert!(!chat.holds("on", Some(Side::Client)));
        assert!(!chat.holds("call", Some(Side::Client)));
        assert!(chat.holds("on", Some(Side::Server)));
        assert!(chat.holds("on_ratelimited", Some(Side::Server)));

        // The server fires `Toast`, and only a server fire reaches all.
        assert!(toast.holds("fire_all", Some(Side::Server)));
        assert!(!toast.holds("fire_all", Some(Side::Client)));
        assert!(toast.holds("wait", Some(Side::Client)));
        assert!(!toast.holds("on_ratelimited", Some(Side::Server)));

        // A `remote function` answers, so the firing side may call it.
        assert!(fetch.holds("call", Some(Side::Client)));
        assert!(!fetch.holds("call", Some(Side::Server)));

        // A file with no side of its own sees both surfaces.
        assert!(chat.holds("fire", None) && chat.holds("on", None));
        assert!(!chat.holds("call", None));
    }

    /// A match lowers to one expression, so a `case` binding has no
    /// local; the pattern says what it holds.
    #[test]
    pub(crate) fn a_case_binding_reads_its_payload() {
        const SRC: &str = "struct Boost as\n    stat: string\n    amount: number\nend\n\nenum Effect as\n    Heal(number)\n    Buff(Boost)\nend\n\nlocal function s(e: Effect): number\n    return match e with\n        case Heal(n) then n\n        case Buff(b) then b.amount\n    end\nend\nprint(s)\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let known = st.known_shapes_at(Some(uri));
        let at = |needle: &str| SRC.find(needle).expect("needle");
        let line_of = |o: usize| position_of(SRC, o).0 as usize;
        let heal = at("case Heal(n) then n") + "case Heal(".len();
        let used = at("then n\n") + "then ".len();
        let bound = at("case Buff(b)") + "case Buff(".len();
        let field = at("b.amount") + "b.".len();

        assert_eq!(
            case_binding_text(doc, line_of(heal), heal, "n", &known),
            Some("```alloy\nn: number\n```\nA binding of `Effect.Heal`.".to_string())
        );
        assert_eq!(
            case_binding_text(doc, line_of(used), used, "n", &known),
            Some("```alloy\nn: number\n```\nA binding of `Effect.Heal`.".to_string())
        );
        assert_eq!(
            case_binding_text(doc, line_of(bound), bound, "b", &known),
            Some("```alloy\nb: Boost\n```\nA binding of `Effect.Buff`.".to_string())
        );
        assert_eq!(
            case_binding_text(doc, line_of(field), field, "amount", &known),
            Some("```alloy\namount: number\n```\nA field of `struct Boost`.".to_string())
        );
    }

    /// A record field of a `type` body hovers as the line declares it.
    /// The child sees a table key and answers with an unnamed function
    /// type, which says nothing about the field.
    #[test]
    pub(crate) fn a_type_body_field_reads_as_it_is_written() {
        const SRC: &str = "export type HudProps = {\n    on_swing: () -> (),\n    label: string,\n}\nprint(nil :: HudProps)\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let at = SRC.find("on_swing").expect("on_swing");

        assert_eq!(
            declared_field_hover(doc, at, at + "on_swing".len()),
            Some("```alloy\non_swing: () -> ()\n```\nA field of `type HudProps`.".to_string())
        );
        let label = SRC.find("label").expect("label");

        assert_eq!(
            declared_field_hover(doc, label, label + "label".len()),
            Some("```alloy\nlabel: string\n```\nA field of `type HudProps`.".to_string())
        );
    }

    /// A `type` that names no record has no field to answer for, and a
    /// name below the closed body belongs to nothing.
    #[test]
    pub(crate) fn a_field_hover_stops_at_the_end_of_the_body() {
        const SRC: &str =
            "type Id = number\ntype Props = {\n    a: number,\n}\nlocal b: number = 1\nprint(b)\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let at = SRC.rfind("b: number").expect("b");

        assert_eq!(declared_field_hover(doc, at, at + 1), None);
    }

    /// The key of a struct's raw constructor names the field, past the
    /// visibility the declaration writes.
    #[test]
    pub(crate) fn a_field_key_reads_past_its_visibility() {
        assert_eq!(field_key("    public read id: number"), Some("id"));
        assert_eq!(field_key("    write notes: string = \"\""), Some("notes"));
        assert_eq!(field_key("end"), None);
    }

    /// A remote's parameter reads as the line declares it; the child
    /// measures the string key the emit writes for it.
    #[test]
    pub(crate) fn a_remote_parameter_reads_as_it_is_written() {
        const SRC: &str = "export remote PickUp(@u32 id: number, @u8 count: number) from client\n";
        let (st, uri) = one_file(SRC);
        let doc = st.docs.get(uri).expect("doc");
        let at = SRC.find("id:").expect("id");

        assert_eq!(
            remote_parameter_hover(doc, at, at + 2),
            Some("```alloy\n@u32 id: number\n```\nA parameter of `remote PickUp`.".to_string())
        );
        let second = SRC.find("count:").expect("count");

        assert_eq!(
            remote_parameter_hover(doc, second, second + 5),
            Some("```alloy\n@u8 count: number\n```\nA parameter of `remote PickUp`.".to_string())
        );
    }

    #[test]
    pub(crate) fn a_byte_count_is_the_keys_own_text() {
        assert!(is_byte_count("```alloy\nstring (5 bytes)\n```"));
        assert!(is_byte_count("```luau\nstring (1 byte)\n```"));
        assert!(!is_byte_count("```alloy\nstring\n```"));
    }

    /// A hover that restates the token under the cursor says nothing.
    #[test]
    pub(crate) fn a_type_alias_to_itself_is_no_hover() {
        assert!(restates_itself("```alloy\ntype Player = Player\n```"));
        assert!(restates_itself("```alloy\ntype keyof<T> = keyof<T>\n```"));
        assert!(!restates_itself(
            "```alloy\ntype Profile = { name: string }\n```"
        ));
    }

    /// A file sees its own declarations and what it imports, no more.
    #[test]
    pub(crate) fn a_type_list_holds_what_the_file_can_write() {
        let (st, uri) = one_file(MATCH_FILE);
        let labels: Vec<String> = st
            .type_completions(uri, &[])
            .iter()
            .map(|i| i["label"].as_str().unwrap_or("").to_string())
            .collect();

        assert!(labels.contains(&"Msg".to_string()));
        assert!(labels.contains(&"Answer".to_string()));
        // The std traits a bound takes, and none of the std's own
        // numbered halves.
        assert!(labels.contains(&"Display".to_string()));
        for internal in ["Iter2", "Array3", "ResultMethods2", "Awaitable"] {
            assert!(!labels.contains(&internal.to_string()), "{internal}");
        }
    }

    /// A struct literal lists the fields of its struct, and hides the
    /// private ones outside the impl.
    #[test]
    pub(crate) fn a_struct_literal_lists_its_own_fields() {
        let src = concat!(
            "struct Round as\n",
            "    public phase: Phase\n",
            "    private ready: number\n",
            "end\n",
            "local r = new Round { \n"
        );
        let (st, uri) = one_file(src);
        let offset = src.rfind("{ ").unwrap() + 2;
        let ctx = context::detect(src, offset).expect("a field slot");
        let items = st.context_items(uri, offset, &ctx);
        let labels: Vec<&str> = items
            .iter()
            .map(|i| i["label"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(labels, ["phase"]);
        assert_eq!(items[0]["textEdit"]["newText"], "phase = ${1:phase}");
    }

    /// A signature reads its parameters, `->` and all.
    #[test]
    pub(crate) fn a_signature_drops_its_receiver_and_names_its_arguments() {
        assert_eq!(
            drop_receiver("({ next: (any) -> number? }, (number) -> boolean) -> boolean"),
            Some("((number) -> boolean) -> boolean".to_string())
        );
        assert_eq!(
            call_snippet("earn", "(self: Profile, amount: number) -> number"),
            Some("earn(${1:self}, ${2:amount})$0".to_string())
        );
        assert_eq!(
            call_snippet("alive", "(Profile) -> boolean"),
            Some("alive(${1:Profile})$0".to_string())
        );
        assert_eq!(
            call_snippet("history", "() -> string[]"),
            Some("history()".to_string())
        );
        // A vararg fills no slot of its own.
        assert_eq!(
            call_snippet("flush", "(...any) -> { Event }"),
            Some("flush()".to_string())
        );
        assert_eq!(plain_snippet("Score($1, $2)"), "Score()");
    }

    /// A derived table pair prints no field the struct keeps private.
    #[test]
    pub(crate) fn a_derived_table_hides_the_private_fields() {
        let private: HashSet<String> = ["coins", "log"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            hide_record(
                "(Profile) -> { coins: number, id: number, log: string[], name: string }",
                &private
            ),
            "(Profile) -> { id: number, name: string }"
        );
    }

    #[test]
    pub(crate) fn an_editor_without_snippets_takes_the_plain_text() {
        let (mut st, uri) = one_file(MATCH_FILE);
        st.snippets = false;
        let items = case_items(&st, uri, MATCH_FILE);
        assert_eq!(items[1]["textEdit"]["newText"], "Join()");
        assert!(items[1].get("insertTextFormat").is_none());
        assert_eq!(
            plain_snippet("[ ${1:first}, ...${2:rest} ]"),
            "[ first, ...rest ]"
        );
    }

    #[test]
    pub(crate) fn import_temps_leave_the_type_names() {
        let shadow = "local _1 = require(\"./inventory\") local add = _1.add\n_2 = require(\"./x\")\nlocal m = require(\"./m\")\n";
        assert_eq!(import_temps(shadow), ["_1", "_2"]);
        let mut v =
            json!({ "contents": { "value": "function total(inv: _1.Inventory): _2.Item" } });
        strip_import_temps(&mut v, shadow);
        assert_eq!(
            v["contents"]["value"],
            "function total(inv: Inventory): Item"
        );
    }

    #[test]
    pub(crate) fn the_quoted_path_of_an_import_line() {
        let src = "import * as M from \"./inventory\"\nlocal x = 1\n";
        assert_eq!(quoted_span_on_line(src, 0), Some((19, 32)));
        assert_eq!(quoted_span_on_line(src, 1), None);
    }

    #[test]
    pub(crate) fn a_variant_signature_splits_into_its_payload_types() {
        assert_eq!(
            payload_types("Msg.Move(Player, number)"),
            vec!["Player", "number"]
        );
        assert_eq!(
            payload_types("Msg.Pair({ x: number, y: number }, Map<string, number>)"),
            vec!["{ x: number, y: number }", "Map<string, number>"]
        );
        assert!(payload_types("Msg.Quit").is_empty());
        assert!(payload_types("Msg.Unit()").is_empty());
    }

    #[test]
    pub(crate) fn a_new_name_after_a_declaring_keyword_completes_to_nothing() {
        let src = "enum Col\nlocal x = fo\nfunction hud(a\nimport x from \"./x\"\nprint(x)\n";
        assert!(declares_a_name_at(src, 8));
        assert!(declares_a_name_at(src, 6));
        assert!(!declares_a_name_at(src, 21));
        assert!(!declares_a_name_at(src, 36));
        assert!(declares_a_name_at(src, 45));
        assert!(!declares_a_name_at(src, src.len() - 2));
    }

    #[test]
    pub(crate) fn a_private_view_in_a_message_reads_as_the_struct() {
        let st = State::default();
        let doc = Doc::new(
            "struct Swinger as\n    private last: number\nend\n".to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );
        let mut d = json!({
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "message": "TypeError: Type 'Swinger & Swinger__private & { last: number, scope: Scope }' does not have key 'self'",
        });
        friendly_message(&mut d, &doc, &st);
        assert_eq!(
            d["message"],
            "TypeError: Type 'Swinger' does not have key 'self'"
        );
    }

    #[test]
    pub(crate) fn a_hint_in_parts_folds_as_one_label() {
        let mut result = json!([{
            "position": { "line": 8, "character": 18 },
            "kind": 1,
            "label": [{ "value": ": " }, { "value": "Swinger", "location": {} }, { "value": " & " }, { "value": "Swinger__private" }, { "value": " & { last: number, scope: Scope }" }]
        }]);
        let joined = hint_label(&result[0]);
        result[0]["label"] = json!(joined);
        crate::shapes::fold_value(&mut result, &crate::shapes::Known::default());
        assert_eq!(result[0]["label"], ": Swinger");
    }

    #[test]
    pub(crate) fn a_private_view_hint_folds_through_the_result_path() {
        let mut result = json!([{
            "position": { "line": 8, "character": 18 },
            "kind": 1,
            "label": ": Swinger & Swinger__private & { last: number, scope: Scope }",
            "textEdits": [{ "range": { "start": { "line": 8, "character": 18 }, "end": { "line": 8, "character": 18 } }, "newText": ": Swinger & Swinger__private & { last: number, scope: Scope }" }]
        }]);
        strip_std_prefix(&mut result);
        crate::shapes::fold_value(&mut result, &crate::shapes::Known::default());
        assert_eq!(result[0]["label"], ": Swinger");
    }

    #[test]
    pub(crate) fn a_doc_the_child_read_is_not_added_again() {
        let src = "--- HUD Component\nexport function Hud(props: number): number\n    return props\nend\n";
        let doc = Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        );

        // The shadow keeps the comment, so the child's hover carries it.
        let from_child =
            "```luau\nfunction Hud(props: number): number\n```\n----------\nHUD Component";
        let restyled = restyle_hover(from_child, &doc, 1, 17).expect("restyled");
        assert_eq!(restyled.matches("HUD Component").count(), 1);
        assert!(restyled.starts_with("```alloy\nexport function Hud("));

        // A hover without the doc gets it from the binding.
        let bare = "```luau\nfunction Hud(props: number): number\n```";
        let restyled = restyle_hover(bare, &doc, 1, 17).expect("restyled");
        assert_eq!(restyled.matches("HUD Component").count(), 1);
    }

    #[test]
    pub(crate) fn a_declared_attribute_names_its_targets_in_the_hover() {
        let hover = "```alloy\n@icon(asset: string)\n```\n\n**Applies to** `struct` · `enum`";
        assert_eq!(declared_attribute_targets(hover), vec!["struct", "enum"]);
        assert!(declared_attribute_targets("```alloy\nlocal x\n```").is_empty());
    }

    #[test]
    pub(crate) fn unused_lints_name_the_variable() {
        assert_eq!(
            unused_name(
                "LocalUnused: Variable 'RunService' is never used; prefix with '_' to silence"
            ),
            Some("RunService")
        );
        assert_eq!(
            unused_name("FunctionUnused: Function 'f' is never used"),
            Some("f")
        );
        assert_eq!(unused_name("DeprecatedApi: Member 'x'"), None);
    }

    #[test]
    pub(crate) fn intrinsic_arguments_count_as_uses() {
        let src = "local RunService = game
local f = $nameof(RunService.Heartbeat)
";
        assert!(consumed_by_intrinsic(src, "RunService"));
        assert!(!consumed_by_intrinsic(src, "game"));
        assert!(consumed_by_intrinsic("$stringify(a + b)", "b"));
        assert!(!consumed_by_intrinsic("$stringify(ab)", "b"));
    }

    #[test]
    pub(crate) fn declaration_files_stay_out_of_the_child() {
        assert!(!child_sees("file:///w/globals.d.aly"));
        assert!(child_sees("file:///w/main.aly"));
    }

    #[test]
    pub(crate) fn uris_move_into_the_mirror_and_back() {
        let st = State {
            root: Some(PathBuf::from("/w")),
            mirror: PathBuf::from("/m"),
            ..State::default()
        };
        assert_eq!(st.child_uri("file:///w/a/b.aly"), "file:///m/a/b.luau");
        assert_eq!(st.child_uri("file:///w/b.d.aly"), "file:///m/b.d.luau");
        assert_eq!(st.child_uri("file:///w/ui.alx"), "file:///m/ui.luau");
        assert_eq!(st.child_uri("file:///w/x.luau"), "file:///m/x.luau");
        assert_eq!(
            st.child_uri("file:///else/x.luau"),
            "file:///m/_outside/else/x.luau"
        );
        assert_eq!(
            st.editor_uri("file:///m/x.luau"),
            ("file:///w/x.luau".to_string(), false)
        );
        assert_eq!(
            st.editor_uri("file:///m/_outside/else/x.luau"),
            ("file:///else/x.luau".to_string(), false)
        );
        let p = PathBuf::from("/a b/c.aly");
        assert_eq!(path_to_uri(&p), "file:///a%20b/c.aly");
        assert_eq!(uri_to_path("file:///a%20b/c.aly"), Some(p));
    }

    /// A mount or an alias written by hand: `\` reads as `/`, and a
    /// leading `~` is the home directory.
    #[test]
    pub(crate) fn a_configured_path_reads_backslashes_and_a_tilde() {
        let base = Path::new("/w");
        let home = Path::new("/home/t");
        let at = |text: &str| config_dir_from(base, text, Some(home));
        assert_eq!(at("packages\\roblox"), PathBuf::from("/w/packages/roblox"));
        assert_eq!(at("packages/roblox"), PathBuf::from("/w/packages/roblox"));
        assert_eq!(at("../shared"), PathBuf::from("/shared"));
        assert_eq!(at("~/pkg"), PathBuf::from("/home/t/pkg"));
        assert_eq!(at("~"), PathBuf::from("/home/t"));
        assert_eq!(at("~pkg"), PathBuf::from("/w/~pkg"));
        // No home: the path stays relative to the project.
        assert_eq!(
            config_dir_from(base, "~/pkg", None),
            PathBuf::from("/w/~/pkg")
        );
    }

    /// A Windows URI: the editor writes the drive as `c%3A` and puts a
    /// slash before it. The path keeps the drive and loses the slash.
    #[test]
    pub(crate) fn a_windows_uri_keeps_its_drive() {
        assert_eq!(
            uri_to_path("file:///c%3A/Users/a/x.aly"),
            Some(PathBuf::from("c:/Users/a/x.aly"))
        );
        assert_eq!(
            uri_to_path("file:///C:/Users/a/x.aly"),
            Some(PathBuf::from("C:/Users/a/x.aly"))
        );
        assert_eq!(
            uri_to_path("file:///c%3A/Program%20Files/x.aly"),
            Some(PathBuf::from("c:/Program Files/x.aly"))
        );
        // A path whose second byte is a colon is a drive only when the
        // colon sits right after one letter.
        assert_eq!(
            uri_to_path("file:///ab:/x.aly"),
            Some(PathBuf::from("/ab:/x.aly"))
        );
        assert_eq!(
            path_to_uri(Path::new("c:/Users/a/x.aly")),
            "file:///c:/Users/a/x.aly"
        );
        // A path the editor wrote comes back as the same path.
        for uri in [
            "file:///c%3A/Users/a/x.aly",
            "file:///c%3A/a%20b/x.aly",
            "file:///home/a/x.aly",
        ] {
            let path = uri_to_path(uri).expect("a path");
            assert_eq!(uri_to_path(&path_to_uri(&path)), Some(path), "{uri}");
        }
    }

    #[test]
    pub(crate) fn results_map_back_to_the_source() {
        let mut st = State {
            root: Some(PathBuf::from("/")),
            mirror: PathBuf::from("/m"),
            ..State::default()
        };
        let src = "local v = a ?? 0\nprint(v)\n";
        st.docs.insert(
            "file:///t.aly".to_string(),
            Doc::new(
                src.to_string(),
                1,
                &EmitOptions::default(),
                &alloy::luaux::Config::default(),
                None,
            ),
        );
        st.shadows
            .insert("file:///m/t.luau".to_string(), "file:///t.aly".to_string());

        let mut result = json!([{
            "uri": "file:///m/t.luau",
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
        }]);
        map_from_shadow(&mut result, None, &st);
        assert_eq!(result[0]["uri"], "file:///t.aly");
        assert_eq!(result[0]["range"]["end"]["character"], 5);

        // A range in a plain Luau file is left alone; its URI leaves the
        // mirror.
        let mut other = json!({ "uri": "file:///m/x.luau", "range": { "start": { "line": 9, "character": 9 }, "end": { "line": 9, "character": 9 } } });
        map_from_shadow(&mut other, None, &st);
        assert_eq!(other["range"]["start"]["line"], 9);
        assert_eq!(other["uri"], "file:///x.luau");
    }

    /// A project root in the temp folder, with the files each test
    /// names.
    pub(crate) fn alias_root(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("alloy-lsp-alias-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        for (rel, text) in files {
            let path = dir.join(rel);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the folder");
            }

            std::fs::write(path, text).expect("the file");
        }

        dir
    }

    /// The alias labels an empty import path offers, `@name/` each.
    pub(crate) fn alias_labels(dir: &Path, root: &Path) -> Vec<String> {
        module_entries(dir, Some(root), "", "sourcemap.json", None)
            .into_iter()
            .map(|(label, _, _)| label)
            .filter(|l| l.starts_with('@') && l != "@self/" && l != "@game/")
            .collect()
    }

    /// The list of a directory leaves out the file being edited and
    /// every `.server` or `.client` script: a module never imports
    /// itself, and Roblox runs a script on its own.
    #[test]
    pub(crate) fn an_import_path_lists_neither_the_file_itself_nor_a_script() {
        let dir = alias_root(
            "self",
            &[
                ("alloy.toml", "[build]\nin = \"src\"\n"),
                ("src/main.aly", ""),
                ("src/helper.aly", ""),
                ("src/boot.server.aly", ""),
                ("src/hud.client.luau", ""),
                ("src/plain.luau", ""),
                ("src/sub/leaf.aly", ""),
            ],
        );
        let src = dir.join("src");
        let own = src.join("main.aly");
        let labels = |head: &str| -> Vec<String> {
            module_entries(&src, Some(&dir), head, "sourcemap.json", Some(&own))
                .into_iter()
                .map(|(label, _, _)| label)
                .filter(|l| !l.starts_with('@'))
                .collect()
        };

        assert_eq!(
            labels(""),
            vec![
                "../".to_string(),
                "helper".to_string(),
                "plain".to_string(),
                "sub/".to_string()
            ]
        );
        // `./` lists the same directory, and drops the same names.
        assert_eq!(
            labels("./"),
            vec![
                "helper".to_string(),
                "plain".to_string(),
                "sub/".to_string()
            ]
        );
        // The neighbour's own directory listing keeps `main`.
        let other = src.join("helper.aly");
        let from_other: Vec<String> =
            module_entries(&src, Some(&dir), "./", "sourcemap.json", Some(&other))
                .into_iter()
                .map(|(label, _, _)| label)
                .filter(|l| !l.starts_with('@'))
                .collect();
        assert_eq!(
            from_other,
            vec!["main".to_string(), "plain".to_string(), "sub/".to_string()],
            "the neighbour's list keeps `main`"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    pub(crate) fn an_import_path_offers_the_luau_config_and_the_mounts() {
        let toml = "[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\n";
        let dir = alias_root(
            "merge",
            &[
                ("alloy.toml", toml),
                (
                    ".config.luau",
                    "return { luau = { aliases = { pkg = \"Packages\" } } }\n",
                ),
                ("src/server/main.aly", ""),
            ],
        );
        let src = dir.join("src/server");

        // The Luau configuration and the table both name aliases.
        assert_eq!(
            alias_labels(&src, &dir),
            vec![
                "@pkg/".to_string(),
                "@server/".to_string(),
                "@shared/".to_string()
            ]
        );

        // The same set resolves a path, so `@shared/` lists its files.
        let shared = project_aliases(&src, Some(&dir))
            .into_iter()
            .find(|(a, _)| a == "shared")
            .map(|(_, p)| p);
        assert_eq!(shared, Some(dir.join("src/shared")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    pub(crate) fn a_luaurc_alias_wins_over_a_mount_of_the_same_name() {
        let toml = "[mount]\nshared = [\"src/shared\", \"@game/ReplicatedStorage/Shared\"]\npkg = [\"Packages\", \"@game/ReplicatedStorage/Packages\"]\n";
        let dir = alias_root(
            "clash",
            &[
                ("alloy.toml", toml),
                (
                    ".luaurc",
                    "{ \"aliases\": { \"shared\": \"vendor/shared\" } }\n",
                ),
                ("src/a.aly", ""),
            ],
        );
        let src = dir.join("src");
        let aliases = project_aliases(&src, Some(&dir));

        assert_eq!(
            aliases,
            vec![
                ("pkg".to_string(), dir.join("Packages")),
                ("shared".to_string(), dir.join("vendor/shared")),
            ]
        );
        assert_eq!(
            alias_labels(&src, &dir),
            vec!["@pkg/".to_string(), "@shared/".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    pub(crate) fn mount_aliases_off_leaves_the_luau_config_alone() {
        let toml = "[project]\nmount_aliases = false\n\n[mount]\nserver = [\"src/server\", \"@game/ServerScriptService/Server\"]\n";
        let dir = alias_root(
            "off",
            &[
                ("alloy.toml", toml),
                (".luaurc", "{ \"aliases\": { \"pkg\": \"Packages\" } }\n"),
                ("src/a.aly", ""),
            ],
        );
        let src = dir.join("src");

        assert_eq!(alias_labels(&src, &dir), vec!["@pkg/".to_string()]);
        assert!(
            project_aliases(&src, Some(&dir))
                .iter()
                .all(|(a, _)| a != "server")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    pub(crate) fn a_mirrored_sourcemap_points_at_luau() {
        let text = r#"{"name":"game","className":"DataModel","children":[{"name":"Shared","className":"ModuleScript","filePaths":["src/shared/init.aly"],"children":[{"name":"Alloy","className":"ModuleScript","filePaths":["build/alloy.luau"]}]}]}"#;
        let root = Path::new("/w");
        let out = mirrored_sourcemap(text, &root.join("src"), Some(&root.join("build")), root);
        let json: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["children"][0]["filePaths"][0], "src/shared/init.luau");
        assert_eq!(
            json["children"][0]["children"][0]["filePaths"][0],
            "src/alloy.luau"
        );
    }

    #[test]
    pub(crate) fn capabilities_lose_formatting_and_gain_renames() {
        let mut m = json!({ "result": { "capabilities": {
            "documentFormattingProvider": true,
            "semanticTokensProvider": { "legend": {}, "full": { "delta": true }, "range": true }
        } } });
        edit_capabilities(&mut m);
        let caps = &m["result"]["capabilities"];
        assert_eq!(caps["documentFormattingProvider"], true);
        assert_eq!(caps["semanticTokensProvider"]["full"], true);
        assert!(caps["semanticTokensProvider"].get("range").is_none());
        assert!(caps["workspace"]["fileOperations"]["didRename"].is_object());
    }
    // --- the comment directives ------------------------------------------------

    /// One child diagnostic, as the checker sends it.
    pub(crate) fn child(line: u32, message: &str, severity: u64) -> Value {
        json!({
            "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 4 } },
            "severity": severity,
            "message": message,
        })
    }

    #[test]
    pub(crate) fn a_region_silences_the_checkers_reports_between_its_pair() {
        let source = concat!(
            "--@alloy-ignore-start\n",
            "local a = undefined_one\n",
            "--@alloy-ignore-end\n",
            "local b = undefined_two\n",
        );
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let config = alloy::config::LintConfig::default();

        assert!(!keep_diagnostic(
            &child(1, "TypeError: Unknown global", 1),
            doc,
            None,
            &config
        ));
        assert!(keep_diagnostic(
            &child(3, "TypeError: Unknown global", 1),
            doc,
            None,
            &config
        ));
    }

    #[test]
    pub(crate) fn a_named_region_silences_that_kind_alone() {
        let source = concat!(
            "--@alloy-ignore-start LocalUnused\n",
            "local a = 1\n",
            "local b = 2\n",
            "--@alloy-ignore-end\n",
        );
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let config = alloy::config::LintConfig::default();

        assert!(!keep_diagnostic(
            &child(1, "LocalUnused: Variable 'a' is never used", 2),
            doc,
            None,
            &config
        ));
        assert!(keep_diagnostic(
            &child(2, "LocalShadow: Variable 'b' shadows", 2),
            doc,
            None,
            &config
        ));
    }

    #[test]
    pub(crate) fn a_region_reads_the_kind_the_author_sees() {
        // The child says `Unknown require`; the editor shows
        // `UnknownModule`, and the region names that.
        let source = concat!(
            "--@alloy-ignore-start UnknownModule\n",
            "local a = require(\"./gone\")\n",
            "--@alloy-ignore-end\n",
        );
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let config = alloy::config::LintConfig::default();

        assert!(!keep_diagnostic(
            &child(1, "TypeError: Unknown require: \"./gone\"", 1),
            doc,
            None,
            &config
        ));
    }

    #[test]
    pub(crate) fn an_unmet_expectation_carries_its_reason() {
        // The covered line must come clean, so it holds no lint of
        // its own: an unused local would meet the expectation.
        let source = "--@alloy-expect-error a negative count is refused\nlocal a = 1\nprint(a)\n";
        let (st, uri) = one_file(source);
        let doc = st.docs.get(uri).unwrap();
        let items = unmet_expectations(doc, &[]);
        assert_eq!(items.len(), 1);
        let message = items[0]["message"].as_str().unwrap_or_default();
        assert!(message.contains("a negative count is refused"), "{message}");
        // The report sits on the directive's own line.
        assert_eq!(items[0]["range"]["start"]["line"], 0);

        // A directive over a line the checker reported on says nothing.
        assert!(unmet_expectations(doc, &[child(1, "TypeError: no", 1)]).is_empty());
    }

    #[test]
    pub(crate) fn a_lint_directive_re_levels_one_document() {
        let source = "--@alloy-lint raw_require=deny\nlocal m = require(\"./m\")\n";
        let (st, uri) = one_file(source);
        let items = st.alloy_diagnostics(uri);
        let raw = items
            .iter()
            .find(|d| {
                d["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("raw_require"))
            })
            .expect("the lint reports");
        // Denied, so the editor shows it as an error.
        assert_eq!(raw["severity"], 1);

        let silent = "--@alloy-lint raw_require=allow\nlocal m = require(\"./m\")\n";
        let (st, uri) = one_file(silent);
        assert!(
            !st.alloy_diagnostics(uri).iter().any(|d| {
                d["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("raw_require"))
            }),
            "an allowed lint still reports"
        );
    }

    #[test]
    pub(crate) fn preserve_keeps_the_quick_fix_off_a_line() {
        let plain = "local n = p and p.Name\n";
        let (st, uri) = one_file(plain);
        let range = ((0, 0), (1, 0));
        assert!(
            st.lint_actions(uri, range)
                .iter()
                .any(|a| a["kind"] == "quickfix"),
            "the rewrite is offered"
        );

        let kept = "--@alloy-preserve the two names read better apart\nlocal n = p and p.Name\n";
        let (st, uri) = one_file(kept);
        let range = ((0, 0), (2, 0));
        assert!(
            st.lint_actions(uri, range).is_empty(),
            "a preserved line still offers a rewrite"
        );

        // The lint still reports, and says the line is preserved.
        let message = st
            .alloy_diagnostics(uri)
            .into_iter()
            .find_map(|d| {
                d["message"]
                    .as_str()
                    .filter(|m| m.starts_with("manual_safe_access"))
                    .map(str::to_string)
            })
            .expect("the lint reports");
        assert!(message.contains("--@alloy-preserve"), "{message}");
    }

    #[test]
    pub(crate) fn the_directive_list_holds_every_directive() {
        let source = "--@\n";
        let (st, uri) = one_file(source);
        let items = st.directive_completions(uri, 0, 3);
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();

        for name in alloy::directives::NAMES {
            assert!(labels.contains(name), "`{name}` is not offered");
        }

        // `--@` asks for Alloy's own; the Luau hot comments stay out.
        assert!(!labels.iter().any(|l| l.starts_with("--!")));
    }
}

#[cfg(test)]
mod wording_tests {
    use super::*;

    #[test]
    pub(crate) fn a_colon_call_drops_the_receiver() {
        assert_eq!(
            drop_receiver("(Account, number) -> number").as_deref(),
            Some("(number) -> number")
        );
        assert_eq!(
            drop_receiver("({read number}) -> number?").as_deref(),
            Some("() -> number?")
        );
        assert_eq!(
            drop_receiver("<U>(read number[], (number, number) -> U) -> U[]").as_deref(),
            Some("<U>((number, number) -> U) -> U[]")
        );
    }

    #[test]
    pub(crate) fn a_method_arity_leaves_out_the_receiver() {
        assert_eq!(
            without_self(
                "Argument count mismatch. Function expects 1 argument, but 3 are specified"
            )
            .as_deref(),
            Some("Argument count mismatch. Function expects 0 arguments, but 2 are specified")
        );
        assert_eq!(
            without_self(
                "Argument count mismatch. Function expects 3 arguments, but only 2 are specified"
            )
            .as_deref(),
            Some("Argument count mismatch. Function expects 2 arguments, but only 1 is specified")
        );
    }

    #[test]
    pub(crate) fn a_private_field_leaves_the_constructor_signature() {
        let private: HashSet<String> = ["token".to_string()].into_iter().collect();
        assert_eq!(
            hide_private("({ name: string, token: string? }) -> Cfg", &private),
            "({ name: string }) -> Cfg"
        );
    }

    #[test]
    pub(crate) fn the_key_a_message_says_is_missing() {
        assert_eq!(
            missing_key("TypeError: Type 'Wallet' does not have key 'balance'"),
            Some("balance")
        );
        assert_eq!(missing_key("TypeError: something else"), None);
    }

    #[test]
    pub(crate) fn an_emit_slot_is_no_parameter_hint() {
        assert!(emit_slot_hint(&json!({ "label": "_1:" })));
        assert!(emit_slot_hint(&json!({ "label": "_12:" })));
        assert!(!emit_slot_hint(&json!({ "label": "amount:" })));
        assert!(!emit_slot_hint(&json!({ "label": "_:" })));
    }

    #[test]
    pub(crate) fn a_type_the_source_cannot_write() {
        assert!(writable_type("number[]"));
        assert!(!writable_type("(@checked (string) -> string)?"));
        assert!(!writable_type("t1 where t1 = { }"));
        assert!(!writable_type("*error-type*"));
    }

    #[test]
    pub(crate) fn a_range_on_whitespace_moves_to_the_next_token() {
        let source = "impl Shape for Alias as\n    function area(self): number\n";
        let mut items = vec![json!({
            "range": { "start": { "line": 1, "character": 12 }, "end": { "line": 1, "character": 13 } },
            "message": "x",
        })];
        snap_ranges(&mut items, source);
        assert_eq!(range_of(&items[0]["range"]), Some(((1, 13), (1, 17))));
    }

    #[test]
    pub(crate) fn one_report_per_problem() {
        let one = json!({
            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 5 } },
            "message": "expected `end`",
        });
        let wide = json!({
            "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 3, "character": 9 } },
            "message": "expected `end`",
        });
        let mut items = vec![one.clone(), one.clone(), wide];
        collapse_diagnostics(&mut items);
        assert_eq!(items, vec![one]);
    }

    #[test]
    pub(crate) fn a_method_finds_the_impl_that_writes_it() {
        let source = "impl Counter as\n    function bump(self): number\n        return 1\n    end\n\n    function make(): Counter\n    end\nend\n";
        assert_eq!(method_owner(source, "bump").as_deref(), Some("Counter"));
        assert_eq!(method_owner(source, "make"), None);
    }
}
