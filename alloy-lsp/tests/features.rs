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

    fn completion_items(&mut self, uri: &str, line: u32, character: u32) -> Vec<Value> {
        let r = self.request(
            "textDocument/completion",
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        );

        r.get("items")
            .and_then(Value::as_array)
            .or_else(|| r.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn completion_labels(&mut self, uri: &str, line: u32, character: u32) -> Vec<String> {
        self.completion_items(uri, line, character)
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    }

    /// Reads every message that arrives inside `window`, so a later
    /// assertion can look at the whole batch.
    fn drain(&mut self, window: Duration) {
        let deadline = Instant::now() + window;

        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left) {
                Ok(m) => self.seen.push(m),

                Err(_) => return,
            }
        }
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
    start_env(child, init_params, &[])
}

/// The same, with environment variables set on the server process.
fn start_env(child: &Path, init_params: Value, env: &[(&str, &str)]) -> Session {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alloy-lsp"));

    for (name, value) in env {
        command.env(name, value);
    }

    let mut server = KillOnDrop(
        command
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
    // The proxy writes the `end` of a block on Enter, whatever the
    // child registers.
    assert_eq!(
        init["capabilities"]["documentOnTypeFormattingProvider"]["firstTriggerCharacter"],
        json!("\n"),
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
    // The file being edited is not among them: a module never imports
    // itself.
    let labels = s.completion_labels(&uri, 80, 24);
    assert!(
        labels.iter().any(|l| l == "ext") && !labels.iter().any(|l| l == "main"),
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
    // An async function's return hint names the Future the caller gets.
    assert!(labels.iter().any(|l| l == ": Future<number>"), "{hints}");
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
/// The names a binding introduces: `if local`, a `case` pattern, and a
/// method call's result. Each is a generated local in the shadow, so the
/// hover has to reach the source position the binding was written at.
const BINDINGS: &str = "\
struct Node as
    name: string
    child: Node?
end

enum Event as
    Key(string)
    Quit
end

local root = new Node { name = \"root\", child = nil }
if local c = root.child then
    print(c.name)
end

local function handle(e: Event): string
    match e with
        case Key(k) then
            return k
        case Quit then
            return \"q\"
    end
end

local function parse(): Result<number, string>
    return Ok(1)
end
local r = parse()
local o = r:ok()
print(root, handle, o)
";

#[test]
fn a_binding_hovers_where_it_was_written() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-bindings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bindings.aly");
    std::fs::write(&file, BINDINGS).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": BINDINGS } } }),
    );

    // `if local c = root.child then`: the name is `c`, the type `Node`.
    let h = s.hover(&uri, 11, 9);
    assert!(h.contains("c") && h.contains("Node"), "if local: {h}");
    // `case Key(k) then`: the payload binds `k` as a string.
    let h = s.hover(&uri, 17, 17);
    assert!(h.contains("k") && h.contains("string"), "case bind: {h}");
    // `local o = r:ok()`: the method's result.
    let h = s.hover(&uri, 28, 6);
    assert!(h.contains("number"), "method result: {h}");

    let _ = std::fs::remove_dir_all(&dir);
}

const LOWERED_BLOCKS: &str = "local function parse(s: string): Result<number, string>\n    return Ok(1)\nend\n\nlocal c = try do\n    local v = try parse(\"1\")\n    return v + 1\nend\n\nlocal j = async do\n    return 1\nend\n\nprint(c, j)\n";

/// `async do` and `try do` lower to a closure the source never wrote.
/// The child answers about that closure on the block's own furniture:
/// the `do`, the `end`, and the blank columns between. That answer
/// names the emit's parameters, so `try do` read
/// `function(__fail: (string, string?) -> (...unknown)): number`. The
/// furniture says nothing now, and every real name still answers.
#[test]
fn a_lowered_block_never_hovers_its_closure() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-blocks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("blocks.aly");
    std::fs::write(&file, LOWERED_BLOCKS).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": LOWERED_BLOCKS } } }),
    );

    // The first request warms the child's index; a cold one answers
    // nothing whatever the code says.
    let _ = s.hover(&uri, 4, 6);

    // The block's furniture: the `do` of `try do`, the blank column
    // inside it, its `end`, and the same inside `async do`.
    for (line, character, what) in [
        (4, 13, "try do's do"),
        (5, 0, "inside try do"),
        (7, 0, "try do's end"),
        (10, 0, "inside async do"),
    ] {
        let h = s.hover(&uri, line, character);
        assert!(
            !h.contains("__fail") && !h.contains("...unknown"),
            "{what} leaks the emit: {h}"
        );
    }

    // The bindings still carry their own types.
    let h = s.hover(&uri, 4, 6);
    assert!(h.contains("Result<number, string>"), "try do binding: {h}");
    let h = s.hover(&uri, 9, 6);
    assert!(h.contains("Future<number>"), "async do binding: {h}");
    // A name written inside the block answers for itself.
    let h = s.hover(&uri, 5, 10);
    assert!(h.contains("number"), "name inside the block: {h}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Format on save reads the project's `[fmt]` table, the way
/// `alloy fmt` does. The proxy formatted with the built-in defaults, so
/// a project that sets `indent_width = 2` still got four spaces in the
/// editor and two in the terminal.
#[test]
fn formatting_reads_the_project_layout() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-fmt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[project]\nname = \"layout\"\n\n[fmt]\nindent_type = \"spaces\"\nindent_width = 2\n",
    )
    .unwrap();

    // Eight spaces in, two levels deep.
    let source = "local function f()\n        local x = 1\n        return x\nend\n\nreturn f\n";
    let file = dir.join("src").join("layout.aly");
    std::fs::write(&file, source).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": source } } }),
    );

    let r = s.request(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 4, "insertSpaces": true } }),
    );
    let text = r[0]["newText"].as_str().unwrap_or_default().to_string();

    assert!(
        text.contains("\n  local x = 1"),
        "the project asked for two spaces: {text:?}"
    );
    assert!(
        !text.contains("\n    local x = 1"),
        "four spaces is the default, not this project's: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

const SERVICES: &str = "import Players from \"game:Players\"\nimport { ReplicatedStorage, RunService as Run } from \"game\"\n\nlocal remotes = ReplicatedStorage:WaitForChild(\"Remotes\")\n\nPlayers.PlayerAdded:Connect(function(player)\n    print(player.Name, remotes, Run.Heartbeat)\nend)\n";

/// A service import binds `game:GetService`, so the hover names the
/// service the reader wrote, on the binding and on the path.
#[test]
fn a_service_import_hovers_as_its_service() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-services-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("services.aly");
    std::fs::write(&file, SERVICES).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": SERVICES } } }),
    );

    // `Players` where the file uses it, on line 6.
    let h = s.hover(&uri, 5, 2);
    // The binding names its type, the way every other binding hovers.
    assert!(h.contains("local Players: Players"), "the binding: {h}");
    assert!(h.contains("a Roblox service"), "the binding: {h}");

    // Inside the path of the second line, on `game`.
    let h = s.hover(&uri, 1, 55);
    assert!(h.contains("ReplicatedStorage"), "the path: {h}");
    assert!(h.contains("RunService"), "the path: {h}");

    // The alias the second line binds.
    let h = s.hover(&uri, 6, 33);
    assert!(h.contains("RunService"), "the alias: {h}");

    let _ = std::fs::remove_dir_all(&dir);
}

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

/// Two projects under one parent: the server opened on one of them
/// never reads the other's files. The parent carries an alloy.toml, so
/// a walk that passes the root would take the parent's input and reach
/// both projects.
#[test]
fn a_sibling_project_stays_out_of_the_workspace() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let base = std::env::temp_dir().join(format!("alloy-lsp-siblings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mine = base.join("mine");
    let other = base.join("other");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    // The parent names itself as the input, the shape that used to pull
    // both projects in.
    std::fs::write(
        base.join("alloy.toml"),
        "[build]\nin = \".\"\nout = \"build\"\n",
    )
    .unwrap();
    let src = "local n: number = 1\nprint(n)\n";
    let file = mine.join("main.aly");
    std::fs::write(&file, src).unwrap();
    // The sibling holds an error, so a server that reads it says so.
    std::fs::write(other.join("sibling.aly"), "local broken = 1 +\n").unwrap();

    let mut s = start(&child, &mine);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // A hover round trip proves the child is up and the workspace has
    // loaded; the sibling's shadow would be open by then.
    let h = s.hover(&uri, 0, 6);
    assert!(!h.is_empty(), "{h}");
    s.drain(Duration::from_secs(3));

    let leaked: Vec<&Value> = s
        .seen
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .filter(|m| {
            m["params"]["uri"]
                .as_str()
                .is_some_and(|u| u.contains("/other/"))
        })
        .collect();
    assert!(leaked.is_empty(), "{leaked:#?}");

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
            .any(|d| d.contains("Unknown require") || d.contains("UnknownModule")),
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
        diags.iter().any(
            |d| d.starts_with("UnknownModule: \"./nope\" names no module")
                && d.contains("no .aly, .alx, or .luau file at")
        ),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.starts_with("UnknownModule: \"@pkg/thing\"") && d.contains("no alias pkg")),
        "{diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.contains("Unknown require")),
        "{diags:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A project with a mount table and no sourcemap: the shadow finds the
/// runtime, so no import draws a report, and the import path carries one
/// document link to the real file.
#[test]
fn an_import_resolves_without_a_sourcemap_and_carries_one_link() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-imports-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/client")).unwrap();
    std::fs::create_dir_all(dir.join("packages")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"game\"\n\n[mount]\nclient = [\"src/client\", \"@game/ReplicatedStorage/Client\"]\npkg = [\"packages\", \"@game/ReplicatedStorage/Packages\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("packages/fluid.luau"),
        "local m = {}\nfunction m.create(x: string): string return x end\nreturn m\n",
    )
    .unwrap();
    // The file uses the runtime, so the shadow requires it, and it sits
    // under a mount, so the ship artifact would name an instance path.
    let src =
        "import fluid from \"@pkg/fluid\"\nlocal xs = [ 1, 2 ]\nprint(fluid.create(\"a\"), xs)\n";
    let file = dir.join("src/client/main.client.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );
    s.drain(Duration::from_secs(6));
    let published: Vec<String> = s
        .seen
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .flat_map(|m| {
            m["params"]["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|d| d["message"].as_str().map(str::to_string))
        .collect();
    assert!(
        !published
            .iter()
            .any(|d| d.contains("names no module") || d.contains("Unknown require")),
        "{published:?}"
    );

    // One link, on the quoted path, to the file on disk: the runtime the
    // shadow requires draws none, and the mirror copy is not the target.
    let links = s.request(
        "textDocument/documentLink",
        json!({ "textDocument": { "uri": uri } }),
    );
    let links = links.as_array().cloned().unwrap_or_default();
    assert_eq!(links.len(), 1, "{links:#?}");
    assert_eq!(links[0]["range"]["start"]["line"], json!(0));
    assert_eq!(
        links[0]["target"].as_str().unwrap_or_default(),
        format!("file://{}", dir.join("packages/fluid.luau").display())
    );

    // The path hovers as the import line alone; the binding hovers as
    // the module's table.
    assert_eq!(
        s.hover(&uri, 0, 22),
        "```alloy\nimport fluid from \"@pkg/fluid\"\n```"
    );
    assert!(s.hover(&uri, 0, 8).contains("create"));

    // A path that names no file reports on the import, whichever form
    // it takes; a `.server` file is a script, not a module.
    std::fs::write(dir.join("src/client/boot.server.aly"), "print(1)\n").unwrap();
    let bad = "import a from \"./gone\"\nimport b from \"../up\"\nimport c from \"@pkg/nope\"\nimport d from \"./boot.server\"\nprint(a, b, c, d)\n";
    let bad_file = dir.join("src/client/bad.aly");
    std::fs::write(&bad_file, bad).unwrap();
    let bad_uri = format!("file://{}", bad_file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": bad_uri, "languageId": "alloy-luau", "version": 1, "text": bad } } }),
    );
    let diags = s.diagnostics(&bad_uri, |ds| {
        ds.iter().filter(|d| d.contains("UnknownModule")).count() >= 4
    });
    assert!(
        diags
            .iter()
            .any(|d| d.contains("\"./gone\" names no module") && d.contains("src/client/gone")),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("\"../up\" names no module") && d.contains("src/up")),
        "{diags:?}"
    );
    // `pkg` is a mount, so the message names the folder it stands for.
    assert!(
        diags
            .iter()
            .any(|d| d.contains("\"@pkg/nope\" names no module") && d.contains("packages/nope")),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.contains("\"./boot.server\" is a script, not a module")),
        "{diags:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A package install writes files after the server started. The poll
/// reads them, the child hears about them, and an import of the new
/// module types without a restart.
#[test]
fn a_module_added_on_disk_types_without_a_restart() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-added-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("packages")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"game\"\n\n[mount]\nsrc = [\"src\", \"@game/ReplicatedStorage/Src\"]\npkg = [\"packages\", \"@game/ReplicatedStorage/Packages\"]\n",
    )
    .unwrap();
    let src = "import vide from \"@pkg/vide\"\nlocal n: number = vide.count(\"a\")\nprint(n)\n";
    let file = dir.join("src/main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start_env(
        &child,
        json!({ "processId": std::process::id(), "rootUri": format!("file://{}", dir.display()), "capabilities": {} }),
        &[("ALLOY_LSP_POLL_SECS", "1")],
    );
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The module is not there yet, so the import draws a report.
    s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("@pkg/vide")));

    // A package install writes the module after the server started.
    std::fs::write(
        dir.join("packages/vide.luau"),
        "local m = {}\nfunction m.count(s: string): string return s end\nreturn m\n",
    )
    .unwrap();

    // The poll reads it: the import resolves, and its type is the
    // module's, so `count` returning a string draws the mismatch.
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut hover = String::new();

    while Instant::now() < deadline {
        s.drain(Duration::from_secs(2));
        hover = s.hover(&uri, 0, 8);

        if hover.contains("count") {
            break;
        }
    }

    assert!(hover.contains("count"), "{hover}");

    // The document was published again against the new module, so the
    // report on the import is gone.
    s.drain(Duration::from_secs(2));
    let last: Vec<String> = s
        .seen
        .iter()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .and_then(|m| m["params"]["diagnostics"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|d| d["message"].as_str().map(str::to_string))
        .collect();
    assert!(!last.iter().any(|d| d.contains("@pkg/vide")), "{last:?}");

    let _ = std::fs::remove_dir_all(&dir);
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
    let src = "enum Msg as\n    Move(number)\n    Quit\nend\nlocal m = Msg.\nprint(m)\nlocal v = Msg.Move(1)\n";
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
    let inserted = mv["textEdit"]["newText"]
        .as_str()
        .or_else(|| mv["insertText"].as_str())
        .unwrap_or("");
    assert_eq!(inserted, "Move(${1:number})", "{mv}");

    // Signature help inside `Msg.Move(`: the variant's shape, not `_1`.
    let help = s.request(
        "textDocument/signatureHelp",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 6, "character": 19 } }),
    );
    let sig = &help["signatures"][0];
    assert_eq!(sig["label"], "Msg.Move(number)", "{help}");
    assert_eq!(sig["parameters"][0]["label"], "number", "{help}");

    // Inside `Move(`: types, and no `assert`.
    let labels = s.completion_labels(&uri, 1, 9);
    assert!(labels.iter().any(|l| l == "number"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Players"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Msg"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "assert"), "{labels:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The names of an import complete from a plain Luau module too, through
/// an `@alias` of `.luaurc`, following a `return M` re-export.
#[test]
fn import_names_come_from_plain_luau_modules() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("packages/inner")).unwrap();
    std::fs::write(
        dir.join(".luaurc"),
        "{ \"languageMode\": \"strict\", \"aliases\": { \"pkg\": \"packages\" } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("packages/inner/jecs.luau"),
        "local jecs = {}\nfunction jecs.world()\n    return {}\nend\njecs.pair = 1\nexport type Entity = number\nreturn jecs\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("packages/jecs.luau"),
        "local module = require(\"./inner/jecs\")\nexport type Entity = module.Entity\nreturn module\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib.luau"),
        "return {\n    a = 1,\n    b = function() end,\n}\n",
    )
    .unwrap();
    let src = "import {  } from '@pkg/jecs'\nimport {  } from \"./lib\"\n";
    let file = dir.join("main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let labels = s.completion_labels(&uri, 0, 9);
    assert!(labels.iter().any(|l| l == "world"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "pair"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "type Entity"), "{labels:?}");

    let labels = s.completion_labels(&uri, 1, 9);
    assert!(
        labels.iter().any(|l| l == "a") && labels.iter().any(|l| l == "b"),
        "{labels:?}"
    );

    // After the closing quote: nothing.
    let labels = s.completion_labels(&uri, 0, 28);
    assert!(labels.is_empty(), "{labels:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Probe: an attribute imported from another file hovers as the
/// attribute, on the import and at a use.
#[test]
fn an_imported_attribute_hovers_as_an_attribute() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-attr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("attrs.aly"),
        "export attribute icon(asset: string) on struct\n",
    )
    .unwrap();
    let src = "import { icon } from \"./attrs\"\n\n@icon(\"rbxassetid://1\")\nstruct V as\n    x: number\nend\nprint(icon)\n";
    let file = dir.join("main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let on_import = s.hover(&uri, 0, 10);
    assert!(
        on_import.contains("@icon(asset: string)"),
        "import: {on_import}"
    );
    let at_use = s.hover(&uri, 2, 2);
    assert!(at_use.contains("@icon(asset: string)"), "use: {at_use}");
    let bare = s.hover(&uri, 6, 7);
    assert!(bare.contains("@icon(asset: string)"), "bare: {bare}");

    let _ = std::fs::remove_dir_all(&dir);
}

/*
An attribute contract reaches the using file: two `export attribute`
declarations one import brings in.

The hover lists what the attribute requires with the argument expanded,
the check reports the member the file does not carry, and the quick fix
writes it in.
*/
#[test]
fn an_attribute_contract_reaches_the_file_that_uses_it() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lifecycle.aly"),
        concat!(
            "--- The stages a provider hooks into.\n",
            "export enum Lifecycle as\n    Init\n    Start\nend\n\n",
            "--- A provider that runs on the stages it names.\n",
            "export attribute provider(lifecycles: Lifecycle[]) on impl as\n",
            "    requires private function each lifecycles(self)\nend\n\n",
            "--- A service the framework starts.\n",
            "export attribute service on impl as\n",
            "    requires public function Start(self)\nend\n"
        ),
    )
    .unwrap();
    let src = concat!(
        "import { Lifecycle, provider, @service } from \"./lifecycle\"\n\n",
        "struct Data as\n    x: number\nend\n\n",
        "@provider({ lifecycles = [ Lifecycle.Init, Lifecycle.Start ] })\n",
        "@service\n",
        "impl Data as\n",
        "    private function Init(self)\n    end\n",
        "    \n",
        "end\n\n",
        "print(Data)\n"
    );
    let file = dir.join("data.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The imported attribute hovers with its clauses, and `each` reads
    // the arguments of this use.
    let imported = s.hover(&uri, 6, 3);
    assert!(imported.contains("**Requires**"), "imported: {imported}");
    assert!(
        imported.contains("- `private function Init(self)`")
            && imported.contains("- `private function Start(self)`"),
        "imported: {imported}"
    );
    assert!(
        !imported.contains("each lifecycles"),
        "the use expands the clause: {imported}"
    );

    // The second attribute of the import hovers the same way.
    let service = s.hover(&uri, 7, 3);
    assert!(
        service.contains("- `public function Start(self)`"),
        "service: {service}"
    );

    // Both contracts report in this file.
    let diags = s.diagnostics(&uri, |ds| {
        ds.iter().any(|d| d.contains("AttributeContract"))
    });
    assert!(
        diags
            .iter()
            .any(|d| d
                == "AttributeContract: `@provider` requires a private function `Start(self)`; `Data` declares none"),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d
                == "AttributeContract: `@service` requires a public function `Start(self)`; `Data` declares none"),
        "{diags:?}"
    );

    // The completion inside the block offers what the contracts ask for.
    let labels = s.completion_labels(&uri, 11, 4);
    assert!(labels.contains(&"Start".to_string()), "{labels:?}");

    // The quick fix on the attribute writes the member in.
    let actions = s.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 7, "character": 0 }, "end": { "line": 7, "character": 8 } },
            "context": { "diagnostics": [] },
        }),
    );
    let list = actions.as_array().cloned().unwrap_or_default();
    let fix = list
        .iter()
        .find(|a| {
            a["title"]
                .as_str()
                .is_some_and(|t| t.starts_with("Write the member `@service` requires"))
        })
        .unwrap_or_else(|| panic!("no contract fix: {list:#?}"));
    let edit = &fix["edit"]["changes"][&uri][0];
    assert_eq!(
        edit["newText"],
        "    public function Start(self)\n    end\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A line the compiler reports on gets no report from the checker: the
/// reserved word alone, not `Unknown global` beside it.
#[test]
fn a_compiler_error_line_silences_the_checker() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-kinds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "local test = namespace\nlocal n: number = \"s\"\nprint(test, n)\n";
    let file = dir.join("k.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let diags = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("number")));
    assert!(
        diags.iter().any(|d| d.starts_with("ReservedWord: ")),
        "{diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.contains("Unknown global")),
        "{diags:?}"
    );
    assert!(
        !diags.iter().any(|d| d.contains("SyntaxError: Expected")),
        "{diags:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A JSON or TOML file imports as a table: hover shows the table type,
/// the keys complete after the name and inside the import braces, the
/// path completes with its extension, definition opens the file, and a
/// saved change regenerates the module.
#[test]
fn data_files_import_as_typed_tables() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/data.json"),
        "{ \"name\": \"game\", \"players\": 12, \"tags\": [\"a\"] }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/config.toml"),
        "title = \"cfg\"\ncoins = 100\n\n[limits]\nmax = 10\n",
    )
    .unwrap();
    let src = "import data from \"./data.json\"\nimport { coins } from \"./config.toml\"\nprint(data.name, coins)\nimport {  } from \"./config.toml\"\nimport more from \"./\"\n";
    let file = dir.join("src/main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The checker ran, and both data imports resolved.
    let diags = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("\"./\"")));
    assert!(
        !diags
            .iter()
            .any(|d| d.contains(".json") || d.contains(".toml")),
        "{diags:?}"
    );

    let hover = s.hover(&uri, 0, 8);
    assert!(
        hover.contains("name") && hover.contains("string") && hover.contains("players"),
        "{hover}"
    );

    let labels = s.completion_labels(&uri, 2, 11);
    assert!(
        ["name", "players", "tags"]
            .iter()
            .all(|k| labels.iter().any(|l| l == k)),
        "{labels:?}"
    );

    let items = s.completion_items(&uri, 3, 9);
    let detail = |key: &str| {
        items
            .iter()
            .find(|i| i["label"] == key)
            .map(|i| i["detail"].as_str().unwrap_or("").to_string())
    };
    assert_eq!(detail("coins").as_deref(), Some("number"));
    assert_eq!(detail("title").as_deref(), Some("string"));
    assert_eq!(detail("limits").as_deref(), Some("{ ... }"));
    assert!(!items.iter().any(|i| i["label"] == "type"), "{items:?}");

    let labels = s.completion_labels(&uri, 4, 20);
    assert!(
        labels.iter().any(|l| l == "data.json") && labels.iter().any(|l| l == "config.toml"),
        "{labels:?}"
    );

    let definition = |s: &mut Session, line: u32, character: u32| {
        s.request(
            "textDocument/definition",
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        )
    };
    let on_path = definition(&mut s, 0, 22);
    assert!(
        on_path[0]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("/src/data.json")),
        "{on_path}"
    );
    let on_name = definition(&mut s, 1, 10);
    assert!(
        on_name[0]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("/src/config.toml")),
        "{on_name}"
    );
    assert_eq!(on_name[0]["range"]["start"]["line"], json!(1));

    // A saved change to the data file reaches the module in the mirror.
    std::fs::write(
        dir.join("src/data.json"),
        "{ \"name\": \"game\", \"players\": 12, \"tags\": [\"a\"], \"level\": 3 }\n",
    )
    .unwrap();
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles", "params": {
            "changes": [{ "uri": format!("file://{}", dir.join("src/data.json").display()), "type": 2 }] } }),
    );
    let mut labels = Vec::new();

    for _ in 0..20 {
        labels = s.completion_labels(&uri, 2, 11);

        if labels.iter().any(|l| l == "level") {
            break;
        }

        std::thread::sleep(Duration::from_millis(200));
    }

    assert!(labels.iter().any(|l| l == "level"), "{labels:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `shout` example ingot of the alloy-ingot crate, built on demand.
fn shout_ingot_dir() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let bin = workspace.join("target/debug/examples/shout");

    if !bin.is_file() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "alloy-ingot", "--example", "shout"])
            .current_dir(&workspace)
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the shout example builds");
    }

    workspace
        .join("alloy-ingot/examples/shout")
        .canonicalize()
        .unwrap()
}

#[test]
fn an_ingot_answers_hover_completion_actions_and_lints() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-ingot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        format!(
            "[build]\nin = \"src\"\n\n[ingots]\nshout = \"{}\"\n",
            shout_ingot_dir().display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    let src = "local a = $shout(\"hi\")\n-- HELLO THERE\nprint(a)\nlocal z = $\n";
    let main = dir.join("src/main.aly");
    std::fs::write(&main, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", main.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The ingot's lint arrives with the compiler's diagnostics, and the
    // transform typed: `a` is a string, so nothing reports `$shout`.
    let diags = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("capitals")));
    assert!(
        diags
            .iter()
            .all(|d| !d.contains("shout") || d.contains("capitals")),
        "{diags:#?}"
    );

    let h = s.hover(&uri, 0, 12);
    assert!(h.contains("string.upper"), "ingot hover: {h}");

    let labels = s.completion_labels(&uri, 3, 11);
    assert!(labels.iter().any(|l| l == "$shout"), "{labels:?}");

    let r = s.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 2, "character": 6 }, "end": { "line": 2, "character": 7 } },
            "context": { "diagnostics": [] },
        }),
    );
    let titles: Vec<String> = r
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a["title"].as_str().map(str::to_string))
        .collect();
    assert!(titles.iter().any(|t| t == "Shout `a`"), "{titles:?}");
    let shout = r
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["title"] == "Shout `a`")
        .unwrap();
    assert_eq!(shout["edit"]["changes"][&uri][0]["newText"], "$shout(a)");

    // The lint's rewrite is a quick fix too.
    let r = s.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } },
            "context": { "diagnostics": [] },
        }),
    );
    let titles: Vec<String> = r
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a["title"].as_str().map(str::to_string))
        .collect();
    assert!(
        titles
            .iter()
            .any(|t| t.contains("loud_comment") || t.contains("capitals")),
        "{titles:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A pull report carries the same set as a push notification: the
/// directive errors, the compile errors and the lints, not the child's
/// reports alone.
#[test]
fn alloy_reports_travel_by_push_alone() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-pull-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    let src = "--@alloy-lint no_such_lint=allow\nlocal unread = 1\nstruct Kit as\n    ammo: number\nend\nlocal k = new Kit { }\nprint(k)\n";
    let file = dir.join("src/main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );
    let pushed = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("no_such_lint")));

    let report = s.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    let pulled: Vec<String> = report["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|d| d["message"].as_str().map(str::to_string))
        .collect();

    // Alloy's reports travel by push alone: a push overwrites the set
    // an earlier server left on the file, and a pull that repeated them
    // would show each twice in an editor that does both.
    for want in ["no_such_lint", "unused_variable", "leaves `ammo` unset"] {
        assert!(
            pushed.iter().any(|d| d.contains(want)),
            "push is missing {want}: {pushed:#?}"
        );
        assert!(
            !pulled.iter().any(|d| d.contains(want)),
            "pull repeats {want}: {pulled:#?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The editor and the terminal say one sentence for a `.` where a `:`
/// belongs, and say it once per line.
#[test]
fn a_dot_called_method_reads_the_same_as_in_the_terminal() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-dotcall-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n",
    )
    .unwrap();
    let src = "struct Wallet as\n    balance: number\nend\n\nimpl Wallet as\n    function add(self, amount: number): number\n        self.balance += amount\n        return self.balance\n    end\nend\n\nlocal w = new Wallet { balance = 0 }\nw.add(5)\nprint(w)\n";
    let file = dir.join("src/main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );
    let diags = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("is a method")));
    assert!(
        diags
            .iter()
            .any(|d| d.contains("`add` is a method; call it with `w:add(...)`, not `w.add(...)`")),
        "{diags:#?}"
    );
    // The shifted arguments draw their own reports; the one sentence
    // that names the mistake stands alone on its line.
    assert_eq!(
        diags.iter().filter(|d| d.contains("Wallet")).count(),
        0,
        "{diags:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The completion items a newline trigger answers with.
fn newline_items(s: &mut Session, uri: &str, line: u32, character: u32) -> Vec<Value> {
    let r = s.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character },
                "context": { "triggerKind": 2, "triggerCharacter": "\n" } }),
    );

    r.get("items")
        .and_then(Value::as_array)
        .or_else(|| r.as_array())
        .cloned()
        .unwrap_or_default()
}

/// `.alx`: the `>` that ends an opening tag names the element to close.
const MARKUP: &str = "\
local function Panel(props: { label: string })
    return <Frame>
        <Badge label={props.label} />
    </Frame>
end
return Panel
";

/// A `.alx` file with a guarded index and a chain, on plain lines and
/// inside a `{ }` hole of a tag. The lowering moves the columns of a
/// markup line, so the map has to cross it byte for byte.
const GUARDED_MARKUP: &str = "\
local React = (nil :: any) :: { createElement: (...any) -> any }

struct Part as
    Name: string,
    Size: number,
end

local function size_of(part: Part): number
    return part.Size
end

local function Panel(props: { parts: { Part }? })
    local parts = props.parts
    local one = parts?[1]
    local name = parts?[1]?.Name
    local a = parts?[1]?.
    local b = parts![1].
    local c = <Frame Size={parts?[1]?.} />
    return <Frame Size={size_of(parts![1])}>
        <TextLabel Text={parts?[1]?.Name} />
    </Frame>
end

return Panel
";

/// The markup lowering used to move every column of the line it wrote,
/// so a member through a guarded index answered from somewhere else.
/// Each answer below reads the same as it does in a `.aly`.
#[test]
fn a_guarded_index_answers_through_the_markup_lowering() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-guarded-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("panel.alx");
    std::fs::write(&file, GUARDED_MARKUP).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau-jsx", "version": 1,
                "text": GUARDED_MARKUP } } }),
    );
    s.drain(Duration::from_secs(5));
    // The first answer warms the child; it has the file only now.
    let _ = s.hover(&uri, 0, 6);

    let holds = |labels: &[String]| {
        labels.iter().any(|l| l == "Name") && labels.iter().any(|l| l == "Size")
    };

    // `parts?[1]?.` and `parts![1].` with nothing after them: the
    // repair pass carries the line, and the member list is the
    // element's.
    let labels = s.completion_labels(&uri, 15, 25);
    assert!(holds(&labels), "optional index: {labels:?}");
    let labels = s.completion_labels(&uri, 16, 24);
    assert!(holds(&labels), "asserted index: {labels:?}");

    // The same, inside the `{ }` of a tag, where the caret has the
    // hole's `}` right after it rather than the end of the line. That
    // is where the author stands while typing the attribute.
    let labels = s.completion_labels(&uri, 17, 38);
    assert!(holds(&labels), "a hole the closer follows: {labels:?}");

    // The same chain written out, on a plain line and in a hole.
    let h = s.hover(&uri, 14, 29);
    assert!(h.contains("string"), "chain on a plain line: {h}");
    let h = s.hover(&uri, 19, 37);
    assert!(h.contains("string"), "chain in a hole: {h}");

    // Every column of a markup line maps, not only the ones inside a
    // word: the two guards of the hole read as the operators they are,
    // and the receiver reads its own type.
    let h = s.hover(&uri, 19, 30);
    assert!(h.contains("a?[k]"), "the guard of the index: {h}");
    let h = s.hover(&uri, 19, 34);
    assert!(h.contains("a?.b"), "the guard of the chain: {h}");
    let h = s.hover(&uri, 19, 26);
    assert!(h.contains("{Part}?"), "the receiver in a hole: {h}");

    // A binding of a guarded index gets its element type as a hint.
    let hints = s.request(
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": uri },
            "range": { "start": { "line": 0, "character": 0 },
                       "end": { "line": 24, "character": 0 } } }),
    );
    let one: Vec<String> = hints
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|h| h["position"]["line"] == 13)
        .map(hint_text)
        .collect();
    assert!(one.iter().any(|l| l == ": Part?"), "{hints}");

    // A name inside a hole goes to where the file declares it.
    let defs = s.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 18, "character": 26 } }),
    );
    assert_eq!(defs[0]["range"]["start"]["line"], 7, "{defs}");
    assert_eq!(defs[0]["range"]["start"]["character"], 15, "{defs}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.alx` that never brings the factory into scope, so the markup
/// cannot lower. Everything but the tags is ordinary Alloy.
const MARKUP_WITHOUT_A_FACTORY: &str = "\
struct Part as
    Name: string,
end

local function App()
    local parts: { Part }? = nil
    local first = parts?[1]
    print(first)
    return <Frame Size={12}>
        <TextLabel Text=\"hi\" />
    </Frame>
end

return App
";

/// The markup used to leave the child the author's tags, which it reads
/// as nothing: one tag it could not lower silenced the whole file. The
/// regions blank to the width they had, so the code around them
/// answers and the one markup error still reports on the tag.
#[test]
fn a_markup_file_that_cannot_lower_still_answers_around_its_tags() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-nofactory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("panel.alx");
    std::fs::write(&file, MARKUP_WITHOUT_A_FACTORY).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau-jsx", "version": 1,
                "text": MARKUP_WITHOUT_A_FACTORY } } }),
    );

    // The tag is the one thing the file gets told about, and it is told
    // once: the artifact behind the blanks is no one's text, so nothing
    // the child says about it stands.
    let reports = s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("not in scope")));
    assert_eq!(reports.len(), 1, "{reports:?}");

    // A local reads its type, and the guarded index still answers.
    let h = s.hover(&uri, 5, 11);
    assert!(h.contains("{ Part }?"), "the binding: {h}");
    let h = s.hover(&uri, 6, 11);
    assert!(h.contains("Part?"), "the guarded index: {h}");

    // A plain line lists the scope, the file's own names included.
    let labels = s.completion_labels(&uri, 7, 15);
    for name in ["first", "parts", "App", "print"] {
        assert!(labels.iter().any(|l| l == name), "{name}: {}", labels.len());
    }

    // Inside a blanked region there is nothing to answer with. The tag
    // itself still reads from the markup, which needs no lowering.
    assert!(
        s.completion_items(&uri, 8, 24).is_empty(),
        "the hole of a blanked tag"
    );
    let h = s.hover(&uri, 8, 13);
    assert!(h.contains("Roblox class `Frame`"), "the tag: {h}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// One inlay hint's label, whether the child sent it whole or in parts.
fn hint_text(hint: &Value) -> String {
    match &hint["label"] {
        Value::String(text) => text.clone(),

        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["value"].as_str())
            .collect::<String>(),

        _ => String::new(),
    }
}

/// The element `alloy/closeTag` names at a position, or null.
fn close_tag(s: &mut Session, uri: &str, line: u32, character: u32) -> Value {
    s.request(
        "alloy/closeTag",
        json!({ "uri": uri, "position": { "line": line, "character": character } }),
    )
}

/// The edits a newline on-type request answers with.
fn newline_edits(s: &mut Session, uri: &str, line: u32, character: u32) -> Value {
    s.request(
        "textDocument/onTypeFormatting",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character },
                "ch": "\n", "options": { "tabSize": 4, "insertSpaces": true } }),
    )
}

#[test]
fn the_server_names_the_tag_the_editor_closes() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-closetag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("panel.alx");
    std::fs::write(&file, MARKUP).unwrap();

    let root = format!("file://{}", dir.display());
    let mut s = start_with(
        &child,
        json!({ "processId": std::process::id(), "rootUri": root, "capabilities": {} }),
    );
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau-jsx", "version": 1, "text": MARKUP } } }),
    );

    // `<Frame>` on line 1 ends at column 18; the cursor sits after `>`.
    // The tag already closes below, so nothing is written twice.
    let answer = close_tag(&mut s, &uri, 1, 18);
    assert_eq!(answer, Value::Null, "closed already: {answer}");

    // A tag with no closing tag yet: the editor writes `</Frame>` and
    // the snippet holds the caret between the two.
    let typed = "local e = <Frame>\n";
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [ { "text": typed } ] } }),
    );
    let answer = close_tag(&mut s, &uri, 0, 17);
    assert_eq!(answer, json!({ "name": "Frame" }), "open tag: {answer}");

    // A self-closing tag closes itself.
    let typed = "local e = <Frame />\n";
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": { "uri": uri, "version": 3 },
            "contentChanges": [ { "text": typed } ] } }),
    );
    let answer = close_tag(&mut s, &uri, 0, 19);
    assert_eq!(answer, Value::Null, "self closing: {answer}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_newline_writes_the_end_of_an_open_block() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-autoend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("blocks.aly");
    let opener = "function test()\n";
    std::fs::write(&file, opener).unwrap();

    let root = format!("file://{}", dir.display());
    let mut s = start_with(
        &child,
        json!({ "processId": std::process::id(), "rootUri": root, "capabilities": {
            "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } } } }),
    );
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": opener } } }),
    );

    // Enter after `function test()` leaves the cursor on line 1. The
    // `end` goes one line below it, at the opener's indentation.
    let edits = newline_edits(&mut s, &uri, 1, 0);
    assert_eq!(
        edits,
        json!([{
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 0 } },
            "newText": "\nend",
        }]),
        "after an opener: {edits}"
    );

    // Enter opens no popup: the newline completion answers nothing.
    let items = newline_items(&mut s, &uri, 1, 0);
    assert!(items.is_empty(), "a popup on Enter: {items:#?}");

    // Each Alloy opener answers, and the indentation follows the
    // opener, whatever the editor wrote on the new line.
    for (text, line, character, want) in [
        ("struct Point as\n", 1, 0, "\nend"),
        ("impl Point as\n", 1, 0, "\nend"),
        ("trait Show as\n", 1, 0, "\nend"),
        ("enum Color as\n", 1, 0, "\nend"),
        ("match m with\n", 1, 0, "\nend"),
        ("do\n", 1, 0, "\nend"),
        ("if x then\n", 1, 0, "\nend"),
        ("for i = 1, 2 do\n", 1, 0, "\nend"),
        ("while x do\n", 1, 0, "\nend"),
        ("local t = {}\nfunction t.f()\n", 2, 0, "\nend"),
        ("function test()\n    ", 1, 4, "\nend"),
        ("if x then\n    while y do\n        ", 2, 8, "\n    end"),
        // A body inside a block that already ends: the `end` goes at
        // the opener's own column, never at the column of the block
        // that holds it.
        (
            "namespace N as\n    function f()\n        \nend\n",
            2,
            8,
            "\n    end",
        ),
        (
            "impl Point as\n    function Point.zero()\n        \nend\n",
            2,
            8,
            "\n    end",
        ),
        (
            "trait Show as\n    function show(self)\n        \nend\n",
            2,
            8,
            "\n    end",
        ),
        (
            "namespace A as\n    namespace B as\n        function f()\n            \n    end\nend\n",
            3,
            12,
            "\n        end",
        ),
        (
            "struct V as\n    n: number\nend\n\nimpl V as\n    function V.scale(self)\n        \nend\n",
            6,
            8,
            "\n    end",
        ),
        // A file that indents with tabs takes the tab back.
        (
            "namespace N as\n\tfunction f()\n\t\t\nend\n",
            2,
            2,
            "\n\tend",
        ),
    ] {
        write(
            &mut s.stdin,
            &json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
                "textDocument": { "uri": uri, "version": 9 },
                "contentChanges": [ { "text": text } ] } }),
        );
        let edits = newline_edits(&mut s, &uri, line, character);
        assert_eq!(edits[0]["newText"], json!(want), "{text}: {edits}");
        assert_eq!(
            edits[0]["range"]["start"],
            json!({ "line": line, "character": character }),
            "{text}: {edits}"
        );
    }

    // A balanced file, and a line the reader has written on, want
    // nothing.
    for (text, line, character) in [
        ("function test()\nend\n", 1, 0),
        ("function test()\n    local x = 1\n", 1, 4),
    ] {
        write(
            &mut s.stdin,
            &json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
                "textDocument": { "uri": uri, "version": 10 },
                "contentChanges": [ { "text": text } ] } }),
        );
        let edits = newline_edits(&mut s, &uri, line, character);
        assert_eq!(edits, json!([]), "{text}: {edits}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_editor_can_turn_both_helpers_off() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-helpers-off-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let aly = dir.join("blocks.aly");
    let alx = dir.join("panel.alx");
    let opener = "function test()\n";
    let tag = "local e = <Frame>\n";
    std::fs::write(&aly, opener).unwrap();
    std::fs::write(&alx, tag).unwrap();

    let root = format!("file://{}", dir.display());
    let mut s = start_with(
        &child,
        json!({ "processId": std::process::id(), "rootUri": root,
                "initializationOptions": { "autoCloseTags": false, "autoEnd": false },
                "capabilities": { "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } } } }),
    );
    let aly_uri = format!("file://{}", aly.display());
    let alx_uri = format!("file://{}", alx.display());

    for (uri, language, text) in [
        (&aly_uri, "alloy-luau", opener),
        (&alx_uri, "alloy-luau-jsx", tag),
    ] {
        write(
            &mut s.stdin,
            &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
                "textDocument": { "uri": uri, "languageId": language, "version": 1, "text": text } } }),
        );
    }

    let answer = close_tag(&mut s, &alx_uri, 0, 17);
    assert_eq!(answer, Value::Null, "tags off: {answer}");

    let edits = newline_edits(&mut s, &aly_uri, 1, 0);
    assert_eq!(edits, json!([]), "end off: {edits}");

    // The editor turns them back on without a restart.
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "workspace/didChangeConfiguration", "params": {
            "settings": { "autoCloseTags": true, "autoEnd": true } } }),
    );
    let answer = close_tag(&mut s, &alx_uri, 0, 17);
    assert_eq!(answer["name"], json!("Frame"), "tags on: {answer}");

    let edits = newline_edits(&mut s, &aly_uri, 1, 0);
    assert_eq!(edits[0]["newText"], json!("\nend"), "end on: {edits}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The migration, end to end: a `global` declaration reports with the
/// removal message, the quick fix writes `export` over the word, and the
/// file that reads the name gets the `import` line from the auto import.
#[test]
fn the_global_removal_reports_and_the_fixes_migrate_it() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-globals-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"game\"\n",
    )
    .unwrap();
    let a_src = "--- A count.\nglobal local counter = 0\n";
    let a = dir.join("src/a.aly");
    std::fs::write(&a, a_src).unwrap();

    let mut s = start(&child, &dir);
    let a_uri = format!("file://{}", a.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": a_uri, "languageId": "alloy-luau", "version": 1, "text": a_src } } }),
    );

    // The declaration reports at the word, and the message names the
    // `export` and the `import` that replace it.
    let diags = s.diagnostics(&a_uri, |ds| {
        ds.iter().any(|d| d.contains("`global` is removed"))
    });
    let said = diags
        .iter()
        .find(|d| d.contains("`global` is removed"))
        .expect("the report");
    assert!(said.starts_with("ImportError:"), "{said}");
    assert!(said.contains("`export local`"), "{said}");
    assert!(said.contains("import { counter } from \"./a\""), "{said}");

    // The quick fix on the declaration rewrites the one word.
    let actions = s.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": a_uri },
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 6 } },
            "context": { "diagnostics": [] },
        }),
    );
    let list = actions.as_array().cloned().unwrap_or_default();
    let fix = list
        .iter()
        .find(|x| x["title"] == json!("replace `global` with `export`"))
        .unwrap_or_else(|| panic!("no fix in {list:#?}"));
    let edit = &fix["edit"]["changes"][&a_uri][0];
    assert_eq!(edit["newText"], json!("export"));
    assert_eq!(edit["range"]["start"], json!({ "line": 1, "character": 0 }));
    assert_eq!(edit["range"]["end"], json!({ "line": 1, "character": 6 }));

    // With the fix applied, the module exports the name.
    let fixed = "--- A count.\nexport local counter = 0\n";
    std::fs::write(&a, fixed).unwrap();
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": { "uri": a_uri, "version": 2 },
            "contentChanges": [{ "text": fixed }] } }),
    );

    // The file that read the name bare now has an unknown name, and the
    // auto import offers the line that binds it.
    let b_src = "print(counter)\n";
    let b = dir.join("src/b.aly");
    std::fs::write(&b, b_src).unwrap();
    let b_uri = format!("file://{}", b.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": b_uri, "languageId": "alloy-luau", "version": 1, "text": b_src } } }),
    );
    let unknown = s.diagnostics(&b_uri, |ds| ds.iter().any(|d| d.contains("counter")));
    let said = unknown
        .iter()
        .find(|d| d.contains("counter"))
        .unwrap_or_else(|| panic!("no report: {unknown:#?}"));
    let where_it_is = json!({
        "start": { "line": 0, "character": 6 },
        "end": { "line": 0, "character": 13 },
    });
    // The editor sends the report it holds with the request, and the
    // import fix reads the name off it.
    let offers = s.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": b_uri },
            "range": where_it_is,
            "context": { "diagnostics": [{ "range": where_it_is, "message": said }] },
        }),
    );
    let offered = offers.as_array().cloned().unwrap_or_default();
    let add = offered
        .iter()
        .find(|x| {
            x["title"]
                .as_str()
                .is_some_and(|t| t.starts_with("Add `import { counter }"))
        })
        .unwrap_or_else(|| panic!("no import fix in {offered:#?}"));
    let line = &add["edit"]["changes"][&b_uri][0];
    assert_eq!(
        line["newText"],
        json!("import { counter } from \"./a\"\n"),
        "{add}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A space in code asks for nothing, so no popup opens where the
/// author is typing words.
#[test]
fn a_space_in_code_completes_nothing() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-sidespace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "local x = 1\n";
    let file = dir.join("t.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let plain = s.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 10 },
            "context": { "triggerKind": 2, "triggerCharacter": " " }
        }),
    );
    assert_eq!(plain, json!([]), "{plain}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A module added on disk after the server started. The poll reads it,
/// the open file that imports it is compiled again, and the report on
/// the import is gone.
#[test]
fn a_module_added_on_disk_reaches_the_open_files() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-newmodule-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"game\"\n",
    )
    .unwrap();
    let src = "import { shout } from \"./shout\"\n\nlocal n: number = shout(\"hi\")\nprint(n)\n";
    let file = dir.join("src/main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start_env(
        &child,
        json!({ "processId": std::process::id(), "rootUri": format!("file://{}", dir.display()), "capabilities": {} }),
        &[("ALLOY_LSP_POLL_SECS", "1")],
    );
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The module is not there yet.
    s.diagnostics(&uri, |ds| ds.iter().any(|d| d.contains("shout")));

    // It lands on disk after the server started.
    std::fs::write(
        dir.join("src/shout.aly"),
        "--- Writes a loud line.\nexport function shout(msg: string): number\n    print(msg)\n    return 1\nend\n",
    )
    .unwrap();

    // The poll reads it, and the open file is compiled again: the
    // import resolves, so the report on it is gone.
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut hover = String::new();

    while Instant::now() < deadline {
        s.drain(Duration::from_secs(2));
        hover = s.hover(&uri, 2, 20);

        if hover.contains("Writes a loud line.") {
            break;
        }
    }

    assert!(hover.contains("Writes a loud line."), "{hover}");

    s.drain(Duration::from_secs(2));
    let last: Vec<String> = s
        .seen
        .iter()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .and_then(|m| m["params"]["diagnostics"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|d| d["message"].as_str().map(str::to_string))
        .collect();
    assert!(
        !last.iter().any(|d| d.contains("shout")),
        "{last:?}\n{hover}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A big project must not hold the editor. The workspace scan compiles
/// every file; on the request thread it used to keep the first
/// `didOpen`, and every request after it, until the whole pass ended.
#[test]
fn a_hover_answers_while_a_large_workspace_opens() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-large-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"game\"\n",
    )
    .unwrap();

    for i in 0..500 {
        std::fs::write(
            dir.join(format!("src/part{i}.aly")),
            format!(
                "struct Part{i} as\n    value: number\nend\n\n\
                 export function take{i}(p: Part{i}): number\n    return p.value\nend\n"
            ),
        )
        .unwrap();
    }

    let src = "struct Main as\n    total: number\nend\n\nlocal m = new Main { total = 1 }\nprint(m.total)\n";
    let file = dir.join("src/main.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // The scan of the other 500 files runs beside this hover.
    let asked = Instant::now();
    let hover = s.hover(&uri, 5, 8);
    let took = asked.elapsed();
    assert!(hover.contains("total"), "{hover}");
    assert!(
        took < Duration::from_secs(2),
        "hover took {took:?}: {hover}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A restart during the workspace pass: the editor gives a server two
/// seconds to answer `shutdown` and kills it after. The proxy answers
/// at once, whatever the child is doing, and leaves on `exit`.
#[test]
fn a_shutdown_answers_at_once_during_the_workspace_pass() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-shutdown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("alloy.toml"),
        "[build]\nin = \"src\"\nout = \"build\"\n\n[project]\nname = \"game\"\n",
    )
    .unwrap();

    for i in 0..300 {
        std::fs::write(
            dir.join(format!("src/part{i}.aly")),
            format!("struct Part{i} as\n    value: number\nend\n"),
        )
        .unwrap();
    }

    let mut s = start(&child, &dir);
    // The pass over the 300 files has just started.
    let asked = Instant::now();
    let answer = s.request("shutdown", json!(null));
    let took = asked.elapsed();
    assert!(answer.is_null(), "{answer}");
    assert!(took < Duration::from_secs(2), "shutdown took {took:?}");

    write(&mut s.stdin, &json!({ "jsonrpc": "2.0", "method": "exit" }));
    let left = Instant::now();

    while s._server.0.try_wait().ok().flatten().is_none() {
        assert!(
            left.elapsed() < Duration::from_secs(3),
            "the server did not leave after exit"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `{` of an object initializer opens its property list, so the
/// reader never presses Ctrl+Space there. Every other `{` opens
/// nothing, and the blank line inside the braces opens the list again.
#[test]
fn an_object_initializer_opens_on_its_brace() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-initbrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "local part = new Instance(\"Part\") {}\nlocal plain = {}\nlocal p2 = new Instance(\"Part\") {\n    \n}\nlocal p3 = new Instance(\"Part\") { Name = \"a\", }\nprint(part, plain, p2, p3)\n";
    let file = dir.join("t.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    let triggered = |s: &mut Session, line: u32, character: u32, trigger: &str| -> Vec<String> {
        let r = s.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "triggerKind": 2, "triggerCharacter": trigger }
            }),
        );

        r.get("items")
            .and_then(Value::as_array)
            .or_else(|| r.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    };

    // Line 0, right after the `{` of `new Instance("Part") {`.
    let props = triggered(&mut s, 0, 35, "{");
    assert!(props.contains(&"Anchored".to_string()), "{props:?}");
    assert!(props.contains(&"Size".to_string()), "{props:?}");

    // Line 1, the `{` of a plain table: nothing.
    assert_eq!(triggered(&mut s, 1, 15, "{"), Vec::<String>::new());

    // Line 3, the blank line inside the braces.
    let inside = triggered(&mut s, 3, 4, "\n");
    assert!(inside.contains(&"Anchored".to_string()), "{inside:?}");

    // Line 5, the space after the comma of the field list.
    let after_comma = triggered(&mut s, 5, 46, " ");
    assert!(
        after_comma.contains(&"Anchored".to_string()),
        "{after_comma:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A service binding hovers as the import line that binds it and the
/// class the definitions declare, and the same import types the binding
/// so a member of the service resolves.
#[test]
fn a_service_binding_hovers_and_types() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-service-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "import Players from \"@game/Players\"\nimport { RunService as Run } from \"@game\"\n\nprint(Players.MaxPlayers, Run.Heartbeat)\n";
    let file = dir.join("t.aly");
    std::fs::write(&file, src).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": src } } }),
    );

    // Line 3, the `Players` of `Players.MaxPlayers`.
    let h = s.hover(&uri, 3, 8);
    assert!(h.contains("local Players: Players"), "{h}");
    assert!(h.contains("a Roblox service"), "{h}");

    // Line 1, the alias the braces rename to.
    let h = s.hover(&uri, 1, 24);
    assert!(h.contains("RunService"), "{h}");

    // The binding carries the service class, so no member of it
    // reports.
    let diags = s.diagnostics(&uri, |ds| {
        ds.iter().all(|d| !d.contains("MaxPlayers")) && ds.iter().all(|d| !d.contains("Heartbeat"))
    });
    assert!(
        diags
            .iter()
            .all(|d| !d.contains("MaxPlayers") && !d.contains("Heartbeat")),
        "{diags:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A colon method on a plain table, and a constructor that writes
/// `new Self()`. Both used to leave the editor with no type: `self`
/// answered nothing and listed nothing, and the constructor's inferred
/// return read `unknown`.
const SELF_AND_NEW: &str = "\
local Provider = { }

Provider.count = 0

function Provider:Bump(n: number): number
    self.count += n
    return self.count
end

function Provider:Reset()
    self.count = 0
    print(self.)
end

struct Test as end

impl Test as
    function new()
        return new Test()
    end
end

print(Provider, Test.new())
";

/// The check artifact writes `self: typeof(Provider)` on a colon method
/// of a plain table, so hover on `self` reads the table and `self.`
/// lists its members. `new Test()` inside `Test`'s own `new` builds the
/// value instead of calling the constructor again, so the hint on the
/// return reads the struct.
#[test]
fn a_table_method_types_self_and_a_constructor_returns_its_struct() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");

        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-self-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("provider.aly");
    std::fs::write(&file, SELF_AND_NEW).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": SELF_AND_NEW } } }),
    );
    s.drain(Duration::from_secs(3));

    // `self` reads the table it stands for, by the name the source
    // gave the value.
    let h = s.hover(&uri, 5, 5);
    assert!(h.contains("typeof(Provider)"), "self in a method: {h}");
    assert!(!h.contains("where"), "self in a method: {h}");

    // `self.` lists every member, the fields and the methods.
    let labels = s.completion_labels(&uri, 11, 15);
    assert!(
        ["count", "Bump", "Reset"]
            .iter()
            .all(|m| labels.iter().any(|l| l == m)),
        "{labels:?}"
    );

    // The constructor's return: the struct, not `unknown`.
    let hints = s.request(
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": uri },
            "range": { "start": { "line": 16, "character": 0 },
                       "end": { "line": 21, "character": 0 } } }),
    );
    let on_new: Vec<String> = hints
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|h| h["position"]["line"] == 17)
        .map(hint_text)
        .collect();
    assert!(on_new.iter().any(|l| l == ": Test"), "{hints}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A struct with two impl blocks: one of its own with a private method,
/// and one for a trait.
const IMPLS: &str = "\
struct Test as
    x: number
end

--- What the block adds.
impl Test as
    function test()
    end

    private function hidden()
    end
end

trait Display as
    function show(self): string
end

impl Display for Test as
    function show(self): string
        return \"t\"
    end
end

local t = new Test { x = 1 }
local a: Test = t
print(t, a, t:test(), t:show())
";

/// The header of an `impl` hovers as the block: its own line, the
/// public methods inside it, and the doc comment above it. The same
/// name anywhere else still hovers as the struct.
#[test]
fn an_impl_header_hovers_as_its_block() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");

        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-impl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("impls.aly");
    std::fs::write(&file, IMPLS).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": IMPLS } } }),
    );
    s.drain(Duration::from_secs(2));

    // The `impl` keyword and the target name both answer with the block.
    for character in [0, 6] {
        let h = s.hover(&uri, 5, character);
        assert!(h.contains("impl Test as"), "impl header: {h}");
        assert!(h.contains("public function test()"), "impl header: {h}");
        assert!(!h.contains("hidden"), "a private method: {h}");
        assert!(!h.contains("struct Test"), "impl header: {h}");
        assert!(h.contains("What the block adds."), "the doc comment: {h}");
    }

    // A block for a trait names the trait first.
    let h = s.hover(&uri, 17, 18);
    assert!(h.contains("impl Display for Test as"), "trait impl: {h}");
    assert!(
        h.contains("public function show(self): string"),
        "trait impl: {h}"
    );

    // The same name in an annotation and in a `new` keeps the struct.
    let h = s.hover(&uri, 24, 10);
    assert!(h.contains("struct Test as"), "annotation: {h}");
    let h = s.hover(&uri, 23, 15);
    assert!(h.contains("struct Test as"), "new: {h}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that writes each contextual word both ways, with a `new` chain.
const CONTEXTUAL: &str = "\
--!strict
struct Thing as
    label: string
    count: number
end

impl Thing as
    function new(count: number): Thing
        return new Thing { label = \"t\", count = count }
    end

    function get(self): number
        return self.count
    end
end

local new = Instance.new
local export = { count = 1 }
local try = pcall
local match = string.match
const LIMIT = 5
local const = LIMIT

local a = new Thing(1):get()
local b = new Thing(2).count
local picked = export.count
local made = new Thing(6)
print(new, export, try, match, const, a, b, picked, made)
";

/*
The five contextual words and the `new` chain, over the protocol.

A local named `new` must hover as the local and complete off its own
type, and a chain off `new Thing()` must offer the struct's members. Both
answers come from the child, so only an end to end run proves them.
*/
#[test]
fn contextual_words_and_a_new_chain() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    let dir = std::env::temp_dir().join(format!("alloy-lsp-ctx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ctx.aly");
    std::fs::write(&file, CONTEXTUAL).unwrap();

    let mut s = start(&child, &dir);
    let uri = format!("file://{}", file.display());
    write(
        &mut s.stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1, "text": CONTEXTUAL } } }),
    );

    // No reserved word and no unknown global: every word is a name here.
    let diags = s.diagnostics(&uri, |_| true);
    assert!(
        diags.iter().all(|d| !d.contains("ReservedWord")),
        "{diags:#?}"
    );
    s.drain(Duration::from_secs(2));

    // Each local hovers with its own type, not with the keyword's page.
    let h = s.hover(&uri, 16, 6);
    assert!(
        h.contains("local new") && h.contains("Instance"),
        "new: {h}"
    );
    let h = s.hover(&uri, 17, 6);
    assert!(
        h.contains("local export") && h.contains("count"),
        "export: {h}"
    );
    let h = s.hover(&uri, 18, 6);
    assert!(h.contains("local try"), "try: {h}");
    let h = s.hover(&uri, 19, 6);
    assert!(h.contains("local match"), "match: {h}");
    let h = s.hover(&uri, 21, 6);
    assert!(
        h.contains("local const") && h.contains("number"),
        "const: {h}"
    );

    // The chain reads the struct's members at each link.
    let h = s.hover(&uri, 23, 23);
    assert!(
        h.contains("Thing") && h.contains("number"),
        "chain get: {h}"
    );
    let h = s.hover(&uri, 24, 23);
    assert!(h.contains("number"), "chain field: {h}");
    let h = s.hover(&uri, 23, 6);
    assert!(
        h.contains("local a") && h.contains("number"),
        "chain type: {h}"
    );
    let h = s.hover(&uri, 24, 6);
    assert!(
        h.contains("local b") && h.contains("number"),
        "chain field type: {h}"
    );

    // `new Thing():` offers the struct's methods; `.` adds its fields.
    let labels = s.completion_labels(&uri, 23, 23);
    assert!(labels.iter().any(|l| l == "get"), "methods: {labels:?}");
    let labels = s.completion_labels(&uri, 24, 23);
    assert!(
        labels.iter().any(|l| l == "count") && labels.iter().any(|l| l == "label"),
        "fields: {labels:?}"
    );

    // `export.` names the table's own field, so the local is a table.
    let labels = s.completion_labels(&uri, 25, 22);
    assert_eq!(labels, vec!["count".to_string()], "export field");

    // `new ` still offers the struct names: the constructor is a keyword
    // there, whatever local shares its spelling.
    let labels = s.completion_labels(&uri, 26, 17);
    assert!(
        labels.iter().any(|l| l == "Thing"),
        "new target: {labels:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
