use super::*;

impl State {
    /// Auto-import items for a completion at a source position.
    pub(crate) fn auto_imports(&self, uri: &str, line: u32, character: u32) -> Vec<Value> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let Some(offset) = offset_of(&doc.source, line, character) else {
            return Vec::new();
        };
        let prefix = imports::word_before(&doc.source, offset);

        if prefix.is_empty() {
            return Vec::new();
        }

        let bound = markup_bound(&doc.source);
        let files: Vec<(PathBuf, &[imports::Export])> = self
            .docs
            .iter()
            .filter_map(|(u, d)| uri_to_path(u).map(|p| (p, d.exports.as_slice())))
            .collect();

        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let aliases = project_aliases(&dir, self.root.as_deref());

        imports::auto_import_items(&doc.source, &path, &files, &prefix, &bound, &aliases)
    }

    /// The child's module and service auto-imports, as Alloy imports.
    ///
    /// luau-lsp offers a module by its instance path and inserts a
    /// `require`. Alloy writes `import name from "@pkg/name"`, so the
    /// item carries the module's name, the spec the project's aliases
    /// give it, and one edit that writes the import under the last one.
    /// A module a dot folder or an `_Index` folder holds, one the
    /// editor's `ignoreGlobs` name, one no alias and no `[build] in`
    /// reaches, and one the file already imports are dropped.
    ///
    /// A service row inserts `local X = game:GetService("X")`. Alloy
    /// writes `import X from "game:X"` instead, or the name inside an
    /// `import { ... } from "game"` line the file already has. A service
    /// the file imports is no offer, so neither form joins the other.
    pub(crate) fn rewrite_child_auto_imports(&self, uri: &str, result: &mut Value) {
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let aliases = project_aliases(&dir, self.root.as_deref());
        let mounts = self.instance_mounts();
        let input = self.input_dir();
        let taken = imports::imported_specs(&doc.source);
        let services = imports::imported_services(&doc.source);
        let source = doc.source.clone();
        let ignored = self.import_ignore_globs();
        let items = match result {
            Value::Array(v) => v,

            Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
                Some(v) => v,

                None => return,
            },

            _ => return,
        };

        items.retain_mut(|item| {
            // A module under a service the file does not bind yet
            // carries both edits, the `GetService` line and the
            // `require` line. It is a module offer, so the module test
            // reads the row before the service one.
            if !is_module_auto_import(item) {
                if let Some(service) = service_auto_import(item) {
                    if services.contains(&service) {
                        return false;
                    }

                    item["label"] = json!(service);
                    item["detail"] = json!(format!("game:GetService(\"{service}\")"));
                    item["insertText"] = json!(service);
                    item["additionalTextEdits"] =
                        json!([imports::service_import_edit(&source, &service)]);

                    return true;
                }

                return true;
            }

            let Some(instance) = item.get("detail").and_then(Value::as_str) else {
                return false;
            };

            if ignored.hides(item, instance) {
                return false;
            }

            let Some(file) = module_file_of(instance, &mounts) else {
                return false;
            };
            let Some(spec) = imports::best_spec(&dir, &file, &aliases) else {
                return false;
            };
            let under_input = input.as_ref().is_some_and(|i| file.starts_with(i));

            // A module the file reads is no offer, and a relative spec
            // outside `[build] in` names a package store the author
            // never writes.
            if taken.contains(&spec) || (spec.starts_with('.') && !under_input) {
                return false;
            }

            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // An Alloy module binds whole under `* as`; a bare name
            // would read its `export default`. A plain Luau module
            // returns one value, which is what a bare name takes.
            let is_alloy = file.extension().is_some_and(|e| e == "aly" || e == "alx");
            let edit = match is_alloy {
                true => imports::namespace_import_edit(&source, &spec, &name),

                false => imports::import_edit(
                    &source,
                    &spec,
                    &imports::Export {
                        name: name.clone(),
                        is_type: false,
                        is_default: true,
                        kind: 9,
                    },
                ),
            };
            item["label"] = json!(name);
            item["detail"] = json!(spec);
            item["insertText"] = json!(name);
            item["additionalTextEdits"] = json!([edit]);

            true
        });
    }
}

/// Whether an item is the child's auto-import of a module: it inserts a
/// `require`, so its detail is the instance path of a module file.
pub(crate) fn is_module_auto_import(item: &Value) -> bool {
    if !is_auto_import(item) {
        return false;
    }

    item.get("additionalTextEdits")
        .and_then(Value::as_array)
        .is_some_and(|edits| {
            edits.iter().any(|e| {
                e.get("newText")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.contains("= require("))
            })
        })
}

/// The service an auto-import row would bind, when the row inserts
/// `local X = game:GetService("X")`.
pub(crate) fn service_auto_import(item: &Value) -> Option<String> {
    if !is_auto_import(item) {
        return None;
    }

    let edits = item.get("additionalTextEdits")?.as_array()?;

    edits.iter().find_map(|e| {
        let text = e.get("newText")?.as_str()?;
        let at = text.find("= game:GetService(\"")? + "= game:GetService(\"".len();
        let name = &text[at..];
        let name = &name[..name.find('"')?];

        alloy::game_import::is_service(name).then(|| name.to_string())
    })
}

/// The globs the luau-lsp extension ships in
/// `luau-lsp.completion.imports.ignoreGlobs`.
const DEFAULT_IGNORE_GLOBS: [&str; 1] = ["**/_Index/**"];

/// What the auto-import lists leave out. A module under a dot folder or
/// an `_Index` folder is a package's own store, not a module the author
/// writes, and `luau-lsp.completion.imports.ignoreGlobs` names more.
pub(crate) struct Ignored {
    globs: globset::GlobSet,
}

impl Ignored {
    /// Whether a row names a module no list offers. The child spells
    /// the path twice: `detail` holds the Luau expression, and the last
    /// line of the documentation holds the plain path the globs match.
    pub(crate) fn hides(&self, item: &Value, instance: &str) -> bool {
        let segments = instance_segments(instance);

        if segments.iter().any(|s| s.starts_with('.') || s == "_Index") {
            return true;
        }

        let path = documented_path(item).unwrap_or_else(|| segments.join("/"));

        path.split('/').any(|s| s.starts_with('.') || s == "_Index") || self.globs.is_match(&path)
    }
}

/// The plain path under the require snippet in an auto-import row:
/// `game/ReplicatedStorage/Packages/.ember/x`.
fn documented_path(item: &Value) -> Option<String> {
    let text = item
        .pointer("/documentation/value")
        .or_else(|| item.get("documentation"))
        .and_then(Value::as_str)?;
    let last = text.trim_end().lines().next_back()?.trim();

    (last.contains('/') && !last.contains(char::is_whitespace) && !last.contains('`'))
        .then(|| last.to_string())
}

impl State {
    /// The ignore globs the editor sends, else the luau-lsp default.
    pub(crate) fn import_ignore_globs(&self) -> Ignored {
        let patterns: Vec<String> = match self
            .settings
            .pointer("/completion/imports/ignoreGlobs")
            .and_then(Value::as_array)
        {
            Some(list) => list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),

            None => DEFAULT_IGNORE_GLOBS.iter().map(|g| g.to_string()).collect(),
        };

        Ignored {
            globs: alloy::build::globs(&patterns).unwrap_or_else(|_| globset::GlobSet::empty()),
        }
    }
}
