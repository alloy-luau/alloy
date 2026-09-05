//! `alloy-lsp` entry point.
//!
//! The server owns the editor connection over stdio. It desugars each
//! Alloy buffer in memory, feeds the emitted Luau to a child luau-lsp
//! process as shadow documents, and maps every result back to source
//! positions. See `proxy.rs`.
//!
//! Arguments:
//!
//! - `--luau-lsp <path>`: the child binary. Default: `ALLOY_LUAU_LSP`,
//!   then `luau-lsp` on the PATH.
//! - `--definitions <path>`: a definitions file for the child; a `.d.aly`
//!   compiles to `.d.luau` in the cache directory first. Repeatable.
//!   Every `.d.aly` under the workspace root joins them on its own, and
//!   every `impl` on a foreign type under the root is injected into the
//!   definitions that declare the target.
//! - `--docs <path>`: the API docs JSON for the child, for hover text.
//! - `--old-solver`: do not pass `--flag:LuauSolverV2=true`.
//! - `--log-level <level>`: what stderr shows: `off`, `error`, `warn`,
//!   `info`, `debug`, or `trace`. Default: `ALLOY_LSP_LOG`, then `warn`.
//! - `--log`: the same as `--log-level trace`.
//! - Everything after `--` goes to the child as is.

mod block_end;
mod context;
mod doc;
mod extensions;
mod imports;
mod keywords;
mod log;
mod markup;
mod proxy;
mod rpc;
mod settings;
mod tokens;

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!(
            "alloy-lsp {} (alloy {})",
            env!("CARGO_PKG_VERSION"),
            alloy::VERSION
        );
        return ExitCode::SUCCESS;
    }

    let mut child_path = std::env::var("ALLOY_LUAU_LSP").unwrap_or_else(|_| "luau-lsp".to_string());
    let mut definitions: Vec<PathBuf> = Vec::new();
    let mut docs: Option<String> = None;
    let mut new_solver = true;
    // An unnamed level in the variable, such as `1`, means trace, so the
    // old on or off use keeps working.
    let mut level = match std::env::var("ALLOY_LSP_LOG") {
        Ok(value) => log::Level::parse(&value).unwrap_or(log::Level::Trace),

        Err(_) => log::Level::DEFAULT,
    };
    let mut passthrough: Vec<String> = Vec::new();
    // Argument complaints wait until the level is known.
    let mut warnings: Vec<String> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--luau-lsp" if i + 1 < args.len() => {
                child_path = args[i + 1].clone();
                i += 1;
            }

            "--definitions" if i + 1 < args.len() => {
                definitions.push(PathBuf::from(&args[i + 1]));
                i += 1;
            }

            "--docs" if i + 1 < args.len() => {
                docs = Some(args[i + 1].clone());
                i += 1;
            }

            "--old-solver" => new_solver = false,

            "--log" => level = log::Level::Trace,

            "--log-level" if i + 1 < args.len() => {
                match log::Level::parse(&args[i + 1]) {
                    Some(l) => level = l,

                    None => warnings.push(format!(
                        "unknown log level {}; expected one of {}",
                        args[i + 1],
                        log::Level::NAMES.join(", ")
                    )),
                }

                i += 1;
            }

            "--" => {
                passthrough.extend(args[i + 1..].iter().cloned());

                break;
            }

            "--stdio" => {}

            other => warnings.push(format!("unknown argument {other}")),
        }

        i += 1;
    }

    log::set(level);

    for w in &warnings {
        log::warn(w);
    }

    // The editor's first message names the workspace, and the workspace
    // names its definitions, so the child starts after that message.
    let mut stdin = BufReader::new(std::io::stdin());
    let first = match rpc::read_message(&mut stdin) {
        Ok(Some(message)) => message,

        _ => return ExitCode::SUCCESS,
    };

    let mut exts = Vec::new();

    if let Some(root) = first
        .pointer("/params/rootUri")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            first
                .pointer("/params/workspaceFolders/0/uri")
                .and_then(serde_json::Value::as_str)
        })
        .and_then(proxy::uri_to_path)
    {
        definitions.extend(workspace_definitions(&root));
        exts = extensions::collect(&workspace_files(&root, |n| {
            n.ends_with(".aly") && !n.ends_with(".d.aly")
        }));
    }

    let mut child_args: Vec<String> = vec!["lsp".to_string(), "--stdio".to_string()];

    // The editor's `fflags` section arrives in the first message.
    let editor_options = first
        .pointer("/params/initializationOptions")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    for flag in settings::child_flags(&editor_options) {
        if flag == "--flag:LuauSolverV2=true" && !new_solver {
            continue;
        }

        child_args.push(flag);
    }

    let mut injected = std::collections::HashSet::new();

    for path in &definitions {
        match prepare_definitions(path).and_then(|p| extensions::apply(&p, &exts, &mut injected)) {
            Ok(p) => child_args.push(format!("--definitions={}", p.display())),

            Err(e) => log::error(&format!("definitions {}: {e}", path.display())),
        }
    }

    match extensions::primitives_file(&exts, &mut injected) {
        Ok(Some(p)) => child_args.push(format!("--definitions={}", p.display())),

        Ok(None) => {}

        Err(e) => log::error(&format!("primitive extensions: {e}")),
    }

    for (i, ext) in exts.iter().enumerate() {
        if !injected.contains(&i) {
            log::warn(&format!(
                "impl {}: no definitions file declares the target, so {} has no type in the editor",
                ext.target, ext.name
            ));
        }
    }

    if let Some(docs) = docs {
        child_args.push(format!("--docs={docs}"));
    }

    child_args.extend(passthrough);

    let mut child = match Command::new(&child_path)
        .args(&child_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,

        Err(e) => {
            log::error(&format!(
                "cannot start {child_path}: {e}; pass --luau-lsp <path> or set ALLOY_LUAU_LSP"
            ));
            return ExitCode::FAILURE;
        }
    };

    let child_in = child.stdin.take().expect("piped");
    let child_out = child.stdout.take().expect("piped");
    let server = Arc::new(proxy::Server::new(
        Box::new(child_in),
        Box::new(std::io::stdout()),
        exts,
    ));

    // Child -> editor on its own thread.
    let reader_server = Arc::clone(&server);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(child_out);

        loop {
            match rpc::read_message(&mut reader) {
                Ok(Some(message)) => reader_server.handle_child(message),

                Ok(None) => break,

                Err(e) => {
                    log::error(&format!("child stream: {e}"));

                    break;
                }
            }
        }
    });

    // Editor -> child on the main thread, the first message included.
    if !server.handle_client(first) {
        return ExitCode::SUCCESS;
    }

    loop {
        match rpc::read_message(&mut stdin) {
            Ok(Some(message)) => {
                if !server.handle_client(message) {
                    break;
                }
            }

            Ok(None) => break,

            Err(e) => {
                log::error(&format!("client stream: {e}"));

                break;
            }
        }
    }

    let _ = child.wait();
    let _ = reader.join();
    let _ = std::io::stdout().flush();

    ExitCode::SUCCESS
}

/// Every `.d.aly` under a workspace root, outside the build output.
fn workspace_definitions(root: &Path) -> Vec<PathBuf> {
    workspace_files(root, |name| name.ends_with(".d.aly"))
}

/// Every file under a workspace root whose name passes `keep`, outside
/// the build output, `.git`, `node_modules`, and `target`.
fn workspace_files(root: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let out = alloy::config::Config::find(root).and_then(|p| {
        alloy::config::Config::load(&p)
            .ok()
            .map(|c| p.parent().unwrap_or(root).join(&c.build.out))
    });
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            if path.is_dir() {
                if matches!(name.as_str(), ".git" | "node_modules" | "target")
                    || out.as_deref() == Some(path.as_path())
                {
                    continue;
                }

                stack.push(path);
            } else if keep(&name) {
                found.push(path);
            }
        }
    }

    found.sort();

    found
}

/// A definitions file the child can read: a `.d.aly` compiles to a
/// `.d.luau` in the cache directory; anything else passes as is.
fn prepare_definitions(path: &Path) -> Result<PathBuf, String> {
    let name = path.to_string_lossy();

    if !name.ends_with(".d.aly") {
        return Ok(path.to_path_buf());
    }

    let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let options = alloy::EmitOptions {
        file_name: name.into_owned(),
        definitions: true,
        ..alloy::EmitOptions::default()
    };
    let out = alloy::compile_with(&source, &options).map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir().join("alloy-lsp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().replace(".d.aly", ""))
        .unwrap_or_else(|| "definitions".to_string());
    let target = dir.join(format!("{stem}.d.luau"));
    std::fs::write(&target, out.check).map_err(|e| e.to_string())?;

    Ok(target)
}
