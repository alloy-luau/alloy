//! End to end: the server, a child luau-lsp, one Alloy file with a type
//! error on a desugared line. Skips when luau-lsp is not installed.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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

/// Kills the server when the test ends, pass or fail, so a stuck child
/// never outlives the test.
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

/// Reads messages on a thread, so a missing message is a failure and
/// not a hang.
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

fn next(rx: &Receiver<Value>, seen: &mut Vec<Value>) -> Value {
    let m = rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("no message within 30s; seen: {seen:#?}"));
    seen.push(m.clone());

    m
}

#[test]
fn diagnostics_come_back_on_source_lines() {
    let Some(child) = luau_lsp() else {
        eprintln!("luau-lsp not found; skipping");
        return;
    };

    scenario(&child);
}

fn scenario(child: &std::path::Path) {
    let dir = std::env::temp_dir().join(format!(
        "alloy-lsp-smoke-{}-{}",
        std::process::id(),
        child.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.aly");
    // Line 1 desugars (`??`), line 2 has the type error, line 3 uses the
    // runtime so the shadow requires it. Strict mode reports the mismatch.
    std::fs::write(
        &file,
        "--!strict\nlocal a: number? = nil\nlocal v = a ?? 0\nlocal n: number = \"text\"\nlocal xs = [1, 2]\nprint(v, n, xs)\nlocal h = HEL\n",
    )
    .unwrap();
    std::fs::write(dir.join("lib.aly"), "export const HELLO = 1\n").unwrap();

    let stderr = if std::env::var_os("ALLOY_LSP_LOG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    let mut server = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_alloy-lsp"))
            .arg("--luau-lsp")
            .arg(child)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap(),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let rx = messages(server.0.stdout.take().unwrap());
    let mut seen = Vec::new();
    let root = format!("file://{}", dir.display());
    let uri = format!("file://{}", file.display());

    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "processId": std::process::id(), "rootUri": root, "capabilities": {} } }),
    );

    let init = loop {
        let m = next(&rx, &mut seen);

        if m.get("id") == Some(&json!(1)) {
            break m;
        }
    };

    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["semanticTokensProvider"]["full"], true, "{caps}");
    assert_eq!(caps["documentFormattingProvider"], true, "{caps}");
    assert!(caps["workspace"]["fileOperations"]["didRename"].is_object());

    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );
    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "alloy-luau", "version": 1,
                "text": std::fs::read_to_string(&file).unwrap() } } }),
    );

    // Wait for a diagnostics batch that holds the type error.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut found: Option<Value> = None;

    while Instant::now() < deadline {
        let m = next(&rx, &mut seen);

        if m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri {
            let diags = m["params"]["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default();

            if let Some(d) = diags
                .iter()
                .find(|d| d["message"].as_str().unwrap_or("").contains("number"))
            {
                found = Some(d.clone());
                break;
            }
        }
    }

    let d = found.unwrap_or_else(|| panic!("no type error for the .aly file; seen: {seen:#?}"));
    assert_eq!(d["range"]["start"]["line"], 3, "{d}");
    assert!(
        d["range"]["end"]["character"].as_u64() > d["range"]["start"]["character"].as_u64(),
        "{d}"
    );

    // The runtime shadow never reaches the editor.
    assert!(
        seen.iter().all(|m| m["params"]["uri"]
            .as_str()
            .is_none_or(|u| !u.ends_with("alloy.luau"))),
        "runtime shadow leaked"
    );

    // The hoisted `??` line carries no layout lint, and nothing repeats.
    let batch = seen
        .iter()
        .rev()
        .find(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
        .unwrap();
    let items = batch["params"]["diagnostics"].as_array().unwrap();
    assert!(
        items.iter().all(|i| !i["message"]
            .as_str()
            .unwrap_or("")
            .starts_with("SameLineStatement")),
        "{items:#?}"
    );

    // Hover on `n` at line 3 maps into the shadow and back.
    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/hover", "params": {
            "textDocument": { "uri": uri }, "position": { "line": 3, "character": 6 } } }),
    );

    let hover = loop {
        let m = next(&rx, &mut seen);

        if m.get("id") == Some(&json!(2)) {
            break m;
        }
    };

    let text = hover["result"].to_string();
    assert!(text.contains("number"), "{hover}");

    // Inlay hints: the loop variables and `v` get insertable types, and
    // nothing lands in generated text.
    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 4, "method": "textDocument/inlayHint", "params": {
            "textDocument": { "uri": uri },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 6, "character": 0 } } } }),
    );

    let hints = loop {
        let m = next(&rx, &mut seen);

        if m.get("id") == Some(&json!(4)) {
            break m;
        }
    };
    let hints = hints["result"].as_array().cloned().unwrap_or_default();
    assert!(
        hints.iter().any(|h| h["position"]["line"] == 2
            && h["textEdits"].as_array().is_some_and(|e| !e.is_empty())),
        "{hints:#?}"
    );

    // Completion offers an auto-import for a name another file exports.
    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 5, "method": "textDocument/completion", "params": {
            "textDocument": { "uri": uri }, "position": { "line": 6, "character": 13 } } }),
    );

    let completion = loop {
        let m = next(&rx, &mut seen);

        if m.get("id") == Some(&json!(5)) {
            break m;
        }
    };
    let items = match &completion["result"] {
        Value::Array(items) => items.clone(),

        other => other["items"].as_array().cloned().unwrap_or_default(),
    };
    let auto = items
        .iter()
        .find(|i| i["label"] == "HELLO")
        .unwrap_or_else(|| panic!("no auto-import item; {} items", items.len()));
    assert_eq!(
        auto["additionalTextEdits"][0]["newText"], "import { HELLO } from \"./lib\"\n",
        "{auto}"
    );

    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
    );
    write(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );
    drop(stdin);
    let _ = std::fs::remove_dir_all(&dir);
}
