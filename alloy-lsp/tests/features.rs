//! End to end over the protocol: hover on Alloy syntax, completion while
//! typing, and an extension method on a foreign type. Needs luau-lsp;
//! skips when it is not installed.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn luau_lsp() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ALLOY_LUAU_LSP") {
        return Some(PathBuf::from(p));
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        PathBuf::from(&home).join(".ember/bin/luau-lsp"),
        PathBuf::from(&home).join(".cargo/bin/luau-lsp"),
        PathBuf::from("/usr/local/bin/luau-lsp"),
        PathBuf::from("/usr/bin/luau-lsp"),
    ];

    candidates.into_iter().find(|p| p.exists())
}

/// The Roblox definitions checked into the repo.
fn global_types() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/types/globalTypes.d.luau")
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write(w: &mut impl Write, v: &Value) {
    let body = serde_json::to_vec(v).unwrap();
    write!(w, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    w.write_all(&body).unwrap();
    w.flush().unwrap();
}

fn read(r: &mut impl BufRead) -> Option<Value> {
    let mut length = 0usize;

    loop {
        let mut line = String::new();

        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }

        let line = line.trim_end();

        if line.is_empty() {
            break;
        }

        if let Some(rest) = line.strip_prefix("Content-Length:") {
            length = rest.trim().parse().ok()?;
        }
    }

    let mut body = vec![0u8; length];
    std::io::Read::read_exact(r, &mut body).ok()?;

    serde_json::from_slice(&body).ok()
}

fn messages(stdout: std::process::ChildStdout) -> Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);

        while let Some(m) = read(&mut reader) {
            if tx.send(m).is_err() {
                break;
            }
        }
    });

    rx
}

/// The whole editor side of one session.
struct Session {
    stdin: std::process::ChildStdin,
    rx: Receiver<Value>,
    seen: Vec<Value>,
    next_id: u64,
    _server: KillOnDrop,
}

impl Session {
    fn next(&mut self) -> Value {
        let m = self
            .rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("no message within 30s; seen: {:#?}", self.seen));
        self.seen.push(m.clone());

        m
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        write(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );

        loop {
            let m = self.next();

            if m.get("id") == Some(&json!(id)) {
                return m["result"].clone();
            }
        }
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> String {
        let r = self.request(
            "textDocument/hover",
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        );

        r["contents"]["value"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| r.to_string())
    }

    fn completion_labels(&mut self, uri: &str, line: u32, character: u32) -> Vec<String> {
        let r = self.request(
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        );
        let items = r
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| r.as_array())
            .cloned()
            .unwrap_or_default();

        items
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    }

    /// Waits for a diagnostics batch for the URI that satisfies `want`.
    /// The server publishes its own empty batch before the child has
    /// analyzed anything, so the first batch proves little.
    fn diagnostics(&mut self, uri: &str, want: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(30);

        while Instant::now() < deadline {
            let m = self.next();

            if m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri {
                let messages: Vec<String> = m["params"]["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|d| d["message"].as_str().map(str::to_string))
                    .collect();

                if want(&messages) {
                    return messages;
                }
            }
        }

        panic!("no matching diagnostics for {uri}; seen: {:#?}", self.seen);
    }
}

const EXT: &str = "\
export impl Vector3
    function flat(self): Vector3
        return Vector3.new(self.X, 0, self.Z)
    end

    function origin(): Vector3
        return Vector3.new(0, 0, 0)
    end
end

export impl string
    function trim(self): string
        return self:match(\"^%s*(.-)%s*$\")
    end

    function shout(s: string): string
        return s:upper()
    end
end
";

const MAIN: &str = "\
--!strict
struct Vec2 as
    x: number
    y: number
end
local p = new Vec2 { x = 1, y = 2 }
local v = Vector3.new(1, 2, 3)
local f = v:flat()
local o = Vector3.origin()
local cache = {}
cache[1] ??= 5
local part = workspace:FindFirstChild(\"Part\")
print(p, v, f, o, cache, part)
local partial = Vec
local g = game:GetSer()
local q = p.x
local t = (\"  hi  \"):trim()
local w = string.shout(t)
local n: number = t
local u = (\"  hi  \"):upper()
local x = string.len(t)
local hm = HashMap.new()
interface Named as
    name: string
end
interface Entity extends Named as
    id: number
end
const limit = 3
async function fetch_it(): number
    return 1
end
export const answer = 42
local async function later(): number
    return 2
end
print(limit, fetch_it, answer, later)
async function stamp()
    return os.clock()
end
print(stamp)
-- export is only a word here
local async function tail_fn(): number
    return 3
end
print(tail_fn)
--- The message a client sends.
enum Msg as
    Quit
    Move(number)
end
local mv = Msg.Move(1)
local who = match mv with
    case Move(n) then n
    default 0
end
print(who)
attribute icon(asset: string) on struct
macro twice(x)
    x * 2
end
@icon(\"rbxassetid://1\")
struct Tagged as
    id: number
end
local tw = $twice(2)
remote Ping(sent_at: number) from client
local items: Vec2[] = []
local part: Partial<Vec2> = {}
type Sink<T> = { [K in keyof T]: write T[K] }
local out: Sink<Vec2> = { x = 1, y = 2 }
print(tw, items, part, out)
local fut = async do
    return 1
end
local [ h, ...rs ] = [ 1, 2 ]
local box = new Instance(\"Part\") {
    Name = \"m\",
}
print(fut, h, rs, box)
import * as Ext from \"./e\"
import * as Symbol from \"./ext\"
print(Symbol)
";

fn start(child: &Path, dir: &Path) -> Session {
    let root = format!("file://{}", dir.display());

    start_with(
        child,
        json!({ "processId": std::process::id(), "rootUri": root, "capabilities": {} }),
    )
}

/// A session initialized with the given `initialize` params.
fn start_with(child: &Path, init_params: Value) -> Session {
    let mut server = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_alloy-lsp"))
            .arg("--luau-lsp")
            .arg(child)
            .arg("--definitions")
            .arg(global_types())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if std::env::var_os("ALLOY_LSP_LOG").is_some() {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()
            .unwrap(),
    );
    let stdin = server.0.stdin.take().unwrap();
    let rx = messages(server.0.stdout.take().unwrap());
    let mut s = Session {
        stdin,
        rx,
        seen: Vec::new(),
        next_id: 0,
        _server: server,
    };
    let init = s.request("initialize", init_params);
    let triggers = init["capabilities"]["completionProvider"]["triggerCharacters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        triggers.contains(&json!("@")) && triggers.contains(&json!("$")),
        "{triggers:?}"
    );
    assert!(
        init["capabilities"]["hoverProvider"].is_boolean()
            || init["capabilities"]["hoverProvider"].is_object(),
        "{init}"
    );
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    s
}

#[test]
fn hover_completion_and_extensions() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-features-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ext.aly"), EXT).unwrap();
    let main = dir.join("main.aly");
    std::fs::write(&main, MAIN).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", main.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": MAIN } } }),
    );

    // The extension methods type: no diagnostic names them, and the
    // string extension's return type reaches the `number` mismatch.
    let diags = s.diagnostics(&uri, |ds| {
        ds.iter()
            .any(|d| d.contains("string") && d.contains("number"))
    });
    assert!(
        diags
            .iter()
            .all(|d| !d.contains("flat") && !d.contains("origin") && !d.contains("trim")),
        "{diags:#?}"
    );

    // Hover on Alloy syntax.
    let h = s.hover(&uri, 1, 2);
    assert!(h.contains("A record with fields"), "struct: {h}");
    let h = s.hover(&uri, 1, 8);
    assert!(
        h.contains("struct Vec2 as\n    x: number\n    y: number\nend"),
        "struct name: {h}"
    );
    let h = s.hover(&uri, 10, 10);
    assert!(h.contains("Assigns `b` to `a`"), "??=: {h}");

    // Hover on an extension method shows the injected signature.
    let h = s.hover(&uri, 7, 13);
    assert!(h.contains("flat") && h.contains("Vector3"), "flat: {h}");

    // Completion after `v:` lists the extension, after `Vector3.` the static.
    let labels = s.completion_labels(&uri, 7, 12);
    assert!(labels.iter().any(|l| l == "flat"), "{labels:?}");
    let labels = s.completion_labels(&uri, 8, 18);
    assert!(labels.iter().any(|l| l == "origin"), "{labels:?}");

    // Completion while typing: a global, a method, a struct field.
    let labels = s.completion_labels(&uri, 13, 19);
    assert!(labels.iter().any(|l| l == "Vector3"), "{labels:?}");
    // The std names are ambient, so they join every plain completion.
    assert!(labels.iter().any(|l| l == "HashMap"), "{labels:?}");
    let labels = s.completion_labels(&uri, 14, 21);
    assert!(labels.iter().any(|l| l == "GetService"), "{labels:?}");
    let labels = s.completion_labels(&uri, 15, 12);
    assert!(
        labels.iter().any(|l| l == "x") && labels.iter().any(|l| l == "y"),
        "{labels:?}"
    );

    // A primitive extension: hover shows its type, completion after `:`
    // on a string lists it beside the string methods, and `string.` lists
    // the static. Completion is asked on plain calls, the way typing
    // reaches a name before it exists.
    let h = s.hover(&uri, 16, 22);
    assert!(h.contains("trim") && h.contains("string"), "trim: {h}");
    let labels = s.completion_labels(&uri, 19, 21);
    assert!(labels.iter().any(|l| l == "trim"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "upper"), "{labels:?}");
    let labels = s.completion_labels(&uri, 20, 17);
    assert!(labels.iter().any(|l| l == "shout"), "{labels:?}");

    // A completion the editor triggers with a newline, the child's `end`
    // trigger, gets no added names: Enter must not pop a list.
    let r = s.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 36, "character": 0 },
            "context": { "triggerKind": 2, "triggerCharacter": "\n" } }),
    );
    let labels: Vec<&str> = r
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| r.as_array())
        .map(|items| items.iter().filter_map(|i| i["label"].as_str()).collect())
        .unwrap_or_default();
    assert!(!labels.contains(&"HashMap"), "{labels:?}");

    // A std name hovers with its Alloy doc, not the raw table type.
    let h = s.hover(&uri, 21, 12);
    assert!(
        h.contains("HashMap.new()") && h.contains("get_or_insert"),
        "HashMap: {h}"
    );

    // An interface hovers as the declaration, with what it extends.
    let h = s.hover(&uri, 25, 12);
    assert!(
        h.contains("interface Entity extends Named as\n    id: number\nend"),
        "interface: {h}"
    );

    // A binding hovers with the keywords the source used.
    let h = s.hover(&uri, 28, 7);
    assert!(h.starts_with("```alloy\nconst limit"), "const: {h}");

    // A keyword in a comment before a declaration is only a word.
    let h = s.hover(&uri, 42, 22);
    assert!(
        h.starts_with("```alloy\nlocal async function tail_fn"),
        "comment: {h}"
    );

    // An enum carries its doc comment; a variant hovers from `Msg.Move`
    // and from a pattern, where the emit has only a string.
    let h = s.hover(&uri, 47, 6);
    assert!(
        h.contains("enum Msg as") && h.contains("The message a client sends."),
        "enum: {h}"
    );
    let h = s.hover(&uri, 51, 16);
    assert!(h.contains("Msg.Move(number)"), "dotted variant: {h}");
    let h = s.hover(&uri, 53, 10);
    assert!(h.contains("Msg.Move(number)"), "pattern variant: {h}");

    // A user attribute and a macro hover as what they are.
    let h = s.hover(&uri, 61, 2);
    assert!(
        h.contains("@icon(asset: string)") && h.contains("**Applies to** `struct`"),
        "attribute: {h}"
    );

    // Go to definition lands on the Alloy declaration: a struct used in
    // an annotation, a macro through its sigil, and a variant.
    let defs = s.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 67, "character": 14 } }),
    );
    assert_eq!(defs[0]["range"]["start"]["line"], 1, "{defs}");
    let defs = s.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 65, "character": 13 } }),
    );
    assert_eq!(defs[0]["range"]["start"]["line"], 58, "{defs}");
    let defs = s.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 51, "character": 16 } }),
    );
    assert_eq!(defs[0]["range"]["start"]["line"], 49, "{defs}");

    // The editor asks after Enter whether the line opened a block; a
    // closed macro wants nothing.
    let answer = s.request(
        "alloy/blockEnd",
        json!({ "textDocument": { "uri": uri }, "line": 58 }),
    );
    assert_eq!(answer, json!(null), "{answer}");
    let h = s.hover(&uri, 65, 12);
    assert!(h.contains("macro twice(x)"), "macro: {h}");

    // `@`, `$`, and a remote's `from` complete from the proxy alone.
    let labels = s.completion_labels(&uri, 61, 1);
    assert!(
        labels.iter().any(|l| l == "@icon") && labels.iter().any(|l| l == "@derive"),
        "{labels:?}"
    );
    let labels = s.completion_labels(&uri, 65, 12);
    assert!(
        labels.iter().any(|l| l == "$twice") && labels.iter().any(|l| l == "$dbg"),
        "{labels:?}"
    );
    let labels = s.completion_labels(&uri, 66, 34);
    assert_eq!(labels, ["client", "server"]);
    let labels = s.completion_labels(&uri, 66, 29);
    assert_eq!(labels, ["from"]);

    // A plain completion offers the Alloy keywords too.
    let labels = s.completion_labels(&uri, 13, 19);
    assert!(
        labels.iter().any(|l| l == "remote") && labels.iter().any(|l| l == "macro"),
        "{labels:?}"
    );

    // The hover keeps the annotation the source wrote.
    let h = s.hover(&uri, 67, 8);
    assert!(h.contains("local items: Vec2[]"), "annotation: {h}");

    // A language-level mapped type hovers with its doc, and a value of it
    // keeps the annotation. A file's own `type Sink` wins over the
    // built-in one, in the emit and in hover.
    let h = s.hover(&uri, 68, 14);
    assert!(
        h.contains("type Partial<T> = { [K in keyof T]: T[K]? }"),
        "Partial: {h}"
    );
    let h = s.hover(&uri, 68, 7);
    assert!(h.contains("local part: Partial<Vec2>"), "part: {h}");

    // A std name the file imports is the file's, not the std's.
    let h = s.hover(&uri, 81, 12);
    assert!(!h.contains("unique key"), "imported Symbol: {h}");
    // Std shapes fold to their names, a struct field key hovers as the
    // field, and an initializer's binding shows its fields.
    let h = s.hover(&uri, 72, 7);
    assert!(h.contains("local fut: Future<number>"), "future: {h}");
    let h = s.hover(&uri, 75, 15);
    assert!(h.contains("local rs: number[]"), "rest: {h}");
    let h = s.hover(&uri, 5, 21);
    assert!(
        h.contains("x: number") && h.contains("A field of `struct Vec2`"),
        "field: {h}"
    );
    let h = s.hover(&uri, 76, 7);
    assert!(
        h.contains("local box: Part") && h.contains("Initialized with"),
        "init: {h}"
    );

    // Inside an import string: the modules beside this file, and `@self`.
    let labels = s.completion_labels(&uri, 80, 24);
    assert!(
        labels.iter().any(|l| l == "ext") && labels.iter().any(|l| l == "main"),
        "{labels:?}"
    );
    let labels = s.completion_labels(&uri, 80, 22);
    assert!(labels.iter().any(|l| l == "@self/"), "{labels:?}");

    let h = s.hover(&uri, 70, 12);
    assert!(
        h.contains("type Sink<T> = { [K in keyof T]: write T[K] }") && !h.contains("std builds"),
        "own Sink: {h}"
    );
    let h = s.hover(&uri, 29, 16);
    assert!(h.contains("async function fetch_it"), "async: {h}");
    assert!(
        h.contains("Future<number>") && !h.contains("__alloy"),
        "async: {h}"
    );

    // An async function without a return type infers one, shown as the
    // Future in hover and as the inner type in the insertable hint.
    let h = s.hover(&uri, 37, 16);
    assert!(
        h.contains("async function stamp(): Future<number>"),
        "inferred: {h}"
    );
    let hints = s.request(
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": uri },
            "range": { "start": { "line": 37, "character": 0 }, "end": { "line": 40, "character": 0 } } }),
    );
    let labels: Vec<String> = hints
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|h| h["position"]["line"] == 37)
        .map(|h| match &h["label"] {
            Value::String(s) => s.clone(),

            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p["value"].as_str())
                .collect::<String>(),

            _ => String::new(),
        })
        .collect();
    assert!(labels.iter().any(|l| l == ": number"), "{hints}");
    let h = s.hover(&uri, 32, 14);
    assert!(h.contains("export const answer"), "export: {h}");
    let h = s.hover(&uri, 33, 22);
    assert!(h.contains("local async function later"), "local async: {h}");

    // Pulled diagnostics get the same filter as pushed ones: the hoisted
    // `??=` line carries no layout lint.
    let report = s.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    let items = report["items"].as_array().cloned().unwrap_or_default();
    assert!(!items.is_empty(), "{report}");
    assert!(
        items.iter().all(|d| {
            let m = d["message"].as_str().unwrap_or("");
            !m.starts_with("SameLineStatement") && !m.starts_with("MultiLineStatement")
        }),
        "{items:#?}"
    );

    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null }),
    );
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A lint with a rewrite is a quick fix, and the file gets a `source.fixAll`.
#[test]
fn code_actions_offer_the_lint_rewrites() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-actions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src =
        "local p = workspace\nlocal n = p and p.Name\nlocal q = math.floor(#n / 2)\nprint(n, q)\n";
    let file = dir.join("fix.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let actions = s.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } },
            "context": { "diagnostics": [] },
        }),
    );
    let list = actions.as_array().cloned().unwrap_or_default();
    let titles: Vec<String> = list
        .iter()
        .filter_map(|a| a["title"].as_str().map(str::to_string))
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("manual_safe_access")),
        "{titles:?}"
    );
    // The floor division sits on line 2, outside the range.
    assert!(
        !titles.iter().any(|t| t.contains("manual_floor_div")),
        "{titles:?}"
    );

    let fix = list
        .iter()
        .find(|a| {
            a["title"]
                .as_str()
                .is_some_and(|t| t.contains("manual_safe_access"))
        })
        .unwrap();
    assert_eq!(fix["kind"], "quickfix");
    let edit = &fix["edit"]["changes"][&uri][0];
    assert_eq!(edit["newText"], "p?");
    assert_eq!(edit["range"]["start"]["line"], 1);

    let all = list
        .iter()
        .find(|a| a["kind"] == "source.fixAll")
        .expect("a fix-all action");
    assert_eq!(all["edit"]["changes"][&uri].as_array().unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A multi-root workspace: the editor names several folders, and a file
/// in a folder other than the first still hovers.
#[test]
fn a_multi_root_workspace_answers_hover() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let base = std::env::temp_dir().join(format!("alloy-lsp-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let first = base.join("crates");
    let other = base.join("examples");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let src = "local xs = [ 1, 2 ]\nprint(xs)\n";
    let file = other.join("list.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start_with(
        &child,
        json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{}", first.display()),
            "capabilities": {},
            "workspaceFolders": [
                { "uri": format!("file://{}", first.display()), "name": "crates" },
                { "uri": format!("file://{}", other.display()), "name": "examples" },
            ],
        }),
    );
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let h = s.hover(&uri, 1, 6);
    assert!(h.contains("Array") || h.contains("xs"), "{h}");

    let _ = std::fs::remove_dir_all(&base);
}

/// A file under its own alloy.toml, outside the root, whose root config
/// names an input with `..` in it: the runtime require still resolves.
#[test]
fn a_file_outside_the_root_finds_the_runtime() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let base = std::env::temp_dir().join(format!("alloy-lsp-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("crates");
    let game = base.join("examples").join("game");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(game.join("src/client")).unwrap();
    std::fs::write(
        root.join("alloy.toml"),
        "[build]\nin = \"../examples\"\nout = \"build\"\n",
    )
    .unwrap();
    std::fs::write(
        game.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    let src = "local xs = [ 1, 2 ]\nlocal n: number = \"s\"\nprint(xs, n)\n";
    let file = game.join("src/client/test.aly");
    std::fs::write(&file, src).unwrap();
    // A neighbour under the root's input, there before the server starts.
    std::fs::write(
        base.join("examples/util.aly"),
        "export function one(): number\n    return 1\nend\n",
    )
    .unwrap();

    let mut s = start(&child, &root);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The type error proves the checker ran on the file; the runtime
    // require it starts with resolved, or there would be a second report.
    let diags = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("number")));
    assert!(
        !diags
            .iter()
            .any(|d| d.contains("Unknown require") || d.contains("unknown module")),
        "{diags:?}"
    );

    // A file under the root's input imports its neighbour: the startup
    // pass shadowed it, so the import resolves.
    let main_src = "import { one } from \"./util\"\nimport { gone } from \"./nope\"\nimport { x } from \"@pkg/thing\"\nlocal n: number = \"s\"\nprint(one(), gone, x, n)\n";
    let main = base.join("examples/main.aly");
    std::fs::write(&main, main_src).unwrap();
    let main_uri = format!("file://{}", main.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": main_uri, "languageId": "alloy-luau", "version": 1, "text": main_src } } }),
    );
    let diags = s.diagnostics(&main_uri, |ds| ds.iter().any(|d| d.contains("number")));
    assert!(!diags.iter().any(|d| d.contains("util")), "{diags:?}");
    assert!(
        diags.iter().any(|d| d.contains("unknown module \"./nope\"")
            && d.contains("no .aly, .alx, or .luau file at")),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("unknown module \"@pkg/thing\"") && d.contains("no alias `@pkg`")),
        "{diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.contains("Unknown require")),
        "{diags:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// An enum's variants complete as enum members with their signatures,
/// and a payload position offers types, not values.
#[test]
fn enum_variants_complete_as_members_and_payloads_take_types() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-enum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "enum Msg as\n    Move(number)\n    Quit\nend\nlocal m = Msg.\nprint(m)\n";
    let file = dir.join("en.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // After `Msg.`: both variants are enum members with a signature.
    let r = s.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 4, "character": 14 } }),
    );
    let items = r
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| r.as_array())
        .cloned()
        .unwrap_or_default();
    let find = |label: &str| items.iter().find(|i| i["label"] == label).cloned();
    let mv = find("Move").expect("Move completes");
    assert_eq!(mv["kind"], 20, "{mv}");
    assert!(
        mv["detail"]
            .as_str()
            .is_some_and(|d| d.contains("Msg.Move(number)")),
        "{mv}"
    );
    let quit = find("Quit").expect("Quit completes");
    assert_eq!(quit["kind"], 20, "{quit}");

    // Inside `Move(`: types, and no `assert`.
    let labels = s.completion_labels(&uri, 1, 9);
    assert!(labels.iter().any(|l| l == "number"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Players"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Msg"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "assert"), "{labels:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
