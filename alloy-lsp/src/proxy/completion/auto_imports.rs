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
    /// A module a dot folder holds, one no alias and no `[build] in`
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
        let items = match result {
            Value::Array(v) => v,

            Value::Object(o) => match o.get_mut("items").and_then(Value::as_array_mut) {
                Some(v) => v,

                None => return,
            },

            _ => return,
        };

        items.retain_mut(|item| {
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

            if !is_module_auto_import(item) {
                return true;
            }

            let Some(instance) = item.get("detail").and_then(Value::as_str) else {
                return false;
            };
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
