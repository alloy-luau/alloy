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

        let quote = imports::quote_for(&doc.source, self.fmt_config(uri).quote_style);

        imports::auto_import_items(&doc.source, &path, &files, &prefix, &bound, &aliases, quote)
    }

    /// Every name the project's other modules export, with the spec
    /// that reaches each one. An `import { }` list that names no
    /// module yet reads them, so the reader picks a name first and the
    /// accept writes the `from` clause. The modules are the open ones
    /// and the ones an alias reaches.
    pub(crate) fn project_exports(
        &self,
        uri: &str,
        prefix: &str,
    ) -> Vec<(String, imports::Export)> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        let Some(path) = uri_to_path(uri) else {
            return Vec::new();
        };
        let bound = markup_bound(&doc.source);
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let aliases = project_aliases(&dir, self.root.as_deref());
        let reached = self.alias_modules(&dir, &aliases);
        let mut files: Vec<(PathBuf, &[imports::Export])> = self
            .docs
            .iter()
            .filter_map(|(u, d)| uri_to_path(u).map(|p| (p, d.exports.as_slice())))
            .collect();

        files.extend(reached.iter().map(|(p, e)| (p.clone(), e.as_slice())));

        imports::auto_import_candidates(&doc.source, &path, &files, prefix, &bound, &aliases)
            .into_iter()
            .map(|(spec, export)| (spec, export.clone()))
            .collect()
    }

    /// The modules an alias reaches that the workspace walk never
    /// opened, each with its exports read from the disk. An alias can
    /// name a folder outside the root, and a name a module there
    /// exports is one the author can import. `best_spec` answers
    /// nothing for a path a dot folder holds, so a package's own store
    /// stays out of the list.
    ///
    /// ponytail: the walk runs per request. It answers one caret, the
    /// `import { }` list, so it costs a folder read at a rare
    /// position; cache it beside `project_impls` if a bigger list
    /// calls it.
    fn alias_modules(
        &self,
        from_dir: &Path,
        aliases: &[(String, PathBuf)],
    ) -> Vec<(PathBuf, Vec<imports::Export>)> {
        let mut out: Vec<(PathBuf, Vec<imports::Export>)> = Vec::new();

        for (_, alias_dir) in aliases {
            let mut files = Vec::new();
            let mut plain = Vec::new();
            super::super::documents::walk(alias_dir, None, &mut files, &mut plain);

            for file in files {
                if self.docs.contains_key(&path_to_uri(&file))
                    || out.iter().any(|(p, _)| *p == file)
                    || imports::best_spec(from_dir, &imports::module_path(&file), aliases).is_none()
                {
                    continue;
                }

                let exports = imports::exports_of_file(&file, 0);

                out.push((file, exports));
            }
        }

        out
    }

    /*
    Quick fixes for a name another module exports that this file does
    not import: one `import` line per module that has the name.

    The completion list already walks the workspace for these edits. The
    action reads the same walk, so the line it writes and the line the
    completion inserts cannot drift apart.
    */
    pub(crate) fn import_actions(&self, uri: &str, diagnostics: &[Value]) -> Vec<Value> {
        let mut actions = Vec::new();
        let Some(doc) = self.docs.get(uri) else {
            return actions;
        };
        let Some(path) = uri_to_path(uri) else {
            return actions;
        };
        let bound = markup_bound(&doc.source);
        let files: Vec<(PathBuf, &[imports::Export])> = self
            .docs
            .iter()
            .filter_map(|(u, d)| uri_to_path(u).map(|p| (p, d.exports.as_slice())))
            .collect();
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let aliases = project_aliases(&dir, self.root.as_deref());
        let quote = imports::quote_for(&doc.source, self.fmt_config(uri).quote_style);
        let mut seen: Vec<String> = Vec::new();

        for report in diagnostics {
            let Some(message) = report.get("message").and_then(Value::as_str) else {
                continue;
            };
            let Some(name) = unresolved_name(message) else {
                continue;
            };
            // `Geo.Vec` is a member of a namespace, and a reader reaches
            // it through the group: the import names `Geo`.
            let name = name.split('.').next().unwrap_or(name);
            // A struct is a value and a type. A name an annotation
            // alone uses imports as a type; a `new Name`, a `Name.`, or
            // a `Name(` in the file wants the value, which is the type
            // too, so one fix resolves the file.
            let as_type = message.contains("Unknown type '") && !uses_as_value(&doc.source, name);
            let offers =
                imports::auto_import_candidates(&doc.source, &path, &files, name, &bound, &aliases);

            for (spec, export) in offers {
                // The prefix walk answers every name that starts with
                // this one; the report named exactly one.
                if export.name != name {
                    continue;
                }

                let typed = as_type && !export.is_default && matches!(export.kind, 7 | 8 | 13);
                let export = imports::Export {
                    is_type: export.is_type || typed,
                    ..export.clone()
                };
                let shape = imports::import_shape(&spec, &export, quote);

                if seen.contains(&shape) {
                    continue;
                }

                seen.push(shape.clone());
                actions.push(json!({
                    "title": format!("Add `{shape}`"),
                    "kind": "quickfix",
                    "isPreferred": true,
                    "diagnostics": [report],
                    "edit": { "changes": { uri: [imports::import_edit(&doc.source, &spec, &export, quote)] } },
                }));
            }
        }

        actions
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
    /// writes `import X from "@game/X"` instead, or the name inside an
    /// `import { ... } from "@game"` line the file already has. A
    /// service the file imports is no offer, so neither form joins the
    /// other.
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
        let quote = imports::quote_for(&source, self.fmt_config(uri).quote_style);
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
                        json!([imports::service_import_edit(&source, &service, quote)]);

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
                true => imports::namespace_import_edit(&source, &spec, &name, quote),

                false => imports::import_edit(
                    &source,
                    &spec,
                    &imports::Export {
                        name: name.clone(),
                        is_type: false,
                        is_default: true,
                        is_attribute: false,
                        kind: 9,
                    },
                    quote,
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
    globs: alloy::globset::GlobSet,
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
            globs: alloy::build::globs(&patterns)
                .unwrap_or_else(|_| alloy::globset::GlobSet::empty()),
        }
    }
}

/// The name an unresolved report names, for the import that would
/// resolve it: `Unknown type 'Vec2'` and
/// `Unknown global 'Utils'; consider assigning to it first`.
fn unresolved_name(message: &str) -> Option<&str> {
    // `new Widget { }` on a name no import bound: the desugar finds a
    // type with no struct behind it, so the report names neither an
    // unknown type nor an unknown global. The name still wants the
    // import, and a name the file already binds offers nothing.
    if let Some((_, rest)) = message.split_once('`')
        && let Some((name, _)) = rest.split_once("` is a type, not a struct")
    {
        return Some(name);
    }

    // A module the file imports already exports the name: the import
    // takes it into that list.
    if let Some((_, rest)) = message.split_once('`')
        && let Some((name, _)) = rest.split_once("` is not imported;")
    {
        return Some(name);
    }

    let rest = message
        .split_once("Unknown type '")
        .or_else(|| message.split_once("Unknown global '"))
        .map(|(_, rest)| rest)?;

    rest.split_once('\'').map(|(name, _)| name)
}

/// Whether a source reads `Name` as a value: `new Name`, a member
/// `Name.x`, or a call `Name(`. A type alone stands in none of them.
fn uses_as_value(src: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';

    src.match_indices(name).any(|(i, _)| {
        let before = &src[..i];
        let after = &src[i + name.len()..];

        if before.ends_with(word) || after.starts_with(word) {
            return false;
        }

        after.starts_with('.') || after.starts_with('(') || {
            let head = before.trim_end();

            head.ends_with("new") && !head[..head.len() - 3].ends_with(word)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{unresolved_name, uses_as_value};

    /// A member, a call, and a `new` read the value; an annotation
    /// alone does not, and a longer name is another name.
    #[test]
    fn a_value_use_is_a_member_a_call_or_a_new() {
        assert!(uses_as_value("return Status.Ok\n", "Status"));
        assert!(uses_as_value("local s = Status(1)\n", "Status"));
        assert!(uses_as_value("local p = new Point { x = 1 }\n", "Point"));
        assert!(!uses_as_value(
            "local s: Status = x\nlocal t = OtherStatus.Kind\n",
            "Status"
        ));
    }

    #[test]
    fn an_unresolved_report_names_what_to_import() {
        assert_eq!(
            unresolved_name("TypeError: Unknown type 'Vec2'"),
            Some("Vec2")
        );
        assert_eq!(
            unresolved_name("TypeError: Unknown global 'Utils'; consider assigning to it first"),
            Some("Utils")
        );
        // `new Vec2 { }` on a name no import bound: the desugar reports
        // a type with no struct behind it.
        assert_eq!(
            unresolved_name("TypeError: `Vec2` is a type, not a struct"),
            Some("Vec2")
        );
        assert_eq!(
            unresolved_name(
                "TypeError: `ORIGIN` is not imported; \"./shapes\" exports it, so add it to that import"
            ),
            Some("ORIGIN")
        );
        assert_eq!(unresolved_name("unused_variable: `x` is never read"), None);
    }
}
