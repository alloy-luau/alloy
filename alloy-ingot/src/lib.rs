//! The guest side of the Alloy ingot protocol.
//!
//! An ingot is an Alloy extension. It ships as an executable that the
//! compiler and the language server start and keep alive. Each message is
//! a 4 byte little endian length, then that many bytes of JSON, in both
//! directions over stdin and stdout. Implement [`Handler`] for the state
//! of your ingot and give it to [`serve`]. The function loops until the
//! host closes the pipe.
//!
//! ```no_run
//! use alloy_ingot::{serve, Edit, File, Handler};
//!
//! struct Shout;
//!
//! impl Handler for Shout {
//!     fn transform(&mut self, file: &File) -> Result<Vec<Edit>, String> {
//!         let mut edits = Vec::new();
//!
//!         for (at, _) in file.source.match_indices("$shout") {
//!             edits.push(Edit::replace(at as u32, at as u32 + 6, "string.upper"));
//!         }
//!
//!         Ok(edits)
//!     }
//! }
//!
//! fn main() {
//!     serve(Shout)
//! }
//! ```
//!
//! # The requests the host sends
//!
//! ```jsonc
//! {"op": "init", "api": 1, "root": "/project", "options": {...}, "lints": {...}, "fmt": {...}}
//! {"op": "transform", "path": "src/a.aly", "kind": "aly", "source": "..."}   // reply {"ok": true, "edits": [[4, 20, "new"]]}
//! {"op": "output", "path": "...", "kind": "aly", "source": "..."}            // the ship Luau; reply {"ok": true, "edits": [...]}
//! {"op": "lint", ...}      // reply {"ok": true, "findings": [{"span": [2, 9], "lint": "x", "message": "..."}]}
//! {"op": "format", ...}    // reply {"ok": true, "edits": [...]}
//! {"op": "hover", ..., "offset": 12}      // reply {"ok": true, "hover": {"contents": "md", "span": [10, 14]}}
//! {"op": "complete", ..., "offset": 12}   // reply {"ok": true, "items": [{"label": "x"}]}
//! {"op": "actions", ..., "span": [0, 4]}  // reply {"ok": true, "actions": [{"title": "t", "edits": [...]}]}
//! {"op": "colors", ...}                   // reply {"ok": true, "colors": [{"span": [3, 13], "red": 1, "green": 0, "blue": 0, "alpha": 1}]}
//! {"op": "present", ..., "span": [3, 13], "color": {"red": 1, ...}}  // reply {"ok": true, "labels": ["bg-red-500"]}
//! {"op": "manifest"}                      // reply {"ok": true, "manifest": "ingot.toml text"}
//! ```
//!
//! Every offset is a byte offset into `source`, and every span is half
//! open. An edit replaces the bytes of its span with its text against the
//! source the request carried; the host applies every edit of one reply
//! at once, so no edit sees another's output.
//!
//! An error replies `{"ok": false, "error": "why"}`, and the ingot goes on
//! serving. One bad file must not stop a watch session or an editor.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
pub use serde_json::Value;

/// The protocol revision this crate speaks. The host refuses an ingot
/// whose manifest names another `api`.
pub const API: u32 = 1;

/// One file the host asks about.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct File {
    /// The path relative to the project root, or absolute when the file
    /// sits outside it.
    pub path: String,
    /// `aly`, `alx`, `d.aly`, or `luau`.
    #[serde(default)]
    pub kind: String,
    /// The whole text.
    #[serde(default)]
    pub source: String,
}

impl File {
    /// The line and column of a byte offset, both zero based.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let at = (offset as usize).min(self.source.len());
        let before = &self.source[..at];
        let line = before.matches('\n').count() as u32;
        let col = (at - before.rfind('\n').map_or(0, |i| i + 1)) as u32;

        (line, col)
    }

    /// The byte span of the line that holds an offset, without its newline.
    pub fn line_span(&self, offset: u32) -> (u32, u32) {
        let at = (offset as usize).min(self.source.len());
        let start = self.source[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = self.source[at..]
            .find('\n')
            .map_or(self.source.len(), |i| at + i);

        (start as u32, end as u32)
    }

    /// The text of the line that holds an offset.
    pub fn line_text(&self, offset: u32) -> &str {
        let (s, e) = self.line_span(offset);

        &self.source[s as usize..e as usize]
    }

    /// The identifier under an offset: letters, digits, and `_`, with its span.
    pub fn word_at(&self, offset: u32) -> Option<(&str, (u32, u32))> {
        let bytes = self.source.as_bytes();
        let at = (offset as usize).min(bytes.len());
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut start = at;

        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }

        let mut end = at;

        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }

        if start == end {
            return None;
        }

        Some((&self.source[start..end], (start as u32, end as u32)))
    }
}

/// A whole span replacement: the start byte, the end byte, and the new
/// text. The host keeps the line count: the text must hold as many
/// newlines as the span it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit(pub u32, pub u32, pub String);

impl Edit {
    pub fn replace(start: u32, end: u32, text: impl Into<String>) -> Self {
        Self(start, end, text.into())
    }

    pub fn insert(at: u32, text: impl Into<String>) -> Self {
        Self(at, at, text.into())
    }

    pub fn remove(start: u32, end: u32) -> Self {
        Self(start, end, String::new())
    }
}

/// One problem a lint found. The level is absent by intent: the host
/// owns the levels, the suppression, and the exit codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The byte span in the source.
    pub span: (u32, u32),
    /// The lint name, as `[lints]` in `ingot.toml` declares it.
    pub lint: String,
    pub message: String,
    /// A rewrite that keeps the program the same, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Edit>,
}

impl Finding {
    pub fn new(lint: impl Into<String>, span: (u32, u32), message: impl Into<String>) -> Self {
        Self {
            span,
            lint: lint.into(),
            message: message.into(),
            fix: None,
        }
    }

    pub fn with_fix(mut self, fix: Edit) -> Self {
        self.fix = Some(fix);

        self
    }
}

/// A hover answer: markdown, and the span it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hover {
    pub contents: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(u32, u32)>,
}

impl Hover {
    pub fn new(contents: impl Into<String>) -> Self {
        Self {
            contents: contents.into(),
            span: None,
        }
    }

    pub fn over(mut self, span: (u32, u32)) -> Self {
        self.span = Some(span);

        self
    }
}

/// The kind of a completion item, by the LSP's numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

impl ItemKind {
    /// The LSP `CompletionItemKind` number.
    pub fn code(self) -> u32 {
        self as u32 + 1
    }
}

/// One completion item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ItemKind>,
    /// The short text beside the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Markdown shown in the documentation pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    /// The text inserted, when it differs from the label. `${1:x}` is a
    /// snippet placeholder when `snippet` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub snippet: bool,
    /// The span the insert replaces. Unset means the word at the cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(u32, u32)>,
}

impl CompletionItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: None,
            detail: None,
            documentation: None,
            insert: None,
            snippet: false,
            span: None,
        }
    }

    pub fn kind(mut self, kind: ItemKind) -> Self {
        self.kind = Some(kind);

        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());

        self
    }

    pub fn documentation(mut self, doc: impl Into<String>) -> Self {
        self.documentation = Some(doc.into());

        self
    }

    pub fn insert(mut self, text: impl Into<String>) -> Self {
        self.insert = Some(text.into());

        self
    }

    pub fn snippet(mut self, text: impl Into<String>) -> Self {
        self.insert = Some(text.into());
        self.snippet = true;

        self
    }

    pub fn over(mut self, span: (u32, u32)) -> Self {
        self.span = Some(span);

        self
    }
}

/// One code action: a title and the edits it applies to the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAction {
    pub title: String,
    /// `quickfix`, `refactor`, `source`, or a dotted subkind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub edits: Vec<Edit>,
}

impl CodeAction {
    pub fn new(title: impl Into<String>, edits: Vec<Edit>) -> Self {
        Self {
            title: title.into(),
            kind: None,
            edits,
        }
    }

    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());

        self
    }
}

/// A diagnostic in the range of a code action request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticRef {
    pub span: (u32, u32),
    pub message: String,
    /// The lint name when the diagnostic is a lint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint: Option<String>,
}

/// A color a file names, for the editor's swatch and picker. The
/// channels run from 0 to 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorInfo {
    pub span: (u32, u32),
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl ColorInfo {
    /// A color from 8 bit channels and an alpha from 0 to 1.
    pub fn rgb(span: (u32, u32), (r, g, b): (u8, u8, u8), alpha: f64) -> Self {
        Self {
            span,
            red: f64::from(r) / 255.0,
            green: f64::from(g) / 255.0,
            blue: f64::from(b) / 255.0,
            alpha,
        }
    }
}

/// A color the editor picked, sent back for a presentation.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Default)]
pub struct Color {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl Color {
    /// The 8 bit channels.
    pub fn rgb8(self) -> (u8, u8, u8) {
        let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;

        (c(self.red), c(self.green), c(self.blue))
    }
}

/// The resolved settings of the project, sent once at init.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    /// The `[ingot.<name>]` table of the project over the defaults the
    /// manifest declares.
    #[serde(default)]
    pub options: Value,
    /// The level of each lint the manifest declares: `allow`, `warn`,
    /// or `deny`. A lint at `allow` is not run.
    #[serde(default)]
    pub lints: BTreeMap<String, String>,
    /// The `[fmt]` table of the project, so an ingot lays its own
    /// constructs out in the style the project asked for.
    #[serde(default)]
    pub fmt: Value,
    /// The project root, absolute. Resolve files against it and never
    /// against the working directory: the compiler runs in the project
    /// and the language server runs wherever the editor started it.
    #[serde(default)]
    pub root: String,
}

/// The operations of an ingot. Each default is a refusal or an empty
/// answer. Implement the ones your `ingot.toml` lists under `hooks`; the
/// host never sends an operation the manifest does not list.
pub trait Handler {
    /// The settings, sent once before the first file.
    fn init(&mut self, settings: &Settings) -> Result<(), String> {
        let _ = settings;

        Ok(())
    }

    /// Edits to the Alloy source, before the desugar. The host keeps the
    /// line count and maps every position through the edits.
    fn transform(&mut self, file: &File) -> Result<Vec<Edit>, String> {
        let _ = file;

        Err("this ingot does not transform".into())
    }

    /// Edits to the ship Luau, after the desugar. Nothing maps back.
    fn output(&mut self, file: &File) -> Result<Vec<Edit>, String> {
        let _ = file;

        Err("this ingot does not rewrite the output".into())
    }

    /// The problems of one file.
    fn lint(&mut self, file: &File) -> Result<Vec<Finding>, String> {
        let _ = file;

        Err("this ingot does not lint".into())
    }

    /// Edits to a file Anneal formatted.
    fn format(&mut self, file: &File) -> Result<Vec<Edit>, String> {
        let _ = file;

        Err("this ingot does not format".into())
    }

    /// The hover at an offset, or `None` to let the host answer.
    fn hover(&mut self, file: &File, offset: u32) -> Result<Option<Hover>, String> {
        let _ = (file, offset);

        Ok(None)
    }

    /// Completion items at an offset. They join the host's list.
    fn complete(
        &mut self,
        file: &File,
        offset: u32,
        trigger: Option<&str>,
    ) -> Result<Vec<CompletionItem>, String> {
        let _ = (file, offset, trigger);

        Ok(Vec::new())
    }

    /// Code actions for a span. They join the host's list.
    fn actions(
        &mut self,
        file: &File,
        span: (u32, u32),
        diagnostics: &[DiagnosticRef],
    ) -> Result<Vec<CodeAction>, String> {
        let _ = (file, span, diagnostics);

        Ok(Vec::new())
    }

    /// The colors a file names, for the editor's swatches. They join the
    /// host's list.
    fn colors(&mut self, file: &File) -> Result<Vec<ColorInfo>, String> {
        let _ = file;

        Ok(Vec::new())
    }

    /// The texts that name `color` at a span the file colors: the labels
    /// the picker offers. Empty when the span is not this ingot's.
    fn present(
        &mut self,
        file: &File,
        span: (u32, u32),
        color: Color,
    ) -> Result<Vec<String>, String> {
        let _ = (file, span, color);

        Ok(Vec::new())
    }

    /// The `ingot.toml` text this binary carries. `cargo install` ships
    /// one binary and no data files; an ingot that returns its manifest
    /// installs from crates.io, and the host writes the text beside the
    /// binary. Embed the file with `include_str!("../ingot.toml")`.
    fn manifest(&self) -> Option<&'static str> {
        None
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Init {
        #[serde(default)]
        api: u32,
        #[serde(flatten)]
        settings: Settings,
    },
    Transform {
        #[serde(flatten)]
        file: File,
    },
    Output {
        #[serde(flatten)]
        file: File,
    },
    Lint {
        #[serde(flatten)]
        file: File,
    },
    Format {
        #[serde(flatten)]
        file: File,
    },
    Hover {
        #[serde(flatten)]
        file: File,
        offset: u32,
    },
    Complete {
        #[serde(flatten)]
        file: File,
        offset: u32,
        #[serde(default)]
        trigger: Option<String>,
    },
    Actions {
        #[serde(flatten)]
        file: File,
        span: (u32, u32),
        #[serde(default)]
        diagnostics: Vec<DiagnosticRef>,
    },
    Colors {
        #[serde(flatten)]
        file: File,
    },
    Present {
        #[serde(flatten)]
        file: File,
        span: (u32, u32),
        #[serde(default)]
        color: Color,
    },
    Manifest,
}

/// Serves the host until it closes the pipe.
///
/// A handler error becomes an `{"ok": false, "error": ...}` reply and
/// does not stop the process. The function returns at end of file, which
/// means the host dropped the ingot.
pub fn serve(mut handler: impl Handler) {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    loop {
        let Some(body) = read_frame(&mut input) else {
            return;
        };

        let reply = match serde_json::from_slice::<Request>(&body) {
            Ok(request) => answer(&mut handler, request),

            Err(e) => error_reply(format!("cannot read the request: {e}")),
        };

        write_frame(&mut output, &reply);
    }
}

fn answer(handler: &mut impl Handler, request: Request) -> Vec<u8> {
    let reply = match request {
        Request::Init { api, settings } => {
            if api != 0 && api != API {
                return error_reply(format!(
                    "this ingot speaks api {API}, the host speaks api {api}"
                ));
            }

            handler
                .init(&settings)
                .map(|()| serde_json::json!({ "ok": true }))
        }

        Request::Transform { file } => handler
            .transform(&file)
            .map(|edits| serde_json::json!({ "ok": true, "edits": edits })),

        Request::Output { file } => handler
            .output(&file)
            .map(|edits| serde_json::json!({ "ok": true, "edits": edits })),

        Request::Lint { file } => handler
            .lint(&file)
            .map(|findings| serde_json::json!({ "ok": true, "findings": findings })),

        Request::Format { file } => handler
            .format(&file)
            .map(|edits| serde_json::json!({ "ok": true, "edits": edits })),

        Request::Hover { file, offset } => handler
            .hover(&file, offset)
            .map(|hover| serde_json::json!({ "ok": true, "hover": hover })),

        Request::Complete {
            file,
            offset,
            trigger,
        } => handler
            .complete(&file, offset, trigger.as_deref())
            .map(|items| serde_json::json!({ "ok": true, "items": items })),

        Request::Actions {
            file,
            span,
            diagnostics,
        } => handler
            .actions(&file, span, &diagnostics)
            .map(|actions| serde_json::json!({ "ok": true, "actions": actions })),

        Request::Colors { file } => handler
            .colors(&file)
            .map(|colors| serde_json::json!({ "ok": true, "colors": colors })),

        Request::Present { file, span, color } => handler
            .present(&file, span, color)
            .map(|labels| serde_json::json!({ "ok": true, "labels": labels })),

        Request::Manifest => match handler.manifest() {
            Some(text) => Ok(serde_json::json!({ "ok": true, "manifest": text })),

            None => Err("this ingot does not carry its manifest".into()),
        },
    };

    match reply {
        Ok(value) => serde_json::to_vec(&value).expect("a reply always serializes"),

        Err(why) => error_reply(why),
    }
}

fn error_reply(why: String) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "ok": false, "error": why }))
        .expect("a reply always serializes")
}

/// Reads one length prefixed frame, or `None` at end of file.
pub fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;

    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    input.read_exact(&mut body).ok()?;

    Some(body)
}

/// Writes one length prefixed frame.
pub fn write_frame(output: &mut impl Write, body: &[u8]) {
    let len = u32::try_from(body.len()).expect("a reply under 4GB");

    // A failed write means the host is gone, and no receiver is left to tell.
    let _ = output.write_all(&len.to_le_bytes());
    let _ = output.write_all(body);
    let _ = output.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Shout;

    impl Handler for Shout {
        fn transform(&mut self, file: &File) -> Result<Vec<Edit>, String> {
            Ok(vec![Edit::replace(
                0,
                file.source.len() as u32,
                file.source.to_uppercase(),
            )])
        }

        fn hover(&mut self, file: &File, offset: u32) -> Result<Option<Hover>, String> {
            Ok(file
                .word_at(offset)
                .map(|(w, span)| Hover::new(format!("word `{w}`")).over(span)))
        }
    }

    fn reply(json: &str) -> String {
        let request: Request = serde_json::from_str(json).unwrap();

        String::from_utf8(answer(&mut Shout, request)).unwrap()
    }

    fn value(json: &str) -> Value {
        serde_json::from_str(&reply(json)).unwrap()
    }

    #[test]
    fn a_transform_round_trips_through_answer() {
        assert_eq!(
            value(r#"{"op":"transform","path":"a.aly","source":"hi"}"#),
            serde_json::json!({ "ok": true, "edits": [[0, 2, "HI"]] })
        );
    }

    #[test]
    fn a_hover_names_the_word() {
        assert_eq!(
            value(r#"{"op":"hover","path":"a.aly","source":"local abc","offset":7}"#),
            serde_json::json!({ "ok": true, "hover": { "contents": "word `abc`", "span": [6, 9] } })
        );
    }

    #[test]
    fn an_undeclared_op_refuses_rather_than_panics() {
        let r = reply(r#"{"op":"lint","path":"a.aly","source":"x"}"#);

        assert!(r.contains(r#""ok":false"#), "{r}");
        assert!(r.contains("does not lint"), "{r}");
    }

    #[test]
    fn an_api_mismatch_is_refused_at_init() {
        let r = reply(r#"{"op":"init","api":9}"#);

        assert!(r.contains("api 1"), "{r}");
    }

    #[test]
    fn file_helpers_find_lines_and_words() {
        let f = File {
            path: "a".into(),
            kind: "aly".into(),
            source: "local a\nprint(ab_c)\n".into(),
        };

        assert_eq!(f.line_col(8), (1, 0));
        assert_eq!(f.line_text(10), "print(ab_c)");
        assert_eq!(f.word_at(15), Some(("ab_c", (14, 18))));
        assert_eq!(
            f.word_at(13),
            Some(("print", (8, 13))),
            "the cursor at the end of a word"
        );
        assert_eq!(f.word_at(19), None);
    }

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{}");
        let mut cursor = std::io::Cursor::new(buf);

        assert_eq!(read_frame(&mut cursor), Some(b"{}".to_vec()));
        assert_eq!(read_frame(&mut cursor), None);
    }
}
