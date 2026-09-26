//! Ingots: Alloy's extensions.
//!
//! An ingot is an executable beside an `ingot.toml`. The compiler and
//! the language server start it once, keep it alive, and send it files
//! over a framed pipe; the guest side is the `alloy-ingot` crate. An
//! ingot edits Alloy source before the desugar, edits the ship Luau
//! after it, lints, formats after Anneal, and answers hover,
//! completion, and code actions in the editor. Its options and lint
//! levels come from `alloy.toml`. The transform keeps the line count and
//! maps through the span map, so a position in the editor always lands
//! on the author's text.
//!
//! The design follows larvae's native worms: one process per ingot,
//! JSON frames, one request per file. No interpreter is embedded.

pub mod fetch;
pub mod manifest;
pub mod process;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::Config;
use crate::lint::{ExternalLint, Level, Lint};
use crate::render::{Edit, SpanMap, apply_edits};
use crate::{Diagnostic, Fix, Output};
pub use manifest::{Hook, Manifest};

/// The wait for an editor request; a build waits `process::TIMEOUT`.
const EDITOR_TIMEOUT: Duration = Duration::from_secs(4);

/// One loaded ingot.
pub struct Ingot {
    pub name: String,
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub binary: PathBuf,
    /// The pass its transform runs in.
    pub order: i64,
    process: Option<process::Process>,
    /// The levels of its lints under the project, sent at init.
    lint_levels: BTreeMap<String, Level>,
}

/// Something that went wrong with one ingot, at load or on a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub ingot: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ingot `{}`: {}", self.ingot, self.message)
    }
}

/// The ingots of one project, in name order.
#[derive(Default)]
pub struct Ingots {
    pub list: Vec<Ingot>,
    pub root: PathBuf,
    /// What failed at load. A failed ingot is not in `list`.
    pub problems: Vec<Problem>,
}

/// The result of the source transforms of one file.
pub struct Layer {
    pub text: String,
    /// The map from the author's text to `text`; `None` when nothing ran.
    pub map: Option<SpanMap>,
    pub diagnostics: Vec<Diagnostic>,
}

/// The kind word of a source path: `aly`, `alx`, or `d.aly`.
pub fn kind_of(path: &str) -> &'static str {
    if path.ends_with(".d.aly") {
        "d.aly"
    } else if path.ends_with(".alx") {
        "alx"
    } else if path.ends_with(".luau") || path.ends_with(".lua") {
        "luau"
    } else {
        "aly"
    }
}

/// The completion answer of the ingots at one position.
#[derive(Debug, Default)]
pub struct Completed {
    pub items: Vec<Value>,
    /// Whether the next keystroke gives a different list. An ingot that
    /// builds its items from the word being typed says so, and the editor
    /// asks again instead of filtering what it holds.
    pub incomplete: bool,
    /// Whether the host adds its own list after the items. Unset, the
    /// ingot owns the spot.
    pub merge: bool,
    /// Whether the host's list leaves out the Roblox classes and their
    /// properties and events.
    pub hide_roblox: bool,
    /// The Roblox class the host completes an attribute slot as, for a
    /// tag the ingot rewrites into that class.
    pub class: Option<String>,
}

impl Ingots {
    /// Loads every ingot of a config. A problem with one ingot never
    /// stops the others; the problems come back beside the list.
    pub fn load(root: &Path, config: &Config) -> Ingots {
        // An ingot runs in its own directory and reads the root it is
        // told: a relative one, `.` from the command line, would name
        // that directory instead of the project.
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let root = root.as_path();
        let mut out = Ingots {
            root: root.to_path_buf(),
            ..Ingots::default()
        };

        let mut started = Vec::new();

        for name in config.ingots.keys() {
            match Ingot::load(root, name, config) {
                Ok(pair) => started.push(pair),

                Err(message) => out.problems.push(Problem {
                    ingot: name.clone(),
                    message,
                }),
            }
        }

        // Init names the ingots that started, so an ingot never counts
        // on one that failed to load.
        let names: Vec<String> = started.iter().map(|(i, _)| i.name.clone()).collect();

        for (ingot, mut init) in started {
            init["ingots"] = json!(names);

            match ingot.process.as_ref().map(|p| p.init(init)) {
                Some(Err(e)) => out.problems.push(Problem {
                    ingot: ingot.name.clone(),
                    message: format!("init failed: {e}"),
                }),

                _ => out.list.push(ingot),
            }
        }

        out
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The path an ingot sees: relative to the root when possible.
    fn rel(&self, path: &str) -> String {
        Path::new(path)
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string())
    }

    fn file(&self, path: &str, source: &str) -> Value {
        json!({ "path": self.rel(path), "kind": kind_of(path), "source": source })
    }

    fn with_hook<'a>(&'a self, hook: Hook, path: &str) -> impl Iterator<Item = &'a Ingot> {
        let kind = kind_of(path);

        self.list
            .iter()
            .filter(move |i| i.manifest.has(hook) && i.manifest.wants(kind))
    }

    /// The source transforms, one pass per distinct order. Every edit of
    /// a pass measures against the pass input, and a later pass reads
    /// the earlier one's output.
    pub fn before(&self, path: &str, source: &str) -> Layer {
        let mut orders: Vec<i64> = self
            .with_hook(Hook::Transform, path)
            .map(|i| i.order)
            .collect();
        orders.sort_unstable();
        orders.dedup();

        let mut text = source.to_string();
        let mut map: Option<SpanMap> = None;
        let mut diagnostics = Vec::new();

        for order in orders {
            let mut edits: Vec<Edit> = Vec::new();
            let mut owners: Vec<&str> = Vec::new();

            for ingot in self
                .with_hook(Hook::Transform, path)
                .filter(|i| i.order == order)
            {
                let mut request = self.file(path, &text);
                request["op"] = json!("transform");

                match ingot.request(&request, process::TIMEOUT) {
                    Ok(reply) => {
                        for e in edits_of(&reply["edits"]) {
                            edits.push(e);
                            owners.push(&ingot.name);
                        }
                    }

                    Err(why) => diagnostics.push(problem(&ingot.name, &why, 0, 0)),
                }
            }

            if edits.is_empty() {
                continue;
            }

            let (next, layer, errors) = apply_edits(&text, &edits);

            for e in errors {
                let owner = edits
                    .iter()
                    .position(|x| *x == e.edit)
                    .map(|i| owners[i])
                    .unwrap_or("?");
                let (start, end) = match &map {
                    Some(m) => (m.to_source(e.edit.start), m.to_source(e.edit.end)),

                    None => (e.edit.start, e.edit.end),
                };
                // A refused edit may split a character, and a report at
                // its offsets would slice inside one.
                diagnostics.push(problem(
                    owner,
                    &e.message,
                    on_char(source, start),
                    on_char(source, end),
                ));
            }

            text = next;
            map = Some(match map {
                Some(outer) => outer.compose(&layer),

                None => layer,
            });
        }

        Layer {
            text,
            map,
            diagnostics,
        }
    }

    /// After the desugar: the lints over the author's text, and the
    /// output edits over the ship artifact.
    pub fn after(&self, path: &str, source: &str, out: &mut Output) {
        for ingot in self.with_hook(Hook::Lint, path) {
            if ingot.lint_levels.values().all(|l| *l == Level::Allow) {
                continue;
            }

            let mut request = self.file(path, source);
            request["op"] = json!("lint");

            match ingot.request(&request, process::TIMEOUT) {
                Ok(reply) => {
                    for f in reply["findings"].as_array().into_iter().flatten() {
                        let Some(lint) = f["lint"].as_str() else {
                            continue;
                        };

                        if !ingot.manifest.lints.contains_key(lint) {
                            out.diagnostics.push(problem(
                                &ingot.name,
                                &format!(
                                    "reported the lint `{lint}` its manifest does not declare"
                                ),
                                0,
                                0,
                            ));

                            continue;
                        }

                        let (start, end) = span_of(&f["span"]).unwrap_or((0, 0));
                        let start = on_char(source, start);
                        let end = on_char(source, end).max(start);
                        let fix = edits_of(&json!([f["fix"]]))
                            .into_iter()
                            .next()
                            .map(|e| Fix::new(source, e.start, e.end, e.text));
                        out.lints.push(Lint {
                            name: crate::lint::intern(&format!("{}/{lint}", ingot.name)),
                            start,
                            end,
                            message: f["message"].as_str().unwrap_or("").to_string(),
                            fix,
                        });
                    }
                }

                Err(why) => out.diagnostics.push(problem(&ingot.name, &why, 0, 0)),
            }
        }

        for ingot in self.with_hook(Hook::Output, path) {
            let mut request = self.file(path, &out.ship);
            request["op"] = json!("output");

            match ingot.request(&request, process::TIMEOUT) {
                Ok(reply) => {
                    let edits = edits_of(&reply["edits"]);
                    let (next, _, errors) = apply_edits(&out.ship, &edits);

                    for e in errors {
                        out.diagnostics.push(problem(
                            &ingot.name,
                            &format!("output edit: {}", e.message),
                            0,
                            0,
                        ));
                    }

                    out.ship = next;
                }

                Err(why) => out.diagnostics.push(problem(&ingot.name, &why, 0, 0)),
            }
        }

        out.lints.sort_by_key(|l| (l.start, l.name));
        out.diagnostics.sort_by_key(|d| d.start);
    }

    /// The format hooks over text Anneal laid out. The line count may
    /// change here; a formatter owns its layout.
    pub fn format(&self, path: &str, text: &str) -> (String, Vec<Problem>) {
        let mut text = text.to_string();
        let mut problems = Vec::new();

        for ingot in self.with_hook(Hook::Format, path) {
            let mut request = self.file(path, &text);
            request["op"] = json!("format");

            match ingot.request(&request, process::TIMEOUT) {
                Ok(reply) => match splice(&text, &edits_of(&reply["edits"])) {
                    Ok(next) => text = next,

                    Err(why) => problems.push(Problem {
                        ingot: ingot.name.clone(),
                        message: why,
                    }),
                },

                Err(why) => problems.push(Problem {
                    ingot: ingot.name.clone(),
                    message: why,
                }),
            }
        }

        (text, problems)
    }

    /// The first hover an ingot answers: `{ contents, span? }`.
    pub fn hover(&self, path: &str, source: &str, offset: u32) -> Option<Value> {
        for ingot in self.with_hook(Hook::Hover, path) {
            let mut request = self.file(path, source);
            request["op"] = json!("hover");
            request["offset"] = json!(offset);

            if let Ok(reply) = ingot.request(&request, EDITOR_TIMEOUT)
                && reply["hover"].is_object()
            {
                return Some(reply["hover"].clone());
            }
        }

        None
    }

    /// Every completion item the ingots offer, as the guest shapes them,
    /// and what the replies ask of the host's own list.
    pub fn complete(
        &self,
        path: &str,
        source: &str,
        offset: u32,
        trigger: Option<&str>,
    ) -> Completed {
        let mut out = Completed::default();

        for ingot in self.with_hook(Hook::Complete, path) {
            let mut request = self.file(path, source);
            request["op"] = json!("complete");
            request["offset"] = json!(offset);
            request["trigger"] = json!(trigger);

            if let Ok(reply) = ingot.request(&request, EDITOR_TIMEOUT) {
                let items = reply["items"].as_array().cloned().unwrap_or_default();
                let flag = |key: &str| reply[key].as_bool() == Some(true);

                // The flags count with no items too: an ingot may ask for
                // the host's list alone, completed as another class.
                out.incomplete |= flag("incomplete");
                out.merge |= flag("merge");
                out.hide_roblox |= flag("hide_roblox");
                out.class = out
                    .class
                    .or_else(|| reply["class"].as_str().map(str::to_string));

                out.items.extend(items);
            }
        }

        out
    }

    /// The props the ingots read on a markup tag of a file: the name,
    /// what it is for, the ingot that reads it, and the snippet the
    /// editor inserts. Neither the Roblox class nor the component
    /// declares one, so the editor lists them from here.
    pub fn props(&self, path: &str) -> Vec<(&str, &str, &str, &str)> {
        let kind = kind_of(path);
        let mut out = Vec::new();

        for ingot in self.list.iter().filter(|i| i.manifest.wants(kind)) {
            for (name, decl) in &ingot.manifest.props {
                out.push((
                    name.as_str(),
                    decl.doc(),
                    ingot.name.as_str(),
                    decl.insert(),
                ));
            }
        }

        out
    }

    /// Every code action the ingots offer for a span.
    pub fn actions(
        &self,
        path: &str,
        source: &str,
        span: (u32, u32),
        diagnostics: &[Value],
    ) -> Vec<Value> {
        let mut actions = Vec::new();

        for ingot in self.with_hook(Hook::Actions, path) {
            let mut request = self.file(path, source);
            request["op"] = json!("actions");
            request["span"] = json!([span.0, span.1]);
            request["diagnostics"] = json!(diagnostics);

            if let Ok(reply) = ingot.request(&request, EDITOR_TIMEOUT) {
                actions.extend(reply["actions"].as_array().cloned().unwrap_or_default());
            }
        }

        actions
    }

    /// Every color the ingots find in a file: `{ span, red, green,
    /// blue, alpha }` each, the channels from 0 to 1.
    pub fn colors(&self, path: &str, source: &str) -> Vec<Value> {
        let mut colors = Vec::new();

        for ingot in self.with_hook(Hook::Colors, path) {
            let mut request = self.file(path, source);
            request["op"] = json!("colors");

            if let Ok(reply) = ingot.request(&request, EDITOR_TIMEOUT) {
                colors.extend(reply["colors"].as_array().cloned().unwrap_or_default());
            }
        }

        colors
    }

    /// The labels the ingots offer for a picked color at a span; empty
    /// when no ingot owns the span.
    pub fn present(
        &self,
        path: &str,
        source: &str,
        span: (u32, u32),
        color: &Value,
    ) -> Vec<String> {
        for ingot in self.with_hook(Hook::Colors, path) {
            let mut request = self.file(path, source);
            request["op"] = json!("present");
            request["span"] = json!([span.0, span.1]);
            request["color"] = color.clone();

            if let Ok(reply) = ingot.request(&request, EDITOR_TIMEOUT) {
                let labels: Vec<String> = reply["labels"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                if !labels.is_empty() {
                    return labels;
                }
            }
        }

        Vec::new()
    }

    /// The ingots that hold a hook, by name.
    pub fn names_with(&self, hook: Hook) -> Vec<&str> {
        self.list
            .iter()
            .filter(|i| i.manifest.has(hook))
            .map(|i| i.name.as_str())
            .collect()
    }
}

impl Ingot {
    /// Starts the ingot, and gives back the init request to send once
    /// every ingot has started.
    fn load(root: &Path, name: &str, config: &Config) -> Result<(Ingot, Value), String> {
        let table = config.ingots[name].table();
        let dir = match (&table.path, &table.repo) {
            (Some(p), _) => root.join(p),

            // A build fetches nothing. `alloy ingot install` puts the
            // release in the store and the lock file names it.
            (None, Some(_)) => fetch::resolve(root, name, &table)?,

            (None, None) => return Err("names neither a path nor a repo".to_string()),
        };
        let path = dir.join(manifest::FILE_NAME);
        // The report names the manifest as the toml wrote it,
        // `ingots/x/ingot.toml`, not the absolute path the read used.
        let manifest = Manifest::load(&path).map_err(|e| match &table.path {
            Some(p) => e.replace(
                &path.display().to_string(),
                &Path::new(p).join(manifest::FILE_NAME).display().to_string(),
            ),

            None => e,
        })?;

        if manifest.name != name {
            return Err(format!(
                "the manifest names it `{}`; the key under [ingots] must match",
                manifest.name
            ));
        }

        let binary = find_binary(&dir, &manifest.binary_name()).ok_or_else(|| {
            format!(
                "no binary `{}` in {}; build the ingot first",
                manifest.binary_name(),
                dir.display()
            )
        })?;
        let order = table
            .order
            .or_else(|| manifest.run.map(|r| r.order()))
            .unwrap_or(0);

        // The lints register before the levels resolve, so `[lint]`
        // names them by `<ingot>/<lint>` or by the ingot's name. The
        // `lints` switches of `[ingots]` replace the manifest's default:
        // every report reads the level through `level_of`, so a switch
        // that only reached the init left the finding at its old level.
        let external: Vec<ExternalLint> = manifest
            .lints
            .iter()
            .map(|(lint, decl)| ExternalLint {
                name: crate::lint::intern(&format!("{name}/{lint}")),
                group: crate::lint::intern(name),
                default: match (table.lints.get(lint), level_from(&decl.default)) {
                    (Some(false), _) => Level::Allow,

                    (Some(true), Level::Allow) => Level::Warn,

                    (_, level) => level,
                },
                summary: decl.summary.clone(),
                detail: decl.detail.clone(),
            })
            .collect();
        crate::lint::register_external(name, external);

        if let Some(lint) = table
            .lints
            .keys()
            .find(|l| !manifest.lints.contains_key(*l))
        {
            return Err(format!(
                "`lints` switches `{lint}`, which the manifest does not declare"
            ));
        }

        let lint_levels: BTreeMap<String, Level> = manifest
            .lints
            .keys()
            .map(|lint| {
                let level = crate::lint::level_of(&config.lint, &format!("{name}/{lint}"));

                (lint.clone(), level)
            })
            .collect();

        let mut options = manifest.options.clone();

        if let Some(user) = config.ingot.get(name) {
            for (k, v) in user {
                if !manifest.options.contains_key(k) {
                    return Err(format!(
                        "[ingot.{name}] sets `{k}`, which the manifest does not declare"
                    ));
                }

                options.insert(k.clone(), v.clone());
            }
        }

        let process = process::Process::start(&binary, &dir)?;
        let init = json!({
            "op": "init",
            "api": manifest::API,
            "root": root.to_string_lossy(),
            "options": serde_json::to_value(&options).unwrap_or(Value::Null),
            "lints": lint_levels.iter().map(|(k, v)| (k.clone(), level_name(*v))).collect::<BTreeMap<_, _>>(),
            "fmt": serde_json::to_value(&config.fmt).unwrap_or(Value::Null),
            // The markup settings, from `alloy.toml` or `.config.aly` alike,
            // so an ingot never parses the config file itself.
            "alx": serde_json::to_value(&config.alx).unwrap_or(Value::Null),
        });

        Ok((
            Ingot {
                name: name.to_string(),
                manifest,
                dir,
                binary,
                order,
                process: Some(process),
                lint_levels,
            },
            init,
        ))
    }

    fn request(&self, body: &Value, timeout: Duration) -> Result<Value, String> {
        match &self.process {
            Some(p) => p.request(body, timeout),

            None => Err("the ingot is not running".to_string()),
        }
    }

    pub fn alive(&self) -> bool {
        self.process.as_ref().is_some_and(|p| p.alive())
    }
}

/// The binary beside the manifest, else under a cargo `target` tree so
/// an ingot in development runs without a copy step.
pub fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    binary_places(dir, name).into_iter().find(|p| p.is_file())
}

/// Every place [`find_binary`] looks, in order.
pub fn binary_places(dir: &Path, name: &str) -> [PathBuf; 3] {
    [
        dir.join(name),
        dir.join("target").join("release").join(name),
        dir.join("target").join("debug").join(name),
    ]
}

fn level_from(word: &str) -> Level {
    match word {
        "allow" => Level::Allow,
        "deny" => Level::Deny,
        _ => Level::Warn,
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Allow => "allow",
        Level::Warn => "warn",
        Level::Deny => "deny",
    }
}

fn problem(ingot: &str, message: &str, start: u32, end: u32) -> Diagnostic {
    Diagnostic {
        start,
        end,
        message: format!("ingot `{ingot}`: {message}"),
    }
}

/// An offset an ingot sent, inside the text and on a character.
fn on_char(text: &str, at: u32) -> u32 {
    crate::directives::char_floor(text, at as usize) as u32
}

fn span_of(v: &Value) -> Option<(u32, u32)> {
    let a = v.as_array()?;

    Some((a.first()?.as_u64()? as u32, a.get(1)?.as_u64()? as u32))
}

/// The edits of a reply: `[[start, end, text], ...]`.
fn edits_of(v: &Value) -> Vec<Edit> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let a = e.as_array()?;

            Some(Edit {
                start: a.first()?.as_u64()? as u32,
                end: a.get(1)?.as_u64()? as u32,
                text: a.get(2)?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// Applies edits with no line rule, for a formatter.
fn splice(text: &str, edits: &[Edit]) -> Result<String, String> {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.start, e.end));
    let mut out = String::with_capacity(text.len());
    let mut at = 0u32;

    for e in sorted {
        if e.start < at || e.start > e.end || e.end as usize > text.len() {
            return Err(format!(
                "format edit {}..{} overlaps another or leaves the file",
                e.start, e.end
            ));
        }

        if !text.is_char_boundary(e.start as usize) || !text.is_char_boundary(e.end as usize) {
            return Err(format!(
                "format edit {}..{} splits a character",
                e.start, e.end
            ));
        }

        out.push_str(&text[at as usize..e.start as usize]);
        out.push_str(&e.text);
        at = e.end;
    }

    out.push_str(&text[at as usize..]);

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_the_extension() {
        assert_eq!(kind_of("a/b.aly"), "aly");
        assert_eq!(kind_of("a/b.alx"), "alx");
        assert_eq!(kind_of("types.d.aly"), "d.aly");
        assert_eq!(kind_of("x.luau"), "luau");
    }

    #[test]
    fn a_splice_refuses_overlap() {
        let e = |s, t, x: &str| Edit {
            start: s,
            end: t,
            text: x.into(),
        };
        assert_eq!(
            splice("abcdef", &[e(1, 3, "X"), e(4, 5, "")]).unwrap(),
            "aXdf"
        );
        assert!(splice("abc", &[e(0, 2, "X"), e(1, 3, "Y")]).is_err());
    }

    #[test]
    fn reply_edits_and_spans_parse() {
        let v = json!([[1, 2, "x"], [3, 3, ""], ["bad"]]);
        let e = edits_of(&v);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1].text, "");
        assert_eq!(span_of(&json!([4, 9])), Some((4, 9)));
        assert_eq!(span_of(&json!(null)), None);
    }
}
